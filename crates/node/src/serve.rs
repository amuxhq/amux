//! Serving: what every client call reads. Subscribe, Fetch and Get read an
//! agent's rows and SubscribeInventory the fleet, each answered by the
//! profile's core through its driver; ResolveAgent reads the fleet here.
//! None touches an agent process.

use std::sync::Arc;

use daemon_core::payload::{CallError, Caller, Opening, Response, Status};
pub use daemon_core::serve::MAX_PAGE;
use prost::{Message as _, Name as _};
use store::{AgentKey, StoreError};
use wire::{
    Agent, AmbiguousAgentName, ErrorCode, ErrorDetail, FetchRequest, FetchResponse, GetRequest,
    InventoryEvent, Item, SessionEvent, inventory_event, session_event,
};

use crate::driver::{Answers, Ask, Delivered};
use crate::runtime::{ProfileRuntime, to_wire};

#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    #[error("no agent with that id")]
    NoAgent,
    #[error("no agent named {0}")]
    NoAgentNamed(String),
    #[error(
        "{} names more than one agent; pick one with {}",
        .detail.name,
        .forms.join(" or ")
    )]
    Ambiguous {
        detail: AmbiguousAgentName,
        /// A form that picks each candidate: `name@host`, or its id where
        /// its host's name does not tell it apart.
        forms: Vec<String>,
    },
    #[error("the agent holds no item {0}")]
    NoItem(String),
    #[error("older history is held by the agent's host, which cannot be reached")]
    OriginUnreachable,
    #[error("the store: {0}")]
    Store(#[from] StoreError),
    #[error("the store: {0}")]
    Failed(String),
    #[error("the profile is no longer running")]
    Stopped,
}

impl From<Status> for ServeError {
    /// How a call the core ended without answering failed.
    fn from(status: Status) -> Self {
        match status {
            Status::Ok => ServeError::Stopped,
            Status::Failed(CallError::NoAgent) => ServeError::NoAgent,
            Status::Failed(CallError::NoItem(key)) => ServeError::NoItem(key),
            Status::Failed(CallError::OriginUnreachable) => ServeError::OriginUnreachable,
            Status::Failed(CallError::Store(why)) => ServeError::Failed(why),
        }
    }
}

impl ServeError {
    /// The error as the wire carries it: a code, and for an ambiguous name
    /// the candidates as an `amux.v1.AmbiguousAgentName` detail.
    pub fn to_wire(&self) -> wire::Error {
        let code = match self {
            Self::NoAgent | Self::NoAgentNamed(_) | Self::NoItem(_) => ErrorCode::NotFound,
            Self::Ambiguous { .. } => ErrorCode::FailedPrecondition,
            Self::OriginUnreachable => ErrorCode::Unreachable,
            Self::Store(_) | Self::Failed(_) => ErrorCode::Internal,
            Self::Stopped => ErrorCode::Unavailable,
        };
        let details = match self {
            Self::Ambiguous { detail, .. } => vec![ErrorDetail {
                r#type: AmbiguousAgentName::full_name(),
                value: detail.encode_to_vec(),
            }],
            _ => Vec::new(),
        };
        wire::Error {
            code: code as i32,
            message: self.to_string(),
            details,
        }
    }
}

/// One Subscribe stream: the opening the core read, then every event the
/// agent's channel carries, verbatim.
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

pub(crate) fn event(of: session_event::Of) -> SessionEvent {
    SessionEvent { of: Some(of) }
}

pub(crate) fn inventory(of: inventory_event::Of) -> InventoryEvent {
    InventoryEvent { of: Some(of) }
}

impl ProfileRuntime {
    /// Opens a Subscribe stream on an agent: its Opening, its Snapshot,
    /// the newest `tail` rows of its block (at most K), its marker if one
    /// is set, then every event its source broadcasts.
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

    /// A page of older items by order, newest first. Held rows answer it;
    /// below a replica's block the history is at the origin, which is asked
    /// and whose answer extends the block where it joins it. An origin that
    /// cannot be asked is an error, never an empty page.
    pub async fn fetch(&self, request: &FetchRequest) -> Result<FetchResponse, ServeError> {
        let ask = Ask::Fetch(request.clone());
        match self.unary(ask).await? {
            Response::Fetched(page) => Ok(page),
            _ => Err(ServeError::Stopped),
        }
    }

    /// Whether the agent is one of this host's own, not a replica of
    /// another host's.
    pub(crate) async fn is_own(&self, agent_id: &[u8]) -> Result<bool, ServeError> {
        let store = self.store.lock().await;
        Ok(store
            .agent(&AgentKey::new(store.own_host(), agent_id))?
            .is_some())
    }

    /// One item in full.
    pub async fn get(&self, request: &GetRequest) -> Result<Item, ServeError> {
        match self.unary(Ask::Get(request.clone())).await? {
            Response::Item(item) => Ok(item),
            _ => Err(ServeError::Stopped),
        }
    }

    /// A call the core answers once.
    async fn unary(&self, ask: Ask) -> Result<Response, ServeError> {
        let mut answers = self.driver().call(Caller::Local, ask, 1).await;
        match answers.next().await {
            Some(Delivered::Answer(response)) => Ok(response),
            Some(Delivered::End(status)) => Err(status.into()),
            None => Err(ServeError::Stopped),
        }
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

    /// The one agent a reference names, across the fleet the store knows.
    /// A reference is any form amux prints: the agent's id with or without
    /// dashes, the eight-character short id `amux ls` shows (any prefix of
    /// the id at least that long), its name, or `name@host` where names on
    /// two hosts meet. Names are unique on a host, so `name@host` picks one.
    pub async fn resolve_agent(&self, reference: &str) -> Result<Agent, ServeError> {
        let rows = {
            let store = self.store.lock().await;
            if let Ok(id) = uuid::Uuid::parse_str(reference) {
                return store
                    .agent_by_id(id.as_bytes())?
                    .map(|row| to_wire(&row))
                    .ok_or_else(|| ServeError::NoAgentNamed(reference.to_owned()));
            }
            let (name, host) = match reference.rsplit_once('@') {
                Some((name, host)) => (name, Some(host)),
                None => (reference, None),
            };
            let mut rows = store.agent_by_name(name)?;
            if let Some(host) = host {
                let host = self
                    .resolve_host(host)
                    .map_err(|_| ServeError::NoAgentNamed(reference.to_owned()))?;
                rows.retain(|row| row.agent.host == host.as_bytes());
            } else if rows.is_empty() && is_short_id(reference) {
                let prefix = reference.to_ascii_lowercase();
                rows = store
                    .agents()?
                    .into_iter()
                    .filter(|row| {
                        uuid::Uuid::from_slice(&row.agent.agent)
                            .is_ok_and(|id| id.simple().to_string().starts_with(&prefix))
                    })
                    .collect();
            }
            rows
        };
        // One agent may be held twice (its own row and a copy); it is one
        // agent all the same.
        let mut matches: Vec<Agent> = Vec::new();
        for row in &rows {
            if !matches
                .iter()
                .any(|agent| agent.agent_id == row.agent.agent)
            {
                matches.push(to_wire(row));
            }
        }
        match matches.len() {
            0 => Err(ServeError::NoAgentNamed(reference.to_owned())),
            1 => Ok(matches.remove(0)),
            _ => {
                let forms = self.picking_forms(&matches);
                Err(ServeError::Ambiguous {
                    detail: AmbiguousAgentName {
                        name: reference.to_owned(),
                        candidates: matches,
                    },
                    forms,
                })
            }
        }
    }

    /// For each of several agents, a reference that picks it alone:
    /// `name@host` when its host's name is its own and no other candidate
    /// shares that host, else its id.
    fn picking_forms(&self, agents: &[Agent]) -> Vec<String> {
        let hosts = self.published_hosts();
        agents
            .iter()
            .map(|agent| {
                let host = hosts
                    .iter()
                    .find(|host| host.host_id == agent.host_id)
                    .map(|host| host.name.as_str())
                    .filter(|name| !name.is_empty());
                let alone_there = agents
                    .iter()
                    .filter(|other| other.host_id == agent.host_id)
                    .count()
                    == 1;
                let named_once =
                    |name: &str| hosts.iter().filter(|host| host.name == name).count() == 1;
                match host {
                    Some(host) if alone_there && named_once(host) => {
                        format!("{}@{host}", agent.name)
                    }
                    _ => uuid::Uuid::from_slice(&agent.agent_id)
                        .map(|id| id.to_string())
                        .unwrap_or_default(),
                }
            })
            .collect()
    }
}

/// Whether a reference could be a short id: at least the eight hex digits
/// `amux ls` prints, at most a whole id.
fn is_short_id(reference: &str) -> bool {
    (8..=32).contains(&reference.len()) && reference.chars().all(|c| c.is_ascii_hexdigit())
}
