//! The store's schema, as forward-only migrations.
//!
//! The first migration is the schema the first release ships. From that
//! release on, migrations are append-only and additive (a column with a
//! default, a table, an index), each one transaction, and shipped text is
//! never edited: the store records each applied migration's hash and refuses
//! a file whose history disagrees, and a test pins the hash of every shipped
//! migration. A drop, rename or rewrite ships only after the schema stamp
//! has been raised past every binary that read the old shape. The same hash
//! check refuses a store written before the first release, whose history
//! is a different first migration: no such store is carried forward.

use sha2::{Digest, Sha256};

/// The minimum number of migrations a binary must know to read a store it
/// writes. Stored as `user_version`; a binary opens any store whose stamp is
/// at most the migrations it has, so an older binary opens a newer store.
pub const SCHEMA_STAMP: u32 = 1;

/// Every migration shipped, in order. Append only from the first release.
pub const MIGRATIONS: &[&str] = &[r#"
-- One profile's store: this host's own agents and the replicas it follows.
-- Enum-valued columns hold the wire integer and are never interpreted by
-- the store, so a value this binary does not know is stored and served.
CREATE TABLE agents (
  origin_host         BLOB    NOT NULL,  -- the host that runs the agent
  agent_id            BLOB    NOT NULL,  -- the agent's id, unique across hosts
  kind                INTEGER NOT NULL,  -- amux.v1.Kind
  name                TEXT    NOT NULL,  -- every agent has one: given, or chosen by the daemon
  cwd                 TEXT    NOT NULL,  -- the directory the agent works in
  parent_host         BLOB,              -- the parent's host and id, when it has one
  parent_id           BLOB,
  lifecycle           INTEGER NOT NULL,  -- amux.v1.Lifecycle
  exit_cause          INTEGER NOT NULL DEFAULT 0, -- amux.v1.ExitCause; 0 while live or unknown
  exit_detail         TEXT,              -- the cause's own words, when it has any
  phase               INTEGER NOT NULL,  -- amux.v1.Phase, copied from the newest snapshot
  working_on          TEXT,              -- copied from the newest snapshot
  phase_since_ms      INTEGER,           -- copied from the newest snapshot
  git                 BLOB,              -- amux.v1.Git, copied from the newest snapshot
  last_activity_ms    INTEGER,           -- the newest snapshot's at_ms, never commit time
  snapshot            BLOB,              -- amux.v1.Snapshot, body opaque; carries its revision
  ingest_cursor       INTEGER NOT NULL DEFAULT 0, -- own rows: the journal offset committed through
  next_revision       INTEGER NOT NULL DEFAULT 1, -- own rows: the next revision to mint
  source_cursor       INTEGER NOT NULL DEFAULT 0, -- replica rows: the origin revision complete through
  source_generation   INTEGER NOT NULL DEFAULT 0, -- replica rows: the origin generation of the block
  complete_from_order INTEGER,           -- the block runs from here; NULL means all history (own) or no block (replica)
  exhausted           INTEGER NOT NULL DEFAULT 0, -- no older history exists (replica) or retention trimmed it (own)
  owes_report         INTEGER NOT NULL DEFAULT 0, -- own rows: the incarnation has not sent its parent its last report
  created_at_ms       INTEGER NOT NULL,  -- when the agent was created, on its origin's clock
  incarnation         INTEGER NOT NULL,  -- the agent's spec.<n>, counting resumes from 1
  background_jobs     INTEGER NOT NULL DEFAULT 0, -- copied from the newest snapshot: jobs the provider runs in the background
  descendants         INTEGER NOT NULL DEFAULT 0, -- live agents below this one: own rows counted here, replicas as their origin counts
  PRIMARY KEY (origin_host, agent_id)
);

-- A child's reports owed to its parent, kept until delivered.
CREATE TABLE deliveries (
  child_id            BLOB    NOT NULL,  -- the own agent whose turn or incarnation ended
  incarnation         INTEGER NOT NULL,  -- the child's incarnation that ended it
  turn_id             INTEGER NOT NULL,  -- the turn that ended; 0 for an exit report
  parent_host         BLOB    NOT NULL,  -- the parent the report is owed to
  parent_id           BLOB    NOT NULL,
  parent_incarnation  INTEGER NOT NULL,  -- 0 until the parent's row is held, then stamped
  exited              INTEGER NOT NULL,  -- 1: the incarnation exited without a turn end; 0: a turn ended
  text                TEXT,              -- turn: the child's last message
  exit_cause          INTEGER,           -- exit: amux.v1.ExitCause, how the incarnation ended
  exit_detail         TEXT,              -- exit: the cause's own words, when it has any
  outcome             INTEGER,           -- turn: amux.v1.TurnOutcome, how the turn ended
  last                INTEGER NOT NULL DEFAULT 0, -- turn: it was the incarnation's last (an exit report always is)
  PRIMARY KEY (child_id, incarnation, exited, turn_id)
);

-- A needs-you transition owed to the person, keyed by the snapshot that
-- raised it; built from envelope fields only.
CREATE TABLE notifications (
  agent_id            BLOB    NOT NULL,  -- the own agent that needs the person
  revision            INTEGER NOT NULL,  -- the snapshot's revision that turned to needs-you
  due_at_ms           INTEGER NOT NULL,  -- sent no earlier, so an answer at the desk cancels it
  name                TEXT,              -- the agent's name when it turned
  working_on          TEXT,              -- the snapshot's working_on
  text                TEXT    NOT NULL DEFAULT '', -- the newest item's text: usually the question
  PRIMARY KEY (agent_id, revision)
);

-- The generation last seen from each origin; bumped only after that
-- host's unclean reboot.
CREATE TABLE hosts (
  host_id             BLOB    PRIMARY KEY, -- a paired host, never this one
  generation          INTEGER NOT NULL   -- the generation its inventory last reported
);

-- Items: full state per key, newest revision only.
CREATE TABLE items (
  origin_host         BLOB    NOT NULL,  -- the agent's row this item belongs to
  agent_id            BLOB    NOT NULL,
  key                 TEXT    NOT NULL,  -- the interpreter's key
  position            INTEGER NOT NULL,  -- the wire `order`: once per key, in arrival order
  revision            INTEGER NOT NULL,  -- on every upsert
  at_ms               INTEGER NOT NULL,  -- when it happened, from the envelope
  producer_version    TEXT    NOT NULL,  -- the agent binary that wrote it
  input_id            BLOB,              -- the prompt or message this item reflects, when any
  text                TEXT    NOT NULL DEFAULT '', -- the one appendable field
  kind                INTEGER NOT NULL,  -- amux.v1.Kind: which per-kind body this is
  attachments         BLOB,              -- amux.v1.Attachment list, length-delimited
  body                BLOB    NOT NULL,  -- the per-kind item, opaque
  PRIMARY KEY (origin_host, agent_id, key)
);
CREATE UNIQUE INDEX items_by_position ON items (origin_host, agent_id, position);
CREATE INDEX items_by_revision ON items (origin_host, agent_id, revision);
CREATE INDEX items_by_input ON items (origin_host, agent_id, input_id) WHERE input_id IS NOT NULL;
"#];

/// The hex SHA-256 of a migration's text, as recorded when it is applied.
pub fn migration_hash(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
