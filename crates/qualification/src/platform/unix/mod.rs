//! Unix: `getloadavg`, `RLIMIT_NOFILE` and the exit status's signal; the
//! memory and model readers per OS beside this file.

use std::io;
use std::os::unix::process::ExitStatusExt as _;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub(super) use linux::{machine_model, memory};
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub(super) use macos::{machine_model, memory};

pub(super) fn load_average() -> io::Result<[f64; 3]> {
    let mut values = [0_f64; 3];
    // SAFETY: `values` holds the three doubles asked for.
    if unsafe { libc::getloadavg(values.as_mut_ptr(), 3) } != 3 {
        return Err(io::Error::other("getloadavg gave fewer than three"));
    }
    Ok(values)
}

pub(super) fn raise_open_files(wanted: u64) -> io::Result<Option<u64>> {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `limit` is a valid rlimit for the kernel to fill and read.
    unsafe {
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) != 0 {
            return Err(io::Error::last_os_error());
        }
        // A u64 on every Unix amux builds for.
        let wanted: libc::rlim_t = wanted;
        if limit.rlim_cur < wanted {
            limit.rlim_cur = wanted.min(limit.rlim_max);
            if libc::setrlimit(libc::RLIMIT_NOFILE, &limit) != 0 {
                return Err(io::Error::last_os_error());
            }
        }
    }
    Ok(Some(limit.rlim_cur))
}

pub(super) fn exit_signal(status: &std::process::ExitStatus) -> Option<i32> {
    status.signal()
}
