//! Unix: SIGINT, SIGTERM and SIGHUP through tokio, listened for by a
//! thread of our own for the rest of the process's life, the first one
//! latched for every call.

use std::io;
use std::sync::OnceLock;
use std::time::Duration;

use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::watch;

use super::Stop;

/// The first stop this process saw, for every call, and to wake the calls
/// already waiting.
fn seen() -> &'static watch::Sender<Option<Stop>> {
    static SEEN: OnceLock<watch::Sender<Option<Stop>>> = OnceLock::new();
    SEEN.get_or_init(|| watch::channel(None).0)
}

fn latch(stop: Stop) {
    seen().send_if_modified(|seen| {
        if seen.is_none() {
            *seen = Some(stop);
            true
        } else {
            false
        }
    });
}

/// The listener, once per process: a thread with a runtime of its own,
/// which holds the three signals' receivers for as long as the process
/// lives and latches what they get. Not a caller's: once tokio has a
/// signal's handler in, the signal no longer ends the process, so a
/// receiver dropped with a caller's future (a `select!` another branch
/// won) would leave a stop that lands before the next call caught by
/// nobody. Returns once the three are registered.
fn listen() -> io::Result<()> {
    static LISTENING: OnceLock<Result<(), (io::ErrorKind, String)>> = OnceLock::new();
    let listening = LISTENING.get_or_init(|| {
        let (ready, registered) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("amux-signals".into())
            .spawn(move || listener(&ready))
            .map_err(|error| {
                (
                    error.kind(),
                    format!("starting the signals' thread: {error}"),
                )
            })?;
        registered
            .recv_timeout(Duration::from_secs(10))
            .map_err(|_| {
                (
                    io::ErrorKind::TimedOut,
                    "the signals weren't registered".to_owned(),
                )
            })?
            .map_err(|error| (error.kind(), format!("registering the signals: {error}")))
    });
    match listening {
        Ok(()) => Ok(()),
        Err((kind, message)) => Err(io::Error::new(*kind, message.clone())),
    }
}

fn listener(ready: &std::sync::mpsc::Sender<io::Result<()>>) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    runtime.block_on(async {
        let registered = (|| {
            Ok::<_, io::Error>((
                signal(SignalKind::terminate())?,
                signal(SignalKind::interrupt())?,
                signal(SignalKind::hangup())?,
            ))
        })();
        let (mut term, mut int, mut hup) = match registered {
            Ok(signals) => signals,
            Err(error) => {
                let _ = ready.send(Err(error));
                return;
            }
        };
        let _ = ready.send(Ok(()));
        loop {
            let stop = tokio::select! {
                _ = int.recv() => Stop::Interrupt,
                _ = term.recv() => Stop::Terminate,
                _ = hup.recv() => Stop::Hangup,
            };
            latch(stop);
        }
    });
}

pub(super) async fn terminated() -> io::Result<Stop> {
    // Listening first, then looking: a stop that lands after the listener
    // registered is latched, whenever it lands.
    listen()?;
    let mut latched = seen().subscribe();
    let stop = latched
        .wait_for(Option::is_some)
        .await
        .map_err(|_| io::Error::other("the stop channel closed"))?;
    Ok(stop.expect("waited for a stop"))
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
