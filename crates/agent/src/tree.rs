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

/// One process found below the provider.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Descendant {
    pid: i32,
    /// When it started, as `ps` prints it: with the pid, the process's
    /// identity.
    started: String,
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
                    started: started.clone(),
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
#[cfg(unix)]
fn table() -> Vec<(i32, i32, String)> {
    let Ok(output) = std::process::Command::new("ps")
        .args(["-A", "-o", "pid=", "-o", "ppid=", "-o", "lstart="])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            let pid = words.next()?.parse().ok()?;
            let ppid = words.next()?.parse().ok()?;
            let started = words.collect::<Vec<_>>().join(" ");
            Some((pid, ppid, started))
        })
        .collect()
}
