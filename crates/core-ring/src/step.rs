//! What every core says about one step: how long it may take, and why it
//! asked for a dump. One definition for the daemon, agent, supervisor,
//! relay and path cores, whose drivers report them the same way.

use std::fmt;
use std::time::{Duration, Instant, SystemTime};

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
    OverBudget {
        micros: u64,
        budget: StepBudget,
        /// The kind of input the step took, when the core names its
        /// inputs: what was slow, and what a driver tells overruns apart
        /// by.
        input: Option<&'static str>,
    },
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
            DumpReason::OverBudget {
                micros,
                budget,
                input,
            } => {
                match input {
                    Some(input) => write!(f, "{} {input} step", article(input))?,
                    None => f.write_str("a step")?,
                }
                write!(
                    f,
                    " took {micros} µs, over its budget of {} µs",
                    budget.micros
                )
            }
        }
    }
}

/// The article an input kind's name takes in a sentence.
fn article(word: &str) -> &'static str {
    match word.chars().next() {
        Some('A' | 'E' | 'I' | 'O' | 'U' | 'a' | 'e' | 'i' | 'o' | 'u') => "an",
        _ => "a",
    }
}

/// When a step began, by the monotonic clock that times it and by the wall
/// clock beside it.
///
/// The monotonic clock is what a step is timed by, but a machine that
/// sleeps or suspends the process in the middle of a step can leave it
/// counting part of the pause, so a laptop whose lid closes mid-step can
/// measure a step of many minutes. The wall clock runs through the
/// whole pause, so when it moved on further than the monotonic clock did,
/// the step's time was the machine's absence, not its work.
#[derive(Clone, Copy, Debug)]
pub struct StepStart {
    pub at: Instant,
    pub wall: SystemTime,
}

impl StepStart {
    pub fn now() -> Self {
        Self {
            at: Instant::now(),
            wall: SystemTime::now(),
        }
    }
}

/// How much further the wall clock may move than the monotonic one across
/// a step before the step is taken to have spanned a sleep: well above the
/// two clocks' drift and an NTP slew over one step, and well below any
/// sleep.
const SLEPT_MARGIN: Duration = Duration::from_secs(1);

/// Whether the machine slept or suspended the process during a step that
/// began at `started` and that the monotonic clock says took `elapsed`.
/// A wall clock set back during the step reads as no sleep.
fn slept(started: &StepStart, elapsed: Duration) -> bool {
    SystemTime::now()
        .duration_since(started.wall)
        .is_ok_and(|wall| wall.saturating_sub(elapsed) > SLEPT_MARGIN)
}

/// The dumps a step that began at `started` ends by asking for: one for
/// `checked`, the core's invariants after the step, when they broke, and
/// one when the step ran longer than `budget`, not counting
/// `excluded_micros` it spent waiting on something no step decides, such as
/// a sync to the drive. `input` names the kind of input the step took, for
/// a core that names them.
///
/// A step the machine slept through is not over its budget, however long
/// it measured. The wall clock is read again only for a step that ran
/// over, so a step that did not costs one clock read more than its timing.
pub fn finish_step(
    started: StepStart,
    budget: StepBudget,
    excluded_micros: u64,
    input: Option<&'static str>,
    checked: Result<(), Violation>,
) -> impl Iterator<Item = DumpReason> {
    let elapsed = started.at.elapsed();
    let micros = u64::try_from(elapsed.as_micros())
        .unwrap_or(u64::MAX)
        .saturating_sub(excluded_micros);
    let over =
        (micros > budget.micros && !slept(&started, elapsed)).then_some(DumpReason::OverBudget {
            micros,
            budget,
            input,
        });
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

    /// A step that began `ago` by the monotonic clock and `wall_ago` by the
    /// wall clock.
    fn began(ago: Duration, wall_ago: Duration) -> StepStart {
        StepStart {
            at: Instant::now().checked_sub(ago).unwrap(),
            wall: SystemTime::now() - wall_ago,
        }
    }

    #[test]
    fn a_step_ends_with_a_dump_for_a_broken_invariant_and_one_for_its_overrun() {
        let broken = Violation {
            invariant: BROKEN,
            detail: "it broke".into(),
        };
        let slow = Duration::from_millis(250);
        let dumps: Vec<DumpReason> = finish_step(
            began(slow, slow),
            StepBudget::DAEMON,
            0,
            None,
            Err(broken.clone()),
        )
        .collect();
        assert!(
            matches!(
                &dumps[..],
                [DumpReason::Violation(v), DumpReason::OverBudget { micros, budget, input: None }]
                    if *v == broken && *micros >= 250_000 && *budget == StepBudget::DAEMON
            ),
            "{dumps:?}"
        );
        // Time spent waiting on the drive is not the step's own.
        assert_eq!(
            finish_step(began(slow, slow), StepBudget::DAEMON, 200_000, None, Ok(())).count(),
            0
        );
        assert_eq!(
            finish_step(StepStart::now(), StepBudget::DAEMON, 0, None, Ok(())).count(),
            0
        );
    }

    #[test]
    fn a_step_the_machine_slept_through_is_not_over_its_budget() {
        let slow = Duration::from_millis(250);
        let budget = StepBudget::DAEMON;
        // The wall clock moved on ten seconds further than the step's own
        // clock: the machine was away, whatever its broken invariants say.
        let slept = began(slow, slow + Duration::from_secs(10));
        let broken = Violation {
            invariant: BROKEN,
            detail: "it broke".into(),
        };
        let dumps: Vec<DumpReason> = finish_step(slept, budget, 0, None, Err(broken)).collect();
        assert!(
            matches!(&dumps[..], [DumpReason::Violation(_)]),
            "{dumps:?}"
        );
        // A wall clock a little ahead is the clocks' drift, not a sleep.
        let drifted = began(slow, slow + Duration::from_millis(500));
        assert_eq!(finish_step(drifted, budget, 0, None, Ok(())).count(), 1);
        // Nor is one set back during the step.
        let set_back = began(slow, Duration::ZERO);
        assert_eq!(finish_step(set_back, budget, 0, None, Ok(())).count(), 1);
    }

    #[test]
    fn an_overrun_names_the_input_it_stepped() {
        let budget = StepBudget { micros: 100 };
        let named = |input| {
            DumpReason::OverBudget {
                micros: 250,
                budget,
                input,
            }
            .to_string()
        };
        assert_eq!(
            named(Some("Retain")),
            "a Retain step took 250 µs, over its budget of 100 µs"
        );
        assert_eq!(
            named(Some("AgentHello")),
            "an AgentHello step took 250 µs, over its budget of 100 µs"
        );
        assert_eq!(named(None), "a step took 250 µs, over its budget of 100 µs");
        let slow = Duration::from_millis(1);
        let dumps: Vec<DumpReason> =
            finish_step(began(slow, slow), budget, 0, Some("Retain"), Ok(())).collect();
        assert!(
            matches!(
                &dumps[..],
                [DumpReason::OverBudget {
                    input: Some("Retain"),
                    ..
                }]
            ),
            "{dumps:?}"
        );
    }
}
