//! What each host reads as, decided from how attempts to reach it went.
//!
//! A host reads online once a channel to it opened on a route still
//! listed, away while the relay refuses channels to it because the
//! account's plan carries no connection between hosts, and offline
//! otherwise. A route coming up changes nothing until an attempt on it
//! concludes, and a failure counts only on the route a fresh call would
//! take first: a route the listing no longer prefers says nothing, since
//! the route that replaced it is tried on its own. A failure because the
//! link went says nothing either: the link going is what decides.
//!
//! A host no channel is open to while a route lists it is tried again in
//! the background, backing off between tries, on one timer per host.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::Daemon;
use crate::ids::{HostId, LinkId, TimerId};
use crate::io::Output;
use crate::payload::{ChannelError, StreamRefusal};

/// How long after a failed try the next one is made, doubling after each
/// failure up to [`RETRY_MAX_MS`].
pub const RETRY_FIRST_MS: u64 = 1_000;
pub const RETRY_MAX_MS: u64 = 30_000;

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

/// How an attempt the core asked for went.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Tried {
    /// A channel is open on the route.
    Opened,
    Failed(ChannelError),
    /// The shell did not try: the host is not one this host trusts, or it
    /// said it no longer trusts this one.
    Abandoned,
}

/// Every host the core has something to say about.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresenceState {
    hosts: BTreeMap<HostId, HostPresence>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct HostPresence {
    /// The route the host's channels last opened on: the one carrying its
    /// traffic.
    active: Option<Route>,
    /// What the last attempt that concluded found; none reads offline.
    reached: Option<Reached>,
    /// Whether the relay's refusal for the account's plan is what the last
    /// concluded attempt found.
    refused_for_plan: bool,
    retry: Option<Retry>,
    /// The view last told, so a view is told when it moves and not merely
    /// when something happened.
    told: HostView,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Reached {
    Online,
    Away,
}

/// A host being tried again in the background.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Retry {
    /// How long the next wait is.
    pause_ms: u64,
    /// The next wait is cut short: someone asked for a try now.
    kicked: bool,
    /// Whether the wait before the try under way was cut short, which
    /// starts the backoff afresh.
    wait_kicked: bool,
    /// A try is under way, and no timer is armed until it concludes.
    trying: bool,
}

impl PresenceState {
    /// What the core last told about `host`.
    pub fn view(&self, host: HostId) -> HostView {
        self.hosts
            .get(&host)
            .map(|known| known.told.clone())
            .unwrap_or_default()
    }

    /// Whether `host` is being tried again in the background.
    pub fn retrying(&self, host: HostId) -> bool {
        self.hosts
            .get(&host)
            .is_some_and(|known| known.retry.is_some())
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

    /// Acts on each route that came up or went since `before`.
    pub(crate) fn routes_moved(
        &mut self,
        before: Vec<(HostId, Vec<Route>)>,
        out: &mut Vec<Output>,
    ) {
        for (host, was) in before {
            self.touched.host(host);
            let now = self.routing.links.routes_to(host);
            for gone in was.iter().filter(|route| !now.contains(route)) {
                self.route_gone(host, *gone, out);
            }
            for up in now.iter().filter(|route| !was.contains(route)) {
                self.route_up(host, *up, out);
            }
        }
    }

    /// A route to `host` is listed: tried at once, so the host reads as
    /// the first channel on it finds. A host the account's relay claims is
    /// tried in the background, since the relay claims every host on the
    /// account and its tunnel takes a round trip.
    fn route_up(&mut self, host: HostId, route: Route, out: &mut Vec<Output>) {
        if self.routing.links.is_relay(host) {
            return;
        }
        let active = self
            .presence
            .hosts
            .get(&host)
            .and_then(|known| known.active);
        match route {
            Route::Direct(_) if !active.is_some_and(|active| active.is_direct()) => {
                self.start_attempt(host, route, out);
            }
            Route::Via(_) if active.is_some() => {}
            Route::Via(relay) if self.routing.links.is_relay(relay) => {
                self.retry(host, true, out);
            }
            Route::Via(_) => self.start_attempt(host, route, out),
            Route::Direct(_) => {}
        }
    }

    /// `route` to `host` is no longer listed, and its channels go. A host
    /// with no route left reads offline. One whose traffic the route
    /// carried while another stays listed reads as it did while the other
    /// is tried at once.
    fn route_gone(&mut self, host: HostId, route: Route, out: &mut Vec<Output>) {
        out.push(Output::DropRoute { host, route });
        let listed = !self.routing.links.routes_to(host).is_empty();
        let Some(known) = self.presence.hosts.get_mut(&host) else {
            return;
        };
        let carried = known.active == Some(route);
        if carried || !listed {
            known.active = None;
        }
        if !listed {
            known.reached = None;
            known.refused_for_plan = false;
        }
        if carried && listed {
            self.retry(host, true, out);
        }
    }

    /// An attempt to reach `host` on `route` concluded, whoever made it.
    pub(crate) fn concluded(
        &mut self,
        host: HostId,
        route: Route,
        outcome: Result<(), &ChannelError>,
        out: &mut Vec<Output>,
    ) {
        self.touched.host(host);
        let routes = self.routing.links.routes_to(host);
        match outcome {
            Ok(()) => {
                if !routes.contains(&route) {
                    return;
                }
                let known = self.presence.hosts.entry(host).or_default();
                known.reached = Some(Reached::Online);
                known.refused_for_plan = false;
                known.retry = None;
                let old = known.active;
                // A direct route carrying the host's traffic stays the one
                // that does when a channel opens through a relay.
                known.active = match old {
                    Some(active) if active.is_direct() && !route.is_direct() => Some(active),
                    _ => Some(route),
                };
                let now_active = known.active;
                self.cancel(TimerId::Retry(host), out);
                if let Some(old) = old
                    && now_active != Some(old)
                {
                    out.push(Output::DropRoute { host, route: old });
                }
            }
            Err(ChannelError::LinkClosed | ChannelError::NoLink) => {}
            Err(_) if routes.first() != Some(&route) => {}
            Err(error) => {
                let known = self.presence.hosts.entry(host).or_default();
                if known.active == Some(route) {
                    known.active = None;
                }
                if known.active.is_none() {
                    if *error == ChannelError::Refused(StreamRefusal::PaymentRequired) {
                        known.reached = Some(Reached::Away);
                        known.refused_for_plan = true;
                    } else {
                        known.reached = None;
                        known.refused_for_plan = false;
                    }
                    self.retry(host, false, out);
                }
            }
        }
    }

    /// Keeps trying `host` in the background while a route lists it and no
    /// channel is open, backing off between tries; `now` tries at once and
    /// starts the backoff afresh. One retry runs per host: asking again
    /// while it runs only hurries it when `now` says so.
    fn retry(&mut self, host: HostId, now: bool, out: &mut Vec<Output>) {
        self.touched.host(host);
        let at = self.timers.now_ms;
        let known = self.presence.hosts.entry(host).or_default();
        match &mut known.retry {
            Some(retry) => {
                if now {
                    retry.kicked = true;
                    if !retry.trying {
                        self.arm(TimerId::Retry(host), at, out);
                    }
                }
            }
            None => {
                known.retry = Some(Retry {
                    pause_ms: RETRY_FIRST_MS,
                    kicked: now,
                    wait_kicked: false,
                    trying: false,
                });
                let wait = if now { 0 } else { RETRY_FIRST_MS };
                self.arm(TimerId::Retry(host), at + wait, out);
            }
        }
    }

    /// The retry timer for `host` fired: try the route a fresh call would
    /// take, unless no route lists the host or a channel is already open.
    pub(crate) fn retry_fired(&mut self, host: HostId, out: &mut Vec<Output>) {
        self.touched.host(host);
        self.timers.armed.remove(&TimerId::Retry(host));
        let route = self.routing.links.routes_to(host).first().copied();
        let Some(known) = self.presence.hosts.get_mut(&host) else {
            return;
        };
        let Some(retry) = known.retry.as_mut().filter(|retry| !retry.trying) else {
            return;
        };
        match route {
            Some(route) if known.active.is_none() => {
                retry.wait_kicked = std::mem::take(&mut retry.kicked);
                retry.trying = true;
                self.start_attempt(host, route, out);
            }
            _ => known.retry = None,
        }
    }

    /// Asks the shell to try `host` on `route`, unless an attempt the core
    /// asked for is already under way there.
    fn start_attempt(&mut self, host: HostId, route: Route, out: &mut Vec<Output>) {
        self.touched.host(host);
        let flying = self.routing.attempts.entry((host, route)).or_default();
        if *flying == 0 {
            *flying += 1;
            out.push(Output::Attempt { host, route });
        }
    }

    /// The shell's try at an attempt the core asked for concluded.
    pub(crate) fn attempt_concluded(
        &mut self,
        host: HostId,
        route: Route,
        tried: Tried,
        out: &mut Vec<Output>,
    ) {
        self.touched.host(host);
        if let Some(flying) = self.routing.attempts.get_mut(&(host, route)) {
            *flying = flying.saturating_sub(1);
            if *flying == 0 {
                self.routing.attempts.remove(&(host, route));
            }
        }
        match &tried {
            Tried::Opened => self.concluded(host, route, Ok(()), out),
            Tried::Failed(error) => self.concluded(host, route, Err(error), out),
            Tried::Abandoned => {
                if let Some(known) = self.presence.hosts.get_mut(&host) {
                    known.retry = None;
                }
                self.cancel(TimerId::Retry(host), out);
            }
        }
        let at = self.timers.now_ms;
        let Some(retry) = self
            .presence
            .hosts
            .get_mut(&host)
            .and_then(|known| known.retry.as_mut())
            .filter(|retry| retry.trying)
        else {
            return;
        };
        retry.trying = false;
        retry.pause_ms = if retry.wait_kicked {
            RETRY_FIRST_MS
        } else {
            (retry.pause_ms * 2).min(RETRY_MAX_MS)
        };
        let wait = if retry.kicked { 0 } else { retry.pause_ms };
        self.arm(TimerId::Retry(host), at + wait, out);
    }

    /// The account's plan changed, so what the relay carries changed with
    /// it: a host it refused for the plan may open now, and a channel it
    /// carried on the old plan ended if the plan lapsed. Every host the
    /// relay carried or refused is tried again at once, on a fresh
    /// channel; each reads as it did until its try concludes.
    pub(crate) fn plan_changed(&mut self, out: &mut Vec<Output>) {
        let hosts: Vec<(HostId, Option<Route>)> = self
            .presence
            .hosts
            .iter()
            .filter_map(|(host, known)| match known.active {
                Some(route @ Route::Via(_)) => Some((*host, Some(route))),
                _ if known.reached == Some(Reached::Away) => Some((*host, None)),
                _ => None,
            })
            .collect();
        for (host, relayed) in hosts {
            if let Some(route) = relayed {
                if let Some(known) = self.presence.hosts.get_mut(&host) {
                    known.active = None;
                }
                out.push(Output::DropRoute { host, route });
            }
            self.retry(host, true, out);
        }
    }

    /// A call to `host` found no channel on the route that lists it within
    /// its patience: the host reads offline until a channel opens again,
    /// and one is tried in the background.
    pub(crate) fn calls_failed(&mut self, host: HostId, out: &mut Vec<Output>) {
        self.touched.host(host);
        let first = self.routing.links.routes_to(host).first().copied();
        let Some(known) = self.presence.hosts.get_mut(&host) else {
            if first.is_some() {
                self.retry(host, false, out);
            }
            return;
        };
        if known.active.or(first).is_none() {
            return;
        }
        known.active = None;
        known.reached = None;
        known.refused_for_plan = false;
        self.retry(host, false, out);
    }

    /// Someone asked for `host` to be tried now.
    pub(crate) fn try_again(&mut self, host: HostId, out: &mut Vec<Output>) {
        self.retry(host, true, out);
    }

    /// `host` is forgotten: its trust ended or is being replaced. Nothing
    /// the core decided about it survives, and its retry stops.
    pub(crate) fn forget_host(&mut self, host: HostId, out: &mut Vec<Output>) {
        self.touched.host(host);
        if let Some(known) = self.presence.hosts.get_mut(&host) {
            *known = HostPresence {
                told: std::mem::take(&mut known.told),
                ..HostPresence::default()
            };
        }
        self.cancel(TimerId::Retry(host), out);
    }

    /// Tells the shell each host the step touched whose view moved.
    pub(crate) fn tell_presence(&mut self, out: &mut Vec<Output>) {
        for host in self.touched.hosts.clone() {
            let routes = self.routing.links.routes_to(host);
            let known = self.presence.hosts.entry(host).or_default();
            let mut view = HostView {
                routes,
                active: known.active,
                presence: Presence::Offline,
            };
            if view.route().is_some() {
                view.presence = match known.reached {
                    Some(Reached::Online) => Presence::Online,
                    Some(Reached::Away) => Presence::Away,
                    None => Presence::Offline,
                };
            }
            if view != known.told {
                known.told = view.clone();
                out.push(Output::Presence { host, view });
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

    /// Presence and routing's cheap invariants, for each host the step
    /// touched.
    pub(crate) fn check_presence(&self) -> Result<(), crate::Violation> {
        for host in &self.touched.hosts {
            for ((_, route), flying) in self
                .routing
                .attempts
                .range((*host, Route::Direct(LinkId(0)))..)
                .take_while(|((of, _), _)| of == host)
            {
                if *flying > 1 {
                    return Err(crate::Violation {
                        invariant: ONE_ATTEMPT_PER_ROUTE,
                        detail: format!(
                            "{flying} attempts are in flight to host {} on {route:?}",
                            crate::hex(&host.0)
                        ),
                    });
                }
            }
            let Some(known) = self.presence.hosts.get(host) else {
                if self.timers.armed.contains(&TimerId::Retry(*host)) {
                    return Err(retry_timer(*host, "it is not being retried"));
                }
                continue;
            };
            if known.reached == Some(Reached::Away) && !known.refused_for_plan {
                return Err(crate::Violation {
                    invariant: AWAY_ONLY_ON_PAYMENT,
                    detail: format!(
                        "host {} reads away with no refusal for the plan recorded",
                        crate::hex(&host.0)
                    ),
                });
            }
            let waiting = known.retry.is_some_and(|retry| !retry.trying);
            let armed = self.timers.armed.contains(&TimerId::Retry(*host));
            if waiting != armed {
                return Err(retry_timer(
                    *host,
                    if armed {
                        "it is not waiting to be retried"
                    } else {
                        "it waits to be retried"
                    },
                ));
            }
        }
        Ok(())
    }
}

fn retry_timer(host: HostId, why: &str) -> crate::Violation {
    crate::Violation {
        invariant: ONE_RETRY_TIMER,
        detail: format!("host {}'s retry timer is wrong: {why}", crate::hex(&host.0)),
    }
}

/// Presence and routing's cheap invariants, as the sentences a violation
/// names.
pub const ONE_ATTEMPT_PER_ROUTE: &str =
    "at most one attempt the core asked for is in flight per host and route";
pub const AWAY_ONLY_ON_PAYMENT: &str =
    "a host reads away only while the relay's refusal for the account's plan is recorded";
pub const ONE_RETRY_TIMER: &str =
    "a host waiting to be retried has its one retry timer armed, and no other host has one";

#[cfg(test)]
mod tests {
    use store::Sqlite;
    use wire::message::Body;
    use wire::{Hello, Message, NeighborDown, NeighborUp};

    use super::*;
    use crate::payload::{CarrierKind, ChannelClass, PublicKey};
    use crate::{DumpReason, Input, StartOptions};

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

    fn concluded(daemon: &mut Daemon, host: HostId, route: Route, tried: Tried) -> Vec<Output> {
        step(daemon, Input::AttemptConcluded { host, route, tried })
    }

    fn view(daemon: &Daemon, host: HostId) -> HostView {
        daemon.presence().view(host)
    }

    fn attempts(out: &[Output]) -> Vec<(HostId, Route)> {
        out.iter()
            .filter_map(|output| match output {
                Output::Attempt { host, route } => Some((*host, *route)),
                _ => None,
            })
            .collect()
    }

    fn retry_at(out: &[Output], host: HostId) -> Option<u64> {
        out.iter().find_map(|output| match output {
            Output::ArmTimer {
                id: TimerId::Retry(of),
                at_ms,
            } if *of == host => Some(*at_ms),
            _ => None,
        })
    }

    fn refused(refusal: StreamRefusal) -> Tried {
        Tried::Failed(ChannelError::Refused(refusal))
    }

    fn tick(daemon: &mut Daemon, now_ms: u64) {
        step(daemon, Input::Tick { now_ms });
    }

    fn fire(daemon: &mut Daemon, host: HostId) -> Vec<Output> {
        step(
            daemon,
            Input::Timer {
                id: TimerId::Retry(host),
            },
        )
    }

    #[test]
    fn a_hosts_own_links_come_first_newest_first_then_each_host_claiming_to_reach_it() {
        let mut daemon = daemon();
        up(&mut daemon, 1, LAPTOP, CarrierKind::Direct);
        claims(&mut daemon, 1, &[DESK]);
        up(&mut daemon, 2, RELAY, CarrierKind::Relay);
        claims(&mut daemon, 2, &[DESK, LAPTOP]);
        assert_eq!(
            view(&daemon, DESK).routes,
            vec![Route::Via(LAPTOP), Route::Via(RELAY)]
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
        down(&mut daemon, 2);
        assert_eq!(
            view(&daemon, DESK).routes,
            vec![Route::Direct(LinkId(3)), Route::Via(LAPTOP)]
        );
        assert_eq!(view(&daemon, LAPTOP).routes, vec![Route::Direct(LinkId(1))]);

        // A neighbor saying it lost the host withdraws its claim alone.
        frame(
            &mut daemon,
            1,
            Body::NeighborDown(NeighborDown {
                host_id: DESK.0.to_vec(),
            }),
        );
        assert_eq!(view(&daemon, DESK).routes, vec![Route::Direct(LinkId(3))]);
        down(&mut daemon, 3);
        assert_eq!(view(&daemon, DESK), HostView::default());
    }

    #[test]
    fn a_relay_is_never_a_route_nor_tried() {
        let mut daemon = daemon();
        let out = up(&mut daemon, 1, RELAY, CarrierKind::Relay);
        assert_eq!(attempts(&out), Vec::new());
        assert_eq!(retry_at(&out, RELAY), None);

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
    fn a_route_coming_up_is_tried_and_the_host_reads_as_the_attempt_on_it_finds() {
        let mut daemon = daemon();
        let out = up(&mut daemon, 1, DESK, CarrierKind::Direct);
        let direct = Route::Direct(LinkId(1));
        assert_eq!(attempts(&out), vec![(DESK, direct)]);
        assert_eq!(
            view(&daemon, DESK).presence,
            Presence::Offline,
            "a route coming up changes nothing until its attempt concludes"
        );

        concluded(&mut daemon, DESK, direct, Tried::Opened);
        let online = view(&daemon, DESK);
        assert_eq!(online.presence, Presence::Online);
        assert_eq!(online.active, Some(direct));

        // A relay's claim adds a route but no attempt: the direct link
        // carries the host's traffic.
        up(&mut daemon, 2, LAPTOP, CarrierKind::Direct);
        let out = claims(&mut daemon, 2, &[DESK]);
        assert_eq!(attempts(&out), Vec::new());
        assert_eq!(view(&daemon, DESK).presence, Presence::Online);
    }

    #[test]
    fn a_failure_counts_only_on_the_route_a_fresh_call_takes_first() {
        let mut daemon = daemon();
        up(&mut daemon, 1, LAPTOP, CarrierKind::Direct);
        claims(&mut daemon, 1, &[DESK]);
        let via = Route::Via(LAPTOP);
        concluded(&mut daemon, DESK, via, Tried::Opened);
        up(&mut daemon, 2, DESK, CarrierKind::Direct);

        // The direct link is first now: the relayed route failing says
        // nothing, and the host reads as it did.
        let out = concluded(
            &mut daemon,
            DESK,
            via,
            Tried::Failed(ChannelError::Handshake("reset".into())),
        );
        assert_eq!(view(&daemon, DESK).presence, Presence::Online);
        assert_eq!(retry_at(&out, DESK), None);

        // The link going under an attempt says nothing either: the link
        // going decides.
        let direct = Route::Direct(LinkId(2));
        concluded(
            &mut daemon,
            DESK,
            direct,
            Tried::Failed(ChannelError::LinkClosed),
        );
        assert_eq!(view(&daemon, DESK).presence, Presence::Online);

        // A failure on the first route with no other carrying traffic
        // reads offline. The relayed route still carries it here.
        concluded(
            &mut daemon,
            DESK,
            direct,
            Tried::Failed(ChannelError::Handshake("timed out".into())),
        );
        assert_eq!(view(&daemon, DESK).active, Some(via));
        assert_eq!(view(&daemon, DESK).presence, Presence::Online);
    }

    #[test]
    fn a_host_reads_away_only_when_the_relay_refuses_it_for_the_plan() {
        let mut daemon = daemon();
        up(&mut daemon, 1, LAPTOP, CarrierKind::Direct);
        claims(&mut daemon, 1, &[DESK]);
        let via = Route::Via(LAPTOP);
        tick(&mut daemon, 100);

        let out = concluded(
            &mut daemon,
            DESK,
            via,
            refused(StreamRefusal::PaymentRequired),
        );
        assert_eq!(view(&daemon, DESK).presence, Presence::Away);
        assert_eq!(retry_at(&out, DESK), Some(100 + RETRY_FIRST_MS));

        let out = concluded(&mut daemon, DESK, via, refused(StreamRefusal::NoRoute));
        assert_eq!(view(&daemon, DESK).presence, Presence::Offline);
        assert_eq!(
            retry_at(&out, DESK),
            None,
            "one retry per host: a second failure does not arm another"
        );
        assert!(daemon.presence().retrying(DESK));
    }

    #[test]
    fn a_retry_backs_off_doubling_and_a_try_now_starts_it_afresh() {
        let mut daemon = daemon();
        up(&mut daemon, 1, DESK, CarrierKind::Direct);
        let direct = Route::Direct(LinkId(1));
        tick(&mut daemon, 0);
        let out = concluded(&mut daemon, DESK, direct, refused(StreamRefusal::NoRoute));
        assert_eq!(retry_at(&out, DESK), Some(RETRY_FIRST_MS));

        let mut now = RETRY_FIRST_MS;
        let mut pauses = Vec::new();
        for _ in 0..7 {
            tick(&mut daemon, now);
            let out = fire(&mut daemon, DESK);
            assert_eq!(attempts(&out), vec![(DESK, direct)]);
            let out = concluded(&mut daemon, DESK, direct, refused(StreamRefusal::NoRoute));
            let at = retry_at(&out, DESK).expect("the next try is armed");
            pauses.push(at - now);
            now = at;
        }
        assert_eq!(
            pauses,
            vec![2_000, 4_000, 8_000, 16_000, 30_000, 30_000, 30_000]
        );

        // Asked to try now while it waits: at once, and the backoff starts
        // afresh after.
        tick(&mut daemon, now);
        let out = step(&mut daemon, Input::TryAgain { host: DESK });
        assert_eq!(retry_at(&out, DESK), Some(now));
        let out = fire(&mut daemon, DESK);
        assert_eq!(attempts(&out), vec![(DESK, direct)]);
        let out = concluded(&mut daemon, DESK, direct, refused(StreamRefusal::NoRoute));
        assert_eq!(retry_at(&out, DESK), Some(now + RETRY_FIRST_MS));

        // A channel opening ends the retry and its timer.
        let out = concluded(&mut daemon, DESK, direct, Tried::Opened);
        assert!(out.contains(&Output::CancelTimer {
            id: TimerId::Retry(DESK)
        }));
        assert!(!daemon.presence().retrying(DESK));
        assert_eq!(
            fire(&mut daemon, DESK),
            Vec::new(),
            "a stale firing does nothing"
        );
    }

    #[test]
    fn an_abandoned_try_and_a_host_with_no_route_end_the_retry() {
        let mut daemon = daemon();
        up(&mut daemon, 1, RELAY, CarrierKind::Relay);
        // A host the relay claims is tried in the background, at once.
        let out = claims(&mut daemon, 1, &[DESK]);
        assert_eq!(attempts(&out), Vec::new());
        assert_eq!(retry_at(&out, DESK), Some(0));
        let out = fire(&mut daemon, DESK);
        assert_eq!(attempts(&out), vec![(DESK, Route::Via(RELAY))]);
        // The shell does not try a host it does not trust.
        concluded(&mut daemon, DESK, Route::Via(RELAY), Tried::Abandoned);
        assert!(!daemon.presence().retrying(DESK));

        // A retry whose host lost every route stops when it fires.
        up(&mut daemon, 2, LAPTOP, CarrierKind::Direct);
        concluded(
            &mut daemon,
            LAPTOP,
            Route::Direct(LinkId(2)),
            refused(StreamRefusal::NoRoute),
        );
        down(&mut daemon, 2);
        assert_eq!(attempts(&fire(&mut daemon, LAPTOP)), Vec::new());
        assert!(!daemon.presence().retrying(LAPTOP));
    }

    #[test]
    fn a_route_that_carried_the_hosts_traffic_going_tries_the_next_at_once() {
        let mut daemon = daemon();
        up(&mut daemon, 1, LAPTOP, CarrierKind::Direct);
        claims(&mut daemon, 1, &[DESK]);
        up(&mut daemon, 2, DESK, CarrierKind::Direct);
        let direct = Route::Direct(LinkId(2));
        concluded(&mut daemon, DESK, direct, Tried::Opened);
        tick(&mut daemon, 50);

        let out = down(&mut daemon, 2);
        assert!(out.contains(&Output::DropRoute {
            host: DESK,
            route: direct
        }));
        assert_eq!(retry_at(&out, DESK), Some(50));
        let reads = view(&daemon, DESK);
        assert_eq!(
            (reads.presence, reads.active),
            (Presence::Online, None),
            "it reads as it did while the next route is tried"
        );

        // The last route going reads offline.
        down(&mut daemon, 1);
        assert_eq!(view(&daemon, DESK).presence, Presence::Offline);
    }

    #[test]
    fn a_plan_change_tries_every_host_the_relay_carried_or_refused_at_once() {
        let mut daemon = daemon();
        up(&mut daemon, 1, RELAY, CarrierKind::Relay);
        claims(&mut daemon, 1, &[DESK, LAPTOP]);
        let via = Route::Via(RELAY);
        concluded(&mut daemon, DESK, via, Tried::Opened);
        concluded(
            &mut daemon,
            LAPTOP,
            via,
            refused(StreamRefusal::PaymentRequired),
        );
        tick(&mut daemon, 10);

        let out = step(&mut daemon, Input::PlanChanged);
        assert!(out.contains(&Output::DropRoute {
            host: DESK,
            route: via
        }));
        assert_eq!(retry_at(&out, DESK), Some(10));
        assert_eq!(retry_at(&out, LAPTOP), Some(10));
        assert_eq!(view(&daemon, DESK).presence, Presence::Online);
        assert_eq!(view(&daemon, LAPTOP).presence, Presence::Away);
    }

    #[test]
    fn a_call_finding_no_channel_reads_the_host_offline_and_tries_it_again() {
        let mut daemon = daemon();
        up(&mut daemon, 1, DESK, CarrierKind::Direct);
        concluded(&mut daemon, DESK, Route::Direct(LinkId(1)), Tried::Opened);
        let out = step(&mut daemon, Input::CallsFailed { host: DESK });
        assert_eq!(view(&daemon, DESK).presence, Presence::Offline);
        assert!(retry_at(&out, DESK).is_some());

        let out = step(&mut daemon, Input::HostForgotten { host: DESK });
        assert!(out.contains(&Output::CancelTimer {
            id: TimerId::Retry(DESK)
        }));
        assert!(!daemon.presence().retrying(DESK));
    }

    #[test]
    fn a_channel_the_core_opened_concludes_an_attempt_on_its_route() {
        let mut daemon = daemon();
        up(&mut daemon, 1, DESK, CarrierKind::Direct);
        let want = |pin| Input::WantChannel {
            link: LinkId(1),
            peer: DESK,
            class: ChannelClass::Session,
            pin,
        };
        step(&mut daemon, want(None));
        assert!(
            daemon.presence().retrying(DESK),
            "a peer with no key pinned failed its attempt"
        );
        let out = step(&mut daemon, want(Some(PublicKey(vec![7; 32]))));
        let Some(channel) = out.iter().find_map(|output| match output {
            Output::OpenChannel { channel, .. } => Some(*channel),
            _ => None,
        }) else {
            panic!("the channel is opened: {out:?}");
        };
        step(
            &mut daemon,
            Input::ChannelOpened {
                channel,
                link: LinkId(1),
                peer: DESK,
                class: ChannelClass::Session,
            },
        );
        assert_eq!(view(&daemon, DESK).presence, Presence::Online);

        // An open channel ending says nothing of the host.
        step(
            &mut daemon,
            Input::ChannelClosed {
                channel,
                reason: ChannelError::Ended,
            },
        );
        assert_eq!(view(&daemon, DESK).presence, Presence::Online);
    }

    #[test]
    fn a_host_read_away_with_no_refusal_recorded_asks_for_a_dump() {
        let mut daemon = daemon();
        up(&mut daemon, 1, DESK, CarrierKind::Direct);
        daemon.presence.hosts.entry(DESK).or_default().reached = Some(Reached::Away);

        let mut out = Vec::new();
        daemon.step(
            Input::LinkControl {
                link: LinkId(1),
                frame: Message {
                    body: Some(Body::Hello(Hello {
                        neighbors: vec![host(DESK)],
                        ..Default::default()
                    })),
                },
            },
            &mut out,
        );
        // The frame names nothing new, so nothing touched the host.
        assert!(
            !out.iter()
                .any(|output| matches!(output, Output::Dump { .. }))
        );

        daemon.step(Input::TryAgain { host: DESK }, &mut out);
        let dumped = out.iter().find_map(|output| match output {
            Output::Dump {
                reason: DumpReason::Violation(violation),
            } => Some(violation.invariant),
            _ => None,
        });
        assert_eq!(dumped, Some(AWAY_ONLY_ON_PAYMENT));
    }
}
