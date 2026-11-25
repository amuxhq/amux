//! Starting processes in the background, containing what a process starts,
//! pinning a process so its pid can't be reused under us, and handing files
//! to a successor.
//!
//! How amux's processes use these: the supervisor and daemon start with
//! [`Detach::Session`], each
//! agent with [`Detach::Group`]; the agent's first act is
//! [`Family::contain_self`], so its provider and everything the provider
//! starts are its family; and the daemon holds each agent as a
//! [`ProcessRef`].

use std::fs::File;
use std::io;
use std::path::Path;
use std::process::Stdio;

use crate::Pid;

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

/// How a background process is detached from the one that starts it.
///
/// Unix: `Group` is a process group of its own (`process_group(0)`, what
/// agents use), `Session` a session of its own (`setsid`, what the
/// supervisor and a daemon started from a terminal use), so the starting
/// terminal's hangup never reaches it.
///
/// Windows: the two are the same. `CREATE_NO_WINDOW` gives the process a
/// console of its own with no window, which everything it starts shares,
/// so nothing below it ever shows one; `CREATE_NEW_PROCESS_GROUP` keeps a
/// Ctrl+C in the starting console from reaching it; and
/// `CREATE_BREAKAWAY_FROM_JOB` takes it out of the starter's job, so a
/// terminal's or an SSH session's job ending doesn't end it. When the
/// starter's job forbids breakaway, [`spawn_background`] starts it inside
/// that job instead and says so. `DETACHED_PROCESS` is never used: a
/// console program started without a console gets a new, visible one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Detach {
    Group,
    Session,
}

mod sealed {
    pub trait Sealed {}
}

/// A command [`background`] can configure and [`spawn_background`] can
/// start: `std::process::Command`, and with the `async` feature
/// `tokio::process::Command`.
pub trait Spawnable: sealed::Sealed {
    /// What a spawn returns.
    type Child;
    #[doc(hidden)]
    fn std_command(&mut self) -> &mut std::process::Command;
    #[doc(hidden)]
    fn spawn_child(&mut self) -> io::Result<Self::Child>;
}

impl sealed::Sealed for std::process::Command {}
impl Spawnable for std::process::Command {
    type Child = std::process::Child;
    fn std_command(&mut self) -> &mut std::process::Command {
        self
    }
    fn spawn_child(&mut self) -> io::Result<std::process::Child> {
        self.spawn()
    }
}

#[cfg(feature = "async")]
impl sealed::Sealed for tokio::process::Command {}
#[cfg(feature = "async")]
impl Spawnable for tokio::process::Command {
    type Child = tokio::process::Child;
    fn std_command(&mut self) -> &mut std::process::Command {
        self.as_std_mut()
    }
    fn spawn_child(&mut self) -> io::Result<tokio::process::Child> {
        self.spawn()
    }
}

/// Configures `cmd` to run in the background: detached as `how` (see
/// [`Detach`]), with no window, its working directory `cwd`, and all three
/// standard streams null.
///
/// Call this before setting any stream the child should have instead:
/// a later `stdin`, `stdout` or `stderr` replaces the null one. (std has no
/// way to ask whether a stream was set, so this can't leave one alone.)
/// Start the command with [`spawn_background`].
pub fn background<C: Spawnable>(cmd: &mut C, how: Detach, cwd: &Path) {
    let std = cmd.std_command();
    std.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .current_dir(cwd);
    imp::detach(std, how);
}

/// A background process just started.
#[derive(Debug)]
pub struct Spawned<C> {
    pub child: C,
    /// Windows: the job this process is in forbade breakaway, so the child
    /// was started again inside it, and ends when that job does. The
    /// caller should log it. Always false on Unix.
    pub breakaway_refused: bool,
}

/// Starts a command [`background`] configured. On Windows, a start refused
/// because this process's job forbids breakaway (`ERROR_ACCESS_DENIED`) is
/// tried once more without breakaway, and the result says so.
pub fn spawn_background<C: Spawnable>(cmd: &mut C) -> io::Result<Spawned<C::Child>> {
    imp::spawn_background(cmd)
}

/// What [`exec_or_spawn`] leaves when the OS has no exec: the successor,
/// running. The caller hands over to it and exits.
#[derive(Debug)]
pub struct Replaced {
    pub child: std::process::Child,
    /// As [`Spawned::breakaway_refused`].
    pub breakaway_refused: bool,
}

/// Replaces this process with `cmd`.
///
/// Unix: exec, which returns only on failure, so this never returns `Ok`.
/// Windows, which has no exec: starts `cmd` detached as
/// [`Detach::Session`] and returns it; the caller hands over and exits.
/// Either way the successor keeps this process's standard streams and
/// working directory unless `cmd` sets its own, as an exec would.
pub fn exec_or_spawn(cmd: std::process::Command) -> io::Result<Replaced> {
    imp::exec_or_spawn(cmd)
}

/// The first call in every amux `main`.
///
/// Windows: clears `HANDLE_FLAG_INHERIT` on this process's own standard
/// handles, so a child started with handles inherited doesn't also inherit
/// the pipe this process was given (std duplicates a stream it passes on
/// as inherited, so nothing a child is meant to have is lost), and sets
/// `SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX`, so a hidden process
/// never blocks on a system dialog no one can see. Unix: opens `/dev/null`
/// onto any of descriptors 0, 1 and 2 that is closed, so no file opened
/// later becomes a standard stream by accident; descriptors need no more,
/// since std closes every one but the three on exec.
pub fn harden_start() {
    imp::harden_start();
}

/// Whether this process shows a console window: what a log says, so a
/// background process that would put one on the desktop is seen to.
/// Windows: whether its console (if any) has a window, which no process
/// [`background`] started has. Unix has no console windows: always false.
pub fn console_window() -> bool {
    imp::console_window()
}

/// Points this process's standard error at `file`, which may be dropped
/// afterwards. Unix: `dup2` onto descriptor 2. Windows: the process's
/// standard error handle (what Rust's `eprintln!` and a panic write to,
/// and what a child inheriting standard error gets) and the C runtime's
/// descriptor 2, both onto non-inheritable duplicates of `file`; a later
/// redirect closes the duplicates an earlier one made. Unix refuses
/// (`InvalidInput`) a `file` that is descriptor 2 itself, which a file
/// opened while standard error was closed is: [`harden_start`] first.
pub fn redirect_stderr(file: &File) -> io::Result<()> {
    imp::redirect_stderr(file)
}

/// Marks `file` to be inherited, or not, by the next process this one
/// starts or execs into. Unix: clears or sets `FD_CLOEXEC`. Windows: sets
/// or clears `HANDLE_FLAG_INHERIT`; std starts every child inheriting
/// every inheritable handle, so mark a file only around the one spawn that
/// should have it.
pub fn set_inheritable(file: &File, on: bool) -> io::Result<()> {
    imp::set_inheritable(file, on)
}

/// The number a successor is told for an inherited `file`, which it turns
/// back into a file with [`file_from_inherited`]: the descriptor on Unix,
/// the handle's value on Windows.
pub fn raw_for_child(file: &File) -> u64 {
    imp::raw_for_child(file)
}

/// Takes ownership of a file this process inherited as `raw` (what the
/// starter's [`raw_for_child`] said), and marks it not to be inherited
/// further.
///
/// `raw` is checked to name an open descriptor or handle (`InvalidInput`
/// when it doesn't, or when it is one of the three standard streams), but
/// whether anything else in this process already owns it can't be checked.
///
/// # Safety
///
/// `raw` must name a descriptor (Unix) or handle (Windows) that this
/// process inherited, that is open, and that nothing else in this process
/// owns or will close: the returned `File` becomes its only owner and
/// closes it on drop. Call this once per value, at the start, before
/// anything else could have opened (and so been given) the same number.
pub unsafe fn file_from_inherited(raw: u64) -> io::Result<File> {
    // SAFETY: the caller's contract above.
    unsafe { imp::file_from_inherited(raw) }
}

/// The calling process and everything it starts, contained.
///
/// Windows: [`Family::contain_self`] assigns this process to a new Job
/// Object (`KILL_ON_JOB_CLOSE | BREAKAWAY_OK`, its handle held
/// non-inheritably and never closed), so every child is born inside it:
/// no suspended start, no race. A process that asks for
/// `CREATE_BREAKAWAY_FROM_JOB` (a build daemon shared between agents)
/// escapes, as a `setsid` escapes a process group on Unix. When this
/// process dies, however it dies, the handle closes and the rest dies.
///
/// Unix: each child started through [`Family::prepare`] and
/// [`Family::join`] leads a process group of its own, tethered to this
/// process (see [`Tether`]); the census is every process descended from
/// this one, each pinned by its start time. A process that daemonizes
/// (leaves the group and is reparented away) escapes.
#[derive(Debug)]
pub struct Family {
    inner: imp::Family,
}

impl Family {
    /// Contains this process and what it starts from now on. Call it once,
    /// first, before starting anything.
    pub fn contain_self() -> io::Result<Family> {
        imp::Family::contain_self().map(|inner| Family { inner })
    }

    /// Readies `cmd` to start a member. Unix: `process_group(0)`, so the
    /// member's group can be signalled and tethered. Windows: nothing, since
    /// every child is born inside the job.
    #[cfg(feature = "async")]
    pub fn prepare(&self, cmd: &mut tokio::process::Command) {
        self.inner.prepare(cmd);
    }

    /// Joins a child just started from a command [`Family::prepare`]
    /// readied. Unix: tethers its group, so the group is asked to finish
    /// when this process dies. Windows: nothing; it was born inside.
    /// Release the member once the child has exited.
    #[cfg(feature = "async")]
    pub fn join(&self, child: &tokio::process::Child) -> io::Result<Member> {
        self.inner.join(child).map(|inner| Member { inner })
    }

    /// Ends every member but this process, at once (SIGKILL,
    /// `TerminateProcess`), each checked to still be the process it was.
    /// `Ok` only once no member is left: a census that can't be read, a
    /// member that can't be opened or ended (Windows: its DACL denies
    /// `PROCESS_TERMINATE`), or one still running after ten seconds is an
    /// error.
    pub fn kill_others(&self) -> io::Result<()> {
        self.inner.kill_others()
    }

    /// Every process in the family but this one, as it stands now. A
    /// process that has exited but not been reaped is not a member; one
    /// that can't be asked after is.
    pub fn members(&self) -> io::Result<Vec<Pid>> {
        self.inner.members()
    }
}

/// A child [`Family::join`] took in.
#[cfg(feature = "async")]
#[derive(Debug)]
pub struct Member {
    inner: imp::Member,
}

#[cfg(feature = "async")]
impl Member {
    /// Lets go once the child has exited. Unix: whatever it left in its
    /// group is sent SIGTERM, and the tether is reaped. Windows: nothing.
    /// Dropping a member unreleased asks its group to finish the same way,
    /// without waiting for the tether.
    pub async fn release(self) {
        self.inner.release().await;
    }
}

/// Ties a process group to the process that started it (Unix).
///
/// A provider leads a process group of its own, so nothing the agent's
/// death does reaches it: a server on a socket would serve on with no one
/// to serve, and headless Claude finishes its turn after its input closes,
/// running tools for no one. The tether is a shell in the provider's process
/// group, reading a pipe only the agent holds: when the agent dies, however
/// it dies, the pipe closes and the shell sends the group SIGTERM. If
/// anything of the group, the provider or a process it left, still runs
/// [`TETHER_GRACE_SECS`] later (Codex with an approval open ignores
/// SIGTERM, and would hold its thread from every later client), the group
/// is killed. Being in the group, the shell keeps the group's id, and so
/// the provider's pid, from being reused while it waits.
///
/// Windows has no process groups to tether: [`Tether::new`] returns
/// `Unsupported`, and [`Family`]'s job is the tether there.
#[cfg(feature = "async")]
#[derive(Debug)]
pub struct Tether {
    inner: imp::Tether,
}

/// How long a tethered group has to finish once asked, before it is
/// killed.
pub const TETHER_GRACE_SECS: u32 = 5;

#[cfg(feature = "async")]
impl Tether {
    /// Tethers the process group `leader` leads, which must not have been
    /// reaped.
    pub fn new(leader: &tokio::process::Child) -> io::Result<Tether> {
        imp::Tether::new(leader).map(|inner| Tether { inner })
    }

    /// The tether's own process, which a census of this process's
    /// descendants finds too.
    pub fn pid(&self) -> Option<Pid> {
        self.inner.pid()
    }

    /// Lets go once the leader has exited: whatever it left in its group
    /// is sent SIGTERM, and the shell is reaped.
    pub async fn release(self) {
        self.inner.release().await;
    }
}

/// One process found below another by [`descendants`]: its pid and its
/// start time, which together are its identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Descendant {
    pid: Pid,
    /// When it started, in the kernel's own unit.
    started: u64,
}

impl Descendant {
    pub fn pid(&self) -> Pid {
        self.pid
    }
}

/// Every process descended from `root`, not `root` itself, read from the
/// kernel's process table (not `ps`, which macOS's sandbox won't run).
///
/// By the time a process has exited its orphans belong to init, so a
/// census of what it leaves must be taken while it still runs.
///
/// Empty where the table can't be read, which includes Windows, where a
/// [`Family`]'s job takes the census instead: this is the Unix census
/// `agent` uses until its callers move to [`Family`].
pub fn descendants(root: Pid) -> Vec<Descendant> {
    imp::descendants(root)
}

/// SIGKILLs each of `census` that is still the process it was: a pid
/// reused since the census is never signalled. Does nothing on Windows,
/// where [`descendants`] finds nothing.
pub fn kill_descendants(census: &[Descendant]) {
    imp::kill_descendants(census);
}

/// A process pinned so its pid can't be reused under us.
///
/// Windows: an owned handle (`PROCESS_TERMINATE | SYNCHRONIZE |
/// QUERY_LIMITED_INFORMATION`, and `SUSPEND_RESUME` when granted). While
/// it is open the pid is not reused, so every act reaches the process it
/// was opened on, alive or exited.
///
/// Unix: the pid and the process's start time (Linux `/proc/<pid>/stat`,
/// macOS `proc_pidinfo`), checked before every act: once the process has
/// gone, the ref answers as for an exited process and never signals
/// whoever has the pid now. The check and the signal are two calls, so a
/// pid reused in the microseconds between them could still be reached;
/// pids are not reused that fast. iOS can't read another process's start
/// time, so [`ProcessRef::open`] is `Unsupported` there.
#[derive(Debug)]
pub struct ProcessRef {
    inner: imp::ProcessRef,
}

impl ProcessRef {
    /// Pins the process `pid` names now. `NotFound` when none does.
    pub fn open(pid: Pid) -> io::Result<ProcessRef> {
        imp::ProcessRef::open(pid).map(|inner| ProcessRef { inner })
    }

    /// Pins a child of this process, which must not have been reaped.
    #[cfg(feature = "async")]
    pub fn of_child(child: &tokio::process::Child) -> io::Result<ProcessRef> {
        imp::ProcessRef::of_child(child).map(|inner| ProcessRef { inner })
    }

    /// Pins a child of this process started with std, which must not have
    /// been reaped.
    pub fn of_std_child(child: &std::process::Child) -> io::Result<ProcessRef> {
        imp::ProcessRef::of_std_child(child).map(|inner| ProcessRef { inner })
    }

    pub fn pid(&self) -> Pid {
        self.inner.pid()
    }

    /// Whether the process still runs. False once it has exited, even
    /// before it is reaped.
    pub fn alive(&self) -> bool {
        self.inner.alive()
    }

    /// Ends the process at once (SIGKILL, `TerminateProcess`). `Ok` when it
    /// has already exited.
    pub fn kill(&self) -> io::Result<()> {
        self.inner.kill()
    }

    /// Resolves once the process has exited. Unix, which has no handle to
    /// wait on for a process that isn't our child, looks every 50 ms.
    #[cfg(feature = "async")]
    pub async fn exited(&self) -> io::Result<()> {
        self.inner.exited().await
    }

    /// Stops the process from running until [`ProcessRef::resume`]
    /// (SIGSTOP, `NtSuspendProcess`), as the test net freezes a host.
    /// `NotFound` once it has exited.
    pub fn suspend(&self) -> io::Result<()> {
        self.inner.suspend()
    }

    /// Lets a suspended process run again (SIGCONT, `NtResumeProcess`).
    pub fn resume(&self) -> io::Result<()> {
        self.inner.resume()
    }
}
