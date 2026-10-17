//! The profile file: SQLite in WAL mode with synchronous=NORMAL, so a
//! commit costs no fsync and power loss can lose the newest transactions
//! but never corrupt the file.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use rusqlite::{Connection, TransactionBehavior, params};

use crate::migrations::{MIGRATIONS, SCHEMA_STAMP, shipped_hash};
use crate::retention::{self, Sweep};
use crate::rows::{self, ABSORBED, COMMITTED, Rows};
use crate::{
    Absorb, Absorbed, AgentKey, AgentRow, Append, CommitClock, Committed, Cut, Delivery, Item,
    Marker, Notification, Page, PageEnd, Record, Snapshot, SourceEvent, Step, StoreError,
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
    /// The store's recorded history is not this build's: written before
    /// the release schema, or by a build whose migration text differs. The
    /// file is left exactly as it is.
    #[error(
        "{path} predates this release's store schema (its migration {id} is not this build's); \
         amux leaves it untouched: move it aside to start this profile's store afresh"
    )]
    EditedMigration { path: String, id: u32 },
}

pub struct Sqlite {
    pub(crate) conn: Connection,
    pub(crate) own_host: Vec<u8>,
    pub(crate) markers: HashMap<AgentKey, Marker>,
    /// How many resets each replica row has absorbed since the store
    /// opened: what a replica that was told to forget its block went
    /// through, for the checks that judge it.
    pub(crate) resets: HashMap<AgentKey, u64>,
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
    /// rows it holds, applying any migrations it lacks. A store whose
    /// recorded history is not this build's is refused and left untouched.
    pub fn open(path: &Path, own_host: impl Into<Vec<u8>>) -> Result<Self, OpenError> {
        let conn = Connection::open(path)?;
        Self::with_connection(conn, &path.display().to_string(), own_host.into())
    }

    /// A store with the same schema that lives only in memory: gone when
    /// it is dropped, for exploring the store's own transitions.
    pub fn open_in_memory(own_host: impl Into<Vec<u8>>) -> Result<Self, OpenError> {
        let conn = Connection::open_in_memory()?;
        Self::with_connection(conn, ":memory:", own_host.into())
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
            resets: HashMap::new(),
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
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let recorded: bool = tx.query_row(
        "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE name = 'schema_migrations')",
        [],
        |row| row.get(0),
    )?;
    let applied = if recorded {
        let mut statement = tx.prepare("SELECT id, sha256 FROM schema_migrations ORDER BY id")?;
        statement
            .query_map([], |row| {
                Ok((row.get::<_, u32>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?
    } else {
        Vec::new()
    };
    // History before the stamp: a store from before the release schema
    // carries a higher stamp than this build knows, and is the past, not
    // the future.
    for (id, hash) in &applied {
        let index = (*id as usize).wrapping_sub(1);
        if index < MIGRATIONS.len() && shipped_hash(index) != hash {
            return Err(OpenError::EditedMigration {
                path: name.into(),
                id: *id,
            });
        }
    }
    let known = MIGRATIONS.len() as u32;
    if stamp > known {
        return Err(OpenError::TooNew { stamp, known });
    }
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (id INTEGER PRIMARY KEY, sha256 TEXT NOT NULL);",
    )?;
    tx.commit()?;
    let done = applied.last().map_or(0, |(id, _)| *id);
    for (index, text) in MIGRATIONS.iter().enumerate().skip(done as usize) {
        let id = index as u32 + 1;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_batch(text)?;
        tx.execute(
            "INSERT INTO schema_migrations (id, sha256) VALUES (?1, ?2)",
            params![id, shipped_hash(index)],
        )?;
        tx.pragma_update(None, "application_id", APPLICATION_ID)?;
        let current: u32 = tx.pragma_query_value(None, "user_version", |row| row.get(0))?;
        tx.pragma_update(None, "user_version", current.max(SCHEMA_STAMP))?;
        tx.commit()?;
    }
    Ok(())
}

/// The lowest order the block counts from.
pub(crate) fn block_floor(row: &AgentRow, own: bool) -> Option<u64> {
    match (own, row.complete_from_order) {
        (_, Some(order)) => Some(order),
        (true, None) => Some(0),
        (false, None) => None,
    }
}

/// The revision of the snapshot a row holds; 0 before its first.
fn held_snapshot(row: &AgentRow) -> u64 {
    row.snapshot
        .as_ref()
        .map_or(0, |snapshot| snapshot.revision)
}

/// Copies the envelope fields a snapshot carries onto its row.
fn copy_envelope(row: &mut AgentRow, snapshot: &Snapshot) {
    row.phase = snapshot.phase;
    row.working_on = snapshot.working_on.clone();
    row.last_activity_ms = Some(snapshot.at_ms);
    row.phase_since_ms = Some(snapshot.phase_since_ms);
    row.git = snapshot.git.clone();
    row.background_jobs = snapshot.background_jobs;
    row.snapshot = Some(snapshot.clone());
}

/// The newest `n` rows of the block, oldest first.
fn last_n(rows: &Rows<'_>, agent: &AgentKey, n: u32) -> Result<Vec<Item>, StoreError> {
    let row = rows.agent(agent)?.ok_or(StoreError::UnknownAgent)?;
    let Some(floor) = block_floor(&row, rows.is_own(agent)) else {
        return Ok(Vec::new());
    };
    let mut items = rows.items_desc(agent, None, floor, n)?;
    items.reverse();
    Ok(items)
}

impl Sqlite {
    /// Runs `f` in one read transaction: every read inside sees one commit
    /// point.
    pub(crate) fn read<R>(
        &self,
        f: impl FnOnce(&Rows<'_>) -> Result<R, StoreError>,
    ) -> Result<R, StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        let result = f(&Rows {
            conn: &tx,
            own_host: &self.own_host,
        });
        tx.finish()?;
        result
    }

    /// Runs `f` in one write transaction: all of it lands or none of it
    /// does.
    fn write<R>(
        &mut self,
        f: impl FnOnce(&Rows<'_>) -> Result<R, StoreError>,
    ) -> Result<R, StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let result = f(&Rows {
            conn: &tx,
            own_host: &self.own_host,
        })?;
        tx.commit()?;
        Ok(result)
    }

    pub fn own_host(&self) -> &[u8] {
        &self.own_host
    }

    pub(crate) fn is_own(&self, agent: &AgentKey) -> bool {
        agent.host == self.own_host
    }

    /// The markers set on agents, read once by a subscriber at join.
    pub fn markers(&self) -> &HashMap<AgentKey, Marker> {
        &self.markers
    }

    pub fn markers_mut(&mut self) -> &mut HashMap<AgentKey, Marker> {
        &mut self.markers
    }

    pub fn agent(&self, agent: &AgentKey) -> Result<Option<AgentRow>, StoreError> {
        self.read(|rows| rows.agent(agent))
    }

    /// Every agents row, own and replica.
    pub fn agents(&self) -> Result<Vec<AgentRow>, StoreError> {
        self.read(|rows| rows.agents())
    }

    /// The row an agent id names, whichever host owns it: this host's own
    /// when it holds one, since an agent id is one agent everywhere.
    pub fn agent_by_id(&self, agent_id: &[u8]) -> Result<Option<AgentRow>, StoreError> {
        self.read(|rows| rows.agent_by_id(agent_id))
    }

    /// The rows whose parent is `parent`, on any host.
    pub fn agents_of_parent(&self, parent: &AgentKey) -> Result<Vec<AgentRow>, StoreError> {
        self.read(|rows| rows.agents_of_parent(parent))
    }

    /// Every row with this exact name, own and replica: more than one when
    /// two hosts chose the same name.
    pub fn agent_by_name(&self, name: &str) -> Result<Vec<AgentRow>, StoreError> {
        self.read(|rows| rows.agents_named(name))
    }

    /// Items by order, newest first, from the block only.
    pub fn page(
        &self,
        agent: &AgentKey,
        before_order: Option<u64>,
        limit: u32,
    ) -> Result<Page, StoreError> {
        let own = self.is_own(agent);
        self.read(|rows| {
            let row = rows.agent(agent)?.ok_or(StoreError::UnknownAgent)?;
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
            let items = rows.items_desc(agent, before_order, floor, limit)?;
            let end = if items.len() == limit as usize {
                PageEnd::More
            } else {
                bottom
            };
            Ok(Page { items, end })
        })
    }

    /// One held item, in the block or not.
    pub fn get(&self, agent: &AgentKey, key: &str) -> Result<Option<Item>, StoreError> {
        self.read(|rows| rows.item(agent, key))
    }

    /// The item that carries `input_id`: for an agent message, the proof
    /// that its recipient accepted it.
    pub fn item_by_input(
        &self,
        agent: &AgentKey,
        input_id: &[u8],
    ) -> Result<Option<Item>, StoreError> {
        if input_id.is_empty() {
            return Ok(None);
        }
        self.read(|rows| rows.item_by_input(agent, input_id))
    }

    /// The newest `n` rows of the block, oldest first.
    pub fn last_n(&self, agent: &AgentKey, n: u32) -> Result<Vec<Item>, StoreError> {
        self.read(|rows| last_n(rows, agent, n))
    }

    /// Replica rows: the source cursor. Own rows: the ingest cursor.
    pub fn cursor(&self, agent: &AgentKey) -> Result<u64, StoreError> {
        let row = self.agent(agent)?.ok_or(StoreError::UnknownAgent)?;
        Ok(if self.is_own(agent) {
            row.ingest_cursor
        } else {
            row.source_cursor
        })
    }

    /// The snapshot, the newest `n` rows and the marker, read together.
    pub fn cut(&self, agent: &AgentKey, n: u32) -> Result<Cut, StoreError> {
        let marker = self.markers.get(agent).copied();
        self.read(|rows| {
            let row = rows.agent(agent)?.ok_or(StoreError::UnknownAgent)?;
            Ok(Cut {
                snapshot: row.snapshot,
                held: last_n(rows, agent, n)?,
                marker,
            })
        })
    }

    /// What a peer that holds everything through `revision` is missing:
    /// every row revised since, in revision order, when there are at most
    /// `cap` of them; None when there are more, and a tail is the answer.
    pub fn after(
        &self,
        agent: &AgentKey,
        revision: u64,
        cap: u32,
    ) -> Result<Option<Vec<Item>>, StoreError> {
        self.read(|rows| {
            let items = rows.items_after(agent, revision, cap.saturating_add(1))?;
            Ok((items.len() <= cap as usize).then_some(items))
        })
    }

    pub fn set_marker(&mut self, agent: &AgentKey, marker: Option<Marker>) {
        match marker {
            Some(marker) => {
                self.markers.insert(agent.clone(), marker);
            }
            None => {
                self.markers.remove(agent);
            }
        }
    }

    /// Creates a row whole, or updates the columns its role owns: for an
    /// own row its registry columns (kind, name, cwd, parent, lifecycle,
    /// exit cause and detail, creation time and incarnation), and for a
    /// replica those and the phase, working_on, phase_since and git its
    /// inventory row carries. Committed state is kept, except that an own
    /// row moving to a new incarnation is back in phase starting: the new
    /// process has said nothing yet.
    pub fn put_agent(&mut self, row: &AgentRow) -> Result<(), StoreError> {
        let update = rows::registry_update(self.is_own(&row.agent));
        self.write(|rows| rows.upsert_agent(row, &update))
    }

    /// The live agents below `parent` in the rows held: own rows walked
    /// into, each replica row counting itself and what its origin counted
    /// below it.
    pub fn live_descendants(&self, parent: &AgentKey) -> Result<u32, StoreError> {
        self.read(|rows| rows.live_descendants(parent))
    }

    /// Writes an own row's count of live descendants; false when there is
    /// no such row.
    pub fn set_descendants(&mut self, agent: &AgentKey, count: u32) -> Result<bool, StoreError> {
        self.write(|rows| {
            let Some(mut row) = rows.agent(agent)? else {
                return Ok(false);
            };
            row.descendants = count;
            rows.upsert_agent(&row, &rows::update_of(&["descendants"]))?;
            Ok(true)
        })
    }

    /// Removes an agent whole: row, items, deliveries and notifications.
    pub fn delete_agent(&mut self, agent: &AgentKey) -> Result<(), StoreError> {
        self.markers.remove(agent);
        self.resets.remove(agent);
        self.write(|rows| rows.remove_agent(agent))
    }

    /// Commits journal frames to an own row in one transaction.
    pub fn commit(
        &mut self,
        agent: &AgentKey,
        frames: &[(u64, Step)],
        clock: CommitClock,
    ) -> Result<Committed, StoreError> {
        if !self.is_own(agent) {
            return Err(StoreError::NotOwn);
        }
        self.write(|rows| commit(rows, agent, frames, clock))
    }

    /// Moves an own row's ingest cursor back to where its journal now
    /// ends, after a power cut took the journal's tail but not the store's
    /// commit of it. Everything committed stays, and so does the next
    /// revision: what the agent writes again is new to every reader.
    pub fn rewind_cursor(&mut self, agent: &AgentKey, cursor: u64) -> Result<(), StoreError> {
        if !self.is_own(agent) {
            return Err(StoreError::NotOwn);
        }
        if self.write(|rows| rows.rewind_cursor(agent, cursor))? {
            Ok(())
        } else {
            Err(StoreError::UnknownAgent)
        }
    }

    pub fn absorb(&mut self, agent: &AgentKey, what: Absorb) -> Result<Absorbed, StoreError> {
        if self.is_own(agent) {
            return Err(StoreError::NotReplica);
        }
        let marker = match &what {
            Absorb::CaughtUp(_) => Some(Some(Marker::CaughtUp)),
            Absorb::Reset { .. } => Some(None),
            _ => None,
        };
        let reset = matches!(what, Absorb::Reset { .. });
        let absorbed = self.write(|rows| absorb(rows, agent, what))?;
        if let Some(marker) = marker {
            self.set_marker(agent, marker);
        }
        if reset {
            *self.resets.entry(agent.clone()).or_default() += 1;
        }
        Ok(absorbed)
    }

    /// Records the generation `host`'s inventory last reported.
    pub fn record_host_generation(
        &mut self,
        host: &[u8],
        generation: u64,
    ) -> Result<(), StoreError> {
        if host == self.own_host {
            return Err(StoreError::OwnHost);
        }
        self.write(|rows| rows.set_host_generation(host, generation))
    }

    pub fn host_generation(&self, host: &[u8]) -> Result<Option<u64>, StoreError> {
        self.read(|rows| rows.host_generation(host))
    }

    /// The bytes own rows (or replica rows) hold against their budget.
    pub fn pool_bytes(&self, own: bool) -> Result<u64, StoreError> {
        self.read(|rows| rows.pool_bytes(own))
    }

    /// Brings own rows under `budget` bytes: exited agents whole, least
    /// recently active first, never an exited child whose parent's row says
    /// live; then the largest remaining agent trimmed by about `chunk`
    /// bytes per round, never below its newest `floor_k` rows. Returns what
    /// it removed, so the daemon deletes the removed agents' directories.
    pub fn sweep_own(
        &mut self,
        budget: u64,
        chunk: u64,
        floor_k: u32,
    ) -> Result<Sweep, StoreError> {
        let sweep = self.write(|rows| retention::sweep_own(rows, budget, chunk, floor_k))?;
        for agent in &sweep.removed {
            self.markers.remove(agent);
            self.resets.remove(agent);
        }
        Ok(sweep)
    }

    /// Brings replica rows under `budget` bytes: every row of agents with
    /// no open source, least recently used first by `last_used` (the
    /// runtime's clock; where it has no entry, the row's `phase_since_ms`,
    /// which every replica row holds from its origin's inventory, unlike
    /// last activity, written only when a snapshot is absorbed), keeping
    /// their agents rows with an empty block; then sourced agents trimmed
    /// to their newest `floor_k` rows, largest first.
    pub fn sweep_replicas(
        &mut self,
        budget: u64,
        floor_k: u32,
        sourced: &HashSet<AgentKey>,
        last_used: &HashMap<AgentKey, i64>,
    ) -> Result<Sweep, StoreError> {
        let sweep = self
            .write(|rows| retention::sweep_replicas(rows, budget, floor_k, sourced, last_used))?;
        for agent in &sweep.removed {
            self.markers.remove(agent);
            self.resets.remove(agent);
        }
        Ok(sweep)
    }

    pub fn put_delivery(&mut self, delivery: &Delivery) -> Result<(), StoreError> {
        self.write(|rows| rows.put_delivery(delivery))
    }

    pub fn deliveries(&self) -> Result<Vec<Delivery>, StoreError> {
        self.read(|rows| rows.deliveries())
    }

    pub fn remove_delivery(&mut self, delivery: &Delivery) -> Result<(), StoreError> {
        self.write(|rows| rows.remove_delivery(delivery))
    }

    pub fn notifications(&self) -> Result<Vec<Notification>, StoreError> {
        self.read(|rows| rows.notifications())
    }

    pub fn remove_notifications(&mut self, agent_id: &[u8]) -> Result<(), StoreError> {
        self.write(|rows| rows.remove_notifications(agent_id))
    }

    /// Removes one notification: the one a sender just sent.
    pub fn remove_notification(&mut self, notification: &Notification) -> Result<(), StoreError> {
        self.write(|rows| rows.remove_notification(&notification.agent_id, notification.revision))
    }
}

fn commit(
    rows: &Rows<'_>,
    agent: &AgentKey,
    frames: &[(u64, Step)],
    clock: CommitClock,
) -> Result<Committed, StoreError> {
    let mut row = rows.agent(agent)?.ok_or(StoreError::UnknownAgent)?;
    let mut committed = Committed::default();
    let mut next_position = rows.max_position(agent)?.map_or(1, |position| position + 1);
    for (offset, step) in frames {
        for item in &step.items {
            let mut item = item.clone();
            item.agent_id = agent.agent.clone();
            item.revision = row.next_revision;
            row.next_revision += 1;
            item.order = rows.commit_item(agent, &item, next_position)?;
            if item.order == next_position {
                next_position += 1;
            }
            committed.records.push(Record::Item(item));
        }
        for append in &step.appends {
            let revision = row.next_revision;
            let Some(base_revision) =
                rows.extend(agent, &append.key, &append.text, revision, None)?
            else {
                committed.skipped_appends += 1;
                continue;
            };
            row.next_revision += 1;
            committed.records.push(Record::Append(Append {
                agent_id: agent.agent.clone(),
                key: append.key.clone(),
                base_revision,
                revision,
                text: append.text.clone(),
            }));
        }
        if let Some(snapshot) = &step.snapshot {
            let mut snapshot = snapshot.clone();
            snapshot.agent_id = agent.agent.clone();
            snapshot.revision = row.next_revision;
            row.next_revision += 1;
            let was = row.phase;
            copy_envelope(&mut row, &snapshot);
            let needs_you = wire::Phase::NeedsYou as i32;
            if snapshot.phase == needs_you && was != needs_you {
                rows.put_notification(&Notification {
                    agent_id: agent.agent.clone(),
                    revision: snapshot.revision,
                    due_at_ms: clock.now_ms + clock.notify_delay_ms,
                    name: Some(row.name.clone()),
                    working_on: row.working_on.clone(),
                    text: rows.newest_text(agent)?,
                })?;
            } else if snapshot.phase != needs_you && was == needs_you {
                rows.remove_notifications(&agent.agent)?;
            }
            committed.records.push(Record::Snapshot(snapshot));
        }
        if let (Some(turn_end), Some(parent)) = (&step.turn_end, &row.parent) {
            let text = if turn_end.last_message_key.is_empty() {
                String::new()
            } else {
                rows.item_text(agent, &turn_end.last_message_key)?
            };
            // Zero when the parent's row is not held: unknown, see Delivery.
            let parent_incarnation = rows.incarnation(parent)?.unwrap_or(0);
            rows.put_delivery(&Delivery {
                child_id: agent.agent.clone(),
                incarnation: row.incarnation,
                turn_id: turn_end.turn_id,
                parent: parent.clone(),
                parent_incarnation,
                end: wire::child_report::End::TurnEnded(wire::TurnEnded {
                    text,
                    outcome: turn_end.outcome,
                    last: turn_end.last,
                }),
            })?;
        }
        // The last turn's report is the incarnation's last. A step that ends
        // one turn may start the next; the snapshot says where the step
        // leaves the agent.
        if step.turn_end.as_ref().is_some_and(|turn_end| turn_end.last) {
            row.owes_report = false;
        }
        // Why the incarnation ended goes onto the row with the step that
        // says so: a daemon that stops before it marks the agent exited
        // leaves the cause where the next one finds it.
        if let Some(end) = &step.incarnation_end {
            row.exit_cause = end.cause;
            row.exit_detail = end.detail.clone();
        }
        if let Some(snapshot) = &step.snapshot
            && (snapshot.phase == wire::Phase::Working as i32
                || snapshot.phase == wire::Phase::NeedsYou as i32)
        {
            row.owes_report = true;
        }
        row.ingest_cursor = *offset;
    }
    committed.cursor = row.ingest_cursor;
    rows.upsert_agent(&row, &rows::update_of(COMMITTED))?;
    Ok(committed)
}

fn absorb(rows: &Rows<'_>, agent: &AgentKey, what: Absorb) -> Result<Absorbed, StoreError> {
    let mut row = rows.agent(agent)?.ok_or(StoreError::UnknownAgent)?;
    let mut absorbed = Absorbed {
        stored: Vec::new(),
        joined: true,
    };
    // An item as this replica holds it: under its agent's id here.
    let absorb_item = |item: &Item| {
        let mut item = item.clone();
        item.agent_id = agent.agent.clone();
        rows.absorb_item(agent, &item)
    };
    match what {
        Absorb::Delta { events, live } => {
            for event in events {
                let record = match event {
                    SourceEvent::Item(item) => absorb_item(&item)?.then_some(Record::Item(item)),
                    SourceEvent::Append(append) => rows
                        .extend(
                            agent,
                            &append.key,
                            &append.text,
                            append.revision,
                            Some(append.base_revision),
                        )?
                        // Otherwise the stream ends with the full item;
                        // nothing to do.
                        .map(|_| Record::Append(append)),
                    SourceEvent::Snapshot(snapshot) => (snapshot.revision > held_snapshot(&row))
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
                rows.remove_items(agent)?;
                row.source_cursor = 0;
                row.source_generation = generation;
            }
            let floor = match tail.iter().map(|item| item.order).min() {
                Some(order) => order,
                None => rows.max_position(agent)?.map_or(0, |position| position + 1),
            };
            for item in tail {
                if absorb_item(&item)? {
                    absorbed.stored.push(Record::Item(item));
                }
            }
            row.complete_from_order = Some(floor);
            row.exhausted = false;
            // Under a new generation the envelope is the origin's whatever
            // its revision: a snapshot numbered 0 is still the one it holds.
            if fresh || snapshot.revision > held_snapshot(&row) {
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
                if absorb_item(&item)? {
                    absorbed.stored.push(Record::Item(item));
                }
            }
            row.exhausted = exhausted;
        }
        Absorb::CaughtUp(revision) => {
            row.source_cursor = revision;
        }
    }
    rows.upsert_agent(&row, &rows::update_of(ABSORBED))?;
    Ok(absorbed)
}
