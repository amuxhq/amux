//! What node alone asks of the operating system, one file per OS, so the
//! rest of node writes no `cfg` for it: this boot of the machine.

use std::io;

#[cfg(any(target_os = "linux", target_os = "android"))]
mod linux;
#[cfg(any(target_os = "linux", target_os = "android"))]
use linux as imp;
#[cfg(any(target_os = "macos", target_os = "ios"))]
mod macos;
#[cfg(any(target_os = "macos", target_os = "ios"))]
use macos as imp;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as imp;

/// This boot of the machine: changes at every reboot and at nothing else.
pub(crate) fn boot_id() -> io::Result<String> {
    imp::boot_id()
}

#[cfg(not(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "linux",
    target_os = "android",
    windows
)))]
mod imp {
    use std::io;

    pub fn boot_id() -> io::Result<String> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "this platform has no boot id",
        ))
    }
}
