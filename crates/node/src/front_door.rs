//! The front door: the installation's own socket, where a client lists the
//! profiles, finds each one's client socket, creates, names, deletes and
//! signs them in, and manages the installation.

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::UNIX_EPOCH;

use agent_dir::local_socket::LocalListener;
use futures_util::Stream;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tonic::{Request, Response, Status};
use uuid::Uuid;
use wire::installation_service_server::{InstallationService, InstallationServiceServer};
use wire::{
    AnswerSignInRequest, BindProfileRequest, CreateProfileRequest, DeleteProfileRequest, Empty,
    ErrorCode, ListProfilesResponse, RefreshProfileRequest, RenameProfileRequest, SignInEvent,
    SignInRequest, sign_in_event,
};

use crate::Tier;
use crate::auth::oauth::DeviceSignIn;
use crate::edge::{Cloud, Relay, RelayCarrier};
use crate::grpc::{self, status, wire_error};
use crate::registry::{ProfileRefusal, ProfileState, Registry, SignIn};

/// A profile as the front door describes it.
pub fn info(state: ProfileState) -> wire::Profile {
    use wire::cloud_standing::Standing;
    use wire::relay_link::State;
    let standing = match &state.cloud {
        Cloud::Unbound => Standing::Unbound(Empty {}),
        Cloud::SignedOut => Standing::SignedOut(Empty {}),
        Cloud::SignedIn(relay) => Standing::SignedIn(wire::RelayLink {
            state: Some(match relay {
                Relay::Connecting => State::Connecting(Empty {}),
                Relay::Connected { tier, carrier } => State::Connected(wire::RelayConnected {
                    tier: match tier {
                        Tier::Free => wire::Tier::Free,
                        Tier::Pro => wire::Tier::Pro,
                    } as i32,
                    carrier: match carrier {
                        RelayCarrier::Quic => wire::RelayCarrier::Quic,
                        RelayCarrier::Tcp => wire::RelayCarrier::Tcp,
                    } as i32,
                }),
                Relay::Retrying => State::Retrying(Empty {}),
                Relay::SignInAgain => State::SignInAgain(Empty {}),
                Relay::VersionMismatch => State::VersionMismatch(Empty {}),
            }),
        }),
    };
    let account = state.account.unwrap_or_default();
    wire::Profile {
        email: account.email.unwrap_or_default(),
        account_name: account.name.unwrap_or_default(),
        picture_url: account.picture.unwrap_or_default(),
        account_subject: account.subject,
        access: state.access.map(access),
        cloud: Some(wire::CloudStanding {
            standing: Some(standing),
        }),
        id: state.id.as_bytes().to_vec(),
        label: state.label,
        socket_path: state.socket.to_string_lossy().into_owned(),
    }
}

fn access(access: crate::AccountAccess) -> wire::Access {
    use wire::access::Grant;
    wire::Access {
        pro: access.pro,
        until_ms: access.until_ms,
        grant: access.grant.map(|grant| match grant {
            crate::Grant::Granted => Grant::Granted(Empty {}),
            crate::Grant::Purchased {
                store,
                will_renew,
                renews_at_ms,
            } => Grant::Purchased(wire::Purchase {
                store: match store {
                    crate::PurchaseStore::AppStore => wire::PurchaseStore::AppStore,
                    crate::PurchaseStore::Web => wire::PurchaseStore::Web,
                } as i32,
                will_renew,
                renews_at_ms,
            }),
        }),
    }
}

/// Serves the profile and installation services on the front door until
/// the task is aborted.
pub(crate) fn serve(listener: LocalListener, registry: Registry) -> JoinHandle<()> {
    let door = FrontDoor::new(registry);
    tokio::spawn(async move {
        let served = tonic::transport::Server::builder()
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
    /// Sign-ins waiting for their client to say whether to adopt a profile
    /// that holds data.
    asking: Arc<Mutex<HashMap<Uuid, oneshot::Sender<bool>>>>,
}

impl FrontDoor {
    pub(crate) fn new(registry: Registry) -> FrontDoor {
        FrontDoor {
            registry,
            asking: Arc::default(),
        }
    }
}

type EventStream<T> = Pin<Box<dyn Stream<Item = Result<T, Status>> + Send>>;

/// A response stream of what arrives on `received`, ending when its
/// sender does.
fn receiving<T: Send + 'static>(received: mpsc::Receiver<Result<T, Status>>) -> EventStream<T> {
    Box::pin(futures_util::stream::unfold(
        received,
        |mut received| async move { received.recv().await.map(|item| (item, received)) },
    ))
}

fn ms_since_epoch(time: std::time::SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH).map_or(0, |since| {
        i64::try_from(since.as_millis()).unwrap_or(i64::MAX)
    })
}

/// A device sign-in run to its end: the approved code's token bound to the
/// profile, asking the client first when the profile holds data. `None`
/// when the client went away or declined.
async fn finish_sign_in(
    registry: Registry,
    asking: Arc<Mutex<HashMap<Uuid, oneshot::Sender<bool>>>>,
    events: &mpsc::Sender<Result<SignInEvent, Status>>,
    device: DeviceSignIn,
    request: SignInRequest,
    profile: Option<Uuid>,
    cloud_url: String,
) -> Result<Option<wire::Profile>, Status> {
    let refresh = tokio::select! {
        refresh = device.finish() => refresh
            .map_err(|error| failed(ErrorCode::Unauthenticated, error.to_string()))?,
        () = events.closed() => return Ok(None),
    };
    let mut adopt = request.adopt_non_pristine;
    loop {
        let sign_in = SignIn {
            profile,
            cloud_url: cloud_url.clone(),
            refresh_token: refresh.clone(),
            client_id: String::new(),
            adopt,
        };
        match registry.bind(sign_in).await {
            Ok(state) => return Ok(Some(info(state))),
            Err(ProfileRefusal::HoldsData) if !adopt => {
                let id = Uuid::new_v4();
                let (answer, answered) = oneshot::channel();
                asking.lock().unwrap().insert(id, answer);
                let named = registry
                    .list()?
                    .into_iter()
                    .find(|state| Some(state.id) == profile)
                    .map(info);
                let asked = SignInEvent {
                    of: Some(sign_in_event::Of::Adopt(wire::SignInAdopt {
                        sign_in_id: id.as_bytes().to_vec(),
                        profile: named,
                    })),
                };
                let yes = events.send(Ok(asked)).await.is_ok()
                    && tokio::select! {
                        yes = answered => yes.unwrap_or(false),
                        () = events.closed() => false,
                    };
                asking.lock().unwrap().remove(&id);
                if !yes {
                    return Ok(None);
                }
                adopt = true;
            }
            Err(refusal) => return Err(refusal.into()),
        }
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

#[tonic::async_trait]
impl InstallationService for FrontDoor {
    type SubscribeProfilesStream = EventStream<ListProfilesResponse>;
    type SignInStream = EventStream<SignInEvent>;
    type SubscribeOnboardingStream = EventStream<wire::Onboarding>;

    async fn get_settings(
        &self,
        _request: Request<Empty>,
    ) -> Result<Response<wire::Settings>, Status> {
        let installation = self.registry.installation()?;
        let settings = &installation.settings;
        Ok(Response::new(crate::host_settings::to_wire(
            &settings.current(),
            &installation.host_name(),
            settings.supervised(),
        )))
    }

    async fn update_settings(
        &self,
        request: Request<wire::UpdateSettingsRequest>,
    ) -> Result<Response<wire::Settings>, Status> {
        let installation = self.registry.installation()?;
        let request = request.into_inner();
        let config = {
            let installation = installation.clone();
            tokio::task::spawn_blocking(move || installation.settings.update(&request))
                .await
                .map_err(|error| failed(ErrorCode::Internal, error.to_string()))?
                .map_err(|error| status(error.to_wire()))?
        };
        installation.apply(&config);
        Ok(Response::new(crate::host_settings::to_wire(
            &config,
            &installation.host_name(),
            installation.settings.supervised(),
        )))
    }

    async fn subscribe_onboarding(
        &self,
        request: Request<wire::OnboardingRequest>,
    ) -> Result<Response<Self::SubscribeOnboardingStream>, Status> {
        let request = request.into_inner();
        let profile = (!request.profile_id.is_empty())
            .then(|| profile_id(&request.profile_id))
            .transpose()?;
        let steps = self.registry.subscribe_onboarding(profile)?;
        Ok(Response::new(Box::pin(futures_util::stream::unfold(
            steps,
            |mut steps| async move { steps.recv().await.map(|now| (Ok(now), steps)) },
        ))))
    }

    async fn answer_onboarding(
        &self,
        request: Request<wire::AnswerOnboardingRequest>,
    ) -> Result<Response<wire::Onboarding>, Status> {
        let registry = self.registry.clone();
        let request = request.into_inner();
        // The answer is written to disk.
        let answered = tokio::task::spawn_blocking(move || registry.answer_onboarding(&request))
            .await
            .map_err(|error| failed(ErrorCode::Internal, error.to_string()))??;
        Ok(Response::new(answered))
    }

    async fn suggest_projects(
        &self,
        _request: Request<Empty>,
    ) -> Result<Response<wire::SuggestProjectsResponse>, Status> {
        let installation = self.registry.installation()?;
        let suggestions = tokio::task::spawn_blocking(move || {
            let suggestions = installation
                .settings
                .home()
                .map(|home| crate::suggestions::suggest(home, std::time::SystemTime::now()))
                .unwrap_or_default();
            installation.onboarding.suggested(suggestions.len());
            suggestions
        })
        .await
        .map_err(|error| failed(ErrorCode::Internal, error.to_string()))?;
        Ok(Response::new(wire::SuggestProjectsResponse { suggestions }))
    }

    async fn web_sign_in_link(
        &self,
        _request: Request<wire::WebSignInLinkRequest>,
    ) -> Result<Response<wire::WebSignInLinkResponse>, Status> {
        Err(Status::unimplemented("web sign-in links"))
    }

    async fn subscribe_profiles(
        &self,
        _request: Request<wire::Empty>,
    ) -> Result<Response<Self::SubscribeProfilesStream>, Status> {
        let mut moved = self.registry.subscribe()?;
        let registry = self.registry.clone();
        let (events, received) = mpsc::channel(4);
        tokio::spawn(async move {
            // The list is read after the watch is marked seen, so a change
            // between the two rings again rather than being lost.
            loop {
                moved.borrow_and_update();
                let listed = registry.list().map(|states| ListProfilesResponse {
                    profiles: states.into_iter().map(info).collect(),
                });
                let stopping = listed.is_err();
                if events.send(listed.map_err(Status::from)).await.is_err() || stopping {
                    return;
                }
                tokio::select! {
                    changed = moved.changed() => if changed.is_err() { return },
                    () = events.closed() => return,
                }
            }
        });
        Ok(Response::new(receiving(received)))
    }

    async fn sign_in(
        &self,
        request: Request<SignInRequest>,
    ) -> Result<Response<Self::SignInStream>, Status> {
        let request = request.into_inner();
        let profile = request.profile_id.as_deref().map(profile_id).transpose()?;
        if let Some(id) = profile
            && !self.registry.list()?.iter().any(|state| state.id == id)
        {
            return Err(ProfileRefusal::NoProfile(id).into());
        }
        let cloud_url = match request.cloud_url.trim() {
            "" => settings::DEFAULT_CLOUD_URL.to_owned(),
            url => url.trim_end_matches('/').to_owned(),
        };
        let device = DeviceSignIn::start(&cloud_url, &self.registry.host_name()?)
            .await
            .map_err(|error| failed(ErrorCode::Unavailable, error.to_string()))?;
        let code = SignInEvent {
            of: Some(sign_in_event::Of::Code(wire::SignInCode {
                verification_uri_complete: device
                    .verification_uri_complete
                    .clone()
                    .unwrap_or_else(|| device.verification_uri.clone()),
                verification_uri: device.verification_uri.clone(),
                user_code: device.user_code.clone(),
                expires_at_ms: ms_since_epoch(device.expires_at),
            })),
        };
        let (events, received) = mpsc::channel(4);
        events
            .send(Ok(code))
            .await
            .expect("the receiver is held here");
        let (registry, asking) = (self.registry.clone(), self.asking.clone());
        tokio::spawn(async move {
            let finished = finish_sign_in(
                registry, asking, &events, device, request, profile, cloud_url,
            )
            .await;
            let last = match finished {
                Ok(Some(profile)) => Ok(SignInEvent {
                    of: Some(sign_in_event::Of::SignedIn(profile)),
                }),
                Ok(None) => Err(failed(ErrorCode::Cancelled, "not signed in")),
                Err(status) => Err(status),
            };
            let _ = events.send(last).await;
        });
        Ok(Response::new(receiving(received)))
    }

    async fn answer_sign_in(
        &self,
        request: Request<AnswerSignInRequest>,
    ) -> Result<Response<Empty>, Status> {
        let request = request.into_inner();
        let id = Uuid::from_slice(&request.sign_in_id)
            .map_err(|_| failed(ErrorCode::InvalidArgument, "a sign-in id is 16 bytes"))?;
        let answer =
            self.asking.lock().unwrap().remove(&id).ok_or_else(|| {
                failed(ErrorCode::NotFound, "no sign-in is waiting for that answer")
            })?;
        let _ = answer.send(request.adopt);
        Ok(Response::new(Empty {}))
    }

    async fn refresh_profile(
        &self,
        request: Request<RefreshProfileRequest>,
    ) -> Result<Response<wire::Profile>, Status> {
        let id = profile_id(&request.into_inner().profile_id)?;
        Ok(Response::new(info(self.registry.refresh(id).await?)))
    }

    async fn list_profiles(
        &self,
        _request: Request<wire::Empty>,
    ) -> Result<Response<ListProfilesResponse>, Status> {
        Ok(Response::new(ListProfilesResponse {
            profiles: self.registry.list()?.into_iter().map(info).collect(),
        }))
    }

    async fn create_profile(
        &self,
        request: Request<CreateProfileRequest>,
    ) -> Result<Response<wire::Profile>, Status> {
        let label = request.into_inner().label;
        Ok(Response::new(info(
            self.registry.create(label.as_deref()).await?,
        )))
    }

    async fn rename_profile(
        &self,
        request: Request<RenameProfileRequest>,
    ) -> Result<Response<wire::Profile>, Status> {
        let request = request.into_inner();
        let id = profile_id(&request.profile_id)?;
        Ok(Response::new(info(
            self.registry.rename(id, &request.name).await?,
        )))
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
    ) -> Result<Response<wire::Profile>, Status> {
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

    async fn get_installation(
        &self,
        _request: Request<wire::Empty>,
    ) -> Result<Response<wire::Installation>, Status> {
        self.registry.installation()?;
        Ok(Response::new(wire::Installation {
            version: crate::version().to_owned(),
        }))
    }

    async fn shutdown(
        &self,
        _request: Request<wire::Empty>,
    ) -> Result<Response<wire::Empty>, Status> {
        self.registry.installation()?.request_shutdown();
        Ok(Response::new(wire::Empty {}))
    }
}
