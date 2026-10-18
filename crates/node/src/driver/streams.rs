//! The streams a client opens through the core: Subscribe on an agent,
//! SubscribeInventory on the fleet. The core answers the opening and every
//! change after it; these read the call's queue.

use std::sync::Arc;

use daemon_core::payload::{Caller, Opening, Response};
use wire::{InventoryEvent, SessionEvent};

use super::{Answers, Ask, Delivered};
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
        self.open_subscription(Caller::Local, agent_id, Opening::Tail(tail))
            .await
    }

    /// Opens a Subscribe stream for a peer that holds everything through
    /// `revision` of this origin's `generation`: the rows revised since,
    /// in revision order, when there are at most `cap`; otherwise, or when
    /// the generation is not the one this origin runs, Reset and the
    /// newest `cap` rows. Either way the Opening and Snapshot lead and the
    /// marker follows, then every event.
    pub async fn subscribe_after(
        &self,
        agent_id: &[u8],
        revision: u64,
        cap: u32,
        generation: u64,
    ) -> Result<Subscription, ServeError> {
        let from = Opening::After {
            revision,
            cap,
            generation,
        };
        self.open_subscription(Caller::Peer, agent_id, from).await
    }

    async fn open_subscription(
        &self,
        who: Caller,
        agent_id: &[u8],
        from: Opening,
    ) -> Result<Subscription, ServeError> {
        let ask = Ask::Subscribe {
            agent_id: agent_id.to_vec(),
            from,
        };
        let room = self.launch().fanout_capacity;
        let mut answers = self.driver().call(who, ask, room).await;
        answers.opened().await?;
        Ok(Subscription { answers })
    }

    /// Opens a SubscribeInventory stream: the host set and every agent row
    /// the store holds, CaughtUp, then every change.
    pub async fn subscribe_inventory(&self) -> Result<InventorySubscription, ServeError> {
        self.open_inventory(Caller::Local, None).await
    }

    /// Opens the SubscribeInventory stream a paired host reads: this host's
    /// entry and its own agents, on opening and live. What this host knows
    /// of other hosts is theirs to tell, so it is never passed on.
    pub(crate) async fn subscribe_own_inventory(
        &self,
    ) -> Result<InventorySubscription, ServeError> {
        self.open_inventory(Caller::Peer, Some(self.host().as_bytes().to_vec()))
            .await
    }

    async fn open_inventory(
        &self,
        who: Caller,
        only: Option<Vec<u8>>,
    ) -> Result<InventorySubscription, ServeError> {
        let room = self.launch().inventory_capacity;
        let ask = Ask::SubscribeInventory { only };
        let mut answers = self.driver().call(who, ask, room).await;
        answers.opened().await?;
        Ok(InventorySubscription { answers })
    }
}
