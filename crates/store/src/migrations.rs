//! The store's schema, as forward-only migrations.
//!
//! Migrations are append-only and additive (a column with a default, a
//! table, an index), each one transaction, and shipped text is never edited:
//! the store records each applied migration's hash and refuses a file whose
//! history disagrees, and a test pins the hash of every shipped migration.
//! A drop, rename or rewrite ships only after the schema stamp has been
//! raised past every binary that read the old shape.

use sha2::{Digest, Sha256};

/// The minimum number of migrations a binary must know to read a store it
/// writes. Stored as `user_version`; a binary opens any store whose stamp is
/// at most the migrations it has, so an older binary opens a newer store.
pub const SCHEMA_STAMP: u32 = 5;

/// Every migration ever shipped, in order. Append only.
pub const MIGRATIONS: &[&str] = &[
    r#"
CREATE TABLE agents (
  origin_host   BLOB NOT NULL,
  agent_id      BLOB NOT NULL,
  kind          TEXT NOT NULL,
  name          TEXT,
  cwd           TEXT NOT NULL,
  parent        BLOB, parent_host BLOB,
  lifecycle     INTEGER NOT NULL,
  exit_cause    TEXT,
  phase         INTEGER NOT NULL,
  working_on    TEXT,
  last_activity INTEGER,
  snapshot      BLOB,
  snapshot_revision INTEGER NOT NULL DEFAULT 0,
  ingest_cursor INTEGER NOT NULL DEFAULT 0,
  next_revision INTEGER NOT NULL DEFAULT 1,
  source_cursor INTEGER NOT NULL DEFAULT 0,
  complete_from_order INTEGER,
  exhausted     INTEGER NOT NULL DEFAULT 0,
  created_at    INTEGER NOT NULL,
  producer_version TEXT NOT NULL,
  incarnation   INTEGER NOT NULL,
  PRIMARY KEY (origin_host, agent_id)
);
CREATE TABLE deliveries (
  child_id      BLOB NOT NULL,
  incarnation   INTEGER NOT NULL,
  turn_id       INTEGER NOT NULL,
  parent_host   BLOB NOT NULL, parent_id BLOB NOT NULL,
  parent_incarnation INTEGER NOT NULL,
  kind          INTEGER NOT NULL,
  body          TEXT,
  PRIMARY KEY (child_id, incarnation, kind, turn_id)
);
CREATE TABLE notifications (
  agent_id      BLOB NOT NULL,
  revision      INTEGER NOT NULL,
  due_at        INTEGER NOT NULL,
  body          TEXT,
  PRIMARY KEY (agent_id, revision)
);
CREATE TABLE hosts (
  host_id       BLOB PRIMARY KEY,
  generation    INTEGER NOT NULL
);
CREATE TABLE items (
  origin_host   BLOB NOT NULL,
  agent_id      BLOB NOT NULL,
  key           TEXT NOT NULL,
  "order"       INTEGER NOT NULL,
  revision      INTEGER NOT NULL,
  at_ms         INTEGER NOT NULL,
  producer_version TEXT NOT NULL,
  input_id      BLOB,
  text          TEXT NOT NULL DEFAULT '',
  kind          TEXT NOT NULL,
  attachments   BLOB,
  body          BLOB NOT NULL,
  PRIMARY KEY (origin_host, agent_id, key)
);
CREATE UNIQUE INDEX items_by_order ON items (origin_host, agent_id, "order");
CREATE INDEX items_by_revision ON items (origin_host, agent_id, revision);
CREATE INDEX items_by_input ON items (origin_host, agent_id, input_id) WHERE input_id IS NOT NULL;
"#,
    r#"
ALTER TABLE agents ADD COLUMN turn_open INTEGER NOT NULL DEFAULT 0;
"#,
    r#"
ALTER TABLE agents ADD COLUMN source_generation INTEGER NOT NULL DEFAULT 0;
"#,
    r#"
ALTER TABLE agents ADD COLUMN phase_since INTEGER;
ALTER TABLE agents ADD COLUMN git BLOB;
"#,
    // A kind is its wire integer, so a kind this build does not know is
    // stored and served with the value its origin gave it. Raises the stamp:
    // a build that read the kind as text cannot read this store.
    r#"
ALTER TABLE agents ADD COLUMN kind_code INTEGER NOT NULL DEFAULT 0;
UPDATE agents SET kind_code = CASE kind
  WHEN 'claude_pty' THEN 1 WHEN 'claude_sdk' THEN 2 WHEN 'codex' THEN 3 ELSE 0 END;
ALTER TABLE agents DROP COLUMN kind;
ALTER TABLE agents RENAME COLUMN kind_code TO kind;
ALTER TABLE items ADD COLUMN kind_code INTEGER NOT NULL DEFAULT 0;
UPDATE items SET kind_code = CASE kind
  WHEN 'claude_pty' THEN 1 WHEN 'claude_sdk' THEN 2 WHEN 'codex' THEN 3 ELSE 0 END;
ALTER TABLE items DROP COLUMN kind;
ALTER TABLE items RENAME COLUMN kind_code TO kind;
"#,
];

/// The hex SHA-256 of a migration's text, as recorded when it is applied.
pub fn migration_hash(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
