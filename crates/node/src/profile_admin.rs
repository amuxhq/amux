//! One profile's administration on its own socket: its account connection,
//! its pairing, the hosts it trusts and its diagnostic dump. The socket names the profile, so
//! no call carries its id.

use tonic::{Request, Response, Status};
use wire::profile_admin_service_server::{ProfileAdminService, ProfileAdminServiceServer};
use wire::{
    AbandonPairRequest, AbandonPairResponse, AnswerSignInOfferRequest, AnswerSignInOfferResponse,
    BeginPairRequest, BeginPairResponse, CancelPairingRequest, CancelPairingResponse,
    ConfirmPairRequest, ConfirmPairResponse, ErrorCode, GetDeviceIdentityRequest,
    GetDeviceIdentityResponse, ListPeersRequest, ListPeersResponse, OfferSignInRequest,
    OfferSignInResponse, SignOutRequest, SignOutResponse, StartPairingRequest,
    StartPairingResponse, SubscribePairingRequest, SubscribePairingResponse,
    SubscribeSignInOffersRequest, SubscribeSignInOffersResponse, TrustSshPeerRequest,
    TrustSshPeerResponse, UnpairRequest, UnpairResponse,
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

/// The paired host a call names.
fn host(bytes: &[u8]) -> Result<uuid::Uuid, Status> {
    uuid::Uuid::from_slice(bytes)
        .map_err(|_| failed(ErrorCode::InvalidArgument, "host_id names no host"))
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
    type SubscribePairingStream = tonic::codegen::BoxStream<SubscribePairingResponse>;
    type SubscribeSignInOffersStream = tonic::codegen::BoxStream<SubscribeSignInOffersResponse>;

    async fn subscribe_pairing(
        &self,
        _request: Request<SubscribePairingRequest>,
    ) -> Result<Response<Self::SubscribePairingStream>, Status> {
        use futures_util::StreamExt as _;
        Ok(Response::new(Box::pin(
            self.edge()?.subscribe_pairing().map(|status| {
                Ok(SubscribePairingResponse {
                    status: Some(status),
                })
            }),
        )))
    }

    async fn offer_sign_in(
        &self,
        request: Request<OfferSignInRequest>,
    ) -> Result<Response<OfferSignInResponse>, Status> {
        let host = host(&request.into_inner().host_id)?;
        self.edge()?.offer_sign_in(host)?;
        Ok(Response::new(OfferSignInResponse {}))
    }

    async fn subscribe_sign_in_offers(
        &self,
        _request: Request<SubscribeSignInOffersRequest>,
    ) -> Result<Response<Self::SubscribeSignInOffersStream>, Status> {
        use futures_util::StreamExt as _;
        Ok(Response::new(Box::pin(
            self.edge()?.watch_sign_in_offers().map(Ok),
        )))
    }

    async fn answer_sign_in_offer(
        &self,
        request: Request<AnswerSignInOfferRequest>,
    ) -> Result<Response<AnswerSignInOfferResponse>, Status> {
        let request = request.into_inner();
        let host = host(&request.host_id)?;
        let edge = self.edge()?;
        if !request.accept {
            edge.dismiss_sign_in_offer(host)?;
            let state = self
                .registry
                .list()?
                .into_iter()
                .find(|state| state.id == self.profile)
                .ok_or_else(|| failed(ErrorCode::NotFound, "the profile is gone"))?;
            return Ok(Response::new(AnswerSignInOfferResponse {
                profile: Some(info(state)),
            }));
        }
        let (ticket, cloud_url) = edge.take_sign_in_ticket(host).await?;
        let state = crate::edge::sign_in_offers::redeem_and_bind(
            &self.registry,
            self.profile,
            &edge,
            ticket,
            cloud_url,
        )
        .await?;
        Ok(Response::new(AnswerSignInOfferResponse {
            profile: Some(info(state)),
        }))
    }

    async fn sign_out(
        &self,
        _request: Request<SignOutRequest>,
    ) -> Result<Response<SignOutResponse>, Status> {
        Ok(Response::new(SignOutResponse {
            profile: Some(info(self.registry.sign_out(self.profile).await?)),
        }))
    }

    async fn start_pairing(
        &self,
        request: Request<StartPairingRequest>,
    ) -> Result<Response<StartPairingResponse>, Status> {
        Ok(Response::new(StartPairingResponse {
            window: Some(self.edge()?.start_pairing(request.into_inner()).await?),
        }))
    }

    async fn cancel_pairing(
        &self,
        _request: Request<CancelPairingRequest>,
    ) -> Result<Response<CancelPairingResponse>, Status> {
        self.edge()?.cancel_pairing().await?;
        Ok(Response::new(CancelPairingResponse {}))
    }

    async fn begin_pair(
        &self,
        request: Request<BeginPairRequest>,
    ) -> Result<Response<BeginPairResponse>, Status> {
        Ok(Response::new(BeginPairResponse {
            pending: Some(self.edge()?.begin_pair(request.into_inner()).await?),
        }))
    }

    async fn confirm_pair(
        &self,
        request: Request<ConfirmPairRequest>,
    ) -> Result<Response<ConfirmPairResponse>, Status> {
        let request = request.into_inner();
        Ok(Response::new(ConfirmPairResponse {
            peer: Some(self.edge()?.confirm_pair(token(&request.token)?).await?),
        }))
    }

    async fn abandon_pair(
        &self,
        request: Request<AbandonPairRequest>,
    ) -> Result<Response<AbandonPairResponse>, Status> {
        let request = request.into_inner();
        self.edge()?.abandon_pair(token(&request.token)?).await?;
        Ok(Response::new(AbandonPairResponse {}))
    }

    async fn get_device_identity(
        &self,
        _request: Request<GetDeviceIdentityRequest>,
    ) -> Result<Response<GetDeviceIdentityResponse>, Status> {
        Ok(Response::new(GetDeviceIdentityResponse {
            identity: Some(self.edge()?.device_identity()),
        }))
    }

    async fn trust_ssh_peer(
        &self,
        request: Request<TrustSshPeerRequest>,
    ) -> Result<Response<TrustSshPeerResponse>, Status> {
        self.edge()?.trust_ssh_peer(request.into_inner()).await?;
        Ok(Response::new(TrustSshPeerResponse {}))
    }

    async fn list_peers(
        &self,
        _request: Request<ListPeersRequest>,
    ) -> Result<Response<ListPeersResponse>, Status> {
        Ok(Response::new(ListPeersResponse {
            peers: self.edge()?.list_peers()?,
        }))
    }

    async fn unpair(
        &self,
        request: Request<UnpairRequest>,
    ) -> Result<Response<UnpairResponse>, Status> {
        let peer = request
            .into_inner()
            .peer
            .ok_or_else(|| failed(ErrorCode::InvalidArgument, "name the peer to unpair"))?;
        Ok(Response::new(UnpairResponse {
            peer: Some(self.edge()?.unpair(peer).await?),
        }))
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
