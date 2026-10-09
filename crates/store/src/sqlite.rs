//! The profile file: SQLite in WAL mode with synchronous=NORMAL, so a
//! commit costs no fsync and power loss can lose the newest transactions
//! but never corrupt the file.

use std::collections::HashMap;
use std::path::Path;

use prost::Message as _;
use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};

use crate::migrations::{MIGRATIONS, SCHEMA_STAMP, migration_hash};
use crate::{
    AgentKey, AgentRow, Backend, Delivery, Item, Marker, Notification, Snapshot, StoreError,
    Tables, decode_attachments, encode_attachments,
};

/// Marks a file as an amux profile store ("amux").
const APPLICATION_ID: i32 = 0x616d_7578;

#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("{0} is not an amux profile store")]
    Foreign(String),
    #[error("this store needs a newer amux: its schema stamp is {stamp}, this build knows {known}")]
    TooNew { stamp: u32, known: u32 },
    #[error("migration {id} applied to this store differs from the one this build ships")]
    EditedMigration { id: u32 },
}

pub struct Sqlite {
    conn: Connection,
    own_host: Vec<u8>,
    markers: HashMap<AgentKey, Marker>,
}

impl std::fmt::Debug for Sqlite {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sqlite")
            .field("path", &self.conn.path())
            .finish_non_exhaustive()
    }
}

impl Sqlite {
    /// Opens or creates the profile store at `path` for the host whose own
    /// rows it holds, applying any migrations it lacks.
    pub fn open(path: &Path, own_host: impl Into<Vec<u8>>) -> Result<Self, OpenError> {
        let conn = Connection::open(path)?;
        Self::with_connection(conn, &path.display().to_string(), own_host.into())
    }

    fn with_connection(
        mut conn: Connection,
        name: &str,
        own_host: Vec<u8>,
    ) -> Result<Self, OpenError> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        migrate(&mut conn, name)?;
        Ok(Self {
            conn,
            own_host,
            markers: HashMap::new(),
        })
    }

    /// The underlying connection, for tests that must reach below the
    /// store's own writes.
    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    /// Moves every committed transaction from the WAL into the database
    /// file and flushes it to the drive itself (fullfsync, which macOS
    /// needs for a flush to pass the drive's cache). Commits are otherwise
    /// not synced, so this is what a clean shutdown promises before it
    /// marks the installation clean.
    pub fn flush_to_drive(&self) -> Result<(), rusqlite::Error> {
        self.conn.pragma_update(None, "fullfsync", true)?;
        let checkpoint = self
            .conn
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
                row.get::<_, i64>(0)
            });
        let restored = self.conn.pragma_update(None, "fullfsync", false);
        match checkpoint? {
            0 => restored,
            _ => Err(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
                Some("a reader held the WAL through the checkpoint".into()),
            )),
        }
    }
}

fn migrate(conn: &mut Connection, name: &str) -> Result<(), OpenError> {
    let stamp: u32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let application_id: i32 = conn.pragma_query_value(None, "application_id", |row| row.get(0))?;
    let fresh = stamp == 0 && application_id == 0;
    if !fresh && application_id != APPLICATION_ID {
        return Err(OpenError::Foreign(name.into()));
    }
    let known = MIGRATIONS.len() as u32;
    if stamp > known {
        return Err(OpenError::TooNew { stamp, known });
    }
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (id INTEGER PRIMARY KEY, sha256 TEXT NOT NULL);",
    )?;
    let applied = {
        let mut statement = tx.prepare("SELECT id, sha256 FROM schema_migrations ORDER BY id")?;
        statement
            .query_map([], |row| {
                Ok((row.get::<_, u32>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?
    };
    for (id, hash) in &applied {
        if let Some(text) = MIGRATIONS.get(*id as usize - 1)
            && migration_hash(text) != *hash
        {
            return Err(OpenError::EditedMigration { id: *id });
        }
    }
    tx.commit()?;
    let done = applied.last().map_or(0, |(id, _)| *id);
    for (index, text) in MIGRATIONS.iter().enumerate().skip(done as usize) {
        let id = index as u32 + 1;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_batch(text)?;
        tx.execute(
            "INSERT INTO schema_migrations (id, sha256) VALUES (?1, ?2)",
            params![id, migration_hash(text)],
        )?;
        tx.pragma_update(None, "application_id", APPLICATION_ID)?;
        let current: u32 = tx.pragma_query_value(None, "user_version", |row| row.get(0))?;
        tx.pragma_update(None, "user_version", current.max(SCHEMA_STAMP))?;
        tx.commit()?;
    }
    Ok(())
}

impl Backend for Sqlite {
    fn own_host(&self) -> &[u8] {
        &self.own_host
    }

    fn read<R>(
        &self,
        f: impl FnOnce(&dyn Tables) -> Result<R, StoreError>,
    ) -> Result<R, StoreError> {
        // One read transaction: every read inside sees one commit point.
        let tx = self.conn.unchecked_transaction()?;
        let result = f(&SqlTables { conn: &tx });
        tx.finish()?;
        result
    }

    fn write<R>(
        &mut self,
        f: impl FnOnce(&mut dyn Tables) -> Result<R, StoreError>,
    ) -> Result<R, StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let result = f(&mut SqlTables { conn: &tx })?;
        tx.commit()?;
        Ok(result)
    }

    fn markers(&self) -> &HashMap<AgentKey, Marker> {
        &self.markers
    }

    fn markers_mut(&mut self) -> &mut HashMap<AgentKey, Marker> {
        &mut self.markers
    }
}

struct SqlTables<'a> {
    conn: &'a Connection,
}

const AGENT_COLUMNS: &str = "origin_host, agent_id, kind, name, cwd, parent, parent_host, \
    lifecycle, exit_cause, phase, working_on, last_activity, snapshot, snapshot_revision, \
    ingest_cursor, next_revision, source_cursor, complete_from_order, exhausted, created_at, \
    incarnation, turn_open, source_generation, phase_since, git, exit_detail";

/// `crate::item_bytes`, in SQL: byte lengths, not character counts.
const ITEM_BYTES: &str = "(length(CAST(key AS BLOB)) + length(CAST(text AS BLOB)) + length(body) \
    + IFNULL(length(attachments), 0) + IFNULL(length(input_id), 0) \
    + length(CAST(producer_version AS BLOB)) + 48)";

const ITEM_COLUMNS: &str =
    "key, \"order\", revision, at_ms, producer_version, input_id, text, kind, attachments, body";

/// An agents row with its snapshot and git columns still encoded.
type RawAgent = (AgentRow, Option<Vec<u8>>, Option<Vec<u8>>);

fn agent_row(row: &Row<'_>) -> rusqlite::Result<RawAgent> {
    let parent: Option<Vec<u8>> = row.get(5)?;
    let parent_host: Option<Vec<u8>> = row.get(6)?;
    let snapshot: Option<Vec<u8>> = row.get(12)?;
    Ok((
        AgentRow {
            agent: AgentKey::new(row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?),
            kind: row.get(2)?,
            name: row.get(3)?,
            cwd: row.get(4)?,
            parent: parent.map(|agent| AgentKey::new(parent_host.unwrap_or_default(), agent)),
            lifecycle: row.get(7)?,
            exit_cause: row.get(8)?,
            phase: row.get(9)?,
            working_on: row.get(10)?,
            last_activity: row.get(11)?,
            snapshot: None,
            snapshot_revision: row.get::<_, i64>(13)? as u64,
            ingest_cursor: row.get::<_, i64>(14)? as u64,
            next_revision: row.get::<_, i64>(15)? as u64,
            source_cursor: row.get::<_, i64>(16)? as u64,
            complete_from_order: row.get::<_, Option<i64>>(17)?.map(|order| order as u64),
            exhausted: row.get::<_, i64>(18)? != 0,
            created_at: row.get(19)?,
            incarnation: row.get(20)?,
            turn_open: row.get::<_, i64>(21)? != 0,
            source_generation: row.get::<_, i64>(22)? as u64,
            phase_since: row.get(23)?,
            git: None,
            exit_detail: row.get(25)?,
        },
        snapshot,
        row.get(24)?,
    ))
}

/// Decodes the message columns `agent_row` leaves as bytes.
fn finish_agent((mut row, snapshot, git): RawAgent) -> Result<AgentRow, StoreError> {
    let corrupt = |error: prost::DecodeError| StoreError::Corrupt(error.to_string());
    row.snapshot = snapshot
        .map(|bytes| Snapshot::decode(bytes.as_slice()))
        .transpose()
        .map_err(corrupt)?;
    row.git = git
        .map(|bytes| wire::Git::decode(bytes.as_slice()))
        .transpose()
        .map_err(corrupt)?;
    Ok(row)
}

fn item_row(row: &Row<'_>, agent: &AgentKey) -> rusqlite::Result<(Item, Option<Vec<u8>>)> {
    Ok((
        Item {
            agent_id: agent.agent.clone(),
            key: row.get(0)?,
            order: row.get::<_, i64>(1)? as u64,
            revision: row.get::<_, i64>(2)? as u64,
            at_ms: row.get(3)?,
            producer_version: row.get(4)?,
            input_id: row.get::<_, Option<Vec<u8>>>(5)?.unwrap_or_default(),
            text: row.get(6)?,
            kind: row.get(7)?,
            attachments: Vec::new(),
            body: row.get(9)?,
        },
        row.get(8)?,
    ))
}

fn finish_item((mut item, attachments): (Item, Option<Vec<u8>>)) -> Result<Item, StoreError> {
    item.attachments = decode_attachments(attachments.as_deref().unwrap_or_default())?;
    Ok(item)
}

fn delivery_row(row: &Row<'_>) -> rusqlite::Result<Delivery> {
    Ok(Delivery {
        child_id: row.get(0)?,
        incarnation: row.get(1)?,
        turn_id: row.get::<_, i64>(2)? as u64,
        parent: AgentKey::new(row.get::<_, Vec<u8>>(3)?, row.get::<_, Vec<u8>>(4)?),
        parent_incarnation: row.get(5)?,
        kind: row.get(6)?,
        body: row.get::<_, Option<String>>(7)?.unwrap_or_default(),
    })
}

impl Tables for SqlTables<'_> {
    fn agent(&self, agent: &AgentKey) -> Result<Option<AgentRow>, StoreError> {
        let found = self
            .conn
            .query_row(
                &format!(
                    "SELECT {AGENT_COLUMNS} FROM agents WHERE origin_host = ?1 AND agent_id = ?2"
                ),
                params![agent.host, agent.agent],
                agent_row,
            )
            .optional()?;
        found.map(finish_agent).transpose()
    }

    fn put_agent(&mut self, row: &AgentRow) -> Result<(), StoreError> {
        let placeholders = (1..=26)
            .map(|n| format!("?{n}"))
            .collect::<Vec<_>>()
            .join(", ");
        self.conn.execute(
            &format!("INSERT OR REPLACE INTO agents ({AGENT_COLUMNS}) VALUES ({placeholders})"),
            params![
                row.agent.host,
                row.agent.agent,
                row.kind,
                row.name,
                row.cwd,
                row.parent.as_ref().map(|parent| &parent.agent),
                row.parent.as_ref().map(|parent| &parent.host),
                row.lifecycle,
                row.exit_cause,
                row.phase,
                row.working_on,
                row.last_activity,
                row.snapshot
                    .as_ref()
                    .map(|snapshot| snapshot.encode_to_vec()),
                row.snapshot_revision as i64,
                row.ingest_cursor as i64,
                row.next_revision as i64,
                row.source_cursor as i64,
                row.complete_from_order.map(|order| order as i64),
                i64::from(row.exhausted),
                row.created_at,
                row.incarnation,
                i64::from(row.turn_open),
                row.source_generation as i64,
                row.phase_since,
                row.git.as_ref().map(|git| git.encode_to_vec()),
                row.exit_detail,
            ],
        )?;
        Ok(())
    }

    fn remove_agent(&mut self, agent: &AgentKey) -> Result<(), StoreError> {
        self.conn.execute(
            "DELETE FROM items WHERE origin_host = ?1 AND agent_id = ?2",
            params![agent.host, agent.agent],
        )?;
        self.conn.execute(
            "DELETE FROM agents WHERE origin_host = ?1 AND agent_id = ?2",
            params![agent.host, agent.agent],
        )?;
        self.conn.execute(
            "DELETE FROM deliveries WHERE child_id = ?1",
            params![agent.agent],
        )?;
        self.conn.execute(
            "DELETE FROM notifications WHERE agent_id = ?1",
            params![agent.agent],
        )?;
        Ok(())
    }

    fn item(&self, agent: &AgentKey, key: &str) -> Result<Option<Item>, StoreError> {
        self.conn
            .query_row(
                &format!(
                    "SELECT {ITEM_COLUMNS} FROM items WHERE origin_host = ?1 AND agent_id = ?2 AND key = ?3"
                ),
                params![agent.host, agent.agent, key],
                |row| item_row(row, agent),
            )
            .optional()?
            .map(finish_item)
            .transpose()
    }

    fn item_by_input(&self, agent: &AgentKey, input_id: &[u8]) -> Result<Option<Item>, StoreError> {
        self.conn
            .query_row(
                &format!(
                    "SELECT {ITEM_COLUMNS} FROM items \
                     WHERE origin_host = ?1 AND agent_id = ?2 AND input_id = ?3 LIMIT 1"
                ),
                params![agent.host, agent.agent, input_id],
                |row| item_row(row, agent),
            )
            .optional()?
            .map(finish_item)
            .transpose()
    }

    fn put_item(&mut self, agent: &AgentKey, item: &Item) -> Result<(), StoreError> {
        self.conn.execute(
            &format!(
                "INSERT INTO items (origin_host, agent_id, {ITEM_COLUMNS}) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12) \
                 ON CONFLICT (origin_host, agent_id, key) DO UPDATE SET \
                 \"order\" = excluded.\"order\", revision = excluded.revision, \
                 at_ms = excluded.at_ms, producer_version = excluded.producer_version, \
                 input_id = excluded.input_id, text = excluded.text, kind = excluded.kind, \
                 attachments = excluded.attachments, body = excluded.body"
            ),
            params![
                agent.host,
                agent.agent,
                item.key,
                item.order as i64,
                item.revision as i64,
                item.at_ms,
                item.producer_version,
                (!item.input_id.is_empty()).then_some(&item.input_id),
                item.text,
                item.kind,
                (!item.attachments.is_empty()).then(|| encode_attachments(&item.attachments)),
                item.body,
            ],
        )?;
        Ok(())
    }

    fn max_order(&self, agent: &AgentKey) -> Result<Option<u64>, StoreError> {
        let max: Option<i64> = self.conn.query_row(
            "SELECT MAX(\"order\") FROM items WHERE origin_host = ?1 AND agent_id = ?2",
            params![agent.host, agent.agent],
            |row| row.get(0),
        )?;
        Ok(max.map(|order| order as u64))
    }

    fn items_desc(
        &self,
        agent: &AgentKey,
        below: Option<u64>,
        min_order: u64,
        limit: u32,
    ) -> Result<Vec<Item>, StoreError> {
        let mut statement = self.conn.prepare(&format!(
            "SELECT {ITEM_COLUMNS} FROM items WHERE origin_host = ?1 AND agent_id = ?2 \
             AND \"order\" >= ?3 AND \"order\" < ?4 ORDER BY \"order\" DESC LIMIT ?5"
        ))?;
        let below = below.map_or(i64::MAX, |order| order.min(i64::MAX as u64) as i64);
        let rows = statement
            .query_map(
                params![agent.host, agent.agent, min_order as i64, below, limit],
                |row| item_row(row, agent),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter().map(finish_item).collect()
    }

    fn items_after(
        &self,
        agent: &AgentKey,
        revision: u64,
        limit: u32,
    ) -> Result<Vec<Item>, StoreError> {
        let mut statement = self.conn.prepare(&format!(
            "SELECT {ITEM_COLUMNS} FROM items WHERE origin_host = ?1 AND agent_id = ?2 \
             AND revision > ?3 ORDER BY revision LIMIT ?4"
        ))?;
        let rows = statement
            .query_map(
                params![
                    agent.host,
                    agent.agent,
                    revision.min(i64::MAX as u64) as i64,
                    limit
                ],
                |row| item_row(row, agent),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter().map(finish_item).collect()
    }

    fn put_delivery(&mut self, delivery: &Delivery) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT OR REPLACE INTO deliveries (child_id, incarnation, turn_id, parent_host, \
             parent_id, parent_incarnation, kind, body) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                delivery.child_id,
                delivery.incarnation,
                delivery.turn_id as i64,
                delivery.parent.host,
                delivery.parent.agent,
                delivery.parent_incarnation,
                delivery.kind,
                delivery.body,
            ],
        )?;
        Ok(())
    }

    fn deliveries(&self) -> Result<Vec<Delivery>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT child_id, incarnation, turn_id, parent_host, parent_id, parent_incarnation, \
             kind, body FROM deliveries ORDER BY child_id, incarnation, kind, turn_id",
        )?;
        let rows = statement
            .query_map([], delivery_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    fn remove_delivery(
        &mut self,
        child_id: &[u8],
        incarnation: u32,
        kind: i32,
        turn_id: u64,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "DELETE FROM deliveries WHERE child_id = ?1 AND incarnation = ?2 AND kind = ?3 \
             AND turn_id = ?4",
            params![child_id, incarnation, kind, turn_id as i64],
        )?;
        Ok(())
    }

    fn put_notification(&mut self, notification: &Notification) -> Result<(), StoreError> {
        let body = serde_json::to_string(&notification.body)
            .map_err(|error| StoreError::Corrupt(error.to_string()))?;
        self.conn.execute(
            "INSERT OR REPLACE INTO notifications (agent_id, revision, due_at, body) \
             VALUES (?1, ?2, ?3, ?4)",
            params![
                notification.agent_id,
                notification.revision as i64,
                notification.due_at,
                body
            ],
        )?;
        Ok(())
    }

    fn notifications(&self) -> Result<Vec<Notification>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT agent_id, revision, due_at, body FROM notifications ORDER BY agent_id, revision",
        )?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, i64>(1)? as u64,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|(agent_id, revision, due_at, body)| {
                Ok(Notification {
                    agent_id,
                    revision,
                    due_at,
                    body: body
                        .map(|body| serde_json::from_str(&body))
                        .transpose()
                        .map_err(|error| StoreError::Corrupt(error.to_string()))?
                        .unwrap_or_default(),
                })
            })
            .collect()
    }

    fn remove_notifications(&mut self, agent_id: &[u8]) -> Result<(), StoreError> {
        self.conn.execute(
            "DELETE FROM notifications WHERE agent_id = ?1",
            params![agent_id],
        )?;
        Ok(())
    }

    fn remove_notification(&mut self, agent_id: &[u8], revision: u64) -> Result<(), StoreError> {
        self.conn.execute(
            "DELETE FROM notifications WHERE agent_id = ?1 AND revision = ?2",
            params![agent_id, revision as i64],
        )?;
        Ok(())
    }

    fn host_generation(&self, host: &[u8]) -> Result<Option<u64>, StoreError> {
        Ok(self
            .conn
            .query_row(
                "SELECT generation FROM hosts WHERE host_id = ?1",
                params![host],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .map(|generation| generation as u64))
    }

    fn set_host_generation(&mut self, host: &[u8], generation: u64) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO hosts (host_id, generation) VALUES (?1, ?2) \
             ON CONFLICT (host_id) DO UPDATE SET generation = excluded.generation",
            params![host, generation as i64],
        )?;
        Ok(())
    }

    fn agents(&self) -> Result<Vec<AgentRow>, StoreError> {
        let mut statement = self.conn.prepare(&format!(
            "SELECT {AGENT_COLUMNS} FROM agents ORDER BY origin_host, agent_id"
        ))?;
        let rows = statement
            .query_map([], agent_row)?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter().map(finish_agent).collect()
    }

    fn agent_bytes(&self, agent: &AgentKey) -> Result<(u64, u64), StoreError> {
        let (bytes, rows): (i64, i64) = self.conn.query_row(
            &format!(
                "SELECT IFNULL(SUM({ITEM_BYTES}), 0), COUNT(*) FROM items \
                 WHERE origin_host = ?1 AND agent_id = ?2"
            ),
            params![agent.host, agent.agent],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        Ok((bytes as u64, rows as u64))
    }

    fn oldest_items(&self, agent: &AgentKey, limit: u32) -> Result<Vec<(u64, u64)>, StoreError> {
        let mut statement = self.conn.prepare(&format!(
            "SELECT \"order\", {ITEM_BYTES} FROM items WHERE origin_host = ?1 AND agent_id = ?2 \
             ORDER BY \"order\" LIMIT ?3"
        ))?;
        let rows = statement
            .query_map(params![agent.host, agent.agent, limit], |row| {
                Ok((row.get::<_, i64>(0)? as u64, row.get::<_, i64>(1)? as u64))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    fn remove_items_below(&mut self, agent: &AgentKey, order: u64) -> Result<(), StoreError> {
        self.conn.execute(
            "DELETE FROM items WHERE origin_host = ?1 AND agent_id = ?2 AND \"order\" < ?3",
            params![agent.host, agent.agent, order as i64],
        )?;
        Ok(())
    }
}
