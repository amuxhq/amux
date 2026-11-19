//! What this host offers before any agent exists: HostService.
//!
//! Answered by this host about itself, the same to every caller: what its
//! providers offer and where it would start an agent are no one caller's.

use std::sync::Weak;

use tonic::service::RoutesBuilder;
use tonic::{Request, Response, Status};
use wire::amux::staging::host_service_server::HostService;
use wire::amux::staging::{
    GetCatalogueRequest, GetCatalogueResponse, ListRepositoriesRequest, ListRepositoriesResponse,
};

use crate::api::Entry;
use crate::grpc::{status, upgrade};
use crate::offers::{ASKED, CatalogueError};
use crate::runtime::ProfileRuntime;

/// Mounts HostService for `entry`: every entry asks the same.
pub(in crate::api) fn mount(
    routes: &mut RoutesBuilder,
    runtime: &Weak<ProfileRuntime>,
    _entry: Entry,
) {
    routes.add_service(wire::amux::staging::host_service_server(Offers {
        runtime: runtime.clone(),
    }));
}

#[derive(Clone)]
struct Offers {
    runtime: Weak<ProfileRuntime>,
}

#[tonic::async_trait]
impl HostService for Offers {
    async fn get_catalogue(
        &self,
        request: Request<GetCatalogueRequest>,
    ) -> Result<Response<GetCatalogueResponse>, Status> {
        let runtime = upgrade(&self.runtime)?;
        let asked = request.into_inner().provider;
        let provider = crate::Provider::try_from(asked)
            .ok()
            .filter(|provider| ASKED.contains(provider))
            .ok_or_else(|| status(CatalogueError::NoProvider(asked).to_wire()))?;
        let offered = runtime
            .host_catalogue(provider)
            .await
            .map_err(|error| status(error.to_wire()))?;
        Ok(Response::new(GetCatalogueResponse {
            hash: offered.blob.hash,
            bytes: offered.blob.bytes,
        }))
    }

    async fn list_repositories(
        &self,
        request: Request<ListRepositoriesRequest>,
    ) -> Result<Response<ListRepositoriesResponse>, Status> {
        let runtime = upgrade(&self.runtime)?;
        let request = request.into_inner();
        let listed = runtime
            .list_repositories(request.query, request.limit)
            .await;
        Ok(Response::new(ListRepositoriesResponse {
            recent: listed.recent,
            repositories: listed.repositories,
        }))
    }
}
