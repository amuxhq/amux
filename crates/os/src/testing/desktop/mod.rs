//! What shows on the desktop, for a test that holds amux to showing
//! nothing there: the top-level windows that are visible, and who owns
//! each, beside the processes of the test's own job, so a window can be
//! put down to the test or left to whatever else runs on the desktop.
//!
//! Windows only in substance. A Unix desktop's windows belong to a display
//! server no amux process talks to, and a Unix process is in no job, so
//! both are [`io::ErrorKind::Unsupported`] there.

use std::io;
use std::path::PathBuf;

use crate::Pid;

#[cfg(unix)]
mod unix;
#[cfg(unix)]
use unix as imp;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as imp;

/// A top-level window that shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Window {
    /// The window's handle, as a number: what tells two windows apart
    /// while both exist.
    pub id: usize,
    /// The process whose thread made it.
    pub pid: Pid,
    /// That process's program, when it could be read.
    pub program: Option<PathBuf>,
    /// The window's class, which names the kind of window it is
    /// (`ConsoleWindowClass` for a console).
    pub class: String,
    pub title: String,
}

/// Every top-level window on this thread's desktop that shows, by its own
/// style (`WS_VISIBLE`), in the order the window manager lists them. A
/// process a scheduled task starts has a desktop of its own, which no
/// person sees, so `IsWindowVisible`, which asks the desktop as well, is
/// false there for every window; by its style a window there is one that
/// would show a person on theirs.
pub fn visible_windows() -> io::Result<Vec<Window>> {
    imp::visible_windows()
}

/// Shows a window of this process's titled `title`, one pixel and off
/// every screen, while `while_shown` runs, then destroys it: a window a
/// test knows is there, to see that whatever looks for windows sees one.
pub fn show_a_window(title: &str, while_shown: impl FnOnce()) -> io::Result<()> {
    imp::show_a_window(title, while_shown)
}

/// Every process in the job this process is in (its innermost, when jobs
/// nest), this one among them: what Buck's test runner put the test in
/// and everything it started, which can't leave that job. An error where
/// this process is in no job.
pub fn job_members() -> io::Result<Vec<Pid>> {
    imp::job_members()
}
