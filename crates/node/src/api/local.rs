//! What this node holds, for the clients attached to it: InventoryService,
//! SessionService and DraftService.
//!
//! Each is answered from this node's store, for its own agents and its
//! copies of other hosts' alike, so a client reads an agent the same way
//! wherever it lives. None is mounted on a link: a paired host learns
//! nothing here of third hosts, reads no draft and resolves names itself.
//! On an agent's tools socket, a read that names an agent is answered only
//! for the agent's own children, checked against the caller.

use std::sync::{Arc, Weak};

use store::{AgentRow, StoreError};
use tonic::service::RoutesBuilder;
use tonic::{Request, Response, Status};
use uuid::Uuid;
use wire::amux::staging::draft_service_server::DraftService;
use wire::amux::staging::inventory_service_server::InventoryService;
use wire::amux::staging::session_service_server::SessionService;
use wire::amux::staging::{
    self as staging, GetBlobRequest, GetBlobResponse, GetDraftRequest, GetDraftResponse,
    GetItemRequest, GetItemResponse, ListItemsRequest, ListItemsResponse, ResolveAgentRequest,
    ResolveAgentResponse, ResolveHostRequest, ResolveHostResponse, SubscribeInventoryRequest,
    SubscribeInventoryResponse, SubscribeSessionRequest, SubscribeSessionResponse,
    UpdateDraftRequest, UpdateDraftResponse,
};
use wire::{ErrorCode, HostEntry};

use crate::api::Entry;
use crate::api::caller::{At, Caller};
use crate::grpc::{EventStream, agent_id, status, upgrade, wire_error};
use crate::runtime::ProfileRuntime;
use crate::{HostId, telemetry};

/// Mounts the local services for `entry`, which is never a link.
pub(in crate::api) fn mount(
    routes: &mut RoutesBuilder,
    runtime: &Weak<ProfileRuntime>,
    entry: Entry,
) {
    let local = Local {
        runtime: runtime.clone(),
        entry,
    };
    routes
        .add_service(staging::inventory_service_server(local.clone()))
        .add_service(staging::session_service_server(local.clone()))
        .add_service(staging::draft_service_server(local));
}

/// The local services, as one entry serves them.
#[derive(Clone)]
struct Local {
    runtime: Weak<ProfileRuntime>,
    entry: Entry,
}

impl Local {
    fn runtime(&self) -> Result<Arc<ProfileRuntime>, Status> {
        upgrade(&self.runtime)
    }

    /// The runtime, for a read of what this node holds of `target`: its
    /// conversation, its files or its draft. A person reads any agent's.
    /// An agent reads only its own direct children's: it acts on the agents
    /// it started, and reaches any other only by messaging it. Whether
    /// `target` is its child is read from the child's row here, own or
    /// copy, which carries the same parent either way. `refusal` says what
    /// was refused.
    async fn reading<T>(
        &self,
        request: &Request<T>,
        target: &[u8],
        refusal: &str,
    ) -> Result<Arc<ProfileRuntime>, Status> {
        let caller = Caller::of(request)?;
        agent_id(target)?;
        let runtime = self.runtime()?;
        let Caller::Agent { agent, .. } = caller else {
            return Ok(runtime);
        };
        let row = runtime
            .row_by_id(target)
            .await
            .map_err(|error| status(wire_error(ErrorCode::Internal, error.to_string())))?;
        let own_child = row.and_then(|row| row.parent).is_some_and(|parent| {
            parent.host == agent.host.as_bytes() && parent.agent == agent.agent.as_bytes()
        });
        if own_child {
            return Ok(runtime);
        }
        Err(status(wire_error(ErrorCode::PermissionDenied, refusal)))
    }
}

const CHATS: &str = "an agent reads only its own children's chats";
const FILES: &str = "an agent reads only its own children's files";
const DRAFTS: &str = "an agent keeps drafts only for its own children";

#[tonic::async_trait]
impl InventoryService for Local {
    type SubscribeInventoryStream = EventStream<SubscribeInventoryResponse>;

    /// Every host and agent row this node holds. A person listing them on
    /// the profile's socket is a client opening, counted here on the node
    /// it opened on and nowhere else: the terminal, the Mac app and the
    /// browser open there, the phone's app counts its own coming to the
    /// front, and an agent listing them is no client at all.
    async fn subscribe_inventory(
        &self,
        request: Request<SubscribeInventoryRequest>,
    ) -> Result<Response<Self::SubscribeInventoryStream>, Status> {
        let caller = Caller::of(&request)?;
        let runtime = self.runtime()?;
        let subscription = runtime
            .subscribe_inventory()
            .await
            .map_err(|error| status(error.to_wire()))?;
        if let (
            Entry::Profile,
            Caller::Person {
                at: At::Here,
                surface,
            },
        ) = (self.entry, caller)
        {
            runtime.analytics().record_at_most_every(
                telemetry::CLIENT_OPENED_EVERY,
                analytics::Event::ClientOpened { client: surface },
            );
        }
        let stream = futures_util::stream::unfold(subscription, |mut subscription| async move {
            let event = subscription.next().await?;
            let response = SubscribeInventoryResponse {
                event: Some(wire::InventoryEvent::clone(&event)),
            };
            Some((Ok(response), subscription))
        });
        Ok(Response::new(Box::pin(stream)))
    }

    async fn resolve_agent(
        &self,
        request: Request<ResolveAgentRequest>,
    ) -> Result<Response<ResolveAgentResponse>, Status> {
        Caller::of(&request)?;
        self.runtime()?
            .resolve_agent(&request.into_inner().name)
            .await
            .map(|agent| Response::new(ResolveAgentResponse { agent: Some(agent) }))
            .map_err(|error| status(error.to_wire()))
    }

    async fn resolve_host(
        &self,
        request: Request<ResolveHostRequest>,
    ) -> Result<Response<ResolveHostResponse>, Status> {
        Caller::of(&request)?;
        self.runtime()?
            .resolve_host_entry(&request.into_inner().name)
            .map(|host| Response::new(ResolveHostResponse { host: Some(host) }))
            .map_err(|error| status(error.to_wire()))
    }
}

#[tonic::async_trait]
impl SessionService for Local {
    type SubscribeSessionStream = EventStream<SubscribeSessionResponse>;

    async fn subscribe_session(
        &self,
        request: Request<SubscribeSessionRequest>,
    ) -> Result<Response<Self::SubscribeSessionStream>, Status> {
        let runtime = self
            .reading(&request, &request.get_ref().agent_id, CHATS)
            .await?;
        let request = request.into_inner();
        let subscription = runtime
            .subscribe_session(&request.agent_id, request.tail)
            .await
            .map_err(|error| status(error.to_wire()))?;
        let stream = futures_util::stream::unfold(subscription, |mut subscription| async move {
            let event = subscription.next().await?;
            let response = SubscribeSessionResponse {
                event: Some(wire::SessionEvent::clone(&event)),
            };
            Some((Ok(response), subscription))
        });
        Ok(Response::new(Box::pin(stream)))
    }

    async fn list_items(
        &self,
        request: Request<ListItemsRequest>,
    ) -> Result<Response<ListItemsResponse>, Status> {
        let runtime = self
            .reading(&request, &request.get_ref().agent_id, CHATS)
            .await?;
        let request = request.into_inner();
        let page = runtime
            .list_items(&wire::ListItemsRequest {
                agent_id: request.agent_id,
                before_order: request.before_order,
                page_size: request.page_size,
            })
            .await
            .map_err(|error| status(error.to_wire()))?;
        Ok(Response::new(ListItemsResponse {
            items: page.items,
            exhausted: page.exhausted,
        }))
    }

    async fn get_item(
        &self,
        request: Request<GetItemRequest>,
    ) -> Result<Response<GetItemResponse>, Status> {
        let runtime = self
            .reading(&request, &request.get_ref().agent_id, CHATS)
            .await?;
        let request = request.into_inner();
        runtime
            .get_item(&wire::GetItemRequest {
                agent_id: request.agent_id,
                key: request.key,
            })
            .await
            .map(|item| Response::new(GetItemResponse { item: Some(item) }))
            .map_err(|error| status(error.to_wire()))
    }

    async fn get_blob(
        &self,
        request: Request<GetBlobRequest>,
    ) -> Result<Response<GetBlobResponse>, Status> {
        let runtime = self
            .reading(&request, &request.get_ref().agent_id, FILES)
            .await?;
        let request = request.into_inner();
        runtime
            .read_through_blob(&request.agent_id, &request.hash)
            .await
            .map(|bytes| Response::new(GetBlobResponse { bytes }))
            .map_err(|error| status(error.to_wire()))
    }
}

#[tonic::async_trait]
impl DraftService for Local {
    async fn update_draft(
        &self,
        request: Request<UpdateDraftRequest>,
    ) -> Result<Response<UpdateDraftResponse>, Status> {
        let runtime = self
            .reading(&request, &request.get_ref().agent_id, DRAFTS)
            .await?;
        let request = request.into_inner();
        let draft = request.draft.unwrap_or_default();
        runtime.update_draft(&request.agent_id, &draft).await;
        Ok(Response::new(UpdateDraftResponse { draft: Some(draft) }))
    }

    async fn get_draft(
        &self,
        request: Request<GetDraftRequest>,
    ) -> Result<Response<GetDraftResponse>, Status> {
        let runtime = self
            .reading(&request, &request.get_ref().agent_id, DRAFTS)
            .await?;
        Ok(Response::new(GetDraftResponse {
            draft: Some(runtime.draft(&request.get_ref().agent_id).await),
        }))
    }
}

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

/// A host name that answers to no trusted host, or to several.
#[derive(Debug, thiserror::Error)]
pub enum HostNameError {
    #[error("no trusted host is named {name}; trusted hosts: {}", .known.join(", "))]
    NotFound { name: String, known: Vec<String> },
    #[error("{count} trusted hosts answer to {name}")]
    Ambiguous { name: String, count: usize },
}

impl HostNameError {
    pub fn to_wire(&self) -> wire::Error {
        match self {
            Self::NotFound { .. } => wire_error(ErrorCode::NotFound, self.to_string()),
            Self::Ambiguous { .. } => {
                wire::Error::new(ErrorCode::FailedPrecondition, self.to_string())
            }
        }
    }
}

/// Reading this node's copy of the fleet: where an agent lives, the row an
/// id names, and the host a name names.
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
        let agent_id = agent_id.to_vec();
        self.read_rows(move |store| store.agent_by_id(&agent_id))
            .await
    }

    /// A host by the name a person uses for it, among this host and the
    /// hosts this profile trusts: an exact match, else one ignoring case.
    pub fn resolve_host(&self, name: &str) -> Result<HostId, HostNameError> {
        let host = self.resolve_host_entry(name)?;
        Uuid::from_slice(host.host_id()).map_err(|_| HostNameError::NotFound {
            name: name.to_owned(),
            known: Vec::new(),
        })
    }

    /// The entry of the host [`ProfileRuntime::resolve_host`] finds, as
    /// this host holds it.
    pub(crate) fn resolve_host_entry(&self, name: &str) -> Result<HostEntry, HostNameError> {
        // Trust as it stands now, not as last published: a host paired a
        // moment ago is a host a spawn may name.
        let published = self.published_hosts();
        let mut hosts = vec![self.host_entry()];
        for (host, name, _) in self.edge().map(|edge| edge.trusted()).unwrap_or_default() {
            let id = host.as_bytes().to_vec();
            hosts.push(
                published
                    .iter()
                    .find(|entry| entry.host_id() == id)
                    .cloned()
                    .unwrap_or_else(|| {
                        HostEntry::trusted_host(
                            id,
                            name,
                            wire::trusted::Reach::Offline(wire::Offline::default()),
                        )
                    }),
            );
        }
        let mut matching: Vec<&HostEntry> =
            hosts.iter().filter(|host| host.name() == name).collect();
        if matching.is_empty() {
            matching = hosts
                .iter()
                .filter(|host| host.name().eq_ignore_ascii_case(name))
                .collect();
        }
        match matching.as_slice() {
            [host] => Ok(HostEntry::clone(host)),
            [] => Err(HostNameError::NotFound {
                name: name.to_owned(),
                known: hosts.iter().map(|host| host.name().to_owned()).collect(),
            }),
            several => Err(HostNameError::Ambiguous {
                name: name.to_owned(),
                count: several.len(),
            }),
        }
    }
}
