//! The supervisor's side of its child: spawning `amux daemon` with the
//! activation pipe, waiting for it, stopping it, and handing all of it to
//! the next supervisor across an exec (Unix) or a spawn-then-exit
//! (Windows).
//!
//! The pipe is two anonymous pipes, one each way, named to the child in
//! [`PIPE_ENV`] as `<read>,<write>`: descriptors on Unix, handle values on
//! Windows. The child reads `go` from the first and writes `prepared` to
//! the second.

use std::fs::File;
use std::io::{self, BufRead as _, BufReader, Write as _};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;

use os::process::{ProcessRef, raw_for_child, set_inheritable};
use tokio::sync::{mpsc, watch};

use crate::activation::PIPE_ENV;

/// What a supervisor hands the binary it replaces itself with: its child,
/// both ends of the pipe it keeps, its lock, and on Windows the child's
/// process handle.
///
/// As `--inherit`'s argument it is `<pid>:<go>:<prepared>:<lock>`, then
/// `:<process>` where a handle is handed over. A Unix supervisor hands
/// over no handle, so it writes the four fields every release reads, and
/// may exec into whichever release it installed or rolled back to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Inherited {
    pub pid: u32,
    pub go: u64,
    pub prepared: u64,
    /// The lock's descriptor; None where the lock cannot cross (Windows,
    /// where a lock goes with the process that took it), and the next
    /// supervisor takes it again.
    pub lock: Option<u64>,
    /// The child's process handle (Windows), which keeps its pid from being
    /// reused until the next supervisor holds it; None on Unix, where the
    /// child stays the exec'd image's own.
    pub process: Option<u64>,
}

impl Inherited {
    pub fn to_arg(&self) -> String {
        let lock = self
            .lock
            .map_or_else(|| "-".to_owned(), |lock| lock.to_string());
        let mut arg = format!("{}:{}:{}:{lock}", self.pid, self.go, self.prepared);
        if let Some(process) = self.process {
            arg.push_str(&format!(":{process}"));
        }
        arg
    }

    pub fn parse(text: &str) -> Option<Self> {
        let mut parts = text.split(':');
        let pid = parts.next()?.parse().ok()?;
        let go = parts.next()?.parse().ok()?;
        let prepared = parts.next()?.parse().ok()?;
        let lock = match parts.next()? {
            "-" => None,
            lock => Some(lock.parse().ok()?),
        };
        let process = match parts.next() {
            None => None,
            Some(process) => Some(process.parse().ok()?),
        };
        parts.next().is_none().then_some(Self {
            pid,
            go,
            prepared,
            lock,
            process,
        })
    }
}

/// How a child ended: its exit code, or on Unix the negated signal.
pub type ExitCode = i32;

pub struct Child {
    pub pid: u32,
    /// Pinned, so its pid is never reused under a kill.
    process: Arc<ProcessRef>,
    exit: watch::Receiver<Option<ExitCode>>,
    go: Option<File>,
    /// Kept open so the next supervisor inherits it; a duplicate feeds the
    /// reader.
    prepared_end: File,
    prepared: Option<mpsc::UnboundedReceiver<()>>,
}

/// What [`Child::hand_over`] made inheritable, kept until the successor has
/// started.
pub struct HandedOver {
    pub inherited: Inherited,
    /// The process handle handed over (Windows): a duplicate, closed here
    /// once the successor holds its own.
    _process: Option<File>,
}

impl Child {
    /// Starts `<binary> <args> daemon` with the pipe, in the background in
    /// `root`.
    pub fn spawn(binary: &Path, args: &[std::ffi::OsString], root: &Path) -> io::Result<Self> {
        let (child_reads, go) = os::process::pipe()?;
        let (prepared_end, child_writes) = os::process::pipe()?;
        let mut command = Command::new(binary);
        os::process::background(&mut command, os::process::Detach::Session, root);
        command
            .args(args)
            .arg("daemon")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            // The supervisor's own standard error, its log: where the
            // daemon's early errors go.
            .stderr(Stdio::inherit())
            .env(
                PIPE_ENV,
                format!(
                    "{},{}",
                    raw_for_child(&child_reads),
                    raw_for_child(&child_writes)
                ),
            );
        // The child's two ends are inheritable for this one spawn, from the
        // one thread the supervisor starts processes on, and closed here
        // right after it.
        set_inheritable(&child_reads, true)?;
        set_inheritable(&child_writes, true)?;
        let child = start(&mut command)?;
        drop((child_reads, child_writes));
        let pid = child.id();
        let process = Arc::new(ProcessRef::of_std_child(&child)?);
        let reader = prepared_end.try_clone()?;
        Ok(Self {
            pid,
            exit: wait(Arc::clone(&process)),
            process,
            go: Some(go),
            prepared_end,
            prepared: Some(read_prepared(reader)),
        })
    }

    /// Takes over a child an earlier supervisor started and activated.
    pub fn inherit(inherited: &Inherited) -> io::Result<Self> {
        // SAFETY: the supervisor this one replaced handed these over in
        // `--inherit`, which is taken once, as the supervisor starts, and
        // nothing else in this process owns them.
        let (go, prepared_end, process) = unsafe {
            (
                os::process::file_from_inherited(inherited.go)?,
                os::process::file_from_inherited(inherited.prepared)?,
                ProcessRef::inherited(inherited.pid, inherited.process)?,
            )
        };
        let process = Arc::new(process);
        Ok(Self {
            pid: inherited.pid,
            exit: wait(Arc::clone(&process)),
            process,
            go: Some(go),
            prepared_end,
            prepared: None,
        })
    }

    /// Resolves when the child writes `prepared`; never, once it cannot.
    pub async fn prepared(&mut self) {
        if let Some(prepared) = self.prepared.as_mut()
            && prepared.recv().await.is_some()
        {
            return;
        }
        self.prepared = None;
        std::future::pending().await
    }

    /// Resolves when the child has exited, with how.
    pub fn exited(&self) -> impl Future<Output = ExitCode> + Send + 'static {
        let mut exit = self.exit.clone();
        async move {
            match exit.wait_for(Option::is_some).await {
                Ok(code) => code.unwrap_or(-1),
                // The waiter is gone without a status: the child cannot be
                // waited for, which only happens when it is not ours.
                Err(_) => -1,
            }
        }
    }

    pub fn has_exited(&self) -> bool {
        self.exit.borrow().is_some()
    }

    /// Answers `prepared`.
    pub fn go(&mut self) -> io::Result<()> {
        let go = self
            .go
            .as_mut()
            .ok_or_else(|| io::Error::other("the pipe is closed"))?;
        go.write_all(format!("{}\n", crate::activation::GO).as_bytes())?;
        go.flush()
    }

    /// Asks the child to stop: SIGTERM on Unix; on Windows, which has no
    /// signals, closing the pipe, whose end of file the daemon exits on.
    pub fn ask_to_stop(&mut self) {
        if self.has_exited() {
            return;
        }
        match self.process.ask_to_stop() {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::Unsupported => drop(self.go.take()),
            Err(error) => {
                tracing::warn!(%error, pid = self.pid, "could not ask the daemon to stop");
            }
        }
    }

    pub fn kill(&mut self) {
        if self.has_exited() {
            return;
        }
        if let Err(error) = self.process.kill() {
            tracing::warn!(%error, pid = self.pid, "could not kill the daemon");
        }
    }

    /// The state the next supervisor needs, with every handle in it made
    /// inheritable: the pipe's ends, the child's process handle where it
    /// has one (Windows), and otherwise the lock, which goes with its
    /// descriptor across the exec.
    pub fn hand_over(&self, lock: &File) -> io::Result<HandedOver> {
        let go = self
            .go
            .as_ref()
            .ok_or_else(|| io::Error::other("the pipe is closed"))?;
        set_inheritable(go, true)?;
        set_inheritable(&self.prepared_end, true)?;
        let process = self.process.for_successor()?;
        // A lock goes with the process that took it on Windows, the one
        // OS that hands over a process handle, so it is taken again there.
        let lock = match &process {
            Some(process) => {
                set_inheritable(process, true)?;
                None
            }
            None => {
                set_inheritable(lock, true)?;
                Some(raw_for_child(lock))
            }
        };
        Ok(HandedOver {
            inherited: Inherited {
                pid: self.pid,
                go: raw_for_child(go),
                prepared: raw_for_child(&self.prepared_end),
                lock,
                process: process.as_ref().map(raw_for_child),
            },
            _process: process,
        })
    }

    /// Undoes [`Child::hand_over`] when the successor did not start.
    pub fn keep(&self, lock: &File) {
        if let Some(go) = &self.go {
            let _ = set_inheritable(go, false);
        }
        let _ = set_inheritable(&self.prepared_end, false);
        let _ = set_inheritable(lock, false);
    }
}

/// Reaps the child on a thread of its own, which works for a child
/// inherited from the supervisor this one replaced too.
fn wait(process: Arc<ProcessRef>) -> watch::Receiver<Option<ExitCode>> {
    let (sender, receiver) = watch::channel(None);
    std::thread::spawn(move || {
        let code = process.reap().unwrap_or(-1);
        sender.send_replace(Some(code));
    });
    receiver
}

/// A thread, not a task: a blocked pipe read must not hold up the runtime
/// when the supervisor returns.
fn read_prepared(file: File) -> mpsc::UnboundedReceiver<()> {
    let (sender, receiver) = mpsc::unbounded_channel();
    std::thread::spawn(move || {
        let mut lines = BufReader::new(file).lines();
        while let Some(Ok(line)) = lines.next() {
            if line.trim_end() == crate::activation::PREPARED && sender.send(()).is_err() {
                return;
            }
        }
    });
    receiver
}

/// Starts the daemon [`os::process::spawn_background`]; a job that keeps it
/// (Windows) is logged.
fn start(command: &mut Command) -> io::Result<std::process::Child> {
    let spawned = os::process::spawn_background(command)?;
    if spawned.breakaway_refused {
        tracing::warn!(
            "the job the supervisor runs in forbids leaving it: the daemon ends when that job does"
        );
    }
    Ok(spawned.child)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Unix supervisor hands over the four fields every release reads;
    /// a Windows one adds the child's process handle.
    #[test]
    fn the_handed_over_state_reads_back_with_or_without_a_process_handle() {
        let unix = Inherited {
            pid: 41,
            go: 7,
            prepared: 8,
            lock: Some(9),
            process: None,
        };
        assert_eq!(unix.to_arg(), "41:7:8:9");
        assert_eq!(Inherited::parse("41:7:8:9"), Some(unix));
        let windows = Inherited {
            pid: 41,
            go: 700,
            prepared: 704,
            lock: None,
            process: Some(708),
        };
        assert_eq!(windows.to_arg(), "41:700:704:-:708");
        assert_eq!(Inherited::parse(&windows.to_arg()), Some(windows));
        for bad in ["41:7:8", "41:7:8:9:x", "41:7:8:9:10:11", "x:7:8:9"] {
            assert_eq!(Inherited::parse(bad), None, "{bad}");
        }
    }
}
