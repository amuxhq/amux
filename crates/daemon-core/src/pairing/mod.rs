//! Pairing: how two hosts come to trust each other.
//!
//! A host in pairing mode holds a window: a one-shot secret (a typed PIN
//! or a QR code's 256 bits) or a reusable demo PIN, until it expires, is
//! used, or too many wrong secrets close it. A host that was handed the
//! secret runs SPAKE2 against the window over a pairing carrier — a
//! pre-trust QUIC stream or the relay — confirms the derived keys, and the
//! two exchange their identities sealed under them. The joining side holds
//! what it learned as a pending pairing until a person confirms or
//! abandons it; a confirmed pairing commits the peer's key on both sides.
//!
//! Every message is an [`Input::PairingMessage`](crate::Input) and every
//! reply an [`Output::Pair`]: the carrier and its stream are the driver's.
//! The window's expiry, each conversation's deadline and each pending
//! pairing's token are timers. A commit goes through the trust store's
//! one-at-a-time writes, and the window is consumed only once the peer's
//! key is on the drive; a commit that lands after its conversation gave up
//! (the window cancelled, the deadline passed) is undone.
//!
//! SSH pairing is the same commit over another carrier: SSH authenticated
//! the identities, so no secret is exchanged and the conversation starts
//! at the commit.

pub(crate) mod crypto;

use std::collections::{BTreeMap, BTreeSet};

use crypto::{Entropy, Keys, Side, Spake2};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use wire::pair_message::Body;
use wire::pairing_error::Reason;

use crate::ids::{CallId, HostId, PairingId, TimerId};
use crate::io::{Output, ShellWork};
use crate::payload::{CallError, PairMessage, Response, Status};
use crate::serve::answered;
use crate::trust::{Reachability, TrustChange, TrustChanged, TrustEntry, TrustError, Waiter};
use crate::{Daemon, codec};

/// Wrong secrets a one-shot window takes, those in flight included,
/// before it closes.
pub const ATTEMPT_LIMIT: u8 = 5;
/// How long the window's side waits on a conversation, at most.
pub const RESPONDER_PATIENCE_MS: u64 = 5 * 60 * 1000;
/// How long the joining side waits on each leg of a conversation.
pub const INITIATOR_PATIENCE_MS: u64 = 30 * 1000;
/// Pending pairings a profile holds at once, awaiting confirmation.
pub const MAX_PENDING: usize = 32;
/// The bytes of a QR code's secret.
pub const QR_SECRET_LEN: usize = 32;
const HOST_ID_LEN: usize = 16;
const PUBKEY_LEN: usize = 32;
const MAX_NAME_BYTES: usize = 256;

/// How a window's secret is delivered.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum WindowMethod {
    /// A fresh six-digit PIN.
    Pin,
    /// A fresh 256-bit secret for a QR code.
    Qr,
    /// The operator's fixed PIN, which survives successes and failures
    /// until it expires or is cancelled: for handing one PIN to someone
    /// with no access to this machine who may pair more than once.
    Demo(String),
}

/// The secret a window opened with.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum WindowSecret {
    Pin(String),
    Qr(Vec<u8>),
}

/// How a secret travelled, as analytics and the reason log count it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Method {
    Pin,
    Demo,
    Qr,
    Ssh,
}

/// Which side of a pairing this host was.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Role {
    /// The host whose window was used.
    Offerer,
    /// The host that presented the secret, or dialled over SSH.
    Joiner,
}

/// Why a pairing failed, as analytics counts it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Failure {
    Abandoned,
    WrongSecret,
    TimedOut,
    SelfPairing,
    Unreachable,
    Other,
}

/// What a pairing came to, for the reason log and analytics.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Outcome {
    /// The reason log's words, and the failure analytics counts, if any.
    Failed {
        reason: String,
        counted: Option<Failure>,
    },
    Succeeded {
        host: HostId,
    },
}

/// One thing a pairing did that the shell records.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PairingEvent {
    pub role: Role,
    pub method: Method,
    pub outcome: Outcome,
}

/// A pairing's refusal, as the carrier's status says it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FailureCode {
    PermissionDenied,
    DeadlineExceeded,
    InvalidArgument,
    Unavailable,
    FailedPrecondition,
    Internal,
    ResourceExhausted,
    Cancelled,
    /// Any other status a carrier ended with.
    Unknown,
}

/// Why a pairing conversation or call ended unpaired, in the words the
/// other side and the person are told.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PairFailure {
    pub code: FailureCode,
    pub message: String,
}

impl PairFailure {
    fn new(code: FailureCode, message: impl Into<String>) -> PairFailure {
        PairFailure {
            code,
            message: message.into(),
        }
    }

    /// Every refusal a guessing peer could learn from reads the same.
    pub fn invalid_pin() -> PairFailure {
        PairFailure::new(FailureCode::PermissionDenied, "INVALID_PIN")
    }

    fn locked() -> PairFailure {
        PairFailure::new(FailureCode::PermissionDenied, "PAIRING_LOCKED")
    }

    fn timeout() -> PairFailure {
        PairFailure::new(FailureCode::DeadlineExceeded, "PAIRING_TIMEOUT")
    }

    fn closed() -> PairFailure {
        PairFailure::new(FailureCode::Unavailable, "pairing stream closed")
    }

    fn violation(detail: &str) -> PairFailure {
        PairFailure::new(
            FailureCode::InvalidArgument,
            format!("PROTOCOL_VIOLATION: {detail}"),
        )
    }

    /// The refusal a person is shown: every one a guessing peer could
    /// learn from reads as a wrong secret, but a host that cannot be
    /// reached, a self pairing and a lockout say so.
    pub fn opaque(self) -> PairFailure {
        match self.code {
            FailureCode::Unavailable | FailureCode::Internal => self,
            FailureCode::InvalidArgument if self.message.contains("SELF_PAIRING") => self,
            FailureCode::PermissionDenied if self.message == "PAIRING_LOCKED" => self,
            _ => PairFailure::invalid_pin(),
        }
    }

    /// How analytics counts this refusal.
    pub fn counted(&self) -> Failure {
        match self.code {
            FailureCode::PermissionDenied => Failure::WrongSecret,
            FailureCode::InvalidArgument if self.message.contains("SELF_PAIRING") => {
                Failure::SelfPairing
            }
            FailureCode::Unavailable => Failure::Unreachable,
            FailureCode::DeadlineExceeded => Failure::TimedOut,
            _ => Failure::Other,
        }
    }

    /// What the other side's error says, as this side reports it.
    fn from_peer(error: &wire::PairingError) -> PairFailure {
        let reason = Reason::try_from(error.reason).unwrap_or(Reason::Unspecified);
        if matches!(reason, Reason::InvalidPin | Reason::NotInPairingMode) {
            return PairFailure::invalid_pin();
        }
        let code = match reason {
            Reason::NotInPairingMode => FailureCode::FailedPrecondition,
            Reason::InvalidPin => FailureCode::PermissionDenied,
            Reason::ProtocolViolation | Reason::SelfPairing | Reason::Unspecified => {
                FailureCode::InvalidArgument
            }
            Reason::Timeout => FailureCode::DeadlineExceeded,
            Reason::UserRejected => FailureCode::Cancelled,
        };
        let label = reason.as_str_name();
        if error.detail.is_empty() {
            PairFailure::new(code, label)
        } else {
            PairFailure::new(code, format!("{label}: {}", error.detail))
        }
    }

    /// A trust commit's refusal, as the conversation reports it.
    fn from_trust(error: &TrustError) -> PairFailure {
        match error {
            TrustError::DuplicatePubkey(_) => PairFailure::violation("pubkey is already trusted"),
            other => PairFailure::new(FailureCode::Internal, other.to_string()),
        }
    }
}

/// Why a window could not open.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum WindowRefusal {
    AlreadyActive,
    InvalidPinFormat,
    /// No entropy has arrived to draw a secret from.
    NoEntropy,
}

/// How a pairing conversation opened.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Opened {
    /// A peer dialled this host's window. `reachability` is how this host
    /// can reach it again, when the carrier says.
    Responder {
        #[serde(with = "codec::message")]
        local: wire::PairingIdentity,
        reachability: Option<Reachability>,
    },
    /// This host dialled a host in pairing mode with the secret it was
    /// handed.
    Initiator(Box<Initiate>),
}

/// What the joining side starts a conversation with.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Initiate {
    #[serde(with = "codec::message")]
    pub local: wire::PairingIdentity,
    pub secret: Vec<u8>,
    /// The host the person meant to pair with, when they named one.
    pub expect: Option<HostId>,
    /// Whether the carrier dialled an address discovery found: a machine
    /// that already has a key here must then present that key.
    pub found: bool,
    /// How the peer is reached once paired: the route this carrier took.
    pub reachability: Reachability,
    /// How the secret was handed over, as analytics counts it.
    pub method: Method,
}

/// The window: its secret and how its attempts stand.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct Window {
    secret: Vec<u8>,
    method: Method,
    failed: u8,
    in_flight: BTreeSet<PairingId>,
    /// The conversation whose commit holds the window.
    reserved: Option<PairingId>,
    expires_at_ms: u64,
}

impl Window {
    fn reusable(&self) -> bool {
        self.method == Method::Demo
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
enum Stage {
    // The window's side.
    AwaitSpake,
    AwaitConfirmation {
        keys: Keys,
    },
    AwaitIdentity {
        keys: Keys,
    },
    Committing,
    // The joining side.
    Started {
        spake: Spake2,
        msg_b: Vec<u8>,
    },
    Confirming {
        keys: Keys,
    },
    Identifying {
        keys: Keys,
    },
    Pending {
        token: Vec<u8>,
        #[serde(with = "codec::message")]
        peer: wire::PairingIdentity,
        sealed: Vec<u8>,
    },
    Completing {
        call: CallId,
        #[serde(with = "codec::message")]
        peer: wire::PairingIdentity,
    },
    Joining {
        call: CallId,
        host: HostId,
    },
    Abandoning {
        call: CallId,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Session {
    opened: Opened,
    stage: Stage,
    /// The method of the window a responder runs against.
    method: Method,
}

/// The pairing window and the pairing conversations in progress.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PairingState {
    entropy: Entropy,
    window: Option<Window>,
    /// Until when too many wrong secrets keep the window closed: an
    /// attempt then is told it is locked out rather than that its secret
    /// did not match.
    locked_out_until_ms: Option<u64>,
    sessions: BTreeMap<PairingId, Session>,
    /// The joining side's pending pairings, by token.
    tokens: BTreeMap<Vec<u8>, PairingId>,
    /// Commits that landed for a conversation that had given up: undone.
    abandoned: BTreeMap<PairingId, (Uuid, Option<TrustEntry>)>,
    /// Commits under way, with the entry they replace.
    committing: BTreeMap<PairingId, (Uuid, Option<TrustEntry>)>,
    /// The window's state as the shell was last told.
    published: (bool, bool),
}

impl PairingState {
    /// Whether a window is open.
    pub fn active(&self) -> bool {
        self.window.is_some()
    }

    /// Whether too many wrong secrets closed the last window.
    pub fn locked_out(&self) -> bool {
        self.locked_out_until_ms.is_some()
    }
}

fn message(body: Body) -> PairMessage {
    PairMessage { body: Some(body) }
}

fn error(reason: Reason, detail: &str) -> PairMessage {
    message(Body::Error(wire::PairingError {
        reason: reason as i32,
        detail: detail.to_owned(),
    }))
}

/// The reason log's words for an error this host sends.
fn failure_words(reason: Reason, detail: &str) -> String {
    match reason {
        Reason::InvalidPin => "invalid PIN".to_owned(),
        Reason::SelfPairing => "self pairing is not allowed".to_owned(),
        Reason::NotInPairingMode => "not in pairing mode".to_owned(),
        Reason::Timeout => "pairing timed out".to_owned(),
        Reason::UserRejected => "pairing rejected by peer".to_owned(),
        Reason::ProtocolViolation => detail.to_owned(),
        Reason::Unspecified => "pairing failed".to_owned(),
    }
}

/// The host a pairing identity names, if it is well formed.
pub fn validate_identity(identity: &wire::PairingIdentity) -> Result<HostId, &'static str> {
    if identity.host_id.len() != HOST_ID_LEN {
        return Err("host_id must be 16 bytes");
    }
    if identity.pubkey.len() != PUBKEY_LEN {
        return Err("pubkey must be 32 bytes");
    }
    if identity.name.len() > MAX_NAME_BYTES {
        return Err("name is too long");
    }
    Ok(HostId::from_slice(&identity.host_id).expect("16 bytes"))
}

fn is_self(local: &wire::PairingIdentity, peer: &wire::PairingIdentity) -> bool {
    peer.host_id == local.host_id || peer.pubkey == local.pubkey
}

fn valid_pin(pin: &str) -> bool {
    pin.len() == 6 && pin.bytes().all(|byte| byte.is_ascii_digit())
}

impl Daemon {
    fn now_ms(&self) -> u64 {
        self.timers.now_ms
    }

    /// Fresh randomness: mixed into what the core draws from.
    pub(crate) fn entropy(&mut self, fresh: [u8; 32]) {
        self.pairing.entropy.add(fresh);
    }

    /// The pairing window and conversations, for reading.
    pub fn pairing(&self) -> &PairingState {
        &self.pairing
    }

    /// The window expired, or its lockout lifted, as of now.
    fn purge_window(&mut self, out: &mut Vec<Output>) {
        let now = self.now_ms();
        if self
            .pairing
            .locked_out_until_ms
            .is_some_and(|until| now >= until)
        {
            self.pairing.locked_out_until_ms = None;
        }
        if let Some(window) = &self.pairing.window
            && now >= window.expires_at_ms
            && window.reserved.is_none()
        {
            let method = window.method;
            self.pairing.window = None;
            out.push(Output::Shell(ShellWork::Pairing(PairingEvent {
                role: Role::Offerer,
                method,
                outcome: Outcome::Failed {
                    reason: "pair mode expired".to_owned(),
                    counted: None,
                },
            })));
        }
        self.publish_window(out);
    }

    /// Tells the shell whether the window is open or locked out, when that
    /// moved, and arms the window's timer for its next change.
    fn publish_window(&mut self, out: &mut Vec<Output>) {
        let now = (self.pairing.active(), self.pairing.locked_out());
        if self.pairing.published != now {
            self.pairing.published = now;
            out.push(Output::Shell(ShellWork::PairWindow {
                active: now.0,
                locked_out: now.1,
            }));
        }
        let next = self
            .pairing
            .window
            .as_ref()
            .filter(|window| window.reserved.is_none())
            .map(|window| window.expires_at_ms)
            .into_iter()
            .chain(self.pairing.locked_out_until_ms)
            .min();
        match next {
            Some(at_ms) => out.push(Output::ArmTimer {
                id: TimerId::PairWindow,
                at_ms,
            }),
            None => out.push(Output::CancelTimer {
                id: TimerId::PairWindow,
            }),
        }
    }

    pub(crate) fn window_timer(&mut self, out: &mut Vec<Output>) {
        self.purge_window(out);
    }

    /// Opens a window with a fresh secret, or the operator's demo PIN, for
    /// `ttl_ms`.
    pub(crate) fn start_pairing(
        &mut self,
        call: CallId,
        method: WindowMethod,
        ttl_ms: u64,
        out: &mut Vec<Output>,
    ) {
        self.purge_window(out);
        let refused = |out: &mut Vec<Output>, refusal| {
            out.push(Output::EndCall {
                call,
                status: Status::Failed(CallError::Window(refusal)),
            })
        };
        if self.pairing.window.is_some() {
            return refused(out, WindowRefusal::AlreadyActive);
        }
        let (secret, method, answer) = match method {
            WindowMethod::Demo(pin) if !valid_pin(&pin) => {
                return refused(out, WindowRefusal::InvalidPinFormat);
            }
            WindowMethod::Demo(pin) => (
                pin.clone().into_bytes(),
                Method::Demo,
                WindowSecret::Pin(pin),
            ),
            WindowMethod::Pin => {
                let Some(random) = self.pairing.entropy.draw() else {
                    return refused(out, WindowRefusal::NoEntropy);
                };
                let value =
                    u32::from_be_bytes(random[..4].try_into().expect("4 bytes")) % 1_000_000;
                let pin = format!("{value:06}");
                (
                    pin.clone().into_bytes(),
                    Method::Pin,
                    WindowSecret::Pin(pin),
                )
            }
            WindowMethod::Qr => {
                let Some(random) = self.pairing.entropy.draw() else {
                    return refused(out, WindowRefusal::NoEntropy);
                };
                let secret = random[..QR_SECRET_LEN].to_vec();
                (secret.clone(), Method::Qr, WindowSecret::Qr(secret))
            }
        };
        self.pairing.locked_out_until_ms = None;
        self.pairing.window = Some(Window {
            secret,
            method,
            failed: 0,
            in_flight: BTreeSet::new(),
            reserved: None,
            expires_at_ms: self.now_ms().saturating_add(ttl_ms),
        });
        self.publish_window(out);
        answered(call, Response::PairingWindow(answer), out);
    }

    /// Closes the window, and lifts a lockout.
    pub(crate) fn cancel_pairing(&mut self, call: CallId, out: &mut Vec<Output>) {
        self.purge_window(out);
        let was_active = self.pairing.window.take().is_some();
        self.pairing.locked_out_until_ms = None;
        self.publish_window(out);
        answered(call, Response::PairingCancelled { was_active }, out);
    }

    /// A pairing conversation's carrier opened.
    pub(crate) fn pairing_opened(
        &mut self,
        session: PairingId,
        opened: Opened,
        out: &mut Vec<Output>,
    ) {
        match opened {
            Opened::Responder { .. } => self.responder_opened(session, opened, out),
            Opened::Initiator(initiate) => self.initiator_opened(session, *initiate, out),
        }
    }

    fn responder_opened(&mut self, session: PairingId, opened: Opened, out: &mut Vec<Output>) {
        self.purge_window(out);
        if self.pairing.window.is_none() && self.pairing.locked_out() {
            return Self::end_pairing(session, Some(PairFailure::locked()), out);
        }
        let now = self.now_ms();
        let Some(window) = self.pairing.window.as_mut() else {
            return Self::end_pairing(session, Some(PairFailure::invalid_pin()), out);
        };
        let spent = window.failed as usize + window.in_flight.len();
        if window.reserved.is_some() || (!window.reusable() && spent >= ATTEMPT_LIMIT as usize) {
            return Self::end_pairing(session, Some(PairFailure::invalid_pin()), out);
        }
        window.in_flight.insert(session);
        let deadline = window
            .expires_at_ms
            .min(now.saturating_add(RESPONDER_PATIENCE_MS));
        let method = window.method;
        self.pairing.sessions.insert(
            session,
            Session {
                opened,
                stage: Stage::AwaitSpake,
                method,
            },
        );
        out.push(Output::ArmTimer {
            id: TimerId::Pairing(session),
            at_ms: deadline,
        });
    }

    fn initiator_opened(&mut self, session: PairingId, initiate: Initiate, out: &mut Vec<Output>) {
        let Some(random) = self.pairing.entropy.draw() else {
            return Self::end_pairing(
                session,
                Some(PairFailure::new(
                    FailureCode::Internal,
                    "failed to generate SPAKE2 scalar",
                )),
                out,
            );
        };
        let (spake, msg_b) = Spake2::start(Side::B, &initiate.secret, random);
        out.push(Output::Pair {
            session,
            message: message(Body::Spake2Message(msg_b.clone())),
        });
        self.pairing.sessions.insert(
            session,
            Session {
                opened: Opened::Initiator(Box::new(initiate)),
                stage: Stage::Started { spake, msg_b },
                method: Method::Pin,
            },
        );
        out.push(Output::ArmTimer {
            id: TimerId::Pairing(session),
            at_ms: self.now_ms().saturating_add(INITIATOR_PATIENCE_MS),
        });
    }

    /// Ends a conversation's carrier: cleanly, or with `failure` as its
    /// status.
    fn end_pairing(session: PairingId, failure: Option<PairFailure>, out: &mut Vec<Output>) {
        out.push(Output::CancelTimer {
            id: TimerId::Pairing(session),
        });
        out.push(Output::PairEnded { session, failure });
    }

    /// Lets go of a responder's attempt without counting it.
    fn release(&mut self, session: PairingId, out: &mut Vec<Output>) {
        if let Some(window) = self.pairing.window.as_mut() {
            window.in_flight.remove(&session);
            if window.reserved == Some(session) {
                window.reserved = None;
            }
        }
        self.purge_window(out);
    }

    /// Counts a responder's wrong secret, closing a one-shot window at its
    /// cap.
    fn record_failure(&mut self, session: PairingId, out: &mut Vec<Output>) {
        if let Some(window) = self.pairing.window.as_mut()
            && window.in_flight.remove(&session)
        {
            window.failed = window.failed.saturating_add(1);
            if !window.reusable() && window.failed >= ATTEMPT_LIMIT {
                self.pairing.locked_out_until_ms = Some(window.expires_at_ms);
                self.pairing.window = None;
            }
        }
        self.publish_window(out);
    }

    fn pairing_event(method: Method, role: Role, outcome: Outcome, out: &mut Vec<Output>) {
        out.push(Output::Shell(ShellWork::Pairing(PairingEvent {
            role,
            method,
            outcome,
        })));
    }

    fn joiner_failed(method: Method, failure: &PairFailure, out: &mut Vec<Output>) {
        Self::pairing_event(
            method,
            Role::Joiner,
            Outcome::Failed {
                reason: failure.message.clone(),
                counted: Some(failure.counted()),
            },
            out,
        );
    }

    fn offerer_failed(
        method: Method,
        reason: &str,
        counted: Option<Failure>,
        out: &mut Vec<Output>,
    ) {
        Self::pairing_event(
            method,
            Role::Offerer,
            Outcome::Failed {
                reason: reason.to_owned(),
                counted,
            },
            out,
        );
    }

    /// The window's side sends an error, and the conversation ends.
    fn responder_refuses(
        &mut self,
        session: PairingId,
        method: Method,
        reason: Reason,
        detail: &str,
        out: &mut Vec<Output>,
    ) {
        if reason == Reason::InvalidPin {
            self.record_failure(session, out);
            Self::offerer_failed(
                method,
                &failure_words(reason, detail),
                Some(Failure::WrongSecret),
                out,
            );
        } else {
            self.release(session, out);
            Self::offerer_failed(method, &failure_words(reason, detail), None, out);
        }
        self.pairing.sessions.remove(&session);
        out.push(Output::Pair {
            session,
            message: error(reason, detail),
        });
        Self::end_pairing(session, None, out);
    }

    /// The window's side lets the conversation go: the peer left.
    fn responder_left(
        &mut self,
        session: PairingId,
        method: Method,
        reason: &str,
        out: &mut Vec<Output>,
    ) {
        self.release(session, out);
        self.pairing.sessions.remove(&session);
        Self::offerer_failed(method, reason, Some(Failure::Abandoned), out);
        Self::end_pairing(session, None, out);
    }

    /// A message of a pairing conversation.
    pub(crate) fn pairing_message(
        &mut self,
        session: PairingId,
        message: PairMessage,
        out: &mut Vec<Output>,
    ) {
        let Some(current) = self.pairing.sessions.get(&session) else {
            return;
        };
        match &current.opened {
            Opened::Responder { .. } => self.responder_message(session, message.body, out),
            Opened::Initiator(_) => self.initiator_message(session, message.body, out),
        }
    }

    /// The carrier ended from the other side, with the status it ended
    /// with, if any.
    pub(crate) fn pairing_closed(
        &mut self,
        session: PairingId,
        status: Option<PairFailure>,
        out: &mut Vec<Output>,
    ) {
        let Some(current) = self.pairing.sessions.get(&session) else {
            return;
        };
        let method = current.method;
        match (&current.opened, &current.stage) {
            (Opened::Responder { .. }, Stage::Committing) => {
                // The commit goes on: the window's side owes nothing more
                // to a peer that has gone.
            }
            (Opened::Responder { .. }, stage) => {
                let before = match stage {
                    Stage::AwaitSpake => "SPAKE2 message",
                    Stage::AwaitConfirmation { .. } => "key confirmation",
                    _ => "sealed identity",
                };
                self.responder_left(
                    session,
                    method,
                    &format!("pairing stream closed before {before}"),
                    out,
                );
            }
            // The peer committed this host before its carrier went: the
            // commit here goes on.
            (Opened::Initiator(_), Stage::Joining { .. }) => {}
            (Opened::Initiator(_), _) => {
                let failure = match (status, &current.stage) {
                    (Some(status), _) => status,
                    (None, Stage::Abandoning { .. }) => PairFailure::new(
                        FailureCode::Unavailable,
                        "pairing stream closed before abandonment acknowledgement",
                    ),
                    (None, _) => PairFailure::closed(),
                };
                self.initiator_fails(session, failure, out);
            }
        }
    }

    /// A conversation's deadline, or a pending pairing's token, ran out.
    pub(crate) fn pairing_timer(&mut self, session: PairingId, out: &mut Vec<Output>) {
        let Some(current) = self.pairing.sessions.get(&session) else {
            return;
        };
        match (&current.opened, &current.stage) {
            (Opened::Responder { .. }, stage) => {
                let method = current.method;
                if *stage == Stage::Committing {
                    // The commit that lands after this is undone.
                    if let Some(commit) = self.pairing.committing.remove(&session) {
                        self.pairing.abandoned.insert(session, commit);
                    }
                }
                self.release(session, out);
                self.pairing.sessions.remove(&session);
                Self::offerer_failed(method, "PAIRING_TIMEOUT", Some(Failure::TimedOut), out);
                Self::end_pairing(session, Some(PairFailure::timeout()), out);
            }
            (Opened::Initiator(_), Stage::Pending { .. }) => {
                self.forget_pending(session);
                Self::end_pairing(session, None, out);
            }
            (Opened::Initiator(_), Stage::Completing { .. } | Stage::Abandoning { .. }) => {
                self.initiator_fails(session, PairFailure::invalid_pin(), out);
            }
            (Opened::Initiator(_), _) => self.initiator_fails(session, PairFailure::timeout(), out),
        }
    }

    fn responder_message(&mut self, session: PairingId, body: Option<Body>, out: &mut Vec<Output>) {
        let current = self.pairing.sessions.get(&session).expect("a session");
        let method = current.method;
        let Opened::Responder {
            local,
            reachability,
        } = &current.opened
        else {
            unreachable!("a responder");
        };
        let (local, reachability) = (local.clone(), reachability.clone());
        let expected = match current.stage {
            Stage::AwaitSpake => "expected SPAKE2 initiator message",
            Stage::AwaitConfirmation { .. } => "expected key confirmation",
            Stage::AwaitIdentity { .. } => "expected sealed identity",
            // Nothing more is read once the commit is under way.
            _ => return,
        };
        match (current.stage.clone(), body) {
            (_, Some(Body::Error(peer_error))) => {
                let rejected = peer_error.reason == Reason::UserRejected as i32
                    && matches!(current.stage, Stage::AwaitIdentity { .. });
                // The attempt is released before any acknowledgement, so a
                // joiner that rejected the pairing can begin again at once.
                self.release(session, out);
                self.pairing.sessions.remove(&session);
                Self::offerer_failed(
                    method,
                    "peer rejected pairing",
                    Some(Failure::Abandoned),
                    out,
                );
                if rejected {
                    out.push(Output::Pair {
                        session,
                        message: message(Body::PairingAbandoned(wire::PairingAbandoned {})),
                    });
                }
                Self::end_pairing(session, None, out);
            }
            (Stage::AwaitSpake, Some(Body::Spake2Message(msg_b))) => {
                let Some(random) = self.pairing.entropy.draw() else {
                    self.release(session, out);
                    self.pairing.sessions.remove(&session);
                    return Self::end_pairing(
                        session,
                        Some(PairFailure::new(
                            FailureCode::Internal,
                            "failed to generate SPAKE2 scalar",
                        )),
                        out,
                    );
                };
                let secret = self
                    .pairing
                    .window
                    .as_ref()
                    .map(|window| window.secret.clone())
                    .unwrap_or_default();
                let (spake, msg_a) = Spake2::start(Side::A, &secret, random);
                out.push(Output::Pair {
                    session,
                    message: message(Body::Spake2Message(msg_a.clone())),
                });
                let Some(shared) = spake.finish(&msg_b) else {
                    return self.responder_refuses(
                        session,
                        method,
                        Reason::InvalidPin,
                        "invalid PIN",
                        out,
                    );
                };
                let keys = Keys::derive(&shared, &msg_b, &msg_a);
                self.set_pairing_stage(session, Stage::AwaitConfirmation { keys });
            }
            (Stage::AwaitConfirmation { keys }, Some(Body::KeyConfirmation(tag))) => {
                if !keys.confirms(Side::B, &tag) {
                    return self.responder_refuses(
                        session,
                        method,
                        Reason::InvalidPin,
                        "invalid PIN",
                        out,
                    );
                }
                out.push(Output::Pair {
                    session,
                    message: message(Body::KeyConfirmation(keys.confirmation(Side::A))),
                });
                let now = self.now_ms();
                let remaining = self
                    .pairing
                    .window
                    .as_ref()
                    .map_or(0, |window| window.expires_at_ms.saturating_sub(now))
                    .min(RESPONDER_PATIENCE_MS);
                let identity = wire::PairingIdentity {
                    expires_at_ms: (now + remaining) as i64,
                    ..local
                };
                out.push(Output::Pair {
                    session,
                    message: message(Body::SealedIdentity(keys.seal(Side::A, &identity))),
                });
                self.set_pairing_stage(session, Stage::AwaitIdentity { keys });
            }
            (Stage::AwaitIdentity { keys }, Some(Body::SealedIdentity(sealed))) => {
                let Some(peer) = keys.open(Side::B, sealed) else {
                    return self.responder_refuses(
                        session,
                        method,
                        Reason::InvalidPin,
                        "invalid PIN",
                        out,
                    );
                };
                let host = match validate_identity(&peer) {
                    Ok(host) => host,
                    Err(detail) => {
                        return self.responder_refuses(
                            session,
                            method,
                            Reason::ProtocolViolation,
                            detail,
                            out,
                        );
                    }
                };
                if is_self(&local, &peer) {
                    return self.responder_refuses(
                        session,
                        method,
                        Reason::SelfPairing,
                        "self pairing is not allowed",
                        out,
                    );
                }
                self.responder_commits(session, host, peer, reachability, out);
            }
            (_, _) => {
                self.responder_refuses(session, method, Reason::ProtocolViolation, expected, out)
            }
        }
    }

    fn set_pairing_stage(&mut self, session: PairingId, stage: Stage) {
        if let Some(current) = self.pairing.sessions.get_mut(&session) {
            current.stage = stage;
        }
    }

    /// The peer proved the secret: the window is held for this
    /// conversation while the peer's key is written.
    fn responder_commits(
        &mut self,
        session: PairingId,
        host: HostId,
        peer: wire::PairingIdentity,
        reachability: Option<Reachability>,
        out: &mut Vec<Output>,
    ) {
        let method = self.pairing.sessions[&session].method;
        let held = self
            .pairing
            .window
            .as_mut()
            .filter(|window| window.reserved.is_none() && window.in_flight.contains(&session));
        let Some(window) = held else {
            self.release(session, out);
            self.pairing.sessions.remove(&session);
            Self::offerer_failed(method, "INVALID_PIN", Some(Failure::Other), out);
            return Self::end_pairing(session, Some(PairFailure::invalid_pin()), out);
        };
        window.in_flight.remove(&session);
        window.reserved = Some(session);
        self.set_pairing_stage(session, Stage::Committing);
        self.publish_window(out);
        self.commit_peer(session, host, peer, reachability, out);
    }

    /// Writes `peer`'s key for the conversation `session`.
    fn commit_peer(
        &mut self,
        session: PairingId,
        host: HostId,
        peer: wire::PairingIdentity,
        reachability: Option<Reachability>,
        out: &mut Vec<Output>,
    ) {
        let host = Uuid::from_bytes(host.0);
        let before = self.trust().entry(host).cloned();
        self.pairing.committing.insert(session, (host, before));
        self.change_trust(
            TrustChange::Pair {
                host,
                pubkey: peer.pubkey,
                name: peer.name,
                reachability,
            },
            Some(Waiter::Pairing(session)),
            out,
        );
    }

    /// A pairing's commit is on the drive, or was refused.
    pub(crate) fn pairing_committed(
        &mut self,
        session: PairingId,
        result: Result<TrustChanged, TrustError>,
        out: &mut Vec<Output>,
    ) {
        let commit = self.pairing.committing.remove(&session);
        if let Some((host, before)) = self.pairing.abandoned.remove(&session).or_else(|| {
            commit
                .clone()
                .filter(|_| !self.pairing.sessions.contains_key(&session))
        }) {
            if result.is_ok() {
                self.change_trust(
                    TrustChange::Restore {
                        host,
                        entry: before,
                    },
                    None,
                    out,
                );
            }
            return;
        }
        let Some(current) = self.pairing.sessions.get(&session) else {
            return;
        };
        let method = current.method;
        match (&current.opened, &current.stage, result) {
            (Opened::Responder { .. }, _, Ok(_)) => {
                let consumed = match self.pairing.window.as_mut() {
                    Some(window) if window.reserved == Some(session) => {
                        if window.reusable() {
                            window.reserved = None;
                        } else {
                            self.pairing.window = None;
                        }
                        true
                    }
                    _ => false,
                };
                self.pairing.sessions.remove(&session);
                let Some((host, before)) = commit else {
                    return;
                };
                if !consumed {
                    // The window closed while the key was written: the
                    // pairing it was for is undone.
                    self.change_trust(
                        TrustChange::Restore {
                            host,
                            entry: before,
                        },
                        None,
                        out,
                    );
                    self.purge_window(out);
                    Self::offerer_failed(method, "INVALID_PIN", Some(Failure::Other), out);
                    return Self::end_pairing(session, Some(PairFailure::invalid_pin()), out);
                }
                self.purge_window(out);
                out.push(Output::Pair {
                    session,
                    message: message(Body::PairingComplete(wire::PairingComplete {})),
                });
                Self::pairing_event(
                    method,
                    Role::Offerer,
                    Outcome::Succeeded {
                        host: HostId(*host.as_bytes()),
                    },
                    out,
                );
                Self::end_pairing(session, None, out);
            }
            (Opened::Responder { .. }, _, Err(error)) => {
                self.release(session, out);
                self.pairing.sessions.remove(&session);
                let failure = PairFailure::from_trust(&error);
                Self::offerer_failed(method, &failure.message, Some(Failure::Other), out);
                Self::end_pairing(session, Some(failure), out);
            }
            (Opened::Initiator(initiate), Stage::Joining { call, host }, result) => {
                let (call, host) = (*call, *host);
                let (method, reachability) = (initiate.method, initiate.reachability.clone());
                self.pairing.sessions.remove(&session);
                Self::end_pairing(session, None, out);
                match result {
                    Ok(_) => {
                        Self::pairing_event(method, Role::Joiner, Outcome::Succeeded { host }, out);
                        answered(
                            call,
                            Response::Paired {
                                host,
                                reachability: Some(reachability),
                            },
                            out,
                        );
                    }
                    Err(error) => {
                        let failure = PairFailure::from_trust(&error).opaque();
                        Self::joiner_failed(method, &failure, out);
                        out.push(Output::EndCall {
                            call,
                            status: Status::Failed(CallError::Pairing(failure)),
                        });
                    }
                }
            }
            (Opened::Initiator(_), _, _) => {}
        }
    }

    fn initiator_message(&mut self, session: PairingId, body: Option<Body>, out: &mut Vec<Output>) {
        let current = self.pairing.sessions.get(&session).expect("a session");
        let Opened::Initiator(initiate) = &current.opened else {
            unreachable!("an initiator");
        };
        let initiate = initiate.clone();
        match (current.stage.clone(), body) {
            (Stage::Abandoning { call }, Some(Body::PairingAbandoned(_))) => {
                self.forget_pending(session);
                Self::end_pairing(session, None, out);
                out.push(Output::EndCall {
                    call,
                    status: Status::Ok,
                });
            }
            (Stage::Completing { call, peer }, Some(Body::PairingComplete(_))) => {
                let host = validate_identity(&peer).expect("validated when it arrived");
                self.set_pairing_stage(session, Stage::Joining { call, host });
                out.push(Output::CancelTimer {
                    id: TimerId::Pairing(session),
                });
                self.commit_peer(
                    session,
                    host,
                    peer,
                    Some(initiate.reachability.clone()),
                    out,
                );
            }
            (Stage::Joining { .. } | Stage::Pending { .. }, _) => {}
            (_, Some(Body::Error(peer_error))) => {
                self.initiator_fails(session, PairFailure::from_peer(&peer_error), out);
            }
            (Stage::Started { spake, msg_b }, Some(Body::Spake2Message(msg_a))) => {
                let Some(shared) = spake.finish(&msg_a) else {
                    return self.initiator_fails(session, PairFailure::invalid_pin(), out);
                };
                let keys = Keys::derive(&shared, &msg_b, &msg_a);
                out.push(Output::Pair {
                    session,
                    message: message(Body::KeyConfirmation(keys.confirmation(Side::B))),
                });
                self.set_pairing_stage(session, Stage::Confirming { keys });
            }
            (Stage::Confirming { keys }, Some(Body::KeyConfirmation(tag))) => {
                if !keys.confirms(Side::A, &tag) {
                    return self.initiator_fails(session, PairFailure::invalid_pin(), out);
                }
                self.set_pairing_stage(session, Stage::Identifying { keys });
            }
            (Stage::Identifying { keys }, Some(Body::SealedIdentity(sealed))) => {
                let Some(peer) = keys.open(Side::A, sealed) else {
                    return self.initiator_fails(session, PairFailure::invalid_pin(), out);
                };
                let host = match validate_identity(&peer) {
                    Ok(host) => host,
                    Err(detail) => {
                        return self.initiator_fails(
                            session,
                            PairFailure::new(FailureCode::InvalidArgument, detail),
                            out,
                        );
                    }
                };
                if is_self(&initiate.local, &peer) {
                    return self.initiator_fails(
                        session,
                        PairFailure::new(FailureCode::InvalidArgument, "SELF_PAIRING"),
                        out,
                    );
                }
                self.pending(session, &initiate, keys, host, peer, out);
            }
            (Stage::Started { .. }, _) => self.initiator_fails(
                session,
                PairFailure::violation("expected SPAKE2 responder message"),
                out,
            ),
            (Stage::Confirming { .. }, _) => self.initiator_fails(
                session,
                PairFailure::violation("expected key confirmation"),
                out,
            ),
            (Stage::Identifying { .. }, _) => self.initiator_fails(
                session,
                PairFailure::violation("expected sealed identity"),
                out,
            ),
            (Stage::Completing { .. }, _) => self.initiator_fails(
                session,
                PairFailure::new(FailureCode::InvalidArgument, "expected pairing completion"),
                out,
            ),
            (Stage::Abandoning { .. }, _) => {
                self.initiator_fails(session, PairFailure::closed(), out)
            }
            (_, _) => {}
        }
    }

    /// The joining side learned who the peer is: it waits for a person to
    /// confirm or abandon the pairing, until the peer's window would close.
    fn pending(
        &mut self,
        session: PairingId,
        initiate: &Initiate,
        keys: Keys,
        host: HostId,
        peer: wire::PairingIdentity,
        out: &mut Vec<Output>,
    ) {
        if initiate.expect.is_some_and(|expected| expected != host) {
            return self.initiator_fails(session, PairFailure::invalid_pin(), out);
        }
        // A machine discovery found that already has a key here must
        // present that key: a stranger answering at its address does not
        // replace it.
        if initiate.found
            && self
                .trust()
                .pubkey_for_host(Uuid::from_bytes(host.0))
                .is_some_and(|pinned| pinned != peer.pubkey.as_slice())
        {
            return self.initiator_fails(session, PairFailure::invalid_pin(), out);
        }
        let now = self.now_ms() as i64;
        if peer.expires_at_ms <= now {
            return self.initiator_fails(session, PairFailure::invalid_pin(), out);
        }
        if self.pairing.tokens.len() >= MAX_PENDING {
            return self.initiator_fails(
                session,
                PairFailure::new(FailureCode::ResourceExhausted, "too many pending pairings"),
                out,
            );
        }
        let Some(random) = self.pairing.entropy.draw() else {
            return self.initiator_fails(
                session,
                PairFailure::new(FailureCode::Internal, "no entropy for a pairing token"),
                out,
            );
        };
        let token = random[..16].to_vec();
        let local = wire::PairingIdentity {
            expires_at_ms: 0,
            ..initiate.local.clone()
        };
        let sealed = keys.seal(Side::B, &local);
        self.pairing.tokens.insert(token.clone(), session);
        out.push(Output::ArmTimer {
            id: TimerId::Pairing(session),
            at_ms: peer.expires_at_ms as u64,
        });
        out.push(Output::PairPending {
            session,
            token: token.clone(),
            peer: peer.clone(),
        });
        self.set_pairing_stage(
            session,
            Stage::Pending {
                token,
                peer,
                sealed,
            },
        );
    }

    fn forget_pending(&mut self, session: PairingId) {
        self.pairing.sessions.remove(&session);
        self.pairing.tokens.retain(|_, pending| *pending != session);
    }

    /// The joining side's conversation failed: told to whoever waits on it.
    fn initiator_fails(&mut self, session: PairingId, failure: PairFailure, out: &mut Vec<Output>) {
        let waiting = self.pairing.sessions.get(&session).and_then(|current| {
            let Opened::Initiator(initiate) = &current.opened else {
                return None;
            };
            match current.stage {
                Stage::Completing { call, .. } => Some((call, Some(initiate.method))),
                Stage::Abandoning { call } => Some((call, None)),
                _ => None,
            }
        });
        self.forget_pending(session);
        match waiting {
            Some((call, confirming)) => {
                let failure = failure.opaque();
                if let Some(method) = confirming {
                    Self::joiner_failed(method, &failure, out);
                }
                Self::end_pairing(session, None, out);
                out.push(Output::EndCall {
                    call,
                    status: Status::Failed(CallError::Pairing(failure)),
                });
            }
            None => Self::end_pairing(session, Some(failure), out),
        }
    }

    fn take_token(
        &mut self,
        call: CallId,
        token: &[u8],
        out: &mut Vec<Output>,
    ) -> Option<PairingId> {
        let session = self.pairing.tokens.remove(token);
        if session.is_none() {
            out.push(Output::EndCall {
                call,
                status: Status::Failed(CallError::Pairing(PairFailure::invalid_pin())),
            });
        }
        session
    }

    /// A person confirmed a pending pairing: this side's identity goes to
    /// the peer, and once the peer committed it, the peer's key is written.
    pub(crate) fn confirm_pair(&mut self, call: CallId, token: Vec<u8>, out: &mut Vec<Output>) {
        let Some(session) = self.take_token(call, &token, out) else {
            return;
        };
        let Some(Stage::Pending { peer, sealed, .. }) = self
            .pairing
            .sessions
            .get(&session)
            .map(|current| current.stage.clone())
        else {
            return;
        };
        if self.now_ms() as i64 >= peer.expires_at_ms {
            self.set_pairing_stage(session, Stage::Completing { call, peer });
            return self.initiator_fails(session, PairFailure::invalid_pin(), out);
        }
        out.push(Output::Pair {
            session,
            message: message(Body::SealedIdentity(sealed)),
        });
        out.push(Output::ArmTimer {
            id: TimerId::Pairing(session),
            at_ms: self.now_ms().saturating_add(INITIATOR_PATIENCE_MS),
        });
        self.set_pairing_stage(session, Stage::Completing { call, peer });
    }

    /// A person abandoned a pending pairing: the peer is told, and the
    /// call answered once it acknowledged.
    pub(crate) fn abandon_pair(&mut self, call: CallId, token: Vec<u8>, out: &mut Vec<Output>) {
        let Some(session) = self.take_token(call, &token, out) else {
            return;
        };
        if let Some(Opened::Initiator(initiate)) = self
            .pairing
            .sessions
            .get(&session)
            .map(|current| &current.opened)
        {
            Self::pairing_event(
                initiate.method,
                Role::Joiner,
                Outcome::Failed {
                    reason: "pairing abandoned".to_owned(),
                    counted: Some(Failure::Abandoned),
                },
                out,
            );
        }
        out.push(Output::Pair {
            session,
            message: error(Reason::UserRejected, ""),
        });
        out.push(Output::ArmTimer {
            id: TimerId::Pairing(session),
            at_ms: self.now_ms().saturating_add(INITIATOR_PATIENCE_MS),
        });
        self.set_pairing_stage(session, Stage::Abandoning { call });
    }

    /// Trusts a peer whose identity an SSH exchange carried. `joined` is
    /// how this host reaches it, when this host dialled.
    pub(crate) fn pair_over_ssh(
        &mut self,
        call: CallId,
        local: wire::PairingIdentity,
        peer: wire::PairingIdentity,
        reachability: Option<Reachability>,
        out: &mut Vec<Output>,
    ) {
        let host = match validate_identity(&peer) {
            Ok(host) => host,
            Err(detail) => {
                return out.push(Output::EndCall {
                    call,
                    status: Status::Failed(CallError::Pairing(PairFailure::new(
                        FailureCode::InvalidArgument,
                        format!("PairingIdentity.{detail}"),
                    ))),
                });
            }
        };
        if is_self(&local, &peer) {
            return out.push(Output::EndCall {
                call,
                status: Status::Failed(CallError::Pairing(PairFailure::new(
                    FailureCode::InvalidArgument,
                    "SELF_PAIRING",
                ))),
            });
        }
        let role = if reachability.is_some() {
            Role::Joiner
        } else {
            Role::Offerer
        };
        self.change_trust(
            TrustChange::Pair {
                host: Uuid::from_bytes(host.0),
                pubkey: peer.pubkey,
                name: peer.name,
                reachability,
            },
            Some(Waiter::Ssh { call, host, role }),
            out,
        );
    }

    /// An SSH pairing's commit is on the drive, or was refused.
    pub(crate) fn ssh_committed(
        &mut self,
        call: CallId,
        host: HostId,
        role: Role,
        result: Result<TrustChanged, TrustError>,
        out: &mut Vec<Output>,
    ) {
        match result {
            Ok(_) => {
                Self::pairing_event(Method::Ssh, role, Outcome::Succeeded { host }, out);
                answered(
                    call,
                    Response::Paired {
                        host,
                        reachability: None,
                    },
                    out,
                );
            }
            Err(error) => out.push(Output::EndCall {
                call,
                status: Status::Failed(CallError::Pairing(PairFailure::from_trust(&error))),
            }),
        }
    }
}

#[cfg(test)]
mod tests;
