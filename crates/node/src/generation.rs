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
    crate::platform::boot_id()
}
