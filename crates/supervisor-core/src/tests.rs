use release::{Manifest, Release};

use super::*;

const TARGET: &str = "test-target";

fn params() -> Params {
    Params {
        check_interval_ms: 3_600_000,
        rollback_after: 3,
        start_deadline_ms: 60_000,
        stop_deadline_ms: 30_000,
        backoff_first_ms: 1_000,
        backoff_max_ms: 30_000,
        backoff_reset_ms: 60_000,
        replace_while_running: true,
    }
}

fn version(text: &str) -> Version {
    Version::parse(text).expect("a version")
}

/// A supervisor under test, the clock it was last told, and every output.
struct Run {
    core: Supervisor,
    now_ms: u64,
    out: Vec<Output>,
}

impl Run {
    fn start(prev: PrevFound, rejected: Option<&str>, inherited: bool) -> Self {
        Self::with(params(), prev, rejected, inherited)
    }

    fn with(params: Params, prev: PrevFound, rejected: Option<&str>, inherited: bool) -> Self {
        let mut out = Vec::new();
        let core = Supervisor::start(
            Start {
                params,
                target: TARGET.into(),
                running: version("1.0.0"),
                prev,
                rejected: rejected.map(version),
                inherited,
                now_ms: 1_000,
            },
            &mut out,
        );
        Self {
            core,
            now_ms: 1_000,
            out,
        }
    }

    fn at(&mut self, now_ms: u64, input: Input) -> Vec<Output> {
        self.now_ms = now_ms;
        let mut out = Vec::new();
        self.core.step(Input::Tick { now_ms }, &mut out);
        self.core.step(input, &mut out);
        self.out.extend(out.iter().cloned());
        out
    }

    fn on(&mut self, input: Input) -> Vec<Output> {
        self.at(self.now_ms + 1, input)
    }

    /// The daemon the last `Spawn` started.
    fn spawned(&self) -> DaemonId {
        self.out
            .iter()
            .rev()
            .find_map(|output| match output {
                Output::Spawn { daemon } => Some(*daemon),
                _ => None,
            })
            .expect("a daemon was started")
    }

    /// The last daemon started prepares and is told go.
    fn serves(&mut self) -> DaemonId {
        let daemon = self.spawned();
        let out = self.on(Input::Prepared { daemon });
        assert!(out.contains(&Output::Go { daemon }), "{out:?}");
        self.on(Input::Activated { daemon });
        daemon
    }

    /// The feed lists `listed`; the look the core asked for gets it.
    fn listed(&mut self, listed: Listed) -> Vec<Output> {
        let job = self
            .out
            .iter()
            .rev()
            .find_map(|output| match output {
                Output::List { job, .. } => Some(*job),
                _ => None,
            })
            .expect("a look was asked for");
        self.on(Input::JobDone {
            job,
            result: JobResult::Listed(listed),
        })
    }

    fn fetched(&mut self) -> Vec<Output> {
        let job = self
            .out
            .iter()
            .rev()
            .find_map(|output| match output {
                Output::Fetch { job, .. } => Some(*job),
                _ => None,
            })
            .expect("a fetch was asked for");
        self.on(Input::JobDone {
            job,
            result: JobResult::Fetched(Ok(())),
        })
    }
}

fn manifest(version: &str) -> Manifest {
    Manifest {
        channel: "stable".into(),
        rollout: None,
        targets: [(
            TARGET.to_owned(),
            Release {
                version: version.into(),
                url: "https://example.invalid/amux".into(),
                sha256: String::new(),
                size: 1,
            },
        )]
        .into(),
        signature: String::new(),
    }
}

fn answers(out: &[Output]) -> Vec<String> {
    out.iter()
        .filter_map(|output| match output {
            Output::Answer { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

fn dumps(out: &[Output]) -> Vec<&DumpReason> {
    out.iter()
        .filter_map(|output| match output {
            Output::Dump { reason } => Some(reason),
            _ => None,
        })
        .collect()
}

/// A supervisor whose daemon serves on 1.0.0 installs 1.1.0 from the feed.
fn swapped() -> Run {
    let mut run = Run::start(PrevFound::None, None, false);
    run.serves();
    run.at(run.now_ms + 3_600_000, Input::Timer(TimerId::Check));
    run.listed(Listed::Manifest {
        manifest: manifest("1.1.0"),
        host: None,
    });
    run.fetched();
    let old = run
        .out
        .iter()
        .find_map(|output| match output {
            Output::Stop { daemon } => Some(*daemon),
            _ => None,
        })
        .expect("the old daemon was stopped");
    run.on(Input::DaemonExited {
        daemon: old,
        code: 0,
    });
    run
}

#[test]
fn a_crash_after_go_restarts_with_backoff_and_never_rolls_back() {
    let mut run = swapped();
    let daemon = run.serves();
    let out = run.on(Input::DaemonExited { daemon, code: 1 });
    assert!(out.contains(&Output::ArmTimer {
        id: TimerId::Restart,
        at_ms: run.now_ms + 1_000,
    }));
    assert!(
        !out.iter()
            .any(|output| matches!(output, Output::RestorePrev | Output::WriteRejected { .. }))
    );
    let out = run.at(run.now_ms + 1_000, Input::Timer(TimerId::Restart));
    assert!(
        out.iter()
            .any(|output| matches!(output, Output::Spawn { .. }))
    );
}

#[test]
fn backoff_doubles_to_its_cap_and_starts_over_after_a_long_run() {
    let mut run = Run::start(PrevFound::None, None, false);
    let mut delays = Vec::new();
    for _ in 0..7 {
        let daemon = run.serves();
        let out = run.on(Input::DaemonExited { daemon, code: 1 });
        let at = out
            .iter()
            .find_map(|output| match output {
                Output::ArmTimer {
                    id: TimerId::Restart,
                    at_ms,
                } => Some(*at_ms),
                _ => None,
            })
            .expect("a restart");
        delays.push(at - run.now_ms);
        run.at(at, Input::Timer(TimerId::Restart));
    }
    assert_eq!(
        delays,
        vec![1_000, 2_000, 4_000, 8_000, 16_000, 30_000, 30_000]
    );
    let daemon = run.serves();
    let out = run.at(run.now_ms + 60_000, Input::DaemonExited { daemon, code: 1 });
    assert!(out.contains(&Output::ArmTimer {
        id: TimerId::Restart,
        at_ms: run.now_ms + 1_000,
    }));
}

#[test]
fn amux_update_waits_for_a_serving_daemon_and_ignores_the_rejected_build() {
    let mut run = Run::start(PrevFound::None, Some("1.1.0"), false);
    let out = run.on(Input::Asked {
        ask: AskId(1),
        request: Request::Check,
    });
    assert_eq!(
        answers(&out),
        vec!["the daemon is starting; try again shortly"]
    );
    run.serves();
    let out = run.on(Input::Asked {
        ask: AskId(2),
        request: Request::Check,
    });
    assert!(out.contains(&Output::List {
        job: JobId(2),
        auto_only: false,
    }));
    let out = run.listed(Listed::Manifest {
        manifest: manifest("1.1.0"),
        host: None,
    });
    assert!(
        out.iter()
            .any(|output| matches!(output, Output::Fetch { .. })),
        "amux update tries the rejected build again"
    );
    let out = run.fetched();
    assert_eq!(answers(&out), vec!["installing 1.1.0"]);
}

#[test]
fn two_daemons_serving_is_a_violation() {
    let mut run = Run::start(PrevFound::None, None, false);
    run.serves();
    run.core.daemons.push(Running {
        id: DaemonId(99),
        phase: Phase::Serving { since: 0 },
    });
    let mut out = Vec::new();
    run.core.step(Input::Tick { now_ms: 9_000 }, &mut out);
    assert!(
        matches!(dumps(&out).as_slice(), [DumpReason::Violation(violation)]
            if violation.invariant == "at most one daemon is in the serving phase"),
        "{out:?}"
    );
}

#[test]
fn no_binary_that_can_start_is_a_violation() {
    let mut run = swapped();
    run.core.installed.prev = None;
    let mut out = Vec::new();
    run.core.step(Input::Tick { now_ms: 9_999_999 }, &mut out);
    assert!(
        matches!(dumps(&out).as_slice(), [DumpReason::Violation(violation)]
            if violation.invariant == "there is always an installed binary that can start"),
        "{out:?}"
    );
}
