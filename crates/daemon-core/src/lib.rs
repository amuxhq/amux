//! The daemon as one function of its state and an input.
//!
//! [`Daemon::step`] takes one decoded [`Input`] and appends the [`Output`]s
//! it wants done. Nothing inside a step locks, spawns, awaits or reads the
//! world: time and randomness arrive as inputs, sockets and codecs are the
//! driver's, and anything that only produces a result is a [`Job`] whose
//! outcome comes back as an input. Every step ends by checking the cheap
//! invariants, and a violation is reported as [`Output::Dump`] rather than
//! carried on from. See the principles in `docs/ARCHITECTURE.md`.

#![forbid(unsafe_code)]

use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use store::Sqlite;

pub mod checkpoints;
mod codec;
pub mod generation;
pub mod ids;
pub mod ingest;
mod io;
pub mod payload;
pub mod serve;

pub use checkpoints::Checkpoints;
pub use ids::{AccountId, AgentId, CallId, ChannelId, HostId, JobId, LinkId, PairingId, TimerId};
pub use ingest::Journals;
pub use io::{FromShell, IngestEnd, Ingested, Input, Job, Output, Settings, Wake};
pub use serve::Answers;

/// The checkpoint layout this build writes and restores. A checkpoint under
/// any other is refused whole.
pub const CHECKPOINT_SCHEMA: u32 = 1;

/// The daemon's whole state: the store owned outright, one machine per
/// concern, open calls by id, armed timers and jobs awaiting their result.
pub struct Daemon {
    store: Sqlite,
    routing: RoutingState,
    presence: PresenceState,
    sources: Sources,
    families: FamilyState,
    pairing: PairingState,
    answers: Answers,
    journals: Journals,
    checkpoints: Checkpoints,
    timers: Timers,
    pending: Pending,
    settings: Settings,
    options: StartOptions,
    budget: StepBudget,
}

/// What a daemon runs under, fixed for the run.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartOptions {
    /// The installation's generation as of this run: the one this host's
    /// own rows are served under.
    pub generation: u64,
    /// The directory holding each own agent's directory, named by its id.
    pub agents: PathBuf,
}

impl Daemon {
    /// A daemon over `store` with nothing linked, open or armed.
    pub fn new(store: Sqlite, options: StartOptions) -> Self {
        Self {
            store,
            options,
            routing: RoutingState::default(),
            presence: PresenceState::default(),
            sources: Sources::default(),
            families: FamilyState::default(),
            pairing: PairingState::default(),
            answers: Answers::default(),
            journals: Journals::default(),
            checkpoints: Checkpoints::default(),
            timers: Timers::default(),
            pending: Pending::default(),
            settings: Settings::default(),
            budget: StepBudget::DAEMON,
        }
    }

    /// One input in, outputs appended. Ends by checking the cheap
    /// invariants; a violation appends [`Output::Dump`].
    pub fn step(&mut self, input: Input, out: &mut Vec<Output>) {
        match input {
            Input::Tick { now_ms } => self.timers.now_ms = now_ms,
            Input::ClientCall { call, who, request } => self.client_call(call, who, request, out),
            Input::CallCancelled { call } => self.cancelled(call, out),
            Input::AnswerLagged { call } => self.lagged(call, out),
            Input::JobDone { job, result } if self.is_checkpoint(job) => {
                self.checkpoint_done(result, out)
            }
            Input::JobDone { job, result } => self.job_done(job, result, out),
            Input::Nudge { agent } => self.ingest(agent, out),
            Input::AgentHello { agent, .. } => self.hello(agent),
            Input::Timer {
                id: TimerId::Ingest(agent),
            } => self.ingest_again(agent, out),
            Input::Timer {
                id: TimerId::Deliveries,
            } => out.push(Output::Wake(Wake::Deliveries)),
            Input::Timer {
                id: TimerId::Checkpoint,
            } => self.take_checkpoint(out),
            Input::Configure(settings) => self.settings = settings,
            Input::FromShell(change) => self.shell_changed(change, out),
            Input::LinkUp { .. }
            | Input::LinkDown { .. }
            | Input::LinkControl { .. }
            | Input::ChannelOpened { .. }
            | Input::ChannelClosed { .. }
            | Input::PeerCall { .. }
            | Input::PeerAnswer { .. }
            | Input::PairingMessage { .. }
            | Input::Discovered { .. }
            | Input::ControlClosed { .. }
            | Input::Entropy(_) => {}
        }
        if let Err(violation) = self.check_step_invariants() {
            out.push(Output::Dump {
                reason: DumpReason::Violation(violation),
            });
        }
    }

    /// The cheap predicates production checks after every step: each costs
    /// constant time per row the step touched and never scans the block.
    pub fn check_step_invariants(&self) -> Result<(), Violation> {
        Ok(())
    }

    /// The whole-block predicates the checker, the simulator and replay
    /// call after every store write; too slow for production.
    pub fn check_all(&self) -> Result<(), Violation> {
        self.check_step_invariants()
    }

    /// The core as of this step. With the store's main file after a passive
    /// WAL checkpoint taken at the same step, it is the daemon exactly.
    pub fn checkpoint(&self) -> Checkpoint {
        let core = (
            &self.routing,
            &self.presence,
            &self.sources,
            &self.families,
            &self.pairing,
            &self.answers,
            &self.journals,
            &self.checkpoints,
            &self.timers,
            &self.pending,
            &self.settings,
            &self.options,
        );
        Checkpoint {
            schema: CHECKPOINT_SCHEMA,
            state: postcard::to_allocvec(&core).expect("the core's state serializes"),
        }
    }

    /// Rebuilds a core from a checkpoint and the store it was taken beside.
    /// A checkpoint under another schema is refused, never half applied.
    pub fn restore(checkpoint: Checkpoint, store: Sqlite) -> Result<Daemon, RestoreError> {
        if checkpoint.schema != CHECKPOINT_SCHEMA {
            return Err(RestoreError::Schema {
                found: checkpoint.schema,
                expected: CHECKPOINT_SCHEMA,
            });
        }
        let (
            routing,
            presence,
            sources,
            families,
            pairing,
            answers,
            journals,
            checkpoints,
            timers,
            pending,
            settings,
            options,
        ) = postcard::from_bytes(&checkpoint.state)
            .map_err(|error| RestoreError::Corrupt(error.to_string()))?;
        Ok(Daemon {
            routing,
            presence,
            sources,
            families,
            pairing,
            answers,
            journals,
            checkpoints,
            timers,
            pending,
            settings,
            ..Daemon::new(store, options)
        })
    }

    /// What the core remembers about its checkpoints.
    pub fn checkpoints(&self) -> &Checkpoints {
        &self.checkpoints
    }

    /// The store the core owns, for reading.
    pub fn store(&self) -> &Sqlite {
        &self.store
    }

    /// Exchanges the store the core reads and writes with `store`. While
    /// the profile's shell still writes the store under a lock of its own,
    /// its driver lends that store to the core for each step and takes it
    /// back after.
    pub fn swap_store(&mut self, store: &mut Sqlite) {
        std::mem::swap(&mut self.store, store);
    }

    /// How long one step may take before it is reported.
    pub fn budget(&self) -> StepBudget {
        self.budget
    }
}

/// Links, routes and the attempts in flight on them.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutingState {
    links: LinkRegistry,
}

/// Every link that is up, by id, with its peer. The relay keeps one too.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkRegistry {}

/// What each paired host reads as: online, away or offline.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresenceState {}

/// Each replica this profile holds and the stream that feeds it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sources {}

/// Parents, children and the deliveries owed between them.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FamilyState {}

/// The pairing conversations in progress.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingState {}

/// The clock as last told and the timers armed.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Timers {
    now_ms: u64,
}

/// Jobs and calls to peers awaiting their result.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pending {
    /// The last job id minted.
    next_job: u64,
    fetches: std::collections::BTreeMap<JobId, serve::PendingFetch>,
}

/// How long one step may take before it is reported with a dump.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StepBudget {
    pub micros: u64,
}

impl StepBudget {
    /// The daemon's: one step delays every other input for its duration.
    pub const DAEMON: StepBudget = StepBudget { micros: 10_000 };
}

/// A cheap invariant that failed: which one, and what it saw.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Violation {
    /// The invariant, as the sentence it checks.
    pub invariant: &'static str,
    /// The values that broke it.
    pub detail: String,
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.invariant, self.detail)
    }
}

/// Why a core asked for a dump.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum DumpReason {
    /// A cheap invariant failed at the end of a step.
    Violation(Violation),
    /// A step took longer than its budget.
    OverBudget { micros: u64, budget: StepBudget },
}

/// The core's state as of one step, serialized, under the layout it was
/// written with.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Checkpoint {
    pub schema: u32,
    pub state: Vec<u8>,
}

impl Checkpoint {
    /// The bytes a checkpoint job writes: the layout first, so a reader
    /// knows whether it can read the rest before it tries.
    pub fn to_bytes(&self) -> Vec<u8> {
        postcard::to_allocvec(self).expect("a checkpoint serializes")
    }

    /// Reads a checkpoint back. Its state is read only by
    /// [`Daemon::restore`], which refuses another layout.
    pub fn from_bytes(bytes: &[u8]) -> Result<Checkpoint, RestoreError> {
        postcard::from_bytes(bytes).map_err(|error| RestoreError::Corrupt(error.to_string()))
    }
}

/// Why a checkpoint could not be restored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RestoreError {
    /// Written under another layout.
    Schema { found: u32, expected: u32 },
    /// The bytes do not decode under this layout.
    Corrupt(String),
}

impl fmt::Display for RestoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RestoreError::Schema { found, expected } => write!(
                f,
                "the checkpoint was written under layout {found}; this build reads {expected}"
            ),
            RestoreError::Corrupt(why) => write!(f, "the checkpoint does not decode: {why}"),
        }
    }
}

impl std::error::Error for RestoreError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn daemon() -> Daemon {
        Daemon::new(
            Sqlite::open_in_memory(*b"own-host-0000000").expect("a store opens"),
            StartOptions {
                generation: 1,
                ..StartOptions::default()
            },
        )
    }

    #[test]
    fn a_tick_asks_for_nothing() {
        let mut daemon = daemon();
        let mut out = Vec::new();
        daemon.step(Input::Tick { now_ms: 5 }, &mut out);
        assert_eq!(out, Vec::new());
    }

    #[test]
    fn a_checkpoint_round_trips_through_its_bytes_and_restores_the_core() {
        let mut daemon = daemon();
        daemon.step(Input::Tick { now_ms: 5 }, &mut Vec::new());
        let checkpoint = daemon.checkpoint();
        let read = Checkpoint::from_bytes(&checkpoint.to_bytes()).expect("it reads back");
        assert_eq!(read, checkpoint);

        let store = Sqlite::open_in_memory(*b"own-host-0000000").expect("a store opens");
        let restored = Daemon::restore(read, store).expect("it restores");
        assert_eq!(restored.timers, daemon.timers);
        assert_eq!(restored.checkpoint(), checkpoint);
        assert_ne!(checkpoint, self::daemon().checkpoint());
    }

    #[test]
    fn a_checkpoint_that_does_not_decode_is_refused() {
        let checkpoint = Checkpoint {
            schema: CHECKPOINT_SCHEMA,
            state: vec![0xff],
        };
        let store = Sqlite::open_in_memory(*b"own-host-0000000").expect("a store opens");
        assert!(matches!(
            Daemon::restore(checkpoint, store),
            Err(RestoreError::Corrupt(_))
        ));
        assert!(matches!(
            Checkpoint::from_bytes(&[]),
            Err(RestoreError::Corrupt(_))
        ));
    }

    #[test]
    fn a_checkpoint_under_another_layout_is_refused() {
        let checkpoint = Checkpoint {
            schema: CHECKPOINT_SCHEMA + 1,
            state: Vec::new(),
        };
        let store = Sqlite::open_in_memory(*b"own-host-0000000").expect("a store opens");
        let refused = Daemon::restore(checkpoint, store).err();
        assert_eq!(
            refused,
            Some(RestoreError::Schema {
                found: CHECKPOINT_SCHEMA + 1,
                expected: CHECKPOINT_SCHEMA,
            })
        );
    }
}
