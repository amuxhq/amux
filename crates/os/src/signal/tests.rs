//! Each stop event, delivered in a process of its own (the first stop a
//! process sees is the one every later call sees too). What holds on
//! every OS is here; each OS's own events are in its `tests` module beside
//! its code.

use std::process::Stdio;
use std::time::Duration;

use super::testing::{Event, block_reason, deliver};
use super::{Stop, finished, imp, terminated};
use crate::process::tests::{Driven, helper, say};

/// The event a role is named for; `None` for a role of one OS's own.
fn event(name: &str) -> Option<Event> {
    Some(match name {
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
        _ => return None,
    })
}

/// Waits for a stop, delivers `rest[0]`, and says the stop, the block
/// reason while the clean shutdown runs and after it, and what the handler
/// answered.
pub(crate) fn signal_role(rest: &[&str]) {
    let Some(event) = event(rest[0]) else {
        return imp::tests::signal_role(rest);
    };
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

#[test]
fn a_later_call_sees_the_first_stop() {
    let said = says("interrupt");
    assert!(said.iter().any(|line| line == "stop Interrupt"), "{said:?}");
    assert!(
        said.iter().any(|line| line == "again Interrupt"),
        "{said:?}"
    );
}

/// The stop a process in the role saw for `name`, the block reason during
/// the clean shutdown, and whether one was held after.
pub(crate) fn sees(name: &str) -> (String, String, String) {
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
