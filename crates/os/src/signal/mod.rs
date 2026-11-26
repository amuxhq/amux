//! The OS asking this process to stop.
//!
//! [`terminated`] resolves when it does; the caller runs its clean
//! shutdown, then calls [`finished`]. On Windows the OS ends the process
//! soon after it asks, and how soon depends on what this process tells it
//! meanwhile: [`finished`] is what lets it go.

use std::io;

#[cfg(unix)]
mod unix;
#[cfg(unix)]
use unix as imp;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as imp;

#[cfg(test)]
pub(crate) mod tests;

/// Why the OS asked this process to stop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    /// SIGINT, or Ctrl+C in this process's console.
    Interrupt,
    /// SIGTERM.
    Terminate,
    /// SIGHUP: the terminal went.
    Hangup,
    /// Windows: the console was closed.
    Close,
    /// Windows: the user is logging off.
    Logoff,
    /// Windows: the system is shutting down or restarting.
    Shutdown,
}

/// Resolves when the OS asks this process to stop. Call it from inside a
/// tokio runtime; every call sees the first stop.
///
/// Unix: SIGINT, SIGTERM and SIGHUP.
///
/// Windows: Ctrl+C, and the console's close, logoff and shutdown events,
/// through a console control handler of ours; and, because a process that
/// has loaded user32 no longer gets the console's logoff and shutdown
/// events, `WM_QUERYENDSESSION`/`WM_ENDSESSION` through a hidden top-level
/// window on a thread of its own. When the session is ending the window
/// holds a shutdown block reason, and its `WM_ENDSESSION` (or the console
/// handler, for close, logoff and shutdown) waits for [`finished`], up to
/// [`GRACE`], before letting the OS end the process. A session end the
/// user cancels is no stop. `CTRL_BREAK` is not a stop either: amux never
/// sends it, and it keeps its default meaning.
///
/// Returns an error if it can't register; never waits forever for that.
pub async fn terminated() -> io::Result<Stop> {
    imp::terminated().await
}

/// Starts listening for a stop now, as [`terminated`] does on its first
/// poll, so a stop that lands before the first wait is still seen by it:
/// for a command that must listen before it changes something a stop
/// would have to undo. Returns an error if it can't register.
pub fn listen() -> io::Result<()> {
    imp::listen()
}

/// This process's terminal being resized.
///
/// Unix: SIGWINCH. Windows: [`Resizes::listen`] is `Unsupported`, as a
/// console reports a resize as an input record among the rest of its
/// input, not to the process.
#[derive(Debug)]
pub struct Resizes {
    inner: imp::Resizes,
}

impl Resizes {
    /// Listens from now on: a resize before the first [`Resizes::recv`]
    /// is kept for it. Call it from inside a tokio runtime.
    pub fn listen() -> io::Result<Resizes> {
        imp::Resizes::listen().map(|inner| Resizes { inner })
    }

    /// The next resize (several at once come as one); `None` once no
    /// more can come.
    pub async fn recv(&mut self) -> Option<()> {
        self.inner.recv().await
    }
}

/// Says the clean shutdown that followed [`terminated`] has finished:
/// Windows releases the shutdown block reason and lets the OS end the
/// process. Unix: nothing. Calling it with no stop seen does nothing but
/// mark that a later stop needs no wait.
pub fn finished() {
    imp::finished();
}

/// The longest a stop waits for [`finished`] before the OS may end the
/// process. The OS's own budget is usually shorter (a closed console
/// gives five seconds); this only keeps the wait from being forever.
pub const GRACE: std::time::Duration = std::time::Duration::from_secs(30);

/// Delivering each stop event in-process, for tests: the console control
/// handler is called with each event, and the hidden window is sent
/// `WM_QUERYENDSESSION`/`WM_ENDSESSION`, as the OS would. Nothing logs the
/// session off.
#[cfg(any(test, feature = "testing"))]
pub mod testing {
    use std::io;

    /// What the OS can send.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Event {
        /// Unix SIGINT; Windows `CTRL_C_EVENT` to the console handler.
        Interrupt,
        /// Unix SIGTERM.
        Terminate,
        /// Unix SIGHUP.
        Hangup,
        /// Windows `CTRL_BREAK_EVENT`, which is not a stop.
        ConsoleBreak,
        /// Windows `CTRL_CLOSE_EVENT`.
        ConsoleClose,
        /// Windows `CTRL_LOGOFF_EVENT`.
        ConsoleLogoff,
        /// Windows `CTRL_SHUTDOWN_EVENT`.
        ConsoleShutdown,
        /// Windows `WM_QUERYENDSESSION` then `WM_ENDSESSION`, the session
        /// ending (`ENDSESSION_LOGOFF` when `logoff`).
        SessionEnd { logoff: bool },
        /// Windows `WM_QUERYENDSESSION` then `WM_ENDSESSION` with the end
        /// cancelled.
        SessionEndCancelled,
    }

    /// Delivers `event` as the OS would, registering first as
    /// [`super::terminated`] does. Returns what the handler answered
    /// (`true`: handled). For the events the OS ends a process after,
    /// this returns only once [`super::finished`] is called or
    /// [`super::GRACE`] passes, so call it from a thread of its own.
    /// `Unsupported` for an event this OS has no such thing as.
    ///
    /// Unix: raises the signal in this process. Only after a call of
    /// [`super::terminated`] has been polled is the signal caught.
    pub fn deliver(event: Event) -> io::Result<bool> {
        super::imp::deliver(event)
    }

    /// The shutdown block reason the hidden window holds now (Windows);
    /// always `None` on Unix.
    pub fn block_reason() -> io::Result<Option<String>> {
        super::imp::block_reason()
    }
}
