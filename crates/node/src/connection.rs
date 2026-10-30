//! Outbound channel selection over native link streams.
//!
//! The profile's core lists the routes to each host and decides how it
//! reads from the links up to it; this manager opens the channels calls
//! ask for on the first route the core lists, and dials what the core
//! says to. A call that fails on a link closes the link and tries one
//! other listed route. It keeps nothing of its own about a host.

use std::sync::Arc;

use daemon_core::Input;
use tonic::transport::Channel;

use crate::HostId;
use crate::driver::CoreLinks;
use crate::link::{ChannelClass, ChannelError, ChannelKey, ChannelPool};
use crate::routing::{LinkCloseRequest, LiveRoute, Route, core_host, host_from_core};
use crate::transport::Replacing;

pub struct ConnectionManager {
    channels: Arc<ChannelPool>,
    replacing: Replacing,
    analytics: analytics::Analytics,
    /// What dials a host the core says to, once the edge dials at all.
    dialer: std::sync::OnceLock<crate::services::Dialer>,
    /// What links to the relay when the core says to, once the edge is up.
    relay: std::sync::OnceLock<RelayDialer>,
}

/// Makes one relay attempt the core asked for.
pub(crate) type RelayDialer = Arc<dyn Fn(daemon_core::AttemptId) + Send + Sync>;

impl ConnectionManager {
    pub(crate) fn new(channels: Arc<ChannelPool>, analytics: analytics::Analytics) -> Arc<Self> {
        Arc::new(Self {
            channels,
            replacing: Replacing::default(),
            analytics,
            dialer: std::sync::OnceLock::new(),
            relay: std::sync::OnceLock::new(),
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

    pub(crate) fn set_dialer(&self, dialer: crate::services::Dialer) {
        let _ = self.dialer.set(dialer);
    }

    /// The core says to dial `host` on every path of `plan`.
    pub(crate) fn dial(
        &self,
        host: daemon_core::HostId,
        attempt: daemon_core::AttemptId,
        plan: daemon_core::Plan,
        early: Vec<daemon_core::payload::PeerRequest>,
    ) {
        if let Some(dialer) = self.dialer.get() {
            dialer.dial(host_from_core(host), attempt, plan, early);
        }
    }

    pub(crate) fn set_relay_dialer(&self, dialer: RelayDialer) {
        let _ = self.relay.set(dialer);
    }

    /// The core says to link to the relay.
    pub(crate) fn dial_relay(&self, attempt: daemon_core::AttemptId) {
        match self.relay.get() {
            Some(dial) => dial(attempt),
            None => self.report(Input::RelayEnded {
                attempt,
                outcome: daemon_core::RelayOutcome::Lost("the edge is not up".to_owned()),
            }),
        }
    }

    /// The core gave up on the attempt to `host`.
    pub(crate) fn abandon(&self, host: daemon_core::HostId, attempt: daemon_core::AttemptId) {
        if let Some(dialer) = self.dialer.get() {
            dialer.abandon(host_from_core(host), attempt);
        }
    }

    /// The core asks the local network again where the hosts it found are.
    pub(crate) fn rediscover(&self) {
        if let Some(dialer) = self.dialer.get() {
            dialer.rediscover();
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
    /// whatever route reaches it, and the link each refused channel was on
    /// closes. Lifting it tells the core to try the host now.
    pub(crate) fn refuse_calls(&self, peer: HostId, refuse: bool) {
        self.channels.refuse(peer, refuse);
        if !refuse {
            self.report(Input::TryAgain {
                host: core_host(peer),
            });
        }
    }

    pub(crate) fn replacing(&self) -> Replacing {
        self.replacing.clone()
    }

    /// A channel for calls to `peer`.
    pub async fn channel_to(&self, peer: HostId) -> Result<Channel, ChannelError> {
        self.channel_to_class(peer, ChannelClass::Calls).await
    }

    /// A call to `peer` that began when the next link would be `since`
    /// found no channel within its patience: the links it had to use
    /// close, as their idle timeout would close them, only sooner, and
    /// redialling brings them back. A link that came up while it waited,
    /// as its own dial may have brought one, has not failed it and stays.
    pub(crate) fn calls_timed_out(&self, peer: HostId, since: u64) {
        tracing::warn!(%peer, "no call channel opened in time; closing the host's links");
        let links = self.channels.link_registry().clone();
        tokio::spawn(async move {
            links
                .close_host_before(peer, since, LinkCloseRequest::CallsFailed)
                .await;
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

    /// Opens a channel to `peer` on `route`. A channel failing on its link,
    /// the link gone under it or a stream that would not open on it (which
    /// closes the link), tries one other route the core lists, if any.
    async fn open_route(
        &self,
        peer: HostId,
        route: Route,
        class: ChannelClass,
    ) -> Result<Channel, ChannelError> {
        match self.activate_route(peer, route, class).await {
            Err(error @ (ChannelError::LinkUnavailable { .. } | ChannelError::Handshake(_))) => {
                let next = self
                    .routes_to(peer)
                    .await
                    .into_iter()
                    .find(|next| *next != route);
                match next {
                    Some(next) => self.activate_route(peer, next, class).await,
                    None => Err(error),
                }
            }
            result => result,
        }
    }

    /// A channel to `peer`'s pairing service through `forwarder`.
    pub(crate) async fn pairing_channel_through(
        &self,
        peer: HostId,
        forwarder: HostId,
    ) -> Result<Channel, ChannelError> {
        self.channels.pairing_channel(peer, forwarder).await
    }

    /// Why the last dial to `peer` failed, until a route to it comes up.
    pub(crate) async fn stored_reachability_error(&self, peer: HostId) -> Option<String> {
        let host = core_host(peer);
        self.look(move |core| core.last_dial_error(host).map(str::to_owned))
            .await
            .flatten()
    }

    /// Keeps the address a trusted peer answered at as the only direct
    /// address it is dialled at: the core writes it, and dials the host
    /// there if it has no link.
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

    /// `peer`'s key is being replaced: its links close, and no stream from
    /// it is admitted until the replacement ends.
    pub(crate) async fn teardown_host(&self, peer: HostId) {
        self.forget(peer);
        self.replacing.begin(peer);
        self.channels
            .link_registry()
            .close_host(peer, LinkCloseRequest::TrustReplaced)
            .await;
        self.forget(peer);
    }

    /// Ends live access to `peer` — its links and the streams on them,
    /// channels and our own routes — without forgetting the relay's word
    /// that it is online.
    /// Used when a peer is unpaired: the trust is gone and nothing of the old
    /// connection survives, but the peer stays reachable for pairing, the way
    /// any other machine on the account is before it is ever paired.
    ///
    /// The links close first, saying USER_REVOKED, while nothing else has
    /// cut them: that is how the peer learns it is no longer trusted.
    /// Its key has already left the trust store, so no link of it gets
    /// back in unless it is paired again.
    pub(crate) async fn close_host_access(&self, peer: HostId) {
        self.channels
            .link_registry()
            .close_host(peer, LinkCloseRequest::Revoked)
            .await;
        self.forget(peer);
    }

    pub(crate) fn finish_host_replacement(&self, peer: HostId) {
        self.replacing.finish(peer);
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
}
