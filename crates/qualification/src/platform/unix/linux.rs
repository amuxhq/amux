//! Linux: the resident set from `/proc/<pid>/statm`, and the DMI product
//! name.

use std::io;

use crate::perf::{MemoryMetric, MemorySample};

pub(in crate::platform) fn memory(pid: u32) -> io::Result<MemorySample> {
    let statm = std::fs::read_to_string(format!("/proc/{pid}/statm"))?;
    let resident_pages = statm
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "statm has no RSS field"))?
        .parse::<u64>()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    // SAFETY: sysconf has no pointer arguments and _SC_PAGESIZE has no
    // side effects. A non-positive answer is handled as an error.
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    let page_size =
        u64::try_from(page_size).map_err(|_| io::Error::other("sysconf(_SC_PAGESIZE) failed"))?;
    let bytes = resident_pages
        .checked_mul(page_size)
        .ok_or_else(|| io::Error::other("RSS byte count overflowed"))?;
    Ok(MemorySample {
        bytes,
        metric: MemoryMetric::Rss,
    })
}

pub(in crate::platform) fn machine_model() -> io::Result<String> {
    Ok(
        std::fs::read_to_string("/sys/devices/virtual/dmi/id/product_name")?
            .trim()
            .to_owned(),
    )
}
