//! The local services: answered by the node the client is attached to,
//! from its own copy, for its own agents and its copies of other hosts'.

use wire::amux::staging::draft_service_client::DraftServiceClient;
use wire::amux::staging::inventory_service_client::InventoryServiceClient;
use wire::amux::staging::session_service_client::SessionServiceClient;
use wire::amux::staging::{self};
use wire::{Agent, HostEntry, InventoryEvent, Item, ListItemsResponse, SessionEvent};

use super::{Conn, stub};
use crate::{EventStream, RpcError, held, held_events};

/// The fleet as the node holds it.
#[derive(Clone)]
pub struct Inventory {
    conn: Conn,
}

impl Inventory {
    pub(super) fn new(conn: Conn) -> Inventory {
        Inventory { conn }
    }

    fn stub(&self) -> InventoryServiceClient<Conn> {
        stub!(
            InventoryServiceClient<Conn>,
            (self.conn.clone(), self.conn.origin())
        )
    }

    /// Every host and agent row the node holds: the current set, CaughtUp,
    /// then changes as they happen.
    pub async fn subscribe(&self) -> Result<EventStream<InventoryEvent>, RpcError> {
        let stream = self
            .stub()
            .subscribe_inventory(staging::SubscribeInventoryRequest {})
            .await?
            .into_inner();
        Ok(held_events(stream, |response| response.event))
    }

    /// An agent by name across the fleet the node holds.
    pub async fn resolve_agent(&self, name: &str) -> Result<Agent, RpcError> {
        let request = staging::ResolveAgentRequest {
            name: name.to_owned(),
        };
        held(self.stub().resolve_agent(request).await?.into_inner().agent)
    }

    /// A host by the name a person uses for it. Its id is what a host
    /// service on it is addressed by ([`super::Client::host`]).
    pub async fn resolve_host(&self, name: &str) -> Result<HostEntry, RpcError> {
        let request = staging::ResolveHostRequest {
            name: name.to_owned(),
        };
        held(self.stub().resolve_host(request).await?.into_inner().host)
    }
}

/// The conversations the node holds, and the files they reference.
#[derive(Clone)]
pub struct Sessions {
    conn: Conn,
}

impl Sessions {
    pub(super) fn new(conn: Conn) -> Sessions {
        Sessions { conn }
    }

    fn stub(&self) -> SessionServiceClient<Conn> {
        stub!(
            SessionServiceClient<Conn>,
            (self.conn.clone(), self.conn.origin())
        )
    }

    /// One conversation from the newest `tail` rows: its opening, the rows,
    /// CaughtUp, and what follows.
    pub async fn subscribe(
        &self,
        agent_id: &[u8],
        tail: u32,
    ) -> Result<EventStream<SessionEvent>, RpcError> {
        let request = staging::SubscribeSessionRequest {
            agent_id: agent_id.to_vec(),
            tail,
        };
        let stream = self.stub().subscribe_session(request).await?.into_inner();
        Ok(held_events(stream, |response| response.event))
    }

    /// Up to `page_size` items older than `before_order`, newest first;
    /// absent, the newest.
    pub async fn list_items(
        &self,
        agent_id: &[u8],
        before_order: Option<u64>,
        page_size: u32,
    ) -> Result<ListItemsResponse, RpcError> {
        let request = staging::ListItemsRequest {
            agent_id: agent_id.to_vec(),
            before_order,
            page_size,
        };
        let page = self.stub().list_items(request).await?.into_inner();
        Ok(ListItemsResponse {
            items: page.items,
            exhausted: page.exhausted,
        })
    }

    pub async fn get_item(&self, agent_id: &[u8], key: &str) -> Result<Item, RpcError> {
        let request = staging::GetItemRequest {
            agent_id: agent_id.to_vec(),
            key: key.to_owned(),
        };
        held(self.stub().get_item(request).await?.into_inner().item)
    }

    /// A file a conversation references, by the hash its reference names.
    /// One the node does not hold yet it fetches from the agent's host.
    pub async fn get_blob(&self, agent_id: &[u8], hash: &[u8]) -> Result<Vec<u8>, RpcError> {
        let request = staging::GetBlobRequest {
            agent_id: agent_id.to_vec(),
            hash: hash.to_vec(),
        };
        Ok(self.stub().get_blob(request).await?.into_inner().bytes)
    }
}

/// What this machine's person has typed into an agent's composer and not
/// sent, kept by the node.
#[derive(Clone)]
pub struct Drafts {
    conn: Conn,
}

impl Drafts {
    pub(super) fn new(conn: Conn) -> Drafts {
        Drafts { conn }
    }

    fn stub(&self) -> DraftServiceClient<Conn> {
        stub!(
            DraftServiceClient<Conn>,
            (self.conn.clone(), self.conn.origin())
        )
    }

    /// Keeps the draft; an empty one forgets it.
    pub async fn update(
        &self,
        agent_id: &[u8],
        draft: wire::Draft,
    ) -> Result<wire::Draft, RpcError> {
        let request = staging::UpdateDraftRequest {
            agent_id: agent_id.to_vec(),
            draft: Some(draft),
        };
        held(self.stub().update_draft(request).await?.into_inner().draft)
    }

    pub async fn get(&self, agent_id: &[u8]) -> Result<wire::Draft, RpcError> {
        let request = staging::GetDraftRequest {
            agent_id: agent_id.to_vec(),
        };
        held(self.stub().get_draft(request).await?.into_inner().draft)
    }
}
