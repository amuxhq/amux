use std::sync::Arc;

use codex_protocol::client::{
    InjectItemsParams, InjectedItem, ThreadIdParams, TurnInterruptParams, TurnStartParams,
    TurnSteerParams,
};
use codex_protocol::items::{TextInput, UserInput};
use codex_protocol::server::{ThreadResponse, TurnStartResponse, TurnSteerResponse};
use codex_protocol::{ClientRequest, ClientResponse, Extra, RequestId, Turn};

use crate::client::dispatch::{ServerInner, ThreadEventReceiver, ThreadRegistration};
use crate::client::error::Error;
use crate::client::thread_event_stream::ThreadEventStream;

// ── Thread ───────────────────────────────────────────────────────

/// Handle to a single conversation thread on the codex app-server.
///
/// Created via [`crate::client::Codex::start_thread()`] or
/// [`crate::client::Codex::resume_thread()`]. Cheap to clone (internally
/// `Arc`-wrapped). Clones share the event channel.
#[derive(Clone)]
pub struct Thread {
    pub(crate) inner: Arc<ThreadInner>,
}

pub(crate) struct ThreadInner {
    pub server: Arc<ServerInner>,
    pub thread_id: String,
    pub session: ThreadResponse,
    pub registration: Arc<ThreadRegistration>,
}

impl Thread {
    pub(crate) fn new(
        server: Arc<ServerInner>,
        session: ThreadResponse,
        registration: Arc<ThreadRegistration>,
    ) -> Self {
        let thread_id = session.thread.id.clone();
        Self {
            inner: Arc::new(ThreadInner {
                server,
                thread_id,
                session,
                registration,
            }),
        }
    }

    /// The thread ID.
    pub fn id(&self) -> &str {
        &self.inner.thread_id
    }

    /// What `thread/start` or `thread/resume` answered.
    pub fn session(&self) -> &ThreadResponse {
        &self.inner.session
    }

    /// Take the continuous receiver for all notifications and server requests
    /// routed to this thread. Only one event consumer may be active at a time.
    pub async fn events(&self) -> Result<ThreadEventStream, Error> {
        let rx = self
            .inner
            .registration
            .take_receiver()
            .await
            .ok_or(Error::TurnActive)?;
        Ok(ThreadEventStream::new(rx, self.inner.clone()))
    }

    // ── Turn management ──────────────────────────────────────────

    /// Start a turn on this thread; `params.thread_id` is filled in.
    ///
    /// The turn's events arrive on the thread's continuous [`Self::events`]
    /// stream.
    pub async fn start_turn(&self, mut params: TurnStartParams) -> Result<Turn, Error> {
        params.thread_id = self.inner.thread_id.clone();
        let start: TurnStartResponse = self
            .inner
            .server
            .request(ClientRequest::TurnStart(params))
            .await?;
        Ok(start.turn)
    }

    /// Start a turn with one plain text input.
    pub async fn say(&self, text: impl Into<String>) -> Result<Turn, Error> {
        self.start_turn(TurnStartParams {
            input: vec![text_input(text)],
            ..TurnStartParams::default()
        })
        .await
    }

    /// Ask the app-server to compact the thread's history now. Progress
    /// arrives on the event stream as a context-compaction item.
    pub async fn compact(&self) -> Result<(), Error> {
        self.inner
            .server
            .request_value(ClientRequest::ThreadCompactStart(ThreadIdParams {
                thread_id: self.inner.thread_id.clone(),
                extra: Extra::new(),
            }))
            .await
            .map(drop)
    }

    /// Append items to the thread's model-visible history without a turn.
    pub async fn inject_items(&self, items: Vec<InjectedItem>) -> Result<(), Error> {
        self.inner
            .server
            .request_value(ClientRequest::ThreadInjectItems(InjectItemsParams {
                thread_id: self.inner.thread_id.clone(),
                items,
                extra: Extra::new(),
            }))
            .await
            .map(drop)
    }

    /// Steer an active turn with additional input; `params.thread_id` is
    /// filled in.
    pub async fn steer(&self, mut params: TurnSteerParams) -> Result<String, Error> {
        params.thread_id = self.inner.thread_id.clone();
        let response: TurnSteerResponse = self
            .inner
            .server
            .request(ClientRequest::TurnSteer(params))
            .await?;
        Ok(response.turn_id)
    }

    /// Interrupt an active turn.
    pub async fn interrupt(&self, turn_id: &str) -> Result<(), Error> {
        self.inner
            .server
            .request_value(ClientRequest::TurnInterrupt(TurnInterruptParams {
                thread_id: self.inner.thread_id.clone(),
                turn_id: turn_id.to_owned(),
                extra: Extra::new(),
            }))
            .await
            .map(drop)
    }

    /// Answer a request the server sent on this thread.
    pub async fn respond(&self, id: RequestId, response: ClientResponse) -> Result<(), Error> {
        self.inner.server.respond(id, response).await
    }
}

/// Plain text, written without the marked spans amux's own prompts carry.
pub fn text_input(text: impl Into<String>) -> UserInput {
    UserInput::Text(TextInput {
        text: text.into(),
        text_elements: None,
        extra: Extra::new(),
    })
}

pub(crate) fn restore_event_receiver(thread_inner: Arc<ThreadInner>, rx: ThreadEventReceiver) {
    let rx = match thread_inner.registration.try_restore_receiver(rx) {
        Ok(()) => return,
        Err(rx) => rx,
    };

    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        handle.spawn(async move {
            thread_inner.registration.restore_receiver(rx).await;
        });
    }
}

#[cfg(test)]
mod tests {
    use tokio::sync::mpsc;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::client::event::ThreadEvent;

    fn test_server() -> Arc<ServerInner> {
        let (stdin_tx, stdin_rx) = mpsc::channel(1);
        drop(stdin_rx);
        Arc::new(ServerInner::new(stdin_tx, CancellationToken::new(), None))
    }

    fn test_thread(server: Arc<ServerInner>) -> Thread {
        let session = codex_protocol::result(&serde_json::json!({
            "thread": {"id": "thread-1", "cwd": "/tmp", "status": {"type": "idle"}},
            "model": "test",
            "cwd": "/tmp",
            "approvalPolicy": "on-request",
            "sandbox": {"type": "readOnly"}
        }))
        .unwrap();
        Thread::new(server, session, ThreadRegistration::new())
    }

    fn event(line: &str) -> ThreadEvent {
        let codex_protocol::ServerMessage::Notification { notification, .. } =
            codex_protocol::decode(line.as_bytes()).unwrap()
        else {
            panic!("{line} is a notification")
        };
        ThreadEvent {
            event: crate::client::Event::Notification(notification),
            replayed: false,
        }
    }

    #[tokio::test]
    async fn continuous_events_span_multiple_turns() {
        let thread = test_thread(test_server());
        let registration = thread.inner.registration.clone();
        let mut events = thread.events().await.unwrap();
        let lines = [
            r#"{"method":"turn/started","params":{"threadId":"thread-1","turn":{"id":"turn-1","items":[],"status":"inProgress"}}}"#,
            r#"{"method":"turn/completed","params":{"threadId":"thread-1","turn":{"id":"turn-2","items":[],"status":"completed"}}}"#,
        ];
        for line in lines {
            assert!(registration.send(event(line)));
        }
        for line in lines {
            assert_eq!(events.next().await.unwrap().unwrap(), event(line));
        }
    }
}
