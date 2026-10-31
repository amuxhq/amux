//! What one host reads as, the routes this host reaches it by, and when
//! this host dials it, as links come and go.
//!
//! The daemon is the real `daemon_core::Daemon`; the state holds it as the
//! checkpoint it takes of itself, and every transition restores it over a
//! store of this thread's, steps it with the input the driver would feed it
//! and checkpoints it again. Presence, routing and dialling never write the
//! store here, so one store per thread serves every transition. Two states
//! are told apart by the core's fingerprint, which renumbers its attempts
//! and leaves out how long its pauses have grown.
//!
//! The world around the daemon is the environment: the target host's own
//! links coming up and going (the target dialling this host, or a call
//! failing on one closing it), an adjacent peer and the account's relay
//! starting and ceasing to claim it, the relay refusing to forward to it
//! for no route, the account's plan changing, the target leaving the
//! address it was last reached at, a call wanting it, and the clock moving
//! on. The core dials the target itself, and the world ends each attempt:
//! with a link of the target's own on a path that reaches it (its address
//! while it is there, the peer while it claims it, the relay while it
//! claims it and the plan pays for connections between hosts), with every
//! forwarder refusing it, with nothing, or with the target refusing this
//! host as revoked; or the attempt's deadline fires first. The relay
//! enforces a change of plan at once, and this host hears of it later.
//!
//! The model starts at launch: the relay's and the peer's links are up and
//! neither has said anything of the target, which this host remembers at
//! an address and as on the account, and dials at once on both.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::net::SocketAddr;

use daemon_core::payload::{CarrierKind, ForwardRefusal};
use daemon_core::trust::TrustStore;
use daemon_core::{
    AttemptOutcome, Daemon, DialPath, Dialling, HostId, HostView, Input, LinkId, Output, Plan,
    Presence, Route, StartOptions, StepBudget, TimerId,
};
use store::Sqlite;
use uuid::Uuid;
use wire::message::Body;
use wire::{Message, NeighborDown, NeighborUp};

use crate::{Machine, Property};

const OWN: [u8; 16] = *b"own-host-0000000";
/// The host whose presence the model follows.
const TARGET: HostId = HostId([1; 16]);
/// An adjacent host that may claim to reach the target.
const PEER: HostId = HostId([2; 16]);
/// The account's relay, which may claim to reach the target.
const RELAY: HostId = HostId([9; 16]);
/// The relay's link is the oldest, as it is on a daemon that started with
/// one; the peer's comes next. Whatever the order the links came up in, a
/// route through a peer is listed before the one through the relay.
const RELAY_LINK: LinkId = LinkId(1);
const PEER_LINK: LinkId = LinkId(2);
/// The target's own links take ids from here, in the order they come up.
const FIRST_DIRECT: u64 = 3;
/// What carries each link of the target's own the target brings up, in
/// turn, as the core is told it: its own connection, then SSH.
const KINDS: [CarrierKind; 2] = [CarrierKind::Direct, CarrierKind::Ssh];
/// How long the clock moves on at a time: past the pause a link must
/// outlive for its loss to be redialled at once.
const TICK_MS: u64 = 2_000;
/// What a failed attempt says.
const NO_ANSWER: &str = "no answer";

fn target_addr() -> SocketAddr {
    "10.0.0.1:7000".parse().unwrap()
}

/// Presence, routing and dialling of one host, within its bounds.
#[derive(Clone, Debug)]
pub struct PresenceRouting {
    /// Links of the target's own the target brings up, and claims of it
    /// that come up, all told.
    pub ups: u8,
    /// Links of the target's own and claims of it that go, all told.
    pub downs: u8,
    /// Attempts that end with nothing, and the relay refusing the target's
    /// packets for no route, all told.
    pub faults: u8,
    /// Times the account's plan changes, all told.
    pub plans: u8,
    /// Attempts every forwarder refused, in a row with nothing else
    /// happening between: the relay refuses for as long as the plan lapses
    /// and the core keeps dialling through it.
    pub payments: u8,
    /// The relay may refuse the target's packets for no route, as a fault.
    pub no_route: bool,
    /// Attempts the target refuses as revoked, all told.
    pub revocations: u8,
    /// Calls wanting the target, all told.
    pub wants: u8,
    /// Times the clock moves on, all told.
    pub ticks: u8,
    /// Retries that fire once the world has stopped changing, so a path on
    /// which nothing reaches the target ends.
    pub rest_retries: u8,
}

impl PresenceRouting {
    /// The bounds `just test` explores exhaustively: two links or claims
    /// up and two down (the target leaving its address among them), so one
    /// route can carry traffic while another comes and goes; one fault;
    /// the plan bought and lapsed again; one attempt refused in a row; one
    /// revocation, one call wanting the target, the clock moving on once,
    /// and one retry once the world rests.
    pub const fn ci() -> Self {
        Self {
            ups: 2,
            downs: 2,
            faults: 1,
            plans: 2,
            payments: 1,
            no_route: true,
            revocations: 1,
            wants: 1,
            ticks: 1,
            rest_retries: 1,
        }
    }

    /// The larger bounds a nightly run explores: a third link or claim up,
    /// a second fault, two refusals in a row and the plan changing a third
    /// time.
    pub const fn nightly() -> Self {
        Self {
            ups: 3,
            faults: 2,
            payments: 2,
            plans: 3,
            ..Self::ci()
        }
    }
}

/// A host that may claim to reach the target.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Via {
    Peer,
    Relay,
}

impl Via {
    fn link(self) -> LinkId {
        match self {
            Via::Peer => PEER_LINK,
            Via::Relay => RELAY_LINK,
        }
    }
}

/// A path an attempt is answered on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Answered {
    /// The target's own address.
    Address,
    /// Through a forwarder claiming it.
    Through(Via),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    /// The target dials this host: a link of its own comes up.
    DirectUp,
    /// A link of the target's own goes: its connection ended, or a call
    /// failing on it closed it.
    DirectDown(LinkId),
    /// The peer or the relay says it reaches the target.
    ClaimUp(Via),
    /// The peer or the relay says it no longer reaches the target.
    ClaimDown(Via),
    /// The relay refuses to forward the target's packets because it holds
    /// no link to it, while its claim lingers.
    NoRoute,
    /// The target leaves the address it was last reached at.
    Leave,
    /// The attempt out is answered on a path, and a link of the target's
    /// own comes up on it.
    Answer(Answered),
    /// A forwarder the attempt goes through, and which does not carry it,
    /// refuses it: the relay for the plan while it claims the target, for
    /// no route otherwise; the peer for no route.
    Refused(Via),
    /// The attempt out ends with nothing.
    Fails,
    /// The target refuses the attempt as revoked.
    Revoked,
    /// The attempt's deadline fires before it ends.
    Deadline,
    /// The pause before the next attempt is over.
    Retry,
    /// A call wants the target now.
    Wanted,
    /// The clock moves on.
    Tick,
    /// A link dialled through a forwarder moves onto a path of its own.
    Carried(LinkId),
    /// The account's plan changed: bought, or lapsed. The relay enforces
    /// it at once; this host hears of it later.
    PlanChanged,
    /// This host hears that the account's plan changed.
    PlanHeard,
    /// The world stops changing: no link or claim comes or goes, the plan
    /// stays, no attempt fails but by a refusal, and nothing reaches the
    /// target that did not. What the core asked for still happens, so a
    /// path ends once the core rests, and the host's liveness is judged
    /// there.
    Rest,
}

/// How much of the bounds a path has used.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
struct Used {
    ups: u8,
    /// The target's own links that came up, which names the next one.
    links: u8,
    /// Those the target brought up, which names the next one's carrier.
    inbound: u8,
    downs: u8,
    faults: u8,
    plans: u8,
    /// Attempts refused since the world last did anything else.
    payments: u8,
    revocations: u8,
    wants: u8,
    ticks: u8,
    rest_retries: u8,
}

/// The rules a step is judged against as it happens, by the sentences of
/// the properties that say them.
const ONE_ATTEMPT: &str = "a host is never in two attempts at once";
const NOT_WHILE_LINKED: &str = "a host with a link of its own up is not dialled";
const NOT_WHILE_REVOKED: &str =
    "a host that refused this one as revoked is not dialled again until that clears";
const NOT_AT_ONCE: &str = "a failed attempt is not followed at once by another: the next waits for \
     its pause, or for something new to dial";
const CALL_DIALS: &str =
    "a call wanting a host no attempt is out to, and no pause holds back, starts one at once";
const LAST_ERROR: &str = "the dial error a client reads is the last failed attempt's";

#[derive(Clone)]
pub struct State {
    /// The core, as its own checkpoint.
    core: Vec<u8>,
    /// The core's fingerprint, which tells states apart in its place.
    print: Vec<u8>,
    /// The target's own links that are up.
    direct: BTreeSet<LinkId>,
    /// Those of them an attempt brought up through a forwarder.
    dialled: BTreeSet<LinkId>,
    /// Those of them that moved onto a path of their own since.
    carried: BTreeSet<LinkId>,
    /// The hosts that claim to reach the target.
    claims: BTreeSet<Via>,
    /// The relay has claimed the target since launch.
    relay_claimed: bool,
    /// The target is still at the address it was last reached at.
    home: bool,
    /// The attempt out, as the core dialled it.
    attempt: Option<Plan>,
    /// The attempt's deadline is armed.
    deadline: bool,
    /// The pause before the next attempt is armed, and whether a link that
    /// lived too short a while armed it.
    pause: Option<bool>,
    /// The account's plan pays for connections between hosts.
    paid: bool,
    /// The plan changed and this host has not heard of it yet.
    untold: bool,
    /// The world has stopped changing.
    rested: bool,
    /// The target refused this host as revoked.
    revoked: bool,
    used: Used,
    /// The relay refused the target for the plan, and since then no link
    /// of its own came up, the relay said it holds no link to it, and some
    /// route listed it throughout.
    refused_for_plan: bool,
    /// What the core last told about the target.
    view: HostView,
    /// The rules a step broke.
    broken: BTreeSet<&'static str>,
    /// The core asked for a dump.
    dumped: Option<String>,
}

impl PartialEq for State {
    fn eq(&self, other: &Self) -> bool {
        self.key() == other.key()
    }
}

impl Eq for State {}

impl Hash for State {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.key().hash(state);
    }
}

impl State {
    /// Everything but the checkpoint, which the fingerprint stands for.
    #[allow(clippy::type_complexity)]
    fn key(
        &self,
    ) -> (
        &[u8],
        (&BTreeSet<LinkId>, &BTreeSet<LinkId>, &BTreeSet<LinkId>),
        (&BTreeSet<Via>, bool, bool),
        (&Option<Plan>, bool, Option<bool>),
        (bool, bool, bool, bool, bool),
        &Used,
        &HostView,
        (&BTreeSet<&'static str>, &Option<String>),
    ) {
        (
            &self.print,
            (&self.direct, &self.dialled, &self.carried),
            (&self.claims, self.relay_claimed, self.home),
            (&self.attempt, self.deadline, self.pause),
            (
                self.paid,
                self.untold,
                self.rested,
                self.revoked,
                self.refused_for_plan,
            ),
            &self.used,
            &self.view,
            (&self.broken, &self.dumped),
        )
    }

    /// The route carrying the target's traffic, as the core last told it.
    pub fn active(&self) -> Option<Route> {
        self.view.active
    }

    /// How the target reads, as the core last told it.
    pub fn presence(&self) -> Presence {
        self.view.presence
    }

    /// Whether an attempt to the target is out.
    pub fn dialling(&self) -> bool {
        self.attempt.is_some()
    }

    /// Whether `via` brings a link of the target's own up.
    fn carries(&self, via: Via) -> bool {
        self.claims.contains(&via) && (via == Via::Peer || self.paid)
    }

    /// The paths the attempt out is answered on.
    fn answers(&self) -> Vec<Answered> {
        let Some(plan) = &self.attempt else {
            return Vec::new();
        };
        let mut answers = Vec::new();
        if self.home && plan.paths.contains(&DialPath::Direct(target_addr())) {
            answers.push(Answered::Address);
        }
        if plan.paths.contains(&DialPath::Through(PEER_LINK)) && self.carries(Via::Peer) {
            answers.push(Answered::Through(Via::Peer));
        }
        if plan.through_relay && self.carries(Via::Relay) {
            answers.push(Answered::Through(Via::Relay));
        }
        answers
    }

    /// The forwarders the attempt out goes through that do not carry it.
    fn refusing(&self) -> Vec<Via> {
        let Some(plan) = &self.attempt else {
            return Vec::new();
        };
        let mut through = Vec::new();
        if plan.paths.contains(&DialPath::Through(PEER_LINK)) && !self.carries(Via::Peer) {
            through.push(Via::Peer);
        }
        if plan.through_relay && !self.carries(Via::Relay) {
            through.push(Via::Relay);
        }
        through
    }

    /// Whether any path the core knows reaches the target now.
    fn reachable(&self) -> bool {
        self.home || self.carries(Via::Peer) || self.carries(Via::Relay)
    }
}

impl Machine for PresenceRouting {
    type State = State;
    type Action = Action;

    fn init(&self) -> State {
        crate::share_sqlite_across_threads();
        let mut daemon = with_store(|store| {
            let mut daemon = Daemon::new(
                store,
                StartOptions {
                    generation: 1,
                    // The model is a function of its actions alone, so no
                    // step may ask for a dump because the machine ran slowly.
                    budget: StepBudget::UNLIMITED,
                    ..StartOptions::default()
                },
            );
            daemon.trust_for_test(trust());
            daemon
        });
        let mut out = Vec::new();
        daemon.step(Input::Started, &mut out);
        // The relay's link and the peer's are up before anything happens
        // to the target.
        for (link, peer, kind) in [
            (RELAY_LINK, RELAY, CarrierKind::Relay),
            (PEER_LINK, PEER, CarrierKind::Direct),
        ] {
            daemon.step(
                Input::LinkUp {
                    link,
                    peer,
                    kind,
                    resumed: None,
                },
                &mut out,
            );
        }
        // Launch: the edge is up and the core dials what it remembers.
        daemon.step(Input::Dialling(Dialling::All), &mut out);
        let view = daemon.presence().view(TARGET);
        let print = daemon.fingerprint().state;
        let core = put_back(daemon);
        let mut state = State {
            core,
            print,
            direct: BTreeSet::new(),
            dialled: BTreeSet::new(),
            carried: BTreeSet::new(),
            claims: BTreeSet::new(),
            relay_claimed: false,
            home: true,
            attempt: None,
            deadline: false,
            pause: None,
            paid: false,
            untold: false,
            rested: false,
            revoked: false,
            used: Used::default(),
            refused_for_plan: false,
            view,
            broken: BTreeSet::new(),
            dumped: None,
        };
        absorb(&mut state, out, Step::Other);
        state
    }

    fn actions(&self, s: &State, out: &mut Vec<Action>) {
        let world = !s.rested;
        if world {
            out.push(Action::Rest);
        }
        if world && s.used.ups < self.ups {
            out.push(Action::DirectUp);
            for via in [Via::Peer, Via::Relay] {
                if !s.claims.contains(&via) {
                    out.push(Action::ClaimUp(via));
                }
            }
        }
        if world && s.used.downs < self.downs {
            out.extend(s.direct.iter().map(|link| Action::DirectDown(*link)));
            out.extend(s.claims.iter().map(|via| Action::ClaimDown(*via)));
            if s.home {
                out.push(Action::Leave);
            }
        }
        if world
            && self.no_route
            && s.used.faults < self.faults
            && s.claims.contains(&Via::Relay)
            && s.direct.is_empty()
        {
            out.push(Action::NoRoute);
        }
        if s.attempt.is_some() {
            let answers = s.answers();
            out.extend(answers.iter().copied().map(Action::Answer));
            if s.used.payments < self.payments {
                out.extend(s.refusing().into_iter().map(Action::Refused));
            }
            if world && s.used.faults < self.faults {
                out.push(Action::Fails);
            }
            if world && s.used.revocations < self.revocations {
                out.push(Action::Revoked);
            }
            // Once the world rests, a path that reaches the target answers
            // before the deadline.
            if s.deadline && (world || answers.is_empty()) {
                out.push(Action::Deadline);
            }
        }
        if s.pause.is_some() && (world || s.used.rest_retries < self.rest_retries) {
            out.push(Action::Retry);
        }
        if world && s.used.wants < self.wants {
            out.push(Action::Wanted);
        }
        if world && s.used.ticks < self.ticks {
            out.push(Action::Tick);
        }
        if world {
            out.extend(
                s.dialled
                    .iter()
                    .filter(|link| !s.carried.contains(link))
                    .map(|link| Action::Carried(*link)),
            );
        }
        if world && s.used.plans < self.plans && !s.untold {
            out.push(Action::PlanChanged);
        }
        if s.untold {
            out.push(Action::PlanHeard);
        }
    }

    fn next(&self, s: &State, action: &Action) -> Option<State> {
        let mut next = s.clone();
        if !matches!(
            action,
            Action::Refused(_) | Action::Retry | Action::Deadline | Action::Rest
        ) {
            next.used.payments = 0;
        }
        let (inputs, step) = match *action {
            Action::DirectUp => {
                next.used.ups += 1;
                let kind = KINDS[usize::from(next.used.inbound) % KINDS.len()];
                next.used.inbound += 1;
                (vec![link_up(&mut next, kind)], Step::Other)
            }
            Action::DirectDown(link) => {
                next.used.downs += 1;
                next.direct.remove(&link);
                next.dialled.remove(&link);
                next.carried.remove(&link);
                let down = Input::LinkDown {
                    link,
                    reason: wire::LinkCloseReason::UserShutdown,
                };
                (vec![down], Step::Lost)
            }
            Action::ClaimUp(via) => {
                next.used.ups += 1;
                next.claims.insert(via);
                next.relay_claimed |= via == Via::Relay;
                let up = NeighborUp {
                    host: Some(wire::Host {
                        host_id: TARGET.0.to_vec(),
                        ..Default::default()
                    }),
                };
                (vec![control(via, Body::NeighborUp(up))], Step::Other)
            }
            Action::ClaimDown(via) => {
                next.used.downs += 1;
                next.claims.remove(&via);
                let down = NeighborDown {
                    host_id: TARGET.0.to_vec(),
                };
                (vec![control(via, Body::NeighborDown(down))], Step::Other)
            }
            Action::NoRoute => {
                next.used.faults += 1;
                next.refused_for_plan = false;
                (
                    vec![refused(Via::Relay, ForwardRefusal::NoRoute)],
                    Step::Other,
                )
            }
            Action::Leave => {
                next.used.downs += 1;
                next.home = false;
                return Some(next);
            }
            Action::Answer(answered) => {
                let (kind, addr) = match answered {
                    Answered::Address => (CarrierKind::Direct, Some(target_addr())),
                    Answered::Through(_) => (CarrierKind::Forwarded, None),
                };
                let up = link_up(&mut next, kind);
                if let (Answered::Through(_), Input::LinkUp { link, .. }) = (answered, &up) {
                    next.dialled.insert(*link);
                }
                let ended = ended(&s.core, AttemptOutcome::Linked { addr });
                (vec![up, ended], Step::Other)
            }
            Action::Refused(via) => {
                next.used.payments += 1;
                let reason = if via == Via::Relay && next.claims.contains(&Via::Relay) {
                    next.refused_for_plan = true;
                    ForwardRefusal::PaymentRequired
                } else {
                    // A refusal by a forwarder that does not claim the
                    // target says nothing of it.
                    if via == Via::Relay && next.claims.contains(&Via::Relay) {
                        next.refused_for_plan = false;
                    }
                    ForwardRefusal::NoRoute
                };
                (vec![refused(via, reason)], Step::Other)
            }
            Action::Fails => {
                next.used.faults += 1;
                let failed = ended(&s.core, AttemptOutcome::Failed(NO_ANSWER.to_owned()));
                (vec![failed], Step::Failed)
            }
            Action::Revoked => {
                next.used.revocations += 1;
                next.revoked = true;
                (vec![ended(&s.core, AttemptOutcome::Revoked)], Step::Revoked)
            }
            Action::Deadline => {
                let fired = Input::Timer {
                    id: TimerId::Attempt(TARGET),
                };
                (vec![fired], Step::Deadline)
            }
            Action::Retry => {
                if s.rested {
                    next.used.rest_retries += 1;
                }
                let fired = Input::Timer {
                    id: TimerId::Retry(TARGET),
                };
                (vec![fired], Step::Other)
            }
            Action::Wanted => {
                next.used.wants += 1;
                (vec![Input::Wanted { host: TARGET }], Step::Wanted)
            }
            Action::Tick => {
                next.used.ticks += 1;
                let now_ms = TICK_MS * u64::from(next.used.ticks);
                (
                    vec![Input::Tick {
                        now_ms,
                        link_ms: now_ms,
                    }],
                    Step::Other,
                )
            }
            Action::Carried(link) => {
                next.carried.insert(link);
                let carried = Input::LinkCarried {
                    link,
                    kind: CarrierKind::Direct,
                };
                (vec![carried], Step::Other)
            }
            Action::PlanChanged => {
                next.used.plans += 1;
                next.paid = !next.paid;
                next.untold = true;
                return Some(next);
            }
            Action::PlanHeard => {
                next.untold = false;
                (vec![Input::PlanChanged], Step::Other)
            }
            Action::Rest => {
                next.rested = true;
                return Some(next);
            }
        };
        let before = next.clone();
        // An attempt the world ended, or whose deadline fired, is over as
        // the core steps it; a pause that ran out is over.
        if matches!(
            action,
            Action::Answer(_) | Action::Fails | Action::Revoked | Action::Deadline
        ) {
            next.attempt = None;
            next.deadline = false;
        }
        if *action == Action::Retry {
            next.pause = None;
        }
        let mut out = Vec::new();
        let mut daemon = restore(&next.core);
        for input in inputs {
            daemon.step(input, &mut out);
        }
        next.view = daemon.presence().view(TARGET);
        let error = daemon.last_dial_error(TARGET).map(str::to_owned);
        next.print = daemon.fingerprint().state;
        next.core = put_back(daemon);
        if step == Step::Failed && error.as_deref() != Some(NO_ANSWER) {
            next.broken.insert(LAST_ERROR);
        }
        absorb(&mut next, out, step);
        if step == Step::Wanted
            && before.direct.is_empty()
            && !before.revoked
            && before.attempt.is_none()
            && before.pause != Some(true)
            && next.attempt.is_none()
        {
            next.broken.insert(CALL_DIALS);
        }
        // A link of the target's own coming up, or the last route going,
        // is the last word on it; a refusal while its own link is up stays
        // the last word once that link goes.
        let came_up = next.direct.iter().any(|link| !s.direct.contains(link));
        if came_up || next.view.routes.is_empty() {
            next.refused_for_plan = false;
        }
        Some(next)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        vec![
            Property::always(PRESENCE_FROM_LINKS, |_, s: &State| {
                reads_by_links(&s.view, s.refused_for_plan)
                    && (s.view.presence == Presence::Online) == !s.direct.is_empty()
            }),
            Property::always(ONE_ATTEMPT, |_, s: &State| !s.broken.contains(ONE_ATTEMPT)),
            Property::always(NOT_WHILE_LINKED, |_, s: &State| {
                !s.broken.contains(NOT_WHILE_LINKED)
            }),
            Property::always(NOT_WHILE_REVOKED, |_, s: &State| {
                !s.broken.contains(NOT_WHILE_REVOKED)
            }),
            Property::always(NOT_AT_ONCE, |_, s: &State| !s.broken.contains(NOT_AT_ONCE)),
            Property::always(CALL_DIALS, |_, s: &State| !s.broken.contains(CALL_DIALS)),
            Property::always(LAST_ERROR, |_, s: &State| !s.broken.contains(LAST_ERROR)),
            Property::always("the core never asks for a dump", |_, s: &State| {
                s.dumped.is_none()
            }),
            Property::eventually(
                "a host some path reaches is eventually linked: once the world stops changing \
                 and the core's attempts come to rest, a host that has not refused this one \
                 and that a path the core knows reaches reads online",
                |model, s: &State| {
                    let mut enabled = Vec::new();
                    model.actions(s, &mut enabled);
                    enabled.is_empty()
                        && (s.view.presence == Presence::Online || s.revoked || !s.reachable())
                },
            ),
            Property::sometimes("a host is sometimes away", |_, s: &State| {
                s.view.presence == Presence::Away
            }),
            Property::sometimes(
                "a host dialled from memory reads online before the relay says anything of it",
                |_, s: &State| s.view.presence == Presence::Online && !s.relay_claimed,
            ),
            Property::sometimes("a host reads online over SSH", |_, s: &State| {
                s.view.presence == Presence::Online
                    && s.view
                        .active
                        .is_some_and(|route| !s.dialled.contains(&route_link(route)))
                    && s.used.inbound >= 2
            }),
            Property::sometimes(
                "a host reads online over a link dialled through a forwarder",
                |_, s: &State| {
                    let dialled =
                        |route| matches!(route, Route::Direct(link) if s.dialled.contains(&link));
                    s.view.presence == Presence::Online && s.view.active.is_some_and(dialled)
                },
            ),
            Property::sometimes(
                "a host linked through a forwarder is later carried on a path of its own \
                 without a new dial",
                |_, s: &State| {
                    s.view.presence == Presence::Online
                        && s.view
                            .active
                            .is_some_and(|route| s.carried.contains(&route_link(route)))
                },
            ),
        ]
    }
}

fn route_link(route: Route) -> LinkId {
    match route {
        Route::Direct(link) => link,
        Route::Via(_) => LinkId(0),
    }
}

/// What kind of input a step was, for the rules judged as it happens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    /// A link of the target's own went.
    Lost,
    Failed,
    Revoked,
    Deadline,
    Wanted,
    Other,
}

/// Carries out what the core asked for of the shell in a step, judging
/// the rules a step must keep.
fn absorb(next: &mut State, out: Vec<Output>, step: Step) {
    for output in out {
        match output {
            Output::Dial { host, plan, .. } => {
                assert_eq!(host, TARGET, "only the target is ever dialled");
                if next.attempt.is_some() {
                    next.broken.insert(ONE_ATTEMPT);
                }
                if !next.direct.is_empty() {
                    next.broken.insert(NOT_WHILE_LINKED);
                }
                if next.revoked {
                    next.broken.insert(NOT_WHILE_REVOKED);
                }
                if matches!(step, Step::Failed | Step::Revoked | Step::Deadline) {
                    next.broken.insert(NOT_AT_ONCE);
                }
                next.attempt = Some(plan);
            }
            Output::Abandon { .. } => next.attempt = None,
            Output::ArmTimer {
                id: TimerId::Attempt(_),
                ..
            } => next.deadline = true,
            Output::CancelTimer {
                id: TimerId::Attempt(_),
            } => next.deadline = false,
            Output::ArmTimer {
                id: TimerId::Retry(_),
                ..
            } => next.pause = Some(step == Step::Lost),
            Output::CancelTimer {
                id: TimerId::Retry(_),
            } => next.pause = None,
            Output::Dump { reason } => {
                next.dumped.get_or_insert_with(|| format!("{reason:?}"));
            }
            _ => {}
        }
    }
}

/// The target trusted, remembered at its address and as on the account.
fn trust() -> TrustStore {
    let mut trust = TrustStore::of_hosts([TARGET]);
    let target = Uuid::from_bytes(TARGET.0);
    let mut entry = trust.entry(target).cloned().expect("the target is trusted");
    entry.direct = vec![target_addr()];
    entry.on_account = true;
    trust.insert_for_test(target, entry);
    trust
}

/// The attempt out to the target, as the core holds it, ending.
fn ended(core: &[u8], outcome: AttemptOutcome) -> Input {
    let attempt =
        restore_then(core, |daemon| daemon.reach().attempt(TARGET)).expect("an attempt is out");
    Input::AttemptEnded {
        host: TARGET,
        attempt,
        outcome,
    }
}

/// A control frame from the host `via`, over its link.
fn control(via: Via, body: Body) -> Input {
    Input::LinkControl {
        link: via.link(),
        frame: Message { body: Some(body) },
    }
}

/// `via` refusing to forward the target's packets, for `reason`.
fn refused(via: Via, reason: ForwardRefusal) -> Input {
    let refused = wire::ForwardRefused {
        dst: TARGET.0.to_vec(),
        reason: reason as i32,
    };
    control(via, Body::ForwardRefused(refused))
}

/// A link of the target's own, carried by `kind`, comes up.
fn link_up(next: &mut State, kind: CarrierKind) -> Input {
    let link = LinkId(FIRST_DIRECT + u64::from(next.used.links));
    next.used.links += 1;
    next.direct.insert(link);
    Input::LinkUp {
        link,
        peer: TARGET,
        kind,
        resumed: None,
    }
}

/// Presence's promise the composed net judges too, with the predicate both
/// judge it by.
pub(crate) const PRESENCE_FROM_LINKS: &str = "presence from links: a host reads online exactly \
     while a link of its own is up, away only while none is and the relay's refusal for the \
     account's plan is the last word on it, and offline otherwise";

/// Whether a host reading as `view` reads as the links listed in it say,
/// `refused_for_plan` being whether the relay's refusal for the plan is
/// the last word on the host.
pub(crate) fn reads_by_links(view: &HostView, refused_for_plan: bool) -> bool {
    let own = view.routes.iter().any(Route::is_direct);
    let away = !own && refused_for_plan && !view.routes.is_empty();
    match view.presence {
        Presence::Online => own,
        Presence::Away => away,
        Presence::Offline => !own && !away,
    }
}

thread_local! {
    /// This thread's store, home between transitions.
    static STORE: RefCell<Option<Sqlite>> = const { RefCell::new(None) };
}

/// This thread's store, which holds the target's trust as the first
/// thread's does.
fn with_store<R>(f: impl FnOnce(Sqlite) -> R) -> R {
    STORE.with(|home| {
        let store = home.borrow_mut().take().unwrap_or_else(|| {
            let mut store = Sqlite::open_in_memory(OWN).expect("a store opens");
            trust().write_for_test(&mut store);
            store
        });
        f(store)
    })
}

/// The core restored from its checkpoint over this thread's store.
fn restore(core: &[u8]) -> Daemon {
    with_store(|store| {
        Daemon::restore(core, store, Default::default()).expect("a checkpoint restores")
    })
}

/// Reads the core restored from its checkpoint, and sends its store home.
fn restore_then<R>(core: &[u8], read: impl FnOnce(&Daemon) -> R) -> R {
    let daemon = restore(core);
    let read = read(&daemon);
    STORE.with(|home| *home.borrow_mut() = Some(daemon.into_store()));
    read
}

/// The core's checkpoint, its store sent home.
fn put_back(daemon: Daemon) -> Vec<u8> {
    let core = daemon.checkpoint();
    let store = daemon.into_store();
    STORE.with(|home| *home.borrow_mut() = Some(store));
    core
}

impl fmt::Debug for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut debug = f.debug_struct("State");
        debug
            .field("presence", &self.view.presence)
            .field("routes", &self.view.routes)
            .field("active", &self.view.active)
            .field("direct", &self.direct)
            .field("claims", &self.claims)
            .field("home", &self.home)
            .field("attempt", &self.attempt)
            .field("deadline", &self.deadline)
            .field("pause", &self.pause)
            .field("paid", &self.paid)
            .field("untold", &self.untold)
            .field("rested", &self.rested)
            .field("revoked", &self.revoked)
            .field("refused_for_plan", &self.refused_for_plan);
        if !self.broken.is_empty() {
            debug.field("broken", &self.broken);
        }
        if let Some(dumped) = &self.dumped {
            debug.field("dumped", dumped);
        }
        debug.finish()
    }
}
