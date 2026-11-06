//! The installation's profiles as a client manages them: the same calls
//! over the front door's socket ([`GrpcInstallation`]) or in process
//! ([`InProcessInstallation`]), as [`Client`](crate::Client) is for one
//! profile's agents. Every method is the `InstallationService` call of the
//! same name, except [`Installation::open_profile`], which reaches a
//! profile's own services.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use tonic::Request;
use wire::installation_service_client::InstallationServiceClient;
use wire::installation_service_server::InstallationService;
use wire::{
    AnswerSignInRequest, BindProfileRequest, CreateProfileRequest, DeleteProfileRequest, Profile,
    RenameProfileRequest, SignInEvent, SignInRequest, SubscribeProfilesResponse,
};

use crate::{Client, EventStream, GrpcClient, RpcError, events, held, held_events};

#[async_trait]
pub trait Installation: Send + Sync + 'static {
    async fn get_installation(&self) -> Result<wire::Installation, RpcError>;
    async fn list_profiles(&self) -> Result<Vec<Profile>, RpcError>;
    /// The whole list now, then again whenever any profile changes.
    async fn subscribe_profiles(&self) -> Result<EventStream<SubscribeProfilesResponse>, RpcError>;
    async fn create_profile(&self, request: CreateProfileRequest) -> Result<Profile, RpcError>;
    async fn bind_profile(&self, request: BindProfileRequest) -> Result<Profile, RpcError>;
    async fn rename_profile(&self, request: RenameProfileRequest) -> Result<Profile, RpcError>;
    async fn delete_profile(&self, request: DeleteProfileRequest) -> Result<(), RpcError>;
    async fn sign_in(&self, request: SignInRequest) -> Result<EventStream<SignInEvent>, RpcError>;
    async fn answer_sign_in(&self, request: AnswerSignInRequest) -> Result<(), RpcError>;
    /// The everyday settings, as the config file resolves them.
    async fn get_settings(&self) -> Result<wire::Settings, RpcError>;
    /// Changes the named settings and applies each at once.
    async fn update_settings(
        &self,
        request: wire::UpdateSettingsRequest,
    ) -> Result<wire::Settings, RpcError>;
    /// One profile's onboarding steps now, then again on every change.
    async fn subscribe_onboarding(
        &self,
        request: wire::SubscribeOnboardingRequest,
    ) -> Result<EventStream<wire::Onboarding>, RpcError>;
    async fn answer_onboarding(
        &self,
        request: wire::AnswerOnboardingRequest,
    ) -> Result<wire::Onboarding, RpcError>;
    /// Folders worth offering as repositories.
    async fn suggest_repositories(&self) -> Result<Vec<wire::RepositoryEntry>, RpcError>;
    /// A one-time link that signs the browser in and opens a page of the
    /// profile's account service.
    async fn web_sign_in_link(
        &self,
        request: wire::WebSignInLinkRequest,
    ) -> Result<String, RpcError>;
    /// The profile's own services: its agents and sessions, and its
    /// administration (signing out, its identity, its dump).
    async fn open_profile(&self, profile: &Profile) -> Result<Arc<dyn Client>, RpcError>;
}

/// `$call!(self, method, request)` makes the call and yields its
/// `Result<Response<_>, Status>`.
macro_rules! installation_impl {
    (impl[$($generics:tt)*] Installation for $ty:ty, $call:ident, $open:ident) => {
        #[async_trait]
        impl<$($generics)*> Installation for $ty {
            async fn get_installation(&self) -> Result<wire::Installation, RpcError> {
                held(
                    $call!(self, get_installation, wire::GetInstallationRequest {})
                        .await?
                        .into_inner()
                        .installation,
                )
            }

            async fn list_profiles(&self) -> Result<Vec<Profile>, RpcError> {
                Ok($call!(self, list_profiles, wire::ListProfilesRequest {})
                    .await?
                    .into_inner()
                    .profiles)
            }

            async fn subscribe_profiles(
                &self,
            ) -> Result<EventStream<SubscribeProfilesResponse>, RpcError> {
                Ok(events(
                    $call!(self, subscribe_profiles, wire::SubscribeProfilesRequest {})
                        .await?
                        .into_inner(),
                ))
            }

            async fn create_profile(
                &self,
                request: CreateProfileRequest,
            ) -> Result<Profile, RpcError> {
                held($call!(self, create_profile, request).await?.into_inner().profile)
            }

            async fn bind_profile(&self, request: BindProfileRequest) -> Result<Profile, RpcError> {
                held($call!(self, bind_profile, request).await?.into_inner().profile)
            }

            async fn rename_profile(
                &self,
                request: RenameProfileRequest,
            ) -> Result<Profile, RpcError> {
                held($call!(self, rename_profile, request).await?.into_inner().profile)
            }

            async fn delete_profile(&self, request: DeleteProfileRequest) -> Result<(), RpcError> {
                $call!(self, delete_profile, request).await?;
                Ok(())
            }

            async fn sign_in(
                &self,
                request: SignInRequest,
            ) -> Result<EventStream<SignInEvent>, RpcError> {
                Ok(held_events(
                    $call!(self, sign_in, request).await?.into_inner(),
                    |response| response.event,
                ))
            }

            async fn answer_sign_in(&self, request: AnswerSignInRequest) -> Result<(), RpcError> {
                $call!(self, answer_sign_in, request).await?;
                Ok(())
            }

            async fn get_settings(&self) -> Result<wire::Settings, RpcError> {
                held(
                    $call!(self, get_settings, wire::GetSettingsRequest {})
                        .await?
                        .into_inner()
                        .settings,
                )
            }

            async fn update_settings(
                &self,
                request: wire::UpdateSettingsRequest,
            ) -> Result<wire::Settings, RpcError> {
                held($call!(self, update_settings, request).await?.into_inner().settings)
            }

            async fn subscribe_onboarding(
                &self,
                request: wire::SubscribeOnboardingRequest,
            ) -> Result<EventStream<wire::Onboarding>, RpcError> {
                Ok(held_events(
                    $call!(self, subscribe_onboarding, request).await?.into_inner(),
                    |response| response.onboarding,
                ))
            }

            async fn answer_onboarding(
                &self,
                request: wire::AnswerOnboardingRequest,
            ) -> Result<wire::Onboarding, RpcError> {
                held($call!(self, answer_onboarding, request).await?.into_inner().onboarding)
            }

            async fn suggest_repositories(&self) -> Result<Vec<wire::RepositoryEntry>, RpcError> {
                Ok($call!(self, suggest_repositories, wire::SuggestRepositoriesRequest {})
                    .await?
                    .into_inner()
                    .suggestions)
            }

            async fn web_sign_in_link(
                &self,
                request: wire::WebSignInLinkRequest,
            ) -> Result<String, RpcError> {
                Ok($call!(self, web_sign_in_link, request).await?.into_inner().url)
            }

            async fn open_profile(&self, profile: &Profile) -> Result<Arc<dyn Client>, RpcError> {
                $open!(self, profile)
            }
        }
    };
}

/// The daemon's front door, over its local socket. Each profile it lists
/// names its own socket, which [`Installation::open_profile`] dials.
#[derive(Clone)]
pub struct GrpcInstallation {
    door: InstallationServiceClient<tonic::transport::Channel>,
    /// What each profile opened from here names its client as.
    surface: Option<&'static str>,
}

impl GrpcInstallation {
    /// Connects to the front door at `path` (`<root>/amux.sock`).
    pub async fn connect(path: &Path) -> Result<GrpcInstallation, RpcError> {
        Ok(GrpcInstallation {
            door: InstallationServiceClient::new(crate::local_channel(path).await?),
            surface: None,
        })
    }

    /// Names this client as `surface` on every call, and the client of
    /// every profile opened from here ([`GrpcClient::naming`]).
    pub fn naming(mut self, surface: &'static str) -> GrpcInstallation {
        self.surface = Some(surface);
        self
    }

    fn request<T>(&self, message: T) -> Request<T> {
        let mut request = Request::new(message);
        if let Some(surface) = self.surface {
            request.metadata_mut().insert(
                wire::SURFACE_HEADER,
                tonic::metadata::AsciiMetadataValue::from_static(surface),
            );
        }
        request
    }
}

macro_rules! grpc_call {
    ($installation:expr, $name:ident, $request:expr) => {
        $installation
            .door
            .clone()
            .$name($installation.request($request))
    };
}

macro_rules! grpc_open {
    ($installation:expr, $profile:expr) => {{
        let client: Arc<dyn Client> = Arc::new(
            GrpcClient::connect(Path::new(&$profile.socket_path))
                .await?
                .naming($installation.surface),
        );
        Ok(client)
    }};
}

installation_impl!(impl[] Installation for GrpcInstallation, grpc_call, grpc_open);

/// Opens a profile's services in process, by its id.
pub type OpenProfile = Arc<dyn Fn(&[u8]) -> Result<Arc<dyn Client>, RpcError> + Send + Sync>;

/// The installation hosted in this process, called directly: the phone's
/// embedded runtime. Errors take the shape they would over a socket.
#[derive(Clone)]
pub struct InProcessInstallation<S> {
    service: S,
    open: OpenProfile,
}

impl<S: InstallationService> InProcessInstallation<S> {
    pub fn new(service: S, open: OpenProfile) -> InProcessInstallation<S> {
        InProcessInstallation { service, open }
    }
}

macro_rules! in_process_call {
    ($installation:expr, $name:ident, $request:expr) => {
        InstallationService::$name(&$installation.service, Request::new($request))
    };
}

macro_rules! in_process_open {
    ($installation:expr, $profile:expr) => {
        ($installation.open)(&$profile.id)
    };
}

installation_impl!(
    impl[S: InstallationService] Installation for InProcessInstallation<S>,
    in_process_call,
    in_process_open
);
