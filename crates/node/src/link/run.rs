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

use super::{LinkCarrier as Carrier, read_message, write_message};
use crate::net_error::{self, status as protocol_status};
use crate::routing::{
    ConnectHandshake, ConnectHandshakeEvent, DirectLinkOrder, Host, LinkAdmission,
    LinkCarrier as RoutingCarrier, LinkCloseRequest, LinkId, LinkProperties, LinkRegistry,
    LinkRole, LiveLocalHost, Registration, RouteUpdateOutcome, RoutingCore, RuntimeInstance,
    core_host, host_from_wire, host_to_wire, inbound_host_from_wire, neighbor_down_from_wire,
    neighbor_up_from_wire, protocol_error_hello_ack, protocol_error_link_close,
    validate_remote_host,
};
use crate::{Clock, HostId, audit};

const LINK_AUTH_REFRESH_BEFORE_EXPIRY: Duration = Duration::from_secs(300);
const LINK_CONNECT_HELLO_TIMEOUT: Duration = Duration::from_secs(10);

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

#[derive(Clone)]
pub struct LinkConnectorAuth {
    token: LinkConnectorToken,
    refresher: Arc<dyn LinkConnectorTokenRefresher>,
    free_refresh_interval: Option<Duration>,
    /// On the policy clock, in milliseconds since the epoch.
    next_refresh_at_ms: i64,
    clock: Arc<dyn Clock>,
    on_refreshed: Option<Arc<dyn Fn(crate::Tier) + Send + Sync>>,
}

impl LinkConnectorAuth {
    pub fn with_free_refresh_interval(
        token: LinkConnectorToken,
        refresher: Arc<dyn LinkConnectorTokenRefresher>,
        free_refresh_interval: Option<Duration>,
    ) -> Self {
        let clock: Arc<dyn Clock> = Arc::new(SystemClock);
        let next_refresh_at_ms = refresh_deadline(&token, free_refresh_interval, clock.as_ref());
        Self {
            token,
            refresher,
            free_refresh_interval,
            next_refresh_at_ms,
            clock,
            on_refreshed: None,
        }
    }

    /// Rebinds product policy to the runtime's clock before the connector is
    /// started. Recomputing the deadline prevents construction time on the
    /// wall clock from leaking into a driven specification.
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.next_refresh_at_ms =
            refresh_deadline(&self.token, self.free_refresh_interval, clock.as_ref());
        self.clock = clock;
        self
    }

    pub(crate) fn with_refresh_observer(
        mut self,
        observer: impl Fn(crate::Tier) + Send + Sync + 'static,
    ) -> Self {
        self.on_refreshed = Some(Arc::new(observer));
        self
    }

    fn refresh_sleep(&self) -> Pin<Box<dyn Future<Output = ()> + Send + 'static>> {
        self.clock.sleep_until(self.next_refresh_at_ms)
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
        self.next_refresh_at_ms =
            refresh_deadline(&self.token, self.free_refresh_interval, self.clock.as_ref());
        if let Some(observer) = &self.on_refreshed {
            observer(tier);
        }
        Ok((message, tier))
    }
}

/// When a connector refreshes its credential, on the policy clock: a free
/// account on a fixed interval, so a purchase reaches the link promptly;
/// otherwise shortly before the credential expires.
fn refresh_deadline(
    token: &LinkConnectorToken,
    free_refresh_interval: Option<Duration>,
    clock: &dyn Clock,
) -> i64 {
    if token.tier == crate::Tier::Free
        && let Some(interval) = free_refresh_interval
    {
        clock.now_ms() + interval.as_millis() as i64
    } else {
        system_time_ms(token.expires_at) - LINK_AUTH_REFRESH_BEFORE_EXPIRY.as_millis() as i64
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
    routing: Arc<RoutingCore>,
    links: Arc<LinkRegistry>,
    incoming_streams_tx: Option<mpsc::Sender<super::InboundStream>>,
    piper: super::Piper,
    expected_peer: Option<HostId>,
    authenticated_peer: Option<HostId>,
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
}

pub(crate) type LinkConnectorCtx = LinkCtx;
type ConnectorTask = tokio::task::JoinHandle<Result<(), tonic::Status>>;
type EstablishmentSender = oneshot::Sender<Result<Host, tonic::Status>>;
pub(crate) type EstablishmentReceiver = oneshot::Receiver<Result<Host, tonic::Status>>;

impl LinkCtx {
    #[cfg(test)]
    pub fn new(local_host: Host, routing: Arc<RoutingCore>, links: Arc<LinkRegistry>) -> Self {
        Self::new_live(LiveLocalHost::new(local_host), routing, links)
    }

    pub(crate) fn new_live(
        local_host: LiveLocalHost,
        routing: Arc<RoutingCore>,
        links: Arc<LinkRegistry>,
    ) -> Self {
        let piper = super::Piper::new(local_host.id(), links.clone());
        Self {
            local_host,
            routing,
            links,
            incoming_streams_tx: None,
            piper,
            expected_peer: None,
            authenticated_peer: None,
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

    /// Keeps the pipes this link forwards with those of the links that share
    /// `pipes`, so a lapse on any of them ends the pipes through it.
    pub(crate) fn with_shared_pipes(mut self, pipes: super::piper::Pipes) -> Self {
        self.piper = self.piper.sharing(pipes);
        self
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
    // The peer's hello or its answer to ours, as the core is told it.
    let peer_frame;
    let ((peer_host, peer_instance), peer_neighbors, snapshot) = match role {
        crate::routing::ConnectRole::Connector => {
            let snapshot = ctx.links.neighbor_snapshot().await;
            write_message(&mut sink, &connector_hello(&ctx, &snapshot)).await?;
            tracing::debug!(path = %carrier.path_stats(), "sent Hello");
            let first = read_first(&mut source, "HelloAck").await?;
            tracing::debug!(path = %carrier.path_stats(), "received the first message after Hello");
            if let Some(wire::pb::message::Body::LinkClose(close)) = first.body.as_ref() {
                let status = link_close_status(close).unwrap_or_else(|| {
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
                ctx = provider.context_for(&user).await;
                session = Some(LinkAuthSession::new(user, authenticator));
            }
            let snapshot = ctx.links.neighbor_snapshot().await;
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
            write_message(&mut sink, &accepted_hello_ack(&ctx, &snapshot, peer.0.id)).await?;
            handshake
                .acceptor_ack_sent()
                .map_err(|error| tonic::Status::invalid_argument(error.to_string()))?;
            ctx.acceptor_session = session;
            ((peer.0, peer.2), peer.1, snapshot)
        }
    };

    run_established(
        ctx,
        carrier,
        sink,
        source,
        handshake,
        (peer_host, peer_instance, peer_neighbors, role, peer_frame),
        snapshot.into_iter().map(|host| host.id).collect(),
    )
    .await
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
    mut handshake: ConnectHandshake,
    peer: (
        Host,
        RuntimeInstance,
        Vec<Host>,
        crate::routing::ConnectRole,
        Option<wire::pb::message::Body>,
    ),
    sent_snapshot: Vec<HostId>,
) -> Result<(), LinkError> {
    debug_assert!(handshake.is_established());
    let (peer_host, peer_instance, peer_neighbors, connect_role, peer_frame) = peer;
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
        _ => ctx.routing_carrier,
    };
    // The core hears of the link before anything can ask for a channel
    // over it, and of the hosts its peer reaches as the peer said them.
    ctx.report(daemon_core::Input::LinkUp {
        link: link.core(),
        peer: core_host(peer_host.id),
        kind: match (link_role, carrier_tag) {
            (LinkRole::CloudRelay, _) => daemon_core::payload::CarrierKind::Relay,
            (LinkRole::Peer, RoutingCarrier::Ssh) => daemon_core::payload::CarrierKind::Ssh,
            (LinkRole::Peer, RoutingCarrier::Direct) => daemon_core::payload::CarrierKind::Direct,
        },
    });
    if let Some(frame) = peer_frame {
        ctx.report_frame(link, frame);
    }
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
                        DirectLinkOrder::for_hosts(ctx.local_host.id(), peer_host.id, connect_role)
                    }),
            },
            &sent_snapshot,
            Some(carrier.clone()),
        )
        .await;
    let Some(Registration {
        close_rx: mut link_close_rx,
        displaced,
    }) = registration
    else {
        signal_establishment(ctx.take_established_tx(), Ok(peer_host));
        ctx.report(daemon_core::Input::LinkDown {
            link: link.core(),
            reason: wire::pb::LinkCloseReason::UserShutdown,
        });
        carrier.close(wire::pb::LinkCloseReason::UserShutdown);
        return Ok(());
    };

    // The peer joins routing before the hosts it claims to reach, and before
    // anything else this link has to do. From the moment it displaced an
    // earlier link, that earlier link carries nothing; every instant until
    // this one lands is an instant when a call to this peer has nowhere to go.
    if ctx.acceptor_session.is_none() {
        match ctx.routing.apply_direct_up(peer_host.clone(), link).await {
            RouteUpdateOutcome::Inserted | RouteUpdateOutcome::AlreadyKnown => {}
            RouteUpdateOutcome::Replacing => {
                tracing::debug!(peer = %peer_host.id, %link, "link established during trust replacement");
            }
            RouteUpdateOutcome::RejectedByCap => {
                let status = tonic::Status::invalid_argument("routing host cap reached");
                signal_establishment(ctx.take_established_tx(), Err(clone_status(&status)));
                let _ = out_tx
                    .send(protocol_error_link_close(status.to_string()))
                    .await;
                cleanup_link(&ctx, link, wire::pb::LinkCloseReason::ProtocolError).await;
                return Err(status.into());
            }
        }
    }
    // The links this one superseded leave routing now, after this link has
    // joined it, so the peer never looks absent. Their tasks remove them again
    // when they finish closing, which by then is a no-op.
    for superseded in displaced {
        ctx.routing.apply_direct_down(superseded).await;
    }
    for neighbor in peer_neighbors {
        if neighbor.id != ctx.local_host.id() && neighbor.id != peer_host.id {
            ctx.routing.apply_claim_up(peer_host.id, neighbor).await;
        }
    }
    signal_establishment(ctx.take_established_tx(), Ok(peer_host.clone()));

    let mut acceptor_auth = ctx.acceptor_session.clone();
    let mut connector_auth = ctx.connector_auth.clone();
    let mut shutdown_rx = ctx.shutdown_rx.clone();
    let mut close_reason = wire::pb::LinkCloseReason::Unspecified;
    let mut close_status = None;

    loop {
        let auth_expiry = acceptor_auth.as_ref().map(|auth| {
            ctx.clock
                .sleep_until(system_time_ms(auth.user().expires_at))
        });
        let refresh_sleep = connector_auth
            .as_ref()
            .map(LinkConnectorAuth::refresh_sleep);
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
            outbound = out_rx.recv() => {
                let Some(message) = outbound else { break };
                if let Err(error) = write_message(&mut sink, &message).await {
                    close_status = Some(tonic::Status::unavailable(error.to_string()));
                    break;
                }
            }
            stream = carrier.accept_stream() => {
                let Some((preface, stream)) = stream else {
                    if let Some((reason, status)) =
                        last_word(&ctx, link, &mut handshake, &mut source).await
                    {
                        close_reason = reason;
                        close_status = status;
                    }
                    break;
                };
                // Only a stream the adjacent peer opened on a direct link of
                // its own can be plain; a relay forwards its openers' streams
                // with their handshakes inside, whatever their prefaces say.
                let plain = preface.plain && carrier.kind() == super::CarrierKind::Quic;
                spawn_inbound_dispatch(ctx.clone(), link, peer_host.id, preface, stream, plain);
            }
            _ = maybe_policy_sleep(auth_expiry), if acceptor_auth.is_some() => {
                audit::auth_jwt_failure("link authorization expired");
                let _ = write_message(&mut sink, &auth_expired_link_close()).await;
                close_reason = wire::pb::LinkCloseReason::AuthExpired;
                break;
            }
            _ = maybe_policy_sleep(refresh_sleep), if connector_auth.is_some() => {
                let Some(auth) = connector_auth.as_mut() else { continue };
                match auth.refresh().await {
                    Ok((message, _)) => {
                        if let Err(error) = write_message(&mut sink, &message).await {
                            close_status = Some(tonic::Status::unavailable(error.to_string()));
                            break;
                        }
                    }
                    Err(status) => {
                        if should_audit_auth_refresh_failure(&status) {
                            audit::auth_jwt_failure(&status);
                        }
                        close_status = Some(status);
                        break;
                    }
                }
            }
            request = receive_refresh_request(ctx.refresh_rx.as_ref()), if connector_auth.is_some() => {
                let Some(request) = request else { continue };
                let Some(auth) = connector_auth.as_mut() else { continue };
                match auth.refresh().await {
                    Ok((message, tier)) => {
                        let failed = match write_message(&mut sink, &message).await {
                            Ok(()) => {
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
        ctx.routing.revocations().note(peer_host.id);
    }
    // Drain control messages already queued before closing, most importantly
    // the protocol close emitted for an invalid inbound frame.
    while let Ok(message) = out_rx.try_recv() {
        let _ = write_message(&mut sink, &message).await;
    }
    cleanup_link(&ctx, link, close_reason).await;
    carrier.close(close_reason);
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

fn spawn_inbound_dispatch(
    ctx: LinkCtx,
    link: LinkId,
    peer: HostId,
    preface: wire::pb::StreamPreface,
    mut stream: super::ByteStream,
    plain: bool,
) {
    tokio::spawn(async move {
        let destination = HostId::from_slice(&preface.dst).ok();
        // `link_role` is CloudRelay on a relay's accepting context and on an
        // embedder's attached relay connector, so those contexts forward even
        // streams addressed to their own host. A daemon-configured cloud
        // connector keeps the Peer role from `link_connector_ctx`; its local
        // binding is registered as CloudRelay separately, so streams addressed
        // to that daemon over the relay still arrive at its inbound dispatcher.
        if destination == Some(ctx.local_host.id()) && ctx.link_role != LinkRole::CloudRelay {
            if let Some(sender) = &ctx.incoming_streams_tx {
                let inbound = super::InboundStream {
                    adjacent_peer: peer,
                    stream,
                    plain,
                };
                if let Err(error) = sender.send(inbound).await {
                    let mut stream = error.0.stream;
                    let _ = stream.reset(wire::pb::StreamRefusal::ShuttingDown).await;
                }
            } else {
                let _ = stream.reset(wire::pb::StreamRefusal::ShuttingDown).await;
            }
        } else {
            let _ = ctx.piper.pipe(link, preface, stream).await;
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
            Ok(host) => {
                ctx.report_frame(link, wire::pb::message::Body::NeighborUp(event));
                if host.id != ctx.local_host.id() && host.id != link.peer() {
                    ctx.routing.apply_claim_up(link.peer(), host).await;
                }
                ControlAction::Continue
            }
            Err(error) => ControlAction::ReplyAndClose(
                Box::new(protocol_error_link_close(error.to_string())),
                wire::pb::LinkCloseReason::ProtocolError,
            ),
        },
        wire::pb::message::Body::NeighborDown(event) => {
            match neighbor_down_from_wire(event.clone()) {
                Ok(host_id) => {
                    ctx.report_frame(link, wire::pb::message::Body::NeighborDown(event));
                    ctx.routing.apply_claim_down(link.peer(), host_id).await;
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
                        ctx.links.update_cloud_tier(&link, auth.tier()).await;
                        if auth.tier() == crate::Tier::Free {
                            ctx.piper.close_link(&link);
                        }
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
                ctx.routing.revocations().note(link.peer());
            }
            ControlAction::Close(reason, link_close_status(&close))
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

fn connector_hello(ctx: &LinkCtx, snapshot: &[Host]) -> wire::pb::Message {
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
        })),
    }
}

fn accepted_hello_ack(ctx: &LinkCtx, snapshot: &[Host], peer: HostId) -> wire::pb::Message {
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

async fn cleanup_link(ctx: &LinkCtx, link: LinkId, reason: wire::pb::LinkCloseReason) {
    ctx.report(daemon_core::Input::LinkDown {
        link: link.core(),
        reason,
    });
    let peer = link.peer();
    ctx.links.remove(&link).await;
    ctx.routing.apply_direct_down(link).await;
    if ctx.links.link_to_peer(peer).await.is_none() {
        ctx.routing.remove_relay_claims(peer).await;
    }
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
    use futures_util::future::BoxFuture;
    use tokio::sync::{Notify, mpsc};

    use super::*;
    use crate::link::{
        ByteStream, CarrierKind, ControlSink, ControlSource, LinkCarrier, MuxCarrier, MuxRole,
        OpenError,
    };
    use crate::routing::{Capabilities, LinkProperties};

    struct BlockingOpenCarrier {
        opened: Notify,
    }

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

    impl BlockingOpenCarrier {
        fn new() -> Self {
            Self {
                opened: Notify::new(),
            }
        }
    }

    impl LinkCarrier for BlockingOpenCarrier {
        fn kind(&self) -> CarrierKind {
            CarrierKind::RelayTcp
        }

        fn control(&self) -> (ControlSink, ControlSource) {
            panic!("registered destination carrier does not run a control loop")
        }

        fn open_stream(
            &self,
            _preface: wire::pb::StreamPreface,
        ) -> BoxFuture<'_, Result<ByteStream, OpenError>> {
            Box::pin(async move {
                self.opened.notify_one();
                future::pending().await
            })
        }

        fn accept_stream(&self) -> BoxFuture<'_, Option<(wire::pb::StreamPreface, ByteStream)>> {
            Box::pin(future::pending())
        }

        fn close(&self, _reason: wire::pb::LinkCloseReason) {}

        fn closed(&self) -> BoxFuture<'_, wire::pb::LinkCloseReason> {
            Box::pin(future::pending())
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

    fn mux_pair() -> (Arc<MuxCarrier>, Arc<MuxCarrier>) {
        let (connector_io, acceptor_io) = tokio::io::duplex(1024 * 1024);
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

    #[tokio::test(start_paused = true)]
    async fn authenticated_link_expires_without_reauthentication() {
        let local = host(1);
        let peer = host(2);
        let routing = Arc::new(RoutingCore::new());
        let links = Arc::new(LinkRegistry::default());
        let user = authenticated_user(
            Uuid::new_v4(),
            SystemTime::now() + Duration::from_secs(60),
            crate::Tier::Free,
        );
        let (connector_carrier, acceptor_carrier) = mux_pair();
        let ctx = LinkCtx::new(local, routing, links.clone())
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
        let routing = Arc::new(RoutingCore::new());
        let links = Arc::new(LinkRegistry::default());
        let (connector_carrier, acceptor_carrier) = mux_pair();
        let mut ctx = LinkCtx::new(local, routing, links).with_authenticated_peer(peer.id);
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
        let routing = Arc::new(RoutingCore::new());
        let links = Arc::new(LinkRegistry::default());
        let (connector_carrier, acceptor_carrier) = mux_pair();
        let ctx = LinkCtx::new(local, routing, links.clone()).with_authenticated_peer(peer.id);
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
        let routing = Arc::new(RoutingCore::new());
        let links = Arc::new(LinkRegistry::default());
        let (connector_carrier, acceptor_carrier) = mux_pair();
        let ctx = LinkCtx::new(local, routing, links.clone()).with_authenticated_peer(peer.id);
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

    #[tokio::test]
    async fn blocked_destination_open_does_not_stall_origin_control_messages() {
        let local = host(1);
        let origin = host(2);
        let destination = host(3);
        let announced = host(4);
        let routing = Arc::new(RoutingCore::new());
        let links = Arc::new(LinkRegistry::default());
        let destination_carrier = Arc::new(BlockingOpenCarrier::new());
        let destination_link = LinkId::new(destination.id);
        let (destination_messages, _destination_rx) = mpsc::channel(8);
        let _destination_close = links
            .register_with_details(
                destination_link,
                destination.clone(),
                destination_messages,
                LinkProperties {
                    role: LinkRole::Peer,
                    admission: LinkAdmission::PinnedKey,
                    carrier: RoutingCarrier::Direct,
                    runtime_instance: crate::routing::RuntimeInstance::random(),
                    direct_order: None,
                },
                &[],
                Some(destination_carrier.clone()),
            )
            .await;

        let (origin_io, local_io) = tokio::io::duplex(1024 * 1024);
        let origin_carrier = Arc::new(MuxCarrier::new(
            origin_io,
            MuxRole::Connector,
            CarrierKind::RelayTcp,
        ));
        let local_carrier = Arc::new(MuxCarrier::new(
            local_io,
            MuxRole::Acceptor,
            CarrierKind::RelayTcp,
        ));
        let ctx =
            LinkCtx::new(local.clone(), routing.clone(), links).with_authenticated_peer(origin.id);
        let link_task = tokio::spawn(run_link(
            ctx,
            local_carrier,
            crate::routing::ConnectRole::Acceptor,
        ));
        let (mut sink, mut source) = origin_carrier.control();
        write_message(
            &mut sink,
            &wire::pb::Message {
                body: Some(wire::pb::message::Body::Hello(wire::pb::Hello {
                    supported_protocol_versions: vec![PROTOCOL_VERSION],
                    host: Some(host_to_wire(&origin)),
                    neighbors: Vec::new(),
                    auth_token: None,
                    runtime_instance_id: crate::routing::RuntimeInstance::random().to_wire(),
                })),
            },
        )
        .await
        .unwrap();
        let ack = read_message(&mut source).await.unwrap().unwrap();
        assert!(matches!(
            ack.body,
            Some(wire::pb::message::Body::HelloAck(wire::pb::HelloAck {
                outcome: Some(wire::pb::hello_ack::Outcome::Accepted(_)),
            }))
        ));

        let stream_open = tokio::spawn({
            let carrier = origin_carrier.clone();
            async move {
                carrier
                    .open_stream(wire::pb::StreamPreface {
                        dst: destination.id.as_bytes().to_vec(),
                        plain: false,
                    })
                    .await
            }
        });
        tokio::time::timeout(
            Duration::from_secs(1),
            destination_carrier.opened.notified(),
        )
        .await
        .expect("origin stream did not reach the blocked destination");

        write_message(
            &mut sink,
            &wire::pb::Message {
                body: Some(wire::pb::message::Body::NeighborUp(wire::pb::NeighborUp {
                    host: Some(host_to_wire(&announced)),
                })),
            },
        )
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if routing.host_entry(announced.id).await.is_some() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("blocked stream dispatch stalled the origin control loop");

        stream_open.abort();
        link_task.abort();
    }
}
