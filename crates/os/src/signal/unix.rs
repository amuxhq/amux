//! Unix: SIGINT, SIGTERM and SIGHUP through tokio.

use std::io;

use tokio::signal::unix::{SignalKind, signal};

use super::Stop;

pub(super) async fn terminated() -> io::Result<Stop> {
    let mut term = signal(SignalKind::terminate())?;
    let mut int = signal(SignalKind::interrupt())?;
    let mut hup = signal(SignalKind::hangup())?;
    tokio::select! {
        _ = int.recv() => Ok(Stop::Interrupt),
        _ = term.recv() => Ok(Stop::Terminate),
        _ = hup.recv() => Ok(Stop::Hangup),
    }
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
