//! `<data_dir>/generation`: the installation's record of its generation,
//! which `daemon_core::generation` explains. The record holds the boot id
//! seen at the last start, whether the last run shut down cleanly, and the
//! counter. At start the counter is decided from what the last run left,
//! and the record is rewritten with the current boot id and the flag
//! cleared before anything is served; at clean shutdown the flag is set
//! last, after every store has been flushed to the drive. The counter opens
//! every session stream the host serves; a peer resuming after a cursor of
//! another generation is answered with a fresh tail that replaces its
//! replica. Both writes are fsync jobs, the same the profiles' cores ask
//! for: the record lands whole and on the drive, or not at all.

use std::io;
use std::path::Path;

use daemon_core::generation::{LastRun, at_start};
use daemon_core::payload::JobResult;
use serde::{Deserialize, Serialize};

use crate::install::GENERATION;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Generation {
    pub boot_id: String,
    /// Set only by a clean shutdown, as its last write.
    pub clean: bool,
    /// The host's generation, carried on its host row.
    pub counter: u64,
    /// The version of the build that wrote the file, so the next start can
    /// tell an update, a rollback or a crash of which build.
    pub version: String,
}

impl Generation {
    /// Reads the file, or None before the first start.
    pub fn read(data_dir: &Path) -> io::Result<Option<Self>> {
        match std::fs::read(data_dir.join(GENERATION)) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// The start-time write: bumps the counter when the machine rebooted
    /// under a daemon that never shut down cleanly, then records this boot
    /// and `version` with the flag cleared. Returns what it wrote and what
    /// was there before; nothing may be served before it returns.
    pub async fn start(
        data_dir: &Path,
        boot_id: &str,
        version: &str,
    ) -> io::Result<(Self, Option<Self>)> {
        let last = Self::read(data_dir)?;
        let counter = at_start(
            last.as_ref().map(|last| LastRun {
                boot_id: &last.boot_id,
                clean: last.clean,
                counter: last.counter,
            }),
            boot_id,
        );
        let now = Self {
            boot_id: boot_id.to_owned(),
            clean: false,
            counter,
            version: version.to_owned(),
        };
        now.write(data_dir).await?;
        Ok((now, last))
    }

    /// Whether this run crashed, as a later start on `boot_id` reads it: it
    /// never shut down cleanly and the machine did not reboot under it.
    pub fn crashed_before(&self, boot_id: &str) -> bool {
        !self.clean && self.boot_id == boot_id
    }

    /// The last write of a clean shutdown, after every store is flushed.
    pub async fn mark_clean(&self, data_dir: &Path) -> io::Result<()> {
        Self {
            clean: true,
            ..self.clone()
        }
        .write(data_dir)
        .await
    }

    async fn write(&self, data_dir: &Path) -> io::Result<()> {
        let bytes = serde_json::to_vec(self).map_err(io::Error::other)?;
        match crate::driver::fsync(data_dir.join(GENERATION), bytes).await {
            JobResult::Done => Ok(()),
            JobResult::Failed(why) => Err(io::Error::other(why)),
            other => Err(io::Error::other(format!("{other:?}"))),
        }
    }
}

/// This boot of the machine: changes at every reboot and at nothing else.
pub fn boot_id() -> io::Result<String> {
    imp::boot_id()
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
mod imp {
    use std::io;

    pub fn boot_id() -> io::Result<String> {
        let name = c"kern.bootsessionuuid";
        let mut buf = [0u8; 64];
        let mut len = buf.len();
        // SAFETY: `buf` and `len` describe a writable buffer that outlives
        // the call; the name is a NUL-terminated string.
        let status = unsafe {
            libc::sysctlbyname(
                name.as_ptr(),
                buf.as_mut_ptr().cast(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        if status != 0 {
            return Err(io::Error::last_os_error());
        }
        let text = &buf[..len.min(buf.len())];
        let text = text.split(|byte| *byte == 0).next().unwrap_or_default();
        Ok(String::from_utf8_lossy(text).into_owned())
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
mod imp {
    use std::io;

    pub fn boot_id() -> io::Result<String> {
        Ok(std::fs::read_to_string("/proc/sys/kernel/random/boot_id")?
            .trim()
            .to_owned())
    }
}

#[cfg(windows)]
mod imp {
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
