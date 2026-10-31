#[cfg(test)]
use std::future::Future as _;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::{future, io};

use daemon_core::payload::CarrierKind;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::{mpsc, oneshot, watch};

use super::carrier::{ByteStream, ControlSink, ControlSource, ControlWrite, OpenError};
use super::early::{Early, EarlyRecv, EarlySend};
use super::framed::{FramedKind, StreamEndpoint};
use super::link_log::{EarlyData, PathKind};
use super::socket::{Datagrams, LinkSocket};
use super::validation::{Server, ServerTokens};
use crate::HostId;
use crate::identity::{DeviceIdentity, IdentityError};
use crate::trust::SharedTrustStore;

const CONTROL_QUEUE_CAPACITY: usize = 32;
pub(crate) const DEVICE_SERVER_NAME: &str = "amux-device.local";

/// The name every link to a host is dialled under, as rustls keys its
/// tickets.
pub(crate) fn device_server_name() -> rustls::pki_types::ServerName<'static> {
    rustls::pki_types::ServerName::try_from(DEVICE_SERVER_NAME).expect("a valid DNS name")
}
// Quinn's first Initial PTO is about one second with its default initial RTT.
// Two seconds lets one lost handshake packet retransmit without allowing one
// black-holed DNS candidate to consume the cloud connection's whole dial.
/// How long a closing link waits for its last control messages, its
/// LinkClose among them, to be acknowledged before the connection closes
/// under them.
const CLOSE_LINGER: std::time::Duration = std::time::Duration::from_secs(1);
pub(crate) const RELAY_QUIC_HANDSHAKE_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(2);
/// How long a test's resumed dial to a host waits for its handshake to
/// finish once it has started sending early data.
#[cfg(test)]
pub(crate) const RESUMED_HANDSHAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);
/// How long a relay dial waits on one address before starting on the
/// next, of the other family where there is one: the connection attempt
/// delay RFC 8305 recommends, so an address family that black-holes costs
/// a quarter second rather than a whole handshake timeout.
pub(crate) const RELAY_CANDIDATE_STAGGER: std::time::Duration =
    std::time::Duration::from_millis(250);

/// A profile's one QUIC endpoint and the link socket under it: every
/// connection the profile dials or answers over UDP is on it, IPv4 and
/// IPv6 alike. A dial names a real address; quinn is handed the stand-in
/// that arrivals from that address map to.
#[derive(Clone, Debug)]
pub(crate) struct LinkEndpoint {
    pub(crate) endpoint: quinn::Endpoint,
    pub(crate) socket: Arc<LinkSocket>,
    /// The address-validation tokens the servers this endpoint dials gave
    /// it, so dialling one again skips its Retry.
    tokens: Arc<ServerTokens>,
    /// The client configuration each peer is dialled with, holding the
    /// session ticket the peer gave the last link.
    resumption: Arc<super::resumption::PeerResumption>,
}

impl LinkEndpoint {
    pub(crate) fn new(
        substrate: Arc<dyn Datagrams>,
        own: HostId,
        server: Option<quinn::ServerConfig>,
    ) -> io::Result<Self> {
        let socket = LinkSocket::bind(substrate, own)?;
        let runtime = quinn::default_runtime()
            .ok_or_else(|| io::Error::other("no async runtime for a QUIC endpoint"))?;
        let endpoint = quinn::Endpoint::new_with_abstract_socket(
            crate::identity::quic_endpoint_config(),
            server,
            socket.clone(),
            runtime,
        )?;
        Ok(Self {
            endpoint,
            socket,
            tokens: Arc::default(),
            resumption: Arc::default(),
        })
    }

    /// The configuration to dial `peer` with, offering the ticket the
    /// peer's last link left.
    pub(crate) fn client_config(
        &self,
        identity: &DeviceIdentity,
        trust_store: &SharedTrustStore,
        peer: HostId,
    ) -> Result<quinn::ClientConfig, IdentityError> {
        self.resumption.client_config(identity, trust_store, peer)
    }

    /// The configuration to dial the relay named `server_name` with,
    /// trusting `roots`, offering the ticket the relay's last link left.
    pub(crate) fn relay_config(
        &self,
        server_name: &str,
        roots: rustls::RootCertStore,
    ) -> Result<quinn::ClientConfig, crate::transport::TransportError> {
        self.resumption.relay_config(server_name, roots)
    }

    /// Keeps the tickets and address-validation tokens peers and the
    /// relay give this endpoint's dials in the profile directory
    /// `profile`, so a relaunched process resumes its links and skips
    /// their Retry.
    pub(crate) fn keeping(mut self, profile: &std::path::Path) -> Self {
        self.resumption = Arc::new(super::resumption::PeerResumption::keeping(
            profile.join(super::client_tickets::CLIENT_TICKETS_DIR),
        ));
        self.tokens = Arc::new(ServerTokens::keeping(
            profile.join(super::validation::CLIENT_TOKENS_DIR),
        ));
        self
    }

    /// An endpoint on a dual-stack socket of its own at `bind`.
    #[cfg(test)]
    pub(crate) fn bind(bind: SocketAddr, server: Option<quinn::ServerConfig>) -> io::Result<Self> {
        Self::new(
            LinkSocket::bind_dual_stack(bind, None)?,
            HostId::new_v4(),
            server,
        )
    }

    /// Dials whoever answers at `addr`, keeping the tokens it gives for
    /// this process.
    pub(crate) fn connect_with(
        &self,
        config: quinn::ClientConfig,
        addr: SocketAddr,
        server_name: &str,
    ) -> Result<quinn::Connecting, quinn::ConnectError> {
        self.connect_to(config, addr, Server::Address(addr), server_name)
    }

    /// Dials `server` at `addr`, presenting a token it gave this profile
    /// before.
    pub(crate) fn connect_to(
        &self,
        mut config: quinn::ClientConfig,
        addr: SocketAddr,
        server: Server,
        server_name: &str,
    ) -> Result<quinn::Connecting, quinn::ConnectError> {
        config.token_store(self.tokens.for_server(server));
        self.endpoint
            .connect_with(config, self.socket.stand_in_at(addr), server_name)
    }

    /// Dials whoever answers at the far end of the byte-stream path `path`
    /// on this endpoint's socket.
    pub(crate) fn connect_on_framed(
        &self,
        config: quinn::ClientConfig,
        path: super::socket::PathId,
        server_name: &str,
    ) -> Result<quinn::Connecting, quinn::ConnectError> {
        self.endpoint
            .connect_with(config, self.socket.stand_in_on(path), server_name)
    }

    /// Dials `peer` through the forwarder at the far end of `through`: the
    /// relay, or a paired daemon. The packets go behind the header naming
    /// `peer` until a path of the peer's own carries the link; a peer some
    /// path already carries is dialled on it.
    pub(crate) fn connect_through(
        &self,
        mut config: quinn::ClientConfig,
        peer: HostId,
        through: super::socket::LinkId,
        server_name: &str,
    ) -> Result<quinn::Connecting, quinn::ConnectError> {
        let stand_in = self.socket.dial_through(peer, through);
        config.token_store(self.tokens.for_server(Server::Host(peer)));
        self.endpoint.connect_with(config, stand_in, server_name)
    }

    /// Dials `peer` on every one of `paths` at once, and through the
    /// relay when `through_relay`, until a packet from the peer arrives on
    /// one: a launch's first flight to a host, before anything says which
    /// path works. A peer some path already carries, with a link up since
    /// its last went, is dialled on it.
    pub(crate) fn connect_on_all(
        &self,
        mut config: quinn::ClientConfig,
        peer: HostId,
        paths: Vec<super::socket::Path>,
        through_relay: bool,
        server_name: &str,
    ) -> Result<quinn::Connecting, quinn::ConnectError> {
        let stand_in = self.socket.dial_on_all(peer, paths, through_relay);
        config.token_store(self.tokens.for_server(Server::Host(peer)));
        self.endpoint.connect_with(config, stand_in, server_name)
    }

    /// The kind of path a link through the forwarding link `through` runs
    /// on.
    pub(crate) fn forwarded_kind(&self, through: super::socket::LinkId) -> PathKind {
        self.socket
            .forwarded_kind(through)
            .unwrap_or(PathKind::ViaDaemon)
    }

    /// The forwarding link a connection from `remote` arrived through, and
    /// the kind of path that makes it: through the relay or through a
    /// paired daemon.
    pub(crate) fn arrived_forwarded(
        &self,
        remote: SocketAddr,
    ) -> Option<(super::socket::LinkId, PathKind)> {
        match self.socket.path_of(remote)? {
            super::socket::Path::Forwarded(through) => {
                Some((through, self.forwarded_kind(through)))
            }
            _ => None,
        }
    }

    /// The network address a connection's remote stand-in stands for, or
    /// the stand-in itself where no one address carries it.
    pub(crate) fn address_of(&self, remote: SocketAddr) -> SocketAddr {
        self.socket.address_of(remote).unwrap_or(remote)
    }

    /// Where the socket is bound, in the family it was bound in.
    pub(crate) fn local_addr(&self) -> io::Result<SocketAddr> {
        self.socket.local_addr()
    }

    /// Moves the endpoint onto a new substrate: connections carry on, and
    /// their peers see them arrive from the new address.
    pub(crate) fn rebind(&self, substrate: Arc<dyn Datagrams>) -> io::Result<()> {
        self.socket.rebind(substrate);
        self.endpoint.rebind_abstract(self.socket.clone())
    }

    /// Binds the profile's own socket afresh on its port, for a network
    /// that changed under it; false where the substrate is not the edge's
    /// to bind (a caller's socket, a router imitation).
    pub(crate) fn rebind_in_place(&self) -> io::Result<bool> {
        if !self.socket.rebind_in_place()? {
            return Ok(false);
        }
        self.endpoint.rebind_abstract(self.socket.clone())?;
        Ok(true)
    }

    pub(crate) fn close(&self, code: quinn::VarInt, reason: &[u8]) {
        self.endpoint.close(code, reason);
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    #[error("device identity configuration failed: {0}")]
    Identity(#[from] IdentityError),
    #[error("could not resolve {host}:{port}: {source}")]
    Resolve {
        host: String,
        port: u16,
        #[source]
        source: io::Error,
    },
    #[error("{host}:{port} resolved to no addresses")]
    NoAddress { host: String, port: u16 },
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
    /// A first flight's handshake ended on an answer from one of its paths,
    /// before any other path could answer.
    #[error("{}: QUIC handshake failed: {source}", turned_on(path))]
    TurnedAway {
        path: super::socket::Path,
        #[source]
        source: quinn::ConnectionError,
    },
    #[error("the link's Hello could not be written: {0}")]
    Opening(#[source] io::Error),
}

impl ConnectError {
    /// The far host closed the handshake refusing this host's certificate
    /// as no longer trusted.
    pub(crate) fn refused_as_revoked(&self) -> bool {
        match self {
            ConnectError::Handshake(error) | ConnectError::TurnedAway { source: error, .. } => {
                refused_as_revoked(error)
            }
            _ => false,
        }
    }
}

/// A path as a turned-away dial's error names it.
fn turned_on(path: &super::socket::Path) -> String {
    match path {
        super::socket::Path::Direct(addr) | super::socket::Path::Punched(addr) => addr.to_string(),
        super::socket::Path::Forwarded(link) => format!("through link {}", link.0),
        super::socket::Path::Framed(id) => format!("byte stream {}", id.0),
    }
}

/// A first flight's handshake that something answered and ended, named by
/// the path the answer came on: the first packet back settles the
/// stand-in's route on its path.
fn turned_away(
    socket: &super::socket::LinkSocket,
    stand_in: SocketAddr,
    error: ConnectError,
) -> ConnectError {
    match error {
        ConnectError::Handshake(source) if answered(&source) => match socket.path_of(stand_in) {
            Some(path) => ConnectError::TurnedAway { path, source },
            None => ConnectError::Handshake(source),
        },
        error => error,
    }
}

/// Whether the handshake ended on something the far end sent, or the
/// local end made of it, rather than on silence or this end giving up.
fn answered(error: &quinn::ConnectionError) -> bool {
    !matches!(
        error,
        quinn::ConnectionError::TimedOut
            | quinn::ConnectionError::LocallyClosed
            | quinn::ConnectionError::CidsExhausted
    )
}

/// Whether the far end closed the connection with TLS's
/// `certificate_revoked` alert: a host that pins this one presents its
/// certificate, so the alert is the host saying it no longer trusts this
/// one (the acceptor's verifier in identity.rs raises it).
fn refused_as_revoked(error: &quinn::ConnectionError) -> bool {
    matches!(
        error,
        quinn::ConnectionError::ConnectionClosed(close)
            if close.error_code
                == quinn::TransportErrorCode::crypto(
                    rustls::AlertDescription::CertificateRevoked.into()
                )
    )
}

/// Addresses in the order a dial tries them: as given, but the two
/// families taking turns, so one family failing never holds up the other.
fn alternate_families(addrs: impl IntoIterator<Item = SocketAddr>) -> Vec<SocketAddr> {
    let addrs: Vec<SocketAddr> = addrs.into_iter().collect();
    let mut take_v6 = addrs.first().is_some_and(SocketAddr::is_ipv6);
    let (v6, v4): (Vec<_>, Vec<_>) = addrs.iter().partition(|addr| addr.is_ipv6());
    let (mut v6, mut v4) = (v6.into_iter(), v4.into_iter());
    let mut ordered = Vec::with_capacity(addrs.len());
    loop {
        let next = if take_v6 {
            v6.next().or_else(|| v4.next())
        } else {
            v4.next().or_else(|| v6.next())
        };
        let Some(addr) = next else {
            return ordered;
        };
        ordered.push(addr);
        take_v6 = !take_v6;
    }
}

pub struct QuicCarrier {
    /// What the connection is apart from its path: a host's own link, one
    /// over SSH, or one between a host and a relay (on either end).
    kind: CarrierKind,
    path: PathKind,
    connection: quinn::Connection,
    /// Whether the connection is on a byte stream's endpoint of its own,
    /// whose stand-ins mean nothing on the profile's socket.
    alone: bool,
    /// The forwarding link a link through a forwarder is carried on.
    through: Option<super::socket::LinkId>,
    /// The data sent before the handshake finished, on a dial that resumed
    /// with the peer's ticket.
    early: Option<Arc<Early>>,
    /// The socket whose route for the peer says which path carries the
    /// link, on a link dialled on every candidate at once: the path is
    /// the one the peer answered on, not one the dial chose.
    follows: Option<Arc<LinkSocket>>,
    /// The forwarder the socket's route last went through alone, for once
    /// the socket has let the route go: the forwarder going takes it.
    last_through: Mutex<Option<super::socket::LinkId>>,
    /// A dialled link's claim on its peer's stand-in, which the peer keeps
    /// while the link is held.
    _dialled: Option<super::socket::DialHold>,
    inbound: tokio::sync::Mutex<mpsc::UnboundedReceiver<ByteStream>>,
    control: Mutex<Option<(ControlSink, ControlSource)>>,
    /// Asks the control writer to finish its stream, answering once the
    /// peer has acknowledged everything on it.
    finish_control: Mutex<Option<oneshot::Sender<oneshot::Sender<()>>>>,
    closed: watch::Sender<Option<wire::LinkCloseReason>>,
    /// The neighbors the Hello the dial already wrote listed, until the
    /// link takes them.
    opened: Mutex<Option<Vec<crate::routing::Host>>>,
}

/// Closes a relay dial's connection when dropped, unless the dial won.
struct CloseUnlessKept(Option<quinn::Connection>);

impl CloseUnlessKept {
    fn keep(mut self) {
        self.0 = None;
    }
}

impl Drop for CloseUnlessKept {
    fn drop(&mut self) {
        if let Some(connection) = self.0.take() {
            connection.close(0_u32.into(), b"another address answered first");
        }
    }
}

/// The Hello a link to the relay opens with and the neighbors it lists,
/// written by the dial itself the moment it starts: a resumed dial sends it
/// as early data on every address it tries, so whichever reaches the relay
/// first answers within a round trip.
#[derive(Clone, Debug)]
pub(crate) struct Opening {
    pub(crate) hello: wire::Message,
    pub(crate) neighbors: Vec<crate::routing::Host>,
    /// The forwarding link the profile's packets for other hosts go out
    /// on through the relay, put on each attempt as it says its Hello:
    /// the relay holds what it is sent until the Hello admits the link,
    /// so a first flight goes with the Hello rather than a round trip
    /// after it, whichever attempt the dial keeps.
    pub(crate) forwarding: Option<EarlyForwarding>,
}

/// A forwarding link the dial points at its attempts until the dial is
/// settled, when its owner points it at the attempt kept. Both point it
/// while holding `settled`, so an attempt finishing late never takes it
/// back from the one kept.
#[derive(Clone, Debug)]
pub(crate) struct EarlyForwarding {
    pub(crate) link: super::socket::LinkId,
    pub(crate) settled: Arc<Mutex<bool>>,
}

impl QuicCarrier {
    /// Dials `peer` at `addr` resuming with the ticket its last link
    /// left: the link carries data at once, and what is written before the
    /// handshake finishes goes out as early data. With no ticket this is
    /// an ordinary dial.
    #[cfg(test)]
    pub(crate) async fn resume_direct(
        endpoint: &LinkEndpoint,
        addr: SocketAddr,
        identity: &DeviceIdentity,
        trust_store: SharedTrustStore,
        peer: HostId,
    ) -> Result<Self, ConnectError> {
        let config = endpoint.client_config(identity, &trust_store, peer)?;
        let connecting =
            endpoint.connect_to(config, addr, Server::Host(peer), DEVICE_SERVER_NAME)?;
        let (connection, early) = handshake(connecting, RESUMED_HANDSHAKE_TIMEOUT).await?;
        let control = open_control(&connection, early.as_ref()).await?;
        Ok(Self::new(connection, CarrierKind::Direct, Some(control))
            .with_path(PathKind::of_addr(addr))
            .following(endpoint.socket.clone())
            .with_early(early))
    }

    /// Dials `peer` resuming with its ticket on every one of `paths` at
    /// once, and through the relay when `through_relay`, as a
    /// resumed dial to one address does: the link's first flight
    /// goes out on all of them, and the first path the peer answers on
    /// carries it. The handshake is given up after `deadline`.
    pub(crate) async fn first_flight(
        endpoint: &LinkEndpoint,
        paths: Vec<super::socket::Path>,
        through_relay: bool,
        identity: &DeviceIdentity,
        trust_store: SharedTrustStore,
        peer: HostId,
        deadline: std::time::Duration,
    ) -> Result<Self, ConnectError> {
        let config = endpoint.client_config(identity, &trust_store, peer)?;
        let connecting =
            endpoint.connect_on_all(config, peer, paths, through_relay, DEVICE_SERVER_NAME)?;
        let stand_in = connecting.remote_address();
        let dialled = endpoint.socket.hold_dial(stand_in);
        let (connection, early) = handshake(connecting, deadline)
            .await
            .map_err(|error| turned_away(&endpoint.socket, stand_in, error))?;
        let control = open_control(&connection, early.as_ref()).await?;
        Ok(Self::new(connection, CarrierKind::Direct, Some(control))
            .following(endpoint.socket.clone())
            .with_early(early)
            .holding(dialled))
    }

    pub(crate) async fn connect_relay(
        endpoint: &LinkEndpoint,
        host: &str,
        port: u16,
        config: quinn::ClientConfig,
        opening: Option<&Opening>,
    ) -> Result<Self, ConnectError> {
        let addrs = tokio::net::lookup_host((host, port))
            .await
            .map_err(|source| ConnectError::Resolve {
                host: host.to_string(),
                port,
                source,
            })?;
        Self::connect_relay_candidates_with_config(endpoint, addrs, host, port, config, opening)
            .await
    }

    /// Dials the relay at each of its addresses, both families, starting
    /// one more every [`RELAY_CANDIDATE_STAGGER`] with the families taking
    /// turns, and keeps the first to finish its handshake. Each dial writes
    /// `opening` at once, as early data where it resumed, so waiting for
    /// the handshake costs nothing: the relay's answer comes no sooner. A
    /// resumed dial that has written nothing yet, alone, is kept before its
    /// handshake, for its caller's early data.
    pub(crate) async fn connect_relay_candidates_with_config(
        endpoint: &LinkEndpoint,
        addrs: impl IntoIterator<Item = SocketAddr>,
        server_name: &str,
        port: u16,
        config: quinn::ClientConfig,
        opening: Option<&Opening>,
    ) -> Result<Self, ConnectError> {
        use futures_util::StreamExt as _;

        let candidates = alternate_families(addrs);
        if candidates.is_empty() {
            return Err(ConnectError::NoAddress {
                host: server_name.to_string(),
                port,
            });
        }
        // Early data going out says nothing of whether the address
        // reaches the relay: only a finished handshake does.
        let settle = opening.is_some() || candidates.len() > 1;
        let mut attempts = candidates
            .into_iter()
            .enumerate()
            .map(|(turn, addr)| {
                let config = config.clone();
                async move {
                    tokio::time::sleep(RELAY_CANDIDATE_STAGGER * turn as u32).await;
                    let attempt = tokio::time::timeout(RELAY_QUIC_HANDSHAKE_TIMEOUT, async {
                        let carrier =
                            Self::connect_relay_with_config(endpoint, addr, server_name, config)
                                .await?;
                        // A dial that loses the race, or fails, may have
                        // said its Hello: it is closed, so the relay holds
                        // no second link for the host.
                        let lost = CloseUnlessKept(Some(carrier.connection.clone()));
                        if let Some(opening) = opening {
                            carrier
                                .open_with(opening)
                                .await
                                .map_err(ConnectError::Opening)?;
                            if let Some(early) = &opening.forwarding
                                && let Some(via) = carrier.remote_address()
                            {
                                let settled = early.settled.lock().expect("settled lock poisoned");
                                if !*settled {
                                    endpoint.socket.add_forwarding(
                                        early.link,
                                        via,
                                        carrier.path_kind(),
                                    );
                                }
                            }
                        }
                        if settle {
                            carrier.handshake().await?;
                        }
                        lost.keep();
                        Ok::<_, ConnectError>(carrier)
                    })
                    .await;
                    (addr, attempt)
                }
            })
            .collect::<futures_util::stream::FuturesUnordered<_>>();
        let mut failures = Vec::new();
        while let Some((addr, attempt)) = attempts.next().await {
            match attempt {
                Ok(Ok(carrier)) => return Ok(carrier),
                Ok(Err(error)) => failures.push(format!("{addr}: {error}")),
                Err(_) => failures.push(format!("{addr}: handshake timed out")),
            }
        }
        Err(ConnectError::Candidates {
            host: server_name.to_string(),
            port,
            failures: failures.join("; "),
        })
    }

    pub(crate) async fn connect_relay_with_config(
        endpoint: &LinkEndpoint,
        addr: SocketAddr,
        server_name: &str,
        config: quinn::ClientConfig,
    ) -> Result<Self, ConnectError> {
        let connecting = endpoint.connect_to(
            config,
            addr,
            Server::Relay(server_name.to_owned()),
            server_name,
        )?;
        let (connection, early) = handshake(connecting, RELAY_QUIC_HANDSHAKE_TIMEOUT).await?;
        let control = open_control(&connection, early.as_ref()).await?;
        Ok(Self::new(connection, CarrierKind::Relay, Some(control)).with_early(early))
    }

    /// Dials `peer` through the forwarder at the far end of `through`,
    /// pinning the key paired for it as a direct dial does.
    pub(crate) async fn connect_through(
        endpoint: &LinkEndpoint,
        through: super::socket::LinkId,
        identity: &DeviceIdentity,
        trust_store: SharedTrustStore,
        peer: HostId,
    ) -> Result<Self, ConnectError> {
        let config = endpoint.client_config(identity, &trust_store, peer)?;
        let connecting = endpoint.connect_through(config, peer, through, DEVICE_SERVER_NAME)?;
        let dialled = endpoint.socket.hold_dial(connecting.remote_address());
        let connection = connecting.await?;
        let control = open_control(&connection, None).await?;
        Ok(Self::new(connection, CarrierKind::Direct, Some(control))
            .with_path(endpoint.forwarded_kind(through))
            .carried_through(Some(through))
            .following(endpoint.socket.clone())
            .holding(dialled))
    }

    /// Dials the far end of `io`, a byte stream made one more path of the
    /// profile's socket, so the link is on the profile's socket like a UDP
    /// one and packets can be forwarded across it: the host's link to the
    /// relay over TLS on TCP.
    pub(crate) async fn connect_framed<IO>(
        endpoint: &LinkEndpoint,
        io: IO,
        kind: FramedKind,
        config: quinn::ClientConfig,
        server_name: &str,
    ) -> Result<Self, ConnectError>
    where
        IO: AsyncRead + AsyncWrite + Send + 'static,
    {
        let path = super::framed::FramedPath::attach(&endpoint.socket, io, kind);
        let (connection, early) = tokio::select! {
            connection = async {
                let connecting = endpoint.connect_on_framed(config, path.id, server_name)?;
                handshake(connecting, RELAY_QUIC_HANDSHAKE_TIMEOUT).await
            } => connection?,
            () = path.closed() => return Err(ConnectError::StreamEnded),
        };
        let control = open_control(&connection, early.as_ref()).await?;
        tokio::spawn(path.carry(connection.clone()));
        Ok(
            Self::new(connection, stream_carrier_kind(kind), Some(control))
                .with_path(stream_path(kind))
                .with_early(early),
        )
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
        let control = open_control(&connection, None).await?;
        let (kind, path) = (
            stream_carrier_kind(stream.kind()),
            stream_path(stream.kind()),
        );
        stream.carry(connection.clone());
        Ok(Self::new(connection, kind, Some(control))
            .with_path(path)
            .alone())
    }

    /// Answers the connection the far end of a byte stream opens.
    pub(crate) async fn accept_over_stream(stream: StreamEndpoint) -> Result<Self, ConnectError> {
        let incoming = stream.accept().await.ok_or(ConnectError::StreamEnded)?;
        let connection = incoming.await?;
        let (kind, path) = (
            stream_carrier_kind(stream.kind()),
            stream_path(stream.kind()),
        );
        stream.carry(connection.clone());
        Ok(Self::from_accepted_with_kind(connection, kind)
            .with_path(path)
            .alone())
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
        Self::from_accepted_with_kind(connection, CarrierKind::Direct)
    }

    pub(crate) fn from_accepted_with_kind(
        connection: quinn::Connection,
        kind: CarrierKind,
    ) -> Self {
        Self::new(connection, kind, None)
    }

    fn with_early(mut self, early: Option<Arc<Early>>) -> Self {
        self.early = early;
        self
    }

    /// Waits for the link's handshake to finish: what became of the data
    /// sent before it, or why it failed. A link dialled with no ticket has
    /// finished its handshake already.
    pub(crate) async fn handshake(&self) -> Result<EarlyData, ConnectError> {
        let Some(early) = &self.early else {
            return Ok(EarlyData::None);
        };
        early.handshake().await.map_err(|error| {
            let error = ConnectError::from(error);
            match &self.follows {
                Some(socket) => turned_away(socket, self.connection.remote_address(), error),
                None => error,
            }
        })
    }

    fn alone(mut self) -> Self {
        self.alone = true;
        self
    }

    /// Names the forwarding link this link's packets go through, where
    /// they go through one.
    pub(crate) fn carried_through(mut self, through: Option<super::socket::LinkId>) -> Self {
        self.through = through;
        self
    }

    /// Reads the link's path from `socket`'s route for the peer, where the
    /// socket has one: every link to a peer on the profile's socket goes
    /// where its path machine moved the peer, whichever path it came up
    /// on.
    pub(crate) fn following(mut self, socket: Arc<LinkSocket>) -> Self {
        self.follows = Some(socket);
        self
    }

    /// Names the path the link runs on, where its kind alone does not say:
    /// a direct link's address, known to whoever dialled or answered it.
    pub(crate) fn with_path(mut self, path: PathKind) -> Self {
        self.path = path;
        self
    }

    fn holding(mut self, dialled: super::socket::DialHold) -> Self {
        self._dialled = Some(dialled);
        self
    }

    fn new(
        connection: quinn::Connection,
        kind: CarrierKind,
        opened_control: Option<(EarlySend, EarlyRecv)>,
    ) -> Self {
        let (inbound_tx, inbound) = mpsc::unbounded_channel();
        let (control_stream_tx, control_stream_rx) = oneshot::channel();
        let (closed, _) = watch::channel(None);
        let driver_connection = connection.clone();
        tokio::spawn(async move {
            let control = match opened_control {
                Some(control) => Ok(control),
                None => driver_connection
                    .accept_bi()
                    .await
                    .map(|(send, recv)| (EarlySend::plain(send), EarlyRecv::plain(recv))),
            };
            if control_stream_tx.send(control).is_err() {
                return;
            }

            // Every stream after the control stream belongs to the host at
            // the other end: the link's handshake authenticated it.
            while let Ok((send, recv)) = driver_connection.accept_bi().await {
                let stream = QuicByteStream::plain(send, recv);
                if inbound_tx.send(stream).is_err() {
                    return;
                }
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
            let error = monitor_connection.closed().await;
            tracing::info!(connection = monitor_connection.stable_id(), %error, "QUIC connection ended");
            let reason = close_reason_from_connection(error);
            if monitor_closed.borrow().is_none() {
                monitor_closed.send_replace(Some(reason));
            }
        });

        Self {
            kind,
            path: match kind {
                CarrierKind::Ssh => PathKind::Ssh,
                CarrierKind::Relay => PathKind::RelayUdp,
                CarrierKind::Direct | CarrierKind::Forwarded => PathKind::Direct,
            },
            connection,
            alone: false,
            through: None,
            early: None,
            follows: None,
            last_through: Mutex::new(None),
            _dialled: None,
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
            opened: Mutex::new(None),
        }
    }

    /// Writes `opening`'s Hello on the control stream, as early data where
    /// the dial resumed.
    async fn open_with(&self, opening: &Opening) -> io::Result<()> {
        let (mut sink, source) = self.control();
        let written = super::carrier::write_message(&mut sink, &opening.hello).await;
        *self.control.lock().expect("control stream lock poisoned") = Some((sink, source));
        written?;
        *self.opened.lock().expect("opened lock poisoned") = Some(opening.neighbors.clone());
        Ok(())
    }

    /// The neighbors the Hello this dial wrote listed, once: the link then
    /// sends no Hello of its own.
    pub(crate) fn take_opened(&self) -> Option<Vec<crate::routing::Host>> {
        self.opened.lock().expect("opened lock poisoned").take()
    }
}

/// Adapts one accepted QUIC bidirectional stream to Tokio IO: the pairing
/// server's single HTTP/2 connection, once the QUIC handshake itself has
/// classified the peer as pre-trust pairing traffic.
pub(crate) fn accepted_quic_bidi_stream(
    send: quinn::SendStream,
    recv: quinn::RecvStream,
) -> ByteStream {
    QuicByteStream::plain(send, recv)
}

impl QuicCarrier {
    /// How the link reaches its peer: a host's own link whose path goes
    /// through a forwarder (the relay, or a paired daemon) is a forwarded
    /// one, whatever it came up on.
    pub fn kind(&self) -> CarrierKind {
        let forwarded = matches!(
            self.path_kind(),
            PathKind::RelayUdp | PathKind::RelayTcp | PathKind::ViaDaemon
        );
        match self.kind {
            CarrierKind::Direct if forwarded => CarrierKind::Forwarded,
            kind => kind,
        }
    }

    pub fn control(&self) -> (ControlSink, ControlSource) {
        self.control
            .lock()
            .expect("control stream lock poisoned")
            .take()
            .expect("a link control stream can only be taken once")
    }

    pub async fn open_stream(&self) -> Result<ByteStream, OpenError> {
        let (send, recv) = match &self.early {
            Some(early) => early.open().await,
            None => self
                .connection
                .open_bi()
                .await
                .map(|(send, recv)| (EarlySend::plain(send), EarlyRecv::plain(recv))),
        }
        .map_err(map_connection_open_error)?;
        Ok(QuicByteStream { send, recv })
    }

    /// The next stream the far end opened, or none once the link ended.
    pub async fn accept_stream(&self) -> Option<ByteStream> {
        self.inbound.lock().await.recv().await
    }

    pub fn close(&self, reason: wire::LinkCloseReason) {
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

    pub fn remote_address(&self) -> Option<SocketAddr> {
        (!self.alone).then(|| self.connection.remote_address())
    }

    pub fn accepted(&self) -> bool {
        self.connection.side().is_server()
    }

    pub fn path_kind(&self) -> PathKind {
        self.follows
            .as_ref()
            .and_then(|socket| socket.kind_of(socket.path_of(self.connection.remote_address())?))
            .unwrap_or(self.path)
    }

    pub fn forwarded_through(&self) -> Option<super::socket::LinkId> {
        let Some(socket) = &self.follows else {
            return self.through;
        };
        let routed = socket.paths_of(self.connection.remote_address());
        // A first flight that only forwarders answer goes on every path
        // until one carries it: through one forwarder alone, it rides that.
        let through = match routed.as_slice() {
            [] => return *self.last_through.lock().unwrap(),
            [super::socket::Path::Forwarded(through), rest @ ..]
                if rest
                    .iter()
                    .all(|path| *path == super::socket::Path::Forwarded(*through)) =>
            {
                Some(*through)
            }
            _ => None,
        };
        *self.last_through.lock().unwrap() = through;
        through
    }

    pub fn early_data(&self) -> EarlyData {
        self.early
            .as_ref()
            .and_then(|early| early.outcome())
            .unwrap_or(EarlyData::None)
    }

    pub fn path_stats(&self) -> String {
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

    pub fn close_reason(&self) -> Option<wire::LinkCloseReason> {
        let noted = *self.closed.borrow();
        noted.or_else(|| {
            self.connection
                .close_reason()
                .map(close_reason_from_connection)
        })
    }

    /// Why the link closed, once it has.
    pub async fn closed(&self) -> wire::LinkCloseReason {
        let mut closed = self.closed.subscribe();
        loop {
            if let Some(reason) = *closed.borrow_and_update() {
                return reason;
            }
            if closed.changed().await.is_err() {
                return wire::LinkCloseReason::Unspecified;
            }
        }
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
        &super::resumption::SessionTickets::in_memory(16),
    )
    .expect("a server configuration");
    let mut roots = rustls::RootCertStore::empty();
    roots.add(der).expect("the certificate as a root");
    let client = crate::transport::relay_quic_client_config_with_roots(roots)
        .expect("a client configuration");
    // A test's paused clock jumps past any idle timeout in one step.
    let mut transport = crate::identity::quic_transport_base();
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

/// An endpoint on loopback whose arrivals each wait `delay` before quinn
/// sees them, so a dial's handshake takes at least that long: a test that
/// writes early data writes it while the handshake is surely under way.
#[cfg(test)]
pub(crate) fn slow_arrivals_endpoint(delay: std::time::Duration) -> LinkEndpoint {
    let substrate = super::socket::LinkSocket::bind_dual_stack(
        "127.0.0.1:0".parse().expect("an address"),
        None,
    )
    .expect("a loopback socket");
    LinkEndpoint::new(
        Arc::new(SlowArrivals {
            inner: substrate,
            delay,
            held: Mutex::default(),
        }),
        HostId::new_v4(),
        None,
    )
    .expect("an endpoint")
}

#[cfg(test)]
struct SlowArrivals {
    inner: Arc<dyn Datagrams>,
    delay: std::time::Duration,
    held: Mutex<SlowQueue>,
}

#[cfg(test)]
#[derive(Default)]
struct SlowQueue {
    arrived: std::collections::VecDeque<(tokio::time::Instant, Vec<u8>, SocketAddr)>,
    wake: Option<Pin<Box<tokio::time::Sleep>>>,
}

#[cfg(test)]
impl Datagrams for SlowArrivals {
    fn send_to(&self, buf: &[u8], to: SocketAddr) -> io::Result<()> {
        self.inner.send_to(buf, to)
    }

    fn poll_send_ready(&self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.inner.poll_send_ready(cx)
    }

    fn poll_recv(
        &self,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<(usize, SocketAddr)>> {
        let mut held = self.held.lock().expect("the queue");
        let mut datagram = [0_u8; 2048];
        loop {
            match self.inner.poll_recv(cx, &mut datagram) {
                Poll::Ready(Ok((len, from))) => held.arrived.push_back((
                    tokio::time::Instant::now() + self.delay,
                    datagram[..len].to_vec(),
                    from,
                )),
                Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
                Poll::Pending => break,
            }
        }
        let Some(due) = held.arrived.front().map(|(due, ..)| *due) else {
            return Poll::Pending;
        };
        if due > tokio::time::Instant::now() {
            let wake = held
                .wake
                .get_or_insert_with(|| Box::pin(tokio::time::sleep_until(due)));
            wake.as_mut().reset(due);
            if wake.as_mut().poll(cx).is_pending() {
                return Poll::Pending;
            }
        }
        let (_, bytes, from) = held.arrived.pop_front().expect("a held datagram");
        buf[..bytes.len()].copy_from_slice(&bytes);
        Poll::Ready(Ok((bytes.len(), from)))
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.inner.local_addr()
    }
}

/// What a link over a byte stream is to the rest of the host. An
/// in-process link stands for a direct one.
pub(crate) fn stream_carrier_kind(kind: FramedKind) -> CarrierKind {
    match kind {
        FramedKind::RelayTcp => CarrierKind::Relay,
        FramedKind::Ssh => CarrierKind::Ssh,
        FramedKind::InProcess => CarrierKind::Direct,
    }
}

/// The path a byte stream's link runs on. The testnet's in-process duplex
/// stands in for two hosts on one network.
pub(crate) fn stream_path(kind: FramedKind) -> PathKind {
    match kind {
        FramedKind::RelayTcp => PathKind::RelayTcp,
        FramedKind::Ssh => PathKind::Ssh,
        FramedKind::InProcess => PathKind::Lan,
    }
}

/// Why the connection closed, as a link close reason. The far end
/// refusing this host's certificate once the handshake seemed done (in TLS
/// 1.3 a client's handshake ends before the server has judged its
/// certificate) is its word that it no longer trusts this one.
fn close_reason_from_connection(error: quinn::ConnectionError) -> wire::LinkCloseReason {
    if refused_as_revoked(&error) {
        return wire::LinkCloseReason::UserRevoked;
    }
    match error {
        quinn::ConnectionError::ApplicationClosed(close) => {
            wire::LinkCloseReason::try_from(close.error_code.into_inner() as i32)
                .unwrap_or(wire::LinkCloseReason::Unspecified)
        }
        _ => wire::LinkCloseReason::Unspecified,
    }
}

async fn run_control_stream(
    stream: oneshot::Receiver<Result<(EarlySend, EarlyRecv), quinn::ConnectionError>>,
    writes: mpsc::Receiver<ControlWrite>,
    reads: mpsc::Sender<io::Result<wire::Message>>,
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
    mut recv: EarlyRecv,
    sender: mpsc::Sender<io::Result<wire::Message>>,
) {
    loop {
        match read_proto::<wire::Message>(&mut recv, wire::MESSAGE_SIZE_LIMIT).await {
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
    mut send: EarlySend,
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
        let write_result = write_frame(&mut send, &bytes, declared_len).await;
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
    send.finish();
    if let Some(answer) = finished {
        send.stopped().await;
        let _ = answer.send(());
    }
}

async fn write_frame(
    send: &mut EarlySend,
    bytes: &[u8],
    declared_len: Option<u32>,
) -> io::Result<()> {
    use tokio::io::AsyncWriteExt as _;

    let len = declared_len.unwrap_or_else(|| {
        u32::try_from(bytes.len()).expect("control message length was checked before queueing")
    });
    send.write_all(&len.to_be_bytes()).await?;
    send.write_all(bytes).await
}

async fn read_proto<M: prost::Message + Default>(
    recv: &mut EarlyRecv,
    limit: usize,
) -> io::Result<Option<M>> {
    use tokio::io::AsyncReadExt as _;

    let mut header = [0_u8; 4];
    let mut filled = 0;
    while filled < header.len() {
        match recv.read(&mut header[filled..]).await? {
            0 if filled == 0 => return Ok(None),
            0 => return Err(io::ErrorKind::UnexpectedEof.into()),
            read => filled += read,
        }
    }
    let len = u32::from_be_bytes(header) as usize;
    if len > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("protobuf frame is {len} bytes; limit is {limit}"),
        ));
    }
    let mut bytes = vec![0; len];
    recv.read_exact(&mut bytes).await?;
    M::decode(bytes.as_slice())
        .map(Some)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

/// Takes `connecting` as far as it must go before the link can carry
/// data: where the peer gave this endpoint a ticket, no further, and what
/// is written next goes out as early data, watched until the handshake
/// finishes within `deadline`; otherwise through the whole handshake,
/// given up at `deadline` too. A dial to an address that never answers
/// would otherwise hold the host's dial until the idle timeout, tens of
/// seconds, and the dial to where the host really is would wait on it.
async fn handshake(
    connecting: quinn::Connecting,
    deadline: std::time::Duration,
) -> Result<(quinn::Connection, Option<Arc<Early>>), ConnectError> {
    match connecting.into_0rtt() {
        Ok((connection, accepted)) => {
            let early = Early::start(connection.clone(), accepted, deadline);
            Ok((connection, Some(early)))
        }
        // Dropping the unfinished handshake closes its connection.
        Err(connecting) => match tokio::time::timeout(deadline, connecting).await {
            Ok(connection) => Ok((connection?, None)),
            Err(_) => Err(ConnectError::Handshake(quinn::ConnectionError::TimedOut)),
        },
    }
}

/// Opens the link's control stream, the connection's first.
async fn open_control(
    connection: &quinn::Connection,
    early: Option<&Arc<Early>>,
) -> Result<(EarlySend, EarlyRecv), quinn::ConnectionError> {
    match early {
        Some(early) => early.open().await,
        None => {
            let (send, recv) = connection.open_bi().await?;
            Ok((EarlySend::plain(send), EarlyRecv::plain(recv)))
        }
    }
}

fn map_connection_open_error(error: quinn::ConnectionError) -> OpenError {
    match error {
        quinn::ConnectionError::LocallyClosed
        | quinn::ConnectionError::ApplicationClosed(_)
        | quinn::ConnectionError::ConnectionClosed(_) => OpenError::LinkClosed,
        error => OpenError::Io(io::Error::from(error)),
    }
}

/// One bidirectional stream of a link.
pub struct QuicByteStream {
    send: EarlySend,
    recv: EarlyRecv,
}

impl QuicByteStream {
    fn plain(send: quinn::SendStream, recv: quinn::RecvStream) -> Self {
        Self {
            send: EarlySend::plain(send),
            recv: EarlyRecv::plain(recv),
        }
    }
}

impl AsyncRead for QuicByteStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        AsyncRead::poll_read(Pin::new(&mut self.recv), cx, buf)
    }
}

impl AsyncWrite for QuicByteStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        AsyncWrite::poll_write(Pin::new(&mut self.send), cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        AsyncWrite::poll_flush(Pin::new(&mut self.send), cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        AsyncWrite::poll_shutdown(Pin::new(&mut self.send), cx)
    }
}

impl QuicByteStream {
    /// Abandons the stream both ways: the far end's reads and writes fail.
    pub fn reset(&mut self) {
        let code = quinn::VarInt::from_u32(0);
        self.send.reset(code);
        self.recv.stop(code);
    }

    /// Whether the far end opened the stream in early data, before the
    /// link's handshake had finished.
    pub fn arrived_early(&self) -> bool {
        self.recv.is_0rtt()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use chrono::Utc;
    use daemon_core::trust::PairingUpdate;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;
    use crate::link::resumption::{HOST_TICKETS, SessionTickets};
    use crate::trust::TrustStore;

    struct LoopbackPair {
        _client_endpoint: LinkEndpoint,
        _server_endpoint: quinn::Endpoint,
        client: Arc<QuicCarrier>,
        server: Arc<QuicCarrier>,
    }

    async fn loopback_pair() -> LoopbackPair {
        let client_identity = DeviceIdentity::for_test(HostId::new_v4());
        let server_identity = DeviceIdentity::for_test(HostId::new_v4());
        let client_trust = trust_for(&server_identity);
        let server_trust = trust_for(&client_identity);
        let server_config = server_identity
            .quic_server_config(
                server_trust,
                &crate::link::resumption::SessionTickets::in_memory(16),
            )
            .unwrap();
        let server_endpoint =
            crate::identity::quic_endpoint(Some(server_config), "127.0.0.1:0".parse().unwrap())
                .unwrap();
        let client_endpoint = LinkEndpoint::bind("127.0.0.1:0".parse().unwrap(), None).unwrap();
        let server_addr = server_endpoint.local_addr().unwrap();

        let accept = async {
            server_endpoint
                .accept()
                .await
                .expect("server endpoint closed")
                .await
                .expect("server handshake failed")
        };
        let connect = QuicCarrier::first_flight(
            &client_endpoint,
            vec![crate::link::socket::Path::Direct(server_addr)],
            false,
            &client_identity,
            client_trust,
            server_identity.host_id,
            std::time::Duration::from_secs(5),
        );
        let (server_connection, client) = tokio::join!(accept, connect);
        let client = Arc::new(client.unwrap());
        let server = Arc::new(QuicCarrier::from_accepted(server_connection));

        // Writing one frame makes the connector-opened control stream visible
        // before any application stream is opened.
        let (mut client_control, _client_source) = client.control();
        super::super::carrier::write_message(&mut client_control, &wire::Message { body: None })
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
            PairingUpdate::Inserted
        );
        Arc::new(std::sync::RwLock::new(trust))
    }

    fn relay_test_configs() -> (quinn::ServerConfig, quinn::ClientConfig) {
        let client_identity = DeviceIdentity::for_test(HostId::new_v4());
        let server_identity = DeviceIdentity::for_test(HostId::new_v4());
        let server_config = server_identity
            .quic_server_config(
                trust_for(&client_identity),
                &crate::link::resumption::SessionTickets::in_memory(16),
            )
            .unwrap();
        let client_config = client_identity
            .quic_client_config_for_peer(trust_for(&server_identity), server_identity.host_id)
            .unwrap();
        (server_config, client_config)
    }

    /// One dual-stack listener answers dials of both families, and names
    /// each dialler by its real address behind the stand-in quinn sees.
    #[tokio::test]
    async fn a_dual_stack_endpoint_answers_both_families_and_names_their_addresses() {
        let client_identity = DeviceIdentity::for_test(HostId::new_v4());
        let server_identity = DeviceIdentity::for_test(HostId::new_v4());
        let server_config = server_identity
            .quic_server_config(
                trust_for(&client_identity),
                &crate::link::resumption::SessionTickets::in_memory(16),
            )
            .unwrap();
        let server = LinkEndpoint::bind("[::]:0".parse().unwrap(), Some(server_config)).unwrap();
        let port = server.local_addr().unwrap().port();
        for (client_bind, server_addr) in [
            ("127.0.0.1:0", SocketAddr::from(([127, 0, 0, 1], port))),
            ("[::1]:0", SocketAddr::new("::1".parse().unwrap(), port)),
        ] {
            let client = LinkEndpoint::bind(client_bind.parse().unwrap(), None).unwrap();
            let accept = async {
                server
                    .endpoint
                    .accept()
                    .await
                    .expect("server endpoint closed")
                    .await
                    .expect("server handshake failed")
            };
            let connect = QuicCarrier::first_flight(
                &client,
                vec![crate::link::socket::Path::Direct(server_addr)],
                false,
                &client_identity,
                trust_for(&server_identity),
                server_identity.host_id,
                std::time::Duration::from_secs(5),
            );
            let (accepted, dialled) = tokio::join!(accept, connect);
            let dialled = dialled.expect("the dial over {server_addr} connects");
            assert_eq!(
                server.address_of(accepted.remote_address()),
                client.local_addr().unwrap(),
                "the listener names the dialler by its address"
            );
            assert_eq!(
                client.address_of(dialled.connection.remote_address()),
                server_addr
            );
            dialled.close(wire::LinkCloseReason::UserShutdown);
            accepted.close(0_u32.into(), b"test complete");
        }
    }

    #[test]
    fn dial_candidates_alternate_families_from_the_first_given() {
        let addr = |text: &str| text.parse::<SocketAddr>().unwrap();
        let ordered = alternate_families([
            addr("[2001:db8::1]:443"),
            addr("[2001:db8::2]:443"),
            addr("[2001:db8::3]:443"),
            addr("192.0.2.1:443"),
        ]);
        assert_eq!(
            ordered,
            [
                addr("[2001:db8::1]:443"),
                addr("192.0.2.1:443"),
                addr("[2001:db8::2]:443"),
                addr("[2001:db8::3]:443"),
            ]
        );
        let ordered = alternate_families([addr("192.0.2.1:443"), addr("[2001:db8::1]:443")]);
        assert_eq!(ordered, [addr("192.0.2.1:443"), addr("[2001:db8::1]:443")]);
    }

    #[tokio::test]
    async fn relay_dial_reaches_a_relay_listening_on_ipv6_only_from_the_dual_stack_socket() {
        let (server_config, client_config) = relay_test_configs();
        let server_endpoint =
            crate::identity::quic_endpoint(Some(server_config), "[::1]:0".parse().unwrap())
                .unwrap();
        let server_addr = server_endpoint.local_addr().unwrap();
        let client_endpoint = LinkEndpoint::bind("[::]:0".parse().unwrap(), None).unwrap();
        // Nothing answers on IPv4 at the relay's port.
        let ipv4_first = SocketAddr::new("127.0.0.1".parse().unwrap(), server_addr.port());
        let started = Instant::now();

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
            [ipv4_first, server_addr],
            DEVICE_SERVER_NAME,
            server_addr.port(),
            client_config,
            None,
        );
        let (server_connection, client) = tokio::join!(accept, connect);

        client
            .expect("the IPv6 relay candidate was not tried")
            .close(wire::LinkCloseReason::UserShutdown);
        assert!(
            started.elapsed() < RELAY_QUIC_HANDSHAKE_TIMEOUT,
            "the IPv6 candidate started before the IPv4 one timed out"
        );
        server_connection.close(0_u32.into(), b"test complete");
    }

    #[tokio::test]
    async fn relay_dial_starts_the_next_candidate_while_an_unresponsive_one_waits() {
        let (server_config, client_config) = relay_test_configs();
        let server_endpoint =
            crate::identity::quic_endpoint(Some(server_config), "127.0.0.1:0".parse().unwrap())
                .unwrap();
        let server_addr = server_endpoint.local_addr().unwrap();
        let client_endpoint = LinkEndpoint::bind("[::]:0".parse().unwrap(), None).unwrap();
        let blackhole = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
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
                None,
            ),
        );
        let (server_connection, client) = tokio::join!(accept, connect);

        let client = client
            .expect("relay candidates exceeded their bounded dial budget")
            .expect("working relay candidate was not tried");
        let elapsed = started.elapsed();
        assert!(
            elapsed >= RELAY_CANDIDATE_STAGGER && elapsed < RELAY_QUIC_HANDSHAKE_TIMEOUT,
            "the second candidate started one stagger in, not after the first timed out: {elapsed:?}"
        );
        client.close(wire::LinkCloseReason::UserShutdown);
        server_connection.close(0_u32.into(), b"test complete");
        drop(blackhole);
    }

    /// A stream reaches the far end with its first bytes, and one the far
    /// end does not read never holds up another.
    #[tokio::test]
    async fn two_streams_do_not_block_each_other_under_a_slow_reader() {
        let pair = loopback_pair().await;
        let mut first = pair.client.open_stream().await.unwrap();
        first.write_all(b"first").await.unwrap();
        let mut slow_reader = pair.server.accept_stream().await.unwrap();
        let blocked_writer = tokio::spawn(async move {
            let payload = vec![7_u8; 8 * 1024 * 1024];
            first.write_all(&payload).await
        });

        let mut second = pair.client.open_stream().await.unwrap();
        second.write_all(b"second").await.unwrap();
        let mut fast_stream =
            tokio::time::timeout(Duration::from_secs(1), pair.server.accept_stream())
                .await
                .expect("second stream was blocked by the unread first stream")
                .unwrap();
        let mut greeting = [0_u8; 6];
        fast_stream.read_exact(&mut greeting).await.unwrap();
        assert_eq!(&greeting, b"second");
        fast_stream.write_all(b"ok").await.unwrap();
        let mut response = [0_u8; 2];
        tokio::time::timeout(Duration::from_secs(1), second.read_exact(&mut response))
            .await
            .expect("second stream response was blocked by the unread first stream")
            .unwrap();
        assert_eq!(&response, b"ok");

        slow_reader.reset();
        assert!(blocked_writer.await.unwrap().is_err());
    }

    /// A host answering on its own endpoint at `bind`, issuing tickets
    /// from `tickets` and trusting `client`. A restarted host's old
    /// endpoint lets go of the port only once its driver next runs.
    async fn ticket_server(
        identity: &DeviceIdentity,
        client: &DeviceIdentity,
        tickets: &Arc<SessionTickets>,
        bind: SocketAddr,
    ) -> LinkEndpoint {
        let config = identity
            .quic_server_config(trust_for(client), tickets)
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match LinkEndpoint::bind(bind, Some(config.clone())) {
                    Ok(server) => break server,
                    Err(error) if error.kind() == io::ErrorKind::AddrInUse => {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                    Err(error) => panic!("binding the host: {error}"),
                }
            }
        })
        .await
        .expect("the port comes free")
    }

    /// A host dialling one peer through the configuration its endpoint
    /// keeps for the peer.
    struct TicketClient {
        endpoint: LinkEndpoint,
        identity: DeviceIdentity,
        trust: SharedTrustStore,
        peer: HostId,
    }

    impl TicketClient {
        fn new(peer: &DeviceIdentity) -> Self {
            Self::on(
                LinkEndpoint::bind("127.0.0.1:0".parse().unwrap(), None).unwrap(),
                peer,
            )
        }

        /// A client whose handshakes take a fifth of a second, so what it
        /// writes early is written before they finish.
        fn slow(peer: &DeviceIdentity) -> Self {
            Self::on(slow_arrivals_endpoint(Duration::from_millis(200)), peer)
        }

        fn on(endpoint: LinkEndpoint, peer: &DeviceIdentity) -> Self {
            Self {
                endpoint,
                identity: DeviceIdentity::for_test(HostId::new_v4()),
                trust: trust_for(peer),
                peer: peer.host_id,
            }
        }

        /// One link to `server`, with a round trip on it so the ticket
        /// the server sent has reached the client. Whether the server
        /// took the client's address as validated.
        async fn link(&self, server: &LinkEndpoint) -> Result<bool, ConnectError> {
            let addr = server.local_addr().unwrap();
            let config = self
                .endpoint
                .client_config(&self.identity, &self.trust, self.peer)?;
            let accept = async {
                let incoming = server.endpoint.accept().await.expect("the server is open");
                let validated = incoming.remote_address_validated();
                let connection = incoming.await?;
                let (mut send, mut recv) = connection.accept_bi().await?;
                let mut byte = [0];
                recv.read_exact(&mut byte).await.unwrap();
                send.write_all(&byte).await.unwrap();
                send.finish().unwrap();
                connection.closed().await;
                Ok::<_, quinn::ConnectionError>(validated)
            };
            let connect = async {
                let connection = self
                    .endpoint
                    .connect_with(config, addr, DEVICE_SERVER_NAME)?
                    .await?;
                let (mut send, mut recv) = connection.open_bi().await?;
                send.write_all(b"t").await.unwrap();
                let mut byte = [0];
                recv.read_exact(&mut byte).await.unwrap();
                connection.close(0_u32.into(), b"done");
                Ok::<_, ConnectError>(())
            };
            let (validated, connected) = tokio::join!(accept, connect);
            connected?;
            Ok(validated.unwrap_or(false))
        }
    }

    const ANY_PORT: &str = "127.0.0.1:0";

    #[tokio::test]
    async fn a_ticket_resumes_after_the_issuing_host_restarts() {
        let dir = tempfile::tempdir().unwrap();
        let server_identity = DeviceIdentity::for_test(HostId::new_v4());
        let client = TicketClient::new(&server_identity);

        let tickets = SessionTickets::open(dir.path(), HOST_TICKETS).unwrap();
        let server = ticket_server(
            &server_identity,
            &client.identity,
            &tickets,
            ANY_PORT.parse().unwrap(),
        )
        .await;
        let addr = server.local_addr().unwrap();
        client.link(&server).await.unwrap();
        assert_eq!(tickets.redeemed(), 0, "the first link has no ticket");
        assert_eq!(tickets.kept(), 1, "the first link was issued one ticket");
        server.close(0_u32.into(), b"restart");
        server.endpoint.wait_idle().await;
        drop((server, tickets));

        let tickets = SessionTickets::open(dir.path(), HOST_TICKETS).unwrap();
        let server = ticket_server(&server_identity, &client.identity, &tickets, addr).await;
        let validated = client.link(&server).await.unwrap();
        assert_eq!(
            tickets.redeemed(),
            1,
            "the restarted host honours the ticket it issued before"
        );
        assert!(
            validated,
            "the restarted host honours the address-validation token it issued before"
        );
    }

    #[tokio::test]
    async fn a_ticket_kept_on_disk_resumes_after_the_client_restarts() {
        use crate::link::client_tickets::saved;

        let server_identity = DeviceIdentity::for_test(HostId::new_v4());
        let profile = tempfile::tempdir().unwrap();
        let tickets_dir = profile.path().join("client-tickets");
        let identity = DeviceIdentity::for_test(HostId::new_v4());
        let client = || TicketClient {
            endpoint: LinkEndpoint::bind(ANY_PORT.parse().unwrap(), None)
                .unwrap()
                .keeping(profile.path()),
            identity: identity.clone(),
            trust: trust_for(&server_identity),
            peer: server_identity.host_id,
        };
        let tickets = SessionTickets::in_memory(HOST_TICKETS);
        let server = ticket_server(
            &server_identity,
            &identity,
            &tickets,
            ANY_PORT.parse().unwrap(),
        )
        .await;
        let peer_dir = tickets_dir
            .join(server_identity.host_id.to_string())
            .join(super::super::resumption::hex(server_identity.public_key()));

        client().link(&server).await.unwrap();
        assert_eq!(
            saved(&peer_dir),
            1,
            "the ticket the link was given is on disk"
        );

        // A new process: a new endpoint, its configurations built afresh.
        let restarted = client();
        restarted.link(&server).await.unwrap();
        assert_eq!(tickets.redeemed(), 1, "the restarted client resumes");
        assert_eq!(
            saved(&peer_dir),
            1,
            "the ticket offered left the disk; the one the new link was given took its place"
        );

        // A ticket is offered once: with its file gone and no new one kept,
        // the next process does a full handshake.
        for entry in std::fs::read_dir(&peer_dir).unwrap() {
            std::fs::remove_file(entry.unwrap().path()).unwrap();
        }
        client().link(&server).await.unwrap();
        assert_eq!(tickets.redeemed(), 1);
    }

    #[tokio::test]
    async fn every_relaunch_resumes_from_the_ticket_the_last_process_kept() {
        use crate::link::client_tickets::saved;

        let server_identity = DeviceIdentity::for_test(HostId::new_v4());
        let profile = tempfile::tempdir().unwrap();
        let identity = DeviceIdentity::for_test(HostId::new_v4());
        // A new process: a new slow endpoint, its configurations and
        // ticket store built afresh from the same directory.
        let client = || TicketClient {
            endpoint: slow_arrivals_endpoint(Duration::from_millis(200)).keeping(profile.path()),
            identity: identity.clone(),
            trust: trust_for(&server_identity),
            peer: server_identity.host_id,
        };
        let tickets = SessionTickets::in_memory(HOST_TICKETS);
        let server = ticket_server(
            &server_identity,
            &identity,
            &tickets,
            ANY_PORT.parse().unwrap(),
        )
        .await;
        let peer_dir = profile
            .path()
            .join("client-tickets")
            .join(server_identity.host_id.to_string())
            .join(super::super::resumption::hex(server_identity.public_key()));
        // The host sends a link's ticket once the client's Finished reaches
        // it: two round trips into a resumed link, one after its early
        // calls are answered. A process ending sooner keeps no ticket.
        let kept_its_ticket = || async {
            tokio::time::timeout(Duration::from_secs(5), async {
                while saved(&peer_dir) == 0 {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("the link's ticket reaches the disk");
        };

        let launch = client();
        let (_, (carrier, first)) =
            tokio::join!(read_link(&server, 2), resumed_call(&launch, &server));
        assert_eq!(first, EarlyData::None, "the first link has no ticket");
        kept_its_ticket().await;
        carrier.close(wire::LinkCloseReason::UserShutdown);
        for relaunch in 1..=2 {
            let relaunched = client();
            let (_, (carrier, early)) =
                tokio::join!(read_link(&server, 2), resumed_call(&relaunched, &server));
            assert_eq!(
                early,
                EarlyData::Accepted,
                "relaunch {relaunch} resumes with its early data taken"
            );
            assert_eq!(tickets.redeemed(), relaunch);
            kept_its_ticket().await;
            carrier.close(wire::LinkCloseReason::UserShutdown);
        }
    }

    #[tokio::test]
    async fn a_ticket_issued_eight_days_ago_gets_a_full_handshake() {
        let dir = tempfile::tempdir().unwrap();
        let clock = crate::link::resumption::TestClock::new();
        let server_identity = DeviceIdentity::for_test(HostId::new_v4());
        let client = TicketClient::new(&server_identity);
        let tickets = SessionTickets::with_clock(Some(dir.path()), HOST_TICKETS, clock.reader());
        let server = ticket_server(
            &server_identity,
            &client.identity,
            &tickets,
            ANY_PORT.parse().unwrap(),
        )
        .await;

        client.link(&server).await.unwrap();
        clock.advance(Duration::from_secs(24 * 60 * 60));
        client.link(&server).await.unwrap();
        assert_eq!(tickets.redeemed(), 1, "a day-old ticket resumes");

        clock.advance(Duration::from_secs(8 * 24 * 60 * 60));
        client
            .link(&server)
            .await
            .expect("an expired ticket costs a full handshake, not the link");
        assert_eq!(tickets.redeemed(), 1, "an eight-day-old ticket does not");
    }

    #[tokio::test]
    async fn a_ticket_from_a_peers_old_key_resumes_no_link() {
        let server_identity = DeviceIdentity::for_test(HostId::new_v4());
        let client = TicketClient::new(&server_identity);
        let tickets = SessionTickets::in_memory(HOST_TICKETS);
        let server = ticket_server(
            &server_identity,
            &client.identity,
            &tickets,
            ANY_PORT.parse().unwrap(),
        )
        .await;
        client.link(&server).await.unwrap();

        // The peer is paired again under a new key; the old key's holder
        // still has the session behind the client's ticket.
        let replacement = DeviceIdentity::for_test(server_identity.host_id);
        client
            .trust
            .write()
            .unwrap()
            .upsert_paired_peer(
                replacement.host_id,
                replacement.public_key().to_vec(),
                "peer".to_string(),
                None,
                Utc::now(),
            )
            .unwrap();
        assert!(
            client.link(&server).await.is_err(),
            "the old key's holder is refused"
        );
        assert_eq!(tickets.redeemed(), 0);
    }

    /// What a host read off one link: for each stream the dialler opened,
    /// in order, whether it arrived as early data and its bytes. The first
    /// is the control stream, read as one length-prefixed frame; the
    /// others are read to their end and answered.
    async fn read_link(server: &LinkEndpoint, streams: usize) -> Vec<(bool, Vec<u8>)> {
        let connection = answer_early(&server.endpoint).await;
        let (mut read, mut control) = (Vec::new(), None);
        for index in 0..streams {
            let (mut send, mut recv) = connection.accept_bi().await.expect("a stream");
            let early = recv.is_0rtt();
            let bytes = if index == 0 {
                let mut len = [0; 4];
                recv.read_exact(&mut len).await.unwrap();
                let mut frame = vec![0; u32::from_be_bytes(len) as usize];
                recv.read_exact(&mut frame).await.unwrap();
                control = Some((send, recv));
                frame
            } else {
                let bytes = recv.read_to_end(1 << 20).await.unwrap();
                send.write_all(b"answered").await.unwrap();
                send.finish().unwrap();
                let _ = send.stopped().await;
                bytes
            };
            read.push((early, bytes));
        }
        drop(control);
        read
    }

    /// The next connection to `endpoint`, taken before its handshake
    /// finishes, so a stream reads as early data exactly when it arrived as
    /// early data (quinn marks a stream a server takes after the handshake
    /// as ordinary, however it arrived).
    async fn answer_early(endpoint: &quinn::Endpoint) -> quinn::Connection {
        let connecting = endpoint
            .accept()
            .await
            .expect("the host is open")
            .accept()
            .expect("the host takes the connection");
        let (connection, _) = connecting
            .into_0rtt()
            .unwrap_or_else(|_| panic!("a server's connection is usable at once"));
        connection
    }

    /// Dials `client`'s peer at `server` resuming where it can, writes a
    /// control frame and one call before the handshake can have finished,
    /// and reads the call's answer. What became of the early data.
    async fn resume_and_call(client: &TicketClient, server: &LinkEndpoint) -> EarlyData {
        let (carrier, early) = resumed_call(client, server).await;
        carrier.close(wire::LinkCloseReason::UserShutdown);
        early
    }

    /// [`resume_and_call`]'s link, left open.
    async fn resumed_call(
        client: &TicketClient,
        server: &LinkEndpoint,
    ) -> (QuicCarrier, EarlyData) {
        let carrier = QuicCarrier::resume_direct(
            &client.endpoint,
            server.local_addr().unwrap(),
            &client.identity,
            client.trust.clone(),
            client.peer,
        )
        .await
        .expect("the dial");
        let (mut control, _source) = carrier.control();
        super::super::carrier::write_message(&mut control, &hello_frame())
            .await
            .unwrap();
        let mut call = carrier.open_stream().await.unwrap();
        call.write_all(b"an early call").await.unwrap();
        call.shutdown().await.unwrap();
        let mut answer = Vec::new();
        call.read_to_end(&mut answer).await.unwrap();
        assert_eq!(answer, b"answered", "the call's answer arrives either way");
        let early = carrier.handshake().await.expect("the handshake");
        assert_eq!(carrier.early_data(), early);
        (carrier, early)
    }

    fn hello_frame() -> wire::Message {
        wire::Message {
            body: Some(wire::message::Body::Hello(wire::Hello {
                auth_token: Some("a token".to_owned()),
                ..Default::default()
            })),
        }
    }

    fn frame_of(message: &wire::Message) -> Vec<u8> {
        prost::Message::encode_to_vec(message)
    }

    #[tokio::test]
    async fn a_dial_with_no_ticket_sends_no_early_data() {
        let server_identity = DeviceIdentity::for_test(HostId::new_v4());
        let client = TicketClient::slow(&server_identity);
        let tickets = SessionTickets::in_memory(HOST_TICKETS);
        let server = ticket_server(
            &server_identity,
            &client.identity,
            &tickets,
            ANY_PORT.parse().unwrap(),
        )
        .await;

        let (read, early) = tokio::join!(read_link(&server, 2), resume_and_call(&client, &server));
        assert_eq!(early, EarlyData::None);
        assert_eq!(
            read,
            [
                (false, frame_of(&hello_frame())),
                (false, b"an early call".to_vec())
            ],
            "the Hello and the call follow the handshake"
        );
    }

    #[tokio::test]
    async fn early_data_a_host_accepts_carries_the_hello_and_the_calls() {
        let server_identity = DeviceIdentity::for_test(HostId::new_v4());
        let client = TicketClient::slow(&server_identity);
        let tickets = SessionTickets::in_memory(HOST_TICKETS);
        let server = ticket_server(
            &server_identity,
            &client.identity,
            &tickets,
            ANY_PORT.parse().unwrap(),
        )
        .await;
        let (_, first) = tokio::join!(read_link(&server, 2), resume_and_call(&client, &server));
        assert_eq!(first, EarlyData::None, "the first link has no ticket");

        let (read, early) = tokio::join!(read_link(&server, 2), resume_and_call(&client, &server));
        assert_eq!(early, EarlyData::Accepted);
        assert_eq!(
            read,
            [
                (true, frame_of(&hello_frame())),
                (true, b"an early call".to_vec())
            ],
            "the Hello and the call arrive as early data"
        );
        assert_eq!(tickets.redeemed(), 1);
    }

    #[tokio::test]
    async fn early_data_a_host_refuses_is_sent_again_after_the_handshake() {
        let server_identity = DeviceIdentity::for_test(HostId::new_v4());
        let client = TicketClient::slow(&server_identity);
        let tickets = SessionTickets::in_memory(HOST_TICKETS);
        let server = ticket_server(
            &server_identity,
            &client.identity,
            &tickets,
            ANY_PORT.parse().unwrap(),
        )
        .await;
        tokio::join!(read_link(&server, 2), resume_and_call(&client, &server));

        // The same host, started again with none of the tickets it issued.
        let forgotten = SessionTickets::in_memory(HOST_TICKETS);
        let restarted = ticket_server(
            &server_identity,
            &client.identity,
            &forgotten,
            ANY_PORT.parse().unwrap(),
        )
        .await;
        let (read, early) = tokio::join!(
            read_link(&restarted, 2),
            resume_and_call(&client, &restarted)
        );
        assert_eq!(early, EarlyData::Rejected);
        assert_eq!(
            read,
            [
                (false, frame_of(&hello_frame())),
                (false, b"an early call".to_vec())
            ],
            "the refused Hello and call are sent again, the control stream first"
        );
        assert_eq!(forgotten.redeemed(), 0);
    }

    /// Links `client` to the relay at `server` as a host does: writes the
    /// Hello on the control stream and reads the relay's answer, which
    /// comes on a stream of the relay's own once it has the Hello, the
    /// link's first round trip. What became of the early data, whether the
    /// Hello reached the relay as early data, and the Hello it read.
    async fn link_to_relay(
        client: &LinkEndpoint,
        server: &LinkEndpoint,
        config: &quinn::ClientConfig,
    ) -> (EarlyData, bool, wire::Message) {
        let addr = server.local_addr().unwrap();
        let dial = async {
            let opening = Opening {
                hello: hello_frame(),
                neighbors: Vec::new(),
                forwarding: None,
            };
            let carrier = QuicCarrier::connect_relay_candidates_with_config(
                client,
                [addr],
                "relay.test",
                addr.port(),
                config.clone(),
                Some(&opening),
            )
            .await
            .expect("the relay dial");
            assert_eq!(
                carrier.take_opened(),
                Some(Vec::new()),
                "the dial wrote the Hello"
            );
            let mut answer = carrier.accept_stream().await.expect("the relay's answer");
            let mut byte = [0];
            answer.read_exact(&mut byte).await.unwrap();
            let early = carrier.handshake().await.unwrap();
            carrier.close(wire::LinkCloseReason::UserShutdown);
            early
        };
        let relay = async {
            let connection = answer_early(&server.endpoint).await;
            let (send, recv) = connection.accept_bi().await.unwrap();
            let early = recv.is_0rtt();
            let message =
                read_proto::<wire::Message>(&mut EarlyRecv::plain(recv), wire::MESSAGE_SIZE_LIMIT)
                    .await
                    .unwrap()
                    .unwrap();
            let (mut reply, _) = connection.open_bi().await.unwrap();
            reply.write_all(b"!").await.unwrap();
            let _ = connection.closed().await;
            drop(send);
            (early, message)
        };
        let (early, (arrived_early, hello)) = tokio::join!(dial, relay);
        (early, arrived_early, hello)
    }

    #[tokio::test]
    async fn a_relay_link_resuming_writes_its_token_as_early_data() {
        let (server_config, client_config) = relay_server_and_client();
        let server = LinkEndpoint::bind(ANY_PORT.parse().unwrap(), Some(server_config)).unwrap();
        let client = slow_arrivals_endpoint(Duration::from_millis(200));
        for expected in [EarlyData::None, EarlyData::Accepted] {
            let (early, arrived_early, hello) =
                link_to_relay(&client, &server, &client_config).await;
            assert_eq!(early, expected);
            assert_eq!(arrived_early, expected == EarlyData::Accepted);
            assert_eq!(hello, hello_frame(), "the Hello carries the relay token");
        }
    }

    /// A resumed dial to a relay at one address puts the profile's
    /// forwarding on the attempt as it says its Hello, before the relay
    /// has so much as taken the connection: a first flight through the
    /// relay leaves with the Hello, not a round trip after it.
    #[tokio::test]
    async fn a_resumed_relay_dial_forwards_through_its_attempt_before_the_relay_answers() {
        let (server_config, client_config) = relay_server_and_client();
        let server = LinkEndpoint::bind(ANY_PORT.parse().unwrap(), Some(server_config)).unwrap();
        let client = LinkEndpoint::bind(ANY_PORT.parse().unwrap(), None).unwrap();
        link_to_relay(&client, &server, &client_config).await;
        let relay = server.local_addr().unwrap();
        let link = super::super::socket::LinkId(42);
        let opening = Opening {
            hello: hello_frame(),
            neighbors: Vec::new(),
            forwarding: Some(EarlyForwarding {
                link,
                settled: Arc::default(),
            }),
        };
        let dial = QuicCarrier::connect_relay_candidates_with_config(
            &client,
            [relay],
            "relay.test",
            relay.port(),
            client_config,
            Some(&opening),
        );
        tokio::pin!(dial);
        tokio::time::timeout(RELAY_QUIC_HANDSHAKE_TIMEOUT, async {
            while client.socket.forwarded_kind(link).is_none() {
                tokio::select! {
                    done = &mut dial => panic!("the dial finished first: {:?}", done.err()),
                    () = tokio::time::sleep(Duration::from_millis(5)) => {}
                }
            }
        })
        .await
        .expect("forwarding is on the attempt while the relay has yet to answer");
        let (carrier, _connection) = tokio::join!(dial, answer_early(&server.endpoint));
        carrier.expect("the relay dial");
    }

    #[tokio::test]
    async fn a_relay_link_a_restarted_relay_refuses_early_data_on_comes_up() {
        // A restarted relay has ticket keys of its own: it cannot read a
        // ticket the relay before it gave, and so refuses the early data.
        // Each relay here is the other's restart, so every link after the
        // first resumes with a ticket the relay it reaches did not give.
        let (before, after, client_config) = a_relay_and_its_restart();
        let before = LinkEndpoint::bind(ANY_PORT.parse().unwrap(), Some(before)).unwrap();
        let after = LinkEndpoint::bind(ANY_PORT.parse().unwrap(), Some(after)).unwrap();
        let client = slow_arrivals_endpoint(Duration::from_millis(50));
        for link in 0..8 {
            let server = if link % 2 == 0 { &before } else { &after };
            let (early, arrived_early, hello) =
                link_to_relay(&client, server, &client_config).await;
            let expected = if link == 0 {
                EarlyData::None
            } else {
                EarlyData::Rejected
            };
            assert_eq!(early, expected, "link {link}");
            assert!(
                !arrived_early,
                "link {link}: the Hello is sent again after the handshake"
            );
            assert_eq!(hello, hello_frame(), "link {link}: the Hello arrives whole");
        }
    }

    #[tokio::test]
    async fn a_resumed_relay_dial_nothing_answers_fails_as_its_handshake_did() {
        let (server_config, client_config) = relay_server_and_client();
        let server = LinkEndpoint::bind(ANY_PORT.parse().unwrap(), Some(server_config)).unwrap();
        let client = LinkEndpoint::bind(ANY_PORT.parse().unwrap(), None).unwrap();
        link_to_relay(&client, &server, &client_config).await;
        // Bound and never read, so the dial hears nothing back at all.
        let silent = std::net::UdpSocket::bind(ANY_PORT).unwrap();
        let carrier = QuicCarrier::connect_relay_with_config(
            &client,
            silent.local_addr().unwrap(),
            "relay.test",
            client_config,
        )
        .await
        .expect("a resumed dial is usable before its handshake");
        let (mut control, mut source) = carrier.control();
        super::super::carrier::write_message(&mut control, &hello_frame())
            .await
            .expect("the Hello goes out as early data");
        let error = super::super::carrier::read_message(&mut source)
            .await
            .expect_err("nothing answers");
        assert_eq!(
            error.kind(),
            io::ErrorKind::TimedOut,
            "the control stream fails as the handshake did, not as refused early data: {error}"
        );
        let handshake = carrier.handshake().await.expect_err("the handshake fails");
        assert!(
            matches!(
                handshake,
                ConnectError::Handshake(quinn::ConnectionError::TimedOut)
            ),
            "{handshake}"
        );
    }

    /// A resumed dial to a relay with two addresses, the first of which
    /// answers nothing (a family that black-holes), reaches the relay at
    /// the second: the first's early data going out is no sign it works.
    #[tokio::test]
    async fn a_resumed_relay_dial_passes_over_an_address_nothing_answers() {
        let (server_config, client_config) = relay_server_and_client();
        let server = LinkEndpoint::bind(ANY_PORT.parse().unwrap(), Some(server_config)).unwrap();
        let client = LinkEndpoint::bind(ANY_PORT.parse().unwrap(), None).unwrap();
        link_to_relay(&client, &server, &client_config).await;
        let silent = std::net::UdpSocket::bind(ANY_PORT).unwrap();
        let relay = server.local_addr().unwrap();
        let dial = QuicCarrier::connect_relay_candidates_with_config(
            &client,
            [silent.local_addr().unwrap(), relay],
            "relay.test",
            relay.port(),
            client_config,
            None,
        );
        let answer = answer_early(&server.endpoint);
        let (carrier, _connection) =
            tokio::time::timeout(RELAY_QUIC_HANDSHAKE_TIMEOUT * 2, async {
                tokio::join!(dial, answer)
            })
            .await
            .expect("the relay answered at its second address");
        let carrier = carrier.expect("the relay reached at its second address");
        assert_eq!(
            carrier.connection.remote_address(),
            client.socket.stand_in_at(relay)
        );
    }

    /// A dial through a forwarder to a host whose last link went goes
    /// through the forwarder, not on the path that link last used; one to
    /// a host a live link carries keeps its path.
    #[tokio::test]
    async fn a_dial_through_a_forwarder_passes_over_a_gone_links_route() {
        let (_server_config, client_config) = relay_server_and_client();
        let client = LinkEndpoint::bind(ANY_PORT.parse().unwrap(), None).unwrap();
        let desk = HostId::new_v4();
        let old: SocketAddr = "192.0.2.50:5000".parse().unwrap();
        assert!(client.socket.name(client.socket.stand_in_at(old), desk));
        let through = super::super::socket::LinkId(7);

        drop(client.connect_through(client_config.clone(), desk, through, "relay.test"));
        assert_eq!(
            client.socket.trusted_route(desk),
            Some(super::super::socket::Path::Direct(old)),
            "a live link's path is kept"
        );

        client.socket.release(desk);
        drop(client.connect_through(client_config, desk, through, "relay.test"));
        assert_eq!(
            client.socket.trusted_route(desk),
            Some(super::super::socket::Path::Forwarded(through))
        );
    }

    #[tokio::test]
    async fn a_relay_ticket_kept_on_disk_resumes_after_the_client_restarts() {
        use crate::link::client_tickets::saved;

        let tickets = SessionTickets::in_memory(16);
        let (server_config, roots) = relay_server_issuing(&tickets);
        let server = LinkEndpoint::bind(ANY_PORT.parse().unwrap(), Some(server_config)).unwrap();
        let addr = server.local_addr().unwrap();
        let profile = tempfile::tempdir().unwrap();
        let tickets_dir = profile.path().join("client-tickets");
        let relay_dir = tickets_dir.join("relay").join("relay.test");
        // Each process dials from an endpoint of its own, its relay
        // configuration built afresh.
        let link = || async {
            let client = LinkEndpoint::bind(ANY_PORT.parse().unwrap(), None)
                .unwrap()
                .keeping(profile.path());
            let config = client.relay_config("relay.test", roots.clone()).unwrap();
            let dial = async {
                let carrier = QuicCarrier::connect_relay_candidates_with_config(
                    &client,
                    [addr],
                    "relay.test",
                    addr.port(),
                    config,
                    None,
                )
                .await
                .expect("the relay dial");
                let (mut control, _source) = carrier.control();
                super::super::carrier::write_message(&mut control, &hello_frame())
                    .await
                    .unwrap();
                // The relay's answer comes after the ticket it gives.
                let mut answer = carrier.accept_stream().await.expect("the relay's answer");
                let mut byte = [0];
                answer.read_exact(&mut byte).await.unwrap();
                let early = carrier.handshake().await.unwrap();
                carrier.close(wire::LinkCloseReason::UserShutdown);
                early
            };
            let relay = async {
                let (connection, handshaken) = server
                    .endpoint
                    .accept()
                    .await
                    .expect("the relay is open")
                    .accept()
                    .expect("the relay takes the connection")
                    .into_0rtt()
                    .unwrap_or_else(|_| panic!("a server's connection is usable at once"));
                let (send, recv) = connection.accept_bi().await.unwrap();
                read_proto::<wire::Message>(&mut EarlyRecv::plain(recv), wire::MESSAGE_SIZE_LIMIT)
                    .await
                    .unwrap()
                    .unwrap();
                // The ticket goes once the handshake is done; answering
                // after it, the client has the ticket when it closes.
                handshaken.await;
                let (mut reply, _) = connection.open_bi().await.unwrap();
                reply.write_all(b"!").await.unwrap();
                let _ = connection.closed().await;
                drop(send);
            };
            tokio::join!(dial, relay).0
        };

        assert_eq!(link().await, EarlyData::None);
        assert_eq!(saved(&relay_dir), 1, "the relay's ticket is on disk");

        assert_eq!(
            link().await,
            EarlyData::Accepted,
            "the relaunched process's Hello goes as early data"
        );
        assert_eq!(
            tickets.redeemed(),
            1,
            "the relaunched process resumes with the ticket the last one saved"
        );
        assert_eq!(
            saved(&relay_dir),
            1,
            "the ticket offered left the disk; the resumed link's took its place"
        );
    }

    /// A relay's configuration, issuing tickets from `tickets`, and the
    /// roots that trust it.
    fn relay_server_issuing(
        tickets: &Arc<SessionTickets>,
    ) -> (quinn::ServerConfig, rustls::RootCertStore) {
        let certified = relay_certificate();
        let roots = relay_roots(&certified);
        (relay_server_from(&certified, tickets), roots)
    }

    /// The configurations of a relay before and after a restart, the same
    /// certificate with ticket keys of each its own, and a client's that
    /// trusts it.
    fn a_relay_and_its_restart() -> (
        quinn::ServerConfig,
        quinn::ServerConfig,
        quinn::ClientConfig,
    ) {
        let certified = relay_certificate();
        let client = crate::transport::relay_quic_client_config_with_roots(relay_roots(&certified))
            .expect("a client configuration");
        (
            relay_server_from(&certified, &SessionTickets::in_memory(16)),
            relay_server_from(&certified, &SessionTickets::in_memory(16)),
            client,
        )
    }

    fn relay_certificate() -> rcgen::CertifiedKey<rcgen::KeyPair> {
        rcgen::generate_simple_self_signed(vec!["relay.test".to_owned()]).expect("a certificate")
    }

    fn relay_server_from(
        certified: &rcgen::CertifiedKey<rcgen::KeyPair>,
        tickets: &Arc<SessionTickets>,
    ) -> quinn::ServerConfig {
        crate::transport::relay_quic_server_config_from_der(
            vec![certified.cert.der().clone()],
            rustls::pki_types::PrivateKeyDer::Pkcs8(rustls::pki_types::PrivatePkcs8KeyDer::from(
                certified.signing_key.serialize_der(),
            )),
            tickets,
        )
        .expect("a server configuration")
    }

    fn relay_roots(certified: &rcgen::CertifiedKey<rcgen::KeyPair>) -> rustls::RootCertStore {
        let mut roots = rustls::RootCertStore::empty();
        roots
            .add(certified.cert.der().clone())
            .expect("the certificate as a root");
        roots
    }

    /// A relay's configuration and a client's that trusts it, the client's
    /// built once as a process builds the relay's.
    fn relay_server_and_client() -> (quinn::ServerConfig, quinn::ClientConfig) {
        let (server, roots) = relay_server_issuing(&SessionTickets::in_memory(16));
        let client = crate::transport::relay_quic_client_config_with_roots(roots)
            .expect("a client configuration");
        (server, client)
    }
}
