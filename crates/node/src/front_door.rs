//! The front door: the installation's own socket, where a client lists the
//! profiles, finds each one's client socket, pairs them with other hosts,
//! signs them in to an account, and manages the installation.

use std::pin::Pin;

use agent_dir::local_socket::LocalListener;
use futures_util::Stream;
use tokio::task::JoinHandle;
use tonic::{Request, Response, Status};
use uuid::Uuid;
use wire::installation_service_server::{InstallationService, InstallationServiceServer};
use wire::profile_service_server::{ProfileService, ProfileServiceServer};
use wire::{
    BeginPairRequest, BindProfileRequest, CaughtUp, CreateProfileRequest, DeleteProfileRequest,
    DeviceIdentity, Empty, ErrorCode, GetPeerRequest, InstallationInfo, Intent, ListPeersResponse,
    ListProfilesResponse, PairingStatus, PairingWindow, PeerEntry, PendingPair, PendingPairRequest,
    ProfileInfo, ProfileRequest, RenameProfileRequest, StartPairingRequest, TrustSshPeerRequest,
    UnpairRequest, profile_event,
};

use crate::Tier;
use crate::edge::{Binding, Edge, Observed, RelayCarrier};
use crate::grpc::{self, status, wire_error};
use crate::registry::{ProfileEvent, ProfileRefusal, ProfileState, Registry, SignIn};

/// A profile as the front door describes it.
fn info(state: ProfileState) -> ProfileInfo {
    let (tier, relay_carrier) = match &state.relay {
        Observed::Connected { tier, carrier } => (
            match tier {
                Tier::Free => wire::Tier::Free,
                Tier::Pro => wire::Tier::Pro,
            },
            match carrier {
                RelayCarrier::Quic => wire::RelayCarrier::Quic,
                RelayCarrier::Tcp => wire::RelayCarrier::Tcp,
            },
        ),
        _ => (wire::Tier::Unspecified, wire::RelayCarrier::Unspecified),
    };
    let intent = match state.binding {
        Binding::Unbound => Intent::Unbound,
        Binding::SignedIn => Intent::Bound,
        Binding::SignedOut => Intent::LoggedOut,
        Binding::Paused => Intent::Paused,
    };
    let account = state.account.unwrap_or_default();
    ProfileInfo {
        email: account.email.unwrap_or_default(),
        account_name: account.name.unwrap_or_default(),
        account_subject: account.subject,
        account_picture: account.picture.unwrap_or_default(),
        intent: intent as i32,
        observed: state.relay.to_wire() as i32,
        tier: tier as i32,
        relay_carrier: relay_carrier as i32,
        id: state.id.as_bytes().to_vec(),
        label: state.label,
        socket_path: state.socket.to_string_lossy().into_owned(),
    }
}

/// Serves the profile and installation services on the front door until
/// the task is aborted.
pub(crate) fn serve(listener: LocalListener, registry: Registry) -> JoinHandle<()> {
    let door = FrontDoor::new(registry);
    tokio::spawn(async move {
        let served = tonic::transport::Server::builder()
            .add_service(ProfileServiceServer::new(door.clone()))
            .add_service(InstallationServiceServer::new(door))
            .serve_with_incoming(grpc::incoming(listener))
            .await;
        if let Err(error) = served {
            tracing::warn!(%error, "the front door stopped serving");
        }
    })
}

/// The profile and installation services, served on the front door's
/// socket.
#[derive(Clone)]
pub struct FrontDoor {
    registry: Registry,
}

impl FrontDoor {
    pub(crate) fn new(registry: Registry) -> FrontDoor {
        FrontDoor { registry }
    }

    /// A hosted profile's network edge, named by its id's bytes.
    fn edge(&self, id: &[u8]) -> Result<std::sync::Arc<Edge>, Status> {
        Ok(self.registry.edge(profile_id(id)?)?)
    }
}

impl From<ProfileRefusal> for Status {
    fn from(refusal: ProfileRefusal) -> Status {
        status(refusal.to_wire())
    }
}

pub(crate) fn failed(code: ErrorCode, message: impl Into<String>) -> Status {
    status(wire_error(code, message))
}

fn profile_id(bytes: &[u8]) -> Result<Uuid, Status> {
    Uuid::from_slice(bytes)
        .map_err(|_| failed(ErrorCode::InvalidArgument, "a profile id is 16 bytes"))
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

type Events = Pin<Box<dyn Stream<Item = Result<wire::ProfileEvent, Status>> + Send>>;

#[tonic::async_trait]
impl ProfileService for FrontDoor {
    type WatchProfilesStream = Events;

    async fn list_profiles(
        &self,
        _request: Request<Empty>,
    ) -> Result<Response<ListProfilesResponse>, Status> {
        Ok(Response::new(ListProfilesResponse {
            profiles: self.registry.list()?.into_iter().map(info).collect(),
        }))
    }

    async fn watch_profiles(
        &self,
        _request: Request<Empty>,
    ) -> Result<Response<Self::WatchProfilesStream>, Status> {
        let mut watch = self.registry.watch()?;
        let opening = std::mem::take(&mut watch.listed)
            .into_iter()
            .map(|state| profile_event::Of::Upserted(info(state)))
            .chain(std::iter::once(profile_event::Of::CaughtUp(CaughtUp {
                revision: 0,
            })))
            .map(|of| Ok(wire::ProfileEvent { of: Some(of) }))
            .collect::<Vec<_>>();
        let live = futures_util::stream::unfold(Some(watch), |watch| async move {
            let mut watch = watch?;
            match watch.next().await? {
                Ok(event) => {
                    let of = match event {
                        ProfileEvent::Upserted(state) => profile_event::Of::Upserted(info(*state)),
                        ProfileEvent::Removed(id) => {
                            profile_event::Of::RemovedId(id.as_bytes().to_vec())
                        }
                    };
                    Some((Ok(wire::ProfileEvent { of: Some(of) }), Some(watch)))
                }
                Err(behind) => Some((Err(failed(ErrorCode::Aborted, behind.to_string())), None)),
            }
        });
        Ok(Response::new(Box::pin(futures_util::StreamExt::chain(
            futures_util::stream::iter(opening),
            live,
        ))))
    }

    async fn create_profile(
        &self,
        request: Request<CreateProfileRequest>,
    ) -> Result<Response<ProfileInfo>, Status> {
        let label = request.into_inner().label;
        Ok(Response::new(info(
            self.registry.create(label.as_deref()).await?,
        )))
    }

    async fn rename_profile(
        &self,
        request: Request<RenameProfileRequest>,
    ) -> Result<Response<ProfileInfo>, Status> {
        let request = request.into_inner();
        let id = profile_id(&request.profile_id)?;
        let label = request.override_name.unwrap_or_default();
        Ok(Response::new(info(self.registry.rename(id, &label).await?)))
    }

    async fn delete_profile(
        &self,
        request: Request<DeleteProfileRequest>,
    ) -> Result<Response<Empty>, Status> {
        let id = profile_id(&request.into_inner().profile_id)?;
        self.registry.delete(id).await?;
        Ok(Response::new(Empty {}))
    }

    async fn bind_profile(
        &self,
        request: Request<BindProfileRequest>,
    ) -> Result<Response<ProfileInfo>, Status> {
        let request = request.into_inner();
        let sign_in = SignIn {
            profile: request.profile_id.as_deref().map(profile_id).transpose()?,
            cloud_url: request.cloud_url,
            refresh_token: request.staged_refresh_token,
            client_id: request.client_id,
            adopt: request.adopt_non_pristine,
        };
        Ok(Response::new(info(self.registry.bind(sign_in).await?)))
    }

    async fn logout_profile(
        &self,
        request: Request<ProfileRequest>,
    ) -> Result<Response<ProfileInfo>, Status> {
        let id = profile_id(&request.into_inner().profile_id)?;
        Ok(Response::new(info(self.registry.sign_out(id).await?)))
    }

    async fn pause_profile(
        &self,
        request: Request<ProfileRequest>,
    ) -> Result<Response<ProfileInfo>, Status> {
        let id = profile_id(&request.into_inner().profile_id)?;
        Ok(Response::new(info(self.registry.set_paused(id, true).await?)))
    }

    async fn resume_profile(
        &self,
        request: Request<ProfileRequest>,
    ) -> Result<Response<ProfileInfo>, Status> {
        let id = profile_id(&request.into_inner().profile_id)?;
        Ok(Response::new(info(
            self.registry.set_paused(id, false).await?,
        )))
    }

    async fn start_pairing(
        &self,
        request: Request<StartPairingRequest>,
    ) -> Result<Response<PairingWindow>, Status> {
        let request = request.into_inner();
        let edge = self.edge(&request.profile_id)?;
        Ok(Response::new(edge.start_pairing(request).await?))
    }

    async fn get_pairing_status(
        &self,
        request: Request<ProfileRequest>,
    ) -> Result<Response<PairingStatus>, Status> {
        let edge = self.edge(&request.into_inner().profile_id)?;
        Ok(Response::new(PairingStatus {
            active: edge.pairing_active(),
            locked_out: edge.pairing_locked_out(),
        }))
    }

    async fn cancel_pairing(
        &self,
        request: Request<ProfileRequest>,
    ) -> Result<Response<Empty>, Status> {
        let edge = self.edge(&request.into_inner().profile_id)?;
        edge.cancel_pairing().await?;
        Ok(Response::new(Empty {}))
    }

    async fn begin_pair(
        &self,
        request: Request<BeginPairRequest>,
    ) -> Result<Response<PendingPair>, Status> {
        let request = request.into_inner();
        let edge = self.edge(&request.profile_id)?;
        Ok(Response::new(edge.begin_pair(request).await?))
    }

    async fn confirm_pair(
        &self,
        request: Request<PendingPairRequest>,
    ) -> Result<Response<PeerEntry>, Status> {
        let request = request.into_inner();
        let edge = self.edge(&request.profile_id)?;
        Ok(Response::new(edge.confirm_pair(token(&request)?).await?))
    }

    async fn abandon_pair(
        &self,
        request: Request<PendingPairRequest>,
    ) -> Result<Response<Empty>, Status> {
        let request = request.into_inner();
        let edge = self.edge(&request.profile_id)?;
        edge.abandon_pair(token(&request)?).await?;
        Ok(Response::new(Empty {}))
    }

    async fn get_device_identity(
        &self,
        request: Request<ProfileRequest>,
    ) -> Result<Response<DeviceIdentity>, Status> {
        let edge = self.edge(&request.into_inner().profile_id)?;
        Ok(Response::new(edge.device_identity()))
    }

    async fn trust_ssh_peer(
        &self,
        request: Request<TrustSshPeerRequest>,
    ) -> Result<Response<Empty>, Status> {
        let request = request.into_inner();
        let edge = self.edge(&request.profile_id)?;
        edge.trust_ssh_peer(request).await?;
        Ok(Response::new(Empty {}))
    }

    async fn list_peers(
        &self,
        request: Request<ProfileRequest>,
    ) -> Result<Response<ListPeersResponse>, Status> {
        let edge = self.edge(&request.into_inner().profile_id)?;
        Ok(Response::new(ListPeersResponse {
            peers: edge.list_peers()?,
        }))
    }

    async fn get_peer(
        &self,
        request: Request<GetPeerRequest>,
    ) -> Result<Response<PeerEntry>, Status> {
        let request = request.into_inner();
        let edge = self.edge(&request.profile_id)?;
        let peer = request
            .peer
            .ok_or_else(|| failed(ErrorCode::InvalidArgument, "name the peer to read"))?;
        Ok(Response::new(edge.peer_entry(peer)?))
    }

    async fn unpair(&self, request: Request<UnpairRequest>) -> Result<Response<PeerEntry>, Status> {
        let request = request.into_inner();
        let edge = self.edge(&request.profile_id)?;
        let peer = request
            .peer
            .ok_or_else(|| failed(ErrorCode::InvalidArgument, "name the peer to unpair"))?;
        Ok(Response::new(edge.unpair(peer, request.reason).await?))
    }
}

#[tonic::async_trait]
impl InstallationService for FrontDoor {
    async fn get_info(
        &self,
        _request: Request<Empty>,
    ) -> Result<Response<InstallationInfo>, Status> {
        self.registry.installation()?;
        Ok(Response::new(InstallationInfo {
            version: crate::version().to_owned(),
        }))
    }

    async fn shutdown(&self, _request: Request<Empty>) -> Result<Response<Empty>, Status> {
        self.registry.installation()?.request_shutdown();
        Ok(Response::new(Empty {}))
    }
}
