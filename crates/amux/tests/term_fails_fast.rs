//! A terminal test whose wait fails ends with the failure instead of
//! hanging: the test's process exits even though the program in its
//! terminal is still running.

#![cfg(unix)]

use std::os::unix::process::CommandExt as _;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use support::desk::{Desk, LONG_GRACE_SECS};
use support::term::Term;

use crate::support;

/// Set in the child test process, the only place the failing wait runs.
const FAILING: &str = "AMUX_TEST_TERM_FAILING_WAIT";

/// How long the failing test may take to end before it counts as a hang.
const ENDS_WITHIN: Duration = Duration::from_secs(60);

#[test]
fn a_failing_terminal_wait_ends_its_test() {
    let said = tempfile::NamedTempFile::new().unwrap();
    // One file description for both streams, so neither overwrites the other.
    let output = said.reopen().unwrap();
    let started = Instant::now();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "term_fails_fast::a_terminal_wait_that_fails",
            "--nocapture",
        ])
        .env(FAILING, "1")
        .stdin(Stdio::null())
        .stdout(output.try_clone().unwrap())
        .stderr(output)
        .process_group(0)
        .spawn()
        .expect("the test binary runs");
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if started.elapsed() > ENDS_WITHIN {
            // SAFETY: kill with a negative pid signals the child's own
            // process group, which it leads.
            unsafe { libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL) };
            let _ = child.wait();
            panic!(
                "the failing terminal test was still running after {ENDS_WITHIN:?}; it said:\n{}",
                std::fs::read_to_string(said.path()).unwrap_or_default()
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let said = std::fs::read_to_string(said.path()).unwrap();
    assert!(
        !status.success() && said.contains("waiting for text the fleet never shows"),
        "the terminal wait did not fail as it should: {status}\n{said}"
    );
    println!(
        "the failing terminal test exited with {status} after {:?}; it said:\n{said}",
        started.elapsed()
    );
}

/// Opens the fleet in a terminal and waits for text it never shows. Runs
/// only as the child of the test above, which expects it to fail.
#[tokio::test]
async fn a_terminal_wait_that_fails() {
    if std::env::var_os(FAILING).is_none() {
        return;
    }
    let desk = Desk::new(
        false,
        LONG_GRACE_SECS,
        node::version(),
        "http://127.0.0.1:9/",
        Vec::new(),
    );
    desk.run(&["server", "start"]).await;
    let mut term = Term::run(&desk, &desk.amux_path(), &[], 30, 100);
    term.until_within(
        "text the fleet never shows",
        Duration::from_secs(5),
        |term| term.contents().contains("text the fleet never shows"),
    )
    .await;
}
