//! Own retention: keeps the profile's own rows under their budget.
//!
//! The store decides what goes (exited agents whole, least recently active
//! first, never an exited child whose parent is live; then the largest live
//! agent trimmed a chunk at a time, never below its newest rows) and says
//! which agents it removed. The daemon then does what the store cannot:
//! ends their streams, tells the fleet they are gone and deletes their
//! directories, which takes their blobs with them. The sweep runs when the
//! background work starts and then on its interval, on the runtime's clock.

use std::collections::HashSet;
use std::io;
use std::path::PathBuf;

use store::{BlobLru, Sweep};
use tokio::task::JoinHandle;
use uuid::Uuid;
use wire::RemovalReason;

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
    /// Runs one own retention sweep under the current budget.
    pub async fn sweep_retention(&self) -> Result<Sweep, RegistryError> {
        let launch = self.launch();
        let sweep = {
            let mut store = self.store.lock().await;
            let sweep = store.sweep_own(
                launch.own_budget_bytes,
                launch.retention_chunk_bytes,
                launch.tail_rows,
            )?;
            for key in &sweep.removed {
                self.forget_draft(&key.agent);
                // Gone for good, as a delete is: the core has its children
                // here follow.
                self.removed(key, RemovalReason::Retention);
            }
            sweep
        };
        for key in &sweep.removed {
            let Ok(id) = Uuid::from_slice(&key.agent) else {
                continue;
            };
            // Only if its row did not come back in the meantime: a running
            // process keeps its directory. A resume finds the row gone and
            // starts nothing. A directory left behind by a crash here has
            // no row, and the next start removes it.
            if self.store.lock().await.agent(key)?.is_some() {
                continue;
            }
            match std::fs::remove_dir_all(self.agent_dir(id)) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        self.retention.send_modify(|retention| {
            retention.runs += 1;
            retention.last = Some(sweep.clone());
        });
        Ok(sweep)
    }

    /// Runs one replica retention sweep: rows to their budget, evicting
    /// agents with no source least recently used first and trimming the
    /// rest to K with their block kept whole, then replica blob files to
    /// theirs, least recently read first. The catalogue a remaining
    /// replica's current snapshot names is never evicted: without it the
    /// agent's picker would lose its offers while the origin is away.
    pub async fn sweep_replica_retention(&self) -> Result<(Sweep, Vec<PathBuf>), RegistryError> {
        let launch = self.launch();
        let (sourced, last_used): (HashSet<_>, _) = self.sources_in_use();
        let (sweep, pinned) = {
            let mut store = self.store.lock().await;
            let sweep = store.sweep_replicas(
                launch.replica_budget_bytes,
                launch.tail_rows,
                &sourced,
                &last_used,
            )?;
            let pinned = store
                .agents()?
                .into_iter()
                .filter(|row| row.agent.host != self.host().as_bytes())
                .filter_map(|row| {
                    let hash = row.snapshot.as_ref()?.catalogue.as_ref()?;
                    Some(self.blob_path(&row, hash))
                })
                .collect::<HashSet<_>>();
            (sweep, pinned)
        };
        let root = self.dir().join(REPLICAS);
        let now = self.clock_now();
        let mut blobs = self.replica_blobs.lock().unwrap();
        if blobs.is_none() {
            *blobs = Some(BlobLru::scan(&root, now)?);
        }
        let removed = blobs
            .as_mut()
            .expect("scanned above")
            .sweep(launch.replica_blob_budget_bytes, &pinned)?;
        drop(blobs);
        Ok((sweep, removed))
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
                match me.sweep_replica_retention().await {
                    Ok((sweep, blobs)) => me.retention.send_modify(|retention| {
                        retention.replicas = Some(sweep);
                        retention.replica_blobs = blobs;
                    }),
                    Err(error) => tracing::warn!(%error, "the replica retention sweep failed"),
                }
                let clock = me.clock().clone();
                let next = clock.now_ms() + me.launch().retention_interval_ms;
                drop(me);
                clock.sleep_until(next).await;
            }
        })
    }
}
