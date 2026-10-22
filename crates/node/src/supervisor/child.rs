//! The supervisor's side of its child: spawning `amux daemon` with the
//! activation pipe, waiting for it, stopping it, and handing all of it to
//! the next supervisor across an exec.
//!
//! The pipe is two anonymous pipes, one each way, named to the child in
//! [`PIPE_ENV`] as `<read>,<write>`: descriptors on Unix, handle values on
//! Windows. The child reads `go` from the first and writes `prepared` to
//! the second.

use std::fs::File;
use std::io::{self, BufRead as _, BufReader, Write as _};
use std::path::Path;
use std::process::{Command, Stdio};

use tokio::sync::{mpsc, watch};

use crate::activation::PIPE_ENV;

/// What a supervisor hands the binary it execs: its child, both ends of
/// the pipe it keeps, and its lock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Inherited {
    pub pid: u32,
    pub go: u64,
    pub prepared: u64,
    /// The lock's descriptor; None where the lock cannot cross (Windows),
    /// and the next supervisor takes it again.
    pub lock: Option<u64>,
}

impl Inherited {
    pub fn to_arg(&self) -> String {
        let lock = self
            .lock
            .map_or_else(|| "-".to_owned(), |lock| lock.to_string());
        format!("{}:{}:{}:{lock}", self.pid, self.go, self.prepared)
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
        parts.next().is_none().then_some(Self {
            pid,
            go,
            prepared,
            lock,
        })
    }
}

/// How a child ended: its exit code, or on Unix the negated signal.
pub type ExitCode = i32;

pub struct Child {
    pub pid: u32,
    exit: watch::Receiver<Option<ExitCode>>,
    go: Option<File>,
    /// Kept open so the next supervisor inherits it; a duplicate feeds the
    /// reader.
    prepared_end: File,
    prepared: Option<mpsc::UnboundedReceiver<()>>,
    #[cfg(windows)]
    process: std::sync::Arc<std::os::windows::io::OwnedHandle>,
}

impl Child {
    /// Starts `<binary> <args> daemon` with the pipe.
    pub fn spawn(binary: &Path, args: &[std::ffi::OsString]) -> io::Result<Self> {
        let (child_reads, go) = io::pipe()?;
        let (prepared_end, child_writes) = io::pipe()?;
        let (child_reads, child_writes) = (file(child_reads), file(child_writes));
        let (go, prepared_end) = (file(go), file(prepared_end));
        let mut command = Command::new(binary);
        command.args(args).arg("daemon").stdin(Stdio::null()).env(
            PIPE_ENV,
            format!("{},{}", raw(&child_reads), raw(&child_writes)),
        );
        let child = platform::spawn(&mut command, &child_reads, &child_writes)?;
        drop((child_reads, child_writes));
        let pid = child.id();
        let reader = prepared_end.try_clone()?;
        #[cfg(windows)]
        let process = platform::process_of(child)?;
        Ok(Self {
            pid,
            #[cfg(unix)]
            exit: platform::wait(pid),
            #[cfg(windows)]
            exit: platform::wait(std::sync::Arc::clone(&process)),
            #[cfg(windows)]
            process,
            go: Some(go),
            prepared_end,
            prepared: Some(read_prepared(reader)),
        })
    }

    /// Takes over a child an earlier supervisor started and activated.
    pub fn inherit(inherited: &Inherited) -> io::Result<Self> {
        let go = platform::file_from_raw(inherited.go)?;
        let prepared_end = platform::file_from_raw(inherited.prepared)?;
        #[cfg(windows)]
        let process = platform::open_process(inherited.pid)?;
        Ok(Self {
            pid: inherited.pid,
            #[cfg(unix)]
            exit: platform::wait(inherited.pid),
            #[cfg(windows)]
            exit: platform::wait(std::sync::Arc::clone(&process)),
            #[cfg(windows)]
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
        #[cfg(unix)]
        platform::signal(self.pid, libc::SIGTERM);
        #[cfg(windows)]
        drop(self.go.take());
    }

    pub fn kill(&mut self) {
        if self.has_exited() {
            return;
        }
        #[cfg(unix)]
        platform::signal(self.pid, libc::SIGKILL);
        #[cfg(windows)]
        platform::terminate(&self.process);
    }

    /// The state the next supervisor needs, with every handle in it made
    /// inheritable.
    pub fn hand_over(&self, lock: &File) -> io::Result<Inherited> {
        let go = self
            .go
            .as_ref()
            .ok_or_else(|| io::Error::other("the pipe is closed"))?;
        platform::inheritable(go, true)?;
        platform::inheritable(&self.prepared_end, true)?;
        #[cfg(unix)]
        platform::inheritable(lock, true)?;
        #[cfg(windows)]
        let _ = lock;
        Ok(Inherited {
            pid: self.pid,
            go: raw(go),
            prepared: raw(&self.prepared_end),
            #[cfg(unix)]
            lock: Some(raw(lock)),
            #[cfg(windows)]
            lock: None,
        })
    }

    /// Undoes [`Child::hand_over`] when the exec failed.
    pub fn keep(&self, lock: &File) {
        if let Some(go) = &self.go {
            let _ = platform::inheritable(go, false);
        }
        let _ = platform::inheritable(&self.prepared_end, false);
        #[cfg(unix)]
        let _ = platform::inheritable(lock, false);
        #[cfg(windows)]
        let _ = lock;
    }
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

#[cfg(unix)]
pub fn raw(file: &File) -> u64 {
    use std::os::fd::AsRawFd as _;
    file.as_raw_fd() as u64
}

#[cfg(windows)]
pub fn raw(file: &File) -> u64 {
    use std::os::windows::io::AsRawHandle as _;
    file.as_raw_handle() as u64
}

/// Execs `binary` in supervise mode with the handed-over state. Returns
/// only if the exec failed.
#[cfg(unix)]
pub fn exec_supervisor(
    binary: &Path,
    args: &[std::ffi::OsString],
    inherited: &Inherited,
) -> io::Error {
    use std::os::unix::process::CommandExt as _;
    Command::new(binary)
        .args(args)
        .arg("supervise")
        .arg("--inherit")
        .arg(inherited.to_arg())
        .exec()
}

/// Windows has no exec: starts `binary` in supervise mode with the
/// handed-over state; the caller exits once it is running.
#[cfg(windows)]
pub fn spawn_supervisor(
    binary: &Path,
    args: &[std::ffi::OsString],
    inherited: &Inherited,
) -> io::Result<()> {
    Command::new(binary)
        .args(args)
        .arg("supervise")
        .arg("--inherit")
        .arg(inherited.to_arg())
        .stdin(Stdio::null())
        .spawn()
        .map(drop)
}

#[cfg(unix)]
type Owned = std::os::fd::OwnedFd;
#[cfg(windows)]
type Owned = std::os::windows::io::OwnedHandle;

/// A pipe end as a File, which is all the pipe needs.
fn file(end: impl Into<Owned>) -> File {
    File::from(end.into())
}

#[cfg(unix)]
mod platform {
    use std::fs::File;
    use std::io;
    use std::os::fd::{AsRawFd as _, FromRawFd as _, RawFd};
    use std::os::unix::process::CommandExt as _;
    use std::process::Command;

    use tokio::sync::watch;

    use super::ExitCode;

    pub fn spawn(
        command: &mut Command,
        child_reads: &File,
        child_writes: &File,
    ) -> io::Result<std::process::Child> {
        let ends = [child_reads.as_raw_fd(), child_writes.as_raw_fd()];
        // SAFETY: fcntl is async-signal-safe and touches only the child's
        // copies of the two descriptors.
        unsafe {
            command.pre_exec(move || {
                for fd in ends {
                    set_cloexec(fd, false)?;
                }
                Ok(())
            });
        }
        command.spawn()
    }

    fn set_cloexec(fd: RawFd, on: bool) -> io::Result<()> {
        // SAFETY: plain descriptor flag calls.
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFD);
            if flags == -1 {
                return Err(io::Error::last_os_error());
            }
            let flags = if on {
                flags | libc::FD_CLOEXEC
            } else {
                flags & !libc::FD_CLOEXEC
            };
            if libc::fcntl(fd, libc::F_SETFD, flags) == -1 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    }

    pub fn inheritable(file: &File, on: bool) -> io::Result<()> {
        set_cloexec(file.as_raw_fd(), !on)
    }

    pub fn file_from_raw(raw: u64) -> io::Result<File> {
        let fd = RawFd::try_from(raw).map_err(io::Error::other)?;
        set_cloexec(fd, true)?;
        // SAFETY: the supervisor that exec'd this one handed the
        // descriptor over and nothing else here owns it.
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    /// Reaps the child on a thread of its own, which also works for a
    /// child inherited across an exec, where no Child handle exists.
    pub fn wait(pid: u32) -> watch::Receiver<Option<ExitCode>> {
        let (sender, receiver) = watch::channel(None);
        std::thread::spawn(move || {
            let mut status = 0;
            let code = loop {
                // SAFETY: waitpid on our own child.
                let reaped = unsafe { libc::waitpid(pid as libc::pid_t, &mut status, 0) };
                if reaped == -1 {
                    if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                        continue;
                    }
                    break -1;
                }
                if libc::WIFEXITED(status) {
                    break libc::WEXITSTATUS(status);
                }
                if libc::WIFSIGNALED(status) {
                    break -libc::WTERMSIG(status);
                }
            };
            sender.send_replace(Some(code));
        });
        receiver
    }

    pub fn signal(pid: u32, signal: libc::c_int) {
        // SAFETY: a signal to our own child, which has not been reaped.
        unsafe {
            libc::kill(pid as libc::pid_t, signal);
        }
    }
}

#[cfg(windows)]
mod platform {
    use std::fs::File;
    use std::io;
    use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle, RawHandle};
    use std::process::Command;
    use std::sync::Arc;

    use tokio::sync::watch;
    use windows_sys::Win32::Foundation::{
        HANDLE, HANDLE_FLAG_INHERIT, SetHandleInformation, WAIT_OBJECT_0,
    };
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, INFINITE, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, TerminateProcess, WaitForSingleObject,
    };

    use super::ExitCode;

    pub fn spawn(
        command: &mut Command,
        child_reads: &File,
        child_writes: &File,
    ) -> io::Result<std::process::Child> {
        // std starts children inheriting every inheritable handle: mark
        // the child's two ends, spawn, and the caller drops them.
        inheritable(child_reads, true)?;
        inheritable(child_writes, true)?;
        command.spawn()
    }

    pub fn inheritable(file: &File, on: bool) -> io::Result<()> {
        let flags = if on { HANDLE_FLAG_INHERIT } else { 0 };
        // SAFETY: a flag on a handle this process owns.
        if unsafe {
            SetHandleInformation(file.as_raw_handle() as HANDLE, HANDLE_FLAG_INHERIT, flags)
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub fn file_from_raw(raw: u64) -> io::Result<File> {
        // SAFETY: the supervisor that started this one handed the handle
        // over and nothing else here owns it.
        let file = unsafe { File::from_raw_handle(raw as RawHandle) };
        inheritable(&file, false)?;
        Ok(file)
    }

    pub fn process_of(child: std::process::Child) -> io::Result<Arc<OwnedHandle>> {
        Ok(Arc::new(OwnedHandle::from(child)))
    }

    pub fn open_process(pid: u32) -> io::Result<Arc<OwnedHandle>> {
        // SAFETY: opening a process by id; a null handle is an error.
        let handle = unsafe {
            OpenProcess(
                PROCESS_SYNCHRONIZE | PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION,
                0,
                pid,
            )
        };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: a fresh handle this process owns.
        Ok(Arc::new(unsafe {
            OwnedHandle::from_raw_handle(handle as RawHandle)
        }))
    }

    pub fn wait(process: Arc<OwnedHandle>) -> watch::Receiver<Option<ExitCode>> {
        let (sender, receiver) = watch::channel(None);
        std::thread::spawn(move || {
            let handle = process.as_raw_handle() as HANDLE;
            // SAFETY: waiting on and querying a process handle we own.
            let code = unsafe {
                if WaitForSingleObject(handle, INFINITE) != WAIT_OBJECT_0 {
                    -1
                } else {
                    let mut code = 0u32;
                    if GetExitCodeProcess(handle, &mut code) == 0 {
                        -1
                    } else {
                        code as i32
                    }
                }
            };
            sender.send_replace(Some(code));
        });
        receiver
    }

    pub fn terminate(process: &OwnedHandle) {
        // SAFETY: terminating a process whose handle we own.
        unsafe {
            TerminateProcess(process.as_raw_handle() as HANDLE, 1);
        }
    }
}
