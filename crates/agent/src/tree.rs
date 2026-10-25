//! The provider's process tree, for stopping it.
//!
//! Killing the provider's process group is not enough to end what it runs:
//! headless Claude's and Codex's tool shells each lead a session, and so a
//! group, of their own, and headless Claude does not even exit on the end
//! of its input while a background shell runs. So the agent takes a census
//! of every process descended from the provider when it asks the provider
//! to finish, and once the provider has exited (or is killed) kills
//! whatever of that census still runs. A command the provider cleaned up
//! itself on SIGTERM is gone by then and is not touched.
//!
//! By the time the provider has exited its orphans belong to init, so the
//! census must be taken while the provider still runs. Each process is
//! remembered with its start time, and is killed only while a process of
//! that pid and start time is running, so a pid reused in between is never
//! signalled.
//!
//! The table is read from the kernel rather than from `ps`: macOS's `ps` is
//! setuid root, and a sandboxed process may not run a setuid program, so
//! under a sandbox `ps` printed nothing and a stop left every command
//! running.

/// One process found below the provider.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Descendant {
    pid: i32,
    /// When it started, in the kernel's own unit: with the pid, the
    /// process's identity.
    started: u64,
}

/// Every process descended from `root`, not `root` itself. Empty where
/// the table cannot be read.
#[cfg(unix)]
pub(crate) fn descendants(root: u32) -> Vec<Descendant> {
    let table = table();
    let mut found = Vec::new();
    let mut parents = vec![root as i32];
    while let Some(parent) = parents.pop() {
        for (pid, ppid, started) in &table {
            if *ppid == parent && !found.iter().any(|found: &Descendant| found.pid == *pid) {
                found.push(Descendant {
                    pid: *pid,
                    started: *started,
                });
                parents.push(*pid);
            }
        }
    }
    found
}

#[cfg(not(unix))]
pub(crate) fn descendants(_root: u32) -> Vec<Descendant> {
    Vec::new()
}

/// SIGKILLs each of `census` that is still the process it was.
#[cfg(unix)]
pub(crate) fn kill(census: &[Descendant]) {
    if census.is_empty() {
        return;
    }
    let table = table();
    for descendant in census {
        let same = table
            .iter()
            .any(|(pid, _, started)| *pid == descendant.pid && *started == descendant.started);
        if same {
            // SAFETY: kill only sends a signal, to a process checked to be
            // the one the census found.
            unsafe {
                libc::kill(descendant.pid, libc::SIGKILL);
            }
        }
    }
}

#[cfg(not(unix))]
pub(crate) fn kill(_census: &[Descendant]) {}

/// Every process as (pid, parent pid, start time).
#[cfg(target_os = "macos")]
fn table() -> Vec<(i32, i32, u64)> {
    // SAFETY: a null buffer asks only for the number of pids.
    let count = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    if count <= 0 {
        return Vec::new();
    }
    // Room for processes started since the count.
    let mut pids = vec![0 as libc::pid_t; count as usize + 64];
    let bytes = (pids.len() * size_of::<libc::pid_t>()) as libc::c_int;
    // SAFETY: the buffer holds `bytes` bytes of pids.
    let count = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
    pids.truncate(count.max(0) as usize);
    pids.into_iter()
        .filter_map(|pid| {
            let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::uninit();
            let size = size_of::<libc::proc_bsdinfo>() as libc::c_int;
            // SAFETY: the buffer holds one proc_bsdinfo, and is read only
            // when the kernel wrote all of it.
            let info = unsafe {
                let written = libc::proc_pidinfo(
                    pid,
                    libc::PROC_PIDTBSDINFO,
                    0,
                    info.as_mut_ptr().cast(),
                    size,
                );
                (written == size).then(|| info.assume_init())?
            };
            let started = info.pbi_start_tvsec * 1_000_000 + info.pbi_start_tvusec;
            Some((pid, info.pbi_ppid as i32, started))
        })
        .collect()
}

/// Every process as (pid, parent pid, start time).
#[cfg(target_os = "linux")]
fn table() -> Vec<(i32, i32, u64)> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| {
            let pid = entry.ok()?.file_name().to_str()?.parse().ok()?;
            let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
            // The command name in parentheses may hold spaces and
            // parentheses of its own; the fields after it are plain. They
            // start at the state, so the parent pid is the second and the
            // start time the twentieth.
            let mut fields = stat.get(stat.rfind(')')? + 1..)?.split_whitespace();
            let ppid = fields.nth(1)?.parse().ok()?;
            let started = fields.nth(17)?.parse().ok()?;
            Some((pid, ppid, started))
        })
        .collect()
}

#[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
fn table() -> Vec<(i32, i32, u64)> {
    Vec::new()
}
