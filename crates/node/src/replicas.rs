//! The shell's side of replication. The profile's daemon core keeps the
//! replicas current: it follows each trusted host's inventory, opens every
//! replica agent's source and absorbs what they bring (see
//! `daemon_core::sources`). What is left here is what the core cannot see
//! yet: which hosts the trust store trusts, which ones have taken back a
//! revocation, the published host set, a dropped replica's files, and the
//! reads the shell makes of what the core decided.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

pub use daemon_core::SourcePolicy;
use daemon_core::{HostId as CoreHost, Input};
use store::AgentKey;
use tokio::task::JoinHandle;
use uuid::Uuid;
use wire::{InventoryEvent, SessionEvent};

use crate::HostId;
use crate::routing::core_host;
use crate::runtime::ProfileRuntime;

/// How often the shell looks at the trust store for hosts trusted or
/// untrusted, and at the hosts that took back a revocation, and recomputes
/// the published host set.
const TRUST_POLL: Duration = Duration::from_millis(25);

/// For tests: what a source or an inventory follower does with an event it
/// has just received.
#[doc(hidden)]
pub enum SourceVerdict {
    Keep,
    /// Drop the stream here, as a link dying at this point would.
    Drop,
    /// Hold the event until the future finishes, as a slow stream would,
    /// then use it.
    Hold(std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>),
}

/// For tests: consulted by every source for every event it receives, before
/// the event reaches the core.
#[doc(hidden)]
pub type SourceHook = Arc<dyn Fn(&AgentKey, &SessionEvent) -> SourceVerdict + Send + Sync>;

/// For tests: consulted by every inventory follower for every event it
/// receives from the host it follows, before the event reaches the core.
#[doc(hidden)]
pub type InventoryHook = Arc<dyn Fn(HostId, &InventoryEvent) -> SourceVerdict + Send + Sync>;

/// What the shell keeps of replication.
#[derive(Default)]
pub(crate) struct Replicas {
    /// The policy as last set, which the core was told.
    policy: SourcePolicy,
    /// The replica agents with a source open, as the core last said.
    open: HashSet<AgentKey>,
    /// Hosts whose inventory has caught up on a live stream, as the core
    /// last said.
    ready: HashSet<Vec<u8>>,
    /// When a client last subscribed to or paged a replica, on the policy
    /// clock: the replica retention sweep's least-recently-used order.
    last_used: HashMap<AgentKey, i64>,
    /// Tells the core of trust changes and revocations taken back.
    watcher: Option<JoinHandle<()>>,
    pub(crate) hook: Option<SourceHook>,
    pub(crate) inventory_hook: Option<InventoryHook>,
}

impl Replicas {
    pub(crate) fn take_watcher(&mut self) -> Option<JoinHandle<()>> {
        self.watcher.take()
    }
}

impl ProfileRuntime {
    /// Sets which replica agents keep a source open; the core sweeps once.
    pub fn set_source_policy(&self, policy: SourcePolicy) {
        let changed = {
            let mut replicas = self.replicas.lock().unwrap();
            let changed = replicas.policy != policy;
            replicas.policy = policy;
            changed
        };
        if changed {
            self.driver().post(Input::Policy(policy));
        }
    }

    pub fn source_policy(&self) -> SourcePolicy {
        self.replicas.lock().unwrap().policy
    }

    /// The replica agents with a source open now.
    pub fn open_sources(&self) -> Vec<AgentKey> {
        let mut open: Vec<AgentKey> = self.replicas.lock().unwrap().open.iter().cloned().collect();
        open.sort();
        open
    }

    /// Whether `host`'s inventory has caught up on a live stream, so its
    /// agents may have sources.
    pub fn host_ready(&self, host: HostId) -> bool {
        self.replicas
            .lock()
            .unwrap()
            .ready
            .contains(host.as_bytes().as_slice())
    }

    /// For tests: consulted by every source for every event it receives.
    #[doc(hidden)]
    pub fn set_source_hook(&self, hook: Option<SourceHook>) {
        self.replicas.lock().unwrap().hook = hook;
    }

    /// For tests: consulted by every inventory follower for every event it
    /// receives.
    #[doc(hidden)]
    pub fn set_inventory_hook(&self, hook: Option<InventoryHook>) {
        self.replicas.lock().unwrap().inventory_hook = hook;
    }

    /// When clients last used each replica: what the replica retention
    /// sweep evicts by.
    pub(crate) fn replicas_last_used(&self) -> HashMap<AgentKey, i64> {
        self.replicas.lock().unwrap().last_used.clone()
    }

    /// Records a client's use of the agent `agent_id` names, when it is a
    /// replica, for the retention sweep.
    pub(crate) fn used(&self, store: &store::Sqlite, agent_id: &[u8]) {
        let Ok(Some(row)) = store.agent_by_id(agent_id) else {
            return;
        };
        if row.agent.host == store.own_host() {
            return;
        }
        let now = self.clock_now();
        self.replicas
            .lock()
            .unwrap()
            .last_used
            .insert(row.agent, now);
    }

    /// The core opened a source for the agent, or closed it.
    pub(crate) fn sourcing(&self, agent: AgentKey, open: bool) {
        let mut replicas = self.replicas.lock().unwrap();
        if open {
            replicas.open.insert(agent);
        } else {
            replicas.open.remove(&agent);
        }
    }

    /// The core found `host`'s rows current, or no longer: its published
    /// entry says so.
    pub(crate) fn host_current(&self, host: CoreHost, current: bool) {
        {
            let mut replicas = self.replicas.lock().unwrap();
            if current {
                replicas.ready.insert(host.0.to_vec());
            } else {
                replicas.ready.remove(host.0.as_slice());
            }
        }
        self.host_current_changed(&host.0, current);
    }

    /// The core dropped the replica from the store: its directory and blob
    /// files go.
    pub(crate) fn replica_dropped(&self, agent: &AgentKey) {
        self.replicas.lock().unwrap().last_used.remove(agent);
        if let (Ok(host), Ok(id)) = (
            Uuid::from_slice(&agent.host),
            Uuid::from_slice(&agent.agent),
        ) {
            let dir = self
                .dir()
                .join(crate::install::REPLICAS)
                .join(host.to_string())
                .join(crate::install::AGENTS)
                .join(id.to_string());
            if let Some(blobs) = self.replica_blobs.lock().unwrap().as_mut() {
                blobs.forget_under(&dir);
            }
            if let Err(error) = std::fs::remove_dir_all(&dir)
                && error.kind() != std::io::ErrorKind::NotFound
            {
                tracing::warn!(%error, "removing a dropped replica's directory failed");
            }
        }
    }

    /// Starts telling the core which paired hosts took back a revocation,
    /// and keeping the published host set current.
    pub(crate) fn start_replication(&self) {
        let runtime = self.me.clone();
        let watcher = tokio::spawn(async move {
            let mut revoked: HashSet<HostId> = HashSet::new();
            loop {
                {
                    let Some(me) = runtime.upgrade() else { return };
                    if let Some(edge) = me.edge() {
                        // A host that takes back its revocation, or is
                        // paired again, takes this host's calls again: its
                        // streams open at once rather than after a backoff
                        // grown on refusals.
                        let now: HashSet<HostId> = edge
                            .trusted()
                            .into_iter()
                            .map(|(host, ..)| host)
                            .filter(|host| edge.revoked(*host))
                            .collect();
                        for host in revoked.difference(&now) {
                            me.driver().report(Input::TryAgain {
                                host: core_host(*host),
                            });
                        }
                        revoked = now;
                    }
                    me.sync_hosts().await;
                }
                tokio::time::sleep(TRUST_POLL).await;
            }
        });
        if let Some(old) = self.replicas.lock().unwrap().watcher.replace(watcher) {
            old.abort();
        }
    }

    /// Stops telling the core of trust, and lets go of every stream the
    /// core opened of a peer, without waiting.
    pub(crate) fn stop_replication(&self) {
        if let Some(watcher) = self.replicas.lock().unwrap().take_watcher() {
            watcher.abort();
        }
        self.driver().hang_up_all();
    }
}
