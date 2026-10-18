//! Calls on another host's agents. The host that owns an agent is the only
//! one that can act on it, so a runtime asked to act on a replica makes the
//! same call on the owner's daemon over the peer link, exactly as a phone
//! would, and answers with what the owner answered. Forwarding goes one hop:
//! a call that arrived from a peer is answered only for this host's own
//! agents, never passed on.
//!
//! A forwarded call goes through the profile's core, which holds it in its
//! pending set until the owner's reply comes back; the driver makes the
//! call itself, here.

use std::time::Duration;

use daemon_core::payload::{
    Answer, CallError, PeerFailure, PeerReply, PeerRequest, Response as CoreResponse,
    Status as CoreStatus,
};
use store::{AgentRow, StoreError};
use tonic::{Code, Response, Status};
use uuid::Uuid;
use wire::{AmbiguousHostName, ErrorCode, ErrorDetail, HostEntry, Trust};

use crate::HostId;
use crate::driver::Delivered;
use crate::grpc::wire_error;
use crate::routing::core_host;
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
    /// The call never left: the relay refused it because the account's
    /// plan lapsed to free, which carries no calls between hosts. Said as
    /// such, so no surface blames the network.
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

impl ForwardError {
    fn of(failure: PeerFailure, host: HostId, name: String) -> Self {
        match failure {
            PeerFailure::NotSent => Self::NotSent { host, name },
            PeerFailure::Lapsed => Self::Lapsed { host, name },
            PeerFailure::Uncertain => Self::Uncertain { host, name },
            PeerFailure::Refused(error) => Self::Remote(error),
        }
    }

    fn failure(self) -> PeerFailure {
        match self {
            Self::NotSent { .. } => PeerFailure::NotSent,
            Self::Lapsed { .. } => PeerFailure::Lapsed,
            Self::Uncertain { .. } => PeerFailure::Uncertain,
            Self::Remote(error) => PeerFailure::Refused(error),
        }
    }
}

/// A PeerService request that can be forwarded, and the reply it gets.
pub(crate) trait PeerRpc {
    type Reply;
    fn request(self) -> PeerRequest;
    fn reply(reply: PeerReply) -> Option<Self::Reply>;
}

macro_rules! peer_rpc {
    ($($request:ty => $variant:ident, $reply:ty => $answered:pat => $value:expr;)*) => {
        $(
            impl PeerRpc for $request {
                type Reply = $reply;
                fn request(self) -> PeerRequest {
                    PeerRequest::$variant(self)
                }
                fn reply(reply: PeerReply) -> Option<$reply> {
                    match reply {
                        $answered => Some($value),
                        _ => None,
                    }
                }
            }
        )*
    };
}

peer_rpc! {
    wire::SendInputRequest => SendInput, wire::SendInputResponse
        => PeerReply::SendInput(reply) => reply;
    wire::CreateAgentRequest => CreateAgent, wire::Agent => PeerReply::Agent(agent) => agent;
    wire::RenameAgentRequest => RenameAgent, wire::Agent => PeerReply::Agent(agent) => agent;
    wire::StopAgentRequest => StopAgent, wire::Empty => PeerReply::Done => wire::Empty {};
    wire::ResumeAgentRequest => ResumeAgent, wire::Agent => PeerReply::Agent(agent) => agent;
    wire::DeleteAgentRequest => DeleteAgent, wire::Empty => PeerReply::Done => wire::Empty {};
    wire::Envelope => SendMessage, wire::SendMessageResponse
        => PeerReply::SendMessage(reply) => reply;
    wire::PutBlobRequest => PutBlob, wire::BlobRef => PeerReply::Blob(blob) => blob;
    wire::GetBlobRequest => GetBlob, wire::GetBlobResponse => PeerReply::GetBlob(blob) => blob;
    wire::DiffRequest => Diff, wire::Diff => PeerReply::Diff(diff) => diff;
    wire::GetCatalogueRequest => GetCatalogue, wire::CatalogueBlob
        => PeerReply::Catalogue(catalogue) => catalogue;
    wire::ListRepositoriesRequest => ListRepositories, wire::ListRepositoriesResponse
        => PeerReply::Repositories(repositories) => repositories;
    wire::DumpRequest => Dump, wire::DumpPart => PeerReply::Dump(part) => part;
    wire::FetchRequest => Fetch, wire::FetchResponse => PeerReply::Fetched(page) => page;
    wire::GetRequest => Get, wire::Item => PeerReply::Item(item) => item;
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
        match tokio::time::timeout(REACH_PATIENCE, edge.peer(host)).await {
            Ok(Ok(client)) => Ok(client),
            // On a free account the relay lists the account's hosts and
            // opens no tunnel between them; the host already reads away.
            Ok(Err(crate::link::ChannelError::Refused(
                wire::pb::StreamRefusal::PaymentRequired,
            ))) => Err(ForwardError::Lapsed {
                host,
                name: self.host_name(host),
            }),
            Ok(Err(_)) | Err(_) => {
                // A host listed as reachable that takes no call is not
                // reachable: it reads offline until a channel to it opens
                // again, which is what releases sends held for the host. A
                // host that revoked this one already reads offline.
                if !edge.revoked(host) && edge.via(host).await != crate::routing::LiveRoute::Offline
                {
                    edge.calls_failed(host).await;
                }
                Err(not_sent())
            }
        }
    }

    /// Makes one call on `host`'s daemon through the profile's core, and
    /// answers with the owner's reply.
    pub(crate) async fn on_peer<R: PeerRpc>(
        &self,
        host: HostId,
        request: R,
    ) -> Result<R::Reply, ForwardError> {
        let mut answers = self
            .driver()
            .forward(core_host(host), request.request())
            .await;
        let mut reply = None;
        loop {
            match answers.next().await {
                Some(Delivered::Answer(CoreResponse::Peer(answered))) => reply = Some(*answered),
                Some(Delivered::Answer(_)) => {}
                Some(Delivered::End(CoreStatus::Ok)) => break,
                Some(Delivered::End(CoreStatus::Failed(CallError::Peer(failure)))) => {
                    return Err(ForwardError::of(failure, host, self.host_name(host)));
                }
                // The profile stopped, or the core could not make the call:
                // either way it never left.
                Some(Delivered::End(CoreStatus::Failed(_))) | None => break,
            }
        }
        reply
            .and_then(R::reply)
            .ok_or_else(|| ForwardError::NotSent {
                host,
                name: self.host_name(host),
            })
    }

    /// Makes a call the profile's core asked for of `host`. Reaching the
    /// host and waiting for its answer have separate limits, so a failure
    /// says whether the call went out.
    pub(crate) async fn call_peer(&self, host: HostId, request: PeerRequest) -> Answer {
        let client = match self.peer_client(host).await {
            Ok(client) => client,
            Err(error) => return Answer::Failed(error.failure()),
        };
        match tokio::time::timeout(ANSWER_PATIENCE, dispatch(client, request)).await {
            Ok(Ok(reply)) => Answer::Reply(Box::new(reply)),
            Ok(Err(status)) => {
                let uncertain = ForwardError::Uncertain {
                    host,
                    name: String::new(),
                };
                Answer::Failed(from_status(uncertain, &status).failure())
            }
            Err(_) => Answer::Failed(PeerFailure::Uncertain),
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

/// Makes one PeerService call and reads its whole reply.
async fn dispatch(
    mut client: crate::PeerClient,
    request: PeerRequest,
) -> Result<PeerReply, Status> {
    fn unary<T>(
        answer: Result<Response<T>, Status>,
        reply: fn(T) -> PeerReply,
    ) -> Result<PeerReply, Status> {
        answer.map(|answer| reply(answer.into_inner()))
    }
    match request {
        PeerRequest::Fetch(request) => unary(client.fetch(request).await, PeerReply::Fetched),
        PeerRequest::Get(request) => unary(client.get(request).await, PeerReply::Item),
        PeerRequest::SendInput(request) => {
            unary(client.send_input(request).await, PeerReply::SendInput)
        }
        PeerRequest::CreateAgent(request) => {
            unary(client.create_agent(request).await, PeerReply::Agent)
        }
        PeerRequest::RenameAgent(request) => {
            unary(client.rename_agent(request).await, PeerReply::Agent)
        }
        PeerRequest::StopAgent(request) => {
            unary(client.stop_agent(request).await, |_| PeerReply::Done)
        }
        PeerRequest::ResumeAgent(request) => {
            unary(client.resume_agent(request).await, PeerReply::Agent)
        }
        PeerRequest::DeleteAgent(request) => {
            unary(client.delete_agent(request).await, |_| PeerReply::Done)
        }
        PeerRequest::SendMessage(envelope) => {
            unary(client.send_message(envelope).await, PeerReply::SendMessage)
        }
        PeerRequest::PutBlob(request) => unary(client.put_blob(request).await, PeerReply::Blob),
        PeerRequest::GetBlob(request) => unary(client.get_blob(request).await, PeerReply::GetBlob),
        PeerRequest::Diff(request) => unary(client.diff(request).await, PeerReply::Diff),
        PeerRequest::GetCatalogue(request) => {
            unary(client.get_catalogue(request).await, PeerReply::Catalogue)
        }
        PeerRequest::ListRepositories(request) => unary(
            client.list_repositories(request).await,
            PeerReply::Repositories,
        ),
        PeerRequest::Dump(request) => unary(client.dump(request).await, PeerReply::Dump),
        // Streams are opened by the replica's source, not forwarded.
        PeerRequest::Subscribe(_) | PeerRequest::SubscribeInventory { .. } => Err(
            Status::unimplemented("a stream is not made as a forwarded call"),
        ),
    }
}
