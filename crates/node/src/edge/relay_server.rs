//! The relay a cloud deployment runs: it accepts links from every host of
//! an account, authenticates each by its connection token, advertises each
//! host's adjacency to the others (NeighborUp/Down) and forwards a stream to
//! the host its preface names. It never terminates a host's streams: the
//! pinned handshake inside each stream is end to end between the two hosts.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use futures_util::{Stream, StreamExt, stream};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{RwLock, Semaphore, mpsc};
use tokio::task::JoinHandle;
use tokio_rustls::TlsAcceptor;
use uuid::Uuid;

use crate::auth::jwt::JwtValidator;
use crate::link::piper::Relaying;
use crate::link::{CarrierKind, ChannelPool, MuxCarrier, MuxRole, QuicCarrier, run_link};
use crate::resource_limits::{
    Admission, EXTERNAL_QUIC_TLS_HANDSHAKE_CONCURRENCY, EXTERNAL_QUIC_TLS_HANDSHAKE_RATE_LIMIT,
    EXTERNAL_QUIC_TLS_HANDSHAKE_RATE_WINDOW, QUIC_RETRY_REPEAT_INTERVAL, RetryAdmission,
};
use crate::routing::{
    AuthenticatedLinkContextProvider, AuthenticatedLinkUser, Capabilities, LinkCtx, LinkRegistry,
    LinkTokenAuthenticator, LiveLocalHost, RoutingCore, local_host,
};
use crate::transport::tcp_incoming;
use crate::{Clock, HostId};

const CLOUD_TLS_HANDSHAKE_CONCURRENCY: usize = 128;
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
    /// The one core every account's streams are admitted by: accounts are
    /// kept apart by its adjacency, not by a core each.
    relaying: Arc<Relaying>,
}

/// One account's side of the relay: the links its hosts hold here and the
/// adjacency they advertise to each other. Accounts never see each other.
struct RelayUser {
    routing: Arc<RoutingCore>,
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
            routing: Arc::new(RoutingCore::new()),
            channels: Arc::new(ChannelPool::new(links)),
            incoming_streams_tx,
            drain,
        }
    }

    fn link_ctx(&self, local_host: &LiveLocalHost, clock: &Arc<dyn Clock>) -> LinkCtx {
        LinkCtx::new_live(
            local_host.clone(),
            self.routing.clone(),
            self.channels.link_registry(),
        )
        .with_incoming_streams(self.incoming_streams_tx.clone())
        .with_clock(clock.clone())
    }
}

impl CloudLinkServer {
    pub fn new(identity: RelayIdentity) -> Self {
        let local_host = LiveLocalHost::new(local_host(
            identity.host_id,
            &identity.name,
            Capabilities {
                cloud_relay: true,
                kinds: Vec::new(),
            },
            false,
        ));
        Self {
            inner: Arc::new(CloudLinkServerInner {
                relaying: Relaying::new(identity.host_id),
                identity,
                local_host,
                users: RwLock::new(HashMap::new()),
            }),
        }
    }

    /// Serves plain TCP: a relay behind a TLS terminator, or a test.
    pub fn serve_on_tcp_listener(&self, listener: TcpListener) -> JoinHandle<()> {
        spawn_cloud_carrier_server(self.clone(), tcp_incoming(listener))
    }

    pub fn serve_on_tls_tcp_listener(
        &self,
        listener: TcpListener,
        acceptor: TlsAcceptor,
        handshake_timeout: Duration,
    ) -> JoinHandle<()> {
        let incoming = cloud_tls_incoming(listener, acceptor, handshake_timeout);
        spawn_cloud_carrier_server(self.clone(), incoming)
    }

    pub fn serve_on_quic_endpoint(
        &self,
        endpoint: quinn::Endpoint,
        handshake_timeout: Duration,
    ) -> JoinHandle<()> {
        self.serve_on_quic_endpoint_with_admission(
            endpoint,
            handshake_timeout,
            Arc::new(Semaphore::new(EXTERNAL_QUIC_TLS_HANDSHAKE_CONCURRENCY)),
        )
    }

    fn serve_on_quic_endpoint_with_admission(
        &self,
        endpoint: quinn::Endpoint,
        handshake_timeout: Duration,
        slots: Arc<Semaphore>,
    ) -> JoinHandle<()> {
        let service = self.clone();
        let admission = Arc::new(tokio::sync::Mutex::new(RetryAdmission::new(
            EXTERNAL_QUIC_TLS_HANDSHAKE_RATE_LIMIT,
            EXTERNAL_QUIC_TLS_HANDSHAKE_RATE_WINDOW,
            QUIC_RETRY_REPEAT_INTERVAL,
        )));
        tokio::spawn(async move {
            let mut connection_tasks = tokio::task::JoinSet::new();
            loop {
                let incoming = tokio::select! {
                    incoming = endpoint.accept() => incoming,
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
                let addr = incoming.remote_address();

                if !incoming.remote_address_validated() {
                    match admission
                        .lock()
                        .await
                        .admit(addr.ip(), incoming.orig_dst_cid())
                    {
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
                    let _permit = permit;
                    let connection = match tokio::time::timeout(handshake_timeout, incoming).await {
                        Ok(Ok(connection)) => connection,
                        Ok(Err(error)) => {
                            tracing::warn!(peer = %addr, error = %error, "cloud QUIC handshake failed");
                            return;
                        }
                        Err(_) => {
                            tracing::warn!(peer = %addr, "cloud QUIC handshake timed out");
                            return;
                        }
                    };
                    drop(_permit);
                    let ctx = service.accepting_link_ctx().await;
                    let carrier = Arc::new(QuicCarrier::from_accepted_with_kind(
                        connection,
                        CarrierKind::RelayQuic,
                    ));
                    if let Err(error) =
                        run_link(ctx, carrier, crate::routing::ConnectRole::Acceptor).await
                    {
                        tracing::warn!(peer = %addr, error = %error, "cloud QUIC link exited with error");
                    }
                });
            }
            connection_tasks.detach_all();
        })
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
            Arc::new(RoutingCore::new()),
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

fn spawn_cloud_carrier_server<I, IO>(service: CloudLinkServer, incoming: I) -> JoinHandle<()>
where
    I: Stream<Item = Result<IO, std::io::Error>> + Send + 'static,
    IO: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut incoming = Box::pin(incoming);
        while let Some(item) = incoming.next().await {
            match item {
                Ok(io) => {
                    let service = service.clone();
                    tokio::spawn(async move {
                        let ctx = service.accepting_link_ctx().await;
                        let carrier = Arc::new(MuxCarrier::new(
                            io,
                            MuxRole::Acceptor,
                            CarrierKind::RelayTcp,
                        ));
                        if let Err(error) =
                            run_link(ctx, carrier, crate::routing::ConnectRole::Acceptor).await
                        {
                            tracing::warn!(error = %error, "cloud link exited with error");
                        }
                    });
                }
                Err(error) => tracing::warn!(error = %error, "cloud stream accept failed"),
            }
        }
    })
}
