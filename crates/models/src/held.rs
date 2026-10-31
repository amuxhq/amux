//! A daemon core and its store as a model's state holds them, and the
//! thread's store a transition steps them over.
//!
//! The state keeps the core as the checkpoint it takes of itself and the
//! store as its dump. A transition restores the core from its checkpoint
//! over a store of this thread's that holds the dump, steps it with the
//! input the driver would feed it, and takes both back: the checkpoint
//! always, the dump only when the step changed the store, since most steps
//! do not and a dump reads every table.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use daemon_core::{AgentId, Daemon, Fingerprint, Input, Output, StartOptions};
use store::{AgentKey, Dump, Marker, Sqlite};

/// One host: its core, as its checkpoint, and its store, as its dump.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Held {
    pub(crate) core: Vec<u8>,
    pub(crate) store: Dump,
}

impl Held {
    /// A host whose core is `daemon`, which hands its store back.
    pub(crate) fn of(daemon: Daemon) -> (Self, Sqlite) {
        let core = daemon.checkpoint();
        let store = daemon.into_store();
        let dump = store.dump().expect("a store dumps");
        (Self { core, store: dump }, store)
    }
}

/// A thread's store for one host, and where its core reads its agents'
/// directories. The store is loaded only when it does not already hold the
/// dump a transition starts from, which saves most loads: a checker takes
/// every transition from one state in turn.
pub(crate) struct Slot {
    store: Option<Sqlite>,
    holds: Dump,
    agents: PathBuf,
}

impl Slot {
    /// A slot for a core that reads no agent's directory.
    pub(crate) fn new(own_host: &[u8]) -> Self {
        Self {
            store: Some(Sqlite::open_in_memory(own_host).expect("a store opens in memory")),
            holds: Dump::default(),
            agents: PathBuf::new(),
        }
    }

    /// A slot whose core reads its agents' directories in `dir`.
    pub(crate) fn beside(own_host: &[u8], dir: &Dir) -> Self {
        Self {
            agents: dir.path().to_path_buf(),
            ..Self::new(own_host)
        }
    }

    fn load(&mut self, dump: &Dump) -> &mut Sqlite {
        let store = self.store.as_mut().expect("the slot's store is home");
        if self.holds != *dump {
            store.load(dump).expect("a store loads");
            self.holds = dump.clone();
        }
        store
    }

    /// Reads the store as `dump` holds it.
    pub(crate) fn read<R>(&mut self, dump: &Dump, f: impl FnOnce(&Sqlite) -> R) -> R {
        f(self.load(dump))
    }

    /// The world writes the host's store outside its core, as the
    /// environment of a model does; the dump is taken again after.
    pub(crate) fn write<R>(&mut self, held: &mut Held, f: impl FnOnce(&mut Sqlite) -> R) -> R {
        let store = self.load(&held.store);
        let result = f(store);
        held.store = store.dump().expect("a store dumps");
        self.holds = held.store.clone();
        result
    }

    /// Replaces the host's core with `daemon`, built over this slot's
    /// store as `held` holds it.
    pub(crate) fn start(&mut self, held: &mut Held, daemon: impl FnOnce(Sqlite) -> Daemon) {
        self.load(&held.store);
        let store = self.store.take().expect("the slot's store is home");
        self.put_back(held, daemon(store), None);
    }

    /// Steps the host's core with one input and returns what it asked for.
    pub(crate) fn step(&mut self, held: &mut Held, input: Input) -> Vec<Output> {
        self.step_reading(held, input, |_| ()).0
    }

    /// Steps the host's core with one input, and reads the core after the
    /// step with `read`.
    pub(crate) fn step_reading<R>(
        &mut self,
        held: &mut Held,
        input: Input,
        read: impl FnOnce(&Daemon) -> R,
    ) -> (Vec<Output>, R) {
        let before = Changes::of(self.load(&held.store));
        let mut daemon = self.restore(held);
        let mut out = Vec::new();
        daemon.step(input, &mut out);
        let read = read(&daemon);
        self.put_back(held, daemon, Some(before));
        (out, read)
    }

    /// Restores the host's core, hands it to `f`, which may step it any
    /// number of times, and takes it back; returns what `f` did and the
    /// core's fingerprint.
    pub(crate) fn with_core<R>(
        &mut self,
        held: &mut Held,
        f: impl FnOnce(&mut Daemon) -> R,
    ) -> (R, Fingerprint) {
        let before = Changes::of(self.load(&held.store));
        let daemon = self.restore(held);
        self.run(held, daemon, f, Some(before))
    }

    /// The core as `held` holds it, over this slot's store as loaded.
    fn restore(&mut self, held: &Held) -> Daemon {
        let store = self.store.take().expect("the slot's store is home");
        Daemon::restore(&held.core, store, self.agents.clone()).expect("a checkpoint restores")
    }

    /// Builds the host's core over this slot's store as `held` holds it,
    /// with this slot's agents' place, then goes on as [`Slot::with_core`]
    /// does.
    pub(crate) fn start_with<R>(
        &mut self,
        held: &mut Held,
        options: StartOptions,
        f: impl FnOnce(&mut Daemon) -> R,
    ) -> (R, Fingerprint) {
        self.load(&held.store);
        let store = self.store.take().expect("the slot's store is home");
        let agents = self.agents.clone();
        let daemon = Daemon::new(store, StartOptions { agents, ..options });
        self.run(held, daemon, f, None)
    }

    fn run<R>(
        &mut self,
        held: &mut Held,
        mut daemon: Daemon,
        f: impl FnOnce(&mut Daemon) -> R,
        before: Option<Changes>,
    ) -> (R, Fingerprint) {
        let result = f(&mut daemon);
        let print = daemon.fingerprint();
        self.put_back(held, daemon, before);
        (result, print)
    }

    /// The store as this slot holds it now, for reading straight after a
    /// step.
    pub(crate) fn store(&self) -> &Sqlite {
        self.store.as_ref().expect("the slot's store is home")
    }

    fn put_back(&mut self, held: &mut Held, daemon: Daemon, before: Option<Changes>) {
        held.core = daemon.checkpoint();
        let store = self.store.insert(daemon.into_store());
        if before.is_none_or(|before| before != Changes::of(store)) {
            held.store = store.dump().expect("a store dumps");
        }
        self.holds = held.store.clone();
    }
}

/// What tells whether a step changed a store: the rows SQLite changed, and
/// the markers the store keeps in memory beside them. A reset count moves
/// only with a write.
#[derive(PartialEq)]
struct Changes {
    rows: u64,
    markers: HashMap<AgentKey, Marker>,
}

impl Changes {
    fn of(store: &Sqlite) -> Self {
        Self {
            rows: store.changes(),
            markers: store.markers().clone(),
        }
    }
}

/// An agent's journal: each segment's start offset and its bytes.
pub type Journal = BTreeMap<u64, Vec<u8>>;

/// The journal's end: the offset one past its last byte.
pub fn journal_end(journal: &Journal) -> u64 {
    journal
        .last_key_value()
        .map_or(0, |(start, bytes)| start + bytes.len() as u64)
}

/// A directory holding one host's agents' journals, laid out only where
/// they differ from what a step or transition starts from.
pub(crate) struct Dir {
    dir: tempfile::TempDir,
    holds: BTreeMap<AgentId, Journal>,
}

impl Dir {
    pub(crate) fn new() -> Self {
        Self {
            dir: tempfile::tempdir().expect("a directory for agents"),
            holds: BTreeMap::new(),
        }
    }

    pub(crate) fn path(&self) -> &Path {
        self.dir.path()
    }

    pub(crate) fn lay_out(&mut self, journals: &BTreeMap<AgentId, Journal>) {
        if self.holds == *journals {
            return;
        }
        let agents: BTreeSet<AgentId> = self.holds.keys().chain(journals.keys()).copied().collect();
        for agent in agents {
            let (held, wanted) = (self.holds.get(&agent), journals.get(&agent));
            if held == wanted {
                continue;
            }
            let dir = daemon_core::ingest::journal_dir(self.dir.path(), agent);
            let _ = std::fs::remove_dir_all(&dir);
            if let Some(journal) = wanted {
                std::fs::create_dir_all(&dir).expect("the journal directory");
                for (start, bytes) in journal {
                    std::fs::write(journal::segment_path(&dir, *start), bytes)
                        .expect("a segment is written");
                }
            }
        }
        self.holds = journals.clone();
    }

    /// Reads back the agent's journal as a step left it.
    pub(crate) fn read_back(&mut self, agent: AgentId) -> Journal {
        let dir = daemon_core::ingest::journal_dir(self.dir.path(), agent);
        let journal: Journal = journal::segments(&dir)
            .expect("the journal lists")
            .into_iter()
            .map(|start| {
                let bytes =
                    std::fs::read(journal::segment_path(&dir, start)).expect("a segment reads");
                (start, bytes)
            })
            .collect();
        self.holds.insert(agent, journal.clone());
        journal
    }

    /// Drops from `journals` the segments a step reclaimed.
    pub(crate) fn reclaimed(&mut self, journals: &mut BTreeMap<AgentId, Journal>) {
        let mut changed = false;
        for (agent, journal) in journals.iter_mut() {
            if journal.len() <= 1 {
                continue;
            }
            let dir = daemon_core::ingest::journal_dir(self.dir.path(), *agent);
            let left: BTreeSet<u64> = journal::segments(&dir)
                .unwrap_or_default()
                .into_iter()
                .collect();
            let before = journal.len();
            journal.retain(|start, _| left.contains(start));
            changed |= journal.len() != before;
        }
        if changed {
            self.holds = journals.clone();
        }
    }
}
