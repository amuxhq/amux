//! PeerService: the calls a trusted host makes over a link, answered by the
//! same runtime as the client service's; the caller is the host whose
//! pinned key the stream's handshake presented. It answers only for this
//! host's own agents and passes nothing on: the inventory it streams is
//! this host's entry and agents, and any call that names an agent this
//! host only replicates is not found here.

use std::sync::Weak;

use futures_util::stream;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tonic::{Request, Response, Status};
use wire::client_service_server::ClientService;
use wire::peer_service_server::PeerService;
use wire::{
    Agent, BlobRef, CreateAgentRequest, DeleteAgentRequest, DeleteAgentResponse, DiffRequest,
    DumpPart, DumpRequest, Empty, Envelope, FetchRequest, FetchResponse, GetBlobRequest,
    GetBlobResponse, GetRequest, InventoryEvent, Item, ListRepositoriesRequest,
    ListRepositoriesResponse, PeerSubscribeRequest, PutBlobRequest, RenameAgentRequest,
    ResumeAgentRequest, SendInputRequest, SendInputResponse, SendMessageResponse, SessionEvent,
    StopAgentRequest, peer_subscribe_request,
};

use crate::HostId;
use crate::grpc::{ClientApi, status};
use crate::runtime::ProfileRuntime;
use crate::serve::ServeError;
use crate::transport::{BoxedGrpcAuth, BoxedGrpcConnectInfo, BoxedGrpcIo, tonic_server_builder};

/// Serves PeerService on the streams whose handshake presented a trusted
/// key, until shutdown.
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
    let service = PeerApi {
        client: ClientApi::for_peer(runtime.clone()),
        runtime,
    };
    tokio::spawn(async move {
        let served = tonic_server_builder()
            .add_service(wire::peer_service_server(service))
            .serve_with_incoming_shutdown(incoming, super::wait_for(shutdown))
            .await;
        if let Err(error) = served {
            tracing::warn!(%error, "the peer service stopped");
        }
    })
}

#[derive(Clone)]
struct PeerApi {
    runtime: Weak<ProfileRuntime>,
    client: ClientApi,
}

impl PeerApi {
    fn runtime(&self) -> Result<std::sync::Arc<ProfileRuntime>, Status> {
        self.runtime
            .upgrade()
            .ok_or_else(|| Status::unavailable("the profile is no longer running"))
    }

    /// Refuses a call on an agent this host does not run. A replica's rows
    /// are this host's copy of another host's, which that host serves
    /// itself, so they are not found here rather than passed on.
    async fn own(&self, agent_id: &[u8]) -> Result<(), Status> {
        match self.runtime()?.is_own(agent_id).await {
            Ok(true) => Ok(()),
            Ok(false) => Err(status(ServeError::NoAgent.to_wire())),
            Err(error) => Err(status(error.to_wire())),
        }
    }
}

/// The host a call came from. Only a stream whose handshake presented a
/// trusted key reaches this service, so a call without one is a bug, and it
/// is refused rather than served as nobody.
fn caller<T>(request: &Request<T>) -> Result<HostId, Status> {
    match request.extensions().get::<BoxedGrpcConnectInfo>() {
        Some(BoxedGrpcConnectInfo {
            auth: BoxedGrpcAuth::Trusted { peer },
        }) => Ok(*peer),
        _ => Err(Status::unauthenticated("the caller is not a trusted host")),
    }
}

#[tonic::async_trait]
impl PeerService for PeerApi {
    type SubscribeInventoryStream = <ClientApi as ClientService>::SubscribeInventoryStream;
    type SubscribeStream = <ClientApi as ClientService>::SubscribeStream;

    async fn subscribe_inventory(
        &self,
        request: Request<Empty>,
    ) -> Result<Response<Self::SubscribeInventoryStream>, Status> {
        caller(&request)?;
        let subscription = self
            .runtime()?
            .subscribe_own_inventory()
            .await
            .map_err(|error| status(error.to_wire()))?;
        let stream = futures_util::stream::unfold(subscription, |mut subscription| async move {
            let event = subscription.next().await?;
            Some((Ok(InventoryEvent::clone(&event)), subscription))
        });
        Ok(Response::new(Box::pin(stream)))
    }

    /// A peer asks for a tail, like a client, or for what came after the
    /// revision its replica holds, capped.
    async fn subscribe(
        &self,
        request: Request<PeerSubscribeRequest>,
    ) -> Result<Response<Self::SubscribeStream>, Status> {
        caller(&request)?;
        let request = request.into_inner();
        self.own(&request.agent_id).await?;
        let asked = std::time::Instant::now();
        let runtime = self.runtime()?;
        let subscription = match request.from {
            Some(peer_subscribe_request::From::After(after)) => {
                runtime
                    .subscribe_after(
                        &request.agent_id,
                        after.revision,
                        after.cap,
                        after.generation,
                    )
                    .await
            }
            Some(peer_subscribe_request::From::Tail(tail)) => {
                runtime.subscribe(&request.agent_id, tail).await
            }
            None => runtime.subscribe(&request.agent_id, 0).await,
        }
        .map_err(|error| status(error.to_wire()))?;
        drop(runtime);
        tracing::debug!(
            agent = %uuid::Uuid::from_slice(&request.agent_id).map(|id| id.to_string()).unwrap_or_default(),
            opened_ms = asked.elapsed().as_millis(),
            "opened a session subscription"
        );
        let stream = futures_util::stream::unfold(subscription, |mut subscription| async move {
            let event = subscription.next().await?;
            Some((Ok(SessionEvent::clone(&event)), subscription))
        });
        Ok(Response::new(Box::pin(stream)))
    }

    async fn fetch(
        &self,
        request: Request<FetchRequest>,
    ) -> Result<Response<FetchResponse>, Status> {
        caller(&request)?;
        self.own(&request.get_ref().agent_id).await?;
        self.client.fetch(request).await
    }

    async fn get(&self, request: Request<GetRequest>) -> Result<Response<Item>, Status> {
        caller(&request)?;
        self.own(&request.get_ref().agent_id).await?;
        self.client.get(request).await
    }

    async fn send_input(
        &self,
        request: Request<SendInputRequest>,
    ) -> Result<Response<SendInputResponse>, Status> {
        caller(&request)?;
        self.own(&request.get_ref().agent_id).await?;
        self.client.send_input(request).await
    }

    /// A host creates agents here for its own agents, or for the person:
    /// the parent it names, if any, must be one of its own.
    async fn create_agent(
        &self,
        request: Request<CreateAgentRequest>,
    ) -> Result<Response<Agent>, Status> {
        let host = caller(&request)?;
        let foreign_parent = request
            .get_ref()
            .parent
            .as_ref()
            .is_some_and(|parent| parent.host_id != host.as_bytes());
        if foreign_parent {
            return Err(status(crate::grpc::wire_error(
                wire::ErrorCode::PermissionDenied,
                "a host creates children only for its own agents",
            )));
        }
        self.client.create_agent(request).await
    }

    async fn rename_agent(
        &self,
        request: Request<RenameAgentRequest>,
    ) -> Result<Response<Agent>, Status> {
        caller(&request)?;
        self.own(&request.get_ref().agent_id).await?;
        self.client.rename_agent(request).await
    }

    async fn stop_agent(
        &self,
        request: Request<StopAgentRequest>,
    ) -> Result<Response<Empty>, Status> {
        caller(&request)?;
        self.own(&request.get_ref().agent_id).await?;
        self.client.stop_agent(request).await
    }

    async fn resume_agent(
        &self,
        request: Request<ResumeAgentRequest>,
    ) -> Result<Response<Agent>, Status> {
        caller(&request)?;
        self.own(&request.get_ref().agent_id).await?;
        self.client.resume_agent(request).await
    }

    async fn delete_agent(
        &self,
        request: Request<DeleteAgentRequest>,
    ) -> Result<Response<DeleteAgentResponse>, Status> {
        caller(&request)?;
        self.own(&request.get_ref().agent_id).await?;
        self.client.delete_agent(request).await
    }

    async fn send_message(
        &self,
        request: Request<Envelope>,
    ) -> Result<Response<SendMessageResponse>, Status> {
        let host = caller(&request)?;
        self.runtime()?
            .send_peer_message(request.into_inner(), host)
            .await
            .map(Response::new)
            .map_err(|error| status(error.to_wire()))
    }

    async fn put_blob(
        &self,
        request: Request<PutBlobRequest>,
    ) -> Result<Response<BlobRef>, Status> {
        caller(&request)?;
        self.own(&request.get_ref().agent_id).await?;
        self.client.put_blob(request).await
    }

    async fn get_blob(
        &self,
        request: Request<GetBlobRequest>,
    ) -> Result<Response<GetBlobResponse>, Status> {
        caller(&request)?;
        self.own(&request.get_ref().agent_id).await?;
        self.client.get_blob(request).await
    }

    async fn diff(&self, request: Request<DiffRequest>) -> Result<Response<wire::Diff>, Status> {
        caller(&request)?;
        self.own(&request.get_ref().agent_id).await?;
        self.client.diff(request).await
    }

    async fn get_catalogue(
        &self,
        request: Request<wire::GetCatalogueRequest>,
    ) -> Result<Response<wire::CatalogueBlob>, Status> {
        caller(&request)?;
        self.client.get_catalogue(request).await
    }

    async fn list_repositories(
        &self,
        request: Request<ListRepositoriesRequest>,
    ) -> Result<Response<ListRepositoriesResponse>, Status> {
        caller(&request)?;
        self.client.list_repositories(request).await
    }

    /// A paired host's dump asks for this host's side of the agents it
    /// runs: the bundle travels in the answer.
    async fn dump(&self, request: Request<DumpRequest>) -> Result<Response<DumpPart>, Status> {
        caller(&request)?;
        self.runtime()?
            .dump_for_peer(request.into_inner())
            .await
            .map(Response::new)
            .map_err(|error| status(error.to_wire()))
    }
}
