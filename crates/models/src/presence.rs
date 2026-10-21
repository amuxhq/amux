//! What one host reads as, and the routes this host reaches it by, as links
//! come and go and attempts to reach it conclude.
//!
//! The daemon is the real `daemon_core::Daemon`; the state holds it as the
//! checkpoint it takes of itself, and every transition restores it over a
//! store of this thread's, steps it with the input the driver would feed it
//! and checkpoints it again. Presence and routing never write the store, so
//! one store per thread serves every transition.
//!
//! The world around the daemon is the environment: the target host's own
//! links coming up and going, an adjacent peer and the account's relay
//! starting and ceasing to claim it, the relay refusing to forward to it, the shell making each attempt the core
//! asks for and reporting how it went, the account's plan changing, and the
//! retry timer the core arms firing. Calls open channels of their own on
//! the routes the core lists, which conclude as attempts do, and a call may
//! find no channel within its patience; when the core says to close the
//! target's own links for it, they close. The relay carries a channel only
//! while the plan pays for connections between hosts and refuses it for
//! the plan otherwise; the peer and the target's own links carry one unless
//! a fault says the attempt was refused or timed out. A route gone before
//! its attempt concluded fails as its link closing, and a call whose
//! channel's link closed under it tries the route the core names next.
//!
//! The model starts at launch: the relay's and the peer's links are up and
//! neither has said anything of the target, which this host remembers and
//! dials at once on every path it knows. So the target's own link may come
//! up before any claim of it, whether its packets go on the network or
//! through the relay forwarding them end to end; every other one of its
//! links is carried over SSH. The relay refuses the target's packets for
//! the plan while it lapses, and for no route when it holds no link to the
//! target any more while its claim has not yet gone.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::fmt;

use daemon_core::payload::{
    CarrierKind, ChannelClass, ChannelError, ForwardRefusal, PublicKey, StreamRefusal,
};
use daemon_core::{
    ChannelId, Checkpoint, Daemon, HostId, HostView, Input, LinkId, Output, Presence, Route,
    StartOptions, StepBudget, TimerId, Tried,
};
use store::Sqlite;
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
/// route through a peer is listed before the one through the relay, which
/// is tried last.
const RELAY_LINK: LinkId = LinkId(1);
const PEER_LINK: LinkId = LinkId(2);
/// The target's own links take ids from here, in the order they come up.
const FIRST_DIRECT: u64 = 3;
/// What carries each of the target's own links, in the order they come up,
/// as the core is told it: the host's own connection, whose packets go on
/// the network or through the relay alike, then SSH. Which path carries a
/// link is the path machine's business; to the core a link forwarded end
/// to end through the relay is the target's own, never a relay's.
const KINDS: [CarrierKind; 2] = [CarrierKind::Direct, CarrierKind::Ssh];

/// Presence and routing of one host, within its bounds.
#[derive(Clone, Debug)]
pub struct PresenceRouting {
    /// Links of the target's own and claims of it that come up, all told.
    pub ups: u8,
    /// Links of the target's own and claims of it that go, all told.
    pub downs: u8,
    /// Attempts and channels on a route still listed that are refused
    /// other than for the plan, or time out, calls that find no channel
    /// within their patience, and forwarders refusing the target's packets
    /// for no route, all told.
    pub faults: u8,
    /// Calls that open a channel of their own, all told.
    pub calls: u8,
    /// Times the account's plan changes, all told.
    pub plans: u8,
    /// Refusals for the plan in a row, of a channel or of the target's
    /// packets, with nothing else happening between but the retry timer: the relay refuses for as long as the plan
    /// lapses, so this bound is what lets a path that only retries the
    /// relay end and have its liveness judged.
    pub payments: u8,
    /// The relay may refuse the target's packets for no route, as a fault.
    pub no_route: bool,
}

impl PresenceRouting {
    /// The bounds `just test` explores exhaustively: three links or claims
    /// up and two down, so a route can carry traffic while another comes
    /// and goes; two failed attempts or calls; one call opening a channel;
    /// the plan bought and lapsed again; one refusal for the plan before
    /// something else happens, which keeps the run inside its time budget.
    pub const fn ci() -> Self {
        Self {
            ups: 3,
            downs: 2,
            faults: 2,
            calls: 1,
            plans: 2,
            payments: 1,
            no_route: false,
        }
    }

    /// The larger bounds a nightly run explores, the relay's retry refused
    /// for the plan a second time in a row among them.
    pub const fn nightly() -> Self {
        Self {
            ups: 4,
            downs: 3,
            faults: 3,
            payments: 2,
            no_route: true,
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

    fn route(self) -> Route {
        match self {
            Via::Peer => Route::Via(PEER),
            Via::Relay => Route::Via(RELAY),
        }
    }
}

/// How the shell's attempt on a route went.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Concluded {
    /// A channel opened on the route.
    Ok,
    /// The far end refused the stream other than for the plan.
    Refused,
    /// The relay refused the stream because the account's plan carries no
    /// connection between hosts.
    Payment,
    /// The handshake took too long.
    Timeout,
    /// The route went before the attempt concluded: its link closed under
    /// it.
    Gone,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    /// A link of the target's own comes up.
    DirectUp,
    /// A link of the target's own goes.
    DirectDown(LinkId),
    /// The peer or the relay says it reaches the target.
    ClaimUp(Via),
    /// The peer or the relay says it no longer reaches the target.
    ClaimDown(Via),
    /// The relay refuses to forward the target's packets: for the plan
    /// (`true`), or for no route.
    ForwardRefused(Via, bool),
    /// The shell's attempt on a route the core asked it to try concluded.
    AttemptConcluded(Route, Concluded),
    /// A call opens a channel of its own on the route a fresh call takes
    /// first, and it concludes. One whose direct link goes under it asks
    /// the core for the route to try next and tries it, concluding as that
    /// route allows. A channel concludes through the core as an attempt
    /// does, so the attempts carry the interleavings of the world with a
    /// try in flight and a call's channel concludes at once.
    Call(Concluded),
    /// A call to the target found no channel within its patience.
    CallsFailed,
    /// The account's plan changed: bought, or lapsed.
    PlanChanged,
    /// The target's retry timer fired.
    TimerFired,
    /// The world stops changing: no link or claim comes or goes, the plan
    /// stays, and no attempt fails but by the relay's refusal for the
    /// plan. What the core asked for still happens, so a path ends once
    /// the core rests, and the host's liveness is judged there.
    Rest,
}

/// How much of the bounds a path has used.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
struct Used {
    ups: u8,
    /// The target's own links that came up, which names the next one.
    direct_ups: u8,
    downs: u8,
    faults: u8,
    calls: u8,
    plans: u8,
    /// Refusals for the plan since the world last did anything but fire
    /// the retry timer.
    payments: u8,
}

#[derive(Clone, PartialEq, Eq, Hash)]
pub struct State {
    /// The core, as its own checkpoint.
    core: Vec<u8>,
    /// The target's own links that are up.
    direct: BTreeSet<LinkId>,
    /// The hosts that claim to reach the target.
    claims: BTreeSet<Via>,
    /// The relay has claimed the target since launch.
    relay_claimed: bool,
    /// The routes to the target the core asked the shell to try, whose
    /// attempts have not concluded.
    attempts: BTreeSet<Route>,
    /// The target's retry timer is armed.
    timer: bool,
    /// The account's plan pays for connections between hosts.
    paid: bool,
    /// The world has stopped changing.
    rested: bool,
    used: Used,
    /// The relay refused an attempt on the target for the plan, and since
    /// then no attempt on it opened a channel and none on its preferred
    /// route failed for another reason.
    refused_for_plan: bool,
    /// Calls to the target failed, and since then no channel to it opened
    /// and the relay refused none for the plan.
    calls_failed: bool,
    /// What the core last told about the target.
    view: HostView,
    /// The first time the target's presence changed other than on a
    /// concluded attempt on its preferred route, or on losing every route.
    unprompted: Option<String>,
    /// The first time the target's presence changed while a route that
    /// stayed listed carried its traffic and nothing concluded on it.
    flapped: Option<String>,
    /// The core asked for an attempt already in flight.
    doubled: Option<String>,
    /// The core asked for a dump.
    dumped: Option<String>,
}

impl State {
    /// Counts what a concluded attempt or channel used of the bounds.
    fn count(&mut self, route: Route, concluded: Concluded) {
        self.hear(route, concluded);
        if matches!(concluded, Concluded::Refused | Concluded::Timeout) {
            self.used.faults += 1;
        }
    }

    /// What an attempt on `route` concluding says of the relay's word on
    /// the plan: a channel opening overrides it, and so does a failure for
    /// any other reason on the route the target is preferred on, which is
    /// the newer word on how the target is reached.
    fn hear(&mut self, route: Route, concluded: Concluded) {
        let preferred = self.view.routes.first() == Some(&route);
        match concluded {
            Concluded::Ok => self.refused_for_plan = false,
            Concluded::Payment => self.refused_for_plan = true,
            Concluded::Refused | Concluded::Timeout if preferred => {
                self.refused_for_plan = false;
            }
            Concluded::Refused | Concluded::Timeout | Concluded::Gone => {}
        }
    }

    /// Whether `route` to the target would carry a channel now.
    fn works(&self, route: Route) -> bool {
        match route {
            Route::Direct(link) => self.direct.contains(&link),
            Route::Via(PEER) => self.claims.contains(&Via::Peer),
            Route::Via(RELAY) => self.claims.contains(&Via::Relay) && self.paid,
            Route::Via(_) => false,
        }
    }
}

impl Machine for PresenceRouting {
    type State = State;
    type Action = Action;

    fn init(&self) -> State {
        crate::share_sqlite_across_threads();
        let mut daemon = with_store(|store| {
            Daemon::new(
                store,
                StartOptions {
                    generation: 1,
                    // The model is a function of its actions alone, so no
                    // step may ask for a dump because the machine ran slowly.
                    budget: StepBudget::UNLIMITED,
                    ..StartOptions::default()
                },
            )
        });
        // The relay's link and the peer's are up before anything happens
        // to the target, and a channel to the peer opened.
        let mut out = Vec::new();
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
        daemon.step(
            Input::AttemptConcluded {
                host: PEER,
                route: Route::Direct(PEER_LINK),
                tried: Tried::Opened,
            },
            &mut out,
        );
        assert!(
            !out.iter()
                .any(|output| matches!(output, Output::Dump { .. })),
            "the links come up cleanly: {out:?}"
        );
        let core = put_back(daemon);
        State {
            core,
            direct: BTreeSet::new(),
            claims: BTreeSet::new(),
            relay_claimed: false,
            attempts: BTreeSet::new(),
            timer: false,
            paid: false,
            rested: false,
            used: Used::default(),
            refused_for_plan: false,
            calls_failed: false,
            view: HostView::default(),
            unprompted: None,
            flapped: None,
            doubled: None,
            dumped: None,
        }
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
        }
        for route in &s.attempts {
            for concluded in self.conclusions(s, *route) {
                out.push(Action::AttemptConcluded(*route, concluded));
            }
        }
        if world
            && s.used.calls < self.calls
            && let Some(first) = s.view.routes.first()
        {
            // The core tells a refusal from a timeout only by the relay's
            // refusal for the plan, so a call's channel fails one way.
            for concluded in self.conclusions(s, *first) {
                if concluded != Concluded::Refused {
                    out.push(Action::Call(concluded));
                }
            }
            if first.is_direct() && s.used.downs < self.downs {
                out.push(Action::Call(Concluded::Gone));
            }
        }
        if world && s.used.faults < self.faults && s.view.route().is_some() {
            out.push(Action::CallsFailed);
        }
        // The relay answers packets sent its way: on the route through it
        // while that route carries the host's traffic or is being tried. A
        // daemon forwarding for its paired hosts refuses through the same
        // input and is left to the core's unit tests, which keeps the run
        // inside its budget.
        let relayed = Via::Relay.route();
        if world
            && s.view.routes.contains(&relayed)
            && (s.view.active == Some(relayed) || s.attempts.contains(&relayed))
        {
            if !s.paid && s.used.payments < self.payments {
                out.push(Action::ForwardRefused(Via::Relay, true));
            }
            if self.no_route && s.used.faults < self.faults {
                out.push(Action::ForwardRefused(Via::Relay, false));
            }
        }
        if world && s.used.plans < self.plans {
            out.push(Action::PlanChanged);
        }
        if s.timer {
            out.push(Action::TimerFired);
        }
    }

    fn next(&self, s: &State, action: &Action) -> Option<State> {
        let mut next = s.clone();
        match action {
            Action::AttemptConcluded(_, Concluded::Payment) | Action::ForwardRefused(_, true) => {
                next.used.payments += 1
            }
            Action::TimerFired | Action::Rest => {}
            _ => next.used.payments = 0,
        }
        let input = match *action {
            Action::DirectUp => {
                next.used.ups += 1;
                next.used.direct_ups += 1;
                let link = LinkId(FIRST_DIRECT + u64::from(s.used.direct_ups));
                next.direct.insert(link);
                Input::LinkUp {
                    link,
                    peer: TARGET,
                    kind: KINDS[usize::from(s.used.direct_ups) % KINDS.len()],
                    resumed: None,
                }
            }
            Action::DirectDown(link) => {
                next.used.downs += 1;
                next.direct.remove(&link);
                Input::LinkDown {
                    link,
                    reason: wire::LinkCloseReason::UserShutdown,
                }
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
                Input::LinkControl {
                    link: via.link(),
                    frame: Message {
                        body: Some(Body::NeighborUp(up)),
                    },
                }
            }
            Action::ClaimDown(via) => {
                next.used.downs += 1;
                next.claims.remove(&via);
                let down = NeighborDown {
                    host_id: TARGET.0.to_vec(),
                };
                Input::LinkControl {
                    link: via.link(),
                    frame: Message {
                        body: Some(Body::NeighborDown(down)),
                    },
                }
            }
            Action::AttemptConcluded(route, concluded) => {
                next.attempts.remove(&route);
                next.count(route, concluded);
                let tried = match failure(concluded) {
                    None => Tried::Opened,
                    Some(error) => Tried::Failed(error),
                };
                Input::AttemptConcluded {
                    host: TARGET,
                    route,
                    tried,
                }
            }
            Action::Call(concluded) => {
                next.used.calls += 1;
                let route = *s.view.routes.first().expect("a call takes a listed route");
                let channel = self.want(&mut next, route);
                if concluded == Concluded::Gone {
                    // The link goes under the call, and the driver reports
                    // it before the call's failure.
                    let Route::Direct(link) = route else {
                        unreachable!("only a direct link goes under a call")
                    };
                    next.used.downs += 1;
                    next.direct.remove(&link);
                    let reason = wire::LinkCloseReason::UserShutdown;
                    self.step(&mut next, Input::LinkDown { link, reason });
                    let mut out = self.step(
                        &mut next,
                        Input::RouteFailed {
                            host: TARGET,
                            route,
                            error: ChannelError::LinkClosed,
                        },
                    );
                    let named = out.drain(..).find_map(|output| match output {
                        Output::NextRoute { route, .. } => Some(route),
                        _ => None,
                    });
                    match named {
                        Some(Some(again)) => {
                            let concluded = if next.works(again) {
                                Concluded::Ok
                            } else if again == Route::Via(RELAY)
                                && next.claims.contains(&Via::Relay)
                            {
                                Concluded::Payment
                            } else {
                                Concluded::Timeout
                            };
                            next.hear(again, concluded);
                            let channel = self.want(&mut next, again);
                            self.conclude(&mut next, channel, again, concluded);
                        }
                        Some(None) => {}
                        None => {
                            next.dumped.get_or_insert_with(|| {
                                "the core named no route after a failed one".to_owned()
                            });
                        }
                    }
                } else {
                    next.count(route, concluded);
                    self.conclude(&mut next, channel, route, concluded);
                }
                self.judge(s, &mut next, action);
                return Some(next);
            }
            Action::ForwardRefused(via, for_plan) => {
                let reason = if for_plan {
                    // The relay's word on the plan, as a refused channel's.
                    next.refused_for_plan = true;
                    next.calls_failed = false;
                    ForwardRefusal::PaymentRequired
                } else {
                    next.used.faults += 1;
                    ForwardRefusal::NoRoute
                };
                let refused = wire::ForwardRefused {
                    dst: TARGET.0.to_vec(),
                    reason: reason as i32,
                };
                Input::LinkControl {
                    link: via.link(),
                    frame: Message {
                        body: Some(Body::ForwardRefused(refused)),
                    },
                }
            }
            Action::CallsFailed => {
                next.used.faults += 1;
                next.calls_failed = true;
                Input::CallsFailed {
                    host: TARGET,
                    error: ChannelError::Handshake("no channel opened in time".into()),
                }
            }
            Action::PlanChanged => {
                next.used.plans += 1;
                next.paid = !next.paid;
                Input::PlanChanged
            }
            Action::TimerFired => {
                next.timer = false;
                Input::Timer {
                    id: TimerId::Retry(TARGET),
                }
            }
            Action::Rest => {
                next.rested = true;
                return Some(next);
            }
        };
        self.step(&mut next, input);
        self.judge(s, &mut next, action);
        Some(next)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        vec![
            Property::always(CHANGES_ON_A_CONCLUDED_ATTEMPT, |_, s: &State| {
                s.unprompted.is_none()
            }),
            Property::always(AWAY_ONLY_ON_PAYMENT_REFUSAL, |_, s: &State| {
                away_explained(&s.view, s.refused_for_plan)
            }),
            Property::always(
                "offline after failed calls until a channel opens: once calls to a host have \
                 failed, it reads offline until a channel to it opens or the relay refuses one or \
                 its packets for the account's plan, whatever routes come, go or are listed again \
                 meanwhile",
                |_, s: &State| !s.calls_failed || s.view.presence == Presence::Offline,
            ),
            Property::always(
                "no flap while another route carries traffic: while a route that stays listed \
                 carries a host's traffic, nothing that happens on another route changes how the \
                 host reads",
                |_, s: &State| s.flapped.is_none(),
            ),
            Property::always(ONE_ATTEMPT_IN_FLIGHT, |_, s: &State| s.doubled.is_none()),
            Property::always("the core never asks for a dump", |_, s: &State| {
                s.dumped.is_none()
            }),
            Property::eventually(
                "a host with a working route eventually reads online: once the world stops \
                 changing and the core comes to rest, a host listed on a route that carries a \
                 channel reads online",
                |model, s: &State| {
                    let mut enabled = Vec::new();
                    model.actions(s, &mut enabled);
                    enabled.is_empty()
                        && (s.view.presence == Presence::Online
                            || !s.view.routes.iter().any(|route| s.works(*route)))
                },
            ),
            Property::sometimes("a host is sometimes away", |_, s: &State| {
                s.view.presence == Presence::Away
            }),
            Property::sometimes(
                "a host dialled from memory reads online before the relay says anything of it",
                |_, s: &State| {
                    s.view.presence == Presence::Online
                        && s.view.active.is_some_and(|route| route.is_direct())
                        && !s.relay_claimed
                },
            ),
            Property::sometimes("a host reads online over SSH", |_, s: &State| {
                s.view.presence == Presence::Online
                    && s.view.active == Some(Route::Direct(LinkId(FIRST_DIRECT + 1)))
            }),
            Property::sometimes("a host reads online through the relay", |_, s: &State| {
                s.view.presence == Presence::Online && s.view.active == Some(Route::Via(RELAY))
            }),
            Property::sometimes(
                "a host reads online over a link of its own while another route is listed",
                |_, s: &State| {
                    s.view.presence == Presence::Online
                        && s.view.active.is_some_and(|route| route.is_direct())
                        && s.view.routes.len() > 1
                },
            ),
        ]
    }
}

/// Why an attempt or channel that concluded `concluded` failed, if it did.
fn failure(concluded: Concluded) -> Option<ChannelError> {
    match concluded {
        Concluded::Ok => None,
        Concluded::Refused => Some(ChannelError::Refused(StreamRefusal::NoRoute)),
        Concluded::Payment => Some(ChannelError::Refused(StreamRefusal::PaymentRequired)),
        Concluded::Timeout => Some(ChannelError::Handshake("the handshake timed out".into())),
        Concluded::Gone => Some(ChannelError::LinkClosed),
    }
}

/// The link a channel on `route` rides.
fn link_of(route: Route) -> LinkId {
    match route {
        Route::Direct(link) => link,
        Route::Via(via) if via == RELAY => RELAY_LINK,
        Route::Via(_) => PEER_LINK,
    }
}

impl PresenceRouting {
    /// How an attempt or channel on `route` may conclude now.
    fn conclusions(&self, s: &State, route: Route) -> Vec<Concluded> {
        if !s.view.routes.contains(&route) {
            return vec![Concluded::Gone];
        }
        let mut can = Vec::new();
        if s.works(route) {
            can.push(Concluded::Ok);
        } else if route == Route::Via(RELAY) && s.used.payments < self.payments {
            // The relay refuses for the plan for as long as it lapses:
            // that is the plan, not a fault.
            can.push(Concluded::Payment);
        }
        if !s.rested && s.used.faults < self.faults {
            can.push(Concluded::Refused);
            can.push(Concluded::Timeout);
        }
        can
    }

    /// A call asks the core for a channel on `route`, with the target's
    /// key pinned: the channel it opens, or none when it refuses.
    fn want(&self, next: &mut State, route: Route) -> Option<ChannelId> {
        let input = Input::WantChannel {
            link: link_of(route),
            peer: TARGET,
            class: ChannelClass::Calls,
            pin: Some(PublicKey(vec![7; 32])),
        };
        self.step(next, input)
            .into_iter()
            .find_map(|output| match output {
                Output::OpenChannel { channel, .. } => Some(channel),
                _ => None,
            })
    }

    /// The channel a call opened on `route` concludes.
    fn conclude(
        &self,
        next: &mut State,
        channel: Option<ChannelId>,
        route: Route,
        concluded: Concluded,
    ) {
        let Some(channel) = channel else {
            return;
        };
        let input = match failure(concluded) {
            None => Input::ChannelOpened {
                channel,
                link: link_of(route),
                peer: TARGET,
                class: ChannelClass::Calls,
            },
            Some(reason) => Input::ChannelClosed { channel, reason },
        };
        self.step(next, input);
    }

    /// Steps the core with one input, as the driver does, and carries out
    /// what it asked for of the shell and the timers; what is left is the
    /// caller's.
    fn step(&self, next: &mut State, input: Input) -> Vec<Output> {
        let heard = match &input {
            Input::ChannelOpened { .. }
            | Input::AttemptConcluded {
                tried: Tried::Opened,
                ..
            } => true,
            Input::AttemptConcluded {
                tried: Tried::Failed(error),
                ..
            }
            | Input::ChannelClosed { reason: error, .. } => {
                *error == ChannelError::Refused(StreamRefusal::PaymentRequired)
            }
            _ => false,
        };
        if heard {
            next.calls_failed = false;
        }
        let mut daemon = restore(&next.core);
        let mut out = Vec::new();
        daemon.step(input, &mut out);
        // The target's own links close when the core says so, and go as
        // any link does.
        let closing = out
            .iter()
            .any(|output| *output == Output::CloseLinks { host: TARGET });
        if closing {
            for link in std::mem::take(&mut next.direct) {
                let reason = wire::LinkCloseReason::UserShutdown;
                daemon.step(Input::LinkDown { link, reason }, &mut out);
            }
        }
        next.view = daemon.presence().view(TARGET);
        next.core = put_back(daemon);
        let mut left = Vec::new();
        for output in out {
            match output {
                Output::Attempt { host, route } if host == TARGET => {
                    if !attempt_not_doubled(&next.attempts, route) && next.doubled.is_none() {
                        next.doubled = Some(format!("{route:?}"));
                    }
                    next.attempts.insert(route);
                }
                Output::Attempt { host, route } => {
                    unreachable!("only the target is ever tried again, not {host:?} on {route:?}")
                }
                Output::ArmTimer {
                    id: TimerId::Retry(TARGET),
                    ..
                } => next.timer = true,
                Output::CancelTimer {
                    id: TimerId::Retry(TARGET),
                } => next.timer = false,
                Output::Dump { reason } => {
                    next.dumped.get_or_insert_with(|| format!("{reason:?}"));
                }
                output => left.push(output),
            }
        }
        left
    }

    /// Records the first change of presence the properties forbid.
    fn judge(&self, s: &State, next: &mut State, action: &Action) {
        let (was, now) = (&s.view, &next.view);
        if was.presence == now.presence {
            return;
        }
        let change = format!("{:?} -> {:?} on {action:?}", was.presence, now.presence);
        let concluded = match action {
            Action::AttemptConcluded(route, concluded) if *concluded != Concluded::Gone => {
                Some(*route)
            }
            Action::Call(Concluded::Gone) => now.routes.first().copied(),
            Action::Call(_) => was.routes.first().copied(),
            // A call finding no channel concludes on the route calls took.
            Action::CallsFailed => was.route(),
            // A forwarder's refusal concludes the route through it.
            Action::ForwardRefused(via, _) => Some(via.route()),
            _ => None,
        };
        // A forwarder refusing the host's packets settles how it reads when
        // the route through it carried its traffic, or none did.
        let refused = match action {
            Action::ForwardRefused(via, _) => was.active.is_none_or(|active| active == via.route()),
            _ => false,
        };
        if !refused
            && !change_prompted(now, concluded, *action == Action::CallsFailed)
            && next.unprompted.is_none()
        {
            next.unprompted = Some(change.clone());
        }
        if let Some(carrying) = was.active
            && now.routes.contains(&carrying)
            && concluded != Some(carrying)
            && next.flapped.is_none()
        {
            next.flapped = Some(format!("{change} while {carrying:?} carried traffic"));
        }
    }
}

/// Presence's promises the composed net judges too, each with the
/// predicate both judge it by.
pub(crate) const CHANGES_ON_A_CONCLUDED_ATTEMPT: &str = "presence changes only on a concluded attempt \
     on the preferred route: a host's presence moves only when an attempt or a call's channel concludes \
     on the route carrying its traffic, or else the first listed, as the attempt leaves them, when a call \
     finds no channel on the route calls take, when a forwarder refuses its packets while the route \
     through it carries its traffic or none does, or when no route lists it";
pub(crate) const AWAY_ONLY_ON_PAYMENT_REFUSAL: &str = "Away only on payment refusal: a host reads away \
     only once the relay refused a channel to it or its packets for the account's plan, and since then no \
     channel to it opened and no attempt on its preferred route failed for another reason";
pub(crate) const ONE_ATTEMPT_IN_FLIGHT: &str =
    "at most one attempt the core asked for is in flight per host and route";

/// Whether a host's presence, having moved to read as `now`, moved for a
/// reason: an attempt or a call's channel concluding on `concluded`, the
/// route calls now take; calls finding no channel (`calls_failed`); or no
/// route listing the host.
pub(crate) fn change_prompted(
    now: &HostView,
    concluded: Option<Route>,
    calls_failed: bool,
) -> bool {
    calls_failed
        || now.routes.is_empty()
        || concluded.is_some_and(|route| now.route() == Some(route))
}

/// Whether a host reading as `view` reads away only after the relay
/// refused it for the plan, `refused_for_plan`.
pub(crate) fn away_explained(view: &HostView, refused_for_plan: bool) -> bool {
    view.presence != Presence::Away || refused_for_plan
}

/// Whether the core asking for an attempt on `route` leaves at most one in
/// flight, `in_flight` being the routes its attempts to the host are on.
pub(crate) fn attempt_not_doubled(in_flight: &BTreeSet<Route>, route: Route) -> bool {
    !in_flight.contains(&route)
}

thread_local! {
    /// This thread's store, home between transitions.
    static STORE: RefCell<Option<Sqlite>> = const { RefCell::new(None) };
}

fn with_store<R>(f: impl FnOnce(Sqlite) -> R) -> R {
    STORE.with(|home| {
        let store = home
            .borrow_mut()
            .take()
            .unwrap_or_else(|| Sqlite::open_in_memory(OWN).expect("a store opens"));
        f(store)
    })
}

/// The core restored from its checkpoint over this thread's store.
fn restore(core: &[u8]) -> Daemon {
    let checkpoint = Checkpoint::from_bytes(core).expect("a checkpoint reads");
    with_store(|store| Daemon::restore(checkpoint, store).expect("a checkpoint restores"))
}

/// The core's checkpoint, its store sent home.
fn put_back(daemon: Daemon) -> Vec<u8> {
    let core = daemon.checkpoint().to_bytes();
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
            .field("attempts", &self.attempts)
            .field("direct", &self.direct)
            .field("claims", &self.claims)
            .field("timer", &self.timer)
            .field("paid", &self.paid)
            .field("rested", &self.rested)
            .field("refused_for_plan", &self.refused_for_plan);
        for (name, value) in [
            ("unprompted", &self.unprompted),
            ("flapped", &self.flapped),
            ("doubled", &self.doubled),
            ("dumped", &self.dumped),
        ] {
            if let Some(value) = value {
                debug.field(name, value);
            }
        }
        debug.finish()
    }
}
