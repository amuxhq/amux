#![allow(dead_code)]

pub(crate) mod qr;
pub(crate) mod ssh;

use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ring::rand::{SecureRandom as _, SystemRandom};

use crate::audit;

/// Stores the trust an SSH identity exchange established: the command
/// running either end of an SSH pairing implements this over the profile's
/// front door.
#[async_trait::async_trait]
pub trait PairingAdmin: Sync {
    fn profile_id(&self) -> crate::ProfileId;
    async fn pair_ssh_peer(
        &self,
        peer: ssh::SshPairingPeer,
        target: Option<ssh::SshTarget>,
    ) -> Result<(), String>;
}

pub(crate) const QR_SECRET_LEN: usize = 32;
pub(crate) const PAIR_MODE_TTL: Duration = Duration::from_secs(5 * 60);
pub(crate) const PAIR_ATTEMPT_LIMIT: u8 = 5;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PairModeError {
    #[error("pairing mode is already active")]
    AlreadyActive,
    #[error("pairing mode is not active")]
    NotActive,
    #[error("PIN must be six decimal digits")]
    InvalidPinFormat,
    #[error("failed to generate pairing secret")]
    SecretGeneration,
    /// Too many wrong secrets closed the window; a new one is needed.
    #[error("pairing mode closed after too many wrong secrets")]
    LockedOut,
}

/// The one-shot pairing secret with its attempt accounting. The delivery
/// mechanism — a typed 6-digit PIN or a QR-carried 256-bit secret — only
/// decides the bytes (and the audit label); the SPAKE2 wire protocol
/// consuming them is the same, so the one-shot/TTL/attempt-cap rules are
/// uniform too.
#[derive(Debug)]
struct PairSecret {
    value: Vec<u8>,
    method: &'static str,
    failed_attempts: u8,
    in_flight_attempts: u8,
    reserved: bool,
    /// A demo secret survives successes and failures until it expires or is
    /// cancelled: it exists so an operator can hand a fixed PIN to someone
    /// (an app reviewer) who has no access to this machine and may pair more
    /// than once. Concurrent attempts still serialise through `reserved`.
    reusable: bool,
}

#[derive(Debug)]
struct PairModeSession {
    id: u64,
    secret: PairSecret,
    expires_at: Instant,
}

#[derive(Debug, Default)]
pub struct PairMode {
    state: Arc<Mutex<PairModeState>>,
}

#[derive(Debug, Default)]
struct PairModeState {
    next_session_id: u64,
    session: Option<PairModeSession>,
    /// The window that too many wrong secrets closed would have lasted
    /// until then: until a new window opens, or that time, an attempt is
    /// told it is locked out rather than that its secret did not match.
    locked_out_until: Option<Instant>,
}

pub(crate) struct PairModeAttempt {
    state: Arc<Mutex<PairModeState>>,
    session_id: u64,
    secret: Vec<u8>,
    method: &'static str,
    active: bool,
    expires_at: Instant,
}

pub(crate) struct PairModeCommit {
    state: Arc<Mutex<PairModeState>>,
    session_id: u64,
    active: bool,
}

impl PairMode {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn is_active(&self) -> bool {
        let mut state = self.state.lock().expect("pair mode mutex poisoned");
        purge_expired(&mut state);
        state.session.is_some()
    }

    /// Whether too many wrong secrets closed the last window, and no new
    /// one has opened since.
    pub(crate) fn is_locked_out(&self) -> bool {
        let mut state = self.state.lock().expect("pair mode mutex poisoned");
        purge_expired(&mut state);
        state.locked_out_until.is_some()
    }

    pub(crate) fn start_pin(&self) -> Result<String, PairModeError> {
        let pin = generate_pin()?;
        self.start_pin_for_duration(pin.clone(), PAIR_MODE_TTL)?;
        Ok(pin)
    }

    pub fn start_pin_for_duration(&self, pin: String, ttl: Duration) -> Result<(), PairModeError> {
        validate_pin(&pin)?;
        self.start_session(
            PairSecret {
                value: pin.into_bytes(),
                method: "pin",
                failed_attempts: 0,
                in_flight_attempts: 0,
                reserved: false,
                reusable: false,
            },
            ttl,
        )
    }

    /// Start a reusable fixed-PIN session for unattended demos. Unlike
    /// one-shot sessions the PIN is chosen by the operator, is not consumed
    /// by success, and is not locked out by failed attempts.
    pub(crate) fn start_demo_pin(&self, pin: String, ttl: Duration) -> Result<(), PairModeError> {
        validate_pin(&pin)?;
        self.start_session(
            PairSecret {
                value: pin.into_bytes(),
                method: "demo",
                failed_attempts: 0,
                in_flight_attempts: 0,
                reserved: false,
                reusable: true,
            },
            ttl,
        )
    }

    pub(crate) fn start_qr_secret_for_duration(
        &self,
        secret: [u8; QR_SECRET_LEN],
        ttl: Duration,
    ) -> Result<(), PairModeError> {
        self.start_session(
            PairSecret {
                value: secret.to_vec(),
                method: "qr",
                failed_attempts: 0,
                in_flight_attempts: 0,
                reserved: false,
                reusable: false,
            },
            ttl,
        )
    }

    pub(crate) fn begin_attempt(&self) -> Result<PairModeAttempt, PairModeError> {
        let mut state = self.state.lock().expect("pair mode mutex poisoned");
        purge_expired(&mut state);
        if state.session.is_none() && state.locked_out_until.is_some() {
            return Err(PairModeError::LockedOut);
        }
        let Some(session) = state.session.as_mut() else {
            return Err(PairModeError::NotActive);
        };
        let secret = &mut session.secret;
        if secret.reserved {
            return Err(PairModeError::NotActive);
        }
        if !secret.reusable
            && secret
                .failed_attempts
                .saturating_add(secret.in_flight_attempts)
                >= PAIR_ATTEMPT_LIMIT
        {
            return Err(PairModeError::NotActive);
        }
        secret.in_flight_attempts = secret.in_flight_attempts.saturating_add(1);
        Ok(PairModeAttempt {
            state: self.state.clone(),
            session_id: session.id,
            secret: secret.value.clone(),
            method: secret.method,
            active: true,
            expires_at: session.expires_at,
        })
    }

    pub(crate) fn record_failure(
        &self,
        attempt: &mut PairModeAttempt,
    ) -> Result<(), PairModeError> {
        let mut state = self.state.lock().expect("pair mode mutex poisoned");
        purge_expired(&mut state);
        if !attempt.active {
            return Err(PairModeError::NotActive);
        }
        let Some(session) = state.session.as_mut() else {
            return Err(PairModeError::NotActive);
        };
        if session.id != attempt.session_id {
            return Err(PairModeError::NotActive);
        }
        let secret = &mut session.secret;
        if secret.reserved {
            return Err(PairModeError::NotActive);
        }
        secret.in_flight_attempts = secret.in_flight_attempts.saturating_sub(1);
        secret.failed_attempts = secret.failed_attempts.saturating_add(1);
        if !secret.reusable && secret.failed_attempts >= PAIR_ATTEMPT_LIMIT {
            state.locked_out_until = Some(session.expires_at);
            state.session = None;
        }
        attempt.active = false;
        Ok(())
    }

    pub(crate) fn begin_commit(
        &self,
        attempt: &mut PairModeAttempt,
    ) -> Result<PairModeCommit, PairModeError> {
        let mut state = self.state.lock().expect("pair mode mutex poisoned");
        purge_expired(&mut state);
        if !attempt.active {
            return Err(PairModeError::NotActive);
        }
        let Some(session) = state.session.as_mut() else {
            return Err(PairModeError::NotActive);
        };
        if session.id != attempt.session_id {
            return Err(PairModeError::NotActive);
        }
        let secret = &mut session.secret;
        if secret.reserved {
            return Err(PairModeError::NotActive);
        }
        secret.in_flight_attempts = secret.in_flight_attempts.saturating_sub(1);
        secret.reserved = true;
        attempt.active = false;
        Ok(PairModeCommit {
            state: self.state.clone(),
            session_id: attempt.session_id,
            active: true,
        })
    }

    pub(crate) fn complete_success(
        &self,
        commit: &mut PairModeCommit,
    ) -> Result<(), PairModeError> {
        let mut state = self.state.lock().expect("pair mode mutex poisoned");
        let Some(session) = state.session.as_mut() else {
            return Err(PairModeError::NotActive);
        };
        if session.id != commit.session_id {
            return Err(PairModeError::NotActive);
        }
        if !session.secret.reserved {
            return Err(PairModeError::NotActive);
        }
        if session.secret.reusable {
            session.secret.reserved = false;
        } else {
            state.session = None;
        }
        commit.active = false;
        Ok(())
    }

    pub(crate) fn abort_commit(&self, commit: &mut PairModeCommit) {
        commit.abort();
    }

    pub(crate) fn cancel(&self) -> bool {
        let mut state = self.state.lock().expect("pair mode mutex poisoned");
        let was_active = state.session.is_some();
        state.session = None;
        state.locked_out_until = None;
        was_active
    }

    fn start_session(&self, secret: PairSecret, ttl: Duration) -> Result<(), PairModeError> {
        let mut state = self.state.lock().expect("pair mode mutex poisoned");
        purge_expired(&mut state);
        if state.session.is_some() {
            return Err(PairModeError::AlreadyActive);
        }
        let id = state.next_session_id;
        state.next_session_id = state.next_session_id.wrapping_add(1);
        state.locked_out_until = None;
        state.session = Some(PairModeSession {
            id,
            secret,
            expires_at: Instant::now() + ttl,
        });
        Ok(())
    }
}

impl PairModeAttempt {
    pub(crate) fn remaining(&self) -> Duration {
        self.expires_at.saturating_duration_since(Instant::now())
    }

    /// How the secret travels, for analytics: a typed PIN, demo or not, or
    /// a QR code.
    pub(crate) fn method(&self) -> analytics::Method {
        match self.method {
            "qr" => analytics::Method::Qr,
            _ => analytics::Method::Pin,
        }
    }

    /// The SPAKE2 password bytes for this attempt: the PIN's ASCII digits
    /// or the QR secret's 32 raw bytes.
    pub(crate) fn secret(&self) -> &[u8] {
        &self.secret
    }

    fn abort(&mut self) {
        if !self.active {
            return;
        }
        let mut state = self.state.lock().expect("pair mode mutex poisoned");
        if let Some(session) = state.session.as_mut()
            && session.id == self.session_id
        {
            session.secret.in_flight_attempts = session.secret.in_flight_attempts.saturating_sub(1);
        }
        purge_expired(&mut state);
        self.active = false;
    }
}

impl fmt::Debug for PairModeAttempt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PairModeAttempt")
            .field("session_id", &self.session_id)
            .field("active", &self.active)
            .finish()
    }
}

impl PartialEq for PairModeAttempt {
    fn eq(&self, other: &Self) -> bool {
        self.session_id == other.session_id && self.active == other.active
    }
}

impl Eq for PairModeAttempt {}

impl fmt::Debug for PairModeCommit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PairModeCommit")
            .field("session_id", &self.session_id)
            .field("active", &self.active)
            .finish()
    }
}

impl PartialEq for PairModeCommit {
    fn eq(&self, other: &Self) -> bool {
        self.session_id == other.session_id && self.active == other.active
    }
}

impl Eq for PairModeCommit {}

impl PairModeCommit {
    fn abort(&mut self) {
        if !self.active {
            return;
        }
        let mut state = self.state.lock().expect("pair mode mutex poisoned");
        if let Some(session) = state.session.as_mut()
            && session.id == self.session_id
        {
            session.secret.reserved = false;
        }
        purge_expired(&mut state);
        self.active = false;
    }
}

impl Drop for PairModeAttempt {
    fn drop(&mut self) {
        self.abort();
    }
}

impl Drop for PairModeCommit {
    fn drop(&mut self) {
        self.abort();
    }
}

fn purge_expired(state: &mut PairModeState) {
    if state
        .locked_out_until
        .is_some_and(|until| Instant::now() >= until)
    {
        state.locked_out_until = None;
    }
    let Some(session) = state.session.as_ref() else {
        return;
    };
    let expired = Instant::now() >= session.expires_at && !session.secret.reserved;
    if !expired {
        return;
    }
    let method = session.secret.method;
    state.session = None;
    audit::pairing_failure(method, "pair mode expired");
}

fn generate_pin() -> Result<String, PairModeError> {
    let rng = SystemRandom::new();
    let mut bytes = [0_u8; 4];
    rng.fill(&mut bytes)
        .map_err(|_| PairModeError::SecretGeneration)?;
    let value = u32::from_be_bytes(bytes) % 1_000_000;
    Ok(format!("{value:06}"))
}

fn validate_pin(pin: &str) -> Result<(), PairModeError> {
    if pin.len() == 6 && pin.bytes().all(|byte| byte.is_ascii_digit()) {
        Ok(())
    } else {
        Err(PairModeError::InvalidPinFormat)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qr_secret_mode_is_active_until_the_secret_is_consumed() {
        let pair_mode = PairMode::new();
        let secret = [7_u8; QR_SECRET_LEN];

        pair_mode
            .start_qr_secret_for_duration(secret, Duration::from_secs(60))
            .unwrap();

        assert!(pair_mode.is_active());
        let mut attempt = pair_mode.begin_attempt().unwrap();
        assert_eq!(attempt.secret(), secret.as_slice());
        let mut commit = pair_mode.begin_commit(&mut attempt).unwrap();
        pair_mode.complete_success(&mut commit).unwrap();
        assert!(!pair_mode.is_active());
        assert_eq!(pair_mode.begin_attempt(), Err(PairModeError::NotActive));
    }

    #[test]
    fn active_pair_mode_blocks_new_secret_until_cancelled_or_expired() {
        let pair_mode = PairMode::new();

        pair_mode
            .start_pin_for_duration("123456".to_string(), Duration::from_secs(60))
            .unwrap();

        assert_eq!(
            pair_mode.start_qr_secret_for_duration([1_u8; QR_SECRET_LEN], Duration::from_secs(60)),
            Err(PairModeError::AlreadyActive)
        );

        pair_mode.cancel();
        pair_mode
            .start_qr_secret_for_duration([1_u8; QR_SECRET_LEN], Duration::from_secs(60))
            .unwrap();
    }

    #[test]
    fn expired_pair_mode_is_not_active_and_allows_new_secret() {
        let pair_mode = PairMode::new();

        pair_mode
            .start_pin_for_duration("123456".to_string(), Duration::ZERO)
            .unwrap();

        assert!(!pair_mode.is_active());
        pair_mode
            .start_qr_secret_for_duration([1_u8; QR_SECRET_LEN], Duration::from_secs(60))
            .unwrap();
        assert!(pair_mode.is_active());
    }

    #[test]
    fn attempt_exposes_the_pin_and_cancels_after_failed_attempt_cap() {
        let pair_mode = PairMode::new();

        pair_mode
            .start_pin_for_duration("123456".to_string(), Duration::from_secs(60))
            .unwrap();

        let mut attempt = pair_mode.begin_attempt().unwrap();
        assert_eq!(attempt.secret(), b"123456");
        for _ in 0..PAIR_ATTEMPT_LIMIT {
            pair_mode.record_failure(&mut attempt).unwrap();
            if pair_mode.is_active() {
                attempt = pair_mode.begin_attempt().unwrap();
            }
        }
        assert!(!pair_mode.is_active());
    }

    #[test]
    fn the_attempt_cap_applies_to_the_qr_secret_too() {
        let pair_mode = PairMode::new();
        let secret = [7_u8; QR_SECRET_LEN];

        pair_mode
            .start_qr_secret_for_duration(secret, Duration::from_secs(60))
            .unwrap();

        for _ in 0..PAIR_ATTEMPT_LIMIT {
            let mut attempt = pair_mode.begin_attempt().unwrap();
            pair_mode.record_failure(&mut attempt).unwrap();
        }
        assert!(!pair_mode.is_active());
        assert!(pair_mode.is_locked_out());
        assert_eq!(pair_mode.begin_attempt(), Err(PairModeError::LockedOut));
    }

    #[test]
    fn a_new_window_lifts_the_lockout() {
        let pair_mode = PairMode::new();
        pair_mode
            .start_pin_for_duration("123456".to_string(), Duration::from_secs(60))
            .unwrap();
        for _ in 0..PAIR_ATTEMPT_LIMIT {
            let mut attempt = pair_mode.begin_attempt().unwrap();
            pair_mode.record_failure(&mut attempt).unwrap();
        }
        assert!(pair_mode.is_locked_out());
        pair_mode
            .start_pin_for_duration("654321".to_string(), Duration::from_secs(60))
            .unwrap();
        assert!(!pair_mode.is_locked_out());
        assert!(pair_mode.begin_attempt().is_ok());
    }

    #[test]
    fn attempts_are_capped_including_in_flight_attempts() {
        let pair_mode = PairMode::new();

        pair_mode
            .start_pin_for_duration("123456".to_string(), Duration::from_secs(60))
            .unwrap();

        let attempts = (0..PAIR_ATTEMPT_LIMIT)
            .map(|_| pair_mode.begin_attempt().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(pair_mode.begin_attempt(), Err(PairModeError::NotActive));
        drop(attempts);
        assert!(pair_mode.begin_attempt().is_ok());
    }

    #[test]
    fn success_consumption_is_bound_to_the_attempt_session() {
        let pair_mode = PairMode::new();

        pair_mode
            .start_pin_for_duration("123456".to_string(), Duration::from_secs(60))
            .unwrap();

        let mut first = pair_mode.begin_attempt().unwrap();
        let mut second = pair_mode.begin_attempt().unwrap();
        assert_eq!(first.secret(), b"123456");
        assert_eq!(second.secret(), b"123456");

        let mut commit = pair_mode.begin_commit(&mut first).unwrap();
        assert_eq!(
            pair_mode.begin_commit(&mut second),
            Err(PairModeError::NotActive)
        );
        pair_mode.complete_success(&mut commit).unwrap();
        assert_eq!(
            pair_mode.begin_commit(&mut second),
            Err(PairModeError::NotActive)
        );
        assert_eq!(
            pair_mode.record_failure(&mut second),
            Err(PairModeError::NotActive)
        );
    }

    #[test]
    fn dropped_commit_aborts_reservation() {
        let pair_mode = PairMode::new();
        pair_mode
            .start_pin_for_duration("123456".to_string(), Duration::from_secs(60))
            .unwrap();

        let mut first = pair_mode.begin_attempt().unwrap();
        drop(pair_mode.begin_commit(&mut first).unwrap());
        let mut second = pair_mode.begin_attempt().unwrap();
        let mut commit = pair_mode.begin_commit(&mut second).unwrap();
        pair_mode.complete_success(&mut commit).unwrap();
        assert!(!pair_mode.is_active());
    }

    #[test]
    fn generated_pin_is_six_decimal_digits() {
        let pair_mode = PairMode::new();

        let pin = pair_mode.start_pin().unwrap();

        assert_eq!(pin.len(), 6);
        assert!(pin.bytes().all(|byte| byte.is_ascii_digit()));
    }

    #[test]
    fn demo_pin_survives_success_and_failures() {
        let pair_mode = PairMode::new();
        pair_mode
            .start_demo_pin("123456".to_string(), Duration::from_secs(60))
            .unwrap();

        for _ in 0..(PAIR_ATTEMPT_LIMIT + 2) {
            let mut attempt = pair_mode.begin_attempt().unwrap();
            pair_mode.record_failure(&mut attempt).unwrap();
        }
        assert!(pair_mode.is_active(), "demo PIN must not lock out");

        for _ in 0..2 {
            let mut attempt = pair_mode.begin_attempt().unwrap();
            assert_eq!(attempt.secret(), b"123456");
            let mut commit = pair_mode.begin_commit(&mut attempt).unwrap();
            pair_mode.complete_success(&mut commit).unwrap();
        }
        assert!(pair_mode.is_active(), "demo PIN must not be consumed");
        assert!(pair_mode.cancel());
        assert!(!pair_mode.is_active());
    }

    #[test]
    fn demo_pin_rejects_bad_format_and_expires() {
        let pair_mode = PairMode::new();
        assert_eq!(
            pair_mode.start_demo_pin("12345".to_string(), Duration::from_secs(60)),
            Err(PairModeError::InvalidPinFormat)
        );
        pair_mode
            .start_demo_pin("123456".to_string(), Duration::from_millis(1))
            .unwrap();
        std::thread::sleep(Duration::from_millis(5));
        assert!(!pair_mode.is_active());
    }
}
