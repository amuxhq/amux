//! The streams a client opens through the core: Subscribe on an agent,
//! SubscribeInventory on the fleet. The core answers the opening and every
//! change after it; these read the call's queue.

use std::sync::Arc;

use daemon_core::payload::{PeerRequest, Response};
use wire::{InventoryEvent, SessionEvent};

use super::{Answers, Ask, Delivered};
use crate::routing::core_host;
use crate::runtime::ProfileRuntime;
use crate::serve::ServeError;

/// One Subscribe stream: the opening the core read, then every change the
/// core answers after it, verbatim.
pub struct Subscription {
    answers: Answers,
}

impl Subscription {
    /// The next event, or None once the stream has ended: after Lagged, or
    /// when the agent is deleted.
    pub async fn next(&mut self) -> Option<Arc<SessionEvent>> {
        loop {
            match self.answers.next().await? {
                Delivered::Answer(Response::Session(event)) => return Some(event),
                Delivered::Answer(_) => {}
                Delivered::End(_) => return None,
            }
        }
    }
}

/// One SubscribeInventory stream: hosts and agent rows, CaughtUp, then
/// every change.
pub struct InventorySubscription {
    answers: Answers,
}

impl InventorySubscription {
    /// The next event, or None once the stream has ended: after Lagged, or
    /// when the profile shuts down.
    pub async fn next(&mut self) -> Option<Arc<InventoryEvent>> {
        loop {
            match self.answers.next().await? {
                Delivered::Answer(Response::Inventory(event)) => return Some(event),
                Delivered::Answer(_) => {}
                Delivered::End(_) => return None,
            }
        }
    }
}

impl ProfileRuntime {
    /// Opens a Subscribe stream on an agent: its Opening, its Snapshot,
    /// the newest `tail` rows of its block (at most K), its marker if one
    /// is set, then every change committed after it.
    pub async fn subscribe(&self, agent_id: &[u8], tail: u32) -> Result<Subscription, ServeError> {
        let ask = Ask::Subscribe {
            agent_id: agent_id.to_vec(),
            tail,
        };
        let room = self.launch().fanout_capacity;
        let mut answers = self.driver().call(ask, room).await;
        answers.opened().await?;
        Ok(Subscription { answers })
    }

    /// Opens a Subscribe stream on one of this host's own agents for the
    /// paired host `from`, whose replica holds it as `request` says: the
    /// rows revised since its cursor, in revision order, when there are at
    /// most its cap; otherwise, or when the cursor's generation is not the
    /// one this origin runs, Reset and the newest rows. Either way the
    /// Opening and Snapshot lead and the marker follows, then every event.
    /// An agent this host only replicates is not found.
    pub async fn peer_subscribe(
        &self,
        from: crate::HostId,
        request: wire::PeerSubscribeRequest,
    ) -> Result<Subscription, ServeError> {
        let room = self.launch().fanout_capacity;
        let request = PeerRequest::Subscribe(request);
        let mut answers = self
            .driver()
            .peer_call(core_host(from), request, room)
            .await;
        answers.opened().await?;
        Ok(Subscription { answers })
    }

    /// Opens a SubscribeInventory stream: the host set and every agent row
    /// the store holds, CaughtUp, then every change.
    pub async fn subscribe_inventory(&self) -> Result<InventorySubscription, ServeError> {
        let room = self.launch().inventory_capacity;
        let mut answers = self.driver().call(Ask::SubscribeInventory, room).await;
        answers.opened().await?;
        Ok(InventorySubscription { answers })
    }

    /// Opens the SubscribeInventory stream the paired host `from` reads:
    /// this host's entry and its own agents, on opening and live. What this
    /// host knows of other hosts is theirs to tell, so it is never passed
    /// on.
    pub(crate) async fn peer_subscribe_inventory(
        &self,
        from: crate::HostId,
    ) -> Result<InventorySubscription, ServeError> {
        let room = self.launch().inventory_capacity;
        let request = PeerRequest::SubscribeInventory;
        let mut answers = self
            .driver()
            .peer_call(core_host(from), request, room)
            .await;
        answers.opened().await?;
        Ok(InventorySubscription { answers })
    }
}
