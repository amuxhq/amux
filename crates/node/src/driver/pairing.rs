//! What the driver carries for the core's pairing and trust: the pairing
//! streams in both directions, the trust store and the pairing window as
//! the core last published them, and the work that follows a trust change
//! on the connections.
//!
//! Every decision is the core's. A carrier here reads a stream's messages
//! into [`Input::PairingMessage`], writes each [`Output::Pair`] to it, and
//! ends it as [`Output::PairEnded`] says.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use daemon_core::pairing::{Failure, Initiate, Opened, PairFailure, PairingEvent, WindowStatus};
use daemon_core::payload::{CallError, ClientRequest, PairMessage, Response, Status};
use daemon_core::{Input, PairingId};
use futures_util::{Stream, stream};
use tokio::sync::{mpsc, oneshot};
use wire::pairing_error::Reason;

use super::calls::Delivered;
use super::{Ask, Driver};
use crate::trust::{Reachability, SharedTrustStore};

/// The pairing window as the core last said: what the pre-trust handshake
/// reads before a stream reaches the core, what the discovery advertisement
/// follows, and what people watching the window are told.
#[derive(Debug)]
pub struct PairWindow {
    status: tokio::sync::watch::Sender<WindowStatus>,
}

impl Default for PairWindow {
    fn default() -> Self {
        PairWindow {
            status: tokio::sync::watch::Sender::new(WindowStatus::default()),
        }
    }
}

impl PairWindow {
    pub(crate) fn is_active(&self) -> bool {
        self.status.borrow().active
    }

    pub(crate) fn is_locked_out(&self) -> bool {
        self.status.borrow().locked_out
    }

    /// The window now and on every change.
    pub(crate) fn watch(&self) -> tokio::sync::watch::Receiver<WindowStatus> {
        self.status.subscribe()
    }

    /// Takes the core's word on the window: whether it opened or closed.
    pub(crate) fn publish(&self, status: WindowStatus) -> bool {
        let mut moved = false;
        self.status.send_if_modified(|current| {
            moved = current.active != status.active;
            let changed = *current != status;
            *current = status;
            changed
        });
        moved
    }
}

/// The window as the wire says it.
pub(crate) fn status_to_wire(status: &WindowStatus) -> wire::PairingStatus {
    wire::PairingStatus {
        active: status.active,
        locked_out: status.locked_out,
        joining: status.joining,
        paired: status.paired.as_ref().map(|peer| wire::Peer {
            host_id: peer.host.0.to_vec(),
            name: peer.name.clone(),
            pubkey: peer.pubkey.clone(),
            ssh_target: None,
        }),
    }
}

/// What the joining side learned, held until a person decides.
pub(crate) struct Pending {
    pub(crate) token: Vec<u8>,
    pub(crate) peer: wire::PairingIdentity,
}

/// One pairing stream the driver carries for the core.
pub(crate) enum Carrier {
    /// A peer's stream to this host's window: the replies go back on it.
    Responder(mpsc::UnboundedSender<Result<PairMessage, tonic::Status>>),
    /// This host's stream to a peer's window, and whoever waits to learn
    /// who the peer is.
    Initiator {
        out: mpsc::UnboundedSender<PairMessage>,
        pending: Option<oneshot::Sender<Result<Pending, PairFailure>>>,
    },
}

/// The pairing streams under way, by conversation.
#[derive(Default)]
pub(crate) struct Carriers {
    next: AtomicU64,
    open: Mutex<HashMap<PairingId, Carrier>>,
}

impl Carriers {
    fn mint(&self) -> PairingId {
        PairingId(self.next.fetch_add(1, Ordering::Relaxed) + 1)
    }

    /// Carries `message` on the conversation's stream.
    pub(crate) fn send(&self, session: PairingId, message: PairMessage) {
        match self.open.lock().unwrap().get(&session) {
            Some(Carrier::Responder(out)) => {
                let _ = out.send(Ok(message));
            }
            Some(Carrier::Initiator { out, .. }) => {
                let _ = out.send(message);
            }
            None => {}
        }
    }

    /// The joining side's conversation holds a pending pairing.
    pub(crate) fn pending(&self, session: PairingId, token: Vec<u8>, peer: wire::PairingIdentity) {
        if let Some(Carrier::Initiator { pending, .. }) =
            self.open.lock().unwrap().get_mut(&session)
            && let Some(pending) = pending.take()
        {
            let _ = pending.send(Ok(Pending { token, peer }));
        }
    }

    /// Ends the conversation's stream, with `failure` as its status.
    pub(crate) fn end(&self, session: PairingId, failure: Option<PairFailure>) {
        let Some(carrier) = self.open.lock().unwrap().remove(&session) else {
            return;
        };
        match (carrier, failure) {
            (Carrier::Responder(out), Some(failure)) => {
                let _ = out.send(Err(status(&failure)));
            }
            (
                Carrier::Initiator {
                    pending: Some(pending),
                    ..
                },
                failure,
            ) => {
                let _ = pending.send(Err(failure.unwrap_or_else(|| {
                    PairFailure::new(wire::ErrorCode::Unavailable, "pairing stream closed")
                })));
            }
            _ => {}
        }
    }

    /// Lets go of every stream: the profile is stopping.
    pub(crate) fn end_all(&self) {
        self.open.lock().unwrap().clear();
    }
}

/// A pairing refusal as the carrier's status says it: its reason rides in
/// the details as a `PairingError`, so the other side never reads the
/// words to tell refusals apart.
pub(crate) fn status(failure: &PairFailure) -> tonic::Status {
    crate::grpc::status(
        wire::Error::new(failure.code, failure.message.clone()).with_detail(&wire::PairingError {
            reason: failure.reason as i32,
            detail: String::new(),
        }),
    )
}

/// A carrier's status, as the core reads a refusal.
pub(crate) fn failure(status: &tonic::Status) -> PairFailure {
    let reason = crate::net_error::from_status(status)
        .and_then(|error| error.detail::<wire::PairingError>())
        .map_or(Reason::Unspecified, |error| error.reason());
    PairFailure::because(
        wire::ErrorCode::try_from(status.code() as i32).unwrap_or(wire::ErrorCode::Unknown),
        reason,
        status.message(),
    )
}

/// A failure as analytics counts it.
pub(crate) fn counted_as(failure: Failure) -> analytics::PairingFailure {
    match failure {
        Failure::Abandoned => analytics::PairingFailure::Abandoned,
        Failure::WrongSecret => analytics::PairingFailure::WrongSecret,
        Failure::TimedOut => analytics::PairingFailure::TimedOut,
        Failure::SelfPairing => analytics::PairingFailure::SelfPairing,
        Failure::Unreachable => analytics::PairingFailure::Unreachable,
        Failure::Other => analytics::PairingFailure::Other,
    }
}

/// What crosses a pairing stream, either way: the step it holds.
trait Holds {
    fn message(self) -> Option<PairMessage>;
}

impl Holds for wire::PairRequest {
    fn message(self) -> Option<PairMessage> {
        self.message
    }
}

impl Holds for wire::PairResponse {
    fn message(self) -> Option<PairMessage> {
        self.message
    }
}

/// Reads a pairing stream's messages into the core until it ends. A
/// message holding no step says nothing the core could read, and is dropped
/// as one it does not know would be.
fn read_into_core<T: Holds + Send + 'static>(
    inputs: super::WeakFeed,
    session: PairingId,
    mut inbound: tonic::Streaming<T>,
) {
    tokio::spawn(async move {
        let status = loop {
            match inbound.message().await {
                Ok(Some(held)) => {
                    if let Some(message) = held.message() {
                        super::feed(&inputs, Input::PairingMessage { session, message }).await
                    }
                }
                Ok(None) => break None,
                Err(status) => break Some(failure(&status)),
            }
        };
        super::feed(&inputs, Input::PairingClosed { session, status }).await;
    });
}

impl Driver {
    /// The trust store as the core last published it.
    pub(crate) fn trust(&self) -> SharedTrustStore {
        self.trust.clone()
    }

    /// The pairing window as the core last said.
    pub(crate) fn pair_window(&self) -> Arc<PairWindow> {
        self.window.clone()
    }

    /// Hands a unary call to the core and waits for its answer: what it
    /// answered, if anything, or why it failed.
    pub(crate) async fn ask(&self, request: ClientRequest) -> Result<Option<Response>, CallError> {
        let mut answers = self.call(Ask::Ready(request), 1).await;
        let answered = match answers.next().await {
            Some(Delivered::Answer(response)) => Some(response),
            Some(Delivered::End(Status::Ok)) => return Ok(None),
            Some(Delivered::End(Status::Failed(error))) => return Err(error),
            None => return Err(CallError::Store("the profile is no longer running".into())),
        };
        let _ = answers.next().await;
        Ok(answered)
    }

    /// Carries a peer's pairing stream to this host's window: the core
    /// answers on the stream returned.
    pub(crate) async fn respond(
        &self,
        local: wire::PairingIdentity,
        reachability: Option<Reachability>,
        inbound: tonic::Streaming<wire::PairRequest>,
    ) -> impl Stream<Item = Result<PairMessage, tonic::Status>> + Send + 'static {
        let session = self.pairings.mint();
        let (out, replies) = mpsc::unbounded_channel();
        self.pairings
            .open
            .lock()
            .unwrap()
            .insert(session, Carrier::Responder(out));
        self.feed(Input::PairingOpened {
            session,
            opened: Opened::Responder {
                local,
                reachability,
            },
        })
        .await;
        read_into_core(self.inputs.downgrade(), session, inbound);
        stream::unfold(replies, |mut replies| async move {
            replies.recv().await.map(|reply| (reply, replies))
        })
    }

    /// Runs the joining side of a pairing over `client`'s stream until the
    /// core learned who the peer is, or why not.
    pub(crate) async fn initiate(
        &self,
        client: &mut wire::pairing_service_client::PairingServiceClient<tonic::transport::Channel>,
        initiate: Initiate,
    ) -> Result<Pending, PairFailure> {
        let session = self.pairings.mint();
        let (out, outbound) = mpsc::unbounded_channel();
        let (pending, learned) = oneshot::channel();
        self.pairings.open.lock().unwrap().insert(
            session,
            Carrier::Initiator {
                out,
                pending: Some(pending),
            },
        );
        self.feed(Input::PairingOpened {
            session,
            opened: Opened::Initiator(Box::new(initiate)),
        })
        .await;
        let outbound = stream::unfold(outbound, |mut outbound| async move {
            outbound.recv().await.map(|message| {
                let request = wire::PairRequest {
                    message: Some(message),
                };
                (request, outbound)
            })
        });
        match client.pair(tonic::Request::new(outbound)).await {
            Ok(inbound) => read_into_core(self.inputs.downgrade(), session, inbound.into_inner()),
            Err(status) => {
                self.feed(Input::PairingClosed {
                    session,
                    status: Some(failure(&status)),
                })
                .await
            }
        }
        learned.await.unwrap_or_else(|_| {
            Err(PairFailure::new(
                wire::ErrorCode::Unavailable,
                "the profile is no longer running",
            ))
        })
    }
}

/// Records what a pairing did: one line in the daemon's log, and the
/// analytics events.
pub(crate) fn record(event: &PairingEvent, analytics: &analytics::Analytics) {
    use daemon_core::pairing::{Method, Outcome, Role};

    let method = match event.method {
        Method::Pin | Method::Demo => analytics::Method::Pin,
        Method::Qr => analytics::Method::Qr,
        Method::Ssh => analytics::Method::Ssh,
    };
    let role = match event.role {
        Role::Offerer => analytics::Role::Offerer,
        Role::Joiner => analytics::Role::Joiner,
    };
    match &event.outcome {
        Outcome::Failed { reason, counted } => {
            tracing::warn!(
                role = ?event.role,
                method = ?event.method,
                reason = %reason,
                "a pairing failed"
            );
            if let Some(counted) = counted {
                analytics.record(analytics::Event::PairingFailed {
                    role,
                    method,
                    reason: counted_as(*counted),
                });
            }
        }
        Outcome::Succeeded { host } => {
            let remote_host = uuid::Uuid::from_bytes(host.0);
            tracing::info!(
                role = ?event.role,
                method = ?event.method,
                host = %remote_host,
                "paired"
            );
            // SSH pairings reach the daemon only once the exchange worked,
            // so a start and its success are counted together.
            if event.method == Method::Ssh {
                analytics.record(analytics::Event::PairingStarted { role, method });
            }
            analytics.record(analytics::Event::PairingSucceeded {
                role,
                method,
                remote_host,
            });
        }
    }
}
