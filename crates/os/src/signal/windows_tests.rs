//! Windows' stop events: the console's (a real pseudo-console closed, and
//! each control event delivered) and the session's end through the hidden
//! window. What holds on every OS is in `signal/tests.rs`.

use std::time::Duration;

use crate::process::tests::PATH;
use crate::signal::tests::sees;
use crate::signal::{finished, terminated};

/// The roles only Windows has.
pub(crate) fn signal_role(rest: &[&str]) {
    match rest[0] {
        "console-close" => console_close_role(),
        other => panic!("no event {other:?}"),
    }
}

/// Run on a pseudo-console the test closes: says (into the file
/// `OS_TEST_PATH` names, the console being the test's to close) that it
/// listens, then the stop the OS delivered.
fn console_close_role() {
    let report = std::env::var_os(PATH).expect("a path");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let waiting = tokio::spawn(terminated());
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        std::fs::write(&report, "listening\n").unwrap();
        let stop = tokio::time::timeout(Duration::from_secs(30), waiting).await;
        let said = match stop {
            Ok(stop) => format!("stop {:?}\n", stop.unwrap().unwrap()),
            Err(_) => "stop none\n".to_owned(),
        };
        std::fs::write(&report, said).unwrap();
        finished();
    });
}

/// The OS's own delivery: a real console (a pseudo-console, which has no
/// window) is closed under a process listening, and the close reaches it
/// through the handler `terminated` registered.
#[test]
fn terminated_sees_a_real_console_close() {
    use portable_pty::{CommandBuilder, PtySize, native_pty_system};
    let folder = tempfile::tempdir().unwrap();
    let report = folder.path().join("report");
    let pair = native_pty_system()
        .openpty(PtySize::default())
        .expect("a pseudo-console");
    let mut command = CommandBuilder::new(std::env::current_exe().unwrap());
    command.args(["--exact", "process::tests::role", "--nocapture", "--quiet"]);
    command.args(["--test-threads", "1"]);
    command.env("OS_TEST_ROLE", "signal console-close");
    command.env(PATH, &report);
    command.cwd(folder.path());
    let mut child = pair.slave.spawn_command(command).expect("the role starts");
    drop(pair.slave);
    // The console's output must be drained, or closing it can wait on it.
    let mut output = pair.master.try_clone_reader().unwrap();
    let shown = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let showing = shown.clone();
    std::thread::spawn(move || {
        let mut buffer = [0u8; 4096];
        while let Ok(read) = std::io::Read::read(&mut output, &mut buffer) {
            if read == 0 {
                break;
            }
            showing.lock().unwrap().extend_from_slice(&buffer[..read]);
        }
    });
    let shown = move || String::from_utf8_lossy(&shown.lock().unwrap()).into_owned();
    // The console asks where its cursor is (ESC[6n) and waits for the
    // answer a terminal would give.
    let mut input = pair.master.take_writer().unwrap();
    std::io::Write::write_all(&mut input, b"\x1b[1;1R").unwrap();
    let read = |want: &str| {
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        loop {
            let said = std::fs::read_to_string(&report).unwrap_or_default();
            if said.starts_with(want) || std::time::Instant::now() >= deadline {
                return said;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    assert_eq!(
        read("listening"),
        "listening\n",
        "the console showed {}",
        shown()
    );
    // Closing the pseudo-console closes the console the role is on.
    drop(pair.master);
    assert_eq!(
        read("stop "),
        "stop Close\n",
        "the console showed {}",
        shown()
    );
    let _ = child.wait();
}

#[test]
fn terminated_sees_the_console_closing() {
    assert_eq!(sees("close").0, "Close");
}

#[test]
fn terminated_sees_each_console_event() {
    assert_eq!(sees("interrupt").0, "Interrupt");
    assert_eq!(sees("logoff").0, "Logoff");
    assert_eq!(sees("shutdown").0, "Shutdown");
}

#[test]
fn ctrl_break_is_not_a_stop() {
    assert_eq!(sees("break").0, "none");
}

/// The window path: the stop arrives with the session's end, and a block
/// reason is held until the clean shutdown finishes.
#[test]
fn terminated_sees_the_session_end_and_holds_it_while_shutting_down() {
    let (stop, during, after) = sees("session-logoff");
    assert_eq!(stop, "Logoff");
    assert_eq!(during, r#"Some("amux is saving its state")"#);
    assert_eq!(after, "None");
    assert_eq!(sees("session-shutdown").0, "Shutdown");
}

#[test]
fn a_cancelled_session_end_is_no_stop() {
    let (stop, _, after) = sees("session-cancelled");
    assert_eq!(stop, "none");
    assert_eq!(after, "None");
}

/// A console reports a resize among its input: none reaches the process.
#[test]
fn resizes_are_unsupported() {
    let error = crate::signal::Resizes::listen().unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::Unsupported);
}
