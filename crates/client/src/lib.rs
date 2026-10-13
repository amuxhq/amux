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

pub use agent_dir::{Clock, ManualClock, SystemClock};
use async_trait::async_trait;
use futures_util::{Stream, StreamExt as _};
use prost::Message as _;
use tonic::{Code, Request, Status};
use wire::client_service_client::ClientServiceClient;
use wire::client_service_server::ClientService;
use wire::{
    Agent, BlobRef, CreateAgentRequest, DeleteAgentRequest, Diff, DiffRequest, DumpReport,
    DumpRequest, Empty, ErrorCode, FetchRequest, FetchResponse, GetBlobRequest, GetBlobResponse,
    GetRequest, InventoryEvent, Item, ListRepositoriesRequest, ListRepositoriesResponse,
    PutBlobRequest, RenameAgentRequest, ResolveAgentRequest, ResumeAgentRequest, SendInputRequest,
    SendInputResponse, SendMessageRequest, SendMessageResponse, SessionEvent, StopAgentRequest,
    SubscribeRequest,
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
            RpcError::Refused(error) => {
                Some(ErrorCode::try_from(error.code).unwrap_or(ErrorCode::Unspecified))
            }
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
            && error.code != ErrorCode::Unspecified as i32
        {
            return RpcError::Refused(error);
        }
        let code = match status.code() {
            Code::Unavailable
            | Code::Unknown
            | Code::Cancelled
            | Code::DeadlineExceeded
            | Code::Internal
            | Code::Aborted => return RpcError::Transport(status.message().to_owned()),
            Code::InvalidArgument | Code::OutOfRange => ErrorCode::InvalidArgument,
            Code::NotFound => ErrorCode::NotFound,
            Code::AlreadyExists => ErrorCode::AlreadyExists,
            Code::PermissionDenied => ErrorCode::PermissionDenied,
            Code::Unauthenticated => ErrorCode::Unauthenticated,
            Code::FailedPrecondition => ErrorCode::FailedPrecondition,
            Code::ResourceExhausted => ErrorCode::ResourceExhausted,
            Code::Unimplemented => ErrorCode::Unimplemented,
            Code::DataLoss => ErrorCode::DataLoss,
            Code::Ok => ErrorCode::Unspecified,
        };
        RpcError::Refused(wire::Error {
            code: code as i32,
            message: status.message().to_owned(),
            details: Vec::new(),
        })
    }
}

/// The client service as a client calls it. Every method is the service
/// call of the same name.
#[async_trait]
pub trait Client: Send + Sync + 'static {
    async fn subscribe_inventory(&self) -> Result<EventStream<InventoryEvent>, RpcError>;
    async fn resolve_agent(&self, request: ResolveAgentRequest) -> Result<Agent, RpcError>;
    async fn subscribe(
        &self,
        request: SubscribeRequest,
    ) -> Result<EventStream<SessionEvent>, RpcError>;
    async fn fetch(&self, request: FetchRequest) -> Result<FetchResponse, RpcError>;
    async fn get(&self, request: GetRequest) -> Result<Item, RpcError>;
    async fn send_input(&self, request: SendInputRequest) -> Result<SendInputResponse, RpcError>;
    async fn create_agent(&self, request: CreateAgentRequest) -> Result<Agent, RpcError>;
    async fn rename_agent(&self, request: RenameAgentRequest) -> Result<Agent, RpcError>;
    async fn stop_agent(&self, request: StopAgentRequest) -> Result<(), RpcError>;
    async fn resume_agent(&self, request: ResumeAgentRequest) -> Result<Agent, RpcError>;
    async fn delete_agent(&self, request: DeleteAgentRequest) -> Result<(), RpcError>;
    async fn send_message(
        &self,
        request: SendMessageRequest,
    ) -> Result<SendMessageResponse, RpcError>;
    async fn put_blob(&self, request: PutBlobRequest) -> Result<BlobRef, RpcError>;
    async fn get_blob(&self, request: GetBlobRequest) -> Result<GetBlobResponse, RpcError>;
    async fn diff(&self, request: DiffRequest) -> Result<Diff, RpcError>;
    async fn get_catalogue(
        &self,
        request: wire::GetCatalogueRequest,
    ) -> Result<wire::CatalogueBlob, RpcError>;
    async fn list_repositories(
        &self,
        request: ListRepositoriesRequest,
    ) -> Result<ListRepositoriesResponse, RpcError>;
    async fn dump(&self, request: DumpRequest) -> Result<DumpReport, RpcError>;
    /// Keeps an agent's unsent draft on this machine; an empty one forgets
    /// it. A client that keeps none of its own answers Unimplemented.
    async fn save_draft(&self, _request: wire::SaveDraftRequest) -> Result<(), RpcError> {
        Err(unimplemented("save_draft"))
    }
    async fn get_draft(
        &self,
        _request: wire::GetDraftRequest,
    ) -> Result<wire::ComposerDraft, RpcError> {
        Err(unimplemented("get_draft"))
    }
}

/// The error a client that does not serve a call answers with.
fn unimplemented(call: &str) -> RpcError {
    RpcError::Refused(wire::Error {
        code: ErrorCode::Unimplemented as i32,
        message: format!("{call} is not served here"),
        details: Vec::new(),
    })
}

/// One implementation of [`Client`] per way of reaching the service:
/// `$call!(self, method, request)` makes the service call and yields its
/// `Result<Response<_>, Status>`.
macro_rules! client_impl {
    (impl[$($generics:tt)*] Client for $ty:ty, $call:ident) => {
        #[async_trait]
        impl<$($generics)*> Client for $ty {
            async fn subscribe_inventory(&self) -> Result<EventStream<InventoryEvent>, RpcError> {
                Ok(events($call!(self, subscribe_inventory, Empty {}).await?.into_inner()))
            }

            async fn resolve_agent(&self, request: ResolveAgentRequest) -> Result<Agent, RpcError> {
                Ok($call!(self, resolve_agent, request).await?.into_inner())
            }

            async fn subscribe(
                &self,
                request: SubscribeRequest,
            ) -> Result<EventStream<SessionEvent>, RpcError> {
                Ok(events($call!(self, subscribe, request).await?.into_inner()))
            }

            async fn fetch(&self, request: FetchRequest) -> Result<FetchResponse, RpcError> {
                Ok($call!(self, fetch, request).await?.into_inner())
            }

            async fn get(&self, request: GetRequest) -> Result<Item, RpcError> {
                Ok($call!(self, get, request).await?.into_inner())
            }

            async fn send_input(
                &self,
                request: SendInputRequest,
            ) -> Result<SendInputResponse, RpcError> {
                Ok($call!(self, send_input, request).await?.into_inner())
            }

            async fn create_agent(&self, request: CreateAgentRequest) -> Result<Agent, RpcError> {
                Ok($call!(self, create_agent, request).await?.into_inner())
            }

            async fn rename_agent(&self, request: RenameAgentRequest) -> Result<Agent, RpcError> {
                Ok($call!(self, rename_agent, request).await?.into_inner())
            }

            async fn stop_agent(&self, request: StopAgentRequest) -> Result<(), RpcError> {
                $call!(self, stop_agent, request).await?;
                Ok(())
            }

            async fn resume_agent(&self, request: ResumeAgentRequest) -> Result<Agent, RpcError> {
                Ok($call!(self, resume_agent, request).await?.into_inner())
            }

            async fn delete_agent(&self, request: DeleteAgentRequest) -> Result<(), RpcError> {
                $call!(self, delete_agent, request).await?;
                Ok(())
            }

            async fn send_message(
                &self,
                request: SendMessageRequest,
            ) -> Result<SendMessageResponse, RpcError> {
                Ok($call!(self, send_message, request).await?.into_inner())
            }

            async fn put_blob(&self, request: PutBlobRequest) -> Result<BlobRef, RpcError> {
                Ok($call!(self, put_blob, request).await?.into_inner())
            }

            async fn get_blob(&self, request: GetBlobRequest) -> Result<GetBlobResponse, RpcError> {
                Ok($call!(self, get_blob, request).await?.into_inner())
            }

            async fn diff(&self, request: DiffRequest) -> Result<Diff, RpcError> {
                Ok($call!(self, diff, request).await?.into_inner())
            }

            async fn get_catalogue(
                &self,
                request: wire::GetCatalogueRequest,
            ) -> Result<wire::CatalogueBlob, RpcError> {
                Ok($call!(self, get_catalogue, request).await?.into_inner())
            }

            async fn list_repositories(
                &self,
                request: ListRepositoriesRequest,
            ) -> Result<ListRepositoriesResponse, RpcError> {
                Ok($call!(self, list_repositories, request).await?.into_inner())
            }

            async fn dump(&self, request: DumpRequest) -> Result<DumpReport, RpcError> {
                Ok($call!(self, dump, request).await?.into_inner())
            }

            async fn save_draft(&self, request: wire::SaveDraftRequest) -> Result<(), RpcError> {
                $call!(self, save_draft, request).await?;
                Ok(())
            }

            async fn get_draft(
                &self,
                request: wire::GetDraftRequest,
            ) -> Result<wire::ComposerDraft, RpcError> {
                Ok($call!(self, get_draft, request).await?.into_inner())
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

/// The daemon's client service over a profile's local socket.
///
/// The channel redials the socket on the next call after the connection
/// drops, so a client outlives a daemon restart; a stream open at the time
/// ends, and its reader subscribes again.
#[derive(Clone)]
pub struct GrpcClient {
    service: ClientServiceClient<tonic::transport::Channel>,
}

impl GrpcClient {
    /// Connects to the client socket at `path`.
    pub async fn connect(path: &Path) -> Result<GrpcClient, RpcError> {
        let path = path.to_owned();
        let channel = tonic::transport::Endpoint::from_static("http://amux.local")
            .connect_with_connector(tower::service_fn(move |_| {
                let path = path.clone();
                async move {
                    agent_dir::local_socket::connect(&path)
                        .await
                        .map(hyper_util::rt::TokioIo::new)
                }
            }))
            .await
            .map_err(|error| RpcError::Transport(error.to_string()))?;
        Ok(GrpcClient::new(channel))
    }

    pub fn new(channel: tonic::transport::Channel) -> GrpcClient {
        GrpcClient {
            service: wire::client_service_client(channel),
        }
    }
}

macro_rules! grpc_call {
    ($client:expr, $name:ident, $request:expr) => {
        $client.service.clone().$name($request)
    };
}

client_impl!(impl[] Client for GrpcClient, grpc_call);

/// A client service hosted in this process, called directly: the phone's
/// embedded runtime. Errors take the same shape they would over a socket.
#[derive(Clone)]
pub struct InProcess<S> {
    service: S,
}

impl<S: ClientService> InProcess<S> {
    pub fn new(service: S) -> InProcess<S> {
        InProcess { service }
    }
}

macro_rules! in_process_call {
    ($client:expr, $name:ident, $request:expr) => {
        ClientService::$name(&$client.service, Request::new($request))
    };
}

client_impl!(impl[S: ClientService] Client for InProcess<S>, in_process_call);

#[cfg(test)]
mod tests {
    use super::*;

    fn status_with(error: &wire::Error, code: Code) -> Status {
        Status::with_details(code, error.message.clone(), error.encode_to_vec().into())
    }

    #[test]
    fn a_status_carrying_the_runtimes_error_is_that_error() {
        let error = wire::Error {
            code: ErrorCode::Unreachable as i32,
            message: "older history is held by the agent's host".into(),
            details: Vec::new(),
        };
        let rpc = RpcError::from(status_with(&error, Code::FailedPrecondition));
        assert_eq!(rpc, RpcError::Refused(error));
        assert_eq!(rpc.code(), Some(ErrorCode::Unreachable));
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
