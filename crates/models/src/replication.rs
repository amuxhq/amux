//! Replication of one agent from its origin to one replica, over a link
//! that may lose what it carries, against an origin that commits, trims
//! its history and reboots uncleanly.
//!
//! Both hosts are the real `daemon_core::Daemon` over the real `Sqlite`, in
//! memory; the state holds each core as its checkpoint and each store as
//! its dump. The replica's core follows the origin's inventory and keeps a
//! source open on the agent: it decides when to open a stream and what to
//! ask for, absorbs what the stream carries, and asks the origin for a page
//! below its block when a client fetches there. The origin's core answers
//! the opening, every row committed after it and the page. What the
//! drivers do between the two (carrying each answer to the other core in
//! order, a page's answer arriving after whatever the stream delivers
//! meanwhile, a stream dying at a message, telling the origin a stream was
//! dropped, firing the timer the replica armed) and what the world does
//! (the agent's commits, which reach the origin's core as the store's own
//! records; retention trimming the origin's oldest row; the origin
//! rebooting uncleanly) are the environment here. The inventory stream is
//! opened once and never lost: following a host is the family model's.
//!
//! The replica's link to the origin is a resumed one, and the opening the
//! replica asks for after the origin reboots rides it as early data: the
//! origin's driver holds it until the origin's core has decided the
//! identity the link's ticket names. Anyone holding the early packets may
//! replay them on a connection of their own, which the origin serves and
//! whose answers never reach the replica.
//!
//! Two states are one when their stores, the link, the environment and the
//! replica's source stand alike. The cores' checkpoints ride along beside
//! that, as the first path to reach the state left them: what else differs
//! between two such cores is the numbering of the calls they made and the
//! length of a backoff, and neither changes any later step's outcome here,
//! where call ids are opaque and no clock runs.

use std::cell::RefCell;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use daemon_core::payload::{
    Answer, CarrierKind, ClientRequest, PeerFailure, PeerReply, PeerRequest, PublicKey, Response,
    Status,
};
use daemon_core::trust::TrustStore;
use daemon_core::{
    AgentId, CallId, Daemon, HostId, Input, LinkId, Output, Route, Settings, Standing,
    StartOptions, StepBudget, TimerId,
};
use prost::Message as _;
use store::{AgentKey, AgentRow, CommitClock, Dump, Marker, Sqlite};
use wire::{Item, Phase, SessionEvent, Snapshot, Step, session_event};

use crate::held::{Held, Slot};
use crate::single_daemon::{EARLY_AFTER_ACCEPT, REPLAY_CHANGES_NOTHING};
use crate::{Machine, Property};

const ORIGIN: HostId = HostId(*b"origin-host-0000");
const REPLICA: HostId = HostId(*b"replica-host-000");
const AGENT: AgentId = AgentId(*b"agent-0000000000");
/// The replica's link to the origin.
const LINK: LinkId = LinkId(1);
/// The link as the origin numbers it, and the connection a replay of the
/// replica's early data arrives on there.
const ORIGIN_LINK: LinkId = LinkId(1);
const REPLAY_LINK: LinkId = LinkId(2);
/// The call the origin's driver numbers a replayed opening with.
const REPLAYED: CallId = CallId(1 << 32);
/// The replica's client that fetches below the block.
const CLIENT: CallId = CallId(1);
const KEYS: [&str; 4] = ["a", "b", "c", "d"];
const CLOCK: CommitClock = CommitClock {
    now_ms: 0,
    notify_delay_ms: 0,
};

/// The one agent replicated.
fn agent() -> AgentKey {
    AgentKey::new(ORIGIN.0.to_vec(), AGENT.0.to_vec())
}

/// Replication between two real daemon cores, within its bounds.
#[derive(Clone, Debug)]
pub struct Replication {
    /// How many distinct keys the origin commits.
    pub keys: usize,
    /// The cap on a delta, the tail a reset carries, and a page's size.
    pub cap: u32,
    /// The newest revision the origin mints in a generation.
    pub revisions: u64,
    /// How many generations the origin runs: one fewer unclean reboots.
    pub generations: u64,
    /// How many messages the link carries at once; one more and the
    /// origin drops the stream as lagging.
    pub in_flight: usize,
    /// How many losses in a row the link may suffer before it delivers.
    pub losses: u8,
    /// How many times the replica's early data is replayed.
    pub replays: u8,
}

impl Replication {
    /// The bounds `just test` explores exhaustively: one key more than the
    /// cap, so a tail, a delta and a page each leave a row out.
    pub const fn ci() -> Self {
        Self {
            keys: 2,
            cap: 1,
            revisions: 4,
            generations: 2,
            in_flight: 2,
            losses: 1,
            replays: 1,
        }
    }

    /// The larger bounds a nightly run explores: a third key, a cap of two,
    /// a fifth revision and a third message in flight.
    pub const fn nightly() -> Self {
        Self {
            keys: 3,
            cap: 2,
            revisions: 5,
            in_flight: 3,
            ..Self::ci()
        }
    }

    fn settings(&self) -> Settings {
        Settings {
            tail_rows: self.cap,
            ..Settings::default()
        }
    }
}

/// One message on the stream from the origin to the replica: the session
/// events one step of the origin's core answered, encoded so a message can
/// be hashed.
#[derive(Clone, PartialEq, Eq, Hash)]
pub enum Msg {
    /// What the origin answered the opening with: its generation, a Reset
    /// when it replaces the block, the snapshot, the rows and its CaughtUp.
    Opening(Vec<Vec<u8>>),
    /// One event after the opening: a row committed since, or that the
    /// stream fell behind.
    Live(Vec<u8>),
}

impl Msg {
    fn events(&self) -> Vec<SessionEvent> {
        let decode = |bytes: &Vec<u8>| SessionEvent::decode(bytes.as_slice()).expect("an event");
        match self {
            Self::Opening(events) => events.iter().map(decode).collect(),
            Self::Live(event) => vec![decode(event)],
        }
    }
}

impl fmt::Debug for Msg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let events: Vec<String> = self.events().iter().map(describe).collect();
        match self {
            Self::Opening(_) => write!(f, "Opening{events:?}"),
            Self::Live(_) => write!(f, "Live({})", events.join("")),
        }
    }
}

/// A session event as a counterexample shows it.
fn describe(event: &SessionEvent) -> String {
    match &event.of {
        Some(session_event::Of::Opening(opening)) => format!("generation {}", opening.generation),
        Some(session_event::Of::Reset(_)) => "Reset".into(),
        Some(session_event::Of::Snapshot(_)) => "Snapshot".into(),
        Some(session_event::Of::Item(item)) => row(item),
        Some(session_event::Of::CaughtUp(caught_up)) => {
            format!("caught up {}", caught_up.revision)
        }
        Some(session_event::Of::Lagged(_)) => "Lagged".into(),
        other => format!("{other:?}"),
    }
}

/// The stream between the two.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Link {
    /// No stream.
    Idle,
    /// The replica's source has asked; the origin has not answered. The
    /// request travels encoded.
    Asked { call: CallId, request: Vec<u8> },
    /// The stream is open and carries `queue` in order. Once it is dropped
    /// (the origin rebooted, it lagged, or a message was lost), `live` is
    /// false: nothing more is added and the stream ends when the queue
    /// drains.
    Open {
        call: CallId,
        queue: Vec<Msg>,
        live: bool,
    },
}

impl Link {
    fn call(&self) -> Option<CallId> {
        match self {
            Link::Idle => None,
            Link::Asked { call, .. } | Link::Open { call, .. } => Some(*call),
        }
    }
}

/// What the stores say that decides which actions are enabled, read after
/// every transition that changed them.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Seen {
    /// The origin's newest revision.
    minted: u64,
    /// Rows the origin holds.
    origin_rows: usize,
    /// The replica has a block and the origin has not said nothing is older.
    can_page: bool,
}

#[derive(Clone)]
pub struct State {
    origin: Held,
    replica: Held,
    /// The origin's generation.
    generation: u64,
    /// The origin's store as it was before its newest commit: what an
    /// unclean reboot rolls it back to.
    durable: Option<Dump>,
    link: Link,
    /// The replica's core armed the timer that opens its source again.
    timer: bool,
    /// The origin's core took the link the replica's calls ride.
    accepted: bool,
    /// Replays of the replica's early data so far.
    replays: u8,
    /// The origin's driver served a call on a link its core did not list.
    early_unaccepted: Option<String>,
    /// A replay of early data changed what a store holds.
    replay_changed: Option<String>,
    /// Losses since the link last delivered.
    lost: u8,
    seen: Seen,
    /// The page the origin answered that has not yet reached the replica:
    /// the call it answers, and the page encoded, or nothing when the
    /// origin failed it.
    page: Option<(CallId, Option<Vec<u8>>)>,
    /// A write the store refused; nothing happens after one.
    refused: Option<String>,
    /// A core asked for a dump.
    dumped: Option<String>,
    /// How the replica's source stands, as its core last said.
    standing: Standing,
}

impl State {
    /// What tells two states apart: everything but the cores' checkpoints
    /// and the calls the link is numbered by, and how the replica's source
    /// stands in their place.
    fn identity(&self) -> impl Hash + Eq + '_ {
        let link = match &self.link {
            Link::Idle => (0, None, &[][..], true),
            Link::Asked { request, .. } => (1, Some(request), &[][..], true),
            Link::Open { queue, live, .. } => (2, None, queue.as_slice(), *live),
        };
        (
            (&self.origin.store, &self.replica.store, &self.durable),
            (link, self.generation, self.timer, self.lost),
            self.page.as_ref().map(|(_, page)| page),
            (&self.refused, &self.dumped, self.standing),
            (
                self.accepted,
                self.replays,
                &self.early_unaccepted,
                &self.replay_changed,
            ),
        )
    }
}

impl PartialEq for State {
    fn eq(&self, other: &Self) -> bool {
        self.identity() == other.identity()
    }
}

impl Eq for State {}

impl Hash for State {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.identity().hash(state);
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    /// The origin commits the key: a new key takes the next order, a held
    /// key keeps its order and takes the next revision.
    Commit { key: &'static str },
    /// The origin's retention drops its oldest row.
    Trim,
    /// The timer the replica's core armed fires, and its source opens a
    /// stream: after its cursor, or for a tail.
    Open,
    /// The origin's driver reports the replica's resumed link to its core,
    /// which takes it or refuses it.
    Resumed,
    /// The origin's core answers the opening: a delta when it fits and the
    /// generation is its own, otherwise a reset and a fresh tail. Its
    /// driver serves it only once the core took the link it rides.
    Answer,
    /// The stream delivers its next message to the replica's core.
    Deliver,
    /// The origin reboots uncleanly, losing its newest commit and coming
    /// back under the next generation to mint those revisions again.
    RebootUnclean,
    /// The stream dies at the message at this place in flight, which goes
    /// with everything behind it.
    Lose(usize),
    /// The replica's opening arrives again, replayed on a connection of its
    /// own.
    Duplicate,
    /// A client fetches below the replica's block: the replica's core asks
    /// the origin's for the page, which answers it at once.
    AskPage,
    /// The origin's answer to the page reaches the replica's core, which
    /// absorbs it where it still joins the block.
    PageArrives,
}

impl Machine for Replication {
    type State = State;
    type Action = Action;

    fn init(&self) -> State {
        crate::share_sqlite_across_threads();
        let mut origin = Sqlite::open_in_memory(ORIGIN.0).expect("an origin opens");
        origin
            .put_agent(&agent_row())
            .expect("the origin takes its agent");
        let first = Step {
            snapshot: Some(Snapshot {
                kind: wire::Kind::Codex as i32,
                phase: Phase::Working as i32,
                ..Default::default()
            }),
            ..Default::default()
        };
        origin
            .commit(&agent(), &[(1, first)], CLOCK)
            .expect("the origin commits");
        origin.set_marker(&agent(), Some(Marker::CaughtUp));
        TrustStore::of_hosts([REPLICA]).write_for_test(&mut origin);
        let mut replica = Sqlite::open_in_memory(REPLICA.0).expect("a replica opens");
        TrustStore::of_hosts([ORIGIN]).write_for_test(&mut replica);
        let (origin, _) = Held::of(Daemon::new(origin, options(1)));
        let (replica, _) = Held::of(Daemon::new(replica, options(1)));
        let mut s = State {
            origin,
            replica,
            generation: 1,
            durable: None,
            link: Link::Idle,
            timer: false,
            accepted: false,
            replays: 0,
            early_unaccepted: None,
            replay_changed: None,
            lost: 0,
            seen: Seen {
                minted: 0,
                origin_rows: 0,
                can_page: false,
            },
            page: None,
            refused: None,
            dumped: None,
            standing: Standing::Closed { settled: false },
        };
        with_slots(|slots| {
            let mut t = Transition {
                model: self,
                slots,
                next: &mut s,
            };
            t.origin(Input::Configure(self.settings()));
            t.origin(Input::Started);
            t.resumed();
            t.replica(Input::Configure(self.settings()));
            t.replica(Input::Started);
            t.replica(Input::LinkUp {
                link: LINK,
                peer: ORIGIN,
                kind: CarrierKind::Direct,
                resumed: None,
            });
            // The replica follows the origin's inventory, and at its
            // CaughtUp opens the agent's source.
            let follow = t.replica(Input::Timer {
                id: TimerId::Follow(ORIGIN),
            });
            let Some((inventory, request)) = follow.iter().find_map(|output| match output {
                Output::Call {
                    call,
                    request: request @ PeerRequest::SubscribeInventory,
                    ..
                } => Some((*call, request.clone())),
                _ => None,
            }) else {
                panic!("the replica follows the origin's inventory: {follow:?}");
            };
            let listed = t.serve(ORIGIN_LINK, inventory, request);
            for output in listed {
                if let Output::Answer {
                    call,
                    response: Response::Inventory(event),
                } = output
                    && call == inventory
                {
                    t.replica(Input::PeerAnswer {
                        call,
                        answer: Answer::Inventory((*event).clone()),
                    });
                }
            }
            assert!(
                matches!(t.next.link, Link::Asked { .. }),
                "the inventory's CaughtUp opens the agent's source"
            );
            t.settle();
        });
        s
    }

    fn actions(&self, s: &State, out: &mut Vec<Action>) {
        if s.refused.is_some() {
            return;
        }
        if s.seen.minted < self.revisions {
            out.extend(KEYS[..self.keys].iter().map(|&key| Action::Commit { key }));
        }
        if s.seen.origin_rows > 1 {
            out.push(Action::Trim);
        }
        if s.generation < self.generations {
            out.push(Action::RebootUnclean);
        }
        if s.timer {
            out.push(Action::Open);
        }
        match &s.link {
            Link::Idle => {}
            Link::Asked { .. } if !s.accepted => out.push(Action::Resumed),
            Link::Asked { .. } => out.push(Action::Answer),
            Link::Open { queue, .. } => {
                if !queue.is_empty() {
                    out.push(Action::Deliver);
                }
                if s.lost < self.losses {
                    out.extend((0..queue.len()).map(Action::Lose));
                }
            }
        }
        if s.replays < self.replays && matches!(s.link, Link::Asked { .. }) {
            out.push(Action::Duplicate);
        }
        match s.page {
            // A page asked for rides the link too, held until the origin
            // takes it.
            None if s.seen.can_page && s.accepted => out.push(Action::AskPage),
            None => {}
            Some(_) => out.push(Action::PageArrives),
        }
    }

    fn next(&self, s: &State, action: &Action) -> Option<State> {
        let mut next = s.clone();
        with_slots(|slots| {
            let mut t = Transition {
                model: self,
                slots,
                next: &mut next,
            };
            if let Err(error) = t.take(action) {
                t.next.refused = Some(format!("{action:?}: {error}"));
            }
            t.drained();
            t.settle();
        });
        Some(next)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        let properties: Vec<Property<Self>> = vec![
            Property::always("the store never refuses a write", |_, s: &State| {
                s.refused.is_none()
            }),
            Property::always("the cores never ask for a dump", |_, s: &State| {
                s.dumped.is_none()
            }),
            Property::always(EARLY_AFTER_ACCEPT, |_, s: &State| {
                s.early_unaccepted.is_none()
            }),
            Property::always(REPLAY_CHANGES_NOTHING, |_, s: &State| {
                s.replay_changed.is_none()
            }),
            Property::always(BLOCK_CONTIGUOUS, |_, s: &State| {
                !s.same_generation()
                    || s.view(|origin, replica| block_contiguous(origin, replica, &agent()))
            }),
            Property::always(BLOCK_AGREES, |_, s: &State| {
                !s.same_generation()
                    || s.view(|origin, replica| block_agrees(origin, replica, &agent()))
            }),
            Property::always(NO_OTHER_GENERATION, |_, s: &State| {
                !s.same_generation()
                    || s.view(|origin, replica| no_other_generation(origin, replica, &agent()))
            }),
            Property::always(CURSOR_COMPLETE, |_, s: &State| {
                !s.same_generation()
                    || s.view(|origin, replica| cursor_complete(origin, replica, &agent()))
            }),
            Property::sometimes("the replica is current", |_, s: &State| s.current()),
            Property::sometimes("the block is non-empty after a reset", |_, s: &State| {
                s.view(|_, replica| {
                    replica.reset_count(&agent()).unwrap() > 0
                        && !replica.block(&agent()).unwrap().is_empty()
                })
            }),
            Property::sometimes(
                "a replica takes the origin's next generation",
                |_, s: &State| s.generation > 1 && s.same_generation(),
            ),
            Property::sometimes(
                "a page joins the block below a reset's tail",
                |_, s: &State| {
                    s.view(|_, replica| {
                        let Some(row) = replica.agent(&agent()).unwrap() else {
                            return false;
                        };
                        row.exhausted
                            && replica.reset_count(&agent()).unwrap() > 0
                            && row.complete_from_order == Some(1)
                    })
                },
            ),
            Property::eventually(
                "a replica whose origin and link fall quiet becomes current",
                |model: &Self, s: &State| model.quiet(s) && s.current(),
            ),
        ];
        properties
    }
}

impl Replication {
    /// Nothing more can happen at the origin or on the link but the
    /// replica's own paging: every revision minted, every generation run,
    /// nothing left to trim, and an open stream with nothing in flight.
    fn quiet(&self, s: &State) -> bool {
        s.seen.minted >= self.revisions
            && s.generation >= self.generations
            && s.seen.origin_rows <= 1
            && matches!(&s.link, Link::Open { queue, live: true, .. } if queue.is_empty())
    }
}

/// The options both cores run under: the generation, and no step budget,
/// since the model is a function of its actions alone and no step may ask
/// for a dump because the machine ran slowly.
fn options(generation: u64) -> StartOptions {
    StartOptions {
        generation,
        budget: StepBudget::UNLIMITED,
        ..StartOptions::default()
    }
}

/// One transition under way: the state it makes, and this thread's stores
/// to step the two cores over.
struct Transition<'a> {
    model: &'a Replication,
    slots: &'a mut Slots,
    next: &'a mut State,
}

impl Transition<'_> {
    /// One action, as the drivers and the world carry it out.
    fn take(&mut self, action: &Action) -> Result<(), store::StoreError> {
        match action {
            Action::Commit { key } => self.commit(key)?,
            Action::Trim => self.slots.origin.write(&mut self.next.origin, |origin| {
                let pool = origin.pool_bytes(true)?;
                origin.sweep_own(pool.saturating_sub(1), 1, 1).map(|_| ())
            })?,
            Action::RebootUnclean => {
                if let Some(durable) = self.next.durable.take() {
                    self.next.origin.store = durable;
                }
                self.next.generation += 1;
                let generation = self.next.generation;
                self.slots.origin.start(&mut self.next.origin, |store| {
                    Daemon::new(store, options(generation))
                });
                self.origin(Input::Configure(self.model.settings()));
                self.origin(Input::Started);
                // The replica's driver resumes the link the reboot dropped,
                // and what it asks next rides it as early data.
                self.next.accepted = false;
                // A page on its way back goes with the link the reboot
                // dropped.
                if let Some((call, _)) = self.next.page.take() {
                    self.replica(Input::PeerAnswer {
                        call,
                        answer: Answer::Failed(PeerFailure::Uncertain),
                    });
                }
                // Every stream the origin answered is gone with its process.
                self.next.link = match std::mem::replace(&mut self.next.link, Link::Idle) {
                    Link::Open { call, queue, .. } => Link::Open {
                        call,
                        queue,
                        live: false,
                    },
                    Link::Asked { call, .. } => Link::Open {
                        call,
                        queue: Vec::new(),
                        live: false,
                    },
                    Link::Idle => Link::Idle,
                };
            }
            Action::Open => {
                self.next.timer = false;
                self.replica(Input::Timer {
                    id: TimerId::Source(ORIGIN, AGENT),
                });
            }
            Action::Resumed => self.resumed(),
            Action::Duplicate => self.replay(),
            Action::Answer => {
                let Link::Asked { call, request } = &self.next.link else {
                    unreachable!("answered without an ask")
                };
                let call = *call;
                let request = wire::SubscribeSessionRequest::decode(request.as_slice())
                    .expect("a request decodes");
                self.next.link = Link::Open {
                    call,
                    queue: Vec::new(),
                    live: true,
                };
                let answered =
                    self.serve(ORIGIN_LINK, call, PeerRequest::SubscribeSession(request));
                let (events, ended) = answers_to(call, &answered);
                let Link::Open { queue, live, .. } = &mut self.next.link else {
                    unreachable!("the stream was just opened")
                };
                if !events.is_empty() {
                    queue.push(Msg::Opening(events));
                }
                *live = !ended;
            }
            Action::Deliver => {
                let Link::Open { call, queue, .. } = &mut self.next.link else {
                    unreachable!("delivered without a stream")
                };
                let call = *call;
                let message = queue.remove(0);
                self.next.lost = 0;
                for event in message.events() {
                    // The replica may have let the stream go midway.
                    if self.next.link.call() != Some(call) {
                        break;
                    }
                    self.replica(Input::PeerAnswer {
                        call,
                        answer: Answer::Session(event),
                    });
                }
            }
            Action::Lose(index) => {
                let Link::Open {
                    call, queue, live, ..
                } = &mut self.next.link
                else {
                    unreachable!("lost without a stream")
                };
                let call = *call;
                queue.truncate(*index);
                *live = false;
                self.next.lost += 1;
                // The origin's driver finds the stream gone.
                self.origin(Input::CallCancelled { call });
            }
            Action::AskPage => self.ask_page(),
            Action::PageArrives => {
                let (call, page) = self.next.page.take().expect("a page in flight");
                let answer = match page {
                    Some(page) => Answer::Reply(Box::new(PeerReply::Fetched(
                        wire::ListItemsResponse::decode(page.as_slice()).expect("a page decodes"),
                    ))),
                    None => Answer::Failed(PeerFailure::Uncertain),
                };
                self.replica(Input::PeerAnswer { call, answer });
            }
        }
        Ok(())
    }

    /// The agent commits the key at the origin, and each record committed
    /// reaches the origin's core, which answers it on every open stream:
    /// onto the link while it has room, and as lagging once it has none.
    fn commit(&mut self, key: &str) -> Result<(), store::StoreError> {
        self.next.durable = Some(self.next.origin.store.clone());
        let generation = self.next.generation;
        let committed = self.slots.origin.write(&mut self.next.origin, |origin| {
            let offset = origin.agent(&agent())?.map_or(0, |row| row.ingest_cursor) + 1;
            let item = Item {
                key: key.into(),
                kind: wire::Kind::Codex as i32,
                text: format!("{key} in generation {generation}"),
                ..Default::default()
            };
            let step = Step {
                items: vec![item],
                ..Default::default()
            };
            origin.commit(&agent(), &[(offset, step)], CLOCK)
        })?;
        for record in committed.records {
            let of = match record {
                store::Record::Item(item) => session_event::Of::Item(item),
                store::Record::Append(append) => session_event::Of::Append(append),
                store::Record::Snapshot(snapshot) => session_event::Of::Snapshot(snapshot),
            };
            let answered = self.origin(Input::ShellSession {
                agent: agent(),
                event: Arc::new(SessionEvent { of: Some(of) }),
            });
            self.carry(answered);
        }
        Ok(())
    }

    /// Puts what the origin's core answered on the stream after its
    /// opening, while there is room; the driver reports a stream with none
    /// as lagging, and the origin's core says so on it and lets it go.
    fn carry(&mut self, answered: Vec<Output>) {
        let Link::Open {
            call, live: true, ..
        } = self.next.link
        else {
            return;
        };
        let (events, ended) = answers_to(call, &answered);
        for event in events {
            let Link::Open { queue, live, .. } = &mut self.next.link else {
                return;
            };
            if !*live {
                return;
            }
            if queue.len() < self.model.in_flight {
                queue.push(Msg::Live(event));
                continue;
            }
            let lagged = self.origin(Input::AnswerLagged { call });
            let (said, _) = answers_to(call, &lagged);
            let Link::Open { queue, live, .. } = &mut self.next.link else {
                return;
            };
            queue.extend(said.into_iter().map(Msg::Live));
            *live = false;
        }
        if ended && let Link::Open { live, .. } = &mut self.next.link {
            *live = false;
        }
    }

    /// A client fetches below the replica's block; the replica's core asks
    /// the origin's for the page, whose answer the link then carries back.
    fn ask_page(&mut self) {
        let floor = self
            .slots
            .replica
            .read(&self.next.replica.store, |replica| {
                replica
                    .agent(&agent())
                    .unwrap()
                    .and_then(|row| row.complete_from_order)
            });
        let Some(floor) = floor else {
            unreachable!("paged without a block")
        };
        let asked = self.replica(Input::ClientCall {
            call: CLIENT,
            request: ClientRequest::ListItems(wire::ListItemsRequest {
                agent_id: AGENT.0.to_vec(),
                before_order: Some(floor),
                page_size: self.model.cap,
            }),
        });
        let Some((made, request)) = asked.into_iter().find_map(|output| match output {
            Output::Call {
                call,
                request: request @ PeerRequest::ListItems(_),
                ..
            } => Some((call, request)),
            _ => None,
        }) else {
            return;
        };
        let answered = self.serve(ORIGIN_LINK, made, request);
        let page = answered
            .into_iter()
            .find_map(|output| match output {
                Output::Answer {
                    call,
                    response: Response::Fetched(page),
                } if call == made => Some(Some(page.encode_to_vec())),
                Output::EndCall {
                    call,
                    status: Status::Failed(_),
                } if call == made => Some(None),
                _ => None,
            })
            .expect("the origin answers a page");
        self.next.page = Some((made, page));
    }

    /// A stream dropped whose queue has drained ends: the replica's driver
    /// reports its end.
    fn drained(&mut self) {
        if let Link::Open {
            call,
            queue,
            live: false,
        } = &self.next.link
            && queue.is_empty()
        {
            let call = *call;
            self.next.link = Link::Idle;
            self.replica(Input::PeerAnswer {
                call,
                answer: Answer::Ended,
            });
        }
    }

    fn origin(&mut self, input: Input) -> Vec<Output> {
        let out = self.slots.origin.step(&mut self.next.origin, input);
        self.dumps(&out);
        out
    }

    /// The origin's driver serves the replica's call that arrived on
    /// `link`, which its core must list to the replica. What the core lists
    /// after the step is what it listed before: a call moves no route.
    fn serve(&mut self, link: LinkId, call: CallId, request: PeerRequest) -> Vec<Output> {
        let input = Input::PeerCall {
            from: REPLICA,
            call,
            request,
        };
        let (out, listed) =
            self.slots
                .origin
                .step_reading(&mut self.next.origin, input, |daemon| {
                    daemon.links().routes_to(REPLICA)
                });
        if !listed.contains(&Route::Direct(link)) {
            self.next.early_unaccepted.get_or_insert_with(|| {
                format!("served on {link:?} while the origin lists {listed:?}")
            });
        }
        self.dumps(&out);
        out
    }

    /// The origin's driver reports the replica's resumed link, its ticket
    /// naming the key the origin pinned for the replica.
    fn resumed(&mut self) {
        let out = self.origin(resumed_up(ORIGIN_LINK));
        self.next.accepted = !out.contains(&Output::RefuseResumed { link: ORIGIN_LINK });
    }

    /// The replica's early opening arrives again on a connection of its
    /// own: the origin serves it there once its core took the connection,
    /// its answers go to whoever replayed it, and the connection dies.
    fn replay(&mut self) {
        let Link::Asked { request, .. } = &self.next.link else {
            unreachable!("only an opening in flight is replayed")
        };
        let request =
            wire::SubscribeSessionRequest::decode(request.as_slice()).expect("a request decodes");
        self.next.replays += 1;
        let before = (
            self.next.origin.store.clone(),
            self.next.replica.store.clone(),
            self.next.link.clone(),
        );
        let out = self.origin(resumed_up(REPLAY_LINK));
        if !out.contains(&Output::RefuseResumed { link: REPLAY_LINK }) {
            self.serve(
                REPLAY_LINK,
                REPLAYED,
                PeerRequest::SubscribeSession(request),
            );
            self.origin(Input::CallCancelled { call: REPLAYED });
        }
        self.origin(Input::LinkDown {
            link: REPLAY_LINK,
            reason: wire::LinkCloseReason::UserShutdown,
        });
        let after = (
            &self.next.origin.store,
            &self.next.replica.store,
            &self.next.link,
        );
        if (&before.0, &before.1, &before.2) != after {
            self.next
                .replay_changed
                .get_or_insert_with(|| "a store or the stream changed".into());
        }
    }

    /// Steps the replica's core and carries out what it asks of the link
    /// and the timers.
    fn replica(&mut self, input: Input) -> Vec<Output> {
        let (out, standing) =
            self.slots
                .replica
                .step_reading(&mut self.next.replica, input, |daemon| {
                    daemon.sources().standing(&agent())
                });
        self.next.standing = standing;
        self.dumps(&out);
        for output in &out {
            match output {
                Output::Call {
                    call,
                    request: PeerRequest::SubscribeSession(request),
                    ..
                } => {
                    if let Some(old) = self.next.link.call() {
                        self.origin(Input::CallCancelled { call: old });
                    }
                    self.next.link = Link::Asked {
                        call: *call,
                        request: request.encode_to_vec(),
                    };
                }
                Output::HangUp { call } if self.next.link.call() == Some(*call) => {
                    self.next.link = Link::Idle;
                    self.origin(Input::CallCancelled { call: *call });
                }
                Output::ArmTimer {
                    id: TimerId::Source(..),
                    ..
                } => self.next.timer = true,
                Output::CancelTimer {
                    id: TimerId::Source(..),
                } => self.next.timer = false,
                _ => {}
            }
        }
        out
    }

    fn dumps(&mut self, out: &[Output]) {
        for output in out {
            if let Output::Dump { reason } = output {
                self.next
                    .dumped
                    .get_or_insert_with(|| format!("{reason:?}"));
            }
        }
    }

    /// What the stores now say about the enabled actions.
    fn settle(&mut self) {
        let next = &mut *self.next;
        let (minted, origin_rows) = self.slots.origin.read(&next.origin.store, |origin| {
            let row = origin
                .agent(&agent())
                .unwrap()
                .expect("the origin holds its agent");
            (
                row.next_revision.saturating_sub(1),
                origin.rows(&agent()).unwrap().len(),
            )
        });
        let can_page = self.slots.replica.read(&next.replica.store, |replica| {
            replica
                .agent(&agent())
                .unwrap()
                .is_some_and(|row| row.complete_from_order.is_some() && !row.exhausted)
        });
        next.seen = Seen {
            minted,
            origin_rows,
            can_page,
        };
    }
}

/// The replica's link coming up at the origin as `link`, resumed with a
/// ticket naming the key the origin pinned for the replica.
fn resumed_up(link: LinkId) -> Input {
    let store = TrustStore::of_hosts([REPLICA]);
    let key = store
        .pubkey_for_host(uuid::Uuid::from_bytes(REPLICA.0))
        .expect("the replica is pinned")
        .to_vec();
    Input::LinkUp {
        link,
        peer: REPLICA,
        kind: CarrierKind::Direct,
        resumed: Some(PublicKey(key)),
    }
}

/// The session events a step answered on `call`, in order, and whether it
/// ended the call.
fn answers_to(call: CallId, out: &[Output]) -> (Vec<Vec<u8>>, bool) {
    let mut events = Vec::new();
    let mut ended = false;
    for output in out {
        match output {
            Output::Answer {
                call: to,
                response: Response::Session(event),
            } if *to == call => events.push(event.encode_to_vec()),
            Output::EndCall { call: to, .. } if *to == call => ended = true,
            _ => {}
        }
    }
    (events, ended)
}

/// This thread's stores for the two hosts.
struct Slots {
    origin: Slot,
    replica: Slot,
}

fn with_slots<R>(f: impl FnOnce(&mut Slots) -> R) -> R {
    thread_local! {
        static SLOTS: RefCell<Option<Slots>> = const { RefCell::new(None) };
    }
    SLOTS.with(|slots| {
        let mut slots = slots.borrow_mut();
        let slots = slots.get_or_insert_with(|| Slots {
            origin: Slot::new(&ORIGIN.0),
            replica: Slot::new(&REPLICA.0),
        });
        f(slots)
    })
}

impl State {
    /// Calls `f` with both stores as this state holds them. A thread keeps
    /// the last state's stores, since every property reads the same state
    /// in turn.
    fn view<R>(&self, f: impl FnOnce(&Sqlite, &Sqlite) -> R) -> R {
        thread_local! {
            static HELD: RefCell<Option<(Slot, Slot)>> = const { RefCell::new(None) };
        }
        HELD.with(|held| {
            let mut held = held.borrow_mut();
            let (origin, replica) =
                held.get_or_insert_with(|| (Slot::new(&ORIGIN.0), Slot::new(&REPLICA.0)));
            origin.read(&self.origin.store, |origin| {
                replica.read(&self.replica.store, |replica| f(origin, replica))
            })
        })
    }

    /// The replica's block was taken under the generation the origin runs.
    fn same_generation(&self) -> bool {
        self.view(|_, replica| {
            replica.source_generation(&agent()).unwrap() == Some(self.generation)
        })
    }

    fn current(&self) -> bool {
        self.same_generation()
            && self.view(|origin, replica| replica.current_with(&agent(), origin).unwrap())
    }
}

impl fmt::Debug for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.view(|origin, replica| {
            let agent = agent();
            let rows = |store: &Sqlite| {
                store
                    .rows(&agent)
                    .unwrap()
                    .iter()
                    .map(row)
                    .collect::<Vec<_>>()
            };
            let origin_row = origin.agent(&agent).unwrap().unwrap();
            let mut debug = f.debug_struct("State");
            debug
                .field("generation", &self.generation)
                .field("origin", &rows(origin))
                .field("origin_from", &origin_row.complete_from_order);
            if let Some(replica_row) = replica.agent(&agent).unwrap() {
                debug
                    .field("replica", &rows(replica))
                    .field("replica_from", &replica_row.complete_from_order)
                    .field("source_cursor", &replica_row.source_cursor)
                    .field("source_generation", &replica_row.source_generation);
            }
            debug
                .field("link", &self.link)
                .field(
                    "page",
                    &self.page.as_ref().map(|(_, page)| {
                        page.as_ref().map(|page| {
                            let page = wire::ListItemsResponse::decode(page.as_slice())
                                .expect("a page decodes");
                            page.items.iter().map(row).collect::<Vec<_>>()
                        })
                    }),
                )
                .field("timer", &self.timer)
                .field("standing", &self.standing)
                .field("refused", &self.refused)
                .field("dumped", &self.dumped)
                .finish()
        })
    }
}

/// The order the comparison of two blocks starts from: neither store's
/// rows below it can be compared, the replica's because they are not its
/// block, the origin's because it no longer holds them.
/// The four promises a replica's block keeps of its origin's rows for one
/// agent, judged under the origin's generation; the composed net judges
/// them too.
pub(crate) const BLOCK_CONTIGUOUS: &str = "the block is contiguous: under the origin's generation, every row the \
     origin holds between the block's oldest and newest rows is in the block";
pub(crate) const BLOCK_AGREES: &str = "the block agrees with the origin: under the origin's generation, every row \
     of the block the origin still holds is the origin's row at that order, or an older revision of it";
pub(crate) const NO_OTHER_GENERATION: &str = "no row survives from another generation: under the origin's \
     generation, every row the replica stores at an order the origin holds is that order's key, no newer than \
     the origin's, and a key the origin has since moved to a later order is an older revision of it";
pub(crate) const CURSOR_COMPLETE: &str = "the cursor is complete: under the origin's generation, the replica \
     holds every row of its block's range that the origin revised at or below the cursor";

pub(crate) fn block_contiguous(origin: &Sqlite, replica: &Sqlite, agent: &AgentKey) -> bool {
    let mine = replica.block(agent).unwrap();
    let (Some(oldest), Some(newest)) = (mine.first(), mine.last()) else {
        return true;
    };
    let from = common_floor(origin, replica, agent).max(oldest.order);
    origin
        .suffix_from(agent, from)
        .unwrap()
        .iter()
        .filter(|theirs| theirs.order <= newest.order)
        .all(|theirs| mine.iter().any(|mine| mine.order == theirs.order))
}

pub(crate) fn block_agrees(origin: &Sqlite, replica: &Sqlite, agent: &AgentKey) -> bool {
    let theirs = origin.suffix_from(agent, 0).unwrap();
    replica
        .suffix_from(agent, common_floor(origin, replica, agent))
        .unwrap()
        .iter()
        .all(|mine| {
            theirs
                .iter()
                .find(|theirs| theirs.order == mine.order)
                .is_some_and(|theirs| no_newer(mine, theirs))
        })
}

pub(crate) fn no_other_generation(origin: &Sqlite, replica: &Sqlite, agent: &AgentKey) -> bool {
    let theirs = origin.rows(agent).unwrap();
    replica.rows(agent).unwrap().iter().all(|mine| {
        theirs.iter().all(|theirs| {
            if theirs.order == mine.order {
                no_newer(mine, theirs)
            } else if theirs.key == mine.key {
                mine.order < theirs.order && mine.revision < theirs.revision
            } else {
                true
            }
        })
    })
}

pub(crate) fn cursor_complete(origin: &Sqlite, replica: &Sqlite, agent: &AgentKey) -> bool {
    let cursor = replica.source_cursor(agent).unwrap();
    let mine = replica.rows(agent).unwrap();
    origin
        .suffix_from(agent, common_floor(origin, replica, agent))
        .unwrap()
        .iter()
        .filter(|theirs| theirs.revision <= cursor)
        .all(|theirs| mine.iter().any(|mine| mine == theirs))
}

fn common_floor(origin: &Sqlite, replica: &Sqlite, agent: &AgentKey) -> u64 {
    let floor = |store: &Sqlite| store.block_orders(agent).unwrap().from.unwrap_or(0);
    floor(origin).max(floor(replica))
}

/// The replica's row is the origin's key at the origin's order, and either
/// the origin's row itself or an older revision of it.
fn no_newer(mine: &Item, theirs: &Item) -> bool {
    mine.order == theirs.order
        && mine.key == theirs.key
        && (mine.revision < theirs.revision || mine == theirs)
}

/// An item as a counterexample shows it: key at order, revision and text.
fn row(item: &Item) -> String {
    format!(
        "{}@{} r{} {:?}",
        item.key, item.order, item.revision, item.text
    )
}

fn agent_row() -> AgentRow {
    let mut row = AgentRow::new(agent(), wire::Kind::Codex, "/src");
    row.name = "worker".into();
    row
}
