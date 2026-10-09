//! Peer sources: how a runtime keeps its replicas of other hosts' agents
//! current.
//!
//! Every trusted host has one follower. While the host is reachable it
//! holds the host's SubscribeInventory stream: at the stream's CaughtUp it
//! records the host's generation, writes the rows the host lists and
//! drops the ones it no longer lists, and only then marks the host ready.
//! That is what "reachable" means for a source.
//!
//! Every replica agent of a ready host may have one source: one Subscribe
//! to the origin. The agents the store remembers of a host get theirs as
//! the host's inventory is subscribed, in the same flight, so a launch
//! reconciles in one round trip to each host rather than two; the
//! inventory's CaughtUp confirms them, and drops any for an agent the host
//! no longer lists. With no block it asks for a tail of K, with one for what
//! came after its cursor, capped at K, naming the origin generation the
//! cursor was taken under; the origin answers a delta when it fits and the
//! generation is the one it runs, and a Reset and a fresh tail otherwise.
//! Every stream opens with the origin's generation, which the block takes
//! at its Reset, so a rewound origin (an unclean reboot mints the same
//! orders and revisions again for different content) resets each of its
//! replicas on that replica's own stream, forgetting every row of the old
//! generation. The source holds a catch-up until the origin's
//! CaughtUp and then absorbs it in one go, so the block is never left
//! half-replaced by a stream that died midway; the cursor moves to the
//! revision CaughtUp carries, then per live record. Records are absorbed
//! before they are broadcast. A stream that ends is Detached to local
//! subscribers and retried after the cursor with backoff on the policy
//! clock while the host stays reachable. A source for an exited agent
//! closes once its catch-up has landed.
//!
//! Which agents get a source is the runtime's [`SourcePolicy`], read by
//! one sweep after every inventory catch-up and change and once when the
//! policy changes; a client's Subscribe opens one for any agent the policy
//! skipped. Under `OnDemand` the sweep keeps exactly the agents a client is
//! subscribed to.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Weak};
use std::time::Duration;

use store::{
    Absorb, AgentKey, AgentRow, Backend as _, Marker, Record, SourceEvent, Store as _, StoreError,
};
use tokio::task::JoinHandle;
use uuid::Uuid;
use wire::{
    Agent, AgentRemoved, Detached, Empty, FetchRequest, FetchResponse, InventoryEvent, Item,
    PeerSubscribeRequest, RemovalReason, SessionEvent, inventory_event, peer_subscribe_request,
    session_event,
};

use crate::HostId;
use crate::routing::HostVia;
use crate::runtime::ProfileRuntime;
use crate::serve::{event, inventory};

/// How often a follower looks at whether its host has a route, and at
/// the trust store for hosts to follow. A route's arrival is not a policy
/// timer: a restored link is used as soon as it is seen.
const ROUTE_POLL: Duration = Duration::from_millis(25);
/// How long a Fetch waits on the origin before it answers unreachable.
const FETCH_PATIENCE: Duration = Duration::from_secs(10);

/// Which replica agents the runtime keeps a source open for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SourcePolicy {
    /// Every agent a reachable host lists. A desktop daemon, and the
    /// phone's runtime in the foreground.
    #[default]
    Listed,
    /// Only agents a client subscribes to: the phone's runtime woken in
    /// the background, bringing the one chat a notification names current.
    OnDemand,
}

/// For tests: what a source or an inventory follower does with an event
/// it has just received.
#[doc(hidden)]
pub enum SourceVerdict {
    Keep,
    /// Drop the stream here, as a link dying at this point would.
    Drop,
    /// Hold the event until the future finishes, as a slow stream would,
    /// then use it.
    Hold(std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>),
}

/// For tests: consulted by every source for every event it receives,
/// before the event is used.
#[doc(hidden)]
pub type SourceHook = Arc<dyn Fn(&AgentKey, &SessionEvent) -> SourceVerdict + Send + Sync>;

/// For tests: consulted by every inventory follower for every event it
/// receives from the host it follows, before the event is used.
#[doc(hidden)]
pub type InventoryHook = Arc<dyn Fn(HostId, &InventoryEvent) -> SourceVerdict + Send + Sync>;

/// The runtime's replication state.
#[derive(Default)]
pub(crate) struct Sources {
    policy: SourcePolicy,
    /// Watches the trust store and keeps one follower per trusted host.
    manager: Option<JoinHandle<()>>,
    followers: HashMap<HostId, JoinHandle<()>>,
    /// Hosts whose inventory has caught up on the current stream, by the
    /// session that stream is. A session ends with its stream.
    ready: HashMap<Vec<u8>, u64>,
    /// Hosts whose inventory is subscribed but has not caught up, by the
    /// session it will confirm: the sources opened with it are current
    /// from the start, so one that is lost waits out its backoff like any
    /// other rather than ending.
    following: HashMap<Vec<u8>, u64>,
    next_session: u64,
    /// Open sources and the host session each belongs to.
    open: HashMap<AgentKey, (u64, JoinHandle<()>)>,
    /// Exited agents whose catch-up has landed: nothing more will come, so
    /// no source opens for them until their host lists them live again.
    settled: HashSet<AgentKey>,
    /// Exited agents whose source was reopened to settle them.
    settling: HashSet<AgentKey>,
    /// When a client last subscribed to or paged a replica, on the policy
    /// clock: the replica retention sweep's least-recently-used order.
    last_used: HashMap<AgentKey, i64>,
    hook: Option<SourceHook>,
    inventory_hook: Option<InventoryHook>,
}

impl Sources {
    /// Stops every task, without waiting.
    pub(crate) fn abort_all(&mut self) {
        if let Some(manager) = self.manager.take() {
            manager.abort();
        }
        for (_, follower) in self.followers.drain() {
            follower.abort();
        }
        for (_, (_, source)) in self.open.drain() {
            source.abort();
        }
        self.settling.clear();
        self.ready.clear();
        self.following.clear();
    }

    /// Whether `session` is the one `host`'s agents are sourced under now,
    /// caught up or still following.
    fn is_current(&self, host: &[u8], session: u64) -> bool {
        self.ready.get(host) == Some(&session) || self.following.get(host) == Some(&session)
    }
}

/// Doubling backoff between reconnects, from the policy's first wait to
/// its ceiling; reset once a stream has caught up.
struct Backoff {
    first_ms: i64,
    max_ms: i64,
    next_ms: i64,
}

impl Backoff {
    fn new(first_ms: i64, max_ms: i64) -> Self {
        Self {
            first_ms: first_ms.max(1),
            max_ms: max_ms.max(first_ms.max(1)),
            next_ms: first_ms.max(1),
        }
    }

    fn next(&mut self) -> i64 {
        let wait = self.next_ms;
        self.next_ms = (self.next_ms * 2).min(self.max_ms);
        wait
    }

    fn reset(&mut self) {
        self.next_ms = self.first_ms;
    }
}

/// How one source stream ended.
enum Ended {
    /// The stream is gone; `caught_up` if it had reached CaughtUp.
    Lost { caught_up: bool },
    /// The agent exited and its catch-up has landed: close for good.
    Settled,
    /// The agent's row is gone or its host's session ended: stop.
    Stale,
}

/// A catch-up held until the origin's CaughtUp.
struct CatchUp {
    /// The origin sent a Reset, or this was a tail request: what arrives
    /// replaces the block.
    reset: bool,
    events: Vec<SourceEvent>,
}

impl ProfileRuntime {
    // --- policy and introspection --------------------------------------------

    /// Sets which replica agents keep a source open, and sweeps once.
    pub fn set_source_policy(&self, policy: SourcePolicy) {
        let changed = {
            let mut sources = self.sources.lock().unwrap();
            let changed = sources.policy != policy;
            sources.policy = policy;
            changed
        };
        if changed {
            self.sweep_sources_soon();
        }
    }

    pub fn source_policy(&self) -> SourcePolicy {
        self.sources.lock().unwrap().policy
    }

    /// The replica agents with a source open now.
    pub fn open_sources(&self) -> Vec<AgentKey> {
        let mut open: Vec<AgentKey> = self.sources.lock().unwrap().open.keys().cloned().collect();
        open.sort();
        open
    }

    /// Whether `host`'s inventory has caught up on a live stream, so its
    /// agents may have sources.
    pub fn host_ready(&self, host: HostId) -> bool {
        self.sources
            .lock()
            .unwrap()
            .ready
            .contains_key(host.as_bytes().as_slice())
    }

    /// For tests: consulted by every source for every event it receives.
    #[doc(hidden)]
    pub fn set_source_hook(&self, hook: Option<SourceHook>) {
        self.sources.lock().unwrap().hook = hook;
    }

    /// For tests: consulted by every inventory follower for every event it
    /// receives.
    #[doc(hidden)]
    pub fn set_inventory_hook(&self, hook: Option<InventoryHook>) {
        self.sources.lock().unwrap().inventory_hook = hook;
    }

    /// The agents with a source open, and when clients last used each
    /// replica: what the replica retention sweep keeps and orders by.
    pub(crate) fn sources_in_use(&self) -> (HashSet<AgentKey>, HashMap<AgentKey, i64>) {
        let sources = self.sources.lock().unwrap();
        (
            sources.open.keys().cloned().collect(),
            sources.last_used.clone(),
        )
    }

    pub(crate) fn source_open(&self, key: &AgentKey) -> bool {
        self.sources.lock().unwrap().open.contains_key(key)
    }

    /// Records a client's use of a replica, for the retention sweep.
    pub(crate) fn used(&self, key: &AgentKey) {
        let now = self.clock_now();
        self.sources
            .lock()
            .unwrap()
            .last_used
            .insert(key.clone(), now);
    }

    // --- followers -----------------------------------------------------------

    /// Starts following every trusted host, and every host trusted later.
    pub(crate) fn start_replication(&self) {
        let runtime = self.me.clone();
        let manager = tokio::spawn(async move {
            let mut first = true;
            loop {
                {
                    let Some(me) = runtime.upgrade() else { return };
                    let untrusted = me.sync_followers();
                    if first || !untrusted.is_empty() {
                        me.drop_untrusted_replicas().await;
                        first = false;
                    }
                    me.sync_hosts().await;
                }
                tokio::time::sleep(ROUTE_POLL).await;
            }
        });
        if let Some(old) = self.sources.lock().unwrap().manager.replace(manager) {
            old.abort();
        }
    }

    /// Keeps one follower per trusted host. Returns the hosts that are no
    /// longer trusted.
    fn sync_followers(&self) -> Vec<HostId> {
        let Some(edge) = self.edge() else {
            return Vec::new();
        };
        let trusted: HashSet<HostId> = edge.trusted().into_iter().map(|(host, ..)| host).collect();
        let mut gone = Vec::new();
        {
            let mut sources = self.sources.lock().unwrap();
            sources.followers.retain(|host, follower| {
                let keep = trusted.contains(host) && !follower.is_finished();
                if !keep {
                    follower.abort();
                    gone.push(*host);
                }
                keep
            });
            for host in &trusted {
                if !sources.followers.contains_key(host) {
                    let follower = tokio::spawn(follow(self.me.clone(), *host));
                    sources.followers.insert(*host, follower);
                }
            }
        }
        let untrusted = gone
            .iter()
            .filter(|host| !trusted.contains(host))
            .copied()
            .collect();
        for host in gone {
            self.host_lost(host);
        }
        untrusted
    }

    /// Drops every replica of a host the profile no longer trusts: its
    /// entry leaves the host set, and its agents go with it.
    async fn drop_untrusted_replicas(&self) {
        let Some(edge) = self.edge() else { return };
        let trusted: HashSet<Vec<u8>> = edge
            .trusted()
            .into_iter()
            .map(|(host, ..)| host.as_bytes().to_vec())
            .collect();
        let own = self.host().as_bytes().to_vec();
        let mut store = self.store.lock().await;
        let Ok(rows) = store.agents() else { return };
        for key in rows.into_iter().map(|row| row.agent) {
            if key.host != own
                && !trusted.contains(&key.host)
                && let Err(error) =
                    self.drop_replica(&mut store, &key, RemovalReason::HostUntrusted)
            {
                tracing::warn!(%error, "dropping an untrusted host's replica failed");
            }
        }
    }

    /// The host's session ended: no source of it may absorb anything more,
    /// and every chat on its agents is told the rows may be stale.
    fn host_lost(&self, host: HostId) {
        let host_bytes = host.as_bytes().to_vec();
        let stopped: Vec<(AgentKey, JoinHandle<()>)> = {
            let mut sources = self.sources.lock().unwrap();
            sources.ready.remove(&host_bytes);
            sources.following.remove(&host_bytes);
            let keys: Vec<AgentKey> = sources
                .open
                .keys()
                .filter(|key| key.host == host_bytes)
                .cloned()
                .collect();
            keys.into_iter()
                .filter_map(|key| sources.open.remove(&key).map(|(_, task)| (key, task)))
                .collect()
        };
        for (_, task) in &stopped {
            task.abort();
        }
        let runtime = self.me.clone();
        tokio::spawn(async move {
            // Aborted tasks let go at their next await; none absorbs past
            // this, since each absorbs and publishes without awaiting.
            for (_, task) in stopped {
                let _ = task.await;
            }
            let Some(me) = runtime.upgrade() else { return };
            let mut store = me.store.lock().await;
            me.host_current_changed(&host_bytes, false);
            let Ok(rows) = store.agents() else { return };
            for row in rows.iter().filter(|row| row.agent.host == host_bytes) {
                me.detach(&mut store, &row.agent);
            }
        });
    }

    /// Tells an agent's subscribers the rows may be stale, once.
    fn detach(&self, store: &mut store::Sqlite, key: &AgentKey) {
        if store.markers().get(key) == Some(&Marker::Detached) {
            return;
        }
        store.set_marker(key, Some(Marker::Detached));
        self.fanout
            .publish(key, event(session_event::Of::Detached(Detached {})));
    }

    /// Begins a host's inventory session: the number its sources are keyed
    /// by until its stream ends. Under the `Listed` policy this also opens
    /// a source for every agent the store remembers of the host, so their
    /// subscriptions leave with the inventory's; the session is confirmed
    /// when the inventory catches up, and an agent it no longer lists is
    /// dropped then, source and all.
    async fn begin_following(&self, host: HostId) -> u64 {
        let host_bytes = host.as_bytes().to_vec();
        let store = self.store.lock().await;
        let rows = store.agents().unwrap_or_default();
        let mut sources = self.sources.lock().unwrap();
        sources.next_session += 1;
        let session = sources.next_session;
        sources.following.insert(host_bytes.clone(), session);
        if sources.policy == SourcePolicy::Listed {
            for row in rows.iter().filter(|row| row.agent.host == host_bytes) {
                let key = &row.agent;
                if sources.open.contains_key(key) || sources.settled.contains(key) {
                    continue;
                }
                self.open_source_in(&mut sources, key, host, session);
                // An exited agent's source only settles it; the sweep
                // would otherwise reopen it for that.
                if row.lifecycle == wire::Lifecycle::Exited as i32 {
                    sources.settling.insert(key.clone());
                }
            }
        }
        drop(sources);
        drop(store);
        session
    }

    /// The host's inventory reached CaughtUp: record its generation, write
    /// what it lists and drop what it no longer does, all under one store
    /// lock, then mark the host ready.
    async fn inventory_caught_up(
        &self,
        host: HostId,
        session: u64,
        generation: Option<u64>,
        listed: Vec<Agent>,
    ) -> Result<u64, StoreError> {
        let host_bytes = host.as_bytes().to_vec();
        let mut store = self.store.lock().await;
        let listed_keys: HashSet<Vec<u8>> =
            listed.iter().map(|agent| agent.agent_id.clone()).collect();
        let unlisted: Vec<AgentKey> = store
            .agents()?
            .into_iter()
            .map(|row| row.agent)
            .filter(|key| key.host == host_bytes && !listed_keys.contains(&key.agent))
            .collect();
        if let Some(generation) = generation
            && store.host_generation(&host_bytes)? != Some(generation)
        {
            // The replicas are not this stream's to reset: each source
            // resumes after its cursor naming the generation it was taken
            // under, and the origin answers the old one with a fresh tail.
            // An exited agent's source had settled and would not look
            // again; the sweep reopens it for that reset.
            tracing::info!(%host, generation, "the host came back under a new generation");
            store.record_host_generation(&host_bytes, generation)?;
            {
                let mut sources = self.sources.lock().unwrap();
                sources.settled.retain(|key| key.host != host_bytes);
                sources.settling.retain(|key| key.host != host_bytes);
            }
            self.host_generation_changed(&host_bytes, generation);
        }
        for key in &unlisted {
            self.drop_replica(&mut store, key, RemovalReason::NoLongerListed)?;
        }
        for agent in &listed {
            self.put_replica_row(&mut store, agent)?;
        }
        {
            let mut sources = self.sources.lock().unwrap();
            sources.following.remove(&host_bytes);
            sources.ready.insert(host_bytes.clone(), session);
        }
        self.host_current_changed(&host_bytes, true);
        // Deliveries to a parent on that host may have waited for it.
        self.deliveries_due.notify_one();
        Ok(session)
    }

    fn put_replica_row(&self, store: &mut store::Sqlite, agent: &Agent) -> Result<(), StoreError> {
        let row = from_wire(agent);
        if row.lifecycle != wire::Lifecycle::Exited as i32 {
            let mut sources = self.sources.lock().unwrap();
            sources.settled.remove(&row.agent);
            sources.settling.remove(&row.agent);
        }
        store.put_agent(&row)?;
        self.publish_row(store, &row.agent)
    }

    fn drop_replica(
        &self,
        store: &mut store::Sqlite,
        key: &AgentKey,
        reason: RemovalReason,
    ) -> Result<(), StoreError> {
        let source = {
            let mut sources = self.sources.lock().unwrap();
            sources.settled.remove(key);
            sources.settling.remove(key);
            sources.last_used.remove(key);
            sources.open.remove(key)
        };
        if let Some((_, task)) = source {
            task.abort();
        }
        store.delete_agent(key)?;
        if let (Ok(host), Ok(agent)) = (Uuid::from_slice(&key.host), Uuid::from_slice(&key.agent)) {
            let dir = self
                .dir()
                .join(crate::install::REPLICAS)
                .join(host.to_string())
                .join(crate::install::AGENTS)
                .join(agent.to_string());
            if let Some(blobs) = self.replica_blobs.lock().unwrap().as_mut() {
                blobs.forget_under(&dir);
            }
            if let Err(error) = std::fs::remove_dir_all(&dir)
                && error.kind() != std::io::ErrorKind::NotFound
            {
                tracing::warn!(%error, "removing a dropped replica's directory failed");
            }
        }
        self.fanout.close(key);
        self.fanout
            .publish_inventory(inventory(inventory_event::Of::AgentRemoved(AgentRemoved {
                host_id: key.host.clone(),
                agent_id: key.agent.clone(),
                reason: reason as i32,
            })));
        Ok(())
    }

    // --- the sweep -----------------------------------------------------------

    fn sweep_sources_soon(&self) {
        let runtime = self.me.clone();
        tokio::spawn(async move {
            if let Some(me) = runtime.upgrade() {
                me.sweep_sources().await;
            }
        });
    }

    /// Opens a source for every replica agent the policy wants and none is
    /// open for, and settles the ones for exited agents. Under `OnDemand`
    /// the policy wants only agents a client is subscribed to: the sweep
    /// closes every other source, so a runtime switched to it keeps nothing
    /// current but what is being watched, and opens one for a watched agent
    /// whose host was not ready when the client subscribed.
    pub(crate) async fn sweep_sources(&self) {
        let mut store = self.store.lock().await;
        let Ok(rows) = store.agents() else { return };
        let own = self.host().as_bytes().to_vec();
        let mut settle = Vec::new();
        let mut unwatched = Vec::new();
        {
            let mut sources = self.sources.lock().unwrap();
            if sources.policy == SourcePolicy::OnDemand {
                // Sources absorb only under the store lock, which this
                // holds, so none is midway through a catch-up.
                let idle: Vec<AgentKey> = sources
                    .open
                    .keys()
                    .filter(|key| !sources.settling.contains(*key) && !self.fanout.watched(key))
                    .cloned()
                    .collect();
                for key in idle {
                    if let Some((_, task)) = sources.open.remove(&key) {
                        task.abort();
                    }
                    unwatched.push(key);
                }
            }
            for row in rows.iter().filter(|row| row.agent.host != own) {
                let key = &row.agent;
                let exited = row.lifecycle == wire::Lifecycle::Exited as i32;
                let caught_up = store.markers().get(key) == Some(&Marker::CaughtUp);
                if sources.open.contains_key(key) {
                    if exited && caught_up && !sources.settling.contains(key) {
                        settle.push(key.clone());
                    }
                    continue;
                }
                let wanted = sources.policy == SourcePolicy::Listed || self.fanout.watched(key);
                if wanted && !sources.settled.contains(key) {
                    self.open_source(&mut sources, key);
                }
            }
            // A live source's marker reads CaughtUp throughout, which says
            // nothing of the records written up to the exit: the origin
            // publishes them before the Exited row, but on another stream,
            // so they may still be on their way. The source starts again
            // after its cursor; the origin has committed them by now, and
            // the catch-up that brings them settles the agent.
            for key in settle {
                if let Some((_, task)) = sources.open.remove(&key) {
                    task.abort();
                }
                self.open_source(&mut sources, &key);
                sources.settling.insert(key);
            }
        }
        for key in unwatched {
            self.detach(&mut store, &key);
        }
    }

    /// Opens a source for a replica agent a client subscribed to, if its
    /// host is ready and none is open. Settled agents need none.
    pub(crate) fn ensure_source(&self, key: &AgentKey) {
        let mut sources = self.sources.lock().unwrap();
        if !sources.open.contains_key(key) && !sources.settled.contains(key) {
            self.open_source(&mut sources, key);
        }
    }

    fn open_source(&self, sources: &mut Sources, key: &AgentKey) {
        let Some(&session) = sources.ready.get(&key.host) else {
            return;
        };
        let Ok(host) = Uuid::from_slice(&key.host) else {
            return;
        };
        self.open_source_in(sources, key, host, session);
    }

    fn open_source_in(&self, sources: &mut Sources, key: &AgentKey, host: HostId, session: u64) {
        let task = tokio::spawn(run_source(self.me.clone(), key.clone(), host, session));
        sources.open.insert(key.clone(), (session, task));
    }

    fn session_current(&self, key: &AgentKey, session: u64) -> bool {
        self.sources.lock().unwrap().is_current(&key.host, session)
    }

    fn source_ended(&self, key: &AgentKey, session: u64, settled: bool) {
        let mut sources = self.sources.lock().unwrap();
        if sources
            .open
            .get(key)
            .is_some_and(|(open, _)| *open == session)
        {
            sources.open.remove(key);
            sources.settling.remove(key);
            if settled {
                sources.settled.insert(key.clone());
            }
        }
    }

    // --- absorbing -------------------------------------------------------------

    /// Absorbs a whole catch-up and the CaughtUp that ends it, then
    /// broadcasts what the subscribers need, all under one store lock. A
    /// Reset begins the block under `generation`, the origin's from the
    /// stream's Opening; `revision` becomes the cursor. Returns whether the
    /// agent has exited.
    async fn land_catch_up(
        &self,
        key: &AgentKey,
        session: u64,
        catch_up: CatchUp,
        revision: u64,
        generation: u64,
    ) -> Result<Option<bool>, StoreError> {
        let mut store = self.store.lock().await;
        if !self.session_current(key, session) {
            return Ok(None);
        }
        let Some(row) = store.agent(key)? else {
            return Ok(None);
        };
        if catch_up.reset {
            let (tail, snapshot, rest) = fold_tail(catch_up.events, &row);
            store.absorb(
                key,
                Absorb::Reset {
                    tail: tail.clone(),
                    snapshot,
                    generation,
                },
            )?;
            self.wrote();
            let absorbed_rest = if rest.is_empty() {
                Vec::new()
            } else {
                store
                    .absorb(
                        key,
                        Absorb::Delta {
                            events: rest,
                            live: false,
                        },
                    )?
                    .stored
            };
            // Forget the transcript; the fresh tail follows, as the store
            // now holds it.
            self.fanout
                .publish(key, event(session_event::Of::Reset(wire::Reset {})));
            let held = store.agent(key)?.unwrap_or(row);
            if let Some(snapshot) = held.snapshot {
                self.fanout
                    .publish(key, event(session_event::Of::Snapshot(snapshot)));
            }
            for item in tail {
                if let Some(stored) = store.get(key, &item.key)? {
                    self.fanout
                        .publish(key, event(session_event::Of::Item(stored)));
                }
            }
            for record in absorbed_rest {
                self.fanout.publish(key, record_event(record));
            }
        } else if !catch_up.events.is_empty() {
            let absorbed = store.absorb(
                key,
                Absorb::Delta {
                    events: catch_up.events,
                    live: false,
                },
            )?;
            self.wrote();
            for record in absorbed.stored {
                self.fanout.publish(key, record_event(record));
            }
        }
        store.absorb(key, Absorb::CaughtUp(revision))?;
        self.wrote();
        self.fanout.publish(
            key,
            event(session_event::Of::CaughtUp(wire::CaughtUp { revision })),
        );
        let row = store.agent(key)?;
        self.publish_row(&store, key)?;
        Ok(row.map(|row| row.lifecycle == wire::Lifecycle::Exited as i32))
    }

    /// Absorbs one live record and broadcasts it if it changed a row.
    async fn land_live(
        &self,
        key: &AgentKey,
        session: u64,
        record: SourceEvent,
    ) -> Result<bool, StoreError> {
        let mut store = self.store.lock().await;
        if !self.session_current(key, session) || store.agent(key)?.is_none() {
            return Ok(false);
        }
        let envelope = matches!(record, SourceEvent::Snapshot(_));
        let absorbed = store.absorb(
            key,
            Absorb::Delta {
                events: vec![record],
                live: true,
            },
        )?;
        self.wrote();
        for record in absorbed.stored {
            self.fanout.publish(key, record_event(record));
        }
        if envelope {
            self.publish_row(&store, key)?;
        }
        Ok(true)
    }

    // --- fetch below the block -------------------------------------------------

    /// Asks the origin for a page and absorbs it where it joins the block.
    pub(crate) async fn fetch_from_origin(
        &self,
        key: &AgentKey,
        before_order: Option<u64>,
        limit: u32,
    ) -> Result<FetchResponse, ()> {
        let host = Uuid::from_slice(&key.host).map_err(|_| ())?;
        let session = self
            .sources
            .lock()
            .unwrap()
            .ready
            .get(&key.host)
            .copied()
            .ok_or(())?;
        let edge = self.edge().ok_or(())?;
        let request = FetchRequest {
            agent_id: key.agent.clone(),
            before_order,
            limit,
        };
        let answer = tokio::time::timeout(FETCH_PATIENCE, async {
            let mut client = edge.peer(host).await.map_err(|_| ())?;
            drop(edge);
            client
                .fetch(request)
                .await
                .map(tonic::Response::into_inner)
                .map_err(|_| ())
        })
        .await
        .map_err(|_| ())??;
        if let Some(before_order) = before_order {
            let mut store = self.store.lock().await;
            // An answer from a session that has since ended describes a
            // generation this store may already have dropped.
            if self.session_current(key, session) && store.agent(key).ok().flatten().is_some() {
                let absorbed = store.absorb(
                    key,
                    Absorb::Page {
                        before_order,
                        items: answer.items.clone(),
                        exhausted: answer.exhausted,
                    },
                );
                match absorbed {
                    Ok(_) => self.wrote(),
                    Err(error) => {
                        tracing::warn!(%error, "absorbing a page from the origin failed");
                    }
                }
            }
        }
        Ok(answer)
    }
}

/// Keeps one host's inventory followed while it is reachable.
async fn follow(runtime: Weak<ProfileRuntime>, host: HostId) {
    let mut backoff: Option<Backoff> = None;
    loop {
        let Some(edge) = runtime.upgrade().and_then(|me| me.edge()) else {
            return;
        };
        let reachable = edge.via(host).await != HostVia::Offline;
        let route = edge.route(host).await;
        drop(edge);
        if !reachable {
            backoff = None;
            tokio::time::sleep(ROUTE_POLL).await;
            continue;
        }
        let caught_up = follow_once(&runtime, host).await;
        let Some(me) = runtime.upgrade() else { return };
        me.host_lost(host);
        let launch = me.launch();
        let backoff = backoff.get_or_insert_with(|| {
            Backoff::new(launch.source_backoff_ms, launch.source_backoff_max_ms)
        });
        if caught_up {
            backoff.reset();
        }
        let clock = me.clock().clone();
        let until = clock.now_ms() + backoff.next();
        let renewed = me.edge().map(|edge| edge.revocation_cleared(host));
        drop(me);
        if redial_early(
            wait_while_reachable(&runtime, host, route, clock.sleep_until(until)),
            renewed,
        )
        .await
        {
            backoff.reset();
        }
    }
}

/// Waits out `wait`, unless the host that refused this host's stream as no
/// longer trusted takes that back first (`renewed`), as when this host
/// pairs with it again: then true, and the stream reopens at once instead
/// of after a backoff grown on refusals.
async fn redial_early(
    wait: impl Future<Output = ()>,
    renewed: Option<impl Future<Output = ()>>,
) -> bool {
    let Some(renewed) = renewed else {
        wait.await;
        return false;
    };
    tokio::select! {
        () = wait => false,
        () = renewed => true,
    }
}

/// Waits out `wait`, unless the host's route is no longer `followed`, the
/// one the lost stream was opened on: a host that goes away, or goes and
/// comes back on a new link, is followed as soon as that is seen. The
/// comparison is with the route before the stream, not the route once it
/// has ended, because a link restored quickly is already up by the time
/// the stream's end is noticed. Only a host that stays on the route it had
/// is waited on for the whole backoff.
async fn wait_while_reachable(
    runtime: &Weak<ProfileRuntime>,
    host: HostId,
    followed: Option<crate::routing::Route>,
    wait: agent_dir::Sleep,
) {
    let moved = async {
        loop {
            let Some(edge) = runtime.upgrade().and_then(|me| me.edge()) else {
                return;
            };
            if followed.is_none() || edge.route(host).await != followed {
                return;
            }
            drop(edge);
            tokio::time::sleep(ROUTE_POLL).await;
        }
    };
    tokio::select! {
        () = wait => {}
        () = moved => {}
    }
}

/// Runs the test hook on an inventory event, if one is set: false when
/// it drops the stream here.
async fn hook_keeps(runtime: &Weak<ProfileRuntime>, host: HostId, event: &InventoryEvent) -> bool {
    let hook = runtime
        .upgrade()
        .and_then(|me| me.sources.lock().unwrap().inventory_hook.clone());
    match hook.map(|hook| hook(host, event)) {
        None | Some(SourceVerdict::Keep) => true,
        Some(SourceVerdict::Drop) => false,
        Some(SourceVerdict::Hold(held)) => {
            held.await;
            true
        }
    }
}

/// One inventory stream: returns once it ends, saying whether it had
/// caught up.
async fn follow_once(runtime: &Weak<ProfileRuntime>, host: HostId) -> bool {
    let Some(edge) = runtime.upgrade().and_then(|me| me.edge()) else {
        return false;
    };
    // The sources open their channel while the inventory's opens, not
    // after it: over a relay each takes two round trips.
    let session = match runtime.upgrade() {
        Some(me) => me.begin_following(host).await,
        None => return false,
    };
    let mut client = match edge.peer(host).await {
        Ok(client) => client,
        Err(error) => {
            tracing::debug!(%host, %error, "no channel for the host's inventory");
            return false;
        }
    };
    drop(edge);
    let mut stream = match client.subscribe_inventory(Empty {}).await {
        Ok(response) => response.into_inner(),
        Err(error) => {
            tracing::debug!(%host, %error, "the host refused its inventory");
            return false;
        }
    };
    tracing::debug!(%host, "subscribed to the host's inventory");
    let host_bytes = host.as_bytes().to_vec();
    let mut generation = None;
    let mut listed = BTreeMap::new();
    loop {
        let Ok(Some(message)) = stream.message().await else {
            return false;
        };
        if !hook_keeps(runtime, host, &message).await {
            return false;
        }
        match message.of {
            Some(inventory_event::Of::Host(entry)) if entry.host_id == host_bytes => {
                generation = Some(entry.generation);
                if let Some(me) = runtime.upgrade() {
                    me.peer_providers_said(&host_bytes, entry.providers);
                }
            }
            Some(inventory_event::Of::Agent(agent)) if agent.host_id == host_bytes => {
                listed.insert(agent.agent_id.clone(), agent);
            }
            Some(inventory_event::Of::AgentRemoved(removed)) if removed.host_id == host_bytes => {
                listed.remove(&removed.agent_id);
            }
            Some(inventory_event::Of::CaughtUp(_)) => {
                tracing::debug!(%host, agents = listed.len(), "the host's inventory caught up");
                break;
            }
            _ => {}
        }
    }
    {
        let Some(me) = runtime.upgrade() else {
            return false;
        };
        if let Err(error) = me
            .inventory_caught_up(host, session, generation, listed.into_values().collect())
            .await
        {
            tracing::warn!(%host, %error, "reconciling the host's inventory failed");
            return false;
        }
        me.sweep_sources().await;
    }
    loop {
        let Ok(Some(message)) = stream.message().await else {
            return true;
        };
        if !hook_keeps(runtime, host, &message).await {
            return true;
        }
        let Some(me) = runtime.upgrade() else {
            return true;
        };
        let changed = match message.of {
            // What its providers offer is said on its own entry.
            Some(inventory_event::Of::Host(entry)) if entry.host_id == host_bytes => {
                if me.peer_providers_said(&host_bytes, entry.providers) {
                    me.sync_hosts().await;
                }
                continue;
            }
            Some(inventory_event::Of::Agent(agent)) if agent.host_id == host_bytes => {
                let mut store = me.store.lock().await;
                me.put_replica_row(&mut store, &agent)
            }
            Some(inventory_event::Of::AgentRemoved(removed)) if removed.host_id == host_bytes => {
                let mut store = me.store.lock().await;
                let key = AgentKey::new(removed.host_id, removed.agent_id);
                match store.agent(&key) {
                    Ok(Some(_)) => me.drop_replica(&mut store, &key, RemovalReason::NoLongerListed),
                    Ok(None) => Ok(()),
                    Err(error) => Err(error),
                }
            }
            // A generation changes only across a restart, which ends this
            // stream.
            _ => continue,
        };
        if let Err(error) = changed {
            tracing::warn!(%host, %error, "applying an inventory change failed");
            return true;
        }
        me.sweep_sources().await;
    }
}

/// One replica agent's source, until it settles or its session ends.
async fn run_source(runtime: Weak<ProfileRuntime>, key: AgentKey, host: HostId, session: u64) {
    let mut backoff: Option<Backoff> = None;
    loop {
        let ended = source_once(&runtime, &key, host, session).await;
        let Some(me) = runtime.upgrade() else { return };
        match ended {
            Ended::Settled => {
                me.source_ended(&key, session, true);
                return;
            }
            Ended::Stale => {
                me.source_ended(&key, session, false);
                return;
            }
            Ended::Lost { caught_up } => {
                if !me.session_current(&key, session) {
                    me.source_ended(&key, session, false);
                    return;
                }
                {
                    let mut store = me.store.lock().await;
                    me.detach(&mut store, &key);
                }
                let launch = me.launch();
                let backoff = backoff.get_or_insert_with(|| {
                    Backoff::new(launch.source_backoff_ms, launch.source_backoff_max_ms)
                });
                if caught_up {
                    backoff.reset();
                }
                let clock = me.clock().clone();
                let until = clock.now_ms() + backoff.next();
                let renewed = me.edge().map(|edge| edge.revocation_cleared(host));
                drop(me);
                if redial_early(clock.sleep_until(until), renewed).await {
                    backoff.reset();
                }
            }
        }
    }
}

/// One Subscribe to the origin, from the request to the stream's end.
async fn source_once(
    runtime: &Weak<ProfileRuntime>,
    key: &AgentKey,
    host: HostId,
    session: u64,
) -> Ended {
    let (request, edge, hook, agent_id) = {
        let Some(me) = runtime.upgrade() else {
            return Ended::Stale;
        };
        let k = me.launch().tail_rows;
        let row = match me.store.lock().await.agent(key) {
            Ok(Some(row)) => row,
            _ => return Ended::Stale,
        };
        let from = if row.complete_from_order.is_some() {
            peer_subscribe_request::From::After(wire::After {
                revision: row.source_cursor,
                cap: k,
                generation: row.source_generation,
            })
        } else {
            peer_subscribe_request::From::Tail(k)
        };
        let Some(edge) = me.edge() else {
            return Ended::Stale;
        };
        let hook = me.sources.lock().unwrap().hook.clone();
        let Ok(agent_id) = Uuid::from_slice(&key.agent) else {
            return Ended::Stale;
        };
        (
            PeerSubscribeRequest {
                agent_id: key.agent.clone(),
                from: Some(from),
            },
            edge,
            hook,
            agent_id,
        )
    };
    let tail = matches!(request.from, Some(peer_subscribe_request::From::Tail(_)));
    let mut client = match edge.session_peer(host).await {
        Ok(client) => client,
        Err(_) => return Ended::Lost { caught_up: false },
    };
    drop(edge);
    let mut stream = match client.subscribe(request).await {
        Ok(response) => response.into_inner(),
        Err(status) if status.code() == tonic::Code::NotFound => return Ended::Stale,
        Err(_) => return Ended::Lost { caught_up: false },
    };
    tracing::debug!(%host, %agent_id, "subscribed to the session");
    let mut catch_up = Some(CatchUp {
        reset: tail,
        events: Vec::new(),
    });
    let mut caught_up = false;
    // The origin's generation, from the stream's Opening.
    let mut generation = None;
    loop {
        let message = match stream.message().await {
            Ok(Some(message)) => message,
            _ => return Ended::Lost { caught_up },
        };
        if let Some(hook) = &hook {
            match hook(key, &message) {
                SourceVerdict::Keep => {}
                SourceVerdict::Drop => return Ended::Lost { caught_up },
                SourceVerdict::Hold(held) => held.await,
            }
        }
        let Some(me) = runtime.upgrade() else {
            return Ended::Stale;
        };
        let record = match message.of {
            Some(session_event::Of::Opening(opening)) => {
                generation = Some(opening.generation);
                continue;
            }
            Some(session_event::Of::Reset(_)) => {
                catch_up = Some(CatchUp {
                    reset: true,
                    events: Vec::new(),
                });
                continue;
            }
            Some(session_event::Of::CaughtUp(marker)) => {
                tracing::debug!(%host, %agent_id, revision = marker.revision, "the session caught up");
                let Some(generation) = generation else {
                    tracing::warn!(%host, %agent_id, "the origin's stream caught up without an Opening");
                    return Ended::Lost { caught_up };
                };
                // Again, after a new Hello at the origin: nothing to
                // replay, the cursor is where it was.
                let held = catch_up.take().unwrap_or(CatchUp {
                    reset: false,
                    events: Vec::new(),
                });
                let landed = me
                    .land_catch_up(key, session, held, marker.revision, generation)
                    .await;
                match landed {
                    Ok(Some(true)) => return Ended::Settled,
                    Ok(Some(false)) => caught_up = true,
                    Ok(None) => return Ended::Stale,
                    Err(error) => {
                        tracing::warn!(%error, "absorbing a catch-up failed");
                        return Ended::Lost { caught_up };
                    }
                }
                continue;
            }
            Some(session_event::Of::Snapshot(snapshot)) => SourceEvent::Snapshot(snapshot),
            Some(session_event::Of::Item(item)) => SourceEvent::Item(item),
            Some(session_event::Of::Append(append)) => SourceEvent::Append(append),
            // The origin closes the stream after Lagged; it is lost.
            Some(session_event::Of::Lagged(_)) => return Ended::Lost { caught_up },
            // An origin follows its own agents; it never detaches them.
            Some(session_event::Of::Detached(_)) | None => continue,
        };
        match catch_up.as_mut() {
            Some(held) => held.events.push(record),
            None => match me.land_live(key, session, record).await {
                Ok(true) => {}
                Ok(false) => return Ended::Stale,
                Err(error) => {
                    tracing::warn!(%error, "absorbing a live record failed");
                    return Ended::Lost { caught_up };
                }
            },
        }
    }
}

/// A replacing catch-up as the store absorbs it: the tail with each key in
/// its newest state, the newest snapshot, and the appends that do not
/// apply to a row of the tail, kept in order for a delta.
fn fold_tail(
    events: Vec<SourceEvent>,
    row: &AgentRow,
) -> (Vec<Item>, wire::Snapshot, Vec<SourceEvent>) {
    let mut items: HashMap<String, Item> = HashMap::new();
    let mut snapshot: Option<wire::Snapshot> = None;
    let mut rest = Vec::new();
    for event in events {
        match event {
            SourceEvent::Item(item) => {
                if items
                    .get(&item.key)
                    .is_none_or(|held| held.revision < item.revision)
                {
                    items.insert(item.key.clone(), item);
                }
            }
            SourceEvent::Append(append) => match items.get_mut(&append.key) {
                Some(held) if held.revision == append.base_revision => {
                    held.text.push_str(&append.text);
                    held.revision = append.revision;
                }
                _ => rest.push(SourceEvent::Append(append)),
            },
            SourceEvent::Snapshot(next) => {
                if snapshot
                    .as_ref()
                    .is_none_or(|held| held.revision < next.revision)
                {
                    snapshot = Some(next);
                }
            }
        }
    }
    let snapshot = snapshot.unwrap_or_else(|| wire::Snapshot {
        agent: row.agent.agent.clone(),
        kind: row.kind,
        ..wire::Snapshot::default()
    });
    let mut tail: Vec<Item> = items.into_values().collect();
    tail.sort_by_key(|item| item.order);
    (tail, snapshot, rest)
}

fn record_event(record: Record) -> SessionEvent {
    event(match record {
        Record::Item(item) => session_event::Of::Item(item),
        Record::Append(append) => session_event::Of::Append(append),
        Record::Snapshot(snapshot) => session_event::Of::Snapshot(snapshot),
    })
}

/// The replica row an inventory row describes.
pub(crate) fn from_wire(agent: &Agent) -> AgentRow {
    let mut row = AgentRow::new(
        AgentKey::new(agent.host_id.clone(), agent.agent_id.clone()),
        agent.kind,
        agent.cwd.clone(),
    );
    row.name = Some(agent.name.clone()).filter(|name| !name.is_empty());
    row.parent = agent
        .parent
        .as_ref()
        .map(|parent| AgentKey::new(parent.host_id.clone(), parent.agent_id.clone()));
    row.lifecycle = agent.lifecycle;
    row.exit_cause = agent.exit_cause;
    row.exit_detail = agent.exit_detail.clone();
    row.phase = agent.phase;
    row.working_on = agent.working_on.as_ref().map(|on| on.text.clone());
    row.phase_since = Some(agent.phase_since_ms);
    row.git = agent.git.clone();
    row.created_at = agent.created_at_ms;
    row.producer_version = agent.producer_version.clone();
    row.incarnation = agent.incarnation;
    row
}

#[cfg(test)]
mod tests {
    use super::*;

    fn following(sources: &mut Sources, host: &[u8]) -> u64 {
        sources.next_session += 1;
        let session = sources.next_session;
        sources.following.insert(host.to_vec(), session);
        session
    }

    fn open(sources: &mut Sources, host: &[u8], agent: u8, session: u64) -> AgentKey {
        let key = AgentKey {
            host: host.to_vec(),
            agent: vec![agent],
        };
        let task = tokio::spawn(std::future::pending::<()>());
        sources.open.insert(key.clone(), (session, task));
        key
    }

    /// A kind this daemon has no name for is a number it carries: the
    /// replica row keeps it from the origin's inventory row to the one this
    /// daemon lists, and the items and snapshot a reset lands keep theirs.
    #[test]
    fn unknown_kind_survives_replication() {
        const UNKNOWN: i32 = 99;
        let dir = tempfile::tempdir().unwrap();
        let mut store =
            store::Sqlite::open(&dir.path().join("store.db"), b"here".to_vec()).unwrap();
        let origin = Agent {
            host_id: b"there".to_vec(),
            agent_id: b"agent".to_vec(),
            kind: UNKNOWN,
            cwd: "/elsewhere".to_owned(),
            ..Agent::default()
        };
        let row = from_wire(&origin);
        assert_eq!(row.kind, UNKNOWN);
        store.put_agent(&row).unwrap();
        let key = row.agent.clone();
        let item = Item {
            key: "said".to_owned(),
            kind: UNKNOWN,
            text: "from a newer daemon".to_owned(),
            order: 1,
            revision: 1,
            ..Item::default()
        };
        let (tail, snapshot, _) = fold_tail(vec![SourceEvent::Item(item)], &row);
        assert_eq!(
            snapshot.kind, UNKNOWN,
            "the default snapshot takes the row's kind"
        );
        store
            .absorb(
                &key,
                Absorb::Reset {
                    tail,
                    snapshot,
                    generation: 1,
                },
            )
            .unwrap();

        let listed = crate::runtime::to_wire(&store.agent(&key).unwrap().unwrap());
        assert_eq!(
            listed.kind, UNKNOWN,
            "listed with its kind, not unspecified"
        );
        assert_eq!(store.get(&key, "said").unwrap().unwrap().kind, UNKNOWN);
    }

    /// An exit cause this daemon has no name for is a number it carries,
    /// with the origin's own words beside it: stored, read back after the
    /// store is opened again, and listed with the value the origin gave.
    #[test]
    fn unknown_exit_cause_survives_replication() {
        const UNKNOWN: i32 = 99;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("store.db");
        let origin = Agent {
            host_id: b"there".to_vec(),
            agent_id: b"agent".to_vec(),
            kind: wire::Kind::Codex as i32,
            cwd: "/elsewhere".to_owned(),
            lifecycle: wire::Lifecycle::Exited as i32,
            exit_cause: UNKNOWN,
            exit_detail: Some("a newer daemon's words".to_owned()),
            ..Agent::default()
        };
        let row = from_wire(&origin);
        let key = row.agent.clone();
        {
            let mut store = store::Sqlite::open(&path, b"here".to_vec()).unwrap();
            store.put_agent(&row).unwrap();
            // The origin lists it again: the held row takes the same values.
            store.put_agent(&from_wire(&origin)).unwrap();
        }
        let store = store::Sqlite::open(&path, b"here".to_vec()).unwrap();
        let listed = crate::runtime::to_wire(&store.agent(&key).unwrap().unwrap());
        assert_eq!(
            (listed.exit_cause, listed.exit_detail.as_deref()),
            (UNKNOWN, Some("a newer daemon's words")),
            "listed with its cause, not unspecified"
        );
    }

    #[tokio::test]
    async fn a_following_session_is_current_until_the_sources_are_aborted() {
        let host = b"host";
        let mut sources = Sources::default();
        let session = following(&mut sources, host);
        open(&mut sources, host, 1, session);
        assert!(sources.is_current(host, session));

        sources.abort_all();

        assert!(!sources.is_current(host, session));
        assert!(sources.open.is_empty());
    }
}
