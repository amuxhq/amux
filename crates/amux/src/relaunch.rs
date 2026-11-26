//! A daemon with no supervisor starting itself again, to run the
//! experiments its config now switches on.
//!
//! On Unix it execs itself in place: the same executable, arguments and
//! environment, and the same process id, so a service manager that runs
//! `amux daemon` sees nothing happen. It does so only after its clean
//! shutdown, which releases the installation lock and closes every socket,
//! and every descriptor it holds is closed on exec, so the new image takes
//! the lock and binds the front door as any start does.
//!
//! The agents the old image started are still this process's children
//! across the exec, and the new image has no handle on them: it reaps each
//! one as it exits ([`reap_inherited`]), so none lingers as a zombie.
//!
//! Windows has no exec. The nearest honest equivalent is a fresh process:
//! the daemon starts the same executable with the same arguments and
//! environment, in the background with no window
//! ([`os::process::exec_or_spawn`]), and exits. Its id changes, so a service manager
//! watching the first process sees it exit cleanly and no longer tracks the
//! daemon that took over.

use std::io;

/// Replaces this process with a fresh start of the same daemon: an exec on
/// Unix; on Windows, which has no exec, the same command started in the
/// background on this process's standard streams, as this one exits.
/// Returns only if that could not be done.
pub fn relaunch() -> io::Error {
    let (exe, args) = match this_command() {
        Ok(found) => found,
        Err(error) => return error,
    };
    let mut command = std::process::Command::new(exe);
    command.args(args);
    match os::process::exec_or_spawn(command, &[]) {
        Ok(replaced) => {
            if replaced.breakaway_refused {
                tracing::warn!(
                    "the job the daemon runs in forbids leaving it: the next one ends when that job does"
                );
            }
            std::process::exit(0)
        }
        Err(error) => error,
    }
}

/// The executable this process runs and the arguments it was given.
fn this_command() -> io::Result<(std::path::PathBuf, Vec<std::ffi::OsString>)> {
    let exe = std::env::current_exe()?;
    Ok((exe, std::env::args_os().skip(1).collect()))
}

/// Reaps, as each exits, the children this process already has as it
/// starts: those an earlier image of it started before it exec'd itself,
/// its agents among them. A daemon that started any other way has none.
/// Nothing else waits for these: every child the daemon starts from here on
/// is a new one, waited for by whoever started it.
#[cfg(unix)]
pub fn reap_inherited() {
    let mut children = children();
    if children.is_empty() {
        return;
    }
    tracing::info!(
        children = children.len(),
        "reaping the children of the daemon this one replaced"
    );
    let spawned = std::thread::Builder::new()
        .name("amux-reap".into())
        .spawn(move || {
            while !children.is_empty() {
                children.retain(|&pid| {
                    let mut status = 0;
                    // SAFETY: a non-blocking wait on one pid this process
                    // was found to be the parent of.
                    let reaped = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
                    reaped == 0
                });
                std::thread::sleep(std::time::Duration::from_secs(5));
            }
        });
    if let Err(error) = spawned {
        tracing::warn!(%error, "could not start reaping the replaced daemon's children");
    }
}

/// The process ids whose parent is this process.
#[cfg(target_os = "macos")]
fn children() -> Vec<libc::pid_t> {
    let mut pids: Vec<libc::pid_t> = vec![0; 4096];
    let bytes = (pids.len() * std::mem::size_of::<libc::pid_t>()) as libc::c_int;
    // SAFETY: the buffer holds `bytes` bytes, and the call writes no more.
    let found =
        unsafe { libc::proc_listchildpids(libc::getpid(), pids.as_mut_ptr().cast(), bytes) };
    let found = usize::try_from(found).unwrap_or(0).min(pids.len());
    pids.truncate(found);
    pids.retain(|&pid| pid > 0);
    pids
}

/// The process ids whose parent is this process, from `/proc`.
#[cfg(target_os = "linux")]
fn children() -> Vec<libc::pid_t> {
    let me = std::process::id().to_string();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| {
            let pid: libc::pid_t = entry.ok()?.file_name().to_str()?.parse().ok()?;
            let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
            // `pid (comm) state ppid ...`; the name may hold anything, so
            // the fields are read after its last parenthesis.
            let rest = &stat[stat.rfind(')')? + 1..];
            let ppid = rest.split_whitespace().nth(1)?;
            (ppid == me).then_some(pid)
        })
        .collect()
}

#[cfg(all(unix, not(any(target_os = "macos", target_os = "linux"))))]
fn children() -> Vec<libc::pid_t> {
    Vec::new()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn a_process_finds_its_own_children() {
        let mut started: Vec<std::process::Child> = (0..2)
            .map(|_| {
                std::process::Command::new("sleep")
                    .arg("30")
                    .spawn()
                    .expect("sleep starts")
            })
            .collect();
        let found = children();
        for child in &started {
            assert!(
                found.contains(&(child.id() as libc::pid_t)),
                "{} not in {found:?}",
                child.id()
            );
        }
        for child in &mut started {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
