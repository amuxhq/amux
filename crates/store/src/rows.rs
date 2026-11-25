//! The store's SQL, inside one transaction.
//!
//! A commit's writes, its items and its row with the cursor, must stay
//! `INSERT ... ON CONFLICT DO UPDATE`, never a bare `UPDATE`: the daemon's
//! crash-injection tests fail a commit with a `BEFORE INSERT` trigger on
//! `items` or `agents`, which a bare `UPDATE` would slip past. The other
//! row writes are upserts too. An upsert names the columns its writer owns;
//! the rest of a held row is left as it is. The bare `UPDATE`s run outside
//! any commit: `rewind_cursor`, power-loss recovery, and the clearing of
//! own rows' experimental entries as the store opens
//! (`experiments::settle`).

use prost::Message as _;
use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::experiments::SUSPENSION_1_RUNS;
use crate::{
    AgentKey, AgentRow, Delivery, Item, Notification, Snapshot, StoreError, TrustedHost,
    decode_attachments, encode_attachments,
};

const AGENT_COLUMNS: &str = "origin_host, agent_id, kind, provider, name, cwd, parent_host, \
    parent_id, lifecycle, exit_cause, exit_detail, status, title, snapshot, ingest_cursor, \
    next_revision, source_cursor, source_generation, complete_from_order, exhausted, owes_report, \
    created_at_ms, incarnation, descendants, filing, filed_at_ms, name_given, native, experimental, \
    parent_incarnation";

const ITEM_COLUMNS: &str =
    "key, position, revision, at_ms, input_id, text, kind, attachments, body";

/// What one held row costs against a retention budget: the byte lengths
/// (not character counts) of its variable fields, 4 bytes for its kind and
/// a fixed 48 for the other integers and the index entries beside them.
/// `{row}` stands for the row the columns are read from: nothing in a query
/// over `items`, `NEW.` or `OLD.` in the triggers that keep the pools'
/// totals (see [`keep_pool_totals`]); [`item_bytes`] fills it in.
const ITEM_BYTES: &str = "(length(CAST({row}key AS BLOB)) + length(CAST({row}text AS BLOB)) \
    + length({row}body) + IFNULL(length({row}attachments), 0) \
    + IFNULL(length({row}input_id), 0) + 4 + 48)";

/// [`ITEM_BYTES`] over the columns of `row` (`""`, `"NEW."` or `"OLD."`).
fn item_bytes(row: &str) -> String {
    ITEM_BYTES.replace("{row}", row)
}

/// Keeps the pools' totals on `conn` from here on: a table of running sums
/// of [`ITEM_BYTES`], one per origin host, seeded with a full count of
/// `items`, and triggers on `items` that move them with every row written
/// or deleted. Retention reads its pools every sweep, and summing every row
/// each time reads the whole table, every page of it; the totals make that
/// a read of a row per host. They are kept per host rather than as own and
/// replica so that they hold whichever host the store takes as its own: an
/// explored store loading a dump takes the dump's.
///
/// The table and triggers are `TEMP`: they live on this connection only and
/// nothing of them reaches the file, so the schema stays the migrations' and
/// a store opened by any build counts its own totals. That holds only
/// because this connection is the store's one writer (the installation lock
/// keeps it so); a write to `items` on any other connection would leave the
/// totals wrong until the store opens again. The temp table takes part in
/// each transaction like any other, so a write that rolls back takes its
/// totals' change back with it.
///
/// Every write to `items` fires one of the triggers: an upsert's insert
/// fires the insert trigger, its `DO UPDATE` the update trigger, and a
/// `DO UPDATE ... WHERE` that declines fires neither. An
/// `INSERT OR REPLACE` would not: the row it replaces is deleted without
/// firing the delete trigger unless `recursive_triggers` is on, which it is
/// not, so the totals would keep the replaced row. Items are written with
/// upserts and never so.
pub(crate) fn keep_pool_totals(conn: &Connection) -> rusqlite::Result<()> {
    // One pass in table order: a GROUP BY would sort every row first.
    let mut totals: std::collections::HashMap<Vec<u8>, i64> = std::collections::HashMap::new();
    {
        let mut rows = conn.prepare(&format!(
            "SELECT origin_host, {} FROM main.items",
            item_bytes("")
        ))?;
        let mut rows = rows.query([])?;
        while let Some(row) = rows.next()? {
            *totals.entry(row.get(0)?).or_default() += row.get::<_, i64>(1)?;
        }
    }
    // A trigger's body may not qualify a table, and an unqualified name
    // looks in `temp` first; no main table is called `item_pools`.
    conn.execute_batch(&format!(
        "CREATE TEMP TABLE item_pools (
             origin_host BLOB PRIMARY KEY,
             bytes INTEGER NOT NULL
         );
         CREATE TEMP TRIGGER item_pools_insert AFTER INSERT ON main.items BEGIN
             INSERT INTO item_pools (origin_host, bytes) VALUES (NEW.origin_host, {new})
             ON CONFLICT (origin_host) DO UPDATE SET bytes = bytes + excluded.bytes;
         END;
         CREATE TEMP TRIGGER item_pools_update AFTER UPDATE ON main.items BEGIN
             UPDATE item_pools SET bytes = bytes - {old} WHERE origin_host = OLD.origin_host;
             INSERT INTO item_pools (origin_host, bytes) VALUES (NEW.origin_host, {new})
             ON CONFLICT (origin_host) DO UPDATE SET bytes = bytes + excluded.bytes;
         END;
         CREATE TEMP TRIGGER item_pools_delete AFTER DELETE ON main.items BEGIN
             UPDATE item_pools SET bytes = bytes - {old} WHERE origin_host = OLD.origin_host;
         END;",
        new = item_bytes("NEW."),
        old = item_bytes("OLD."),
    ))?;
    let mut seed =
        conn.prepare("INSERT INTO temp.item_pools (origin_host, bytes) VALUES (?1, ?2)")?;
    for (host, bytes) in totals {
        seed.execute(params![host, bytes])?;
    }
    Ok(())
}

/// The columns the registry owns: what an own row's daemon or a replica's
/// inventory says about the agent itself.
const REGISTRY: &[&str] = &[
    "kind",
    "name",
    "cwd",
    "parent_host",
    "parent_id",
    "lifecycle",
    "exit_cause",
    "exit_detail",
    "created_at_ms",
    "incarnation",
    "filing",
    "filed_at_ms",
    "title",
    "name_given",
    "native",
    "provider",
];

/// What a replica's inventory row carries of its newest snapshot. Not its
/// last activity: the inventory row does not hold it.
const INVENTORY_ENVELOPE: &[&str] = &["status"];

/// What a replica's inventory row counts that its origin owns: its live
/// descendants. An own row's count is the daemon's, written apart.
const INVENTORY_COUNTS: &[&str] = &["descendants"];

/// The row's experimental map. A replica's inventory row carries it, and it
/// is replaced whole from the origin's row whatever this host runs; an own
/// row's is written apart, by the experiments this host runs.
pub(crate) const EXPERIMENTAL: &[&str] = &["experimental"];

/// What a commit writes: the newest snapshot's envelope, the cause the
/// journal ended its incarnation with, and the own row's cursors.
pub(crate) const COMMITTED: &[&str] = &[
    "exit_cause",
    "exit_detail",
    "status",
    "title",
    "snapshot",
    "ingest_cursor",
    "next_revision",
    "owes_report",
];

/// What a commit writes of a provider subagent: what it writes of any
/// agent, and the lifecycle its parts' starts and ends move, which for an
/// agent with a process of its own is the registry's.
pub(crate) const SUBAGENT_COMMITTED: &[&str] = &[
    "exit_cause",
    "exit_detail",
    "status",
    "title",
    "snapshot",
    "ingest_cursor",
    "next_revision",
    "owes_report",
    "lifecycle",
    "filing",
    "filed_at_ms",
];

/// What an absorb writes: the newest snapshot's envelope and the replica's
/// block.
pub(crate) const ABSORBED: &[&str] = &[
    "status",
    "title",
    "snapshot",
    "source_cursor",
    "source_generation",
    "complete_from_order",
    "exhausted",
];

/// Where a block starts after retention trimmed it.
pub(crate) const BLOCK: &[&str] = &["complete_from_order", "exhausted"];

/// A replica emptied by retention: no block, and nothing its source held.
pub(crate) const EVICTED: &[&str] = &[
    "complete_from_order",
    "exhausted",
    "source_cursor",
    "source_generation",
];

/// The `DO UPDATE SET` list for the named columns.
pub(crate) fn update_of(columns: &[&str]) -> String {
    columns
        .iter()
        .map(|column| format!("{column} = excluded.{column}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// What `put_agent` writes over a held row. An own row takes the registry
/// columns, and one moving to a new incarnation is back in phase starting
/// with a turn owed: the new process has said nothing yet. Its status is
/// then `?{STARTED}`, the held status in phase starting, which
/// [`Rows::upsert_agent`] binds. Its parent incarnation only rises, so a
/// write from a row read before the parent was last heard from does not
/// undo it. A replica takes the registry columns, its origin's parent
/// incarnation and the envelope its inventory row carries.
pub(crate) fn registry_update(own: bool) -> String {
    let registry = update_of(REGISTRY);
    if own {
        // Every expression on the right reads the held row as it was, so
        // `agents.incarnation` is the old one.
        format!(
            "{registry}, \
             status = CASE WHEN excluded.incarnation > agents.incarnation \
               THEN ?{STARTED} ELSE agents.status END, \
             owes_report = CASE WHEN excluded.incarnation > agents.incarnation \
               THEN 1 ELSE agents.owes_report END, \
             parent_incarnation = MAX(agents.parent_incarnation, excluded.parent_incarnation)",
        )
    } else {
        format!(
            "{registry}, parent_incarnation = excluded.parent_incarnation, {}, {}, {}",
            update_of(INVENTORY_ENVELOPE),
            update_of(INVENTORY_COUNTS),
            update_of(EXPERIMENTAL)
        )
    }
}

/// The parameter `registry_update` binds an own row's started status to:
/// the one after every column of [`AGENT_COLUMNS`].
pub(crate) const STARTED: usize = 31;

/// An agents row with its status, snapshot, native and experimental
/// columns still encoded.
type RawAgent = (
    AgentRow,
    Vec<u8>,
    Option<Vec<u8>>,
    Option<Vec<u8>>,
    Option<Vec<u8>>,
);

/// The `experimental` column: a protobuf message whose one field is the
/// row's map, `map<string, bytes> entries = 1`. A map field encodes each
/// entry as the same key-and-value pair whichever message holds it, so the
/// column holds the entries exactly as `Agent.experimental` carries them on
/// the wire, any later build reads it knowing no experiment, and `protoc
/// --decode_raw` shows it. A row with no entries holds NULL, as every row
/// written before the column does.
#[derive(Clone, PartialEq, prost::Message)]
struct ExperimentalColumn {
    #[prost(btree_map = "string, bytes", tag = "1")]
    entries: std::collections::BTreeMap<String, Vec<u8>>,
}

/// The column's value for a row's map.
pub(crate) fn encode_experimental(
    map: &std::collections::BTreeMap<String, Vec<u8>>,
) -> Option<Vec<u8>> {
    (!map.is_empty()).then(|| {
        ExperimentalColumn {
            entries: map.clone(),
        }
        .encode_to_vec()
    })
}

/// The row's map from its column's value.
pub(crate) fn decode_experimental(
    column: Option<&[u8]>,
) -> Result<std::collections::BTreeMap<String, Vec<u8>>, prost::DecodeError> {
    match column {
        None => Ok(Default::default()),
        Some(bytes) => Ok(ExperimentalColumn::decode(bytes)?.entries),
    }
}

fn agent_row(row: &Row<'_>) -> rusqlite::Result<RawAgent> {
    let parent_host: Option<Vec<u8>> = row.get(6)?;
    let parent_id: Option<Vec<u8>> = row.get(7)?;
    Ok((
        AgentRow {
            agent: AgentKey::new(row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?),
            kind: row.get(2)?,
            provider: row.get(3)?,
            name: row.get(4)?,
            cwd: row.get(5)?,
            parent: parent_id.map(|agent| AgentKey::new(parent_host.unwrap_or_default(), agent)),
            lifecycle: row.get(8)?,
            exit_cause: row.get(9)?,
            exit_detail: row.get(10)?,
            status: wire::AgentStatus::default(),
            title: row.get(12)?,
            snapshot: None,
            ingest_cursor: row.get::<_, i64>(14)? as u64,
            next_revision: row.get::<_, i64>(15)? as u64,
            source_cursor: row.get::<_, i64>(16)? as u64,
            source_generation: row.get::<_, i64>(17)? as u64,
            complete_from_order: row.get::<_, Option<i64>>(18)?.map(|order| order as u64),
            exhausted: row.get::<_, i64>(19)? != 0,
            owes_report: row.get::<_, i64>(20)? != 0,
            created_at_ms: row.get(21)?,
            incarnation: row.get(22)?,
            descendants: row.get(23)?,
            filing: row.get(24)?,
            filed_at_ms: row.get(25)?,
            name_given: row.get(26)?,
            native: None,
            experimental: Default::default(),
            parent_incarnation: row.get(29)?,
        },
        row.get(11)?,
        row.get(13)?,
        row.get(27)?,
        row.get(28)?,
    ))
}

/// Decodes the message columns `agent_row` leaves as bytes.
fn finish_agent(
    (mut row, status, snapshot, native, experimental): RawAgent,
) -> Result<AgentRow, StoreError> {
    let corrupt = |error: prost::DecodeError| StoreError::Corrupt(error.to_string());
    row.status = wire::AgentStatus::decode(status.as_slice()).map_err(corrupt)?;
    row.snapshot = snapshot
        .map(|bytes| Snapshot::decode(bytes.as_slice()))
        .transpose()
        .map_err(corrupt)?;
    row.native = native
        .map(|bytes| wire::NativeChild::decode(bytes.as_slice()))
        .transpose()
        .map_err(corrupt)?;
    row.experimental = decode_experimental(experimental.as_deref()).map_err(corrupt)?;
    Ok(row)
}

fn item_row(row: &Row<'_>) -> rusqlite::Result<(Item, Option<Vec<u8>>)> {
    Ok((
        Item {
            key: row.get(0)?,
            order: row.get::<_, i64>(1)? as u64,
            revision: row.get::<_, i64>(2)? as u64,
            at_ms: row.get(3)?,
            input_id: row.get::<_, Option<Vec<u8>>>(4)?.unwrap_or_default(),
            text: row.get(5)?,
            kind: row.get(6)?,
            attachments: Vec::new(),
            body: row.get(8)?,
        },
        row.get(7)?,
    ))
}

fn finish_item((mut item, attachments): (Item, Option<Vec<u8>>)) -> Result<Item, StoreError> {
    item.attachments = decode_attachments(attachments.as_deref().unwrap_or_default())?;
    Ok(item)
}

fn delivery_row(row: &Row<'_>) -> rusqlite::Result<Delivery> {
    use wire::child_report::End;
    let end = if row.get::<_, i64>(6)? != 0 {
        End::Exited(wire::Exited {
            ended: Some(wire::IncarnationEnd {
                cause: row.get::<_, Option<i32>>(8)?.unwrap_or_default(),
                detail: row.get(9)?,
            }),
        })
    } else {
        End::TurnEnded(wire::TurnEnded {
            text: row.get::<_, Option<String>>(7)?.unwrap_or_default(),
            outcome: row.get::<_, Option<i32>>(10)?.unwrap_or_default(),
            last: row.get::<_, i64>(11)? != 0,
        })
    };
    Ok(Delivery {
        child_id: row.get(0)?,
        incarnation: row.get(1)?,
        turn_id: row.get::<_, i64>(2)? as u64,
        parent: AgentKey::new(row.get::<_, Vec<u8>>(3)?, row.get::<_, Vec<u8>>(4)?),
        parent_incarnation: row.get(5)?,
        end,
    })
}

fn notification_row(row: &Row<'_>) -> rusqlite::Result<Notification> {
    Ok(Notification {
        agent_id: row.get(0)?,
        revision: row.get::<_, i64>(1)? as u64,
        due_at_ms: row.get(2)?,
        name: row.get(3)?,
        notice: row.get(4)?,
    })
}

/// A revision or order as SQLite's signed integer, saturating.
fn signed(value: u64) -> i64 {
    value.min(i64::MAX as u64) as i64
}

/// The tables, inside one read or write transaction.
pub(crate) struct Rows<'a> {
    pub(crate) conn: &'a Connection,
    pub(crate) own_host: &'a [u8],
    /// The `suspension/1` experiment's table is there: a row removed takes
    /// its run with it.
    pub(crate) suspension_1: bool,
    /// The `reviews/1` experiment's tables are there: a row removed takes
    /// its review with it.
    pub(crate) reviews_1: bool,
}

impl Rows<'_> {
    pub(crate) fn is_own(&self, agent: &AgentKey) -> bool {
        agent.host == self.own_host
    }

    fn agents_where(
        &self,
        condition: &str,
        params: impl rusqlite::Params,
    ) -> Result<Vec<AgentRow>, StoreError> {
        let mut statement = self.conn.prepare_cached(&format!(
            "SELECT {AGENT_COLUMNS} FROM agents WHERE {condition} ORDER BY origin_host, agent_id"
        ))?;
        let rows = statement
            .query_map(params, agent_row)?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter().map(finish_agent).collect()
    }

    pub(crate) fn agent(&self, agent: &AgentKey) -> Result<Option<AgentRow>, StoreError> {
        Ok(self
            .agents_where(
                "origin_host = ?1 AND agent_id = ?2",
                params![agent.host, agent.agent],
            )?
            .pop())
    }

    pub(crate) fn agents(&self) -> Result<Vec<AgentRow>, StoreError> {
        self.agents_where("1", params![])
    }

    /// The row an agent id names, this host's own first.
    pub(crate) fn agent_by_id(&self, agent_id: &[u8]) -> Result<Option<AgentRow>, StoreError> {
        let mut statement = self.conn.prepare_cached(&format!(
            "SELECT {AGENT_COLUMNS} FROM agents WHERE agent_id = ?1 \
             ORDER BY origin_host = ?2 DESC, origin_host LIMIT 1"
        ))?;
        statement
            .query_row(params![agent_id, self.own_host], agent_row)
            .optional()?
            .map(finish_agent)
            .transpose()
    }

    pub(crate) fn agents_of_parent(&self, parent: &AgentKey) -> Result<Vec<AgentRow>, StoreError> {
        self.agents_where(
            "parent_host = ?1 AND parent_id = ?2",
            params![parent.host, parent.agent],
        )
    }

    pub(crate) fn agents_named(&self, name: &str) -> Result<Vec<AgentRow>, StoreError> {
        self.agents_where("name = ?1", params![name])
    }

    /// Inserts the row whole, or writes `update` (a `DO UPDATE SET` list)
    /// over the held one.
    pub(crate) fn upsert_agent(&self, row: &AgentRow, update: &str) -> Result<(), StoreError> {
        // What an own row moving to a new incarnation keeps of its status:
        // all of it but its phase, which is back at starting.
        let started = if update.contains(&format!("?{STARTED}")) {
            let held = self.agent(&row.agent)?.map(|held| held.status);
            Some(
                wire::AgentStatus {
                    phase: wire::Phase::Starting as i32,
                    ..held.unwrap_or_else(|| row.status.clone())
                }
                .encode_to_vec(),
            )
        } else {
            None
        };
        self.conn
            .prepare_cached(&format!(
                "INSERT INTO agents ({AGENT_COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, \
                 ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, \
                 ?25, ?26, ?27, ?28, ?29, ?30) ON CONFLICT (origin_host, agent_id) DO UPDATE SET {update}"
            ))?
            .execute(rusqlite::params_from_iter(
                params![
                    row.agent.host,
                    row.agent.agent,
                    row.kind,
                    row.provider,
                    row.name,
                    row.cwd,
                    row.parent.as_ref().map(|parent| &parent.host),
                    row.parent.as_ref().map(|parent| &parent.agent),
                    row.lifecycle,
                    row.exit_cause,
                    row.exit_detail,
                    row.status.encode_to_vec(),
                    row.title,
                    row.snapshot
                        .as_ref()
                        .map(|snapshot| snapshot.encode_to_vec()),
                    signed(row.ingest_cursor),
                    signed(row.next_revision),
                    signed(row.source_cursor),
                    signed(row.source_generation),
                    row.complete_from_order.map(signed),
                    i64::from(row.exhausted),
                    i64::from(row.owes_report),
                    row.created_at_ms,
                    row.incarnation,
                    row.descendants,
                    row.filing,
                    row.filed_at_ms,
                    row.name_given,
                    row.native.as_ref().map(|native| native.encode_to_vec()),
                    encode_experimental(&row.experimental),
                    row.parent_incarnation,
                ]
                .iter()
                .copied()
                .chain(started.as_ref().map(|started| started as &dyn rusqlite::ToSql)),
            ))?;
        Ok(())
    }

    /// The live agents below `parent`, as far as the rows held say: an own
    /// row is walked into, and a replica row counts itself and the
    /// descendants its origin counted, since its origin knows its family
    /// better than this host does.
    pub(crate) fn live_descendants(&self, parent: &AgentKey) -> Result<u32, StoreError> {
        let count: i64 = self.conn.query_row(
            "WITH RECURSIVE below (origin_host, agent_id) AS ( \
               SELECT origin_host, agent_id FROM agents \
                 WHERE parent_host = ?1 AND parent_id = ?2 \
               UNION \
               SELECT agents.origin_host, agents.agent_id FROM agents JOIN below \
                 ON agents.parent_host = below.origin_host \
                 AND agents.parent_id = below.agent_id \
                 WHERE below.origin_host = ?3 \
             ) \
             SELECT IFNULL(SUM((agents.lifecycle = ?4) \
               + CASE WHEN agents.origin_host = ?3 THEN 0 ELSE agents.descendants END), 0) \
             FROM below JOIN agents USING (origin_host, agent_id)",
            params![
                parent.host,
                parent.agent,
                self.own_host,
                wire::Lifecycle::Live as i32
            ],
            |row| row.get(0),
        )?;
        Ok(count.clamp(0, i64::from(u32::MAX)) as u32)
    }

    /// Moves an own row's ingest cursor back to `cursor`, never forward.
    /// False when there is no such row.
    pub(crate) fn rewind_cursor(&self, agent: &AgentKey, cursor: u64) -> Result<bool, StoreError> {
        let changed = self.conn.execute(
            "UPDATE agents SET ingest_cursor = MIN(ingest_cursor, ?3) \
             WHERE origin_host = ?1 AND agent_id = ?2",
            params![agent.host, agent.agent, signed(cursor)],
        )?;
        Ok(changed > 0)
    }

    /// Removes the row, its items, its deliveries and its notifications.
    pub(crate) fn remove_agent(&self, agent: &AgentKey) -> Result<(), StoreError> {
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
        if self.suspension_1 && self.is_own(agent) {
            self.put_run_start(&agent.agent, None)?;
        }
        if self.reviews_1 && self.is_own(agent) {
            crate::reviews::remove_agent(self, &agent.agent)?;
        }
        self.remove_notifications(&agent.agent)
    }

    /// The incarnation an own agent's current run began at, from the
    /// `suspension/1` table.
    pub(crate) fn run_start(&self, agent: &[u8]) -> Result<Option<u32>, StoreError> {
        Ok(self
            .conn
            .prepare_cached(&format!(
                "SELECT incarnation FROM {SUSPENSION_1_RUNS} WHERE agent_id = ?1"
            ))?
            .query_row(params![agent], |row| row.get::<_, u32>(0))
            .optional()?)
    }

    /// Writes an own agent's `suspension/1` run, or removes it.
    pub(crate) fn put_run_start(
        &self,
        agent: &[u8],
        incarnation: Option<u32>,
    ) -> Result<(), StoreError> {
        match incarnation {
            Some(incarnation) => self
                .conn
                .prepare_cached(&format!(
                    "INSERT INTO {SUSPENSION_1_RUNS} (agent_id, incarnation) VALUES (?1, ?2) \
                     ON CONFLICT (agent_id) DO UPDATE SET incarnation = excluded.incarnation"
                ))?
                .execute(params![agent, incarnation])?,
            None => self
                .conn
                .prepare_cached(&format!(
                    "DELETE FROM {SUSPENSION_1_RUNS} WHERE agent_id = ?1"
                ))?
                .execute(params![agent])?,
        };
        Ok(())
    }

    fn items_where(
        &self,
        agent: &AgentKey,
        condition: &str,
        params: &[&dyn rusqlite::ToSql],
    ) -> Result<Vec<Item>, StoreError> {
        let mut statement = self.conn.prepare_cached(&format!(
            "SELECT {ITEM_COLUMNS} FROM items WHERE origin_host = ?1 AND agent_id = ?2 {condition}"
        ))?;
        let mut all: Vec<&dyn rusqlite::ToSql> = vec![&agent.host, &agent.agent];
        all.extend_from_slice(params);
        let rows = statement
            .query_map(all.as_slice(), item_row)?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter().map(finish_item).collect()
    }

    pub(crate) fn item(&self, agent: &AgentKey, key: &str) -> Result<Option<Item>, StoreError> {
        Ok(self.items_where(agent, "AND key = ?3", &[&key])?.pop())
    }

    /// An item whose input id is `input_id`, if the agent holds one.
    pub(crate) fn item_by_input(
        &self,
        agent: &AgentKey,
        input_id: &[u8],
    ) -> Result<Option<Item>, StoreError> {
        Ok(self
            .items_where(agent, "AND input_id = ?3 LIMIT 1", &[&input_id])?
            .pop())
    }

    /// One held item's text, empty when the key is not held.
    pub(crate) fn item_text(&self, agent: &AgentKey, key: &str) -> Result<String, StoreError> {
        Ok(self
            .conn
            .prepare_cached(
                "SELECT text FROM items WHERE origin_host = ?1 AND agent_id = ?2 AND key = ?3",
            )?
            .query_row(params![agent.host, agent.agent, key], |row| row.get(0))
            .optional()?
            .unwrap_or_default())
    }

    /// Writes an own item: a new key takes `next_position`, a held key keeps
    /// its own. Returns the position the item has.
    pub(crate) fn commit_item(
        &self,
        agent: &AgentKey,
        item: &Item,
        next_position: u64,
    ) -> Result<u64, StoreError> {
        let position: i64 = self
            .conn
            .prepare_cached(&format!(
                "INSERT INTO items (origin_host, agent_id, {ITEM_COLUMNS}) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11) \
                 ON CONFLICT (origin_host, agent_id, key) DO UPDATE SET \
                 revision = excluded.revision, at_ms = excluded.at_ms, \
                 input_id = excluded.input_id, text = excluded.text, kind = excluded.kind, \
                 attachments = excluded.attachments, body = excluded.body \
                 RETURNING position"
            ))?
            .query_row(
                rusqlite::params_from_iter(item_params(agent, item, next_position)),
                |row| row.get(0),
            )?;
        Ok(position as u64)
    }

    /// Writes a replica item unless a revision at least as new is held.
    /// Returns whether it was written.
    pub(crate) fn absorb_item(&self, agent: &AgentKey, item: &Item) -> Result<bool, StoreError> {
        let changed = self
            .conn
            .prepare_cached(&format!(
                "INSERT INTO items (origin_host, agent_id, {ITEM_COLUMNS}) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11) \
                 ON CONFLICT (origin_host, agent_id, key) DO UPDATE SET \
                 position = excluded.position, revision = excluded.revision, \
                 at_ms = excluded.at_ms, input_id = excluded.input_id, \
                 text = excluded.text, kind = excluded.kind, \
                 attachments = excluded.attachments, body = excluded.body \
                 WHERE excluded.revision > items.revision"
            ))?
            .execute(rusqlite::params_from_iter(item_params(
                agent, item, item.order,
            )))?;
        Ok(changed > 0)
    }

    /// Appends `text` to a held item and gives it `revision`, when the item
    /// is held and, if `base` names one, at that revision. Returns the
    /// revision it had.
    pub(crate) fn extend(
        &self,
        agent: &AgentKey,
        key: &str,
        text: &str,
        revision: u64,
        base: Option<u64>,
    ) -> Result<Option<u64>, StoreError> {
        let held: Option<(i64, i64)> = self
            .conn
            .prepare_cached(
                "SELECT position, revision FROM items \
                 WHERE origin_host = ?1 AND agent_id = ?2 AND key = ?3",
            )?
            .query_row(params![agent.host, agent.agent, key], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .optional()?;
        let Some((position, held)) = held else {
            return Ok(None);
        };
        let held = held as u64;
        if base.is_some_and(|base| base != held) {
            return Ok(None);
        }
        // The key is held in this transaction, so the insert always meets
        // its conflict and only the update runs; its values fill the
        // columns an insert would need and are never stored.
        self.conn
            .prepare_cached(
                "INSERT INTO items (origin_host, agent_id, key, position, revision, at_ms, \
                 text, kind, body) \
                 VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6, 0, x'') \
                 ON CONFLICT (origin_host, agent_id, key) DO UPDATE SET \
                 text = items.text || excluded.text, revision = excluded.revision",
            )?
            .execute(params![
                agent.host,
                agent.agent,
                key,
                position,
                signed(revision),
                text
            ])?;
        Ok(Some(held))
    }

    pub(crate) fn max_position(&self, agent: &AgentKey) -> Result<Option<u64>, StoreError> {
        let max: Option<i64> = self
            .conn
            .prepare_cached(
                "SELECT MAX(position) FROM items WHERE origin_host = ?1 AND agent_id = ?2",
            )?
            .query_row(params![agent.host, agent.agent], |row| row.get(0))?;
        Ok(max.map(|position| position as u64))
    }

    /// Items with `min_position <= position < below`, newest first.
    pub(crate) fn items_desc(
        &self,
        agent: &AgentKey,
        below: Option<u64>,
        min_position: u64,
        limit: u32,
    ) -> Result<Vec<Item>, StoreError> {
        let below = below.map_or(i64::MAX, signed);
        self.items_where(
            agent,
            "AND position >= ?3 AND position < ?4 ORDER BY position DESC LIMIT ?5",
            &[&signed(min_position), &below, &limit],
        )
    }

    /// Items whose revision is above `revision`, oldest revision first.
    pub(crate) fn items_after(
        &self,
        agent: &AgentKey,
        revision: u64,
        limit: u32,
    ) -> Result<Vec<Item>, StoreError> {
        self.items_where(
            agent,
            "AND revision > ?3 ORDER BY revision LIMIT ?4",
            &[&signed(revision), &limit],
        )
    }

    pub(crate) fn put_delivery(&self, delivery: &Delivery) -> Result<(), StoreError> {
        use wire::child_report::End;
        let (text, ended, outcome, last) = match &delivery.end {
            End::TurnEnded(turn) => (Some(turn.text.clone()), None, Some(turn.outcome), turn.last),
            End::Exited(exited) => (None, exited.ended.clone(), None, false),
        };
        self.conn
            .prepare_cached(
                "INSERT INTO deliveries (child_id, incarnation, turn_id, parent_incarnation, \
                 exited, text, exit_cause, exit_detail, outcome, last) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10) \
                 ON CONFLICT (child_id, incarnation, exited, turn_id) DO UPDATE SET \
                 parent_incarnation = excluded.parent_incarnation, text = excluded.text, \
                 exit_cause = excluded.exit_cause, exit_detail = excluded.exit_detail, \
                 outcome = excluded.outcome, last = excluded.last",
            )?
            .execute(params![
                delivery.child_id,
                delivery.incarnation,
                signed(delivery.turn_id),
                delivery.parent_incarnation,
                i64::from(delivery.is_exit()),
                text,
                ended.as_ref().map(|end| end.cause),
                ended.and_then(|end| end.detail),
                outcome,
                i64::from(last),
            ])?;
        Ok(())
    }

    pub(crate) fn deliveries(&self) -> Result<Vec<Delivery>, StoreError> {
        let mut statement = self.conn.prepare_cached(
            "SELECT d.child_id, d.incarnation, d.turn_id, IFNULL(a.parent_host, x''), \
             a.parent_id, d.parent_incarnation, d.exited, d.text, d.exit_cause, d.exit_detail, \
             d.outcome, d.last FROM deliveries d JOIN agents a \
             ON a.origin_host = ?1 AND a.agent_id = d.child_id AND a.parent_id IS NOT NULL \
             ORDER BY d.child_id, d.incarnation, d.exited, d.turn_id",
        )?;
        let rows = statement
            .query_map([self.own_host], delivery_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub(crate) fn remove_delivery(&self, delivery: &Delivery) -> Result<(), StoreError> {
        self.conn.execute(
            "DELETE FROM deliveries WHERE child_id = ?1 AND incarnation = ?2 AND exited = ?3 \
             AND turn_id = ?4",
            params![
                delivery.child_id,
                delivery.incarnation,
                i64::from(delivery.is_exit()),
                signed(delivery.turn_id)
            ],
        )?;
        Ok(())
    }

    pub(crate) fn put_notification(&self, notification: &Notification) -> Result<(), StoreError> {
        self.conn
            .prepare_cached(
                "INSERT INTO notifications (agent_id, revision, due_at_ms, name, notice) \
                 VALUES (?1, ?2, ?3, ?4, ?5) \
                 ON CONFLICT (agent_id, revision) DO UPDATE SET due_at_ms = excluded.due_at_ms, \
                 name = excluded.name, notice = excluded.notice",
            )?
            .execute(params![
                notification.agent_id,
                signed(notification.revision),
                notification.due_at_ms,
                notification.name,
                notification.notice,
            ])?;
        Ok(())
    }

    pub(crate) fn notifications(&self) -> Result<Vec<Notification>, StoreError> {
        let mut statement = self.conn.prepare_cached(
            "SELECT agent_id, revision, due_at_ms, name, notice FROM notifications \
             ORDER BY agent_id, revision",
        )?;
        let rows = statement
            .query_map([], notification_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub(crate) fn remove_notifications(&self, agent_id: &[u8]) -> Result<(), StoreError> {
        self.conn.execute(
            "DELETE FROM notifications WHERE agent_id = ?1",
            params![agent_id],
        )?;
        Ok(())
    }

    pub(crate) fn remove_notification(
        &self,
        agent_id: &[u8],
        revision: u64,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "DELETE FROM notifications WHERE agent_id = ?1 AND revision = ?2",
            params![agent_id, signed(revision)],
        )?;
        Ok(())
    }

    pub(crate) fn trusted_hosts(&self) -> Result<Vec<TrustedHost>, StoreError> {
        let mut read = self.conn.prepare_cached(
            "SELECT host_id, pubkey, name, on_account, signed_in, platform, ssh, direct, listed, \
             push_key FROM trusted_hosts ORDER BY host_id",
        )?;
        let hosts = read
            .query_map([], |row| {
                Ok(TrustedHost {
                    host_id: row.get(0)?,
                    pubkey: row.get(1)?,
                    name: row.get(2)?,
                    on_account: row.get(3)?,
                    signed_in: row.get(4)?,
                    platform: row.get(5)?,
                    ssh: row.get(6)?,
                    direct: row.get(7)?,
                    listed: row.get(8)?,
                    push_key: row.get(9)?,
                })
            })?
            .collect::<Result<_, _>>()?;
        Ok(hosts)
    }

    pub(crate) fn put_trusted_host(&self, host: &TrustedHost) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO trusted_hosts (host_id, pubkey, name, on_account, \
             signed_in, platform, ssh, direct, listed, push_key) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10) \
             ON CONFLICT (host_id) DO UPDATE SET pubkey = excluded.pubkey, \
             name = excluded.name, on_account = excluded.on_account, signed_in = excluded.signed_in, \
             platform = excluded.platform, ssh = excluded.ssh, direct = excluded.direct, \
             listed = excluded.listed, push_key = excluded.push_key",
            params![
                host.host_id,
                host.pubkey,
                host.name,
                host.on_account,
                host.signed_in,
                host.platform,
                host.ssh,
                host.direct,
                host.listed,
                host.push_key,
            ],
        )?;
        Ok(())
    }

    pub(crate) fn remove_trusted_host(&self, host: &[u8]) -> Result<(), StoreError> {
        self.conn.execute(
            "DELETE FROM trusted_hosts WHERE host_id = ?1",
            params![host],
        )?;
        self.conn
            .execute("DELETE FROM hosts WHERE host_id = ?1", params![host])?;
        Ok(())
    }

    pub(crate) fn draft(&self, agent_id: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
        Ok(self
            .conn
            .query_row(
                "SELECT draft FROM drafts WHERE agent_id = ?1",
                params![agent_id],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub(crate) fn put_draft(&self, agent_id: &[u8], draft: &[u8]) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO drafts (agent_id, draft) VALUES (?1, ?2) \
             ON CONFLICT (agent_id) DO UPDATE SET draft = excluded.draft",
            params![agent_id, draft],
        )?;
        Ok(())
    }

    pub(crate) fn delete_draft(&self, agent_id: &[u8]) -> Result<bool, StoreError> {
        Ok(self
            .conn
            .execute("DELETE FROM drafts WHERE agent_id = ?1", params![agent_id])?
            > 0)
    }

    pub(crate) fn record_recent_directory(&self, path: &str, at_ms: i64) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO recent_directories (path, last_used_ms) VALUES (?1, ?2) \
             ON CONFLICT (path) DO UPDATE SET \
             last_used_ms = max(last_used_ms, excluded.last_used_ms)",
            params![path, at_ms],
        )?;
        self.conn.execute(
            "DELETE FROM recent_directories WHERE path NOT IN \
             (SELECT path FROM recent_directories ORDER BY last_used_ms DESC, path LIMIT ?1)",
            params![crate::RECENT_DIRECTORIES as i64],
        )?;
        Ok(())
    }

    pub(crate) fn recent_directories(&self) -> Result<Vec<(String, i64)>, StoreError> {
        let mut read = self.conn.prepare_cached(
            "SELECT path, last_used_ms FROM recent_directories \
             ORDER BY last_used_ms DESC, path",
        )?;
        let directories = read
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<_, _>>()?;
        Ok(directories)
    }

    pub(crate) fn host_generation(&self, host: &[u8]) -> Result<Option<u64>, StoreError> {
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

    pub(crate) fn set_host_generation(
        &self,
        host: &[u8],
        generation: u64,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO hosts (host_id, generation) VALUES (?1, ?2) \
             ON CONFLICT (host_id) DO UPDATE SET generation = excluded.generation",
            params![host, signed(generation)],
        )?;
        Ok(())
    }

    /// The bytes own rows (or replica rows) hold, by [`ITEM_BYTES`]: from
    /// the running totals [`keep_pool_totals`] keeps, not a sum over `items`.
    pub(crate) fn pool_bytes(&self, own: bool) -> Result<u64, StoreError> {
        let bytes: i64 = self
            .conn
            .prepare_cached(
                "SELECT IFNULL(SUM(bytes), 0) FROM temp.item_pools \
                 WHERE (origin_host = ?1) = ?2",
            )?
            .query_row(params![self.own_host, own], |row| row.get(0))?;
        Ok(bytes as u64)
    }

    /// The bytes own rows (or replica rows) hold, summed over every row of
    /// `items`: what [`Rows::pool_bytes`] must always equal.
    #[cfg(test)]
    pub(crate) fn pool_bytes_counted(&self, own: bool) -> Result<u64, StoreError> {
        let bytes: i64 = self
            .conn
            .prepare_cached(&format!(
                "SELECT IFNULL(SUM({}), 0) FROM items WHERE (origin_host = ?1) = ?2",
                item_bytes("")
            ))?
            .query_row(params![self.own_host, own], |row| row.get(0))?;
        Ok(bytes as u64)
    }

    /// The agent's held rows: their total [`ITEM_BYTES`] and count.
    pub(crate) fn agent_bytes(&self, agent: &AgentKey) -> Result<(u64, u64), StoreError> {
        let (bytes, rows): (i64, i64) = self
            .conn
            .prepare_cached(&format!(
                "SELECT IFNULL(SUM({}), 0), COUNT(*) FROM items \
                 WHERE origin_host = ?1 AND agent_id = ?2",
                item_bytes("")
            ))?
            .query_row(params![agent.host, agent.agent], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?;
        Ok((bytes as u64, rows as u64))
    }

    /// The oldest held rows' positions and [`ITEM_BYTES`], oldest
    /// first.
    pub(crate) fn oldest_items(
        &self,
        agent: &AgentKey,
        limit: u32,
    ) -> Result<Vec<(u64, u64)>, StoreError> {
        let mut statement = self.conn.prepare_cached(&format!(
            "SELECT position, {} FROM items WHERE origin_host = ?1 AND agent_id = ?2 \
             ORDER BY position LIMIT ?3",
            item_bytes("")
        ))?;
        let rows = statement
            .query_map(params![agent.host, agent.agent, limit], |row| {
                Ok((row.get::<_, i64>(0)? as u64, row.get::<_, i64>(1)? as u64))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub(crate) fn remove_items_below(
        &self,
        agent: &AgentKey,
        position: u64,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "DELETE FROM items WHERE origin_host = ?1 AND agent_id = ?2 AND position < ?3",
            params![agent.host, agent.agent, signed(position)],
        )?;
        Ok(())
    }

    /// Removes every item the agent holds.
    pub(crate) fn remove_items(&self, agent: &AgentKey) -> Result<(), StoreError> {
        self.conn.execute(
            "DELETE FROM items WHERE origin_host = ?1 AND agent_id = ?2",
            params![agent.host, agent.agent],
        )?;
        Ok(())
    }
}

/// An item's eleven column values, in `ITEM_COLUMNS` order after its agent.
fn item_params<'a>(
    agent: &'a AgentKey,
    item: &'a Item,
    position: u64,
) -> Vec<Box<dyn rusqlite::ToSql + 'a>> {
    vec![
        Box::new(&agent.host),
        Box::new(&agent.agent),
        Box::new(&item.key),
        Box::new(signed(position)),
        Box::new(signed(item.revision)),
        Box::new(item.at_ms),
        Box::new((!item.input_id.is_empty()).then_some(&item.input_id)),
        Box::new(&item.text),
        Box::new(item.kind),
        Box::new((!item.attachments.is_empty()).then(|| encode_attachments(&item.attachments))),
        Box::new(&item.body),
    ]
}
