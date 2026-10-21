//! Outbound channel selection over native link streams, and what a host's
//! presence is, as the profile's core decides it from how opening channels
//! to the host went.
//!
//! The core lists the routes to each host, picks the one carrying its
//! traffic, decides how it reads, which route a failed call tries next and
//! what a call finding no channel does, and retries each host in the
//! background; this manager reads what the core decided, opens the
//! channels calls ask for on the route it names, and tries what the core
//! asks it to try. It keeps nothing of its own about a host.

use std::sync::{Arc, Weak};
use std::time::Duration;

use daemon_core::{Input, Tried};
use tonic::transport::Channel;
use wire::Presence;

use crate::HostId;
use crate::driver::{CoreLinks, core_error};
use crate::link::{ChannelClass, ChannelError, ChannelKey, ChannelPool};
use crate::routing::{LinkCloseRequest, LiveRoute, Route, core_host, host_from_core};
use crate::transport::TrustedPeerConnections;

/// How long one try the core asks for may take to open its channel before
/// it counts as failed: as long as a call waits to reach a host.
const ATTEMPT_PATIENCE: Duration = Duration::from_secs(10);

pub struct ConnectionManager {
    /// This manager, for the tries it runs in the background.
    me: Weak<ConnectionManager>,
    channels: Arc<ChannelPool>,
    trusted_connections: TrustedPeerConnections,
    analytics: analytics::Analytics,
    /// What dials a host the core says to, once the edge dials at all.
    dialer: std::sync::OnceLock<crate::services::Dialer>,
}

impl ConnectionManager {
    pub(crate) fn new(channels: Arc<ChannelPool>, analytics: analytics::Analytics) -> Arc<Self> {
        Arc::new_cyclic(|me| Self {
            me: me.clone(),
            channels,
            trusted_connections: TrustedPeerConnections::default(),
            analytics,
            dialer: std::sync::OnceLock::new(),
        })
    }

    /// The profile's core, when the links are a profile's.
    fn core(&self) -> Option<CoreLinks> {
        self.channels.link_registry().core().cloned()
    }

    /// Tells the profile's core what happened, when the links are a
    /// profile's.
    fn report(&self, input: Input) {
        if let Some(core) = self.core() {
            core.report(input);
        }
    }

    /// Looks at what the profile's core decided; with no profile, or once
    /// it is gone, nothing is decided.
    async fn look<T: Send + 'static>(
        &self,
        look: impl FnOnce(&daemon_core::Daemon) -> T + Send + 'static,
    ) -> Option<T> {
        self.core()?.look(look).await
    }

    /// The core asks for a calls channel to `host` on `route`, and to be
    /// told how it went.
    pub(crate) fn attempt(&self, host: daemon_core::HostId, route: daemon_core::Route) {
        let me = self.me.clone();
        tokio::spawn(async move {
            let Some(manager) = me.upgrade() else {
                return;
            };
            let peer = host_from_core(host);
            let opened = tokio::time::timeout(
                ATTEMPT_PATIENCE,
                manager.open_route(peer, Route::from_core(peer, route), ChannelClass::Calls),
            )
            .await;
            let tried = match opened {
                Ok(Ok(_)) => Tried::Opened,
                Ok(Err(error)) => Tried::Failed(core_error(&error)),
                Err(_) => Tried::Failed(daemon_core::payload::ChannelError::Handshake(
                    "no channel opened in time".to_owned(),
                )),
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

    /// A call to `peer` found no channel within its patience, or the one
    /// it opened failed: the core decides what that says of the host.
    pub(crate) fn calls_failed(&self, peer: HostId, error: daemon_core::payload::ChannelError) {
        tracing::debug!(%peer, ?error, "a call to the host found no channel");
        self.report(Input::CallsFailed {
            host: core_host(peer),
            error,
        });
    }

    /// The core says calls over `host`'s own links find no channel: they
    /// close, as their idle timeout would close them, only sooner.
    pub(crate) fn close_links(&self, host: daemon_core::HostId) {
        let peer = host_from_core(host);
        tracing::warn!(%peer, "no call channel opened on the host's own link; closing it");
        let links = self.channels.link_registry().clone();
        tokio::spawn(async move {
            links.close_host(peer, LinkCloseRequest::CallsFailed).await;
        });
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
            .await
            .first()
            .copied()
            .ok_or(ChannelError::NoRoute { host_id: peer })?;
        let opened = self.open_route(peer, route, class).await;
        if let Err(ChannelError::Refused(wire::pb::ForwardRefusal::PaymentRequired)) = &opened {
            self.analytics.record_at_most_every(
                crate::telemetry::RELAY_REFUSED_EVERY,
                analytics::Event::RelayRefused,
            );
        }
        opened
    }

    /// Opens a channel to `peer` on `route`. A link can close under a call
    /// that just chose it while the core still lists it: the core is told,
    /// and the call tries the route it names next, if any.
    async fn open_route(
        &self,
        peer: HostId,
        route: Route,
        class: ChannelClass,
    ) -> Result<Channel, ChannelError> {
        match self.activate_route(peer, route, class).await {
            Err(error @ ChannelError::LinkUnavailable { .. }) => {
                let next = match self.core() {
                    Some(core) => {
                        core.route_failed(peer, route.core(), core_error(&error))
                            .await
                    }
                    None => None,
                };
                match next {
                    Some(next) => {
                        self.activate_route(peer, Route::from_core(peer, next), class)
                            .await
                    }
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
        self.channels.pairing_channel(peer, relay).await
    }

    pub(crate) async fn has_cloud_route(&self, peer: HostId) -> bool {
        self.cloud_relay_for(peer).await.is_ok()
    }

    /// Why the last dial to `peer` failed, until a route to it comes up or
    /// a channel to it opens.
    pub(crate) async fn stored_reachability_error(&self, peer: HostId) -> Option<String> {
        let host = core_host(peer);
        self.look(move |core| core.last_dial_error(host).map(str::to_owned))
            .await
            .flatten()
    }

    pub(crate) async fn record_reachability_error(&self, peer: HostId, error: impl Into<String>) {
        self.dial_error(peer, Some(error.into()));
    }

    pub(crate) async fn clear_reachability_error(&self, peer: HostId) {
        self.dial_error(peer, None);
    }

    /// Tells the core why the last dial to `peer` failed, or that it
    /// succeeded: the host's entry carries it.
    fn dial_error(&self, peer: HostId, error: Option<String>) {
        self.report(Input::DialError {
            host: core_host(peer),
            error,
        });
    }

    /// Keeps the address a trusted peer answered at as the only direct
    /// address it is dialled at: the core writes it.
    pub(crate) fn remember_direct_addr(&self, peer: HostId, addr: std::net::SocketAddr) {
        self.report(Input::ChangeTrust(crate::trust::TrustChange::DirectAddrs {
            host: peer,
            addrs: vec![addr],
        }));
    }

    /// Every machine discovery holds in this profile's scope now, newest
    /// first: the host set lists those not paired as candidates.
    pub(crate) fn candidates(&self, found: Vec<crate::discovery::Advertisement>) {
        self.report(Input::Candidates {
            found: found
                .into_iter()
                .map(|advert| daemon_core::host_set::Candidate {
                    host: core_host(advert.host_id),
                    name: advert.name,
                    addrs: advert.addrs.iter().map(ToString::to_string).collect(),
                })
                .collect(),
        });
    }

    /// Every route the core lists to `peer`, the one a fresh call takes
    /// first.
    pub(crate) async fn routes_to(&self, peer: HostId) -> Vec<Route> {
        let host = core_host(peer);
        self.look(move |core| core.links().routes_to(host))
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|route| Route::from_core(peer, route))
            .collect()
    }

    /// The kind of route calls to `peer` take now, however it reads.
    pub(crate) async fn via_for(&self, peer: HostId) -> LiveRoute {
        let host = core_host(peer);
        self.look(move |core| core.route_kind(host))
            .await
            .map_or(LiveRoute::Offline, LiveRoute::from_wire)
    }

    /// How `peer` reads, and the route that carries its calls, as every
    /// client is told: online once a channel to it has opened on a route
    /// still listed, away while the relay refuses channels to it for the
    /// account's plan, offline otherwise, with no route then.
    pub(crate) async fn presence_for(&self, peer: HostId) -> (Presence, LiveRoute) {
        let host = core_host(peer);
        match self.look(move |core| core.reads(host)).await {
            Some((presence, via)) => (presence, LiveRoute::from_wire(via)),
            None => (Presence::Offline, LiveRoute::Offline),
        }
    }

    pub(crate) async fn teardown_host(&self, peer: HostId) {
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
        self.channels
            .link_registry()
            .close_host(peer, LinkCloseRequest::Revoked)
            .await;
        self.forget(peer);
        self.trusted_connections.close_host(peer).await;
        self.forget(peer);
        self.trusted_connections.finish_host_replacement(peer);
    }

    pub(crate) fn finish_host_replacement(&self, peer: HostId) {
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
        if !self.routes_to(peer).await.contains(&route) {
            self.channels.drop_host(peer);
            return Err(ChannelError::NoRoute { host_id: peer });
        }
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
        let relays = self
            .routes_to(peer)
            .await
            .into_iter()
            .filter_map(|route| match route {
                Route::Via(relay) => Some(relay),
                Route::Direct(_) => None,
            });
        for relay in relays {
            if registry.has_cloud_relay_link_to(relay).await {
                return Ok(relay);
            }
        }
        Err(ChannelError::CloudPairingUnavailable)
    }
}
