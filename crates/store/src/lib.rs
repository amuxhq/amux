//! The profile store: one database per profile, the truth for this host's
//! own agents and a cache of its peers' agents, and the only history any
//! client on the machine reads, through its runtime.
//!
//! Own rows are written by [`Store::commit`] from an agent's journal: the
//! store assigns every record the agent's next revision and every new key
//! the next order, in journal order. Replica rows are written only by
//! [`Store::absorb`], which copies the origin's revisions and orders and
//! keeps the replica's rows one contiguous block ending at the origin's
//! newest (see [`Absorb`]). Both are single transactions.
//!
//! The logic lives here once, over the [`Tables`] primitives; [`Sqlite`]
//! (the profile file) and [`InMemory`] (tests, replay) differ only in how
//! they keep rows, so one conformance suite holds both to the same reads.

use std::collections::{HashMap, HashSet};

use prost::Message as _;
use serde::{Deserialize, Serialize};
pub use wire::{Append, Attachment, Item, Snapshot, Step};

mod memory;
mod migrations;
mod sqlite;

mod blobs;
mod retention;

pub use blobs::BlobLru;
pub use memory::InMemory;
pub use migrations::{MIGRATIONS, SCHEMA_STAMP, migration_hash};
pub use retention::{Sweep, SweepStep};
pub use sqlite::{OpenError, Sqlite};

/// An agent by the host that owns it and its id.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AgentKey {
    pub host: Vec<u8>,
    pub agent: Vec<u8>,
}

impl AgentKey {
    pub fn new(host: impl Into<Vec<u8>>, agent: impl Into<Vec<u8>>) -> Self {
        Self {
            host: host.into(),
            agent: agent.into(),
        }
    }
}

/// The agents row. Kind, lifecycle and phase are kept as their wire
/// integers so a value from a newer peer is stored and served, never
/// refused.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentRow {
    pub agent: AgentKey,
    pub kind: i32,
    pub name: Option<String>,
    pub cwd: String,
    pub parent: Option<AgentKey>,
    pub lifecycle: i32,
    pub exit_cause: Option<String>,
    /// Copied from the snapshot envelope at commit.
    pub phase: i32,
    pub working_on: Option<String>,
    /// The newest snapshot's at_ms, never commit time.
    pub last_activity: Option<i64>,
    /// Copied from the snapshot envelope at commit: when the phase began.
    pub phase_since: Option<i64>,
    /// Copied from the snapshot envelope at commit.
    pub git: Option<wire::Git>,
    pub snapshot: Option<Snapshot>,
    pub snapshot_revision: u64,
    /// Own rows: the journal offset committed through.
    pub ingest_cursor: u64,
    pub next_revision: u64,
    /// Replica rows: the origin revision the source is complete through.
    pub source_cursor: u64,
    /// Replica rows: the origin generation the block was taken under, from
    /// the Opening of the stream whose Reset began it. The origin answers a
    /// cursor of another generation with a Reset.
    pub source_generation: u64,
    /// The block runs from this order to the newest held row. Replicas:
    /// none means no block; set by a Reset, moved down by joining pages.
    /// Own rows: none means all history is held; set only by retention.
    pub complete_from_order: Option<u64>,
    /// Replicas: the origin said no older history exists. Own rows:
    /// retention trimmed the older history away.
    pub exhausted: bool,
    pub created_at: i64,
    pub producer_version: String,
    pub incarnation: u32,
    /// Own rows: the incarnation has a turn it has not ended, from its
    /// start until its first turn end and again whenever a later turn
    /// starts. An incarnation that exits with one open tells its parent it
    /// failed; one that ended its turn already told it it finished.
    pub turn_open: bool,
}

impl AgentRow {
    /// A new live row in phase starting, with nothing committed yet.
    pub fn new(agent: AgentKey, kind: impl Into<i32>, cwd: impl Into<String>) -> Self {
        Self {
            agent,
            kind: kind.into(),
            name: None,
            cwd: cwd.into(),
            parent: None,
            lifecycle: wire::Lifecycle::Live as i32,
            exit_cause: None,
            phase: wire::Phase::Starting as i32,
            working_on: None,
            last_activity: None,
            phase_since: None,
            git: None,
            snapshot: None,
            snapshot_revision: 0,
            ingest_cursor: 0,
            next_revision: 1,
            source_cursor: 0,
            source_generation: 0,
            complete_from_order: None,
            exhausted: false,
            created_at: 0,
            producer_version: String::new(),
            incarnation: 1,
            turn_open: true,
        }
    }
}

/// One committed or absorbed record, as it is broadcast after the
/// transaction: items with their order and revision, appends with the base
/// revision a client must hold to apply them.
#[derive(Clone, Debug, PartialEq)]
pub enum Record {
    Item(Item),
    Append(Append),
    Snapshot(Snapshot),
}

impl Record {
    pub fn revision(&self) -> u64 {
        match self {
            Self::Item(item) => item.revision,
            Self::Append(append) => append.revision,
            Self::Snapshot(snapshot) => snapshot.revision,
        }
    }
}

/// What the daemon needs from the clock and its parameters at commit.
#[derive(Clone, Copy, Debug)]
pub struct CommitClock {
    pub now_ms: i64,
    /// How long a notification waits before it is sent, so an answer from
    /// the desktop cancels it.
    pub notify_delay_ms: i64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Committed {
    pub records: Vec<Record>,
    /// The journal offset the row is now committed through.
    pub cursor: u64,
    /// Appends naming a key the store does not hold; the item they extend
    /// was never committed, so there is nothing to extend.
    pub skipped_appends: usize,
}

/// A record from a peer source.
#[derive(Clone, Debug, PartialEq)]
pub enum SourceEvent {
    Item(Item),
    Append(Append),
    Snapshot(Snapshot),
}

/// The one write primitive for replica rows. Built per call and consumed at
/// once, so the snapshot a reset carries is not worth boxing.
#[derive(Clone, Debug, PartialEq)]
#[allow(clippy::large_enum_variant)]
pub enum Absorb {
    /// Records that join the block above its newest row. `live` is false
    /// during a catch-up, which leaves the source cursor alone; live
    /// records advance it to their revision.
    Delta {
        events: Vec<SourceEvent>,
        live: bool,
    },
    /// Forget the block: the tail replaces it and complete_from_order moves
    /// to the tail's oldest order. Rows below stay stored for Get but no
    /// longer count, unless `generation` is not the one the block was
    /// taken under: then every held row goes, because the origin numbers
    /// orders and revisions afresh after an unclean reboot and nothing
    /// held compares with the tail.
    Reset {
        tail: Vec<Item>,
        snapshot: Snapshot,
        generation: u64,
    },
    /// An origin page for orders below `before_order`. Stored only when it
    /// joins the block, that is when `before_order` is the block's
    /// boundary; otherwise it is served to its requester and dropped,
    /// because only a source starts a block.
    Page {
        before_order: u64,
        items: Vec<Item>,
        exhausted: bool,
    },
    /// The origin's replay is complete through this revision.
    CaughtUp(u64),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Absorbed {
    /// Records that changed a row, in the order applied.
    pub stored: Vec<Record>,
    /// False for a page that did not join the block and was not stored.
    pub joined: bool,
}

/// The marker a subscriber reads once at join, after its held rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Marker {
    CaughtUp,
    Detached,
}

/// A subscription's opening: one point in the commit sequence.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Cut {
    pub snapshot: Option<Snapshot>,
    /// The newest rows of the block, oldest first.
    pub held: Vec<Item>,
    pub marker: Option<Marker>,
}

/// How a page ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageEnd {
    /// The limit was reached; ask again below the oldest item.
    More,
    /// No older history exists anywhere.
    Exhausted,
    /// The block ends here; older history is at the origin.
    Boundary,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Page {
    /// Newest first.
    pub items: Vec<Item>,
    pub end: PageEnd,
}

/// The outbox row that tells a parent its child finished or failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Delivery {
    pub child_id: Vec<u8>,
    pub incarnation: u32,
    pub turn_id: u64,
    pub parent: AgentKey,
    /// The parent's incarnation when the row was written; the receiving
    /// daemon drops a row for any other. Zero when the parent's row was not
    /// held then, which incarnations never are: unknown, and stamped by the
    /// drain once the parent's row is held, never sent as it is.
    pub parent_incarnation: u32,
    /// `wire::EnvelopeKind` as an integer: finished or failed.
    pub kind: i32,
    /// The child's last message, or the failure's cause.
    pub body: String,
}

/// The push outbox row for one turn into needs_you.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notification {
    pub agent_id: Vec<u8>,
    pub revision: u64,
    pub due_at: i64,
    pub body: NotificationBody,
}

/// Envelope fields only: nothing here needs a body decoded.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationBody {
    pub name: Option<String>,
    pub working_on: Option<String>,
    pub text: String,
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("no agent row for this agent")]
    UnknownAgent,
    #[error("commit writes own rows only")]
    NotOwn,
    #[error("absorb writes replica rows only")]
    NotReplica,
    #[error("a host's replicas cannot be rewound on the host itself")]
    OwnHost,
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("stored data does not decode: {0}")]
    Corrupt(String),
}

/// The primitive row operations, inside one transaction. Everything else
/// is written once, over these.
pub trait Tables {
    fn agent(&self, agent: &AgentKey) -> Result<Option<AgentRow>, StoreError>;
    fn put_agent(&mut self, row: &AgentRow) -> Result<(), StoreError>;
    /// Removes the row, its items, its deliveries and its notifications.
    fn remove_agent(&mut self, agent: &AgentKey) -> Result<(), StoreError>;
    fn item(&self, agent: &AgentKey, key: &str) -> Result<Option<Item>, StoreError>;
    /// An item whose input id is `input_id`, if the agent holds one.
    fn item_by_input(&self, agent: &AgentKey, input_id: &[u8]) -> Result<Option<Item>, StoreError>;
    /// Inserts or replaces by key. A second key at an order already taken
    /// is an error: one key per order.
    fn put_item(&mut self, agent: &AgentKey, item: &Item) -> Result<(), StoreError>;
    fn max_order(&self, agent: &AgentKey) -> Result<Option<u64>, StoreError>;
    /// Items with `min_order <= order < below`, newest first.
    fn items_desc(
        &self,
        agent: &AgentKey,
        below: Option<u64>,
        min_order: u64,
        limit: u32,
    ) -> Result<Vec<Item>, StoreError>;
    /// Items whose revision is above `revision`, oldest revision first.
    fn items_after(
        &self,
        agent: &AgentKey,
        revision: u64,
        limit: u32,
    ) -> Result<Vec<Item>, StoreError>;
    fn put_delivery(&mut self, delivery: &Delivery) -> Result<(), StoreError>;
    fn deliveries(&self) -> Result<Vec<Delivery>, StoreError>;
    fn remove_delivery(
        &mut self,
        child_id: &[u8],
        incarnation: u32,
        kind: i32,
        turn_id: u64,
    ) -> Result<(), StoreError>;
    fn put_notification(&mut self, notification: &Notification) -> Result<(), StoreError>;
    fn notifications(&self) -> Result<Vec<Notification>, StoreError>;
    fn remove_notifications(&mut self, agent_id: &[u8]) -> Result<(), StoreError>;
    fn remove_notification(&mut self, agent_id: &[u8], revision: u64) -> Result<(), StoreError>;
    fn host_generation(&self, host: &[u8]) -> Result<Option<u64>, StoreError>;
    fn set_host_generation(&mut self, host: &[u8], generation: u64) -> Result<(), StoreError>;
    fn agents(&self) -> Result<Vec<AgentRow>, StoreError>;
    /// The agent's held rows: their total [`item_bytes`] and count.
    fn agent_bytes(&self, agent: &AgentKey) -> Result<(u64, u64), StoreError>;
    /// The oldest held rows' orders and [`item_bytes`], oldest first.
    fn oldest_items(&self, agent: &AgentKey, limit: u32) -> Result<Vec<(u64, u64)>, StoreError>;
    fn remove_items_below(&mut self, agent: &AgentKey, order: u64) -> Result<(), StoreError>;
}

/// What one held row costs against a retention budget: its envelope's
/// variable fields plus a fixed allowance for the integers and the index
/// entries beside them.
pub fn item_bytes(item: &Item) -> u64 {
    (item.key.len()
        + item.text.len()
        + item.body.len()
        + encode_attachments(&item.attachments).len()
        + item.input_id.len()
        + item.producer_version.len()) as u64
        + ITEM_OVERHEAD_BYTES
}

/// The fixed per-row allowance in [`item_bytes`], the kind's integer among
/// it.
pub const ITEM_OVERHEAD_BYTES: u64 = 48;

/// How a store keeps its rows: transactions over [`Tables`] and the
/// in-memory marker flags.
pub trait Backend {
    fn own_host(&self) -> &[u8];
    fn read<R>(
        &self,
        f: impl FnOnce(&dyn Tables) -> Result<R, StoreError>,
    ) -> Result<R, StoreError>;
    /// Runs `f` in one transaction: all of it lands or none of it does.
    fn write<R>(
        &mut self,
        f: impl FnOnce(&mut dyn Tables) -> Result<R, StoreError>,
    ) -> Result<R, StoreError>;
    fn markers(&self) -> &HashMap<AgentKey, Marker>;
    fn markers_mut(&mut self) -> &mut HashMap<AgentKey, Marker>;
}

/// The runtime's interface to its store.
pub trait Store {
    fn own_host(&self) -> &[u8];
    fn agent(&self, agent: &AgentKey) -> Result<Option<AgentRow>, StoreError>;
    /// Every agents row, own and replica.
    fn agents(&self) -> Result<Vec<AgentRow>, StoreError>;
    /// Items by order, newest first, from the block only.
    fn page(
        &self,
        agent: &AgentKey,
        before_order: Option<u64>,
        limit: u32,
    ) -> Result<Page, StoreError>;
    /// One held item, in the block or not.
    fn get(&self, agent: &AgentKey, key: &str) -> Result<Option<Item>, StoreError>;
    /// The item that carries `input_id`: for an agent message, the proof
    /// that its recipient accepted it.
    fn item_by_input(&self, agent: &AgentKey, input_id: &[u8]) -> Result<Option<Item>, StoreError>;
    /// The newest `n` rows of the block, oldest first.
    fn last_n(&self, agent: &AgentKey, n: u32) -> Result<Vec<Item>, StoreError>;
    /// Replica rows: the source cursor. Own rows: the ingest cursor.
    fn cursor(&self, agent: &AgentKey) -> Result<u64, StoreError>;
    /// The snapshot, the newest `n` rows and the marker, read together.
    fn cut(&self, agent: &AgentKey, n: u32) -> Result<Cut, StoreError>;
    /// What a peer that holds everything through `revision` is missing:
    /// every row revised since, in revision order, when there are at most
    /// `cap` of them; None when there are more, and a tail is the answer.
    fn after(
        &self,
        agent: &AgentKey,
        revision: u64,
        cap: u32,
    ) -> Result<Option<Vec<Item>>, StoreError>;
    fn set_marker(&mut self, agent: &AgentKey, marker: Option<Marker>);

    /// Creates a row or updates its registry fields: kind, name, cwd,
    /// parent, lifecycle, exit cause, creation time, producer version and
    /// incarnation, and for a replica the phase, working_on and last
    /// activity its inventory row carries. Committed state is kept, except
    /// that an own row moving to a new incarnation is back in phase
    /// starting: the new process has said nothing yet.
    fn put_agent(&mut self, row: &AgentRow) -> Result<(), StoreError>;
    /// Removes an agent whole: row, items, deliveries and notifications.
    fn delete_agent(&mut self, agent: &AgentKey) -> Result<(), StoreError>;
    /// Commits journal frames to an own row in one transaction.
    fn commit(
        &mut self,
        agent: &AgentKey,
        frames: &[(u64, Step)],
        clock: CommitClock,
    ) -> Result<Committed, StoreError>;
    /// Moves an own row's ingest cursor back to where its journal now
    /// ends, after a power cut took the journal's tail but not the store's
    /// commit of it. Everything committed stays, and so does the next
    /// revision: what the agent writes again is new to every reader.
    fn rewind_cursor(&mut self, agent: &AgentKey, cursor: u64) -> Result<(), StoreError>;
    fn absorb(&mut self, agent: &AgentKey, what: Absorb) -> Result<Absorbed, StoreError>;
    /// Records the generation `host`'s inventory last reported.
    fn record_host_generation(&mut self, host: &[u8], generation: u64) -> Result<(), StoreError>;
    fn host_generation(&self, host: &[u8]) -> Result<Option<u64>, StoreError>;

    /// The bytes own rows (or replica rows) hold against their budget.
    fn pool_bytes(&self, own: bool) -> Result<u64, StoreError>;
    /// Brings own rows under `budget` bytes: exited agents whole, least
    /// recently active first, never an exited child whose parent's row says
    /// live; then the largest remaining agent trimmed by about `chunk`
    /// bytes per round, never below its newest `floor_k` rows. Returns what
    /// it removed, so the daemon deletes the removed agents' directories.
    fn sweep_own(&mut self, budget: u64, chunk: u64, floor_k: u32) -> Result<Sweep, StoreError>;
    /// Brings replica rows under `budget` bytes: every row of agents with
    /// no open source, least recently used first by `last_used` (the
    /// runtime's clock; the row's last activity where it has no entry),
    /// keeping their agents rows with an empty block; then sourced agents
    /// trimmed to their newest `floor_k` rows, largest first.
    fn sweep_replicas(
        &mut self,
        budget: u64,
        floor_k: u32,
        sourced: &HashSet<AgentKey>,
        last_used: &HashMap<AgentKey, i64>,
    ) -> Result<Sweep, StoreError>;

    fn put_delivery(&mut self, delivery: &Delivery) -> Result<(), StoreError>;
    fn deliveries(&self) -> Result<Vec<Delivery>, StoreError>;
    fn remove_delivery(&mut self, delivery: &Delivery) -> Result<(), StoreError>;
    fn notifications(&self) -> Result<Vec<Notification>, StoreError>;
    fn remove_notifications(&mut self, agent_id: &[u8]) -> Result<(), StoreError>;
    /// Removes one notification: the one a sender just sent.
    fn remove_notification(&mut self, notification: &Notification) -> Result<(), StoreError>;
}

fn is_own(backend: &impl Backend, agent: &AgentKey) -> bool {
    agent.host == backend.own_host()
}

/// The lowest order the block counts from.
fn block_floor(row: &AgentRow, own: bool) -> Option<u64> {
    match (own, row.complete_from_order) {
        (_, Some(order)) => Some(order),
        (true, None) => Some(0),
        (false, None) => None,
    }
}

impl<B: Backend> Store for B {
    fn own_host(&self) -> &[u8] {
        Backend::own_host(self)
    }

    fn agent(&self, agent: &AgentKey) -> Result<Option<AgentRow>, StoreError> {
        self.read(|tables| tables.agent(agent))
    }

    fn agents(&self) -> Result<Vec<AgentRow>, StoreError> {
        self.read(|tables| tables.agents())
    }

    fn page(
        &self,
        agent: &AgentKey,
        before_order: Option<u64>,
        limit: u32,
    ) -> Result<Page, StoreError> {
        let own = is_own(self, agent);
        self.read(|tables| {
            let row = tables.agent(agent)?.ok_or(StoreError::UnknownAgent)?;
            let bottom = if own || row.exhausted {
                PageEnd::Exhausted
            } else {
                PageEnd::Boundary
            };
            let Some(floor) = block_floor(&row, own) else {
                return Ok(Page {
                    items: Vec::new(),
                    end: PageEnd::Boundary,
                });
            };
            if limit == 0 {
                return Ok(Page {
                    items: Vec::new(),
                    end: PageEnd::More,
                });
            }
            let items = tables.items_desc(agent, before_order, floor, limit)?;
            let end = if items.len() == limit as usize {
                PageEnd::More
            } else {
                bottom
            };
            Ok(Page { items, end })
        })
    }

    fn get(&self, agent: &AgentKey, key: &str) -> Result<Option<Item>, StoreError> {
        self.read(|tables| tables.item(agent, key))
    }

    fn item_by_input(&self, agent: &AgentKey, input_id: &[u8]) -> Result<Option<Item>, StoreError> {
        if input_id.is_empty() {
            return Ok(None);
        }
        self.read(|tables| tables.item_by_input(agent, input_id))
    }

    fn last_n(&self, agent: &AgentKey, n: u32) -> Result<Vec<Item>, StoreError> {
        let own = is_own(self, agent);
        self.read(|tables| last_n(tables, agent, own, n))
    }

    fn cursor(&self, agent: &AgentKey) -> Result<u64, StoreError> {
        let own = is_own(self, agent);
        let row = self.agent(agent)?.ok_or(StoreError::UnknownAgent)?;
        Ok(if own {
            row.ingest_cursor
        } else {
            row.source_cursor
        })
    }

    fn cut(&self, agent: &AgentKey, n: u32) -> Result<Cut, StoreError> {
        let own = is_own(self, agent);
        let marker = self.markers().get(agent).copied();
        self.read(|tables| {
            let row = tables.agent(agent)?.ok_or(StoreError::UnknownAgent)?;
            Ok(Cut {
                snapshot: row.snapshot,
                held: last_n(tables, agent, own, n)?,
                marker,
            })
        })
    }

    fn after(
        &self,
        agent: &AgentKey,
        revision: u64,
        cap: u32,
    ) -> Result<Option<Vec<Item>>, StoreError> {
        self.read(|tables| {
            let items = tables.items_after(agent, revision, cap.saturating_add(1))?;
            Ok((items.len() <= cap as usize).then_some(items))
        })
    }

    fn set_marker(&mut self, agent: &AgentKey, marker: Option<Marker>) {
        match marker {
            Some(marker) => {
                self.markers_mut().insert(agent.clone(), marker);
            }
            None => {
                self.markers_mut().remove(agent);
            }
        }
    }

    fn put_agent(&mut self, row: &AgentRow) -> Result<(), StoreError> {
        let own = is_own(self, &row.agent);
        self.write(|tables| {
            let merged = match tables.agent(&row.agent)? {
                None => row.clone(),
                Some(mut held) => {
                    held.kind = row.kind;
                    held.name = row.name.clone();
                    held.cwd = row.cwd.clone();
                    held.parent = row.parent.clone();
                    held.lifecycle = row.lifecycle;
                    held.exit_cause = row.exit_cause.clone();
                    held.created_at = row.created_at;
                    held.producer_version = row.producer_version.clone();
                    if own && row.incarnation > held.incarnation {
                        held.phase = wire::Phase::Starting as i32;
                        held.turn_open = true;
                    }
                    held.incarnation = row.incarnation;
                    if !own {
                        held.phase = row.phase;
                        held.working_on = row.working_on.clone();
                        held.last_activity = row.last_activity;
                        held.phase_since = row.phase_since;
                        held.git = row.git.clone();
                    }
                    held
                }
            };
            tables.put_agent(&merged)
        })
    }

    fn delete_agent(&mut self, agent: &AgentKey) -> Result<(), StoreError> {
        self.markers_mut().remove(agent);
        self.write(|tables| tables.remove_agent(agent))
    }

    fn commit(
        &mut self,
        agent: &AgentKey,
        frames: &[(u64, Step)],
        clock: CommitClock,
    ) -> Result<Committed, StoreError> {
        if !is_own(self, agent) {
            return Err(StoreError::NotOwn);
        }
        self.write(|tables| commit(tables, agent, frames, clock))
    }

    fn rewind_cursor(&mut self, agent: &AgentKey, cursor: u64) -> Result<(), StoreError> {
        if !is_own(self, agent) {
            return Err(StoreError::NotOwn);
        }
        self.write(|tables| {
            let mut row = tables.agent(agent)?.ok_or(StoreError::UnknownAgent)?;
            row.ingest_cursor = row.ingest_cursor.min(cursor);
            tables.put_agent(&row)
        })
    }

    fn absorb(&mut self, agent: &AgentKey, what: Absorb) -> Result<Absorbed, StoreError> {
        if is_own(self, agent) {
            return Err(StoreError::NotReplica);
        }
        let marker = match &what {
            Absorb::CaughtUp(_) => Some(Some(Marker::CaughtUp)),
            Absorb::Reset { .. } => Some(None),
            _ => None,
        };
        let absorbed = self.write(|tables| absorb(tables, agent, what))?;
        if let Some(marker) = marker {
            self.set_marker(agent, marker);
        }
        Ok(absorbed)
    }

    fn record_host_generation(&mut self, host: &[u8], generation: u64) -> Result<(), StoreError> {
        if host == Backend::own_host(self) {
            return Err(StoreError::OwnHost);
        }
        self.write(|tables| tables.set_host_generation(host, generation))
    }

    fn host_generation(&self, host: &[u8]) -> Result<Option<u64>, StoreError> {
        self.read(|tables| tables.host_generation(host))
    }

    fn pool_bytes(&self, own: bool) -> Result<u64, StoreError> {
        let own_host = Backend::own_host(self).to_vec();
        self.read(|tables| retention::pool_bytes(tables, &own_host, own))
    }

    fn sweep_own(&mut self, budget: u64, chunk: u64, floor_k: u32) -> Result<Sweep, StoreError> {
        let own_host = Backend::own_host(self).to_vec();
        let sweep =
            self.write(|tables| retention::sweep_own(tables, &own_host, budget, chunk, floor_k))?;
        for agent in &sweep.removed {
            self.markers_mut().remove(agent);
        }
        Ok(sweep)
    }

    fn sweep_replicas(
        &mut self,
        budget: u64,
        floor_k: u32,
        sourced: &HashSet<AgentKey>,
        last_used: &HashMap<AgentKey, i64>,
    ) -> Result<Sweep, StoreError> {
        let own_host = Backend::own_host(self).to_vec();
        let sweep = self.write(|tables| {
            retention::sweep_replicas(tables, &own_host, budget, floor_k, sourced, last_used)
        })?;
        for agent in &sweep.removed {
            self.markers_mut().remove(agent);
        }
        Ok(sweep)
    }

    fn put_delivery(&mut self, delivery: &Delivery) -> Result<(), StoreError> {
        self.write(|tables| tables.put_delivery(delivery))
    }

    fn deliveries(&self) -> Result<Vec<Delivery>, StoreError> {
        self.read(|tables| tables.deliveries())
    }

    fn remove_delivery(&mut self, delivery: &Delivery) -> Result<(), StoreError> {
        self.write(|tables| {
            tables.remove_delivery(
                &delivery.child_id,
                delivery.incarnation,
                delivery.kind,
                delivery.turn_id,
            )
        })
    }

    fn notifications(&self) -> Result<Vec<Notification>, StoreError> {
        self.read(|tables| tables.notifications())
    }

    fn remove_notifications(&mut self, agent_id: &[u8]) -> Result<(), StoreError> {
        self.write(|tables| tables.remove_notifications(agent_id))
    }

    fn remove_notification(&mut self, notification: &Notification) -> Result<(), StoreError> {
        self.write(|tables| {
            tables.remove_notification(&notification.agent_id, notification.revision)
        })
    }
}

fn last_n(
    tables: &dyn Tables,
    agent: &AgentKey,
    own: bool,
    n: u32,
) -> Result<Vec<Item>, StoreError> {
    let row = tables.agent(agent)?.ok_or(StoreError::UnknownAgent)?;
    let Some(floor) = block_floor(&row, own) else {
        return Ok(Vec::new());
    };
    let mut items = tables.items_desc(agent, None, floor, n)?;
    items.reverse();
    Ok(items)
}

fn commit(
    tables: &mut dyn Tables,
    agent: &AgentKey,
    frames: &[(u64, Step)],
    clock: CommitClock,
) -> Result<Committed, StoreError> {
    let mut row = tables.agent(agent)?.ok_or(StoreError::UnknownAgent)?;
    let mut committed = Committed::default();
    let mut next_order = tables.max_order(agent)?.map_or(1, |order| order + 1);
    for (offset, step) in frames {
        for item in &step.items {
            let mut item = item.clone();
            item.agent = agent.agent.clone();
            item.revision = row.next_revision;
            row.next_revision += 1;
            item.order = match tables.item(agent, &item.key)? {
                Some(held) => held.order,
                None => {
                    next_order += 1;
                    next_order - 1
                }
            };
            tables.put_item(agent, &item)?;
            committed.records.push(Record::Item(item));
        }
        for append in &step.appends {
            let Some(mut held) = tables.item(agent, &append.key)? else {
                committed.skipped_appends += 1;
                continue;
            };
            let base_revision = held.revision;
            held.text.push_str(&append.text);
            held.revision = row.next_revision;
            row.next_revision += 1;
            tables.put_item(agent, &held)?;
            committed.records.push(Record::Append(Append {
                agent: agent.agent.clone(),
                key: append.key.clone(),
                base_revision,
                revision: held.revision,
                text: append.text.clone(),
            }));
        }
        if let Some(snapshot) = &step.snapshot {
            let mut snapshot = snapshot.clone();
            snapshot.agent = agent.agent.clone();
            snapshot.revision = row.next_revision;
            row.next_revision += 1;
            let was = row.phase;
            copy_envelope(&mut row, &snapshot);
            let needs_you = wire::Phase::NeedsYou as i32;
            if snapshot.phase == needs_you && was != needs_you {
                let text = newest_text(tables, agent)?;
                tables.put_notification(&Notification {
                    agent_id: agent.agent.clone(),
                    revision: snapshot.revision,
                    due_at: clock.now_ms + clock.notify_delay_ms,
                    body: NotificationBody {
                        name: row.name.clone(),
                        working_on: row.working_on.clone(),
                        text,
                    },
                })?;
            } else if snapshot.phase != needs_you && was == needs_you {
                tables.remove_notifications(&agent.agent)?;
            }
            committed.records.push(Record::Snapshot(snapshot));
        }
        if let (Some(turn_end), Some(parent)) = (&step.turn_end, &row.parent) {
            let body = if turn_end.last_message_key.is_empty() {
                String::new()
            } else {
                tables
                    .item(agent, &turn_end.last_message_key)?
                    .map(|item| item.text)
                    .unwrap_or_default()
            };
            // Zero when the parent's row is not held: unknown, see Delivery.
            let parent_incarnation = tables.agent(parent)?.map_or(0, |parent| parent.incarnation);
            tables.put_delivery(&Delivery {
                child_id: agent.agent.clone(),
                incarnation: row.incarnation,
                turn_id: turn_end.turn_id,
                parent: parent.clone(),
                parent_incarnation,
                kind: wire::EnvelopeKind::Finished as i32,
                body,
            })?;
        }
        // A step that ends one turn may start the next; the snapshot says
        // where the step leaves the agent.
        if step.turn_end.is_some() {
            row.turn_open = false;
        }
        if let Some(snapshot) = &step.snapshot
            && (snapshot.phase == wire::Phase::Working as i32
                || snapshot.phase == wire::Phase::NeedsYou as i32)
        {
            row.turn_open = true;
        }
        row.ingest_cursor = *offset;
    }
    committed.cursor = row.ingest_cursor;
    tables.put_agent(&row)?;
    Ok(committed)
}

/// Copies the envelope fields a snapshot carries onto its row.
fn copy_envelope(row: &mut AgentRow, snapshot: &Snapshot) {
    row.phase = snapshot.phase;
    row.working_on = snapshot.working_on.clone();
    row.last_activity = Some(snapshot.at_ms);
    row.phase_since = Some(snapshot.phase_since_ms);
    row.git = snapshot.git.clone();
    row.snapshot_revision = snapshot.revision;
    row.snapshot = Some(snapshot.clone());
}

fn newest_text(tables: &dyn Tables, agent: &AgentKey) -> Result<String, StoreError> {
    Ok(tables
        .items_desc(agent, None, 0, 1)?
        .into_iter()
        .next()
        .map(|item| item.text)
        .unwrap_or_default())
}

/// Stores an item unless the row already holds a newer revision of its key.
fn upsert_newer(
    tables: &mut dyn Tables,
    agent: &AgentKey,
    item: &Item,
) -> Result<bool, StoreError> {
    if let Some(held) = tables.item(agent, &item.key)?
        && held.revision >= item.revision
    {
        return Ok(false);
    }
    let mut item = item.clone();
    item.agent = agent.agent.clone();
    tables.put_item(agent, &item)?;
    Ok(true)
}

fn absorb(tables: &mut dyn Tables, agent: &AgentKey, what: Absorb) -> Result<Absorbed, StoreError> {
    let mut row = tables.agent(agent)?.ok_or(StoreError::UnknownAgent)?;
    let mut absorbed = Absorbed {
        stored: Vec::new(),
        joined: true,
    };
    match what {
        Absorb::Delta { events, live } => {
            for event in events {
                let record = match event {
                    SourceEvent::Item(item) => {
                        upsert_newer(tables, agent, &item)?.then_some(Record::Item(item))
                    }
                    SourceEvent::Append(append) => match tables.item(agent, &append.key)? {
                        Some(mut held) if held.revision == append.base_revision => {
                            held.text.push_str(&append.text);
                            held.revision = append.revision;
                            tables.put_item(agent, &held)?;
                            Some(Record::Append(append))
                        }
                        // The stream ends with the full item; nothing to do.
                        _ => None,
                    },
                    SourceEvent::Snapshot(snapshot) => (snapshot.revision > row.snapshot_revision)
                        .then(|| {
                            copy_envelope(&mut row, &snapshot);
                            Record::Snapshot(snapshot)
                        }),
                };
                if let Some(record) = record {
                    if live {
                        row.source_cursor = row.source_cursor.max(record.revision());
                    }
                    absorbed.stored.push(record);
                }
            }
        }
        Absorb::Reset {
            tail,
            snapshot,
            generation,
        } => {
            let fresh = generation != row.source_generation;
            if fresh {
                if let Some(max) = tables.max_order(agent)? {
                    tables.remove_items_below(agent, max + 1)?;
                }
                row.snapshot_revision = 0;
                row.source_cursor = 0;
                row.source_generation = generation;
            }
            let floor = match tail.iter().map(|item| item.order).min() {
                Some(order) => order,
                None => tables.max_order(agent)?.map_or(0, |order| order + 1),
            };
            for item in tail {
                if upsert_newer(tables, agent, &item)? {
                    absorbed.stored.push(Record::Item(item));
                }
            }
            row.complete_from_order = Some(floor);
            row.exhausted = false;
            // Under a new generation the envelope is the origin's whatever
            // its revision: a snapshot numbered 0 is still the one it holds.
            if fresh || snapshot.revision > row.snapshot_revision {
                copy_envelope(&mut row, &snapshot);
                absorbed.stored.push(Record::Snapshot(snapshot));
            }
        }
        Absorb::Page {
            before_order,
            items,
            exhausted,
        } => {
            if row.complete_from_order != Some(before_order)
                || items.iter().any(|item| item.order >= before_order)
            {
                absorbed.joined = false;
                return Ok(absorbed);
            }
            if let Some(lowest) = items.iter().map(|item| item.order).min() {
                row.complete_from_order = Some(lowest);
            }
            for item in items {
                if upsert_newer(tables, agent, &item)? {
                    absorbed.stored.push(Record::Item(item));
                }
            }
            row.exhausted = exhausted;
        }
        Absorb::CaughtUp(revision) => {
            row.source_cursor = revision;
        }
    }
    tables.put_agent(&row)?;
    Ok(absorbed)
}

/// Encodes an attachment list for its column.
pub(crate) fn encode_attachments(attachments: &[Attachment]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for attachment in attachments {
        attachment
            .encode_length_delimited(&mut bytes)
            .expect("a Vec grows to fit");
    }
    bytes
}

pub(crate) fn decode_attachments(mut bytes: &[u8]) -> Result<Vec<Attachment>, StoreError> {
    let mut attachments = Vec::new();
    while !bytes.is_empty() {
        attachments.push(
            Attachment::decode_length_delimited(&mut bytes)
                .map_err(|error| StoreError::Corrupt(error.to_string()))?,
        );
    }
    Ok(attachments)
}
