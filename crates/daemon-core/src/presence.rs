//! What each host reads as, decided from the links that reach it.
//!
//! A host reads online while a link of its own is up, whether its packets
//! go on the network, over SSH or through a forwarder end to end; away
//! while no such link is up and the relay last refused to forward to it
//! because the account's plan carries no connection between hosts; and
//! offline otherwise. A call that fails on a link closes the link, which
//! is how a link that cannot carry calls stops reading online.
//!
//! A host with no link of its own that another host claims to reach is
//! dialled through that host. The shell keeps dialling it, backing off
//! between tries, until a link of its own is up or no claim lists it.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::Daemon;
use crate::ids::{HostId, LinkId, TimerId};
use crate::io::Output;
use crate::payload::ForwardRefusal;

/// How this host reaches another: over a link of its own, or through one
/// adjacent host that claims to reach it. There are no longer routes than
/// this: forwarding is not recursive.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Route {
    Direct(LinkId),
    Via(HostId),
}

impl Route {
    pub fn is_direct(&self) -> bool {
        matches!(self, Route::Direct(_))
    }
}

/// What a host reads as.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Presence {
    Online,
    Away,
    #[default]
    Offline,
}

/// What the core tells the shell about a host whenever it moves: every
/// route listed to it, the one carrying its traffic, and how it reads.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct HostView {
    pub routes: Vec<Route>,
    pub active: Option<Route>,
    pub presence: Presence,
}

impl HostView {
    /// The route calls take now: the one carrying traffic, or the one a
    /// fresh call would take.
    pub fn route(&self) -> Option<Route> {
        self.active.or_else(|| self.routes.first().copied())
    }
}

/// Every host the core has something to say about.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresenceState {
    hosts: BTreeMap<HostId, HostPresence>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct HostPresence {
    /// Whether the relay's refusal for the account's plan is the last thing
    /// a forwarder said of the host since its last link of its own.
    refused_for_plan: bool,
    /// The view last told, so a view is told when it moves and not merely
    /// when something happened.
    told: HostView,
}

impl PresenceState {
    /// What the core last told about `host`.
    pub fn view(&self, host: HostId) -> HostView {
        self.hosts
            .get(&host)
            .map(|known| known.told.clone())
            .unwrap_or_default()
    }
}

impl Daemon {
    /// What each host reads as, as the core last told it.
    pub fn presence(&self) -> &PresenceState {
        &self.presence
    }

    /// The routes to each host, before a step moves the links under them.
    pub(crate) fn routes_before(
        &self,
        hosts: impl IntoIterator<Item = HostId>,
    ) -> Vec<(HostId, Vec<Route>)> {
        hosts
            .into_iter()
            .map(|host| (host, self.routing.links.routes_to(host)))
            .collect()
    }

    /// Acts on each route that came up or went since `before`. A route
    /// gone takes its channels with it. A link of the host's own coming up
    /// says the relay's last refusal and the last dial error are out of
    /// date. A host left with no link of its own is dialled through a host
    /// claiming to reach it, when such a claim comes up or its own last
    /// link goes.
    pub(crate) fn routes_moved(
        &mut self,
        before: Vec<(HostId, Vec<Route>)>,
        out: &mut Vec<Output>,
    ) {
        for (host, was) in before {
            self.touched.host(host);
            let now = self.routing.links.routes_to(host);
            for gone in was.iter().filter(|route| !now.contains(route)) {
                out.push(Output::DropRoute { host, route: *gone });
            }
            if now.iter().any(|route| !was.contains(route)) {
                // A route coming up says the last dial error is out of date.
                self.dial_error(host, None);
            }
            let own = now.iter().any(Route::is_direct);
            if (own || now.is_empty())
                && let Some(known) = self.presence.hosts.get_mut(&host)
            {
                known.refused_for_plan = false;
            }
            let own_went = !own && was.iter().any(Route::is_direct);
            let claimed = now
                .iter()
                .any(|route| !route.is_direct() && !was.contains(route));
            if own_went || claimed {
                self.try_again(host, out);
            }
        }
    }

    /// Asks the shell to dial `host` through a host claiming to reach it,
    /// while no link of its own is up. The account's relay is never dialled
    /// through to itself. A host this profile does not trust, or one that
    /// said it no longer trusts this one, is not dialled: pairing again
    /// opens a link of its own.
    pub(crate) fn try_again(&mut self, host: HostId, out: &mut Vec<Output>) {
        let routes = self.routing.links.routes_to(host);
        if self.routing.links.is_relay(host)
            || routes.iter().any(Route::is_direct)
            || routes.is_empty()
            || self.dial_barred(host)
        {
            return;
        }
        out.push(Output::DialThrough { host });
    }

    /// The account's plan changed, so what the relay carries changed with
    /// it: every host it refused for the plan is dialled through it again.
    /// Each reads as it did until that dial brings up a link of its own.
    pub(crate) fn plan_changed(&mut self, out: &mut Vec<Output>) {
        let refused: Vec<HostId> = self
            .presence
            .hosts
            .iter()
            .filter(|(_, known)| known.refused_for_plan)
            .map(|(host, _)| *host)
            .collect();
        for host in refused {
            self.try_again(host, out);
        }
    }

    /// The peer at the other end of `link`, a relay or a daemon forwarding
    /// for its paired hosts, refused to forward to `host`. A refusal for
    /// the plan leaves a host with no link of its own reading away, and
    /// one because the forwarder holds no link to the host says the last
    /// refusal for the plan is out of date. A refusal for the rate or a
    /// shutdown says nothing of the host, and neither does one through a
    /// forwarder no longer claiming it: what it refused was sent before it
    /// said it no longer reaches the host.
    pub(crate) fn forward_refused(&mut self, link: LinkId, host: HostId, reason: ForwardRefusal) {
        let Some(forwarder) = self.routing.links.link(link).map(|link| link.peer) else {
            return;
        };
        if !self
            .routing
            .links
            .routes_to(host)
            .contains(&Route::Via(forwarder))
        {
            return;
        }
        let refused_for_plan = match reason {
            ForwardRefusal::PaymentRequired => true,
            ForwardRefusal::NoRoute => false,
            ForwardRefusal::RateLimited
            | ForwardRefusal::ShuttingDown
            | ForwardRefusal::Unspecified => return,
        };
        self.touched.host(host);
        self.presence
            .hosts
            .entry(host)
            .or_default()
            .refused_for_plan = refused_for_plan;
    }

    /// `host` is forgotten: its trust ended or is being replaced. Nothing
    /// the core decided about it survives.
    pub(crate) fn forget_host(&mut self, host: HostId) {
        self.touched.host(host);
        if let Some(known) = self.presence.hosts.get_mut(&host) {
            known.refused_for_plan = false;
        }
    }

    /// Settles the view of each host the step touched. The route carrying
    /// a host's traffic is the first link of its own listed; a relay reads
    /// as no host.
    pub(crate) fn tell_presence(&mut self) {
        for host in self.touched.hosts.clone() {
            let routes = self.routing.links.routes_to(host);
            let relay = self.routing.links.is_relay(host);
            let known = self.presence.hosts.entry(host).or_default();
            let active = routes
                .iter()
                .copied()
                .find(Route::is_direct)
                .filter(|_| !relay);
            let presence = match active {
                Some(_) => Presence::Online,
                None if known.refused_for_plan && !routes.is_empty() => Presence::Away,
                None => Presence::Offline,
            };
            let view = HostView {
                routes,
                active,
                presence,
            };
            if view != known.told {
                self.touched.host_set = true;
                known.told = view;
            }
            if *known == HostPresence::default() {
                self.presence.hosts.remove(&host);
            }
        }
    }

    /// Arms `id` to fire at `at_ms`, moving it if it is armed.
    pub(crate) fn arm(&mut self, id: TimerId, at_ms: u64, out: &mut Vec<Output>) {
        self.timers.armed.insert(id);
        out.push(Output::ArmTimer { id, at_ms });
    }

    pub(crate) fn cancel(&mut self, id: TimerId, out: &mut Vec<Output>) {
        if self.timers.armed.remove(&id) {
            out.push(Output::CancelTimer { id });
        }
    }
}

#[cfg(test)]
mod tests {
    use store::Sqlite;
    use wire::message::Body;
    use wire::{Hello, Message, NeighborDown, NeighborUp};

    use super::*;
    use crate::payload::CarrierKind;
    use crate::{Input, StartOptions};

    const OWN: [u8; 16] = *b"own-host-0000000";
    const DESK: HostId = HostId([1; 16]);
    const LAPTOP: HostId = HostId([2; 16]);
    const RELAY: HostId = HostId([9; 16]);

    fn daemon() -> Daemon {
        Daemon::new(
            Sqlite::open_in_memory(OWN).expect("a store opens"),
            StartOptions::default(),
        )
    }

    fn step(daemon: &mut Daemon, input: Input) -> Vec<Output> {
        let mut out = Vec::new();
        daemon.step(input, &mut out);
        assert!(
            !out.iter()
                .any(|output| matches!(output, Output::Dump { .. })),
            "no step breaks an invariant: {out:?}"
        );
        out
    }

    fn up(daemon: &mut Daemon, link: u64, peer: HostId, kind: CarrierKind) -> Vec<Output> {
        step(
            daemon,
            Input::LinkUp {
                link: LinkId(link),
                peer,
                kind,
                resumed: None,
            },
        )
    }

    fn down(daemon: &mut Daemon, link: u64) -> Vec<Output> {
        step(
            daemon,
            Input::LinkDown {
                link: LinkId(link),
                reason: wire::LinkCloseReason::UserShutdown,
            },
        )
    }

    fn host(id: HostId) -> wire::Host {
        wire::Host {
            host_id: id.0.to_vec(),
            ..Default::default()
        }
    }

    fn frame(daemon: &mut Daemon, link: u64, body: Body) -> Vec<Output> {
        step(
            daemon,
            Input::LinkControl {
                link: LinkId(link),
                frame: Message { body: Some(body) },
            },
        )
    }

    fn claims(daemon: &mut Daemon, link: u64, hosts: &[HostId]) -> Vec<Output> {
        let hello = Hello {
            neighbors: hosts.iter().copied().map(host).collect(),
            ..Default::default()
        };
        frame(daemon, link, Body::Hello(hello))
    }

    fn view(daemon: &Daemon, host: HostId) -> HostView {
        daemon.presence().view(host)
    }

    /// The hosts a step asked to be dialled through a forwarder.
    fn dialled_through(out: &[Output]) -> Vec<HostId> {
        out.iter()
            .filter_map(|output| match output {
                Output::DialThrough { host } => Some(*host),
                _ => None,
            })
            .collect()
    }

    fn trust(daemon: &mut Daemon, hosts: Vec<HostId>) -> Vec<Output> {
        {
            daemon.trust_for_test(crate::trust::TrustStore::of_hosts(hosts));
            step(daemon, Input::Started)
        }
    }

    fn forward_refused(
        daemon: &mut Daemon,
        link: u64,
        dst: HostId,
        reason: ForwardRefusal,
    ) -> Vec<Output> {
        frame(
            daemon,
            link,
            Body::ForwardRefused(wire::ForwardRefused {
                dst: dst.0.to_vec(),
                reason: reason as i32,
            }),
        )
    }

    /// A host the relay claims, which it refused to carry for the plan.
    fn refused_desk() -> Daemon {
        let mut daemon = daemon();
        up(&mut daemon, 1, RELAY, CarrierKind::Relay);
        claims(&mut daemon, 1, &[DESK]);
        forward_refused(&mut daemon, 1, DESK, ForwardRefusal::PaymentRequired);
        assert_eq!(view(&daemon, DESK).presence, Presence::Away);
        daemon
    }

    #[test]
    fn a_hosts_own_links_come_first_newest_first_then_each_host_claiming_to_reach_it() {
        let mut daemon = daemon();
        // The relay's link is the older, and still its claim comes last.
        up(&mut daemon, 1, RELAY, CarrierKind::Relay);
        claims(&mut daemon, 1, &[DESK, LAPTOP]);
        up(&mut daemon, 2, LAPTOP, CarrierKind::Direct);
        claims(&mut daemon, 2, &[DESK]);
        assert_eq!(
            view(&daemon, DESK).routes,
            vec![Route::Via(LAPTOP), Route::Via(RELAY)],
            "the relay is the route of last resort"
        );

        up(&mut daemon, 3, DESK, CarrierKind::Direct);
        up(&mut daemon, 4, DESK, CarrierKind::Direct);
        assert_eq!(
            view(&daemon, DESK).routes,
            vec![
                Route::Direct(LinkId(4)),
                Route::Direct(LinkId(3)),
                Route::Via(LAPTOP),
                Route::Via(RELAY),
            ],
            "a second link to a peer is the route to it, ahead of the first"
        );

        // One link of two going leaves the other; the relay going takes
        // every claim it made.
        down(&mut daemon, 4);
        down(&mut daemon, 1);
        assert_eq!(
            view(&daemon, DESK).routes,
            vec![Route::Direct(LinkId(3)), Route::Via(LAPTOP)]
        );
        assert_eq!(view(&daemon, LAPTOP).routes, vec![Route::Direct(LinkId(2))]);

        // A neighbor saying it lost the host withdraws its claim alone.
        frame(
            &mut daemon,
            2,
            Body::NeighborDown(NeighborDown {
                host_id: DESK.0.to_vec(),
            }),
        );
        assert_eq!(view(&daemon, DESK).routes, vec![Route::Direct(LinkId(3))]);
        down(&mut daemon, 3);
        assert_eq!(view(&daemon, DESK), HostView::default());
    }

    #[test]
    fn a_host_reads_online_over_its_own_link_the_newest_on_a_path_of_its_own_first() {
        let mut daemon = daemon();
        let (forwarded, own) = (Route::Direct(LinkId(1)), Route::Direct(LinkId(2)));
        up(&mut daemon, 1, DESK, CarrierKind::Forwarded);
        let reads = view(&daemon, DESK);
        assert_eq!(
            (reads.presence, reads.active),
            (Presence::Online, Some(forwarded))
        );

        // The newer link, on the same network, takes the host's traffic.
        up(&mut daemon, 2, DESK, CarrierKind::Direct);
        assert_eq!(view(&daemon, DESK).active, Some(own));

        // A forwarded link coming up later is listed after it.
        let later = Route::Direct(LinkId(3));
        up(&mut daemon, 3, DESK, CarrierKind::Forwarded);
        assert_eq!(view(&daemon, DESK).routes, vec![own, later, forwarded]);
        assert_eq!(view(&daemon, DESK).active, Some(own));
    }

    #[test]
    fn a_relay_is_never_a_route_nor_dialled_through() {
        let mut daemon = daemon();
        let out = up(&mut daemon, 1, RELAY, CarrierKind::Relay);
        assert_eq!(dialled_through(&out), Vec::new());
        assert_eq!(view(&daemon, RELAY).presence, Presence::Offline);

        let relay = wire::Host {
            capabilities: Some(wire::Capabilities {
                cloud_relay: true,
                ..Default::default()
            }),
            ..host(HostId([8; 16]))
        };
        up(&mut daemon, 2, LAPTOP, CarrierKind::Direct);
        frame(
            &mut daemon,
            2,
            Body::NeighborUp(NeighborUp { host: Some(relay) }),
        );
        assert_eq!(view(&daemon, HostId([8; 16])).routes, Vec::new());
    }

    #[test]
    fn a_host_with_no_link_of_its_own_is_dialled_through_each_host_claiming_it() {
        let mut daemon = daemon();
        up(&mut daemon, 1, RELAY, CarrierKind::Relay);
        let out = claims(&mut daemon, 1, &[DESK]);
        assert_eq!(dialled_through(&out), vec![DESK]);
        assert_eq!(view(&daemon, DESK).presence, Presence::Offline);

        // Its own link coming up is what reads it online; a claim coming up
        // then asks for no dial.
        up(&mut daemon, 2, DESK, CarrierKind::Forwarded);
        assert_eq!(view(&daemon, DESK).presence, Presence::Online);
        up(&mut daemon, 3, LAPTOP, CarrierKind::Direct);
        assert_eq!(
            dialled_through(&claims(&mut daemon, 3, &[DESK])),
            Vec::new()
        );

        // Its own last link going, while claims still list it, dials it
        // through them again; the last route going reads it offline.
        let out = down(&mut daemon, 2);
        assert!(out.contains(&Output::DropRoute {
            host: DESK,
            route: Route::Direct(LinkId(2))
        }));
        assert_eq!(dialled_through(&out), vec![DESK]);
        assert_eq!(view(&daemon, DESK).presence, Presence::Offline);
        down(&mut daemon, 3);
        assert_eq!(dialled_through(&down(&mut daemon, 1)), Vec::new());
        assert_eq!(view(&daemon, DESK), HostView::default());
    }

    #[test]
    fn a_host_not_trusted_is_not_dialled_through_until_it_is() {
        let mut daemon = daemon();
        trust(&mut daemon, vec![LAPTOP]);
        up(&mut daemon, 1, LAPTOP, CarrierKind::Direct);
        assert_eq!(
            dialled_through(&claims(&mut daemon, 1, &[DESK])),
            Vec::new()
        );

        let trusted = trust(&mut daemon, vec![DESK, LAPTOP]);
        assert_eq!(dialled_through(&trusted), vec![DESK]);
    }

    #[test]
    fn a_refusal_for_the_plan_reads_the_host_away_until_a_link_of_its_own_is_up() {
        let mut daemon = refused_desk();
        up(&mut daemon, 2, DESK, CarrierKind::Direct);
        assert_eq!(view(&daemon, DESK).presence, Presence::Online);

        // The refusal went with the link it predates.
        down(&mut daemon, 2);
        assert_eq!(view(&daemon, DESK).presence, Presence::Offline);
    }

    #[test]
    fn a_plan_change_dials_every_host_the_relay_refused_for_the_plan() {
        let mut daemon = refused_desk();
        claims(&mut daemon, 1, &[DESK, LAPTOP]);
        let out = step(&mut daemon, Input::PlanChanged);
        assert_eq!(dialled_through(&out), vec![DESK]);
        assert_eq!(view(&daemon, DESK).presence, Presence::Away);
    }

    #[test]
    fn a_forwarder_holding_no_link_to_the_host_reads_it_offline_at_once() {
        let mut daemon = refused_desk();
        forward_refused(&mut daemon, 1, DESK, ForwardRefusal::NoRoute);
        assert_eq!(view(&daemon, DESK).presence, Presence::Offline);
    }

    #[test]
    fn a_forward_refused_after_the_forwarder_let_the_host_go_says_nothing() {
        let mut daemon = daemon();
        up(&mut daemon, 1, RELAY, CarrierKind::Relay);
        claims(&mut daemon, 1, &[DESK]);
        frame(
            &mut daemon,
            1,
            Body::NeighborDown(NeighborDown {
                host_id: DESK.0.to_vec(),
            }),
        );
        let out = forward_refused(&mut daemon, 1, DESK, ForwardRefusal::PaymentRequired);
        assert_eq!(out, Vec::new());
        assert_eq!(view(&daemon, DESK), HostView::default());
    }

    #[test]
    fn a_forward_refused_for_the_rate_or_a_shutdown_says_nothing_of_the_host() {
        let mut daemon = refused_desk();
        for reason in [ForwardRefusal::RateLimited, ForwardRefusal::ShuttingDown] {
            assert_eq!(forward_refused(&mut daemon, 1, DESK, reason), Vec::new());
            assert_eq!(view(&daemon, DESK).presence, Presence::Away);
        }
    }

    #[test]
    fn a_forgotten_host_forgets_the_relays_refusal() {
        let mut daemon = refused_desk();
        step(&mut daemon, Input::HostForgotten { host: DESK });
        assert_eq!(view(&daemon, DESK).presence, Presence::Offline);
    }

    #[test]
    fn a_route_coming_up_clears_the_last_dial_error() {
        let mut daemon = daemon();
        step(
            &mut daemon,
            Input::DialError {
                host: DESK,
                error: Some("connection refused".to_owned()),
            },
        );
        assert_eq!(daemon.last_dial_error(DESK), Some("connection refused"));
        up(&mut daemon, 1, DESK, CarrierKind::Direct);
        assert_eq!(daemon.last_dial_error(DESK), None);
    }

    #[test]
    fn a_host_reads_with_the_carrier_of_the_link_its_calls_take() {
        let mut daemon = daemon();
        up(&mut daemon, 1, DESK, CarrierKind::Ssh);
        assert_eq!(daemon.route_kind(DESK), wire::RouteKind::Ssh);
        assert_eq!(
            daemon.reads(DESK),
            wire::trusted::Reach::Online(wire::RouteKind::Ssh as i32)
        );
        down(&mut daemon, 1);
        assert_eq!(daemon.route_kind(DESK), wire::RouteKind::Unspecified);
    }

    #[test]
    fn a_host_reads_by_the_path_now_carrying_its_link() {
        let mut daemon = daemon();
        up(&mut daemon, 1, DESK, CarrierKind::Direct);
        let carried = |daemon: &mut Daemon, kind| {
            step(
                daemon,
                Input::LinkCarried {
                    link: LinkId(1),
                    kind,
                },
            )
        };
        carried(&mut daemon, CarrierKind::Forwarded);
        assert_eq!(
            daemon.reads(DESK),
            wire::trusted::Reach::Online(wire::RouteKind::Relay as i32)
        );
        carried(&mut daemon, CarrierKind::Direct);
        assert_eq!(
            daemon.reads(DESK),
            wire::trusted::Reach::Online(wire::RouteKind::Direct as i32)
        );
    }
}
