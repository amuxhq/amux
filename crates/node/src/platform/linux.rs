//! Linux and Android.

use std::io;

/// The kernel's random id for this boot.
pub fn boot_id() -> io::Result<String> {
    Ok(std::fs::read_to_string("/proc/sys/kernel/random/boot_id")?
        .trim()
        .to_owned())
}
