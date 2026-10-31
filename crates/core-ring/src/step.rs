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

/// One cheap invariant a core checks at the end of every step.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Check {
    /// A stable token naming the check, `<core>.<name>`: what dumps, reports
    /// and usage events group by, so rewording the sentence does not split
    /// one check's history in two. Lowercase letters, digits, `_` and `.`.
    pub id: &'static str,
    /// The invariant, as the sentence it checks, for people.
    pub sentence: &'static str,
}

impl Check {
    /// Whether `id` has the shape every check id has; the cores' lists are
    /// tested against it, and the account service accepts nothing else.
    pub fn is_id(id: &str) -> bool {
        !id.is_empty()
            && id.len() <= 64
            && id.contains('.')
            && id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'.')
    }
}

impl fmt::Display for Check {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.sentence)
    }
}

/// A cheap invariant that failed: which one, and what it saw.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Violation {
    pub invariant: Check,
    /// The values that broke it.
    pub detail: String,
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.invariant.sentence, self.detail)
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

impl DumpReason {
    /// The check that broke, or `None` for a step over its budget.
    pub fn check(&self) -> Option<Check> {
        match self {
            DumpReason::Violation(violation) => Some(violation.invariant),
            DumpReason::OverBudget { .. } => None,
        }
    }
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
