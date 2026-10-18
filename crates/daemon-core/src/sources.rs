//! Replica sources: how the core keeps its replicas of other hosts' agents
//! current.
//!
//! Every trusted host with a route has one follower. It holds the host's
//! SubscribeInventory call: at the stream's CaughtUp it records the host's
//! generation, writes the rows the host lists and drops the ones it no
//! longer lists, and only then marks the host ready. That is what
//! "reachable" means for a source.
//!
//! Every replica agent of a ready host may have one source: one Subscribe
//! call to the origin. The agents the store remembers of a host get theirs
//! as the host's inventory is subscribed, in the same step, so a launch
//! reconciles in one round trip to each host rather than two; the
//! inventory's CaughtUp confirms them, and drops any for an agent the host
//! no longer lists. With no block a source asks for a tail of K, with one
//! for what came after its cursor, capped at K, naming the origin
//! generation the cursor was taken under; the origin answers a delta when
//! it fits and the generation is the one it runs, and a Reset and a fresh
//! tail otherwise. Every stream opens with the origin's generation, which
//! the block takes at its Reset, so a rewound origin (an unclean reboot
//! mints the same orders and revisions again for different content) resets
//! each of its replicas on that replica's own stream, forgetting every row
//! of the old generation. A source holds a catch-up until the origin's
//! CaughtUp and then absorbs it in one step, so the block is never left
//! half-replaced by a stream that died midway; the cursor moves to the
//! revision CaughtUp carries, then per live record. Records are absorbed
//! before they are answered to anyone. A stream that ends is Detached to
//! local subscribers and opened again after the cursor with backoff while
//! the host stays reachable. A source for an exited agent closes once its
//! catch-up has landed.
//!
//! Which agents get a source is the [`SourcePolicy`], read by one sweep
//! after every inventory catch-up and change and once when the policy
//! changes; a client's Subscribe opens one for any agent the policy
//! skipped. Under `OnDemand` the sweep keeps exactly the agents a client is
//! subscribed to.
//!
//! A follower or source is opened, and opened again after its backoff, only
//! by a timer, so it is always opened by a step that holds the store, never
//! by one that reports a link.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use store::{Absorb, AgentKey, AgentRow, Marker, Record, SourceEvent, StoreError};
use wire::{
    Agent, AgentRemoved, FetchResponse, Item, PeerSubscribeRequest, RemovalReason, SessionEvent,
    inventory_event, peer_subscribe_request, session_event,
};

use crate::ids::{AgentId, CallId, HostId, TimerId};
use crate::io::{Output, ParentSays, ShellWork, SourcePolicy, Wake};
use crate::payload::{Answer, CallError, PeerFailure, PeerReply, PeerRequest, Response, Status};
use crate::peers::{Page, Waiting};
use crate::presence::Route;
use crate::serve::{Key, answered, event, inventory};
use crate::{Daemon, codec};

/// Each replica this profile holds and the stream that feeds it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Sources {
    policy: SourcePolicy,
    /// The hosts this profile trusts, as the shell last said.
    trusted: BTreeSet<HostId>,
    /// One per trusted host.
    followers: BTreeMap<HostId, Follower>,
    /// Hosts whose inventory has caught up on the current stream, by the
    /// session that stream is. A session ends with its stream.
    ready: BTreeMap<HostId, u64>,
    /// Hosts whose inventory is subscribed but has not caught up, by the
    /// session it will confirm: the sources opened with it are current
    /// from the start, so one that is lost waits out its backoff like any
    /// other rather than ending.
    following: BTreeMap<HostId, u64>,
    last_session: u64,
    /// Open sources, each under the host session it belongs to.
    open: BTreeMap<Key, Source>,
    /// Exited agents whose catch-up has landed: nothing more will come, so
    /// no source opens for them until their host lists them live again.
    settled: BTreeSet<Key>,
    /// Exited agents whose source was opened again to settle them.
    settling: BTreeSet<Key>,
}

/// One trusted host's inventory, followed while the host has a route.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct Follower {
    /// The wait before the next stream once one is lost; none until one
    /// is, and none again once the host has no route.
    backoff: Option<Backoff>,
    state: Following,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
enum Following {
    /// No route reaches the host: followed once one does.
    #[default]
    Idle,
    /// To be followed when its timer fires, which is now.
    Due,
    /// A stream on `route` was lost: followed again after the backoff, or
    /// at once if the host's route is no longer that one. A host that goes
    /// away and comes back, or comes back on a new link before the loss is
    /// read, is followed as soon as that is seen.
    Waiting {
        route: Option<Route>,
    },
    Streaming(Box<Inventory>),
}

/// An inventory call in flight, and what it has listed before its
/// CaughtUp.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Inventory {
    call: CallId,
    session: u64,
    /// The route calls to the host took as the stream was opened.
    route: Option<Route>,
    caught_up: bool,
    /// A stream before this one, which lagged, had caught up.
    caught_up_before: bool,
    generation: Option<u64>,
    listed: BTreeMap<Vec<u8>, Listed>,
    /// This host's own agents whose parent lives on the host, as they were
    /// when the listing was asked for: only they are judged by it.
    children: BTreeSet<Key>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Listed(#[serde(with = "codec::message")] Agent);

/// One replica agent's source.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Source {
    session: u64,
    backoff: Option<Backoff>,
    feeding: Feeding,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
enum Feeding {
    Streaming {
        call: CallId,
        /// The origin's, from the stream's Opening.
        generation: Option<u64>,
        catch_up: Option<CatchUp>,
        caught_up: bool,
    },
    /// Lost: opened again when its backoff timer fires.
    Waiting,
}

/// A catch-up held until the origin's CaughtUp.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct CatchUp {
    /// The origin sent a Reset, or this was a tail request: what arrives
    /// replaces the block.
    reset: bool,
    #[serde(with = "codec::messages")]
    events: Vec<SessionEvent>,
}

/// Doubling backoff between reconnects, from the first wait to the
/// ceiling; reset once a stream has caught up.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Backoff {
    next_ms: u64,
}

impl Sources {
    /// Whether `session` is the one `host`'s agents are sourced under now,
    /// caught up or still following.
    fn is_current(&self, host: HostId, session: u64) -> bool {
        self.ready.get(&host) == Some(&session) || self.following.get(&host) == Some(&session)
    }

    /// The replica agents with a source open now.
    pub fn open(&self) -> impl Iterator<Item = AgentKey> + '_ {
        self.open.keys().map(Key::agent_key)
    }

    /// Whether `host`'s inventory has caught up on a live stream, so its
    /// agents may have sources.
    pub fn ready(&self, host: HostId) -> bool {
        self.ready.contains_key(&host)
    }

    pub fn policy(&self) -> SourcePolicy {
        self.policy
    }
}

impl Daemon {
    /// The replicas' sources, for reading.
    pub fn sources(&self) -> &Sources {
        &self.sources
    }

    fn is_own_host(&self, host: &[u8]) -> bool {
        host == self.store.own_host()
    }

    fn arm_now(&self, id: TimerId, out: &mut Vec<Output>) {
        out.push(Output::ArmTimer {
            id,
            at_ms: self.timers.now_ms,
        });
    }

    /// The wait `backoff` gives now, created at the settings' first wait.
    fn wait(&self, backoff: &mut Option<Backoff>, caught_up: bool) -> u64 {
        let first = self.settings.source_backoff_ms.max(1);
        let max = self.settings.source_backoff_max_ms.max(first);
        let backoff = backoff.get_or_insert(Backoff { next_ms: first });
        if caught_up {
            backoff.next_ms = first;
        }
        let wait = backoff.next_ms.min(max);
        backoff.next_ms = (wait * 2).min(max);
        wait
    }

    /// The route calls to `host` take now.
    fn route_now(&self, host: HostId) -> Option<Route> {
        self.presence.view(host).route()
    }

    fn reachable(&self, host: HostId) -> bool {
        !self.routing.links.routes_to(host).is_empty()
    }

    fn hang_up(&mut self, call: CallId, out: &mut Vec<Output>) {
        self.pending.peers.waiting.remove(&call);
        out.push(Output::HangUp { call });
    }

    // --- trust and policy ------------------------------------------------------

    /// The hosts this profile trusts now. A host no longer trusted is
    /// followed no more and its replicas go; this host's own agents whose
    /// parent lives there carry on, and the reports they owe it go with
    /// the outbox's next pass.
    pub(crate) fn trusted(&mut self, hosts: Vec<HostId>, out: &mut Vec<Output>) {
        let own = self.store.own_host().to_vec();
        let trusted: BTreeSet<HostId> = hosts.into_iter().filter(|host| host.0 != *own).collect();
        let gone: Vec<HostId> = self.sources.trusted.difference(&trusted).copied().collect();
        self.sources.trusted = trusted.clone();
        for host in gone {
            self.unfollow(host, out);
        }
        if let Err(error) = self.drop_untrusted(out) {
            tracing::warn!(%error, "dropping an untrusted host's replicas failed");
        }
        for host in trusted {
            self.follow_if_reachable(host, out);
        }
    }

    fn drop_untrusted(&mut self, out: &mut Vec<Output>) -> Result<(), StoreError> {
        let untrusted: Vec<AgentKey> = self
            .store
            .agents()?
            .into_iter()
            .map(|row| row.agent)
            .filter(|key| {
                !self.is_own_host(&key.host)
                    && HostId::from_slice(&key.host)
                        .is_none_or(|host| !self.sources.trusted.contains(&host))
            })
            .collect();
        for key in &untrusted {
            self.drop_replica(key, RemovalReason::HostUntrusted, out)?;
        }
        // The outbox drops what is owed to agents there.
        out.push(Output::Wake(Wake::Deliveries));
        Ok(())
    }

    /// Sets which replica agents keep a source open, and sweeps once.
    pub(crate) fn set_policy(&mut self, policy: SourcePolicy, out: &mut Vec<Output>) {
        if self.sources.policy != policy {
            self.sources.policy = policy;
            self.sweep_sources(out);
        }
    }

    // --- followers -------------------------------------------------------------

    /// After each step, for every host whose routes or presence it moved:
    /// a trusted host a route reaches again is followed, and one whose
    /// route changed while it waited out a backoff is followed at once.
    pub(crate) fn follow_touched(&mut self, out: &mut Vec<Output>) {
        for host in self.touched.hosts.clone() {
            self.follow_if_reachable(host, out);
        }
    }

    fn follow_if_reachable(&mut self, host: HostId, out: &mut Vec<Output>) {
        if !self.sources.trusted.contains(&host) {
            return;
        }
        let reachable = self.reachable(host);
        let route = self.route_now(host);
        let follower = self.sources.followers.entry(host).or_default();
        match &follower.state {
            Following::Idle if reachable => follower.state = Following::Due,
            Following::Waiting { route: followed } if *followed != route => {
                if reachable {
                    follower.state = Following::Due;
                } else {
                    follower.state = Following::Idle;
                    follower.backoff = None;
                    out.push(Output::CancelTimer {
                        id: TimerId::Follow(host),
                    });
                    return;
                }
            }
            _ => return,
        }
        self.arm_now(TimerId::Follow(host), out);
    }

    /// The host's follow timer fired: its inventory is subscribed, if it is
    /// still trusted and a route reaches it.
    pub(crate) fn follow_due(&mut self, host: HostId, out: &mut Vec<Output>) {
        if !self.sources.trusted.contains(&host) {
            return;
        }
        let reachable = self.reachable(host);
        let follower = self.sources.followers.entry(host).or_default();
        match follower.state {
            Following::Due | Following::Waiting { .. } if !reachable => {
                follower.state = Following::Idle;
                follower.backoff = None;
            }
            Following::Due | Following::Waiting { .. } => {
                let route = self.route_now(host);
                self.begin_following(host, None, false, route, out);
            }
            Following::Idle | Following::Streaming(_) => {}
        }
    }

    /// Something that kept the host from taking this host's calls is gone:
    /// every backoff on it is cut short.
    pub(crate) fn sources_try_again(&mut self, host: HostId, out: &mut Vec<Output>) {
        if let Some(follower) = self.sources.followers.get_mut(&host)
            && matches!(follower.state, Following::Waiting { .. })
        {
            follower.backoff = None;
            follower.state = Following::Due;
            self.arm_now(TimerId::Follow(host), out);
        }
        let waiting: Vec<Key> = self
            .sources
            .open
            .iter()
            .filter(|(key, source)| {
                key.host == host.0 && matches!(source.feeding, Feeding::Waiting)
            })
            .map(|(key, _)| key.clone())
            .collect();
        for key in waiting {
            if let Some(source) = self.sources.open.get_mut(&key) {
                source.backoff = None;
            }
            if let Some(agent) = AgentId::from_slice(&key.agent) {
                self.arm_now(TimerId::Source(host, agent), out);
            }
        }
    }

    /// Begins a host's inventory session: the number its sources are keyed
    /// by until its stream ends. Under the `Listed` policy this also opens
    /// a source for every agent the store remembers of the host, so their
    /// calls leave with the inventory's; the session is confirmed when the
    /// inventory catches up, and an agent it no longer lists is dropped
    /// then, source and all. `resume` is the session of a stream that
    /// lagged: its sources stay current across the fresh listing.
    ///
    /// Also notes this host's own agents whose parent lives on `host`, as
    /// they are now: the host's listing is asked for after them, so it can
    /// judge their parents (see [`Daemon::inventory_caught_up`]).
    fn begin_following(
        &mut self,
        host: HostId,
        resume: Option<u64>,
        caught_up_before: bool,
        route: Option<Route>,
        out: &mut Vec<Output>,
    ) {
        let rows = self.store.agents().unwrap_or_default();
        let own = self.store.own_host().to_vec();
        let children = rows
            .iter()
            .filter(|row| {
                row.agent.host == own
                    && row
                        .parent
                        .as_ref()
                        .is_some_and(|parent| parent.host == host.0)
            })
            .map(|row| Key::from(&row.agent))
            .collect();
        let session = resume.unwrap_or_else(|| {
            self.sources.last_session += 1;
            self.sources.last_session
        });
        self.sources.following.insert(host, session);
        if self.sources.policy == SourcePolicy::Listed {
            for row in rows.iter().filter(|row| row.agent.host == host.0) {
                let key = Key::from(&row.agent);
                if self.sources.open.contains_key(&key) || self.sources.settled.contains(&key) {
                    continue;
                }
                self.open_source_in(&row.agent, host, session, out);
                // An exited agent's source only settles it; the sweep
                // would otherwise open it again for that.
                if row.lifecycle == wire::Lifecycle::Exited as i32 {
                    self.sources.settling.insert(key);
                }
            }
        }
        let call = self.pending.peers.mint();
        self.pending
            .peers
            .waiting
            .insert(call, Waiting::Inventory { host });
        out.push(Output::Call {
            host,
            call,
            request: PeerRequest::SubscribeInventory { hosts: Vec::new() },
            absorbed: true,
        });
        let follower = self.sources.followers.entry(host).or_default();
        follower.state = Following::Streaming(Box::new(Inventory {
            call,
            session,
            route,
            caught_up: false,
            caught_up_before,
            generation: None,
            listed: BTreeMap::new(),
            children,
        }));
    }

    /// The host is followed no more: its trust ended.
    fn unfollow(&mut self, host: HostId, out: &mut Vec<Output>) {
        if let Some(follower) = self.sources.followers.remove(&host) {
            if let Following::Streaming(inventory) = follower.state {
                self.hang_up(inventory.call, out);
            }
            out.push(Output::CancelTimer {
                id: TimerId::Follow(host),
            });
        }
        self.host_lost(host, out);
    }

    /// One event of a host's inventory, or its end.
    pub(crate) fn inventory_answered(
        &mut self,
        host: HostId,
        made: CallId,
        answer: Answer,
        out: &mut Vec<Output>,
    ) {
        let Some(Following::Streaming(inventory)) = self
            .sources
            .followers
            .get_mut(&host)
            .map(|follower| &mut follower.state)
        else {
            return;
        };
        if inventory.call != made {
            return;
        }
        let caught_up = inventory.caught_up;
        let event = match answer {
            Answer::Inventory(event) => event,
            Answer::Reply(_) | Answer::Session(_) | Answer::Ended | Answer::Failed(_) => {
                let caught_up = caught_up || inventory.caught_up_before;
                return self.inventory_lost(host, caught_up, out);
            }
        };
        let changed = match event.of {
            Some(inventory_event::Of::Host(entry)) => {
                // A generation changes only across a restart, which ends
                // the stream.
                if !caught_up {
                    inventory.generation = Some(entry.generation);
                }
                out.push(Output::Shell(ShellWork::Providers {
                    host,
                    providers: entry.providers,
                }));
                return;
            }
            Some(inventory_event::Of::Agent(agent)) if !caught_up => {
                inventory
                    .listed
                    .insert(agent.agent_id.clone(), Listed(agent));
                return;
            }
            Some(inventory_event::Of::AgentRemoved(removed)) if !caught_up => {
                inventory.listed.remove(&removed.agent_id);
                return;
            }
            Some(inventory_event::Of::CaughtUp(_)) if !caught_up => {
                inventory.caught_up = true;
                let session = inventory.session;
                let generation = inventory.generation;
                let listed = std::mem::take(&mut inventory.listed)
                    .into_values()
                    .map(|listed| listed.0)
                    .collect();
                let children = std::mem::take(&mut inventory.children);
                tracing::debug!(host = %crate::hex(&host.0), "the host's inventory caught up");
                if let Err(error) =
                    self.inventory_caught_up(host, session, generation, listed, &children, out)
                {
                    tracing::warn!(%error, "reconciling the host's inventory failed");
                    let caught_up_before = self.caught_up_before(host);
                    self.hang_up(made, out);
                    return self.inventory_lost(host, caught_up_before, out);
                }
                Ok(())
            }
            Some(inventory_event::Of::Agent(agent)) => self.replica_changed(&agent, out),
            Some(inventory_event::Of::AgentRemoved(removed)) => {
                self.replica_removed(&AgentKey::new(removed.host_id, removed.agent_id), out)
            }
            // What it missed is in the fresh listing, which reconciles the
            // rows at its CaughtUp: the host is still there, and its
            // sources are not disturbed meanwhile.
            Some(inventory_event::Of::Lagged(_)) => {
                tracing::debug!(host = %crate::hex(&host.0), "the host's inventory lagged");
                let session = inventory.session;
                let route = inventory.route;
                let caught_up_before = caught_up || inventory.caught_up_before;
                self.hang_up(made, out);
                self.begin_following(host, Some(session), caught_up_before, route, out);
                return;
            }
            Some(inventory_event::Of::CaughtUp(_) | inventory_event::Of::HostRemoved(_)) | None => {
                return;
            }
        };
        if let Err(error) = changed {
            tracing::warn!(%error, "applying an inventory change failed");
            self.hang_up(made, out);
            return self.inventory_lost(host, true, out);
        }
        self.sweep_sources(out);
    }

    fn caught_up_before(&self, host: HostId) -> bool {
        match self
            .sources
            .followers
            .get(&host)
            .map(|follower| &follower.state)
        {
            Some(Following::Streaming(inventory)) => inventory.caught_up_before,
            _ => false,
        }
    }

    /// The host's inventory stream is gone: its session ends, and it is
    /// followed again after a backoff, reset if the stream had caught up.
    fn inventory_lost(&mut self, host: HostId, caught_up: bool, out: &mut Vec<Output>) {
        let Some(follower) = self.sources.followers.get(&host) else {
            return;
        };
        let route = match &follower.state {
            Following::Streaming(inventory) => {
                self.pending.peers.waiting.remove(&inventory.call);
                inventory.route
            }
            _ => None,
        };
        self.host_lost(host, out);
        let mut backoff = self.sources.followers.get(&host).and_then(|f| f.backoff);
        let wait = self.wait(&mut backoff, caught_up);
        if let Some(follower) = self.sources.followers.get_mut(&host) {
            follower.backoff = backoff;
            follower.state = Following::Waiting { route };
        }
        out.push(Output::ArmTimer {
            id: TimerId::Follow(host),
            at_ms: self.timers.now_ms.saturating_add(wait),
        });
        // A link restored quickly is up by the time the end is read.
        self.follow_if_reachable(host, out);
    }

    /// The host's session ended: no source of it absorbs anything more,
    /// and every chat on its agents is told the rows may be stale.
    fn host_lost(&mut self, host: HostId, out: &mut Vec<Output>) {
        self.sources.ready.remove(&host);
        self.sources.following.remove(&host);
        let keys: Vec<Key> = self
            .sources
            .open
            .keys()
            .filter(|key| key.host == host.0)
            .cloned()
            .collect();
        for key in keys {
            self.close_source(&key, out);
        }
        out.push(Output::Shell(ShellWork::HostCurrent {
            host,
            current: false,
        }));
        let Ok(rows) = self.store.agents() else {
            return;
        };
        for row in rows.iter().filter(|row| row.agent.host == host.0) {
            self.detach(&row.agent, out);
        }
    }

    /// Tells an agent's subscribers the rows may be stale, once.
    fn detach(&mut self, key: &AgentKey, out: &mut Vec<Output>) {
        if self.store.markers().get(key) == Some(&Marker::Detached) {
            return;
        }
        self.store.set_marker(key, Some(Marker::Detached));
        let detached = event(session_event::Of::Detached(wire::Detached {}));
        self.publish_session(key, Arc::new(detached), out);
    }

    /// The host's inventory reached CaughtUp: record its generation, write
    /// what it lists and drop what it no longer does, then mark the host
    /// ready.
    ///
    /// The listing is the host's whole fleet, so it also settles the family
    /// rule for this host's own agents whose parent lives there, whatever
    /// happened while the hosts were apart: a parent listed as exited stops
    /// its children here, and one no longer listed deletes them. Only
    /// `children`, the agents that existed before the listing was asked
    /// for, are judged by it: a child made since may belong to a parent the
    /// listing predates, and the live stream speaks for that parent.
    fn inventory_caught_up(
        &mut self,
        host: HostId,
        session: u64,
        generation: Option<u64>,
        listed: Vec<Agent>,
        children: &BTreeSet<Key>,
        out: &mut Vec<Output>,
    ) -> Result<(), StoreError> {
        let listed_keys: BTreeSet<&[u8]> = listed
            .iter()
            .map(|agent| agent.agent_id.as_slice())
            .collect();
        let unlisted: Vec<AgentKey> = self
            .store
            .agents()?
            .into_iter()
            .map(|row| row.agent)
            .filter(|key| key.host == host.0 && !listed_keys.contains(key.agent.as_slice()))
            .collect();
        if let Some(generation) = generation
            && self.store.host_generation(&host.0)? != Some(generation)
        {
            // The replicas are not this stream's to reset: each source
            // resumes after its cursor naming the generation it was taken
            // under, and the origin answers the old one with a fresh tail.
            // An exited agent's source had settled and would not look
            // again; the sweep opens it again for that reset.
            tracing::info!(host = %crate::hex(&host.0), generation, "the host came back under a new generation");
            self.store.record_host_generation(&host.0, generation)?;
            self.sources.settled.retain(|key| key.host != host.0);
            self.sources.settling.retain(|key| key.host != host.0);
            out.push(Output::Shell(ShellWork::HostGeneration {
                host,
                generation,
            }));
        }
        for key in &unlisted {
            self.drop_replica(key, RemovalReason::NoLongerListed, out)?;
            // A parent held here and listed no more is gone for good, so
            // every child here follows, however recently it was made.
            self.parent_changed(key, ParentSays::Deleted, out)?;
        }
        for agent in &listed {
            self.put_replica_row(agent, out)?;
        }
        for child in children {
            let child = child.agent_key();
            let Some(row) = self.store.agent(&child)? else {
                continue;
            };
            let Some(parent) = row.parent.as_ref() else {
                continue;
            };
            let Some(id) = AgentId::from_slice(&child.agent) else {
                continue;
            };
            let says = match listed.iter().find(|agent| agent.agent_id == parent.agent) {
                None if !unlisted.contains(parent) => Some(ParentSays::Deleted),
                Some(parent)
                    if parent.lifecycle == wire::Lifecycle::Exited as i32
                        && row.lifecycle == wire::Lifecycle::Live as i32 =>
                {
                    Some(ParentSays::Exited)
                }
                None | Some(_) => None,
            };
            if let Some(says) = says {
                out.push(Output::Shell(ShellWork::FollowParent { child: id, says }));
            }
        }
        self.sources.following.remove(&host);
        self.sources.ready.insert(host, session);
        out.push(Output::Shell(ShellWork::HostCurrent {
            host,
            current: true,
        }));
        // Deliveries to a parent on that host may have waited for it.
        out.push(Output::Wake(Wake::Deliveries));
        Ok(())
    }

    /// A row the host's live stream says changed: written, and when it says
    /// the agent has just exited, its children here follow. Only a change
    /// to exited counts, not a row that already said so, so a child its
    /// person resumed under an exited parent is not stopped again by every
    /// later change to the parent's row (a recount, a rename).
    fn replica_changed(&mut self, agent: &Agent, out: &mut Vec<Output>) -> Result<(), StoreError> {
        let key = AgentKey::new(agent.host_id.clone(), agent.agent_id.clone());
        let exited = wire::Lifecycle::Exited as i32;
        let was_exited = self
            .store
            .agent(&key)?
            .is_some_and(|was| was.lifecycle == exited && was.incarnation == agent.incarnation);
        self.put_replica_row(agent, out)?;
        if agent.lifecycle == exited && !was_exited {
            self.parent_changed(&key, ParentSays::Exited, out)?;
        }
        Ok(())
    }

    /// An agent the host's live stream says is gone for good, deleted or
    /// removed by its retention: its row goes, and its children here
    /// follow, whether or not this host ever held its row.
    fn replica_removed(&mut self, key: &AgentKey, out: &mut Vec<Output>) -> Result<(), StoreError> {
        if self.store.agent(key)?.is_some() {
            self.drop_replica(key, RemovalReason::NoLongerListed, out)?;
        }
        self.parent_changed(key, ParentSays::Deleted, out)
    }

    fn put_replica_row(&mut self, agent: &Agent, out: &mut Vec<Output>) -> Result<(), StoreError> {
        let row = from_wire(agent);
        if row.lifecycle != wire::Lifecycle::Exited as i32 {
            let key = Key::from(&row.agent);
            self.sources.settled.remove(&key);
            self.sources.settling.remove(&key);
        }
        self.store.put_agent(&row)?;
        self.publish_row(&row.agent, out)?;
        self.recount_from(row.parent, out)
    }

    fn drop_replica(
        &mut self,
        key: &AgentKey,
        reason: RemovalReason,
        out: &mut Vec<Output>,
    ) -> Result<(), StoreError> {
        let held = Key::from(key);
        self.close_source(&held, out);
        self.sources.settled.remove(&held);
        self.sources.settling.remove(&held);
        let parent = self.store.agent(key)?.and_then(|row| row.parent);
        self.store.delete_agent(key)?;
        // An agent its host no longer lists is gone for good, deleted or
        // removed by retention there; one whose host is untrusted is not.
        out.push(Output::Shell(ShellWork::ReplicaDropped {
            agent: key.clone(),
            listed: reason == RemovalReason::NoLongerListed,
        }));
        let removed = AgentRemoved {
            host_id: key.host.clone(),
            agent_id: key.agent.clone(),
            reason: reason as i32,
        };
        let removed = inventory(inventory_event::Of::AgentRemoved(removed));
        self.agent_removed(key, Arc::new(removed), out);
        self.recount_from(parent, out)
    }

    /// Each of this host's own agents under `parent`, whose row now says
    /// `says`, follows it: exited stops each live child, deleted deletes
    /// each child. A parent that is merely out of reach says neither, so
    /// its children carry on and their reports wait in the outbox.
    fn parent_changed(
        &mut self,
        parent: &AgentKey,
        says: ParentSays,
        out: &mut Vec<Output>,
    ) -> Result<(), StoreError> {
        for child in self.store.agents_of_parent(parent)? {
            if !self.is_own_host(&child.agent.host)
                || (says == ParentSays::Exited && child.lifecycle != wire::Lifecycle::Live as i32)
            {
                continue;
            }
            if let Some(child) = AgentId::from_slice(&child.agent.agent) {
                out.push(Output::Shell(ShellWork::FollowParent { child, says }));
            }
        }
        Ok(())
    }

    /// Counts the live descendants of `parent` again, and of each own
    /// ancestor above it, writing and answering every count that changed;
    /// a count that did not change leaves those above it unchanged too. A
    /// replica ancestor's count is its origin's. A running own `parent`
    /// also hears which of its children are live, when that changed.
    fn recount_from(
        &mut self,
        mut parent: Option<AgentKey>,
        out: &mut Vec<Output>,
    ) -> Result<(), StoreError> {
        if let Some(key) = &parent
            && self.is_own_host(&key.host)
            && let Some(agent) = AgentId::from_slice(&key.agent)
        {
            out.push(Output::Shell(ShellWork::TellChildren { agent }));
        }
        while let Some(key) = parent {
            if !self.is_own_host(&key.host) {
                break;
            }
            let Some(row) = self.store.agent(&key)? else {
                break;
            };
            let count = self.store.live_descendants(&key)?;
            if count == row.descendants {
                break;
            }
            self.store.set_descendants(&key, count)?;
            self.publish_row(&key, out)?;
            parent = row.parent;
        }
        Ok(())
    }

    // --- the sweep ---------------------------------------------------------------

    /// Opens a source for every replica agent the policy wants and none is
    /// open for, and settles the ones for exited agents. Under `OnDemand`
    /// the policy wants only agents a client is subscribed to: the sweep
    /// closes every other source, so a profile switched to it keeps nothing
    /// current but what is being watched, and opens one for a watched agent
    /// whose host was not ready when the client subscribed.
    pub(crate) fn sweep_sources(&mut self, out: &mut Vec<Output>) {
        let Ok(rows) = self.store.agents() else {
            return;
        };
        let mut unwatched = Vec::new();
        if self.sources.policy == SourcePolicy::OnDemand {
            let idle: Vec<Key> = self
                .sources
                .open
                .keys()
                .filter(|key| !self.sources.settling.contains(*key) && !self.answers.watched(key))
                .cloned()
                .collect();
            for key in idle {
                self.close_source(&key, out);
                unwatched.push(key);
            }
        }
        let mut settle = Vec::new();
        let own = self.store.own_host().to_vec();
        for row in rows.iter().filter(|row| row.agent.host != own) {
            let key = Key::from(&row.agent);
            let exited = row.lifecycle == wire::Lifecycle::Exited as i32;
            let caught_up = self.store.markers().get(&row.agent) == Some(&Marker::CaughtUp);
            if self.sources.open.contains_key(&key) {
                if exited && caught_up && !self.sources.settling.contains(&key) {
                    settle.push(row.agent.clone());
                }
                continue;
            }
            let wanted = self.sources.policy == SourcePolicy::Listed || self.answers.watched(&key);
            if wanted && !self.sources.settled.contains(&key) {
                self.open_source(&row.agent, out);
            }
        }
        // A live source's marker reads CaughtUp throughout, which says
        // nothing of the records written up to the exit: the origin
        // publishes them before the Exited row, but on another stream, so
        // they may still be on their way. The source starts again after its
        // cursor; the origin has committed them by now, and the catch-up
        // that brings them settles the agent.
        for agent in settle {
            let key = Key::from(&agent);
            self.close_source(&key, out);
            self.open_source(&agent, out);
            self.sources.settling.insert(key);
        }
        for key in unwatched {
            self.detach(&key.agent_key(), out);
        }
    }

    /// For a client's Subscribe on a replica: opens its source if none is
    /// and its host is ready, and says whether one follows it now.
    pub(crate) fn follow_replica(&mut self, agent: &AgentKey, out: &mut Vec<Output>) -> bool {
        let key = Key::from(agent);
        if !self.sources.open.contains_key(&key) && !self.sources.settled.contains(&key) {
            self.open_source(agent, out);
        }
        self.sources.open.contains_key(&key)
    }

    fn open_source(&mut self, agent: &AgentKey, out: &mut Vec<Output>) {
        let Some(host) = HostId::from_slice(&agent.host) else {
            return;
        };
        let Some(&session) = self.sources.ready.get(&host) else {
            return;
        };
        self.open_source_in(agent, host, session, out);
    }

    fn open_source_in(
        &mut self,
        agent: &AgentKey,
        host: HostId,
        session: u64,
        out: &mut Vec<Output>,
    ) {
        let Some((call, tail)) = self.source_call(agent, host, out) else {
            return;
        };
        self.sources.open.insert(
            Key::from(agent),
            Source {
                session,
                backoff: None,
                feeding: streaming(call, tail),
            },
        );
        out.push(Output::Shell(ShellWork::Sourcing {
            agent: agent.clone(),
            open: true,
        }));
    }

    /// Makes the Subscribe call that feeds `agent`'s replica: a tail of K
    /// with no block, otherwise what came after its cursor. None when the
    /// replica's row is gone. Says whether it asked for a tail.
    fn source_call(
        &mut self,
        agent: &AgentKey,
        host: HostId,
        out: &mut Vec<Output>,
    ) -> Option<(CallId, bool)> {
        let row = self.store.agent(agent).ok().flatten()?;
        let k = self.settings.tail_rows;
        let from = if row.complete_from_order.is_some() {
            peer_subscribe_request::From::After(wire::After {
                revision: row.source_cursor,
                cap: k,
                generation: row.source_generation,
            })
        } else {
            peer_subscribe_request::From::Tail(k)
        };
        let tail = matches!(from, peer_subscribe_request::From::Tail(_));
        let call = self.pending.peers.mint();
        self.pending.peers.waiting.insert(
            call,
            Waiting::Source {
                agent: Key::from(agent),
            },
        );
        out.push(Output::Call {
            host,
            call,
            request: PeerRequest::Subscribe(PeerSubscribeRequest {
                agent_id: agent.agent.clone(),
                from: Some(from),
            }),
            absorbed: true,
        });
        Some((call, tail))
    }

    /// Lets the agent's source go, whatever it was doing.
    fn close_source(&mut self, key: &Key, out: &mut Vec<Output>) {
        let Some(source) = self.sources.open.remove(key) else {
            return;
        };
        match source.feeding {
            Feeding::Streaming { call, .. } => self.hang_up(call, out),
            Feeding::Waiting => {
                if let (Some(host), Some(agent)) = (
                    HostId::from_slice(&key.host),
                    AgentId::from_slice(&key.agent),
                ) {
                    out.push(Output::CancelTimer {
                        id: TimerId::Source(host, agent),
                    });
                }
            }
        }
        out.push(Output::Shell(ShellWork::Sourcing {
            agent: key.agent_key(),
            open: false,
        }));
    }

    /// The source is done: it settled the agent, or the agent's row or
    /// its host's session is gone.
    fn source_ended(&mut self, key: &Key, settled: bool, out: &mut Vec<Output>) {
        self.close_source(key, out);
        self.sources.settling.remove(key);
        if settled {
            self.sources.settled.insert(key.clone());
        }
    }

    /// The source's stream is gone. While its host's session lasts, the
    /// agent's subscribers hear the rows may be stale and it is opened
    /// again after its backoff, reset if the stream had caught up.
    fn source_lost(&mut self, key: &Key, caught_up: bool, out: &mut Vec<Output>) {
        let Some(source) = self.sources.open.get(key) else {
            return;
        };
        let session = source.session;
        if let Feeding::Streaming { call, .. } = source.feeding {
            self.hang_up(call, out);
        }
        let (Some(host), Some(agent)) = (
            HostId::from_slice(&key.host),
            AgentId::from_slice(&key.agent),
        ) else {
            return self.source_ended(key, false, out);
        };
        if !self.sources.is_current(host, session) {
            return self.source_ended(key, false, out);
        }
        self.detach(&key.agent_key(), out);
        let mut backoff = self.sources.open.get(key).and_then(|source| source.backoff);
        let wait = self.wait(&mut backoff, caught_up);
        if let Some(source) = self.sources.open.get_mut(key) {
            source.backoff = backoff;
            source.feeding = Feeding::Waiting;
        }
        out.push(Output::ArmTimer {
            id: TimerId::Source(host, agent),
            at_ms: self.timers.now_ms.saturating_add(wait),
        });
    }

    /// The source's backoff is waited out: it is opened again after its
    /// cursor, while its host's session lasts and its row is held.
    pub(crate) fn source_due(&mut self, host: HostId, agent: AgentId, out: &mut Vec<Output>) {
        let key = Key {
            host: host.0.to_vec(),
            agent: agent.0.to_vec(),
        };
        let Some(source) = self.sources.open.get(&key) else {
            return;
        };
        if !matches!(source.feeding, Feeding::Waiting) {
            return;
        }
        if !self.sources.is_current(host, source.session) {
            return self.source_ended(&key, false, out);
        }
        match self.source_call(&key.agent_key(), host, out) {
            Some((call, tail)) => {
                if let Some(source) = self.sources.open.get_mut(&key) {
                    source.feeding = streaming(call, tail);
                }
            }
            None => self.source_ended(&key, false, out),
        }
    }

    /// One event of a source's stream, or its end.
    pub(crate) fn source_answered(
        &mut self,
        key: &Key,
        made: CallId,
        answer: Answer,
        out: &mut Vec<Output>,
    ) {
        let Some(source) = self.sources.open.get_mut(key) else {
            return;
        };
        let session = source.session;
        let Feeding::Streaming {
            call,
            generation,
            catch_up,
            caught_up,
        } = &mut source.feeding
        else {
            return;
        };
        if *call != made {
            return;
        }
        let event = match answer {
            Answer::Session(event) => event,
            // The origin does not hold the agent.
            Answer::Failed(PeerFailure::Refused(error))
                if error.code == wire::ErrorCode::NotFound as i32 =>
            {
                return self.source_ended(key, false, out);
            }
            Answer::Reply(_) | Answer::Inventory(_) | Answer::Ended | Answer::Failed(_) => {
                let caught_up = *caught_up;
                return self.source_lost(key, caught_up, out);
            }
        };
        let record = match event.of {
            Some(session_event::Of::Opening(opening)) => {
                *generation = Some(opening.generation);
                return;
            }
            Some(session_event::Of::Reset(_)) => {
                *catch_up = Some(CatchUp {
                    reset: true,
                    events: Vec::new(),
                });
                return;
            }
            Some(session_event::Of::CaughtUp(marker)) => {
                let was_caught_up = *caught_up;
                let Some(generation) = *generation else {
                    tracing::warn!("the origin's stream caught up without an Opening");
                    return self.source_lost(key, was_caught_up, out);
                };
                // Again, after a new Hello at the origin: nothing to
                // replay, the cursor is where it was.
                let held = catch_up.take().unwrap_or(CatchUp {
                    reset: false,
                    events: Vec::new(),
                });
                let agent = key.agent_key();
                match self.land_catch_up(&agent, session, held, marker.revision, generation, out) {
                    Ok(Some(true)) => self.source_ended(key, true, out),
                    Ok(Some(false)) => {
                        if let Some(Source {
                            feeding: Feeding::Streaming { caught_up, .. },
                            ..
                        }) = self.sources.open.get_mut(key)
                        {
                            *caught_up = true;
                        }
                    }
                    Ok(None) => self.source_ended(key, false, out),
                    Err(error) => {
                        tracing::warn!(%error, "absorbing a catch-up failed");
                        self.source_lost(key, was_caught_up, out);
                    }
                }
                return;
            }
            Some(
                of @ (session_event::Of::Snapshot(_)
                | session_event::Of::Item(_)
                | session_event::Of::Append(_)),
            ) => of,
            // The origin closes the stream after Lagged; it is lost.
            Some(session_event::Of::Lagged(_)) => {
                let caught_up = *caught_up;
                return self.source_lost(key, caught_up, out);
            }
            // An origin follows its own agents; it never detaches them.
            Some(session_event::Of::Detached(_)) | None => return,
        };
        if let Some(held) = catch_up.as_mut() {
            held.events.push(event_of(record));
            return;
        }
        let was_caught_up = *caught_up;
        match self.land_live(&key.agent_key(), session, source_event(record), out) {
            Ok(true) => {}
            Ok(false) => self.source_ended(key, false, out),
            Err(error) => {
                tracing::warn!(%error, "absorbing a live record failed");
                self.source_lost(key, was_caught_up, out);
            }
        }
    }

    // --- absorbing -------------------------------------------------------------

    /// Absorbs a whole catch-up and the CaughtUp that ends it, then answers
    /// what the subscribers need. A Reset begins the block under
    /// `generation`, the origin's from the stream's Opening; `revision`
    /// becomes the cursor. Returns whether the agent has exited, or None
    /// when the source's session or the agent's row is gone.
    fn land_catch_up(
        &mut self,
        key: &AgentKey,
        session: u64,
        catch_up: CatchUp,
        revision: u64,
        generation: u64,
        out: &mut Vec<Output>,
    ) -> Result<Option<bool>, StoreError> {
        let Some(host) = HostId::from_slice(&key.host) else {
            return Ok(None);
        };
        if !self.sources.is_current(host, session) {
            return Ok(None);
        }
        let Some(row) = self.store.agent(key)? else {
            return Ok(None);
        };
        let events: Vec<SourceEvent> = catch_up
            .events
            .into_iter()
            .filter_map(|event| event.of.map(source_event))
            .collect();
        let publish = |daemon: &mut Daemon, out: &mut Vec<Output>, of| {
            daemon.publish_session(key, Arc::new(event(of)), out);
        };
        if catch_up.reset {
            let (tail, snapshot, rest) = fold_tail(events, &row);
            self.store.absorb(
                key,
                Absorb::Reset {
                    tail: tail.clone(),
                    snapshot,
                    generation,
                },
            )?;
            let absorbed_rest = if rest.is_empty() {
                Vec::new()
            } else {
                self.store
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
            publish(self, out, session_event::Of::Reset(wire::Reset {}));
            let held = self.store.agent(key)?.unwrap_or(row);
            if let Some(snapshot) = held.snapshot {
                publish(self, out, session_event::Of::Snapshot(snapshot));
            }
            for item in tail {
                if let Some(stored) = self.store.get(key, &item.key)? {
                    publish(self, out, session_event::Of::Item(stored));
                }
            }
            for record in absorbed_rest {
                publish(self, out, record_of(record));
            }
        } else if !events.is_empty() {
            let absorbed = self.store.absorb(
                key,
                Absorb::Delta {
                    events,
                    live: false,
                },
            )?;
            for record in absorbed.stored {
                publish(self, out, record_of(record));
            }
        }
        self.store.absorb(key, Absorb::CaughtUp(revision))?;
        out.push(Output::Shell(ShellWork::Wrote));
        publish(
            self,
            out,
            session_event::Of::CaughtUp(wire::CaughtUp { revision }),
        );
        let row = self.store.agent(key)?;
        self.publish_row(key, out)?;
        Ok(row.map(|row| row.lifecycle == wire::Lifecycle::Exited as i32))
    }

    /// Absorbs one live record and answers it if it changed a row. False
    /// when the source's session or the agent's row is gone.
    fn land_live(
        &mut self,
        key: &AgentKey,
        session: u64,
        record: SourceEvent,
        out: &mut Vec<Output>,
    ) -> Result<bool, StoreError> {
        let current = HostId::from_slice(&key.host)
            .is_some_and(|host| self.sources.is_current(host, session));
        if !current || self.store.agent(key)?.is_none() {
            return Ok(false);
        }
        let envelope = matches!(record, SourceEvent::Snapshot(_));
        let absorbed = self.store.absorb(
            key,
            Absorb::Delta {
                events: vec![record],
                live: true,
            },
        )?;
        out.push(Output::Shell(ShellWork::Wrote));
        for record in absorbed.stored {
            self.publish_session(key, Arc::new(event(record_of(record))), out);
        }
        if envelope {
            self.publish_row(key, out)?;
        }
        Ok(true)
    }

    // --- pages below the block -----------------------------------------------------

    /// Asks a replica's origin for up to `limit` items below
    /// `before_order` for a client's Fetch, which answers with them after
    /// what is `held`, while the replica's host is ready. When the origin
    /// cannot be asked the held items come back.
    pub(crate) fn ask_page(
        &mut self,
        call: CallId,
        agent: &AgentKey,
        before_order: Option<u64>,
        held: Vec<Item>,
        limit: u32,
        out: &mut Vec<Output>,
    ) -> Result<(), Vec<Item>> {
        let Some(host) = HostId::from_slice(&agent.host) else {
            return Err(held);
        };
        let Some(&session) = self.sources.ready.get(&host) else {
            return Err(held);
        };
        let page = Page {
            call,
            agent: Key::from(agent),
            before_order,
            session,
            held,
        };
        let made = self.pending.peers.mint();
        self.pending
            .peers
            .waiting
            .insert(made, Waiting::Page(Box::new(page)));
        out.push(Output::Call {
            host,
            call: made,
            request: PeerRequest::Fetch(wire::FetchRequest {
                agent_id: agent.agent.clone(),
                before_order,
                limit,
            }),
            absorbed: true,
        });
        Ok(())
    }

    /// The origin's page, or why there is none: absorbed where it joins the
    /// block, and answered after what was held.
    pub(crate) fn page_answered(&mut self, page: Page, answer: Answer, out: &mut Vec<Output>) {
        let older = match answer {
            Answer::Reply(reply) => match *reply {
                PeerReply::Fetched(page) => Some(page),
                _ => None,
            },
            _ => None,
        };
        let agent = page.agent.agent_key();
        if let (Some(older), Some(before_order)) = (&older, page.before_order) {
            // An answer from a session that has since ended describes a
            // generation this store may already have dropped.
            let current = HostId::from_slice(&agent.host)
                .is_some_and(|host| self.sources.is_current(host, page.session));
            if current && self.store.agent(&agent).ok().flatten().is_some() {
                let absorbed = self.store.absorb(
                    &agent,
                    Absorb::Page {
                        before_order,
                        items: older.items.clone(),
                        exhausted: older.exhausted,
                    },
                );
                match absorbed {
                    Ok(_) => out.push(Output::Shell(ShellWork::Wrote)),
                    Err(error) => {
                        tracing::warn!(%error, "absorbing a page from the origin failed");
                    }
                }
            }
        }
        // A call cancelled while its origin was asked wants no answer.
        if !self.release_call(page.call) {
            return;
        }
        answer_page(page.call, page.held, older, out);
    }
}

/// Answers a Fetch with what was held and the origin's page below it. An
/// origin that could not be asked is an error when nothing was held, and
/// otherwise what is held is served and the next page down asks again.
pub(crate) fn answer_page(
    call: CallId,
    held: Vec<Item>,
    older: Option<FetchResponse>,
    out: &mut Vec<Output>,
) {
    let response = match older {
        Some(older) => {
            let mut items = held;
            items.extend(older.items);
            FetchResponse {
                items,
                exhausted: older.exhausted,
            }
        }
        None if held.is_empty() => {
            out.push(Output::EndCall {
                call,
                status: Status::Failed(CallError::OriginUnreachable),
            });
            return;
        }
        None => FetchResponse {
            items: held,
            exhausted: false,
        },
    };
    answered(call, Response::Fetched(response), out);
}

fn streaming(call: CallId, tail: bool) -> Feeding {
    Feeding::Streaming {
        call,
        generation: None,
        catch_up: Some(CatchUp {
            reset: tail,
            events: Vec::new(),
        }),
        caught_up: false,
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
        agent_id: row.agent.agent.clone(),
        kind: row.kind,
        ..wire::Snapshot::default()
    });
    let mut tail: Vec<Item> = items.into_values().collect();
    tail.sort_by_key(|item| item.order);
    (tail, snapshot, rest)
}

fn record_of(record: Record) -> session_event::Of {
    match record {
        Record::Item(item) => session_event::Of::Item(item),
        Record::Append(append) => session_event::Of::Append(append),
        Record::Snapshot(snapshot) => session_event::Of::Snapshot(snapshot),
    }
}

/// A record as a catch-up holds it until its CaughtUp.
fn event_of(of: session_event::Of) -> SessionEvent {
    event(of)
}

/// A record of a source's stream as the store absorbs it. Only records are
/// held or landed.
fn source_event(of: session_event::Of) -> SourceEvent {
    match of {
        session_event::Of::Item(item) => SourceEvent::Item(item),
        session_event::Of::Append(append) => SourceEvent::Append(append),
        session_event::Of::Snapshot(snapshot) => SourceEvent::Snapshot(snapshot),
        other => unreachable!("only records are held: {other:?}"),
    }
}

/// The replica row an inventory row describes.
pub fn from_wire(agent: &Agent) -> AgentRow {
    let mut row = AgentRow::new(
        AgentKey::new(agent.host_id.clone(), agent.agent_id.clone()),
        agent.kind,
        agent.cwd.clone(),
    );
    row.name = agent.name.clone();
    row.parent = agent
        .parent
        .as_ref()
        .map(|parent| AgentKey::new(parent.host_id.clone(), parent.agent_id.clone()));
    row.lifecycle = agent.lifecycle;
    row.exit_cause = agent.exit_cause;
    row.exit_detail = agent.exit_detail.clone();
    row.phase = agent.phase;
    row.working_on = agent.working_on.clone();
    row.phase_since_ms = Some(agent.phase_since_ms);
    row.git = agent.git.clone();
    row.created_at_ms = agent.created_at_ms;
    row.incarnation = agent.incarnation;
    row.background_jobs = agent.background_jobs;
    row.descendants = agent.descendants;
    row
}

#[cfg(test)]
mod tests {
    use store::Sqlite;
    use wire::{CaughtUp, HostEntry, InventoryEvent, Opening};

    use super::*;
    use crate::payload::{CarrierKind, ClientRequest};
    use crate::{Input, LinkId, StartOptions};

    const OWN: [u8; 16] = *b"own-host-0000000";
    const PEER: [u8; 16] = *b"far-host-0000000";
    const THEIRS: [u8; 16] = *b"agent-1111111111";
    /// The client's Subscribe on the replica.
    const WATCHING: CallId = CallId(50);

    fn daemon() -> Daemon {
        Daemon::new(
            Sqlite::open_in_memory(OWN).expect("a store opens"),
            StartOptions {
                generation: 1,
                ..StartOptions::default()
            },
        )
    }

    fn step(daemon: &mut Daemon, input: Input) -> Vec<Output> {
        let mut out = Vec::new();
        daemon.step(input, &mut out);
        out
    }

    fn key() -> AgentKey {
        AgentKey::new(PEER, THEIRS)
    }

    fn listed(lifecycle: wire::Lifecycle) -> Agent {
        Agent {
            host_id: PEER.to_vec(),
            agent_id: THEIRS.to_vec(),
            kind: wire::Kind::Codex as i32,
            cwd: "/src".into(),
            lifecycle: lifecycle as i32,
            ..Agent::default()
        }
    }

    /// The calls the step made of `PEER`.
    fn calls(out: &[Output]) -> Vec<(CallId, PeerRequest)> {
        out.iter()
            .filter_map(|output| match output {
                Output::Call {
                    host,
                    call,
                    request,
                    absorbed: true,
                } if *host == HostId(PEER) => Some((*call, request.clone())),
                _ => None,
            })
            .collect()
    }

    fn inventory_event(call: CallId, of: inventory_event::Of) -> Input {
        Input::PeerAnswer {
            call,
            answer: Answer::Inventory(InventoryEvent { of: Some(of) }),
        }
    }

    fn session(call: CallId, of: session_event::Of) -> Input {
        Input::PeerAnswer {
            call,
            answer: Answer::Session(event(of)),
        }
    }

    fn item(key: &str, order: u64, revision: u64) -> Item {
        Item {
            key: key.into(),
            text: format!("row {order}"),
            order,
            revision,
            ..Item::default()
        }
    }

    /// What the client's stream on the replica was answered.
    fn answered_to_watching(out: &[Output]) -> Vec<session_event::Of> {
        out.iter()
            .filter_map(|output| match output {
                Output::Answer {
                    call: WATCHING,
                    response: Response::Session(event),
                } => event.of.clone(),
                _ => None,
            })
            .collect()
    }

    /// A daemon that trusts `PEER`, linked to it, following its inventory:
    /// the inventory call it made.
    fn following(daemon: &mut Daemon) -> CallId {
        assert_eq!(
            step(
                daemon,
                Input::Trusted {
                    hosts: vec![HostId(PEER)]
                }
            )
            .iter()
            .filter(|output| matches!(output, Output::ArmTimer { .. }))
            .count(),
            0,
            "nothing is followed before a route reaches it"
        );
        let up = step(
            daemon,
            Input::LinkUp {
                link: LinkId(1),
                peer: HostId(PEER),
                kind: CarrierKind::Direct,
            },
        );
        assert!(
            up.contains(&Output::ArmTimer {
                id: TimerId::Follow(HostId(PEER)),
                at_ms: 0,
            }),
            "a route reaching a trusted host follows it at once, by a timer: {up:?}"
        );
        assert_eq!(
            calls(&up),
            Vec::new(),
            "a step that reports a link makes no call: it holds no store"
        );
        let due = step(
            daemon,
            Input::Timer {
                id: TimerId::Follow(HostId(PEER)),
            },
        );
        match calls(&due).as_slice() {
            [(call, PeerRequest::SubscribeInventory { .. })] => *call,
            other => panic!("the timer subscribes to the inventory: {other:?}"),
        }
    }

    /// Following, listed the replica agent and caught up: the source call.
    fn sourced(daemon: &mut Daemon) -> (CallId, CallId) {
        let inventory = following(daemon);
        let host = HostEntry {
            host_id: PEER.to_vec(),
            generation: 3,
            ..HostEntry::default()
        };
        step(
            daemon,
            inventory_event(inventory, inventory_event::Of::Host(host)),
        );
        let agent = listed(wire::Lifecycle::Live);
        let quiet = step(
            daemon,
            inventory_event(inventory, inventory_event::Of::Agent(agent)),
        );
        assert!(
            daemon.store.agent(&key()).unwrap().is_none(),
            "a listing is written only at its CaughtUp: {quiet:?}"
        );
        let caught_up = step(
            daemon,
            inventory_event(
                inventory,
                inventory_event::Of::CaughtUp(CaughtUp::default()),
            ),
        );
        assert!(caught_up.contains(&Output::Shell(ShellWork::HostCurrent {
            host: HostId(PEER),
            current: true,
        })));
        assert!(daemon.sources().ready(HostId(PEER)));
        assert_eq!(daemon.store.host_generation(&PEER).unwrap(), Some(3));
        let source = match calls(&caught_up).as_slice() {
            [(call, PeerRequest::Subscribe(request))] => {
                assert_eq!(request.agent_id, THEIRS.to_vec());
                assert_eq!(
                    request.from,
                    Some(peer_subscribe_request::From::Tail(200)),
                    "with no block it asks for a tail of K"
                );
                *call
            }
            other => panic!("the sweep opens the listed agent's source: {other:?}"),
        };
        assert!(caught_up.contains(&Output::Shell(ShellWork::Sourcing {
            agent: key(),
            open: true,
        })));
        (inventory, source)
    }

    #[test]
    fn a_trusted_host_a_route_reaches_is_followed_and_its_listing_opens_a_source_per_agent() {
        let mut daemon = daemon();
        sourced(&mut daemon);
        assert_eq!(daemon.sources().open().collect::<Vec<_>>(), vec![key()]);
    }

    #[test]
    fn a_catch_up_is_absorbed_whole_at_its_caught_up_and_only_then_answered() {
        let mut daemon = daemon();
        let (_, source) = sourced(&mut daemon);
        let opened = step(
            &mut daemon,
            Input::ClientCall {
                call: WATCHING,
                request: ClientRequest::Subscribe {
                    agent_id: THEIRS.to_vec(),
                    tail: 10,
                },
            },
        );
        assert!(
            !answered_to_watching(&opened)
                .iter()
                .any(|of| matches!(of, session_event::Of::Detached(_))),
            "a followed replica is not served as detached: {opened:?}"
        );

        step(
            &mut daemon,
            session(
                source,
                session_event::Of::Opening(Opening { generation: 3 }),
            ),
        );
        let held = step(
            &mut daemon,
            session(source, session_event::Of::Item(item("a", 5, 1))),
        );
        assert_eq!(answered_to_watching(&held), Vec::new());
        assert!(daemon.store.get(&key(), "a").unwrap().is_none());

        let landed = step(
            &mut daemon,
            session(
                source,
                session_event::Of::CaughtUp(CaughtUp { revision: 1 }),
            ),
        );
        assert!(daemon.store.get(&key(), "a").unwrap().is_some());
        let told = answered_to_watching(&landed);
        assert!(
            matches!(
                told.as_slice(),
                [
                    session_event::Of::Reset(_),
                    session_event::Of::Snapshot(_),
                    session_event::Of::Item(_),
                    session_event::Of::CaughtUp(CaughtUp { revision: 1 }),
                ]
            ),
            "a tail replaces the block, then reads as the store holds it: {told:?}"
        );
        assert!(landed.contains(&Output::Shell(ShellWork::Wrote)));
        let row = daemon.store.agent(&key()).unwrap().unwrap();
        assert_eq!((row.source_cursor, row.source_generation), (1, 3));

        // A live record after the catch-up is absorbed and answered at once.
        let live = step(
            &mut daemon,
            session(source, session_event::Of::Item(item("b", 6, 2))),
        );
        assert!(matches!(
            answered_to_watching(&live).as_slice(),
            [session_event::Of::Item(Item { revision: 2, .. })]
        ));
    }

    #[test]
    fn a_lost_inventory_ends_the_session_and_backs_off_unless_the_route_moves() {
        let mut daemon = daemon();
        let (inventory, source) = sourced(&mut daemon);
        step(
            &mut daemon,
            Input::ClientCall {
                call: WATCHING,
                request: ClientRequest::Subscribe {
                    agent_id: THEIRS.to_vec(),
                    tail: 10,
                },
            },
        );
        let lost = step(
            &mut daemon,
            Input::PeerAnswer {
                call: inventory,
                answer: Answer::Ended,
            },
        );
        assert!(lost.contains(&Output::HangUp { call: source }));
        assert!(lost.contains(&Output::Shell(ShellWork::HostCurrent {
            host: HostId(PEER),
            current: false,
        })));
        assert!(matches!(
            answered_to_watching(&lost).as_slice(),
            [session_event::Of::Detached(_)]
        ));
        assert!(
            lost.contains(&Output::ArmTimer {
                id: TimerId::Follow(HostId(PEER)),
                at_ms: 1_000,
            }),
            "followed again after the first backoff: {lost:?}"
        );
        assert!(!daemon.sources().ready(HostId(PEER)));
        assert_eq!(daemon.sources().open().count(), 0);

        // An answer of the session that ended is not absorbed.
        step(
            &mut daemon,
            session(source, session_event::Of::Item(item("late", 9, 9))),
        );
        assert!(daemon.store.get(&key(), "late").unwrap().is_none());

        // The host comes back on a new link: followed at once.
        step(
            &mut daemon,
            Input::LinkDown {
                link: LinkId(1),
                reason: wire::LinkCloseReason::Unspecified,
            },
        );
        let back = step(
            &mut daemon,
            Input::LinkUp {
                link: LinkId(2),
                peer: HostId(PEER),
                kind: CarrierKind::Direct,
            },
        );
        assert!(
            back.contains(&Output::ArmTimer {
                id: TimerId::Follow(HostId(PEER)),
                at_ms: 0,
            }),
            "{back:?}"
        );
    }

    #[test]
    fn a_page_below_the_block_is_asked_of_the_origin_and_joins_the_block() {
        let mut daemon = daemon();
        let (_, source) = sourced(&mut daemon);
        step(
            &mut daemon,
            session(
                source,
                session_event::Of::Opening(Opening { generation: 3 }),
            ),
        );
        step(
            &mut daemon,
            session(source, session_event::Of::Item(item("e", 5, 4))),
        );
        step(
            &mut daemon,
            session(
                source,
                session_event::Of::CaughtUp(CaughtUp { revision: 4 }),
            ),
        );

        let asked = step(
            &mut daemon,
            Input::ClientCall {
                call: CallId(60),
                request: ClientRequest::Fetch(wire::FetchRequest {
                    agent_id: THEIRS.to_vec(),
                    before_order: None,
                    limit: 10,
                }),
            },
        );
        let page = match calls(&asked).as_slice() {
            [(call, PeerRequest::Fetch(request))] => {
                assert_eq!((request.before_order, request.limit), (Some(5), 9));
                *call
            }
            other => panic!("the origin is asked below the block: {other:?}"),
        };
        let reply = wire::FetchResponse {
            items: vec![item("c", 3, 2)],
            exhausted: true,
        };
        let answered = step(
            &mut daemon,
            Input::PeerAnswer {
                call: page,
                answer: Answer::Reply(Box::new(PeerReply::Fetched(reply))),
            },
        );
        match answered.as_slice() {
            [
                Output::Shell(ShellWork::Wrote),
                Output::Answer {
                    call: CallId(60),
                    response: Response::Fetched(fetched),
                },
                Output::EndCall {
                    call: CallId(60),
                    status: Status::Ok,
                },
            ] => {
                let orders: Vec<u64> = fetched.items.iter().map(|item| item.order).collect();
                assert_eq!(orders, vec![5, 3], "held first, then the origin's");
                assert!(fetched.exhausted);
            }
            other => panic!("the page is absorbed and answered: {other:?}"),
        }
        assert!(
            daemon.store.get(&key(), "c").unwrap().is_some(),
            "the page joins the block"
        );
    }

    #[test]
    fn a_host_no_longer_trusted_is_followed_no_more_and_its_replicas_go() {
        let mut daemon = daemon();
        let (inventory, source) = sourced(&mut daemon);
        let untrusted = step(&mut daemon, Input::Trusted { hosts: Vec::new() });
        assert!(untrusted.contains(&Output::HangUp { call: inventory }));
        assert!(untrusted.contains(&Output::HangUp { call: source }));
        assert!(
            untrusted.contains(&Output::Shell(ShellWork::ReplicaDropped {
                agent: key(),
                listed: false,
            }))
        );
        assert!(daemon.store.agent(&key()).unwrap().is_none());
        assert!(!daemon.sources.is_current(HostId(PEER), 1));
        assert_eq!(daemon.pending.peer_calls(), 0, "nothing is left waiting");
    }

    #[test]
    fn sources_survive_a_checkpoint() {
        let mut daemon = daemon();
        sourced(&mut daemon);
        let checkpoint =
            crate::Checkpoint::from_bytes(&daemon.checkpoint().to_bytes()).expect("it reads back");
        let store = std::mem::replace(
            &mut daemon.store,
            Sqlite::open_in_memory(OWN).expect("a store opens"),
        );
        let restored = Daemon::restore(checkpoint, store).expect("it restores");
        assert_eq!(restored.sources, daemon.sources);
        assert_eq!(restored.pending, daemon.pending);
    }

    /// A kind this daemon has no name for is a number it carries: the
    /// replica row keeps it from the origin's inventory row to the one this
    /// daemon lists, and the items and snapshot a reset lands keep theirs.
    #[test]
    fn unknown_kind_survives_replication() {
        const UNKNOWN: i32 = 99;
        let mut store = Sqlite::open_in_memory(*b"here-0000000000x").unwrap();
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

        let listed = crate::serve::to_wire(&store.agent(&key).unwrap().unwrap());
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
            let mut store = Sqlite::open(&path, b"here".to_vec()).unwrap();
            store.put_agent(&row).unwrap();
            // The origin lists it again: the held row takes the same values.
            store.put_agent(&from_wire(&origin)).unwrap();
        }
        let store = Sqlite::open(&path, b"here".to_vec()).unwrap();
        let listed = crate::serve::to_wire(&store.agent(&key).unwrap().unwrap());
        assert_eq!(
            (listed.exit_cause, listed.exit_detail.as_deref()),
            (UNKNOWN, Some("a newer daemon's words")),
            "listed with its cause, not unspecified"
        );
    }
}
