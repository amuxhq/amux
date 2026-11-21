//! What this host offers before any agent exists: HostService.
//!
//! Answered by this host about itself. What its providers offer is the
//! same to every caller. Its folders are not asked here: they ride on its
//! own entry, which every node holds a copy of. Pinning one changes its
//! `repositories` setting, which is its person's, as the rest of its
//! settings are: an agent, here or on a paired host, is refused.

use std::sync::Weak;

use tonic::service::RoutesBuilder;
use tonic::{Request, Response, Status};
use wire::host_service_server::HostService;
use wire::{
    ErrorCode, GetCatalogueRequest, GetCatalogueResponse, PinFolderRequest, PinFolderResponse,
};

use crate::api::Entry;
use crate::api::caller::{Caller, for_a_caller};
use crate::grpc::{settings_status, status, upgrade, wire_error};
use crate::runtime::ProfileRuntime;

/// Mounts HostService for `entry`: every entry asks the same.
pub(in crate::api) fn mount(
    routes: &mut RoutesBuilder,
    runtime: &Weak<ProfileRuntime>,
    _entry: Entry,
) {
    routes.add_service(for_a_caller(wire::host_service_server(Offers {
        runtime: runtime.clone(),
    })));
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
        let provider = request.into_inner().provider;
        let offered = runtime
            .host_catalogue(&provider)
            .await
            .map_err(|error| status(error.to_wire()))?;
        Ok(Response::new(GetCatalogueResponse {
            hash: offered.blob.hash,
            bytes: offered.blob.bytes,
        }))
    }

    async fn pin_folder(
        &self,
        request: Request<PinFolderRequest>,
    ) -> Result<Response<PinFolderResponse>, Status> {
        if let Caller::Agent { .. } = Caller::of(&request)? {
            return Err(status(wire_error(
                ErrorCode::PermissionDenied,
                "only a person pins a folder: the list is the host's setting",
            )));
        }
        let runtime = upgrade(&self.runtime)?;
        let request = request.into_inner();
        runtime
            .pin_folder(&request.path, request.pinned)
            .await
            .map_err(settings_status)?;
        Ok(Response::new(PinFolderResponse {}))
    }
}
