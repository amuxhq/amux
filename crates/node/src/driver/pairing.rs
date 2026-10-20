//! What the driver carries for the core's pairing and trust: the pairing
//! streams in both directions, the trust store and the pairing window as
//! the core last published them, and the work that follows a trust change
//! on the connections.
//!
//! Every decision is the core's. A carrier here reads a stream's messages
//! into [`Input::PairingMessage`], writes each [`Output::Pair`] to it, and
//! ends it as [`Output::PairEnded`] says.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use daemon_core::pairing::{FailureCode, Initiate, Opened, PairFailure, PairingEvent};
use daemon_core::payload::{CallError, ClientRequest, PairMessage, Response, Status};
use daemon_core::{Input, PairingId};
use futures_util::{Stream, stream};
use tokio::sync::{mpsc, oneshot};
use tonic::Code;

use super::calls::Delivered;
use super::{Ask, Driver};
use crate::trust::{Reachability, SharedTrustStore};

/// Whether the pairing window is open, and whether too many wrong secrets
/// closed the last one, as the core last said: what the pre-trust
/// handshake reads before a stream reaches the core.
#[derive(Debug, Default)]
pub struct PairWindow {
    active: AtomicBool,
    locked_out: AtomicBool,
}

impl PairWindow {
    pub(crate) fn is_active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }

    pub(crate) fn is_locked_out(&self) -> bool {
        self.locked_out.load(Ordering::Acquire)
    }

    /// Takes the core's word on the window.
    pub(crate) fn publish(&self, active: bool, locked_out: bool) {
        self.active.store(active, Ordering::Release);
        self.locked_out.store(locked_out, Ordering::Release);
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
                let _ = pending.send(Err(failure.unwrap_or_else(|| PairFailure {
                    code: FailureCode::Unavailable,
                    message: "pairing stream closed".to_owned(),
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

/// A pairing refusal as the carrier's status says it.
pub(crate) fn status(failure: &PairFailure) -> tonic::Status {
    let code = match failure.code {
        FailureCode::PermissionDenied => Code::PermissionDenied,
        FailureCode::DeadlineExceeded => Code::DeadlineExceeded,
        FailureCode::InvalidArgument => Code::InvalidArgument,
        FailureCode::Unavailable => Code::Unavailable,
        FailureCode::FailedPrecondition => Code::FailedPrecondition,
        FailureCode::Internal => Code::Internal,
        FailureCode::ResourceExhausted => Code::ResourceExhausted,
        FailureCode::Cancelled => Code::Cancelled,
        FailureCode::Unknown => Code::Unknown,
    };
    tonic::Status::new(code, failure.message.clone())
}

/// A carrier's status, as the core reads a refusal.
pub(crate) fn failure(status: &tonic::Status) -> PairFailure {
    let code = match status.code() {
        Code::PermissionDenied => FailureCode::PermissionDenied,
        Code::DeadlineExceeded => FailureCode::DeadlineExceeded,
        Code::InvalidArgument => FailureCode::InvalidArgument,
        Code::Unavailable => FailureCode::Unavailable,
        Code::FailedPrecondition => FailureCode::FailedPrecondition,
        Code::Internal => FailureCode::Internal,
        Code::ResourceExhausted => FailureCode::ResourceExhausted,
        Code::Cancelled => FailureCode::Cancelled,
        _ => FailureCode::Unknown,
    };
    PairFailure {
        code,
        message: status.message().to_owned(),
    }
}

/// Reads a pairing stream's messages into the core until it ends.
fn read_into_core(
    inputs: super::WeakFeed,
    session: PairingId,
    mut inbound: tonic::Streaming<PairMessage>,
) {
    tokio::spawn(async move {
        let status = loop {
            match inbound.message().await {
                Ok(Some(message)) => {
                    super::feed(&inputs, Input::PairingMessage { session, message }).await
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
        inbound: tonic::Streaming<PairMessage>,
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
            outbound.recv().await.map(|message| (message, outbound))
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
            Err(PairFailure {
                code: FailureCode::Unavailable,
                message: "the profile is no longer running".to_owned(),
            })
        })
    }
}

/// Records what a pairing did in the audit log and analytics.
pub(crate) fn record(event: &PairingEvent, analytics: &analytics::Analytics) {
    use daemon_core::pairing::{Failure, Method, Outcome, Role};

    let method = match event.method {
        Method::Pin | Method::Demo => analytics::Method::Pin,
        Method::Qr => analytics::Method::Qr,
        Method::Ssh => analytics::Method::Ssh,
    };
    let role = match event.role {
        Role::Offerer => analytics::Role::Offerer,
        Role::Joiner => analytics::Role::Joiner,
    };
    let label = match (event.method, event.role) {
        (Method::Ssh, _) => "ssh",
        (_, Role::Offerer) => "spake2",
        (_, Role::Joiner) => "direct_pin",
    };
    match &event.outcome {
        Outcome::Failed { audit, counted } => {
            if event.role == Role::Offerer {
                crate::audit::pairing_failure(label, audit);
            }
            if let Some(counted) = counted {
                analytics.record(analytics::Event::PairingFailed {
                    role,
                    method,
                    reason: match counted {
                        Failure::Abandoned => analytics::PairingFailure::Abandoned,
                        Failure::WrongSecret => analytics::PairingFailure::WrongSecret,
                        Failure::TimedOut => analytics::PairingFailure::TimedOut,
                        Failure::SelfPairing => analytics::PairingFailure::SelfPairing,
                        Failure::Unreachable => analytics::PairingFailure::Unreachable,
                        Failure::Other => analytics::PairingFailure::Other,
                    },
                });
            }
        }
        Outcome::Succeeded { host } => {
            let remote_host = uuid::Uuid::from_bytes(host.0);
            if event.role == Role::Offerer {
                crate::audit::pairing_success(label, remote_host);
            }
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
