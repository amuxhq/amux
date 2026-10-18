//! The agent process around its interpreter, as one function of its state
//! and an input.
//!
//! [`Agent::step`] takes one decoded [`Input`] (a provider fact, the
//! daemon's arrival or departure, a person's input, a timer) and appends
//! the [`Output`]s it wants done. Journal appends are outputs the driver
//! writes inline; fsync, provider writes and socket writes are the
//! driver's. Every step ends by checking the cheap invariants, and a
//! violation is reported as [`Output::Dump`]. See the principles in
//! `docs/ARCHITECTURE.md`.

#![forbid(unsafe_code)]

use std::fmt;
use std::path::PathBuf;

use interpret::Interpreter;
use wire::{CtlFrame, Envelope, ExitCause, Step};

/// A provider fact exactly as it arrived.
pub type ProviderEvent = interpret::Fact;

/// The checkpoint layout this build writes and restores. A checkpoint under
/// any other is refused whole.
pub const CHECKPOINT_SCHEMA: u32 = 1;

/// A timer the core asked the driver to arm.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TimerId(pub u64);

/// A job the driver runs; its outcome returns as [`Input::JobDone`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct JobId(pub u64);

/// One connection from a daemon to the agent's control socket.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ConnId(pub u64);

/// A person's input, by the id its client gave it.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct InputId(pub Vec<u8>);

/// Bytes for the provider child: a line on its stdin or keys on its
/// terminal.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct ProviderWrite(pub Vec<u8>);

/// What a job produced.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct JobResult;

/// Everything that can happen to the agent process, decoded by the driver.
#[derive(Clone, Debug, PartialEq)]
pub enum Input {
    Provider(ProviderEvent),
    /// A daemon connected to the control socket.
    DaemonDials {
        connection: ConnId,
    },
    /// The daemon's connection closed.
    DaemonGone,
    Control {
        connection: ConnId,
        frame: CtlFrame,
    },
    Input {
        id: InputId,
        event: wire::Input,
    },
    AgentMessage(Envelope),
    Timer(TimerId),
    ProviderExited {
        code: Option<i32>,
    },
    /// The clock, as an input.
    Tick {
        now_ms: u64,
    },
    JobDone {
        job: JobId,
        result: JobResult,
    },
}

/// Everything the agent process wants done.
#[derive(Clone, Debug, PartialEq)]
pub enum Output {
    /// Append the step to the journal, inline.
    AppendJournal(Step),
    Job {
        id: JobId,
        op: AgentJob,
    },
    WriteProvider(ProviderWrite),
    WriteControl {
        connection: ConnId,
        frame: CtlFrame,
    },
    ArmTimer {
        id: TimerId,
        at_ms: u64,
    },
    CancelTimer {
        id: TimerId,
    },
    KillProvider,
    /// The process exits; the cause is the journal's last step.
    Exit(ExitCause),
    /// Take a dump: the core saw something it must not carry on from.
    Dump {
        reason: DumpReason,
    },
}

/// A result-only mechanism the driver runs off the core thread.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum AgentJob {
    Fsync(PathBuf),
    /// Write the serialized core as the facts ring's new checkpoint.
    Checkpoint {
        state: Vec<u8>,
    },
}

/// The incarnation, the grace and drain timers, the queue, the pending set
/// and the open ask around one interpreter.
#[expect(
    dead_code,
    reason = "each part is read once the inputs that move it are"
)]
pub struct Agent<I: Interpreter> {
    interpreter: I::State,
    incarnation: u32,
    connection: Option<ConnId>,
    grace: Option<TimerId>,
    drain: Option<TimerId>,
    queue: Queue,
    pending: Pending,
    ask: Option<OpenAsk>,
    turn: Turn,
    ring: InputRing,
    budget: StepBudget,
}

/// Inputs accepted and waiting for the provider.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Queue {}

/// Agent messages handed to the provider and not yet consumed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Pending {}

/// The question the provider is waiting on a person to answer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OpenAsk {}

/// Whether a turn is running, and how far it has got.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Turn {}

/// The inputs since the last checkpoint, kept so a dump can replay them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InputRing {}

impl<I: Interpreter> Agent<I> {
    /// An agent in `incarnation` around an interpreter in `interpreter`,
    /// with no daemon connected and nothing queued.
    pub fn new(interpreter: I::State, incarnation: u32) -> Self {
        Self {
            interpreter,
            incarnation,
            connection: None,
            grace: None,
            drain: None,
            queue: Queue::default(),
            pending: Pending::default(),
            ask: None,
            turn: Turn::default(),
            ring: InputRing::default(),
            budget: StepBudget::AGENT,
        }
    }

    /// One input in, outputs appended. Ends by checking the cheap
    /// invariants; a violation appends [`Output::Dump`].
    pub fn step(&mut self, input: Input, out: &mut Vec<Output>) {
        match input {
            Input::Provider(_)
            | Input::DaemonDials { .. }
            | Input::DaemonGone
            | Input::Control { .. }
            | Input::Input { .. }
            | Input::AgentMessage(_)
            | Input::Timer(_)
            | Input::ProviderExited { .. }
            | Input::Tick { .. }
            | Input::JobDone { .. } => {}
        }
        if let Err(violation) = self.check_step_invariants() {
            out.push(Output::Dump {
                reason: DumpReason::Violation(violation),
            });
        }
    }

    /// The cheap predicates checked after every step, each in constant time.
    pub fn check_step_invariants(&self) -> Result<(), Violation> {
        Ok(())
    }

    /// The core as of this step, serialized.
    pub fn checkpoint(&self) -> Checkpoint {
        Checkpoint {
            schema: CHECKPOINT_SCHEMA,
            state: interpret::encode_checkpoint(&self.interpreter),
        }
    }

    /// Rebuilds a core from a checkpoint in `incarnation`. A checkpoint
    /// under another layout, or one that does not decode, is refused.
    pub fn restore(checkpoint: Checkpoint, incarnation: u32) -> Result<Self, RestoreError> {
        if checkpoint.schema != CHECKPOINT_SCHEMA {
            return Err(RestoreError::Schema {
                found: checkpoint.schema,
                expected: CHECKPOINT_SCHEMA,
            });
        }
        let interpreter = interpret::decode_checkpoint(&checkpoint.state)
            .map_err(|error| RestoreError::Corrupt(error.to_string()))?;
        Ok(Self::new(interpreter, incarnation))
    }
}

/// How long one step may take before it is reported with a dump.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StepBudget {
    pub micros: u64,
}

impl StepBudget {
    /// The agent process's: a step stands between the provider and its
    /// journal.
    pub const AGENT: StepBudget = StepBudget { micros: 10_000 };
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

/// Why the core asked for a dump.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum DumpReason {
    /// A cheap invariant failed at the end of a step.
    Violation(Violation),
    /// A step took longer than its budget.
    OverBudget { micros: u64, budget: StepBudget },
}

/// The core's state as of one step, serialized, under the layout it was
/// written with.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Checkpoint {
    pub schema: u32,
    pub state: Vec<u8>,
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
    use interpret::claude_sdk::ClaudeSdk;

    use super::*;

    fn agent() -> Agent<ClaudeSdk> {
        let (state, _) = ClaudeSdk::initial(&wire::AgentSpec::default(), "0.0.0");
        Agent::new(state, 1)
    }

    #[test]
    fn a_tick_asks_for_nothing() {
        let mut agent = agent();
        let mut out = Vec::new();
        agent.step(Input::Tick { now_ms: 5 }, &mut out);
        assert_eq!(out, Vec::new());
    }

    #[test]
    fn a_checkpoint_restores_to_the_same_checkpoint() {
        let checkpoint = agent().checkpoint();
        let restored = Agent::<ClaudeSdk>::restore(checkpoint.clone(), 1).expect("it restores");
        assert_eq!(restored.checkpoint(), checkpoint);
    }

    #[test]
    fn a_checkpoint_under_another_layout_is_refused() {
        let mut checkpoint = agent().checkpoint();
        checkpoint.schema += 1;
        let refused = Agent::<ClaudeSdk>::restore(checkpoint, 1).err();
        assert_eq!(
            refused,
            Some(RestoreError::Schema {
                found: CHECKPOINT_SCHEMA + 1,
                expected: CHECKPOINT_SCHEMA,
            })
        );
    }
}
