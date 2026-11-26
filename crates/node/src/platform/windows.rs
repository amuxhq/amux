//! Windows.

use std::io;
use std::time::{SystemTime, UNIX_EPOCH};

/// Windows names no boot id; the boot time, from the wall clock minus
/// the uptime, is one. It is rounded to ten seconds because the two
/// readings drift apart by milliseconds; a reading that lands across a
/// rounding edge looks like a reboot, which bumps the generation only
/// after an unclean shutdown, the safe direction.
pub fn boot_id() -> io::Result<String> {
    // SAFETY: no arguments; reads the system's uptime counter.
    let uptime_ms = unsafe { windows_sys::Win32::System::SystemInformation::GetTickCount64() };
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_millis() as u64;
    Ok(format!(
        "boot-{}",
        now_ms.saturating_sub(uptime_ms) / 10_000
    ))
}
