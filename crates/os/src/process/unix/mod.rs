//! Unix: process groups and sessions, exec, descriptors, the tether and the
//! census over the kernel's process table.

use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd as _, FromRawFd as _, RawFd};
use std::os::unix::process::CommandExt as _;

use super::{Descendant, Detach, Replaced, Spawnable, Spawned};
use crate::Pid;

mod table;
#[cfg(test)]
pub(super) mod tests;
#[cfg(feature = "async")]
mod tether;

#[cfg(feature = "async")]
pub(crate) use tether::Tether;

pub(super) fn detach(cmd: &mut std::process::Command, how: Detach) {
    match how {
        Detach::Group => {
            cmd.process_group(0);
        }
        Detach::Session => {
            // SAFETY: setsid is async-signal-safe and touches no parent state.
            unsafe {
                cmd.pre_exec(|| {
                    if libc::setsid() == -1 {
                        return Err(io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }
    }
}

pub(super) fn spawn_background<C: Spawnable>(cmd: &mut C) -> io::Result<Spawned<C::Child>> {
    Ok(Spawned {
        child: cmd.spawn_child()?,
        breakaway_refused: false,
    })
}

pub(super) fn exec_or_spawn(mut cmd: std::process::Command) -> io::Result<Replaced> {
    Err(cmd.exec())
}

pub(super) fn console_window() -> bool {
    false
}

pub(super) fn harden_start() {
    for fd in [libc::STDIN_FILENO, libc::STDOUT_FILENO, libc::STDERR_FILENO] {
        // SAFETY: a flag query, then, for a closed standard descriptor,
        // /dev/null opened into the lowest free number, which is it.
        unsafe {
            if libc::fcntl(fd, libc::F_GETFD) == -1 {
                let null = libc::open(c"/dev/null".as_ptr(), libc::O_RDWR);
                if null != -1 && null != fd {
                    libc::dup2(null, fd);
                    libc::close(null);
                }
            }
        }
    }
}

pub(super) fn redirect_stderr(file: &File) -> io::Result<()> {
    if file.as_raw_fd() == libc::STDERR_FILENO {
        // Opened while descriptor 2 was closed: the file is standard error
        // already, and dropping it would close standard error.
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the file is descriptor 2 itself (opened while standard error was closed; \
             harden_start first)",
        ));
    }
    // SAFETY: both descriptors are open for the call's length, and dup2
    // only replaces descriptor 2 with a copy of ours.
    if unsafe { libc::dup2(file.as_raw_fd(), libc::STDERR_FILENO) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
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

pub(super) fn set_inheritable(file: &File, on: bool) -> io::Result<()> {
    set_cloexec(file.as_raw_fd(), !on)
}

pub(super) fn raw_for_child(file: &File) -> u64 {
    file.as_raw_fd() as u64
}

/// # Safety
///
/// As [`crate::process::file_from_inherited`].
pub(super) unsafe fn file_from_inherited(raw: u64) -> io::Result<File> {
    let invalid = || {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{raw} names no inherited descriptor"),
        )
    };
    let fd = RawFd::try_from(raw).map_err(|_| invalid())?;
    if fd <= libc::STDERR_FILENO {
        return Err(invalid());
    }
    // An open descriptor answers F_GETFD; a closed one is EBADF.
    // SAFETY: a flag query on a number, which touches nothing if it isn't
    // open.
    if unsafe { libc::fcntl(fd, libc::F_GETFD) } == -1 {
        return Err(invalid());
    }
    set_cloexec(fd, true)?;
    // SAFETY: open, checked above; the caller vouches that nothing else
    // here owns it.
    Ok(unsafe { File::from_raw_fd(fd) })
}

pub(super) fn descendants(root: Pid) -> Vec<Descendant> {
    let Ok(root) = i32::try_from(root) else {
        return Vec::new();
    };
    below(root, &table::table())
}

fn below(root: i32, table: &[table::Row]) -> Vec<Descendant> {
    let mut found: Vec<Descendant> = Vec::new();
    let mut parents = vec![root];
    while let Some(parent) = parents.pop() {
        for row in table {
            if row.ppid == parent && !found.iter().any(|found| found.pid == row.pid as Pid) {
                found.push(Descendant {
                    pid: row.pid as Pid,
                    started: row.started,
                });
                parents.push(row.pid);
            }
        }
    }
    found
}

pub(super) fn kill_descendants(census: &[Descendant]) {
    // Nothing to kill is the common stop (Provider::finish): no scan.
    if census.is_empty() {
        return;
    }
    let _ = signal_descendants(census, libc::SIGKILL, &table::table());
}

/// Sends `signal` to each process of `census` still the one the census
/// found; the ones that refused it, and why. One gone meanwhile is not
/// among them.
fn signal_descendants(
    census: &[Descendant],
    signal: libc::c_int,
    table: &[table::Row],
) -> Vec<(Pid, io::Error)> {
    let mut refused = Vec::new();
    for descendant in census {
        let same = table
            .iter()
            .any(|row| row.pid as Pid == descendant.pid && row.started == descendant.started);
        // SAFETY: kill only sends a signal, to a process checked to be the
        // one the census found.
        if same && unsafe { libc::kill(descendant.pid as libc::pid_t, signal) } != 0 {
            note_refusal(&mut refused, descendant.pid);
        }
    }
    refused
}

/// Notes the signal to `pid` that just failed, unless only because
/// nothing is there any more.
fn note_refusal(refused: &mut Vec<(Pid, io::Error)>, pid: Pid) {
    let error = io::Error::last_os_error();
    if error.raw_os_error() != Some(libc::ESRCH) {
        refused.push((pid, error));
    }
}

/// A member's group, joined: its leader's pid (the group's id) and the
/// tether in it. The family holds the tether's child unreaped for as long
/// as the entry stands, and a process group's id isn't reused while any
/// process of the group, a zombie among them, is unreaped. So every group
/// listed here can be signalled without reaching a stranger, and
/// `Member::release` takes its entry out before it reaps the tether.
#[derive(Debug)]
struct Joined {
    leader: Pid,
    #[cfg(feature = "async")]
    tether: Tether,
}

#[derive(Debug, Default)]
struct Groups(std::sync::Mutex<Vec<Joined>>);

impl Groups {
    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Joined>> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Takes the entry for `leader` out, so nothing signals its group again.
    #[cfg(feature = "async")]
    fn remove(&self, leader: Pid) -> Option<Joined> {
        let mut groups = self.lock();
        let at = groups.iter().position(|joined| joined.leader == leader)?;
        Some(groups.swap_remove(at))
    }
}

#[derive(Debug)]
pub(super) struct Family {
    groups: std::sync::Arc<Groups>,
}

impl Family {
    pub(super) fn contain_self() -> io::Result<Family> {
        if !table::READABLE {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "this OS's process table can't be read, so a family can't be counted",
            ));
        }
        Ok(Family {
            groups: std::sync::Arc::default(),
        })
    }

    #[cfg(feature = "async")]
    pub(super) fn prepare(&self, cmd: &mut tokio::process::Command) {
        cmd.process_group(0);
    }

    #[cfg(feature = "async")]
    pub(super) fn join(&self, child: &tokio::process::Child) -> io::Result<Member> {
        let leader = child
            .id()
            .ok_or_else(|| io::Error::other("the member has already been reaped"))?;
        let tether = Tether::new(child)?;
        self.groups.lock().push(Joined { leader, tether });
        Ok(Member {
            leader,
            groups: self.groups.clone(),
        })
    }

    /// SIGKILLs every group and every descendant, then goes round until
    /// a census finds nobody live and nothing an earlier one found still
    /// runs, within one deadline for all of it (as Windows does): a member
    /// may fork while the last is killed (one in a group of its own, which
    /// killpg misses), and a killed one stuck in the kernel runs on a
    /// while. A signal refused (not a process gone, which is what is
    /// wanted) is the error while its member runs; what runs at the
    /// deadline is named.
    ///
    /// What no census sees can't be ended: a child forked and orphaned
    /// between two censuses (its parent killed, it reparented to init or a
    /// subreaper) is out of the tree, as a daemon that detached itself is
    /// (proposal §2.2). Windows' job holds those; Unix has no such
    /// containment.
    pub(super) fn kill_others(&self) -> io::Result<()> {
        let me = std::process::id();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut refused: Vec<(Pid, io::Error)> = Vec::new();
        // Everything a census found: a killed member's children go to
        // init, out of later censuses, but must be gone too.
        let mut found: Vec<Descendant> = Vec::new();
        loop {
            // The census first, while everything still runs: once a parent
            // is killed its children belong to init and are no longer
            // below us.
            let table = table::try_table();
            let census = match &table {
                Ok(table) => below(me as i32, table),
                Err(_) => Vec::new(),
            };
            for descendant in &census {
                if !found.contains(descendant) {
                    found.push(descendant.clone());
                }
            }
            for joined in self.groups.lock().iter() {
                // SAFETY: killpg only sends a signal, to a group whose id
                // the unreaped tether in it keeps from reuse (see Joined).
                if unsafe { libc::killpg(joined.leader as libc::pid_t, libc::SIGKILL) } != 0 {
                    note_refusal(&mut refused, joined.leader);
                }
            }
            // A table that can't be read is an error, not an empty
            // family: the groups are killed, but nothing else could be
            // found to kill, or seen gone.
            let table = table?;
            for (pid, error) in signal_descendants(&census, libc::SIGKILL, &table) {
                refused.push((pid, error));
            }
            let table = table::try_table()?;
            // And what has forked since this round's census, below us
            // still: it is killed on the next round.
            for descendant in below(me as i32, &table) {
                if !found.contains(&descendant) {
                    found.push(descendant);
                }
            }
            let left: Vec<Pid> = found
                .iter()
                .filter(|descendant| {
                    table.iter().any(|row| {
                        row.pid as Pid == descendant.pid
                            && row.started == descendant.started
                            && !row.zombie
                    })
                })
                .map(|descendant| descendant.pid)
                .collect();
            if left.is_empty() {
                return Ok(());
            }
            if let Some((pid, error)) = refused.iter().find(|(pid, _)| left.contains(pid)) {
                return Err(io::Error::new(
                    error.kind(),
                    format!("member {pid} can't be ended: {error}"),
                ));
            }
            if std::time::Instant::now() >= deadline {
                return Err(io::Error::other(format!(
                    "{} members still run after being killed: {left:?}",
                    left.len()
                )));
            }
            refused.clear();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    pub(super) fn members(&self) -> io::Result<Vec<Pid>> {
        let me = std::process::id() as i32;
        let table = table::try_table()?;
        #[cfg(feature = "async")]
        let tethers: Vec<Pid> = self
            .groups
            .lock()
            .iter()
            .filter_map(|joined| joined.tether.pid())
            .collect();
        #[cfg(not(feature = "async"))]
        let tethers: Vec<Pid> = Vec::new();
        Ok(below(me, &table)
            .into_iter()
            .filter(|found| !tethers.contains(&found.pid))
            .filter(|found| {
                table
                    .iter()
                    .any(|row| row.pid as Pid == found.pid && !row.zombie)
            })
            .map(|found| found.pid)
            .collect())
    }
}

#[cfg(feature = "async")]
#[derive(Debug)]
pub(super) struct Member {
    leader: Pid,
    groups: std::sync::Arc<Groups>,
}

#[cfg(feature = "async")]
impl Member {
    pub(super) async fn release(self) {
        // Out of the family first, so no kill reaches the group once the
        // tether is reaped and the group's id is free.
        if let Some(joined) = self.groups.remove(self.leader) {
            joined.tether.release().await;
        }
    }
}

#[cfg(feature = "async")]
impl Drop for Member {
    fn drop(&mut self) {
        // Unreleased: the tether is dropped, its pipe closes and it asks
        // the group to finish; tokio reaps it later, out of the family.
        self.groups.remove(self.leader);
    }
}

#[derive(Debug)]
pub(super) struct ProcessRef {
    pid: Pid,
    started: u64,
}

impl ProcessRef {
    pub(super) fn open(pid: Pid) -> io::Result<ProcessRef> {
        if !table::READABLE {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "this OS can't read a process's start time",
            ));
        }
        let raw = i32::try_from(pid).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        match table::row(raw) {
            Some(row) if !row.zombie => Ok(ProcessRef {
                pid,
                started: row.started,
            }),
            _ => Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("no process {pid} runs"),
            )),
        }
    }

    #[cfg(feature = "async")]
    pub(super) fn of_child(child: &tokio::process::Child) -> io::Result<ProcessRef> {
        let pid = child
            .id()
            .ok_or_else(|| io::Error::other("the child has already been reaped"))?;
        Self::of_unreaped(pid)
    }

    pub(super) fn of_std_child(child: &std::process::Child) -> io::Result<ProcessRef> {
        Self::of_unreaped(child.id())
    }

    /// A child, which may have exited but can't have been reused while
    /// unreaped.
    fn of_unreaped(pid: Pid) -> io::Result<ProcessRef> {
        if !table::READABLE {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "this OS can't read a process's start time",
            ));
        }
        let raw = i32::try_from(pid).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        let row = table::row(raw)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "the child is gone"))?;
        Ok(ProcessRef {
            pid,
            started: row.started,
        })
    }

    #[cfg(test)]
    pub(super) fn forged(pid: Pid, started_after: u64) -> ProcessRef {
        let reference = Self::open(pid).expect("the process runs");
        ProcessRef {
            pid,
            started: reference.started + started_after,
        }
    }

    pub(super) fn pid(&self) -> Pid {
        self.pid
    }

    /// The process is still the one pinned and hasn't exited.
    pub(super) fn alive(&self) -> bool {
        table::row(self.pid as i32).is_some_and(|row| row.started == self.started && !row.zombie)
    }

    fn signal(&self, signal: libc::c_int) -> io::Result<bool> {
        if !self.alive() {
            return Ok(false);
        }
        // SAFETY: kill only sends a signal, to the process just checked
        // to be the one pinned.
        if unsafe { libc::kill(self.pid as libc::pid_t, signal) } == -1 {
            let error = io::Error::last_os_error();
            return match error.raw_os_error() {
                Some(libc::ESRCH) => Ok(false),
                _ => Err(error),
            };
        }
        Ok(true)
    }

    pub(super) fn kill(&self) -> io::Result<()> {
        self.signal(libc::SIGKILL).map(drop)
    }

    #[cfg(feature = "async")]
    pub(super) async fn exited(&self) -> io::Result<()> {
        while self.alive() {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        Ok(())
    }

    pub(super) fn suspend(&self) -> io::Result<()> {
        self.act(libc::SIGSTOP)
    }

    pub(super) fn resume(&self) -> io::Result<()> {
        self.act(libc::SIGCONT)
    }

    fn act(&self, signal: libc::c_int) -> io::Result<()> {
        if self.signal(signal)? {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("process {} has exited", self.pid),
            ))
        }
    }
}
