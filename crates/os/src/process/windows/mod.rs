//! Windows: creation flags, handle hygiene, the agent's Job Object and
//! process handles.

use std::fs::File;
use std::io;
use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle, RawHandle};
use std::os::windows::process::CommandExt as _;

use windows_sys::Win32::Foundation::{
    CloseHandle, DUPLICATE_SAME_ACCESS, DuplicateHandle, ERROR_ACCESS_DENIED,
    ERROR_INVALID_PARAMETER, ERROR_MORE_DATA, HANDLE, HANDLE_FLAG_INHERIT, SetHandleInformation,
    WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::System::Console::{
    GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SetStdHandle,
};
use windows_sys::Win32::System::Diagnostics::Debug::{
    SEM_FAILCRITICALERRORS, SEM_NOGPFAULTERRORBOX, SetErrorMode,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, IsProcessInJob, JOB_OBJECT_LIMIT_BREAKAWAY_OK,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_BASIC_PROCESS_ID_LIST,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectBasicProcessIdList,
    JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject,
};
use windows_sys::Win32::System::Threading::{
    CREATE_BREAKAWAY_FROM_JOB, CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, GetCurrentProcess,
    GetProcessId, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SUSPEND_RESUME,
    PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, TerminateProcess, WaitForSingleObject,
};

use super::{Descendant, Detach, Replaced, Spawnable, Spawned};
use crate::Pid;

#[cfg(feature = "async")]
mod wait;

/// Every background process's flags (see [`Detach`]).
const BACKGROUND: u32 = CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB;

pub(super) fn detach(cmd: &mut std::process::Command, _how: Detach) {
    cmd.creation_flags(BACKGROUND);
}

pub(super) fn spawn_background<C: Spawnable>(cmd: &mut C) -> io::Result<Spawned<C::Child>> {
    match cmd.spawn_child() {
        Ok(child) => Ok(Spawned {
            child,
            breakaway_refused: false,
        }),
        // Our job doesn't allow breakaway: start the child inside it.
        Err(error) if error.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) => {
            cmd.std_command()
                .creation_flags(BACKGROUND & !CREATE_BREAKAWAY_FROM_JOB);
            Ok(Spawned {
                child: cmd.spawn_child()?,
                breakaway_refused: true,
            })
        }
        Err(error) => Err(error),
    }
}

pub(super) fn exec_or_spawn(mut cmd: std::process::Command) -> io::Result<Replaced> {
    detach(&mut cmd, Detach::Session);
    let spawned = spawn_background(&mut cmd)?;
    Ok(Replaced {
        child: spawned.child,
        breakaway_refused: spawned.breakaway_refused,
    })
}

pub(super) fn harden_start() {
    for which in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        // SAFETY: reading our own standard handle and clearing a flag on
        // it. A missing handle (null) or a console pseudo-handle fails
        // harmlessly.
        unsafe {
            let handle = GetStdHandle(which);
            if !handle.is_null() && handle != windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE {
                SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0);
            }
        }
    }
    // SAFETY: sets this process's error mode only.
    unsafe {
        SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX);
    }
}

// The C runtime's descriptors, which Rust links on every Windows target.
unsafe extern "C" {
    fn _open_osfhandle(handle: isize, flags: i32) -> i32;
    fn _dup2(from: i32, to: i32) -> i32;
    fn _close(fd: i32) -> i32;
    fn _get_osfhandle(fd: i32) -> isize;
}

/// `_O_APPEND`, `_O_BINARY`.
const CRT_FLAGS: i32 = 0x0008 | 0x8000;

pub(super) fn redirect_stderr(file: &File) -> io::Result<()> {
    // The C runtime's descriptor 2 first: its _dup2 onto a standard
    // descriptor may set the standard handle too, which ours then replaces.
    let crt = duplicate(file.as_raw_handle() as HANDLE)?;
    // SAFETY: _open_osfhandle takes ownership of the fresh duplicate; the
    // descriptor it returns is closed once copied onto 2.
    unsafe {
        let fd = _open_osfhandle(crt as isize, CRT_FLAGS);
        if fd == -1 {
            CloseHandle(crt);
            return Err(io::Error::other("the C runtime refused the handle"));
        }
        let copied = _dup2(fd, 2);
        _close(fd);
        if copied == -1 {
            return Err(io::Error::other(
                "the C runtime couldn't copy onto descriptor 2",
            ));
        }
        // _dup2 duplicates inheritably; nothing should inherit it.
        let two = _get_osfhandle(2);
        if two != -1 {
            SetHandleInformation(two as HANDLE, HANDLE_FLAG_INHERIT, 0);
        }
    }
    let ours = duplicate(file.as_raw_handle() as HANDLE)?;
    // SAFETY: the standard handle takes the fresh duplicate, which this
    // process then holds for its life.
    if unsafe { SetStdHandle(STD_ERROR_HANDLE, ours) } == 0 {
        let error = io::Error::last_os_error();
        // SAFETY: the duplicate is ours and nothing took it.
        unsafe { CloseHandle(ours) };
        return Err(error);
    }
    Ok(())
}

/// A non-inheritable duplicate of `handle` in this process.
fn duplicate(handle: HANDLE) -> io::Result<HANDLE> {
    let mut copy: HANDLE = std::ptr::null_mut();
    // SAFETY: duplicating a handle this process holds into itself.
    let done = unsafe {
        DuplicateHandle(
            GetCurrentProcess(),
            handle,
            GetCurrentProcess(),
            &mut copy,
            0,
            0,
            DUPLICATE_SAME_ACCESS,
        )
    };
    if done == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(copy)
}

pub(super) fn set_inheritable(file: &File, on: bool) -> io::Result<()> {
    let flags = if on { HANDLE_FLAG_INHERIT } else { 0 };
    // SAFETY: a flag on a handle this process owns.
    if unsafe { SetHandleInformation(file.as_raw_handle() as HANDLE, HANDLE_FLAG_INHERIT, flags) }
        == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub(super) fn raw_for_child(file: &File) -> u64 {
    file.as_raw_handle() as u64
}

pub(super) fn file_from_inherited(raw: u64) -> io::Result<File> {
    // SAFETY: the caller vouches that the process that started this one
    // handed the handle over and nothing else here owns it.
    let file = unsafe { File::from_raw_handle(raw as RawHandle) };
    set_inheritable(&file, false)?;
    Ok(file)
}

pub(super) fn descendants(_root: Pid) -> Vec<Descendant> {
    Vec::new()
}

pub(super) fn kill_descendants(_census: &[Descendant]) {}

/// A raw handle this process holds for its life.
#[derive(Debug, Clone, Copy)]
struct Held(HANDLE);
// SAFETY: a kernel handle is usable from any thread.
unsafe impl Send for Held {}
// SAFETY: as above; the job is only queried and assigned to.
unsafe impl Sync for Held {}

#[derive(Debug)]
pub(super) struct Family {
    /// Never closed: closing it would end every member and this process
    /// too, which is what this process's death does.
    job: Held,
}

impl Family {
    pub(super) fn contain_self() -> io::Result<Family> {
        // SAFETY: a new, unnamed job whose handle isn't inheritable (no
        // security attributes), set up and joined; on failure the handle
        // is closed before this process is in it.
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return Err(io::Error::last_os_error());
            }
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            limits.BasicLimitInformation.LimitFlags =
                JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_BREAKAWAY_OK;
            let set = SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            if set == 0 || AssignProcessToJobObject(job, GetCurrentProcess()) == 0 {
                let error = io::Error::last_os_error();
                CloseHandle(job);
                return Err(error);
            }
            Ok(Family { job: Held(job) })
        }
    }

    #[cfg(feature = "async")]
    pub(super) fn prepare(&self, _cmd: &mut tokio::process::Command) {}

    #[cfg(feature = "async")]
    pub(super) fn join(&self, _child: &tokio::process::Child) -> io::Result<Member> {
        Ok(Member)
    }

    /// Every process id in the job, this one among them.
    fn ids(&self) -> io::Result<Vec<Pid>> {
        let header = std::mem::offset_of!(JOBOBJECT_BASIC_PROCESS_ID_LIST, ProcessIdList);
        let mut room = 64usize;
        loop {
            let words = header.div_ceil(size_of::<usize>()) + room;
            let mut buffer = vec![0usize; words];
            let bytes = (words * size_of::<usize>()) as u32;
            // SAFETY: the buffer is `bytes` long and aligned for the list,
            // which the call writes no more than.
            let done = unsafe {
                QueryInformationJobObject(
                    self.job.0,
                    JobObjectBasicProcessIdList,
                    buffer.as_mut_ptr().cast(),
                    bytes,
                    std::ptr::null_mut(),
                )
            };
            // SAFETY: the buffer starts with the list's header.
            let list = unsafe { &*buffer.as_ptr().cast::<JOBOBJECT_BASIC_PROCESS_ID_LIST>() };
            if done == 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() == Some(ERROR_MORE_DATA as i32) {
                    room = (list.NumberOfAssignedProcesses as usize).max(room * 2) + 16;
                    continue;
                }
                return Err(error);
            }
            let count = list.NumberOfProcessIdsInList as usize;
            let first = header / size_of::<usize>();
            return Ok(buffer[first..first + count]
                .iter()
                .map(|&id| id as Pid)
                .collect());
        }
    }

    /// Each member but us, opened, checked to be in our job (so not a
    /// process that took the id of one that has left it) and running. The
    /// job's list can still name a member that has just exited.
    fn live_members(&self) -> Vec<(Pid, OwnedHandle)> {
        let me = std::process::id();
        let ids = self.ids().unwrap_or_default();
        ids.into_iter()
            .filter(|&pid| pid != me)
            .filter_map(|pid| {
                // SAFETY: opening a process by id; a null handle means it
                // has gone (or isn't ours to touch, which no member is).
                let handle = unsafe {
                    OpenProcess(
                        PROCESS_TERMINATE | PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
                        0,
                        pid,
                    )
                };
                if handle.is_null() {
                    return None;
                }
                // SAFETY: a fresh handle this process owns.
                let handle = unsafe { OwnedHandle::from_raw_handle(handle as RawHandle) };
                let raw = handle.as_raw_handle() as HANDLE;
                let mut inside = 0;
                // SAFETY: queries of a handle we own and of our job.
                let ours = unsafe {
                    IsProcessInJob(raw, self.job.0, &mut inside) != 0
                        && inside != 0
                        && WaitForSingleObject(raw, 0) == WAIT_TIMEOUT
                };
                ours.then_some((pid, handle))
            })
            .collect()
    }

    pub(super) fn members(&self) -> Vec<Pid> {
        self.live_members()
            .into_iter()
            .map(|(pid, _)| pid)
            .collect()
    }

    pub(super) fn kill_others(&self) -> io::Result<()> {
        // A member may start another while the last is killed: go round
        // until none is left.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let others = self.live_members();
            if others.is_empty() {
                return Ok(());
            }
            if std::time::Instant::now() >= deadline {
                let left: Vec<Pid> = others.iter().map(|(pid, _)| *pid).collect();
                return Err(io::Error::other(format!(
                    "{} members still run after being killed: {left:?}",
                    left.len()
                )));
            }
            for (_, handle) in &others {
                // SAFETY: terminating a member whose handle we own.
                unsafe {
                    TerminateProcess(handle.as_raw_handle() as HANDLE, 1);
                }
            }
            for (_, handle) in &others {
                // SAFETY: waiting on a process handle we own.
                unsafe {
                    WaitForSingleObject(handle.as_raw_handle() as HANDLE, 1_000);
                }
            }
        }
    }
}

#[cfg(feature = "async")]
#[derive(Debug)]
pub(super) struct Member;

#[cfg(feature = "async")]
impl Member {
    pub(super) async fn release(self) {}
}

#[cfg(feature = "async")]
#[derive(Debug)]
pub(crate) struct Tether;

#[cfg(feature = "async")]
impl Tether {
    pub(crate) fn new(_leader: &tokio::process::Child) -> io::Result<Tether> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Windows has no process groups to tether; a Family's job is the tether",
        ))
    }

    pub(crate) fn pid(&self) -> Option<Pid> {
        None
    }

    pub(crate) async fn release(self) {}
}

// The suspend and resume ntdll exports, which windows-sys doesn't carry.
// raw-dylib needs no import library.
#[link(name = "ntdll.dll", kind = "raw-dylib", modifiers = "+verbatim")]
unsafe extern "system" {
    fn NtSuspendProcess(process: HANDLE) -> i32;
    fn NtResumeProcess(process: HANDLE) -> i32;
}

#[derive(Debug)]
pub(super) struct ProcessRef {
    handle: OwnedHandle,
    pid: Pid,
}

const RIGHTS: u32 = PROCESS_TERMINATE | PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION;

impl ProcessRef {
    pub(super) fn open(pid: Pid) -> io::Result<ProcessRef> {
        // SAFETY: opening a process by id; a null handle is an error.
        let mut handle = unsafe { OpenProcess(RIGHTS | PROCESS_SUSPEND_RESUME, 0, pid) };
        if handle.is_null() {
            // SAFETY: as above, without the right to suspend it.
            handle = unsafe { OpenProcess(RIGHTS, 0, pid) };
        }
        if handle.is_null() {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(ERROR_INVALID_PARAMETER as i32) {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("no process {pid} runs"),
                ));
            }
            return Err(error);
        }
        // SAFETY: a fresh handle this process owns.
        let handle = unsafe { OwnedHandle::from_raw_handle(handle as RawHandle) };
        Ok(ProcessRef { handle, pid })
    }

    #[cfg(feature = "async")]
    pub(super) fn of_child(child: &tokio::process::Child) -> io::Result<ProcessRef> {
        let handle = child
            .raw_handle()
            .ok_or_else(|| io::Error::other("the child has already been reaped"))?;
        Self::of_handle(handle as HANDLE)
    }

    pub(super) fn of_std_child(child: &std::process::Child) -> io::Result<ProcessRef> {
        Self::of_handle(child.as_raw_handle() as HANDLE)
    }

    fn of_handle(handle: HANDLE) -> io::Result<ProcessRef> {
        let copy = duplicate(handle)?;
        // SAFETY: a fresh duplicate this process owns.
        let handle = unsafe { OwnedHandle::from_raw_handle(copy as RawHandle) };
        // SAFETY: querying a process handle we own.
        let pid = unsafe { GetProcessId(handle.as_raw_handle() as HANDLE) };
        if pid == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(ProcessRef { handle, pid })
    }

    fn raw(&self) -> HANDLE {
        self.handle.as_raw_handle() as HANDLE
    }

    pub(super) fn pid(&self) -> Pid {
        self.pid
    }

    pub(super) fn alive(&self) -> bool {
        // SAFETY: a zero-timeout wait on a process handle we own.
        unsafe { WaitForSingleObject(self.raw(), 0) == WAIT_TIMEOUT }
    }

    pub(super) fn kill(&self) -> io::Result<()> {
        if !self.alive() {
            return Ok(());
        }
        // SAFETY: terminating the process whose handle we own.
        if unsafe { TerminateProcess(self.raw(), 1) } == 0 {
            let error = io::Error::last_os_error();
            // It exited between the look and the kill.
            // SAFETY: as in alive.
            if unsafe { WaitForSingleObject(self.raw(), 0) } == WAIT_OBJECT_0 {
                return Ok(());
            }
            return Err(error);
        }
        Ok(())
    }

    #[cfg(feature = "async")]
    pub(super) async fn exited(&self) -> io::Result<()> {
        if !self.alive() {
            return Ok(());
        }
        wait::signalled(self.raw())?.await;
        Ok(())
    }

    pub(super) fn suspend(&self) -> io::Result<()> {
        // SAFETY: suspending the process whose handle we own.
        self.nt(unsafe { NtSuspendProcess(self.raw()) })
    }

    pub(super) fn resume(&self) -> io::Result<()> {
        // SAFETY: resuming the process whose handle we own.
        self.nt(unsafe { NtResumeProcess(self.raw()) })
    }

    fn nt(&self, status: i32) -> io::Result<()> {
        if status >= 0 {
            return Ok(());
        }
        if !self.alive() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("process {} has exited", self.pid),
            ));
        }
        /// STATUS_ACCESS_DENIED.
        const ACCESS_DENIED: i32 = 0xC000_0022_u32 as i32;
        if status == ACCESS_DENIED {
            return Err(io::Error::from(io::ErrorKind::PermissionDenied));
        }
        Err(io::Error::other(format!(
            "NTSTATUS {status:#010x} for process {}",
            self.pid
        )))
    }
}

#[cfg(test)]
pub(super) fn in_job(job: Option<HANDLE>) -> bool {
    let mut inside = 0;
    // SAFETY: asking about this process, with a job handle the caller
    // holds or null for any job.
    unsafe {
        IsProcessInJob(
            GetCurrentProcess(),
            job.unwrap_or(std::ptr::null_mut()),
            &mut inside,
        );
    }
    inside != 0
}
