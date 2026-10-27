//! Own and replica retention: keeps the profile's rows under their budgets.
//!
//! The sweeps are the core's (see `daemon_core::retention`): it removes
//! what the store says goes, ends the removed agents' streams and tells the
//! fleet. What is left here is the interval the sweeps run on, on the
//! runtime's clock, the removed agents' directories, which take their blobs
//! with them, and the replica blob files, swept to their budget least
//! recently read first, never a catalogue a remaining replica's snapshot
//! names.

use std::path::PathBuf;

use daemon_core::Retain;
use store::{BlobLru, Sweep};
use tokio::task::JoinHandle;

use crate::install::REPLICAS;
use crate::runtime::{ProfileRuntime, RegistryError};

/// The retention sweeps run so far.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Retention {
    /// How many sweeps have finished.
    pub runs: u64,
    /// What the last one did.
    pub last: Option<Sweep>,
    /// What the last one did to replica rows.
    pub replicas: Option<Sweep>,
    /// The replica blob files the last one deleted.
    pub replica_blobs: Vec<std::path::PathBuf>,
}

impl ProfileRuntime {
    /// Runs one retention pass under the current budgets: own rows, then
    /// replica rows, then replica blob files. Returns what it did.
    pub async fn sweep_retention(&self) -> Result<Retention, RegistryError> {
        let launch = self.launch();
        let last_used = self.replicas_last_used();
        let retain = Retain {
            own_budget_bytes: launch.own_budget_bytes,
            chunk_bytes: launch.retention_chunk_bytes,
            replica_budget_bytes: launch.replica_budget_bytes,
            last_used,
        };
        let retained = self
            .driver()
            .retain(retain)
            .await
            .ok_or(crate::runtime::Stopped)?;
        for dir in &retained.removed_dirs {
            agent_dir::local_socket::forget_short_addresses(dir);
            match std::fs::remove_dir_all(dir) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        let own = retained.own.map_err(RegistryError::Core);
        let replicas = retained.replicas.map_err(RegistryError::Core);
        let blobs = match &replicas {
            Ok(_) => Some(self.sweep_replica_blobs(&retained.pinned)),
            Err(_) => None,
        };
        let mut done = Retention::default();
        self.retention.send_modify(|retention| {
            if let Ok(sweep) = &own {
                retention.runs += 1;
                retention.last = Some(sweep.clone());
            }
            if let (Ok(sweep), Some(Ok(blobs))) = (&replicas, &blobs) {
                retention.replicas = Some(sweep.clone());
                retention.replica_blobs = blobs.clone();
            }
            done = retention.clone();
        });
        own?;
        replicas?;
        blobs.transpose()?;
        Ok(done)
    }

    /// Sweeps replica blob files to their budget, least recently read
    /// first, keeping `pinned`.
    fn sweep_replica_blobs(
        &self,
        pinned: &std::collections::HashSet<PathBuf>,
    ) -> Result<Vec<PathBuf>, RegistryError> {
        let launch = self.launch();
        let root = self.dir().join(REPLICAS);
        let now = self.clock_now();
        let mut blobs = self.replica_blobs.lock().unwrap();
        if blobs.is_none() {
            *blobs = Some(BlobLru::scan(&root, now)?);
        }
        Ok(blobs
            .as_mut()
            .expect("scanned above")
            .sweep(launch.replica_blob_budget_bytes, pinned)?)
    }

    /// The sweeps run so far, and each one as it finishes.
    pub fn retention(&self) -> tokio::sync::watch::Receiver<Retention> {
        self.retention.subscribe()
    }

    pub(crate) fn start_retention(&self) -> JoinHandle<()> {
        let runtime = self.me.clone();
        tokio::spawn(async move {
            loop {
                let Some(me) = runtime.upgrade() else { return };
                if let Err(error) = me.sweep_retention().await {
                    tracing::warn!(%error, "the retention sweep failed");
                }
                let clock = me.clock().clone();
                let next = clock.now_ms() + me.launch().retention_interval_ms;
                drop(me);
                clock.sleep_until(next).await;
            }
        })
    }
}
