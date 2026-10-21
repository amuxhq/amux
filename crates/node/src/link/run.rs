//! Carrier-independent link control runtime.
//!
//! The connector opens the control stream and sends `Hello`; the acceptor
//! answers with `HelloAck`. The handshake carries the current adjacency
//! snapshot, and registry deltas, reauthentication, and orderly shutdown all
//! continue on that same length-prefixed protobuf stream.

use std::future::{self, Future};
use std::pin::Pin;
use std::sync::{Arc, RwLock as StdRwLock};
use std::time::{Duration, SystemTime};

use agent_dir::SystemClock;
use bytes::Bytes;
use tokio::sync::{mpsc, oneshot, watch};
use uuid::Uuid;
use wire::{self, PROTOCOL_VERSION};

use super::link_log::{EarlyData, LinkEvent, LinkTransition, PathKind};
use super::{LinkCarrier as Carrier, read_message, write_message};
use crate::net_error::{self, status as protocol_status};
use crate::routing::{
    ConnectHandshake, ConnectHandshakeEvent, Host, LinkAdmission, LinkCarrier as RoutingCarrier,
    LinkCloseRequest, LinkId, LinkProperties, LinkRegistry, LinkRole, LiveLocalHost, Registration,
    RuntimeInstance, core_host, host_from_wire, host_to_wire, inbound_host_from_wire,
    neighbor_down_from_wire, neighbor_up_from_wire, protocol_error_hello_ack,
    protocol_error_link_close, validate_remote_host,
};
use crate::{Clock, HostId, audit};

const LINK_CONNECT_HELLO_TIMEOUT: Duration = Duration::from_secs(10);

/// How often a relay looks again at where a paid host's packets come from,
/// to tell it when that moved. quinn says nothing when a connection's
/// address changes, so the relay reads it; a host it moved for learns
/// within this long.
const OBSERVE_EVERY: Duration = Duration::from_secs(2);

#[derive(Debug, thiserror::Error)]
pub enum LinkError {
    #[error("link control I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("link rejected: {0}")]
    Status(#[from] tonic::Status),
}

#[derive(Debug, Clone)]
pub struct AuthenticatedLinkUser {
    pub user_id: Uuid,
    pub client_id: String,
    pub expires_at: SystemTime,
    pub tier: crate::Tier,
}

#[tonic::async_trait]
pub trait LinkTokenAuthenticator: Send + Sync + 'static {
    async fn authenticate_token(&self, token: &str)
    -> Result<AuthenticatedLinkUser, tonic::Status>;
}

#[tonic::async_trait]
pub(crate) trait AuthenticatedLinkContextProvider: Send + Sync + 'static {
    async fn context_for(&self, user: &AuthenticatedLinkUser) -> LinkCtx;
}

#[tonic::async_trait]
impl<T> LinkTokenAuthenticator for Arc<T>
where
    T: LinkTokenAuthenticator + ?Sized,
{
    async fn authenticate_token(
        &self,
        token: &str,
    ) -> Result<AuthenticatedLinkUser, tonic::Status> {
        (**self).authenticate_token(token).await
    }
}

#[derive(Clone)]
pub struct LinkAuthSession {
    user: Arc<StdRwLock<AuthenticatedLinkUser>>,
    authenticator: Arc<dyn LinkTokenAuthenticator>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReauthError {
    pub original_user_id: Uuid,
    pub reauth_user_id: Uuid,
}

impl LinkAuthSession {
    pub fn new<T>(user: AuthenticatedLinkUser, authenticator: T) -> Self
    where
        T: LinkTokenAuthenticator,
    {
        Self {
            user: Arc::new(StdRwLock::new(user)),
            authenticator: Arc::new(authenticator),
        }
    }

    pub fn tier(&self) -> crate::Tier {
        self.user().tier
    }

    pub fn apply_reauth(&self, user: AuthenticatedLinkUser) -> Result<(), ReauthError> {
        let mut current = self
            .user
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if current.user_id != user.user_id {
            return Err(ReauthError {
                original_user_id: current.user_id,
                reauth_user_id: user.user_id,
            });
        }
        *current = user;
        Ok(())
    }

    fn user(&self) -> AuthenticatedLinkUser {
        self.user
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

#[derive(Debug, Clone)]
pub struct LinkConnectorToken {
    pub token: String,
    pub expires_at: SystemTime,
    /// When the account service issued it, on the link's clock.
    pub fetched_at: SystemTime,
    pub tier: crate::Tier,
}

pub(crate) struct LinkConnectorRefreshRequest {
    pub(crate) response: oneshot::Sender<Result<crate::Tier, tonic::Status>>,
}

pub(crate) type LinkConnectorRefreshReceiver =
    Arc<tokio::sync::Mutex<mpsc::Receiver<LinkConnectorRefreshRequest>>>;

#[tonic::async_trait]
pub trait LinkConnectorTokenRefresher: Send + Sync + 'static {
    async fn refresh_routing_token(&self) -> Result<LinkConnectorToken, tonic::Status>;
}

/// The credential a connector presents to the relay, and what refreshes it.
/// When it is refreshed is the profile core's to decide: the link reports
/// each credential it presents, and refreshes when the core asks.
#[derive(Clone)]
pub struct LinkConnectorAuth {
    token: LinkConnectorToken,
    refresher: Arc<dyn LinkConnectorTokenRefresher>,
    free_refresh_interval: Option<Duration>,
    on_refreshed: Option<Arc<dyn Fn(crate::Tier) + Send + Sync>>,
}

impl LinkConnectorAuth {
    pub fn with_free_refresh_interval(
        token: LinkConnectorToken,
        refresher: Arc<dyn LinkConnectorTokenRefresher>,
        free_refresh_interval: Option<Duration>,
    ) -> Self {
        Self {
            token,
            refresher,
            free_refresh_interval,
            on_refreshed: None,
        }
    }

    pub(crate) fn with_refresh_observer(
        mut self,
        observer: impl Fn(crate::Tier) + Send + Sync + 'static,
    ) -> Self {
        self.on_refreshed = Some(Arc::new(observer));
        self
    }

    /// The credential presented on `link`, as the profile's core hears it.
    fn credential(&self, link: LinkId) -> daemon_core::Input {
        let free_every = match self.token.tier {
            crate::Tier::Free => self.free_refresh_interval,
            _ => None,
        };
        daemon_core::Input::Credential {
            link: link.core(),
            expires_at_ms: system_time_ms(self.token.expires_at).max(0) as u64,
            fetched_at_ms: system_time_ms(self.token.fetched_at).max(0) as u64,
            free_every_ms: free_every.map(|every| every.as_millis() as u64),
        }
    }

    async fn refresh(&mut self) -> Result<(wire::pb::Message, crate::Tier), tonic::Status> {
        let token = self.refresher.refresh_routing_token().await?;
        let message = wire::pb::Message {
            body: Some(wire::pb::message::Body::Reauth(wire::pb::Reauth {
                auth_token: token.token.clone(),
            })),
        };
        let tier = token.tier;
        self.token = token;
        Ok((message, tier))
    }

    /// The fresh credential is on its way to the relay: only now is the
    /// plan it carries the link's, so a call made on hearing of it finds
    /// the relay already judging by it.
    fn presented(&self, link: LinkId, ctx: &LinkCtx) {
        if let Some(observer) = &self.on_refreshed {
            observer(self.token.tier);
        }
        ctx.report(self.credential(link));
    }
}

pub(crate) fn system_time_ms(time: SystemTime) -> i64 {
    time.duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as i64)
}

/// Everything the control runtime needs from its owner.
#[derive(Clone)]
pub struct LinkCtx {
    local_host: LiveLocalHost,
    links: Arc<LinkRegistry>,
    incoming_streams_tx: Option<mpsc::Sender<super::InboundStream>>,
    expected_peer: Option<HostId>,
    authenticated_peer: Option<HostId>,
    /// The key a resumed handshake's ticket names, which no certificate
    /// proved this time: the core decides it before the link answers.
    resumed: Option<Vec<u8>>,
    link_role: LinkRole,
    routing_carrier: RoutingCarrier,
    acceptor_session: Option<LinkAuthSession>,
    authenticator: Option<Arc<dyn LinkTokenAuthenticator>>,
    authenticated_context_provider: Option<Arc<dyn AuthenticatedLinkContextProvider>>,
    clock: Arc<dyn Clock>,
    connector_auth: Option<LinkConnectorAuth>,
    established_tx: Option<Arc<StdRwLock<Option<EstablishmentSender>>>>,
    shutdown_rx: Option<watch::Receiver<bool>>,
    refresh_rx: Option<LinkConnectorRefreshReceiver>,
    /// On a relay: the socket whose senders' addresses a paid host is told
    /// it is seen at.
    observes: Option<Arc<super::socket::LinkSocket>>,
}

pub(crate) type LinkConnectorCtx = LinkCtx;
type ConnectorTask = tokio::task::JoinHandle<Result<(), tonic::Status>>;
type EstablishmentSender = oneshot::Sender<Result<Host, tonic::Status>>;
pub(crate) type EstablishmentReceiver = oneshot::Receiver<Result<Host, tonic::Status>>;

impl LinkCtx {
    #[cfg(test)]
    pub fn new(local_host: Host, links: Arc<LinkRegistry>) -> Self {
        Self::new_live(LiveLocalHost::new(local_host), links)
    }

    pub(crate) fn new_live(local_host: LiveLocalHost, links: Arc<LinkRegistry>) -> Self {
        Self {
            local_host,
            links,
            incoming_streams_tx: None,
            expected_peer: None,
            authenticated_peer: None,
            resumed: None,
            link_role: LinkRole::Peer,
            routing_carrier: RoutingCarrier::Direct,
            acceptor_session: None,
            authenticator: None,
            authenticated_context_provider: None,
            clock: Arc::new(SystemClock),
            connector_auth: None,
            established_tx: None,
            shutdown_rx: None,
            refresh_rx: None,
            observes: None,
        }
    }

    /// Tells the profile's core, when the links are a profile's, what a
    /// link did or what its peer said it reaches.
    fn report(&self, input: daemon_core::Input) {
        if let Some(core) = self.links.core() {
            core.report(input);
        }
    }

    /// Hands a control frame from the peer to the core.
    fn report_frame(&self, link: LinkId, body: wire::pb::message::Body) {
        self.report(daemon_core::Input::LinkControl {
            link: link.core(),
            frame: wire::pb::Message { body: Some(body) },
        });
    }

    pub fn with_expected_peer(mut self, peer: HostId) -> Self {
        self.expected_peer = Some(peer);
        self
    }

    pub fn with_authenticated_peer(mut self, peer: HostId) -> Self {
        self.authenticated_peer = Some(peer);
        self
    }

    /// The link resumed a session whose ticket names `key`. Its peer is the
    /// host its Hello names, and the core decides whether that host's pin
    /// is `key` before the link answers the Hello or serves a stream.
    pub(crate) fn with_resumed(mut self, key: Vec<u8>) -> Self {
        self.resumed = Some(key);
        self
    }

    pub fn with_link_role(mut self, role: LinkRole) -> Self {
        self.link_role = role;
        self
    }

    pub fn with_carrier(mut self, carrier: RoutingCarrier) -> Self {
        self.routing_carrier = carrier;
        self
    }

    pub(crate) fn with_incoming_streams(
        mut self,
        sender: mpsc::Sender<super::InboundStream>,
    ) -> Self {
        self.incoming_streams_tx = Some(sender);
        self
    }

    /// Tells each paid host linked on `socket` the address its packets
    /// come from: what a relay does, so its hosts can punch.
    pub(crate) fn observing(mut self, socket: Arc<super::socket::LinkSocket>) -> Self {
        self.observes = Some(socket);
        self
    }

    /// The address a host linked over `carrier` is told its packets come
    /// from: only on a relay, only for a paid host, and only where one
    /// address carries it, which a byte stream's packets do not.
    fn observed(
        &self,
        carrier: &dyn Carrier,
        session: Option<&LinkAuthSession>,
    ) -> Option<std::net::SocketAddr> {
        let socket = self.observes.as_ref()?;
        if session?.tier() != crate::Tier::Pro {
            return None;
        }
        socket.address_of(carrier.remote_address()?)
    }

    pub(crate) fn with_shutdown(mut self, shutdown_rx: watch::Receiver<bool>) -> Self {
        self.shutdown_rx = Some(shutdown_rx);
        self
    }

    pub fn with_token_authenticator(
        mut self,
        authenticator: Arc<dyn LinkTokenAuthenticator>,
    ) -> Self {
        self.authenticator = Some(authenticator);
        self
    }

    /// The policy clock an acceptor measures a caller's credential against.
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    pub(crate) fn with_authenticated_context_provider(
        mut self,
        provider: Arc<dyn AuthenticatedLinkContextProvider>,
    ) -> Self {
        self.authenticated_context_provider = Some(provider);
        self
    }

    fn take_established_tx(&self) -> Option<EstablishmentSender> {
        self.established_tx
            .as_ref()
            .and_then(|slot| slot.write().ok()?.take())
    }
}

/// Runs one complete link for either side of any `LinkCarrier`.
pub async fn run_link(
    mut ctx: LinkCtx,
    carrier: Arc<dyn Carrier>,
    role: crate::routing::ConnectRole,
) -> Result<(), LinkError> {
    let (mut sink, mut source) = carrier.control();
    let mut handshake = ConnectHandshake::new(role);
    // A host lists where it can be reached to its peers, and to the relay
    // nothing: punching needs only what the relay sees.
    let mut addresses = ctx
        .connector_auth
        .is_none()
        .then(|| ctx.local_host.addresses());
    let own = addresses
        .as_mut()
        .map(|addresses| addresses.borrow_and_update().clone())
        .unwrap_or_default();
    let mut observed = None;
    // The peer's hello or its answer to ours, as the core is told it.
    let peer_frame;
    let mut held_ack = None;
    let ((peer_host, peer_instance), peer_neighbors, snapshot) = match role {
        crate::routing::ConnectRole::Connector => {
            let snapshot = neighbor_snapshot(&ctx, carrier.as_ref()).await;
            write_message(&mut sink, &connector_hello(&ctx, &snapshot, &own)).await?;
            tracing::debug!(path = %carrier.path_stats(), "sent Hello");
            let first = read_first(&mut source, "HelloAck").await?;
            tracing::debug!(path = %carrier.path_stats(), "received the first message after Hello");
            if let Some(wire::pb::message::Body::LinkClose(close)) = first.body.as_ref() {
                let status = link_close_status(close).unwrap_or_else(|| {
                    // The host refused the identity a resumed handshake's
                    // ticket named: it no longer trusts this one.
                    if close.reason == wire::pb::LinkCloseReason::UserRevoked as i32 {
                        return super::ChannelError::Revoked.status();
                    }
                    tonic::Status::unavailable("link closed during authentication")
                });
                return Err(status.into());
            }
            peer_frame = first.body.clone();
            match handshake.receive(first) {
                Ok(ConnectHandshakeEvent::Accepted(accepted)) => {
                    let (peer, neighbors, runtime_instance) =
                        accept_peer_hello_ack(&ctx, accepted).map_err(protocol_status)?;
                    ((peer, runtime_instance), neighbors, snapshot)
                }
                Ok(ConnectHandshakeEvent::Rejected(error)) => {
                    return Err(protocol_status(error).into());
                }
                Ok(_) => return Err(tonic::Status::invalid_argument("expected HelloAck").into()),
                Err(error) => return Err(tonic::Status::invalid_argument(error.to_string()).into()),
            }
        }
        crate::routing::ConnectRole::Acceptor => {
            let first =
                match tokio::time::timeout(LINK_CONNECT_HELLO_TIMEOUT, read_message(&mut source))
                    .await
                {
                    Ok(Ok(Some(first))) => first,
                    Ok(Ok(None)) => return Ok(()),
                    Ok(Err(error)) => {
                        write_message(&mut sink, &protocol_error_link_close(error.to_string()))
                            .await?;
                        carrier.close(wire::pb::LinkCloseReason::ProtocolError);
                        return Ok(());
                    }
                    Err(_) => {
                        carrier.close(wire::pb::LinkCloseReason::ProtocolError);
                        return Ok(());
                    }
                };
            let hello = match handshake.receive(first) {
                Ok(ConnectHandshakeEvent::Hello(hello)) => {
                    // The core records its inputs on the drive, so the
                    // credential stays here.
                    peer_frame = Some(wire::pb::message::Body::Hello(wire::pb::Hello {
                        auth_token: None,
                        ..hello.clone()
                    }));
                    hello
                }
                Ok(_) => {
                    write_message(
                        &mut sink,
                        &protocol_error_hello_ack("unexpected message while awaiting Hello"),
                    )
                    .await?;
                    carrier.close(wire::pb::LinkCloseReason::ProtocolError);
                    return Ok(());
                }
                Err(error) => {
                    write_message(&mut sink, &protocol_error_hello_ack(error.to_string())).await?;
                    carrier.close(wire::pb::LinkCloseReason::ProtocolError);
                    return Ok(());
                }
            };
            let mut session = match authenticate_hello(&ctx, &hello).await {
                Ok(session) => session,
                Err(status) => {
                    audit::auth_jwt_failure(&status);
                    write_message(&mut sink, &auth_expired_link_close()).await?;
                    carrier.close(wire::pb::LinkCloseReason::AuthExpired);
                    return Ok(());
                }
            };
            if let (Some(provider), Some(authenticated)) =
                (ctx.authenticated_context_provider.clone(), session.as_ref())
            {
                let user = authenticated.user();
                let authenticator = authenticated.authenticator.clone();
                let observes = ctx.observes.take();
                ctx = provider.context_for(&user).await;
                ctx.observes = observes;
                session = Some(LinkAuthSession::new(user, authenticator));
            }
            let snapshot = neighbor_snapshot(&ctx, carrier.as_ref()).await;
            let peer = match accept_peer_hello(&ctx, hello, session.as_ref()) {
                Ok(peer) => peer,
                Err(error) => {
                    write_message(&mut sink, &error_hello_ack(error)).await?;
                    carrier.close(wire::pb::LinkCloseReason::ProtocolError);
                    return Ok(());
                }
            };
            tracing::debug!(
                peer = %peer.0.id,
                runtime_instance = ?peer.2,
                "accepted a link from a peer"
            );
            observed = ctx.observed(carrier.as_ref(), session.as_ref());
            let ack = accepted_hello_ack(&ctx, &snapshot, peer.0.id, &own, observed);
            // A resumed link's answer waits for the core to take the
            // identity its ticket names.
            if ctx.resumed.is_some() {
                held_ack = Some(ack);
            } else {
                write_message(&mut sink, &ack).await?;
                handshake
                    .acceptor_ack_sent()
                    .map_err(|error| tonic::Status::invalid_argument(error.to_string()))?;
            }
            ctx.acceptor_session = session;
            ((peer.0, peer.2), peer.1, snapshot)
        }
    };

    run_established(
        ctx,
        carrier,
        sink,
        source,
        (handshake, held_ack),
        (peer_host, peer_instance, peer_neighbors, role, peer_frame),
        Told {
            neighbors: snapshot.into_iter().map(|host| host.id).collect(),
            addresses,
            observed,
        },
    )
    .await
}

/// What this end told its peer in the handshake, so it tells it again
/// only of a change.
struct Told {
    /// The neighbor snapshot.
    neighbors: Vec<HostId>,
    /// Where this host can be reached, watched while the link lists it.
    addresses: Option<watch::Receiver<crate::routing::OwnAddresses>>,
    /// On a relay, the address the peer was told its packets come from.
    observed: Option<std::net::SocketAddr>,
}

/// Where this host can be reached, once that changes from what the link
/// last told its peer; never, on a link that lists nothing.
async fn addresses_changed(
    addresses: &mut Option<watch::Receiver<crate::routing::OwnAddresses>>,
) -> crate::routing::OwnAddresses {
    let Some(addresses) = addresses else {
        return future::pending().await;
    };
    if addresses.changed().await.is_err() {
        return future::pending().await;
    }
    addresses.borrow_and_update().clone()
}

fn addresses_message(own: &crate::routing::OwnAddresses) -> wire::pb::Message {
    wire::pb::Message {
        body: Some(wire::pb::message::Body::Addresses(wire::pb::Addresses {
            addrs: own.listed.iter().map(ToString::to_string).collect(),
            public_addr: own.public.map(|addr| addr.to_string()),
        })),
    }
}

/// Whether `carrier` is two hosts' own link whose packets a forwarder
/// carries: the relay, or a paired daemon.
fn forwarded(carrier: &dyn Carrier) -> bool {
    carrier.kind() == super::CarrierKind::Quic
        && matches!(
            carrier.path_kind(),
            PathKind::RelayUdp | PathKind::RelayTcp | PathKind::ViaDaemon
        )
}

/// The neighbors a link's handshake lists: none on a link through a
/// forwarder, since forwarding is one hop.
async fn neighbor_snapshot(ctx: &LinkCtx, carrier: &dyn Carrier) -> Vec<Host> {
    if forwarded(carrier) {
        return Vec::new();
    }
    ctx.links.neighbor_snapshot().await
}

async fn read_first(
    source: &mut super::ControlSource,
    expected: &'static str,
) -> Result<wire::pb::Message, LinkError> {
    match read_message(source).await? {
        Some(message) => Ok(message),
        None => Err(tonic::Status::unavailable(format!("link closed before {expected}")).into()),
    }
}

async fn authenticate_hello(
    ctx: &LinkCtx,
    hello: &wire::pb::Hello,
) -> Result<Option<LinkAuthSession>, tonic::Status> {
    let Some(authenticator) = &ctx.authenticator else {
        return Ok(ctx.acceptor_session.clone());
    };
    let token = hello
        .auth_token
        .as_deref()
        .filter(|token| !token.is_empty())
        .ok_or_else(|| tonic::Status::unauthenticated("missing link authorization"))?;
    let user = authenticator.authenticate_token(token).await?;
    Ok(Some(LinkAuthSession::new(user, authenticator.clone())))
}

async fn run_established(
    ctx: LinkCtx,
    carrier: Arc<dyn Carrier>,
    mut sink: super::ControlSink,
    mut source: super::ControlSource,
    (mut handshake, held_ack): (ConnectHandshake, Option<wire::pb::Message>),
    peer: (
        Host,
        RuntimeInstance,
        Vec<Host>,
        crate::routing::ConnectRole,
        Option<wire::pb::message::Body>,
    ),
    mut told: Told,
) -> Result<(), LinkError> {
    debug_assert!(held_ack.is_some() || handshake.is_established());
    let (peer_host, peer_instance, _neighbors, connect_role, peer_frame) = peer;
    if ctx
        .shutdown_rx
        .as_ref()
        .is_some_and(|shutdown| *shutdown.borrow())
    {
        let status = tonic::Status::unavailable("link connector is shutting down");
        signal_establishment(ctx.take_established_tx(), Err(clone_status(&status)));
        carrier.close(wire::pb::LinkCloseReason::UserShutdown);
        return Err(status.into());
    }
    let link = LinkId::new(peer_host.id);
    let link_role = if ctx.connector_auth.is_some() {
        LinkRole::CloudRelay
    } else {
        ctx.link_role
    };
    let admission = ctx
        .acceptor_session
        .as_ref()
        .map(|session| LinkAdmission::CloudToken {
            tier: session.tier(),
        })
        .unwrap_or(LinkAdmission::PinnedKey);
    let carrier_tag = match carrier.kind() {
        super::CarrierKind::Ssh => RoutingCarrier::Ssh,
        _ if forwarded(carrier.as_ref()) => RoutingCarrier::Forwarded,
        _ => ctx.routing_carrier,
    };
    let (out_tx, mut out_rx) = mpsc::channel(256);
    let registration = ctx
        .links
        .register_displacing(
            link,
            peer_host.clone(),
            out_tx.clone(),
            LinkProperties {
                role: link_role,
                admission,
                carrier: carrier_tag,
                runtime_instance: peer_instance,
                direct_order: (link_role == LinkRole::Peer
                    && carrier_tag == RoutingCarrier::Direct)
                    .then(|| {
                        crate::routing::direct_order(
                            ctx.local_host.id(),
                            peer_host.id,
                            connect_role,
                        )
                    }),
            },
            &told.neighbors,
            Some(carrier.clone()),
        )
        .await;
    let Some(Registration {
        close_rx: mut link_close_rx,
        ..
    }) = registration
    else {
        signal_establishment(ctx.take_established_tx(), Ok(peer_host));
        carrier.close(wire::pb::LinkCloseReason::UserShutdown);
        return Ok(());
    };
    // The core hears of the link once it is registered, so a route it
    // lists finds the link's carrier, and of the hosts its peer reaches as
    // the peer said them. Nothing picks a link to open a channel on but
    // from the routes the core lists.
    let up = daemon_core::Input::LinkUp {
        link: link.core(),
        peer: core_host(peer_host.id),
        kind: match (link_role, carrier_tag) {
            (LinkRole::CloudRelay, _) => daemon_core::payload::CarrierKind::Relay,
            (LinkRole::Peer, RoutingCarrier::Ssh) => daemon_core::payload::CarrierKind::Ssh,
            (LinkRole::Peer, RoutingCarrier::Direct) => daemon_core::payload::CarrierKind::Direct,
            (LinkRole::Peer, RoutingCarrier::Forwarded) => {
                daemon_core::payload::CarrierKind::Forwarded
            }
        },
        resumed: ctx.resumed.clone().map(daemon_core::payload::PublicKey),
    };
    match held_ack {
        None => ctx.report(up),
        // Nothing the peer sent early is read until the core takes the
        // identity its ticket names: the streams wait in the carrier, and
        // the Hello's answer with them. One the core refuses is reset with
        // the revocation, as a full handshake's verifier would refuse it.
        Some(ack) => {
            let taken = match ctx.links.core() {
                Some(core) => core.resumed(up).await,
                None => false,
            };
            if !taken {
                tracing::info!(
                    peer = %peer_host.id,
                    "refused a resumed link: its ticket names a key not pinned for its peer"
                );
                let _ = write_message(
                    &mut sink,
                    &link_close(wire::pb::LinkCloseReason::UserRevoked),
                )
                .await;
                cleanup_link(&ctx, link, wire::pb::LinkCloseReason::UserRevoked).await;
                carrier.close(wire::pb::LinkCloseReason::UserRevoked);
                return Ok(());
            }
            write_message(&mut sink, &ack).await?;
            handshake
                .acceptor_ack_sent()
                .map_err(|error| tonic::Status::invalid_argument(error.to_string()))?;
        }
    }
    let logged = LinkEvent {
        peer: peer_host.id,
        event: LinkTransition::Up,
        path: carrier.path_kind(),
        // The dialling end knows it resumed by its early data being taken.
        resumed: ctx.resumed.is_some() || carrier.early_data() == EarlyData::Accepted,
        early_data: carrier.early_data(),
        first_packet_to_catch_up_ms: None,
    };
    logged.write(ctx.local_host.id());
    if let Some(frame) = peer_frame {
        ctx.report_frame(link, frame);
    }
    if let Some(auth) = &ctx.connector_auth {
        ctx.report(auth.credential(link));
    }

    signal_establishment(ctx.take_established_tx(), Ok(peer_host.clone()));

    let mut acceptor_auth = ctx.acceptor_session.clone();
    let mut connector_auth = ctx.connector_auth.clone();
    let mut shutdown_rx = ctx.shutdown_rx.clone();
    let mut close_reason = wire::pb::LinkCloseReason::Unspecified;
    let mut close_status = None;
    let observing = ctx.observes.is_some();
    let mut observe = tokio::time::interval(OBSERVE_EVERY);
    observe.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        let auth_expiry = acceptor_auth.as_ref().map(|auth| {
            ctx.clock
                .sleep_until(system_time_ms(auth.user().expires_at))
        });
        tokio::select! {
            inbound = read_message(&mut source) => {
                let message = match inbound {
                    Ok(Some(message)) => message,
                    Ok(None) => break,
                    Err(error) => {
                        let _ = out_tx.send(protocol_error_link_close(error.to_string())).await;
                        close_reason = wire::pb::LinkCloseReason::ProtocolError;
                        break;
                    }
                };
                match handshake.receive(message) {
                    Ok(ConnectHandshakeEvent::PostHandshake(body)) => {
                        match handle_control_body(&ctx, link, body, acceptor_auth.as_mut()).await {
                            ControlAction::Continue => {}
                            ControlAction::Close(reason, status) => {
                                close_reason = reason;
                                close_status = status;
                                break;
                            }
                            ControlAction::ReplyAndClose(message, reason) => {
                                let _ = out_tx.send(*message).await;
                                close_reason = reason;
                                break;
                            }
                        }
                    }
                    Ok(_) => unreachable!("handshake cannot repeat after establishment"),
                    Err(error) => {
                        let _ = out_tx.send(protocol_error_link_close(error.to_string())).await;
                        close_reason = wire::pb::LinkCloseReason::ProtocolError;
                        break;
                    }
                }
            }
            own = addresses_changed(&mut told.addresses) => {
                if let Err(error) = write_message(&mut sink, &addresses_message(&own)).await {
                    close_status = Some(tonic::Status::unavailable(error.to_string()));
                    break;
                }
            }
            _ = observe.tick(), if observing => {
                match ctx.observed(carrier.as_ref(), acceptor_auth.as_ref()) {
                    Some(seen) if told.observed != Some(seen) => {
                        let message = wire::pb::Message {
                            body: Some(wire::pb::message::Body::ObservedAddress(
                                wire::pb::ObservedAddress { addr: seen.to_string() },
                            )),
                        };
                        if let Err(error) = write_message(&mut sink, &message).await {
                            close_status = Some(tonic::Status::unavailable(error.to_string()));
                            break;
                        }
                        told.observed = Some(seen);
                    }
                    Some(_) => {}
                    // A host that went free is told nothing, and is told
                    // afresh once it pays again.
                    None => told.observed = None,
                }
            }
            outbound = out_rx.recv() => {
                let Some(message) = outbound else { break };
                if let Err(error) = write_message(&mut sink, &message).await {
                    close_status = Some(tonic::Status::unavailable(error.to_string()));
                    break;
                }
            }
            stream = carrier.accept_stream() => {
                let Some(stream) = stream else {
                    if let Some((reason, status)) =
                        last_word(&ctx, link, &mut handshake, &mut source).await
                    {
                        close_reason = reason;
                        close_status = status;
                    }
                    break;
                };
                spawn_inbound_dispatch(ctx.clone(), peer_host.id, stream);
            }
            _ = maybe_policy_sleep(auth_expiry), if acceptor_auth.is_some() => {
                audit::auth_jwt_failure("link authorization expired");
                // The pairs through the link end and both ends are told
                // before the link itself goes.
                ctx.links.expire(&link).await;
                let _ = write_message(&mut sink, &auth_expired_link_close()).await;
                close_reason = wire::pb::LinkCloseReason::AuthExpired;
                break;
            }
            request = receive_refresh_request(ctx.refresh_rx.as_ref()), if connector_auth.is_some() => {
                let Some(request) = request else { continue };
                let Some(auth) = connector_auth.as_mut() else { continue };
                match auth.refresh().await {
                    Ok((message, tier)) => {
                        let failed = match write_message(&mut sink, &message).await {
                            Ok(()) => {
                                auth.presented(link, &ctx);
                                let _ = request.response.send(Ok(tier));
                                false
                            }
                            Err(error) => {
                                let _ = request.response.send(Err(tonic::Status::unavailable(error.to_string())));
                                true
                            }
                        };
                        if failed { break; }
                    }
                    Err(status) => {
                        if should_audit_auth_refresh_failure(&status) {
                            audit::auth_jwt_failure(&status);
                        }
                        let _ = request.response.send(Err(clone_status(&status)));
                        close_status = Some(status);
                        break;
                    }
                }
            }
            request = link_close_rx.recv() => {
                close_status = Some(match request {
                    Some(LinkCloseRequest::OutgoingQueueFull) => tonic::Status::resource_exhausted("link outgoing queue full"),
                    Some(LinkCloseRequest::TrustReplaced) => tonic::Status::permission_denied("peer trust was replaced"),
                    Some(LinkCloseRequest::Superseded) => tonic::Status::unavailable("direct link superseded"),
                    Some(LinkCloseRequest::CallsFailed) => tonic::Status::unavailable("no call channel opened on the link"),
                    Some(LinkCloseRequest::ForwarderGone) => tonic::Status::unavailable("the link to the forwarder carrying it went"),
                    Some(LinkCloseRequest::PeerRevoked) => tonic::Status::permission_denied("the peer no longer trusts this host"),
                    Some(LinkCloseRequest::Revoked) => {
                        let _ = write_message(&mut sink, &link_close(wire::pb::LinkCloseReason::UserRevoked)).await;
                        close_reason = wire::pb::LinkCloseReason::UserRevoked;
                        tonic::Status::permission_denied("peer trust was revoked")
                    }
                    None => tonic::Status::unavailable("link closed"),
                });
                break;
            }
            _ = wait_for_connector_shutdown(&mut shutdown_rx) => {
                let message = link_close(wire::pb::LinkCloseReason::UserShutdown);
                let _ = write_message(&mut sink, &message).await;
                close_reason = wire::pb::LinkCloseReason::UserShutdown;
                break;
            }
            reason = carrier.closed() => {
                close_reason = reason;
                if let Some((reason, status)) =
                    last_word(&ctx, link, &mut handshake, &mut source).await
                {
                    close_reason = reason;
                    close_status = status;
                }
                break;
            }
        }
    }

    // A connection the peer closed saying USER_REVOKED is its word, whether
    // its close message was read first, the connection's end was seen
    // first, or the control stream simply ended with it. This side has not
    // closed the carrier yet, so any reason it gives is the peer's.
    if carrier.close_reason() == Some(wire::pb::LinkCloseReason::UserRevoked) {
        peer_revoked(&ctx, link, peer_host.id).await;
    }
    // Drain control messages already queued before closing, most importantly
    // the protocol close emitted for an invalid inbound frame.
    while let Ok(message) = out_rx.try_recv() {
        let _ = write_message(&mut sink, &message).await;
    }
    cleanup_link(&ctx, link, close_reason).await;
    carrier.close(close_reason);
    LinkEvent {
        event: LinkTransition::Down,
        first_packet_to_catch_up_ms: None,
        ..logged
    }
    .write(ctx.local_host.id());
    match close_status {
        Some(status) => Err(status.into()),
        None => Ok(()),
    }
}

/// The peer's close, saying why, from what it sent before its connection
/// ended. The close arrives just ahead of the end, and the end can be
/// noticed first, with the close still unread; the connection is gone, so
/// what is left to read runs out at once.
async fn last_word(
    ctx: &LinkCtx,
    link: LinkId,
    handshake: &mut ConnectHandshake,
    source: &mut super::ControlSource,
) -> Option<(wire::pb::LinkCloseReason, Option<tonic::Status>)> {
    while let Ok(Some(message)) = read_message(source).await {
        if let Ok(ConnectHandshakeEvent::PostHandshake(
            body @ wire::pb::message::Body::LinkClose(_),
        )) = handshake.receive(message)
            && let ControlAction::Close(reason, status) =
                handle_control_body(ctx, link, body, None).await
        {
            return Some((reason, status));
        }
    }
    None
}

/// Hands a stream the peer opened to this host's server. A stream is its
/// opener's own: the link's handshake authenticated the host at its other
/// end, and nothing on a link reaches any other host. A context with no
/// server, a relay's, resets it.
fn spawn_inbound_dispatch(ctx: LinkCtx, peer: HostId, mut stream: super::ByteStream) {
    tokio::spawn(async move {
        let Some(sender) = &ctx.incoming_streams_tx else {
            stream.reset();
            return;
        };
        let inbound = super::InboundStream {
            adjacent_peer: peer,
            stream,
        };
        if let Err(mut error) = sender.send(inbound).await {
            error.0.stream.reset();
        }
    });
}

enum ControlAction {
    Continue,
    Close(wire::pb::LinkCloseReason, Option<tonic::Status>),
    /// Boxed: the control message dwarfs every other variant, and this one
    /// is the rare terminal path.
    ReplyAndClose(Box<wire::pb::Message>, wire::pb::LinkCloseReason),
}

async fn handle_control_body(
    ctx: &LinkCtx,
    link: LinkId,
    body: wire::pb::message::Body,
    acceptor_auth: Option<&mut LinkAuthSession>,
) -> ControlAction {
    match body {
        wire::pb::message::Body::NeighborUp(event) => match neighbor_up_from_wire(event.clone()) {
            Ok(_) => {
                ctx.report_frame(link, wire::pb::message::Body::NeighborUp(event));
                ControlAction::Continue
            }
            Err(error) => ControlAction::ReplyAndClose(
                Box::new(protocol_error_link_close(error.to_string())),
                wire::pb::LinkCloseReason::ProtocolError,
            ),
        },
        wire::pb::message::Body::NeighborDown(event) => {
            match neighbor_down_from_wire(event.clone()) {
                Ok(_) => {
                    ctx.report_frame(link, wire::pb::message::Body::NeighborDown(event));
                    ControlAction::Continue
                }
                Err(error) => ControlAction::ReplyAndClose(
                    Box::new(protocol_error_link_close(error.to_string())),
                    wire::pb::LinkCloseReason::ProtocolError,
                ),
            }
        }
        wire::pb::message::Body::Reauth(reauth) => {
            let Some(auth) = acceptor_auth else {
                return ControlAction::ReplyAndClose(
                    Box::new(protocol_error_link_close(
                        "reauth received on unauthenticated link",
                    )),
                    wire::pb::LinkCloseReason::ProtocolError,
                );
            };
            match auth
                .authenticator
                .authenticate_token(&reauth.auth_token)
                .await
            {
                Ok(user) => match auth.apply_reauth(user) {
                    Ok(()) => {
                        // A lapse to free stops the streams through the link
                        // too: the forwarding core cuts them as it hears.
                        ctx.links.update_cloud_tier(&link, auth.tier()).await;
                        ControlAction::Continue
                    }
                    Err(error) => {
                        audit::auth_jwt_failure("link reauth user mismatch");
                        tracing::warn!(
                            original_user_id = %error.original_user_id,
                            reauth_user_id = %error.reauth_user_id,
                            "link reauth user mismatch"
                        );
                        ControlAction::ReplyAndClose(
                            Box::new(auth_expired_link_close()),
                            wire::pb::LinkCloseReason::AuthExpired,
                        )
                    }
                },
                Err(status) => {
                    audit::auth_jwt_failure(&status);
                    ControlAction::ReplyAndClose(
                        Box::new(auth_expired_link_close()),
                        wire::pb::LinkCloseReason::AuthExpired,
                    )
                }
            }
        }
        wire::pb::message::Body::LinkClose(close) => {
            let reason = wire::pb::LinkCloseReason::try_from(close.reason)
                .unwrap_or(wire::pb::LinkCloseReason::Unspecified);
            if reason == wire::pb::LinkCloseReason::UserRevoked {
                peer_revoked(ctx, link, link.peer()).await;
            }
            ControlAction::Close(reason, link_close_status(&close))
        }
        // The links the forwarder no longer carries go before the core
        // hears why, so it reads the host by the refusal and not by a
        // link that merely went.
        wire::pb::message::Body::ForwardRefused(refused) => {
            if let Ok(host) = HostId::from_slice(&refused.dst) {
                let reason = wire::pb::ForwardRefusal::try_from(refused.reason)
                    .unwrap_or(wire::pb::ForwardRefusal::Unspecified);
                ctx.links.close_forwarded(host, link.core(), reason).await;
            }
            ctx.report_frame(link, wire::pb::message::Body::ForwardRefused(refused));
            ControlAction::Continue
        }
        // The core keeps what a peer lists and what the relay sees.
        body @ (wire::pb::message::Body::Addresses(_)
        | wire::pb::message::Body::ObservedAddress(_)) => {
            ctx.report_frame(link, body);
            ControlAction::Continue
        }
        wire::pb::message::Body::Hello(_) | wire::pb::message::Body::HelloAck(_) => {
            unreachable!("repeated handshake message is rejected by ConnectHandshake")
        }
    }
}

fn accept_peer_hello(
    ctx: &LinkCtx,
    hello: wire::pb::Hello,
    _auth_session: Option<&LinkAuthSession>,
) -> Result<(Host, Vec<Host>, RuntimeInstance), wire::pb::Error> {
    if !hello
        .supported_protocol_versions
        .contains(&PROTOCOL_VERSION)
    {
        return Err(net_error::version_mismatch(
            vec![PROTOCOL_VERSION],
            hello.supported_protocol_versions,
        ));
    }
    let host = hello
        .host
        .ok_or_else(|| "Hello.host is required".to_string())
        .and_then(|host| host_from_wire(host).map_err(|error| error.to_string()))
        .map_err(invalid_argument_error)?;
    validate_remote_host(&host).map_err(invalid_argument_error)?;
    if let Some(authenticated_peer) = ctx.authenticated_peer
        && host.id != authenticated_peer
    {
        return Err(invalid_argument_error(format!(
            "Hello.host_id {} does not match carrier peer {}",
            host.id, authenticated_peer
        )));
    }
    if host.id == ctx.local_host.id() {
        return Err(host_id_collision_error(format!(
            "peer host_id {} matches local host_id",
            host.id
        )));
    }
    let runtime_instance =
        RuntimeInstance::from_wire(&hello.runtime_instance_id).map_err(invalid_argument_error)?;
    Ok((
        host,
        neighbors_from_wire(hello.neighbors)?,
        runtime_instance,
    ))
}

fn accept_peer_hello_ack(
    ctx: &LinkCtx,
    accepted: wire::pb::HelloAccepted,
) -> Result<(Host, Vec<Host>, RuntimeInstance), wire::pb::Error> {
    if accepted.protocol_version != PROTOCOL_VERSION {
        return Err(net_error::version_mismatch(
            vec![PROTOCOL_VERSION],
            vec![accepted.protocol_version],
        ));
    }
    let host = accepted
        .host
        .ok_or_else(|| "HelloAccepted.host is required".to_string())
        .and_then(|host| host_from_wire(host).map_err(|error| error.to_string()))
        .map_err(invalid_argument_error)?;
    validate_remote_host(&host).map_err(invalid_argument_error)?;
    if let Some(expected_peer) = ctx.expected_peer
        && host.id != expected_peer
    {
        return Err(invalid_argument_error(format!(
            "HelloAccepted.host_id {} does not match expected peer {}",
            host.id, expected_peer
        )));
    }
    if host.id == ctx.local_host.id() {
        return Err(host_id_collision_error(
            "peer host_id matches local host_id",
        ));
    }
    let runtime_instance = RuntimeInstance::from_wire(&accepted.runtime_instance_id)
        .map_err(invalid_argument_error)?;
    Ok((
        host,
        neighbors_from_wire(accepted.neighbors)?,
        runtime_instance,
    ))
}

fn connector_hello(
    ctx: &LinkCtx,
    snapshot: &[Host],
    own: &crate::routing::OwnAddresses,
) -> wire::pb::Message {
    wire::pb::Message {
        body: Some(wire::pb::message::Body::Hello(wire::pb::Hello {
            supported_protocol_versions: vec![PROTOCOL_VERSION],
            host: Some(host_to_wire(&ctx.local_host.snapshot())),
            neighbors: snapshot.iter().map(host_to_wire).collect(),
            auth_token: ctx
                .connector_auth
                .as_ref()
                .map(|auth| auth.token.token.clone()),
            runtime_instance_id: ctx.local_host.runtime_instance().to_wire(),
            addrs: own.listed.iter().map(ToString::to_string).collect(),
            public_addr: own.public.map(|addr| addr.to_string()),
        })),
    }
}

fn accepted_hello_ack(
    ctx: &LinkCtx,
    snapshot: &[Host],
    peer: HostId,
    own: &crate::routing::OwnAddresses,
    observed: Option<std::net::SocketAddr>,
) -> wire::pb::Message {
    wire::pb::Message {
        body: Some(wire::pb::message::Body::HelloAck(wire::pb::HelloAck {
            outcome: Some(wire::pb::hello_ack::Outcome::Accepted(
                wire::pb::HelloAccepted {
                    protocol_version: PROTOCOL_VERSION,
                    host: Some(host_to_wire(&ctx.local_host.snapshot())),
                    neighbors: snapshot
                        .iter()
                        .filter(|host| host.id != peer)
                        .map(host_to_wire)
                        .collect(),
                    runtime_instance_id: ctx.local_host.runtime_instance().to_wire(),
                    addrs: own.listed.iter().map(ToString::to_string).collect(),
                    observed_addr: observed.map(|addr| addr.to_string()),
                    public_addr: own.public.map(|addr| addr.to_string()),
                },
            )),
        })),
    }
}

fn neighbors_from_wire(neighbors: Vec<wire::pb::Host>) -> Result<Vec<Host>, wire::pb::Error> {
    neighbors
        .into_iter()
        .map(|host| {
            inbound_host_from_wire(host, "handshake neighbor")
                .map_err(|error| invalid_argument_error(error.to_string()))
        })
        .collect()
}

fn error_hello_ack(error: wire::pb::Error) -> wire::pb::Message {
    wire::pb::Message {
        body: Some(wire::pb::message::Body::HelloAck(wire::pb::HelloAck {
            outcome: Some(wire::pb::hello_ack::Outcome::Error(error)),
        })),
    }
}

fn invalid_argument_error(message: impl Into<String>) -> wire::pb::Error {
    net_error::invalid_argument(message)
}

fn host_id_collision_error(message: impl Into<String>) -> wire::pb::Error {
    net_error::error(wire::ErrorCode::AlreadyExists, message)
}

fn auth_expired_link_close() -> wire::pb::Message {
    wire::pb::Message {
        body: Some(wire::pb::message::Body::LinkClose(wire::pb::LinkClose {
            reason: wire::pb::LinkCloseReason::AuthExpired as i32,
            error: Some(wire::pb::Error {
                code: wire::pb::ErrorCode::Unauthenticated as i32,
                message: "link authorization expired".to_string(),
                details: Vec::new(),
            }),
        })),
    }
}

fn link_close(reason: wire::pb::LinkCloseReason) -> wire::pb::Message {
    wire::pb::Message {
        body: Some(wire::pb::message::Body::LinkClose(wire::pb::LinkClose {
            reason: reason as i32,
            error: None,
        })),
    }
}

fn link_close_status(close: &wire::pb::LinkClose) -> Option<tonic::Status> {
    let reason = wire::pb::LinkCloseReason::try_from(close.reason)
        .unwrap_or(wire::pb::LinkCloseReason::Unspecified);
    if let Some(error) = close.error.clone() {
        return Some(protocol_status(error));
    }
    (reason == wire::pb::LinkCloseReason::VersionMismatch)
        .then(|| tonic::Status::failed_precondition("the other machine needs updating"))
}

fn should_audit_auth_refresh_failure(status: &tonic::Status) -> bool {
    net_error::code_of(status) != Some(wire::ErrorCode::PaymentRequired)
}

/// `peer` said on `link` that it no longer trusts this host. Its other
/// links go first, so no call that hears of it picks one of them.
async fn peer_revoked(ctx: &LinkCtx, link: LinkId, peer: HostId) {
    ctx.links.forsake(peer, link).await;
    ctx.report(daemon_core::Input::PeerRevoked {
        host: core_host(peer),
    });
}

async fn cleanup_link(ctx: &LinkCtx, link: LinkId, reason: wire::pb::LinkCloseReason) {
    ctx.report(daemon_core::Input::LinkDown {
        link: link.core(),
        reason,
    });
    ctx.links.remove(&link).await;
}

async fn receive_refresh_request(
    refresh_rx: Option<&LinkConnectorRefreshReceiver>,
) -> Option<LinkConnectorRefreshRequest> {
    match refresh_rx {
        Some(rx) => match rx.lock().await.recv().await {
            Some(request) => Some(request),
            None => future::pending().await,
        },
        None => future::pending().await,
    }
}

async fn wait_for_connector_shutdown(shutdown_rx: &mut Option<watch::Receiver<bool>>) {
    let Some(shutdown_rx) = shutdown_rx else {
        future::pending::<()>().await;
        return;
    };
    loop {
        if *shutdown_rx.borrow() {
            return;
        }
        if shutdown_rx.changed().await.is_err() {
            future::pending::<()>().await;
        }
    }
}

async fn maybe_policy_sleep(sleep: Option<Pin<Box<dyn Future<Output = ()> + Send + 'static>>>) {
    match sleep {
        Some(sleep) => sleep.await,
        None => future::pending().await,
    }
}

fn clone_status(status: &tonic::Status) -> tonic::Status {
    let mut cloned = tonic::Status::with_details(
        status.code(),
        status.message().to_string(),
        Bytes::copy_from_slice(status.details()),
    );
    *cloned.metadata_mut() = status.metadata().clone();
    cloned
}

fn signal_establishment(sender: Option<EstablishmentSender>, result: Result<Host, tonic::Status>) {
    if let Some(sender) = sender {
        let _ = sender.send(result);
    }
}

pub fn spawn_connector_with_establishment(
    ctx: LinkConnectorCtx,
    carrier: Arc<dyn Carrier>,
) -> (ConnectorTask, EstablishmentReceiver) {
    spawn_connector(ctx, carrier, None, None, None)
}

pub(crate) fn spawn_connector_with_establishment_and_shutdown(
    ctx: LinkConnectorCtx,
    carrier: Arc<dyn Carrier>,
    shutdown_rx: watch::Receiver<bool>,
) -> (ConnectorTask, EstablishmentReceiver) {
    spawn_connector(ctx, carrier, None, Some(shutdown_rx), None)
}

pub(crate) fn spawn_connector_with_auth_establishment_and_shutdown(
    ctx: LinkConnectorCtx,
    carrier: Arc<dyn Carrier>,
    auth: LinkConnectorAuth,
    shutdown_rx: watch::Receiver<bool>,
    refresh_rx: Option<LinkConnectorRefreshReceiver>,
) -> (ConnectorTask, EstablishmentReceiver) {
    spawn_connector(ctx, carrier, Some(auth), Some(shutdown_rx), refresh_rx)
}

fn spawn_connector(
    mut ctx: LinkConnectorCtx,
    carrier: Arc<dyn Carrier>,
    auth: Option<LinkConnectorAuth>,
    shutdown_rx: Option<watch::Receiver<bool>>,
    refresh_rx: Option<LinkConnectorRefreshReceiver>,
) -> (ConnectorTask, EstablishmentReceiver) {
    let (established_tx, established_rx) = oneshot::channel();
    let established = Arc::new(StdRwLock::new(Some(established_tx)));
    ctx.connector_auth = auth;
    ctx.established_tx = Some(established.clone());
    ctx.shutdown_rx = shutdown_rx;
    ctx.refresh_rx = refresh_rx;
    let task = tokio::spawn(async move {
        let result = run_link(ctx, carrier, crate::routing::ConnectRole::Connector)
            .await
            .map_err(link_error_status);
        if let Some(sender) = established
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        {
            let status =
                result.as_ref().err().map(clone_status).unwrap_or_else(|| {
                    tonic::Status::unavailable("link closed before establishment")
                });
            let _ = sender.send(Err(status));
        }
        result
    });
    (task, established_rx)
}

fn link_error_status(error: LinkError) -> tonic::Status {
    match error {
        LinkError::Status(status) => status,
        LinkError::Io(error) => tonic::Status::unavailable(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::link::{ControlSink, ControlSource, LinkCarrier, QuicCarrier};
    use crate::routing::Capabilities;

    #[derive(Clone)]
    struct FixedAuthenticator(AuthenticatedLinkUser);

    #[tonic::async_trait]
    impl LinkTokenAuthenticator for FixedAuthenticator {
        async fn authenticate_token(
            &self,
            _token: &str,
        ) -> Result<AuthenticatedLinkUser, tonic::Status> {
            Ok(self.0.clone())
        }
    }

    fn host(id: u128) -> Host {
        Host {
            id: HostId::from_u128(id),
            name: format!("host-{id}"),
            version: "test".to_string(),
            capabilities: Capabilities::default(),
            signed_in: Some(false),
            platform: None,
        }
    }

    async fn carrier_pair() -> (Arc<QuicCarrier>, Arc<QuicCarrier>) {
        crate::link::quic::stream_pair(crate::link::framed::FramedKind::RelayTcp).await
    }

    fn authenticated_user(
        user_id: Uuid,
        expires_at: SystemTime,
        tier: crate::Tier,
    ) -> AuthenticatedLinkUser {
        AuthenticatedLinkUser {
            user_id,
            client_id: "test-client".to_string(),
            expires_at,
            tier,
        }
    }

    async fn send_hello(sink: &mut ControlSink, peer: &Host, auth_token: Option<&str>) {
        write_message(
            sink,
            &wire::pb::Message {
                body: Some(wire::pb::message::Body::Hello(wire::pb::Hello {
                    supported_protocol_versions: vec![PROTOCOL_VERSION],
                    host: Some(host_to_wire(peer)),
                    neighbors: Vec::new(),
                    auth_token: auth_token.map(str::to_string),
                    runtime_instance_id: crate::routing::RuntimeInstance::random().to_wire(),
                    addrs: Vec::new(),
                    public_addr: None,
                })),
            },
        )
        .await
        .unwrap();
    }

    async fn expect_accepted(source: &mut ControlSource) {
        let ack = read_message(source).await.unwrap().unwrap();
        assert!(matches!(
            ack.body,
            Some(wire::pb::message::Body::HelloAck(wire::pb::HelloAck {
                outcome: Some(wire::pb::hello_ack::Outcome::Accepted(_)),
            }))
        ));
    }

    async fn expect_link_close(
        source: &mut ControlSource,
        expected: wire::pb::LinkCloseReason,
    ) -> wire::pb::LinkClose {
        let message = read_message(source).await.unwrap().unwrap();
        let Some(wire::pb::message::Body::LinkClose(close)) = message.body else {
            panic!("expected LinkClose, got {message:?}");
        };
        assert_eq!(
            wire::pb::LinkCloseReason::try_from(close.reason).unwrap(),
            expected
        );
        close
    }

    /// The link log this test process writes, as the daemon's log would
    /// hold it. It is the process's one subscriber, not one scoped to a
    /// test: a scoped one misses events whose callsites another test's
    /// thread registered first, while none was listening.
    #[derive(Clone, Default)]
    struct Traces(Arc<std::sync::Mutex<Vec<u8>>>);

    fn traces() -> &'static Traces {
        static TRACES: std::sync::OnceLock<Traces> = std::sync::OnceLock::new();
        TRACES.get_or_init(|| {
            use tracing_subscriber::Layer as _;
            use tracing_subscriber::layer::SubscriberExt as _;
            use tracing_subscriber::util::SubscriberInitExt as _;

            let traces = Traces::default();
            let writer = traces.clone();
            tracing_subscriber::registry()
                .with(
                    tracing_subscriber::fmt::layer()
                        .with_ansi(false)
                        .with_writer(move || writer.clone())
                        .with_filter(tracing_subscriber::filter::Targets::new().with_target(
                            crate::link_log::TARGET,
                            tracing_subscriber::filter::LevelFilter::INFO,
                        )),
                )
                .try_init()
                .expect("no other subscriber in node's unit tests");
            traces
        })
    }

    impl std::io::Write for Traces {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl Traces {
        /// What `host` wrote to the link log.
        fn link_log(&self, host: HostId) -> Vec<crate::link_log::LinkEvent> {
            String::from_utf8_lossy(&self.0.lock().unwrap())
                .lines()
                .filter_map(crate::link_log::LinkEvent::parse)
                .filter(|(writer, _)| *writer == host)
                .map(|(_, event)| event)
                .collect()
        }
    }

    /// A direct link over UDP writes its link log line when it comes up
    /// and again when it goes down, and the log reads both back.
    #[tokio::test]
    async fn link_log_names_a_direct_link_up_and_down() {
        use crate::identity::DeviceIdentity;
        use crate::link::LinkEndpoint;
        use crate::link_log::{EarlyData, LinkEvent, LinkTransition, PathKind};

        let traces = traces();
        let dialler = DeviceIdentity::for_test(HostId::new_v4());
        let listener = DeviceIdentity::for_test(HostId::new_v4());
        let server = LinkEndpoint::bind(
            "127.0.0.1:0".parse().unwrap(),
            Some(
                listener
                    .quic_server_config(
                        trust_for(&dialler),
                        &crate::link::resumption::SessionTickets::in_memory(16),
                    )
                    .unwrap(),
            ),
        )
        .unwrap();
        let client = LinkEndpoint::bind("127.0.0.1:0".parse().unwrap(), None).unwrap();
        let accept = async {
            let connection = server.endpoint.accept().await.unwrap().await.unwrap();
            let from = server.address_of(connection.remote_address());
            QuicCarrier::from_accepted(connection).with_path(PathKind::of_addr(from))
        };
        let dial = QuicCarrier::connect_direct(
            &client,
            server.local_addr().unwrap(),
            &dialler,
            trust_for(&listener),
            listener.host_id,
        );
        let (accepted, dialled) = tokio::join!(accept, dial);
        let dialled = dialled.unwrap();

        let (local, peer) = (host_for(&listener), host_for(&dialler));
        let links = Arc::new(LinkRegistry::new(local.id));
        let ctx = LinkCtx::new(local.clone(), links.clone()).with_authenticated_peer(peer.id);
        let task = tokio::spawn(run_link(
            ctx,
            Arc::new(accepted),
            crate::routing::ConnectRole::Acceptor,
        ));
        let (mut sink, mut source) = dialled.control();
        send_hello(&mut sink, &peer, None).await;
        expect_accepted(&mut source).await;
        dialled.close(wire::pb::LinkCloseReason::UserShutdown);
        let _ = task.await.unwrap();

        let up = LinkEvent {
            peer: peer.id,
            event: LinkTransition::Up,
            path: PathKind::Lan,
            resumed: false,
            early_data: EarlyData::None,
            first_packet_to_catch_up_ms: None,
        };
        let down = LinkEvent {
            event: LinkTransition::Down,
            ..up
        };
        assert_eq!(traces.link_log(local.id), [up, down]);
    }

    fn host_for(identity: &crate::identity::DeviceIdentity) -> Host {
        Host {
            id: identity.host_id,
            ..host(0)
        }
    }

    fn trust_for(peer: &crate::identity::DeviceIdentity) -> crate::trust::SharedTrustStore {
        let mut trust = crate::trust::TrustStore::default();
        trust
            .upsert_paired_peer(
                peer.host_id,
                peer.public_key().to_vec(),
                "peer".to_string(),
                None,
                chrono::Utc::now(),
            )
            .unwrap();
        Arc::new(std::sync::RwLock::new(trust))
    }

    #[tokio::test(start_paused = true)]
    async fn authenticated_link_expires_without_reauthentication() {
        let local = host(1);
        let peer = host(2);
        let links = Arc::new(LinkRegistry::new(crate::HostId::nil()));
        let user = authenticated_user(
            Uuid::new_v4(),
            SystemTime::now() + Duration::from_secs(60),
            crate::Tier::Free,
        );
        let (connector_carrier, acceptor_carrier) = carrier_pair().await;
        let ctx = LinkCtx::new(local, links.clone())
            .with_authenticated_peer(peer.id)
            .with_token_authenticator(Arc::new(FixedAuthenticator(user)));
        let task = tokio::spawn(run_link(
            ctx,
            acceptor_carrier.clone(),
            crate::routing::ConnectRole::Acceptor,
        ));
        let (mut sink, mut source) = connector_carrier.control();

        send_hello(&mut sink, &peer, Some("initial-token")).await;
        expect_accepted(&mut source).await;
        assert!(links.link_to_peer(peer.id).await.is_some());

        tokio::time::advance(Duration::from_secs(61)).await;
        let close = expect_link_close(&mut source, wire::pb::LinkCloseReason::AuthExpired).await;
        assert_eq!(
            close.error.as_ref().map(|error| error.code),
            Some(wire::pb::ErrorCode::Unauthenticated as i32)
        );
        assert_eq!(
            acceptor_carrier.closed().await,
            wire::pb::LinkCloseReason::AuthExpired
        );
        task.await.unwrap().unwrap();
        assert!(links.link_to_peer(peer.id).await.is_none());
    }

    #[tokio::test]
    async fn reauthentication_for_another_user_expires_without_changing_tier() {
        let local = host(1);
        let peer = host(2);
        let user_id = Uuid::new_v4();
        let initial = authenticated_user(
            user_id,
            SystemTime::now() + Duration::from_secs(3600),
            crate::Tier::Free,
        );
        let other_user = authenticated_user(
            Uuid::new_v4(),
            SystemTime::now() + Duration::from_secs(3600),
            crate::Tier::Pro,
        );
        let session = LinkAuthSession::new(initial, FixedAuthenticator(other_user));
        let links = Arc::new(LinkRegistry::new(crate::HostId::nil()));
        let (connector_carrier, acceptor_carrier) = carrier_pair().await;
        let mut ctx = LinkCtx::new(local, links).with_authenticated_peer(peer.id);
        ctx.acceptor_session = Some(session.clone());
        let task = tokio::spawn(run_link(
            ctx,
            acceptor_carrier.clone(),
            crate::routing::ConnectRole::Acceptor,
        ));
        let (mut sink, mut source) = connector_carrier.control();

        send_hello(&mut sink, &peer, None).await;
        expect_accepted(&mut source).await;
        write_message(
            &mut sink,
            &wire::pb::Message {
                body: Some(wire::pb::message::Body::Reauth(wire::pb::Reauth {
                    auth_token: "another-user-token".to_string(),
                })),
            },
        )
        .await
        .unwrap();

        expect_link_close(&mut source, wire::pb::LinkCloseReason::AuthExpired).await;
        assert_eq!(session.tier(), crate::Tier::Free);
        assert_eq!(session.user().user_id, user_id);
        assert_eq!(
            acceptor_carrier.closed().await,
            wire::pb::LinkCloseReason::AuthExpired
        );
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn a_peer_sharing_no_protocol_version_is_refused_as_needing_an_update() {
        let local = host(1);
        let peer = host(2);
        let links = Arc::new(LinkRegistry::new(crate::HostId::nil()));
        let (connector_carrier, acceptor_carrier) = carrier_pair().await;
        let ctx = LinkCtx::new(local, links.clone()).with_authenticated_peer(peer.id);
        let task = tokio::spawn(run_link(
            ctx,
            acceptor_carrier.clone(),
            crate::routing::ConnectRole::Acceptor,
        ));
        let (mut sink, mut source) = connector_carrier.control();

        write_message(
            &mut sink,
            &wire::pb::Message {
                body: Some(wire::pb::message::Body::Hello(wire::pb::Hello {
                    supported_protocol_versions: vec![PROTOCOL_VERSION + 1],
                    host: Some(host_to_wire(&peer)),
                    neighbors: Vec::new(),
                    auth_token: None,
                    runtime_instance_id: crate::routing::RuntimeInstance::random().to_wire(),
                    addrs: Vec::new(),
                    public_addr: None,
                })),
            },
        )
        .await
        .unwrap();
        let ack = read_message(&mut source).await.unwrap().unwrap();
        let Some(wire::pb::message::Body::HelloAck(wire::pb::HelloAck {
            outcome: Some(wire::pb::hello_ack::Outcome::Error(error)),
        })) = ack.body
        else {
            panic!("expected rejected HelloAck, got {ack:?}");
        };
        assert!(net_error::is_version_mismatch(&error), "{error:?}");
        assert_eq!(
            acceptor_carrier.closed().await,
            wire::pb::LinkCloseReason::ProtocolError
        );
        task.await.unwrap().unwrap();
        assert!(links.link_to_peer(peer.id).await.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn acceptor_without_hello_times_out_without_registering_a_link() {
        let local = host(1);
        let peer = host(2);
        let links = Arc::new(LinkRegistry::new(crate::HostId::nil()));
        let (connector_carrier, acceptor_carrier) = carrier_pair().await;
        let ctx = LinkCtx::new(local, links.clone()).with_authenticated_peer(peer.id);
        let task = tokio::spawn(run_link(
            ctx,
            acceptor_carrier.clone(),
            crate::routing::ConnectRole::Acceptor,
        ));
        let (_sink, _source) = connector_carrier.control();

        tokio::task::yield_now().await;
        assert!(links.link_to_peer(peer.id).await.is_none());
        tokio::time::advance(LINK_CONNECT_HELLO_TIMEOUT + Duration::from_millis(1)).await;

        assert_eq!(
            acceptor_carrier.closed().await,
            wire::pb::LinkCloseReason::ProtocolError
        );
        task.await.unwrap().unwrap();
        assert!(links.link_to_peer(peer.id).await.is_none());
    }

    /// What a link tells its peer of where this host can be reached.
    mod addresses {
        use super::*;

        #[tokio::test]
        async fn a_link_tells_its_peer_where_this_host_is_and_again_when_that_changes() {
            let local = crate::routing::LiveLocalHost::new(host(1));
            let listed: Vec<std::net::SocketAddr> = vec![
                "192.0.2.7:4100".parse().unwrap(),
                "[2001:db8::7]:4100".parse().unwrap(),
            ];
            local.set_listed(listed.clone());
            let peer = host(2);
            let links = Arc::new(LinkRegistry::new(local.id()));
            let (connector_carrier, acceptor_carrier) = carrier_pair().await;
            let ctx = LinkCtx::new_live(local.clone(), links).with_authenticated_peer(peer.id);
            let task = tokio::spawn(run_link(
                ctx,
                acceptor_carrier.clone(),
                crate::routing::ConnectRole::Acceptor,
            ));
            let (mut sink, mut source) = connector_carrier.control();
            send_hello(&mut sink, &peer, None).await;
            let Some(wire::pb::message::Body::HelloAck(wire::pb::HelloAck {
                outcome: Some(wire::pb::hello_ack::Outcome::Accepted(accepted)),
            })) = read_message(&mut source).await.unwrap().unwrap().body
            else {
                panic!("the host accepts its peer");
            };
            assert_eq!(accepted.addrs, ["192.0.2.7:4100", "[2001:db8::7]:4100"]);
            assert_eq!(accepted.public_addr, None, "nothing told it one");
            assert_eq!(accepted.observed_addr, None, "only a relay observes");

            // The relay told the host where it sees it.
            let public: std::net::SocketAddr = "203.0.113.9:41000".parse().unwrap();
            local.set_public(public);
            let next = tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if let Some(wire::pb::message::Body::Addresses(addresses)) =
                        read_message(&mut source).await.unwrap().unwrap().body
                    {
                        return addresses;
                    }
                }
            })
            .await
            .expect("the change is sent");
            assert_eq!(next.addrs, accepted.addrs);
            assert_eq!(next.public_addr.as_deref(), Some("203.0.113.9:41000"));

            // The same again changes nothing, so says nothing.
            local.set_public(public);
            local.set_listed(listed);
            assert!(
                tokio::time::timeout(Duration::from_millis(300), read_message(&mut source))
                    .await
                    .is_err(),
                "nothing more is sent"
            );
            connector_carrier.close(wire::pb::LinkCloseReason::UserShutdown);
            let _ = task.await;
        }
    }
}
