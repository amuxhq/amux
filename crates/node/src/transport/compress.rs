//! Streaming zstd for the channels between daemons.
//!
//! A channel to a paired host is one HTTP/2 connection carrying many calls:
//! every agent subscription to that host shares one, and the inventory,
//! pages and blobs share another. What crosses it is protobuf messages of a
//! few hundred bytes that mostly repeat each other (every snapshot is the
//! whole snapshot again), so compressing each message alone finds almost
//! nothing; one compressor that keeps its history across the whole channel
//! removes most of it. [`Compressed`] is that compressor, wrapped around the
//! channel's byte stream below HTTP/2 and above TLS, so a relay copies the
//! compressed ciphertext without knowing it is there. Both ends of every
//! peer channel wrap it; nothing on the wire says so.

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll, ready};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use zstd::zstd_safe::{self, CCtx, CParameter, DCtx, InBuffer, OutBuffer};

/// zstd's default level: hundreds of MB/s on a phone, far beyond what a
/// channel carries, and within a few percent of the ratio of slower levels
/// on this data.
const LEVEL: i32 = 3;
/// The history a channel compresses against, as a power of two: 128 KiB.
/// Replayed agent journals, twenty agents to a channel, compress 5.5x with
/// it against 5.8x with zstd's default 2 MiB; doubling it gains 4% and costs
/// both ends a quarter more memory, which a phone pays for each of the
/// channel ends it holds per host. The decompressor's window is the same
/// size.
const WINDOW_LOG: i32 = 17;
/// The match-finder tables, as powers of two. The level's defaults are sized
/// for megabytes of input; these cost under 1% of the ratio on the same
/// measurement.
const HASH_LOG: i32 = 15;
const CHAIN_LOG: i32 = 14;
/// Compressed bytes read from the stream below and not yet decompressed.
const INCOMING: usize = 32 * 1024;
/// The most decompressed bytes one read hands up, so a large read buffer is
/// not zeroed whole on every call.
const READ_CHUNK: usize = 64 * 1024;

pub(crate) struct Compressed<T> {
    inner: T,
    compressor: CCtx<'static>,
    /// Compressed bytes not yet written to `inner`, from `sent` on.
    outgoing: Vec<u8>,
    sent: usize,
    /// The frame has been ended for shutdown; nothing more is compressed.
    finished: bool,
    decompressor: DCtx<'static>,
    incoming: Box<[u8]>,
    start: usize,
    end: usize,
    /// The last decompression filled its output, so the decompressor may
    /// hold more without further input.
    held: bool,
    eof: bool,
}

impl<T> Compressed<T> {
    pub(crate) fn new(inner: T) -> Self {
        let mut compressor = CCtx::create();
        for parameter in [
            CParameter::CompressionLevel(LEVEL),
            CParameter::WindowLog(WINDOW_LOG as u32),
            CParameter::HashLog(HASH_LOG as u32),
            CParameter::ChainLog(CHAIN_LOG as u32),
        ] {
            compressor
                .set_parameter(parameter)
                .expect("the channel's zstd parameters are within zstd's bounds");
        }
        Self {
            inner,
            compressor,
            outgoing: Vec::new(),
            sent: 0,
            finished: false,
            decompressor: DCtx::create(),
            incoming: vec![0; INCOMING].into_boxed_slice(),
            start: 0,
            end: 0,
            held: false,
            eof: false,
        }
    }

    /// Runs one zstd step that appends to `outgoing`, growing it first so
    /// the step has room; answers what zstd says is left to emit.
    fn emit(
        &mut self,
        step: impl FnOnce(&mut CCtx<'static>, &mut OutBuffer<'_, Vec<u8>>) -> zstd_safe::SafeResult,
    ) -> io::Result<usize> {
        self.outgoing.reserve(CCtx::out_size());
        let pos = self.outgoing.len();
        let mut output = OutBuffer::around_pos(&mut self.outgoing, pos);
        step(&mut self.compressor, &mut output).map_err(zstd_error)
    }

    #[cfg(test)]
    fn memory(&self) -> usize {
        self.compressor.sizeof() + self.decompressor.sizeof()
    }
}

impl<T: AsyncWrite + Unpin> Compressed<T> {
    /// Writes every pending compressed byte to `inner`.
    fn poll_drain(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        while self.sent < self.outgoing.len() {
            let written =
                ready!(Pin::new(&mut self.inner).poll_write(cx, &self.outgoing[self.sent..]))?;
            if written == 0 {
                return Poll::Ready(Err(io::ErrorKind::WriteZero.into()));
            }
            self.sent += written;
        }
        self.outgoing.clear();
        self.sent = 0;
        Poll::Ready(Ok(()))
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for Compressed<T> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = &mut *self;
        if this.finished {
            return Poll::Ready(Err(io::ErrorKind::BrokenPipe.into()));
        }
        // What the last write produced goes out before more is taken, so a
        // stalled stream below pushes back instead of buffering here.
        ready!(this.poll_drain(cx))?;
        let mut input = InBuffer::around(buf);
        while input.pos() < buf.len() {
            this.emit(|compressor, output| {
                compressor.compress_stream2(
                    output,
                    &mut input,
                    zstd_safe::zstd_sys::ZSTD_EndDirective::ZSTD_e_continue,
                )
            })?;
        }
        // Start it on its way; the bytes are taken whatever this answers.
        if let Poll::Ready(Err(error)) = this.poll_drain(cx) {
            return Poll::Ready(Err(error));
        }
        Poll::Ready(Ok(buf.len()))
    }

    /// Ends a zstd block, so everything written so far reaches the other end
    /// as bytes it can decompress at once. HTTP/2 flushes after the frames
    /// it writes; without this a live message would wait in the compressor
    /// until something else was written behind it.
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = &mut *self;
        if !this.finished {
            while this.emit(|compressor, output| compressor.flush_stream(output))? > 0 {}
        }
        ready!(this.poll_drain(cx))?;
        Pin::new(&mut this.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = &mut *self;
        if !this.finished {
            while this.emit(|compressor, output| compressor.end_stream(output))? > 0 {}
            this.finished = true;
        }
        ready!(this.poll_drain(cx))?;
        Pin::new(&mut this.inner).poll_shutdown(cx)
    }
}

impl<T: AsyncRead + Unpin> AsyncRead for Compressed<T> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = &mut *self;
        if buf.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        loop {
            if this.start < this.end || this.held {
                let room = buf.remaining().min(READ_CHUNK);
                let out = buf.initialize_unfilled_to(room);
                let mut input = InBuffer::around(&this.incoming[this.start..this.end]);
                let mut output = OutBuffer::around(out);
                this.decompressor
                    .decompress_stream(&mut output, &mut input)
                    .map_err(zstd_error)?;
                let (consumed, produced) = (input.pos(), output.pos());
                this.start += consumed;
                this.held = produced == room;
                if produced > 0 {
                    buf.advance(produced);
                    return Poll::Ready(Ok(()));
                }
                if consumed > 0 && this.start < this.end {
                    continue;
                }
            }
            if this.eof {
                // The stream below ended; what it carried has been handed
                // up, and the end is the caller's to read as it reads any
                // end, a frame cut short included.
                return Poll::Ready(Ok(()));
            }
            if this.start == this.end {
                this.start = 0;
                this.end = 0;
            } else if this.end == this.incoming.len() {
                this.incoming.copy_within(this.start..this.end, 0);
                this.end -= this.start;
                this.start = 0;
            }
            let mut read = ReadBuf::new(&mut this.incoming[this.end..]);
            ready!(Pin::new(&mut this.inner).poll_read(cx, &mut read))?;
            match read.filled().len() {
                0 => this.eof = true,
                n => this.end += n,
            }
        }
    }
}

fn zstd_error(code: usize) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("channel compression: {}", zstd_safe::get_error_name(code)),
    )
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream, duplex};

    use super::*;

    fn pair() -> (Compressed<DuplexStream>, Compressed<DuplexStream>) {
        let (a, b) = duplex(64 * 1024);
        (Compressed::new(a), Compressed::new(b))
    }

    /// Reads whatever one read hands up, failing rather than hanging when
    /// nothing arrives.
    async fn read_some(reader: &mut Compressed<DuplexStream>) -> Vec<u8> {
        let mut buf = vec![0; 4096];
        let n = tokio::time::timeout(Duration::from_secs(5), reader.read(&mut buf))
            .await
            .expect("the flushed bytes arrived without anything written behind them")
            .unwrap();
        buf.truncate(n);
        buf
    }

    #[tokio::test]
    async fn a_flushed_message_arrives_with_nothing_written_behind_it() {
        let (mut writer, mut reader) = pair();
        for message in [&b"first"[..], b"second", b"first"] {
            writer.write_all(message).await.unwrap();
            writer.flush().await.unwrap();
            assert_eq!(read_some(&mut reader).await, message);
        }
    }

    #[tokio::test]
    async fn both_directions_carry_on_one_stream() {
        let (mut left, mut right) = pair();
        left.write_all(b"ping").await.unwrap();
        left.flush().await.unwrap();
        right.write_all(b"pong").await.unwrap();
        right.flush().await.unwrap();
        assert_eq!(read_some(&mut right).await, b"ping");
        assert_eq!(read_some(&mut left).await, b"pong");
    }

    #[tokio::test]
    async fn large_writes_round_trip_through_a_small_pipe() {
        let (a, b) = duplex(1024);
        let (mut writer, mut reader) = (Compressed::new(a), Compressed::new(b));
        // Incompressible bytes, as an image's are, and text after them.
        let mut state = 0x2545_f491_4f6c_dd1d_u64;
        let mut sent: Vec<u8> = (0..3 * 1024 * 1024)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state as u8
            })
            .collect();
        sent.extend(b"a line of a log\n".repeat(64 * 1024));
        let expected = sent.clone();
        let writing = tokio::spawn(async move {
            for chunk in sent.chunks(100_000) {
                writer.write_all(chunk).await.unwrap();
            }
            writer.shutdown().await.unwrap();
        });
        let mut received = Vec::new();
        reader.read_to_end(&mut received).await.unwrap();
        writing.await.unwrap();
        assert!(received == expected, "the bytes came back changed");
    }

    #[tokio::test]
    async fn a_stream_cut_short_reads_as_its_end() {
        let (a, b) = duplex(64 * 1024);
        let mut writer = Compressed::new(a);
        let mut reader = Compressed::new(b);
        writer.write_all(b"held up to here").await.unwrap();
        writer.flush().await.unwrap();
        writer.write_all(b"never flushed").await.unwrap();
        drop(writer);
        let mut received = Vec::new();
        reader.read_to_end(&mut received).await.unwrap();
        assert_eq!(received, b"held up to here");
    }

    /// A real agent's journal, each step written and flushed as HTTP/2
    /// writes a message, compresses far better through one channel than
    /// through a fresh one per step: the channel carries its history from
    /// message to message, which is the whole of the gain on messages this
    /// small.
    #[tokio::test]
    async fn a_channel_compresses_each_message_against_the_ones_before() {
        let journal = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../replay-support/tests/fixtures/bundle/agents/",
            "d6832ca2-2f0a-4307-9cf6-5a0e13e937cc/journal/0000000000"
        ))
        .unwrap();
        let mut frames = Vec::new();
        let mut rest = &journal[..];
        while !rest.is_empty() {
            let (len, used) = varint(rest);
            frames.push(&rest[used..used + len]);
            rest = &rest[used + len..];
        }
        let one_channel = compressed_size(&frames).await;
        let mut channel_each = 0;
        for frame in &frames {
            channel_each += compressed_size(&[frame]).await;
        }
        let raw: usize = frames.iter().map(|frame| frame.len()).sum();
        assert!(
            one_channel * 2 < channel_each && one_channel * 2 < raw,
            "{} steps of {raw} bytes: {one_channel} through one channel, \
             {channel_each} through a channel each",
            frames.len()
        );
    }

    /// The bytes `messages` take on the stream below, each written and
    /// flushed, and the stream then shut.
    async fn compressed_size(messages: &[&[u8]]) -> usize {
        let (a, mut b) = duplex(4 * 1024 * 1024);
        let mut writer = Compressed::new(a);
        for message in messages {
            writer.write_all(message).await.unwrap();
            writer.flush().await.unwrap();
        }
        writer.shutdown().await.unwrap();
        drop(writer);
        let mut wire = Vec::new();
        b.read_to_end(&mut wire).await.unwrap();
        wire.len()
    }

    fn varint(bytes: &[u8]) -> (usize, usize) {
        let mut value = 0;
        for (i, byte) in bytes.iter().enumerate() {
            value |= usize::from(byte & 0x7f) << (7 * i);
            if byte & 0x80 == 0 {
                return (value, i + 1);
            }
        }
        panic!("a truncated varint");
    }

    /// What one end of a channel holds once it has carried traffic both
    /// ways, so its compressor and its decompressor have both allocated.
    #[tokio::test]
    async fn one_channel_end_holds_under_two_megabytes() {
        let (mut left, mut right) = pair();
        let text = b"a line of a log\n".repeat(32 * 1024);
        for _ in 0..2 {
            left.write_all(&text).await.unwrap();
            left.flush().await.unwrap();
            right.write_all(&text).await.unwrap();
            right.flush().await.unwrap();
            let mut received = vec![0; text.len()];
            left.read_exact(&mut received).await.unwrap();
            right.read_exact(&mut received).await.unwrap();
        }
        let bytes = left.memory();
        assert!(bytes < 2 * 1024 * 1024, "one end holds {bytes} bytes");
    }
}
