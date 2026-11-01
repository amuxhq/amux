//! QUIC packets over a byte stream, each behind a two-byte length: TLS on
//! TCP to the relay where UDP is blocked, the SSH session's stdio into
//! `link.sock`, and the in-process duplex the testnet declares links on.
//!
//! A byte stream is one more path of the [`LinkSocket`]: quinn sees the
//! same stand-in whichever path carries the link, so a link can start on
//! TCP and move to UDP without noticing.

use std::io;
use std::net::{Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader, BufWriter};
use tokio::sync::{mpsc, watch};

use super::socket::{Datagrams, FramedIo, LinkSocket, Path, PathId};
use crate::HostId;

/// The largest packet a frame holds: its length is two bytes.
pub const MAX_FRAME: usize = u16::MAX as usize;

/// How many packets may wait for the stream's writer; more are dropped, as
/// a full UDP buffer drops them, and QUIC sends them again.
const SEND_QUEUE: usize = 256;

/// How many bytes of frames a path's writer gathers before it writes them
/// to the stream, and its reader takes from the stream at once. One write
/// per packet made a TLS record and a system call of every 1,200-byte
/// packet; behind them the send queue filled and dropped packets.
const STREAM_BUFFER: usize = 64 * 1024;

/// What a byte-stream path runs over, for the link log and the route kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FramedKind {
    /// TLS on TCP to the relay, where UDP does not get through.
    RelayTcp,
    /// An SSH session's stdio into the far host's `link.sock`.
    Ssh,
    /// The duplex the testnet links two in-process hosts with.
    InProcess,
}

/// How long a closed connection may take to put its close on the stream
/// before the stream ends under it.
const CLOSE_DRAIN: Duration = Duration::from_secs(1);

/// A QUIC endpoint of its own whose one path is a byte stream, so a link
/// over TLS on TCP, SSH or the testnet's duplex is a QUIC connection like
/// any other, with the handshake that pins its peer.
pub(crate) struct StreamEndpoint {
    pub(crate) endpoint: quinn::Endpoint,
    /// Where a dialler sends: the far end of the stream.
    pub(crate) far: SocketAddr,
    path: FramedPath,
}

impl StreamEndpoint {
    /// An endpoint on `io`, answering connections when given a server
    /// configuration and dialling otherwise.
    pub(crate) fn new<IO>(
        io: IO,
        kind: FramedKind,
        server: Option<quinn::ServerConfig>,
    ) -> io::Result<Self>
    where
        IO: AsyncRead + AsyncWrite + Send + 'static,
    {
        // The stream has one far end, so one stand-in serves both ways:
        // whatever arrives on it comes from that end.
        let far_end = HostId::nil();
        let socket = LinkSocket::bind(Arc::new(StreamOnly), far_end)?;
        let path = FramedPath::attach(&socket, io, kind);
        socket.set_carrying(far_end, Path::Framed(path.id));
        let far = socket.stand_in(far_end);
        let runtime = quinn::default_runtime()
            .ok_or_else(|| io::Error::other("no async runtime for a QUIC endpoint"))?;
        let endpoint = quinn::Endpoint::new_with_abstract_socket(
            crate::identity::quic_endpoint_config(),
            server,
            socket,
            runtime,
        )?;
        Ok(Self {
            endpoint,
            far,
            path,
        })
    }

    pub(crate) fn kind(&self) -> FramedKind {
        self.path.kind
    }

    /// The first connection the far end opens; None if the stream ends
    /// first.
    pub(crate) async fn accept(&self) -> Option<quinn::Incoming> {
        tokio::select! {
            incoming = self.endpoint.accept() => incoming,
            () = self.path.closed() => None,
        }
    }

    /// Ties `connection` and the stream together: the stream ending closes
    /// the connection, and the connection closing ends the stream once its
    /// close has gone out.
    pub(crate) fn carry(self, connection: quinn::Connection) {
        let Self { endpoint, path, .. } = self;
        tokio::spawn(async move {
            tokio::select! {
                _ = connection.closed() => {
                    let _ = tokio::time::timeout(CLOSE_DRAIN, endpoint.wait_idle()).await;
                }
                () = path.closed() => {
                    connection.close(0_u32.into(), b"the byte stream ended");
                }
            }
            drop(path);
        });
    }
}

/// The substrate under a stream's endpoint: no datagrams, only the stream.
pub(crate) struct StreamOnly;

impl Datagrams for StreamOnly {
    fn send_to(&self, _: &[u8], _: SocketAddr) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::NotConnected,
            "a byte stream's endpoint sends only on the stream",
        ))
    }

    fn poll_recv(
        &self,
        _: &mut Context<'_>,
        _: &mut [u8],
    ) -> Poll<io::Result<(usize, SocketAddr)>> {
        Poll::Pending
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        Ok(SocketAddr::new(Ipv6Addr::UNSPECIFIED.into(), 0))
    }
}

/// A byte stream read and written a packet at a time.
pub struct Framed<IO> {
    io: IO,
}

impl<IO: AsyncRead + AsyncWrite + Unpin> Framed<IO> {
    pub fn new(io: IO) -> Self {
        Self { io }
    }

    pub async fn write_packet(&mut self, packet: &[u8]) -> io::Result<()> {
        write_packet(&mut self.io, packet).await?;
        self.io.flush().await
    }

    /// Reads the next packet into `buf`, returning its length; 0 when the
    /// stream ended between packets.
    pub async fn read_packet(&mut self, buf: &mut Vec<u8>) -> io::Result<usize> {
        read_packet(&mut self.io, buf).await
    }

    pub fn into_inner(self) -> IO {
        self.io
    }
}

async fn write_packet<W: AsyncWrite + Unpin>(io: &mut W, packet: &[u8]) -> io::Result<()> {
    if packet.is_empty() || packet.len() > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "a framed packet holds 1 to {MAX_FRAME} bytes, not {}",
                packet.len()
            ),
        ));
    }
    io.write_all(&(packet.len() as u16).to_be_bytes()).await?;
    io.write_all(packet).await
}

async fn read_packet<R: AsyncRead + Unpin>(io: &mut R, buf: &mut Vec<u8>) -> io::Result<usize> {
    let mut len = [0; 2];
    // A stream that ends before a length is a clean end; one that ends
    // inside a frame lost a packet.
    match io.read(&mut len[..1]).await? {
        0 => return Ok(0),
        _ => io.read_exact(&mut len[1..]).await?,
    };
    let len = u16::from_be_bytes(len) as usize;
    if len == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "an empty framed packet",
        ));
    }
    buf.resize(len, 0);
    io.read_exact(buf).await?;
    Ok(len)
}

/// A byte stream registered as a path of a [`LinkSocket`]. Closing or
/// dropping it, or the stream ending, removes the path.
pub struct FramedPath {
    pub id: PathId,
    pub kind: FramedKind,
    closed: watch::Receiver<bool>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
}

impl FramedPath {
    /// Registers `io` with `socket` and runs its reader and writer until the
    /// stream ends or the path is closed.
    pub fn attach<IO>(socket: &Arc<LinkSocket>, io: IO, kind: FramedKind) -> Self
    where
        IO: AsyncRead + AsyncWrite + Send + 'static,
    {
        let (queue, mut outgoing) = mpsc::channel::<Vec<u8>>(SEND_QUEUE);
        let id = socket.add_framed(Arc::new(Sender { queue, kind }));
        let (closed_tx, closed) = watch::channel(false);
        let (stop, mut stopped) = tokio::sync::oneshot::channel();
        let (reader, writer) = tokio::io::split(io);
        let mut reader = BufReader::with_capacity(STREAM_BUFFER, reader);
        let mut writer = BufWriter::with_capacity(STREAM_BUFFER, writer);
        let socket = Arc::downgrade(socket);
        tokio::spawn(async move {
            let write = async {
                while let Some(packet) = outgoing.recv().await {
                    if write_packet(&mut writer, &packet).await.is_err() {
                        return;
                    }
                    // Packets queued behind this one go out in one flush.
                    while let Ok(packet) = outgoing.try_recv() {
                        if write_packet(&mut writer, &packet).await.is_err() {
                            return;
                        }
                    }
                    if writer.flush().await.is_err() {
                        return;
                    }
                }
            };
            let read = async {
                let mut buf = Vec::new();
                loop {
                    match read_packet(&mut reader, &mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(_) => match socket.upgrade() {
                            Some(socket) => socket.deliver_framed(id, std::mem::take(&mut buf)),
                            None => return,
                        },
                    }
                }
            };
            let stopped = tokio::select! {
                () = write => false,
                () = read => false,
                _ = &mut stopped => true,
            };
            // A closed path still sends what was queued: a connection's
            // close is the last packet on the stream.
            if stopped {
                while let Ok(packet) = outgoing.try_recv() {
                    if write_packet(&mut writer, &packet).await.is_err() {
                        break;
                    }
                }
                let _ = writer.flush().await;
            }
            let _ = writer.shutdown().await;
            if let Some(socket) = socket.upgrade() {
                socket.remove_framed(id);
            }
            let _ = closed_tx.send(true);
        });
        Self {
            id,
            kind,
            closed,
            stop: Some(stop),
        }
    }

    /// Waits until the stream has ended and the path is gone.
    pub async fn closed(&self) {
        let mut closed = self.closed.clone();
        let _ = closed.wait_for(|closed| *closed).await;
    }

    pub fn close(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }

    /// Ties `connection`, carried on this path, and the stream together:
    /// the stream ending closes the connection, and the connection closing
    /// ends the stream once the far end has had the close.
    pub(crate) async fn carry(self, connection: quinn::Connection) {
        tokio::select! {
            _ = connection.closed() => {
                // The far end ends the stream once it has the close.
                let _ = tokio::time::timeout(CLOSE_DRAIN, self.closed()).await;
            }
            () = self.closed() => {
                connection.close(0_u32.into(), b"the byte stream ended");
            }
        }
    }
}

impl Drop for FramedPath {
    fn drop(&mut self) {
        self.close();
    }
}

struct Sender {
    queue: mpsc::Sender<Vec<u8>>,
    kind: FramedKind,
}

impl FramedIo for Sender {
    fn send(&self, packet: &[u8]) -> io::Result<()> {
        match self.queue.try_send(packet.to_vec()) {
            Ok(()) | Err(mpsc::error::TrySendError::Full(_)) => Ok(()),
            Err(mpsc::error::TrySendError::Closed(_)) => Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "the byte stream has ended",
            )),
        }
    }

    fn kind(&self) -> FramedKind {
        self.kind
    }
}

#[cfg(test)]
mod tests {
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use std::time::Duration;

    use tokio::io::ReadBuf;

    use super::*;

    /// Hands the reader at most `chunk` bytes per read.
    struct Trickle<R> {
        inner: R,
        chunk: usize,
    }

    impl<R: AsyncRead + Unpin> AsyncRead for Trickle<R> {
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            let chunk = self.chunk.min(buf.remaining());
            let mut small = vec![0; chunk];
            let mut small_buf = ReadBuf::new(&mut small);
            match Pin::new(&mut self.inner).poll_read(cx, &mut small_buf) {
                Poll::Ready(Ok(())) => {
                    buf.put_slice(small_buf.filled());
                    Poll::Ready(Ok(()))
                }
                other => other,
            }
        }
    }

    impl<R: AsyncWrite + Unpin> AsyncWrite for Trickle<R> {
        fn poll_write(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            Pin::new(&mut self.inner).poll_write(cx, buf)
        }

        fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Pin::new(&mut self.inner).poll_flush(cx)
        }

        fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Pin::new(&mut self.inner).poll_shutdown(cx)
        }
    }

    #[tokio::test]
    async fn framed_reads_a_packet_across_partial_reads() {
        let (near, far) = tokio::io::duplex(4096);
        let mut writer = Framed::new(near);
        let mut reader = Framed::new(Trickle {
            inner: far,
            chunk: 1,
        });
        let packet: Vec<u8> = (0..1200).map(|i| i as u8).collect();
        let reading = async {
            let mut buf = Vec::new();
            let mut got = Vec::new();
            for _ in 0..2 {
                let len = reader.read_packet(&mut buf).await.unwrap();
                got.push(buf[..len].to_vec());
            }
            got
        };
        let writing = async {
            writer.write_packet(&packet).await.unwrap();
            writer.write_packet(b"\x40second").await.unwrap();
        };
        let (got, ()) = tokio::join!(reading, writing);
        assert_eq!(got, vec![packet, b"\x40second".to_vec()]);
    }

    #[tokio::test]
    async fn framed_joins_a_packet_split_across_writes() {
        let (mut near, far) = tokio::io::duplex(4096);
        let mut reader = Framed::new(far);
        let packet = b"\x40split across writes";
        let mut frame = (packet.len() as u16).to_be_bytes().to_vec();
        frame.extend_from_slice(packet);
        let mut buf = Vec::new();
        // The length's first byte alone, then the rest of the length and
        // part of the packet, then the packet's end.
        let (a, rest) = frame.split_at(1);
        let (b, c) = rest.split_at(6);
        let reading = reader.read_packet(&mut buf);
        let writing = async {
            for part in [a, b, c] {
                near.write_all(part).await.unwrap();
                near.flush().await.unwrap();
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            near
        };
        let (len, near) = tokio::join!(reading, writing);
        assert_eq!(&buf[..len.unwrap()], packet);

        // A stream ending between packets is a clean end; inside one, an
        // error.
        drop(near);
        assert_eq!(reader.read_packet(&mut buf).await.unwrap(), 0);
        let (mut near, far) = tokio::io::duplex(64);
        near.write_all(&[0, 10, 0x40]).await.unwrap();
        drop(near);
        let error = Framed::new(far).read_packet(&mut buf).await.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[tokio::test]
    async fn framed_holds_packets_up_to_64_kb() {
        let (near, far) = tokio::io::duplex(1 << 18);
        let mut writer = Framed::new(near);
        let mut reader = Framed::new(far);
        let largest = vec![0x40; MAX_FRAME];
        writer.write_packet(&largest).await.unwrap();
        let mut buf = Vec::new();
        assert_eq!(reader.read_packet(&mut buf).await.unwrap(), MAX_FRAME);
        assert_eq!(buf, largest);

        for refused in [vec![0x40; MAX_FRAME + 1], Vec::new()] {
            let error = writer.write_packet(&refused).await.unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        }
        // Nothing of a refused packet reached the stream.
        writer.write_packet(b"\x40next").await.unwrap();
        let len = reader.read_packet(&mut buf).await.unwrap();
        assert_eq!(&buf[..len], b"\x40next");
    }

    /// Two link sockets joined by a duplex: a packet quinn sends to the
    /// stand-in crosses as a frame and arrives from the far stand-in, and
    /// the path reports its kind until the stream ends.
    #[tokio::test]
    async fn framed_path_carries_a_link_socket_peer() {
        use quinn::AsyncUdpSocket;

        use super::super::socket::{Datagrams, Path};
        use crate::HostId;

        struct Nowhere;
        impl Datagrams for Nowhere {
            fn send_to(&self, _: &[u8], _: std::net::SocketAddr) -> io::Result<()> {
                Ok(())
            }
            fn poll_recv(
                &self,
                _: &mut Context<'_>,
                _: &mut [u8],
            ) -> Poll<io::Result<(usize, std::net::SocketAddr)>> {
                Poll::Pending
            }
            fn local_addr(&self) -> io::Result<std::net::SocketAddr> {
                Ok("127.0.0.1:1".parse().unwrap())
            }
        }

        let (a_host, b_host) = (HostId::new_v4(), HostId::new_v4());
        let a = LinkSocket::bind(Arc::new(Nowhere), a_host).unwrap();
        let b = LinkSocket::bind(Arc::new(Nowhere), b_host).unwrap();
        let (a_io, b_io) = tokio::io::duplex(4096);
        let mut a_path = FramedPath::attach(&a, a_io, FramedKind::Ssh);
        let b_path = FramedPath::attach(&b, b_io, FramedKind::Ssh);
        assert_eq!(a.framed_kind(a_path.id), Some(FramedKind::Ssh));
        a.set_carrying(b_host, Path::Framed(a_path.id));

        let packet = b"\x40over the stream";
        a.try_send(&quinn::udp::Transmit {
            destination: a.stand_in(b_host),
            ecn: None,
            contents: packet,
            segment_size: None,
            src_ip: None,
        })
        .unwrap();
        let mut storage = [0u8; 2048];
        let (from, len) = tokio::time::timeout(
            Duration::from_secs(5),
            std::future::poll_fn(|cx| {
                let mut bufs = [std::io::IoSliceMut::new(&mut storage)];
                let mut meta = [quinn::udp::RecvMeta::default()];
                b.poll_recv(cx, &mut bufs, &mut meta)
                    .map_ok(|_| (meta[0].addr, meta[0].len))
            }),
        )
        .await
        .expect("the packet crosses")
        .unwrap();
        assert_eq!(&storage[..len], packet);
        // b has not been told who is on the stream: an unnamed sender,
        // answered on the stream.
        assert_eq!(b.peer_of(from), None);

        a_path.close();
        tokio::time::timeout(Duration::from_secs(5), b_path.closed())
            .await
            .expect("the far end sees the stream end");
        a_path.closed().await;
        assert_eq!(a.framed_kind(a_path.id), None);
        assert_eq!(b.framed_kind(b_path.id), None);
    }

    #[tokio::test]
    async fn framed_quic_link_reports_the_stream_it_runs_over() {
        use daemon_core::payload::CarrierKind;
        use tokio::io::AsyncReadExt as _;

        use crate::link::link_log::PathKind;

        for (kind, carrier_kind, path) in [
            (FramedKind::RelayTcp, CarrierKind::Relay, PathKind::RelayTcp),
            (FramedKind::Ssh, CarrierKind::Ssh, PathKind::Ssh),
            (FramedKind::InProcess, CarrierKind::Direct, PathKind::Lan),
        ] {
            let (connector, acceptor) = crate::link::quic::stream_pair(kind).await;
            assert_eq!(connector.kind(), carrier_kind);
            assert_eq!(acceptor.kind(), carrier_kind);
            assert_eq!(connector.path_kind(), path);
            assert_eq!(acceptor.path_kind(), path);

            let mut opened = connector.open_stream().await.unwrap();
            opened.write_all(b"across the stream").await.unwrap();
            let mut accepted =
                tokio::time::timeout(Duration::from_secs(5), acceptor.accept_stream())
                    .await
                    .expect("the stream arrives")
                    .unwrap();
            let mut read = [0; 17];
            accepted.read_exact(&mut read).await.unwrap();
            assert_eq!(&read, b"across the stream");

            connector.close(wire::LinkCloseReason::UserShutdown);
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(5), acceptor.closed())
                    .await
                    .expect("the far end hears the close"),
                wire::LinkCloseReason::UserShutdown
            );
        }
    }
}
