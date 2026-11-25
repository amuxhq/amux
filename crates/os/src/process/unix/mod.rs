//! Unix: process groups and sessions, exec, descriptors, the tether and the
//! census over the kernel's process table.

use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd as _, FromRawFd as _, RawFd};
use std::os::unix::process::CommandExt as _;

use super::{Descendant, Detach, Replaced, Spawnable, Spawned};
use crate::Pid;

mod table;
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

pub(super) fn harden_start() {}

pub(super) fn redirect_stderr(file: &File) -> io::Result<()> {
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

pub(super) fn file_from_inherited(raw: u64) -> io::Result<File> {
    let fd = RawFd::try_from(raw).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    set_cloexec(fd, true)?;
    // SAFETY: the caller vouches that the process that started this one
    // handed the descriptor over and nothing else here owns it.
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
    if census.is_empty() {
        return;
    }
    let table = table::table();
    for descendant in census {
        let same = table
            .iter()
            .any(|row| row.pid as Pid == descendant.pid && row.started == descendant.started);
        if same {
            // SAFETY: kill only sends a signal, to a process checked to be
            // the one the census found.
            unsafe {
                libc::kill(descendant.pid as libc::pid_t, libc::SIGKILL);
            }
        }
    }
}

/// The groups this process's members lead, each with its tether's pid,
/// while the tether holds the group's id from reuse.
#[derive(Debug, Default)]
struct Groups(std::sync::Mutex<Vec<(Pid, Option<Pid>)>>);

impl Groups {
    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<(Pid, Option<Pid>)>> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
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
        let tether = Tether::new(child)?;
        let leader = child
            .id()
            .ok_or_else(|| io::Error::other("the member has already been reaped"))?;
        self.groups.lock().push((leader, tether.pid()));
        Ok(Member {
            tether: Some(tether),
            leader,
            groups: self.groups.clone(),
        })
    }

    pub(super) fn kill_others(&self) -> io::Result<()> {
        let me = std::process::id();
        // The census first, while everything still runs: once a parent is
        // killed its children belong to init and are no longer below us.
        let census = descendants(me);
        for (leader, _) in self.groups.lock().iter() {
            // SAFETY: killpg only sends a signal, to a group whose id the
            // tether in it keeps from reuse.
            unsafe {
                libc::killpg(*leader as libc::pid_t, libc::SIGKILL);
            }
        }
        kill_descendants(&census);
        // And whatever forked while the census was taken.
        kill_descendants(&descendants(me));
        Ok(())
    }

    pub(super) fn members(&self) -> Vec<Pid> {
        let me = std::process::id() as i32;
        let table = table::table();
        let tethers: Vec<Pid> = self
            .groups
            .lock()
            .iter()
            .filter_map(|(_, tether)| *tether)
            .collect();
        below(me, &table)
            .into_iter()
            .filter(|found| !tethers.contains(&found.pid))
            .filter(|found| {
                table
                    .iter()
                    .any(|row| row.pid as Pid == found.pid && !row.zombie)
            })
            .map(|found| found.pid)
            .collect()
    }
}

#[cfg(feature = "async")]
#[derive(Debug)]
pub(super) struct Member {
    tether: Option<Tether>,
    leader: Pid,
    groups: std::sync::Arc<Groups>,
}

#[cfg(feature = "async")]
impl Member {
    pub(super) async fn release(mut self) {
        if let Some(tether) = self.tether.take() {
            tether.release().await;
        }
    }
}

#[cfg(feature = "async")]
impl Drop for Member {
    fn drop(&mut self) {
        // Once the tether has gone nothing holds the group's id.
        let leader = self.leader;
        self.groups.lock().retain(|(group, _)| *group != leader);
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
