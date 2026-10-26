//! The supervisor's update decisions as one function of its state and an
//! input.
//!
//! [`Supervisor::step`] takes what the release feed listed, what a fetch
//! staged, what a daemon did (prepared, activated, exited, never started),
//! the requests on the control socket, a timer or the clock, and says which
//! daemon to start, tell `go`, stop or kill, which release to fetch, which
//! files beside the binary to move, and when to hand over to the installed
//! binary or exit. Processes, downloads, renames and the activation pipe are
//! the driver's. Every step ends by checking the cheap invariants, and a
//! violation is reported as [`Output::Dump`]. See the principles in
//! `docs/ARCHITECTURE.md`.
//!
//! The rules it decides:
//!
//! - It restarts the daemon whenever it exits: after `go`, with backoff,
//!   never rolled back; before `go`, as a failed start.
//! - It installs the channel's build only if the version is newer than the
//!   running one, is not the rejected build (unless `amux update --retry`
//!   asked),
//!   and the host is inside the rollout percentage; it checks only while
//!   the daemon serves and no update is being activated.
//! - The swap keeps the current binary as prev and puts the staged one at
//!   the path, stopping the daemon after (before, where a running binary
//!   cannot be replaced), then starts the new one.
//! - Activation: on `prepared`, prev goes and the daemon is told `go`; once
//!   it has been, a supervisor running a binary other than the installed
//!   one hands over to it.
//! - K starts that never activate while prev exists: record the rejected
//!   build, then put prev back and start it at once.
//! - prev is the whole record of an update in progress: a starting
//!   supervisor that finds it counts failed starts against it, finishes a
//!   rollback already recorded, or, when prev is the binary itself, removes
//!   it.
//! - Stopping the daemon is a signal, a stop deadline, then a kill.

#![forbid(unsafe_code)]

use std::time::Instant;

use release::{Choice, Manifest, Release, Skip};
pub use semver::Version;
use uuid::Uuid;

/// A timer the core asks the driver to arm. One of each at a time: arming
/// one replaces it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum TimerId {
    /// The daemon's restart, after a backoff.
    Restart,
    /// A daemon started has not prepared.
    StartDeadline,
    /// A daemon asked to stop has not exited.
    StopDeadline,
    /// The release feed is due a look.
    Check,
}

/// A job the driver runs; its outcome returns as [`Input::JobDone`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct JobId(pub u64);

/// One daemon process the supervisor started or inherited.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DaemonId(pub u64);

/// The daemon a supervisor inherits from the one that exec'd it.
pub const INHERITED_DAEMON: DaemonId = DaemonId(0);

/// A request on the control socket, by the id the driver gave it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AskId(pub u64);

/// What the control socket asks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Request {
    /// `amux update`: check the channel now. The build rolled back here is
    /// installed again only on `retry` (`amux update --retry`).
    Check { retry: bool },
    /// Stop the daemon and the supervisor.
    Stop,
}

/// What a starting supervisor finds beside the binary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PrevFound {
    None,
    /// prev is the same file as the binary: a swap that never renamed
    /// over the path.
    SameAsBinary,
    /// prev is another binary: an update is in progress.
    Other,
}

/// The supervisor's timings and thresholds.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Params {
    pub check_interval_ms: u64,
    /// Starts that never activate, while prev exists, before rolling back.
    pub rollback_after: u32,
    /// From spawn to `prepared`.
    pub start_deadline_ms: u64,
    /// From the stop signal to the kill.
    pub stop_deadline_ms: u64,
    pub backoff_first_ms: u64,
    pub backoff_max_ms: u64,
    /// A daemon up this long starts the backoff over.
    pub backoff_reset_ms: u64,
    /// The binary a daemon runs may be renamed over while it runs (not on
    /// Windows, where the daemon stops before the swap).
    pub replace_while_running: bool,
    /// How long one step may take before it asks for a dump.
    pub step_budget: StepBudget,
}

/// How a supervisor starts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Start {
    pub params: Params,
    /// The build target the channel's manifest is read for.
    pub target: String,
    /// This binary's version.
    pub running: Version,
    pub prev: PrevFound,
    /// The rolled-back build's version, if one was recorded.
    pub rejected: Option<Version>,
    /// The supervisor that exec'd this one handed over its daemon, already
    /// told `go`; no recovery runs.
    pub inherited: bool,
    pub now_ms: u64,
}

/// Everything that can happen to the supervisor, decoded by the driver.
/// The driver feeds [`Input::Tick`] with its clock before every other
/// input.
#[derive(Clone, Debug, PartialEq)]
pub enum Input {
    /// The daemon wrote `prepared`: it migrated and looked without writing.
    Prepared {
        daemon: DaemonId,
    },
    /// The daemon was told `go` and may write.
    Activated {
        daemon: DaemonId,
    },
    /// The daemon could not be started.
    StartFailed {
        daemon: DaemonId,
        why: String,
    },
    /// The daemon exited: its code, or on Unix the negated signal.
    DaemonExited {
        daemon: DaemonId,
        code: i32,
    },
    /// A request on the control socket.
    Asked {
        ask: AskId,
        request: Request,
    },
    /// The supervisor was signalled to stop.
    Terminate,
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

/// Everything the supervisor wants done, in order. The file outputs
/// (`RemovePrev`, `RetirePrev`, `WriteRejected`, `RestorePrev`,
/// `InstallStaged`) are performed inline and durably; one that fails ends
/// the supervisor with the error, performing nothing after it.
#[derive(Clone, Debug, PartialEq)]
pub enum Output {
    /// Start `<binary> daemon` with the activation pipe.
    Spawn {
        daemon: DaemonId,
    },
    /// Answer the daemon's `prepared` with `go`; [`Input::Activated`] when
    /// it was written.
    Go {
        daemon: DaemonId,
    },
    /// Ask the daemon to stop.
    Stop {
        daemon: DaemonId,
    },
    Kill {
        daemon: DaemonId,
    },
    /// Read the channel's manifest, at most its size limit, and verify its
    /// signature. With `auto_only`, only when updates are automatic.
    List {
        job: JobId,
        auto_only: bool,
    },
    /// Download the release beside the binary, verify it against its
    /// manifest entry and make it executable.
    Fetch {
        job: JobId,
        release: Release,
    },
    /// prev is the binary itself: remove it.
    RemovePrev,
    /// Activation's cleanup: prev is no longer needed.
    RetirePrev,
    /// Record the rolled-back build, before prev moves.
    WriteRejected {
        version: Version,
    },
    /// Rename prev back over the binary.
    RestorePrev,
    /// Put the staged binary at the path, keeping the current one as prev.
    InstallStaged,
    /// Replace this process with the installed binary in supervise mode,
    /// keeping the daemon, its pipe and the lock. A failure leaves this
    /// supervisor running, which is still correct.
    BecomeInstalled {
        daemon: DaemonId,
    },
    /// Answer a request on the control socket.
    Answer {
        ask: AskId,
        text: String,
    },
    ArmTimer {
        id: TimerId,
        at_ms: u64,
    },
    CancelTimer {
        id: TimerId,
    },
    /// The supervisor is done: its daemon has stopped.
    Exit,
    /// The supervisor cannot go on; the driver ends it with this error.
    Fail {
        what: &'static str,
        why: String,
    },
    /// Take a dump: the core saw something it must not carry on from.
    Dump {
        reason: DumpReason,
    },
}

/// What a job produced.
#[derive(Clone, Debug, PartialEq)]
pub enum JobResult {
    /// What the release feed listed.
    Listed(Listed),
    /// The release is staged beside the binary, verified; or why not.
    Fetched(Result<(), String>),
}

/// The channel's word, as the driver read it.
#[derive(Clone, Debug, PartialEq)]
pub enum Listed {
    /// Updates are not automatic, and the look was not asked for.
    Off,
    /// Nothing can be installed, and why.
    Nothing(String),
    /// The verified manifest, and the host id that places this
    /// installation in a rollout.
    Manifest {
        manifest: Manifest,
        host: Option<Uuid>,
    },
    /// The look failed.
    Failed(String),
}

/// Which file a binary is: the one this supervisor runs, or another.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum File {
    Mine,
    Other,
}

/// A binary beside the install: which file, its version when known, and
/// whether a daemon from it has served here, so it can start.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Build {
    file: File,
    version: Option<Version>,
    proven: bool,
}

/// The binaries on disk: the one at the path, and prev while an update is
/// being activated.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Installed {
    path: Build,
    prev: Option<Build>,
}

/// A daemon the supervisor started, and where it is in its life.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Running {
    id: DaemonId,
    phase: Phase,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Phase {
    /// Started, read only until it prepares and is told `go`; stopped at
    /// the deadline.
    Starting { deadline: u64, told_go: bool },
    /// Told `go`: it may write.
    Serving { since: u64 },
    /// Asked to stop; killed at the deadline. What follows its exit.
    Stopping { deadline: u64, then: Then },
}

/// What follows a stopped daemon's exit.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Then {
    /// It never activated: a failed start.
    FailedStart,
    /// The staged release goes in (where the binary cannot be replaced
    /// while it runs), then the new daemon starts.
    Install,
    /// The new binary is in place: start it.
    Restart,
    /// The supervisor exits.
    Exit,
}

/// A look at the feed in flight, and the `amux update` callers waiting
/// for its answer.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Checking {
    job: JobId,
    manual: bool,
    /// The rolled-back build may be installed again.
    retry: bool,
    answers: Vec<AskId>,
    /// The release being fetched.
    fetching: Option<Version>,
}

/// Restart delays: doubling from the first to the cap, starting over once
/// a daemon has stayed up long enough.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Backoff {
    next_ms: u64,
}

/// What this step did that the invariants judge. Not carried from one
/// step to the next.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
struct Touched {
    spawned: bool,
}

/// Installed binaries, the running daemon and its phase, the release seen,
/// failed starts and the rollback target. A value between steps: equal
/// supervisors step alike.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Supervisor {
    params: Params,
    target: String,
    /// This binary's version.
    running: Version,
    installed: Installed,
    daemons: Vec<Running>,
    /// The rolled-back build: the rollback target is prev.
    rejected: Option<Version>,
    /// Starts that never activated since prev appeared.
    failed_starts: u32,
    backoff: Backoff,
    restart_at: Option<u64>,
    /// The release feed is due a look once the daemon serves.
    check_due: bool,
    checking: Option<Checking>,
    /// The release staged and on its way to the path.
    fetched: Option<Version>,
    /// `amux update` callers waiting for the daemon to serve, and whether
    /// any of them asked to retry the rolled-back build.
    waiting: Vec<AskId>,
    waiting_retry: bool,
    stopping: bool,
    exited: bool,
    now_ms: u64,
    next_id: u64,
    touched: Touched,
}

impl Supervisor {
    /// A starting supervisor: it reads the record an update left (unless it
    /// inherited its daemon), arms the first look at the feed, and starts
    /// the daemon if it has none.
    pub fn start(start: Start, out: &mut Vec<Output>) -> Self {
        let Start {
            params,
            target,
            running,
            prev,
            rejected,
            inherited,
            now_ms,
        } = start;
        let mine = |proven| Build {
            file: File::Mine,
            version: Some(running.clone()),
            proven,
        };
        let other = || Build {
            file: File::Other,
            version: None,
            proven: true,
        };
        let installed = match (inherited, prev) {
            (_, PrevFound::None) => Installed {
                path: mine(true),
                prev: None,
            },
            (true, PrevFound::SameAsBinary) => Installed {
                path: mine(true),
                prev: Some(mine(true)),
            },
            (false, PrevFound::SameAsBinary) => {
                out.push(Output::RemovePrev);
                Installed {
                    path: mine(true),
                    prev: None,
                }
            }
            // A recorded rollback whose rename never happened: finish it.
            (false, PrevFound::Other) if rejected.as_ref() == Some(&running) => {
                out.push(Output::RestorePrev);
                Installed {
                    path: other(),
                    prev: None,
                }
            }
            // An update in progress: failed starts count against it.
            (_, PrevFound::Other) => Installed {
                path: mine(false),
                prev: Some(other()),
            },
        };
        let mut supervisor = Self {
            backoff: Backoff {
                next_ms: params.backoff_first_ms,
            },
            target,
            running,
            installed,
            daemons: Vec::new(),
            rejected,
            failed_starts: 0,
            restart_at: None,
            check_due: false,
            checking: None,
            fetched: None,
            waiting: Vec::new(),
            waiting_retry: false,
            stopping: false,
            exited: false,
            now_ms,
            next_id: 0,
            touched: Touched::default(),
            params,
        };
        let check = now_ms.saturating_add(supervisor.params.check_interval_ms);
        out.push(Output::ArmTimer {
            id: TimerId::Check,
            at_ms: check,
        });
        if inherited {
            supervisor.daemons.push(Running {
                id: INHERITED_DAEMON,
                phase: Phase::Serving { since: now_ms },
            });
        }
        supervisor.settle(out);
        supervisor.touched = Touched::default();
        supervisor
    }

    /// One input in, outputs appended. Ends by checking the cheap
    /// invariants; a violation appends [`Output::Dump`], and so does a step
    /// that took longer than its budget.
    pub fn step(&mut self, input: Input, out: &mut Vec<Output>) {
        // The step's own duration is the one reading of the world a core
        // makes: it decides nothing but whether to ask for a dump.
        let started = Instant::now();
        self.touched = Touched::default();
        if !self.exited {
            self.take(input, out);
            if !self.exited {
                self.settle(out);
            }
        }
        if let Err(violation) = self.check_step_invariants() {
            out.push(Output::Dump {
                reason: DumpReason::Violation(violation),
            });
        }
        self.touched = Touched::default();
        let budget = self.params.step_budget;
        let micros = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
        if micros > budget.micros {
            out.push(Output::Dump {
                reason: DumpReason::OverBudget { micros, budget },
            });
        }
    }

    /// The cheap predicates checked after every step:
    ///
    /// - there is always an installed binary that can start: the one at
    ///   the path has served here, or prev, which has, is kept beside it;
    /// - at most one daemon is in the serving phase, and no daemon starts
    ///   while another lives.
    pub fn check_step_invariants(&self) -> Result<(), Violation> {
        let Installed { path, prev } = &self.installed;
        if !path.proven && !prev.as_ref().is_some_and(|prev| prev.proven) {
            return Err(Violation {
                invariant: "there is always an installed binary that can start",
                detail: format!(
                    "the binary at the path ({:?}) has not served and prev is {:?}",
                    path.version, prev
                ),
            });
        }
        let serving = self
            .daemons
            .iter()
            .filter(|daemon| matches!(daemon.phase, Phase::Serving { .. }))
            .count();
        if serving > 1 {
            return Err(Violation {
                invariant: "at most one daemon is in the serving phase",
                detail: format!("{serving} daemons serve: {:?}", self.daemons),
            });
        }
        if self.touched.spawned && self.daemons.len() > 1 {
            return Err(Violation {
                invariant: "no daemon starts while another lives",
                detail: format!("{:?}", self.daemons),
            });
        }
        Ok(())
    }

    // --- inputs ----------------------------------------------------------

    fn take(&mut self, input: Input, out: &mut Vec<Output>) {
        match input {
            Input::Tick { now_ms } => self.now_ms = self.now_ms.max(now_ms),
            Input::Prepared { daemon } => self.prepared(daemon, out),
            Input::Activated { daemon } => self.activated(daemon, out),
            Input::StartFailed { daemon, .. } => {
                if self.forget(daemon).is_some() {
                    self.failed_start(out);
                }
            }
            Input::DaemonExited { daemon, .. } => self.daemon_exited(daemon, out),
            Input::Asked { ask, request } => self.asked(ask, request, out),
            Input::Terminate => self.stop_everything(out),
            Input::Timer(id) => self.timer(id, out),
            Input::JobDone { job, result } => self.job_done(job, result, out),
        }
    }

    fn prepared(&mut self, daemon: DaemonId, out: &mut Vec<Output>) {
        let Some(running) = self.daemons.iter_mut().find(|running| running.id == daemon) else {
            return;
        };
        let Phase::Starting { told_go, .. } = &mut running.phase else {
            return;
        };
        if *told_go {
            return;
        }
        *told_go = true;
        // A daemon that prepared has started: the build at the path can.
        self.installed.path.proven = true;
        if self.installed.prev.take().is_some() {
            out.push(Output::RetirePrev);
        }
        self.failed_starts = 0;
        out.push(Output::Go { daemon });
    }

    fn activated(&mut self, daemon: DaemonId, out: &mut Vec<Output>) {
        let now = self.now_ms;
        let Some(running) = self.daemons.iter_mut().find(|running| running.id == daemon) else {
            return;
        };
        if !matches!(running.phase, Phase::Starting { told_go: true, .. }) {
            return;
        }
        running.phase = Phase::Serving { since: now };
        out.push(Output::CancelTimer {
            id: TimerId::StartDeadline,
        });
        if self.installed.path.file != File::Mine {
            out.push(Output::BecomeInstalled { daemon });
        }
    }

    fn daemon_exited(&mut self, daemon: DaemonId, out: &mut Vec<Output>) {
        let Some(running) = self.forget(daemon) else {
            return;
        };
        match running.phase {
            Phase::Serving { since } => {
                let up = self.now_ms.saturating_sub(since);
                let delay = self.next_backoff(up);
                self.restart_in(delay, out);
            }
            Phase::Starting { .. } => {
                out.push(Output::CancelTimer {
                    id: TimerId::StartDeadline,
                });
                self.failed_start(out);
            }
            Phase::Stopping { then, .. } => {
                out.push(Output::CancelTimer {
                    id: TimerId::StopDeadline,
                });
                match then {
                    Then::FailedStart => self.failed_start(out),
                    Then::Install => {
                        out.push(Output::InstallStaged);
                        self.installed_staged();
                    }
                    Then::Restart => {}
                    Then::Exit => {
                        self.exited = true;
                        out.push(Output::Exit);
                    }
                }
            }
        }
    }

    fn asked(&mut self, ask: AskId, request: Request, out: &mut Vec<Output>) {
        match request {
            Request::Stop => {
                out.push(Output::Answer {
                    ask,
                    text: "stopping".into(),
                });
                self.stop_everything(out);
            }
            Request::Check { retry } => {
                let text = if self.installed.prev.is_some() {
                    "an update is being activated; try again once it is"
                } else if self.serving().is_some() {
                    self.waiting.push(ask);
                    self.waiting_retry |= retry;
                    return;
                } else if self.daemons.is_empty() || self.stopping_daemon() {
                    "the daemon is restarting; try again shortly"
                } else {
                    "the daemon is starting; try again shortly"
                };
                out.push(Output::Answer {
                    ask,
                    text: text.into(),
                });
            }
        }
    }

    fn timer(&mut self, id: TimerId, out: &mut Vec<Output>) {
        let now = self.now_ms;
        match id {
            TimerId::Restart => {
                if self.restart_at.is_some_and(|at| at <= now) {
                    self.restart_at = None;
                }
            }
            TimerId::StartDeadline => {
                let late = self.daemons.iter().find_map(|running| match running.phase {
                    Phase::Starting { deadline, .. } if deadline <= now => Some(running.id),
                    _ => None,
                });
                if let Some(daemon) = late {
                    self.stop(daemon, Then::FailedStart, out);
                }
            }
            TimerId::StopDeadline => {
                for running in &self.daemons {
                    if matches!(running.phase, Phase::Stopping { deadline, .. } if deadline <= now)
                    {
                        out.push(Output::Kill { daemon: running.id });
                    }
                }
            }
            TimerId::Check => {
                self.check_due = true;
            }
        }
    }

    fn job_done(&mut self, job: JobId, result: JobResult, out: &mut Vec<Output>) {
        let Some(mut checking) = self.checking.take_if(|checking| checking.job == job) else {
            return;
        };
        let answer = match result {
            JobResult::Listed(Listed::Off) => String::new(),
            JobResult::Listed(Listed::Nothing(why)) => why,
            JobResult::Listed(Listed::Failed(why)) => format!("the check failed: {why}"),
            JobResult::Listed(Listed::Manifest { manifest, host }) => {
                let rejected = (!checking.retry)
                    .then_some(self.rejected.as_ref())
                    .flatten();
                let choice = release::choose(
                    &manifest,
                    &self.target,
                    &self.running,
                    rejected,
                    host.as_ref(),
                );
                match choice {
                    Choice::Install { release, version } => {
                        checking.job = JobId(self.id());
                        checking.fetching = Some(version);
                        out.push(Output::Fetch {
                            job: checking.job,
                            release,
                        });
                        self.checking = Some(checking);
                        return;
                    }
                    Choice::Skip(skip) => skipped(skip, &self.target, &self.running),
                }
            }
            JobResult::Fetched(Err(why)) => format!("the check failed: {why}"),
            JobResult::Fetched(Ok(())) => {
                let Some(version) = checking.fetching else {
                    return;
                };
                for ask in checking.answers {
                    out.push(Output::Answer {
                        ask,
                        text: format!("installing {version}"),
                    });
                }
                self.swap(version, out);
                return;
            }
        };
        for ask in checking.answers {
            out.push(Output::Answer {
                ask,
                text: answer.clone(),
            });
        }
    }

    // --- decisions -------------------------------------------------------

    /// After anything happened: a look at the feed that is due and may run
    /// starts, and with no daemon, no restart pending and nothing stopping,
    /// the daemon starts.
    fn settle(&mut self, out: &mut Vec<Output>) {
        let settled = self.serving().is_some() && self.installed.prev.is_none();
        if settled && self.checking.is_none() {
            if !self.waiting.is_empty() {
                let answers = std::mem::take(&mut self.waiting);
                let retry = std::mem::take(&mut self.waiting_retry);
                self.look(true, retry, answers, out);
            } else if self.check_due {
                self.check_due = false;
                let at_ms = self.now_ms.saturating_add(self.params.check_interval_ms);
                out.push(Output::ArmTimer {
                    id: TimerId::Check,
                    at_ms,
                });
                self.look(false, false, Vec::new(), out);
            }
        }
        if self.daemons.is_empty() && self.restart_at.is_none() && !self.stopping {
            let daemon = DaemonId(self.id());
            let deadline = self.now_ms.saturating_add(self.params.start_deadline_ms);
            self.daemons.push(Running {
                id: daemon,
                phase: Phase::Starting {
                    deadline,
                    told_go: false,
                },
            });
            self.touched.spawned = true;
            out.push(Output::Spawn { daemon });
            out.push(Output::ArmTimer {
                id: TimerId::StartDeadline,
                at_ms: deadline,
            });
        }
    }

    fn look(&mut self, manual: bool, retry: bool, answers: Vec<AskId>, out: &mut Vec<Output>) {
        let job = JobId(self.id());
        self.checking = Some(Checking {
            job,
            manual,
            retry,
            answers,
            fetching: None,
        });
        out.push(Output::List {
            job,
            auto_only: !manual,
        });
    }

    /// A start that never activated. While prev exists it counts toward
    /// the rollback; the K-th rolls back and starts prev at once.
    fn failed_start(&mut self, out: &mut Vec<Output>) {
        if self.installed.prev.is_some() {
            self.failed_starts += 1;
            if self.failed_starts >= self.params.rollback_after {
                self.roll_back(out);
                return;
            }
        }
        let delay = self.next_backoff(0);
        self.restart_in(delay, out);
    }

    /// Record first, rename second: a crash between the two leaves a
    /// rejected version equal to the installed binary with prev present,
    /// which a starting supervisor finishes.
    fn roll_back(&mut self, out: &mut Vec<Output>) {
        let Some(rejected) = self.installed.path.version.clone() else {
            out.push(Output::Fail {
                what: "rolling back",
                why: "the installed version is not known".into(),
            });
            self.exited = true;
            return;
        };
        out.push(Output::WriteRejected {
            version: rejected.clone(),
        });
        out.push(Output::RestorePrev);
        self.rejected = Some(rejected);
        if let Some(prev) = self.installed.prev.take() {
            self.installed.path = prev;
        }
        self.failed_starts = 0;
        self.backoff = Backoff {
            next_ms: self.params.backoff_first_ms,
        };
    }

    /// The staged release goes in and the daemon restarts on it: where a
    /// running binary can be replaced, before the daemon stops; elsewhere
    /// after.
    fn swap(&mut self, version: Version, out: &mut Vec<Output>) {
        self.fetched = Some(version);
        let alive = self
            .daemons
            .iter()
            .find(|running| !matches!(running.phase, Phase::Stopping { .. }))
            .map(|running| running.id);
        if self.params.replace_while_running || alive.is_none() {
            out.push(Output::InstallStaged);
            self.installed_staged();
            if let Some(daemon) = alive {
                self.stop(daemon, Then::Restart, out);
            }
        } else if let Some(daemon) = alive {
            self.stop(daemon, Then::Install, out);
        }
        if self.restart_at.take().is_some() {
            out.push(Output::CancelTimer {
                id: TimerId::Restart,
            });
        }
    }

    /// The staged binary is at the path; the one it replaced is prev.
    fn installed_staged(&mut self) {
        let version = self.fetched.take();
        let replaced = std::mem::replace(
            &mut self.installed.path,
            Build {
                file: File::Other,
                version,
                proven: false,
            },
        );
        self.installed.prev = Some(replaced);
        self.failed_starts = 0;
        self.backoff = Backoff {
            next_ms: self.params.backoff_first_ms,
        };
    }

    fn stop(&mut self, daemon: DaemonId, then: Then, out: &mut Vec<Output>) {
        let Some(running) = self.daemons.iter_mut().find(|running| running.id == daemon) else {
            return;
        };
        if let Phase::Stopping { then: already, .. } = &mut running.phase {
            if then == Then::Exit {
                *already = Then::Exit;
            }
            return;
        }
        if matches!(running.phase, Phase::Starting { .. }) {
            out.push(Output::CancelTimer {
                id: TimerId::StartDeadline,
            });
        }
        let deadline = self.now_ms.saturating_add(self.params.stop_deadline_ms);
        running.phase = Phase::Stopping { deadline, then };
        out.push(Output::Stop { daemon });
        out.push(Output::ArmTimer {
            id: TimerId::StopDeadline,
            at_ms: deadline,
        });
    }

    /// The supervisor stops: its daemon first, then itself.
    fn stop_everything(&mut self, out: &mut Vec<Output>) {
        self.stopping = true;
        let alive: Vec<DaemonId> = self.daemons.iter().map(|running| running.id).collect();
        if alive.is_empty() {
            self.exited = true;
            out.push(Output::Exit);
            return;
        }
        for daemon in alive {
            self.stop(daemon, Then::Exit, out);
        }
    }

    fn restart_in(&mut self, delay: u64, out: &mut Vec<Output>) {
        let at_ms = self.now_ms.saturating_add(delay);
        self.restart_at = Some(at_ms);
        out.push(Output::ArmTimer {
            id: TimerId::Restart,
            at_ms,
        });
    }

    fn next_backoff(&mut self, up_ms: u64) -> u64 {
        if up_ms >= self.params.backoff_reset_ms {
            self.backoff.next_ms = self.params.backoff_first_ms;
        }
        let delay = self.backoff.next_ms;
        self.backoff.next_ms = delay.saturating_mul(2).min(self.params.backoff_max_ms);
        delay
    }

    fn serving(&self) -> Option<DaemonId> {
        self.daemons
            .iter()
            .find(|running| matches!(running.phase, Phase::Serving { .. }))
            .map(|running| running.id)
    }

    fn stopping_daemon(&self) -> bool {
        self.daemons
            .iter()
            .any(|running| matches!(running.phase, Phase::Stopping { .. }))
    }

    fn forget(&mut self, daemon: DaemonId) -> Option<Running> {
        let at = self
            .daemons
            .iter()
            .position(|running| running.id == daemon)?;
        Some(self.daemons.remove(at))
    }

    fn id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }
}

/// Why nothing was installed, as `amux update` says it.
pub fn skipped(skip: Skip, target: &str, running: &Version) -> String {
    match skip {
        Skip::NoBuildForTarget => format!("the channel has no build for {target}"),
        Skip::UnreadableVersion(version) => {
            format!("the channel names {version:?}, which is not a version")
        }
        Skip::NotNewer(version) => {
            format!("up to date: {running} is running and the channel names {version}")
        }
        Skip::Rejected(version) => {
            format!("{version} was rolled back here; `amux update --retry` tries it again")
        }
        Skip::OutsideRollout => {
            "the channel's release is rolling out and has not reached this host yet".into()
        }
    }
}

pub use core_ring::{DumpReason, StepBudget, Violation};

#[cfg(test)]
mod tests;
