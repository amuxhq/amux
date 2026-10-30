//! A stand-in for the amux binary in the supervisor tests.
//!
//! `fake-amux supervise` runs the real supervisor with short timings, so
//! everything an update does to the binary, the lock and the child is the
//! shipped code. `fake-amux daemon` is a scripted daemon: it takes the
//! activation pipe the way `amux daemon` does and then behaves as the
//! behaviour file for its version says. Every process appends what it did
//! to `$FAKE_AMUX_DIR/events`, one line each: `<what> <version> <pid>`.
//!
//! Behaviour files are `$FAKE_AMUX_DIR/behave-<version>`, holding one of:
//! `prepare` (the default: prepare, wait for go, run until end of file on
//! the pipe or SIGTERM), `exit` (exit before preparing), `hang` (never
//! prepare), `ignore-term` (prepare and then ignore SIGTERM) and
//! `crash-after-go` (exit once after go, rewriting the file to `prepare`).
//!
//! The version is the binary's stamp, so a re-stamped copy is another
//! release.

use std::io::Write as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use node::supervisor::{
    Inherited, Params, StepBudget, SuperviseOptions, UpdatePolicy, UpdateSource,
};

pub const DIR_ENV: &str = "FAKE_AMUX_DIR";
pub const MANIFEST_ENV: &str = "FAKE_AMUX_MANIFEST";
/// The supervisor core's step budget in microseconds, for a test of the
/// budget itself.
pub const STEP_BUDGET_ENV: &str = "FAKE_AMUX_STEP_BUDGET_MICROS";

fn dir() -> PathBuf {
    PathBuf::from(std::env::var_os(DIR_ENV).expect("FAKE_AMUX_DIR is set"))
}

/// A second unless the test says otherwise: these are debug builds run
/// beside every other test binary, where a step the shipped budget allows
/// can wait past it for the processor, and a test of updates is not one of
/// the budget.
fn step_budget() -> StepBudget {
    let micros = std::env::var(STEP_BUDGET_ENV)
        .ok()
        .map_or(1_000_000, |micros| micros.parse().expect("a number of µs"));
    StepBudget { micros }
}

fn event(what: &str) {
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir().join("events"))
        .expect("the events file opens");
    // One write, so a reader never sees half a line.
    let line = format!("{what} {} {}\n", node::version(), std::process::id());
    file.write_all(line.as_bytes())
        .expect("the events file takes a line");
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("daemon") => {
            // SAFETY: no other thread exists yet.
            let pipe = unsafe { node::InheritedPipe::take() }.expect("the pipe is readable");
            daemon(pipe);
        }
        Some("supervise") => {
            let inherit = match args.get(1).map(String::as_str) {
                Some("--inherit") => Some(
                    Inherited::parse(args.get(2).expect("a state")).expect("a handed-over state"),
                ),
                _ => None,
            };
            supervise(inherit);
        }
        other => panic!("fake-amux: unknown mode {other:?}"),
    }
}

fn behaviour() -> String {
    std::fs::read_to_string(dir().join(format!("behave-{}", node::version())))
        .map(|text| text.trim().to_owned())
        .unwrap_or_else(|_| "prepare".to_owned())
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("a runtime")
}

fn daemon(pipe: Option<node::InheritedPipe>) {
    event("start");
    let behaviour = behaviour();
    match behaviour.as_str() {
        "exit" => {
            event("exit");
            std::process::exit(3);
        }
        "hang" => loop {
            std::thread::sleep(Duration::from_secs(3600));
        },
        _ => {}
    }
    runtime().block_on(async move {
        let mut term = Term::new();
        let mut pipe = pipe.map(|pipe| pipe.into_pipe().expect("the pipe opens"));
        if let Some(pipe) = pipe.as_mut() {
            event("prepared");
            if let Err(error) = pipe.activate().await {
                event(&format!("no-go:{}", error.to_string().replace(' ', "_")));
                std::process::exit(5);
            }
        }
        event("go");
        if behaviour == "crash-after-go" {
            let file = dir().join(format!("behave-{}", node::version()));
            std::fs::write(file, "prepare").expect("the behaviour file is writable");
            event("crash");
            std::process::exit(4);
        }
        let closed = async move {
            match pipe {
                Some(pipe) => pipe.closed().await,
                None => std::future::pending().await,
            }
        };
        tokio::pin!(closed);
        loop {
            tokio::select! {
                () = &mut closed => {
                    event("eof");
                    std::process::exit(0);
                }
                () = term.recv() => {
                    if behaviour == "ignore-term" {
                        event("ignored-term");
                        continue;
                    }
                    event("term");
                    std::process::exit(0);
                }
            }
        }
    });
}

/// SIGTERM, the supervisor's stop on Unix. A Windows supervisor stops its
/// child by closing the pipe instead.
#[cfg(unix)]
struct Term(tokio::signal::unix::Signal);

#[cfg(unix)]
impl Term {
    fn new() -> Self {
        use tokio::signal::unix::{SignalKind, signal};
        Self(signal(SignalKind::terminate()).expect("a SIGTERM handler"))
    }

    async fn recv(&mut self) {
        self.0.recv().await;
    }
}

#[cfg(not(unix))]
struct Term;

#[cfg(not(unix))]
impl Term {
    fn new() -> Self {
        Self
    }

    async fn recv(&mut self) {
        std::future::pending().await
    }
}

fn supervise(inherited: Option<Inherited>) {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();
    event(if inherited.is_some() {
        "supervise-inherited"
    } else {
        "supervise"
    });
    let source = std::env::var(MANIFEST_ENV)
        .ok()
        .map(|manifest_url| UpdateSource {
            channel: "stable".to_owned(),
            manifest_url,
            keys: node::release::release_keys().expect("debug builds trust the test key"),
        });
    let updates = Arc::new(move || UpdatePolicy {
        auto: source.is_some(),
        source: source.clone(),
    });
    let options = SuperviseOptions {
        binary: std::env::current_exe().expect("the binary's path"),
        args: Vec::new(),
        data_dir: dir().join("data"),
        running: node::version().parse().expect("a semver stamp"),
        target: node::release::TARGET.to_owned(),
        updates,
        keep_awake: false,
        clock: Arc::new(agent_dir::SystemClock),
        params: Params {
            check_interval: Duration::from_millis(300),
            rollback_after: 3,
            start_deadline: Duration::from_secs(2),
            stop_deadline: Duration::from_secs(1),
            backoff_first: Duration::from_millis(100),
            backoff_max: Duration::from_millis(800),
            backoff_reset: Duration::from_secs(5),
            step_budget: step_budget(),
        },
        inherited,
    };
    let result = runtime().block_on(node::supervisor::supervise(options));
    event("supervisor-exit");
    if let Err(error) = result {
        eprintln!("fake-amux supervise: {error}");
        std::process::exit(1);
    }
}
