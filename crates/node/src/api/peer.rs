//! What a paired daemon asks this host for itself: PeerService.
//!
//! Everything a paired daemon needs to keep its copy of this host, and the
//! one thing it hands back, a child's report. Every call is answered for
//! this host's own entry and agents only, never for its copies of other
//! hosts': what this host knows of a third host is that host's to tell,
//! and a file held here as a copy is fetched from its own host. Nothing
//! here passes a call on, so a call goes one hop.

use std::sync::{Arc, Weak};

use tonic::service::RoutesBuilder;
use tonic::{Request, Response, Status};
use wire::peer_service_server::PeerService;
use wire::{
    DeliverReportRequest, DeliverReportResponse, ErrorCode, FollowAgentsRequest,
    FollowAgentsResponse, FollowSessionRequest, FollowSessionResponse, PeerServiceGetBlobRequest,
    PeerServiceGetBlobResponse, PeerServiceListItemsRequest, PeerServiceListItemsResponse,
};

use crate::api::caller::PairedDaemon;
use crate::grpc::{EventStream, agent_id, status, upgrade, wire_error};
use crate::runtime::ProfileRuntime;
use crate::serve::ServeError;

/// Mounts PeerService, which only links serve.
pub(in crate::api) fn mount(routes: &mut RoutesBuilder, runtime: &Weak<ProfileRuntime>) {
    routes.add_service(wire::peer_service_server(Peer {
        runtime: runtime.clone(),
    }));
}

/// The peer service, answering paired daemons.
#[derive(Clone)]
struct Peer {
    runtime: Weak<ProfileRuntime>,
}

impl Peer {
    fn runtime(&self) -> Result<Arc<ProfileRuntime>, Status> {
        upgrade(&self.runtime)
    }
}

fn invalid(why: &str) -> Status {
    status(wire_error(ErrorCode::InvalidArgument, why))
}

#[tonic::async_trait]
impl PeerService for Peer {
    type FollowAgentsStream = EventStream<FollowAgentsResponse>;
    type FollowSessionStream = EventStream<FollowSessionResponse>;

    /// This host's entry and its own agents, on opening and live.
    async fn follow_agents(
        &self,
        request: Request<FollowAgentsRequest>,
    ) -> Result<Response<Self::FollowAgentsStream>, Status> {
        let PairedDaemon(from) = PairedDaemon::of(&request)?;
        let subscription = self
            .runtime()?
            .peer_subscribe_inventory(from)
            .await
            .map_err(|error| status(error.to_wire()))?;
        let stream = futures_util::stream::unfold(subscription, |mut subscription| async move {
            let event = subscription.next().await?;
            let response = FollowAgentsResponse {
                event: Some(wire::InventoryEvent::clone(&event)),
            };
            Some((Ok(response), subscription))
        });
        Ok(Response::new(Box::pin(stream)))
    }

    /// One of this host's own conversations, from a tail for a copy that
    /// holds no cursor yet, or from the cursor the copy holds.
    async fn follow_session(
        &self,
        request: Request<FollowSessionRequest>,
    ) -> Result<Response<Self::FollowSessionStream>, Status> {
        let PairedDaemon(from) = PairedDaemon::of(&request)?;
        let request = request.into_inner();
        agent_id(&request.agent_id)?;
        let subscription = self
            .runtime()?
            .peer_subscribe_session(from, request)
            .await
            .map_err(|error| status(error.to_wire()))?;
        let stream = futures_util::stream::unfold(subscription, |mut subscription| async move {
            let event = subscription.next().await?;
            let response = FollowSessionResponse {
                event: Some(wire::SessionEvent::clone(&event)),
            };
            Some((Ok(response), subscription))
        });
        Ok(Response::new(Box::pin(stream)))
    }

    async fn list_items(
        &self,
        request: Request<PeerServiceListItemsRequest>,
    ) -> Result<Response<PeerServiceListItemsResponse>, Status> {
        let PairedDaemon(from) = PairedDaemon::of(&request)?;
        let request = request.into_inner();
        agent_id(&request.agent_id)?;
        let page = self
            .runtime()?
            .peer_list_items(
                from,
                wire::ListItemsRequest {
                    agent_id: request.agent_id,
                    before_order: request.before_order,
                    page_size: request.page_size,
                },
            )
            .await
            .map_err(|error| status(error.to_wire()))?;
        Ok(Response::new(PeerServiceListItemsResponse {
            items: page.items,
            exhausted: page.exhausted,
        }))
    }

    /// A file in one of this host's own agents' directories. A file held
    /// here only as a copy of another host's is not found: the asking
    /// daemon fetches it from that host itself.
    async fn get_blob(
        &self,
        request: Request<PeerServiceGetBlobRequest>,
    ) -> Result<Response<PeerServiceGetBlobResponse>, Status> {
        PairedDaemon::of(&request)?;
        let request = request.into_inner();
        agent_id(&request.agent_id)?;
        let runtime = self.runtime()?;
        match runtime.is_own(&request.agent_id).await {
            Ok(true) => {}
            Ok(false) => return Err(status(ServeError::NoAgent.to_wire())),
            Err(error) => return Err(status(error.to_wire())),
        }
        runtime
            .get_blob(wire::GetBlobRequest {
                agent_id: request.agent_id,
                hash: request.hash,
            })
            .await
            .map(|blob| Response::new(PeerServiceGetBlobResponse { bytes: blob.bytes }))
            .map_err(|error| status(error.to_wire()))
    }

    /// A child's report to its parent here. The child is the calling
    /// host's, whoever the link authenticated, so a daemon reports only for
    /// its own agents. The report is for one incarnation of its parent: one
    /// for any other is refused as stale, and none wakes an exited parent.
    async fn deliver_report(
        &self,
        request: Request<DeliverReportRequest>,
    ) -> Result<Response<DeliverReportResponse>, Status> {
        let PairedDaemon(from) = PairedDaemon::of(&request)?;
        let request = request.into_inner();
        if request.id.is_empty() {
            return Err(invalid(
                "a report carries its id, so one sent again is kept once",
            ));
        }
        agent_id(&request.child_agent_id)?;
        agent_id(&request.parent_agent_id)?;
        if request.report.is_none() {
            return Err(invalid("a report carries what the child did"));
        }
        // Its recipient is this host's agent, so it is never passed on.
        self.runtime()?
            .deliver_report(from, request)
            .await
            .map(|_| Response::new(DeliverReportResponse {}))
            .map_err(|error| status(error.to_wire()))
    }
}
