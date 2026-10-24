//! Serving: what every client call reads. Fetch and Get read an agent's
//! rows, answered by the profile's core through its driver (whose streams
//! answer Subscribe and SubscribeInventory); ResolveAgent reads the fleet
//! here. None touches an agent process.

use daemon_core::payload::{CallError, PeerRequest, Response, Status};
pub use daemon_core::serve::MAX_PAGE;
use prost::{Message as _, Name as _};
use store::{AgentKey, StoreError};
use wire::{
    Agent, AmbiguousAgentName, ErrorCode, ErrorDetail, FetchRequest, FetchResponse, GetRequest,
    Item,
};

use crate::driver::{Answers, Ask, Delivered};
use crate::routing::core_host;
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

impl From<crate::runtime::Stopped> for ServeError {
    fn from(_: crate::runtime::Stopped) -> Self {
        ServeError::Stopped
    }
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
            Status::Failed(
                error @ (CallError::Peer(_)
                | CallError::NotServed
                | CallError::Registry(_)
                | CallError::NoBlob(_)
                | CallError::Blob(_)
                | CallError::Message(_)
                | CallError::Trust(_)
                | CallError::Window(_)
                | CallError::Pairing(_)),
            ) => ServeError::Failed(format!("{error:?}")),
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

impl ProfileRuntime {
    /// A page of older items by order, newest first. Held rows answer it;
    /// below a replica's block the history is at the origin, which is asked
    /// and whose answer extends the block where it joins it. An origin that
    /// cannot be asked is an error, never an empty page.
    pub async fn fetch(&self, request: &FetchRequest) -> Result<FetchResponse, ServeError> {
        let answers = self.driver().call(Ask::Fetch(request.clone()), 1).await;
        fetched(answers).await
    }

    /// A page of older items of one of this host's own agents, for the
    /// paired host `from`. An agent this host only replicates is not found.
    pub(crate) async fn peer_fetch(
        &self,
        from: crate::HostId,
        request: FetchRequest,
    ) -> Result<FetchResponse, ServeError> {
        let request = PeerRequest::Fetch(request);
        let answers = self.driver().peer_call(core_host(from), request, 1).await;
        fetched(answers).await
    }

    /// Whether the agent is one of this host's own, not a replica of
    /// another host's.
    pub(crate) async fn is_own(&self, agent_id: &[u8]) -> Result<bool, ServeError> {
        let agent_id = agent_id.to_vec();
        Ok(self
            .read_rows(move |store| store.agent(&AgentKey::new(store.own_host(), agent_id)))
            .await?
            .is_some())
    }

    /// One item in full.
    pub async fn get(&self, request: &GetRequest) -> Result<Item, ServeError> {
        let answers = self.driver().call(Ask::Get(request.clone()), 1).await;
        item(answers).await
    }

    /// One item in full of one of this host's own agents, for the paired
    /// host `from`. An agent this host only replicates is not found.
    pub(crate) async fn peer_get(
        &self,
        from: crate::HostId,
        request: GetRequest,
    ) -> Result<Item, ServeError> {
        let request = PeerRequest::Get(request);
        item(self.driver().peer_call(core_host(from), request, 1).await).await
    }

    /// The one agent a reference names, across the fleet the store knows.
    /// A reference is any form amux prints: the agent's id with or without
    /// dashes, the eight-character short id `amux ls` shows (any prefix of
    /// the id at least that long), its name, or `name@host` where names on
    /// two hosts meet. Names are unique on a host, so `name@host` picks one.
    pub async fn resolve_agent(&self, reference: &str) -> Result<Agent, ServeError> {
        if let Ok(id) = uuid::Uuid::parse_str(reference) {
            return self
                .read_rows(move |store| store.agent_by_id(id.as_bytes()))
                .await?
                .map(|row| to_wire(&row))
                .ok_or_else(|| ServeError::NoAgentNamed(reference.to_owned()));
        }
        let (name, host) = match reference.rsplit_once('@') {
            Some((name, host)) => (name, Some(host)),
            None => (reference, None),
        };
        let host = match host {
            Some(host) => Some(
                self.resolve_host(host)
                    .map_err(|_| ServeError::NoAgentNamed(reference.to_owned()))?,
            ),
            None => None,
        };
        let short =
            (host.is_none() && is_short_id(reference)).then(|| reference.to_ascii_lowercase());
        let name = name.to_owned();
        let rows = self
            .read_rows(move |store| {
                let mut rows = store.agent_by_name(&name)?;
                if let Some(host) = host {
                    rows.retain(|row| row.agent.host == host.as_bytes());
                } else if let Some(prefix) = short.filter(|_| rows.is_empty()) {
                    rows = store
                        .agents()?
                        .into_iter()
                        .filter(|row| {
                            uuid::Uuid::from_slice(&row.agent.agent)
                                .is_ok_and(|id| id.simple().to_string().starts_with(&prefix))
                        })
                        .collect();
                }
                Ok(rows)
            })
            .await?;
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
                    .find(|host| host.host_id() == agent.host_id)
                    .map(|host| host.name())
                    .filter(|name| !name.is_empty());
                let alone_there = agents
                    .iter()
                    .filter(|other| other.host_id == agent.host_id)
                    .count()
                    == 1;
                let named_once =
                    |name: &str| hosts.iter().filter(|host| host.name() == name).count() == 1;
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

async fn fetched(answers: Answers) -> Result<FetchResponse, ServeError> {
    match unary(answers).await? {
        Response::Fetched(page) => Ok(page),
        _ => Err(ServeError::Stopped),
    }
}

async fn item(answers: Answers) -> Result<Item, ServeError> {
    match unary(answers).await? {
        Response::Item(item) => Ok(item),
        _ => Err(ServeError::Stopped),
    }
}

/// A call the core answers once.
async fn unary(mut answers: Answers) -> Result<Response, ServeError> {
    match answers.next().await {
        Some(Delivered::Answer(response)) => Ok(response),
        Some(Delivered::End(status)) => Err(status.into()),
        None => Err(ServeError::Stopped),
    }
}
