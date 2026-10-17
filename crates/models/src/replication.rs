//! Replication of one agent from its origin to one replica, over a link
//! that may lose what it carries, against an origin that commits, trims
//! its history and reboots uncleanly.
//!
//! Both stores are the real `Sqlite`, in memory; the state holds their
//! dumps. Every transition rebuilds the stores, calls the real store the
//! way the daemon does, and dumps them again. What the daemon's shell does
//! around the store (choosing a delta or a reset for an opening, folding a
//! catch-up, the order a stream delivers in) is the environment here.

use std::cell::RefCell;
use std::fmt;

use prost::Message as _;
use store::{Absorb, AgentKey, AgentRow, CommitClock, Dump, Marker, SourceEvent, Sqlite};
use wire::{Item, Phase, Snapshot, Step};

use crate::{Machine, Property};

const ORIGIN: &[u8] = b"origin";
const REPLICA: &[u8] = b"replica";
const KEYS: [&str; 4] = ["a", "b", "c", "d"];
const CLOCK: CommitClock = CommitClock {
    now_ms: 0,
    notify_delay_ms: 0,
};

/// The one agent replicated.
fn agent() -> AgentKey {
    AgentKey::new(ORIGIN, b"agent".to_vec())
}

/// Replication between two real stores, within its bounds.
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
    /// Adds the deliberately wrong promise that the block stays empty, to
    /// show what a failure prints.
    pub wrong: bool,
}

impl Replication {
    /// The bounds `just test` explores exhaustively.
    pub const fn ci() -> Self {
        Self {
            keys: 3,
            cap: 2,
            revisions: 4,
            generations: 2,
            in_flight: 2,
            losses: 1,
            wrong: false,
        }
    }
}

/// What the replica asked the origin for when it opened a stream.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Ask {
    /// No block yet: the newest rows.
    Tail,
    /// What came after the cursor, under the generation it was taken in.
    After { revision: u64, generation: u64 },
}

/// One message on the stream from the origin to the replica. Items and
/// snapshots travel encoded, so a message can be hashed.
#[derive(Clone, PartialEq, Eq, Hash)]
pub enum Msg {
    /// A stream's opening, as the replica's source lands it: the origin's
    /// generation, whether it replaces the block, the snapshot, the rows,
    /// and the revision the origin is caught up through.
    CatchUp {
        generation: u64,
        reset: bool,
        snapshot: Vec<u8>,
        items: Vec<Vec<u8>>,
        caught_up: u64,
    },
    /// A row committed after the opening.
    Live(Vec<u8>),
}

impl fmt::Debug for Msg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CatchUp {
                generation,
                reset,
                items,
                caught_up,
                ..
            } => {
                let items: Vec<String> = items.iter().map(|item| row(&decode_item(item))).collect();
                let kind = if *reset { "Reset" } else { "Delta" };
                write!(
                    f,
                    "{kind}(generation {generation}, {items:?}, caught up {caught_up})"
                )
            }
            Self::Live(item) => write!(f, "Live({})", row(&decode_item(item))),
        }
    }
}

/// The stream between the two.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Link {
    /// No stream; the replica may open one.
    Idle,
    /// The replica has asked; the origin has not answered.
    Asked(Ask),
    /// The stream is open and carries `queue` in order. Once the origin
    /// has dropped it (a reboot, a lag or a loss), `live` is false: nothing
    /// more is added and the stream ends when the queue drains.
    Open { queue: Vec<Msg>, live: bool },
}

/// What the stores say that decides which actions are enabled, read after
/// every transition.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Seen {
    /// The origin's newest revision.
    minted: u64,
    /// Rows the origin holds.
    origin_rows: usize,
    /// The replica has a block and the origin has not said nothing is older.
    can_page: bool,
}

#[derive(Clone, PartialEq, Eq, Hash)]
pub struct State {
    origin: Dump,
    replica: Dump,
    /// The origin's generation.
    generation: u64,
    /// The origin as it was before its newest commit: what an unclean
    /// reboot rolls it back to.
    durable: Option<Dump>,
    link: Link,
    /// Losses since the link last delivered.
    lost: u8,
    seen: Seen,
    /// A write the store refused; nothing happens after one.
    refused: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    /// The origin commits the key: a new key takes the next order, a held
    /// key keeps its order and takes the next revision.
    Commit { key: &'static str },
    /// The origin's retention drops its oldest row.
    Trim,
    /// The origin loses its newest commit and comes back under the next
    /// generation, minting those revisions again.
    UncleanReboot,
    /// The replica opens a stream: after its cursor, or for a tail.
    Open,
    /// The origin answers the opening: a delta when it fits and the
    /// generation is its own, otherwise a reset and a fresh tail.
    Answer,
    /// The stream delivers its next message to the replica.
    Deliver,
    /// The stream dies at this message: it and everything behind it are
    /// gone.
    Lose(usize),
    /// The replica fetches the page below its block from the origin and
    /// absorbs it.
    AskPage,
}

impl Machine for Replication {
    type State = State;
    type Action = Action;

    fn init(&self) -> State {
        crate::share_sqlite_across_threads();
        let mut origin = Sqlite::open_in_memory(ORIGIN).expect("an origin opens");
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
        let mut replica = Sqlite::open_in_memory(REPLICA).expect("a replica opens");
        replica
            .put_agent(&agent_row())
            .expect("the replica takes the agent's row");
        settle(
            &origin,
            &replica,
            State {
                origin: Dump::default(),
                replica: Dump::default(),
                generation: 1,
                durable: None,
                link: Link::Idle,
                lost: 0,
                seen: Seen {
                    minted: 0,
                    origin_rows: 0,
                    can_page: false,
                },
                refused: None,
            },
            Wrote::BOTH,
        )
    }

    fn actions(&self, s: &State, out: &mut Vec<Action>) {
        if s.refused.is_some() {
            return;
        }
        if s.seen.minted < self.revisions {
            out.extend(
                KEYS[..self.keys]
                    .iter()
                    .map(|&key| Action::Commit { key }),
            );
        }
        if s.seen.origin_rows > 1 {
            out.push(Action::Trim);
        }
        if s.generation < self.generations {
            out.push(Action::UncleanReboot);
        }
        match &s.link {
            Link::Idle => out.push(Action::Open),
            Link::Asked(_) => out.push(Action::Answer),
            Link::Open { queue, .. } => {
                if !queue.is_empty() {
                    out.push(Action::Deliver);
                }
                if s.lost < self.losses {
                    out.extend((0..queue.len()).map(Action::Lose));
                }
            }
        }
        if s.seen.can_page {
            out.push(Action::AskPage);
        }
    }

    fn next(&self, s: &State, action: &Action) -> Option<State> {
        Some(with_scratch(s, |origin, replica| {
            let mut next = s.clone();
            let mut wrote = Wrote::by(action);
            if let Err(error) = self.take(action, origin, replica, &mut next) {
                next.refused = Some(format!("{action:?}: {error}"));
                wrote = Wrote::BOTH;
            }
            if let Link::Open { queue, live: false } = &next.link
                && queue.is_empty()
            {
                next.link = Link::Idle;
            }
            settle(origin, replica, next, wrote)
        }))
    }

    fn properties(&self) -> Vec<Property<Self>> {
        let mut properties: Vec<Property<Self>> = vec![
            Property::always("the store never refuses a write", |_, s: &State| {
                s.refused.is_none()
            }),
            Property::always(
                "the block is contiguous: under the origin's generation, every row the origin holds between \
                 the block's oldest and newest rows is in the block",
                |_, s: &State| {
                    !s.same_generation()
                        || s.view(|origin, replica| {
                            let mine = replica.block(&agent()).unwrap();
                            let (Some(oldest), Some(newest)) = (mine.first(), mine.last()) else {
                                return true;
                            };
                            let from = common_floor(origin, replica).max(oldest.order);
                            origin
                                .suffix_from(&agent(), from)
                                .unwrap()
                                .iter()
                                .filter(|theirs| theirs.order <= newest.order)
                                .all(|theirs| mine.iter().any(|mine| mine.order == theirs.order))
                        })
                },
            ),
            Property::always(
                "the block agrees with the origin: under the origin's generation, every row of the block the \
                 origin still holds is the origin's row at that order, or an older revision of it",
                |_, s: &State| {
                    !s.same_generation()
                        || s.view(|origin, replica| {
                            let theirs = origin.suffix_from(&agent(), 0).unwrap();
                            replica
                                .suffix_from(&agent(), common_floor(origin, replica))
                                .unwrap()
                                .iter()
                                .all(|mine| {
                                    theirs
                                        .iter()
                                        .find(|theirs| theirs.order == mine.order)
                                        .is_some_and(|theirs| no_newer(mine, theirs))
                                })
                        })
                },
            ),
            Property::always(
                "no row survives from another generation: under the origin's generation, every row the \
                 replica stores at an order the origin holds is that order's key, no newer than the origin's, \
                 and a key the origin has since moved to a later order is an older revision of it",
                |_, s: &State| {
                    !s.same_generation()
                        || s.view(|origin, replica| {
                            let theirs = origin.rows(&agent()).unwrap();
                            replica.rows(&agent()).unwrap().iter().all(|mine| {
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
                        })
                },
            ),
            Property::always(
                "the cursor is complete: under the origin's generation, the replica holds every row of its \
                 block's range that the origin revised at or below the cursor",
                |_, s: &State| {
                    !s.same_generation()
                        || s.view(|origin, replica| {
                            let cursor = replica.source_cursor(&agent()).unwrap();
                            let mine = replica.rows(&agent()).unwrap();
                            origin
                                .suffix_from(&agent(), common_floor(origin, replica))
                                .unwrap()
                                .iter()
                                .filter(|theirs| theirs.revision <= cursor)
                                .all(|theirs| mine.iter().any(|mine| mine == theirs))
                        })
                },
            ),
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
                        let row = replica.agent(&agent()).unwrap().unwrap();
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
        if self.wrong {
            properties.push(Property::always(
                "the block is never non-empty",
                |_, s: &State| s.view(|_, replica| replica.block(&agent()).unwrap().is_empty()),
            ));
        }
        properties
    }
}

impl Replication {
    /// One action, calling the real stores as the daemon does.
    fn take(
        &self,
        action: &Action,
        origin: &mut Sqlite,
        replica: &mut Sqlite,
        next: &mut State,
    ) -> Result<(), store::StoreError> {
        let agent = agent();
        match action {
            Action::Commit { key } => {
                next.durable = Some(next.origin.clone());
                let offset = origin.agent(&agent)?.map_or(0, |row| row.ingest_cursor) + 1;
                let item = Item {
                    key: (*key).into(),
                    kind: wire::Kind::Codex as i32,
                    text: format!("{key} in generation {}", next.generation),
                    ..Default::default()
                };
                let step = Step {
                    items: vec![item],
                    ..Default::default()
                };
                let committed = origin.commit(&agent, &[(offset, step)], CLOCK)?;
                if let Link::Open { queue, live } = &mut next.link
                    && *live
                {
                    for record in committed.records {
                        if let store::Record::Item(item) = record {
                            if queue.len() < self.in_flight {
                                queue.push(Msg::Live(item.encode_to_vec()));
                            } else {
                                // The origin drops a subscriber that falls
                                // this far behind.
                                *live = false;
                            }
                        }
                    }
                }
            }
            Action::Trim => {
                let pool = origin.pool_bytes(true)?;
                origin.sweep_own(pool.saturating_sub(1), 1, 1)?;
            }
            Action::UncleanReboot => {
                if let Some(durable) = next.durable.take() {
                    origin.load(&durable)?;
                }
                next.generation += 1;
                next.link = match std::mem::replace(&mut next.link, Link::Idle) {
                    Link::Open { queue, .. } => Link::Open { queue, live: false },
                    Link::Asked(_) | Link::Idle => Link::Idle,
                };
            }
            Action::Open => {
                let row = replica
                    .agent(&agent)?
                    .ok_or(store::StoreError::UnknownAgent)?;
                next.link = Link::Asked(match row.complete_from_order {
                    Some(_) => Ask::After {
                        revision: row.source_cursor,
                        generation: row.source_generation,
                    },
                    None => Ask::Tail,
                });
            }
            Action::Answer => {
                let Link::Asked(ask) = &next.link else {
                    unreachable!("answered without an ask")
                };
                let opening = self.answer(origin, ask, next.generation)?;
                next.link = Link::Open {
                    queue: vec![opening],
                    live: true,
                };
            }
            Action::Deliver => {
                let Link::Open { queue, .. } = &mut next.link else {
                    unreachable!("delivered without a stream")
                };
                let message = queue.remove(0);
                next.lost = 0;
                land(replica, message)?;
            }
            Action::Lose(index) => {
                let Link::Open { queue, live } = &mut next.link else {
                    unreachable!("lost without a stream")
                };
                queue.truncate(*index);
                *live = false;
                next.lost += 1;
            }
            Action::AskPage => {
                let floor = replica
                    .agent(&agent)?
                    .and_then(|row| row.complete_from_order);
                let Some(before_order) = floor else {
                    unreachable!("paged without a block")
                };
                let page = origin.page(&agent, Some(before_order), self.cap)?;
                replica.absorb(
                    &agent,
                    Absorb::Page {
                        before_order,
                        items: page.items,
                        exhausted: page.end == store::PageEnd::Exhausted,
                    },
                )?;
            }
        }
        Ok(())
    }

    /// The opening the origin builds for an ask, as the daemon builds it,
    /// landed as the replica's source lands a catch-up: an ask for a tail
    /// replaces the block whatever the origin answers.
    fn answer(
        &self,
        origin: &Sqlite,
        ask: &Ask,
        generation: u64,
    ) -> Result<Msg, store::StoreError> {
        let agent = agent();
        let delta = match ask {
            Ask::After {
                revision,
                generation: held,
            } if *held == generation => origin.after(&agent, *revision, self.cap)?,
            _ => None,
        };
        let (reset, items, cut) = match delta {
            Some(delta) => (false, delta, origin.cut(&agent, 0)?),
            None => {
                let mut cut = origin.cut(&agent, self.cap)?;
                (true, std::mem::take(&mut cut.held), cut)
            }
        };
        let row = origin.agent(&agent)?.expect("the origin holds its agent");
        let snapshot = cut.snapshot.unwrap_or_else(|| Snapshot {
            agent_id: agent.agent.clone(),
            kind: row.kind,
            ..Default::default()
        });
        Ok(Msg::CatchUp {
            generation,
            reset,
            snapshot: snapshot.encode_to_vec(),
            items: items.iter().map(|item| item.encode_to_vec()).collect(),
            caught_up: row.next_revision.saturating_sub(1),
        })
    }

    /// Nothing more can happen at the origin or on the link but the
    /// replica's own paging: every revision minted, every generation run,
    /// nothing left to trim, and an open stream with nothing in flight.
    fn quiet(&self, s: &State) -> bool {
        s.seen.minted >= self.revisions
            && s.generation >= self.generations
            && s.seen.origin_rows <= 1
            && matches!(&s.link, Link::Open { queue, live: true } if queue.is_empty())
    }
}

/// A catch-up or a live row, absorbed as the replica's source absorbs it.
fn land(replica: &mut Sqlite, message: Msg) -> Result<(), store::StoreError> {
    let agent = agent();
    match message {
        Msg::CatchUp {
            generation,
            reset,
            snapshot,
            items,
            caught_up,
        } => {
            let snapshot = Snapshot::decode(snapshot.as_slice()).expect("a snapshot decodes");
            let items: Vec<Item> = items.iter().map(|item| decode_item(item)).collect();
            if reset {
                replica.absorb(
                    &agent,
                    Absorb::Reset {
                        tail: items,
                        snapshot,
                        generation,
                    },
                )?;
            } else {
                let mut events = vec![SourceEvent::Snapshot(snapshot)];
                events.extend(items.into_iter().map(SourceEvent::Item));
                replica.absorb(
                    &agent,
                    Absorb::Delta {
                        events,
                        live: false,
                    },
                )?;
            }
            replica.absorb(&agent, Absorb::CaughtUp(caught_up))?;
        }
        Msg::Live(item) => {
            let events = vec![SourceEvent::Item(decode_item(&item))];
            replica.absorb(&agent, Absorb::Delta { events, live: true })?;
        }
    }
    Ok(())
}

/// Calls `f` with this thread's two stores holding `s`, to take a
/// transition from it, and returns the state `f` makes. The stores keep
/// their schema between transitions and are loaded only when they do not
/// already hold what `s` holds, which saves most loads: a checker takes
/// every transition from one state in turn.
fn with_scratch(s: &State, f: impl FnOnce(&mut Sqlite, &mut Sqlite) -> State) -> State {
    struct Scratch {
        origin: Sqlite,
        replica: Sqlite,
        holds: (Dump, Dump),
    }
    thread_local! {
        static SCRATCH: RefCell<Option<Scratch>> = const { RefCell::new(None) };
    }
    SCRATCH.with(|scratch| {
        let mut scratch = scratch.borrow_mut();
        let scratch = scratch.get_or_insert_with(|| Scratch {
            origin: store(ORIGIN),
            replica: store(REPLICA),
            holds: (Dump::default(), Dump::default()),
        });
        if scratch.holds.0 != s.origin {
            scratch.origin.load(&s.origin).expect("an origin loads");
        }
        if scratch.holds.1 != s.replica {
            scratch.replica.load(&s.replica).expect("a replica loads");
        }
        let next = f(&mut scratch.origin, &mut scratch.replica);
        scratch.holds = (next.origin.clone(), next.replica.clone());
        next
    })
}

fn store(host: &[u8]) -> Sqlite {
    Sqlite::open_in_memory(host).expect("a store opens in memory")
}

/// The state after a transition: the stores it wrote dumped, and what they
/// say about the enabled actions read back.
fn settle(origin: &Sqlite, replica: &Sqlite, mut next: State, wrote: Wrote) -> State {
    let agent = agent();
    if wrote.origin {
        let row = origin
            .agent(&agent)
            .unwrap()
            .expect("the origin holds its agent");
        next.seen.minted = row.next_revision.saturating_sub(1);
        next.seen.origin_rows = origin.rows(&agent).unwrap().len();
        next.origin = origin.dump().expect("the origin dumps");
    }
    if wrote.replica {
        let row = replica
            .agent(&agent)
            .unwrap()
            .expect("the replica holds the agent's row");
        next.seen.can_page = row.complete_from_order.is_some() && !row.exhausted;
        next.replica = replica.dump().expect("the replica dumps");
    }
    next
}

/// Which stores a transition may have written.
#[derive(Clone, Copy)]
struct Wrote {
    origin: bool,
    replica: bool,
}

impl Wrote {
    const BOTH: Self = Self {
        origin: true,
        replica: true,
    };

    fn by(action: &Action) -> Self {
        match action {
            Action::Commit { .. } | Action::Trim | Action::UncleanReboot => Self {
                origin: true,
                replica: false,
            },
            Action::Deliver | Action::AskPage => Self {
                origin: false,
                replica: true,
            },
            Action::Open | Action::Answer | Action::Lose(_) => Self {
                origin: false,
                replica: false,
            },
        }
    }
}

impl State {
    /// Calls `f` with both stores as this state holds them. A thread keeps
    /// the last state's stores, since every property reads the same state
    /// in turn.
    fn view<R>(&self, f: impl FnOnce(&Sqlite, &Sqlite) -> R) -> R {
        thread_local! {
            static HELD: RefCell<Option<(Dump, Dump, Sqlite, Sqlite)>> = const { RefCell::new(None) };
        }
        HELD.with(|held| {
            let mut held = held.borrow_mut();
            let (origin_dump, replica_dump, origin, replica) = held.get_or_insert_with(|| {
                (
                    Dump::default(),
                    Dump::default(),
                    store(ORIGIN),
                    store(REPLICA),
                )
            });
            if *origin_dump != self.origin || *replica_dump != self.replica {
                origin.load(&self.origin).expect("an origin loads");
                replica.load(&self.replica).expect("a replica loads");
                *origin_dump = self.origin.clone();
                *replica_dump = self.replica.clone();
            }
            f(origin, replica)
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
            let replica_row = replica.agent(&agent).unwrap().unwrap();
            let origin_row = origin.agent(&agent).unwrap().unwrap();
            f.debug_struct("State")
                .field("generation", &self.generation)
                .field("origin", &rows(origin))
                .field("origin_from", &origin_row.complete_from_order)
                .field("replica", &rows(replica))
                .field("replica_from", &replica_row.complete_from_order)
                .field("source_cursor", &replica_row.source_cursor)
                .field("source_generation", &replica_row.source_generation)
                .field("link", &self.link)
                .field("refused", &self.refused)
                .finish()
        })
    }
}

/// The order the comparison of two blocks starts from: neither store's
/// rows below it can be compared, the replica's because they are not its
/// block, the origin's because it no longer holds them.
fn common_floor(origin: &Sqlite, replica: &Sqlite) -> u64 {
    let floor = |store: &Sqlite| store.block_orders(&agent()).unwrap().from.unwrap_or(0);
    floor(origin).max(floor(replica))
}

/// The replica's row is the origin's key at the origin's order, and either
/// the origin's row itself or an older revision of it.
fn no_newer(mine: &Item, theirs: &Item) -> bool {
    mine.order == theirs.order
        && mine.key == theirs.key
        && (mine.revision < theirs.revision || mine == theirs)
}

fn decode_item(bytes: &[u8]) -> Item {
    Item::decode(bytes).expect("an item decodes")
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
