//! Each stop event, delivered in a process of its own (the first stop a
//! process sees is the one every later call sees too).

use std::process::Stdio;
use std::time::Duration;

use super::testing::{Event, block_reason, deliver};
use super::{Stop, finished, terminated};
use crate::process::tests::{Driven, helper, say};

fn event(name: &str) -> Event {
    match name {
        "interrupt" => Event::Interrupt,
        "terminate" => Event::Terminate,
        "hangup" => Event::Hangup,
        "break" => Event::ConsoleBreak,
        "close" => Event::ConsoleClose,
        "logoff" => Event::ConsoleLogoff,
        "shutdown" => Event::ConsoleShutdown,
        "session-logoff" => Event::SessionEnd { logoff: true },
        "session-shutdown" => Event::SessionEnd { logoff: false },
        "session-cancelled" => Event::SessionEndCancelled,
        other => panic!("no event {other:?}"),
    }
}

/// Waits for a stop, delivers `rest[0]`, and says the stop, the block
/// reason while the clean shutdown runs and after it, and what the handler
/// answered.
pub(crate) fn signal_role(rest: &[&str]) {
    #[cfg(windows)]
    if rest[0] == "console-close" {
        return console_close_role();
    }
    let event = event(rest[0]);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let waiting = tokio::spawn(terminated());
        // The listener registers on its first poll.
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        let delivering = std::thread::spawn(move || deliver(event));
        match tokio::time::timeout(Duration::from_millis(1500), waiting).await {
            Ok(stop) => {
                let stop: Stop = stop.unwrap().unwrap();
                say(format!("stop {stop:?}"));
                say(format!("during {:?}", block_reason().unwrap()));
                finished();
            }
            Err(_) => say("stop none"),
        }
        let answered = delivering.join().unwrap().unwrap();
        // The window's thread drops the reason once it has the message.
        tokio::time::sleep(Duration::from_millis(200)).await;
        say(format!("after {:?}", block_reason().unwrap()));
        say(format!("answered {answered}"));
        // A call after the stop sees it too, without another.
        match tokio::time::timeout(Duration::from_millis(500), terminated()).await {
            Ok(stop) => say(format!("again {:?}", stop.unwrap())),
            Err(_) => say("again none"),
        }
    });
}

/// Every line a process in the role says for `name`.
fn says(name: &str) -> Vec<String> {
    let mut command = helper(&format!("signal {name}"));
    command.stdin(Stdio::null());
    Driven::start(command).rest()
}

/// Run on a pseudo-console the test closes: says (into the file
/// `OS_TEST_PATH` names, the console being the test's to close) that it
/// listens, then the stop the OS delivered.
#[cfg(windows)]
fn console_close_role() {
    let report = std::env::var_os(crate::process::tests::PATH).expect("a path");
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
#[cfg(windows)]
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
    command.env(crate::process::tests::PATH, &report);
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
    assert_eq!(read("listening"), "listening\n", "the console showed {}", shown());
    // Closing the pseudo-console closes the console the role is on.
    drop(pair.master);
    assert_eq!(read("stop "), "stop Close\n", "the console showed {}", shown());
    let _ = child.wait();
}

#[test]
fn a_later_call_sees_the_first_stop() {
    let said = says("interrupt");
    assert!(said.iter().any(|line| line == "stop Interrupt"), "{said:?}");
    assert!(said.iter().any(|line| line == "again Interrupt"), "{said:?}");
}

/// The stop a process in the role saw for `name`, the block reason during
/// the clean shutdown, and whether one was held after.
fn sees(name: &str) -> (String, String, String) {
    let mut command = helper(&format!("signal {name}"));
    command.stdin(Stdio::null());
    let driven = Driven::start(command);
    let stop = driven.expect("stop");
    let during = if stop == "none" {
        String::new()
    } else {
        driven.expect("during")
    };
    let after = driven.expect("after");
    let _ = driven.expect("answered");
    (stop, during, after)
}

#[cfg(windows)]
#[test]
fn terminated_sees_the_console_closing() {
    assert_eq!(sees("close").0, "Close");
}

#[cfg(windows)]
#[test]
fn terminated_sees_each_console_event() {
    assert_eq!(sees("interrupt").0, "Interrupt");
    assert_eq!(sees("logoff").0, "Logoff");
    assert_eq!(sees("shutdown").0, "Shutdown");
}

#[cfg(windows)]
#[test]
fn ctrl_break_is_not_a_stop() {
    assert_eq!(sees("break").0, "none");
}

/// The window path: the stop arrives with the session's end, and a block
/// reason is held until the clean shutdown finishes.
#[cfg(windows)]
#[test]
fn terminated_sees_the_session_end_and_holds_it_while_shutting_down() {
    let (stop, during, after) = sees("session-logoff");
    assert_eq!(stop, "Logoff");
    assert_eq!(during, r#"Some("amux is saving its state")"#);
    assert_eq!(after, "None");
    assert_eq!(sees("session-shutdown").0, "Shutdown");
}

#[cfg(windows)]
#[test]
fn a_cancelled_session_end_is_no_stop() {
    let (stop, _, after) = sees("session-cancelled");
    assert_eq!(stop, "none");
    assert_eq!(after, "None");
}

#[cfg(unix)]
#[test]
fn terminated_sees_each_signal() {
    assert_eq!(sees("interrupt").0, "Interrupt");
    assert_eq!(sees("terminate").0, "Terminate");
    assert_eq!(sees("hangup").0, "Hangup");
}

#[test]
fn an_event_this_os_lacks_is_unsupported() {
    let missing = if cfg!(windows) {
        Event::Hangup
    } else {
        Event::ConsoleClose
    };
    assert_eq!(
        deliver(missing).unwrap_err().kind(),
        std::io::ErrorKind::Unsupported
    );
}
