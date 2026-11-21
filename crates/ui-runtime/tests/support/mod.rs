//! An in-memory runtime for driver tests: fake handlers of the node's
//! services, behind the routes a client calls in memory. Every call the
//! driver makes arrives here as a [`Call`] the test answers by hand, so a
//! test states exactly what the runtime said and when; a call the test did
//! not expect fails it. The driver's calls go through the real client
//! library and the real gRPC encoding, so what a test answers reads to the
//! driver as a node's answer would, refusals and transport failures alike.

#![allow(dead_code)]

use std::future::Future;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use client::{EventStream, RpcError, Surface};
use futures_util::StreamExt as _;
use tokio::sync::{mpsc, oneshot};
use tonic::body::Body;
use tonic::codegen::http;
use tonic::service::Routes;
use tonic::{Request, Response, Status};
use wire::agent_service_server::AgentService;
use wire::draft_service_server::DraftService;
use wire::experimental::reviews1_server::Reviews1;
use wire::experimental::{
    ReviewEditRequest, ReviewGetFilesRequest, ReviewGetFilesResponse, ReviewOpenRequest,
    ReviewSummary,
};
use wire::inventory_service_server::InventoryService;
use wire::profile_admin_service_server::ProfileAdminService;
use wire::session_service_server::SessionService;
use wire::{
    Agent, Attachment, BlobRef, DumpRequest, DumpResponse, InventoryEvent, Item, Kind, Lifecycle,
    ListItemsResponse, Phase, QueuedInput, SendInputResponse, SessionEvent, Snapshot, attachment,
    inventory_event, send_input_response, session_event,
};

/// How long a test waits for the driver's next call before failing.
pub const PATIENCE: Duration = Duration::from_secs(5);

/// The host the authored agents live on: 16 bytes, so it is an id a call
/// can be addressed to.
pub const HOST_A: &[u8] = b"host-a\0\0\0\0\0\0\0\0\0\0";
/// Another host.
pub const HOST_B: &[u8] = b"host-b\0\0\0\0\0\0\0\0\0\0";

pub type Reply<T> = oneshot::Sender<Result<T, RpcError>>;

pub enum Call {
    Subscribe(
        wire::SubscribeSessionRequest,
        Reply<EventStream<SessionEvent>>,
    ),
    Inventory(Reply<EventStream<InventoryEvent>>),
    Fetch(wire::ListItemsRequest, Reply<ListItemsResponse>),
    Get(wire::GetItemRequest, Reply<Item>),
    SendInput(wire::SendInputRequest, Reply<SendInputResponse>),
    Resume(wire::ResumeAgentRequest, Reply<Agent>),
    GetBlob(wire::GetBlobRequest, Reply<wire::GetBlobResponse>),
    Close(wire::SetFilingRequest, Reply<Agent>),
    Dump(DumpRequest, Reply<DumpResponse>),
    ReviewOpen(ReviewOpenRequest, Reply<ReviewSummary>),
    ReviewFiles(ReviewGetFilesRequest, Reply<ReviewGetFilesResponse>),
    ReviewEdit(ReviewEditRequest, Reply<ReviewSummary>),
}

impl std::fmt::Debug for Call {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Call::Subscribe(request, _) => write!(f, "Subscribe({request:?})"),
            Call::Inventory(_) => write!(f, "Inventory"),
            Call::Fetch(request, _) => write!(f, "Fetch({request:?})"),
            Call::Get(request, _) => write!(f, "Get({request:?})"),
            Call::SendInput(request, _) => write!(f, "SendInput({request:?})"),
            Call::Resume(request, _) => write!(f, "Resume({request:?})"),
            Call::GetBlob(request, _) => write!(f, "GetBlob({request:?})"),
            Call::Close(request, _) => write!(f, "Settle({request:?})"),
            Call::Dump(request, _) => write!(f, "Dump({request:?})"),
            Call::ReviewOpen(request, _) => write!(f, "ReviewOpen({request:?})"),
            Call::ReviewFiles(request, _) => write!(f, "ReviewFiles({request:?})"),
            Call::ReviewEdit(request, _) => write!(f, "ReviewEdit({request:?})"),
        }
    }
}

/// The runtime side of one open stream: drop it to end the stream.
pub struct Feed<T> {
    tx: mpsc::UnboundedSender<Result<T, RpcError>>,
}

impl<T> Feed<T> {
    pub fn send(&self, event: T) {
        self.tx
            .send(Ok(event))
            .expect("the driver reads its stream");
    }

    /// Whether the driver dropped the stream.
    pub fn is_closed(&self) -> bool {
        self.tx.is_closed()
    }
}

fn stream<T: Send + 'static>() -> (Feed<T>, EventStream<T>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let stream = futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|item| (item, rx))
    });
    (Feed { tx }, Box::pin(stream))
}

/// The fake node's handlers, one for every service the drivers call.
#[derive(Clone)]
struct FakeRuntime {
    calls: mpsc::UnboundedSender<Call>,
}

/// The test's end: the calls the driver made, in order.
pub struct Calls {
    rx: mpsc::UnboundedReceiver<Call>,
    addressed: Addressed,
}

/// Each call's method and the authority it was addressed to, in order.
type Addressed = Arc<Mutex<Vec<(String, String)>>>;

/// A client of the fake runtime, in memory, and the test's end of it.
pub fn runtime() -> (client::Client, Calls) {
    let (tx, rx) = mpsc::unbounded_channel();
    let fake = FakeRuntime { calls: tx };
    let routes = Routes::default()
        .add_service(wire::inventory_service_server(fake.clone()))
        .add_service(wire::session_service_server(fake.clone()))
        .add_service(wire::draft_service_server(fake.clone()))
        .add_service(wire::agent_service_server(fake.clone()))
        .add_service(wire::experimental::reviews1_server(fake.clone()))
        .add_service(wire::profile_admin_service_server::ProfileAdminServiceServer::new(fake));
    let addressed = Addressed::default();
    let served = Recorded {
        routes,
        addressed: addressed.clone(),
    };
    (
        client::Client::in_memory(served, Surface::Terminal),
        Calls { rx, addressed },
    )
}

/// The routes, noting where each call was addressed.
#[derive(Clone)]
struct Recorded {
    routes: Routes,
    addressed: Addressed,
}

impl tower::Service<http::Request<Body>> for Recorded {
    type Response = http::Response<Body>;
    type Error = std::convert::Infallible;
    type Future = <Routes as tower::Service<http::Request<Body>>>::Future;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        tower::Service::<http::Request<Body>>::poll_ready(&mut self.routes, cx)
    }

    fn call(&mut self, request: http::Request<Body>) -> Self::Future {
        let authority = request
            .uri()
            .authority()
            .map_or_else(String::new, |authority| authority.to_string());
        self.addressed
            .lock()
            .unwrap()
            .push((request.uri().path().to_owned(), authority));
        self.routes.call(request)
    }
}

impl FakeRuntime {
    async fn call<T>(&self, make: impl FnOnce(Reply<T>) -> Call) -> Result<T, Status> {
        let (tx, rx) = oneshot::channel();
        if self.calls.send(make(tx)).is_err() {
            return Err(Status::unavailable("the test runtime is gone"));
        }
        rx.await
            .unwrap_or_else(|_| Err(RpcError::Transport("the call was dropped".into())))
            .map_err(RpcError::into_status)
    }
}

type Streamed<T> = std::pin::Pin<Box<dyn futures_util::Stream<Item = Result<T, Status>> + Send>>;

/// The test's stream of events as the responses a server streams.
fn served<T: Send + 'static, R: Send + 'static>(
    events: EventStream<T>,
    response: fn(T) -> R,
) -> Response<Streamed<R>> {
    Response::new(Box::pin(
        events.map(move |event| event.map(response).map_err(RpcError::into_status)),
    ))
}

#[tonic::async_trait]
impl InventoryService for FakeRuntime {
    type SubscribeInventoryStream = Streamed<wire::SubscribeInventoryResponse>;

    async fn subscribe_inventory(
        &self,
        _: Request<wire::SubscribeInventoryRequest>,
    ) -> Result<Response<Self::SubscribeInventoryStream>, Status> {
        let events = self.call(Call::Inventory).await?;
        Ok(served(events, |event| wire::SubscribeInventoryResponse {
            event: Some(event),
        }))
    }

    async fn resolve_agent(
        &self,
        _: Request<wire::ResolveAgentRequest>,
    ) -> Result<Response<wire::ResolveAgentResponse>, Status> {
        unimplemented!("resolve_agent")
    }

    async fn resolve_host(
        &self,
        _: Request<wire::ResolveHostRequest>,
    ) -> Result<Response<wire::ResolveHostResponse>, Status> {
        unimplemented!("resolve_host")
    }
}

#[tonic::async_trait]
impl SessionService for FakeRuntime {
    type SubscribeSessionStream = Streamed<wire::SubscribeSessionResponse>;

    async fn subscribe_session(
        &self,
        request: Request<wire::SubscribeSessionRequest>,
    ) -> Result<Response<Self::SubscribeSessionStream>, Status> {
        let request = request.into_inner();
        let events = self.call(|reply| Call::Subscribe(request, reply)).await?;
        Ok(served(events, |event| wire::SubscribeSessionResponse {
            event: Some(event),
        }))
    }

    async fn list_items(
        &self,
        request: Request<wire::ListItemsRequest>,
    ) -> Result<Response<wire::ListItemsResponse>, Status> {
        let request = request.into_inner();
        let page = self.call(|reply| Call::Fetch(request, reply)).await?;
        Ok(Response::new(wire::ListItemsResponse {
            items: page.items,
            exhausted: page.exhausted,
        }))
    }

    async fn get_item(
        &self,
        request: Request<wire::GetItemRequest>,
    ) -> Result<Response<wire::GetItemResponse>, Status> {
        let request = request.into_inner();
        let item = self.call(|reply| Call::Get(request, reply)).await?;
        Ok(Response::new(wire::GetItemResponse { item: Some(item) }))
    }

    async fn get_blob(
        &self,
        request: Request<wire::GetBlobRequest>,
    ) -> Result<Response<wire::GetBlobResponse>, Status> {
        let request = request.into_inner();
        Ok(Response::new(
            self.call(|reply| Call::GetBlob(request, reply)).await?,
        ))
    }
}

#[tonic::async_trait]
impl DraftService for FakeRuntime {
    async fn update_draft(
        &self,
        _: Request<wire::UpdateDraftRequest>,
    ) -> Result<Response<wire::UpdateDraftResponse>, Status> {
        unimplemented!("update_draft")
    }

    async fn get_draft(
        &self,
        _: Request<wire::GetDraftRequest>,
    ) -> Result<Response<wire::GetDraftResponse>, Status> {
        unimplemented!("get_draft")
    }
}

#[tonic::async_trait]
impl AgentService for FakeRuntime {
    async fn create_agent(
        &self,
        _: Request<wire::CreateAgentRequest>,
    ) -> Result<Response<wire::CreateAgentResponse>, Status> {
        unimplemented!("create_agent")
    }

    async fn rename_agent(
        &self,
        _: Request<wire::RenameAgentRequest>,
    ) -> Result<Response<wire::RenameAgentResponse>, Status> {
        unimplemented!("rename_agent")
    }

    async fn set_filing(
        &self,
        request: Request<wire::SetFilingRequest>,
    ) -> Result<Response<wire::SetFilingResponse>, Status> {
        let request = request.into_inner();
        let agent = self.call(|reply| Call::Close(request, reply)).await?;
        Ok(Response::new(wire::SetFilingResponse {
            agent: Some(agent),
        }))
    }

    async fn resume_agent(
        &self,
        request: Request<wire::ResumeAgentRequest>,
    ) -> Result<Response<wire::ResumeAgentResponse>, Status> {
        let request = request.into_inner();
        let agent = self.call(|reply| Call::Resume(request, reply)).await?;
        Ok(Response::new(wire::ResumeAgentResponse {
            agent: Some(agent),
        }))
    }

    async fn delete_agent(
        &self,
        _: Request<wire::DeleteAgentRequest>,
    ) -> Result<Response<wire::DeleteAgentResponse>, Status> {
        unimplemented!("delete_agent")
    }

    async fn preview_delete(
        &self,
        _: Request<wire::PreviewDeleteRequest>,
    ) -> Result<Response<wire::PreviewDeleteResponse>, Status> {
        unimplemented!("preview_delete")
    }

    async fn send_input(
        &self,
        request: Request<wire::SendInputRequest>,
    ) -> Result<Response<SendInputResponse>, Status> {
        let request = request.into_inner();
        Ok(Response::new(
            self.call(|reply| Call::SendInput(request, reply)).await?,
        ))
    }

    async fn send_message(
        &self,
        _: Request<wire::SendMessageRequest>,
    ) -> Result<Response<wire::SendMessageResponse>, Status> {
        unimplemented!("send_message")
    }

    async fn put_blob(
        &self,
        _: Request<wire::PutBlobRequest>,
    ) -> Result<Response<wire::PutBlobResponse>, Status> {
        unimplemented!("put_blob")
    }
}

#[tonic::async_trait]
impl Reviews1 for FakeRuntime {
    async fn open(
        &self,
        request: Request<ReviewOpenRequest>,
    ) -> Result<Response<ReviewSummary>, Status> {
        let request = request.into_inner();
        Ok(Response::new(
            self.call(|reply| Call::ReviewOpen(request, reply)).await?,
        ))
    }

    async fn get_files(
        &self,
        request: Request<ReviewGetFilesRequest>,
    ) -> Result<Response<ReviewGetFilesResponse>, Status> {
        let request = request.into_inner();
        Ok(Response::new(
            self.call(|reply| Call::ReviewFiles(request, reply)).await?,
        ))
    }

    async fn edit(
        &self,
        request: Request<ReviewEditRequest>,
    ) -> Result<Response<ReviewSummary>, Status> {
        let request = request.into_inner();
        Ok(Response::new(
            self.call(|reply| Call::ReviewEdit(request, reply)).await?,
        ))
    }
}

#[tonic::async_trait]
impl ProfileAdminService for FakeRuntime {
    type SubscribePairingStream = Streamed<wire::SubscribePairingResponse>;
    type SubscribeSignInOffersStream = Streamed<wire::SubscribeSignInOffersResponse>;

    async fn dump(&self, request: Request<DumpRequest>) -> Result<Response<DumpResponse>, Status> {
        let request = request.into_inner();
        Ok(Response::new(
            self.call(|reply| Call::Dump(request, reply)).await?,
        ))
    }

    async fn sign_out(
        &self,
        _: Request<wire::SignOutRequest>,
    ) -> Result<Response<wire::SignOutResponse>, Status> {
        unimplemented!("sign_out")
    }

    async fn start_pairing(
        &self,
        _: Request<wire::StartPairingRequest>,
    ) -> Result<Response<wire::StartPairingResponse>, Status> {
        unimplemented!("start_pairing")
    }

    async fn subscribe_pairing(
        &self,
        _: Request<wire::SubscribePairingRequest>,
    ) -> Result<Response<Self::SubscribePairingStream>, Status> {
        unimplemented!("subscribe_pairing")
    }

    async fn cancel_pairing(
        &self,
        _: Request<wire::CancelPairingRequest>,
    ) -> Result<Response<wire::CancelPairingResponse>, Status> {
        unimplemented!("cancel_pairing")
    }

    async fn begin_pair(
        &self,
        _: Request<wire::BeginPairRequest>,
    ) -> Result<Response<wire::BeginPairResponse>, Status> {
        unimplemented!("begin_pair")
    }

    async fn confirm_pair(
        &self,
        _: Request<wire::ConfirmPairRequest>,
    ) -> Result<Response<wire::ConfirmPairResponse>, Status> {
        unimplemented!("confirm_pair")
    }

    async fn abandon_pair(
        &self,
        _: Request<wire::AbandonPairRequest>,
    ) -> Result<Response<wire::AbandonPairResponse>, Status> {
        unimplemented!("abandon_pair")
    }

    async fn get_device_identity(
        &self,
        _: Request<wire::GetDeviceIdentityRequest>,
    ) -> Result<Response<wire::GetDeviceIdentityResponse>, Status> {
        unimplemented!("get_device_identity")
    }

    async fn trust_ssh_peer(
        &self,
        _: Request<wire::TrustSshPeerRequest>,
    ) -> Result<Response<wire::TrustSshPeerResponse>, Status> {
        unimplemented!("trust_ssh_peer")
    }

    async fn list_peers(
        &self,
        _: Request<wire::ListPeersRequest>,
    ) -> Result<Response<wire::ListPeersResponse>, Status> {
        unimplemented!("list_peers")
    }

    async fn unpair(
        &self,
        _: Request<wire::UnpairRequest>,
    ) -> Result<Response<wire::UnpairResponse>, Status> {
        unimplemented!("unpair")
    }

    async fn submit_report(
        &self,
        _: Request<wire::SubmitReportRequest>,
    ) -> Result<Response<wire::SubmitReportResponse>, Status> {
        unimplemented!("submit_report")
    }

    async fn offer_sign_in(
        &self,
        _: Request<wire::OfferSignInRequest>,
    ) -> Result<Response<wire::OfferSignInResponse>, Status> {
        unimplemented!("offer_sign_in")
    }

    async fn subscribe_sign_in_offers(
        &self,
        _: Request<wire::SubscribeSignInOffersRequest>,
    ) -> Result<Response<Self::SubscribeSignInOffersStream>, Status> {
        unimplemented!("subscribe_sign_in_offers")
    }

    async fn answer_sign_in_offer(
        &self,
        _: Request<wire::AnswerSignInOfferRequest>,
    ) -> Result<Response<wire::AnswerSignInOfferResponse>, Status> {
        unimplemented!("answer_sign_in_offer")
    }
}

impl Calls {
    pub async fn next(&mut self) -> Call {
        tokio::time::timeout(PATIENCE, self.rx.recv())
            .await
            .expect("the driver made no call in time")
            .expect("the driver is gone")
    }

    /// Asserts the driver makes no call within a short while.
    pub async fn none(&mut self) {
        if let Ok(Some(call)) =
            tokio::time::timeout(Duration::from_millis(50), self.rx.recv()).await
        {
            panic!("unexpected call {call:?}");
        }
    }

    /// The authority every call to `method` (its last path segment) was
    /// addressed to, in order.
    pub fn addressed(&self, method: &str) -> Vec<String> {
        self.addressed
            .lock()
            .unwrap()
            .iter()
            .filter(|(path, _)| path.rsplit('/').next() == Some(method))
            .map(|(_, authority)| authority.clone())
            .collect()
    }

    /// Answers the next call, a Subscribe, with a fresh stream.
    pub async fn subscribe_session(
        &mut self,
    ) -> (wire::SubscribeSessionRequest, Feed<SessionEvent>) {
        match self.next().await {
            Call::Subscribe(request, reply) => {
                let (feed, stream) = stream();
                reply.send(Ok(stream)).ok();
                (request, feed)
            }
            other => panic!("expected Subscribe, got {other:?}"),
        }
    }

    /// Fails the next call, a Subscribe.
    pub async fn refuse_subscribe(&mut self, error: RpcError) {
        match self.next().await {
            Call::Subscribe(_, reply) => {
                reply.send(Err(error)).ok();
            }
            other => panic!("expected Subscribe, got {other:?}"),
        }
    }

    pub async fn inventory(&mut self) -> Feed<InventoryEvent> {
        match self.next().await {
            Call::Inventory(reply) => {
                let (feed, stream) = stream();
                reply.send(Ok(stream)).ok();
                feed
            }
            other => panic!("expected Inventory, got {other:?}"),
        }
    }

    pub async fn send_input(&mut self) -> (wire::SendInputRequest, Reply<SendInputResponse>) {
        match self.next().await {
            Call::SendInput(request, reply) => (request, reply),
            other => panic!("expected SendInput, got {other:?}"),
        }
    }

    pub async fn list_items(&mut self) -> (wire::ListItemsRequest, Reply<ListItemsResponse>) {
        match self.next().await {
            Call::Fetch(request, reply) => (request, reply),
            other => panic!("expected Fetch, got {other:?}"),
        }
    }

    pub async fn get_item(&mut self) -> (wire::GetItemRequest, Reply<Item>) {
        match self.next().await {
            Call::Get(request, reply) => (request, reply),
            other => panic!("expected Get, got {other:?}"),
        }
    }

    pub async fn get_blob(&mut self) -> (wire::GetBlobRequest, Reply<wire::GetBlobResponse>) {
        match self.next().await {
            Call::GetBlob(request, reply) => (request, reply),
            other => panic!("expected GetBlob, got {other:?}"),
        }
    }

    pub async fn close_agent(&mut self) -> (wire::SetFilingRequest, Reply<Agent>) {
        match self.next().await {
            Call::Close(request, reply) => (request, reply),
            other => panic!("expected Stop, got {other:?}"),
        }
    }

    pub async fn resume(&mut self) -> (wire::ResumeAgentRequest, Reply<Agent>) {
        match self.next().await {
            Call::Resume(request, reply) => (request, reply),
            other => panic!("expected Resume, got {other:?}"),
        }
    }

    pub async fn review_open(&mut self) -> (ReviewOpenRequest, Reply<ReviewSummary>) {
        match self.next().await {
            Call::ReviewOpen(request, reply) => (request, reply),
            other => panic!("expected ReviewOpen, got {other:?}"),
        }
    }

    pub async fn review_files(&mut self) -> (ReviewGetFilesRequest, Reply<ReviewGetFilesResponse>) {
        match self.next().await {
            Call::ReviewFiles(request, reply) => (request, reply),
            other => panic!("expected ReviewFiles, got {other:?}"),
        }
    }

    pub async fn review_edit(&mut self) -> (ReviewEditRequest, Reply<ReviewSummary>) {
        match self.next().await {
            Call::ReviewEdit(request, reply) => (request, reply),
            other => panic!("expected ReviewEdit, got {other:?}"),
        }
    }
}

// Records, authored.

pub fn agent(kind: Kind) -> Agent {
    Agent {
        agent_id: b"agent-1".to_vec(),
        host_id: HOST_A.to_vec(),
        kind: kind as i32,
        name: "worker".into(),
        cwd: "/src".into(),
        lifecycle: Lifecycle::Live as i32,
        phase: Phase::Idle as i32,
        incarnation: 1,
        ..Agent::default()
    }
}

pub fn exited(mut agent: Agent) -> Agent {
    agent.lifecycle = Lifecycle::Exited as i32;
    agent.exit_cause = wire::ExitCause::Finished as i32;
    agent
}

fn event(of: session_event::Of) -> SessionEvent {
    SessionEvent { of: Some(of) }
}

pub fn snapshot(kind: Kind, revision: u64, queue: &[&[u8]]) -> SessionEvent {
    event(session_event::Of::Snapshot(Snapshot {
        revision,
        kind: kind as i32,
        queue: queue
            .iter()
            .map(|id| QueuedInput {
                input_id: id.to_vec(),
                text: "queued".into(),
                ..QueuedInput::default()
            })
            .collect(),
        phase: Phase::Working as i32,
        ..Snapshot::default()
    }))
}

pub fn text_item(kind: Kind, order: u64, revision: u64, text: &str) -> Item {
    Item {
        key: format!("k{order}"),
        order,
        revision,
        text: text.into(),
        kind: kind as i32,
        at_ms: 1_000 + order as i64,
        ..Item::default()
    }
}

/// A prompt's reflection: the item carrying the input id.
pub fn reflection(kind: Kind, order: u64, revision: u64, input_id: &[u8]) -> Item {
    Item {
        input_id: input_id.to_vec(),
        ..text_item(kind, order, revision, "a prompt")
    }
}

pub fn image_item(kind: Kind, order: u64, revision: u64, hash: &[u8]) -> Item {
    Item {
        attachments: vec![Attachment {
            of: Some(attachment::Of::Image(BlobRef {
                hash: hash.to_vec(),
                name: "shot.png".into(),
                mime: "image/png".into(),
                size: 3,
            })),
            ..Default::default()
        }],
        ..text_item(kind, order, revision, "look")
    }
}

pub fn ev(item: Item) -> SessionEvent {
    event(session_event::Of::Item(item))
}

pub fn append(key: &str, base: u64, revision: u64, text: &str) -> SessionEvent {
    event(session_event::Of::Append(wire::Append {
        key: key.into(),
        base_revision: base,
        revision,
        text: text.into(),
    }))
}

pub fn caught_up(revision: u64) -> SessionEvent {
    event(session_event::Of::CaughtUp(wire::CaughtUp { revision }))
}

pub fn lagged() -> SessionEvent {
    event(session_event::Of::Lagged(wire::Lagged {}))
}

pub fn detached() -> SessionEvent {
    event(session_event::Of::Detached(wire::Detached {}))
}

pub fn accepted(queued: bool) -> SendInputResponse {
    SendInputResponse {
        of: Some(send_input_response::Of::Accepted(wire::Accepted { queued })),
    }
}

pub fn rejected(refusal: wire::Refusal) -> SendInputResponse {
    SendInputResponse {
        of: Some(send_input_response::Of::Rejected(wire::Rejected {
            refusal: refusal as i32,
            detail: None,
        })),
    }
}

pub fn unreachable() -> RpcError {
    RpcError::Refused(wire::Error::because(
        wire::ErrorReason::HostUnreachable,
        "older history is held by the agent's host, which cannot be reached",
    ))
}

/// A call the node could not carry to the agent's host, which no link
/// reaches: it never left.
pub fn host_unreachable() -> RpcError {
    RpcError::Refused(wire::Error::because(
        wire::ErrorReason::HostUnreachable,
        "desk cannot be reached",
    ))
}

/// A call the node could not carry to the agent's host because only the
/// relay reaches it and the plan lapsed: it never left either.
pub fn lapsed() -> RpcError {
    RpcError::Refused(wire::Error::because(
        wire::ErrorReason::PaymentRequired,
        "desk can be reached only through the relay, and this account's plan has lapsed",
    ))
}

/// A call the node carried to the agent's host and lost the answer to: it
/// may or may not have arrived.
pub fn aborted() -> RpcError {
    RpcError::Refused(wire::Error::new(
        wire::ErrorCode::Aborted,
        "desk stopped answering; the call may or may not have arrived",
    ))
}

pub fn transport() -> RpcError {
    RpcError::Transport("connection reset".into())
}

pub fn inventory_agent(agent: Agent) -> InventoryEvent {
    InventoryEvent {
        of: Some(inventory_event::Of::Agent(agent)),
    }
}

pub fn inventory_lagged() -> InventoryEvent {
    InventoryEvent {
        of: Some(inventory_event::Of::Lagged(wire::Lagged {})),
    }
}

pub fn inventory_caught_up() -> InventoryEvent {
    InventoryEvent {
        of: Some(inventory_event::Of::CaughtUp(wire::CaughtUp {
            revision: 0,
        })),
    }
}

/// Waits until `check` holds for the state, looking again on every change
/// the driver announces: no polling, and a failure names the call site.
#[track_caller]
pub fn until<T>(
    changed: tokio::sync::watch::Receiver<()>,
    check: impl FnMut() -> Option<T>,
) -> impl Future<Output = T> {
    until_within(PATIENCE, changed, check)
}

/// [`until`] with its own patience, for a state that real processes feed.
#[track_caller]
pub fn until_within<T>(
    patience: Duration,
    mut changed: tokio::sync::watch::Receiver<()>,
    mut check: impl FnMut() -> Option<T>,
) -> impl Future<Output = T> {
    let caller = std::panic::Location::caller();
    async move {
        tokio::time::timeout(patience, async {
            loop {
                if let Some(found) = check() {
                    return found;
                }
                changed.changed().await.expect("the driver is open");
            }
        })
        .await
        .unwrap_or_else(|_| panic!("the state never got there within {patience:?} ({caller})"))
    }
}

/// The secret anywhere in a dump part's files: as text, hex-encoded (the
/// form the facts ring keeps bytes in) or as the byte list a Debug print
/// of its bytes shows.
pub fn part_holds(part: &wire::DumpPart, secret: &str) -> Option<String> {
    let hex: String = secret.bytes().map(|byte| format!("{byte:02x}")).collect();
    let list = format!("{:?}", secret.as_bytes());
    let list = list.trim_start_matches('[').trim_end_matches(']');
    part.files
        .iter()
        .find(|file| {
            let text = String::from_utf8_lossy(&file.contents);
            text.contains(secret) || text.contains(&hex) || text.contains(list)
        })
        .map(|file| file.name.clone())
}
