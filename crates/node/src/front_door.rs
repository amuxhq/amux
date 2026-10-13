//! The front door: the installation's own socket, where a client lists the
//! profiles, finds each one's client socket, pairs them with other hosts,
//! signs them in to an account, and manages the installation.

use std::pin::Pin;
use std::sync::{Arc, Weak};

use agent_dir::local_socket::LocalListener;
use futures_util::Stream;
use tokio::sync::broadcast::error::RecvError;
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
use crate::daemon::{Hosted, Installation};
use crate::edge::{Edge, Observed, RelayCarrier, account};
use crate::grpc::{self, status, wire_error};
use crate::profiles::{PROFILE_SOCKET, Registry, profile_dir};

/// A change the front door's watchers see.
#[derive(Clone, Debug)]
pub(crate) enum ProfileEvent {
    Upserted(Box<ProfileInfo>),
    Removed(Uuid),
}

/// A hosted profile as the front door describes it.
pub(crate) fn info(hosted: &Hosted) -> ProfileInfo {
    let edge = hosted.runtime.edge();
    let record = edge.as_ref().and_then(|edge| edge.account().record());
    let observed = edge.as_ref().map(|edge| edge.observed());
    let (tier, relay_carrier) = match &observed {
        Some(Observed::Connected { tier, carrier }) => (
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
    ProfileInfo {
        email: record
            .as_ref()
            .and_then(|record| record.email.clone())
            .unwrap_or_default(),
        account_name: record
            .as_ref()
            .and_then(|record| record.name.clone())
            .unwrap_or_default(),
        account_subject: record
            .as_ref()
            .map(|record| record.subject.clone())
            .unwrap_or_default(),
        intent: edge
            .as_ref()
            .map_or(Intent::Unbound, |edge| edge.account().intent()) as i32,
        observed: observed.map_or(wire::Observed::Local, |observed| observed.to_wire()) as i32,
        tier: tier as i32,
        relay_carrier: relay_carrier as i32,
        id: hosted.entry.id.as_bytes().to_vec(),
        label: hosted.entry.label.clone(),
        socket_path: hosted
            .runtime
            .dir()
            .join(PROFILE_SOCKET)
            .to_string_lossy()
            .into_owned(),
        host_id: hosted.runtime.host().as_bytes().to_vec(),
        revision: hosted.entry.revision,
    }
}

/// The analytics a sign-in's outcome is recorded on: the profile it bound
/// or named, else the oldest.
fn sign_in_analytics(
    installation: &Installation,
    profile: Option<crate::ProfileId>,
) -> Option<analytics::Analytics> {
    let hosted = installation.hosted.lock().unwrap();
    profile
        .and_then(|profile| hosted.get(&profile))
        .or_else(|| hosted.values().min_by_key(|hosted| hosted.position))
        .map(|hosted| hosted.runtime.analytics().clone())
}

/// Why a sign-in failed, as analytics reads the refusal.
fn sign_in_failure(status: &Status) -> analytics::SignInFailure {
    use analytics::SignInFailure;
    match crate::net_error::code_of(status) {
        Some(ErrorCode::Unauthenticated) => SignInFailure::Rejected,
        Some(ErrorCode::Unavailable) => SignInFailure::Unreachable,
        Some(ErrorCode::AlreadyExists) => SignInFailure::AccountElsewhere,
        Some(ErrorCode::FailedPrecondition) => SignInFailure::ProfileConflict,
        _ => SignInFailure::Other,
    }
}

/// Serves the profile and installation services on the front door until
/// the task is aborted.
pub(crate) fn serve(listener: LocalListener, installation: Weak<Installation>) -> JoinHandle<()> {
    let door = FrontDoor::new(installation);
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
/// socket or called in process by an embedder that serves no sockets.
#[derive(Clone)]
pub struct FrontDoor {
    installation: Weak<Installation>,
}

impl FrontDoor {
    pub(crate) fn new(installation: Weak<Installation>) -> FrontDoor {
        FrontDoor { installation }
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

impl FrontDoor {
    fn installation(&self) -> Result<Arc<Installation>, Status> {
        self.installation
            .upgrade()
            .ok_or_else(|| Status::unavailable("the daemon is shutting down"))
    }

    /// Every hosted profile, oldest first.
    fn list(installation: &Installation) -> Vec<ProfileInfo> {
        let hosted = installation.hosted.lock().unwrap();
        let mut hosted: Vec<&Hosted> = hosted.values().collect();
        hosted.sort_by_key(|hosted| hosted.position);
        hosted.into_iter().map(info).collect()
    }

    /// A hosted profile's network edge.
    fn edge(installation: &Installation, id: &[u8]) -> Result<(Uuid, Arc<Edge>), Status> {
        let id = profile_id(id)?;
        let edge = installation
            .hosted
            .lock()
            .unwrap()
            .get(&id)
            .ok_or_else(|| failed(ErrorCode::NotFound, format!("no profile {id}")))?
            .runtime
            .edge()
            .ok_or_else(|| failed(ErrorCode::Unavailable, "the profile is not in service"))?;
        Ok((id, edge))
    }

    async fn set_paused(
        &self,
        request: ProfileRequest,
        paused: bool,
    ) -> Result<Response<ProfileInfo>, Status> {
        let installation = self.installation()?;
        let (id, edge) = Self::edge(&installation, &request.profile_id)?;
        if edge.account().record().is_none() {
            return Err(failed(
                ErrorCode::FailedPrecondition,
                "the profile is not bound to an account",
            ));
        }
        edge.set_paused(paused)
            .await
            .map_err(|error| failed(ErrorCode::Internal, error.to_string()))?;
        installation.republish(id);
        Ok(Response::new(Self::one(&installation, id)?))
    }

    fn one(installation: &Installation, id: Uuid) -> Result<ProfileInfo, Status> {
        installation
            .hosted
            .lock()
            .unwrap()
            .get(&id)
            .map(info)
            .ok_or_else(|| failed(ErrorCode::NotFound, format!("no profile {id}")))
    }
}

type Events = Pin<Box<dyn Stream<Item = Result<wire::ProfileEvent, Status>> + Send>>;

#[tonic::async_trait]
impl ProfileService for FrontDoor {
    type WatchProfilesStream = Events;

    async fn list_profiles(
        &self,
        _request: Request<Empty>,
    ) -> Result<Response<ListProfilesResponse>, Status> {
        let installation = self.installation()?;
        Ok(Response::new(ListProfilesResponse {
            profiles: Self::list(&installation),
        }))
    }

    async fn watch_profiles(
        &self,
        _request: Request<Empty>,
    ) -> Result<Response<Self::WatchProfilesStream>, Status> {
        let installation = self.installation()?;
        // The publishing lock is held while every change is published, so
        // the list read under it is exactly the state before the first
        // event the receiver sees.
        let (profiles, events) = {
            let _publishing = installation.publishing.lock().unwrap();
            let events = installation.events.subscribe();
            (Self::list(&installation), events)
        };
        let opening = profiles
            .into_iter()
            .map(profile_event::Of::Upserted)
            .chain(std::iter::once(profile_event::Of::CaughtUp(CaughtUp {
                revision: 0,
            })))
            .map(|of| Ok(wire::ProfileEvent { of: Some(of) }))
            .collect::<Vec<_>>();
        let live = futures_util::stream::unfold(Some(events), |events| async move {
            let mut events = events?;
            match events.recv().await {
                Ok(event) => {
                    let of = match event {
                        ProfileEvent::Upserted(info) => profile_event::Of::Upserted(*info),
                        ProfileEvent::Removed(id) => {
                            profile_event::Of::RemovedId(id.as_bytes().to_vec())
                        }
                    };
                    Some((Ok(wire::ProfileEvent { of: Some(of) }), Some(events)))
                }
                // A watcher that fell behind watches again for a fresh list.
                Err(RecvError::Lagged(_)) => Some((
                    Err(failed(
                        ErrorCode::Aborted,
                        "the watch fell behind; watch again",
                    )),
                    None,
                )),
                Err(RecvError::Closed) => None,
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
        let installation = self.installation()?;
        let label = request
            .into_inner()
            .label
            .filter(|label| !label.trim().is_empty())
            .unwrap_or_else(|| "profile".to_owned());
        let _change = installation.registry_changes.lock().await;
        let id = installation
            .create(label.trim())
            .await
            .map_err(|error| failed(ErrorCode::Internal, error.to_string()))?;
        Ok(Response::new(Self::one(&installation, id)?))
    }

    async fn rename_profile(
        &self,
        request: Request<RenameProfileRequest>,
    ) -> Result<Response<ProfileInfo>, Status> {
        let installation = self.installation()?;
        let request = request.into_inner();
        let id = profile_id(&request.profile_id)?;
        let label = request
            .override_name
            .map(|name| name.trim().to_owned())
            .filter(|name| !name.is_empty())
            .ok_or_else(|| failed(ErrorCode::InvalidArgument, "a profile needs a name"))?;
        let _change = installation.registry_changes.lock().await;
        let mut registry = Registry::read(&installation.data_dir)
            .map_err(|error| failed(ErrorCode::Internal, error.to_string()))?;
        if registry
            .profiles
            .iter()
            .any(|entry| entry.id != id && entry.label == label)
        {
            return Err(failed(
                ErrorCode::AlreadyExists,
                format!("another profile is named {label}"),
            ));
        }
        let entry = registry
            .profiles
            .iter_mut()
            .find(|entry| entry.id == id)
            .ok_or_else(|| failed(ErrorCode::NotFound, format!("no profile {id}")))?;
        if request.expected_revision != 0 && request.expected_revision != entry.revision {
            return Err(failed(
                ErrorCode::Aborted,
                "the profile changed since it was read; read it again",
            ));
        }
        entry.label = label;
        entry.revision += 1;
        let entry = entry.clone();
        registry
            .write(&installation.data_dir)
            .map_err(|error| failed(ErrorCode::Internal, error.to_string()))?;
        let info = {
            let mut hosted = installation.hosted.lock().unwrap();
            let hosted = hosted
                .get_mut(&id)
                .ok_or_else(|| failed(ErrorCode::NotFound, format!("no profile {id}")))?;
            hosted.entry = entry;
            info(hosted)
        };
        installation.publish(crate::front_door::ProfileEvent::Upserted(Box::new(
            info.clone(),
        )));
        Ok(Response::new(info))
    }

    async fn delete_profile(
        &self,
        request: Request<DeleteProfileRequest>,
    ) -> Result<Response<Empty>, Status> {
        let installation = self.installation()?;
        let request = request.into_inner();
        let id = profile_id(&request.profile_id)?;
        let _change = installation.registry_changes.lock().await;
        let mut registry = Registry::read(&installation.data_dir)
            .map_err(|error| failed(ErrorCode::Internal, error.to_string()))?;
        let entry = registry
            .entry(id)
            .cloned()
            .ok_or_else(|| failed(ErrorCode::NotFound, format!("no profile {id}")))?;
        if request.confirm_revision != entry.revision {
            return Err(failed(
                ErrorCode::Aborted,
                "the profile changed since it was read; read it again",
            ));
        }
        if registry.profiles.len() == 1 {
            return Err(failed(
                ErrorCode::FailedPrecondition,
                "an installation keeps at least one profile",
            ));
        }
        let runtime = installation
            .hosted
            .lock()
            .unwrap()
            .get(&id)
            .map(|hosted| hosted.runtime.clone());
        if runtime
            .as_ref()
            .is_some_and(|runtime| !runtime.live().is_empty())
        {
            return Err(failed(
                ErrorCode::FailedPrecondition,
                "the profile has running agents; stop them first",
            ));
        }
        // The registry entry goes first: a crash after it leaves a
        // directory nothing lists, never a listed profile half removed.
        registry.profiles.retain(|entry| entry.id != id);
        registry
            .write(&installation.data_dir)
            .map_err(|error| failed(ErrorCode::Internal, error.to_string()))?;
        let hosted = installation.hosted.lock().unwrap().remove(&id);
        if let Some(hosted) = hosted {
            hosted
                .runtime
                .stop_edge(wire::LinkCloseReason::UserShutdown)
                .await;
            hosted.runtime.stop_background().await;
            hosted.runtime.stop_watching().await;
        }
        drop(runtime);
        std::fs::remove_dir_all(profile_dir(&installation.data_dir, id))
            .map_err(|error| failed(ErrorCode::Internal, error.to_string()))?;
        installation.publish(ProfileEvent::Removed(id));
        Ok(Response::new(Empty {}))
    }

    async fn bind_profile(
        &self,
        request: Request<BindProfileRequest>,
    ) -> Result<Response<ProfileInfo>, Status> {
        let installation = self.installation()?;
        let request = request.into_inner();
        let explicit = request.profile_id.as_deref().map(profile_id).transpose()?;
        let bound = account::bind(&installation, explicit, request).await;
        // A failure that named no profile is the installation's: its oldest
        // profile records it.
        let profile = bound.as_ref().ok().copied().or(explicit);
        if let Some(analytics) = sign_in_analytics(&installation, profile) {
            analytics.record(match &bound {
                Ok(_) => analytics::Event::SignedIn,
                Err(status) => analytics::Event::SignInFailed {
                    reason: sign_in_failure(status),
                },
            });
        }
        let id = bound?;
        installation.republish(id);
        Ok(Response::new(Self::one(&installation, id)?))
    }

    async fn logout_profile(
        &self,
        request: Request<ProfileRequest>,
    ) -> Result<Response<ProfileInfo>, Status> {
        let installation = self.installation()?;
        let (id, edge) = Self::edge(&installation, &request.into_inner().profile_id)?;
        edge.sign_out()
            .await
            .map_err(|error| failed(ErrorCode::Internal, error.to_string()))?;
        edge.analytics().record(analytics::Event::SignedOut);
        installation.republish(id);
        Ok(Response::new(Self::one(&installation, id)?))
    }

    async fn pause_profile(
        &self,
        request: Request<ProfileRequest>,
    ) -> Result<Response<ProfileInfo>, Status> {
        self.set_paused(request.into_inner(), true).await
    }

    async fn resume_profile(
        &self,
        request: Request<ProfileRequest>,
    ) -> Result<Response<ProfileInfo>, Status> {
        self.set_paused(request.into_inner(), false).await
    }

    async fn start_pairing(
        &self,
        request: Request<StartPairingRequest>,
    ) -> Result<Response<PairingWindow>, Status> {
        let installation = self.installation()?;
        let request = request.into_inner();
        let (_, edge) = Self::edge(&installation, &request.profile_id)?;
        Ok(Response::new(edge.start_pairing(request).await?))
    }

    async fn get_pairing_status(
        &self,
        request: Request<ProfileRequest>,
    ) -> Result<Response<PairingStatus>, Status> {
        let installation = self.installation()?;
        let (_, edge) = Self::edge(&installation, &request.into_inner().profile_id)?;
        Ok(Response::new(PairingStatus {
            active: edge.pairing_active(),
            locked_out: edge.pairing_locked_out(),
        }))
    }

    async fn cancel_pairing(
        &self,
        request: Request<ProfileRequest>,
    ) -> Result<Response<Empty>, Status> {
        let installation = self.installation()?;
        let (_, edge) = Self::edge(&installation, &request.into_inner().profile_id)?;
        edge.cancel_pairing().await?;
        Ok(Response::new(Empty {}))
    }

    async fn begin_pair(
        &self,
        request: Request<BeginPairRequest>,
    ) -> Result<Response<PendingPair>, Status> {
        let installation = self.installation()?;
        let request = request.into_inner();
        let (_, edge) = Self::edge(&installation, &request.profile_id)?;
        Ok(Response::new(edge.begin_pair(request).await?))
    }

    async fn confirm_pair(
        &self,
        request: Request<PendingPairRequest>,
    ) -> Result<Response<PeerEntry>, Status> {
        let installation = self.installation()?;
        let request = request.into_inner();
        let (_, edge) = Self::edge(&installation, &request.profile_id)?;
        Ok(Response::new(edge.confirm_pair(token(&request)?).await?))
    }

    async fn abandon_pair(
        &self,
        request: Request<PendingPairRequest>,
    ) -> Result<Response<Empty>, Status> {
        let installation = self.installation()?;
        let request = request.into_inner();
        let (_, edge) = Self::edge(&installation, &request.profile_id)?;
        edge.abandon_pair(token(&request)?).await?;
        Ok(Response::new(Empty {}))
    }

    async fn get_device_identity(
        &self,
        request: Request<ProfileRequest>,
    ) -> Result<Response<DeviceIdentity>, Status> {
        let installation = self.installation()?;
        let (_, edge) = Self::edge(&installation, &request.into_inner().profile_id)?;
        Ok(Response::new(edge.device_identity()))
    }

    async fn trust_ssh_peer(
        &self,
        request: Request<TrustSshPeerRequest>,
    ) -> Result<Response<Empty>, Status> {
        let installation = self.installation()?;
        let request = request.into_inner();
        let (_, edge) = Self::edge(&installation, &request.profile_id)?;
        edge.trust_ssh_peer(request).await?;
        Ok(Response::new(Empty {}))
    }

    async fn list_peers(
        &self,
        request: Request<ProfileRequest>,
    ) -> Result<Response<ListPeersResponse>, Status> {
        let installation = self.installation()?;
        let (_, edge) = Self::edge(&installation, &request.into_inner().profile_id)?;
        Ok(Response::new(ListPeersResponse {
            peers: edge.list_peers()?,
        }))
    }

    async fn get_peer(
        &self,
        request: Request<GetPeerRequest>,
    ) -> Result<Response<PeerEntry>, Status> {
        let installation = self.installation()?;
        let request = request.into_inner();
        let (_, edge) = Self::edge(&installation, &request.profile_id)?;
        let peer = request
            .peer
            .ok_or_else(|| failed(ErrorCode::InvalidArgument, "name the peer to read"))?;
        Ok(Response::new(edge.peer_entry(peer)?))
    }

    async fn unpair(&self, request: Request<UnpairRequest>) -> Result<Response<PeerEntry>, Status> {
        let installation = self.installation()?;
        let request = request.into_inner();
        let (_, edge) = Self::edge(&installation, &request.profile_id)?;
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
        self.installation()?;
        Ok(Response::new(InstallationInfo {
            version: crate::version().to_owned(),
        }))
    }

    async fn shutdown(&self, _request: Request<Empty>) -> Result<Response<Empty>, Status> {
        self.installation()?.request_shutdown();
        Ok(Response::new(Empty {}))
    }
}
