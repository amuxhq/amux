use std::collections::VecDeque;
use std::future::{self, poll_fn};
use std::io;
use std::pin::Pin;
use std::sync::Mutex;
use std::task::{Context, Poll};

use futures_util::FutureExt as _;
use futures_util::future::BoxFuture;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::sync::{mpsc, oneshot, watch};
use tokio_util::compat::{FuturesAsyncReadCompatExt, TokioAsyncReadCompatExt};
use wire::{self, pb};

use super::carrier::{
    AsyncStream, BoxIoFuture, ByteStream, CarrierKind, ControlSink, ControlSource, ControlWrite,
    LinkCarrier, OpenError,
};

const STREAM_ACCEPTED: u8 = 0;
const STREAM_REFUSED: u8 = 1;
const CONTROL_QUEUE_CAPACITY: usize = 32;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MuxRole {
    Connector,
    Acceptor,
}

#[derive(Debug)]
enum DriverOpenError {
    Closed,
    Io(String),
}

enum DriverCommand {
    Open(oneshot::Sender<Result<yamux::Stream, DriverOpenError>>),
    Close,
}

pub struct MuxCarrier {
    kind: CarrierKind,
    commands: mpsc::UnboundedSender<DriverCommand>,
    inbound: tokio::sync::Mutex<mpsc::UnboundedReceiver<(pb::StreamPreface, ByteStream)>>,
    control: Mutex<Option<(ControlSink, ControlSource)>>,
    control_ready: watch::Receiver<bool>,
    closed: watch::Sender<Option<pb::LinkCloseReason>>,
}

impl MuxCarrier {
    pub fn new<IO>(io: IO, role: MuxRole, kind: CarrierKind) -> Self
    where
        IO: AsyncRead + AsyncWrite + Send + Unpin + 'static,
    {
        let (commands, command_rx) = mpsc::unbounded_channel();
        let (inbound_tx, inbound) = mpsc::unbounded_channel();
        let (control_stream_tx, control_stream_rx) = oneshot::channel();
        let (closed, _) = watch::channel(None);
        let (control_ready_tx, control_ready) = watch::channel(false);

        let mut first_inbound = match role {
            MuxRole::Connector => {
                commands
                    .send(DriverCommand::Open(control_stream_tx))
                    .expect("new yamux driver must accept its control-stream command");
                None
            }
            MuxRole::Acceptor => Some(control_stream_tx),
        };
        let mode = match role {
            MuxRole::Connector => yamux::Mode::Client,
            MuxRole::Acceptor => yamux::Mode::Server,
        };
        let driver_closed = closed.clone();
        tokio::spawn(async move {
            drive_connection(
                yamux::Connection::new(Lingering::new(io).compat(), yamux::Config::default(), mode),
                command_rx,
                inbound_tx,
                &mut first_inbound,
                driver_closed,
            )
            .await;
        });

        let (control_write_tx, control_write_rx) = mpsc::channel(CONTROL_QUEUE_CAPACITY);
        let (control_read_tx, control_read_rx) = mpsc::channel(CONTROL_QUEUE_CAPACITY);
        tokio::spawn(run_control_stream(
            control_stream_rx,
            control_write_rx,
            control_read_tx,
            control_ready_tx,
            closed.subscribe(),
        ));

        Self {
            kind,
            commands,
            inbound: tokio::sync::Mutex::new(inbound),
            control: Mutex::new(Some((
                ControlSink {
                    tx: control_write_tx,
                },
                ControlSource {
                    rx: control_read_rx,
                },
            ))),
            control_ready,
            closed,
        }
    }

    async fn next_inbound(&self) -> Option<(pb::StreamPreface, ByteStream)> {
        self.inbound.lock().await.recv().await
    }

    async fn request_stream(&self) -> Result<yamux::Stream, OpenError> {
        let mut ready = self.control_ready.clone();
        while !*ready.borrow_and_update() {
            ready.changed().await.map_err(|_| OpenError::LinkClosed)?;
        }
        let (result, opened) = oneshot::channel();
        self.commands
            .send(DriverCommand::Open(result))
            .map_err(|_| OpenError::LinkClosed)?;
        match opened.await {
            Ok(Ok(stream)) => Ok(stream),
            Ok(Err(DriverOpenError::Closed)) | Err(_) => Err(OpenError::LinkClosed),
            Ok(Err(DriverOpenError::Io(error))) => Err(OpenError::Io(io::Error::other(error))),
        }
    }
}

impl LinkCarrier for MuxCarrier {
    fn kind(&self) -> CarrierKind {
        self.kind
    }

    fn control(&self) -> (ControlSink, ControlSource) {
        self.control
            .lock()
            .expect("control stream lock poisoned")
            .take()
            .expect("a link control stream can only be taken once")
    }

    fn open_stream(
        &self,
        preface: pb::StreamPreface,
    ) -> BoxFuture<'_, Result<ByteStream, OpenError>> {
        Box::pin(async move {
            let mut stream = self.request_stream().await?.compat();
            // yamux does not wake a stream waiting on its connection when
            // the connection closes: the answer is waited for only while
            // the link lasts.
            let answer = tokio::select! {
                answer = async {
                    write_proto(&mut stream, &preface, wire::MESSAGE_SIZE_LIMIT).await?;
                    stream.flush().await?;
                    match stream.read_u8().await? {
                        STREAM_ACCEPTED => Ok(Ok(())),
                        STREAM_REFUSED => {
                            let code = stream.read_u8().await?;
                            Ok(Err(pb::StreamRefusal::try_from(i32::from(code))
                                .unwrap_or(pb::StreamRefusal::Unspecified)))
                        }
                        other => Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!("invalid yamux stream-open status {other}"),
                        )),
                    }
                } => answer.map_err(OpenError::Io)?,
                _ = self.closed() => return Err(OpenError::LinkClosed),
            };
            answer.map_err(OpenError::Refused)?;
            Ok(Box::new(MuxByteStream::accepted(stream, self.closed.subscribe())) as ByteStream)
        })
    }

    fn accept_stream(&self) -> BoxFuture<'_, Option<(pb::StreamPreface, ByteStream)>> {
        Box::pin(self.next_inbound())
    }

    /// The driver sends what the streams still hold, the control
    /// stream's close saying why above all, and then ends once the peer has
    /// read it and closed its end (see [`Lingering`]). It is not cut short:
    /// a close dropped before the peer has read it loses the peer the reason
    /// it was told.
    fn close(&self, reason: pb::LinkCloseReason) {
        self.closed.send_replace(Some(reason));
        let _ = self.commands.send(DriverCommand::Close);
    }

    fn close_reason(&self) -> Option<pb::LinkCloseReason> {
        *self.closed.borrow()
    }

    fn closed(&self) -> BoxFuture<'_, pb::LinkCloseReason> {
        Box::pin(async move {
            let mut closed = self.closed.subscribe();
            loop {
                if let Some(reason) = *closed.borrow_and_update() {
                    return reason;
                }
                if closed.changed().await.is_err() {
                    return pb::LinkCloseReason::Unspecified;
                }
            }
        })
    }
}

enum DriverEvent {
    Inbound(yamux::Stream),
    Closed,
}

async fn drive_connection<IO>(
    mut connection: yamux::Connection<tokio_util::compat::Compat<IO>>,
    mut commands: mpsc::UnboundedReceiver<DriverCommand>,
    inbound: mpsc::UnboundedSender<(pb::StreamPreface, ByteStream)>,
    first_inbound: &mut Option<oneshot::Sender<Result<yamux::Stream, DriverOpenError>>>,
    closed: watch::Sender<Option<pb::LinkCloseReason>>,
) where
    IO: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    let mut pending_opens = VecDeque::new();
    let mut closing = false;

    loop {
        let event = poll_fn(|cx| {
            for _ in 0..64 {
                match commands.poll_recv(cx) {
                    Poll::Ready(Some(DriverCommand::Open(result))) if !closing => {
                        pending_opens.push_back(result);
                    }
                    Poll::Ready(Some(DriverCommand::Open(result))) => {
                        let _ = result.send(Err(DriverOpenError::Closed));
                    }
                    Poll::Ready(Some(DriverCommand::Close)) | Poll::Ready(None) => {
                        closing = true;
                        break;
                    }
                    Poll::Pending => break,
                }
            }

            if closing {
                return match connection.poll_close(cx) {
                    Poll::Ready(_) => Poll::Ready(DriverEvent::Closed),
                    Poll::Pending => Poll::Pending,
                };
            }

            while let Some(result) = pending_opens.pop_front() {
                match connection.poll_new_outbound(cx) {
                    Poll::Ready(Ok(stream)) => {
                        let _ = result.send(Ok(stream));
                    }
                    Poll::Ready(Err(yamux::ConnectionError::Closed)) => {
                        let _ = result.send(Err(DriverOpenError::Closed));
                    }
                    Poll::Ready(Err(error)) => {
                        let _ = result.send(Err(DriverOpenError::Io(error.to_string())));
                    }
                    Poll::Pending => {
                        pending_opens.push_front(result);
                        break;
                    }
                }
            }

            match connection.poll_next_inbound(cx) {
                Poll::Ready(Some(Ok(stream))) => Poll::Ready(DriverEvent::Inbound(stream)),
                Poll::Ready(Some(Err(_))) | Poll::Ready(None) => Poll::Ready(DriverEvent::Closed),
                Poll::Pending => Poll::Pending,
            }
        })
        .await;

        match event {
            DriverEvent::Inbound(stream) => {
                if let Some(control) = first_inbound.take() {
                    let _ = control.send(Ok(stream));
                } else {
                    let inbound = inbound.clone();
                    let ended = closed.subscribe();
                    tokio::spawn(async move {
                        prepare_inbound_stream(stream, inbound, ended).await;
                    });
                }
            }
            DriverEvent::Closed => break,
        }
    }

    for result in pending_opens {
        let _ = result.send(Err(DriverOpenError::Closed));
    }
    if let Some(control) = first_inbound.take() {
        let _ = control.send(Err(DriverOpenError::Closed));
    }
    if closed.borrow().is_none() {
        closed.send_replace(Some(pb::LinkCloseReason::Unspecified));
    }
}

async fn prepare_inbound_stream(
    stream: yamux::Stream,
    inbound: mpsc::UnboundedSender<(pb::StreamPreface, ByteStream)>,
    mut ended: watch::Receiver<Option<pb::LinkCloseReason>>,
) {
    let mut stream = stream.compat();
    let preface = tokio::select! {
        preface = read_proto::<_, pb::StreamPreface>(&mut stream, wire::MESSAGE_SIZE_LIMIT) => preface,
        _ = ended.wait_for(Option::is_some) => return,
    };
    match preface {
        Ok(Some(preface)) => {
            let stream = Box::new(MuxByteStream::pending(stream, ended)) as ByteStream;
            if let Err(error) = inbound.send((preface, stream)) {
                let (_, mut stream) = error.0;
                let _ = stream.reset(pb::StreamRefusal::ShuttingDown).await;
            }
        }
        Ok(None) | Err(_) => {
            let _ = refuse_stream(&mut stream, pb::StreamRefusal::NotAdjacent).await;
        }
    }
}

async fn run_control_stream(
    stream: oneshot::Receiver<Result<yamux::Stream, DriverOpenError>>,
    writes: mpsc::Receiver<ControlWrite>,
    reads: mpsc::Sender<io::Result<pb::Message>>,
    ready: watch::Sender<bool>,
    mut closed: watch::Receiver<Option<pb::LinkCloseReason>>,
) {
    let Ok(Ok(stream)) = stream.await else {
        return;
    };
    ready.send_replace(true);
    let (reader, writer) = tokio::io::split(stream.compat());
    let read = read_control_messages(reader, reads);
    let write = write_control_messages(writer, writes);
    tokio::pin!(read);
    tokio::pin!(write);
    tokio::select! {
        _ = &mut read => {}
        // A write fails once the connection has ended, which is also when
        // the peer's close may be waiting unread: it is handed over too.
        _ = &mut write => {
            let _ = read.as_mut().now_or_never();
        }
        _ = closed.wait_for(Option::is_some) => {
            // The connection has ended, and a yamux stream need not say
            // so to a reader waiting on it: hand over what already
            // arrived, the peer's close saying why above all, and end.
            let _ = read.as_mut().now_or_never();
        }
    }
}

/// A carrier's connection whose close waits for the peer to close too.
///
/// yamux sends what is pending before it reads: a peer with anything to
/// write (a window update, an RTT ping) whose write fails gives up on the
/// connection with frames still unread, and the close that told it why is
/// among them. Writes fail once this side has dropped its end, and without
/// this wrapper a close drops it as soon as its own frames are written, so
/// a peer slow to read, or with a write of its own in hand, lost the reason
/// and saw a connection that merely ended. So closing shuts down only this
/// side's writing, then reads and discards whatever still comes until the
/// peer, having read everything up to the close, ends its side.
struct Lingering<IO> {
    io: IO,
    write_shut: bool,
}

impl<IO> Lingering<IO> {
    fn new(io: IO) -> Self {
        Self {
            io,
            write_shut: false,
        }
    }
}

impl<IO: AsyncRead + Unpin> AsyncRead for Lingering<IO> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.io).poll_read(cx, buf)
    }
}

impl<IO: AsyncRead + AsyncWrite + Unpin> AsyncWrite for Lingering<IO> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.io).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.io).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = &mut *self;
        if !this.write_shut {
            std::task::ready!(Pin::new(&mut this.io).poll_shutdown(cx))?;
            this.write_shut = true;
        }
        let mut scratch = [0_u8; 4096];
        loop {
            let mut discarded = ReadBuf::new(&mut scratch);
            match std::task::ready!(Pin::new(&mut this.io).poll_read(cx, &mut discarded)) {
                Ok(()) if discarded.filled().is_empty() => return Poll::Ready(Ok(())),
                Ok(()) => {}
                // A peer whose end failed has closed it too.
                Err(_) => return Poll::Ready(Ok(())),
            }
        }
    }
}

async fn refuse_stream(
    stream: &mut tokio_util::compat::Compat<yamux::Stream>,
    reason: pb::StreamRefusal,
) -> io::Result<()> {
    stream.write_all(&[STREAM_REFUSED, reason as u8]).await?;
    stream.flush().await?;
    stream.shutdown().await
}

async fn read_control_messages<R>(mut reader: R, sender: mpsc::Sender<io::Result<pb::Message>>)
where
    R: AsyncRead + Unpin,
{
    loop {
        match read_proto(&mut reader, wire::MESSAGE_SIZE_LIMIT).await {
            Ok(Some(message)) => {
                if sender.send(Ok(message)).await.is_err() {
                    return;
                }
            }
            Ok(None) => return,
            Err(error) => {
                let _ = sender.send(Err(error)).await;
                future::pending::<()>().await;
            }
        }
    }
}

async fn write_control_messages<W>(mut writer: W, mut receiver: mpsc::Receiver<ControlWrite>)
where
    W: AsyncWrite + Unpin,
{
    while let Some(ControlWrite {
        bytes,
        declared_len,
        result,
    }) = receiver.recv().await
    {
        let write_result = write_frame(&mut writer, &bytes, declared_len).await;
        match write_result {
            Ok(()) => {
                let _ = result.send(Ok(()));
            }
            Err(error) => {
                let kind = error.kind();
                let message = error.to_string();
                let _ = result.send(Err(io::Error::new(kind, message.clone())));
                while let Ok(command) = receiver.try_recv() {
                    let _ = command
                        .result
                        .send(Err(io::Error::new(kind, message.clone())));
                }
                return;
            }
        }
    }
    let _ = writer.shutdown().await;
}

async fn write_frame<W>(writer: &mut W, bytes: &[u8], declared_len: Option<u32>) -> io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    let len = match declared_len {
        Some(len) => len,
        None => u32::try_from(bytes.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "frame is too large"))?,
    };
    writer.write_all(&len.to_be_bytes()).await?;
    writer.write_all(bytes).await?;
    writer.flush().await
}

async fn write_proto<W, M>(writer: &mut W, message: &M, limit: usize) -> io::Result<()>
where
    W: AsyncWrite + Unpin,
    M: prost::Message,
{
    let encoded_len = message.encoded_len();
    if encoded_len > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("protobuf frame is {encoded_len} bytes; limit is {limit}"),
        ));
    }
    let mut bytes = Vec::with_capacity(encoded_len);
    message
        .encode(&mut bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    write_frame(writer, &bytes, None).await
}

async fn read_proto<R, M>(reader: &mut R, limit: usize) -> io::Result<Option<M>>
where
    R: AsyncRead + Unpin,
    M: prost::Message + Default,
{
    let mut header = [0_u8; 4];
    match reader.read_exact(&mut header).await {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error),
    }
    let len = u32::from_be_bytes(header) as usize;
    if len > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("protobuf frame is {len} bytes; limit is {limit}"),
        ));
    }
    let mut bytes = vec![0; len];
    reader.read_exact(&mut bytes).await?;
    M::decode(bytes.as_slice())
        .map(Some)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Admission {
    Pending,
    Accepted,
    Closed,
}

struct MuxByteStream {
    stream: Option<tokio_util::compat::Compat<yamux::Stream>>,
    admission: Admission,
    accept_offset: usize,
    accepting_flush: bool,
    link_ended: LinkEnded,
}

impl MuxByteStream {
    fn pending(
        stream: tokio_util::compat::Compat<yamux::Stream>,
        closed: watch::Receiver<Option<pb::LinkCloseReason>>,
    ) -> Self {
        Self {
            stream: Some(stream),
            admission: Admission::Pending,
            accept_offset: 0,
            accepting_flush: false,
            link_ended: LinkEnded::new(closed),
        }
    }

    fn accepted(
        stream: tokio_util::compat::Compat<yamux::Stream>,
        closed: watch::Receiver<Option<pb::LinkCloseReason>>,
    ) -> Self {
        Self {
            stream: Some(stream),
            admission: Admission::Accepted,
            accept_offset: 0,
            accepting_flush: false,
            link_ended: LinkEnded::new(closed),
        }
    }

    /// A read or write the stream left waiting fails once the link has
    /// closed: yamux closes a connection without waking its streams, and
    /// no more data reaches them after it, so the wait would never end.
    fn unless_link_ended<T>(
        &mut self,
        cx: &mut Context<'_>,
        poll: Poll<io::Result<T>>,
    ) -> Poll<io::Result<T>> {
        match poll {
            Poll::Pending if self.link_ended.poll(cx) => Poll::Ready(Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "the link closed",
            ))),
            other => other,
        }
    }

    fn poll_accept(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        if self.admission != Admission::Pending {
            return Poll::Ready(Ok(()));
        }
        let stream = self.stream.as_mut().ok_or_else(stream_closed_error)?;
        if !self.accepting_flush {
            while self.accept_offset < 1 {
                let written = match Pin::new(&mut *stream)
                    .poll_write(cx, &[STREAM_ACCEPTED][self.accept_offset..])
                {
                    Poll::Ready(result) => result?,
                    Poll::Pending => return Poll::Pending,
                };
                if written == 0 {
                    return Poll::Ready(Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "could not accept yamux stream",
                    )));
                }
                self.accept_offset += written;
            }
            self.accepting_flush = true;
        }
        match Pin::new(stream).poll_flush(cx) {
            Poll::Ready(Ok(())) => {
                self.admission = Admission::Accepted;
                Poll::Ready(Ok(()))
            }
            other => other,
        }
    }
}

impl AsyncRead for MuxByteStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let accepted = self.poll_accept(cx);
        std::task::ready!(self.unless_link_ended(cx, accepted))?;
        let read = match self.stream.as_mut() {
            Some(stream) => Pin::new(stream).poll_read(cx, buf),
            None => return Poll::Ready(Ok(())),
        };
        self.unless_link_ended(cx, read)
    }
}

impl AsyncWrite for MuxByteStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let accepted = self.poll_accept(cx);
        std::task::ready!(self.unless_link_ended(cx, accepted))?;
        let written = match self.stream.as_mut() {
            Some(stream) => Pin::new(stream).poll_write(cx, buf),
            None => return Poll::Ready(Err(stream_closed_error())),
        };
        self.unless_link_ended(cx, written)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let accepted = self.poll_accept(cx);
        std::task::ready!(self.unless_link_ended(cx, accepted))?;
        let flushed = match self.stream.as_mut() {
            Some(stream) => Pin::new(stream).poll_flush(cx),
            None => return Poll::Ready(Err(stream_closed_error())),
        };
        self.unless_link_ended(cx, flushed)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let accepted = self.poll_accept(cx);
        std::task::ready!(self.unless_link_ended(cx, accepted))?;
        let shut = match self.stream.as_mut() {
            Some(stream) => Pin::new(stream).poll_shutdown(cx),
            None => return Poll::Ready(Ok(())),
        };
        self.unless_link_ended(cx, shut)
    }
}

impl AsyncStream for MuxByteStream {
    fn reset(&mut self, code: pb::StreamRefusal) -> BoxIoFuture<'_, ()> {
        Box::pin(async move {
            if self.admission == Admission::Pending
                && let Some(stream) = self.stream.as_mut()
            {
                stream.write_all(&[STREAM_REFUSED, code as u8]).await?;
                stream.flush().await?;
            }
            self.admission = Admission::Closed;
            self.stream.take();
            Ok(())
        })
    }
}

/// Whether a stream's link has closed, remembered once seen.
struct LinkEnded {
    wait: Option<BoxFuture<'static, ()>>,
}

impl LinkEnded {
    fn new(mut closed: watch::Receiver<Option<pb::LinkCloseReason>>) -> Self {
        Self {
            wait: Some(Box::pin(async move {
                let _ = closed.wait_for(Option::is_some).await;
            })),
        }
    }

    /// True once the link has closed; until then the task is woken when
    /// it does.
    fn poll(&mut self, cx: &mut Context<'_>) -> bool {
        match self.wait.as_mut() {
            None => true,
            Some(wait) => {
                let ended = wait.as_mut().poll(cx).is_ready();
                if ended {
                    self.wait = None;
                }
                ended
            }
        }
    }
}

fn stream_closed_error() -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, "yamux stream closed")
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use wire::pb::message;

    use super::*;
    use crate::link::carrier::{read_message, write_message};

    fn new_carriers() -> (Arc<MuxCarrier>, Arc<MuxCarrier>) {
        let (connector_io, acceptor_io) = tokio::io::duplex(2 * 1024 * 1024);
        (
            Arc::new(MuxCarrier::new(
                connector_io,
                MuxRole::Connector,
                CarrierKind::RelayTcp,
            )),
            Arc::new(MuxCarrier::new(
                acceptor_io,
                MuxRole::Acceptor,
                CarrierKind::RelayTcp,
            )),
        )
    }

    async fn carriers() -> (Arc<MuxCarrier>, Arc<MuxCarrier>) {
        let (connector, acceptor) = new_carriers();
        let (mut connector_sink, _connector_source) = connector.control();
        let (_acceptor_sink, mut acceptor_source) = acceptor.control();
        write_message(&mut connector_sink, &pb::Message { body: None })
            .await
            .unwrap();
        assert_eq!(
            read_message(&mut acceptor_source).await.unwrap(),
            Some(pb::Message { body: None })
        );
        (connector, acceptor)
    }

    fn preface(dst: u8) -> pb::StreamPreface {
        pb::StreamPreface {
            dst: vec![dst; 16],
            plain: false,
        }
    }

    async fn open_with_partial_preface(
        carrier: &MuxCarrier,
        preface: &pb::StreamPreface,
    ) -> (tokio_util::compat::Compat<yamux::Stream>, Vec<u8>) {
        let mut stream = carrier.request_stream().await.unwrap().compat();
        let bytes = prost::Message::encode_to_vec(preface);
        let header = u32::try_from(bytes.len()).unwrap().to_be_bytes();
        stream.write_all(&header[..2]).await.unwrap();
        stream.flush().await.unwrap();
        (stream, [header[2..].to_vec(), bytes].concat())
    }

    #[tokio::test]
    async fn both_sides_open_streams() {
        let (connector, acceptor) = carriers().await;
        let connector_open = {
            let connector = connector.clone();
            tokio::spawn(async move { connector.open_stream(preface(1)).await.unwrap() })
        };
        let (got, mut inbound) = acceptor.accept_stream().await.unwrap();
        assert_eq!(got, preface(1));
        let inbound_read = tokio::spawn(async move {
            let mut bytes = [0; 4];
            inbound.read_exact(&mut bytes).await.unwrap();
            bytes
        });
        let mut outbound = connector_open.await.unwrap();
        outbound.write_all(b"ping").await.unwrap();
        assert_eq!(inbound_read.await.unwrap(), *b"ping");

        let acceptor_open = {
            let acceptor = acceptor.clone();
            tokio::spawn(async move { acceptor.open_stream(preface(2)).await.unwrap() })
        };
        let (got, mut inbound) = connector.accept_stream().await.unwrap();
        assert_eq!(got, preface(2));
        let inbound_write = tokio::spawn(async move {
            inbound.write_all(b"pong").await.unwrap();
        });
        let mut outbound = acceptor_open.await.unwrap();
        let mut bytes = [0; 4];
        outbound.read_exact(&mut bytes).await.unwrap();
        inbound_write.await.unwrap();
        assert_eq!(bytes, *b"pong");
    }

    #[tokio::test]
    async fn refusal_code_survives_the_reset() {
        let (connector, acceptor) = carriers().await;
        let opening = tokio::spawn(async move { connector.open_stream(preface(3)).await });
        let (_, mut inbound) = acceptor.accept_stream().await.unwrap();
        inbound
            .reset(pb::StreamRefusal::PaymentRequired)
            .await
            .unwrap();
        assert!(matches!(
            opening.await.unwrap(),
            Err(OpenError::Refused(pb::StreamRefusal::PaymentRequired))
        ));
    }

    #[tokio::test]
    async fn finished_stream_reads_eof() {
        let (connector, acceptor) = carriers().await;
        let opening = tokio::spawn(async move { connector.open_stream(preface(4)).await.unwrap() });
        let (_, mut inbound) = acceptor.accept_stream().await.unwrap();
        inbound.shutdown().await.unwrap();
        let mut outbound = opening.await.unwrap();
        let mut byte = [0];
        assert_eq!(outbound.read(&mut byte).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn inbound_stream_survives_a_cancelled_accept_before_its_preface_arrives() {
        let (connector, acceptor) = carriers().await;
        let expected = preface(5);
        let (mut outbound, rest) = open_with_partial_preface(&connector, &expected).await;

        assert!(
            tokio::time::timeout(Duration::from_millis(50), acceptor.accept_stream())
                .await
                .is_err()
        );

        outbound.write_all(&rest).await.unwrap();
        outbound.flush().await.unwrap();
        let (received, mut inbound) =
            tokio::time::timeout(Duration::from_secs(1), acceptor.accept_stream())
                .await
                .expect("cancelled accept must not consume the pending stream")
                .unwrap();
        assert_eq!(received, expected);
        inbound.flush().await.unwrap();
    }

    #[tokio::test]
    async fn stream_withholding_its_preface_does_not_block_later_accepts() {
        let (connector, acceptor) = carriers().await;
        let blocked_preface = preface(6);
        let (mut blocked, rest) = open_with_partial_preface(&connector, &blocked_preface).await;

        let expected = preface(7);
        let opening = {
            let connector = connector.clone();
            let expected = expected.clone();
            tokio::spawn(async move { connector.open_stream(expected).await.unwrap() })
        };
        let (received, mut inbound) =
            tokio::time::timeout(Duration::from_secs(1), acceptor.accept_stream())
                .await
                .expect("later stream must overtake a stream withholding its preface")
                .unwrap();
        assert_eq!(received, expected);
        inbound.flush().await.unwrap();
        opening.await.unwrap();

        blocked.write_all(&rest).await.unwrap();
        blocked.flush().await.unwrap();
        let (received, mut inbound) =
            tokio::time::timeout(Duration::from_secs(1), acceptor.accept_stream())
                .await
                .expect("withheld stream must be delivered once its preface arrives")
                .unwrap();
        assert_eq!(received, blocked_preface);
        inbound.flush().await.unwrap();
    }

    #[tokio::test]
    async fn ten_concurrent_streams_do_not_block_behind_a_slow_reader() {
        let (connector, acceptor) = carriers().await;
        let slow_open = {
            let connector = connector.clone();
            tokio::spawn(async move { connector.open_stream(preface(0)).await.unwrap() })
        };
        let (_, mut slow_inbound) = acceptor.accept_stream().await.unwrap();
        let slow_accept = tokio::spawn(async move {
            slow_inbound.flush().await.unwrap();
            tokio::time::sleep(Duration::from_millis(250)).await;
            let mut payload = Vec::new();
            slow_inbound.read_to_end(&mut payload).await.unwrap();
            payload
        });
        let mut slow_outbound = slow_open.await.unwrap();
        let slow_payload = vec![7; 512 * 1024];
        let slow_write = tokio::spawn(async move {
            slow_outbound.write_all(&slow_payload).await.unwrap();
            slow_outbound.shutdown().await.unwrap();
        });

        let opens = (1..=10)
            .map(|id| {
                let connector = connector.clone();
                tokio::spawn(async move { connector.open_stream(preface(id)).await.unwrap() })
            })
            .collect::<Vec<_>>();
        let mut inbound = Vec::new();
        for id in 1..=10 {
            let (got, mut stream) = acceptor.accept_stream().await.unwrap();
            assert_eq!(got, preface(id));
            inbound.push(tokio::spawn(async move {
                stream.flush().await.unwrap();
                let mut byte = [0];
                stream.read_exact(&mut byte).await.unwrap();
                byte[0]
            }));
        }
        for (id, opened) in opens.into_iter().enumerate() {
            let mut stream = opened.await.unwrap();
            stream.write_all(&[id as u8 + 1]).await.unwrap();
        }
        for (id, received) in inbound.into_iter().enumerate() {
            assert_eq!(received.await.unwrap(), id as u8 + 1);
        }

        slow_write.await.unwrap();
        assert_eq!(slow_accept.await.unwrap().len(), 512 * 1024);
    }

    /// Test IO whose reads can be held back, and which says when its
    /// writing is shut down.
    struct Held {
        io: tokio::io::DuplexStream,
        hold: watch::Receiver<bool>,
        held: Option<Pin<Box<dyn Future<Output = ()> + Send>>>,
        shut: Option<oneshot::Sender<()>>,
    }

    impl AsyncRead for Held {
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            if *self.hold.borrow() {
                let mut hold = self.hold.clone();
                let held = self.held.get_or_insert_with(|| {
                    Box::pin(async move {
                        let _ = hold.wait_for(|held| !held).await;
                    })
                });
                std::task::ready!(held.as_mut().poll(cx));
            }
            self.held = None;
            Pin::new(&mut self.io).poll_read(cx, buf)
        }
    }

    impl AsyncWrite for Held {
        fn poll_write(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            Pin::new(&mut self.io).poll_write(cx, buf)
        }

        fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Pin::new(&mut self.io).poll_flush(cx)
        }

        fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            if let Some(shut) = self.shut.take() {
                let _ = shut.send(());
            }
            Pin::new(&mut self.io).poll_shutdown(cx)
        }
    }

    fn held(io: tokio::io::DuplexStream) -> (Held, watch::Sender<bool>, oneshot::Receiver<()>) {
        let (hold, receiver) = watch::channel(false);
        let (shut, shut_rx) = oneshot::channel();
        (
            Held {
                io,
                hold: receiver,
                held: None,
                shut: Some(shut),
            },
            hold,
            shut_rx,
        )
    }

    fn link_close(reason: pb::LinkCloseReason) -> pb::Message {
        pb::Message {
            body: Some(message::Body::LinkClose(pb::LinkClose {
                reason: reason as i32,
                error: None,
            })),
        }
    }

    /// The peer is told why the link closed however long it takes to read
    /// it, and even when it writes before it reads. yamux sends before it
    /// reads, so a peer whose write fails abandons what it has not read; the
    /// closing side must still be there to take that write.
    #[tokio::test]
    async fn a_close_reaches_a_peer_that_writes_before_it_reads() {
        let (closing_io, peer_io) = tokio::io::duplex(2 * 1024 * 1024);
        let (closing_io, _, closing_shut) = held(closing_io);
        let (peer_io, peer_reads, _) = held(peer_io);
        let closing = MuxCarrier::new(closing_io, MuxRole::Connector, CarrierKind::RelayTcp);
        let peer = MuxCarrier::new(peer_io, MuxRole::Acceptor, CarrierKind::RelayTcp);
        let (mut closing_sink, _closing_source) = closing.control();
        let (mut peer_sink, mut peer_source) = peer.control();
        write_message(&mut closing_sink, &pb::Message { body: None })
            .await
            .unwrap();
        assert_eq!(
            read_message(&mut peer_source).await.unwrap(),
            Some(pb::Message { body: None })
        );

        // The peer reads nothing while this side says why it closes, closes,
        // and has shut its writing down; then, longer than any fixed grace
        // for the close would allow, the peer writes before it reads again.
        peer_reads.send_replace(true);
        let revoked = link_close(pb::LinkCloseReason::UserRevoked);
        write_message(&mut closing_sink, &revoked).await.unwrap();
        closing.close(pb::LinkCloseReason::UserRevoked);
        closing_shut.await.unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        let _ = write_message(&mut peer_sink, &pb::Message { body: None }).await;
        tokio::task::yield_now().await;
        peer_reads.send_replace(false);

        assert_eq!(
            tokio::time::timeout(Duration::from_secs(5), read_message(&mut peer_source))
                .await
                .expect("the peer's control stream ends")
                .unwrap(),
            Some(revoked)
        );
    }

    /// A severed link: both ends close at once, the way a dead connection
    /// is cleaned up on each side.
    fn sever(connector: &MuxCarrier, acceptor: &MuxCarrier) {
        connector.close(pb::LinkCloseReason::Unspecified);
        acceptor.close(pb::LinkCloseReason::Unspecified);
    }

    /// yamux's close does not wake a stream's waiting reader: a call whose
    /// answer was on the way when the link went must still end, or its
    /// caller waits on the dead link forever.
    #[tokio::test]
    async fn reads_waiting_when_the_link_closes_end() {
        let (connector, acceptor) = carriers().await;
        let opening = {
            let connector = connector.clone();
            tokio::spawn(async move { connector.open_stream(preface(5)).await.unwrap() })
        };
        let (_, mut inbound) = acceptor.accept_stream().await.unwrap();
        inbound.write_all(b"hi").await.unwrap();
        let mut outbound = opening.await.unwrap();
        let mut greeting = [0; 2];
        outbound.read_exact(&mut greeting).await.unwrap();
        let outbound_read = tokio::spawn(async move {
            let mut byte = [0];
            outbound.read(&mut byte).await
        });
        let inbound_read = tokio::spawn(async move {
            let mut byte = [0];
            inbound.read(&mut byte).await
        });
        tokio::time::sleep(Duration::from_millis(50)).await;

        sever(&connector, &acceptor);
        for (side, read) in [("outbound", outbound_read), ("inbound", inbound_read)] {
            let read = tokio::time::timeout(Duration::from_secs(5), read)
                .await
                .unwrap_or_else(|_| panic!("the {side} read waits on a closed link"))
                .unwrap();
            assert!(!matches!(read, Ok(n) if n > 0), "{side} read {read:?}");
        }
    }

    /// An open waiting for the peer's answer when the link closes fails.
    #[tokio::test]
    async fn an_open_waiting_for_its_answer_when_the_link_closes_fails() {
        let (connector, acceptor) = carriers().await;
        let opening = {
            let connector = connector.clone();
            tokio::spawn(async move { connector.open_stream(preface(6)).await })
        };
        // Taken but not yet answered: the answer goes out on first use.
        let (_, _inbound) = acceptor.accept_stream().await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;

        sever(&connector, &acceptor);
        let opened = tokio::time::timeout(Duration::from_secs(5), opening)
            .await
            .expect("the open waits on a closed link")
            .unwrap();
        assert!(opened.is_err());
    }
}
