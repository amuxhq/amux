//! The client services on the link streams whose handshake presented a
//! trusted key: a paired daemon calls the same AgentService and
//! SessionService a client does, answered by [`PeerApi`] under a paired
//! host's rules.

use std::sync::{Arc, Weak};

use futures_util::stream;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tonic::{Request, Response, Status};
use wire::agent_service_server::AgentService;
use wire::session_service_server::SessionService;
use wire::{
    CreateAgentRequest, CreateAgentResponse, DeleteAgentRequest, DeleteAgentResponse, ErrorCode,
    FileAgentRequest, FileAgentResponse, GetBlobRequest, GetBlobResponse, GetDiffRequest,
    GetDiffResponse, GetItemRequest, GetItemResponse, InventoryEvent, ListItemsRequest,
    ListItemsResponse, ListRepositoriesRequest, ListRepositoriesResponse, PutBlobRequest,
    PutBlobResponse, RenameAgentRequest, RenameAgentResponse, ResolveAgentRequest,
    ResolveAgentResponse, ResumeAgentRequest, ResumeAgentResponse, SendInputRequest,
    SendInputResponse, SendMessageRequest, SendMessageResponse, SessionEvent,
    SubscribeInventoryRequest, SubscribeInventoryResponse, SubscribeSessionRequest,
    SubscribeSessionResponse,
};

use crate::grpc::{
    EventStream, agent_id, envelope_of, host_id, own_catalogue, status, upgrade, wire_error,
};
use crate::runtime::ProfileRuntime;
use crate::serve::ServeError;
use crate::transport::{BoxedGrpcAuth, BoxedGrpcConnectInfo, BoxedGrpcIo};

/// Serves the client services to paired daemons on `incoming`, until
/// shutdown.
pub(super) fn serve(
    runtime: Weak<ProfileRuntime>,
    incoming: mpsc::Receiver<BoxedGrpcIo>,
    shutdown: watch::Receiver<bool>,
) -> JoinHandle<()> {
    let incoming = stream::unfold(incoming, |mut incoming| async move {
        incoming
            .recv()
            .await
            .map(|io| (Ok::<_, std::io::Error>(io), incoming))
    });
    let service = PeerApi { runtime };
    tokio::spawn(async move {
        let served = tonic::transport::Server::builder()
            .add_service(wire::agent_service_server(service.clone()))
            .add_service(wire::session_service_server(service))
            .serve_with_incoming_shutdown(incoming, super::wait_for(shutdown))
            .await;
        if let Err(error) = served {
            tracing::warn!(%error, "the services for paired hosts stopped");
        }
    })
}

/// The client services as a paired host's calls reach them over a link.
/// Each is answered as a person's call, only for this host's own agents,
/// providers and repositories, and never passed on: forwarding goes one
/// hop, so no two hosts can pass a call back and forth. The calling daemon
/// stamps what it says for its own callers, a message's sender and a
/// create's parent, and this host takes them as given. Nothing a paired
/// host asks is counted here; its own daemon counted it.
#[derive(Clone)]
pub(crate) struct PeerApi {
    runtime: Weak<ProfileRuntime>,
}

impl PeerApi {
    fn runtime(&self) -> Result<Arc<ProfileRuntime>, Status> {
        upgrade(&self.runtime)
    }

    /// The paired host a call came from. Only a link stream whose handshake
    /// presented a trusted key is served here, so a call without one is a
    /// bug, and it is refused rather than served as nobody.
    fn caller<T>(request: &Request<T>) -> Result<crate::HostId, Status> {
        match request.extensions().get::<BoxedGrpcConnectInfo>() {
            Some(BoxedGrpcConnectInfo {
                auth: BoxedGrpcAuth::Trusted { peer },
            }) => Ok(*peer),
            _ => Err(Status::unauthenticated("the caller is not a trusted host")),
        }
    }

    /// This host's runtime, for a call on one of its own agents. A
    /// replica's rows are this host's copy of another host's, which that
    /// host serves itself, so they are not found here rather than passed
    /// on.
    async fn own<T>(
        &self,
        request: &Request<T>,
        agent_id: &[u8],
    ) -> Result<Arc<ProfileRuntime>, Status> {
        Self::caller(request)?;
        let runtime = self.runtime()?;
        match runtime.is_own(agent_id).await {
            Ok(true) => Ok(runtime),
            Ok(false) => Err(status(ServeError::NoAgent.to_wire())),
            Err(error) => Err(status(error.to_wire())),
        }
    }
}

/// Refuses a call naming another host than this one, in `refusal`'s words.
fn this_host(runtime: &ProfileRuntime, host: crate::HostId, refusal: &str) -> Result<(), Status> {
    if host == runtime.host() {
        return Ok(());
    }
    refused(refusal)
}

/// A call a paired host never makes, refused in `refusal`'s words.
fn refused<T>(refusal: &str) -> Result<T, Status> {
    Err(status(wire_error(ErrorCode::InvalidArgument, refusal)))
}

#[tonic::async_trait]
impl AgentService for PeerApi {
    type SubscribeInventoryStream = EventStream<SubscribeInventoryResponse>;

    /// This host's entry and this host's agents, never what this host
    /// knows of other hosts.
    async fn subscribe_inventory(
        &self,
        request: Request<SubscribeInventoryRequest>,
    ) -> Result<Response<Self::SubscribeInventoryStream>, Status> {
        let from = Self::caller(&request)?;
        let subscription = self
            .runtime()?
            .peer_subscribe_inventory(from)
            .await
            .map_err(|error| status(error.to_wire()))?;
        let stream = futures_util::stream::unfold(subscription, |mut subscription| async move {
            let event = subscription.next().await?;
            let response = SubscribeInventoryResponse {
                event: Some(InventoryEvent::clone(&event)),
            };
            Some((Ok(response), subscription))
        });
        Ok(Response::new(Box::pin(stream)))
    }

    async fn resolve_agent(
        &self,
        request: Request<ResolveAgentRequest>,
    ) -> Result<Response<ResolveAgentResponse>, Status> {
        Self::caller(&request)?;
        refused("a paired host resolves names itself")
    }

    async fn create_agent(
        &self,
        request: Request<CreateAgentRequest>,
    ) -> Result<Response<CreateAgentResponse>, Status> {
        Self::caller(&request)?;
        let request = request.into_inner();
        let runtime = self.runtime()?;
        let host = runtime
            .target_host(&request)
            .map_err(|error| status(error.to_wire()))?;
        this_host(
            &runtime,
            host,
            "a paired host starts agents only on this host",
        )?;
        // The parent, one of the calling host's agents or none for the
        // person, is named in the request by the calling daemon.
        let from_person = request.parent.is_none();
        let agent = runtime
            .spawn(request, None)
            .await
            .map_err(|error| status(error.to_wire()))?;
        // A person's agent started from a paired device is what finishes
        // the onboarding's first-agent step on this host.
        if from_person && let Err(error) = runtime.onboarding().agent_from_device(runtime.profile())
        {
            tracing::warn!(%error, "recording the first agent from a device");
        }
        Ok(Response::new(CreateAgentResponse { agent: Some(agent) }))
    }

    async fn rename_agent(
        &self,
        request: Request<RenameAgentRequest>,
    ) -> Result<Response<RenameAgentResponse>, Status> {
        let runtime = self.own(&request, &request.get_ref().agent_id).await?;
        let request = request.into_inner();
        runtime
            .rename(agent_id(&request.agent_id)?, &request.name)
            .await
            .map(|agent| Response::new(RenameAgentResponse { agent: Some(agent) }))
            .map_err(|error| status(error.to_wire()))
    }

    /// The calling daemon already held its caller to the filing rules: an
    /// agent settles only its own children, and only the person pins or
    /// un-settles.
    async fn file_agent(
        &self,
        request: Request<FileAgentRequest>,
    ) -> Result<Response<FileAgentResponse>, Status> {
        let runtime = self.own(&request, &request.get_ref().agent_id).await?;
        let request = request.into_inner();
        let Some(filing) = daemon_core::Filing::from_wire(request.filing()) else {
            return refused("a filing is active, pinned or settled");
        };
        runtime
            .file(agent_id(&request.agent_id)?, filing)
            .await
            .map(|agent| Response::new(FileAgentResponse { agent: Some(agent) }))
            .map_err(|error| status(error.to_wire()))
    }

    async fn resume_agent(
        &self,
        request: Request<ResumeAgentRequest>,
    ) -> Result<Response<ResumeAgentResponse>, Status> {
        let runtime = self.own(&request, &request.get_ref().agent_id).await?;
        let request = request.into_inner();
        let bootstrap = request
            .initial_prompt
            .map(crate::spec::prompt_bootstrap)
            .transpose()
            .map_err(|error| status(error.to_wire()))?;
        runtime
            .resume(agent_id(&request.agent_id)?, bootstrap)
            .await
            .map(|agent| Response::new(ResumeAgentResponse { agent: Some(agent) }))
            .map_err(|error| status(error.to_wire()))
    }

    async fn delete_agent(
        &self,
        request: Request<DeleteAgentRequest>,
    ) -> Result<Response<DeleteAgentResponse>, Status> {
        let runtime = self.own(&request, &request.get_ref().agent_id).await?;
        runtime
            .delete(agent_id(&request.get_ref().agent_id)?)
            .await
            .map(|()| Response::new(DeleteAgentResponse {}))
            .map_err(|error| status(error.to_wire()))
    }

    async fn send_message(
        &self,
        request: Request<SendMessageRequest>,
    ) -> Result<Response<SendMessageResponse>, Status> {
        Self::caller(&request)?;
        // The sending daemon stamped the sender from its own caller.
        self.runtime()?
            .send_peer_message(envelope_of(request.into_inner())?)
            .await
            .map(Response::new)
            .map_err(|error| status(error.to_wire()))
    }

    async fn put_blob(
        &self,
        request: Request<PutBlobRequest>,
    ) -> Result<Response<PutBlobResponse>, Status> {
        let runtime = self.own(&request, &request.get_ref().agent_id).await?;
        let request = request.into_inner();
        crate::blobs::within_limit(&request).map_err(|error| status(error.to_wire()))?;
        runtime
            .put_blob(request)
            .await
            .map(|blob| Response::new(PutBlobResponse { blob: Some(blob) }))
            .map_err(|error| status(error.to_wire()))
    }

    async fn get_blob(
        &self,
        request: Request<GetBlobRequest>,
    ) -> Result<Response<GetBlobResponse>, Status> {
        let runtime = self.own(&request, &request.get_ref().agent_id).await?;
        runtime
            .get_blob(request.into_inner())
            .await
            .map(Response::new)
            .map_err(|error| status(error.to_wire()))
    }

    async fn get_diff(
        &self,
        request: Request<GetDiffRequest>,
    ) -> Result<Response<GetDiffResponse>, Status> {
        let runtime = self.own(&request, &request.get_ref().agent_id).await?;
        runtime
            .get_diff(request.into_inner())
            .await
            .map(|diff| Response::new(GetDiffResponse { diff: Some(diff) }))
            .map_err(|error| status(error.to_wire()))
    }

    async fn get_catalogue(
        &self,
        request: Request<wire::GetCatalogueRequest>,
    ) -> Result<Response<wire::GetCatalogueResponse>, Status> {
        Self::caller(&request)?;
        let request = request.into_inner();
        let runtime = self.runtime()?;
        let host = host_id(&request.host_id)?;
        this_host(
            &runtime,
            host,
            "a paired host answers only for its own providers",
        )?;
        own_catalogue(&runtime, request.provider).await
    }

    async fn list_repositories(
        &self,
        request: Request<ListRepositoriesRequest>,
    ) -> Result<Response<ListRepositoriesResponse>, Status> {
        Self::caller(&request)?;
        let request = request.into_inner();
        let runtime = self.runtime()?;
        if let Some(host) = request.host_id.as_deref().map(host_id).transpose()? {
            this_host(
                &runtime,
                host,
                "a paired host lists only its own repositories",
            )?;
        }
        Ok(Response::new(
            runtime
                .list_repositories(request.query, request.limit)
                .await,
        ))
    }
}

#[tonic::async_trait]
impl SessionService for PeerApi {
    type SubscribeSessionStream = EventStream<SubscribeSessionResponse>;

    /// A tail, as a client asks, or what came after the revision the
    /// caller's replica holds, capped.
    async fn subscribe_session(
        &self,
        request: Request<SubscribeSessionRequest>,
    ) -> Result<Response<Self::SubscribeSessionStream>, Status> {
        let from = Self::caller(&request)?;
        let subscription = self
            .runtime()?
            .peer_subscribe_session(from, request.into_inner())
            .await
            .map_err(|error| status(error.to_wire()))?;
        let stream = futures_util::stream::unfold(subscription, |mut subscription| async move {
            let event = subscription.next().await?;
            let response = SubscribeSessionResponse {
                event: Some(SessionEvent::clone(&event)),
            };
            Some((Ok(response), subscription))
        });
        Ok(Response::new(Box::pin(stream)))
    }

    async fn list_items(
        &self,
        request: Request<ListItemsRequest>,
    ) -> Result<Response<ListItemsResponse>, Status> {
        let from = Self::caller(&request)?;
        self.runtime()?
            .peer_list_items(from, request.into_inner())
            .await
            .map(Response::new)
            .map_err(|error| status(error.to_wire()))
    }

    async fn get_item(
        &self,
        request: Request<GetItemRequest>,
    ) -> Result<Response<GetItemResponse>, Status> {
        let from = Self::caller(&request)?;
        self.runtime()?
            .peer_get_item(from, request.into_inner())
            .await
            .map(|item| Response::new(GetItemResponse { item: Some(item) }))
            .map_err(|error| status(error.to_wire()))
    }

    async fn send_input(
        &self,
        request: Request<SendInputRequest>,
    ) -> Result<Response<SendInputResponse>, Status> {
        let runtime = self.own(&request, &request.get_ref().agent_id).await?;
        runtime
            .send_input(request.get_ref())
            .await
            .map(Response::new)
            .map_err(|error| status(error.to_wire()))
    }

    async fn update_draft(
        &self,
        request: Request<wire::UpdateDraftRequest>,
    ) -> Result<Response<wire::UpdateDraftResponse>, Status> {
        Self::caller(&request)?;
        refused("a draft is kept by its client's own machine")
    }

    async fn get_draft(
        &self,
        request: Request<wire::GetDraftRequest>,
    ) -> Result<Response<wire::GetDraftResponse>, Status> {
        Self::caller(&request)?;
        refused("a draft is kept by its client's own machine")
    }
}
