//! What every core says about one step: how long it may take, and why it
//! asked for a dump. One definition for the daemon, agent, supervisor,
//! relay and path cores, whose drivers report them the same way.

use std::fmt;

use serde::{Deserialize, Serialize};

/// How long one step may take before it is reported with a dump.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StepBudget {
    pub micros: u64,
}

impl StepBudget {
    /// The daemon's: one step delays every other input for its duration,
    /// and a tenth of a second is a stall a person notices. A full ingest
    /// batch at the performance budget's ceiling of 100 µs a frame takes
    /// about 26 ms, so a step past this is doing something no step should.
    pub const DAEMON: StepBudget = StepBudget { micros: 100_000 };
    /// Test installations': debug builds on a loaded four-core CI runner
    /// take 118 to 143 ms for steps the daemon's budget allows 100 ms, and
    /// a dump written for that overrun lands in `reports/` while a test may
    /// be reading it. A second is still short enough that a step genuinely
    /// stuck is reported. Tests of the tripwire set their own budget.
    pub const TEST: StepBudget = StepBudget { micros: 1_000_000 };
    /// The agent process's: a step stands between the provider and its
    /// journal.
    pub const AGENT: StepBudget = StepBudget { micros: 10_000 };
    /// The supervisor's: it decides rarely and never under load.
    pub const SUPERVISOR: StepBudget = StepBudget { micros: 10_000 };
    /// A budget no step runs over, for a run that must be a function of
    /// its inputs alone: a checker, a simulator, a replay.
    pub const UNLIMITED: StepBudget = StepBudget { micros: u64::MAX };
}

impl Default for StepBudget {
    fn default() -> Self {
        Self::DAEMON
    }
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

impl fmt::Display for DumpReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DumpReason::Violation(violation) => write!(f, "an invariant failed: {violation}"),
            DumpReason::OverBudget { micros, budget } => write!(
                f,
                "a step took {micros} µs, over its budget of {} µs",
                budget.micros
            ),
        }
    }
}
