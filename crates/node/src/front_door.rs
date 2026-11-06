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
    AnswerOnboardingRequest, AnswerOnboardingResponse, AnswerSignInRequest, AnswerSignInResponse,
    BindProfileRequest, BindProfileResponse, CreateProfileRequest, CreateProfileResponse,
    DeleteProfileRequest, DeleteProfileResponse, Empty, ErrorCode, GetInstallationRequest,
    GetInstallationResponse, GetSettingsRequest, GetSettingsResponse, ListProfilesRequest,
    ListProfilesResponse, RenameProfileRequest, RenameProfileResponse, ShutdownRequest,
    ShutdownResponse, SignInEvent, SignInRequest, SignInResponse, SubscribeOnboardingRequest,
    SubscribeOnboardingResponse, SubscribeProfilesRequest, SubscribeProfilesResponse,
    SuggestRepositoriesRequest, UpdateSettingsRequest, UpdateSettingsResponse, sign_in_event,
};

use crate::Tier;
use crate::auth::oauth::DeviceSignIn;
use crate::edge::{Cloud, Relay};
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
                Relay::Connected { tier, .. } => State::Connected(wire::RelayConnected {
                    tier: match tier {
                        Tier::Free => wire::Tier::Free,
                        Tier::Pro => wire::Tier::Pro,
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
    /// The client a call counts as when it does not name one.
    client: analytics::Client,
}

impl FrontDoor {
    pub(crate) fn new(registry: Registry) -> FrontDoor {
        FrontDoor {
            registry,
            asking: Arc::default(),
            client: analytics::Client::Terminal,
        }
    }

    /// Counts a call that names no client as `client`: the phone, which
    /// hosts this door in process.
    pub fn answering(mut self, client: analytics::Client) -> FrontDoor {
        self.client = client;
        self
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
    events: &mpsc::Sender<Result<SignInResponse, Status>>,
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
                let asked = sign_in_response(sign_in_event::Of::Adopt(wire::SignInAdopt {
                    sign_in_id: id.as_bytes().to_vec(),
                    profile: named,
                }));
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

fn sign_in_response(of: sign_in_event::Of) -> SignInResponse {
    SignInResponse {
        event: Some(SignInEvent { of: Some(of) }),
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
    type SubscribeProfilesStream = EventStream<SubscribeProfilesResponse>;
    type SignInStream = EventStream<SignInResponse>;
    type SubscribeOnboardingStream = EventStream<SubscribeOnboardingResponse>;

    async fn get_settings(
        &self,
        _request: Request<GetSettingsRequest>,
    ) -> Result<Response<GetSettingsResponse>, Status> {
        let installation = self.registry.installation()?;
        // A change made beside the daemon, by `amux config` or an editor,
        // is in force for the client asking now, not only at the next poll.
        let reread = installation.clone();
        tokio::task::spawn_blocking(move || {
            if let Some(config) = reread.settings.reread() {
                reread.apply(&config);
            }
        })
        .await
        .map_err(|error| failed(ErrorCode::Internal, error.to_string()))?;
        let settings = &installation.settings;
        Ok(Response::new(GetSettingsResponse {
            settings: Some(crate::host_settings::to_wire(
                &settings.current(),
                &installation.host_name(),
                settings.supervised(),
            )),
        }))
    }

    async fn update_settings(
        &self,
        request: Request<UpdateSettingsRequest>,
    ) -> Result<Response<UpdateSettingsResponse>, Status> {
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
        Ok(Response::new(UpdateSettingsResponse {
            settings: Some(crate::host_settings::to_wire(
                &config,
                &installation.host_name(),
                installation.settings.supervised(),
            )),
        }))
    }

    async fn subscribe_onboarding(
        &self,
        request: Request<SubscribeOnboardingRequest>,
    ) -> Result<Response<Self::SubscribeOnboardingStream>, Status> {
        let request = request.into_inner();
        let profile = (!request.profile_id.is_empty())
            .then(|| profile_id(&request.profile_id))
            .transpose()?;
        let steps = self.registry.subscribe_onboarding(profile)?;
        Ok(Response::new(Box::pin(futures_util::stream::unfold(
            steps,
            |mut steps| async move {
                let now = steps.recv().await?;
                let response = SubscribeOnboardingResponse {
                    onboarding: Some(now),
                };
                Some((Ok(response), steps))
            },
        ))))
    }

    async fn answer_onboarding(
        &self,
        request: Request<AnswerOnboardingRequest>,
    ) -> Result<Response<AnswerOnboardingResponse>, Status> {
        let registry = self.registry.clone();
        let surface = grpc::surface_named(request.metadata(), self.client);
        let request = request.into_inner();
        // The answer is written to disk.
        let answered =
            tokio::task::spawn_blocking(move || registry.answer_onboarding(&request, surface))
                .await
                .map_err(|error| failed(ErrorCode::Internal, error.to_string()))??;
        Ok(Response::new(AnswerOnboardingResponse {
            onboarding: Some(answered),
        }))
    }

    async fn suggest_repositories(
        &self,
        _request: Request<SuggestRepositoriesRequest>,
    ) -> Result<Response<wire::SuggestRepositoriesResponse>, Status> {
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
        Ok(Response::new(wire::SuggestRepositoriesResponse {
            suggestions,
        }))
    }

    async fn web_sign_in_link(
        &self,
        request: Request<wire::WebSignInLinkRequest>,
    ) -> Result<Response<wire::WebSignInLinkResponse>, Status> {
        let request = request.into_inner();
        let edge = self.registry.edge(profile_id(&request.profile_id)?)?;
        let url = edge.web_sign_in_link(&request.path).await?;
        Ok(Response::new(wire::WebSignInLinkResponse { url }))
    }

    async fn subscribe_profiles(
        &self,
        _request: Request<SubscribeProfilesRequest>,
    ) -> Result<Response<Self::SubscribeProfilesStream>, Status> {
        let mut moved = self.registry.subscribe()?;
        let registry = self.registry.clone();
        let (events, received) = mpsc::channel(4);
        tokio::spawn(async move {
            // The list is read after the watch is marked seen, so a change
            // between the two rings again rather than being lost.
            loop {
                moved.borrow_and_update();
                let listed = registry.list().map(|states| SubscribeProfilesResponse {
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
        let code = sign_in_response(sign_in_event::Of::Code(wire::SignInCode {
            verification_uri_complete: device
                .verification_uri_complete
                .clone()
                .unwrap_or_else(|| device.verification_uri.clone()),
            verification_uri: device.verification_uri.clone(),
            user_code: device.user_code.clone(),
            expires_at_ms: ms_since_epoch(device.expires_at),
        }));
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
                Ok(Some(profile)) => Ok(sign_in_response(sign_in_event::Of::SignedIn(profile))),
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
    ) -> Result<Response<AnswerSignInResponse>, Status> {
        let request = request.into_inner();
        let id = Uuid::from_slice(&request.sign_in_id)
            .map_err(|_| failed(ErrorCode::InvalidArgument, "a sign-in id is 16 bytes"))?;
        let answer =
            self.asking.lock().unwrap().remove(&id).ok_or_else(|| {
                failed(ErrorCode::NotFound, "no sign-in is waiting for that answer")
            })?;
        let _ = answer.send(request.adopt);
        Ok(Response::new(AnswerSignInResponse {}))
    }

    async fn list_profiles(
        &self,
        _request: Request<ListProfilesRequest>,
    ) -> Result<Response<ListProfilesResponse>, Status> {
        Ok(Response::new(ListProfilesResponse {
            profiles: self.registry.list()?.into_iter().map(info).collect(),
        }))
    }

    async fn create_profile(
        &self,
        request: Request<CreateProfileRequest>,
    ) -> Result<Response<CreateProfileResponse>, Status> {
        let label = request.into_inner().label;
        Ok(Response::new(CreateProfileResponse {
            profile: Some(info(self.registry.create(label.as_deref()).await?)),
        }))
    }

    async fn rename_profile(
        &self,
        request: Request<RenameProfileRequest>,
    ) -> Result<Response<RenameProfileResponse>, Status> {
        let request = request.into_inner();
        let id = profile_id(&request.profile_id)?;
        Ok(Response::new(RenameProfileResponse {
            profile: Some(info(self.registry.rename(id, &request.name).await?)),
        }))
    }

    async fn delete_profile(
        &self,
        request: Request<DeleteProfileRequest>,
    ) -> Result<Response<DeleteProfileResponse>, Status> {
        let id = profile_id(&request.into_inner().profile_id)?;
        self.registry.delete(id).await?;
        Ok(Response::new(DeleteProfileResponse {}))
    }

    async fn bind_profile(
        &self,
        request: Request<BindProfileRequest>,
    ) -> Result<Response<BindProfileResponse>, Status> {
        let request = request.into_inner();
        let sign_in = SignIn {
            profile: request.profile_id.as_deref().map(profile_id).transpose()?,
            cloud_url: request.cloud_url,
            refresh_token: request.staged_refresh_token,
            client_id: request.client_id,
            adopt: request.adopt_non_pristine,
        };
        Ok(Response::new(BindProfileResponse {
            profile: Some(info(self.registry.bind(sign_in).await?)),
        }))
    }

    async fn get_installation(
        &self,
        _request: Request<GetInstallationRequest>,
    ) -> Result<Response<GetInstallationResponse>, Status> {
        self.registry.installation()?;
        Ok(Response::new(GetInstallationResponse {
            installation: Some(wire::Installation {
                version: crate::version().to_owned(),
            }),
        }))
    }

    async fn shutdown(
        &self,
        _request: Request<ShutdownRequest>,
    ) -> Result<Response<ShutdownResponse>, Status> {
        self.registry.installation()?.request_shutdown();
        Ok(Response::new(ShutdownResponse {}))
    }
}
