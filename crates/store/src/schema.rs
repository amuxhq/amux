//! The store's schema. A fresh file is given it whole; any other file must
//! carry this build's version, or it is refused and left as it is. There
//! are no migrations: a schema change raises the version, and a store
//! written under another version is moved aside by the person.

/// The version of [`SCHEMA`], kept in the file's `user_version`. Raise it
/// with every change to the schema.
pub const SCHEMA_VERSION: u32 = 2;

/// The whole schema, run once on a fresh file.
pub(crate) const SCHEMA: &str = r#"
-- One profile's store: this host's own agents and the replicas it follows.
-- Enum-valued columns hold the wire integer and are never interpreted by
-- the store, so a value this binary does not know is stored and served.
-- Every table is STRICT: a value of the wrong type is refused, not stored.
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
) STRICT;

-- A child's reports owed to its parent, kept until delivered.
CREATE TABLE deliveries (
  child_id            BLOB    NOT NULL,  -- the own agent whose turn or incarnation ended
  incarnation         INTEGER NOT NULL,  -- the child's incarnation that ended it
  turn_id             INTEGER NOT NULL,  -- the turn that ended; 0 for an exit report
  parent_incarnation  INTEGER NOT NULL,  -- the parent's, as the child's row names it; 0 until its row is held, then stamped
  exited              INTEGER NOT NULL,  -- 1: the incarnation exited without a turn end; 0: a turn ended
  text                TEXT,              -- turn: the child's last message
  exit_cause          INTEGER,           -- exit: amux.v1.ExitCause, how the incarnation ended
  exit_detail         TEXT,              -- exit: the cause's own words, when it has any
  outcome             INTEGER,           -- turn: amux.v1.TurnOutcome, how the turn ended
  last                INTEGER NOT NULL DEFAULT 0, -- turn: it was the incarnation's last (an exit report always is)
  PRIMARY KEY (child_id, incarnation, exited, turn_id)
) STRICT;

-- A needs-you transition owed to the person, keyed by the snapshot that
-- raised it; built from envelope fields only.
CREATE TABLE notifications (
  agent_id            BLOB    NOT NULL,  -- the own agent that needs the person
  revision            INTEGER NOT NULL,  -- the snapshot's revision that turned to needs-you
  due_at_ms           INTEGER NOT NULL,  -- sent no earlier, so an answer at the desk cancels it
  name                TEXT    NOT NULL,  -- the agent's name when it turned
  working_on          TEXT,              -- the snapshot's working_on
  text                TEXT    NOT NULL DEFAULT '', -- the newest item's text: usually the question
  PRIMARY KEY (agent_id, revision)
) STRICT;

-- The generation last seen from each origin; bumped only after that
-- host's unclean reboot.
CREATE TABLE hosts (
  host_id             BLOB    PRIMARY KEY, -- a paired host, never this one
  generation          INTEGER NOT NULL   -- the generation its inventory last reported
) STRICT;

-- The hosts this profile is paired with: each one's pinned key, its name,
-- how it was last reached and what it last said of itself.
CREATE TABLE trusted_hosts (
  host_id             BLOB    PRIMARY KEY, -- a paired host, never this one
  pubkey              BLOB    NOT NULL,  -- its pinned Ed25519 key
  name                TEXT    NOT NULL,  -- what it is called here
  paired_at_ms        INTEGER NOT NULL,
  on_account          INTEGER NOT NULL,  -- the account's relay reaches it
  signed_in           INTEGER,           -- NULL: the host never said
  platform            INTEGER NOT NULL,  -- amux.v1.Platform, last it said one
  ssh                 TEXT,              -- the SSH destination it paired at, when it paired that way
  direct              TEXT    NOT NULL,  -- JSON: the addresses it last answered at directly
  listed              TEXT    NOT NULL   -- JSON: every address it last listed, with where each came from
) STRICT;

-- An unsent draft per agent, kept for this machine's clients.
CREATE TABLE drafts (
  agent_id            BLOB    PRIMARY KEY,
  draft               BLOB    NOT NULL   -- amux.v1.Draft
) STRICT;

-- The directories agents were started in, newest first, outliving the
-- agents that used them.
CREATE TABLE recent_directories (
  path                TEXT    PRIMARY KEY,
  last_used_ms        INTEGER NOT NULL
) STRICT;
CREATE INDEX recent_directories_by_use ON recent_directories (last_used_ms);

-- Items: full state per key, newest revision only.
CREATE TABLE items (
  origin_host         BLOB    NOT NULL,  -- the agent's row this item belongs to
  agent_id            BLOB    NOT NULL,
  key                 TEXT    NOT NULL,  -- the interpreter's key
  position            INTEGER NOT NULL,  -- the wire `order`: once per key, in arrival order
  revision            INTEGER NOT NULL,  -- on every upsert
  at_ms               INTEGER NOT NULL,  -- when it happened, from the envelope
  input_id            BLOB,              -- the prompt or message this item reflects, when any
  text                TEXT    NOT NULL DEFAULT '', -- the one appendable field
  kind                INTEGER NOT NULL,  -- amux.v1.Kind: which per-kind body this is
  attachments         BLOB,              -- amux.v1.Attachment list, length-delimited
  body                BLOB    NOT NULL,  -- the per-kind item, opaque
  PRIMARY KEY (origin_host, agent_id, key)
) STRICT;
CREATE UNIQUE INDEX items_by_position ON items (origin_host, agent_id, position);
CREATE INDEX items_by_revision ON items (origin_host, agent_id, revision);
CREATE INDEX items_by_input ON items (origin_host, agent_id, input_id) WHERE input_id IS NOT NULL;
"#;
