//! The relay a cloud deployment runs: it accepts links from every host of
//! an account, authenticates each by its connection token, advertises each
//! host's adjacency to the others (NeighborUp/Down) and forwards the packets
//! a host sends behind a header naming another host of its account, while
//! both are paid. It reads nothing of what it forwards: the link between
//! the two hosts is their own QUIC connection, pinned at pairing.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};

use futures_util::{Stream, StreamExt, stream};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{RwLock, Semaphore, mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_rustls::TlsAcceptor;
use uuid::Uuid;

use crate::auth::jwt::JwtValidator;
use crate::link::framed::{FramedKind, FramedPath};
use crate::link::quic::{stream_carrier_kind, stream_path};
use crate::link::relaying::Relaying;
use crate::link::socket::{LinkSocket, Path, PathId};
use crate::link::{CarrierKind, ChannelPool, LinkEndpoint, QuicCarrier, run_link};
use crate::resource_limits::HandshakeAdmission;
use crate::routing::{
    AuthenticatedLinkContextProvider, AuthenticatedLinkUser, Capabilities, LinkCtx, LinkRegistry,
    LinkTokenAuthenticator, LiveLocalHost, local_host,
};
use crate::transport::tcp_incoming;
use crate::{Clock, HostId};

const CLOUD_TLS_HANDSHAKE_CONCURRENCY: usize = 128;
/// How long a host on plain TCP has for the QUIC handshake inside it.
const STREAM_HANDSHAKE: Duration = Duration::from_secs(10);
type CloudTlsTransport = tokio_rustls::server::TlsStream<TcpStream>;

/// Who the relay is and how it checks a host's connection token.
#[derive(Clone)]
pub struct RelayIdentity {
    pub host_id: HostId,
    pub name: String,
    pub authenticator: Arc<dyn LinkTokenAuthenticator>,
    /// The policy clock a link's credential expires by.
    pub clock: Arc<dyn Clock>,
}

/// Checks connection tokens against the cloud's signing keys: a token names
/// the relay it was minted for, and the relay refuses one minted for
/// another.
pub struct JwtCloudLinkAuthenticator {
    validator: JwtValidator,
    host_name: String,
    tcp_port: u16,
}

impl JwtCloudLinkAuthenticator {
    pub fn new(cloud_url: &str, host_name: String, tcp_port: u16, clock: Arc<dyn Clock>) -> Self {
        Self {
            validator: JwtValidator::new_with_clock(cloud_url, clock),
            host_name,
            tcp_port,
        }
    }
}

#[tonic::async_trait]
impl LinkTokenAuthenticator for JwtCloudLinkAuthenticator {
    async fn authenticate_token(
        &self,
        token: &str,
    ) -> Result<AuthenticatedLinkUser, tonic::Status> {
        let claims = self
            .validator
            .validate(token, &self.host_name, self.tcp_port)
            .await
            .map_err(|error| {
                tracing::warn!(error = %error, "routing token validation failed");
                tonic::Status::unauthenticated("invalid routing authorization")
            })?;
        let user_id = claims.sub.parse::<Uuid>().map_err(|_| {
            tracing::warn!(sub = %claims.sub, "routing token has invalid user id");
            tonic::Status::unauthenticated("invalid routing authorization")
        })?;
        let expires_at = UNIX_EPOCH
            .checked_add(Duration::from_secs(claims.exp))
            .ok_or_else(|| tonic::Status::unauthenticated("invalid routing authorization"))?;
        Ok(AuthenticatedLinkUser {
            user_id,
            client_id: claims.client_id,
            expires_at,
            tier: claims.tier,
        })
    }
}

#[derive(Clone)]
pub struct CloudLinkServer {
    inner: Arc<CloudLinkServerInner>,
}

struct CloudLinkServerInner {
    identity: RelayIdentity,
    local_host: LiveLocalHost,
    users: RwLock<HashMap<Uuid, RelayUser>>,
    /// The one core every account's links are paired by: accounts are
    /// kept apart by its adjacency, not by a core each.
    relaying: Arc<Relaying>,
    admission: HandshakeAdmission,
}

/// One account's side of the relay: the links its hosts hold here and the
/// adjacency they advertise to each other. Accounts never see each other.
struct RelayUser {
    channels: Arc<ChannelPool>,
    incoming_streams_tx: mpsc::Sender<crate::link::InboundStream>,
    drain: JoinHandle<()>,
}

impl Drop for RelayUser {
    fn drop(&mut self) {
        self.drain.abort();
    }
}

impl RelayUser {
    fn new(relaying: Arc<Relaying>, user_id: Uuid) -> Self {
        let links = Arc::new(LinkRegistry::relayed(
            relaying,
            relay_core::UserId(user_id.into_bytes()),
        ));
        let (incoming_streams_tx, mut incoming) = mpsc::channel(64);
        // A stream addressed to the relay itself has nothing to reach: the
        // relay serves no calls of its own.
        let drain = tokio::spawn(async move { while incoming.recv().await.is_some() {} });
        Self {
            channels: Arc::new(ChannelPool::new(links)),
            incoming_streams_tx,
            drain,
        }
    }

    fn link_ctx(&self, local_host: &LiveLocalHost, clock: &Arc<dyn Clock>) -> LinkCtx {
        LinkCtx::new_live(local_host.clone(), self.channels.link_registry())
            .with_incoming_streams(self.incoming_streams_tx.clone())
            .with_clock(clock.clone())
    }
}

impl CloudLinkServer {
    /// A relay writing the dumps its core asks for into `reports`. A relay
    /// in production always has one; a test that never reads a dump passes
    /// None, and such a dump is then only logged.
    pub fn new(identity: RelayIdentity, reports: Option<std::path::PathBuf>) -> Self {
        let local_host = LiveLocalHost::new(local_host(
            identity.host_id,
            &identity.name,
            Capabilities {
                cloud_relay: true,
                kinds: Vec::new(),
            },
            false,
        ));
        let relaying = Relaying::new(identity.host_id);
        if let Some(reports) = reports {
            relaying.report_to(reports);
        }
        Self {
            inner: Arc::new(CloudLinkServerInner {
                relaying,
                identity,
                local_host,
                users: RwLock::new(HashMap::new()),
                admission: HandshakeAdmission::default(),
            }),
        }
    }

    /// Serves the relay on one link socket over `udp`, with every byte
    /// stream accepted on `listener` one more path of it: plain TCP, for a
    /// relay behind a TLS terminator or a test. Every connection, over UDP
    /// or a stream, is answered with `quic` by one QUIC endpoint, so a
    /// packet forwarded between two hosts crosses from either kind of path
    /// to the other.
    pub fn serve_tcp(
        &self,
        udp: std::net::UdpSocket,
        listener: TcpListener,
        quic: quinn::ServerConfig,
    ) -> std::io::Result<RelayServing> {
        self.serve(udp, tcp_incoming(listener), quic, STREAM_HANDSHAKE)
    }

    /// Serves the relay as [`CloudLinkServer::serve_tcp`] does, with TLS on
    /// each TCP stream, for hosts whose UDP does not get through.
    pub fn serve_tls(
        &self,
        udp: std::net::UdpSocket,
        listener: TcpListener,
        acceptor: TlsAcceptor,
        quic: quinn::ServerConfig,
        handshake_timeout: Duration,
    ) -> std::io::Result<RelayServing> {
        let incoming = cloud_tls_incoming(listener, acceptor, handshake_timeout);
        self.serve(udp, incoming, quic, handshake_timeout)
    }

    fn serve<I, IO>(
        &self,
        udp: std::net::UdpSocket,
        incoming: I,
        quic: quinn::ServerConfig,
        handshake_timeout: Duration,
    ) -> std::io::Result<RelayServing>
    where
        I: Stream<Item = Result<IO, std::io::Error>> + Send + 'static,
        IO: AsyncRead + AsyncWrite + Send + 'static,
    {
        let endpoint = LinkEndpoint::new(
            LinkSocket::on_udp(udp)?,
            self.inner.identity.host_id,
            Some(quic),
        )?;
        self.inner.relaying.forward_on(endpoint.socket.clone());
        let streams = Streams::default();
        let tasks = vec![
            self.accept_streams(&endpoint, incoming, streams.clone(), handshake_timeout),
            self.accept_connections(&endpoint, streams, handshake_timeout),
        ];
        Ok(RelayServing { endpoint, tasks })
    }

    /// Makes each byte stream a path of the relay's socket, held until the
    /// connection on it ends or it ends first.
    fn accept_streams<I, IO>(
        &self,
        endpoint: &LinkEndpoint,
        incoming: I,
        streams: Streams,
        handshake_timeout: Duration,
    ) -> JoinHandle<()>
    where
        I: Stream<Item = Result<IO, std::io::Error>> + Send + 'static,
        IO: AsyncRead + AsyncWrite + Send + 'static,
    {
        let socket = endpoint.socket.clone();
        tokio::spawn(async move {
            let mut incoming = Box::pin(incoming);
            while let Some(item) = incoming.next().await {
                let io = match item {
                    Ok(io) => io,
                    Err(error) => {
                        tracing::warn!(error = %error, "cloud stream accept failed");
                        continue;
                    }
                };
                let path = FramedPath::attach(&socket, io, FramedKind::RelayTcp);
                let (tx, rx) = oneshot::channel();
                lock(&streams).insert(path.id, tx);
                tokio::spawn(carry_stream(path, rx, streams.clone(), handshake_timeout));
            }
        })
    }

    fn accept_connections(
        &self,
        endpoint: &LinkEndpoint,
        streams: Streams,
        handshake_timeout: Duration,
    ) -> JoinHandle<()> {
        let service = self.clone();
        let endpoint = endpoint.clone();
        tokio::spawn(async move {
            let mut connection_tasks = tokio::task::JoinSet::new();
            loop {
                let incoming = tokio::select! {
                    incoming = endpoint.endpoint.accept() => incoming,
                    completed = connection_tasks.join_next(), if !connection_tasks.is_empty() => {
                        if let Some(Err(error)) = completed
                            && !error.is_cancelled()
                        {
                            tracing::warn!(error = %error, "cloud QUIC connection task failed");
                        }
                        continue;
                    }
                };
                let Some(incoming) = incoming else { break };
                let stand_in = incoming.remote_address();
                // A byte stream's far end proved its address opening the
                // stream; a UDP sender proves it by answering a Retry or
                // presenting a token from an earlier connection.
                let stream = match endpoint.socket.path_of(stand_in) {
                    Some(Path::Framed(path)) => match lock(&streams).remove(&path) {
                        Some(stream) => Some(stream),
                        None => {
                            incoming.refuse();
                            continue;
                        }
                    },
                    _ => None,
                };
                let addr = endpoint.address_of(stand_in);
                let Some((incoming, permit)) =
                    service
                        .inner
                        .admission
                        .admit(incoming, addr, stream.is_some())
                else {
                    continue;
                };
                let service = service.clone();
                let socket = endpoint.socket.clone();
                connection_tasks.spawn(async move {
                    let accepted = tokio::time::timeout(handshake_timeout, incoming).await;
                    drop(permit);
                    let connection = match accepted {
                        Ok(Ok(connection)) => connection,
                        Ok(Err(error)) => {
                            tracing::warn!(peer = %addr, error = %error, "cloud QUIC handshake failed");
                            service.inner.relaying.not_admitted(stand_in);
                            return;
                        }
                        Err(_) => {
                            tracing::warn!(peer = %addr, "cloud QUIC handshake timed out");
                            service.inner.relaying.not_admitted(stand_in);
                            return;
                        }
                    };
                    let carrier = match stream {
                        Some(stream) => {
                            let _ = stream.send(connection.clone());
                            QuicCarrier::from_accepted_with_kind(
                                connection,
                                stream_carrier_kind(FramedKind::RelayTcp),
                            )
                            .with_path(stream_path(FramedKind::RelayTcp))
                        }
                        None => {
                            QuicCarrier::from_accepted_with_kind(connection, CarrierKind::RelayQuic)
                        }
                    };
                    let ctx = service
                        .accepting_link_ctx()
                        .await
                        .observing(socket);
                    if let Err(error) = run_link(
                        ctx,
                        Arc::new(carrier),
                        crate::routing::ConnectRole::Acceptor,
                    )
                    .await
                    {
                        tracing::warn!(peer = %addr, error = %error, "cloud link exited with error");
                    }
                    service.inner.relaying.not_admitted(stand_in);
                });
            }
            connection_tasks.detach_all();
        })
    }

    /// The relay with another per-source handshake limit, before it
    /// serves: a test's lower one, or a benchmark's higher one where every
    /// host dials from the same loopback address.
    #[doc(hidden)]
    pub fn with_handshake_rate_limit(mut self, rate: usize) -> Self {
        Arc::get_mut(&mut self.inner)
            .expect("set before the relay serves")
            .admission = HandshakeAdmission::with_limits(
            rate,
            crate::resource_limits::QUIC_HANDSHAKE_CONCURRENCY,
        );
        self
    }

    /// What the relay's forwarder has done with the packets hosts sent it
    /// to forward, once it serves UDP.
    #[doc(hidden)]
    pub fn forward_counts(&self) -> Option<crate::link::forwarder::Counts> {
        self.inner.relaying.forward_counts()
    }

    /// How many Retries the relay has answered unvalidated Initials with.
    #[doc(hidden)]
    pub fn retries_sent(&self) -> u64 {
        self.inner.admission.retries_sent()
    }

    /// How many handshakes the relay's per-source limit dropped.
    #[doc(hidden)]
    pub fn handshakes_limited(&self) -> u64 {
        self.inner.admission.limited()
    }

    async fn link_ctx_for_user(&self, user_id: Uuid) -> LinkCtx {
        let inner = &self.inner;
        if let Some(ctx) = inner
            .users
            .read()
            .await
            .get(&user_id)
            .map(|user| user.link_ctx(&inner.local_host, &inner.identity.clock))
        {
            return ctx;
        }
        let mut users = inner.users.write().await;
        users
            .entry(user_id)
            .or_insert_with(|| RelayUser::new(inner.relaying.clone(), user_id))
            .link_ctx(&inner.local_host, &inner.identity.clock)
    }

    async fn accepting_link_ctx(&self) -> LinkCtx {
        LinkCtx::new_live(
            self.inner.local_host.clone(),
            Arc::new(LinkRegistry::new(self.inner.identity.host_id)),
        )
        .with_token_authenticator(self.inner.identity.authenticator.clone())
        .with_authenticated_context_provider(Arc::new(self.clone()))
        .with_clock(self.inner.identity.clock.clone())
    }

    pub async fn user_has_link_to(&self, user_id: Uuid, host_id: HostId) -> bool {
        let tunnels = self
            .inner
            .users
            .read()
            .await
            .get(&user_id)
            .map(|services| services.channels.clone());
        match tunnels {
            Some(tunnels) => tunnels
                .link_registry()
                .link_to_peer(host_id)
                .await
                .is_some(),
            None => false,
        }
    }

    /// Which hosts this account is connected to the relay by, and how many
    /// links each of them holds. One per host is what a client multiplexing
    /// its work over a single connection looks like from here.
    #[doc(hidden)]
    pub async fn user_links(&self, user_id: Uuid) -> Vec<(HostId, usize)> {
        let channels = self
            .inner
            .users
            .read()
            .await
            .get(&user_id)
            .map(|services| services.channels.clone());
        match channels {
            Some(channels) => channels.link_registry().links_per_peer().await,
            None => Vec::new(),
        }
    }

    pub async fn send_link_close_to_all(&self, reason: wire::pb::LinkCloseReason) {
        let tunnels = {
            let users = self.inner.users.read().await;
            users
                .values()
                .map(|services| services.channels.clone())
                .collect::<Vec<_>>()
        };
        for tunnels in tunnels {
            tunnels.link_registry().send_link_close_to_all(reason).await;
        }
    }
}

#[tonic::async_trait]
impl AuthenticatedLinkContextProvider for CloudLinkServer {
    async fn context_for(&self, user: &AuthenticatedLinkUser) -> LinkCtx {
        self.link_ctx_for_user(user.user_id)
            .await
            .with_link_role(crate::routing::LinkRole::CloudRelay)
    }
}

fn cloud_tls_incoming(
    listener: TcpListener,
    acceptor: TlsAcceptor,
    handshake_timeout: Duration,
) -> impl Stream<Item = Result<CloudTlsTransport, std::io::Error>> + Send + 'static {
    let (tx, rx) = mpsc::channel(CLOUD_TLS_HANDSHAKE_CONCURRENCY);
    let slots = Arc::new(Semaphore::new(CLOUD_TLS_HANDSHAKE_CONCURRENCY));

    tokio::spawn(async move {
        loop {
            let permit = tokio::select! {
                permit = slots.clone().acquire_owned() => {
                    match permit {
                        Ok(permit) => permit,
                        Err(_) => break,
                    }
                }
                _ = tx.closed() => break,
            };

            let (stream, addr) = tokio::select! {
                accepted = listener.accept() => {
                    match accepted {
                        Ok(accepted) => accepted,
                        Err(error) => {
                            if tx.send(Err(error)).await.is_err() {
                                break;
                            }
                            continue;
                        }
                    }
                }
                _ = tx.closed() => break,
            };

            if let Err(error) = stream.set_nodelay(true) {
                tracing::warn!(error = %error, "failed to set TCP_NODELAY");
            }
            let acceptor = acceptor.clone();
            let tx = tx.clone();
            tokio::spawn(async move {
                let _permit = permit;
                match tokio::time::timeout(handshake_timeout, acceptor.accept(stream)).await {
                    Ok(Ok(tls_stream)) => {
                        let _ = tx.send(Ok(tls_stream)).await;
                    }
                    Ok(Err(error)) => {
                        tracing::warn!(peer = %addr, error = %error, "TLS handshake failed");
                    }
                    Err(_) => {
                        tracing::warn!(peer = %addr, "TLS handshake timed out");
                    }
                }
            });
        }
    });

    stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|item| (item, rx))
    })
}

/// The byte streams accepted and waiting for the connection on them, by
/// their path on the relay's socket.
type Streams = Arc<Mutex<HashMap<PathId, oneshot::Sender<quinn::Connection>>>>;

/// Holds a byte stream's path until the connection on it ends, closing the
/// connection if the stream ends first. A stream that carries no
/// connection within the handshake timeout is closed.
async fn carry_stream(
    path: FramedPath,
    connection: oneshot::Receiver<quinn::Connection>,
    streams: Streams,
    handshake_timeout: Duration,
) {
    let connection = tokio::select! {
        connection = tokio::time::timeout(handshake_timeout, connection) => connection.ok().and_then(Result::ok),
        () = path.closed() => None,
    };
    lock(&streams).remove(&path.id);
    if let Some(connection) = connection {
        path.carry(connection).await;
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The relay serving: its one QUIC endpoint and the tasks accepting onto
/// it.
pub struct RelayServing {
    endpoint: LinkEndpoint,
    tasks: Vec<JoinHandle<()>>,
}

impl RelayServing {
    /// Where the relay's UDP socket is bound.
    pub fn udp_addr(&self) -> std::io::Result<std::net::SocketAddr> {
        self.endpoint.local_addr()
    }

    /// Closes every connection and stops accepting.
    pub fn close(&self) {
        self.endpoint.close(0_u32.into(), b"relay shutting down");
        for task in &self.tasks {
            task.abort();
        }
    }
}

impl Drop for RelayServing {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use agent_dir::SystemClock;
    use tokio::sync::watch;

    use super::*;
    use crate::Tier;
    use crate::link::run::spawn_connector_with_auth_establishment_and_shutdown;
    use crate::link::socket::LinkId;
    use crate::routing::{LinkConnectorAuth, LinkConnectorToken, LinkConnectorTokenRefresher};

    const NAME: &str = "relay.test";

    /// A token is `account/tier`: the account it names on the plan it
    /// names.
    struct Tokens;

    #[tonic::async_trait]
    impl LinkTokenAuthenticator for Tokens {
        async fn authenticate_token(
            &self,
            token: &str,
        ) -> Result<AuthenticatedLinkUser, tonic::Status> {
            let (user, tier) = token
                .split_once('/')
                .ok_or_else(|| tonic::Status::unauthenticated("not a token"))?;
            Ok(AuthenticatedLinkUser {
                user_id: user
                    .parse()
                    .map_err(|_| tonic::Status::unauthenticated("not an account"))?,
                client_id: "relay-test".to_owned(),
                expires_at: SystemTime::now() + Duration::from_secs(3600),
                tier: if tier == "pro" { Tier::Pro } else { Tier::Free },
            })
        }
    }

    struct NoRefresh;

    #[tonic::async_trait]
    impl LinkConnectorTokenRefresher for NoRefresh {
        async fn refresh_routing_token(&self) -> Result<LinkConnectorToken, tonic::Status> {
            Err(tonic::Status::unavailable("a test host does not refresh"))
        }
    }

    struct Running {
        server: CloudLinkServer,
        serving: RelayServing,
        tcp: std::net::SocketAddr,
        client: quinn::ClientConfig,
    }

    async fn relay() -> Running {
        relay_limited(crate::resource_limits::QUIC_HANDSHAKE_RATE_LIMIT).await
    }

    /// A relay taking `rate` handshakes a window from each source.
    async fn relay_limited(rate: usize) -> Running {
        let rcgen::CertifiedKey { cert, signing_key } =
            rcgen::generate_simple_self_signed(vec![NAME.to_owned()]).unwrap();
        let der = rustls::pki_types::CertificateDer::from(cert.der().to_vec());
        let quic = crate::transport::relay_quic_server_config_from_der(
            vec![der.clone()],
            rustls::pki_types::PrivateKeyDer::Pkcs8(rustls::pki_types::PrivatePkcs8KeyDer::from(
                signing_key.serialize_der(),
            )),
            &crate::link::resumption::SessionTickets::in_memory(
                crate::link::resumption::RELAY_TICKETS,
            ),
        )
        .unwrap();
        let mut roots = rustls::RootCertStore::empty();
        roots.add(der).unwrap();
        let client = crate::transport::relay_quic_client_config_with_roots(roots).unwrap();
        let server = CloudLinkServer::new(
            RelayIdentity {
                host_id: HostId::new_v4(),
                name: NAME.to_owned(),
                authenticator: Arc::new(Tokens),
                clock: Arc::new(SystemClock),
            },
            None,
        )
        .with_handshake_rate_limit(rate);
        let udp = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let tcp = listener.local_addr().unwrap();
        let serving = server.serve_tcp(udp, listener, quic).unwrap();
        Running {
            server,
            serving,
            tcp,
            client,
        }
    }

    /// A host's link to the relay over `carrier`, presenting `token`, up
    /// once the relay has accepted it.
    async fn link(
        id: HostId,
        carrier: Arc<QuicCarrier>,
        token: &str,
    ) -> (JoinHandle<Result<(), tonic::Status>>, watch::Sender<bool>) {
        link_in(Arc::new(LinkRegistry::new(id)), id, carrier, token).await
    }

    /// Links host `id` to the relay as [`link`] does, its links kept in
    /// `registry`.
    async fn link_in(
        registry: Arc<LinkRegistry>,
        id: HostId,
        carrier: Arc<QuicCarrier>,
        token: &str,
    ) -> (JoinHandle<Result<(), tonic::Status>>, watch::Sender<bool>) {
        let host = local_host(
            id,
            "test-host",
            Capabilities {
                cloud_relay: false,
                kinds: Vec::new(),
            },
            true,
        );
        let ctx = LinkCtx::new_live(LiveLocalHost::new(host), registry);
        let auth = LinkConnectorAuth::with_free_refresh_interval(
            LinkConnectorToken {
                token: token.to_owned(),
                expires_at: SystemTime::now() + Duration::from_secs(3600),
                fetched_at: SystemTime::now(),
                tier: Tier::Pro,
            },
            Arc::new(NoRefresh),
            None,
        );
        let (shutdown, shutdown_rx) = watch::channel(false);
        let (task, established) = spawn_connector_with_auth_establishment_and_shutdown(
            ctx,
            carrier,
            auth,
            shutdown_rx,
            None,
        );
        established.await.unwrap().unwrap();
        (task, shutdown)
    }

    /// A host on UDP and a host on TCP, both on one paid account: a probe
    /// the first sends the second through the relay, behind the header
    /// naming it, reaches it across the relay's one socket, and the
    /// answer comes back the same way.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_relay_forwards_between_a_udp_host_and_a_tcp_host_of_one_paid_account() {
        let relay = relay().await;
        let account = Uuid::new_v4();
        let udp_addr = relay.serving.udp_addr().unwrap();

        let near = HostId::new_v4();
        let endpoint = LinkEndpoint::bind("127.0.0.1:0".parse().unwrap(), None).unwrap();
        let carrier = Arc::new(
            QuicCarrier::connect_relay_with_config(&endpoint, udp_addr, NAME, relay.client.clone())
                .await
                .unwrap(),
        );
        assert_eq!(carrier.kind(), CarrierKind::RelayQuic);
        let _near = link(near, carrier, &format!("{account}/pro")).await;

        let far = HostId::new_v4();
        let tcp = tokio::net::TcpStream::connect(relay.tcp).await.unwrap();
        let stream =
            crate::link::framed::StreamEndpoint::new(tcp, FramedKind::RelayTcp, None).unwrap();
        let carrier = Arc::new(
            QuicCarrier::connect_over_stream(stream, relay.client.clone(), NAME)
                .await
                .unwrap(),
        );
        let _far = link(far, carrier, &format!("{account}/pro")).await;
        assert_eq!(
            relay.server.user_links(account).await.len(),
            2,
            "both hosts hold a link"
        );

        let through = LinkId(1);
        let via = endpoint.socket.stand_in_at(udp_addr);
        endpoint
            .socket
            .add_forwarding(through, via, crate::link::link_log::PathKind::RelayUdp);
        let mut answers = endpoint.socket.probe_answers();
        let mut answered = None;
        for nonce in 0..50 {
            endpoint
                .socket
                .probe(far, Path::Forwarded(through), nonce)
                .unwrap();
            if let Ok(Some(answer)) =
                tokio::time::timeout(Duration::from_millis(100), answers.recv()).await
            {
                answered = Some(answer);
                break;
            }
        }
        let answer = answered.expect("the probe's answer came back through the relay");
        assert!(answer.nonce < 50, "{answer:?}");
        relay.serving.close();
    }

    /// A free account's hosts reach the relay, and nothing one sends the
    /// other through it arrives: the sender is told the plan carries no
    /// connections between them.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_relay_forwards_nothing_between_hosts_of_a_free_account() {
        let relay = relay().await;
        let account = Uuid::new_v4();
        let udp_addr = relay.serving.udp_addr().unwrap();
        let mut hosts = Vec::new();
        let mut links = Vec::new();
        let mut refusals = None;
        for _ in 0..2 {
            let id = HostId::new_v4();
            let registry = Arc::new(LinkRegistry::new(id));
            refusals.get_or_insert_with(|| registry.forward_refusals());
            let endpoint = LinkEndpoint::bind("127.0.0.1:0".parse().unwrap(), None).unwrap();
            let carrier = Arc::new(
                QuicCarrier::connect_relay_with_config(
                    &endpoint,
                    udp_addr,
                    NAME,
                    relay.client.clone(),
                )
                .await
                .unwrap(),
            );
            links.push(link_in(registry, id, carrier, &format!("{account}/free")).await);
            let via = endpoint.socket.stand_in_at(udp_addr);
            endpoint.socket.add_forwarding(
                LinkId(1),
                via,
                crate::link::link_log::PathKind::RelayUdp,
            );
            hosts.push((id, endpoint));
        }
        let mut answers = hosts[0].1.socket.probe_answers();
        let mut refusals = refusals.unwrap();
        for nonce in 0..10 {
            hosts[0]
                .1
                .socket
                .probe(hosts[1].0, Path::Forwarded(LinkId(1)), nonce)
                .unwrap();
        }
        let refused = tokio::time::timeout(Duration::from_secs(10), refusals.recv())
            .await
            .expect("the relay told the sender in time")
            .unwrap();
        assert_eq!(
            refused,
            (hosts[1].0, wire::pb::ForwardRefusal::PaymentRequired)
        );
        assert!(
            answers.try_recv().is_err(),
            "no probe crossed the relay before its refusal"
        );
        relay.serving.close();
    }

    /// A host the relay validated with a Retry comes back with the token
    /// it was given and is answered at once.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_host_returning_to_the_relay_skips_its_retry() {
        let relay = relay().await;
        let udp_addr = relay.serving.udp_addr().unwrap();
        let endpoint = LinkEndpoint::bind("127.0.0.1:0".parse().unwrap(), None).unwrap();
        let first = endpoint
            .connect_with(relay.client.clone(), udp_addr, NAME)
            .unwrap()
            .await
            .unwrap();
        assert_eq!(relay.server.retries_sent(), 1, "a first dial is retried");
        patience::until("the relay's tokens to arrive", || {
            let received = first.stats().frame_rx.new_token;
            async move {
                (received >= 1)
                    .then_some(())
                    .ok_or_else(|| format!("{received} tokens"))
            }
        })
        .await
        .unwrap();
        first.close(0_u32.into(), b"done");
        let second = endpoint
            .connect_with(relay.client.clone(), udp_addr, NAME)
            .unwrap()
            .await
            .unwrap();
        assert_eq!(
            relay.server.retries_sent(),
            1,
            "the returning host presented its token instead"
        );
        second.close(0_u32.into(), b"done");
        relay.serving.close();
    }

    /// A relaunched host presents the token the relay gave its previous
    /// process, kept in its profile, and is answered at once.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_relaunched_host_presents_the_relays_validation_token_from_disk() {
        let relay = relay().await;
        let udp_addr = relay.serving.udp_addr().unwrap();
        let profile = tempfile::tempdir().unwrap();
        let process = || {
            LinkEndpoint::bind("127.0.0.1:0".parse().unwrap(), None)
                .unwrap()
                .keeping(profile.path())
        };
        let first = process()
            .connect_to(
                relay.client.clone(),
                udp_addr,
                crate::link::validation::Server::Relay(NAME.to_owned()),
                NAME,
            )
            .unwrap()
            .await
            .unwrap();
        assert_eq!(relay.server.retries_sent(), 1, "a first dial is retried");
        patience::until("the relay's tokens to arrive", || {
            let received = first.stats().frame_rx.new_token;
            async move {
                (received >= 1)
                    .then_some(())
                    .ok_or_else(|| format!("{received} tokens"))
            }
        })
        .await
        .unwrap();
        first.close(0_u32.into(), b"done");

        let relaunched = process();
        let carrier = QuicCarrier::connect_relay_with_config(
            &relaunched,
            udp_addr,
            NAME,
            relay.client.clone(),
        )
        .await
        .unwrap();
        assert_eq!(
            relay.server.retries_sent(),
            1,
            "the relaunched host presented the token its last process kept"
        );
        carrier.close(wire::pb::LinkCloseReason::UserShutdown);
        relay.serving.close();
    }

    /// A host whose NAT gives it a new port comes back from there with
    /// the token it was given at the old one and is answered at once: the
    /// relay's QUIC sees senders from one IP at one stand-in IP, which is
    /// all a token names.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_host_returning_from_a_new_port_skips_its_retry() {
        let relay = relay().await;
        let udp_addr = relay.serving.udp_addr().unwrap();
        let endpoint = LinkEndpoint::bind("127.0.0.1:0".parse().unwrap(), None).unwrap();
        let first = endpoint
            .connect_with(relay.client.clone(), udp_addr, NAME)
            .unwrap()
            .await
            .unwrap();
        patience::until("the relay's tokens to arrive", || {
            let received = first.stats().frame_rx.new_token;
            async move {
                (received >= 1)
                    .then_some(())
                    .ok_or_else(|| format!("{received} tokens"))
            }
        })
        .await
        .unwrap();
        first.close(0_u32.into(), b"done");
        let old_port = endpoint.socket.local_addr().unwrap();
        endpoint
            .socket
            .rebind(LinkSocket::on_udp(std::net::UdpSocket::bind("127.0.0.1:0").unwrap()).unwrap());
        assert_ne!(endpoint.socket.local_addr().unwrap(), old_port);
        let second = endpoint
            .connect_with(relay.client.clone(), udp_addr, NAME)
            .unwrap()
            .await
            .unwrap();
        assert_eq!(
            relay.server.retries_sent(),
            1,
            "the host presented its token from the new port"
        );
        second.close(0_u32.into(), b"done");
        relay.serving.close();
    }

    /// A host presenting a token skips the Retry but not the relay's
    /// per-source limit: its handshake counts as an attempt, so a source
    /// that spent its attempts is not taken on again however it proves its
    /// address.
    #[tokio::test(flavor = "multi_thread")]
    async fn relay_admission_counts_a_validated_handshake_against_the_limit() {
        let relay = relay_limited(2).await;
        let udp_addr = relay.serving.udp_addr().unwrap();
        let endpoint = LinkEndpoint::bind("127.0.0.1:0".parse().unwrap(), None).unwrap();
        let first = endpoint
            .connect_with(relay.client.clone(), udp_addr, NAME)
            .unwrap()
            .await
            .unwrap();
        patience::until("the relay's tokens to arrive", || {
            let received = first.stats().frame_rx.new_token;
            async move {
                (received >= 2)
                    .then_some(())
                    .ok_or_else(|| format!("{received} tokens"))
            }
        })
        .await
        .unwrap();
        first.close(0_u32.into(), b"done");
        // The retried attempt was the first; a token is the second.
        let second = endpoint
            .connect_with(relay.client.clone(), udp_addr, NAME)
            .unwrap()
            .await
            .unwrap();
        second.close(0_u32.into(), b"done");
        let third = endpoint
            .connect_with(relay.client.clone(), udp_addr, NAME)
            .unwrap();
        patience::until("the relay to drop the third attempt", || {
            let limited = relay.server.handshakes_limited();
            async move {
                (limited >= 1)
                    .then_some(())
                    .ok_or_else(|| format!("{limited} dropped"))
            }
        })
        .await
        .unwrap();
        assert_eq!(
            relay.server.handshakes_limited(),
            1,
            "only the third attempt from the source is not taken on"
        );
        drop(third);
        assert_eq!(
            relay.server.retries_sent(),
            1,
            "both later dials had tokens"
        );
        relay.serving.close();
    }

    /// What the relay tells a host of where it sees it, read off the
    /// host's own control stream.
    mod addresses {
        use super::*;
        use crate::link::{read_message, write_message};

        /// A host's raw link to the relay, as `token`: the relay's answer
        /// to its Hello and the control stream after it.
        async fn hello(
            relay: &Running,
            endpoint: &LinkEndpoint,
            token: &str,
        ) -> (
            Arc<QuicCarrier>,
            wire::pb::HelloAccepted,
            crate::link::ControlSink,
            crate::link::ControlSource,
        ) {
            let carrier = Arc::new(
                QuicCarrier::connect_relay_with_config(
                    endpoint,
                    relay.serving.udp_addr().unwrap(),
                    NAME,
                    relay.client.clone(),
                )
                .await
                .unwrap(),
            );
            let (mut sink, mut source) = carrier.control();
            let host = local_host(HostId::new_v4(), "test-host", Capabilities::default(), true);
            write_message(
                &mut sink,
                &wire::pb::Message {
                    body: Some(wire::pb::message::Body::Hello(wire::pb::Hello {
                        supported_protocol_versions: vec![crate::PROTOCOL_VERSION],
                        host: Some(crate::routing::host_to_wire(&host)),
                        auth_token: Some(token.to_owned()),
                        runtime_instance_id: crate::routing::RuntimeInstance::random().to_wire(),
                        ..Default::default()
                    })),
                },
            )
            .await
            .unwrap();
            let Some(wire::pb::message::Body::HelloAck(wire::pb::HelloAck {
                outcome: Some(wire::pb::hello_ack::Outcome::Accepted(accepted)),
            })) = read_message(&mut source).await.unwrap().unwrap().body
            else {
                panic!("the relay accepts the host");
            };
            (carrier, accepted, sink, source)
        }

        /// The next ObservedAddress the relay sends within `patience`.
        async fn observed(
            source: &mut crate::link::ControlSource,
            patience: Duration,
        ) -> Option<String> {
            tokio::time::timeout(patience, async {
                loop {
                    match read_message(source).await.unwrap()?.body {
                        Some(wire::pb::message::Body::ObservedAddress(observed)) => {
                            return Some(observed.addr);
                        }
                        _ => continue,
                    }
                }
            })
            .await
            .ok()
            .flatten()
        }

        /// The host moves to a new port, and says so by sending from it.
        async fn move_host(endpoint: &LinkEndpoint, sink: &mut crate::link::ControlSink) {
            endpoint
                .rebind(
                    LinkSocket::on_udp(std::net::UdpSocket::bind("127.0.0.1:0").unwrap()).unwrap(),
                )
                .unwrap();
            write_message(
                sink,
                &wire::pb::Message {
                    body: Some(wire::pb::message::Body::Addresses(
                        wire::pb::Addresses::default(),
                    )),
                },
            )
            .await
            .unwrap();
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn a_paid_host_is_told_where_the_relay_sees_it_and_told_again_when_that_moves() {
            let relay = relay().await;
            let endpoint = LinkEndpoint::bind("127.0.0.1:0".parse().unwrap(), None).unwrap();
            let (_carrier, accepted, mut sink, mut source) =
                hello(&relay, &endpoint, &format!("{}/pro", Uuid::new_v4())).await;
            let first = endpoint.local_addr().unwrap().to_string();
            assert_eq!(accepted.observed_addr.as_deref(), Some(first.as_str()));
            assert!(
                accepted.addrs.is_empty(),
                "the relay lists nothing of its own"
            );
            assert_eq!(
                observed(&mut source, OBSERVED_PATIENCE).await,
                None,
                "nothing moved"
            );

            move_host(&endpoint, &mut sink).await;
            let moved = endpoint.local_addr().unwrap().to_string();
            assert_ne!(moved, first);
            assert_eq!(
                observed(&mut source, OBSERVED_PATIENCE).await.as_deref(),
                Some(moved.as_str())
            );
            relay.serving.close();
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn a_free_host_is_never_told_where_the_relay_sees_it() {
            let relay = relay().await;
            let endpoint = LinkEndpoint::bind("127.0.0.1:0".parse().unwrap(), None).unwrap();
            let (_carrier, accepted, mut sink, mut source) =
                hello(&relay, &endpoint, &format!("{}/free", Uuid::new_v4())).await;
            assert_eq!(accepted.observed_addr, None);
            move_host(&endpoint, &mut sink).await;
            assert_eq!(observed(&mut source, OBSERVED_PATIENCE).await, None);
            relay.serving.close();
        }

        /// Past the relay's next look at where its hosts are.
        const OBSERVED_PATIENCE: Duration = Duration::from_millis(4_500);
    }

    /// Two of the relay curve's hosts on one paid account, each linked over
    /// TCP: one dials the other through its link to the relay, and a stream
    /// on that end-to-end link carries bytes both ways.
    #[tokio::test(flavor = "multi_thread")]
    async fn relay_hosts_reach_each_other_on_a_link_the_relay_forwards() {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        let relay = relay().await;
        let account = Uuid::new_v4();
        let mut hosts = Vec::new();
        for _ in 0..2 {
            let tcp = TcpStream::connect(relay.tcp).await.unwrap();
            hosts.push(
                crate::link::relay_host::RelayHost::connect(
                    tcp,
                    relay.client.clone(),
                    NAME,
                    format!("{account}/pro"),
                    SystemTime::now() + Duration::from_secs(3600),
                )
                .await
                .unwrap(),
            );
        }
        hosts[0].trust(&hosts[1]).unwrap();
        hosts[1].trust(&hosts[0]).unwrap();
        tokio::time::timeout(Duration::from_secs(10), hosts[0].link_to(hosts[1].id()))
            .await
            .expect("the link through the relay came up in time")
            .unwrap();

        let mut opened = hosts[1].open(hosts[0].id()).await.unwrap();
        opened.write_all(b"ping").await.unwrap();
        opened.flush().await.unwrap();
        let mut accepted = tokio::time::timeout(Duration::from_secs(10), hosts[0].accept())
            .await
            .expect("the stream arrived in time")
            .unwrap();
        let mut read = [0_u8; 4];
        accepted.read_exact(&mut read).await.unwrap();
        assert_eq!(&read, b"ping");
        accepted.write_all(b"pong").await.unwrap();
        accepted.flush().await.unwrap();
        opened.read_exact(&mut read).await.unwrap();
        assert_eq!(&read, b"pong");
        relay.serving.close();
    }
}
