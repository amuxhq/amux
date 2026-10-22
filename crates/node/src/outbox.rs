//! The two outboxes the daemon drains: deliveries, which tell a parent a
//! turn of its child's ended or the child's incarnation exited, and
//! notifications, which push "needs you" to the person's devices.
//!
//! The daemon core drains both outboxes: it hands each deliveries row to
//! the parent and asks for each push as a job. What is here is where the
//! push job hands its push, and a way to wait for a deliveries pass.

use std::future::Future;
use std::pin::Pin;

pub use daemon_core::DrainReport;
pub use daemon_core::payload::Push;
use uuid::Uuid;

use crate::runtime::ProfileRuntime;

#[derive(Debug, thiserror::Error)]
#[error("sending a push: {0}")]
pub struct PushError(pub String);

pub type PushFuture<'a> = Pin<Box<dyn Future<Output = Result<(), PushError>> + Send + 'a>>;

/// Where pushes go. The daemon never talks to a push service or holds a
/// device token.
pub trait PushSender: Send + Sync + 'static {
    fn send<'a>(&'a self, push: &'a Push) -> PushFuture<'a>;
}

/// Sends nothing: what the daemon runs until the cloud endpoint exists.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoopSender;

impl PushSender for NoopSender {
    fn send<'a>(&'a self, push: &'a Push) -> PushFuture<'a> {
        tracing::debug!(revision = push.revision, "a push with nowhere to go");
        Box::pin(async { Ok(()) })
    }
}

/// POSTs each push to the account's endpoint with the daemon's relay
/// credentials; the endpoint looks up the account's devices and calls the
/// platform push services.
#[derive(Clone, Debug)]
pub struct HttpSender {
    client: reqwest::Client,
    endpoint: String,
    token: String,
}

impl HttpSender {
    pub fn new(endpoint: impl Into<String>, token: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            endpoint: endpoint.into(),
            token: token.into(),
        }
    }
}

impl PushSender for HttpSender {
    fn send<'a>(&'a self, push: &'a Push) -> PushFuture<'a> {
        Box::pin(async move {
            let body = serde_json::json!({
                "host_id": Uuid::from_slice(&push.host_id).map(|id| id.to_string()).unwrap_or_default(),
                "agent_id": Uuid::from_slice(&push.agent_id).map(|id| id.to_string()).unwrap_or_default(),
                "revision": push.revision,
                "name": push.name,
                "working_on": push.working_on,
                "text": push.text,
            });
            let response = self
                .client
                .post(&self.endpoint)
                .bearer_auth(&self.token)
                .json(&body)
                .send()
                .await
                .map_err(|error| PushError(error.to_string()))?;
            if response.status().is_success() {
                Ok(())
            } else {
                Err(PushError(format!(
                    "the endpoint answered {}",
                    response.status()
                )))
            }
        })
    }
}

impl ProfileRuntime {
    /// What a pass over the deliveries outbox delivered, dropped as stale
    /// and kept: the pass under way, else one begun now. The core runs one
    /// whenever a row may have become deliverable; tests call it too.
    pub async fn drain_deliveries(&self) -> DrainReport {
        self.driver().drain().await
    }
}
