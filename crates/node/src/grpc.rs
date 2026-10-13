//! The client service over a local socket: one profile's socket for
//! clients, and each agent's tools socket for its MCP server, where the
//! socket is the caller's identity.
//!
//! Every call is a thin mapping onto the profile runtime. The service holds
//! the runtime weakly: a connection that outlives its runtime (a restart in
//! process, a deleted profile) is answered unavailable, never kept alive.

use std::pin::Pin;
use std::sync::{Arc, Weak};
use std::task::{Context, Poll};

use agent_dir::local_socket::{LocalListener, LocalStream};
use futures_util::Stream;
use prost::Message as _;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::task::JoinHandle;
use tonic::{Code, Request, Response, Status};
use uuid::Uuid;
use wire::client_service_server::ClientService;
use wire::{
    Agent, BlobRef, CreateAgentRequest, DeleteAgentRequest, DeleteAgentResponse, DiffRequest,
    DumpReport, DumpRequest, Empty, ErrorCode, FetchRequest, FetchResponse, GetBlobRequest,
    GetBlobResponse, GetRequest, InventoryEvent, Item, ListRepositoriesRequest,
    ListRepositoriesResponse, PutBlobRequest, RenameAgentRequest, ResolveAgentRequest,
    ResumeAgentRequest, SendInputRequest, SendInputResponse, SendMessageRequest,
    SendMessageResponse, SessionEvent, StopAgentRequest, SubscribeRequest,
};

use crate::blobs::BlobError;
use crate::forward::{ForwardError, Owner};
use crate::offers::CatalogueError;
use crate::runtime::{AgentId, ProfileRuntime};
use crate::telemetry::{self, InputShape};

/// A wire error as a gRPC status: the coarse code for generic clients, and
/// the whole error, details included, in the status details for ours.
pub fn status(error: wire::Error) -> Status {
    let code = match ErrorCode::try_from(error.code).unwrap_or(ErrorCode::Unspecified) {
        ErrorCode::Unspecified | ErrorCode::Internal => Code::Internal,
        ErrorCode::Cancelled => Code::Cancelled,
        ErrorCode::InvalidArgument => Code::InvalidArgument,
        ErrorCode::NotFound => Code::NotFound,
        ErrorCode::AlreadyExists => Code::AlreadyExists,
        ErrorCode::PermissionDenied | ErrorCode::PaymentRequired => Code::PermissionDenied,
        ErrorCode::Unauthenticated => Code::Unauthenticated,
        // Unavailable means this daemon is not answering, which a caller
        // retries; another host being unreachable is an answer.
        ErrorCode::FailedPrecondition | ErrorCode::Unreachable => Code::FailedPrecondition,
        ErrorCode::Aborted => Code::Aborted,
        ErrorCode::ResourceExhausted => Code::ResourceExhausted,
        ErrorCode::Unimplemented => Code::Unimplemented,
        ErrorCode::Unavailable => Code::Unavailable,
        ErrorCode::DataLoss => Code::DataLoss,
    };
    Status::with_details(code, error.message.clone(), error.encode_to_vec().into())
}

pub(crate) fn wire_error(code: ErrorCode, message: impl Into<String>) -> wire::Error {
    wire::Error {
        code: code as i32,
        message: message.into(),
        details: Vec::new(),
    }
}

/// The client service for one profile. `caller` is set on an agent's
/// tools socket: that agent is the parent of what it spawns and the sender
/// of what it sends.
#[derive(Clone)]
pub struct ClientApi {
    runtime: Weak<ProfileRuntime>,
    caller: Option<AgentId>,
    /// Whether a call on another host's agent is made on that host's
    /// daemon. Off for calls that arrived from a peer: forwarding goes one
    /// hop, so no two hosts can pass a call back and forth.
    forwards: bool,
    /// The client this service counts as opened when it lists the agents:
    /// set on the profile's socket, where the terminal client connects.
    opened: Option<analytics::Client>,
}

impl ClientApi {
    pub fn new(runtime: &Arc<ProfileRuntime>, caller: Option<AgentId>) -> Self {
        Self::weak(Arc::downgrade(runtime), caller)
    }

    pub(crate) fn weak(runtime: Weak<ProfileRuntime>, caller: Option<AgentId>) -> Self {
        Self {
            runtime,
            caller,
            forwards: true,
            opened: None,
        }
    }

    /// Counts a client opened each time a caller lists the agents, at most
    /// once an hour: the terminal client lists them when it starts.
    pub fn opened_as(mut self, client: analytics::Client) -> Self {
        self.opened = Some(client);
        self
    }

    /// Whether this call is counted: a person's, made on the host where it
    /// started, so a call forwarded to a paired host is counted once.
    fn counts(&self) -> bool {
        self.caller.is_none() && self.forwards
    }

    /// Records a person's accepted input, where it went.
    fn record_input(
        &self,
        runtime: &ProfileRuntime,
        shape: Option<InputShape>,
        on: analytics::On,
        queued: Option<bool>,
    ) {
        if let (true, Some(shape), Some(queued)) = (self.counts(), shape, queued) {
            runtime.analytics().record(shape.event(on, queued));
        }
    }

    /// The service as a paired host's calls reach it: as a person's, and
    /// answered only for this host's own agents.
    pub(crate) fn for_peer(runtime: Weak<ProfileRuntime>) -> Self {
        Self {
            runtime,
            caller: None,
            forwards: false,
            opened: None,
        }
    }

    /// The host to forward a call on `agent_id` to, when it is another
    /// host's agent and this service forwards; `None` answers it here.
    async fn forward_to(
        &self,
        runtime: &ProfileRuntime,
        agent_id: &[u8],
    ) -> Result<Option<crate::HostId>, Status> {
        if !self.forwards {
            return Ok(None);
        }
        match runtime.owner(agent_id).await {
            Ok(Owner::Peer(host)) => Ok(Some(host)),
            Ok(Owner::Here | Owner::Unknown) => Ok(None),
            Err(error) => Err(status(wire_error(ErrorCode::Internal, error.to_string()))),
        }
    }

    /// What a provider offers on a host with no agent running: this host
    /// answers its own, a peer's is forwarded to the peer.
    async fn host_catalogue(
        &self,
        request: wire::GetCatalogueRequest,
    ) -> Result<Response<wire::CatalogueBlob>, Status> {
        let runtime = self.runtime()?;
        let host = host_id(&request.host_id)?;
        if host != runtime.host() {
            if !self.forwards {
                return Err(status(wire_error(
                    ErrorCode::InvalidArgument,
                    "a paired host answers only for its own providers",
                )));
            }
            return forwarded(&runtime, host, |mut client| async move {
                client.get_catalogue(request).await
            })
            .await;
        }
        let provider = crate::Provider::try_from(request.provider)
            .ok()
            .filter(|provider| crate::offers::ASKED.contains(provider))
            .ok_or_else(|| status(CatalogueError::NoProvider(request.provider).to_wire()))?;
        runtime
            .host_catalogue(provider)
            .await
            .map(|offered| Response::new(offered.blob))
            .map_err(|error| status(error.to_wire()))
    }

    fn runtime(&self) -> Result<Arc<ProfileRuntime>, Status> {
        self.runtime
            .upgrade()
            .ok_or_else(|| Status::unavailable("the profile is no longer running"))
    }

    /// On an agent's tools socket, input and stops go only to the caller's
    /// own direct children: an agent acts on the agents it started, and
    /// reaches any other only by messaging it. `refusal` says what was
    /// refused.
    async fn lineage(
        &self,
        runtime: &ProfileRuntime,
        target: &[u8],
        refusal: &str,
    ) -> Result<(), Status> {
        let Some(caller) = self.caller else {
            return Ok(());
        };
        agent_id(target)?;
        // The child may live on another host: its replica row carries the
        // same parent edge.
        let target = runtime
            .row_by_id(target)
            .await
            .map_err(|error| status(wire_error(ErrorCode::Internal, error.to_string())))?;
        let own_child = target
            .and_then(|target| target.parent)
            .is_some_and(|parent| {
                parent.host == runtime.host().as_bytes() && parent.agent == caller.as_bytes()
            });
        if own_child {
            return Ok(());
        }
        Err(status(wire_error(ErrorCode::PermissionDenied, refusal)))
    }
}

// The verdict for an input to an agent whose host cannot be reached.

/// One call made on another host's daemon, answered with its answer.
async fn forwarded<T, F, Fut>(
    runtime: &ProfileRuntime,
    host: crate::HostId,
    call: F,
) -> Result<Response<T>, Status>
where
    F: FnOnce(crate::PeerClient) -> Fut,
    Fut: std::future::Future<Output = Result<Response<T>, Status>>,
{
    runtime
        .on_peer(host, call)
        .await
        .map(Response::new)
        .map_err(|error| status(error.to_wire()))
}

fn agent_id(bytes: &[u8]) -> Result<AgentId, Status> {
    Uuid::from_slice(bytes).map_err(|_| {
        status(wire_error(
            ErrorCode::InvalidArgument,
            format!("an agent id is 16 bytes, not {}", bytes.len()),
        ))
    })
}

fn host_id(bytes: &[u8]) -> Result<crate::HostId, Status> {
    Uuid::from_slice(bytes).map_err(|_| {
        status(wire_error(
            ErrorCode::InvalidArgument,
            format!("a host id is 16 bytes, not {}", bytes.len()),
        ))
    })
}

type EventStream<T> = Pin<Box<dyn Stream<Item = Result<T, Status>> + Send>>;

#[tonic::async_trait]
impl ClientService for ClientApi {
    type SubscribeInventoryStream = EventStream<InventoryEvent>;
    type SubscribeStream = EventStream<SessionEvent>;

    async fn subscribe_inventory(
        &self,
        _request: Request<Empty>,
    ) -> Result<Response<Self::SubscribeInventoryStream>, Status> {
        let runtime = self.runtime()?;
        let subscription = runtime
            .subscribe_inventory()
            .await
            .map_err(|error| status(error.to_wire()))?;
        if let Some(client) = self.opened.filter(|_| self.counts()) {
            runtime.analytics().record_at_most_every(
                telemetry::CLIENT_OPENED_EVERY,
                analytics::Event::ClientOpened { client },
            );
        }
        let stream = futures_util::stream::unfold(subscription, |mut subscription| async move {
            let event = subscription.next().await?;
            Some((Ok(InventoryEvent::clone(&event)), subscription))
        });
        Ok(Response::new(Box::pin(stream)))
    }

    async fn resolve_agent(
        &self,
        request: Request<ResolveAgentRequest>,
    ) -> Result<Response<Agent>, Status> {
        self.runtime()?
            .resolve_agent(&request.into_inner().name)
            .await
            .map(Response::new)
            .map_err(|error| status(error.to_wire()))
    }

    async fn subscribe(
        &self,
        request: Request<SubscribeRequest>,
    ) -> Result<Response<Self::SubscribeStream>, Status> {
        let request = request.into_inner();
        let subscription = self
            .runtime()?
            .subscribe(&request.agent_id, request.tail)
            .await
            .map_err(|error| status(error.to_wire()))?;
        let stream = futures_util::stream::unfold(subscription, |mut subscription| async move {
            let event = subscription.next().await?;
            Some((Ok(SessionEvent::clone(&event)), subscription))
        });
        Ok(Response::new(Box::pin(stream)))
    }

    async fn fetch(
        &self,
        request: Request<FetchRequest>,
    ) -> Result<Response<FetchResponse>, Status> {
        self.runtime()?
            .fetch(&request.into_inner())
            .await
            .map(Response::new)
            .map_err(|error| status(error.to_wire()))
    }

    async fn get(&self, request: Request<GetRequest>) -> Result<Response<Item>, Status> {
        self.runtime()?
            .get(&request.into_inner())
            .await
            .map(Response::new)
            .map_err(|error| status(error.to_wire()))
    }

    async fn send_input(
        &self,
        request: Request<SendInputRequest>,
    ) -> Result<Response<SendInputResponse>, Status> {
        let request = request.into_inner();
        let runtime = self.runtime()?;
        self.lineage(
            &runtime,
            &request.agent_id,
            "an agent sends input only to its own children",
        )
        .await?;
        let shape = request.input.as_ref().and_then(InputShape::of);
        if let Some(host) = self.forward_to(&runtime, &request.agent_id).await? {
            let forwarded = runtime
                .on_peer(host, |mut client| async move {
                    client.send_input(request).await
                })
                .await;
            return match forwarded {
                Ok(verdict) => {
                    let on = analytics::On::PairedHost(host);
                    self.record_input(&runtime, shape, on, telemetry::accepted(&verdict));
                    Ok(Response::new(verdict))
                }
                // Never left: definitely not sent. A send that went out and
                // lost its answer comes back aborted, which the sender
                // settles at its next catch-up.
                Err(ForwardError::NotSent { .. }) => Ok(Response::new(crate::relay::rejected(
                    wire::Refusal::HostUnreachable,
                ))),
                // Never left either, and the lapse is why: the refusal
                // says so, so a client holds it like any unreachable send
                // and words the reason.
                Err(error @ ForwardError::Lapsed { .. }) => Ok(Response::new(SendInputResponse {
                    of: Some(wire::send_input_response::Of::Rejected(wire::Rejected {
                        refusal: wire::Refusal::HostUnreachable as i32,
                        detail: Some(error.to_string()),
                    })),
                })),
                Err(error) => Err(status(error.to_wire())),
            };
        }
        let verdict = runtime
            .send_input(&request)
            .await
            .map_err(|error| status(error.to_wire()))?;
        let on = analytics::On::ThisHost;
        self.record_input(&runtime, shape, on, telemetry::accepted(&verdict));
        Ok(Response::new(verdict))
    }

    async fn create_agent(
        &self,
        request: Request<CreateAgentRequest>,
    ) -> Result<Response<Agent>, Status> {
        let request = request.into_inner();
        let runtime = self.runtime()?;
        let host = runtime
            .target_host(&request)
            .map_err(|error| status(error.to_wire()))?;
        let kind = request
            .agent
            .as_ref()
            .and_then(|agent| telemetry::kind(agent.kind()));
        let first = kind
            .filter(|_| request.initial_prompt.is_some())
            .map(InputShape::Prompt);
        let on = if host == runtime.host() || !self.forwards {
            analytics::On::ThisHost
        } else {
            analytics::On::PairedHost(host)
        };
        let spawned = match on {
            analytics::On::ThisHost => runtime.spawn(request, self.caller).await,
            analytics::On::PairedHost(host) => runtime.spawn_on(host, request, self.caller).await,
        }
        .map_err(|error| status(error.to_wire()))?;
        if self.forwards
            && let Some(kind) = kind
        {
            let by = match self.caller {
                Some(_) => analytics::By::Agent,
                None => analytics::By::Person,
            };
            runtime
                .analytics()
                .record(analytics::Event::AgentCreated { kind, on, by });
            self.record_input(&runtime, first, on, Some(false));
        }
        Ok(Response::new(spawned))
    }

    async fn rename_agent(
        &self,
        request: Request<RenameAgentRequest>,
    ) -> Result<Response<Agent>, Status> {
        let request = request.into_inner();
        let runtime = self.runtime()?;
        if let Some(host) = self.forward_to(&runtime, &request.agent_id).await? {
            return forwarded(&runtime, host, |mut client| async move {
                client.rename_agent(request).await
            })
            .await;
        }
        runtime
            .rename(agent_id(&request.agent_id)?, &request.name)
            .await
            .map(Response::new)
            .map_err(|error| status(error.to_wire()))
    }

    async fn stop_agent(
        &self,
        request: Request<StopAgentRequest>,
    ) -> Result<Response<Empty>, Status> {
        let request = request.into_inner();
        let runtime = self.runtime()?;
        self.lineage(
            &runtime,
            &request.agent_id,
            "an agent stops only its own children",
        )
        .await?;
        if let Some(host) = self.forward_to(&runtime, &request.agent_id).await? {
            return forwarded(&runtime, host, |mut client| async move {
                client.stop_agent(request).await
            })
            .await;
        }
        runtime
            .stop(agent_id(&request.agent_id)?)
            .await
            .map(|_| Response::new(Empty {}))
            .map_err(|error| status(error.to_wire()))
    }

    async fn resume_agent(
        &self,
        request: Request<ResumeAgentRequest>,
    ) -> Result<Response<Agent>, Status> {
        let request = request.into_inner();
        let runtime = self.runtime()?;
        let first = match &request.initial_prompt {
            Some(_) => runtime
                .row_by_id(&request.agent_id)
                .await
                .ok()
                .flatten()
                .and_then(|row| telemetry::kind_coded(row.kind))
                .map(InputShape::Prompt),
            None => None,
        };
        if let Some(host) = self.forward_to(&runtime, &request.agent_id).await? {
            let resumed = forwarded(&runtime, host, |mut client| async move {
                client.resume_agent(request).await
            })
            .await?;
            let on = analytics::On::PairedHost(host);
            self.record_input(&runtime, first, on, Some(false));
            return Ok(resumed);
        }
        let bootstrap = request.initial_prompt.map(crate::spec::prompt_bootstrap);
        let resumed = runtime
            .resume(agent_id(&request.agent_id)?, bootstrap)
            .await
            .map_err(|error| status(error.to_wire()))?;
        self.record_input(&runtime, first, analytics::On::ThisHost, Some(false));
        Ok(Response::new(resumed))
    }

    async fn delete_agent(
        &self,
        request: Request<DeleteAgentRequest>,
    ) -> Result<Response<DeleteAgentResponse>, Status> {
        let request = request.into_inner();
        let runtime = self.runtime()?;
        if let Some(host) = self.forward_to(&runtime, &request.agent_id).await? {
            return forwarded(&runtime, host, |mut client| async move {
                client.delete_agent(request).await
            })
            .await;
        }
        runtime
            .delete(agent_id(&request.agent_id)?)
            .await
            .map(Response::new)
            .map_err(|error| status(error.to_wire()))
    }

    async fn send_message(
        &self,
        request: Request<SendMessageRequest>,
    ) -> Result<Response<SendMessageResponse>, Status> {
        self.runtime()?
            .send_message(request.into_inner(), self.caller)
            .await
            .map(Response::new)
            .map_err(|error| status(error.to_wire()))
    }

    async fn put_blob(
        &self,
        request: Request<PutBlobRequest>,
    ) -> Result<Response<BlobRef>, Status> {
        let request = request.into_inner();
        crate::blobs::within_limit(&request).map_err(|error| status(error.to_wire()))?;
        let runtime = self.runtime()?;
        if let Some(host) = self.forward_to(&runtime, &request.agent_id).await? {
            return forwarded(&runtime, host, |mut client| async move {
                client.put_blob(request).await
            })
            .await;
        }
        runtime
            .put_blob(request)
            .await
            .map(Response::new)
            .map_err(|error| status(error.to_wire()))
    }

    async fn get_blob(
        &self,
        request: Request<GetBlobRequest>,
    ) -> Result<Response<GetBlobResponse>, Status> {
        let request = request.into_inner();
        let runtime = self.runtime()?;
        match runtime.get_blob(request.clone()).await {
            Err(BlobError::NoBlob(_)) => {}
            read => {
                return read
                    .map(Response::new)
                    .map_err(|error| status(error.to_wire()));
            }
        }
        // A peer's blob not fetched yet: its origin reads it, and this
        // host keeps the bytes under the replica for the next reader.
        let Some(host) = self.forward_to(&runtime, &request.agent_id).await? else {
            return Err(status(
                BlobError::NoBlob(crate::blobs::hex(&request.hash)).to_wire(),
            ));
        };
        let (agent_id, hash) = (request.agent_id.clone(), request.hash.clone());
        let fetched = forwarded(&runtime, host, |mut client| async move {
            client.get_blob(request).await
        })
        .await?;
        runtime
            .keep_replica_blob(&agent_id, &hash, &fetched.get_ref().bytes)
            .await
            .map_err(|error| status(error.to_wire()))?;
        Ok(fetched)
    }

    async fn diff(&self, request: Request<DiffRequest>) -> Result<Response<wire::Diff>, Status> {
        let request = request.into_inner();
        let runtime = self.runtime()?;
        // The working tree is on the agent's own host, and so is the patch.
        if let Some(host) = self.forward_to(&runtime, &request.agent_id).await? {
            return forwarded(&runtime, host, |mut client| async move {
                client.diff(request).await
            })
            .await;
        }
        runtime
            .diff(request)
            .await
            .map(Response::new)
            .map_err(|error| status(error.to_wire()))
    }

    async fn get_catalogue(
        &self,
        request: Request<wire::GetCatalogueRequest>,
    ) -> Result<Response<wire::CatalogueBlob>, Status> {
        self.host_catalogue(request.into_inner()).await
    }

    async fn list_repositories(
        &self,
        request: Request<ListRepositoriesRequest>,
    ) -> Result<Response<ListRepositoriesResponse>, Status> {
        let request = request.into_inner();
        let runtime = self.runtime()?;
        let named = request.host_id.as_deref().map(host_id).transpose()?;
        if let Some(host) = named.filter(|host| *host != runtime.host()) {
            // A peer's call is answered for the peer's own host only.
            if !self.forwards {
                return Err(status(wire_error(
                    ErrorCode::InvalidArgument,
                    "a paired host lists only its own repositories",
                )));
            }
            return forwarded(&runtime, host, |mut client| async move {
                client.list_repositories(request).await
            })
            .await;
        }
        Ok(Response::new(
            runtime
                .list_repositories(request.query, request.limit)
                .await,
        ))
    }

    async fn save_draft(
        &self,
        request: Request<wire::SaveDraftRequest>,
    ) -> Result<Response<wire::Empty>, Status> {
        if !self.forwards {
            return Err(status(wire_error(
                ErrorCode::InvalidArgument,
                "a draft is kept by its client's own machine",
            )));
        }
        let request = request.into_inner();
        self.runtime()?
            .save_draft(&request.agent_id, &request.draft.unwrap_or_default());
        Ok(Response::new(wire::Empty {}))
    }

    async fn get_draft(
        &self,
        request: Request<wire::GetDraftRequest>,
    ) -> Result<Response<wire::ComposerDraft>, Status> {
        if !self.forwards {
            return Err(status(wire_error(
                ErrorCode::InvalidArgument,
                "a draft is kept by its client's own machine",
            )));
        }
        let request = request.into_inner();
        Ok(Response::new(self.runtime()?.draft(&request.agent_id)))
    }

    async fn dump(&self, request: Request<DumpRequest>) -> Result<Response<DumpReport>, Status> {
        // A local caller reads the bundle where it was written.
        let path = self
            .runtime()?
            .dump(request.into_inner())
            .await
            .map_err(|error| status(error.to_wire()))?;
        Ok(Response::new(DumpReport {
            report_path: path.to_string_lossy().into_owned(),
        }))
    }
}

/// A connection accepted on a local socket, as tonic serves it.
pub(crate) struct Connection(LocalStream);

impl tonic::transport::server::Connected for Connection {
    type ConnectInfo = ();

    fn connect_info(&self) -> Self::ConnectInfo {}
}

impl AsyncRead for Connection {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.0).poll_read(cx, buf)
    }
}

impl AsyncWrite for Connection {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.0).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.0).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.0).poll_shutdown(cx)
    }
}

/// Every connection the listener accepts, until it fails.
pub(crate) fn incoming(
    listener: LocalListener,
) -> impl Stream<Item = std::io::Result<Connection>> + Send + 'static {
    futures_util::stream::unfold(Some(listener), |listener| async move {
        let mut listener = listener?;
        match listener.accept().await {
            Ok(stream) => Some((Ok(Connection(stream)), Some(listener))),
            Err(error) => Some((Err(error), None)),
        }
    })
}

/// Serves the client service on a listener until the task is aborted,
/// which drops the listener and removes its socket, and closes every
/// connection it accepted.
pub(crate) fn serve_client(listener: LocalListener, api: ClientApi) -> JoinHandle<()> {
    tokio::spawn(async move {
        // The signal never fires; asking for graceful shutdown at all is
        // what ties each accepted connection to this future. Without it a
        // connection outlives the abort, and a client on it is answered
        // "no longer running" by a dead runtime instead of redialling the
        // socket, where a restarted runtime may already be listening.
        let served = tonic::transport::Server::builder()
            .add_service(wire::client_service_server(api))
            .serve_with_incoming_shutdown(incoming(listener), std::future::pending::<()>())
            .await;
        if let Err(error) = served {
            tracing::warn!(%error, "a client socket stopped serving");
        }
    })
}
