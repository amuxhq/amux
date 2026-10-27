//! The agent directory, `agents/<id>/`: the whole contract between an agent
//! process and its daemon.
//!
//! The agent process holds the directory's lock for its lifetime, listens
//! on `ctl.sock` (and `pty.sock` for terminal clients) and appends to
//! `journal/`; the daemon writes `spec.<n>`, listens on `tools.sock`, dials
//! `ctl.sock` and reads the journal. This crate holds what both sides must
//! agree on: the names, the lock, the local sockets both kinds of socket
//! are, the framing on the control socket, and the clock every deadline on
//! either side runs on.

pub mod clock;
pub mod ctl;
pub mod local_socket;

use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::Path;

pub use clock::{Clock, ManualClock, Sleep, SystemClock};
pub use ctl::{MAX_FRAME_BYTES, read_frame, read_message, write_frame, write_message};

/// Held by the agent process for its lifetime.
pub const LOCK: &str = "lock";
/// The agent listens; the daemon is its only client.
pub const CTL_SOCK: &str = "ctl.sock";
/// The agent listens; local terminal clients connect.
pub const PTY_SOCK: &str = "pty.sock";
/// The daemon listens; the agent's tool server is its only client, and the
/// socket it arrives on is its identity.
pub const TOOLS_SOCK: &str = "tools.sock";
/// Journal segments, named by the global offset of their first byte.
pub const JOURNAL: &str = "journal";
/// Raw terminal bytes, named like the journal.
pub const PTY: &str = "pty";
/// The agent's own state; the daemon never reads it.
pub const PRIVATE: &str = "private";
/// What the agent process writes to stderr: the daemon opens it for the
/// process, which has no terminal and no daemon pipe to write to.
pub const AGENT_LOG: &str = "agent.log";
/// Bytes the agent references, named by their hash: attachments, images
/// the model read, and what the agent offers as an encoded Catalogue, whose
/// hash the snapshot names.
pub const BLOBS: &str = "blobs";

/// The directory's exclusive lock, held for the process's lifetime; the
/// kernel releases it when the process dies.
#[derive(Debug)]
pub struct Lock(#[allow(dead_code)] File);

/// Takes `<dir>/lock`, or reports that a live process holds it.
pub fn lock(dir: &Path) -> io::Result<Option<Lock>> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(LOCK))?;
    match file.try_lock() {
        Ok(()) => Ok(Some(Lock(file))),
        Err(fs::TryLockError::WouldBlock) => Ok(None),
        Err(fs::TryLockError::Error(error)) => Err(error),
    }
}

/// Whether a live process holds `<dir>/lock`. Finding out takes the lock
/// and releases it at once, so a process starting at that instant can fail
/// to take it; callers probe only directories whose process they are not
/// starting.
pub fn locked(dir: &Path) -> bool {
    let Ok(file) = OpenOptions::new().write(true).open(dir.join(LOCK)) else {
        // No lock file: no process ever started here.
        return false;
    };
    match file.try_lock() {
        Ok(()) => {
            let _ = file.unlock();
            false
        }
        Err(fs::TryLockError::WouldBlock) => true,
        Err(fs::TryLockError::Error(_)) => false,
    }
}
