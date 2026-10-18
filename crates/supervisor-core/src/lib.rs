//! The supervisor's update decisions as one function of its state and an
//! input.
//!
//! [`Supervisor::step`] takes what the release feed listed, what a start
//! did or how the daemon exited, and says which binary to fetch, start,
//! stop, set aside, go with or roll back to. Processes, downloads and
//! renames are the driver's. Every step ends by checking the cheap
//! invariants, and a violation is reported as [`Output::Dump`]. See the
//! principles in `docs/ARCHITECTURE.md`.

#![forbid(unsafe_code)]

use std::fmt;
use std::path::PathBuf;

use release::{Manifest, Release};

/// A release's version, as its manifest and the binary's stamp state it.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Version(pub String);

/// A timer the core asked the driver to arm.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TimerId(pub u64);

/// A job the driver runs; its outcome returns as [`Input::JobDone`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct JobId(pub u64);

/// What a job produced.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct JobResult;

/// Everything that can happen to the supervisor, decoded by the driver.
#[derive(Clone, Debug, PartialEq)]
pub enum Input {
    /// The feed's manifest, after the driver verified its signature.
    ReleaseListed(Manifest),
    /// A fetched release is unpacked and verified beside the installed one.
    Prepared {
        version: Version,
    },
    /// The daemon started from `version` reports it is serving.
    Activated {
        version: Version,
    },
    StartFailed {
        version: Version,
    },
    DaemonExited {
        code: Option<i32>,
    },
    Timer(TimerId),
    /// The clock, as an input.
    Tick {
        now_ms: u64,
    },
    JobDone {
        job: JobId,
        result: JobResult,
    },
}

/// Everything the supervisor wants done.
#[derive(Clone, Debug, PartialEq)]
pub enum Output {
    Fetch {
        release: Release,
    },
    Spawn {
        binary: PathBuf,
    },
    /// Ask the daemon to stop, killing it if it is still running at the
    /// deadline.
    Stop {
        deadline_ms: u64,
    },
    Kill,
    RenameAside {
        from: PathBuf,
        to: PathBuf,
    },
    /// The new daemon may start writing: everything before this is read
    /// only, apart from migration and the generation.
    Go {
        version: Version,
    },
    Rollback {
        to: Version,
    },
    ArmTimer {
        id: TimerId,
        at_ms: u64,
    },
    /// Take a dump: the core saw something it must not carry on from.
    Dump {
        reason: DumpReason,
    },
}

/// Installed binaries, the running daemon and its phase, the release seen,
/// failed starts and the rollback target.
#[expect(
    dead_code,
    reason = "each part is read once the inputs that move it are"
)]
pub struct Supervisor {
    installed: Installed,
    running: Option<Running>,
    seen: Option<Version>,
    failed_starts: u32,
    rollback: Option<Version>,
    params: Params,
    budget: StepBudget,
}

/// The binaries on disk: the current one and any set aside.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Installed {}

/// The daemon the supervisor started and the phase it is in.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Running {}

/// The supervisor's settings: the channel, the update policy and its
/// timings.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Params {}

impl Supervisor {
    /// A supervisor over `installed` with no daemon running yet.
    pub fn new(installed: Installed, params: Params) -> Self {
        Self {
            installed,
            running: None,
            seen: None,
            failed_starts: 0,
            rollback: None,
            params,
            budget: StepBudget::SUPERVISOR,
        }
    }

    /// One input in, outputs appended. Ends by checking the cheap
    /// invariants; a violation appends [`Output::Dump`].
    pub fn step(&mut self, input: Input, out: &mut Vec<Output>) {
        match input {
            Input::ReleaseListed(_)
            | Input::Prepared { .. }
            | Input::Activated { .. }
            | Input::StartFailed { .. }
            | Input::DaemonExited { .. }
            | Input::Timer(_)
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
}

/// How long one step may take before it is reported with a dump.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StepBudget {
    pub micros: u64,
}

impl StepBudget {
    /// The supervisor's: it decides rarely and never under load.
    pub const SUPERVISOR: StepBudget = StepBudget { micros: 10_000 };
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tick_asks_for_nothing() {
        let mut supervisor = Supervisor::new(Installed::default(), Params::default());
        let mut out = Vec::new();
        supervisor.step(Input::Tick { now_ms: 5 }, &mut out);
        assert_eq!(out, Vec::new());
    }
}
