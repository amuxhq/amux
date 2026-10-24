//! The front door: the installation's own socket, where a client lists the
//! profiles, finds each one's client socket, creates, names, deletes and
//! signs them in, and manages the installation.

use std::pin::Pin;

use agent_dir::local_socket::LocalListener;
use futures_util::Stream;
use tokio::task::JoinHandle;
use tonic::{Request, Response, Status};
use uuid::Uuid;
use wire::installation_service_server::{InstallationService, InstallationServiceServer};
use wire::profile_service_server::{ProfileService, ProfileServiceServer};
use wire::{
    BindProfileRequest, CaughtUp, CreateProfileRequest, DeleteProfileRequest, Empty, ErrorCode,
    InstallationInfo, Intent, ListProfilesResponse, ProfileInfo, RenameProfileRequest,
    profile_event,
};

use crate::Tier;
use crate::edge::{Binding, Observed, RelayCarrier};
use crate::grpc::{self, status, wire_error};
use crate::registry::{ProfileEvent, ProfileRefusal, ProfileState, Registry, SignIn};

/// A profile as the front door describes it.
pub(crate) fn info(state: ProfileState) -> ProfileInfo {
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
