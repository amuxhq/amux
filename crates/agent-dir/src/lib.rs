//! The agent directory, `agents/<id>/`: the whole contract between an agent
//! process and its daemon.
//!
//! The agent process holds the directory's lock for its lifetime, listens
//! on `ctl.sock` (and `pty.sock` for terminal clients) and appends to
//! `journal/`; the daemon writes `spec.<n>`, listens on `tools.sock`, dials
//! `ctl.sock` and reads the journal. This crate holds what both sides must
//! agree on: the names, the lock, the local sockets both kinds of socket
//! are, the framing on the control socket, and the clock every deadline on
//! either side runs on. The agent's tool server is a third process that
//! works in the directory; how it reaches its agent process on
//! `private/agent.sock` is here too ([`agent_sock`]).

pub mod agent_sock;
pub mod clock;
pub mod ctl;
pub mod local_socket;

use std::io;
use std::path::Path;

use os::lock::FileLock;
use os::process::ProcessRef;

pub use clock::{Clock, ManualClock, Sleep, SystemClock};
pub use ctl::{MAX_FRAME_BYTES, read_frame, read_message, write_frame, write_message};

/// Held by the agent process for its lifetime; its data is the holder's
/// pid ([`Lock`]).
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
/// The agent listens, under `private/`; its tool server is the only
/// client.
pub const AGENT_SOCK: &str = "agent.sock";
/// What the agent process writes to stderr: the daemon opens it for the
/// process, which has no terminal and no daemon pipe to write to.
pub const AGENT_LOG: &str = "agent.log";
/// Bytes the agent references, named by their hash: attachments, images
/// the model read, and what the agent offers as an encoded Catalogue, whose
/// hash the snapshot names.
pub const BLOBS: &str = "blobs";
/// The directory's exclusive lock, held for the process's lifetime; the
/// kernel releases it when the process dies.
///
/// The lock file's data is the holder's pid, in decimal, so another
/// process can find the holder ([`lock_holder`]). The lock is an
/// [`os::lock::FileLock`], which on Windows locks one byte past any data,
/// so the pid stays readable while it is held; that byte lies inside the
/// range an older build's whole-file lock covers, so old and new builds
/// exclude each other. An older build's agent writes no pid.
#[derive(Debug)]
pub struct Lock(FileLock);

impl Drop for Lock {
    fn drop(&mut self) {
        // The pid goes before the lock does, so a later holder of an older
        // build, which writes none, is never taken for this process.
        let _ = self.0.write_data(b"");
    }
}

/// Takes `<dir>/lock` and names this process as its holder, or reports
/// that a live process holds it.
pub fn lock(dir: &Path) -> io::Result<Option<Lock>> {
    let Some(mut lock) = FileLock::try_acquire(&dir.join(LOCK))? else {
        return Ok(None);
    };
    // A holder that can't be named is still the holder: lock_holder reports
    // it as one that names no process, as it does an older build's.
    if let Err(error) = lock.write_data(std::process::id().to_string().as_bytes()) {
        tracing::warn!(dir = %dir.display(), %error, "writing this process's pid into its lock");
    }
    Ok(Some(Lock(lock)))
}

/// Whether a live process holds `<dir>/lock`. Finding out takes the lock
/// and releases it at once, so a process starting at that instant can fail
/// to take it; callers probe only directories whose process they are not
/// starting.
pub fn locked(dir: &Path) -> bool {
    // No lock file: no process ever started here.
    FileLock::held(&dir.join(LOCK)).unwrap_or(false)
}

/// Who holds a directory's lock, as [`lock_holder`] finds out.
#[derive(Debug)]
pub enum Holder {
    /// No process holds it.
    Free,
    /// The process named in the lock file holds it, pinned so its pid
    /// can't be reused under the caller.
    Process(ProcessRef),
    /// A process holds it but the file names none that could be pinned:
    /// an older build's agent, which writes no pid (on Windows its
    /// whole-file lock keeps the file from being read at all), or one that
    /// has taken the lock and not yet written its pid.
    Unnamed,
    /// This process holds it: an agent run inside a test's own process.
    /// Never pinned, so no caller can end itself by ending the holder.
    ThisProcess,
}

/// How often [`lock_holder`] looks again when the holder changed while it
/// looked.
const HOLDER_LOOKS: usize = 3;

/// Who holds `<dir>/lock`: the lock is held, the file names a pid, and the
/// process with that pid is opened. The daemon's adoption fallback, the
/// CLI and the test harness find an agent's process this way.
///
/// Like [`locked`], finding out takes the lock and releases it (twice), so
/// it is for directories whose process the caller is not starting. A pid
/// read is only believed while the lock stays held across the open and the
/// file still names it, so a holder that exits meanwhile is reported
/// [`Holder::Free`], never as whoever has its pid now.
pub fn lock_holder(dir: &Path) -> io::Result<Holder> {
    let path = dir.join(LOCK);
    for _ in 0..HOLDER_LOOKS {
        if !FileLock::held(&path)? {
            return Ok(Holder::Free);
        }
        let Some(pid) = named_pid(&path) else {
            return Ok(Holder::Unnamed);
        };
        if pid == std::process::id() {
            return Ok(Holder::ThisProcess);
        }
        let process = match ProcessRef::open(pid) {
            Ok(process) => process,
            // Gone: the holder exited after the look, or the pid is one an
            // older build's holder left in place.
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        if !FileLock::held(&path)? {
            return Ok(Holder::Free);
        }
        if named_pid(&path) == Some(pid) {
            return Ok(Holder::Process(process));
        }
    }
    Ok(if FileLock::held(&path)? {
        Holder::Unnamed
    } else {
        Holder::Free
    })
}

/// The pid a lock file names, if it can be read and names one.
fn named_pid(path: &Path) -> Option<os::Pid> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use std::fs::{File, OpenOptions, TryLockError};
    use std::io::{BufRead as _, BufReader, Write as _};
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    use super::*;

    /// Names the directory whose lock [`helper_holds_the_lock`] takes.
    const HOLD_ENV: &str = "AGENT_DIR_TEST_HOLD";
    /// What it prints once it holds the lock.
    const HELD: &str = "agent-dir-test: held";

    /// Not a test of its own: the process the tests below start, which
    /// takes the lock of the directory [`HOLD_ENV`] names, says so, and
    /// holds it until its input closes. Run without that variable, it
    /// does nothing.
    #[test]
    fn helper_holds_the_lock() {
        let Some(dir) = std::env::var_os(HOLD_ENV) else {
            return;
        };
        let _lock = lock(Path::new(&dir)).unwrap().expect("the lock is free");
        println!("{HELD}");
        std::io::stdout().flush().unwrap();
        let _ = io::Read::read_to_end(&mut io::stdin(), &mut Vec::new());
    }

    /// Another process holding `dir`'s lock, once it says it does.
    fn holder(dir: &Path) -> Child {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "tests::helper_holds_the_lock", "--nocapture"])
            .env(HOLD_ENV, dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut out = BufReader::new(child.stdout.take().unwrap()).lines();
        loop {
            match out.next() {
                Some(Ok(line)) if line.trim() == HELD => {
                    // Read the rest, so what the helper prints as it ends
                    // doesn't fail on a closed pipe.
                    std::thread::spawn(move || out.for_each(drop));
                    return child;
                }
                Some(Ok(_)) => {}
                Some(Err(error)) => panic!("reading the helper: {error}"),
                None => break,
            }
        }
        panic!("the helper ended without taking the lock: {:?}", child.wait());
    }

    /// Waits for the lock to come free, as it does once its holder has
    /// gone.
    fn until_free(dir: &Path) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match lock_holder(dir).unwrap() {
                Holder::Free => return,
                held if Instant::now() > deadline => panic!("still held: {held:?}"),
                _ => std::thread::sleep(Duration::from_millis(20)),
            }
        }
    }

    /// What a build before the pid did: std's whole-file lock, writing
    /// nothing.
    fn old_style_lock(dir: &Path) -> File {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join(LOCK))
            .unwrap();
        match file.try_lock() {
            Ok(()) => file,
            Err(TryLockError::WouldBlock) => panic!("already held"),
            Err(TryLockError::Error(error)) => panic!("{error}"),
        }
    }

    #[test]
    fn a_lock_no_process_holds_is_free_whatever_its_file_names() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(lock_holder(dir.path()).unwrap(), Holder::Free), "no file");
        // What a holder that crashed leaves: its pid, now anyone's.
        std::fs::write(dir.path().join(LOCK), std::process::id().to_string()).unwrap();
        assert!(matches!(lock_holder(dir.path()).unwrap(), Holder::Free));
        assert!(!locked(dir.path()));
    }

    #[test]
    fn a_live_holder_is_pinned_by_the_pid_its_lock_names_and_its_death_frees_the_lock() {
        let dir = tempfile::tempdir().unwrap();
        let mut child = holder(dir.path());
        assert!(locked(dir.path()));
        let Holder::Process(process) = lock_holder(dir.path()).unwrap() else {
            panic!("the helper holds the lock");
        };
        assert_eq!(process.pid(), child.id());
        assert!(process.alive());
        // Killed outright, as the test net ends an agent: the kernel lets
        // go of the lock, and its pid is still in the file.
        process.kill().unwrap();
        child.wait().unwrap();
        assert!(!process.alive());
        until_free(dir.path());
        assert_eq!(
            std::fs::read_to_string(dir.path().join(LOCK)).unwrap(),
            child.id().to_string()
        );
    }

    #[test]
    fn a_holder_that_ends_cleanly_takes_its_pid_with_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut child = holder(dir.path());
        drop(child.stdin.take());
        assert!(child.wait().unwrap().success());
        until_free(dir.path());
        assert_eq!(std::fs::read_to_string(dir.path().join(LOCK)).unwrap(), "");
    }

    /// An older build's agent holds the lock with std's whole-file lock and
    /// writes no pid: held, by no process this can name.
    #[test]
    fn an_older_builds_holder_is_held_but_unnamed() {
        let dir = tempfile::tempdir().unwrap();
        let old = old_style_lock(dir.path());
        assert!(locked(dir.path()));
        assert!(matches!(lock_holder(dir.path()).unwrap(), Holder::Unnamed));
        assert!(lock(dir.path()).unwrap().is_none(), "the two exclude each other");
        old.unlock().unwrap();
        drop(old);
        assert!(matches!(lock_holder(dir.path()).unwrap(), Holder::Free));
    }

    #[test]
    fn a_lock_this_process_holds_is_never_pinned() {
        let dir = tempfile::tempdir().unwrap();
        let held = lock(dir.path()).unwrap().expect("free");
        assert!(lock(dir.path()).unwrap().is_none());
        assert!(matches!(lock_holder(dir.path()).unwrap(), Holder::ThisProcess));
        drop(held);
        assert!(matches!(lock_holder(dir.path()).unwrap(), Holder::Free));
    }
}
