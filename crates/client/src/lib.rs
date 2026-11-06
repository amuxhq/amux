//! The one seam both amux clients call their local runtime through.
//!
//! A client never opens a store and never talks to another host: it calls
//! the profile's client service on its own machine, which answers from its
//! rows and reaches origins itself. On the desktop that service is the
//! daemon behind the profile's local socket ([`GrpcClient`]); on the phone
//! it is the same runtime hosted in process ([`InProcess`]). Both implement
//! [`Client`], so the session and fleet drivers above them are written once.

use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;

mod installation;
pub use agent_dir::{Clock, ManualClock, SystemClock};
use async_trait::async_trait;
use futures_util::{Stream, StreamExt as _};
pub use installation::{GrpcInstallation, InProcessInstallation, Installation, OpenProfile};
use prost::Message as _;
use tonic::{Code, Request, Status};
use wire::agent_service_server::AgentService;
use wire::profile_admin_service_client::ProfileAdminServiceClient;
use wire::profile_admin_service_server::ProfileAdminService;
use wire::session_service_server::SessionService;
use wire::{
    Agent, BlobRef, CreateAgentRequest, DeleteAgentRequest, Diff, DumpRequest, DumpResponse,
    Envelope, ErrorCode, FileAgentRequest, GetBlobRequest, GetBlobResponse, GetDiffRequest,
    GetItemRequest, InventoryEvent, Item, ListItemsRequest, ListItemsResponse,
    ListRepositoriesRequest, ListRepositoriesResponse, PutBlobRequest, RenameAgentRequest,
    ResolveAgentRequest, ResumeAgentRequest, SendInputRequest, SendInputResponse,
    SendMessageRequest, SendMessageResponse, SessionEvent, SubscribeInventoryRequest,
    SubscribeSessionRequest,
};

/// A server stream. An `Err` item or the end of the stream both mean the
/// stream is over; only the first says the transport failed.
pub type EventStream<T> = Pin<Box<dyn Stream<Item = Result<T, RpcError>> + Send>>;

/// Why a call did not return its answer.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum RpcError {
    /// The runtime could not be reached, or the connection failed before
    /// the answer arrived: whether the call took effect is unknown.
    #[error("the local runtime is not answering: {0}")]
    Transport(String),
    /// The runtime answered, with an error.
    #[error("{}", .0.message)]
    Refused(wire::Error),
}

impl RpcError {
    /// The error's code, when the runtime answered.
    pub fn code(&self) -> Option<ErrorCode> {
        match self {
            RpcError::Transport(_) => None,
            RpcError::Refused(error) => Some(error.code()),
        }
    }

    /// The amux reason the runtime gave, when it gave one.
    pub fn reason(&self) -> Option<wire::ErrorReason> {
        match self {
            RpcError::Transport(_) => None,
            RpcError::Refused(error) => error.reason(),
        }
    }

    pub fn is_transport(&self) -> bool {
        matches!(self, RpcError::Transport(_))
    }
}

impl From<Status> for RpcError {
    /// The runtime puts its whole error in the status details; a status
    /// without one came from the transport, or from a service that is no
    /// longer running, and says nothing about whether the call landed.
    fn from(status: Status) -> RpcError {
        if let Ok(error) = wire::Error::decode(status.details())
            && error.code != ErrorCode::Ok as i32
        {
            return RpcError::Refused(error);
        }
        match status.code() {
            Code::Unavailable
            | Code::Unknown
            | Code::Cancelled
            | Code::DeadlineExceeded
            | Code::Internal
            | Code::Aborted
            | Code::Ok => RpcError::Transport(status.message().to_owned()),
            Code::Unimplemented if status.message().is_empty() => RpcError::Refused(
                wire::Error::new(ErrorCode::Unimplemented, wire::HOST_NEEDS_UPDATE),
            ),
            // tonic's codes are gRPC's, as an error's are.
            code => RpcError::Refused(wire::Error::new(
                ErrorCode::try_from(code as i32).unwrap_or(ErrorCode::Unknown),
                status.message(),
            )),
        }
    }
}

/// The client service as a client calls it. Every method is the service
/// call of the same name.
#[async_trait]
pub trait Client: Send + Sync + 'static {
    async fn subscribe_inventory(&self) -> Result<EventStream<InventoryEvent>, RpcError>;
    async fn resolve_agent(&self, request: ResolveAgentRequest) -> Result<Agent, RpcError>;
    async fn subscribe_session(
        &self,
        request: SubscribeSessionRequest,
    ) -> Result<EventStream<SessionEvent>, RpcError>;
    async fn list_items(&self, request: ListItemsRequest) -> Result<ListItemsResponse, RpcError>;
    async fn get_item(&self, request: GetItemRequest) -> Result<Item, RpcError>;
    async fn send_input(&self, request: SendInputRequest) -> Result<SendInputResponse, RpcError>;
    async fn create_agent(&self, request: CreateAgentRequest) -> Result<Agent, RpcError>;
    async fn rename_agent(&self, request: RenameAgentRequest) -> Result<Agent, RpcError>;
    async fn file_agent(&self, request: FileAgentRequest) -> Result<Agent, RpcError>;
    async fn resume_agent(&self, request: ResumeAgentRequest) -> Result<Agent, RpcError>;
    async fn delete_agent(&self, request: DeleteAgentRequest) -> Result<(), RpcError>;
    async fn send_message(&self, request: Envelope) -> Result<SendMessageResponse, RpcError>;
    async fn put_blob(&self, request: PutBlobRequest) -> Result<BlobRef, RpcError>;
    async fn get_blob(&self, request: GetBlobRequest) -> Result<GetBlobResponse, RpcError>;
    async fn get_diff(&self, request: GetDiffRequest) -> Result<Diff, RpcError>;
    async fn get_catalogue(
        &self,
        request: wire::GetCatalogueRequest,
    ) -> Result<wire::GetCatalogueResponse, RpcError>;
    async fn list_repositories(
        &self,
        request: ListRepositoriesRequest,
    ) -> Result<ListRepositoriesResponse, RpcError>;
    async fn dump(&self, request: DumpRequest) -> Result<DumpResponse, RpcError>;
    /// Writes a person's report around a dump and sends it on when the
    /// profile is signed in.
    async fn submit_report(
        &self,
        request: wire::SubmitReportRequest,
    ) -> Result<wire::SubmitReportResponse, RpcError>;
    /// Keeps an agent's unsent draft on this machine; an empty one forgets
    /// it.
    async fn update_draft(
        &self,
        request: wire::UpdateDraftRequest,
    ) -> Result<wire::Draft, RpcError>;
    async fn get_draft(&self, request: wire::GetDraftRequest) -> Result<wire::Draft, RpcError>;
    /// Signs the profile out: its relay link goes down and its refresh
    /// token is forgotten, but it stays tied to its account.
    async fn sign_out(&self) -> Result<(), RpcError>;
    /// This profile's identity as its peers pin it.
    async fn get_device_identity(&self) -> Result<wire::DeviceIdentity, RpcError>;
    /// Opens a pairing window on this profile.
    async fn start_pairing(
        &self,
        request: wire::StartPairingRequest,
    ) -> Result<wire::PairingWindow, RpcError> {
        let _ = request;
        Err(not_served("start_pairing"))
    }
    /// Closes the profile's pairing window, if one is open.
    async fn cancel_pairing(&self) -> Result<(), RpcError> {
        Err(not_served("cancel_pairing"))
    }
    /// The profile's pairing window, now and on every change.
    async fn subscribe_pairing(&self) -> Result<EventStream<wire::PairingStatus>, RpcError> {
        Err(not_served("subscribe_pairing"))
    }
    /// Offers the paired host `host_id` this profile's account.
    async fn offer_sign_in(&self, host_id: Vec<u8>) -> Result<(), RpcError> {
        let _ = host_id;
        Err(not_served("offer_sign_in"))
    }
    /// The offers paired hosts made this profile, now and on every change.
    async fn subscribe_sign_in_offers(
        &self,
    ) -> Result<EventStream<wire::SubscribeSignInOffersResponse>, RpcError> {
        Err(not_served("subscribe_sign_in_offers"))
    }
    /// Accepts an offer, which signs this profile in, or dismisses it.
    async fn answer_sign_in_offer(
        &self,
        request: wire::AnswerSignInOfferRequest,
    ) -> Result<wire::Profile, RpcError> {
        let _ = request;
        Err(not_served("answer_sign_in_offer"))
    }
}

/// A call an implementation does not serve, as the runtime would refuse it.
fn not_served(call: &str) -> RpcError {
    RpcError::Refused(wire::Error::new(
        ErrorCode::Unimplemented,
        format!("{call} is not served here"),
    ))
}

/// One implementation of [`Client`] per way of reaching the services:
/// `$call!(self, service, method, request)` makes the call on `agents`,
/// `sessions` or `admin` and yields its `Result<Response<_>, Status>`.
macro_rules! client_impl {
    (impl[$($generics:tt)*] Client for $ty:ty, $call:ident) => {
        #[async_trait]
        impl<$($generics)*> Client for $ty {
            async fn subscribe_inventory(&self) -> Result<EventStream<InventoryEvent>, RpcError> {
                Ok(held_events(
                    $call!(self, agents, subscribe_inventory, SubscribeInventoryRequest {})
                        .await?
                        .into_inner(),
                    |response| response.event,
                ))
            }

            async fn resolve_agent(&self, request: ResolveAgentRequest) -> Result<Agent, RpcError> {
                held($call!(self, agents, resolve_agent, request).await?.into_inner().agent)
            }

            async fn subscribe_session(
                &self,
                request: SubscribeSessionRequest,
            ) -> Result<EventStream<SessionEvent>, RpcError> {
                Ok(held_events(
                    $call!(self, sessions, subscribe_session, request).await?.into_inner(),
                    |response| response.event,
                ))
            }

            async fn list_items(&self, request: ListItemsRequest) -> Result<ListItemsResponse, RpcError> {
                Ok($call!(self, sessions, list_items, request).await?.into_inner())
            }

            async fn get_item(&self, request: GetItemRequest) -> Result<Item, RpcError> {
                held($call!(self, sessions, get_item, request).await?.into_inner().item)
            }

            async fn send_input(
                &self,
                request: SendInputRequest,
            ) -> Result<SendInputResponse, RpcError> {
                Ok($call!(self, sessions, send_input, request).await?.into_inner())
            }

            async fn create_agent(&self, request: CreateAgentRequest) -> Result<Agent, RpcError> {
                held($call!(self, agents, create_agent, request).await?.into_inner().agent)
            }

            async fn rename_agent(&self, request: RenameAgentRequest) -> Result<Agent, RpcError> {
                held($call!(self, agents, rename_agent, request).await?.into_inner().agent)
            }

            async fn file_agent(&self, request: FileAgentRequest) -> Result<Agent, RpcError> {
                held($call!(self, agents, file_agent, request).await?.into_inner().agent)
            }

            async fn resume_agent(&self, request: ResumeAgentRequest) -> Result<Agent, RpcError> {
                held($call!(self, agents, resume_agent, request).await?.into_inner().agent)
            }

            async fn delete_agent(&self, request: DeleteAgentRequest) -> Result<(), RpcError> {
                $call!(self, agents, delete_agent, request).await?;
                Ok(())
            }

            async fn send_message(
                &self,
                request: Envelope,
            ) -> Result<SendMessageResponse, RpcError> {
                let request = SendMessageRequest {
                    envelope: Some(request),
                };
                Ok($call!(self, agents, send_message, request).await?.into_inner())
            }

            async fn put_blob(&self, request: PutBlobRequest) -> Result<BlobRef, RpcError> {
                held($call!(self, agents, put_blob, request).await?.into_inner().blob)
            }

            async fn get_blob(&self, request: GetBlobRequest) -> Result<GetBlobResponse, RpcError> {
                Ok($call!(self, agents, get_blob, request).await?.into_inner())
            }

            async fn get_diff(&self, request: GetDiffRequest) -> Result<Diff, RpcError> {
                held($call!(self, agents, get_diff, request).await?.into_inner().diff)
            }

            async fn get_catalogue(
                &self,
                request: wire::GetCatalogueRequest,
            ) -> Result<wire::GetCatalogueResponse, RpcError> {
                Ok($call!(self, agents, get_catalogue, request).await?.into_inner())
            }

            async fn list_repositories(
                &self,
                request: ListRepositoriesRequest,
            ) -> Result<ListRepositoriesResponse, RpcError> {
                Ok($call!(self, agents, list_repositories, request).await?.into_inner())
            }

            async fn dump(&self, request: DumpRequest) -> Result<DumpResponse, RpcError> {
                Ok($call!(self, admin, dump, request).await?.into_inner())
            }

            async fn submit_report(
                &self,
                request: wire::SubmitReportRequest,
            ) -> Result<wire::SubmitReportResponse, RpcError> {
                Ok($call!(self, admin, submit_report, request).await?.into_inner())
            }

            async fn update_draft(
                &self,
                request: wire::UpdateDraftRequest,
            ) -> Result<wire::Draft, RpcError> {
                held($call!(self, sessions, update_draft, request).await?.into_inner().draft)
            }

            async fn get_draft(
                &self,
                request: wire::GetDraftRequest,
            ) -> Result<wire::Draft, RpcError> {
                held($call!(self, sessions, get_draft, request).await?.into_inner().draft)
            }

            async fn sign_out(&self) -> Result<(), RpcError> {
                $call!(self, admin, sign_out, wire::SignOutRequest {}).await?;
                Ok(())
            }

            async fn get_device_identity(&self) -> Result<wire::DeviceIdentity, RpcError> {
                held(
                    $call!(self, admin, get_device_identity, wire::GetDeviceIdentityRequest {})
                        .await?
                        .into_inner()
                        .identity,
                )
            }

            async fn start_pairing(
                &self,
                request: wire::StartPairingRequest,
            ) -> Result<wire::PairingWindow, RpcError> {
                held($call!(self, admin, start_pairing, request).await?.into_inner().window)
            }

            async fn cancel_pairing(&self) -> Result<(), RpcError> {
                $call!(self, admin, cancel_pairing, wire::CancelPairingRequest {}).await?;
                Ok(())
            }

            async fn subscribe_pairing(&self) -> Result<EventStream<wire::PairingStatus>, RpcError> {
                Ok(held_events(
                    $call!(self, admin, subscribe_pairing, wire::SubscribePairingRequest {})
                        .await?
                        .into_inner(),
                    |response| response.status,
                ))
            }

            async fn offer_sign_in(&self, host_id: Vec<u8>) -> Result<(), RpcError> {
                $call!(self, admin, offer_sign_in, wire::OfferSignInRequest { host_id }).await?;
                Ok(())
            }

            async fn subscribe_sign_in_offers(
                &self,
            ) -> Result<EventStream<wire::SubscribeSignInOffersResponse>, RpcError> {
                Ok(events(
                    $call!(self, admin, subscribe_sign_in_offers, wire::SubscribeSignInOffersRequest {})
                        .await?
                        .into_inner(),
                ))
            }

            async fn answer_sign_in_offer(
                &self,
                request: wire::AnswerSignInOfferRequest,
            ) -> Result<wire::Profile, RpcError> {
                held($call!(self, admin, answer_sign_in_offer, request).await?.into_inner().profile)
            }
        }
    };
}

fn events<T, S>(stream: S) -> EventStream<T>
where
    S: Stream<Item = Result<T, Status>> + Send + 'static,
{
    Box::pin(stream.map(|item| item.map_err(RpcError::from)))
}

/// A stream of what each of its responses holds. A response holding nothing
/// is dropped, as an event this build does not know is.
fn held_events<R, T, S>(stream: S, event: fn(R) -> Option<T>) -> EventStream<T>
where
    R: 'static,
    T: Send + 'static,
    S: Stream<Item = Result<R, Status>> + Send + 'static,
{
    Box::pin(stream.filter_map(move |item| {
        futures_util::future::ready(match item {
            Ok(response) => event(response).map(Ok),
            Err(status) => Some(Err(RpcError::from(status))),
        })
    }))
}

/// The value a response holds. A response without it never came from a
/// runtime that answered the call, so it says nothing of whether it landed.
fn held<T>(value: Option<T>) -> Result<T, RpcError> {
    value.ok_or_else(|| RpcError::Transport("the answer held nothing".to_owned()))
}

/// A gRPC channel over the local socket at `path`, which redials it on the
/// next call after the connection drops.
pub async fn local_channel(path: &Path) -> Result<tonic::transport::Channel, RpcError> {
    let path = path.to_owned();
    tonic::transport::Endpoint::from_static("http://amux.local")
        .connect_with_connector(tower::service_fn(move |_| {
            let path = path.clone();
            async move {
                agent_dir::local_socket::connect(&path)
                    .await
                    .map(hyper_util::rt::TokioIo::new)
            }
        }))
        .await
        .map_err(|error| RpcError::Transport(error.to_string()))
}

/// The daemon's client service over a profile's local socket.
///
/// The channel redials the socket on the next call after the connection
/// drops, so a client outlives a daemon restart; a stream open at the time
/// ends, and its reader subscribes again.
#[derive(Clone)]
pub struct GrpcClient {
    service: wire::ClientStubs,
    admin: ProfileAdminServiceClient<tonic::transport::Channel>,
    /// What every call names this client as, in [`wire::SURFACE_HEADER`].
    surface: Option<tonic::metadata::AsciiMetadataValue>,
}

impl GrpcClient {
    /// Connects to the client socket at `path`.
    pub async fn connect(path: &Path) -> Result<GrpcClient, RpcError> {
        Ok(GrpcClient::new(local_channel(path).await?))
    }

    pub fn new(channel: tonic::transport::Channel) -> GrpcClient {
        GrpcClient {
            service: wire::client_stubs(channel.clone()),
            admin: ProfileAdminServiceClient::new(channel),
            surface: None,
        }
    }

    /// Names this client as `surface` on every call (`mac` for the Mac
    /// app), so the daemon counts it as that client opening.
    pub fn naming(mut self, surface: Option<&'static str>) -> GrpcClient {
        self.surface = surface.map(tonic::metadata::AsciiMetadataValue::from_static);
        self
    }

    fn request<T>(&self, message: T) -> Request<T> {
        let mut request = Request::new(message);
        if let Some(surface) = &self.surface {
            request
                .metadata_mut()
                .insert(wire::SURFACE_HEADER, surface.clone());
        }
        request
    }
}

macro_rules! grpc_call {
    ($client:expr, admin, $name:ident, $request:expr) => {
        $client.admin.clone().$name($client.request($request))
    };
    ($client:expr, $service:ident, $name:ident, $request:expr) => {
        $client
            .service
            .$service
            .clone()
            .$name($client.request($request))
    };
}

client_impl!(impl[] Client for GrpcClient, grpc_call);

/// A profile's administration served in process, its streams boxed so one
/// trait object can stand for any implementation.
pub type DynProfileAdmin = dyn ProfileAdminService<
        SubscribePairingStream = tonic::codegen::BoxStream<wire::SubscribePairingResponse>,
        SubscribeSignInOffersStream = tonic::codegen::BoxStream<
            wire::SubscribeSignInOffersResponse,
        >,
    >;

/// The client services hosted in this process, called directly: the
/// phone's embedded runtime. Errors take the same shape they would over a
/// socket. Without the profile's administration, a dump is unimplemented.
#[derive(Clone)]
pub struct InProcess<S> {
    service: S,
    admin: Option<Arc<DynProfileAdmin>>,
}

impl<S: AgentService + SessionService> InProcess<S> {
    pub fn new(service: S) -> InProcess<S> {
        InProcess {
            service,
            admin: None,
        }
    }

    /// Serves the dump through the profile's administration.
    pub fn with_admin(mut self, admin: Arc<DynProfileAdmin>) -> InProcess<S> {
        self.admin = Some(admin);
        self
    }
}

macro_rules! in_process_call {
    ($client:expr, admin, $name:ident, $request:expr) => {
        async {
            match &$client.admin {
                Some(admin) => {
                    ProfileAdminService::$name(admin.as_ref(), Request::new($request)).await
                }
                None => Err(Status::unimplemented(concat!(
                    stringify!($name),
                    " is not served here"
                ))),
            }
        }
    };
    ($client:expr, agents, $name:ident, $request:expr) => {
        AgentService::$name(&$client.service, Request::new($request))
    };
    ($client:expr, sessions, $name:ident, $request:expr) => {
        SessionService::$name(&$client.service, Request::new($request))
    };
}

client_impl!(impl[S: AgentService + SessionService] Client for InProcess<S>, in_process_call);

#[cfg(test)]
mod tests {
    use super::*;

    fn status_with(error: &wire::Error, code: Code) -> Status {
        Status::with_details(code, error.message.clone(), error.encode_to_vec().into())
    }

    #[test]
    fn a_status_carrying_the_runtimes_error_is_that_error() {
        let error = wire::Error::because(
            wire::ErrorReason::HostUnreachable,
            "older history is held by the agent's host",
        );
        let rpc = RpcError::from(status_with(&error, Code::FailedPrecondition));
        assert_eq!(rpc, RpcError::Refused(error));
        assert_eq!(rpc.code(), Some(ErrorCode::FailedPrecondition));
        assert_eq!(rpc.reason(), Some(wire::ErrorReason::HostUnreachable));
    }

    /// gRPC answers a method the server does not have with no words at all.
    #[test]
    fn an_unknown_method_says_the_host_needs_updating() {
        let rpc = RpcError::from(Status::unimplemented(""));
        assert_eq!(rpc.code(), Some(ErrorCode::Unimplemented));
        assert_eq!(rpc.to_string(), wire::HOST_NEEDS_UPDATE);
        assert_eq!(
            RpcError::from(Status::unimplemented("no such thing here")).to_string(),
            "no such thing here",
            "a refusal with its own words keeps them"
        );
    }

    #[test]
    fn a_bare_status_is_a_transport_failure_only_when_it_says_nothing_about_the_call() {
        for code in [
            Code::Unavailable,
            Code::Unknown,
            Code::Cancelled,
            Code::Internal,
        ] {
            assert!(
                RpcError::from(Status::new(code, "connection reset")).is_transport(),
                "{code:?}"
            );
        }
        let refused = RpcError::from(Status::not_found("no such agent"));
        assert_eq!(refused.code(), Some(ErrorCode::NotFound));
    }
}
