use std::net::{SocketAddr, UdpSocket};
use std::pin::Pin;
use std::sync::Mutex;
use std::task::{Context, Poll, ready};
use std::{future, io};

use futures_util::future::BoxFuture;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::{mpsc, oneshot, watch};
use wire::{self, pb};

use super::carrier::{
    AsyncStream, BoxIoFuture, ByteStream, CarrierKind, ControlSink, ControlSource, ControlWrite,
    LinkCarrier, OpenError,
};
use super::framed::{FramedKind, StreamEndpoint};
use crate::HostId;
use crate::identity::{DeviceIdentity, IdentityError};
use crate::trust::SharedTrustStore;

const STREAM_ACCEPTED: u8 = 0;
const CONTROL_QUEUE_CAPACITY: usize = 32;
pub(crate) const DEVICE_SERVER_NAME: &str = "amux-device.local";
// Quinn's first Initial PTO is about one second with its default initial RTT.
// Two seconds lets one lost handshake packet retransmit without allowing one
// black-holed DNS candidate to consume the cloud connection's whole dial.
/// How long a closing link waits for its last control messages, its
/// LinkClose among them, to be acknowledged before the connection closes
/// under them.
const CLOSE_LINGER: std::time::Duration = std::time::Duration::from_secs(1);
pub(crate) const RELAY_QUIC_HANDSHAKE_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(2);

#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    #[error("device identity configuration failed: {0}")]
    Identity(#[from] IdentityError),
    #[error("could not resolve {host}:{port}: {source}")]
    #[allow(dead_code)]
    Resolve {
        host: String,
        port: u16,
        #[source]
        source: io::Error,
    },
    #[error("{host}:{port} resolved to no addresses usable by this QUIC endpoint")]
    #[allow(dead_code)]
    NoAddress { host: String, port: u16 },
    #[error("could not inspect the QUIC endpoint's local address: {0}")]
    EndpointAddress(#[source] io::Error),
    #[error("QUIC connection to {host}:{port} failed for every usable address: {failures}")]
    Candidates {
        host: String,
        port: u16,
        failures: String,
    },
    #[error("the byte stream ended before a QUIC connection arrived on it")]
    StreamEnded,
    #[error("QUIC connection setup failed: {0}")]
    Setup(#[from] quinn::ConnectError),
    #[error("QUIC handshake failed: {0}")]
    Handshake(#[from] quinn::ConnectionError),
}

pub struct QuicCarrier {
    kind: CarrierKind,
    connection: quinn::Connection,
    inbound: tokio::sync::Mutex<mpsc::UnboundedReceiver<(pb::StreamPreface, ByteStream)>>,
    control: Mutex<Option<(ControlSink, ControlSource)>>,
    /// Asks the control writer to finish its stream, answering once the
    /// peer has acknowledged everything on it.
    finish_control: Mutex<Option<oneshot::Sender<oneshot::Sender<()>>>>,
    closed: watch::Sender<Option<pb::LinkCloseReason>>,
}

impl QuicCarrier {
    #[cfg(test)]
    pub async fn connect_direct(
        endpoint: &quinn::Endpoint,
        addr: SocketAddr,
        identity: &DeviceIdentity,
        trust_store: SharedTrustStore,
        peer: HostId,
    ) -> Result<Self, ConnectError> {
        Self::connect_direct_with_transport(endpoint, addr, identity, trust_store, peer, None).await
    }

    pub(crate) async fn connect_direct_with_transport(
        endpoint: &quinn::Endpoint,
        addr: SocketAddr,
        identity: &DeviceIdentity,
        trust_store: SharedTrustStore,
        peer: HostId,
        transport: Option<std::sync::Arc<quinn::TransportConfig>>,
    ) -> Result<Self, ConnectError> {
        let mut config = identity.quic_client_config_for_peer(trust_store, peer)?;
        if let Some(transport) = transport {
            config.transport_config(transport);
        }
        let dialled = std::time::Instant::now();
        let connection = endpoint
            .connect_with(config, addr, DEVICE_SERVER_NAME)?
            .await?;
        let stats = connection.stats();
        tracing::debug!(
            %addr,
            elapsed_ms = dialled.elapsed().as_millis(),
            rtt_ms = stats.path.rtt.as_millis(),
            sent = stats.path.sent_packets,
            lost = stats.path.lost_packets,
            "QUIC connection established"
        );
        let control = connection.open_bi().await?;
        Ok(Self::new(connection, CarrierKind::Quic, Some(control)))
    }

    #[allow(dead_code)]
    pub(crate) async fn connect_relay(
        endpoint: &quinn::Endpoint,
        host: &str,
        port: u16,
    ) -> Result<Self, ConnectError> {
        let addrs = tokio::net::lookup_host((host, port))
            .await
            .map_err(|source| ConnectError::Resolve {
                host: host.to_string(),
                port,
                source,
            })?;
        let config = crate::transport::relay_quic_client_config()
            .map_err(|error| IdentityError::TlsConfig(error.to_string()))?;
        Self::connect_relay_candidates_with_config(endpoint, addrs, host, port, config).await
    }

    pub(crate) async fn connect_relay_candidates_with_config(
        endpoint: &quinn::Endpoint,
        addrs: impl IntoIterator<Item = SocketAddr>,
        server_name: &str,
        port: u16,
        config: quinn::ClientConfig,
    ) -> Result<Self, ConnectError> {
        let local_addr = endpoint
            .local_addr()
            .map_err(ConnectError::EndpointAddress)?;
        let mut failures = Vec::new();
        let mut usable = false;

        for addr in addrs
            .into_iter()
            .filter(|addr| addr.is_ipv4() == local_addr.is_ipv4())
        {
            usable = true;
            match tokio::time::timeout(
                RELAY_QUIC_HANDSHAKE_TIMEOUT,
                Self::connect_relay_with_config(endpoint, addr, server_name, config.clone()),
            )
            .await
            {
                Ok(Ok(carrier)) => return Ok(carrier),
                Ok(Err(error)) => failures.push(format!("{addr}: {error}")),
                Err(_) => failures.push(format!("{addr}: handshake timed out")),
            }
        }

        if !usable {
            return Err(ConnectError::NoAddress {
                host: server_name.to_string(),
                port,
            });
        }
        Err(ConnectError::Candidates {
            host: server_name.to_string(),
            port,
            failures: failures.join("; "),
        })
    }

    pub(crate) async fn connect_relay_with_config(
        endpoint: &quinn::Endpoint,
        addr: SocketAddr,
        server_name: &str,
        config: quinn::ClientConfig,
    ) -> Result<Self, ConnectError> {
        let connection = endpoint.connect_with(config, addr, server_name)?.await?;
        let control = connection.open_bi().await?;
        Ok(Self::new(connection, CarrierKind::RelayQuic, Some(control)))
    }

    /// Dials the far end of a byte stream, the only connection on its
    /// endpoint.
    pub(crate) async fn connect_over_stream(
        stream: StreamEndpoint,
        config: quinn::ClientConfig,
        server_name: &str,
    ) -> Result<Self, ConnectError> {
        let connection = stream
            .endpoint
            .connect_with(config, stream.far, server_name)?
            .await?;
        let control = connection.open_bi().await?;
        let kind = stream_carrier_kind(stream.kind());
        stream.carry(connection.clone());
        Ok(Self::new(connection, kind, Some(control)))
    }

    /// Answers the connection the far end of a byte stream opens.
    pub(crate) async fn accept_over_stream(stream: StreamEndpoint) -> Result<Self, ConnectError> {
        let incoming = stream.accept().await.ok_or(ConnectError::StreamEnded)?;
        let connection = incoming.await?;
        let kind = stream_carrier_kind(stream.kind());
        stream.carry(connection.clone());
        Ok(Self::from_accepted_with_kind(connection, kind))
    }

    /// The trusted host whose key the handshake pinned, if the far end
    /// presented one the trust store knows.
    pub(crate) fn pinned_peer(&self, trust_store: &SharedTrustStore) -> Option<HostId> {
        let certificates = self
            .connection
            .peer_identity()?
            .downcast::<Vec<rustls::pki_types::CertificateDer<'static>>>()
            .ok()?;
        let trust_store = trust_store.read().ok()?;
        crate::identity::host_id_for_certificate(&trust_store, certificates.first()?)
            .ok()
            .flatten()
    }

    pub(crate) fn from_accepted(connection: quinn::Connection) -> Self {
        Self::from_accepted_with_kind(connection, CarrierKind::Quic)
    }

    pub(crate) fn from_accepted_with_kind(
        connection: quinn::Connection,
        kind: CarrierKind,
    ) -> Self {
        Self::new(connection, kind, None)
    }

    pub(crate) fn rebind(endpoint: &quinn::Endpoint, socket: UdpSocket) -> io::Result<()> {
        endpoint.rebind(socket)
    }

    fn new(
        connection: quinn::Connection,
        kind: CarrierKind,
        opened_control: Option<(quinn::SendStream, quinn::RecvStream)>,
    ) -> Self {
        let (inbound_tx, inbound) = mpsc::unbounded_channel();
        let (control_stream_tx, control_stream_rx) = oneshot::channel();
        let (closed, _) = watch::channel(None);
        let driver_connection = connection.clone();
        tokio::spawn(async move {
            let control = match opened_control {
                Some(control) => Ok(control),
                None => driver_connection.accept_bi().await,
            };
            if control_stream_tx.send(control).is_err() {
                return;
            }

            while let Ok((send, recv)) = driver_connection.accept_bi().await {
                let inbound = inbound_tx.clone();
                tokio::spawn(async move {
                    prepare_inbound_stream(send, recv, inbound).await;
                });
            }
        });

        let (control_write_tx, control_write_rx) = mpsc::channel(CONTROL_QUEUE_CAPACITY);
        let (control_read_tx, control_read_rx) = mpsc::channel(CONTROL_QUEUE_CAPACITY);
        let (finish_control, finish_rx) = oneshot::channel();
        tokio::spawn(run_control_stream(
            control_stream_rx,
            control_write_rx,
            control_read_tx,
            finish_rx,
        ));
        let monitor_connection = connection.clone();
        let monitor_closed = closed.clone();
        tokio::spawn(async move {
            let reason = close_reason_from_connection(monitor_connection.closed().await);
            if monitor_closed.borrow().is_none() {
                monitor_closed.send_replace(Some(reason));
            }
        });

        Self {
            kind,
            connection,
            inbound: tokio::sync::Mutex::new(inbound),
            control: Mutex::new(Some((
                ControlSink {
                    tx: control_write_tx,
                },
                ControlSource {
                    rx: control_read_rx,
                },
            ))),
            finish_control: Mutex::new(Some(finish_control)),
            closed,
        }
    }

    async fn next_inbound(&self) -> Option<(pb::StreamPreface, ByteStream)> {
        self.inbound.lock().await.recv().await
    }
}

/// Adapts one already-accepted QUIC bidirectional stream to Tokio IO without
/// the native-link stream admission marker. The direct pairing server uses
/// this for its single HTTP/2 connection after the QUIC handshake itself has
/// classified the peer as pre-trust pairing traffic.
pub(crate) fn accepted_quic_bidi_stream(
    send: quinn::SendStream,
    recv: quinn::RecvStream,
) -> ByteStream {
    Box::new(QuicByteStream::accepted(send, recv))
}

impl LinkCarrier for QuicCarrier {
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
            let (mut send, recv) = self
                .connection
                .open_bi()
                .await
                .map_err(map_connection_open_error)?;
            let preface_len = prost::Message::encoded_len(&preface);
            if preface_len > wire::MESSAGE_SIZE_LIMIT {
                return Err(OpenError::Io(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "stream preface is {preface_len} bytes; limit is {}",
                        wire::MESSAGE_SIZE_LIMIT
                    ),
                )));
            }
            write_proto(&mut send, &preface)
                .await
                .map_err(map_write_error)?;
            Ok(Box::new(QuicByteStream::opened(send, recv)) as ByteStream)
        })
    }

    fn accept_stream(&self) -> BoxFuture<'_, Option<(pb::StreamPreface, ByteStream)>> {
        Box::pin(self.next_inbound())
    }

    fn close(&self, reason: pb::LinkCloseReason) {
        self.closed.send_replace(Some(reason));
        let close = {
            let connection = self.connection.clone();
            move || {
                connection.close(
                    quinn::VarInt::from_u32(reason as u32),
                    reason.as_str_name().as_bytes(),
                )
            }
        };
        // Closing a QUIC connection drops what its streams have not yet
        // delivered, so the control stream is finished first and the
        // connection closed once the peer has it all, its LinkClose too.
        let finish = self
            .finish_control
            .lock()
            .expect("control finish lock poisoned")
            .take();
        match (finish, tokio::runtime::Handle::try_current()) {
            (Some(finish), Ok(runtime)) => {
                let (finished, delivered) = oneshot::channel();
                let _ = finish.send(finished);
                runtime.spawn(async move {
                    let _ = tokio::time::timeout(CLOSE_LINGER, delivered).await;
                    close();
                });
            }
            _ => close(),
        }
    }

    fn path_stats(&self) -> String {
        let stats = self.connection.stats();
        format!(
            "rtt={}ms min_rtt={}ms sent={} lost={} congestion_events={} cwnd={}",
            stats.path.rtt.as_millis(),
            stats.path.min_rtt.as_millis(),
            stats.path.sent_packets,
            stats.path.lost_packets,
            stats.path.congestion_events,
            stats.path.cwnd,
        )
    }

    fn close_reason(&self) -> Option<pb::LinkCloseReason> {
        let noted = *self.closed.borrow();
        noted.or_else(|| {
            self.connection
                .close_reason()
                .map(close_reason_from_connection)
        })
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

/// Two carriers joined by one QUIC connection over an in-memory pipe, as a
/// relay and a host on TCP are: the connector checks a self-signed
/// certificate, and the acceptor asks for none.
#[cfg(test)]
pub(crate) async fn stream_pair(
    kind: FramedKind,
) -> (std::sync::Arc<QuicCarrier>, std::sync::Arc<QuicCarrier>) {
    const NAME: &str = "relay.test";
    let rcgen::CertifiedKey { cert, signing_key } =
        rcgen::generate_simple_self_signed(vec![NAME.to_owned()]).expect("a certificate");
    let der = rustls::pki_types::CertificateDer::from(cert.der().to_vec());
    let server = crate::transport::relay_quic_server_config_from_der(
        vec![der.clone()],
        rustls::pki_types::PrivateKeyDer::Pkcs8(rustls::pki_types::PrivatePkcs8KeyDer::from(
            signing_key.serialize_der(),
        )),
    )
    .expect("a server configuration");
    let mut roots = rustls::RootCertStore::empty();
    roots.add(der).expect("the certificate as a root");
    let client = crate::transport::relay_quic_client_config_with_roots(roots)
        .expect("a client configuration");
    // A test's paused clock jumps past any idle timeout in one step.
    let mut transport = quinn::TransportConfig::default();
    transport.max_idle_timeout(None);
    let transport = std::sync::Arc::new(transport);
    let mut server = server;
    server.transport_config(transport.clone());
    let mut client = client;
    client.transport_config(transport);
    let (near, far) = tokio::io::duplex(2 * 1024 * 1024);
    let connecting = StreamEndpoint::new(near, kind, None).expect("an endpoint");
    let accepting = StreamEndpoint::new(far, kind, Some(server)).expect("an endpoint");
    let (connector, acceptor) = tokio::join!(
        QuicCarrier::connect_over_stream(connecting, client, NAME),
        QuicCarrier::accept_over_stream(accepting),
    );
    (
        std::sync::Arc::new(connector.expect("the connector's handshake")),
        std::sync::Arc::new(acceptor.expect("the acceptor's handshake")),
    )
}

/// What a link over a byte stream is to the rest of the host. An
/// in-process link stands for a direct one.
fn stream_carrier_kind(kind: FramedKind) -> CarrierKind {
    match kind {
        FramedKind::RelayTcp => CarrierKind::RelayTcp,
        FramedKind::Ssh => CarrierKind::Ssh,
        FramedKind::InProcess => CarrierKind::Quic,
    }
}

fn close_reason_from_connection(error: quinn::ConnectionError) -> pb::LinkCloseReason {
    match error {
        quinn::ConnectionError::ApplicationClosed(close) => {
            pb::LinkCloseReason::try_from(close.error_code.into_inner() as i32)
                .unwrap_or(pb::LinkCloseReason::Unspecified)
        }
        _ => pb::LinkCloseReason::Unspecified,
    }
}

async fn prepare_inbound_stream(
    mut send: quinn::SendStream,
    mut recv: quinn::RecvStream,
    inbound: mpsc::UnboundedSender<(pb::StreamPreface, ByteStream)>,
) {
    match read_proto::<pb::StreamPreface>(&mut recv, wire::MESSAGE_SIZE_LIMIT).await {
        Ok(Some(preface)) => {
            let stream = Box::new(QuicByteStream::pending(send, recv)) as ByteStream;
            if let Err(error) = inbound.send((preface, stream)) {
                let (_, mut stream) = error.0;
                let _ = stream.reset(pb::StreamRefusal::ShuttingDown).await;
            }
        }
        Ok(None) | Err(_) => {
            reset_stream_pair(&mut send, &mut recv, pb::StreamRefusal::NotAdjacent);
        }
    }
}

async fn run_control_stream(
    stream: oneshot::Receiver<
        Result<(quinn::SendStream, quinn::RecvStream), quinn::ConnectionError>,
    >,
    writes: mpsc::Receiver<ControlWrite>,
    reads: mpsc::Sender<io::Result<pb::Message>>,
    finish: oneshot::Receiver<oneshot::Sender<()>>,
) {
    let Ok(Ok((send, recv))) = stream.await else {
        return;
    };
    let read = read_control_messages(recv, reads);
    let write = write_control_messages(send, writes, finish);
    tokio::pin!(read);
    tokio::pin!(write);
    tokio::select! {
        _ = &mut read => {}
        _ = &mut write => {}
    }
}

async fn read_control_messages(
    mut recv: quinn::RecvStream,
    sender: mpsc::Sender<io::Result<pb::Message>>,
) {
    loop {
        match read_proto::<pb::Message>(&mut recv, wire::MESSAGE_SIZE_LIMIT).await {
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

async fn write_control_messages(
    mut send: quinn::SendStream,
    mut receiver: mpsc::Receiver<ControlWrite>,
    mut finish: oneshot::Receiver<oneshot::Sender<()>>,
) {
    let mut finished = None;
    let mut listening = true;
    loop {
        let write = tokio::select! {
            write = receiver.recv() => write,
            answer = &mut finish, if listening => {
                listening = false;
                // What was queued before the close still goes out.
                if let Ok(answer) = answer {
                    finished = Some(answer);
                    receiver.close();
                }
                continue;
            }
        };
        let Some(ControlWrite {
            bytes,
            declared_len,
            result,
        }) = write
        else {
            break;
        };
        let write_result = write_frame(&mut send, &bytes, declared_len)
            .await
            .map_err(io::Error::from);
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
    let _ = send.finish();
    if let Some(answer) = finished {
        let _ = send.stopped().await;
        let _ = answer.send(());
    }
}

async fn write_frame(
    send: &mut quinn::SendStream,
    bytes: &[u8],
    declared_len: Option<u32>,
) -> Result<(), quinn::WriteError> {
    let len = declared_len.unwrap_or_else(|| {
        u32::try_from(bytes.len()).expect("control message length was checked before queueing")
    });
    send.write_all(&len.to_be_bytes()).await?;
    send.write_all(bytes).await
}

async fn write_proto<M: prost::Message>(
    send: &mut quinn::SendStream,
    message: &M,
) -> Result<(), quinn::WriteError> {
    let encoded_len = message.encoded_len();
    let mut bytes = Vec::with_capacity(encoded_len);
    message
        .encode(&mut bytes)
        .expect("encoding into a Vec cannot fail");
    write_frame(send, &bytes, None).await
}

async fn read_proto<M: prost::Message + Default>(
    recv: &mut quinn::RecvStream,
    limit: usize,
) -> io::Result<Option<M>> {
    let mut header = [0_u8; 4];
    match recv.read_exact(&mut header).await {
        Ok(()) => {}
        Err(quinn::ReadExactError::FinishedEarly(0)) => return Ok(None),
        Err(error) => return Err(io::Error::new(io::ErrorKind::UnexpectedEof, error)),
    }
    let len = u32::from_be_bytes(header) as usize;
    if len > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("protobuf frame is {len} bytes; limit is {limit}"),
        ));
    }
    let mut bytes = vec![0; len];
    recv.read_exact(&mut bytes)
        .await
        .map_err(|error| io::Error::new(io::ErrorKind::UnexpectedEof, error))?;
    M::decode(bytes.as_slice())
        .map(Some)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn map_connection_open_error(error: quinn::ConnectionError) -> OpenError {
    match error {
        quinn::ConnectionError::LocallyClosed
        | quinn::ConnectionError::ApplicationClosed(_)
        | quinn::ConnectionError::ConnectionClosed(_) => OpenError::LinkClosed,
        error => OpenError::Io(io::Error::from(error)),
    }
}

fn map_write_error(error: quinn::WriteError) -> OpenError {
    match error {
        quinn::WriteError::Stopped(code) => OpenError::Refused(refusal_from_varint(code)),
        quinn::WriteError::ConnectionLost(_) | quinn::WriteError::ClosedStream => {
            OpenError::LinkClosed
        }
        error => OpenError::Io(io::Error::from(error)),
    }
}

/// What a read that found no acceptance byte says about the open.
fn map_read_error(error: io::Error) -> OpenError {
    match error
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<quinn::ReadError>())
    {
        Some(quinn::ReadError::Reset(code)) => OpenError::Refused(refusal_from_varint(*code)),
        Some(quinn::ReadError::ConnectionLost(_)) => OpenError::LinkClosed,
        _ => OpenError::Io(error),
    }
}

fn refusal_from_varint(code: quinn::VarInt) -> pb::StreamRefusal {
    pb::StreamRefusal::try_from(code.into_inner() as i32).unwrap_or(pb::StreamRefusal::Unspecified)
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Admission {
    /// Opened to this end: the acceptance byte goes out ahead of its first
    /// read or write.
    Pending,
    /// Opened by this end: the far end's answer is read ahead of its first
    /// read, while writes go out at once behind the preface.
    Opened,
    Accepted,
    Closed,
}

struct QuicByteStream {
    send: quinn::SendStream,
    recv: quinn::RecvStream,
    admission: Admission,
    accept_offset: usize,
}

impl QuicByteStream {
    fn pending(send: quinn::SendStream, recv: quinn::RecvStream) -> Self {
        Self::with_admission(send, recv, Admission::Pending)
    }

    fn opened(send: quinn::SendStream, recv: quinn::RecvStream) -> Self {
        Self::with_admission(send, recv, Admission::Opened)
    }

    fn accepted(send: quinn::SendStream, recv: quinn::RecvStream) -> Self {
        Self::with_admission(send, recv, Admission::Accepted)
    }

    fn with_admission(
        send: quinn::SendStream,
        recv: quinn::RecvStream,
        admission: Admission,
    ) -> Self {
        Self {
            send,
            recv,
            admission,
            accept_offset: 0,
        }
    }

    /// The far end's answer to a stream this end opened: nothing to read
    /// until it has accepted, and its refusal is the reason it was reset
    /// with.
    fn poll_answer(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), OpenError>> {
        if self.admission != Admission::Opened {
            return Poll::Ready(Ok(()));
        }
        let mut status = [0_u8; 1];
        let mut buf = ReadBuf::new(&mut status);
        let answer = match ready!(AsyncRead::poll_read(Pin::new(&mut self.recv), cx, &mut buf)) {
            Ok(()) if buf.filled().is_empty() => Err(OpenError::Io(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "QUIC stream ended before it was accepted",
            ))),
            Ok(()) if status[0] == STREAM_ACCEPTED => Ok(()),
            Ok(()) => Err(OpenError::Io(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid QUIC stream-open status {}", status[0]),
            ))),
            Err(error) => Err(map_read_error(error)),
        };
        self.admission = match answer {
            Ok(()) => Admission::Accepted,
            Err(_) => Admission::Closed,
        };
        Poll::Ready(answer)
    }

    fn poll_accept(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        if self.admission != Admission::Pending {
            return Poll::Ready(Ok(()));
        }
        while self.accept_offset < 1 {
            let written = match AsyncWrite::poll_write(
                Pin::new(&mut self.send),
                cx,
                &[STREAM_ACCEPTED][self.accept_offset..],
            ) {
                Poll::Ready(result) => result?,
                Poll::Pending => return Poll::Pending,
            };
            if written == 0 {
                return Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "could not accept QUIC stream",
                )));
            }
            self.accept_offset += written;
        }
        self.admission = Admission::Accepted;
        Poll::Ready(Ok(()))
    }
}

impl AsyncRead for QuicByteStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if let Err(error) = ready!(self.poll_answer(cx)) {
            return Poll::Ready(Err(error.into_io()));
        }
        match self.poll_accept(cx) {
            Poll::Ready(Ok(())) => AsyncRead::poll_read(Pin::new(&mut self.recv), cx, buf),
            Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl AsyncWrite for QuicByteStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.poll_accept(cx) {
            Poll::Ready(Ok(())) => AsyncWrite::poll_write(Pin::new(&mut self.send), cx, buf),
            Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
            Poll::Pending => Poll::Pending,
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.poll_accept(cx) {
            Poll::Ready(Ok(())) => AsyncWrite::poll_flush(Pin::new(&mut self.send), cx),
            Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
            Poll::Pending => Poll::Pending,
        }
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.poll_accept(cx) {
            Poll::Ready(Ok(())) => {
                self.admission = Admission::Closed;
                AsyncWrite::poll_shutdown(Pin::new(&mut self.send), cx)
            }
            Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl AsyncStream for QuicByteStream {
    fn reset(&mut self, code: pb::StreamRefusal) -> BoxIoFuture<'_, ()> {
        reset_stream_pair(&mut self.send, &mut self.recv, code);
        self.admission = Admission::Closed;
        Box::pin(future::ready(Ok(())))
    }

    fn accepted(&mut self) -> BoxFuture<'_, Result<(), OpenError>> {
        Box::pin(future::poll_fn(move |cx| self.poll_answer(cx)))
    }
}

impl Drop for QuicByteStream {
    fn drop(&mut self) {
        if self.admission == Admission::Pending {
            reset_stream_pair(
                &mut self.send,
                &mut self.recv,
                pb::StreamRefusal::ShuttingDown,
            );
        }
    }
}

fn reset_stream_pair(
    send: &mut quinn::SendStream,
    recv: &mut quinn::RecvStream,
    code: pb::StreamRefusal,
) {
    let code = quinn::VarInt::from_u32(code as u32);
    let _ = send.reset(code);
    let _ = recv.stop(code);
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use chrono::Utc;
    use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
    use rustls::client::{
        ClientSessionMemoryCache, ClientSessionStore, Resumption, Tls12ClientSessionValue,
        Tls13ClientSessionValue,
    };
    use rustls::crypto::{
        WebPkiSupportedAlgorithms, verify_tls12_signature, verify_tls13_signature,
    };
    use rustls::pki_types::{
        CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime,
    };
    use rustls::{CertificateError, DigitallySignedStruct, NamedGroup, SignatureScheme};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;
    use crate::identity::{QUIC_ALPN, ed25519_public_key_from_certificate, quic_transport_config};
    use crate::trust::{TrustStore, TrustStorePairingUpdate};

    struct LoopbackPair {
        _client_endpoint: quinn::Endpoint,
        _server_endpoint: quinn::Endpoint,
        client: Arc<QuicCarrier>,
        server: Arc<QuicCarrier>,
    }

    async fn loopback_pair() -> LoopbackPair {
        let client_identity = DeviceIdentity::for_test(HostId::new_v4());
        let server_identity = DeviceIdentity::for_test(HostId::new_v4());
        let client_trust = trust_for(&server_identity);
        let server_trust = trust_for(&client_identity);
        let server_config = server_identity.quic_server_config(server_trust).unwrap();
        let server_endpoint =
            crate::identity::quic_endpoint(Some(server_config), "127.0.0.1:0".parse().unwrap())
                .unwrap();
        let client_endpoint =
            crate::identity::quic_endpoint(None, "127.0.0.1:0".parse().unwrap()).unwrap();
        let server_addr = server_endpoint.local_addr().unwrap();

        let accept = async {
            server_endpoint
                .accept()
                .await
                .expect("server endpoint closed")
                .await
                .expect("server handshake failed")
        };
        let connect = QuicCarrier::connect_direct(
            &client_endpoint,
            server_addr,
            &client_identity,
            client_trust,
            server_identity.host_id,
        );
        let (server_connection, client) = tokio::join!(accept, connect);
        let client = Arc::new(client.unwrap());
        let server = Arc::new(QuicCarrier::from_accepted(server_connection));

        // Writing one frame makes the connector-opened control stream visible
        // before any application stream is opened.
        let (mut client_control, _client_source) = client.control();
        super::super::carrier::write_message(&mut client_control, &pb::Message { body: None })
            .await
            .unwrap();

        LoopbackPair {
            _client_endpoint: client_endpoint,
            _server_endpoint: server_endpoint,
            client,
            server,
        }
    }

    fn trust_for(peer: &DeviceIdentity) -> SharedTrustStore {
        let mut trust = TrustStore::default();
        assert_eq!(
            trust
                .upsert_paired_peer(
                    peer.host_id,
                    peer.public_key().to_vec(),
                    "peer".to_string(),
                    None,
                    Utc::now(),
                )
                .unwrap(),
            TrustStorePairingUpdate::Inserted
        );
        Arc::new(std::sync::RwLock::new(trust))
    }

    fn relay_test_configs() -> (quinn::ServerConfig, quinn::ClientConfig) {
        let client_identity = DeviceIdentity::for_test(HostId::new_v4());
        let server_identity = DeviceIdentity::for_test(HostId::new_v4());
        let server_config = server_identity
            .quic_server_config(trust_for(&client_identity))
            .unwrap();
        let client_config = client_identity
            .quic_client_config_for_peer(trust_for(&server_identity), server_identity.host_id)
            .unwrap();
        (server_config, client_config)
    }

    #[tokio::test]
    async fn relay_dial_skips_an_ipv6_result_for_an_ipv4_endpoint() {
        let (server_config, client_config) = relay_test_configs();
        let server_endpoint =
            crate::identity::quic_endpoint(Some(server_config), "127.0.0.1:0".parse().unwrap())
                .unwrap();
        let server_addr = server_endpoint.local_addr().unwrap();
        let client_endpoint =
            crate::identity::quic_endpoint(None, "0.0.0.0:0".parse().unwrap()).unwrap();
        let ipv6_first = SocketAddr::new("::1".parse().unwrap(), server_addr.port());

        let accept = async {
            server_endpoint
                .accept()
                .await
                .expect("server endpoint closed")
                .await
                .expect("server handshake failed")
        };
        let connect = QuicCarrier::connect_relay_candidates_with_config(
            &client_endpoint,
            [ipv6_first, server_addr],
            DEVICE_SERVER_NAME,
            server_addr.port(),
            client_config,
        );
        let (server_connection, client) = tokio::join!(accept, connect);

        client
            .expect("IPv4 relay candidate was not tried")
            .close(pb::LinkCloseReason::UserShutdown);
        server_connection.close(0_u32.into(), b"test complete");
    }

    #[tokio::test]
    async fn relay_dial_falls_through_a_bound_unresponsive_candidate() {
        let (server_config, client_config) = relay_test_configs();
        let server_endpoint =
            crate::identity::quic_endpoint(Some(server_config), "127.0.0.1:0".parse().unwrap())
                .unwrap();
        let server_addr = server_endpoint.local_addr().unwrap();
        let client_endpoint =
            crate::identity::quic_endpoint(None, "0.0.0.0:0".parse().unwrap()).unwrap();
        let blackhole = UdpSocket::bind("127.0.0.1:0").unwrap();
        let blackhole_addr = blackhole.local_addr().unwrap();
        let started = Instant::now();

        let accept = async {
            server_endpoint
                .accept()
                .await
                .expect("server endpoint closed")
                .await
                .expect("server handshake failed")
        };
        let connect = tokio::time::timeout(
            RELAY_QUIC_HANDSHAKE_TIMEOUT + Duration::from_secs(1),
            QuicCarrier::connect_relay_candidates_with_config(
                &client_endpoint,
                [blackhole_addr, server_addr],
                DEVICE_SERVER_NAME,
                server_addr.port(),
                client_config,
            ),
        );
        let (server_connection, client) = tokio::join!(accept, connect);

        let client = client
            .expect("relay candidates exceeded their bounded dial budget")
            .expect("working relay candidate was not tried");
        assert!(started.elapsed() >= RELAY_QUIC_HANDSHAKE_TIMEOUT);
        client.close(pb::LinkCloseReason::UserShutdown);
        server_connection.close(0_u32.into(), b"test complete");
        drop(blackhole);
    }

    fn preface(byte: u8) -> pb::StreamPreface {
        pb::StreamPreface {
            dst: vec![byte],
            plain: false,
        }
    }

    #[tokio::test]
    async fn refusal_code_survives_a_reset() {
        let pair = loopback_pair().await;
        let client = pair.client.clone();
        let opening = tokio::spawn(async move {
            let mut stream = client.open_stream(preface(1)).await?;
            stream.accepted().await
        });
        let (_, mut inbound) = pair.server.accept_stream().await.unwrap();
        inbound
            .reset(pb::StreamRefusal::PaymentRequired)
            .await
            .unwrap();

        assert!(matches!(
            opening.await.unwrap(),
            Err(OpenError::Refused(pb::StreamRefusal::PaymentRequired))
        ));
    }

    /// An opener that does not wait sends behind its preface at once; a
    /// refusal then comes out of its first read, with its reason.
    #[tokio::test]
    async fn an_unwaited_stream_sends_at_once_and_reads_its_refusal() {
        let pair = loopback_pair().await;
        let mut stream = pair.client.open_stream(preface(1)).await.unwrap();
        stream.write_all(b"hello").await.unwrap();
        let (got, mut inbound) = pair.server.accept_stream().await.unwrap();
        assert_eq!(got, preface(1));
        let mut greeting = [0_u8; 5];
        inbound.read_exact(&mut greeting).await.unwrap();
        assert_eq!(&greeting, b"hello");

        inbound
            .reset(pb::StreamRefusal::ShuttingDown)
            .await
            .unwrap();
        let mut answer = [0_u8; 1];
        let error = stream.read_exact(&mut answer).await.unwrap_err();
        assert_eq!(
            OpenError::refusal(&error),
            Some(pb::StreamRefusal::ShuttingDown)
        );
    }

    #[tokio::test]
    async fn two_streams_do_not_block_each_other_under_a_slow_reader() {
        let pair = loopback_pair().await;
        let client = pair.client.clone();
        let first_open = tokio::spawn(async move { client.open_stream(preface(1)).await.unwrap() });
        let (_, mut slow_reader) = pair.server.accept_stream().await.unwrap();
        slow_reader.flush().await.unwrap();
        let mut first = first_open.await.unwrap();

        let blocked_writer = tokio::spawn(async move {
            let payload = vec![7_u8; 8 * 1024 * 1024];
            first.write_all(&payload).await
        });

        let client = pair.client.clone();
        let second_open =
            tokio::spawn(async move { client.open_stream(preface(2)).await.unwrap() });
        let (got, mut fast_stream) =
            tokio::time::timeout(Duration::from_secs(1), pair.server.accept_stream())
                .await
                .expect("second stream was blocked by the unread first stream")
                .unwrap();
        assert_eq!(got, preface(2));
        fast_stream.write_all(b"ok").await.unwrap();
        let mut second = second_open.await.unwrap();
        let mut response = [0_u8; 2];
        tokio::time::timeout(Duration::from_secs(1), second.read_exact(&mut response))
            .await
            .expect("second stream response was blocked by the unread first stream")
            .unwrap();
        assert_eq!(&response, b"ok");

        slow_reader
            .reset(pb::StreamRefusal::ShuttingDown)
            .await
            .unwrap();
        assert!(blocked_writer.await.unwrap().is_err());
    }

    #[tokio::test]
    async fn a_client_offered_session_ticket_is_not_honoured() {
        let client_identity = DeviceIdentity::for_test(HostId::new_v4());
        let server_identity = DeviceIdentity::for_test(HostId::new_v4());
        let server_trust = trust_for(&client_identity);

        let mut ticket_server_tls = server_identity
            .server_tls_config(server_trust.clone())
            .unwrap();
        ticket_server_tls.alpn_protocols = vec![QUIC_ALPN.to_vec()];
        ticket_server_tls.session_storage = rustls::server::ServerSessionMemoryCache::new(32);
        ticket_server_tls.send_tls13_tickets = 2;
        ticket_server_tls.max_early_data_size = 0;
        let ticket_crypto =
            quinn::crypto::rustls::QuicServerConfig::try_from(ticket_server_tls).unwrap();
        let mut ticket_server_config = quinn::ServerConfig::with_crypto(Arc::new(ticket_crypto));
        ticket_server_config.transport_config(quic_transport_config());
        ticket_server_config.migration(true);

        let server_endpoint = crate::identity::quic_endpoint(
            Some(ticket_server_config),
            "127.0.0.1:0".parse().unwrap(),
        )
        .unwrap();
        let server_addr = server_endpoint.local_addr().unwrap();

        let full_handshakes = Arc::new(AtomicUsize::new(0));
        let verifier = Arc::new(CountingPinnedVerifier::new(
            server_identity.public_key().to_vec(),
            full_handshakes.clone(),
        ));
        let sessions = Arc::new(TrackingSessionStore::default());
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
            client_identity.private_key_pkcs8().to_vec(),
        ));
        let mut client_tls =
            rustls::ClientConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
                .dangerous()
                .with_custom_certificate_verifier(verifier)
                .with_client_auth_cert(
                    vec![CertificateDer::from(
                        client_identity.certificate_der().unwrap(),
                    )],
                    key,
                )
                .unwrap();
        client_tls.alpn_protocols = vec![QUIC_ALPN.to_vec()];
        client_tls.resumption = Resumption::store(sessions.clone());
        client_tls.enable_early_data = false;
        let client_crypto = quinn::crypto::rustls::QuicClientConfig::try_from(client_tls).unwrap();
        let mut client_config = quinn::ClientConfig::new(Arc::new(client_crypto));
        client_config.transport_config(quic_transport_config());
        let mut client_endpoint =
            crate::identity::quic_endpoint(None, "127.0.0.1:0".parse().unwrap()).unwrap();
        client_endpoint.set_default_client_config(client_config);

        let (first_server, first_client) =
            connect_raw(&server_endpoint, &client_endpoint, server_addr).await;
        tokio::time::timeout(Duration::from_secs(1), async {
            while sessions.inserted.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("ticket-issuing server did not provide a session ticket");
        first_client.close(0_u32.into(), b"test complete");
        first_server.close(0_u32.into(), b"test complete");

        server_endpoint.set_server_config(Some(
            server_identity.quic_server_config(server_trust).unwrap(),
        ));
        let (second_server, second_client) =
            connect_raw(&server_endpoint, &client_endpoint, server_addr).await;

        assert!(
            sessions.taken.load(Ordering::SeqCst) > 0,
            "the client must actually offer its cached ticket"
        );
        assert_eq!(
            full_handshakes.load(Ordering::SeqCst),
            2,
            "the ticket-disabled server must force a full certificate handshake"
        );
        second_client.close(0_u32.into(), b"test complete");
        second_server.close(0_u32.into(), b"test complete");
    }

    async fn connect_raw(
        server: &quinn::Endpoint,
        client: &quinn::Endpoint,
        addr: SocketAddr,
    ) -> (quinn::Connection, quinn::Connection) {
        let accept = async {
            server
                .accept()
                .await
                .expect("server endpoint closed")
                .await
                .expect("server handshake failed")
        };
        let connect = async {
            client
                .connect(addr, DEVICE_SERVER_NAME)
                .unwrap()
                .await
                .expect("client handshake failed")
        };
        tokio::join!(accept, connect)
    }

    #[derive(Debug)]
    struct CountingPinnedVerifier {
        expected_pubkey: Vec<u8>,
        full_handshakes: Arc<AtomicUsize>,
        supported_algs: WebPkiSupportedAlgorithms,
    }

    impl CountingPinnedVerifier {
        fn new(expected_pubkey: Vec<u8>, full_handshakes: Arc<AtomicUsize>) -> Self {
            Self {
                expected_pubkey,
                full_handshakes,
                supported_algs: rustls::crypto::ring::default_provider()
                    .signature_verification_algorithms,
            }
        }
    }

    impl ServerCertVerifier for CountingPinnedVerifier {
        fn verify_server_cert(
            &self,
            end_entity: &CertificateDer<'_>,
            _intermediates: &[CertificateDer<'_>],
            _server_name: &ServerName<'_>,
            _ocsp_response: &[u8],
            _now: UnixTime,
        ) -> Result<ServerCertVerified, rustls::Error> {
            let pubkey = ed25519_public_key_from_certificate(end_entity)
                .map_err(|_| rustls::Error::InvalidCertificate(CertificateError::BadEncoding))?;
            if pubkey.as_slice() != self.expected_pubkey.as_slice() {
                return Err(rustls::Error::InvalidCertificate(
                    CertificateError::BadSignature,
                ));
            }
            self.full_handshakes.fetch_add(1, Ordering::SeqCst);
            Ok(ServerCertVerified::assertion())
        }

        fn verify_tls12_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            verify_tls12_signature(message, cert, dss, &self.supported_algs)
        }

        fn verify_tls13_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            verify_tls13_signature(message, cert, dss, &self.supported_algs)
        }

        fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
            self.supported_algs.supported_schemes()
        }
    }

    #[derive(Debug)]
    struct TrackingSessionStore {
        inner: ClientSessionMemoryCache,
        inserted: AtomicUsize,
        taken: AtomicUsize,
    }

    impl Default for TrackingSessionStore {
        fn default() -> Self {
            Self {
                inner: ClientSessionMemoryCache::new(32),
                inserted: AtomicUsize::new(0),
                taken: AtomicUsize::new(0),
            }
        }
    }

    impl ClientSessionStore for TrackingSessionStore {
        fn set_kx_hint(&self, server_name: ServerName<'static>, group: NamedGroup) {
            self.inner.set_kx_hint(server_name, group);
        }

        fn kx_hint(&self, server_name: &ServerName<'_>) -> Option<NamedGroup> {
            self.inner.kx_hint(server_name)
        }

        fn set_tls12_session(
            &self,
            server_name: ServerName<'static>,
            value: Tls12ClientSessionValue,
        ) {
            self.inner.set_tls12_session(server_name, value);
        }

        fn tls12_session(&self, server_name: &ServerName<'_>) -> Option<Tls12ClientSessionValue> {
            self.inner.tls12_session(server_name)
        }

        fn remove_tls12_session(&self, server_name: &ServerName<'static>) {
            self.inner.remove_tls12_session(server_name);
        }

        fn insert_tls13_ticket(
            &self,
            server_name: ServerName<'static>,
            value: Tls13ClientSessionValue,
        ) {
            self.inserted.fetch_add(1, Ordering::SeqCst);
            self.inner.insert_tls13_ticket(server_name, value);
        }

        fn take_tls13_ticket(
            &self,
            server_name: &ServerName<'static>,
        ) -> Option<Tls13ClientSessionValue> {
            let ticket = self.inner.take_tls13_ticket(server_name);
            if ticket.is_some() {
                self.taken.fetch_add(1, Ordering::SeqCst);
            }
            ticket
        }
    }
}
