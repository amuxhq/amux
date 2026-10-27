//! The profile store: one database per profile, the truth for this host's
//! own agents and a cache of its peers' agents, and the only history any
//! client on the machine reads, through its runtime.
//!
//! Own rows are written by [`Sqlite::commit`] from an agent's journal: the
//! store assigns every record the agent's next revision and every new key
//! the next order, in journal order. Replica rows are written only by
//! [`Sqlite::absorb`], which copies the origin's revisions and orders and
//! keeps the replica's rows one contiguous block ending at the origin's
//! newest (see [`Absorb`]). Both are single transactions.
//!
//! [`Sqlite`] is the store: one SQLite file per profile, whose queries do
//! the work, with one conformance suite holding it to its answers.

use prost::Message as _;
pub use wire::{Append, Attachment, Item, Snapshot, Step};

mod migrations;
mod rows;
mod sqlite;

mod blobs;
mod explore;
pub mod redact;
mod retention;

pub use blobs::BlobLru;
pub use explore::{BlockOrders, Dump};
pub use migrations::{MIGRATIONS, SCHEMA_STAMP, migration_hash};
pub use retention::{Sweep, SweepStep};
pub use sqlite::{Durability, OpenError, Sqlite};

/// A paired host as the store keeps it. The two address lists are JSON the
/// daemon core writes and reads; the store does not interpret them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrustedHost {
    pub host_id: Vec<u8>,
    pub pubkey: Vec<u8>,
    pub name: String,
    pub paired_at_ms: i64,
    pub on_account: bool,
    pub signed_in: Option<bool>,
    /// The wire integer of `amux.v1.Platform`.
    pub platform: i32,
    pub ssh: Option<String>,
    pub direct: String,
    pub listed: String,
}

/// The most directories [`Sqlite::record_recent_directory`] keeps.
pub const RECENT_DIRECTORIES: usize = 200;

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

/// The agents row. Kind, lifecycle, exit cause and phase are kept as their
/// wire integers so a value from a newer peer is stored and served, never
/// refused.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentRow {
    pub agent: AgentKey,
    pub kind: i32,
    /// Every agent has one: given, or chosen by the daemon.
    pub name: String,
    pub cwd: String,
    pub parent: Option<AgentKey>,
    pub lifecycle: i32,
    pub exit_cause: i32,
    /// The exit cause's own words, when it has any.
    pub exit_detail: Option<String>,
    /// Copied from the snapshot envelope at commit.
    pub phase: i32,
    pub working_on: Option<String>,
    /// Copied from the snapshot envelope at commit: when the phase began.
    pub phase_since_ms: Option<i64>,
    /// Copied from the snapshot envelope at commit.
    pub git: Option<wire::Git>,
    /// The newest snapshot, which carries its own revision.
    pub snapshot: Option<Snapshot>,
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
    pub created_at_ms: i64,
    pub incarnation: u32,
    /// Own rows: the incarnation has not yet sent its parent its last
    /// report, from its start until a turn end marked last (and again
    /// should a later turn start). An incarnation that exits still owing
    /// one, its turn open or ended without being its last, tells its parent
    /// it failed: every child incarnation ends with exactly one last report.
    pub owes_report: bool,
    /// Copied from the snapshot envelope at commit, and for a replica from
    /// its inventory row: how many jobs the provider runs in the
    /// background.
    pub background_jobs: u32,
    /// Live agents below this one. An own row's is counted by its daemon
    /// over the rows it holds ([`Sqlite::live_descendants`]); a replica's is
    /// what its origin counted.
    pub descendants: u32,
}

impl AgentRow {
    /// The newest snapshot's at_ms, never commit time. A replica's
    /// inventory row carries no snapshot, so a replica nobody opened has
    /// none.
    pub fn last_activity_ms(&self) -> Option<i64> {
        self.snapshot.as_ref().map(|snapshot| snapshot.at_ms)
    }

    /// A new live row in phase starting, with nothing committed yet. Its
    /// name is empty until the caller gives it one.
    pub fn new(agent: AgentKey, kind: impl Into<i32>, cwd: impl Into<String>) -> Self {
        Self {
            agent,
            kind: kind.into(),
            name: String::new(),
            cwd: cwd.into(),
            parent: None,
            lifecycle: wire::Lifecycle::Live as i32,
            exit_cause: 0,
            exit_detail: None,
            phase: wire::Phase::Starting as i32,
            working_on: None,
            phase_since_ms: None,
            git: None,
            snapshot: None,
            ingest_cursor: 0,
            next_revision: 1,
            source_cursor: 0,
            source_generation: 0,
            complete_from_order: None,
            exhausted: false,
            created_at_ms: 0,
            incarnation: 1,
            owes_report: true,
            background_jobs: 0,
            descendants: 0,
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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

/// The outbox row that carries a child's report to its parent: a turn of
/// the child's ended, or its incarnation exited without a turn end.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Delivery {
    pub child_id: Vec<u8>,
    pub incarnation: u32,
    /// The turn that ended; zero for an exit report.
    pub turn_id: u64,
    /// The parent the child's row names; read from it, never written.
    pub parent: AgentKey,
    /// The parent's incarnation when the row was written; the receiving
    /// daemon drops a row for any other. Zero when the parent's row was not
    /// held then, which incarnations never are: unknown, and stamped by the
    /// drain once the parent's row is held, never sent as it is.
    pub parent_incarnation: u32,
    /// What the report says, as the envelope carries it.
    pub end: wire::child_report::End,
}

impl Delivery {
    /// Whether this is the report of an incarnation that exited without a
    /// turn end, rather than of a turn that ended.
    pub fn is_exit(&self) -> bool {
        matches!(self.end, wire::child_report::End::Exited(_))
    }
}

/// The push outbox row for one turn into needs_you, built from envelope
/// fields only: nothing here needs a body decoded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notification {
    pub agent_id: Vec<u8>,
    /// The revision of the snapshot that turned to needs_you.
    pub revision: u64,
    pub due_at_ms: i64,
    pub name: String,
    pub working_on: Option<String>,
    /// The newest item's text when it turned: usually the question.
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
    /// Whoever owned the store let it go: nothing reads or writes it now.
    #[error("the store is closed")]
    Closed,
}

/// What one held row costs against a retention budget: its envelope's
/// variable fields, the kind's integer, and a fixed allowance for the other
/// integers and the index entries beside them. The store's SQL counts the
/// same bytes, so the two agree on every row.
pub fn item_bytes(item: &Item) -> u64 {
    (item.key.len()
        + item.text.len()
        + item.body.len()
        + encode_attachments(&item.attachments).len()
        + item.input_id.len()
        + item.producer_version.len()) as u64
        + KIND_BYTES
        + ITEM_OVERHEAD_BYTES
}

/// What [`item_bytes`] counts for a row's kind: a fixed 4 bytes, whatever
/// integer it holds.
pub const KIND_BYTES: u64 = 4;

/// The fixed per-row allowance in [`item_bytes`] besides the kind.
pub const ITEM_OVERHEAD_BYTES: u64 = 48;

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
