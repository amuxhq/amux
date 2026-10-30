//! Reaching each trusted host: when it is dialled, on which paths, for how
//! long, and when it is tried again.
//!
//! A host is wanted while the driver dials at all, this profile trusts it,
//! it is neither this host nor a relay, it has not said it no longer trusts
//! this one, and no link of its own is up. A wanted host is dialled with one
//! connection whose first flight goes out on every path the core knows for
//! it at once (the addresses the local network found it at, the address it
//! last answered at, SSH where it paired that way, each paired host that
//! claims to reach it, and the relay for a host on the account), and the
//! first path it answers on carries the link. The driver makes the attempt
//! and says how it ended; the core holds the one attempt per host, its
//! deadline, and the pause before the next.
//!
//! An attempt that ends without a link is a failure, and the host is tried
//! again after a pause of a second doubling to thirty. Anything that gives
//! the host a path it did not have, or a call wanting it now, ends the
//! pause and starts the count afresh; a path found while an attempt is out
//! starts a fresh attempt on every path at once. A host that lost a link
//! that lived less than a second waits out the rest of that second before
//! it is dialled again: a link closed that soon was most likely refused as
//! it came up, and an immediate redial is refused the same way, often
//! enough to trip the peer's handshake rate limit.

use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::Daemon;
use crate::ids::{AttemptId, HostId, LinkId, TimerId};
use crate::io::Output;
use crate::payload::{CarrierKind, ForwardRefusal, PeerAddress, PeerRequest, Source};
use crate::presence::Route;
use crate::trust::TrustChange;

/// How long an attempt has to bring a link up: as long as a link's hello
/// may take once its connection is up, so a slow host is not given up on
/// while it answers.
pub const ATTEMPT_DEADLINE_MS: u64 = 10_000;

/// The pause after a host's first failed attempt, doubling after each
/// failure in a row up to [`RETRY_MAX_MS`]. A machine that restarts may be
/// advertised again before it answers, a browser that saw its
/// advertisement come and go says nothing more, and a host paired by
/// address across a network discovery cannot see is never advertised at
/// all: only dialling it again finds it.
pub const RETRY_FIRST_MS: u64 = 1_000;
pub const RETRY_MAX_MS: u64 = 30_000;

/// How long a host's link must have lived for the next dial to follow its
/// loss at once, and how long after it came up the next dial waits
/// otherwise.
pub const SHORT_LINK_MS: u64 = 1_000;

/// What the driver dials, as it said last.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Dialling {
    /// Nothing: its edge is not up, or is stopping.
    #[default]
    Off,
    /// Everything but a host's own addresses: the host stays off the local
    /// network, so it dials through forwarders and over SSH alone.
    Indirect,
    /// Every path.
    All,
}

/// One way an attempt may reach a host.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum DialPath {
    /// Straight to an address.
    Direct(SocketAddr),
    /// Through the paired host at the far end of a link, which forwards
    /// packets between the hosts it links to.
    Through(LinkId),
    /// Over SSH to a destination, which runs the host's link there.
    Ssh(String),
}

/// How an attempt ended, as the driver saw it.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AttemptOutcome {
    /// A link of the host's own came up on it, at the address it answered
    /// at when it answered at one directly.
    Linked { addr: Option<SocketAddr> },
    /// No link came up, in the driver's words.
    Failed(String),
    /// The host refused this one's certificate: it no longer trusts it.
    Revoked,
    /// Something answered on `path` that ended the attempt before any other
    /// path could: a machine with another key where the host used to be,
    /// or a listener refusing the connection. It says nothing of the other
    /// paths.
    TurnedAway { path: DialPath, error: String },
}

/// Every host being reached, and whether the driver dials at all.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reach {
    /// What the driver dials.
    dialling: Dialling,
    hosts: BTreeMap<HostId, Reaching>,
    /// The last attempt id minted.
    next_attempt: u64,
    /// The addresses each host's path machine was last told, so it hears
    /// only of a change.
    told: BTreeMap<HostId, Vec<PeerAddress>>,
    /// The link to the relay.
    relay: RelayReach,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Reaching {
    /// The attempt out, from its dial until it ends or its deadline fires.
    attempt: Option<Attempt>,
    /// Attempts in a row that ended without a link.
    failures: u32,
    /// The pause the host's retry timer is armed for.
    pause: Option<Pause>,
    /// Since when a link of the host's own has been up, while one is.
    linked_since_ms: Option<u64>,
    /// The paths that turned an attempt away, with what each answered:
    /// left out of every attempt until one links or the run of them fails.
    turned_away: BTreeMap<DialPath, String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Attempt {
    id: AttemptId,
    plan: Plan,
    /// The forwarders that refused to carry it, by the link to each.
    refused: BTreeSet<LinkId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Pause {
    /// After a failed attempt: something new to dial ends it.
    Backoff,
    /// After a link that closed almost as it came up: nothing ends it, or
    /// whatever refused the link is asked again at once.
    ShortLink,
    /// The host just paired with this one as the joiner, which dials: a
    /// dial from here too would cross it, and a call on the link it
    /// displaced would fail. Its link coming or an attempt's deadline
    /// passing ends it.
    Expecting,
}

/// What one attempt dials.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Plan {
    pub paths: Vec<DialPath>,
    /// The relay too, for a host on the account: every link to the relay
    /// is a path, those that come up while the attempt is out as well.
    pub through_relay: bool,
}

impl Plan {
    fn is_empty(&self) -> bool {
        self.paths.is_empty() && !self.through_relay
    }

    /// Whether `self` has a path `other` lacks.
    fn adds_to(&self, other: &Plan) -> bool {
        (self.through_relay && !other.through_relay)
            || self.paths.iter().any(|path| !other.paths.contains(path))
    }

    /// Whether every path of the plan goes through a forwarder.
    fn forwarded_only(&self) -> bool {
        self.paths
            .iter()
            .all(|path| matches!(path, DialPath::Through(_)))
    }
}

impl Reach {
    /// What the driver dials.
    pub fn dialling(&self) -> Dialling {
        self.dialling
    }

    /// Whether a link is expected from `host` itself, which just paired
    /// with this host as the joiner and dials it: this host does not dial it
    /// meanwhile, so a call waits for that link.
    pub fn expecting(&self, host: HostId) -> bool {
        self.hosts
            .get(&host)
            .is_some_and(|reaching| reaching.pause == Some(Pause::Expecting))
    }

    /// The attempt out to `host`, if one is.
    pub fn attempt(&self, host: HostId) -> Option<AttemptId> {
        self.hosts
            .get(&host)
            .and_then(|reaching| reaching.attempt.as_ref())
            .map(|attempt| attempt.id)
    }

    /// Every attempt out, for a checker that renumbers them.
    pub(crate) fn attempts(&self) -> impl Iterator<Item = AttemptId> + '_ {
        self.hosts
            .values()
            .filter_map(|reaching| reaching.attempt.as_ref().map(|attempt| attempt.id))
    }

    /// The state as a checker tells two apart: attempts renumbered by
    /// `rank`, and no counter of failures, which only lengthens a pause.
    pub(crate) fn canonical(&self, rank: impl Fn(AttemptId) -> AttemptId) -> Reach {
        let mut reach = self.clone();
        reach.next_attempt = 0;
        reach.relay = self.relay.canonical(&rank);
        for reaching in reach.hosts.values_mut() {
            reaching.failures = 0;
            reaching.linked_since_ms = reaching.linked_since_ms.map(|_| 0);
            if let Some(attempt) = &mut reaching.attempt {
                attempt.id = rank(attempt.id);
            }
        }
        reach
    }
}

impl Daemon {
    /// Every host being reached, as the core holds it.
    pub fn reach(&self) -> &Reach {
        &self.routing.reach
    }

    /// The driver dials from now on, or no longer: its edge started, or is
    /// stopping. Stopping abandons every attempt out and every pause.
    pub(crate) fn dialling(&mut self, dialling: Dialling, out: &mut Vec<Output>) {
        if self.routing.reach.dialling == dialling {
            return;
        }
        self.routing.reach.dialling = dialling;
        if dialling == Dialling::Off {
            for host in self.routing.reach.hosts.keys().copied().collect::<Vec<_>>() {
                self.stop_reaching(host, out);
            }
            return;
        }
        for host in self.sources.trusted().collect::<Vec<_>>() {
            self.reach_host(host, out);
        }
    }

    /// Whether `host` is to be dialled, attempt and pause aside.
    fn wanted(&self, host: HostId) -> bool {
        self.routing.reach.dialling != Dialling::Off
            && self.sources.trusts(host)
            && HostId::from_slice(self.store.own_host()) != Some(host)
            && !self.routing.links.is_relay(host)
            && !self.routing.catalogue.revoked(host)
            && !self
                .routing
                .links
                .routes_to(host)
                .iter()
                .any(Route::is_direct)
    }

    /// Whether `host` said it no longer trusts this one: it is not dialled
    /// until something says it may have taken that back.
    pub fn revoked_by(&self, host: HostId) -> bool {
        self.routing.catalogue.revoked(host)
    }

    /// Every address `host` may be reached at, each with where it came
    /// from: what it listed, then where the local network found it, then
    /// where it last answered. The addresses a dial tries are these, and a
    /// link up on another path moves to one of them that answers; none
    /// for a host no longer trusted.
    pub fn path_candidates(&self, host: HostId) -> Vec<PeerAddress> {
        let Some(entry) = self.trust().entry(Uuid::from_bytes(host.0)) else {
            return Vec::new();
        };
        let mut addrs: Vec<PeerAddress> = self
            .routing
            .addresses
            .get(&host)
            .cloned()
            .unwrap_or_default();
        let found = self.host_set.found_at(host);
        let tagged = found
            .iter()
            .map(|addr| (*addr, Source::Discovery))
            .chain(entry.direct.iter().map(|addr| (*addr, Source::LastWorked)));
        for (addr, source) in tagged {
            if !addrs.iter().any(|kept| kept.addr == addr) {
                addrs.push(PeerAddress { addr, source });
            }
        }
        addrs
    }

    /// Tells `host`'s path machine its addresses, when they changed.
    pub(crate) fn tell_paths(&mut self, host: HostId, out: &mut Vec<Output>) {
        let addrs = self.path_candidates(host);
        let told = &mut self.routing.reach.told;
        if told.get(&host).map(Vec::as_slice).unwrap_or_default() == addrs.as_slice() {
            return;
        }
        if addrs.is_empty() {
            told.remove(&host);
        } else {
            told.insert(host, addrs.clone());
        }
        out.push(Output::PeerAddresses { host, addrs });
    }

    /// Every path the core knows `host` by now: where the local network
    /// found it and where it last answered, newest first, then SSH, then
    /// each paired host claiming it on a path of its own; and the relay,
    /// for a host on the account or one the relay claims. A driver that
    /// stays off the local network is given none of its own addresses.
    pub fn dial_plan(&self, host: HostId) -> Plan {
        let mut plan = self.every_path(host);
        if self.routing.reach.dialling == Dialling::Indirect {
            plan.paths
                .retain(|path| !matches!(path, DialPath::Direct(_)));
        }
        plan
    }

    /// What the next attempt to `host` dials: its plan, less the paths
    /// that turned this run of attempts away.
    fn attempt_plan(&self, host: HostId) -> Plan {
        let mut plan = self.dial_plan(host);
        if let Some(reaching) = self.routing.reach.hosts.get(&host) {
            plan.paths
                .retain(|path| !reaching.turned_away.contains_key(path));
        }
        plan
    }

    fn every_path(&self, host: HostId) -> Plan {
        let mut paths = Vec::new();
        let direct = |addr: SocketAddr, paths: &mut Vec<DialPath>| {
            let path = DialPath::Direct(addr);
            if !paths.contains(&path) {
                paths.push(path);
            }
        };
        for addr in self.host_set.found_at(host) {
            direct(addr, &mut paths);
        }
        let entry = self.trust().entry(Uuid::from_bytes(host.0));
        for addr in entry
            .map(|entry| entry.direct.as_slice())
            .unwrap_or_default()
        {
            direct(*addr, &mut paths);
        }
        if let Some(ssh) = entry.and_then(|entry| entry.ssh.clone()) {
            paths.push(DialPath::Ssh(ssh));
        }
        let links = &self.routing.links;
        let mut through_relay = entry.is_some_and(|entry| entry.on_account);
        for route in links.routes_to(host) {
            let Route::Via(forwarder) = route else {
                continue;
            };
            if links.is_relay(forwarder) {
                through_relay = true;
                continue;
            }
            let carrying = links.routes_to(forwarder).into_iter().find_map(|route| {
                let Route::Direct(link) = route else {
                    return None;
                };
                (links.link(link)?.kind == CarrierKind::Direct).then_some(link)
            });
            if let Some(link) = carrying {
                paths.push(DialPath::Through(link));
            }
        }
        Plan {
            paths,
            through_relay,
        }
    }

    /// Dials `host` if it is wanted and nothing holds it back: no attempt
    /// out, no pause armed, and something to dial.
    pub(crate) fn reach_host(&mut self, host: HostId, out: &mut Vec<Output>) {
        if !self.wanted(host) {
            return;
        }
        let reaching = self.routing.reach.hosts.entry(host).or_default();
        if reaching.attempt.is_some() || reaching.pause.is_some() {
            return;
        }
        self.dial(host, out);
    }

    fn dial(&mut self, host: HostId, out: &mut Vec<Output>) {
        if self.dial_within(host, None, out) {
            let at_ms = self.timers.link_ms.saturating_add(ATTEMPT_DEADLINE_MS);
            self.arm(TimerId::Attempt(host), at_ms, out);
        }
    }

    /// Sends an attempt to `host` on its attempt plan, if that has a path,
    /// under the deadline already armed for it; `turned` names what turned
    /// the last one away, for the dial error when no path is left.
    fn dial_within(&mut self, host: HostId, turned: Option<String>, out: &mut Vec<Output>) -> bool {
        let plan = self.attempt_plan(host);
        if plan.is_empty() {
            if let Some(error) = turned {
                self.cancel(TimerId::Attempt(host), out);
                self.failed(host, error, out);
            }
            return false;
        }
        let early = self.first_calls(host);
        let reach = &mut self.routing.reach;
        reach.next_attempt += 1;
        let id = AttemptId(reach.next_attempt);
        reach.hosts.entry(host).or_default().attempt = Some(Attempt {
            id,
            plan: plan.clone(),
            refused: BTreeSet::new(),
        });
        out.push(Output::Dial {
            host,
            attempt: id,
            plan,
            early,
        });
        true
    }

    /// The calls this host makes of `host` as it begins following it, to
    /// go with its dial as early data.
    fn first_calls(&self, host: HostId) -> Vec<PeerRequest> {
        self.first_flight()
            .into_iter()
            .find_map(|(planned, calls)| (planned == host).then_some(calls))
            .unwrap_or_default()
    }

    /// Something gave `host` a path it may not have had, or a call wants
    /// it now: a pause after failures ends, the count starts afresh, and
    /// an attempt out that lacks one of its paths now gives way to one on
    /// all of them.
    pub(crate) fn reconsider(&mut self, host: HostId, out: &mut Vec<Output>) {
        self.cut_pause(host, out);
        self.redial_if_stale(host, out);
    }

    /// Dials `host` if nothing holds it back, or gives the attempt out to
    /// it a fresh start on every path when it lacks one it has now.
    fn redial_if_stale(&mut self, host: HostId, out: &mut Vec<Output>) {
        let stale = self
            .routing
            .reach
            .hosts
            .get(&host)
            .and_then(|reaching| reaching.attempt.as_ref())
            .is_some_and(|attempt| {
                self.wanted(host) && self.attempt_plan(host).adds_to(&attempt.plan)
            });
        if stale {
            self.abandon(host, out);
            self.dial(host, out);
            return;
        }
        self.reach_host(host, out);
    }

    /// A call needs a link to `host` now: a pause after failures ends and
    /// the host is dialled, unless an attempt is out already or the link is
    /// expected from the host itself (see [`Reach::expecting`]).
    pub(crate) fn want_host(&mut self, host: HostId, out: &mut Vec<Output>) {
        self.cut_pause(host, out);
        self.reach_host(host, out);
    }

    /// `host` is pairing with this host as the joiner, which dials this one
    /// once the pairing commits: this host holds off its own dial for as
    /// long as an attempt may take, so the two do not cross.
    pub(crate) fn expect_link(&mut self, host: HostId, out: &mut Vec<Output>) {
        let reaching = self.routing.reach.hosts.entry(host).or_default();
        if reaching.attempt.is_some() {
            return;
        }
        reaching.pause = Some(Pause::Expecting);
        let at_ms = self.timers.link_ms.saturating_add(ATTEMPT_DEADLINE_MS);
        self.arm(TimerId::Retry(host), at_ms, out);
    }

    fn cut_pause(&mut self, host: HostId, out: &mut Vec<Output>) {
        let Some(reaching) = self.routing.reach.hosts.get_mut(&host) else {
            return;
        };
        reaching.failures = 0;
        if reaching.pause == Some(Pause::Backoff) {
            reaching.pause = None;
            self.cancel(TimerId::Retry(host), out);
        }
    }

    /// Ends the attempt out to `host` here: the driver drops it.
    fn abandon(&mut self, host: HostId, out: &mut Vec<Output>) {
        let Some(attempt) = self
            .routing
            .reach
            .hosts
            .get_mut(&host)
            .and_then(|reaching| reaching.attempt.take())
        else {
            return;
        };
        self.cancel(TimerId::Attempt(host), out);
        out.push(Output::Abandon {
            host,
            attempt: attempt.id,
        });
    }

    /// The driver says how the attempt `attempt` to `host` ended. One the
    /// core already gave up on, or replaced, says nothing.
    pub(crate) fn attempt_ended(
        &mut self,
        host: HostId,
        attempt: AttemptId,
        outcome: AttemptOutcome,
        out: &mut Vec<Output>,
    ) {
        let wanted = self.wanted(host);
        let reaching = self.routing.reach.hosts.entry(host).or_default();
        if reaching.attempt.as_ref().map(|out| out.id) != Some(attempt) {
            return;
        }
        reaching.attempt = None;
        let outcome = match outcome {
            AttemptOutcome::TurnedAway { path, error } if wanted => {
                // The rest of the paths go again at once, within the
                // deadline.
                reaching.turned_away.insert(path, error.clone());
                self.dial_within(host, Some(error), out);
                return;
            }
            outcome => outcome,
        };
        reaching.turned_away.clear();
        self.cancel(TimerId::Attempt(host), out);
        match outcome {
            AttemptOutcome::Linked { addr } => {
                self.dial_error(host, None);
                if let Some(addr) = addr {
                    self.remember_answered_at(host, addr, out);
                }
                if let Some(reaching) = self.routing.reach.hosts.get_mut(&host) {
                    reaching.failures = 0;
                }
                // Its link went already, or another link displaced it.
                self.reach_host(host, out);
            }
            AttemptOutcome::Failed(error) => self.failed(host, error, out),
            AttemptOutcome::Revoked => {
                self.peer_revoked(host);
                self.failed(host, crate::payload::TRUST_REVOKED.to_owned(), out);
            }
            // Not wanted any more: the run ends as any failure would.
            AttemptOutcome::TurnedAway { error, .. } => self.failed(host, error, out),
        }
    }

    /// The host answered directly at `addr`: the address it is dialled at
    /// from now on beside what discovery finds, unless it is already.
    fn remember_answered_at(&mut self, host: HostId, addr: SocketAddr, out: &mut Vec<Output>) {
        let uuid = Uuid::from_bytes(host.0);
        let known = self
            .trust()
            .entry(uuid)
            .is_some_and(|entry| entry.direct == [addr]);
        if !known {
            self.change_trust(
                TrustChange::DirectAddrs {
                    host: uuid,
                    addrs: vec![addr],
                },
                None,
                out,
            );
        }
    }

    /// An attempt ended without a link: the error is the host's to show,
    /// and a host still wanted is tried again after its pause.
    fn failed(&mut self, host: HostId, error: String, out: &mut Vec<Output>) {
        self.dial_error(host, Some(error));
        if let Some(reaching) = self.routing.reach.hosts.get_mut(&host) {
            reaching.turned_away.clear();
        }
        if !self.wanted(host) {
            return;
        }
        let reaching = self.routing.reach.hosts.entry(host).or_default();
        reaching.failures = reaching.failures.saturating_add(1);
        let pause = RETRY_FIRST_MS
            .saturating_mul(1 << (reaching.failures - 1).min(5))
            .min(RETRY_MAX_MS);
        reaching.pause = Some(Pause::Backoff);
        let at_ms = self.timers.link_ms.saturating_add(pause);
        self.arm(TimerId::Retry(host), at_ms, out);
    }

    /// The attempt to `host` had its time and no link came up.
    pub(crate) fn attempt_due(&mut self, host: HostId, out: &mut Vec<Output>) {
        if !self.timers.armed.remove(&TimerId::Attempt(host)) {
            return;
        }
        self.abandon(host, out);
        let silent = format!(
            "no path answered within {} seconds",
            ATTEMPT_DEADLINE_MS / 1000
        );
        // What turned the run away earlier is the more telling half.
        let turned: Vec<String> = self
            .routing
            .reach
            .hosts
            .get(&host)
            .map(|reaching| reaching.turned_away.values().cloned().collect())
            .unwrap_or_default();
        let error = match turned.is_empty() {
            true => silent,
            false => format!(
                "{}; no other path answered within {} seconds",
                turned.join("; "),
                ATTEMPT_DEADLINE_MS / 1000
            ),
        };
        self.failed(host, error, out);
    }

    /// The pause before `host`'s next attempt is over.
    pub(crate) fn retry_due(&mut self, host: HostId, out: &mut Vec<Output>) {
        if !self.timers.armed.remove(&TimerId::Retry(host)) {
            return;
        }
        if let Some(reaching) = self.routing.reach.hosts.get_mut(&host) {
            reaching.pause = None;
        }
        self.reach_host(host, out);
    }

    /// `host`'s routes moved from `was` to `now`. A link of its own coming
    /// up ends any pause; its last going has it dialled again, at once
    /// after a link that lived, a moment later after one that did not; a
    /// claim coming up is a new path.
    pub(crate) fn reach_routes_moved(
        &mut self,
        host: HostId,
        was: &[Route],
        now: &[Route],
        out: &mut Vec<Output>,
    ) {
        let own_was = was.iter().any(Route::is_direct);
        let own_now = now.iter().any(Route::is_direct);
        let now_ms = self.timers.link_ms;
        if own_now && !own_was {
            let reaching = self.routing.reach.hosts.entry(host).or_default();
            reaching.linked_since_ms = Some(now_ms);
            reaching.failures = 0;
            if reaching.pause.take().is_some() {
                self.cancel(TimerId::Retry(host), out);
            }
            return;
        }
        if own_was && !own_now {
            let wanted = self.wanted(host);
            let reaching = self.routing.reach.hosts.entry(host).or_default();
            let since = reaching.linked_since_ms.take().unwrap_or(0);
            reaching.failures = 0;
            if !wanted {
                return;
            }
            // Asked again, the network finds the host where it is now.
            out.push(Output::Rediscover);
            let resume = since.saturating_add(SHORT_LINK_MS);
            if now_ms < resume && reaching.pause.is_none() && reaching.attempt.is_none() {
                reaching.pause = Some(Pause::ShortLink);
                self.arm(TimerId::Retry(host), resume, out);
                return;
            }
            // What was claimed while the link was up is a path now.
            self.redial_if_stale(host, out);
            return;
        }
        if now
            .iter()
            .any(|route| !route.is_direct() && !was.contains(route))
        {
            self.reconsider(host, out);
        }
    }

    /// The forwarder at the far end of `link` refused to carry this host's
    /// packets to `host`. An attempt whose every path goes through a
    /// forwarder, each of which has refused it, ends now: nothing else
    /// will answer it.
    pub(crate) fn reach_refused(
        &mut self,
        link: LinkId,
        host: HostId,
        reason: ForwardRefusal,
        out: &mut Vec<Output>,
    ) {
        let relays: Vec<LinkId> = self
            .routing
            .links
            .links()
            .filter(|(_, up)| up.kind == CarrierKind::Relay)
            .map(|(id, _)| id)
            .collect();
        let Some(attempt) = self
            .routing
            .reach
            .hosts
            .get_mut(&host)
            .and_then(|reaching| reaching.attempt.as_mut())
        else {
            return;
        };
        attempt.refused.insert(link);
        let plan = &attempt.plan;
        let through = plan.paths.iter().filter_map(|path| match path {
            DialPath::Through(link) => Some(*link),
            DialPath::Direct(_) | DialPath::Ssh(_) => None,
        });
        let relayed = relays.iter().copied().filter(|_| plan.through_relay);
        let mut forwarders = through.chain(relayed).peekable();
        let all_refused = plan.forwarded_only()
            && forwarders.peek().is_some()
            && forwarders.all(|forwarder| attempt.refused.contains(&forwarder));
        if !all_refused {
            return;
        }
        self.abandon(host, out);
        let error = format!("the forwarder refused to carry the link: {reason:?}");
        self.failed(host, error, out);
    }

    /// `host` is no longer reached: its attempt is abandoned and its pause
    /// cancelled.
    pub(crate) fn stop_reaching(&mut self, host: HostId, out: &mut Vec<Output>) {
        self.tell_paths(host, out);
        self.abandon(host, out);
        if let Some(reaching) = self.routing.reach.hosts.remove(&host)
            && reaching.pause.is_some()
        {
            self.cancel(TimerId::Retry(host), out);
        }
    }

    /// The hosts discovery holds changed from `before`: each found at an
    /// address it was not before has a new path, and each host's path
    /// machine hears where it is found now.
    pub(crate) fn reach_found(
        &mut self,
        before: &BTreeMap<HostId, Vec<SocketAddr>>,
        out: &mut Vec<Output>,
    ) {
        let now: Vec<HostId> = self.host_set.found_hosts().collect();
        let moved: BTreeSet<HostId> = before.keys().chain(&now).copied().collect();
        for host in moved {
            self.tell_paths(host, out);
        }
        for host in now {
            let had = before.get(&host).map(Vec::as_slice).unwrap_or_default();
            if self
                .host_set
                .found_at(host)
                .iter()
                .any(|addr| !had.contains(addr))
            {
                self.reconsider(host, out);
            }
        }
    }
}

mod pairing_dial;
mod relay;
#[cfg(test)]
mod tests;

pub use pairing_dial::{PAIRING_DIAL_MS, PAIRING_SHORT_TRY_MS, PairingDial};
pub use relay::{
    RELAY_LIVED_MS, RELAY_RETRY_FIRST_MS, RELAY_RETRY_MAX_MS, RelayCarriers, RelayOutcome,
    RelayReach, TCP_HEAD_START_MS, UDP_BLOCKED_MS,
};
