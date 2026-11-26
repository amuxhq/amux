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
//! one as it exits ([`os::process::reap_inherited`]), so none lingers as a
//! zombie.
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
