//! Unix: SIGINT, SIGTERM and SIGHUP through tokio, the first one latched
//! for every later call.

use std::io;
use std::sync::OnceLock;

use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::watch;

use super::Stop;

/// The first stop this process saw, for every call, and to wake the calls
/// already waiting when another caller's listener got it.
fn seen() -> &'static watch::Sender<Option<Stop>> {
    static SEEN: OnceLock<watch::Sender<Option<Stop>>> = OnceLock::new();
    SEEN.get_or_init(|| watch::channel(None).0)
}

fn latch(stop: Stop) -> Stop {
    let mut first = stop;
    seen().send_if_modified(|seen| match seen {
        Some(earlier) => {
            first = *earlier;
            false
        }
        None => {
            *seen = Some(stop);
            true
        }
    });
    first
}

pub(super) async fn terminated() -> io::Result<Stop> {
    // Listening first, then looking: a stop that lands between the two is
    // either already latched or caught by these.
    let mut term = signal(SignalKind::terminate())?;
    let mut int = signal(SignalKind::interrupt())?;
    let mut hup = signal(SignalKind::hangup())?;
    let mut latched = seen().subscribe();
    let stop = tokio::select! {
        _ = int.recv() => latch(Stop::Interrupt),
        _ = term.recv() => latch(Stop::Terminate),
        _ = hup.recv() => latch(Stop::Hangup),
        stop = latched.wait_for(Option::is_some) => {
            let stop = stop.map_err(|_| io::Error::other("the stop channel closed"))?;
            stop.expect("waited for a stop")
        }
    };
    Ok(stop)
}

pub(super) fn finished() {}

#[cfg(any(test, feature = "testing"))]
pub(super) fn deliver(event: super::testing::Event) -> io::Result<bool> {
    use super::testing::Event;
    let signal = match event {
        Event::Interrupt => libc::SIGINT,
        Event::Terminate => libc::SIGTERM,
        Event::Hangup => libc::SIGHUP,
        _ => return Err(io::Error::from(io::ErrorKind::Unsupported)),
    };
    // SAFETY: raise only sends a signal to this process.
    if unsafe { libc::raise(signal) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(true)
}

#[cfg(any(test, feature = "testing"))]
pub(super) fn block_reason() -> io::Result<Option<String>> {
    Ok(None)
}

#[cfg(test)]
#[path = "unix_tests.rs"]
pub(super) mod tests;
