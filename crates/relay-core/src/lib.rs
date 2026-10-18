//! The relay's admission and splice decisions as one function of its state
//! and an input.
//!
//! The driver verifies a token's signature and decodes a stream's preface
//! before the core sees either; [`Relay::step`] judges the claims, the tier
//! and the adjacency, and answers with a refusal or a splice. The driver
//! copies the bytes of a spliced stream; no byte enters the core. Every
//! step ends by checking the cheap invariants, and a violation is reported
//! as [`Output::Dump`]. See the principles in `docs/ARCHITECTURE.md`.
//!
//! A daemon forwarding a stream between two of its links is a relay with
//! one account, so its driver asks this core too.

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::Instant;

use daemon_core::payload::{StreamPreface, StreamRefusal};
use daemon_core::{DumpReason, HostId, LinkId, StepBudget, Violation};

/// An account, by the id its tokens carry. A daemon's links are all one
/// account's.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct UserId(pub [u8; 16]);

/// One stream a host opened over its link, named by the driver as it
/// accepts it; the driver never names two streams alike.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StreamId(pub u64);

/// What an account's plan lets its hosts do through the relay.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tier {
    /// Hosts reach the relay but carry no streams between each other.
    Free,
    /// Streams between the account's hosts are carried.
    Pro,
}

/// How many streams one link may open within [`RATE_WINDOW_MS`].
pub const RATE_LIMIT: usize = 30;
/// The sliding window [`RATE_LIMIT`] counts over.
pub const RATE_WINDOW_MS: u64 = 60_000;

/// Everything that can happen to the relay, decoded by the driver.
#[derive(Clone, Debug, PartialEq)]
pub enum Input {
    /// A link authenticated: by a token naming its account and plan, or by
    /// a pinned key, which no plan governs (`tier: None`).
    LinkUp {
        link: LinkId,
        user: UserId,
        host: HostId,
        tier: Option<Tier>,
    },
    /// The link is gone. The streams spliced through it end with its
    /// transport; the core forgets them at once.
    LinkDown { link: LinkId },
    /// A host opened a stream over `link` with this preface.
    StreamOpened {
        stream: StreamId,
        link: LinkId,
        preface: StreamPreface,
    },
    /// The driver is done with the stream: its copy ended, or it was reset
    /// after the core refused it.
    StreamEnded { stream: StreamId },
    /// A token-authenticated link presented a fresh token with this plan.
    TierChanged { link: LinkId, tier: Tier },
    /// The clock, as an input: the rate window is measured by it.
    Tick { now_ms: u64 },
}

/// Everything the relay wants done.
#[derive(Clone, Debug, PartialEq)]
pub enum Output {
    /// Refuse the stream before any byte of it is copied. The driver resets
    /// it and then reports it ended.
    Refuse {
        stream: StreamId,
        reason: StreamRefusal,
    },
    /// Copy the stream both ways to a new stream on `to_link`, and report
    /// it ended when the copy ends.
    Splice { stream: StreamId, to_link: LinkId },
    /// Stop a spliced stream now, opened or still opening: the plan of a
    /// link it runs through no longer carries it. The core has forgotten it.
    Cut {
        stream: StreamId,
        reason: StreamRefusal,
    },
    /// Take a dump: the core saw something it must not carry on from.
    Dump { reason: DumpReason },
}

/// The invariants [`Relay::check_step_invariants`] asserts, as the
/// sentences a violation names.
pub const SPLICED_BETWEEN_LIVE_LINKS: &str =
    "a stream is spliced only between two links that are up";
pub const SPLICED_ONCE: &str = "no stream is spliced twice";
pub const REFUSED_NEVER_SPLICED: &str = "a stream is never both refused and spliced";

/// One link that is up.
#[derive(Clone, Debug, PartialEq, Eq)]
struct RelayLink {
    user: UserId,
    host: HostId,
    tier: Option<Tier>,
    /// When each stream it opened within the rate window was opened.
    opened_ms: VecDeque<u64>,
}

/// What the core holds of a stream until the driver reports it ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Held {
    Refused,
    Spliced { from: LinkId, to: LinkId },
}

/// Which links each account's streams may be spliced between: per
/// account, per host, the links that host holds here.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Adjacency {
    hosts: BTreeMap<UserId, BTreeMap<HostId, BTreeSet<LinkId>>>,
}

impl Adjacency {
    fn add(&mut self, user: UserId, host: HostId, link: LinkId) {
        self.hosts
            .entry(user)
            .or_default()
            .entry(host)
            .or_default()
            .insert(link);
    }

    fn remove(&mut self, user: UserId, host: HostId, link: LinkId) {
        let Some(hosts) = self.hosts.get_mut(&user) else {
            return;
        };
        if let Some(links) = hosts.get_mut(&host) {
            links.remove(&link);
            if links.is_empty() {
                hosts.remove(&host);
            }
        }
        if hosts.is_empty() {
            self.hosts.remove(&user);
        }
    }

    /// The link a stream to `host` on `user`'s account is spliced onto: its
    /// newest, since a second link to a host replaces the first.
    fn route(&self, user: UserId, host: HostId) -> Option<LinkId> {
        self.hosts.get(&user)?.get(&host)?.last().copied()
    }
}

/// What one step decided about a stream, against what the core held of
/// it before, for the invariants to judge.
#[derive(Clone, Copy, Debug)]
struct Decided {
    stream: StreamId,
    before: Option<Held>,
    spliced: bool,
}

/// What the current step touched; cleared as each step begins.
#[derive(Debug, Default)]
struct Touched {
    streams: Vec<StreamId>,
    links: Vec<LinkId>,
    decided: Vec<Decided>,
}

/// The relay's whole state: the links up, which hosts of one account may
/// reach each other, the streams decided and not yet ended, and its step
/// budget.
pub struct Relay {
    /// The relay's own host: a stream addressed to it has nothing to reach.
    own: HostId,
    links: BTreeMap<LinkId, RelayLink>,
    adjacency: Adjacency,
    streams: BTreeMap<StreamId, Held>,
    /// The spliced streams through each link, either end.
    through: BTreeMap<LinkId, BTreeSet<StreamId>>,
    now_ms: u64,
    touched: Touched,
    budget: StepBudget,
}

impl Relay {
    /// Tighter than the daemon's: admission must never queue.
    pub const BUDGET: StepBudget = StepBudget { micros: 1_000 };

    /// A relay with no link up, whose own host is `own`.
    pub fn new(own: HostId) -> Self {
        Self::with_budget(own, Self::BUDGET)
    }

    /// A relay with another step budget: [`StepBudget::UNLIMITED`] for a
    /// run that must be a function of its inputs alone.
    pub fn with_budget(own: HostId, budget: StepBudget) -> Self {
        Self {
            own,
            links: BTreeMap::new(),
            adjacency: Adjacency::default(),
            streams: BTreeMap::new(),
            through: BTreeMap::new(),
            now_ms: 0,
            touched: Touched::default(),
            budget,
        }
    }

    /// One input in, outputs appended. Ends by checking the cheap
    /// invariants; a violation appends [`Output::Dump`], as does a step
    /// over its budget.
    pub fn step(&mut self, input: Input, out: &mut Vec<Output>) {
        let started = Instant::now();
        self.touched = Touched::default();
        match input {
            Input::LinkUp {
                link,
                user,
                host,
                tier,
            } => self.link_up(link, user, host, tier),
            Input::LinkDown { link } => self.link_down(link),
            Input::StreamOpened {
                stream,
                link,
                preface,
            } => self.stream_opened(stream, link, &preface, out),
            Input::StreamEnded { stream } => self.stream_ended(stream),
            Input::TierChanged { link, tier } => self.tier_changed(link, tier, out),
            Input::Tick { now_ms } => self.now_ms = self.now_ms.max(now_ms),
        }
        if let Err(violation) = self.check_step_invariants() {
            out.push(Output::Dump {
                reason: DumpReason::Violation(violation),
            });
        }
        let micros = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
        if micros > self.budget.micros {
            out.push(Output::Dump {
                reason: DumpReason::OverBudget {
                    micros,
                    budget: self.budget,
                },
            });
        }
    }

    fn link_up(&mut self, link: LinkId, user: UserId, host: HostId, tier: Option<Tier>) {
        self.link_down(link);
        self.touched.links.push(link);
        self.links.insert(
            link,
            RelayLink {
                user,
                host,
                tier,
                opened_ms: VecDeque::new(),
            },
        );
        self.adjacency.add(user, host, link);
    }

    fn link_down(&mut self, link: LinkId) {
        let Some(gone) = self.links.remove(&link) else {
            return;
        };
        self.touched.links.push(link);
        self.adjacency.remove(gone.user, gone.host, link);
        for stream in self.through.remove(&link).unwrap_or_default() {
            self.forget(stream);
        }
    }

    /// Admission, in the order the cheapest refusal is found: a preface
    /// naming no other host, an origin gone or on the free plan, an origin
    /// opening too fast, then a destination with no link here or on the
    /// free plan.
    fn stream_opened(
        &mut self,
        stream: StreamId,
        origin: LinkId,
        preface: &StreamPreface,
        out: &mut Vec<Output>,
    ) {
        self.touched.streams.push(stream);
        let before = self.streams.get(&stream).copied();
        let verdict = self.admit(origin, preface);
        self.touched.decided.push(Decided {
            stream,
            before,
            spliced: verdict.is_ok(),
        });
        match verdict {
            Ok(to) => {
                self.streams
                    .insert(stream, Held::Spliced { from: origin, to });
                self.through.entry(origin).or_default().insert(stream);
                self.through.entry(to).or_default().insert(stream);
                out.push(Output::Splice {
                    stream,
                    to_link: to,
                });
            }
            Err(reason) => {
                self.streams.insert(stream, Held::Refused);
                out.push(Output::Refuse { stream, reason });
            }
        }
    }

    fn admit(&mut self, origin: LinkId, preface: &StreamPreface) -> Result<LinkId, StreamRefusal> {
        let destination = match HostId::from_slice(&preface.dst) {
            Some(destination) if destination != self.own => destination,
            _ => return Err(StreamRefusal::NotAdjacent),
        };
        let now_ms = self.now_ms;
        let Some(from) = self.links.get_mut(&origin) else {
            return Err(StreamRefusal::NoRoute);
        };
        if from.tier == Some(Tier::Free) {
            return Err(StreamRefusal::PaymentRequired);
        }
        while from
            .opened_ms
            .front()
            .is_some_and(|at| at.saturating_add(RATE_WINDOW_MS) <= now_ms)
        {
            from.opened_ms.pop_front();
        }
        if from.opened_ms.len() >= RATE_LIMIT {
            return Err(StreamRefusal::RateLimited);
        }
        from.opened_ms.push_back(now_ms);
        let user = from.user;
        let Some(to) = self.adjacency.route(user, destination) else {
            return Err(StreamRefusal::NoRoute);
        };
        if self.links.get(&to).and_then(|link| link.tier) == Some(Tier::Free) {
            return Err(StreamRefusal::PaymentRequired);
        }
        Ok(to)
    }

    fn stream_ended(&mut self, stream: StreamId) {
        self.touched.streams.push(stream);
        self.forget(stream);
    }

    /// A lapse to the free plan stops every stream through the link, not
    /// only those it opens next: what it opened while paid for does not
    /// carry on unpaid.
    fn tier_changed(&mut self, link: LinkId, tier: Tier, out: &mut Vec<Output>) {
        let Some(changed) = self.links.get_mut(&link) else {
            return;
        };
        if changed.tier.is_none() {
            return;
        }
        changed.tier = Some(tier);
        self.touched.links.push(link);
        if tier != Tier::Free {
            return;
        }
        for stream in self.through.remove(&link).unwrap_or_default() {
            self.touched.streams.push(stream);
            self.forget(stream);
            out.push(Output::Cut {
                stream,
                reason: StreamRefusal::PaymentRequired,
            });
        }
    }

    /// Drops the stream and its place in the links it runs through.
    fn forget(&mut self, stream: StreamId) {
        if let Some(Held::Spliced { from, to }) = self.streams.remove(&stream) {
            for link in [from, to] {
                if let Some(streams) = self.through.get_mut(&link) {
                    streams.remove(&stream);
                    if streams.is_empty() {
                        self.through.remove(&link);
                    }
                }
            }
        }
    }

    /// The cheap predicates checked after every step, each in constant time
    /// per link or stream the step touched.
    pub fn check_step_invariants(&self) -> Result<(), Violation> {
        for decided in &self.touched.decided {
            match (decided.before, decided.spliced) {
                (Some(Held::Spliced { .. }), true) => {
                    return Err(Violation {
                        invariant: SPLICED_ONCE,
                        detail: format!("stream {} was spliced already", decided.stream.0),
                    });
                }
                (Some(Held::Refused), true) => {
                    return Err(Violation {
                        invariant: REFUSED_NEVER_SPLICED,
                        detail: format!("stream {} was refused, then spliced", decided.stream.0),
                    });
                }
                (Some(Held::Spliced { .. }), false) => {
                    return Err(Violation {
                        invariant: REFUSED_NEVER_SPLICED,
                        detail: format!("stream {} was spliced, then refused", decided.stream.0),
                    });
                }
                (Some(Held::Refused) | None, false) | (None, true) => {}
            }
        }
        for stream in &self.touched.streams {
            if let Some(Held::Spliced { from, to }) = self.streams.get(stream) {
                for link in [from, to] {
                    if !self.links.contains_key(link) {
                        return Err(Violation {
                            invariant: SPLICED_BETWEEN_LIVE_LINKS,
                            detail: format!(
                                "stream {} runs through link {}, which is down",
                                stream.0, link.0
                            ),
                        });
                    }
                }
            }
        }
        for link in &self.touched.links {
            if !self.links.contains_key(link)
                && let Some(stream) = self.through.get(link).and_then(|s| s.first())
            {
                return Err(Violation {
                    invariant: SPLICED_BETWEEN_LIVE_LINKS,
                    detail: format!(
                        "link {} went down with stream {} still spliced through it",
                        link.0, stream.0
                    ),
                });
            }
        }
        Ok(())
    }

    /// How long one step may take before it is reported.
    pub fn budget(&self) -> StepBudget {
        self.budget
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RELAY: HostId = HostId([0xee; 16]);
    const ALICE: UserId = UserId([1; 16]);
    const BOB: UserId = UserId([2; 16]);
    const DESK: HostId = HostId([0xd0; 16]);
    const LAPTOP: HostId = HostId([0x1a; 16]);

    fn relay() -> Relay {
        Relay::with_budget(RELAY, StepBudget::UNLIMITED)
    }

    fn step(relay: &mut Relay, input: Input) -> Vec<Output> {
        let mut out = Vec::new();
        relay.step(input, &mut out);
        out
    }

    fn up(relay: &mut Relay, link: u64, user: UserId, host: HostId, tier: Option<Tier>) {
        assert_eq!(
            step(
                relay,
                Input::LinkUp {
                    link: LinkId(link),
                    user,
                    host,
                    tier,
                },
            ),
            Vec::new()
        );
    }

    fn open(relay: &mut Relay, stream: u64, link: u64, to: HostId) -> Vec<Output> {
        step(
            relay,
            Input::StreamOpened {
                stream: StreamId(stream),
                link: LinkId(link),
                preface: StreamPreface {
                    dst: to.0.to_vec(),
                    plain: false,
                },
            },
        )
    }

    fn refused(stream: u64, reason: StreamRefusal) -> Vec<Output> {
        vec![Output::Refuse {
            stream: StreamId(stream),
            reason,
        }]
    }

    fn spliced(stream: u64, to_link: u64) -> Vec<Output> {
        vec![Output::Splice {
            stream: StreamId(stream),
            to_link: LinkId(to_link),
        }]
    }

    fn violation(out: &[Output]) -> Option<&'static str> {
        out.iter().find_map(|output| match output {
            Output::Dump {
                reason: DumpReason::Violation(violation),
            } => Some(violation.invariant),
            _ => None,
        })
    }

    #[test]
    fn a_tick_asks_for_nothing() {
        let mut relay = relay();
        assert_eq!(step(&mut relay, Input::Tick { now_ms: 5 }), Vec::new());
    }

    #[test]
    fn a_stream_is_spliced_to_the_newest_link_of_its_destination_on_the_same_account() {
        let mut relay = relay();
        up(&mut relay, 1, ALICE, DESK, Some(Tier::Pro));
        up(&mut relay, 2, ALICE, LAPTOP, Some(Tier::Pro));
        up(&mut relay, 3, ALICE, LAPTOP, Some(Tier::Pro));
        assert_eq!(open(&mut relay, 1, 1, LAPTOP), spliced(1, 3));
    }

    #[test]
    fn accounts_never_reach_each_other() {
        let mut relay = relay();
        up(&mut relay, 1, ALICE, DESK, Some(Tier::Pro));
        up(&mut relay, 2, BOB, LAPTOP, Some(Tier::Pro));
        assert_eq!(
            open(&mut relay, 1, 1, LAPTOP),
            refused(1, StreamRefusal::NoRoute)
        );
    }

    #[test]
    fn a_preface_naming_the_relay_or_no_host_is_not_adjacent() {
        let mut relay = relay();
        up(&mut relay, 1, ALICE, DESK, None);
        assert_eq!(
            open(&mut relay, 1, 1, RELAY),
            refused(1, StreamRefusal::NotAdjacent)
        );
        let out = step(
            &mut relay,
            Input::StreamOpened {
                stream: StreamId(2),
                link: LinkId(1),
                preface: StreamPreface {
                    dst: vec![1, 2, 3],
                    plain: false,
                },
            },
        );
        assert_eq!(out, refused(2, StreamRefusal::NotAdjacent));
    }

    #[test]
    fn the_free_plan_carries_no_stream_from_or_to_its_hosts() {
        let mut relay = relay();
        up(&mut relay, 1, ALICE, DESK, Some(Tier::Free));
        up(&mut relay, 2, ALICE, LAPTOP, Some(Tier::Pro));
        assert_eq!(
            open(&mut relay, 1, 1, LAPTOP),
            refused(1, StreamRefusal::PaymentRequired)
        );
        assert_eq!(
            open(&mut relay, 2, 2, DESK),
            refused(2, StreamRefusal::PaymentRequired)
        );
    }

    #[test]
    fn a_link_opening_too_fast_is_refused_until_its_window_moves_on() {
        let mut relay = relay();
        up(&mut relay, 1, ALICE, DESK, None);
        for stream in 0..RATE_LIMIT as u64 {
            assert_eq!(
                open(&mut relay, stream, 1, LAPTOP),
                refused(stream, StreamRefusal::NoRoute)
            );
        }
        assert_eq!(
            open(&mut relay, 100, 1, LAPTOP),
            refused(100, StreamRefusal::RateLimited)
        );
        step(
            &mut relay,
            Input::Tick {
                now_ms: RATE_WINDOW_MS,
            },
        );
        assert_eq!(
            open(&mut relay, 101, 1, LAPTOP),
            refused(101, StreamRefusal::NoRoute)
        );
    }

    #[test]
    fn a_lapse_to_free_cuts_every_stream_through_the_link_and_a_renewal_cuts_none() {
        let mut relay = relay();
        up(&mut relay, 1, ALICE, DESK, Some(Tier::Pro));
        up(&mut relay, 2, ALICE, LAPTOP, Some(Tier::Pro));
        assert_eq!(open(&mut relay, 1, 1, LAPTOP), spliced(1, 2));
        assert_eq!(open(&mut relay, 2, 2, DESK), spliced(2, 1));
        assert_eq!(
            step(
                &mut relay,
                Input::TierChanged {
                    link: LinkId(1),
                    tier: Tier::Pro,
                },
            ),
            Vec::new()
        );
        let out = step(
            &mut relay,
            Input::TierChanged {
                link: LinkId(2),
                tier: Tier::Free,
            },
        );
        assert_eq!(
            out,
            vec![
                Output::Cut {
                    stream: StreamId(1),
                    reason: StreamRefusal::PaymentRequired,
                },
                Output::Cut {
                    stream: StreamId(2),
                    reason: StreamRefusal::PaymentRequired,
                },
            ]
        );
        // The copies the driver stops report their end; nothing is held.
        assert_eq!(
            step(
                &mut relay,
                Input::StreamEnded {
                    stream: StreamId(1)
                }
            ),
            Vec::new()
        );
    }

    #[test]
    fn a_pinned_key_link_has_no_plan_to_lapse() {
        let mut relay = relay();
        up(&mut relay, 1, ALICE, DESK, None);
        up(&mut relay, 2, ALICE, LAPTOP, None);
        assert_eq!(open(&mut relay, 1, 1, LAPTOP), spliced(1, 2));
        assert_eq!(
            step(
                &mut relay,
                Input::TierChanged {
                    link: LinkId(1),
                    tier: Tier::Free,
                },
            ),
            Vec::new()
        );
        assert_eq!(open(&mut relay, 2, 1, LAPTOP), spliced(2, 2));
    }

    #[test]
    fn a_link_going_down_takes_its_streams_with_it_and_its_host_out_of_reach() {
        let mut relay = relay();
        up(&mut relay, 1, ALICE, DESK, None);
        up(&mut relay, 2, ALICE, LAPTOP, None);
        assert_eq!(open(&mut relay, 1, 1, LAPTOP), spliced(1, 2));
        assert_eq!(
            step(&mut relay, Input::LinkDown { link: LinkId(2) }),
            Vec::new()
        );
        assert_eq!(
            open(&mut relay, 2, 1, LAPTOP),
            refused(2, StreamRefusal::NoRoute)
        );
        assert_eq!(
            step(
                &mut relay,
                Input::StreamEnded {
                    stream: StreamId(1)
                }
            ),
            Vec::new()
        );
    }

    #[test]
    fn a_stream_named_twice_and_spliced_twice_asks_for_a_dump() {
        let mut relay = relay();
        up(&mut relay, 1, ALICE, DESK, None);
        up(&mut relay, 2, ALICE, LAPTOP, None);
        assert_eq!(open(&mut relay, 7, 1, LAPTOP), spliced(7, 2));
        let out = open(&mut relay, 7, 1, LAPTOP);
        assert_eq!(violation(&out), Some(SPLICED_ONCE), "{out:?}");
    }

    #[test]
    fn a_refused_stream_named_again_and_spliced_asks_for_a_dump() {
        let mut relay = relay();
        up(&mut relay, 1, ALICE, DESK, None);
        assert_eq!(
            open(&mut relay, 7, 1, LAPTOP),
            refused(7, StreamRefusal::NoRoute)
        );
        up(&mut relay, 2, ALICE, LAPTOP, None);
        let out = open(&mut relay, 7, 1, LAPTOP);
        assert_eq!(violation(&out), Some(REFUSED_NEVER_SPLICED), "{out:?}");

        // Reported ended, the name is free again.
        let mut relay = relay_with_both_up();
        assert_eq!(
            open(&mut relay, 7, 1, RELAY),
            refused(7, StreamRefusal::NotAdjacent)
        );
        step(
            &mut relay,
            Input::StreamEnded {
                stream: StreamId(7),
            },
        );
        assert_eq!(open(&mut relay, 7, 1, LAPTOP), spliced(7, 2));
    }

    fn relay_with_both_up() -> Relay {
        let mut relay = relay();
        up(&mut relay, 1, ALICE, DESK, None);
        up(&mut relay, 2, ALICE, LAPTOP, None);
        relay
    }

    #[test]
    fn a_splice_onto_a_link_that_is_down_asks_for_a_dump() {
        let mut relay = relay();
        up(&mut relay, 1, ALICE, DESK, None);
        // A bug that leaves a link in the account's adjacency after it went.
        relay.adjacency.add(ALICE, LAPTOP, LinkId(9));
        let out = open(&mut relay, 1, 1, LAPTOP);
        assert_eq!(violation(&out), Some(SPLICED_BETWEEN_LIVE_LINKS), "{out:?}");
    }

    #[test]
    fn a_step_over_its_budget_asks_for_a_dump() {
        let mut relay = Relay::with_budget(RELAY, StepBudget { micros: 0 });
        let mut out = Vec::new();
        // Any step takes longer than nothing, given a clock fine enough;
        // spin until one is measured over.
        while out.is_empty() {
            relay.step(Input::Tick { now_ms: 1 }, &mut out);
        }
        assert!(matches!(
            out[..],
            [Output::Dump {
                reason: DumpReason::OverBudget { .. }
            }]
        ));
    }
}
