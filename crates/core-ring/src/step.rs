//! What every core says about one step: how long it may take, and why it
//! asked for a dump. One definition for the daemon, agent, supervisor,
//! relay and path cores, whose drivers report them the same way.

use std::fmt;
use std::time::Instant;

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

/// The dumps a step that began at `started` ends by asking for: one for
/// `checked`, the core's invariants after the step, when they broke, and
/// one when the step ran longer than `budget`, not counting
/// `excluded_micros` it spent waiting on something no step decides, such as
/// a sync to the drive.
pub fn finish_step(
    started: Instant,
    budget: StepBudget,
    excluded_micros: u64,
    checked: Result<(), Violation>,
) -> impl Iterator<Item = DumpReason> {
    let micros = u64::try_from(started.elapsed().as_micros())
        .unwrap_or(u64::MAX)
        .saturating_sub(excluded_micros);
    let over = (micros > budget.micros).then_some(DumpReason::OverBudget { micros, budget });
    checked
        .err()
        .map(DumpReason::Violation)
        .into_iter()
        .chain(over)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BROKEN: Check = Check {
        id: "test.broken",
        sentence: "nothing breaks",
    };

    #[test]
    fn a_step_ends_with_a_dump_for_a_broken_invariant_and_one_for_its_overrun() {
        let broken = Violation {
            invariant: BROKEN,
            detail: "it broke".into(),
        };
        // Any step takes longer than nothing, given a clock fine enough;
        // spin until one is measured over.
        let started = Instant::now();
        while started.elapsed().as_micros() == 0 {}
        let zero = StepBudget { micros: 0 };
        let dumps: Vec<DumpReason> = finish_step(started, zero, 0, Err(broken.clone())).collect();
        assert!(
            matches!(
                &dumps[..],
                [DumpReason::Violation(v), DumpReason::OverBudget { micros, budget }]
                    if *v == broken && *micros > 0 && *budget == zero
            ),
            "{dumps:?}"
        );
        // Time spent waiting on the drive is not the step's own.
        assert_eq!(finish_step(started, zero, u64::MAX, Ok(())).count(), 0);
        assert_eq!(
            finish_step(Instant::now(), StepBudget::DAEMON, 0, Ok(())).count(),
            0
        );
    }
}
