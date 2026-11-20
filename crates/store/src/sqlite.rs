//! The profile file: SQLite in WAL mode with synchronous=NORMAL, so a
//! commit costs no fsync and power loss can lose the newest transactions
//! but never corrupt the file.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use prost::Message as _;
use rusqlite::{Connection, TransactionBehavior};

use crate::retention::{self, Sweep};
use crate::rows::{self, ABSORBED, COMMITTED, Rows, SUBAGENT_COMMITTED};
use crate::schema::MIGRATIONS;
use crate::{
    Absorb, Absorbed, AgentKey, AgentRow, Append, CommitClock, Committed, Cut, Delivery, Item,
    Marker, Notification, Page, PageEnd, Record, Snapshot, Step, StoreError, SubagentChange,
    TrustedHost,
};

/// Marks a file as an amux profile store ("amux").
const APPLICATION_ID: i32 = 0x616d_7578;

#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("{0} is not an amux profile store")]
    Foreign(String),
    /// One of the migrations the file lacked did not run. They run in one
    /// transaction, so the file is left exactly as it was.
    #[error("{path}: store migration {migration} failed, and the store is left as it was: {error}")]
    Migration {
        path: String,
        /// The migration's place in the list, counting from 1.
        migration: u32,
        error: rusqlite::Error,
    },
}

/// How far a write must reach before it returns. Commits are otherwise not
/// synced one by one (`synchronous=NORMAL`): they survive the process
/// crashing, and a crash of the machine may lose those since the owner's
/// last checkpoint. A synced write survives that too, at the cost of a
/// sync of the WAL to the drive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Durability {
    Normal,
    Synced,
}

pub struct Sqlite {
    pub(crate) conn: Connection,
    pub(crate) own_host: Vec<u8>,
    pub(crate) markers: HashMap<AgentKey, Marker>,
    /// The `suspension/1` experiment's table is there: each own agent's
    /// run is kept in it.
    pub(crate) suspension_1: bool,
    /// The `reviews/1` experiment's tables are there: each own agent's
    /// review is kept in them.
    pub(crate) reviews_1: bool,
    /// How many resets each replica row has absorbed since the store
    /// opened: what a replica that was told to forget its block went
    /// through, for the checks that judge it.
    #[cfg(feature = "explore")]
    pub(crate) resets: HashMap<AgentKey, u64>,
}

impl std::fmt::Debug for Sqlite {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sqlite")
            .field("path", &self.conn.path())
            .finish_non_exhaustive()
    }
}

/// How long a clean shutdown's flush waits out a checkpoint another
/// connection is taking.
const FLUSH_PATIENCE: std::time::Duration = std::time::Duration::from_secs(2);

impl Sqlite {
    /// Opens or creates the profile store at `path` for the host whose own
    /// rows it holds, running the migrations the file has not had.
    pub fn open(path: &Path, own_host: impl Into<Vec<u8>>) -> Result<Self, OpenError> {
        let conn = Connection::open(path)?;
        Self::with_connection(conn, &path.display().to_string(), own_host.into())
    }

    /// Opens or creates the profile store at `path` as [`Sqlite::open`]
    /// does, for a daemon running the experiment versions in `experiments`
    /// (`workspaces/1`): their tables are made, and every other
    /// experiment's dropped, with its data (see [`crate::experiments`]),
    /// and this host's own rows keep only those versions' experimental
    /// entries. Only the store's owner opens it so; a tool that opens a
    /// copy leaves experiment tables and entries as they are.
    pub fn open_with(
        path: &Path,
        own_host: impl Into<Vec<u8>>,
        experiments: &[&str],
    ) -> Result<Self, OpenError> {
        let mut store = Self::open(path, own_host)?;
        store.settle_experiments(experiments)?;
        Ok(store)
    }

    /// Makes the tables of the experiment versions in `experiments` and
    /// drops every other experiment's, and clears from this host's own
    /// rows every experimental entry of a version not in `experiments`, as
    /// [`Sqlite::open_with`] does; for a store opened otherwise, such as
    /// one in memory. A host speaks only for its own rows: a replica's
    /// entries are its origin's, kept whatever this host runs.
    pub fn settle_experiments(&mut self, experiments: &[&str]) -> Result<(), OpenError> {
        crate::experiments::settle(&mut self.conn, &self.own_host, experiments)?;
        self.suspension_1 = experiments.contains(&crate::experiments::SUSPENSION_1.version);
        self.reviews_1 = experiments.contains(&crate::experiments::REVIEWS_1.version);
        Ok(())
    }

    /// Writes an own row's experimental map whole, as the experiments this
    /// host runs say it; false when there is no such row. A replica's map
    /// is its origin's, written only by [`Sqlite::put_agent`].
    pub fn set_experimental(
        &mut self,
        agent: &AgentKey,
        map: &std::collections::BTreeMap<String, Vec<u8>>,
    ) -> Result<bool, StoreError> {
        if !self.is_own(agent) {
            return Err(StoreError::NotOwn);
        }
        self.write(|rows| {
            let Some(mut row) = rows.agent(agent)? else {
                return Ok(false);
            };
            row.experimental = map.clone();
            rows.upsert_agent(&row, &rows::update_of(rows::EXPERIMENTAL))?;
            Ok(true)
        })
    }

    /// The incarnation the own agent's current run began at, under the
    /// `suspension/1` experiment; none without an entry or the table.
    pub fn run_start(&self, agent: &[u8]) -> Result<Option<u32>, StoreError> {
        if !self.suspension_1 {
            return Ok(None);
        }
        self.read(|rows| rows.run_start(agent))
    }

    /// Records the incarnation the own agent's current run began at, or
    /// forgets it. Does nothing without the experiment's table.
    pub fn put_run_start(
        &mut self,
        agent: &[u8],
        incarnation: Option<u32>,
    ) -> Result<(), StoreError> {
        if !self.suspension_1 {
            return Ok(());
        }
        self.write(|rows| rows.put_run_start(agent, incarnation))
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
        // Every statement the rows run is one of a few dozen fixed texts;
        // kept prepared, a commit skips parsing them again for each frame.
        conn.set_prepared_statement_cache_capacity(64);
        migrate(&mut conn, name, MIGRATIONS)?;
        let tx = conn.transaction()?;
        rows::keep_pool_totals(&tx)?;
        tx.commit()?;
        Ok(Self {
            conn,
            own_host,
            markers: HashMap::new(),
            suspension_1: false,
            reviews_1: false,
            #[cfg(feature = "explore")]
            resets: HashMap::new(),
        })
    }

    /// The underlying connection, for tests that must reach below the
    /// store's own writes.
    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    /// Turns SQLite's own WAL checkpoints off, leaving every one to whoever
    /// owns the store: a commit never moves the WAL into the main file, so
    /// the main file changes only at a checkpoint its owner asked for.
    pub fn leave_checkpoints_to_owner(&self) -> Result<(), rusqlite::Error> {
        self.conn.pragma_update(None, "wal_autocheckpoint", 0)
    }

    /// Moves every committed transaction from the WAL into the database
    /// file and flushes it to the drive itself (fullfsync, which macOS
    /// needs for a flush to pass the drive's cache). Commits are otherwise
    /// not synced, so this is what a clean shutdown promises before it
    /// marks the installation clean. A checkpoint on another connection
    /// (the owner's passive one, running as a job) holds the checkpoint
    /// lock while it copies, so a busy checkpoint is tried again for up to
    /// two seconds.
    pub fn flush_to_drive(&self) -> Result<(), rusqlite::Error> {
        self.conn.pragma_update(None, "fullfsync", true)?;
        let started = std::time::Instant::now();
        let checkpoint = loop {
            let checkpoint = self
                .conn
                .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
                    row.get::<_, i64>(0)
                });
            match checkpoint {
                Ok(busy) if busy != 0 && started.elapsed() < FLUSH_PATIENCE => {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                checkpoint => break checkpoint,
            }
        };
        let restored = self.conn.pragma_update(None, "fullfsync", false);
        match checkpoint? {
            0 => restored,
            _ => Err(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
                Some("a reader held the WAL through the checkpoint".into()),
            )),
        }
    }

    /// Moves every transaction committed to the WAL of the store at `path`
    /// into its main file, on a connection of its own and without waiting
    /// for a writer: the passive checkpoint, which the store's owner runs
    /// off its own thread while the store stays open there. The main file
    /// is synced once the checkpoint completes, so on success it holds
    /// exactly what was committed before the call; with SQLite's own
    /// checkpoints left to the owner, it changes again only at the owner's
    /// next one. One that cannot move the whole WAL, or cannot run at all
    /// because another connection is checkpointing, fails as busy.
    pub fn checkpoint_wal(path: &Path) -> Result<(), rusqlite::Error> {
        let conn = Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        // The journal segments below the checkpoint are deleted once it
        // completes, so its sync must pass the drive's cache too, which on
        // macOS takes a full flush (F_FULLFSYNC) rather than a plain fsync.
        conn.pragma_update(None, "checkpoint_fullfsync", true)?;
        // SQLite answers busy, the WAL's pages and the pages moved; a
        // checkpoint that could not run at all, because another connection
        // holds the checkpoint lock, answers busy with both counts -1.
        let (busy, log, checkpointed) =
            conn.query_row("PRAGMA wal_checkpoint(PASSIVE)", [], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })?;
        if busy != 0 || log < 0 || checkpointed < log {
            return Err(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
                Some(format!(
                    "the checkpoint moved {checkpointed} of the WAL's {log} pages (busy {busy})"
                )),
            ));
        }
        Ok(())
    }

    /// How many rows this store's writes have changed since it opened: a
    /// count that moves whenever a write was made.
    pub fn changes(&self) -> u64 {
        self.conn.total_changes()
    }
}

/// Brings the file up to this build's schema: runs each migration it has
/// not had and stamps it with how many it now holds. A fresh file has had
/// none, so it is given them all. One transaction that takes the write lock
/// first, so two openers do not both run a migration, and a migration that
/// fails leaves the file as it was.
///
/// A file stamped higher than this build knows is one a newer build brought
/// up to date. It is opened as it is: migrations only add, so everything
/// this build reads and writes is still there.
fn migrate(conn: &mut Connection, name: &str, migrations: &[&str]) -> Result<(), OpenError> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let had: u32 = tx.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let application_id: i32 = tx.pragma_query_value(None, "application_id", |row| row.get(0))?;
    if had == 0 && application_id == 0 {
        tx.pragma_update(None, "application_id", APPLICATION_ID)?;
    } else if application_id != APPLICATION_ID {
        return Err(OpenError::Foreign(name.into()));
    }
    let known = migrations.len() as u32;
    if had >= known {
        return Ok(());
    }
    for (index, migration) in migrations.iter().enumerate().skip(had as usize) {
        tx.execute_batch(migration)
            .map_err(|error| OpenError::Migration {
                path: name.into(),
                migration: index as u32 + 1,
                error,
            })?;
    }
    tx.pragma_update(None, "user_version", known)?;
    tx.commit()?;
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
/// What makes one notice a different thing to tell the person. A change
/// within one, such as how many other asks are open, leaves what is owed
/// alone, and its delay with it. Another ask starts again: an answer at the
/// desk then cancels that one too.
#[derive(PartialEq)]
enum Owed {
    Ask(String),
    Blocked,
    Nothing,
}

impl Owed {
    fn of(notice: &wire::Notice) -> Self {
        match &notice.of {
            Some(wire::notice::Of::Ask(ask)) => Self::Ask(ask.key.clone()),
            Some(wire::notice::Of::Blocked(_)) => Self::Blocked,
            Some(wire::notice::Of::Died(_)) | None => Self::Nothing,
        }
    }
}

/// Only the person can answer an ask, wherever in a family it opened. A
/// member blocked or dead is its parent's to hear of and deal with.
fn is_ask(notice: &wire::Notice) -> bool {
    matches!(notice.of, Some(wire::notice::Of::Ask(_)))
}

fn copy_envelope(row: &mut AgentRow, snapshot: &Snapshot) {
    row.phase = snapshot.phase;
    row.working_on = snapshot.working_on.clone();
    // A provider subagent's title is the registry's: the provider's
    // description when its row was made, or none once a person renamed it.
    // Its host's interpreter has no say in it, here or on a replica.
    if row.native.is_none() {
        row.title = snapshot.title.clone();
    }
    row.last_said = snapshot.last_said.clone();
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
            suspension_1: self.suspension_1,
            reviews_1: self.reviews_1,
        });
        tx.finish()?;
        result
    }

    /// Runs `f` in one write transaction; a [`Durability::Synced`] one is
    /// on the drive, past its cache, when this returns.
    fn write_with<R>(
        &mut self,
        durability: Durability,
        f: impl FnOnce(&Rows<'_>) -> Result<R, StoreError>,
    ) -> Result<R, StoreError> {
        match durability {
            Durability::Normal => self.write(f),
            Durability::Synced => {
                // In WAL mode FULL syncs the WAL as the transaction commits;
                // fullfsync makes that sync pass the drive's cache on macOS.
                self.conn.pragma_update(None, "synchronous", "FULL")?;
                self.conn.pragma_update(None, "fullfsync", true)?;
                let result = self.write(f);
                let restored = self
                    .conn
                    .pragma_update(None, "fullfsync", false)
                    .and_then(|()| self.conn.pragma_update(None, "synchronous", "NORMAL"));
                let result = result?;
                restored?;
                Ok(result)
            }
        }
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
            suspension_1: self.suspension_1,
            reviews_1: self.reviews_1,
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
    /// replica those and the phase, working_on, phase_since, git and
    /// experimental map its inventory row carries, the map replaced whole. Committed state is kept, except that an own
    /// row moving to a new incarnation is back in phase starting: the new
    /// process has said nothing yet.
    pub fn put_agent(&mut self, row: &AgentRow) -> Result<(), StoreError> {
        let update = rows::registry_update(self.is_own(&row.agent));
        self.write(|rows| rows.upsert_agent(row, &update))
    }

    /// Writes several rows as [`Sqlite::put_agent`] does, in one
    /// transaction: an agent's process ending and the provider subagents it
    /// ran ending with it land together, or not at all.
    pub fn put_agents(&mut self, agents: &[AgentRow]) -> Result<(), StoreError> {
        let own = rows::registry_update(true);
        let replica = rows::registry_update(false);
        let own_host = self.own_host.clone();
        self.write(|rows| {
            for row in agents {
                let update = if row.agent.host == own_host {
                    &own
                } else {
                    &replica
                };
                rows.upsert_agent(row, update)?;
            }
            Ok(())
        })
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

    /// Writes an own row's phase and when it began, which otherwise only a
    /// committed snapshot sets: the daemon's word for a process that is
    /// gone and left its row live. False when there is no such row.
    pub fn set_phase(
        &mut self,
        agent: &AgentKey,
        phase: wire::Phase,
        since_ms: i64,
    ) -> Result<bool, StoreError> {
        if !self.is_own(agent) {
            return Err(StoreError::NotOwn);
        }
        self.write(|rows| {
            let Some(mut row) = rows.agent(agent)? else {
                return Ok(false);
            };
            row.phase = phase as i32;
            row.phase_since_ms = Some(since_ms);
            rows.upsert_agent(&row, &rows::update_of(&["phase", "phase_since_ms"]))?;
            Ok(true)
        })
    }

    /// Writes an own row's git facts, which otherwise only a committed
    /// snapshot sets: the daemon's word when a diff it made of the folder
    /// shows the agent's last read is out of date. The snapshot the row
    /// holds is left as the agent wrote it. False when there is no such
    /// row.
    pub fn set_git(
        &mut self,
        agent: &AgentKey,
        git: Option<wire::Git>,
    ) -> Result<bool, StoreError> {
        if !self.is_own(agent) {
            return Err(StoreError::NotOwn);
        }
        self.write(|rows| {
            let Some(mut row) = rows.agent(agent)? else {
                return Ok(false);
            };
            row.git = git;
            rows.upsert_agent(&row, &rows::update_of(&["git"]))?;
            Ok(true)
        })
    }

    /// Removes an agent whole: row, items, deliveries and notifications.
    pub fn delete_agent(&mut self, agent: &AgentKey) -> Result<(), StoreError> {
        self.markers.remove(agent);
        #[cfg(feature = "explore")]
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
        #[cfg(feature = "explore")]
        let reset = matches!(what, Absorb::Reset { .. });
        let absorbed = self.write(|rows| absorb(rows, agent, what))?;
        if let Some(marker) = marker {
            self.set_marker(agent, marker);
        }
        #[cfg(feature = "explore")]
        if reset {
            *self.resets.entry(agent.clone()).or_default() += 1;
        }
        Ok(absorbed)
    }

    /// Every paired host.
    pub fn trusted_hosts(&self) -> Result<Vec<TrustedHost>, StoreError> {
        self.read(|rows| rows.trusted_hosts())
    }

    /// Writes `host` over what was kept of it.
    pub fn put_trusted_host(
        &mut self,
        host: &TrustedHost,
        durability: Durability,
    ) -> Result<(), StoreError> {
        if host.host_id == self.own_host {
            return Err(StoreError::OwnHost);
        }
        self.write_with(durability, |rows| rows.put_trusted_host(host))
    }

    /// Forgets the paired host, and with it the generation its inventory
    /// last reported.
    pub fn remove_trusted_host(
        &mut self,
        host: &[u8],
        durability: Durability,
    ) -> Result<(), StoreError> {
        self.write_with(durability, |rows| rows.remove_trusted_host(host))
    }

    /// The agent's draft, encoded, when one is kept.
    pub fn draft(&self, agent_id: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
        self.read(|rows| rows.draft(agent_id))
    }

    /// Keeps the encoded draft for the agent.
    pub fn put_draft(&mut self, agent_id: &[u8], draft: &[u8]) -> Result<(), StoreError> {
        self.write(|rows| rows.put_draft(agent_id, draft))
    }

    /// Forgets the agent's draft; true when one was kept.
    pub fn delete_draft(&mut self, agent_id: &[u8]) -> Result<bool, StoreError> {
        self.write(|rows| rows.delete_draft(agent_id))
    }

    /// Marks `path` used at `at_ms` (a later use wins) and keeps only the
    /// [`RECENT_DIRECTORIES`](crate::RECENT_DIRECTORIES) most recently used.
    pub fn record_recent_directory(&mut self, path: &str, at_ms: i64) -> Result<(), StoreError> {
        self.write(|rows| rows.record_recent_directory(path, at_ms))
    }

    /// The directories agents were started in, most recently used first.
    pub fn recent_directories(&self) -> Result<Vec<(String, i64)>, StoreError> {
        self.read(|rows| rows.recent_directories())
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
            #[cfg(feature = "explore")]
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
            #[cfg(feature = "explore")]
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

    pub fn put_notification(&mut self, notification: &Notification) -> Result<(), StoreError> {
        self.write(|rows| rows.put_notification(notification))
    }

    pub fn remove_notifications(&mut self, agent_id: &[u8]) -> Result<(), StoreError> {
        self.write(|rows| rows.remove_notifications(agent_id))
    }

    /// Removes one notification: the one a sender just sent.
    pub fn remove_notification(&mut self, notification: &Notification) -> Result<(), StoreError> {
        self.write(|rows| rows.remove_notification(&notification.agent_id, notification.revision))
    }
}

/// One agent's row while a commit writes it: the host's, or a provider
/// subagent's its parts are filed under.
struct Writing {
    row: AgentRow,
    next_position: u64,
    records: Vec<Record>,
}

impl Writing {
    fn open(rows: &Rows<'_>, row: AgentRow) -> Result<Self, StoreError> {
        // Retention empties an ended provider subagent's block and keeps
        // its row, the block starting above what it held: a continuation
        // files from there, not from order 1 under what a replica holds.
        let next_position = rows
            .max_position(&row.agent)?
            .map_or(1, |position| position + 1)
            .max(row.complete_from_order.unwrap_or(1));
        Ok(Self {
            row,
            next_position,
            records: Vec::new(),
        })
    }

    fn live(&self) -> bool {
        self.row.lifecycle == wire::Lifecycle::Live as i32
    }

    /// Files one agent's items, appends and snapshot: each takes the
    /// agent's next revision, and each new key its next order.
    fn file(
        &mut self,
        rows: &Rows<'_>,
        items: &[Item],
        appends: &[wire::Append],
        snapshot: Option<&Snapshot>,
        clock: CommitClock,
    ) -> Result<(), StoreError> {
        let agent = self.row.agent.clone();
        for item in items {
            let mut item = item.clone();
            item.revision = self.row.next_revision;
            self.row.next_revision += 1;
            item.order = rows.commit_item(&agent, &item, self.next_position)?;
            if item.order == self.next_position {
                self.next_position += 1;
            }
            self.records.push(Record::Item(item));
        }
        for append in appends {
            let revision = self.row.next_revision;
            let Some(base_revision) =
                rows.extend(&agent, &append.key, &append.text, revision, None)?
            else {
                // The item it extends was never committed: nothing to extend.
                continue;
            };
            self.row.next_revision += 1;
            self.records.push(Record::Append(Append {
                key: append.key.clone(),
                base_revision,
                revision,
                text: append.text.clone(),
            }));
        }
        if let Some(snapshot) = snapshot {
            let row = &mut self.row;
            let mut snapshot = snapshot.clone();
            snapshot.revision = row.next_revision;
            row.next_revision += 1;
            let was = row
                .snapshot
                .as_ref()
                .and_then(|last| last.notice.as_deref())
                .map(Owed::of);
            copy_envelope(row, &snapshot);
            let now = snapshot.notice.as_deref().map(Owed::of);
            if now != was {
                rows.remove_notifications(&agent.agent)?;
                let notice = snapshot
                    .notice
                    .as_ref()
                    .filter(|notice| row.parent.is_none() || is_ask(notice));
                if let Some(notice) = notice {
                    rows.put_notification(&Notification {
                        agent_id: agent.agent.clone(),
                        revision: snapshot.revision,
                        due_at_ms: clock.now_ms + clock.notify_delay_ms,
                        // The person reads a push by what the agent is
                        // about; its name is only an address.
                        name: row.title.clone().unwrap_or_else(|| row.name.clone()),
                        notice: notice.encode_to_vec(),
                    })?;
                }
            }
            self.records.push(Record::Snapshot(snapshot));
        }
        Ok(())
    }
}

/// The row a subagent's part names, when it is one of `host`'s own
/// provider subagents: a part naming anything else is not the host's to
/// write, and is skipped.
fn subagent_row(
    rows: &Rows<'_>,
    host: &AgentKey,
    agent_id: &[u8],
) -> Result<Option<AgentRow>, StoreError> {
    let key = AgentKey::new(host.host.clone(), agent_id.to_vec());
    Ok(rows
        .agent(&key)?
        .filter(|row| row.native.is_some() && row.parent.as_ref() == Some(host)))
}

fn commit(
    rows: &Rows<'_>,
    agent: &AgentKey,
    frames: &[(u64, Step)],
    clock: CommitClock,
) -> Result<Committed, StoreError> {
    let host = rows.agent(agent)?.ok_or(StoreError::UnknownAgent)?;
    let mut host = Writing::open(rows, host)?;
    let mut committed = Committed::default();
    // Each subagent's row, opened at its first part, in the order the
    // parts first named them.
    let mut subagents: Vec<Writing> = Vec::new();
    for (offset, step) in frames {
        host.file(
            rows,
            &step.items,
            &step.appends,
            step.snapshot.as_ref(),
            clock,
        )?;
        let row = &mut host.row;
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
        for part in &step.agents {
            let at = match subagents
                .iter()
                .position(|sub| sub.row.agent.agent == part.agent_id)
            {
                Some(at) => at,
                None => match subagent_row(rows, agent, &part.agent_id)? {
                    Some(row) => {
                        subagents.push(Writing::open(rows, row)?);
                        subagents.len() - 1
                    }
                    None => {
                        committed.skipped.push(part.agent_id.clone());
                        continue;
                    }
                },
            };
            let sub = &mut subagents[at];
            // A subagent has no process of its own: its starts and ends are
            // its lifecycle, written in this transaction with the host's
            // cursor, so a daemon stopping after the commit cannot leave a
            // row the next one will not read the step that moved it again.
            //
            // A start on a row that has ended is its parent's model
            // continuing it: live and open again, the cause of its last end
            // no longer holding.
            if part.start.is_some() && !sub.live() {
                sub.row.lifecycle = wire::Lifecycle::Live as i32;
                sub.row.exit_cause = wire::ExitCause::Unspecified as i32;
                sub.row.exit_detail = None;
                if sub.row.is_closed() {
                    sub.row.filing = wire::Filing::Open as i32;
                    sub.row.filed_at_ms = clock.now_ms;
                }
                committed
                    .changes
                    .push(SubagentChange::Continued(sub.row.agent.clone()));
            }
            // A part that carries its own end is the provider closing the
            // subagent's steps, as a resumed host does of one its killed
            // process left, whose row that process's exit already ended.
            if !sub.live()
                && part.end.is_none()
                && !(part.items.is_empty() && part.appends.is_empty())
            {
                committed.after_end.push(sub.row.agent.clone());
            }
            sub.file(
                rows,
                &part.items,
                &part.appends,
                part.snapshot.as_ref(),
                clock,
            )?;
            // A subagent's answer goes back through its parent's provider,
            // so it owes nobody a report, whatever its phase.
            sub.row.owes_report = false;
            // An end files one that did its work closed, as its parent
            // closing it would. An end for a row already ended is the
            // provider saying so again, as a resumed Claude does of a
            // subagent the previous process left: nothing changes.
            if let Some(end) = &part.end
                && sub.live()
            {
                sub.row.lifecycle = wire::Lifecycle::Exited as i32;
                sub.row.exit_cause = end.cause;
                sub.row.exit_detail = end.detail.clone();
                if end.cause == wire::ExitCause::Finished as i32 && !sub.row.is_closed() {
                    sub.row.filing = wire::Filing::Closed as i32;
                    sub.row.filed_at_ms = clock.now_ms;
                }
                committed
                    .changes
                    .push(SubagentChange::Ended(sub.row.agent.clone(), end.clone()));
            }
        }
    }
    committed.cursor = host.row.ingest_cursor;
    rows.upsert_agent(&host.row, &rows::update_of(COMMITTED))?;
    committed.records = host.records;
    for sub in subagents {
        rows.upsert_agent(&sub.row, &rows::update_of(SUBAGENT_COMMITTED))?;
        committed.subagents.push((sub.row.agent, sub.records));
    }
    Ok(committed)
}

fn absorb(rows: &Rows<'_>, agent: &AgentKey, what: Absorb) -> Result<Absorbed, StoreError> {
    let mut row = rows.agent(agent)?.ok_or(StoreError::UnknownAgent)?;
    let mut absorbed = Absorbed {
        stored: Vec::new(),
        joined: true,
    };
    let absorb_item = |item: &Item| rows.absorb_item(agent, item);
    match what {
        Absorb::Delta { events, live } => {
            for event in events {
                let record = match event {
                    Record::Item(item) => absorb_item(&item)?.then_some(Record::Item(item)),
                    Record::Append(append) => rows
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
                    Record::Snapshot(snapshot) => {
                        (snapshot.revision > held_snapshot(&row)).then(|| {
                            copy_envelope(&mut row, &snapshot);
                            Record::Snapshot(snapshot)
                        })
                    }
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

#[cfg(test)]
mod tests {
    use rusqlite::params;

    use super::*;

    const FIRST: &str = "CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT NOT NULL) STRICT;
                         INSERT INTO notes (body) VALUES ('made by the first');";
    const SECOND: &str = "ALTER TABLE notes ADD COLUMN pinned INTEGER NOT NULL DEFAULT 0;
                          CREATE TABLE tags (name TEXT PRIMARY KEY) STRICT;
                          UPDATE notes SET pinned = 1;";

    fn stamp(conn: &Connection) -> u32 {
        conn.pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap()
    }

    fn notes(conn: &Connection) -> i64 {
        conn.query_row("SELECT count(*) FROM notes", [], |row| row.get(0))
            .unwrap()
    }

    /// The case every schema change after a release is: a store an earlier
    /// build made is opened by a build that knows one migration more.
    #[test]
    fn a_store_an_earlier_build_made_is_given_the_migrations_it_lacks() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&mut conn, "store", &[FIRST]).unwrap();
        assert_eq!(stamp(&conn), 1);
        conn.execute("INSERT INTO notes (body) VALUES ('the person''s')", [])
            .unwrap();

        migrate(&mut conn, "store", &[FIRST, SECOND]).unwrap();

        assert_eq!(stamp(&conn), 2);
        assert_eq!(notes(&conn), 2, "the first migration did not run again");
        let pinned: i64 = conn
            .query_row("SELECT sum(pinned) FROM notes", [], |row| row.get(0))
            .unwrap();
        assert_eq!(pinned, 2, "the migration filled the column it added");
        conn.execute("INSERT INTO tags (name) VALUES ('new')", [])
            .unwrap();
    }

    #[test]
    fn a_fresh_store_is_given_every_migration() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&mut conn, "store", &[FIRST, SECOND]).unwrap();
        assert_eq!(stamp(&conn), 2);
        assert_eq!(notes(&conn), 1);
        conn.execute("INSERT INTO tags (name) VALUES ('new')", [])
            .unwrap();
    }

    #[test]
    fn a_store_that_is_up_to_date_is_left_alone() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&mut conn, "store", &[FIRST, SECOND]).unwrap();
        migrate(&mut conn, "store", &[FIRST, SECOND]).unwrap();
        assert_eq!(stamp(&conn), 2);
        assert_eq!(notes(&conn), 1);
    }

    /// A rollback: the previous release opens a store the release after it
    /// migrated. It runs nothing and leaves the stamp, so the newer build
    /// does not run its migration a second time when it returns.
    #[test]
    fn a_store_a_later_build_migrated_is_opened_as_it_is() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&mut conn, "store", &[FIRST, SECOND]).unwrap();

        migrate(&mut conn, "store", &[FIRST]).unwrap();
        assert_eq!(stamp(&conn), 2, "the stamp is the newer build's");
        // What the earlier build writes: it names its own columns.
        conn.execute("INSERT INTO notes (body) VALUES ('after the rollback')", [])
            .unwrap();

        migrate(&mut conn, "store", &[FIRST, SECOND]).unwrap();
        assert_eq!(notes(&conn), 2);
        let unpinned: i64 = conn
            .query_row("SELECT count(*) FROM notes WHERE pinned = 0", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(
            unpinned, 1,
            "a row written during the rollback holds the column's default"
        );
    }

    #[test]
    fn a_migration_that_fails_is_named_and_leaves_the_store_as_it_was() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&mut conn, "store", &[FIRST]).unwrap();
        let broken = "CREATE TABLE tags (name TEXT PRIMARY KEY) STRICT;
                      ALTER TABLE missing ADD COLUMN x TEXT;";

        let error = migrate(&mut conn, "the/store", &[FIRST, SECOND, broken]).unwrap_err();

        assert!(
            matches!(error, OpenError::Migration { migration: 3, .. }),
            "{error:?}"
        );
        assert!(error.to_string().contains("the/store"), "{error}");
        assert_eq!(stamp(&conn), 1, "the second migration was undone with it");
        let tables: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name = 'tags'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(tables, 0);
    }

    /// A row the first release's schema held, as that release wrote it,
    /// reads as an agent amux started: the column marking a provider
    /// subagent is NULL on it. The indexes the subagents' rows need are
    /// made beside it.
    #[test]
    fn a_row_from_before_provider_subagents_reads_as_no_subagent() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&mut conn, "store", &MIGRATIONS[..1]).unwrap();
        conn.execute(
            "INSERT INTO agents (origin_host, agent_id, kind, name, cwd, lifecycle, phase, \
             created_at_ms, incarnation) VALUES (?1, ?2, 2, 'old', '/', 1, 1, 0, 1)",
            params![b"own".as_slice(), b"agent".as_slice()],
        )
        .unwrap();

        let store = Sqlite::with_connection(conn, "store", b"own".to_vec()).unwrap();

        let key = AgentKey::new(b"own".to_vec(), b"agent".to_vec());
        let row = store.agent(&key).unwrap().expect("the old row is held");
        assert_eq!(row.name, "old");
        assert_eq!(row.native, None);
        let indexes: Vec<String> = store
            .connection()
            .prepare(
                "SELECT name FROM sqlite_master WHERE type = 'index' AND tbl_name = 'agents' \
                 AND sql IS NOT NULL ORDER BY name",
            )
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            indexes,
            ["agents_by_id", "agents_by_name", "agents_by_parent"]
        );
    }

    const OWN_HOST: &[u8] = b"own-host";
    const PEER_HOST: &[u8] = b"peer-host";

    /// Each pool's running total, which must be what a full count says.
    fn pools(store: &Sqlite) -> (u64, u64) {
        let held = |own| store.read(|rows| rows.pool_bytes(own)).unwrap();
        let counted = |own| store.read(|rows| rows.pool_bytes_counted(own)).unwrap();
        assert_eq!(
            held(true),
            counted(true),
            "the own pool's total is its count"
        );
        assert_eq!(
            held(false),
            counted(false),
            "the replica pool's total is its count"
        );
        (held(true), held(false))
    }

    fn item(key: &str, text: usize, body: usize) -> Item {
        Item {
            key: key.into(),
            text: "x".repeat(text),
            body: vec![7; body],
            kind: wire::Kind::Codex as i32,
            ..Item::default()
        }
    }

    fn step(items: Vec<Item>, appends: Vec<crate::Append>) -> Step {
        Step {
            items,
            appends,
            ..Step::default()
        }
    }

    const CLOCK: CommitClock = CommitClock {
        now_ms: 0,
        notify_delay_ms: 0,
    };

    /// The pools' totals follow every kind of write to `items`, own and
    /// replica, a write that rolls back included, and a store opened again
    /// counts them afresh.
    #[test]
    fn the_pools_totals_follow_every_write_and_are_counted_again_at_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("store.sqlite");
        let mut store = Sqlite::open(&path, OWN_HOST).unwrap();
        assert_eq!(pools(&store), (0, 0), "a fresh store holds nothing");

        let a = AgentKey::new(OWN_HOST, b"a".as_slice());
        let b = AgentKey::new(OWN_HOST, b"b".as_slice());
        for agent in [&a, &b] {
            store
                .put_agent(&AgentRow::new(agent.clone(), wire::Kind::Codex, "/src"))
                .unwrap();
        }
        // New keys: the upsert inserts.
        let fresh = (0..6)
            .map(|n| item(&format!("{n:02}"), 100 * (n + 1), 50))
            .collect();
        store
            .commit(&a, &[(1, step(fresh, vec![]))], CLOCK)
            .unwrap();
        let fresh = (0..30).map(|n| item(&format!("{n:02}"), 500, 10)).collect();
        store
            .commit(&b, &[(1, step(fresh, vec![]))], CLOCK)
            .unwrap();
        let (own, replica) = pools(&store);
        assert!(own > 0);
        assert_eq!(replica, 0);

        // Held keys rewritten larger and smaller, with an input id and
        // attachments: the upsert updates. Then text appended to one.
        let mut larger = item("00", 4000, 900);
        larger.input_id = b"input".to_vec();
        larger.attachments = vec![wire::Attachment::default()];
        let rewritten = vec![larger, item("01", 1, 0)];
        store
            .commit(&a, &[(2, step(rewritten, vec![]))], CLOCK)
            .unwrap();
        let grown = pools(&store).0;
        assert_ne!(grown, own, "the rewrites moved the total");
        let append = crate::Append {
            key: "02".into(),
            text: "y".repeat(777),
            ..crate::Append::default()
        };
        store
            .commit(&a, &[(3, step(vec![], vec![append]))], CLOCK)
            .unwrap();
        assert_eq!(pools(&store).0, grown + 777, "the append's bytes count");

        // Replica rows: a reset's tail, then live records, one too old to
        // be written, one newer and smaller.
        let r = AgentKey::new(PEER_HOST, b"r".as_slice());
        store
            .put_agent(&AgentRow::new(r.clone(), wire::Kind::Codex, "/src"))
            .unwrap();
        let tail = (1..=8)
            .map(|order| Item {
                order,
                revision: order,
                ..item(&format!("{order:02}"), 300, 20)
            })
            .collect();
        let snapshot = Snapshot {
            revision: 9,
            ..Snapshot::default()
        };
        store
            .absorb(
                &r,
                crate::Absorb::Reset {
                    tail,
                    snapshot,
                    generation: 1,
                },
            )
            .unwrap();
        let (_, replica) = pools(&store);
        assert!(replica > 0);
        let events = vec![
            crate::Record::Item(Item {
                order: 1,
                revision: 1,
                ..item("01", 9000, 0)
            }),
            crate::Record::Item(Item {
                order: 2,
                revision: 10,
                ..item("02", 5, 0)
            }),
            crate::Record::Item(Item {
                order: 9,
                revision: 11,
                ..item("09", 40, 40)
            }),
        ];
        store
            .absorb(&r, crate::Absorb::Delta { events, live: true })
            .unwrap();
        let (_, absorbed) = pools(&store);
        assert_ne!(absorbed, replica);

        // A commit that fails after it rewrote a held row: the whole
        // transaction rolls back, and the total with it.
        let before = pools(&store);
        store
            .connection()
            .execute_batch(
                "CREATE TEMP TRIGGER refuse BEFORE INSERT ON items WHEN NEW.key = 'refused' \
                 BEGIN SELECT RAISE(ABORT, 'refused'); END;",
            )
            .unwrap();
        let failing = step(vec![item("03", 6000, 600), item("refused", 1, 1)], vec![]);
        assert!(store.commit(&a, &[(4, failing)], CLOCK).is_err());
        store
            .connection()
            .execute_batch("DROP TRIGGER temp.refuse")
            .unwrap();
        assert_eq!(
            pools(&store),
            before,
            "the failed commit left no bytes behind"
        );

        // Trimming deletes: own rows past their budget, the largest agent
        // trimmed a chunk at a time, and replica rows trimmed to their tail.
        let sweep = store.sweep_own(own / 2, 2000, 4).unwrap();
        assert!(
            sweep
                .steps
                .iter()
                .any(|step| matches!(step, crate::SweepStep::Trimmed { .. })),
            "{sweep:?}"
        );
        assert_eq!(pools(&store).0, sweep.pool_after);
        let sourced = HashSet::from([r.clone()]);
        let sweep = store
            .sweep_replicas(1, 2, &sourced, &HashMap::new())
            .unwrap();
        assert!(!sweep.steps.is_empty(), "{sweep:?}");
        assert_eq!(pools(&store).1, sweep.pool_after);
        // And an unsourced replica evicted whole.
        let sweep = store
            .sweep_replicas(0, 2, &HashSet::new(), &HashMap::new())
            .unwrap();
        assert_eq!(sweep.removed, vec![r.clone()]);
        assert_eq!(pools(&store).1, 0);

        // An agent deleted with its rows.
        store.delete_agent(&b).unwrap();
        let (own, _) = pools(&store);
        assert!(own > 0, "a's rows are still counted");

        // A store opened again counts its totals afresh, by the host it is
        // opened for.
        let more = (1..=3)
            .map(|order| Item {
                order,
                revision: order,
                ..item(&format!("{order:02}"), 70, 7)
            })
            .collect();
        store
            .absorb(
                &r,
                crate::Absorb::Reset {
                    tail: more,
                    snapshot: Snapshot {
                        revision: 20,
                        ..Snapshot::default()
                    },
                    generation: 1,
                },
            )
            .unwrap();
        let held = pools(&store);
        drop(store);
        let store = Sqlite::open(&path, OWN_HOST).unwrap();
        assert_eq!(pools(&store), held);
        drop(store);
        let store = Sqlite::open(&path, Vec::new()).unwrap();
        assert_eq!(
            pools(&store),
            (0, held.0 + held.1),
            "every row is a replica's"
        );
    }

    /// An explored store that loads a dump takes the dump's host as its
    /// own, and its pools split by that host.
    #[cfg(feature = "explore")]
    #[test]
    fn a_loaded_dump_splits_the_pools_by_its_host() {
        let mut origin = Sqlite::open_in_memory(OWN_HOST).unwrap();
        let a = AgentKey::new(OWN_HOST, b"a".as_slice());
        origin
            .put_agent(&AgentRow::new(a.clone(), wire::Kind::Codex, "/src"))
            .unwrap();
        let items = (0..4).map(|n| item(&format!("{n:02}"), 300, 30)).collect();
        origin
            .commit(&a, &[(1, step(items, vec![]))], CLOCK)
            .unwrap();
        let held = pools(&origin);
        assert!(held.0 > 0);

        let mut other = Sqlite::open_in_memory(PEER_HOST).unwrap();
        let p = AgentKey::new(PEER_HOST, b"p".as_slice());
        other
            .put_agent(&AgentRow::new(p.clone(), wire::Kind::Codex, "/src"))
            .unwrap();
        let items = (0..9).map(|n| item(&format!("{n:02}"), 50, 5)).collect();
        other
            .commit(&p, &[(1, step(items, vec![]))], CLOCK)
            .unwrap();
        assert!(pools(&other).0 > 0);

        other.load(&origin.dump().unwrap()).unwrap();
        assert_eq!(pools(&other), held);
    }
}
