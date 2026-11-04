//! What each host reads as, decided from the links that reach it.
//!
//! A host reads online while a link of its own is up, whether its packets
//! go on the network, over SSH or through a forwarder end to end; away
//! while no such link is up and the relay last refused to forward to it
//! because the account's plan carries no connection between hosts; and
//! offline otherwise. A call that fails on a link closes the link, which
//! is how a link that cannot carry calls stops reading online.
//!
//! Away says paying is the only way left to the host, so it waits until
//! that is known. The relay refuses within a round trip, long before a
//! dial on the local network ends, so while an attempt is out the
//! refusal says nothing yet: the host reads offline until that attempt
//! ends without a link, or online when one comes up. Once told, away
//! holds through every attempt after it, so it does not blink at each
//! retry, until the refusal stops being the relay's last word: a link of
//! the host's own comes up, the relay says it holds no link to the host,
//! or the relay no longer claims it.
//!
//! When a host is dialled, and on which paths, is the reach module's.

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
    /// The forwarder, the relay, whose refusal for the account's plan is
    /// the last thing it said of the host, while it still claims the host
    /// and no link of the host's own has come up since.
    refused_by: Option<HostId>,
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
    /// date, and the relay no longer claiming the host says its refusal
    /// is. What the move means for dialling the host is the reach module's.
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
            if let Some(known) = self.presence.hosts.get_mut(&host)
                && known
                    .refused_by
                    .is_some_and(|forwarder| own || !now.contains(&Route::Via(forwarder)))
            {
                known.refused_by = None;
            }
            self.reach_routes_moved(host, &was, &now, out);
        }
    }

    /// The account's plan changed, so what the relay carries changed with
    /// it: every host it refused for the plan is dialled again at once.
    /// Each reads as it did until that dial brings up a link of its own.
    pub(crate) fn plan_changed(&mut self, out: &mut Vec<Output>) {
        let refused: Vec<HostId> = self
            .presence
            .hosts
            .iter()
            .filter(|(_, known)| known.refused_by.is_some())
            .map(|(host, _)| *host)
            .collect();
        for host in refused {
            self.reconsider(host, out);
        }
    }

    /// The peer at the other end of `link`, a relay or a daemon forwarding
    /// for its paired hosts, refused to forward to `host`. A refusal for
    /// the plan leaves a host with no link of its own reading away once no
    /// attempt to it is out, and one because the same forwarder holds no
    /// link to the host says its last refusal for the plan is out of date;
    /// another forwarder's says nothing of what the relay carries, so a
    /// paired host refusing a forward does not end away. A refusal for the
    /// rate or a shutdown says nothing of the host, and neither does one
    /// through a forwarder no longer claiming it: what it refused was sent
    /// before it said it no longer reaches the host.
    pub(crate) fn forward_refused(
        &mut self,
        link: LinkId,
        host: HostId,
        reason: ForwardRefusal,
        out: &mut Vec<Output>,
    ) {
        self.reach_refused(link, host, reason, out);
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
        let refused_by = match reason {
            ForwardRefusal::PaymentRequired => Some(forwarder),
            ForwardRefusal::NoRoute => None,
            ForwardRefusal::RateLimited | ForwardRefusal::Unspecified => return,
        };
        self.touched.host(host);
        let known = self.presence.hosts.entry(host).or_default();
        if refused_by.is_some() || known.refused_by == Some(forwarder) {
            known.refused_by = refused_by;
        }
    }

    /// `host` is forgotten: its trust ended. Nothing the core decided
    /// about it survives.
    pub(crate) fn forget_host(&mut self, host: HostId) {
        self.touched.host(host);
        if let Some(known) = self.presence.hosts.get_mut(&host) {
            known.refused_by = None;
        }
    }

    /// Settles the view of each host the step touched. The route carrying
    /// a host's traffic is the first link of its own listed; a relay reads
    /// as no host. A host the relay refused for the plan reads away once
    /// no attempt to it is out, and goes on reading away through the
    /// attempts after; each such host is settled at every step, since an
    /// attempt ending touches nothing else of it.
    pub(crate) fn tell_presence(&mut self) {
        let refused = self
            .presence
            .hosts
            .iter()
            .filter(|(_, known)| known.refused_by.is_some())
            .map(|(host, _)| *host);
        let mut hosts = self.touched.hosts.clone();
        for host in refused {
            if !hosts.contains(&host) {
                hosts.push(host);
            }
        }
        for host in hosts {
            let routes = self.routing.links.routes_to(host);
            let relay = self.routing.links.is_relay(host);
            let trying = self.routing.reach.attempt(host).is_some();
            let known = self.presence.hosts.entry(host).or_default();
            let active = routes
                .iter()
                .copied()
                .find(Route::is_direct)
                .filter(|_| !relay);
            let refused = known
                .refused_by
                .is_some_and(|forwarder| routes.contains(&Route::Via(forwarder)));
            let presence = match active {
                Some(_) => Presence::Online,
                None if refused && (!trying || known.told.presence == Presence::Away) => {
                    Presence::Away
                }
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

    /// The hosts a step asked to be dialled.
    fn dialled(out: &[Output]) -> Vec<HostId> {
        out.iter()
            .filter_map(|output| match output {
                Output::Dial { host, .. } => Some(*host),
                _ => None,
            })
            .collect()
    }

    /// A daemon trusting the desk and the laptop, whose driver dials.
    fn dialling() -> Daemon {
        let mut daemon = daemon();
        trust(&mut daemon, vec![DESK, LAPTOP]);
        step(&mut daemon, Input::Dialling(crate::reach::Dialling::All));
        daemon
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
        let mut daemon = dialling();
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
        let mut daemon = dialling();
        let out = up(&mut daemon, 1, RELAY, CarrierKind::Relay);
        assert_eq!(dialled(&out), Vec::new());
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
    fn a_host_not_trusted_is_not_dialled_through_until_it_is() {
        let mut daemon = daemon();
        trust(&mut daemon, vec![LAPTOP]);
        step(&mut daemon, Input::Dialling(crate::reach::Dialling::All));
        up(&mut daemon, 1, LAPTOP, CarrierKind::Direct);
        assert_eq!(dialled(&claims(&mut daemon, 1, &[DESK])), Vec::new());

        let trusted = trust(&mut daemon, vec![DESK, LAPTOP]);
        assert_eq!(dialled(&trusted), vec![DESK]);
    }

    #[test]
    fn a_plan_change_dials_every_host_the_relay_refused_for_the_plan() {
        let mut daemon = refused_desk();
        claims(&mut daemon, 1, &[DESK, LAPTOP]);
        let out = step(&mut daemon, Input::PlanChanged);
        assert_eq!(dialled(&out), vec![DESK]);
        assert_eq!(view(&daemon, DESK).presence, Presence::Away);
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

    const DESK_ADDR: &str = "192.168.1.20:7000";

    /// A daemon trusting the desk, remembered at its address on the local
    /// network and as on the account, with the relay claiming it and an
    /// attempt out on both: the phone at launch.
    fn launched() -> (Daemon, crate::ids::AttemptId) {
        let mut daemon = daemon();
        let mut trust = crate::trust::TrustStore::of_hosts([DESK]);
        let uuid = uuid::Uuid::from_bytes(DESK.0);
        let mut entry = trust.entry(uuid).cloned().expect("the desk is trusted");
        entry.direct = vec![DESK_ADDR.parse().unwrap()];
        entry.on_account = true;
        trust.insert_for_test(uuid, entry);
        daemon.trust_for_test(trust);
        step(&mut daemon, Input::Started);
        up(&mut daemon, 1, RELAY, CarrierKind::Relay);
        step(&mut daemon, Input::Dialling(crate::reach::Dialling::All));
        claims(&mut daemon, 1, &[DESK]);
        let attempt = daemon.reach().attempt(DESK).expect("the desk is dialled");
        let plan = daemon.dial_plan(DESK);
        assert!(plan.through_relay && !plan.paths.is_empty());
        (daemon, attempt)
    }

    fn ended(daemon: &mut Daemon, attempt: crate::ids::AttemptId, outcome: crate::AttemptOutcome) {
        step(
            daemon,
            Input::AttemptEnded {
                host: DESK,
                attempt,
                outcome,
            },
        );
    }

    #[test]
    fn a_host_at_home_never_reads_away_while_its_own_address_is_still_being_dialled() {
        let (mut daemon, attempt) = launched();
        // The relay answers within a round trip; the local network's dial
        // is still out.
        forward_refused(&mut daemon, 1, DESK, ForwardRefusal::PaymentRequired);
        assert_eq!(view(&daemon, DESK).presence, Presence::Offline);
        assert_eq!(daemon.reach().attempt(DESK), Some(attempt));

        up(&mut daemon, 2, DESK, CarrierKind::Direct);
        ended(
            &mut daemon,
            attempt,
            crate::AttemptOutcome::Linked {
                addr: Some(DESK_ADDR.parse().unwrap()),
            },
        );
        assert_eq!(view(&daemon, DESK).presence, Presence::Online);
    }

    #[test]
    fn a_refused_host_reads_away_once_its_own_paths_fail_and_holds_through_every_retry() {
        let (mut daemon, attempt) = launched();
        forward_refused(&mut daemon, 1, DESK, ForwardRefusal::PaymentRequired);
        ended(
            &mut daemon,
            attempt,
            crate::AttemptOutcome::Failed("no answer".to_owned()),
        );
        assert_eq!(view(&daemon, DESK).presence, Presence::Away);

        for _ in 0..3 {
            step(
                &mut daemon,
                Input::Timer {
                    id: TimerId::Retry(DESK),
                },
            );
            let retry = daemon
                .reach()
                .attempt(DESK)
                .expect("the desk is dialled again");
            assert_eq!(view(&daemon, DESK).presence, Presence::Away);
            forward_refused(&mut daemon, 1, DESK, ForwardRefusal::PaymentRequired);
            assert_eq!(view(&daemon, DESK).presence, Presence::Away);
            step(
                &mut daemon,
                Input::Timer {
                    id: TimerId::Attempt(DESK),
                },
            );
            assert_eq!(daemon.reach().attempt(DESK), None, "{retry:?} timed out");
            assert_eq!(view(&daemon, DESK).presence, Presence::Away);
        }

        // Home again: the host's own link ends it.
        up(&mut daemon, 2, DESK, CarrierKind::Direct);
        assert_eq!(view(&daemon, DESK).presence, Presence::Online);
    }

    #[test]
    fn away_ends_when_the_relay_no_longer_claims_the_host_whoever_else_does() {
        let mut daemon = refused_desk();
        up(&mut daemon, 2, LAPTOP, CarrierKind::Direct);
        claims(&mut daemon, 2, &[DESK]);
        assert_eq!(view(&daemon, DESK).presence, Presence::Away);
        frame(
            &mut daemon,
            1,
            Body::NeighborDown(NeighborDown {
                host_id: DESK.0.to_vec(),
            }),
        );
        assert_eq!(view(&daemon, DESK).routes, vec![Route::Via(LAPTOP)]);
        assert_eq!(view(&daemon, DESK).presence, Presence::Offline);
        // The relay claiming it again says nothing of the plan until it
        // refuses again.
        claims(&mut daemon, 1, &[DESK]);
        assert_eq!(view(&daemon, DESK).presence, Presence::Offline);
    }

    #[test]
    fn a_paired_host_with_no_route_to_a_refused_host_leaves_it_away() {
        let mut daemon = refused_desk();
        up(&mut daemon, 2, LAPTOP, CarrierKind::Direct);
        claims(&mut daemon, 2, &[DESK]);
        forward_refused(&mut daemon, 2, DESK, ForwardRefusal::NoRoute);
        assert_eq!(view(&daemon, DESK).presence, Presence::Away);
        // The relay saying it holds no link to the host does end it.
        forward_refused(&mut daemon, 1, DESK, ForwardRefusal::NoRoute);
        assert_eq!(view(&daemon, DESK).presence, Presence::Offline);
    }

    #[test]
    fn a_forward_refused_for_the_rate_says_nothing_of_the_host() {
        let mut daemon = refused_desk();
        assert_eq!(
            forward_refused(&mut daemon, 1, DESK, ForwardRefusal::RateLimited),
            Vec::new()
        );
        assert_eq!(view(&daemon, DESK).presence, Presence::Away);
    }

    #[test]
    fn a_forgotten_host_forgets_the_relays_refusal() {
        let mut daemon = refused_desk();
        step(&mut daemon, Input::HostForgotten { host: DESK });
        assert_eq!(view(&daemon, DESK).presence, Presence::Offline);
    }

    #[test]
    fn a_route_coming_up_clears_the_last_dial_error() {
        let mut daemon = dialling();
        up(&mut daemon, 1, RELAY, CarrierKind::Relay);
        let out = claims(&mut daemon, 1, &[DESK]);
        let attempt = out
            .iter()
            .find_map(|output| match output {
                Output::Dial { attempt, .. } => Some(*attempt),
                _ => None,
            })
            .expect("the desk is dialled");
        step(
            &mut daemon,
            Input::AttemptEnded {
                host: DESK,
                attempt,
                outcome: crate::AttemptOutcome::Failed("connection refused".to_owned()),
            },
        );
        assert_eq!(daemon.last_dial_error(DESK), Some("connection refused"));
        up(&mut daemon, 2, DESK, CarrierKind::Forwarded);
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
