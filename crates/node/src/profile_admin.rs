//! One profile's administration on its own socket: its account connection,
//! its pairing and the hosts it trusts. The socket names the profile, so
//! no call carries its id.

use tonic::{Request, Response, Status};
use wire::profile_admin_service_server::{ProfileAdminService, ProfileAdminServiceServer};
use wire::{
    BeginPairRequest, DeviceIdentity, Empty, ErrorCode, HostRef, ListPeersResponse, PairingStatus,
    PairingWindow, PeerEntry, PendingPair, PendingPairRequest, ProfileInfo, StartPairingRequest,
    TrustSshPeerRequest, UnpairRequest,
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
fn token(request: &PendingPairRequest) -> Result<&[u8], Status> {
    if request.token.is_empty() {
        return Err(failed(
            ErrorCode::InvalidArgument,
            "name the pairing by the token BeginPair answered",
        ));
    }
    Ok(&request.token)
}

#[tonic::async_trait]
impl ProfileAdminService for ProfileAdmin {
    async fn sign_out(&self, _request: Request<Empty>) -> Result<Response<ProfileInfo>, Status> {
        Ok(Response::new(info(
            self.registry.sign_out(self.profile).await?,
        )))
    }

    async fn pause(&self, _request: Request<Empty>) -> Result<Response<ProfileInfo>, Status> {
        Ok(Response::new(info(
            self.registry.set_paused(self.profile, true).await?,
        )))
    }

    async fn resume(&self, _request: Request<Empty>) -> Result<Response<ProfileInfo>, Status> {
        Ok(Response::new(info(
            self.registry.set_paused(self.profile, false).await?,
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
        _request: Request<Empty>,
    ) -> Result<Response<PairingStatus>, Status> {
        let edge = self.edge()?;
        Ok(Response::new(PairingStatus {
            active: edge.pairing_active(),
            locked_out: edge.pairing_locked_out(),
        }))
    }

    async fn cancel_pairing(&self, _request: Request<Empty>) -> Result<Response<Empty>, Status> {
        self.edge()?.cancel_pairing().await?;
        Ok(Response::new(Empty {}))
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
        request: Request<PendingPairRequest>,
    ) -> Result<Response<PeerEntry>, Status> {
        let request = request.into_inner();
        Ok(Response::new(
            self.edge()?.confirm_pair(token(&request)?).await?,
        ))
    }

    async fn abandon_pair(
        &self,
        request: Request<PendingPairRequest>,
    ) -> Result<Response<Empty>, Status> {
        let request = request.into_inner();
        self.edge()?.abandon_pair(token(&request)?).await?;
        Ok(Response::new(Empty {}))
    }

    async fn get_device_identity(
        &self,
        _request: Request<Empty>,
    ) -> Result<Response<DeviceIdentity>, Status> {
        Ok(Response::new(self.edge()?.device_identity()))
    }

    async fn trust_ssh_peer(
        &self,
        request: Request<TrustSshPeerRequest>,
    ) -> Result<Response<Empty>, Status> {
        self.edge()?.trust_ssh_peer(request.into_inner()).await?;
        Ok(Response::new(Empty {}))
    }

    async fn list_peers(
        &self,
        _request: Request<Empty>,
    ) -> Result<Response<ListPeersResponse>, Status> {
        Ok(Response::new(ListPeersResponse {
            peers: self.edge()?.list_peers()?,
        }))
    }

    async fn get_peer(&self, request: Request<HostRef>) -> Result<Response<PeerEntry>, Status> {
        Ok(Response::new(
            self.edge()?.peer_entry(request.into_inner())?,
        ))
    }

    async fn unpair(&self, request: Request<UnpairRequest>) -> Result<Response<PeerEntry>, Status> {
        let request = request.into_inner();
        let peer = request
            .peer
            .ok_or_else(|| failed(ErrorCode::InvalidArgument, "name the peer to unpair"))?;
        Ok(Response::new(
            self.edge()?.unpair(peer, request.reason).await?,
        ))
    }
}
