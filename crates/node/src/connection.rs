//! Outbound channel selection over native link streams, and what a host's
//! presence is, as the profile's core decides it from how opening channels
//! to the host went.
//!
//! The core lists the routes to each host, picks the one carrying its
//! traffic, decides how it reads and retries it in the background; this
//! manager keeps what the core last told about each host, opens the
//! channels calls ask for on the route it names, and tries what the core
//! asks it to try.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use daemon_core::{HostView, Input, Tried};
use tonic::transport::Channel;
use wire::Presence;

use crate::HostId;
use crate::driver::core_error;
use crate::link::{ChannelClass, ChannelError, ChannelKey, ChannelPool};
use crate::routing::{
    LinkCarrier, LinkCloseRequest, LiveRoute, Route, RoutingCore, core_host, host_from_core,
};
use crate::transport::{TrustedPeerConnections, pairing_channel_from_io};

/// How long one try the core asks for may take to open its channel before
/// it counts as failed: as long as a call waits to reach a host.
const ATTEMPT_PATIENCE: Duration = Duration::from_secs(10);

pub struct ConnectionManager {
    /// This manager, for the tries it runs in the background.
    me: Weak<ConnectionManager>,
    routing: Arc<RoutingCore>,
    channels: Arc<ChannelPool>,
    trusted_connections: TrustedPeerConnections,
    /// What the profile's core last told about each host it has something
    /// to say about.
    views: Mutex<HashMap<HostId, View>>,
    reachability_errors: Mutex<HashMap<HostId, String>>,
    analytics: analytics::Analytics,
    /// What dials a host the core says to, once the edge dials at all.
    dialer: std::sync::OnceLock<crate::services::Dialer>,
}

/// A host as the profile's core last told it: every route listed to it,
/// the one carrying its traffic, and how it reads.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct View {
    routes: Vec<Route>,
    active: Option<Route>,
    presence: daemon_core::Presence,
}

impl View {
    fn from_core(host: HostId, view: HostView) -> Self {
        Self {
            routes: view
                .routes
                .into_iter()
                .map(|route| Route::from_core(host, route))
                .collect(),
            active: view.active.map(|route| Route::from_core(host, route)),
            presence: view.presence,
        }
    }
}

impl ConnectionManager {
    pub(crate) fn new(
        routing: Arc<RoutingCore>,
        channels: Arc<ChannelPool>,
        analytics: analytics::Analytics,
    ) -> Arc<Self> {
        Arc::new_cyclic(|me| Self {
            me: me.clone(),
            routing,
            channels,
            trusted_connections: TrustedPeerConnections::default(),
            views: Mutex::default(),
            reachability_errors: Mutex::default(),
            analytics,
            dialer: std::sync::OnceLock::new(),
        })
    }

    /// Tells the profile's core what happened, when the links are a
    /// profile's.
    fn report(&self, input: Input) {
        if let Some(core) = self.channels.link_registry().core() {
            core.report(input);
        }
    }

    /// The core told how `host` reads and is reached now.
    pub(crate) fn told(&self, host: daemon_core::HostId, view: HostView) {
        let host = host_from_core(host);
        let view = View::from_core(host, view);
        let before = self
            .views
            .lock()
            .unwrap()
            .insert(host, view.clone())
            .unwrap_or_default();
        // A route coming up says the last dial error is out of date.
        if view
            .routes
            .iter()
            .any(|route| !before.routes.contains(route))
        {
            self.reachability_errors.lock().unwrap().remove(&host);
        }
    }

    /// The core asks for a calls channel to `host` on `route`, and to be
    /// told how it went. A host this one does not trust, or that said it
    /// no longer trusts this one, is not tried: pairing again opens one of
    /// its own. A host a channel on another route is open to answers this
    /// one, so what it said before is out of date: it is tried, and its
    /// answer on the new channel clears what it said. It may have refused
    /// a stream only because its half of the pairing had not landed yet.
    pub(crate) fn attempt(&self, host: daemon_core::HostId, route: daemon_core::Route) {
        let me = self.me.clone();
        tokio::spawn(async move {
            let Some(manager) = me.upgrade() else {
                return;
            };
            let peer = host_from_core(host);
            let answering = manager
                .views
                .lock()
                .unwrap()
                .get(&peer)
                .is_some_and(|view| view.active.is_some());
            let revoked = manager.routing.revocations().contains(peer) && !answering;
            let tried = if revoked || !manager.routing.trusts(peer) {
                Tried::Abandoned
            } else {
                let opened = tokio::time::timeout(
                    ATTEMPT_PATIENCE,
                    manager.open_route(peer, Route::from_core(peer, route), ChannelClass::Calls),
                )
                .await;
                match opened {
                    Ok(Ok(_)) => Tried::Opened,
                    Ok(Err(error)) => Tried::Failed(core_error(&error)),
                    Err(_) => Tried::Failed(daemon_core::payload::ChannelError::Handshake(
                        "no channel opened in time".to_owned(),
                    )),
                }
            };
            manager.report(Input::AttemptConcluded { host, route, tried });
        });
    }

    /// Discovery resolved `host` in this profile's scope.
    pub(crate) fn discovered(&self, host: HostId, trusted: bool) {
        self.report(Input::Discovered {
            found: vec![daemon_core::payload::Advertisement {
                host: core_host(host),
                trusted,
            }],
        });
    }

    pub(crate) fn set_dialer(&self, dialer: crate::services::Dialer) {
        let _ = self.dialer.set(dialer);
    }

    /// The core says to dial `host`.
    pub(crate) fn dial(&self, host: daemon_core::HostId) {
        if let Some(dialer) = self.dialer.get() {
            dialer.dial(host_from_core(host));
        }
    }

    /// The core says `route` no longer carries `host`'s traffic: the
    /// channels cached on it go, so the next call opens a fresh one.
    pub(crate) fn drop_route(&self, host: daemon_core::HostId, route: daemon_core::Route) {
        let peer = host_from_core(host);
        self.channels
            .drop_route(peer, Route::from_core(peer, route));
    }

    /// From now on no channel to `peer` opens while `refuse` holds,
    /// whatever route reaches it. Lifting it tries a channel at once, as a
    /// link that starts carrying calls again would be found to.
    pub(crate) fn refuse_calls(&self, peer: HostId, refuse: bool) {
        self.channels.refuse(peer, refuse);
        if !refuse {
            self.report(Input::TryAgain {
                host: core_host(peer),
            });
        }
    }

    pub(crate) fn trusted_connections(&self) -> TrustedPeerConnections {
        self.trusted_connections.clone()
    }

    /// A channel for calls to `peer`.
    pub async fn channel_to(&self, peer: HostId) -> Result<Channel, ChannelError> {
        self.channel_to_class(peer, ChannelClass::Calls).await
    }

    /// A call to `peer` found no channel on the route that lists it within
    /// its patience, or one failed to open for a reason other than the
    /// account's plan: the host reads offline until a channel opens again.
    ///
    /// The host's own direct or SSH link is closed, as its idle timeout
    /// would close it, only sooner; redialling brings it back as it does
    /// any lost link, and the new link's first channel says whether calls
    /// get through. Any other route, the relay's tunnel or a link through
    /// another host, cannot be closed for this host alone: the channel
    /// cached on it is dropped, so the next try opens a fresh stream, and
    /// one is tried in the background until it opens.
    pub(crate) async fn calls_failed(&self, peer: HostId) {
        let Some(route) = self.route_for(peer).await else {
            return;
        };
        tracing::warn!(%peer, ?route, "no call channel opened on a listed route; the host reads offline");
        self.report(Input::CallsFailed {
            host: core_host(peer),
        });
        match route {
            Route::Direct(link) if link.peer() == peer => {
                self.channels
                    .link_registry()
                    .close_host(peer, LinkCloseRequest::CallsFailed)
                    .await;
            }
            route => self.channels.drop_route(peer, route),
        }
    }

    /// The account's plan changed, so what the relay carries changed with
    /// it; the core tries again every host the relay carried or refused.
    pub(crate) fn plan_changed(&self) {
        self.report(Input::PlanChanged);
    }

    pub async fn session_channel_to(&self, peer: HostId) -> Result<Channel, ChannelError> {
        self.channel_to_class(peer, ChannelClass::Session).await
    }

    pub async fn bulk_channel_to(&self, peer: HostId) -> Result<Channel, ChannelError> {
        self.channel_to_class(peer, ChannelClass::Bulk).await
    }

    async fn channel_to_class(
        &self,
        peer: HostId,
        class: ChannelClass,
    ) -> Result<Channel, ChannelError> {
        let route = self
            .routes_to(peer)
            .first()
            .copied()
            .ok_or(ChannelError::NoRoute { host_id: peer })?;
        let opened = self.open_route(peer, route, class).await;
        if let Err(ChannelError::Refused(wire::pb::StreamRefusal::PaymentRequired)) = &opened {
            self.analytics.record_at_most_every(
                crate::telemetry::RELAY_REFUSED_EVERY,
                analytics::Event::RelayRefused,
            );
        }
        opened
    }

    /// Opens a channel to `peer` on `route`. A direct link can close under
    /// a call that just chose it: the preferred of two crossed dials
    /// supersedes the other, or the peer ends the link, and the core may
    /// still list the closed link for a moment. The call tries the next
    /// route listed, once: the link that replaced it, or the relay, which
    /// lets a host that revoked this one say so. The link going under it
    /// says nothing of the host to the core; the link's end does.
    async fn open_route(
        &self,
        peer: HostId,
        route: Route,
        class: ChannelClass,
    ) -> Result<Channel, ChannelError> {
        match self.activate_route(peer, route, class).await {
            Err(error @ ChannelError::LinkUnavailable { .. }) if route.is_direct() => {
                let next = self.routes_to(peer).into_iter().find(|next| *next != route);
                match next {
                    Some(next) => self.activate_route(peer, next, class).await,
                    None => Err(error),
                }
            }
            result => result,
        }
    }

    pub(crate) async fn cloud_pairing_channel_to(
        &self,
        peer: HostId,
    ) -> Result<Channel, ChannelError> {
        let relay = self.cloud_relay_for(peer).await?;
        let stream = self
            .channels
            .pairing_stream(peer, Route::Via(relay))
            .await?;
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            pairing_channel_from_io(stream),
        )
        .await
        .map_err(|_| ChannelError::Handshake("pairing TLS handshake timed out".into()))?
        .map_err(|error| ChannelError::Tls(error.to_string()))
    }

    pub(crate) async fn has_cloud_route(&self, peer: HostId) -> bool {
        self.cloud_relay_for(peer).await.is_ok()
    }

    pub(crate) async fn stored_reachability_error(&self, peer: HostId) -> Option<String> {
        self.reachability_errors.lock().unwrap().get(&peer).cloned()
    }

    pub(crate) async fn record_reachability_error(&self, peer: HostId, error: impl Into<String>) {
        self.reachability_errors
            .lock()
            .unwrap()
            .insert(peer, error.into());
    }

    pub(crate) async fn clear_reachability_error(&self, peer: HostId) {
        self.reachability_errors.lock().unwrap().remove(&peer);
    }

    /// Every route the core lists to `peer`, the one a fresh call takes
    /// first.
    pub(crate) fn routes_to(&self, peer: HostId) -> Vec<Route> {
        self.views
            .lock()
            .unwrap()
            .get(&peer)
            .map(|view| view.routes.clone())
            .unwrap_or_default()
    }

    /// The route calls to `peer` take now: the one carrying traffic, or
    /// the one a fresh call would take.
    pub(crate) async fn route_for(&self, peer: HostId) -> Option<Route> {
        let views = self.views.lock().unwrap();
        let view = views.get(&peer)?;
        view.active.or_else(|| view.routes.first().copied())
    }

    pub(crate) async fn via_for(&self, peer: HostId) -> LiveRoute {
        match self.route_for(peer).await {
            Some(Route::Via(_)) => LiveRoute::Relay,
            Some(Route::Direct(link)) => match self.channels.link_registry().carrier(&link).await {
                Some(LinkCarrier::Ssh) => LiveRoute::Ssh,
                Some(LinkCarrier::Direct) => LiveRoute::Direct,
                None => LiveRoute::Offline,
            },
            None => LiveRoute::Offline,
        }
    }

    /// How `peer` reads, and the route that carries its calls: online once
    /// a channel to it has opened on a route still listed, away while the
    /// relay refuses channels to it for the account's plan, offline
    /// otherwise, with no route then.
    pub(crate) async fn presence_for(&self, peer: HostId) -> (Presence, LiveRoute) {
        let via = self.via_for(peer).await;
        let reads = self
            .views
            .lock()
            .unwrap()
            .get(&peer)
            .map_or(daemon_core::Presence::Offline, |view| view.presence);
        match (via, reads) {
            (LiveRoute::Offline, _) | (_, daemon_core::Presence::Offline) => {
                (Presence::Offline, LiveRoute::Offline)
            }
            (via, daemon_core::Presence::Online) => (Presence::Online, via),
            (via, daemon_core::Presence::Away) => (Presence::Away, via),
        }
    }

    pub(crate) async fn teardown_host(&self, peer: HostId) {
        self.routing.begin_replacement(peer).await;
        self.routing.remove_host(peer).await;
        self.forget(peer);
        self.trusted_connections.close_host(peer).await;
        self.channels
            .link_registry()
            .close_host(peer, LinkCloseRequest::TrustReplaced)
            .await;
        self.forget(peer);
    }

    /// Ends live access to `peer` — trusted streams, channels, links and our
    /// own routes — without forgetting the relay's word that it is online.
    /// Used when a peer is unpaired: the trust is gone and nothing of the old
    /// connection survives, but the peer stays reachable for pairing, the way
    /// any other machine on the account is before it is ever paired.
    ///
    /// The links close first, saying USER_REVOKED, while nothing else has
    /// cut them: that is how the peer learns it is no longer trusted.
    ///
    /// Closing ends with the host admitted again: its key has already left
    /// the trust store, so nothing of it gets back in unless it is paired
    /// again, and then its new streams must not be closed as they arrive.
    pub(crate) async fn close_host_access(&self, peer: HostId) {
        self.routing.revocations().clear(peer);
        self.channels
            .link_registry()
            .close_host(peer, LinkCloseRequest::Revoked)
            .await;
        self.routing.remove_direct_links(peer).await;
        self.forget(peer);
        self.trusted_connections.close_host(peer).await;
        self.forget(peer);
        self.trusted_connections.finish_host_replacement(peer);
    }

    /// This host has just paired with `peer`: whatever `peer` said about
    /// no longer trusting it is out of date, and streams waiting to reopen
    /// to it try again now.
    pub(crate) fn trust_renewed(&self, peer: HostId) {
        self.routing.revocations().clear(peer);
    }

    pub(crate) async fn finish_host_replacement(&self, peer: HostId) {
        self.routing.finish_replacement(peer).await;
        self.trusted_connections.finish_host_replacement(peer);
    }

    /// Opens a channel to `peer` on `route`, while the core still lists it.
    async fn activate_route(
        &self,
        peer: HostId,
        route: Route,
        class: ChannelClass,
    ) -> Result<Channel, ChannelError> {
        let channel = self
            .channels
            .channel(ChannelKey { peer, route, class })
            .await?;
        if !self.routes_to(peer).contains(&route) {
            self.channels.drop_host(peer);
            return Err(ChannelError::NoRoute { host_id: peer });
        }
        self.clear_reachability_error(peer).await;
        Ok(channel)
    }

    /// Nothing the core decided about `peer` survives, and no channel to
    /// it either.
    fn forget(&self, peer: HostId) {
        self.report(Input::HostForgotten {
            host: core_host(peer),
        });
        self.channels.drop_host(peer);
    }

    async fn cloud_relay_for(&self, peer: HostId) -> Result<HostId, ChannelError> {
        let registry = self.channels.link_registry();
        for relay in self.routing.relays_to(peer).await {
            if registry.has_cloud_relay_link_to(relay).await {
                return Ok(relay);
            }
        }
        Err(ChannelError::CloudPairingUnavailable)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use futures_util::future::{self, BoxFuture};
    use tokio::sync::mpsc;
    use uuid::Uuid;

    use super::*;
    use crate::link::{ByteStream, CarrierKind, ControlSink, ControlSource, OpenError};
    use crate::routing::{
        Capabilities, Host, LinkAdmission, LinkCarrier as RoutingCarrier, LinkId, LinkProperties,
        LinkRegistry, LinkRole,
    };

    /// Tells `manager` the core lists `link` alone to the peer.
    fn listed(manager: &ConnectionManager, link: LinkId) {
        manager.told(
            core_host(peer().id),
            HostView {
                routes: vec![daemon_core::Route::Direct(link.core())],
                ..HostView::default()
            },
        );
    }

    fn peer() -> Host {
        Host {
            platform: None,
            id: Uuid::from_u128(2),
            name: "peer".to_string(),
            version: "test".to_string(),
            capabilities: Capabilities {
                cloud_relay: false,
                kinds: Vec::new(),
            },
            signed_in: Some(true),
        }
    }

    /// A link that is superseded while a stream is being opened on it: the
    /// core lists the replacement in its place, and the open fails the way
    /// a closed QUIC connection fails it.
    struct SupersededDuringOpen {
        manager: std::sync::OnceLock<Weak<ConnectionManager>>,
        replacement: LinkId,
    }

    /// A link that answers every open with a refusal, recording that it was
    /// asked.
    #[derive(Default)]
    struct Refusing {
        asked: AtomicBool,
    }

    impl crate::link::LinkCarrier for SupersededDuringOpen {
        fn kind(&self) -> CarrierKind {
            CarrierKind::Quic
        }

        fn control(&self) -> (ControlSink, ControlSource) {
            panic!("the test drives no control loop")
        }

        fn open_stream(
            &self,
            _preface: wire::pb::StreamPreface,
        ) -> BoxFuture<'_, Result<ByteStream, OpenError>> {
            Box::pin(async move {
                let manager = self.manager.get().and_then(Weak::upgrade);
                listed(&manager.expect("the manager is set"), self.replacement);
                Err(OpenError::LinkClosed)
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

    impl crate::link::LinkCarrier for Refusing {
        fn kind(&self) -> CarrierKind {
            CarrierKind::Quic
        }

        fn control(&self) -> (ControlSink, ControlSource) {
            panic!("the test drives no control loop")
        }

        fn open_stream(
            &self,
            _preface: wire::pb::StreamPreface,
        ) -> BoxFuture<'_, Result<ByteStream, OpenError>> {
            self.asked.store(true, Ordering::SeqCst);
            Box::pin(async { Err(OpenError::Refused(wire::pb::StreamRefusal::ShuttingDown)) })
        }

        fn accept_stream(&self) -> BoxFuture<'_, Option<(wire::pb::StreamPreface, ByteStream)>> {
            Box::pin(future::pending())
        }

        fn close(&self, _reason: wire::pb::LinkCloseReason) {}

        fn closed(&self) -> BoxFuture<'_, wire::pb::LinkCloseReason> {
            Box::pin(future::pending())
        }
    }

    async fn register(
        links: &LinkRegistry,
        link: LinkId,
        carrier: Arc<dyn crate::link::LinkCarrier>,
    ) -> mpsc::Receiver<crate::routing::LinkCloseRequest> {
        let (tx, _rx) = mpsc::channel(8);
        links
            .register_with_details(
                link,
                peer(),
                tx,
                LinkProperties {
                    role: LinkRole::Peer,
                    admission: LinkAdmission::PinnedKey,
                    carrier: RoutingCarrier::Direct,
                    runtime_instance: crate::routing::RuntimeInstance::random(),
                    direct_order: None,
                },
                &[],
                Some(carrier),
            )
            .await
            .expect("a lone link registers")
    }

    #[tokio::test]
    async fn a_call_on_a_link_superseded_while_opening_retries_on_its_replacement() {
        let links = Arc::new(LinkRegistry::default());
        let manager = ConnectionManager::new(
            Arc::new(RoutingCore::new()),
            Arc::new(ChannelPool::new(links.clone())),
            analytics::Analytics::off(),
        );
        let superseded = LinkId::new(peer().id);
        let replacement = LinkId::new(peer().id);
        let refusing = Arc::new(Refusing::default());
        let superseding = Arc::new(SupersededDuringOpen {
            manager: std::sync::OnceLock::new(),
            replacement,
        });
        let _ = superseding.manager.set(Arc::downgrade(&manager));
        let _superseded_close = register(&links, superseded, superseding).await;
        let _replacement_close = register(&links, replacement, refusing.clone()).await;
        listed(&manager, superseded);

        let error = manager.channel_to(peer().id).await.unwrap_err();

        assert!(
            refusing.asked.load(Ordering::SeqCst),
            "the replacement link was never tried"
        );
        assert!(
            matches!(
                error,
                ChannelError::Refused(wire::pb::StreamRefusal::ShuttingDown)
            ),
            "the call ended on the superseded link: {error}"
        );
    }
}
