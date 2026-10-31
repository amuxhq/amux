use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicI64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex as StdMutex, OnceLock, Weak};

use codex_protocol::server::InitializeResponse;
use codex_protocol::{
    ClientMessage, ClientNotification, ClientRequest, ClientResponse, Extra, RequestId, RpcError,
    ServerMessage,
};
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::{Mutex, Notify, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::client::error::Error;
use crate::client::event::{Event, ThreadEvent};

// ── ServerInner ──────────────────────────────────────────────────

pub(crate) struct ServerInner {
    pub stdin_tx: mpsc::Sender<Vec<u8>>,
    pub pending_requests: Mutex<HashMap<i64, oneshot::Sender<Result<Value, RpcError>>>>,
    pub thread_channels: Mutex<HashMap<String, Weak<ThreadRegistration>>>,
    pub init_result: OnceLock<InitializeResponse>,
    pub request_counter: AtomicI64,
    pub cancel: CancellationToken,
    pub child_waiter: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

const THREAD_CHANNEL_CAPACITY: usize = 256;
const THREAD_CHANNEL_OPEN: u8 = 0;
const THREAD_CHANNEL_OVERFLOW: u8 = 1;
const THREAD_CHANNEL_CLOSED: u8 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ThreadChannelState {
    Open,
    Overflow,
    Closed,
}

pub(crate) struct ThreadRegistration {
    tx: mpsc::Sender<ThreadEvent>,
    event_rx: Mutex<Option<ThreadEventReceiver>>,
    route: StdMutex<ThreadEventRoute>,
    state: AtomicU8,
    state_changed: Notify,
}

enum ThreadEventRoute {
    Live,
    Staging(VecDeque<ThreadEvent>),
}

pub(crate) struct ThreadEventReceiver {
    staged: VecDeque<ThreadEvent>,
    live: mpsc::Receiver<ThreadEvent>,
}

impl ThreadEventReceiver {
    pub(crate) fn try_recv(&mut self) -> Result<ThreadEvent, mpsc::error::TryRecvError> {
        self.staged
            .pop_front()
            .map_or_else(|| self.live.try_recv(), Ok)
    }

    pub(crate) async fn recv(&mut self) -> Option<ThreadEvent> {
        match self.staged.pop_front() {
            Some(event) => Some(event),
            None => self.live.recv().await,
        }
    }
}

impl ThreadRegistration {
    pub(crate) fn new() -> Arc<Self> {
        Self::new_with_route(ThreadEventRoute::Live)
    }

    fn new_staging() -> Arc<Self> {
        Self::new_with_route(ThreadEventRoute::Staging(VecDeque::new()))
    }

    fn new_with_route(route: ThreadEventRoute) -> Arc<Self> {
        let (tx, event_rx) = mpsc::channel(THREAD_CHANNEL_CAPACITY);
        Arc::new(Self {
            tx,
            event_rx: Mutex::new(Some(ThreadEventReceiver {
                staged: VecDeque::new(),
                live: event_rx,
            })),
            route: StdMutex::new(route),
            state: AtomicU8::new(THREAD_CHANNEL_OPEN),
            state_changed: Notify::new(),
        })
    }

    pub(crate) async fn take_receiver(&self) -> Option<ThreadEventReceiver> {
        self.event_rx.lock().await.take()
    }

    pub(crate) async fn restore_receiver(&self, rx: ThreadEventReceiver) {
        *self.event_rx.lock().await = Some(rx);
    }

    pub(crate) fn try_restore_receiver(
        &self,
        rx: ThreadEventReceiver,
    ) -> Result<(), ThreadEventReceiver> {
        let Ok(mut receiver) = self.event_rx.try_lock() else {
            return Err(rx);
        };
        *receiver = Some(rx);
        Ok(())
    }

    pub(crate) fn state(&self) -> ThreadChannelState {
        match self.state.load(Ordering::Acquire) {
            THREAD_CHANNEL_OPEN => ThreadChannelState::Open,
            THREAD_CHANNEL_OVERFLOW => ThreadChannelState::Overflow,
            _ => ThreadChannelState::Closed,
        }
    }

    pub(crate) fn state_changed(&self) -> &Notify {
        &self.state_changed
    }

    pub(crate) fn send(&self, event: ThreadEvent) -> bool {
        if self.state.load(Ordering::Acquire) != THREAD_CHANNEL_OPEN {
            return false;
        }
        let mut route = self
            .route
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if let ThreadEventRoute::Staging(events) = &mut *route {
            events.push_back(event);
            return true;
        }
        match self.tx.try_send(event) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => {
                if self
                    .state
                    .compare_exchange(
                        THREAD_CHANNEL_OPEN,
                        THREAD_CHANNEL_OVERFLOW,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .is_ok()
                {
                    self.state_changed.notify_waiters();
                }
                false
            }
            Err(TrySendError::Closed(_)) => false,
        }
    }

    pub(crate) async fn finish_staging(&self) {
        let staged = {
            let mut route = self
                .route
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            match std::mem::replace(&mut *route, ThreadEventRoute::Live) {
                ThreadEventRoute::Staging(events) => events,
                ThreadEventRoute::Live => return,
            }
        };
        // Everything staged arrived before the resume was answered: the
        // thread's history being replayed, not the thread doing anything now.
        let staged = staged
            .into_iter()
            .map(|event| ThreadEvent {
                replayed: true,
                ..event
            })
            .collect();
        self.event_rx
            .lock()
            .await
            .as_mut()
            .expect("resume staging receiver was taken before resume completed")
            .staged = staged;
    }

    fn close(&self) {
        if self
            .state
            .compare_exchange(
                THREAD_CHANNEL_OPEN,
                THREAD_CHANNEL_CLOSED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
        {
            self.state_changed.notify_waiters();
        }
    }
}

impl ServerInner {
    pub(crate) fn new(
        stdin_tx: mpsc::Sender<Vec<u8>>,
        cancel: CancellationToken,
        child_waiter: Option<tokio::task::JoinHandle<()>>,
    ) -> Self {
        Self {
            stdin_tx,
            pending_requests: Mutex::new(HashMap::new()),
            thread_channels: Mutex::new(HashMap::new()),
            init_result: OnceLock::new(),
            request_counter: AtomicI64::new(1),
            cancel,
            child_waiter: Mutex::new(child_waiter),
        }
    }

    /// Send a request and read its answer as `R`.
    pub async fn request<R: DeserializeOwned>(&self, request: ClientRequest) -> Result<R, Error> {
        let result = self.request_value(request).await?;
        Ok(codex_protocol::result(&result)?)
    }

    /// Send a request and wait for its answer, whatever it holds.
    pub async fn request_value(&self, request: ClientRequest) -> Result<Value, Error> {
        let id = self.request_counter.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending_requests.lock().await.insert(id, tx);
        self.send(&ClientMessage::request(RequestId::Integer(id), request))
            .await?;
        rx.await
            .map_err(|_| Error::TransportClosed)?
            .map_err(Error::Rpc)
    }

    pub async fn notify(&self, notification: ClientNotification) -> Result<(), Error> {
        self.send(&ClientMessage::notification(notification)).await
    }

    /// Answer one of the server's requests.
    pub async fn respond(&self, id: RequestId, response: ClientResponse) -> Result<(), Error> {
        self.send(&ClientMessage::response(id, response)).await
    }

    async fn send(&self, message: &ClientMessage) -> Result<(), Error> {
        self.stdin_tx
            .send(codex_protocol::encode(message))
            .await
            .map_err(|_| Error::TransportClosed)
    }

    /// Return the live registration for a thread, creating one if needed.
    pub async fn register_thread(&self, thread_id: &str) -> Arc<ThreadRegistration> {
        let mut channels = self.thread_channels.lock().await;
        if let Some(registration) = channels.get(thread_id).and_then(Weak::upgrade) {
            return registration;
        }
        let registration = ThreadRegistration::new();
        channels.insert(thread_id.to_owned(), Arc::downgrade(&registration));
        registration
    }

    /// Install an unbounded pre-response staging registration for `thread/resume`.
    ///
    /// Replaces any existing registration, which recovers from overflow and
    /// connection-local terminal state. The old consumer is closed and wakes
    /// promptly.
    pub async fn reregister_thread_for_resume(&self, thread_id: &str) -> Arc<ThreadRegistration> {
        self.replace_thread_registration(thread_id, ThreadRegistration::new_staging())
            .await
    }

    async fn replace_thread_registration(
        &self,
        thread_id: &str,
        registration: Arc<ThreadRegistration>,
    ) -> Arc<ThreadRegistration> {
        let mut channels = self.thread_channels.lock().await;
        if let Some(old) = channels.insert(thread_id.to_owned(), Arc::downgrade(&registration))
            && let Some(old) = old.upgrade()
        {
            old.close();
        }
        registration
    }

    pub async fn close_thread_channels(&self) {
        let channels = std::mem::take(&mut *self.thread_channels.lock().await);
        for registration in channels.values().filter_map(Weak::upgrade) {
            registration.close();
        }
    }

    async fn send_thread_event(&self, thread_id: &str, event: Event) -> bool {
        let registration = self
            .thread_channels
            .lock()
            .await
            .get(thread_id)
            .and_then(Weak::upgrade);
        registration.is_some_and(|registration| {
            registration.send(ThreadEvent {
                event,
                replayed: false,
            })
        })
    }

    pub(crate) async fn shutdown(&self) {
        self.cancel.cancel();
        if let Some(waiter) = self.child_waiter.lock().await.take() {
            let _ = waiter.await;
        }
    }

    /// Dispatch a single line from the server.
    pub(crate) async fn dispatch_line(&self, line: &str) {
        let Ok(message) = codex_protocol::decode(line.as_bytes()) else {
            return;
        };
        match message {
            ServerMessage::Response { id, result, .. } => {
                // This client numbers its requests; any other id answers
                // nothing it asked.
                if let RequestId::Integer(id) = id
                    && let Some(tx) = self.pending_requests.lock().await.remove(&id)
                {
                    let _ = tx.send(result);
                }
            }
            ServerMessage::Request { id, request, .. } => {
                let method = request.method();
                let thread_id = request.thread_id().to_owned();
                if !self
                    .send_thread_event(
                        &thread_id,
                        Event::Request {
                            id: id.clone(),
                            request,
                        },
                    )
                    .await
                {
                    self.refuse(id, method, -32000);
                }
            }
            ServerMessage::Notification { notification, .. } => {
                // Nothing reads the notifications that name no thread.
                if let Some(thread_id) = notification.thread_id().map(str::to_owned) {
                    let _ = self
                        .send_thread_event(&thread_id, Event::Notification(notification))
                        .await;
                }
            }
            ServerMessage::Unknown(unknown) => {
                // A request this client cannot read can never be answered;
                // refusing it keeps Codex from waiting on it. Nothing reads
                // an unknown notification.
                if let Some(id) = unknown.id.clone() {
                    let method = unknown.method.clone().unwrap_or_default();
                    self.refuse(id, &method, -32601);
                }
            }
        }
    }

    /// Answer a server request no consumer can take with a JSON-RPC error.
    fn refuse(&self, id: RequestId, method: &str, code: i64) {
        let error = RpcError {
            code,
            message: format!("client did not handle server request `{method}`"),
            data: None,
            extra: Extra::new(),
        };
        let line = codex_protocol::encode(&ClientMessage::Response {
            id,
            response: Err(error),
            extra: Extra::new(),
        });
        let stdin_tx = self.stdin_tx.clone();
        tokio::spawn(async move {
            let _ = stdin_tx.send(line).await;
        });
    }
}

#[cfg(test)]
mod tests {
    use codex_protocol::ServerRequest;

    use super::*;

    fn test_inner() -> (ServerInner, mpsc::Receiver<Vec<u8>>) {
        let (stdin_tx, stdin_rx) = mpsc::channel(8);
        (
            ServerInner::new(stdin_tx, CancellationToken::new(), None),
            stdin_rx,
        )
    }

    fn warning() -> ThreadEvent {
        let line = br#"{"method":"warning","params":{"threadId":"thread-1","message":"fill"}}"#;
        let ServerMessage::Notification { notification, .. } =
            codex_protocol::decode(line).unwrap()
        else {
            panic!("a warning decodes as a notification")
        };
        ThreadEvent {
            event: Event::Notification(notification),
            replayed: false,
        }
    }

    fn tool_call_json(thread_id: &str) -> String {
        serde_json::json!({
            "id": 41,
            "method": "item/tool/call",
            "params": {
                "threadId": thread_id,
                "turnId": "turn-1",
                "callId": "call-1",
                "tool": "lookup",
                "namespace": "demo",
                "arguments": {"key": "value"}
            }
        })
        .to_string()
    }

    fn written(line: Vec<u8>) -> Value {
        serde_json::from_slice(&line).unwrap()
    }

    #[tokio::test]
    async fn string_request_id_is_preserved_in_response() {
        let (inner, mut stdin_rx) = test_inner();
        let registration = inner.register_thread("thread-1").await;
        let mut rx = registration.take_receiver().await.unwrap();
        let request = serde_json::json!({
            "id": "approval-41",
            "method": "item/tool/call",
            "params": {
                "threadId": "thread-1",
                "turnId": "turn-1",
                "callId": "call-1",
                "tool": "lookup",
                "arguments": {}
            }
        });

        inner.dispatch_line(&request.to_string()).await;
        let Event::Request { id, .. } = rx.recv().await.expect("tool call event").event else {
            panic!("unexpected event")
        };
        assert_eq!(id, RequestId::String("approval-41".into()));

        inner
            .respond(
                id,
                ClientResponse::ToolCall(codex_protocol::client::ToolCallResponse {
                    content_items: Vec::new(),
                    success: true,
                    extra: Extra::new(),
                }),
            )
            .await
            .unwrap();
        let response = written(stdin_rx.recv().await.expect("tool call response"));
        assert_eq!(response["id"], "approval-41");
    }

    #[tokio::test]
    async fn unhandled_tool_call_gets_json_rpc_error() {
        let (inner, mut stdin_rx) = test_inner();

        inner.dispatch_line(&tool_call_json("missing")).await;

        let response = written(stdin_rx.recv().await.expect("error response"));
        assert_eq!(response["id"], 41);
        assert_eq!(response["error"]["code"], -32000);
        assert!(response.get("result").is_none());
    }

    #[tokio::test]
    async fn unknown_server_request_is_refused_as_not_found() {
        let (inner, mut stdin_rx) = test_inner();

        inner
            .dispatch_line(r#"{"id":9,"method":"item/future/request","params":{"threadId":"t"}}"#)
            .await;

        let response = written(stdin_rx.recv().await.expect("error response"));
        assert_eq!(response["id"], 9);
        assert_eq!(response["error"]["code"], -32601);
    }

    #[tokio::test]
    async fn full_consumer_channel_does_not_block_reader() {
        let (inner, mut stdin_rx) = test_inner();
        let registration = inner.register_thread("thread-1").await;
        for _ in 0..THREAD_CHANNEL_CAPACITY {
            assert!(registration.send(warning()));
        }

        tokio::time::timeout(
            std::time::Duration::from_millis(100),
            inner.dispatch_line(&tool_call_json("thread-1")),
        )
        .await
        .expect("dispatch blocked on consumer");

        let response = written(stdin_rx.recv().await.expect("error response"));
        assert_eq!(response["error"]["code"], -32000);
        assert_eq!(registration.state(), ThreadChannelState::Overflow);
    }

    /// A question for the person must reach a consumer as a request; the
    /// client never answers it on its own.
    #[tokio::test]
    async fn user_input_is_surfaced_as_a_correlated_server_request() {
        let (inner, mut stdin_rx) = test_inner();
        let registration = inner.register_thread("thread-1").await;
        let mut rx = registration.take_receiver().await.unwrap();
        let request = serde_json::json!({
            "id": 52,
            "method": "item/tool/requestUserInput",
            "params": {
                "threadId": "thread-1",
                "turnId": "turn-1",
                "itemId": "item-1",
                "questions": [{
                    "id": "choice",
                    "header": "Choice",
                    "question": "Pick one"
                }]
            }
        });

        inner.dispatch_line(&request.to_string()).await;

        assert!(matches!(
            rx.recv().await.map(|event| event.event),
            Some(Event::Request {
                id: RequestId::Integer(52),
                request: ServerRequest::RequestUserInput(ref params),
            }) if params.turn_id == "turn-1"
        ));
        assert!(matches!(
            stdin_rx.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
    }

    #[tokio::test]
    async fn duplicate_thread_registration_reuses_live_channel() {
        let (inner, _stdin_rx) = test_inner();
        let first = inner.register_thread("thread-1").await;
        let second = inner.register_thread("thread-1").await;

        assert!(Arc::ptr_eq(&first, &second));
        drop(first);
        let third = inner.register_thread("thread-1").await;
        assert!(Arc::ptr_eq(&second, &third));
        assert!(third.send(warning()));
        let mut rx = second.take_receiver().await.unwrap();
        assert!(rx.recv().await.is_some());
    }

    #[tokio::test]
    async fn reregister_replaces_overflowed_registration() {
        let (inner, _stdin_rx) = test_inner();
        let old = inner.register_thread("thread-1").await;
        for _ in 0..THREAD_CHANNEL_CAPACITY {
            assert!(old.send(warning()));
        }
        assert!(!old.send(warning()));
        assert_eq!(old.state(), ThreadChannelState::Overflow);

        let fresh = inner.reregister_thread_for_resume("thread-1").await;
        assert!(!Arc::ptr_eq(&old, &fresh));
        assert_eq!(old.state(), ThreadChannelState::Overflow);
        assert_eq!(fresh.state(), ThreadChannelState::Open);
        assert!(fresh.send(warning()));
        fresh.finish_staging().await;
        let mut rx = fresh.take_receiver().await.unwrap();
        assert!(rx.recv().await.is_some());
    }
}
