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
    });
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
