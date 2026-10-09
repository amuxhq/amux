//! An in-memory runtime for driver tests. Every call the driver makes
//! arrives here as a [`Call`] the test answers by hand, so a test states
//! exactly what the runtime said and when; a call the test did not expect
//! fails it.

#![allow(dead_code)]

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use client::{Client, EventStream, RpcError};
use tokio::sync::{mpsc, oneshot};
use wire::{
    Agent, Attachment, BlobRef, CreateAgentRequest, DeleteAgentRequest, DeleteAgentResponse, Diff,
    DiffRequest, DumpReport, DumpRequest, ErrorCode, FetchRequest, FetchResponse, GetBlobRequest,
    GetBlobResponse, GetRequest, InventoryEvent, Item, Kind, Lifecycle, ListRepositoriesRequest,
    ListRepositoriesResponse, Phase, PutBlobRequest, QueuedInput, RenameAgentRequest,
    ResolveAgentRequest, ResumeAgentRequest, SendInputRequest, SendInputResponse,
    SendMessageResponse, SessionEvent, Snapshot, StopAgentRequest, SubscribeRequest, attachment,
    inventory_event, send_input_response, session_event,
};

/// How long a test waits for the driver's next call before failing.
pub const PATIENCE: Duration = Duration::from_secs(5);

pub type Reply<T> = oneshot::Sender<Result<T, RpcError>>;

pub enum Call {
    Subscribe(SubscribeRequest, Reply<EventStream<SessionEvent>>),
    Inventory(Reply<EventStream<InventoryEvent>>),
    Fetch(FetchRequest, Reply<FetchResponse>),
    Get(GetRequest, Reply<Item>),
    SendInput(SendInputRequest, Reply<SendInputResponse>),
    Resume(ResumeAgentRequest, Reply<Agent>),
    GetBlob(GetBlobRequest, Reply<GetBlobResponse>),
    PutBlob(PutBlobRequest, Reply<BlobRef>),
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
            Call::PutBlob(request, _) => write!(f, "PutBlob({:?})", request.name),
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

pub struct FakeRuntime {
    calls: mpsc::UnboundedSender<Call>,
}

/// The test's end: the calls the driver made, in order.
pub struct Calls {
    rx: mpsc::UnboundedReceiver<Call>,
}

pub fn runtime() -> (Arc<dyn Client>, Calls) {
    let (tx, rx) = mpsc::unbounded_channel();
    (Arc::new(FakeRuntime { calls: tx }), Calls { rx })
}

impl FakeRuntime {
    async fn call<T>(&self, make: impl FnOnce(Reply<T>) -> Call) -> Result<T, RpcError> {
        let (tx, rx) = oneshot::channel();
        if self.calls.send(make(tx)).is_err() {
            return Err(RpcError::Transport("the test runtime is gone".into()));
        }
        rx.await
            .unwrap_or_else(|_| Err(RpcError::Transport("the call was dropped".into())))
    }
}

#[async_trait]
impl Client for FakeRuntime {
    async fn subscribe_inventory(&self) -> Result<EventStream<InventoryEvent>, RpcError> {
        self.call(Call::Inventory).await
    }

    async fn resolve_agent(&self, _: ResolveAgentRequest) -> Result<Agent, RpcError> {
        unimplemented!("resolve_agent")
    }

    async fn subscribe(
        &self,
        request: SubscribeRequest,
    ) -> Result<EventStream<SessionEvent>, RpcError> {
        self.call(|reply| Call::Subscribe(request, reply)).await
    }

    async fn fetch(&self, request: FetchRequest) -> Result<FetchResponse, RpcError> {
        self.call(|reply| Call::Fetch(request, reply)).await
    }

    async fn get(&self, request: GetRequest) -> Result<Item, RpcError> {
        self.call(|reply| Call::Get(request, reply)).await
    }

    async fn send_input(&self, request: SendInputRequest) -> Result<SendInputResponse, RpcError> {
        self.call(|reply| Call::SendInput(request, reply)).await
    }

    async fn create_agent(&self, _: CreateAgentRequest) -> Result<Agent, RpcError> {
        unimplemented!("create_agent")
    }

    async fn rename_agent(&self, _: RenameAgentRequest) -> Result<Agent, RpcError> {
        unimplemented!("rename_agent")
    }

    async fn stop_agent(&self, _: StopAgentRequest) -> Result<(), RpcError> {
        unimplemented!("stop_agent")
    }

    async fn resume_agent(&self, request: ResumeAgentRequest) -> Result<Agent, RpcError> {
        self.call(|reply| Call::Resume(request, reply)).await
    }

    async fn delete_agent(&self, _: DeleteAgentRequest) -> Result<DeleteAgentResponse, RpcError> {
        unimplemented!("delete_agent")
    }

    async fn send_message(
        &self,
        _: wire::SendMessageRequest,
    ) -> Result<SendMessageResponse, RpcError> {
        unimplemented!("send_message")
    }

    async fn put_blob(&self, request: PutBlobRequest) -> Result<BlobRef, RpcError> {
        self.call(|reply| Call::PutBlob(request, reply)).await
    }

    async fn get_blob(&self, request: GetBlobRequest) -> Result<GetBlobResponse, RpcError> {
        self.call(|reply| Call::GetBlob(request, reply)).await
    }

    async fn diff(&self, _: DiffRequest) -> Result<Diff, RpcError> {
        unimplemented!("diff")
    }

    async fn get_catalogue(
        &self,
        _: wire::GetCatalogueRequest,
    ) -> Result<wire::CatalogueBlob, RpcError> {
        unimplemented!("get_catalogue")
    }

    async fn list_repositories(
        &self,
        _: ListRepositoriesRequest,
    ) -> Result<ListRepositoriesResponse, RpcError> {
        unimplemented!("list_repositories")
    }

    async fn dump(&self, _: DumpRequest) -> Result<DumpReport, RpcError> {
        unimplemented!("dump")
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

    /// Answers the next call, a Subscribe, with a fresh stream.
    pub async fn subscribe(&mut self) -> (SubscribeRequest, Feed<SessionEvent>) {
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

    pub async fn send_input(&mut self) -> (SendInputRequest, Reply<SendInputResponse>) {
        match self.next().await {
            Call::SendInput(request, reply) => (request, reply),
            other => panic!("expected SendInput, got {other:?}"),
        }
    }

    pub async fn fetch(&mut self) -> (FetchRequest, Reply<FetchResponse>) {
        match self.next().await {
            Call::Fetch(request, reply) => (request, reply),
            other => panic!("expected Fetch, got {other:?}"),
        }
    }

    pub async fn get(&mut self) -> (GetRequest, Reply<Item>) {
        match self.next().await {
            Call::Get(request, reply) => (request, reply),
            other => panic!("expected Get, got {other:?}"),
        }
    }

    pub async fn get_blob(&mut self) -> (GetBlobRequest, Reply<GetBlobResponse>) {
        match self.next().await {
            Call::GetBlob(request, reply) => (request, reply),
            other => panic!("expected GetBlob, got {other:?}"),
        }
    }

    pub async fn resume(&mut self) -> (ResumeAgentRequest, Reply<Agent>) {
        match self.next().await {
            Call::Resume(request, reply) => (request, reply),
            other => panic!("expected Resume, got {other:?}"),
        }
    }
}

// Records, authored.

pub fn agent(kind: Kind) -> Agent {
    Agent {
        agent_id: b"agent-1".to_vec(),
        host_id: b"host-a".to_vec(),
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
        agent_id: b"agent-1".to_vec(),
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
        agent_id: b"agent-1".to_vec(),
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
        }],
        ..text_item(kind, order, revision, "look")
    }
}

pub fn ev(item: Item) -> SessionEvent {
    event(session_event::Of::Item(item))
}

pub fn append(key: &str, base: u64, revision: u64, text: &str) -> SessionEvent {
    event(session_event::Of::Append(wire::Append {
        agent_id: b"agent-1".to_vec(),
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
    RpcError::Refused(wire::Error {
        code: ErrorCode::Unreachable as i32,
        message: "older history is held by the agent's host, which cannot be reached".into(),
        details: Vec::new(),
    })
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
