//! Fan-out: one bounded broadcast channel per agent, and one for the
//! inventory.
//!
//! Ingest publishes each committed record once, wrapped in an Arc, after
//! its transaction commits; every subscription holds its own cursor into
//! the channel's ring. Publishing never waits: a subscription that falls
//! more than the capacity behind is told it lagged and closed, and its
//! client re-tails from the store, which is where late arrivers catch up.
//! The store is never a buffer for a live stream.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use store::AgentKey;
use tokio::sync::broadcast;
use wire::{InventoryEvent, SessionEvent};

pub(crate) struct Fanout {
    capacity: usize,
    agents: Mutex<HashMap<AgentKey, broadcast::Sender<Arc<SessionEvent>>>>,
    inventory: broadcast::Sender<Arc<InventoryEvent>>,
    /// Agents being deleted: what their stop writes is not published, so
    /// no subscriber reads the stop as an exit it could resume from before
    /// the removal arrives.
    muted: Mutex<HashSet<AgentKey>>,
}

impl Fanout {
    pub(crate) fn new(capacity: usize, inventory_capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            agents: Mutex::new(HashMap::new()),
            inventory: broadcast::Sender::new(inventory_capacity.max(1)),
            muted: Mutex::new(HashSet::new()),
        }
    }

    /// Publishes nothing more about `agent`, its session or its row, until
    /// [`Fanout::unmute`]; its subscriptions stay open.
    pub(crate) fn mute(&self, agent: &AgentKey) {
        self.muted.lock().unwrap().insert(agent.clone());
    }

    pub(crate) fn unmute(&self, agent: &AgentKey) {
        self.muted.lock().unwrap().remove(agent);
    }

    pub(crate) fn muted(&self, agent: &AgentKey) -> bool {
        self.muted.lock().unwrap().contains(agent)
    }

    /// Joins an agent's channel, creating it on first use.
    pub(crate) fn join(&self, agent: &AgentKey) -> broadcast::Receiver<Arc<SessionEvent>> {
        self.agents
            .lock()
            .unwrap()
            .entry(agent.clone())
            .or_insert_with(|| broadcast::Sender::new(self.capacity))
            .subscribe()
    }

    /// Publishes to an agent's subscribers. With none, the event is dropped:
    /// whoever subscribes later reads it from the store.
    pub(crate) fn publish(&self, agent: &AgentKey, event: SessionEvent) {
        if self.muted(agent) {
            return;
        }
        let mut agents = self.agents.lock().unwrap();
        let Some(sender) = agents.get(agent) else {
            return;
        };
        if sender.send(Arc::new(event)).is_err() {
            // Every subscriber has gone; forget the channel so an idle
            // agent holds no ring.
            agents.remove(agent);
        }
    }

    /// Whether any client is subscribed to an agent now.
    pub(crate) fn watched(&self, agent: &AgentKey) -> bool {
        self.agents
            .lock()
            .unwrap()
            .get(agent)
            .is_some_and(|sender| sender.receiver_count() > 0)
    }

    /// Ends every subscription on an agent that no longer exists.
    pub(crate) fn close(&self, agent: &AgentKey) {
        self.agents.lock().unwrap().remove(agent);
    }

    pub(crate) fn join_inventory(&self) -> broadcast::Receiver<Arc<InventoryEvent>> {
        self.inventory.subscribe()
    }

    pub(crate) fn publish_inventory(&self, event: InventoryEvent) {
        // No subscribers is the ordinary case, not an error.
        let _ = self.inventory.send(Arc::new(event));
    }
}
