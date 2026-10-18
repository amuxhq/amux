//! A daemon core and its store as a model's state holds them, and the
//! thread's store a transition steps them over.
//!
//! The state keeps the core as the checkpoint it takes of itself and the
//! store as its dump. A transition restores the core from its checkpoint
//! over a store of this thread's that holds the dump, steps it with the
//! input the driver would feed it, and takes both back: the checkpoint
//! always, the dump only when the step changed the store, since most steps
//! do not and a dump reads every table.

use std::collections::HashMap;

use daemon_core::{Checkpoint, Daemon, Input, Output};
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
        let core = daemon.checkpoint().to_bytes();
        let store = daemon.into_store();
        let dump = store.dump().expect("a store dumps");
        (Self { core, store: dump }, store)
    }
}

/// A thread's store for one host. It is loaded only when it does not
/// already hold the dump a transition starts from, which saves most loads:
/// a checker takes every transition from one state in turn.
pub(crate) struct Slot {
    store: Option<Sqlite>,
    holds: Dump,
}

impl Slot {
    pub(crate) fn new(own_host: &[u8]) -> Self {
        Self {
            store: Some(Sqlite::open_in_memory(own_host).expect("a store opens in memory")),
            holds: Dump::default(),
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
        let store = self.store.take().expect("the slot's store is home");
        let checkpoint = Checkpoint::from_bytes(&held.core).expect("a checkpoint reads");
        let mut daemon = Daemon::restore(checkpoint, store).expect("a checkpoint restores");
        let mut out = Vec::new();
        daemon.step(input, &mut out);
        let read = read(&daemon);
        self.put_back(held, daemon, Some(before));
        (out, read)
    }

    fn put_back(&mut self, held: &mut Held, daemon: Daemon, before: Option<Changes>) {
        held.core = daemon.checkpoint().to_bytes();
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
            rows: store.connection().total_changes(),
            markers: store.markers().clone(),
        }
    }
}
