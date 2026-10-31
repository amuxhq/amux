//! One profile's administration on its own socket: its account connection,
//! its pairing, the hosts it trusts and its diagnostic dump. The socket names the profile, so
//! no call carries its id.

use tonic::{Request, Response, Status};
use wire::profile_admin_service_server::{ProfileAdminService, ProfileAdminServiceServer};
use wire::{
    BeginPairRequest, DeviceIdentity, ErrorCode, ListPeersResponse, PairingStatus, PairingWindow,
    PendingPair, StartPairingRequest, TrustSshPeerRequest, UnpairRequest,
};

use crate::ProfileId;
use crate::edge::Edge;
use crate::front_door::{failed, info};
use crate::registry::Registry;

/// The administration service of the profile whose socket serves it, or
/// called in process.
#[derive(Clone)]
pub struct ProfileAdmin {
    registry: Registry,
    profile: ProfileId,
}

impl ProfileAdmin {
    pub(crate) fn new(registry: Registry, profile: ProfileId) -> ProfileAdmin {
        ProfileAdmin { registry, profile }
    }

    pub(crate) fn server(self) -> ProfileAdminServiceServer<Self> {
        ProfileAdminServiceServer::new(self)
    }

    fn edge(&self) -> Result<std::sync::Arc<Edge>, Status> {
        Ok(self.registry.edge(self.profile)?)
    }
}

/// A pairing's token, which `BeginPair` handed out: never empty.
fn token(token: &[u8]) -> Result<&[u8], Status> {
    if token.is_empty() {
        return Err(failed(
            ErrorCode::InvalidArgument,
            "name the pairing by the token BeginPair answered",
        ));
    }
    Ok(token)
}

#[tonic::async_trait]
impl ProfileAdminService for ProfileAdmin {
    async fn sign_out(
        &self,
        _request: Request<wire::SignOutRequest>,
    ) -> Result<Response<wire::Profile>, Status> {
        Ok(Response::new(info(
            self.registry.sign_out(self.profile).await?,
        )))
    }

    async fn start_pairing(
        &self,
        request: Request<StartPairingRequest>,
    ) -> Result<Response<PairingWindow>, Status> {
        Ok(Response::new(
            self.edge()?.start_pairing(request.into_inner()).await?,
        ))
    }

    async fn get_pairing_status(
        &self,
        _request: Request<wire::GetPairingStatusRequest>,
    ) -> Result<Response<PairingStatus>, Status> {
        let edge = self.edge()?;
        Ok(Response::new(PairingStatus {
            active: edge.pairing_active(),
            locked_out: edge.pairing_locked_out(),
        }))
    }

    async fn cancel_pairing(
        &self,
        _request: Request<wire::CancelPairingRequest>,
    ) -> Result<Response<wire::CancelPairingResponse>, Status> {
        self.edge()?.cancel_pairing().await?;
        Ok(Response::new(wire::CancelPairingResponse {}))
    }

    async fn begin_pair(
        &self,
        request: Request<BeginPairRequest>,
    ) -> Result<Response<PendingPair>, Status> {
        Ok(Response::new(
            self.edge()?.begin_pair(request.into_inner()).await?,
        ))
    }

    async fn confirm_pair(
        &self,
        request: Request<wire::ConfirmPairRequest>,
    ) -> Result<Response<wire::Peer>, Status> {
        let request = request.into_inner();
        Ok(Response::new(
            self.edge()?.confirm_pair(token(&request.token)?).await?,
        ))
    }

    async fn abandon_pair(
        &self,
        request: Request<wire::AbandonPairRequest>,
    ) -> Result<Response<wire::AbandonPairResponse>, Status> {
        let request = request.into_inner();
        self.edge()?.abandon_pair(token(&request.token)?).await?;
        Ok(Response::new(wire::AbandonPairResponse {}))
    }

    async fn get_device_identity(
        &self,
        _request: Request<wire::GetDeviceIdentityRequest>,
    ) -> Result<Response<DeviceIdentity>, Status> {
        Ok(Response::new(self.edge()?.device_identity()))
    }

    async fn trust_ssh_peer(
        &self,
        request: Request<TrustSshPeerRequest>,
    ) -> Result<Response<wire::TrustSshPeerResponse>, Status> {
        self.edge()?.trust_ssh_peer(request.into_inner()).await?;
        Ok(Response::new(wire::TrustSshPeerResponse {}))
    }

    async fn list_peers(
        &self,
        _request: Request<wire::ListPeersRequest>,
    ) -> Result<Response<ListPeersResponse>, Status> {
        Ok(Response::new(ListPeersResponse {
            peers: self.edge()?.list_peers()?,
        }))
    }

    async fn unpair(
        &self,
        request: Request<UnpairRequest>,
    ) -> Result<Response<wire::Peer>, Status> {
        let peer = request
            .into_inner()
            .peer
            .ok_or_else(|| failed(ErrorCode::InvalidArgument, "name the peer to unpair"))?;
        Ok(Response::new(self.edge()?.unpair(peer).await?))
    }

    async fn dump(
        &self,
        request: Request<wire::DumpRequest>,
    ) -> Result<Response<wire::DumpResponse>, Status> {
        // A local caller reads the bundle where it was written.
        let path = self
            .registry
            .runtime(self.profile)?
            .dump(request.into_inner())
            .await
            .map_err(|error| crate::grpc::status(error.to_wire()))?;
        Ok(Response::new(wire::DumpResponse {
            report_path: path.to_string_lossy().into_owned(),
        }))
    }

    async fn submit_report(
        &self,
        request: Request<wire::SubmitReportRequest>,
    ) -> Result<Response<wire::SubmitReportResponse>, Status> {
        let response = self
            .registry
            .runtime(self.profile)?
            .submit_report(request.into_inner())
            .await
            .map_err(|error| crate::grpc::status(error.to_wire()))?;
        Ok(Response::new(response))
    }
}
