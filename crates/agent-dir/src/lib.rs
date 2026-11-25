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
use std::time::Duration;

pub use clock::{Clock, ManualClock, Sleep, SystemClock};
pub use ctl::{MAX_FRAME_BYTES, read_frame, read_message, write_frame, write_message};
use os::lock::FileLock;
use os::process::ProcessRef;

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
/// so the pid stays readable while it is held.
#[derive(Debug)]
pub struct Lock(FileLock);

impl Drop for Lock {
    fn drop(&mut self) {
        // The pid goes before the lock does, so a file no process holds
        // names none, and a successor a moment into its start, before it
        // writes its own, is never taken for this process.
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
    // it as one it cannot confirm.
    if let Err(error) = lock.write_data(std::process::id().to_string().as_bytes()) {
        tracing::warn!(dir = %dir.display(), %error, "writing this process's pid into its lock");
    }
    Ok(Some(Lock(lock)))
}

/// Whether a live process holds `<dir>/lock`. Finding out takes the lock
/// and releases it at once, so a process starting at that instant can fail
/// to take it; callers probe only directories whose process they are not
/// starting.
///
/// A probe that fails is taken for a holder: the daemon treats an unlocked
/// directory as one whose process has gone, and a live agent taken for
/// gone would be orphaned, where one taken for live is only dialled.
pub fn locked(dir: &Path) -> bool {
    // No lock file: no process ever started here.
    match FileLock::held(&dir.join(LOCK)) {
        Ok(held) => held,
        Err(error) => {
            tracing::debug!(dir = %dir.display(), %error, "probing an agent's lock failed; taking it for held");
            true
        }
    }
}

/// Who holds a directory's lock, as [`lock_holder`] finds out.
#[derive(Debug)]
pub enum Holder {
    /// No process holds it.
    Free,
    /// The process named in the lock file holds it, pinned so its pid
    /// can't be reused under the caller.
    Process(ProcessRef),
    /// A process holds it, but which one can't be confirmed: an agent a
    /// moment into its start, which has taken the lock and not yet written
    /// its pid or bound its agent socket (or failed to write its pid). Its
    /// file names no pid then, or the one a crashed holder left, which the
    /// OS may since have given to anyone, so a pid is believed only from
    /// the process that serves the directory's agent socket. A later look
    /// names the agent once it has written its pid and serves its socket.
    Unconfirmed,
    /// This process holds it: an agent run inside a test's own process.
    /// Never pinned, so no caller can end itself by ending the holder.
    ThisProcess,
}

/// How often [`lock_holder`] looks again when the holder changed while it
/// looked.
const HOLDER_LOOKS: usize = 3;

/// How long [`lock_holder`] waits for the directory's agent socket to
/// answer: a live agent's answers at once, and a socket that does not is
/// reason enough not to name the holder.
const SOCKET_PATIENCE: Duration = Duration::from_millis(500);

/// Who holds `<dir>/lock`: the lock is held, the file names a pid, the
/// process with that pid is opened, and it is the one serving the
/// directory's agent socket ([`agent_sock`]). The CLI and the test harness
/// find an agent's process this way, to end it.
///
/// Like [`locked`], finding out takes the lock and releases it (twice), so
/// it is for directories whose process the caller is not starting. A pid
/// read is only believed while the lock stays held across the open and the
/// file still names it, so a holder that exits meanwhile is reported
/// [`Holder::Free`], never as whoever has its pid now. That alone is not
/// proof: an agent a moment into its start holds the lock over the pid a
/// crashed holder left, until it writes its own, so the pid is believed
/// only when the operating system names the same process as the agent
/// socket's server, and is otherwise [`Holder::Unconfirmed`].
pub fn lock_holder(dir: &Path) -> io::Result<Holder> {
    let path = dir.join(LOCK);
    for _ in 0..HOLDER_LOOKS {
        if !FileLock::held(&path)? {
            return Ok(Holder::Free);
        }
        let Some(pid) = named_pid(&path) else {
            return Ok(Holder::Unconfirmed);
        };
        if pid == std::process::id() {
            return Ok(Holder::ThisProcess);
        }
        let process = match ProcessRef::open(pid) {
            Ok(process) => process,
            // Gone: the holder exited after the look, or the pid is one a
            // crashed holder left, which its successor has not yet replaced.
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        if !FileLock::held(&path)? {
            return Ok(Holder::Free);
        }
        if named_pid(&path) == Some(pid) {
            return Ok(if agent_socket_server(dir) == Some(pid) {
                Holder::Process(process)
            } else {
                Holder::Unconfirmed
            });
        }
    }
    Ok(if FileLock::held(&path)? {
        Holder::Unconfirmed
    } else {
        Holder::Free
    })
}

/// The process serving `dir`'s agent socket, as the operating system names
/// the socket's server, when it answers within [`SOCKET_PATIENCE`]. The
/// dial is one the agent's tool server could make, closed before it asks
/// anything, which the agent shrugs off. Callers are synchronous and may
/// hold a runtime's only thread, so it runs on a thread and a runtime of
/// its own.
fn agent_socket_server(dir: &Path) -> Option<u32> {
    let socket = agent_sock::path(dir);
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .ok()?;
                runtime.block_on(async {
                    let (stream, pid) =
                        local_socket::connect_to_process_within(&socket, SOCKET_PATIENCE)
                            .await
                            .ok()?;
                    stream.close();
                    pid
                })
            })
            .join()
            .ok()
            .flatten()
    })
}

/// The pid a lock file names, if it can be read and names one.
fn named_pid(path: &Path) -> Option<os::Pid> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead as _, BufReader, Write as _};
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    use super::*;

    /// Names the directory whose lock [`helper_holds_the_lock`] takes.
    const HOLD_ENV: &str = "AGENT_DIR_TEST_HOLD";
    /// Set when the helper is to serve no agent socket.
    const NO_SOCKET_ENV: &str = "AGENT_DIR_TEST_NO_SOCKET";
    /// What it prints once it holds the lock.
    const HELD: &str = "agent-dir-test: held";

    /// Not a test of its own: the process the tests below start, which
    /// takes the lock of the directory [`HOLD_ENV`] names, serves its
    /// agent socket as an agent process does (unless [`NO_SOCKET_ENV`] is
    /// set), says so, and holds both until its input closes. Run without
    /// that variable, it does nothing.
    #[test]
    fn helper_holds_the_lock() {
        let Some(dir) = std::env::var_os(HOLD_ENV) else {
            return;
        };
        let dir = Path::new(&dir);
        let _lock = lock(dir).unwrap().expect("the lock is free");
        let _serving = std::env::var_os(NO_SOCKET_ENV)
            .is_none()
            .then(|| serve_agent_socket(dir));
        println!("{HELD}");
        std::io::stdout().flush().unwrap();
        let _ = io::Read::read_to_end(&mut io::stdin(), &mut Vec::new());
    }

    /// Serves `dir`'s `private/agent.sock` from this process, accepting
    /// and dropping every connection, until the runtime drops.
    fn serve_agent_socket(dir: &Path) -> tokio::runtime::Runtime {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        std::fs::create_dir_all(dir.join(PRIVATE)).unwrap();
        let mut listener = {
            let _entered = runtime.enter();
            local_socket::LocalListener::bind(&agent_sock::path(dir)).unwrap()
        };
        runtime.spawn(async move {
            while let Ok(stream) = listener.accept().await {
                drop(stream);
            }
        });
        runtime
    }

    /// Another process holding `dir`'s lock and serving its agent socket,
    /// once it says it does.
    fn holder(dir: &Path) -> Child {
        holder_serving(dir, true)
    }

    /// Another process holding `dir`'s lock, and serving its agent socket
    /// when `socket`, once it says it does.
    fn holder_serving(dir: &Path, socket: bool) -> Child {
        let mut command = Command::new(std::env::current_exe().unwrap());
        if !socket {
            command.env(NO_SOCKET_ENV, "1");
        }
        let mut child = command
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
        panic!(
            "the helper ended without taking the lock: {:?}",
            child.wait()
        );
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

    #[test]
    fn a_lock_no_process_holds_is_free_whatever_its_file_names() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            matches!(lock_holder(dir.path()).unwrap(), Holder::Free),
            "no file"
        );
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

    /// The pid in a lock file is believed only when the process serving
    /// the directory's agent socket is the same one: a pid a crashed holder
    /// left, which a process outside amux has since taken while a new agent
    /// holds the lock and has not yet written its own, names no process to
    /// end. Here the file names a live process that serves no socket, as
    /// an agent has yet to.
    #[test]
    fn a_holder_that_serves_no_agent_socket_is_unconfirmed() {
        let dir = tempfile::tempdir().unwrap();
        let mut child = holder_serving(dir.path(), false);
        let holder = lock_holder(dir.path()).unwrap();
        drop(child.stdin.take());
        child.wait().unwrap();
        assert!(matches!(holder, Holder::Unconfirmed), "{holder:?}");
    }

    /// And here the socket answers, but another process serves it: the one
    /// the lock file names is not the directory's agent.
    #[test]
    fn a_holder_whose_agent_socket_another_process_serves_is_unconfirmed() {
        let dir = tempfile::tempdir().unwrap();
        let mut child = holder_serving(dir.path(), false);
        let serving = serve_agent_socket(dir.path());
        let holder = lock_holder(dir.path()).unwrap();
        drop(serving);
        drop(child.stdin.take());
        child.wait().unwrap();
        assert!(matches!(holder, Holder::Unconfirmed), "{holder:?}");
    }

    /// A holder that has taken the lock and not yet written its pid: held,
    /// by no process this can confirm.
    #[test]
    fn a_holder_that_names_no_pid_yet_is_held_but_unconfirmed() {
        let dir = tempfile::tempdir().unwrap();
        let starting = FileLock::try_acquire(&dir.path().join(LOCK))
            .unwrap()
            .expect("free");
        assert!(locked(dir.path()));
        assert!(matches!(
            lock_holder(dir.path()).unwrap(),
            Holder::Unconfirmed
        ));
        drop(starting);
        assert!(matches!(lock_holder(dir.path()).unwrap(), Holder::Free));
    }

    /// A probe that fails says nothing of the holder, so it is never
    /// taken for a free lock: here the lock is a folder, which no probe can
    /// lock, as an NFS mount refuses a probe it cannot honour.
    #[test]
    fn a_lock_that_cannot_be_probed_is_taken_for_held() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(LOCK)).unwrap();
        assert!(locked(dir.path()));
        assert!(lock_holder(dir.path()).is_err());
    }

    #[test]
    fn a_lock_this_process_holds_is_never_pinned() {
        let dir = tempfile::tempdir().unwrap();
        let held = lock(dir.path()).unwrap().expect("free");
        assert!(lock(dir.path()).unwrap().is_none());
        assert!(matches!(
            lock_holder(dir.path()).unwrap(),
            Holder::ThisProcess
        ));
        drop(held);
        assert!(matches!(lock_holder(dir.path()).unwrap(), Holder::Free));
    }
}
