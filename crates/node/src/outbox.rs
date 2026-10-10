//! The two outboxes the daemon drains: deliveries, which tell a parent its
//! child finished or failed, and notifications, which push "needs you" to
//! the person's devices.
//!
//! A deliveries row is written in the transaction that commits the child's
//! turn end, or when a child's process goes without one. The drain hands
//! each row to the parent through the parent's lane and deletes it once the
//! parent has accepted; a row whose parent has moved to another incarnation
//! is dropped, because a resumed parent is not waiting for anything. The
//! drain first runs only after every own journal has been read to its end
//! once after start, so the lane's lookup for an earlier acceptance never
//! runs against a store that is behind it.
//!
//! A notifications row is written in the transaction that turns an agent's
//! phase to needs_you, with a due time a delay away, and deleted unsent if
//! the phase leaves needs_you first, at the agent's exit or with the agent.
//! The drain sends each once it is due and deletes it.

use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;

use store::{Delivery, Notification};
use uuid::Uuid;
use wire::{AgentRef, Envelope, ErrorCode, Lifecycle};

use crate::forward::ForwardError;
use crate::relay::{RelayError, delivery_envelope_id};
use crate::runtime::ProfileRuntime;

/// What a push carries: envelope fields only, so producing it interprets
/// nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Push {
    pub host_id: Vec<u8>,
    pub agent_id: Vec<u8>,
    /// The revision of the snapshot that turned the agent to needs_you.
    pub revision: u64,
    pub name: Option<String>,
    pub working_on: Option<String>,
    /// The agent's newest item's text when it turned.
    pub text: String,
}

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

/// What one pass over the deliveries outbox did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DrainReport {
    /// Accepted by the parent, or found already accepted; deleted.
    pub delivered: usize,
    /// For a parent incarnation that is gone; deleted unsent.
    pub stale: usize,
    /// Still waiting: the parent is away, exited, unknown or busy failing.
    pub kept: usize,
}

enum Outcome {
    Delivered,
    Stale,
    Kept,
}

impl ProfileRuntime {
    /// One pass over the deliveries outbox. The daemon runs one after start
    /// and whenever a row may have become deliverable; tests call it too.
    pub async fn drain_deliveries(&self) -> DrainReport {
        let mut report = DrainReport::default();
        let mut read = self.journals_read.subscribe();
        let _ = read.wait_for(|read| *read).await;
        let rows = match self.store.lock().await.deliveries() {
            Ok(rows) => rows,
            Err(error) => {
                tracing::warn!(%error, "reading the deliveries outbox failed");
                return report;
            }
        };
        // A parent that lost one delivery or could not be reached is not
        // tried again this pass: each try could wait out a full patience.
        let mut unreachable = HashSet::new();
        for row in rows {
            if unreachable.contains(&row.parent) {
                report.kept += 1;
                continue;
            }
            let parent = row.parent.clone();
            match self.deliver_row(row).await {
                Ok(Outcome::Delivered) => report.delivered += 1,
                Ok(Outcome::Stale) => report.stale += 1,
                Ok(Outcome::Kept) => report.kept += 1,
                Err(error) => {
                    tracing::warn!(%error, "a delivery failed; it stays in the outbox");
                    if matches!(error, RelayError::Lost | RelayError::Unavailable) {
                        unreachable.insert(parent);
                    }
                    report.kept += 1;
                }
            }
        }
        report
    }

    /// Hands one row to its parent: through the parent's lane here, or to
    /// the parent's own daemon over the peer link, which makes the same
    /// incarnation and envelope id checks where the parent lives. The
    /// parent's row here, own or replica, answers the questions this host
    /// can answer first: whether the parent is still the incarnation the
    /// row is for, and whether it is live to receive it.
    async fn deliver_row(&self, mut row: Delivery) -> Result<Outcome, RelayError> {
        let Ok(parent_id) = Uuid::from_slice(&row.parent.agent) else {
            return Ok(Outcome::Kept);
        };
        let (parent, child) = {
            let mut store = self.store.lock().await;
            let Some(parent) = store.agent(&row.parent)? else {
                if row.parent_incarnation == 0 {
                    tracing::warn!(
                        parent = %parent_id,
                        "a child's delivery waits: its parent's row is not held"
                    );
                    return Ok(Outcome::Kept);
                }
                // The parent was deleted and its children with it, or the
                // cascade failed; nothing is waiting for this.
                store.remove_delivery(&row)?;
                return Ok(Outcome::Stale);
            };
            if row.parent_incarnation == 0 {
                // Written while the parent's row was not held: stamped now,
                // with the parent's incarnation as its row first shows it.
                row.parent_incarnation = parent.incarnation;
                store.put_delivery(&row)?;
            }
            if parent.incarnation != row.parent_incarnation {
                store.remove_delivery(&row)?;
                return Ok(Outcome::Stale);
            }
            let child = store.agent(&store::AgentKey::new(
                self.host().as_bytes().to_vec(),
                row.child_id.clone(),
            ))?;
            (parent, child)
        };
        if parent.lifecycle == Lifecycle::Exited as i32 {
            // Not waiting now; a resume makes the row stale.
            return Ok(Outcome::Kept);
        }
        let child_id = Uuid::from_slice(&row.child_id).unwrap_or_default();
        let envelope = Envelope {
            id: delivery_envelope_id(&row),
            context: None,
            from: Some(self.agent_sender(child_id, child.as_ref())),
            to: Some(AgentRef {
                host_id: row.parent.host.clone(),
                agent_id: row.parent.agent.clone(),
            }),
            kind: row.kind,
            text: row.body.clone(),
            ended: row.ended.clone(),
            incarnation: Some(row.parent_incarnation),
        };
        let sent = if row.parent.host == self.host().as_bytes() {
            self.deliver(parent_id, envelope, false, Some(row.parent_incarnation))
                .await
        } else {
            self.deliver_remote(&row.parent.host, envelope).await
        };
        match sent {
            Ok(()) => {
                self.store.lock().await.remove_delivery(&row)?;
                Ok(Outcome::Delivered)
            }
            Err(RelayError::Stale) => {
                self.store.lock().await.remove_delivery(&row)?;
                Ok(Outcome::Stale)
            }
            Err(error @ RelayError::Rejected(_)) => {
                tracing::debug!(%error, "a parent turned a delivery away; it stays");
                Ok(Outcome::Kept)
            }
            Err(error) => Err(error),
        }
    }

    /// A delivery to a parent on another host, sent to that host's daemon
    /// as a message from the child. Its answers read as the lane's would:
    /// not found means the incarnation or the parent is gone, a refusal
    /// keeps the row, a call that never left means the host is away, and
    /// one that lost its answer may have arrived: the row is tried again
    /// and the envelope id catches a duplicate.
    async fn deliver_remote(&self, host: &[u8], envelope: Envelope) -> Result<(), RelayError> {
        let host = Uuid::from_slice(host).map_err(|_| RelayError::NoAgent)?;
        let sent = self
            .on_peer(host, |mut client| async move {
                client.send_message(envelope).await
            })
            .await;
        match sent {
            Ok(_) => Ok(()),
            Err(ForwardError::NotSent { .. }) => Err(RelayError::Unavailable),
            Err(ForwardError::Uncertain { .. }) => Err(RelayError::Lost),
            Err(error) => match error.code() {
                ErrorCode::NotFound => Err(RelayError::Stale),
                // The peer's refusal arrives as an error; its words are
                // what the log keeps.
                ErrorCode::FailedPrecondition => Err(RelayError::Rejected(wire::Rejected {
                    refusal: wire::Refusal::Unspecified as i32,
                    detail: Some(error.to_string()),
                })),
                ErrorCode::Aborted | ErrorCode::Unavailable => Err(RelayError::Lost),
                _ => Err(error.into()),
            },
        }
    }

    /// Sends every notification that is due and deletes it. Returns when
    /// the next one falls due.
    pub async fn drain_notifications(&self) -> Option<i64> {
        let now = self.clock_now();
        let rows = match self.store.lock().await.notifications() {
            Ok(rows) => rows,
            Err(error) => {
                tracing::warn!(%error, "reading the notifications outbox failed");
                return None;
            }
        };
        let mut next: Option<i64> = None;
        for row in rows {
            if row.due_at_ms > now {
                next = Some(next.map_or(row.due_at_ms, |next| next.min(row.due_at_ms)));
                continue;
            }
            let push = self.push_for(&row);
            match self.push.send(&push).await {
                Ok(()) => {
                    if let Err(error) = self.store.lock().await.remove_notification(&row) {
                        tracing::warn!(%error, "deleting a sent notification failed");
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, "a push failed; it is tried again");
                    let retry = now + self.launch().push_retry_ms;
                    next = Some(next.map_or(retry, |next| next.min(retry)));
                }
            }
        }
        next
    }

    fn push_for(&self, row: &Notification) -> Push {
        Push {
            host_id: self.host().as_bytes().to_vec(),
            agent_id: row.agent_id.clone(),
            revision: row.revision,
            name: row.name.clone(),
            working_on: row.working_on.clone(),
            text: row.text.clone(),
        }
    }
}
