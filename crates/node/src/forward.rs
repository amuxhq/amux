//! Calls on another host's agents. The host that owns an agent is the only
//! one that can act on it, so a runtime asked to act on a replica makes the
//! same call on the owner's daemon over the peer link, exactly as a phone
//! would, and answers with what the owner answered. Forwarding goes one hop:
//! a call that arrived from a peer is answered only for this host's own
//! agents, never passed on.

use std::future::Future;
use std::time::Duration;

use store::{AgentKey, AgentRow, StoreError};
use tonic::{Code, Response, Status};
use uuid::Uuid;
use wire::{
    Agent, AmbiguousHostName, DeleteAgentRequest, DeleteAgentResponse, ErrorCode, ErrorDetail,
    HostEntry, Trust,
};

use crate::HostId;
use crate::grpc::wire_error;
use crate::runtime::ProfileRuntime;

/// How long reaching a host may take: finding its link and opening a
/// channel on it. Past it the call never left, and the host counts as
/// unreachable.
const REACH_PATIENCE: Duration = Duration::from_secs(10);

/// How long a call that went out waits for its answer. A spawn waits for
/// the child's Hello on the far side, so this covers a start as well as a
/// round trip. Past it the call may or may not have arrived.
const ANSWER_PATIENCE: Duration = Duration::from_secs(30);

/// Where an agent lives, as this runtime's store knows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Owner {
    /// One of this host's own agents.
    Here,
    /// A replica: the agent lives on that host.
    Peer(HostId),
    /// No row names it.
    Unknown,
}

/// A forwarded call that did not get an answer from the owner, or got a
/// refusal. Hosts are named as a person knows them: by the name they were
/// paired under.
#[derive(Debug, thiserror::Error)]
pub enum ForwardError {
    /// The call never left this host: no link to the owner, the owner is not
    /// trusted, or no channel could be opened. Nothing reached it.
    #[error("{name} cannot be reached")]
    NotSent { host: HostId, name: String },
    /// The call never left: only the relay reaches the owner, and the
    /// account's plan lapsed to free, on which the relay carries no calls
    /// between hosts. Said at once, so no surface blames the network.
    #[error(
        "{name} can be reached only through the relay, and this account's subscription has \
         lapsed to the free plan, which carries no calls between hosts; renew it to reach {name}"
    )]
    Lapsed { host: HostId, name: String },
    /// The call went out and its answer did not come back: the link failed
    /// or the answer took too long. The owner may have acted on it.
    #[error("{name} stopped answering; the call may or may not have arrived")]
    Uncertain { host: HostId, name: String },
    #[error("{}", .0.message)]
    Remote(wire::Error),
}

impl ForwardError {
    /// The owner's own error passes through unchanged, so a caller sees
    /// the same answer it would have had from the owner directly.
    pub fn to_wire(&self) -> wire::Error {
        match self {
            Self::NotSent { .. } | Self::Lapsed { .. } | Self::Uncertain { .. } => {
                wire_error(self.code(), self.to_string())
            }
            Self::Remote(error) => error.clone(),
        }
    }

    pub fn code(&self) -> ErrorCode {
        match self {
            Self::NotSent { .. } => ErrorCode::Unreachable,
            Self::Lapsed { .. } => ErrorCode::PaymentRequired,
            Self::Uncertain { .. } => ErrorCode::Aborted,
            Self::Remote(error) => {
                ErrorCode::try_from(error.code).unwrap_or(ErrorCode::Unspecified)
            }
        }
    }
}

/// A spawn's host name that answers to no trusted host, or to several.
#[derive(Debug, thiserror::Error)]
pub enum HostNameError {
    #[error("no trusted host is named {name}; trusted hosts: {}", .known.join(", "))]
    NotFound { name: String, known: Vec<String> },
    #[error("{} trusted hosts answer to {}", .0.candidates.len(), .0.name)]
    Ambiguous(AmbiguousHostName),
}

impl HostNameError {
    pub fn to_wire(&self) -> wire::Error {
        match self {
            Self::NotFound { .. } => wire_error(ErrorCode::NotFound, self.to_string()),
            Self::Ambiguous(ambiguous) => {
                use prost::{Message as _, Name as _};
                wire::Error {
                    code: ErrorCode::FailedPrecondition as i32,
                    message: self.to_string(),
                    details: vec![ErrorDetail {
                        r#type: AmbiguousHostName::full_name(),
                        value: ambiguous.encode_to_vec(),
                    }],
                }
            }
        }
    }
}

/// The owner's error from a status: the wire error our daemons put in the
/// details, or one made from the code. A transport failure on a call that
/// went out leaves it uncertain: the owner may have acted on it before the
/// answer was lost.
fn from_status(uncertain: ForwardError, status: &Status) -> ForwardError {
    if let Some(error) = crate::net_error::from_status(status) {
        return ForwardError::Remote(error);
    }
    match status.code() {
        Code::Unavailable | Code::DeadlineExceeded | Code::Cancelled | Code::Unknown => uncertain,
        code => ForwardError::Remote(wire_error(
            match code {
                Code::NotFound => ErrorCode::NotFound,
                Code::InvalidArgument => ErrorCode::InvalidArgument,
                Code::PermissionDenied => ErrorCode::PermissionDenied,
                Code::FailedPrecondition => ErrorCode::FailedPrecondition,
                Code::AlreadyExists => ErrorCode::AlreadyExists,
                Code::Unimplemented => ErrorCode::Unimplemented,
                Code::Aborted => ErrorCode::Aborted,
                _ => ErrorCode::Internal,
            },
            status.message(),
        )),
    }
}

impl ProfileRuntime {
    /// Which host an agent id belongs to: this one's own row first, then
    /// any replica.
    pub async fn owner(&self, agent_id: &[u8]) -> Result<Owner, StoreError> {
        Ok(match self.row_by_id(agent_id).await? {
            None => Owner::Unknown,
            Some(row) if row.agent.host == self.host().as_bytes() => Owner::Here,
            Some(row) => match Uuid::from_slice(&row.agent.host) {
                Ok(host) => Owner::Peer(host),
                Err(_) => Owner::Unknown,
            },
        })
    }

    /// The row an agent id names, own or replica.
    pub(crate) async fn row_by_id(&self, agent_id: &[u8]) -> Result<Option<AgentRow>, StoreError> {
        self.store.lock().await.agent_by_id(agent_id)
    }

    /// Whether a call to `host` can leave now: why not, as a call to it
    /// would fail before it went out. Nothing is asked of the host.
    pub(crate) async fn reachable(&self, host: HostId) -> Result<(), ForwardError> {
        self.peer_client(host).await.map(drop)
    }

    /// A client for `host`, or why no call to it can leave.
    async fn peer_client(&self, host: HostId) -> Result<crate::PeerClient, ForwardError> {
        let not_sent = || ForwardError::NotSent {
            host,
            name: self.host_name(host),
        };
        let edge = self.edge().ok_or_else(not_sent)?;
        if !edge.is_trusted(host) {
            return Err(not_sent());
        }
        // On a free account the relay lists the account's hosts and opens
        // no tunnel between them, as the host set's presence says.
        let free = matches!(
            edge.observed(),
            crate::edge::Observed::Connected {
                tier: crate::Tier::Free,
                ..
            }
        );
        if free && edge.via(host).await == crate::routing::LiveRoute::Relay {
            return Err(ForwardError::Lapsed {
                host,
                name: self.host_name(host),
            });
        }
        match tokio::time::timeout(REACH_PATIENCE, edge.peer(host)).await {
            Ok(Ok(client)) => Ok(client),
            Ok(Err(_)) | Err(_) => {
                // A host listed as reachable that takes no call is not
                // reachable: its route goes down, so presence says so, and
                // comes back once a call gets through, which is what
                // releases sends held for the host. A host that revoked
                // this one already reads as away.
                if !edge.revoked(host) && edge.via(host).await != crate::routing::LiveRoute::Offline
                {
                    edge.calls_failed(host).await;
                }
                Err(not_sent())
            }
        }
    }

    /// Makes one call on `host`'s daemon. Reaching the host and waiting for
    /// its answer have separate limits, so a failure says whether the call
    /// went out.
    pub(crate) async fn on_peer<T, F, Fut>(&self, host: HostId, call: F) -> Result<T, ForwardError>
    where
        F: FnOnce(crate::PeerClient) -> Fut,
        Fut: Future<Output = Result<Response<T>, Status>>,
    {
        let uncertain = || ForwardError::Uncertain {
            host,
            name: self.host_name(host),
        };
        let client = self.peer_client(host).await?;
        match tokio::time::timeout(ANSWER_PATIENCE, call(client)).await {
            Ok(answer) => answer
                .map(Response::into_inner)
                .map_err(|status| from_status(uncertain(), &status)),
            Err(_) => Err(uncertain()),
        }
    }

    /// `host` as a person knows it: the name it was paired under.
    pub(crate) fn host_name(&self, host: HostId) -> String {
        self.edge()
            .and_then(|edge| {
                edge.trusted()
                    .into_iter()
                    .find(|(id, name, _)| *id == host && !name.is_empty())
            })
            .map_or_else(|| format!("host {host}"), |(_, name, _)| name)
    }

    /// The cascade's step for a child on another host: the delete is
    /// forwarded to the child's host, which deletes the child's own
    /// children in turn. A child already gone there counts as deleted; any
    /// other failure is the caller's to fail the delete with.
    pub(crate) async fn delete_remote_child(
        &self,
        child: AgentKey,
        wire: Agent,
        response: &mut DeleteAgentResponse,
    ) -> Result<(), ForwardError> {
        let Ok(host) = Uuid::from_slice(&child.host) else {
            return Ok(());
        };
        let request = DeleteAgentRequest {
            agent_id: child.agent.clone(),
        };
        match self
            .on_peer(host, |mut client| async move {
                client.delete_agent(request).await
            })
            .await
        {
            Ok(answer) => {
                response.removed_children.push(wire);
                response.removed_children.extend(answer.removed_children);
                Ok(())
            }
            // Already gone on its host.
            Err(error) if error.code() == ErrorCode::NotFound => {
                response.removed_children.push(wire);
                Ok(())
            }
            Err(error) => {
                tracing::warn!(%host, %error, "a cascaded delete could not reach the child's host");
                Err(error)
            }
        }
    }

    /// A host by the name a person uses for it, among this host and the
    /// hosts this profile trusts: an exact match, else one ignoring case.
    pub fn resolve_host(&self, name: &str) -> Result<HostId, HostNameError> {
        // Trust as it stands now, not as last published: a host paired a
        // moment ago is a host a spawn may name.
        let published = self.published_hosts();
        let mut hosts = vec![self.host_entry()];
        for (host, name, _) in self.edge().map(|edge| edge.trusted()).unwrap_or_default() {
            let id = host.as_bytes().to_vec();
            hosts.push(
                published
                    .iter()
                    .find(|entry| entry.host_id == id)
                    .cloned()
                    .unwrap_or(HostEntry {
                        host_id: id,
                        name,
                        trust: Trust::Trusted as i32,
                        ..HostEntry::default()
                    }),
            );
        }
        let mut matching: Vec<&HostEntry> = hosts.iter().filter(|host| host.name == name).collect();
        if matching.is_empty() {
            matching = hosts
                .iter()
                .filter(|host| host.name.eq_ignore_ascii_case(name))
                .collect();
        }
        match matching.as_slice() {
            [host] => Uuid::from_slice(&host.host_id).map_err(|_| HostNameError::NotFound {
                name: name.to_owned(),
                known: Vec::new(),
            }),
            [] => Err(HostNameError::NotFound {
                name: name.to_owned(),
                known: hosts.iter().map(|host| host.name.clone()).collect(),
            }),
            several => Err(HostNameError::Ambiguous(AmbiguousHostName {
                name: name.to_owned(),
                candidates: several.iter().map(|host| (*host).clone()).collect(),
            })),
        }
    }
}
