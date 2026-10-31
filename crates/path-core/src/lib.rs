//! Which path a link's packets take, as one function of its state and an
//! input.
//!
//! A link between two hosts is one QUIC connection; beneath it the packets
//! can go straight to one of the peer's addresses, through a forwarding
//! link (the cloud relay's or a paired daemon's), or over a byte stream
//! such as SSH. [`Paths`] holds one link's candidates, the probes it has in
//! flight and the path carrying the link, and [`Paths::step`] answers each
//! input with what the driver should do: probe a path, carry the link on
//! one, send on every candidate, forget an address, wake it at a time.
//!
//! The rules:
//!
//! - A path carries the link only once a probe on it was answered. Until
//!   something carries it, the link's packets go on every candidate at
//!   once, so the first flight reaches the peer by whichever path works.
//! - A path that does not touch a forwarder is preferred to one that does,
//!   and among paths of one kind the carrying path stays: the link moves
//!   only to a better kind, and never leaves a working path within
//!   [`HOLD_DOWN_MS`] of starting to carry on it.
//! - While the link is direct, every forwarding path is kept ready: probed
//!   again whenever it has not been heard from for [`PROBE_INTERVAL_MS`],
//!   so falling back costs no round trip of validation.
//! - An address stays a candidate however long it goes unanswered: it is
//!   probed again every [`PROBE_INTERVAL_MS`], so a link that left it for
//!   the relay returns once it answers. Only the peer no longer listing it
//!   forgets it, or a lapse, for an address reached by punching.
//! - An address learned only from the relay's observation of the peer is
//!   reached by punching, which is paid: it is never probed or carried
//!   while the account is free, and a lapse drops it.
//! - A network change makes every path stale; each is probed again at once,
//!   and the link carries on whichever answers first. So does the link
//!   going: the next one carries only on a path that answered for it.
//!
//! The clock is an input like any other: nothing inside a step reads it,
//! locks, spawns or awaits. Every step ends by checking the invariants, and
//! a violation is reported as [`Output::Dump`]. See the principles in
//! `docs/ARCHITECTURE.md`.

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::time::Instant;

pub use daemon_core::payload::{ForwardRefusal, Source};
pub use daemon_core::{Check, DumpReason, LinkId, StepBudget, Violation};

/// How long the link stays on a path that keeps working before it may move
/// to a better one, so two working paths never trade the link back and
/// forth.
pub const HOLD_DOWN_MS: u64 = 3_000;
/// How long a probe waits for its answer before the path counts as failed.
pub const PROBE_TIMEOUT_MS: u64 = 2_000;
/// How long a path may go unheard while the link is up before it is probed
/// again, whether it answered last or not.
pub const PROBE_INTERVAL_MS: u64 = 5_000;

/// A byte-stream path (SSH, a framed TCP connection, an in-process pipe),
/// named by the driver as it registers it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PathId(pub u64);

/// One way the link's packets can travel. Direct and Punched carry a peer
/// address; Forwarded names the forwarding link (the relay's or a daemon's);
/// Framed is a byte-stream path by id.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Path {
    Direct(SocketAddr),
    Punched(SocketAddr),
    Forwarded(LinkId),
    Framed(PathId),
}

impl Path {
    /// Whether the path goes through a forwarder rather than to the peer.
    pub fn is_forwarded(&self) -> bool {
        matches!(self, Path::Forwarded(_))
    }
}

/// Everything the driver can tell the machine; the clock is an input like
/// any other.
#[derive(Clone, Debug, PartialEq)]
pub enum Input {
    Candidate {
        addr: SocketAddr,
        source: Source,
    },
    CandidateGone {
        addr: SocketAddr,
    },
    ProbeAnswered {
        path: Path,
        rtt_ms: u32,
    },
    ProbeTimedOut {
        path: Path,
    },
    /// One of the link's packets came in on `path`; the driver need not
    /// report every one.
    PacketArrived {
        path: Path,
    },
    ForwardingUp {
        link: LinkId,
    },
    ForwardingDown {
        link: LinkId,
    },
    ForwardRefused {
        link: LinkId,
        reason: ForwardRefusal,
    },
    /// The host's own interface addresses changed; every path is suspect.
    NetworkChanged,
    Plan {
        paid: bool,
    },
    LinkUp,
    LinkDown,
    Tick {
        now_ms: u64,
    },
}

/// Everything the machine wants done. SendOnAll lasts until the first Carry.
#[derive(Clone, Debug, PartialEq)]
pub enum Output {
    /// Send a probe on the path; its answer or its timeout comes back.
    Probe {
        path: Path,
    },
    /// Put the link's packets on this path alone.
    Carry {
        path: Path,
    },
    /// Copy the link's packets onto every one of these paths until a Carry.
    SendOnAll {
        paths: Vec<Path>,
    },
    /// Forget the address: a lapse no longer allows punching to it.
    DropCandidate {
        addr: SocketAddr,
    },
    /// Step the machine with a Tick no later than this.
    Arm {
        at_ms: u64,
    },
    Dump {
        reason: DumpReason,
    },
}

/// The invariants [`Paths::check_step_invariants`] asserts, as the
/// sentences a violation names. The path model judges the same sentences.
pub const CARRIES_ONLY_ANSWERED: Check = Check {
    id: "path.carries_only_answered",
    sentence: "a path carries the link only after a probe on it was answered, the first flight's copies aside",
};
pub const NO_CROSS_NETWORK_AFTER_LAPSE: Check = Check {
    id: "path.no_cross_network_after_lapse",
    sentence: "after a lapse no packet goes on a path across networks",
};
pub const NO_SWING: Check = Check {
    id: "path.no_swing",
    sentence: "the link never leaves a working path for another within the hold-down",
};
pub const RELAY_READY_WHILE_DIRECT: Check = Check {
    id: "path.relay_ready_while_direct",
    sentence: "the relay path stays ready while the link is direct",
};

/// Every check the path machine makes at the end of a step.
pub const CHECKS: &[Check] = &[
    CARRIES_ONLY_ANSWERED,
    NO_CROSS_NETWORK_AFTER_LAPSE,
    NO_SWING,
    RELAY_READY_WHILE_DIRECT,
];

/// What a path is, apart from its kind: a punched path and a direct one to
/// the same address are the same path, which becomes direct once the peer
/// lists the address itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Key {
    Addr(SocketAddr),
    Forwarded(LinkId),
    Framed(PathId),
}

impl From<Path> for Key {
    fn from(path: Path) -> Self {
        match path {
            Path::Direct(addr) | Path::Punched(addr) => Key::Addr(addr),
            Path::Forwarded(link) => Key::Forwarded(link),
            Path::Framed(id) => Key::Framed(id),
        }
    }
}

/// What the machine knows of one path.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
struct PathState {
    /// When a probe on it was last answered, or a packet last arrived on
    /// it after one had been.
    heard_ms: Option<u64>,
    /// The round trip its last answered probe measured.
    rtt_ms: Option<u32>,
    /// When the probe in flight times out.
    probe_deadline_ms: Option<u64>,
    /// When the last probe went.
    probed_ms: Option<u64>,
    /// A probe timed out since it was last heard.
    failed: bool,
    /// The network changed since it was last heard.
    stale: bool,
}

impl PathState {
    fn working(&self) -> bool {
        self.heard_ms.is_some() && !self.failed && !self.stale
    }
}

/// The path carrying the link, since when.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Carrying {
    key: Key,
    path: Path,
    since_ms: u64,
}

/// The link moving off a path this step, kept for the invariant check.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Switch {
    from: Key,
    from_working: bool,
    held_ms: u64,
}

/// The machine: pure, cloneable, stepped by the driver;
/// invariants checked after every step as the daemon and relay cores do.
/// Hashed and compared whole, so a checker can hold it as its state.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Paths {
    now_ms: u64,
    paid: bool,
    link_up: bool,
    /// Where each peer address came from.
    sources: BTreeMap<SocketAddr, BTreeSet<Source>>,
    paths: BTreeMap<Key, PathState>,
    /// Forwarding links that refused the link for its plan, unused until
    /// the plan is paid or the forwarding link comes up again.
    refused: BTreeSet<LinkId>,
    carrying: Option<Carrying>,
    /// The paths the last SendOnAll named, while nothing carries.
    on_all: Vec<Path>,
    /// The Tick last asked for.
    armed: Option<u64>,
    switched: Option<Switch>,
    budget: StepBudget,
}

impl Paths {
    /// Its steps are a handful of map operations over a few paths; a
    /// millisecond is far past anything one should take.
    pub const BUDGET: StepBudget = StepBudget { micros: 1_000 };

    /// A machine with no candidates and no link up, on the account's plan.
    pub fn new(now_ms: u64, paid: bool) -> Self {
        Self::with_budget(now_ms, paid, Self::BUDGET)
    }

    /// A machine with another step budget: [`StepBudget::UNLIMITED`] for a
    /// run that must be a function of its inputs alone.
    pub fn with_budget(now_ms: u64, paid: bool, budget: StepBudget) -> Self {
        Self {
            now_ms,
            paid,
            link_up: false,
            sources: BTreeMap::new(),
            paths: BTreeMap::new(),
            refused: BTreeSet::new(),
            carrying: None,
            on_all: Vec::new(),
            armed: None,
            switched: None,
            budget,
        }
    }

    /// One input in, outputs appended. Ends by checking the invariants; a
    /// violation appends [`Output::Dump`], as does a step over its budget.
    pub fn step(&mut self, input: Input, out: &mut Vec<Output>) {
        let started = Instant::now();
        self.switched = None;
        match input {
            Input::Candidate { addr, source } => self.candidate(addr, source, out),
            Input::CandidateGone { addr } => {
                self.sources.remove(&addr);
                self.paths.remove(&Key::Addr(addr));
            }
            Input::ProbeAnswered { path, rtt_ms } => self.answered(path.into(), rtt_ms),
            Input::ProbeTimedOut { path } => self.timed_out(path.into()),
            Input::PacketArrived { path } => self.arrived(path, out),
            Input::ForwardingUp { link } => {
                self.refused.remove(&link);
                self.paths.entry(Key::Forwarded(link)).or_default();
                if self.link_up {
                    self.probe(Key::Forwarded(link), out);
                }
            }
            Input::ForwardingDown { link } => {
                self.refused.remove(&link);
                self.paths.remove(&Key::Forwarded(link));
            }
            Input::ForwardRefused { link, reason } => self.forward_refused(link, reason, out),
            Input::NetworkChanged => self.network_changed(out),
            Input::Plan { paid } => self.plan(paid, out),
            Input::LinkUp => {
                self.link_up = true;
                let keys: Vec<Key> = self.paths.keys().copied().collect();
                for key in keys {
                    if self.usable(key) && !self.is_working(key) {
                        self.probe(key, out);
                    }
                }
            }
            // What answered for the link that went says nothing of the
            // next one, which may come from a peer that restarted or moved:
            // its paths are probed afresh.
            Input::LinkDown => {
                self.link_up = false;
                self.carrying = None;
                for state in self.paths.values_mut() {
                    state.stale = true;
                    state.probe_deadline_ms = None;
                }
            }
            Input::Tick { now_ms } => self.now_ms = self.now_ms.max(now_ms),
        }
        self.expire_probes();
        self.keep_probing(out);
        self.select(out);
        self.arm(out);
        let checked = self.check_step_invariants();
        out.extend(
            daemon_core::finish_step(started, self.budget, 0, checked)
                .map(|reason| Output::Dump { reason }),
        );
    }

    /// The path carrying the link, for display; None while nothing has
    /// answered.
    pub fn carrying(&self) -> Option<&Path> {
        self.carrying.as_ref().map(|carrying| &carrying.path)
    }

    /// The invariants, checked after every step over the few paths held.
    pub fn check_step_invariants(&self) -> Result<(), Violation> {
        if let Some(carrying) = self.carrying {
            let answered = self
                .paths
                .get(&carrying.key)
                .is_some_and(|state| state.heard_ms.is_some());
            if !answered || !self.link_up {
                return Err(Violation {
                    invariant: CARRIES_ONLY_ANSWERED,
                    detail: format!(
                        "{:?} carries the link with no answered probe or no link up",
                        self.path_of(carrying.key)
                    ),
                });
            }
        }
        if !self.paid {
            let carried = self.carrying.map(|carrying| carrying.key);
            let crossing = self
                .paths
                .keys()
                .copied()
                .filter(|key| {
                    self.path_of(*key)
                        .is_some_and(|p| matches!(p, Path::Punched(_)))
                })
                .chain(carried.filter(|key| self.is_refused(*key)))
                .chain(
                    self.on_all
                        .iter()
                        .map(|path| Key::from(*path))
                        .filter(|key| {
                            self.is_refused(*key)
                                || self
                                    .path_of(*key)
                                    .is_none_or(|p| matches!(p, Path::Punched(_)))
                        }),
                )
                .next();
            if let Some(key) = crossing {
                return Err(Violation {
                    invariant: NO_CROSS_NETWORK_AFTER_LAPSE,
                    detail: format!("{key:?} is held or sent on while the plan is free"),
                });
            }
        }
        if let Some(switch) = self.switched
            && switch.from_working
            && self.now_ms.saturating_sub(switch.held_ms) < HOLD_DOWN_MS
        {
            return Err(Violation {
                invariant: NO_SWING,
                detail: format!(
                    "the link left {:?}, still working, {} ms after it began carrying",
                    switch.from,
                    self.now_ms.saturating_sub(switch.held_ms)
                ),
            });
        }
        if self.link_up
            && self
                .carrying
                .is_some_and(|carrying| !self.is_forwarded(carrying.key))
        {
            for (key, state) in &self.paths {
                let ready = state.probe_deadline_ms.is_some()
                    || state
                        .heard_ms
                        .max(state.probed_ms)
                        .is_some_and(|at| self.now_ms < at + PROBE_INTERVAL_MS);
                if matches!(key, Key::Forwarded(_)) && self.usable(*key) && !ready {
                    return Err(Violation {
                        invariant: RELAY_READY_WHILE_DIRECT,
                        detail: format!(
                            "{key:?} has gone unheard and unprobed for the probe interval"
                        ),
                    });
                }
            }
        }
        Ok(())
    }

    fn candidate(&mut self, addr: SocketAddr, source: Source, out: &mut Vec<Output>) {
        if source == Source::Observed && !self.paid {
            return;
        }
        self.sources.entry(addr).or_default().insert(source);
        let key = Key::Addr(addr);
        let known = self.paths.contains_key(&key);
        self.paths.entry(key).or_default();
        if self.link_up && !known {
            self.probe(key, out);
        }
    }

    /// A probe's answer proves the path works only while that probe is
    /// outstanding: an answer to one that timed out, or sent before the
    /// network changed under the path, says nothing about the path now.
    fn answered(&mut self, key: Key, rtt_ms: u32) {
        let now_ms = self.now_ms;
        if let Some(state) = self.paths.get_mut(&key)
            && state.probe_deadline_ms.is_some()
        {
            state.heard_ms = Some(now_ms);
            state.rtt_ms = Some(rtt_ms);
            state.probe_deadline_ms = None;
            state.failed = false;
            state.stale = false;
        }
    }

    /// A probe went unanswered. The path stays, to be probed again after
    /// the interval: an outage of any length ends with the path back.
    fn timed_out(&mut self, key: Key) {
        if let Some(state) = self.paths.get_mut(&key) {
            state.probe_deadline_ms = None;
            state.failed = true;
        }
    }

    /// A packet proves a path that has answered is still alive; on any other
    /// path it is a reason to probe, since an arrival alone validates
    /// nothing.
    fn arrived(&mut self, path: Path, out: &mut Vec<Output>) {
        let key = Key::from(path);
        if matches!(path, Path::Punched(_)) && !self.paid {
            return;
        }
        // A forwarding link the driver never reported up is not a path.
        if matches!(key, Key::Forwarded(_)) && !self.paths.contains_key(&key) {
            return;
        }
        let now_ms = self.now_ms;
        let state = self.paths.entry(key).or_default();
        if state.working() {
            state.heard_ms = Some(now_ms);
            return;
        }
        if state.probe_deadline_ms.is_none() {
            self.probe(key, out);
        }
    }

    fn forward_refused(&mut self, link: LinkId, reason: ForwardRefusal, out: &mut Vec<Output>) {
        let key = Key::Forwarded(link);
        if !self.paths.contains_key(&key) {
            return;
        }
        match reason {
            ForwardRefusal::PaymentRequired => {
                self.refused.insert(link);
                self.plan(false, out);
            }
            ForwardRefusal::NoRoute | ForwardRefusal::RateLimited | ForwardRefusal::Unspecified => {
                if let Some(state) = self.paths.get_mut(&key) {
                    state.failed = true;
                    state.probe_deadline_ms = None;
                }
            }
        }
    }

    fn network_changed(&mut self, out: &mut Vec<Output>) {
        for state in self.paths.values_mut() {
            state.stale = true;
            state.probe_deadline_ms = None;
        }
        if self.link_up {
            let keys: Vec<Key> = self.paths.keys().copied().collect();
            for key in keys {
                if self.usable(key) {
                    self.probe(key, out);
                }
            }
        }
    }

    /// The plan changed. A lapse drops every address only the relay's
    /// observation gave, since reaching it was punching.
    fn plan(&mut self, paid: bool, out: &mut Vec<Output>) {
        let was_paid = self.paid;
        self.paid = paid;
        if paid {
            self.refused.clear();
            if !was_paid && self.link_up {
                let keys: Vec<Key> = self.paths.keys().copied().collect();
                for key in keys {
                    if matches!(key, Key::Forwarded(_)) && !self.is_working(key) {
                        self.probe(key, out);
                    }
                }
            }
            return;
        }
        let punched: Vec<SocketAddr> = self
            .sources
            .iter()
            .filter(|(_, sources)| sources.contains(&Source::Observed))
            .map(|(addr, _)| *addr)
            .collect();
        for addr in punched {
            let sources = self.sources.get_mut(&addr).expect("listed above");
            sources.remove(&Source::Observed);
            if sources.is_empty() {
                self.sources.remove(&addr);
                self.paths.remove(&Key::Addr(addr));
                out.push(Output::DropCandidate { addr });
            }
        }
    }

    /// Times out every probe whose deadline has passed.
    fn expire_probes(&mut self) {
        let expired: Vec<Key> = self
            .paths
            .iter()
            .filter(|(_, state)| state.probe_deadline_ms.is_some_and(|at| at <= self.now_ms))
            .map(|(key, _)| *key)
            .collect();
        for key in expired {
            self.timed_out(key);
        }
    }

    /// While the link is up, probes every usable path not heard from, and
    /// not probed, within the interval.
    fn keep_probing(&mut self, out: &mut Vec<Output>) {
        if !self.link_up {
            return;
        }
        let due: Vec<Key> = self
            .paths
            .iter()
            .filter(|(key, state)| {
                self.usable(**key) && self.probe_due(state).is_some_and(|at| at <= self.now_ms)
            })
            .map(|(key, _)| *key)
            .collect();
        for key in due {
            self.probe(key, out);
        }
    }

    /// When a path next wants a probe, if none is in flight.
    fn probe_due(&self, state: &PathState) -> Option<u64> {
        if state.probe_deadline_ms.is_some() {
            return None;
        }
        let last = state.heard_ms.max(state.probed_ms)?;
        Some(last + PROBE_INTERVAL_MS)
    }

    fn probe(&mut self, key: Key, out: &mut Vec<Output>) {
        let Some(path) = self.path_of(key) else {
            return;
        };
        let now_ms = self.now_ms;
        let state = self.paths.get_mut(&key).expect("a path just named");
        state.probe_deadline_ms = Some(now_ms + PROBE_TIMEOUT_MS);
        state.probed_ms = Some(now_ms);
        out.push(Output::Probe { path });
    }

    /// Chooses the carrying path, or sends on every candidate while none
    /// has answered.
    fn select(&mut self, out: &mut Vec<Output>) {
        if !self.link_up {
            self.carrying = None;
        }
        let best = self.best();
        let next = match self.carrying {
            Some(current) if self.usable(current.key) && self.is_working(current.key) => match best
            {
                Some(best)
                    if self.is_forwarded(current.key)
                        && !self.is_forwarded(best)
                        && self.now_ms >= current.since_ms + HOLD_DOWN_MS =>
                {
                    Some(best)
                }
                _ => Some(current.key),
            },
            _ => best.filter(|_| self.link_up),
        };
        let before = self.carrying;
        if next != before.map(|carrying| carrying.key) {
            if let Some(before) = before {
                self.switched = Some(Switch {
                    from: before.key,
                    from_working: self.usable(before.key) && self.is_working(before.key),
                    held_ms: before.since_ms,
                });
            }
            self.carrying = next.and_then(|key| {
                Some(Carrying {
                    key,
                    path: self.path_of(key)?,
                    since_ms: self.now_ms,
                })
            });
            if let Some(carrying) = self.carrying {
                out.push(Output::Carry {
                    path: carrying.path,
                });
            }
        } else if let Some(carrying) = self.carrying
            && let Some(path) = self.path_of(carrying.key)
            && path != carrying.path
        {
            // The same address, newly listed by the peer, is direct rather
            // than punched: the same path under its new name.
            self.carrying = Some(Carrying { path, ..carrying });
            out.push(Output::Carry { path });
        }
        if self.carrying.is_some() {
            self.on_all.clear();
            return;
        }
        let all: Vec<Path> = self
            .paths
            .keys()
            .filter(|key| self.usable(**key))
            .filter_map(|key| self.path_of(*key))
            .collect();
        if all != self.on_all {
            self.on_all = all.clone();
            out.push(Output::SendOnAll { paths: all });
        }
    }

    /// The best working path: one that skips the forwarders first, then the
    /// quickest, then IPv6.
    fn best(&self) -> Option<Key> {
        self.paths
            .iter()
            .filter(|(key, state)| self.usable(**key) && state.working())
            .min_by_key(|(key, state)| {
                let ipv4 = matches!(key, Key::Addr(addr) if addr.is_ipv4());
                (
                    self.is_forwarded(**key),
                    state.rtt_ms.unwrap_or(u32::MAX),
                    ipv4,
                    **key,
                )
            })
            .map(|(key, _)| *key)
    }

    /// Asks for the next Tick the machine needs, when it changed.
    fn arm(&mut self, out: &mut Vec<Output>) {
        let probes = self
            .paths
            .values()
            .filter_map(|state| state.probe_deadline_ms);
        let due = self
            .paths
            .iter()
            .filter(|(key, _)| self.link_up && self.usable(**key))
            .filter_map(|(_, state)| self.probe_due(state));
        let hold_down = self.carrying.and_then(|current| {
            let better = self.best().is_some_and(|best| !self.is_forwarded(best));
            (self.is_forwarded(current.key) && better).then_some(current.since_ms + HOLD_DOWN_MS)
        });
        let next = probes.chain(due).chain(hold_down).min();
        if next != self.armed {
            self.armed = next;
            if let Some(at_ms) = next {
                out.push(Output::Arm { at_ms });
            }
        }
    }

    /// The path a key names now: punched while only the relay's observation
    /// gave the address.
    fn path_of(&self, key: Key) -> Option<Path> {
        Some(match key {
            Key::Addr(addr) => {
                let observed_only = self.sources.get(&addr).is_some_and(|sources| {
                    !sources.is_empty() && sources.iter().all(|s| *s == Source::Observed)
                });
                if observed_only {
                    Path::Punched(addr)
                } else {
                    Path::Direct(addr)
                }
            }
            Key::Forwarded(link) => Path::Forwarded(link),
            Key::Framed(id) => Path::Framed(id),
        })
    }

    fn usable(&self, key: Key) -> bool {
        match key {
            Key::Addr(_) => self.paid || !matches!(self.path_of(key), Some(Path::Punched(_))),
            Key::Forwarded(link) => !self.refused.contains(&link),
            Key::Framed(_) => true,
        }
    }

    fn is_working(&self, key: Key) -> bool {
        self.paths.get(&key).is_some_and(PathState::working)
    }

    fn is_forwarded(&self, key: Key) -> bool {
        matches!(key, Key::Forwarded(_))
    }

    fn is_refused(&self, key: Key) -> bool {
        matches!(key, Key::Forwarded(link) if self.refused.contains(&link))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RELAY: LinkId = LinkId(7);

    fn addr(text: &str) -> SocketAddr {
        text.parse().unwrap()
    }

    fn lan() -> SocketAddr {
        addr("192.168.1.20:4433")
    }

    fn public() -> SocketAddr {
        addr("81.2.3.4:51820")
    }

    fn relayed() -> Path {
        Path::Forwarded(RELAY)
    }

    /// Steps the machine and fails the test on any dump.
    fn step(paths: &mut Paths, input: Input) -> Vec<Output> {
        let mut out = Vec::new();
        paths.step(input, &mut out);
        if let Some(Output::Dump { reason }) = out.iter().find(|o| matches!(o, Output::Dump { .. }))
        {
            panic!("the machine asked for a dump: {reason}");
        }
        out
    }

    fn probes(out: &[Output]) -> Vec<Path> {
        out.iter()
            .filter_map(|output| match output {
                Output::Probe { path } => Some(*path),
                _ => None,
            })
            .collect()
    }

    fn carry(out: &[Output]) -> Option<Path> {
        out.iter().find_map(|output| match output {
            Output::Carry { path } => Some(*path),
            _ => None,
        })
    }

    fn send_on_all(out: &[Output]) -> Option<Vec<Path>> {
        out.iter().find_map(|output| match output {
            Output::SendOnAll { paths } => Some(paths.clone()),
            _ => None,
        })
    }

    fn tick(paths: &mut Paths, now_ms: u64) -> Vec<Output> {
        step(paths, Input::Tick { now_ms })
    }

    /// A link up with a listed LAN address and the relay, nothing answered.
    fn up(paid: bool) -> Paths {
        let mut paths = Paths::with_budget(0, paid, StepBudget::UNLIMITED);
        step(
            &mut paths,
            Input::Candidate {
                addr: lan(),
                source: Source::Listed,
            },
        );
        step(&mut paths, Input::ForwardingUp { link: RELAY });
        step(&mut paths, Input::LinkUp);
        paths
    }

    fn answer(paths: &mut Paths, path: Path, rtt_ms: u32) -> Vec<Output> {
        step(paths, Input::ProbeAnswered { path, rtt_ms })
    }

    #[test]
    fn the_first_flight_goes_on_every_candidate_until_a_probe_answers() {
        let mut paths = Paths::with_budget(0, true, StepBudget::UNLIMITED);
        let out = step(
            &mut paths,
            Input::Candidate {
                addr: lan(),
                source: Source::Listed,
            },
        );
        assert_eq!(send_on_all(&out), Some(vec![Path::Direct(lan())]));
        assert!(
            probes(&out).is_empty(),
            "nothing is probed before the link is up"
        );
        let out = step(&mut paths, Input::ForwardingUp { link: RELAY });
        assert_eq!(
            send_on_all(&out),
            Some(vec![Path::Direct(lan()), relayed()])
        );

        let out = step(&mut paths, Input::LinkUp);
        assert_eq!(probes(&out), vec![Path::Direct(lan()), relayed()]);
        assert_eq!(carry(&out), None);
        assert_eq!(paths.carrying(), None);

        let out = answer(&mut paths, relayed(), 80);
        assert_eq!(carry(&out), Some(relayed()));
        assert_eq!(send_on_all(&out), None);
        assert_eq!(paths.carrying(), Some(&relayed()));
    }

    #[test]
    fn the_link_never_leaves_a_working_path_for_one_of_the_same_kind() {
        let mut paths = up(true);
        let other = addr("[fd00::20]:4433");
        step(
            &mut paths,
            Input::Candidate {
                addr: other,
                source: Source::Discovery,
            },
        );
        answer(&mut paths, Path::Direct(lan()), 40);
        let out = answer(&mut paths, Path::Direct(other), 2);
        assert_eq!(carry(&out), None);
        for now_ms in (1_000..30_000).step_by(1_000) {
            step(
                &mut paths,
                Input::PacketArrived {
                    path: Path::Direct(lan()),
                },
            );
            step(
                &mut paths,
                Input::ProbeAnswered {
                    path: Path::Direct(other),
                    rtt_ms: 2,
                },
            );
            let out = tick(&mut paths, now_ms);
            assert_eq!(carry(&out), None, "moved at {now_ms} ms");
        }
        assert_eq!(paths.carrying(), Some(&Path::Direct(lan())));
    }

    #[test]
    fn a_path_that_stops_answering_is_left_at_once_hold_down_or_not() {
        let mut paths = up(true);
        answer(&mut paths, relayed(), 80);
        answer(&mut paths, Path::Direct(lan()), 4);
        let out = tick(&mut paths, HOLD_DOWN_MS);
        assert_eq!(carry(&out), Some(Path::Direct(lan())));

        tick(&mut paths, HOLD_DOWN_MS + 100);
        let out = step(
            &mut paths,
            Input::ProbeTimedOut {
                path: Path::Direct(lan()),
            },
        );
        assert_eq!(carry(&out), Some(relayed()), "the relay was kept ready");
    }

    #[test]
    fn a_paid_account_punches_to_an_observed_address() {
        let mut paths = up(true);
        let out = step(
            &mut paths,
            Input::Candidate {
                addr: public(),
                source: Source::Observed,
            },
        );
        assert_eq!(probes(&out), vec![Path::Punched(public())]);
        let out = answer(&mut paths, Path::Punched(public()), 30);
        assert_eq!(carry(&out), Some(Path::Punched(public())));
    }

    #[test]
    fn of_two_addresses_as_near_the_link_chooses_the_ipv6_one() {
        let lan6 = addr("[fd00::20]:4433");
        let mut paths = up(true);
        for addr in [lan6, public()] {
            step(
                &mut paths,
                Input::Candidate {
                    addr,
                    source: Source::Listed,
                },
            );
        }
        let out = answer(&mut paths, Path::Direct(public()), 30);
        assert_eq!(carry(&out), Some(Path::Direct(public())));
        answer(&mut paths, Path::Direct(lan()), 3);
        answer(&mut paths, Path::Direct(lan6), 3);
        assert_eq!(
            paths.carrying(),
            Some(&Path::Direct(public())),
            "the carrying path stays among paths of its kind"
        );

        // The carrying address goes: of the two left, equally near, the
        // IPv6 one takes the link.
        let out = step(&mut paths, Input::CandidateGone { addr: public() });
        assert_eq!(carry(&out), Some(Path::Direct(lan6)), "{out:?}");
    }

    #[test]
    fn a_free_account_uses_discovered_and_listed_addresses_and_byte_streams() {
        let discovered = addr("192.168.1.30:4433");
        let mut paths = Paths::with_budget(0, false, StepBudget::UNLIMITED);
        step(&mut paths, Input::LinkUp);
        let out = step(
            &mut paths,
            Input::Candidate {
                addr: discovered,
                source: Source::Discovery,
            },
        );
        assert_eq!(probes(&out), vec![Path::Direct(discovered)]);
        let out = step(
            &mut paths,
            Input::Candidate {
                addr: lan(),
                source: Source::Listed,
            },
        );
        assert_eq!(probes(&out), vec![Path::Direct(lan())]);
        let out = step(
            &mut paths,
            Input::Candidate {
                addr: public(),
                source: Source::Observed,
            },
        );
        assert!(
            probes(&out).is_empty(),
            "the observed address is not punched"
        );
        let out = answer(&mut paths, Path::Direct(lan()), 4);
        assert_eq!(carry(&out), Some(Path::Direct(lan())));

        let ssh = Path::Framed(PathId(3));
        let out = step(&mut paths, Input::PacketArrived { path: ssh });
        assert_eq!(probes(&out), vec![ssh], "a byte stream is free too");
    }

    #[test]
    fn an_address_the_peer_lists_stays_through_a_lapse() {
        let mut paths = up(true);
        step(
            &mut paths,
            Input::Candidate {
                addr: public(),
                source: Source::Observed,
            },
        );
        let out = step(
            &mut paths,
            Input::Candidate {
                addr: public(),
                source: Source::Listed,
            },
        );
        assert!(probes(&out).is_empty(), "the same path, already probed");
        let out = answer(&mut paths, Path::Direct(public()), 30);
        assert_eq!(carry(&out), Some(Path::Direct(public())));
        let out = step(&mut paths, Input::Plan { paid: false });
        assert!(!out.contains(&Output::DropCandidate { addr: public() }));
        assert_eq!(paths.carrying(), Some(&Path::Direct(public())));
    }

    #[test]
    fn a_punched_path_the_peer_then_lists_is_carried_as_direct() {
        let mut paths = up(true);
        step(
            &mut paths,
            Input::Candidate {
                addr: public(),
                source: Source::Observed,
            },
        );
        answer(&mut paths, Path::Punched(public()), 30);
        let out = step(
            &mut paths,
            Input::Candidate {
                addr: public(),
                source: Source::Listed,
            },
        );
        assert_eq!(carry(&out), Some(Path::Direct(public())));
    }

    #[test]
    fn the_link_returns_to_an_address_that_answers_again_however_long_it_was_silent() {
        let mut paths = up(true);
        answer(&mut paths, Path::Direct(lan()), 4);
        tick(&mut paths, 1_000);
        answer(&mut paths, relayed(), 80);
        assert_eq!(paths.carrying(), Some(&Path::Direct(lan())));

        // The address stops answering for ten minutes; the relay answers.
        let mut lan_probes = 0;
        let mut on_relay_at = None;
        for now_ms in (1_250..600_000).step_by(250) {
            let out = tick(&mut paths, now_ms);
            assert!(
                !out.iter()
                    .any(|o| matches!(o, Output::DropCandidate { .. })),
                "the silent address was dropped at {now_ms} ms"
            );
            if carry(&out) == Some(relayed()) {
                on_relay_at = Some(now_ms);
            }
            lan_probes += probes(&out)
                .iter()
                .filter(|p| **p == Path::Direct(lan()))
                .count();
            if probes(&out).contains(&relayed()) {
                answer(&mut paths, relayed(), 80);
            }
        }
        assert!(
            on_relay_at.is_some(),
            "the link never left the silent address"
        );
        assert_eq!(paths.carrying(), Some(&relayed()));
        assert!(
            lan_probes >= 100,
            "{lan_probes} probes of the silent address in ten minutes"
        );

        // It answers again: the next probe on it moves the link back.
        let mut back_at = None;
        for now_ms in (600_000..620_000).step_by(250) {
            let out = tick(&mut paths, now_ms);
            if probes(&out).contains(&Path::Direct(lan())) {
                let out = answer(&mut paths, Path::Direct(lan()), 4);
                if carry(&out) == Some(Path::Direct(lan())) {
                    back_at = Some(now_ms);
                    break;
                }
            }
            if probes(&out).contains(&relayed()) {
                answer(&mut paths, relayed(), 80);
            }
        }
        assert!(
            back_at.is_some_and(|at| at <= 600_000 + PROBE_INTERVAL_MS),
            "back on the address at {back_at:?}"
        );
    }

    #[test]
    fn a_forward_with_no_route_stops_the_relay_carrying() {
        let mut paths = up(true);
        answer(&mut paths, relayed(), 80);
        let out = step(
            &mut paths,
            Input::ForwardRefused {
                link: RELAY,
                reason: ForwardRefusal::NoRoute,
            },
        );
        assert_eq!(
            send_on_all(&out),
            Some(vec![Path::Direct(lan()), relayed()])
        );
        let out = tick(&mut paths, PROBE_INTERVAL_MS);
        assert!(probes(&out).contains(&relayed()), "and it is tried again");
    }

    #[test]
    fn the_machine_asks_for_the_tick_its_probes_need() {
        let mut paths = Paths::with_budget(0, true, StepBudget::UNLIMITED);
        step(
            &mut paths,
            Input::Candidate {
                addr: lan(),
                source: Source::Listed,
            },
        );
        let out = step(&mut paths, Input::LinkUp);
        assert!(out.contains(&Output::Arm {
            at_ms: PROBE_TIMEOUT_MS
        }));
        let out = answer(&mut paths, Path::Direct(lan()), 4);
        assert!(out.contains(&Output::Arm {
            at_ms: PROBE_INTERVAL_MS
        }));
    }

    fn violation(paths: &Paths) -> Option<Check> {
        paths.check_step_invariants().err().map(|v| v.invariant)
    }

    #[test]
    fn each_invariant_names_its_sentence() {
        let mut paths = up(true);
        paths.carrying = Some(Carrying {
            key: Key::Addr(lan()),
            path: Path::Direct(lan()),
            since_ms: 0,
        });
        assert_eq!(violation(&paths), Some(CARRIES_ONLY_ANSWERED));

        let mut paths = up(false);
        paths.sources.insert(public(), [Source::Observed].into());
        paths
            .paths
            .insert(Key::Addr(public()), PathState::default());
        assert_eq!(violation(&paths), Some(NO_CROSS_NETWORK_AFTER_LAPSE));

        let mut paths = up(true);
        answer(&mut paths, Path::Direct(lan()), 4);
        paths.switched = Some(Switch {
            from: Key::Forwarded(RELAY),
            from_working: true,
            held_ms: 0,
        });
        assert_eq!(violation(&paths), Some(NO_SWING));

        let mut paths = up(true);
        answer(&mut paths, Path::Direct(lan()), 4);
        let relay = paths.paths.get_mut(&Key::Forwarded(RELAY)).unwrap();
        relay.probe_deadline_ms = None;
        relay.probed_ms = None;
        assert_eq!(violation(&paths), Some(RELAY_READY_WHILE_DIRECT));
    }

    #[test]
    fn a_violation_is_a_dump() {
        let mut paths = up(false);
        paths.sources.insert(public(), [Source::Observed].into());
        paths
            .paths
            .insert(Key::Addr(public()), PathState::default());
        let mut out = Vec::new();
        paths.step(Input::Tick { now_ms: 1 }, &mut out);
        assert!(out.iter().any(|output| matches!(
            output,
            Output::Dump { reason: DumpReason::Violation(v) } if v.invariant == NO_CROSS_NETWORK_AFTER_LAPSE
        )));
    }
}
