//! What one host reads as, and the routes this host reaches it by, as links
//! come and go.
//!
//! The daemon is the real `daemon_core::Daemon`; the state holds it as the
//! checkpoint it takes of itself, and every transition restores it over a
//! store of this thread's, steps it with the input the driver would feed it
//! and checkpoints it again. Presence and routing never write the store, so
//! one store per thread serves every transition.
//!
//! The world around the daemon is the environment: the target host's own
//! links coming up and going (a call failing on one closes it, which is
//! one going), an adjacent peer and the account's relay starting and
//! ceasing to claim it, the relay refusing to forward to it for no route,
//! the account's plan changing, and the shell dialling the target through
//! a host claiming it whenever the core says to, again after each failed
//! try. A dial through the peer brings up a link of the target's own, and
//! so does one through the relay while the plan pays for connections
//! between hosts; the relay refuses it for the plan otherwise. The relay
//! enforces a change of plan at once, and this host hears of it later.
//!
//! The model starts at launch: the relay's and the peer's links are up and
//! neither has said anything of the target, which this host remembers and
//! dials at once on every path it knows. So the target's own link may come
//! up before any claim of it; its links alternate between its own
//! connection and SSH, and a dialled one is forwarded end to end.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::fmt;

use daemon_core::payload::{CarrierKind, ForwardRefusal};
use daemon_core::{
    Checkpoint, Daemon, HostId, HostView, Input, LinkId, Output, Presence, Route, StartOptions,
    StepBudget,
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
/// route through a peer is listed before the one through the relay.
const RELAY_LINK: LinkId = LinkId(1);
const PEER_LINK: LinkId = LinkId(2);
/// The target's own links take ids from here, in the order they come up.
const FIRST_DIRECT: u64 = 3;
/// What carries each link of the target's own the world brings up, in
/// turn, as the core is told it: the host's own connection, then SSH.
const KINDS: [CarrierKind; 2] = [CarrierKind::Direct, CarrierKind::Ssh];

/// Presence and routing of one host, within its bounds.
#[derive(Clone, Debug)]
pub struct PresenceRouting {
    /// Links of the target's own and claims of it that come up, all told.
    pub ups: u8,
    /// Links of the target's own and claims of it that go, all told.
    pub downs: u8,
    /// Dials through a forwarder that fail other than for the plan, and
    /// the relay refusing the target's packets for no route, all told.
    pub faults: u8,
    /// Times the account's plan changes, all told.
    pub plans: u8,
    /// Refusals for the plan in a row with nothing else happening between:
    /// the relay refuses for as long as the plan lapses and the shell keeps
    /// dialling through it, so this bound is what lets such a path end and
    /// have its liveness judged.
    pub payments: u8,
    /// The relay may refuse the target's packets for no route, as a fault.
    pub no_route: bool,
}

impl PresenceRouting {
    /// The bounds `just test` explores exhaustively: four links or claims
    /// up and three down, so one route can carry traffic while two others
    /// come and go; three failures, among them the relay refusing for no
    /// route; the plan bought and lapsed again; two refusals for the plan
    /// in a row.
    pub const fn ci() -> Self {
        Self {
            ups: 4,
            downs: 3,
            faults: 3,
            plans: 2,
            payments: 2,
            no_route: true,
        }
    }

    /// The larger bounds a nightly run explores: a fifth link or claim up
    /// and a fourth down, and the plan changing a third time.
    pub const fn nightly() -> Self {
        Self {
            ups: 5,
            downs: 4,
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

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    /// A link of the target's own comes up.
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
    /// The shell's dial through the first host claiming the target ends:
    /// with a link of the target's own up, the relay's refusal for the
    /// plan, or, when `fails`, nothing at all; or it finds the target
    /// linked or claimed by none and stops.
    DialTried { fails: bool },
    /// The account's plan changed: bought, or lapsed. The relay enforces
    /// it at once; this host hears of it later.
    PlanChanged,
    /// This host hears that the account's plan changed.
    PlanHeard,
    /// The world stops changing: no link or claim comes or goes, the plan
    /// stays, and no dial fails but by the relay's refusal for the plan.
    /// What the core asked for still happens, so a path ends once the
    /// core rests, and the host's liveness is judged there.
    Rest,
}

/// How much of the bounds a path has used.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
struct Used {
    ups: u8,
    /// The target's own links that came up, which names the next one.
    links: u8,
    downs: u8,
    faults: u8,
    plans: u8,
    /// Refusals for the plan since the world last did anything else.
    payments: u8,
}

#[derive(Clone, PartialEq, Eq, Hash)]
pub struct State {
    /// The core, as its own checkpoint.
    core: Vec<u8>,
    /// The target's own links that are up.
    direct: BTreeSet<LinkId>,
    /// Those of them a dial through a forwarder brought up.
    dialled: BTreeSet<LinkId>,
    /// The hosts that claim to reach the target.
    claims: BTreeSet<Via>,
    /// The relay has claimed the target since launch.
    relay_claimed: bool,
    /// The shell is dialling the target through a forwarder, as the core
    /// last said to.
    dialling: bool,
    /// The account's plan pays for connections between hosts.
    paid: bool,
    /// The plan changed and this host has not heard of it yet.
    untold: bool,
    /// The world has stopped changing.
    rested: bool,
    used: Used,
    /// The relay refused the target for the plan, and since then no link
    /// of its own came up, the relay said it holds no link to it, and some
    /// route listed it throughout.
    refused_for_plan: bool,
    /// What the core last told about the target.
    view: HostView,
    /// The core asked for a dump.
    dumped: Option<String>,
}

impl State {
    /// The route carrying the target's traffic, as the core last told it.
    pub fn active(&self) -> Option<Route> {
        self.view.active
    }

    /// How the target reads, as the core last told it.
    pub fn presence(&self) -> Presence {
        self.view.presence
    }

    /// Whether the shell is dialling the target through a forwarder.
    pub fn dialling(&self) -> bool {
        self.dialling
    }

    /// The forwarder a dial takes: the first host the core lists as
    /// claiming the target.
    fn forwarder(&self) -> Option<Via> {
        self.view.routes.iter().find_map(|route| match route {
            Route::Via(PEER) => Some(Via::Peer),
            Route::Via(RELAY) => Some(Via::Relay),
            _ => None,
        })
    }

    /// Whether a dial through `via` brings up a link of the target's own.
    fn carries(&self, via: Via) -> bool {
        self.claims.contains(&via) && (via == Via::Peer || self.paid)
    }

    /// Whether the shell's dial is over: the target has a link of its own,
    /// or no host claims it.
    fn dial_over(&self) -> bool {
        !self.direct.is_empty() || self.forwarder().is_none()
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
        // to the target.
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
        assert!(
            !out.iter()
                .any(|output| matches!(output, Output::Dump { .. })),
            "the links come up cleanly: {out:?}"
        );
        let core = put_back(daemon);
        State {
            core,
            direct: BTreeSet::new(),
            dialled: BTreeSet::new(),
            claims: BTreeSet::new(),
            relay_claimed: false,
            dialling: false,
            paid: false,
            untold: false,
            rested: false,
            used: Used::default(),
            refused_for_plan: false,
            view: HostView::default(),
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
        if world
            && self.no_route
            && s.used.faults < self.faults
            && s.claims.contains(&Via::Relay)
            && s.direct.is_empty()
        {
            out.push(Action::NoRoute);
        }
        if s.dialling {
            // The relay refuses for the plan for as long as it lapses: that
            // is the plan, not a fault, but each refusal in a row counts.
            let refused =
                !s.dial_over() && s.forwarder() == Some(Via::Relay) && !s.carries(Via::Relay);
            if !refused || s.used.payments < self.payments {
                out.push(Action::DialTried { fails: false });
            }
            if world && !s.dial_over() && s.used.faults < self.faults {
                out.push(Action::DialTried { fails: true });
            }
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
        if !matches!(action, Action::DialTried { fails: false } | Action::Rest) {
            next.used.payments = 0;
        }
        let input = match *action {
            Action::DirectUp => {
                next.used.ups += 1;
                let kind = KINDS[usize::from(s.used.links) % KINDS.len()];
                self.link_up(&mut next, kind)
            }
            Action::DirectDown(link) => {
                next.used.downs += 1;
                next.direct.remove(&link);
                next.dialled.remove(&link);
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
                control(via, Body::NeighborUp(up))
            }
            Action::ClaimDown(via) => {
                next.used.downs += 1;
                next.claims.remove(&via);
                let down = NeighborDown {
                    host_id: TARGET.0.to_vec(),
                };
                control(via, Body::NeighborDown(down))
            }
            Action::NoRoute => {
                next.used.faults += 1;
                next.refused_for_plan = false;
                refused(ForwardRefusal::NoRoute)
            }
            Action::DialTried { fails } => {
                let Some(via) = s.forwarder().filter(|_| !s.dial_over()) else {
                    next.dialling = false;
                    return Some(next);
                };
                if fails {
                    next.used.faults += 1;
                    return Some(next);
                }
                if s.carries(via) {
                    next.dialling = false;
                    let input = self.link_up(&mut next, CarrierKind::Forwarded);
                    if let Input::LinkUp { link, .. } = input {
                        next.dialled.insert(link);
                    }
                    input
                } else {
                    // Only the relay claims a host it cannot carry: it
                    // refuses for the plan.
                    next.used.payments += 1;
                    next.refused_for_plan = true;
                    refused(ForwardRefusal::PaymentRequired)
                }
            }
            Action::PlanChanged => {
                next.used.plans += 1;
                next.paid = !next.paid;
                next.untold = true;
                return Some(next);
            }
            Action::PlanHeard => {
                next.untold = false;
                Input::PlanChanged
            }
            Action::Rest => {
                next.rested = true;
                return Some(next);
            }
        };
        self.step(&mut next, input);
        if !next.direct.is_empty() || next.view.routes.is_empty() {
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
            Property::always(
                "a claimed host is dialled: while no link of a host's own is up and a host \
                 claims to reach it, the shell is dialling it through a forwarder",
                |_, s: &State| !s.direct.is_empty() || s.claims.is_empty() || s.dialling,
            ),
            Property::always("the core never asks for a dump", |_, s: &State| {
                s.dumped.is_none()
            }),
            Property::eventually(
                "a host with a working route eventually reads online: once the world stops \
                 changing and the shell's dials come to rest, a host that a forwarder carries \
                 reads online",
                |model, s: &State| {
                    let mut enabled = Vec::new();
                    model.actions(s, &mut enabled);
                    enabled.is_empty()
                        && (s.view.presence == Presence::Online
                            || ![Via::Peer, Via::Relay]
                                .into_iter()
                                .any(|via| s.carries(via)))
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
                    && s.view.active == Some(Route::Direct(LinkId(FIRST_DIRECT + 1)))
            }),
            Property::sometimes(
                "a host reads online over a link dialled through a forwarder",
                |_, s: &State| {
                    let dialled =
                        |route| matches!(route, Route::Direct(link) if s.dialled.contains(&link));
                    s.view.presence == Presence::Online && s.view.active.is_some_and(dialled)
                },
            ),
        ]
    }
}

/// A control frame from the host `via`, over its link.
fn control(via: Via, body: Body) -> Input {
    Input::LinkControl {
        link: via.link(),
        frame: Message { body: Some(body) },
    }
}

/// The relay refusing to forward the target's packets, for `reason`.
fn refused(reason: ForwardRefusal) -> Input {
    let refused = wire::ForwardRefused {
        dst: TARGET.0.to_vec(),
        reason: reason as i32,
    };
    control(Via::Relay, Body::ForwardRefused(refused))
}

impl PresenceRouting {
    /// A link of the target's own, carried by `kind`, comes up.
    fn link_up(&self, next: &mut State, kind: CarrierKind) -> Input {
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

    /// Steps the core with one input, as the driver does, and carries out
    /// what it asked for of the shell.
    fn step(&self, next: &mut State, input: Input) {
        let mut daemon = restore(&next.core);
        let mut out = Vec::new();
        daemon.step(input, &mut out);
        next.view = daemon.presence().view(TARGET);
        next.core = put_back(daemon);
        for output in out {
            match output {
                Output::DialThrough { host } if host == TARGET => next.dialling = true,
                Output::DialThrough { host } => {
                    unreachable!("only the target is ever dialled through, not {host:?}")
                }
                Output::Dump { reason } => {
                    next.dumped.get_or_insert_with(|| format!("{reason:?}"));
                }
                _ => {}
            }
        }
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
            .field("direct", &self.direct)
            .field("claims", &self.claims)
            .field("dialling", &self.dialling)
            .field("paid", &self.paid)
            .field("untold", &self.untold)
            .field("rested", &self.rested)
            .field("refused_for_plan", &self.refused_for_plan);
        if let Some(dumped) = &self.dumped {
            debug.field("dumped", dumped);
        }
        debug.finish()
    }
}
