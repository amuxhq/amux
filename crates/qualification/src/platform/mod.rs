//! What the qualification workloads read of the OS and nothing else in
//! amux does: a process's memory by the measure each platform's report
//! promises, the machine's model that enrolls it, the load average, the
//! open-file allowance a thousand-host run needs, and the signal that ended
//! a child. One API with an implementation per OS (`unix/` with its
//! `macos.rs` and `linux.rs`, `windows.rs`), so the rest of the crate
//! writes no `cfg`.

use std::io;

use crate::perf::MemorySample;

#[cfg(unix)]
mod unix;
#[cfg(unix)]
use unix as imp;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as imp;

/// The process `pid`'s memory: macOS's physical footprint, Linux's
/// resident set, Windows' private bytes ([`crate::perf::MemoryMetric`]).
pub fn memory(pid: u32) -> io::Result<MemorySample> {
    imp::memory(pid)
}

/// The machine's hardware model (`hw.model`; Linux's DMI product name).
/// Windows: `Unsupported`, as no Windows machine is enrolled.
pub fn machine_model() -> io::Result<String> {
    imp::machine_model()
}

/// The 1, 5 and 15 minute load averages. Windows, which keeps none:
/// `Unsupported`.
pub fn load_average() -> io::Result<[f64; 3]> {
    imp::load_average()
}

/// Raises this process's allowance of open files to `wanted`, or as far
/// as its hard limit lets it, and returns the allowance it then has.
/// `None` where there is no such limit (Windows).
pub fn raise_open_files(wanted: u64) -> io::Result<Option<u64>> {
    imp::raise_open_files(wanted)
}

/// The signal that ended a child, if one did. Always `None` on Windows,
/// which ends processes with an exit code.
pub fn exit_signal(status: &std::process::ExitStatus) -> Option<i32> {
    imp::exit_signal(status)
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_open_file_allowance_is_raised_or_unlimited() {
        let allowed = super::raise_open_files(256).unwrap();
        assert!(allowed.is_none_or(|allowed| allowed >= 256), "{allowed:?}");
        // Asking for less never lowers it.
        assert_eq!(super::raise_open_files(1).unwrap(), allowed);
    }
}
