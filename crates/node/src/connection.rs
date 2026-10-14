//! Outbound channel selection over native link streams, and what a host's
//! presence is: the outcome of opening a channel to it.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use tokio::sync::{Notify, RwLock};
use tokio::task::JoinHandle;
use tonic::transport::Channel;
use wire::Presence;

use crate::HostId;
use crate::link::{ChannelClass, ChannelError, ChannelKey, ChannelPool};
use crate::routing::{LinkCarrier, LinkCloseRequest, LiveRoute, Route, RoutingCore, RoutingEvent};
use crate::transport::{TrustedPeerConnections, pairing_channel_from_io};

/// How long after a channel to a listed host failed to open the next one
/// is tried, doubling after each failure up to [`RETRY_MAX`].
const RETRY_FIRST: Duration = Duration::from_secs(1);
const RETRY_MAX: Duration = Duration::from_secs(30);
/// How long one try may take to open its channel before it counts as
/// failed: as long as a call waits to reach a host.
const RETRY_PATIENCE: Duration = Duration::from_secs(10);

pub struct ConnectionManager {
    /// This manager, for the retries it starts in the background.
    me: Weak<ConnectionManager>,
    routing: Arc<RoutingCore>,
    channels: Arc<ChannelPool>,
    trusted_connections: TrustedPeerConnections,
    state: RwLock<ConnectionState>,
    analytics: analytics::Analytics,
    /// Hosts no channel opens to while the link stays up: a fault a test
    /// network injects.
    refused_calls: Mutex<HashSet<HostId>>,
}

#[derive(Default)]
struct ConnectionState {
    /// The route each host's channels last opened on: the one carrying its
    /// traffic.
    active: HashMap<HostId, Route>,
    /// What the last channel attempt to each host that concluded found:
    /// online once a channel opened, away once the relay refused one
    /// because the account's plan carries no connection between hosts. A
    /// host missing here reads offline. A route coming up changes nothing
    /// here until an attempt on it concludes, so a host that connects goes
    /// from how it read straight to what the attempt found, and a host
    /// whose route went while another stays listed reads as it did until
    /// the attempt on the other one concludes.
    reached: HashMap<HostId, Presence>,
    reachability_errors: HashMap<HostId, String>,
    /// What each host was last described as, so a description is sent when
    /// the answer has moved and not merely when something happened.
    announced: HashMap<HostId, HostDescription>,
    /// The background retry running for each listed host no channel is
    /// open to, and what wakes it to try at once.
    retrying: HashMap<HostId, Arc<Notify>>,
}

/// What a client would be told about a host: the route carrying its traffic,
/// how that reads, its presence, and the account-binding fact recorded for
/// it.
///
/// The route's identity belongs here beside the way it reads, because one
/// direct link replacing another reads the same both times and still ends
/// every stream on the carrier that went away.
type HostDescription = (Option<Route>, LiveRoute, Presence, Option<bool>);

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
            state: RwLock::new(ConnectionState::default()),
            analytics,
            refused_calls: Mutex::default(),
        })
    }

    /// From now on no channel to `peer` opens while `refuse` holds,
    /// whatever route reaches it. Lifting it tries a channel at once, as a
    /// link that starts carrying calls again would be found to.
    pub(crate) async fn refuse_calls(&self, peer: HostId, refuse: bool) {
        {
            let mut refused = self.refused_calls.lock().unwrap();
            if refuse {
                refused.insert(peer);
            } else {
                refused.remove(&peer);
            }
        }
        if !refuse {
            self.retry(peer, true).await;
        }
    }

    pub(crate) fn trusted_connections(&self) -> TrustedPeerConnections {
        self.trusted_connections.clone()
    }

    pub(crate) async fn attach_routing_events(self: Arc<Self>) -> JoinHandle<()> {
        for event in self.routing.routing_events_snapshot().await {
            self.handle_event(event).await;
        }
        let mut rx = self.routing.subscribe_routing_events().await;
        tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                self.handle_event(event).await;
            }
        })
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
        {
            let mut state = self.state.write().await;
            state.active.remove(&peer);
            state.reached.remove(&peer);
        }
        self.describe_again(peer).await;
        match route {
            Route::Direct(link) if link.peer() == peer => {
                self.channels
                    .link_registry()
                    .close_host(peer, LinkCloseRequest::CallsFailed)
                    .await;
            }
            route => self.channels.drop_route(peer, route),
        }
        self.retry(peer, false).await;
    }

    /// The account's plan changed, so what the relay carries changed with
    /// it: a host it refused for the plan may open now, and a channel it
    /// carried on the old plan ended if the plan lapsed. Every host the
    /// relay carries or refused is tried again at once, on a fresh
    /// channel; each reads as it did until its try concludes.
    pub(crate) async fn plan_changed(&self) {
        let hosts: Vec<(HostId, Option<Route>)> = {
            let mut state = self.state.write().await;
            let relayed: Vec<(HostId, Route)> = state
                .active
                .iter()
                .filter(|(_, route)| !route.is_direct())
                .map(|(peer, route)| (*peer, *route))
                .collect();
            for (peer, _) in &relayed {
                state.active.remove(peer);
            }
            relayed
                .into_iter()
                .map(|(peer, route)| (peer, Some(route)))
                .chain(
                    state
                        .reached
                        .iter()
                        .filter(|(_, presence)| **presence == Presence::Away)
                        .map(|(peer, _)| (*peer, None)),
                )
                .collect()
        };
        for (peer, route) in hosts {
            if let Some(route) = route {
                self.channels.drop_route(peer, route);
            }
            self.retry(peer, true).await;
        }
    }

    /// Keeps trying a channel to `peer` in the background while a route
    /// lists it and none is open, backing off between tries; `now` tries
    /// at once, and starts the backoff afresh. One retry runs per host:
    /// asking again while it runs only wakes it when `now` says so.
    async fn retry(&self, peer: HostId, now: bool) {
        let kick = {
            let mut state = self.state.write().await;
            if let Some(kick) = state.retrying.get(&peer) {
                if now {
                    kick.notify_one();
                }
                return;
            }
            let kick = Arc::new(Notify::new());
            state.retrying.insert(peer, kick.clone());
            kick
        };
        if now {
            // Stored until the retry first waits, which then returns at once.
            kick.notify_one();
        }
        tokio::spawn(Self::retrying(self.me.clone(), peer, kick));
    }

    /// The retry [`Self::retry`] starts, boxed: a try records its outcome,
    /// which can ask for a retry in turn.
    fn retrying(
        me: Weak<Self>,
        peer: HostId,
        kick: Arc<Notify>,
    ) -> futures_util::future::BoxFuture<'static, ()> {
        Box::pin(async move {
            let mut pause = RETRY_FIRST;
            loop {
                let kicked = tokio::select! {
                    () = tokio::time::sleep(pause) => false,
                    () = kick.notified() => true,
                };
                let Some(manager) = me.upgrade() else {
                    return;
                };
                let route = {
                    let mut state = manager.state.write().await;
                    if !state
                        .retrying
                        .get(&peer)
                        .is_some_and(|ours| Arc::ptr_eq(ours, &kick))
                    {
                        return;
                    }
                    // A host that said it no longer trusts this one refuses
                    // every channel until that changes, and pairing again
                    // opens one of its own.
                    let revoked = manager.routing.revocations().contains(peer);
                    match manager.routing.route_to(peer).await {
                        Some(route) if !state.active.contains_key(&peer) && !revoked => route,
                        _ => {
                            state.retrying.remove(&peer);
                            return;
                        }
                    }
                };
                let tried = tokio::time::timeout(
                    RETRY_PATIENCE,
                    manager.channel_to_class(peer, ChannelClass::Calls),
                )
                .await;
                match tried {
                    Ok(Ok(_)) => {
                        let mut state = manager.state.write().await;
                        if state
                            .retrying
                            .get(&peer)
                            .is_some_and(|ours| Arc::ptr_eq(ours, &kick))
                        {
                            state.retrying.remove(&peer);
                        }
                        return;
                    }
                    Ok(Err(_)) => {}
                    Err(_) => {
                        let error = ChannelError::Handshake("no channel opened in time".to_owned());
                        manager.channel_failed(peer, route, &error).await;
                    }
                }
                pause = if kicked {
                    RETRY_FIRST
                } else {
                    (pause * 2).min(RETRY_MAX)
                };
            }
        })
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
            .routing
            .route_to(peer)
            .await
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

    /// Opens a channel to `peer` on `route`, and records what that found.
    async fn open_route(
        &self,
        peer: HostId,
        route: Route,
        class: ChannelClass,
    ) -> Result<Channel, ChannelError> {
        let (route, opened) = match self.activate_route(peer, route, class).await {
            // A direct link can close under a call that just chose it: the
            // preferred of two crossed dials supersedes the other, or the
            // peer ends the link. Routing may still list the closed link for
            // a moment, since the link leaves the registry before it leaves
            // routing. Try the next route routing holds, once: the link that
            // replaced it, or the relay, which lets a host that revoked this
            // one say so.
            Err(error @ ChannelError::LinkUnavailable { .. }) if route.is_direct() => {
                let next = self
                    .routing
                    .routes_to(peer)
                    .await
                    .into_iter()
                    .find(|next| *next != route);
                match next {
                    // When the next route fails too, both failures are
                    // recorded: the one the routing table prefers by now
                    // is the one that says how the host reads.
                    Some(next) => {
                        let opened = self.activate_route(peer, next, class).await;
                        if opened.is_err() {
                            self.channel_failed(peer, route, &error).await;
                        }
                        (next, opened)
                    }
                    None => (route, Err(error)),
                }
            }
            result => (route, result),
        };
        if let Err(error) = &opened {
            self.channel_failed(peer, route, error).await;
        }
        opened
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

    #[cfg(test)]
    pub(crate) fn routing(&self) -> &Arc<RoutingCore> {
        &self.routing
    }

    #[cfg(test)]
    pub async fn known_routes(&self, peer: HostId) -> Vec<Route> {
        self.routing.routes_to(peer).await
    }

    pub(crate) async fn stored_reachability_error(&self, peer: HostId) -> Option<String> {
        self.state
            .read()
            .await
            .reachability_errors
            .get(&peer)
            .cloned()
    }

    pub(crate) async fn record_reachability_error(&self, peer: HostId, error: impl Into<String>) {
        self.state
            .write()
            .await
            .reachability_errors
            .insert(peer, error.into());
    }

    pub(crate) async fn clear_reachability_error(&self, peer: HostId) {
        self.state.write().await.reachability_errors.remove(&peer);
    }

    /// The route calls to `peer` take now: the one carrying traffic, or
    /// the one routing would pick.
    pub(crate) async fn route_for(&self, peer: HostId) -> Option<Route> {
        let active = self.state.read().await.active.get(&peer).copied();
        match active {
            Some(route) => Some(route),
            None => self.routing.route_to(peer).await,
        }
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
        let reached = self.state.read().await.reached.get(&peer).copied();
        match (via, reached) {
            (LiveRoute::Offline, _) | (_, None) => (Presence::Offline, LiveRoute::Offline),
            (via, Some(presence)) => (presence, via),
        }
    }

    pub(crate) async fn teardown_host(&self, peer: HostId) {
        self.routing.begin_replacement(peer).await;
        self.routing.remove_host(peer).await;
        self.remove_host_runtime_state(peer).await;
        self.trusted_connections.close_host(peer).await;
        self.channels
            .link_registry()
            .close_host(peer, LinkCloseRequest::TrustReplaced)
            .await;
        self.remove_host_runtime_state(peer).await;
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
        self.remove_host_runtime_state(peer).await;
        self.trusted_connections.close_host(peer).await;
        self.remove_host_runtime_state(peer).await;
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

    /// Applies one routing event, then describes the host again if what a
    /// client would be told about it has moved.
    ///
    /// The routing table knows where a host can be reached; this manager knows
    /// which of those routes is carrying traffic, and it is the second that a
    /// client is shown and that its inventory subscription rides on.
    ///
    /// The comparison is against what was last announced rather than against
    /// the moment before this event, because the active route is not only
    /// settled here: an outgoing call opens a channel on whatever the routing
    /// table prefers, so a direct link can already be carrying traffic by the
    /// time its own event arrives. Read as a difference across the handler,
    /// that would look like nothing happening; read against what a client was
    /// last told, it is exactly the change the client is waiting for.
    async fn handle_event(&self, event: RoutingEvent) {
        let host_id = event.host_id();
        // A host nobody has described yet was just announced as present by the
        // routing table, and that announcement carried whatever was true as
        // this event was queued. That is the client's starting point, so it is
        // this one's: a first link that changes nothing after it is applied
        // leaves the host correctly described and says nothing further.
        if !self.state.read().await.announced.contains_key(&host_id) {
            let baseline = self.host_description(host_id).await;
            self.state
                .write()
                .await
                .announced
                .entry(host_id)
                .or_insert(baseline);
        }
        self.apply_event(event).await;
        self.describe_again(host_id).await;
    }

    /// Describes `host_id` again after something moved it, announcing it
    /// if what a client would be told moved.
    async fn describe_again(&self, host_id: HostId) {
        let description = self.host_description(host_id).await;
        let before = self
            .state
            .write()
            .await
            .announced
            .insert(host_id, description);
        if before != Some(description) {
            self.routing.announce_route(host_id).await;
        }
    }

    async fn host_description(&self, host_id: HostId) -> HostDescription {
        let active = self.state.read().await.active.get(&host_id).copied();
        let (presence, via) = self.presence_for(host_id).await;
        (active, via, presence, self.routing.signed_in_for(host_id))
    }

    /// Applies a routing event. A route that comes up is tried at once, so
    /// the host's presence follows what the first channel on it finds.
    async fn apply_event(&self, event: RoutingEvent) {
        match event {
            RoutingEvent::NeighborUp { host, link } => {
                self.clear_reachability_error(host.id).await;
                if host.capabilities.cloud_relay {
                    return;
                }
                let already_direct = matches!(
                    self.state.read().await.active.get(&host.id),
                    Some(Route::Direct(_))
                );
                if !already_direct
                    && let Err(error) = self
                        .open_route(host.id, Route::Direct(link), ChannelClass::Calls)
                        .await
                {
                    tracing::warn!(peer = %host.id, error = %error, "failed to activate direct route");
                }
            }
            RoutingEvent::NeighborDown { host_id, link, .. } => {
                self.channels.drop_link(link);
                self.route_gone(host_id, Route::Direct(link)).await;
            }
            RoutingEvent::ClaimUp { relay, host } => {
                self.clear_reachability_error(host.id).await;
                if host.capabilities.cloud_relay
                    || self.state.read().await.active.contains_key(&host.id)
                {
                    return;
                }
                // The cloud relay claims every host on the account, paired
                // or not, so only a host this one trusts is tried, and in
                // the background: its tunnel takes a round trip through the
                // relay, which routing events must not wait behind.
                if self
                    .channels
                    .link_registry()
                    .has_cloud_relay_link_to(relay)
                    .await
                {
                    if self.routing.trusts(host.id) {
                        self.retry(host.id, true).await;
                    }
                    return;
                }
                if let Err(error) = self
                    .open_route(host.id, Route::Via(relay), ChannelClass::Calls)
                    .await
                {
                    tracing::warn!(peer = %host.id, relay = %relay, error = %error, "failed to activate relay route");
                }
            }
            RoutingEvent::ClaimDown { relay, host_id } => {
                self.channels.drop_route(host_id, Route::Via(relay));
                self.route_gone(host_id, Route::Via(relay)).await;
            }
        }
    }

    /// `route` to `peer` is no longer listed. A host with no route left
    /// reads offline. One whose traffic the route carried while another
    /// stays listed reads as it did while a channel is tried on the other
    /// at once.
    async fn route_gone(&self, peer: HostId, route: Route) {
        let listed = self.routing.route_to(peer).await.is_some();
        let carried = {
            let mut state = self.state.write().await;
            let carried = state.active.get(&peer) == Some(&route);
            if carried || !listed {
                state.active.remove(&peer);
            }
            if !listed {
                state.reached.remove(&peer);
            }
            carried
        };
        if carried && listed {
            self.retry(peer, true).await;
        }
    }

    async fn activate_route(
        &self,
        peer: HostId,
        route: Route,
        class: ChannelClass,
    ) -> Result<Channel, ChannelError> {
        if self.refused_calls.lock().unwrap().contains(&peer) {
            return Err(ChannelError::Refused(wire::pb::StreamRefusal::NoRoute));
        }
        let channel = self
            .channels
            .channel(ChannelKey { peer, route, class })
            .await?;
        let old = {
            let mut state = self.state.write().await;
            if !self.routing.routes_to(peer).await.contains(&route) {
                drop(state);
                self.channels.drop_host(peer);
                return Err(ChannelError::NoRoute { host_id: peer });
            }
            state.reached.insert(peer, Presence::Online);
            match state.active.get(&peer) {
                Some(active @ Route::Direct(_)) if !route.is_direct() && *active != route => None,
                _ => state.active.insert(peer, route),
            }
        };
        if let Some(old_route) = old
            && old_route != route
        {
            self.channels.drop_route(peer, old_route);
        }
        self.clear_reachability_error(peer).await;
        self.describe_again(peer).await;
        Ok(channel)
    }

    /// A channel to `peer` on `route` did not open. When that is the
    /// route a call would take now and no other carries the host's
    /// traffic, the host reads away if the relay refused it for the
    /// account's plan, and offline for any other reason, and a channel is
    /// tried again in the background. A failure on a route routing no
    /// longer prefers says nothing: the route that replaced it is tried
    /// on its own.
    async fn channel_failed(&self, peer: HostId, route: Route, error: &ChannelError) {
        if self.routing.route_to(peer).await != Some(route) {
            return;
        }
        let carried = {
            let mut state = self.state.write().await;
            if state.active.get(&peer) == Some(&route) {
                state.active.remove(&peer);
            }
            let carried = state.active.contains_key(&peer);
            if !carried {
                match error {
                    ChannelError::Refused(wire::pb::StreamRefusal::PaymentRequired) => {
                        state.reached.insert(peer, Presence::Away);
                    }
                    _ => {
                        state.reached.remove(&peer);
                    }
                }
            }
            carried
        };
        self.describe_again(peer).await;
        if !carried {
            self.retry(peer, false).await;
        }
    }

    async fn remove_host_runtime_state(&self, peer: HostId) {
        let mut state = self.state.write().await;
        state.active.remove(&peer);
        state.reached.remove(&peer);
        // Dropping the host's retry stops it.
        state.retrying.remove(&peer);
        // A host that is gone is described afresh if it ever comes back.
        state.announced.remove(&peer);
        drop(state);
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
    /// replacement joins routing, this link leaves it, and the open fails the
    /// way a closed QUIC connection fails it.
    struct SupersededDuringOpen {
        routing: Arc<RoutingCore>,
        superseded: LinkId,
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
                self.routing.apply_direct_up(peer(), self.replacement).await;
                self.routing.apply_direct_down(self.superseded).await;
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
        let routing = Arc::new(RoutingCore::new());
        let manager = ConnectionManager::new(
            routing.clone(),
            Arc::new(ChannelPool::new(links.clone())),
            analytics::Analytics::off(),
        );
        let superseded = LinkId::new(peer().id);
        let replacement = LinkId::new(peer().id);
        let refusing = Arc::new(Refusing::default());
        let _superseded_close = register(
            &links,
            superseded,
            Arc::new(SupersededDuringOpen {
                routing: routing.clone(),
                superseded,
                replacement,
            }),
        )
        .await;
        let _replacement_close = register(&links, replacement, refusing.clone()).await;
        routing.apply_direct_up(peer(), superseded).await;

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
