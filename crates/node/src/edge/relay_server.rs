//! The relay a cloud deployment runs: it accepts links from every host of
//! an account, authenticates each by its connection token, advertises each
//! host's adjacency to the others (NeighborUp/Down) and forwards the packets
//! a host sends behind a header naming another host of its account, while
//! both are paid. It reads nothing of what it forwards: the link between
//! the two hosts is their own QUIC connection, pinned at pairing.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
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
use crate::resource_limits::{
    Admission, EXTERNAL_QUIC_TLS_HANDSHAKE_CONCURRENCY, EXTERNAL_QUIC_TLS_HANDSHAKE_RATE_LIMIT,
    EXTERNAL_QUIC_TLS_HANDSHAKE_RATE_WINDOW, QUIC_RETRY_REPEAT_INTERVAL, RetryAdmission,
};
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
    retries: AtomicU64,
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
                retries: AtomicU64::new(0),
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
        let slots = Arc::new(Semaphore::new(EXTERNAL_QUIC_TLS_HANDSHAKE_CONCURRENCY));
        let mut admission = RetryAdmission::new(
            EXTERNAL_QUIC_TLS_HANDSHAKE_RATE_LIMIT,
            EXTERNAL_QUIC_TLS_HANDSHAKE_RATE_WINDOW,
            QUIC_RETRY_REPEAT_INTERVAL,
        );
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
                if stream.is_none() && !incoming.remote_address_validated() {
                    match admission.admit(addr.ip(), incoming.orig_dst_cid()) {
                        Admission::Retry => {}
                        Admission::Repeat => {
                            incoming.ignore();
                            continue;
                        }
                        Admission::Refused => {
                            tracing::warn!(peer = %addr, "cloud QUIC handshake rate limit exceeded");
                            incoming.ignore();
                            continue;
                        }
                    }
                    service.inner.retries.fetch_add(1, Ordering::Relaxed);
                    if let Err(error) = incoming.retry() {
                        tracing::debug!(peer = %addr, error = %error, "cloud QUIC address was already validated");
                        error.into_incoming().ignore();
                    }
                    continue;
                }
                let permit = match slots.clone().try_acquire_owned() {
                    Ok(permit) => permit,
                    Err(_) => {
                        tracing::warn!(peer = %addr, "cloud QUIC handshake concurrency limit exceeded");
                        incoming.refuse();
                        continue;
                    }
                };
                let service = service.clone();
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
                    let ctx = service.accepting_link_ctx().await;
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

    /// How many Retries the relay has answered unvalidated Initials with.
    #[doc(hidden)]
    pub fn retries_sent(&self) -> u64 {
        self.inner.retries.load(Ordering::Relaxed)
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
            crate::transport::configure_relay_tcp_keepalive(&stream);
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
    use crate::link::LinkCarrier as _;
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
        let rcgen::CertifiedKey { cert, signing_key } =
            rcgen::generate_simple_self_signed(vec![NAME.to_owned()]).unwrap();
        let der = rustls::pki_types::CertificateDer::from(cert.der().to_vec());
        let quic = crate::transport::relay_quic_server_config_from_der(
            vec![der.clone()],
            rustls::pki_types::PrivateKeyDer::Pkcs8(rustls::pki_types::PrivatePkcs8KeyDer::from(
                signing_key.serialize_der(),
            )),
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
        );
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
        let host = local_host(
            id,
            "test-host",
            Capabilities {
                cloud_relay: false,
                kinds: Vec::new(),
            },
            true,
        );
        let ctx = LinkCtx::new_live(LiveLocalHost::new(host), Arc::new(LinkRegistry::new(id)));
        let auth = LinkConnectorAuth::with_free_refresh_interval(
            LinkConnectorToken {
                token: token.to_owned(),
                expires_at: SystemTime::now() + Duration::from_secs(3600),
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
    /// other through it arrives.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_relay_forwards_nothing_between_hosts_of_a_free_account() {
        let relay = relay().await;
        let account = Uuid::new_v4();
        let udp_addr = relay.serving.udp_addr().unwrap();
        let mut hosts = Vec::new();
        let mut links = Vec::new();
        for _ in 0..2 {
            let id = HostId::new_v4();
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
            links.push(link(id, carrier, &format!("{account}/free")).await);
            let via = endpoint.socket.stand_in_at(udp_addr);
            endpoint.socket.add_forwarding(
                LinkId(1),
                via,
                crate::link::link_log::PathKind::RelayUdp,
            );
            hosts.push((id, endpoint));
        }
        let mut answers = hosts[0].1.socket.probe_answers();
        for nonce in 0..10 {
            hosts[0]
                .1
                .socket
                .probe(hosts[1].0, Path::Forwarded(LinkId(1)), nonce)
                .unwrap();
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(500), answers.recv())
                .await
                .is_err(),
            "no probe crossed the relay"
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
}
