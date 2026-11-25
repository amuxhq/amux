//! The installation's profiles as a client manages them, through its
//! front door: over the front door's socket, or called in memory where the
//! installation runs in the client's own process. Opening a profile gives a
//! [`Client`] of that profile's node.

use std::path::Path;
use std::sync::Arc;

use tonic::body::Body;
use tonic::codegen::{StdError, http};
use wire::installation_service_client::InstallationServiceClient;
use wire::{
    AnswerSignInRequest, BindProfileRequest, CreateProfileRequest, DeleteProfileRequest, Profile,
    RenameProfileRequest, SignInEvent, SignInRequest, SubscribeProfilesResponse,
};

use crate::transport::{Address, Memory, Socket, Transport};
use crate::{Client, Conn, EventStream, RpcError, Surface, events, held, held_events};

/// Opens a profile hosted in this process by its id.
type Open = Arc<dyn Fn(&[u8]) -> Result<Client, RpcError> + Send + Sync>;

#[derive(Clone)]
pub struct Installation {
    door: Conn,
    surface: Surface,
    /// None where each profile names the socket a client dials.
    open: Option<Open>,
}

impl Installation {
    /// The front door at `path` (`<root>/amux.sock`). Each profile it lists
    /// names its own socket, which [`Installation::open_profile`] dials.
    pub async fn connect(path: &Path, surface: Surface) -> Result<Installation, RpcError> {
        let door = Socket::connect(path).await?;
        Ok(Installation {
            door: Conn::on_node(door.node(), surface),
            surface,
            open: None,
        })
    }

    /// The front door's service called in memory, with `open` making a
    /// client of a hosted profile from its id.
    pub fn in_memory<S>(
        service: S,
        surface: Surface,
        open: impl Fn(&[u8]) -> Result<Client, RpcError> + Send + Sync + 'static,
    ) -> Installation
    where
        S: tower::Service<http::Request<Body>, Response = http::Response<Body>>
            + Clone
            + Send
            + 'static,
        S::Error: Into<StdError>,
        S::Future: Send + 'static,
    {
        Installation {
            door: Transport::Memory(Memory::new(service)).conn(Address::Node, surface),
            surface,
            open: Some(Arc::new(open)),
        }
    }

    fn stub(&self) -> InstallationServiceClient<Conn> {
        crate::stub!(
            InstallationServiceClient<Conn>,
            (self.door.clone(), self.door.origin())
        )
    }

    /// The profile's own services, as a client of its node.
    pub async fn open_profile(&self, profile: &Profile) -> Result<Client, RpcError> {
        match &self.open {
            Some(open) => open(&profile.id),
            None => Client::connect(Path::new(&profile.socket_path), self.surface).await,
        }
    }

    pub async fn get_installation(&self) -> Result<wire::Installation, RpcError> {
        let request = wire::GetInstallationRequest {};
        held(
            self.stub()
                .get_installation(request)
                .await?
                .into_inner()
                .installation,
        )
    }

    pub async fn list_profiles(&self) -> Result<Vec<Profile>, RpcError> {
        let request = wire::ListProfilesRequest {};
        Ok(self
            .stub()
            .list_profiles(request)
            .await?
            .into_inner()
            .profiles)
    }

    /// The whole list now, then again whenever any profile changes.
    pub async fn subscribe_profiles(
        &self,
    ) -> Result<EventStream<SubscribeProfilesResponse>, RpcError> {
        let request = wire::SubscribeProfilesRequest {};
        Ok(events(
            self.stub().subscribe_profiles(request).await?.into_inner(),
        ))
    }

    pub async fn create_profile(&self, request: CreateProfileRequest) -> Result<Profile, RpcError> {
        held(
            self.stub()
                .create_profile(request)
                .await?
                .into_inner()
                .profile,
        )
    }

    pub async fn bind_profile(&self, request: BindProfileRequest) -> Result<Profile, RpcError> {
        held(
            self.stub()
                .bind_profile(request)
                .await?
                .into_inner()
                .profile,
        )
    }

    pub async fn rename_profile(&self, request: RenameProfileRequest) -> Result<Profile, RpcError> {
        held(
            self.stub()
                .rename_profile(request)
                .await?
                .into_inner()
                .profile,
        )
    }

    pub async fn delete_profile(&self, request: DeleteProfileRequest) -> Result<(), RpcError> {
        self.stub().delete_profile(request).await?;
        Ok(())
    }

    pub async fn sign_in(
        &self,
        request: SignInRequest,
    ) -> Result<EventStream<SignInEvent>, RpcError> {
        let stream = self.stub().sign_in(request).await?.into_inner();
        Ok(held_events(stream, |response| response.event))
    }

    pub async fn answer_sign_in(&self, request: AnswerSignInRequest) -> Result<(), RpcError> {
        self.stub().answer_sign_in(request).await?;
        Ok(())
    }

    /// The everyday settings, as the config file resolves them.
    pub async fn get_settings(&self) -> Result<wire::Settings, RpcError> {
        let request = wire::GetSettingsRequest {};
        held(
            self.stub()
                .get_settings(request)
                .await?
                .into_inner()
                .settings,
        )
    }

    /// Changes the named settings and applies each at once.
    pub async fn update_settings(
        &self,
        request: wire::UpdateSettingsRequest,
    ) -> Result<wire::Settings, RpcError> {
        held(
            self.stub()
                .update_settings(request)
                .await?
                .into_inner()
                .settings,
        )
    }

    /// One profile's onboarding steps now, then again on every change.
    pub async fn subscribe_onboarding(
        &self,
        request: wire::SubscribeOnboardingRequest,
    ) -> Result<EventStream<wire::Onboarding>, RpcError> {
        let stream = self
            .stub()
            .subscribe_onboarding(request)
            .await?
            .into_inner();
        Ok(held_events(stream, |response| response.onboarding))
    }

    pub async fn answer_onboarding(
        &self,
        request: wire::AnswerOnboardingRequest,
    ) -> Result<wire::Onboarding, RpcError> {
        held(
            self.stub()
                .answer_onboarding(request)
                .await?
                .into_inner()
                .onboarding,
        )
    }

    /// A one-time link that signs the browser in and opens a page of the
    /// profile's account service.
    pub async fn web_sign_in_link(
        &self,
        request: wire::WebSignInLinkRequest,
    ) -> Result<String, RpcError> {
        Ok(self
            .stub()
            .web_sign_in_link(request)
            .await?
            .into_inner()
            .url)
    }
}
