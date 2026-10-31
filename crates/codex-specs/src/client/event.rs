use codex_protocol::{RequestId, ServerNotification, ServerRequest};

/// One message Codex's server sent that is not an answer to a request.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Notification(ServerNotification),
    /// A request the caller must answer with [`crate::client::Thread::respond`].
    Request {
        id: RequestId,
        request: ServerRequest,
    },
}

impl Event {
    /// The JSON-RPC method the server sent this as.
    pub fn method(&self) -> &str {
        match self {
            Self::Notification(notification) => notification.method(),
            Self::Request { request, .. } => request.method(),
        }
    }
}

/// An event routed to one thread.
#[derive(Debug, Clone, PartialEq)]
pub struct ThreadEvent {
    pub event: Event,
    /// Sent while a `thread/resume` was still being answered, which is when
    /// the app-server replays the thread's history. Such an event describes
    /// something that already happened, not something happening now.
    pub replayed: bool,
}

impl ThreadEvent {
    pub fn notification(&self) -> Option<&ServerNotification> {
        match &self.event {
            Event::Notification(notification) => Some(notification),
            _ => None,
        }
    }
}
