//! Local sockets named by a path in the agent directory: Unix domain
//! sockets on Unix, named pipes on Windows.
//!
//! A socket's name is its path (`agents/<id>/ctl.sock`); what the operating
//! system binds is derived from it. On Unix that is the path itself unless
//! it is too long for `sockaddr_un`, which agent directories under a deep
//! data directory easily are; then the socket is reached through a short
//! symbolic link to its directory in a per-user runtime directory, so the
//! file still lives at its path. On Windows the file at the path names a
//! pipe that each bind makes afresh: a pipe lives on while any connection to
//! it is open, so a successor could not take its predecessor's name, just as
//! a Unix bind replaces the file and leaves old connections to finish.
//!
//! What either end may rely on, on every OS:
//!
//! - **Binding replaces, unbinding is careful.** A bind replaces what a
//!   dead predecessor left at the path; dropping the listener removes the
//!   file only while it is still this listener's, so a successor that bound
//!   the path since keeps it.
//! - **Accepting never ends.** [`LocalListener::accept`] does not fail: an
//!   error accepting, or making the next pipe instance, is logged and
//!   retried with backoff, and a connection already made is never lost.
//! - **Connecting has a deadline.** [`connect`] gives up after
//!   [`CONNECT_PATIENCE`]; [`connect_within`] takes the caller's.
//! - **Closing is closing.** [`LocalStream::close`] (or a [`Closer`] taken
//!   before the stream was split) ends the connection outright: the peer
//!   reads end of file and this end's reader and writer stop, whoever holds
//!   them. Shutting down a write half is not that on Windows, where it does
//!   nothing.
//!
//! On Windows, also: a pipe's name is `\\.\pipe\amux-` and 32 random hex
//! digits, and a client opens nothing else, whatever the file says; the
//! pipe admits its user alone, local clients only; and a client checks that
//! the pipe it opened is owned by its own user before it says a word.

use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

#[cfg(unix)]
mod unix;
#[cfg(unix)]
use unix as imp;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as imp;

#[cfg(test)]
mod tests;

/// How long [`connect`] keeps trying a socket whose server is busy or
/// between binds before it gives up.
pub const CONNECT_PATIENCE: Duration = Duration::from_secs(2);

/// Listens on the socket named by a path; removes it when dropped, unless
/// a successor has bound the path since.
pub struct LocalListener {
    path: PathBuf,
    inner: imp::Listener,
}

impl LocalListener {
    /// Binds `path`, replacing a socket a dead predecessor left behind. The
    /// caller holds the directory's lock, so nothing live owns it.
    pub fn bind(path: &Path) -> io::Result<Self> {
        let path = std::path::absolute(path)?;
        Ok(Self {
            inner: imp::Listener::bind(&path)?,
            path,
        })
    }

    /// The next connection. It never fails: what goes wrong accepting is
    /// logged and retried with backoff, so a loop over it runs for the
    /// listener's life. (It returns a `Result` so callers written for one
    /// that could fail keep compiling.)
    pub async fn accept(&mut self) -> io::Result<LocalStream> {
        Ok(LocalStream::new(self.inner.accept().await))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for LocalListener {
    fn drop(&mut self) {
        self.inner.unbind(&self.path);
    }
}

/// Connects to the socket named by `path`, trying for at most
/// [`CONNECT_PATIENCE`].
pub async fn connect(path: &Path) -> io::Result<LocalStream> {
    connect_within(path, CONNECT_PATIENCE).await
}

/// Connects to the socket named by `path`, trying for at most `patience`.
///
/// No socket at the path is an error at once, as is (on Windows) a file
/// naming anything but an amux pipe, or a pipe owned by another user. A
/// server whose every pipe instance is taken, or a file naming a pipe that
/// is gone (a successor is binding), is tried again, the file read afresh
/// each time, until `patience` runs out: then [`io::ErrorKind::TimedOut`]
/// for a busy server, [`io::ErrorKind::ConnectionRefused`] for a gone one.
pub async fn connect_within(path: &Path, patience: Duration) -> io::Result<LocalStream> {
    let (stream, _) = imp::connect(&std::path::absolute(path)?, patience).await?;
    Ok(LocalStream::new(stream))
}

/// Connects to the socket named by `path`, with the id of the process
/// that answered, as the operating system names it, when it can.
///
/// The process on the other end is the one serving the socket, whoever
/// started it, so a daemon that adopted an agent from an earlier daemon
/// still knows which process to end when the agent does not answer.
pub async fn connect_to_process(path: &Path) -> io::Result<(LocalStream, Option<u32>)> {
    let (stream, pid) = imp::connect(&std::path::absolute(path)?, CONNECT_PATIENCE).await?;
    Ok((LocalStream::new(stream), pid))
}

/// The path a Unix socket named `path` is bound and dialled at: the path
/// itself, or one through a short link when it is too long.
#[cfg(unix)]
pub fn unix_address(path: &Path) -> io::Result<PathBuf> {
    imp::address(&std::path::absolute(path)?)
}

/// The path to hand a program that binds the socket itself and insists its
/// directory be a real, private one (terminal Claude's messaging socket):
/// the path itself when it fits, else the same name in a real directory,
/// only this user's, in the per-user runtime directory. Nothing but the
/// program's own announcement of it leads there, so it needs no link.
#[cfg(unix)]
pub fn unix_private_address(path: &Path) -> io::Result<PathBuf> {
    imp::private_address(&std::path::absolute(path)?)
}

/// Removes the short addresses made for sockets in `dir` and its private
/// directory, which go with the directory itself: the link to each, and
/// the real directory a program binding its own socket was given. Nothing
/// to do on Windows, where a pipe ends with its last handle.
pub fn forget_short_addresses(dir: &Path) {
    if let Ok(dir) = std::path::absolute(dir) {
        imp::forget(&dir);
        imp::forget(&dir.join(crate::PRIVATE));
    }
}

// The connection --------------------------------------------------------------

/// A connected local socket, either end.
///
/// Read and write it whole, or split it (`tokio::io::split`); either way
/// [`close`](Self::close) or a [`Closer`] ends it outright.
pub struct LocalStream {
    shared: Arc<Shared>,
}

/// Ends a [`LocalStream`] from wherever its owner keeps this: beside a
/// write half whose read half another task holds, say. It does not keep
/// the connection open: once the stream (both halves, if split) is
/// dropped, closing is nothing to do.
#[derive(Clone)]
pub struct Closer(Weak<Shared>);

struct Shared(Mutex<State>);

struct State {
    /// None once closed.
    stream: Option<imp::Stream>,
    /// Who waits on a read or a write that the stream itself would have
    /// woken: a close drops the stream, and with it those wake-ups, so it
    /// wakes them itself.
    reading: Option<Waker>,
    writing: Option<Waker>,
}

impl LocalStream {
    fn new(stream: imp::Stream) -> Self {
        Self {
            shared: Arc::new(Shared(Mutex::new(State {
                stream: Some(stream),
                reading: None,
                writing: None,
            }))),
        }
    }

    /// A handle that closes this stream, kept apart from it.
    pub fn closer(&self) -> Closer {
        Closer(Arc::downgrade(&self.shared))
    }

    /// Closes the connection outright: the peer reads end of file, and
    /// nothing more is read or written here. What was written and the peer
    /// had room for still arrives first; a write still waiting for room is
    /// abandoned. Unix: `shutdown(Both)`, then the socket closed. Windows:
    /// the pipe's pending operations cancelled, then its handle closed.
    pub fn close(self) {
        self.shared.close();
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.shared.state()
    }
}

impl Closer {
    /// Closes the stream as [`LocalStream::close`] does, though its halves
    /// are held elsewhere: a read or write waiting on it ends at once, a
    /// read with end of file and a write with [`io::ErrorKind::BrokenPipe`].
    /// Closing a closed or dropped stream does nothing.
    pub fn close(&self) {
        if let Some(shared) = self.0.upgrade() {
            shared.close();
        }
    }
}

impl Shared {
    fn state(&self) -> MutexGuard<'_, State> {
        // A poll that panicked left the state whole: each step is one
        // assignment.
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn close(&self) {
        let (stream, reading, writing) = {
            let mut state = self.state();
            (
                state.stream.take(),
                state.reading.take(),
                state.writing.take(),
            )
        };
        if let Some(stream) = stream {
            imp::close(stream);
        }
        for waiting in [reading, writing].into_iter().flatten() {
            waiting.wake();
        }
    }
}

fn closed() -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, "the connection was closed")
}

/// Keeps the waker of a poll that waits, so a close can wake it.
fn remember<T>(poll: Poll<T>, slot: &mut Option<Waker>, cx: &Context<'_>) -> Poll<T> {
    match poll {
        Poll::Pending => {
            match slot {
                Some(waker) if waker.will_wake(cx.waker()) => {}
                _ => *slot = Some(cx.waker().clone()),
            }
            Poll::Pending
        }
        ready => ready,
    }
}

impl AsyncRead for LocalStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let mut state = self.state();
        let state = &mut *state;
        match state.stream.as_mut() {
            // Closed: end of file.
            None => Poll::Ready(Ok(())),
            Some(stream) => remember(Pin::new(stream).poll_read(cx, buf), &mut state.reading, cx),
        }
    }
}

impl AsyncWrite for LocalStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let mut state = self.state();
        let state = &mut *state;
        match state.stream.as_mut() {
            None => Poll::Ready(Err(closed())),
            Some(stream) => remember(Pin::new(stream).poll_write(cx, buf), &mut state.writing, cx),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let mut state = self.state();
        let state = &mut *state;
        match state.stream.as_mut() {
            None => Poll::Ready(Ok(())),
            Some(stream) => remember(Pin::new(stream).poll_flush(cx), &mut state.writing, cx),
        }
    }

    /// Shuts down the write direction, as the socket does: on Windows that
    /// is nothing at all. To end a connection, [`LocalStream::close`] it.
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let mut state = self.state();
        let state = &mut *state;
        match state.stream.as_mut() {
            None => Poll::Ready(Ok(())),
            Some(stream) => remember(Pin::new(stream).poll_shutdown(cx), &mut state.writing, cx),
        }
    }
}

// Patience ----------------------------------------------------------------------

/// Doubling pauses between retries, from a few milliseconds to a second.
struct Backoff {
    next: Duration,
}

impl Backoff {
    const FIRST: Duration = Duration::from_millis(10);
    const MOST: Duration = Duration::from_secs(1);

    fn new() -> Self {
        Self { next: Self::FIRST }
    }

    fn pause(&mut self) -> Duration {
        let pause = self.next;
        self.next = (self.next * 2).min(Self::MOST);
        pause
    }

    fn reset(&mut self) {
        self.next = Self::FIRST;
    }
}
