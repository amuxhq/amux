use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use curve25519_dalek::constants::ED25519_BASEPOINT_POINT;
use curve25519_dalek::edwards::{CompressedEdwardsY, EdwardsPoint};
use curve25519_dalek::scalar::Scalar;
use curve25519_dalek::traits::IsIdentity as _;
use futures_util::{Stream, stream};
use prost::Message as _;
use ring::rand::{SecureRandom as _, SystemRandom};
use ring::{aead, digest, hkdf, hmac};
use tokio::sync::mpsc;
use tonic::{Code, Status};
use wire::{self, PROTOCOL_VERSION};

use crate::connection::ConnectionManager;
use crate::identity::{DeviceIdentity, IdentityError};
use crate::pairing::ssh::SshPairingPeer;
use crate::pairing::{PairMode, PairModeAttempt};
use crate::transport::{BoxedGrpcAuth, BoxedGrpcConnectInfo, PreTrustPairingReachability};
use crate::trust::{Reachability, SharedTrustStore, TrustStore, TrustStorePairingUpdate};
use crate::{HostId, audit};

const HOST_ID_LEN: usize = 16;
const PUBKEY_LEN: usize = 32;
const MAX_PAIRING_NAME_BYTES: usize = 256;
const PAIRING_HKDF_SALT: &[u8] = b"amux-pair-spake2-v1";
const PAIR_CONFIRM_A: &[u8] = b"amux-pair-confirm-A";
const PAIR_CONFIRM_B: &[u8] = b"amux-pair-confirm-B";
const PAIR_ID_AAD: &[u8] = b"amux-pair-id";
const AEAD_A_TO_B_INFO: &[u8] = "aead/A→B".as_bytes();
const AEAD_B_TO_A_INFO: &[u8] = "aead/B→A".as_bytes();
const A_TO_B_DIRECTION: u8 = 0x01;
const B_TO_A_DIRECTION: u8 = 0x02;
pub(crate) const PAIR_INITIATOR_TIMEOUT: Duration = Duration::from_secs(30);
const PAIR_RESPONDER_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const SPAKE2_ED25519_M: [u8; 32] = [
    0xd0, 0x48, 0x03, 0x2c, 0x6e, 0xa0, 0xb6, 0xd6, 0x97, 0xdd, 0xc2, 0xe8, 0x6b, 0xda, 0x85, 0xa3,
    0x3a, 0xda, 0xc9, 0x20, 0xf1, 0xbf, 0x18, 0xe1, 0xb0, 0xc6, 0xd1, 0x66, 0xa5, 0xce, 0xcd, 0xaf,
];
const SPAKE2_ED25519_N: [u8; 32] = [
    0xd3, 0xbf, 0xb5, 0x18, 0xf4, 0x4f, 0x34, 0x30, 0xf2, 0x9d, 0x0c, 0x92, 0xaf, 0x50, 0x38, 0x65,
    0xa1, 0xed, 0x32, 0x81, 0xdc, 0x69, 0xb3, 0x5d, 0xd8, 0x68, 0xba, 0x85, 0xf8, 0x86, 0xc4, 0xab,
];

type PairStream = Pin<Box<dyn Stream<Item = Result<wire::pb::PairMessage, Status>> + Send>>;
pub(crate) type SharedTrustCommitLock = Arc<crate::trust::TrustGate>;

#[derive(Clone)]
pub struct PeerTrustCommitContext {
    trust_store: SharedTrustStore,
    trust_commit_lock: SharedTrustCommitLock,
    connections: Arc<ConnectionManager>,
    data_dir: PathBuf,
}

impl PeerTrustCommitContext {
    pub fn new(
        trust_store: SharedTrustStore,
        trust_commit_lock: SharedTrustCommitLock,
        connections: Arc<ConnectionManager>,
        data_dir: PathBuf,
    ) -> Self {
        Self {
            trust_store,
            trust_commit_lock,
            connections,
            data_dir,
        }
    }
}

pub struct PeerTrustUpdate {
    host_id: HostId,
    pubkey: Vec<u8>,
    name: String,
    reachability: Option<Reachability>,
}

impl PeerTrustUpdate {
    pub fn new(
        host_id: HostId,
        pubkey: Vec<u8>,
        name: String,
        reachability: Option<Reachability>,
    ) -> Self {
        Self {
            host_id,
            pubkey,
            name,
            reachability,
        }
    }
}

#[derive(Clone)]
pub(crate) struct LocalPairingIdentity {
    host_id: HostId,
    pubkey: Vec<u8>,
}

impl LocalPairingIdentity {
    pub(crate) fn new(host_id: HostId, pubkey: Vec<u8>) -> Self {
        Self { host_id, pubkey }
    }

    pub(crate) fn from_device_identity(identity: &DeviceIdentity) -> Self {
        Self::new(identity.host_id, identity.public_key().to_vec())
    }
}

#[derive(Clone)]
pub(crate) struct PairingService {
    pair_mode: Arc<PairMode>,
    local_identity: LocalPairingIdentity,
    host_name: String,
    trust_store: SharedTrustStore,
    trust_commit_lock: SharedTrustCommitLock,
    connections: Arc<ConnectionManager>,
    data_dir: PathBuf,
    spake2_responder_timeout: Duration,
    analytics: analytics::Analytics,
}

impl PairingService {
    pub(crate) fn new(
        pair_mode: Arc<PairMode>,
        local_identity: LocalPairingIdentity,
        host_name: String,
        trust_store: SharedTrustStore,
        trust_commit_lock: SharedTrustCommitLock,
        connections: Arc<ConnectionManager>,
        data_dir: PathBuf,
    ) -> Self {
        Self {
            pair_mode,
            local_identity,
            host_name,
            trust_store,
            trust_commit_lock,
            connections,
            data_dir,
            spake2_responder_timeout: PAIR_RESPONDER_TIMEOUT,
            analytics: analytics::Analytics::off(),
        }
    }

    /// Records how pairings this host offered turn out.
    pub(crate) fn with_analytics(mut self, analytics: analytics::Analytics) -> Self {
        self.analytics = analytics;
        self
    }

    fn failed(&self, method: analytics::Method, reason: analytics::PairingFailure) {
        self.analytics.record(analytics::Event::PairingFailed {
            role: analytics::Role::Offerer,
            method,
            reason,
        });
    }

    #[cfg(test)]
    fn with_spake2_responder_timeout(mut self, timeout: Duration) -> Self {
        self.spake2_responder_timeout = timeout;
        self
    }

    async fn stage_peer_trust(
        &self,
        host_id: HostId,
        pubkey: Vec<u8>,
        name: String,
        reachability: Option<Reachability>,
    ) -> Result<PeerTrustCommitGuard, Status> {
        stage_peer_trust_update(
            PeerTrustCommitContext::new(
                self.trust_store.clone(),
                self.trust_commit_lock.clone(),
                self.connections.clone(),
                self.data_dir.clone(),
            ),
            PeerTrustUpdate::new(host_id, pubkey, name, reachability),
        )
        .await
    }

    async fn commit_pairing(
        &self,
        attempt: &mut PairModeAttempt,
        host_id: HostId,
        pubkey: Vec<u8>,
        name: String,
        reachability: Option<Reachability>,
    ) -> Result<(), Status> {
        let mut commit = self
            .pair_mode
            .begin_commit(attempt)
            .map_err(pair_mode_status)?;
        match self
            .stage_peer_trust(host_id, pubkey, name, reachability)
            .await
        {
            Ok(trust_commit) => match self.pair_mode.complete_success(&mut commit) {
                Ok(()) => trust_commit.commit().await,
                Err(error) => {
                    trust_commit.rollback().await;
                    self.pair_mode.abort_commit(&mut commit);
                    Err(pair_mode_status(error))
                }
            },
            Err(error) => {
                self.pair_mode.abort_commit(&mut commit);
                Err(error)
            }
        }
    }

    async fn run_spake2_responder(
        self,
        mut attempt: PairModeAttempt,
        reachability: Option<Reachability>,
        mut inbound: tonic::Streaming<wire::pb::PairMessage>,
        outbound: mpsc::Sender<Result<wire::pb::PairMessage, Status>>,
    ) -> Result<(), Status> {
        validate_name(&self.host_name)?;
        let method = attempt.method();
        let peer_spake_msg = match read_spake2_message(&mut inbound).await? {
            PairingRead::Expected(bytes) => bytes,
            PairingRead::PeerError(_) => {
                audit::pairing_failure("spake2", "peer rejected pairing");
                self.failed(method, analytics::PairingFailure::Abandoned);
                return Ok(());
            }
            PairingRead::Eof => {
                audit::pairing_failure("spake2", "pairing stream closed before SPAKE2 message");
                self.failed(method, analytics::PairingFailure::Abandoned);
                return Ok(());
            }
            PairingRead::Unexpected => {
                send_pairing_error(
                    &outbound,
                    wire::pb::pairing_error::Reason::ProtocolViolation,
                    "expected SPAKE2 initiator message",
                )
                .await;
                return Ok(());
            }
        };

        let (spake, local_spake_msg) = Spake2ResponderState::start(attempt.secret())?;
        send_body(
            &outbound,
            wire::pb::pair_message::Body::Spake2Message(local_spake_msg.clone()),
        )
        .await?;

        let shared = match spake.finish(&peer_spake_msg) {
            Ok(shared) => shared,
            Err(_) => {
                let _ = self.pair_mode.record_failure(&mut attempt);
                self.failed(method, analytics::PairingFailure::WrongSecret);
                send_pairing_error(
                    &outbound,
                    wire::pb::pairing_error::Reason::InvalidPin,
                    "invalid PIN",
                )
                .await;
                return Ok(());
            }
        };
        let keys = derive_spake2_keys(&shared, &peer_spake_msg, &local_spake_msg)?;

        let peer_confirmation = match read_key_confirmation(&mut inbound).await? {
            PairingRead::Expected(bytes) => bytes,
            PairingRead::PeerError(_) => {
                audit::pairing_failure("spake2", "peer rejected pairing");
                self.failed(method, analytics::PairingFailure::Abandoned);
                return Ok(());
            }
            PairingRead::Eof => {
                audit::pairing_failure("spake2", "pairing stream closed before key confirmation");
                self.failed(method, analytics::PairingFailure::Abandoned);
                return Ok(());
            }
            PairingRead::Unexpected => {
                send_pairing_error(
                    &outbound,
                    wire::pb::pairing_error::Reason::ProtocolViolation,
                    "expected key confirmation",
                )
                .await;
                return Ok(());
            }
        };
        if !verify_hmac_confirm(
            &keys.kc_b,
            PAIR_CONFIRM_B,
            &keys.transcript_hash,
            &peer_confirmation,
        ) {
            let _ = self.pair_mode.record_failure(&mut attempt);
            self.failed(method, analytics::PairingFailure::WrongSecret);
            send_pairing_error(
                &outbound,
                wire::pb::pairing_error::Reason::InvalidPin,
                "invalid PIN",
            )
            .await;
            return Ok(());
        }
        send_body(
            &outbound,
            wire::pb::pair_message::Body::KeyConfirmation(keys.confirm_a.clone()),
        )
        .await?;

        let local_identity = wire::pb::PairingIdentity {
            expires_at_ms: Utc::now().timestamp_millis()
                + attempt
                    .remaining()
                    .min(self.spake2_responder_timeout)
                    .as_millis() as i64,
            host_id: self.local_identity.host_id.as_bytes().to_vec(),
            pubkey: self.local_identity.pubkey.clone(),
            name: self.host_name.clone(),
        };
        send_body(
            &outbound,
            wire::pb::pair_message::Body::SealedIdentity(seal_identity(
                &keys.aead_a_to_b,
                A_TO_B_DIRECTION,
                &keys.transcript_hash,
                &local_identity,
            )?),
        )
        .await?;

        let sealed_peer_identity = match read_sealed_identity(&mut inbound).await? {
            PairingRead::Expected(bytes) => bytes,
            PairingRead::PeerError(error) => {
                audit::pairing_failure("spake2", "peer rejected pairing");
                self.failed(method, analytics::PairingFailure::Abandoned);
                if error.reason == wire::pb::pairing_error::Reason::UserRejected as i32 {
                    // Release the attempt before acknowledging cancellation so callers
                    // can immediately begin again without consuming an attempt slot.
                    drop(attempt);
                    send_body(
                        &outbound,
                        wire::pb::pair_message::Body::PairingAbandoned(
                            wire::pb::PairingAbandoned {},
                        ),
                    )
                    .await?;
                }
                return Ok(());
            }
            PairingRead::Eof => {
                audit::pairing_failure("spake2", "pairing stream closed before sealed identity");
                self.failed(method, analytics::PairingFailure::Abandoned);
                return Ok(());
            }
            PairingRead::Unexpected => {
                send_pairing_error(
                    &outbound,
                    wire::pb::pairing_error::Reason::ProtocolViolation,
                    "expected sealed identity",
                )
                .await;
                return Ok(());
            }
        };
        let peer_identity = match open_identity(
            &keys.aead_b_to_a,
            B_TO_A_DIRECTION,
            &keys.transcript_hash,
            sealed_peer_identity,
        ) {
            Ok(identity) => identity,
            Err(()) => {
                let _ = self.pair_mode.record_failure(&mut attempt);
                self.failed(method, analytics::PairingFailure::WrongSecret);
                send_pairing_error(
                    &outbound,
                    wire::pb::pairing_error::Reason::InvalidPin,
                    "invalid PIN",
                )
                .await;
                return Ok(());
            }
        };
        let peer_host_id = match validate_pairing_identity(&peer_identity) {
            Ok(host_id) => host_id,
            Err(error) => {
                send_pairing_error(
                    &outbound,
                    wire::pb::pairing_error::Reason::ProtocolViolation,
                    error,
                )
                .await;
                return Ok(());
            }
        };
        if peer_host_id == self.local_identity.host_id
            || peer_identity.pubkey.as_slice() == self.local_identity.pubkey.as_slice()
        {
            send_pairing_error(
                &outbound,
                wire::pb::pairing_error::Reason::SelfPairing,
                "self pairing is not allowed",
            )
            .await;
            return Ok(());
        }
        self.commit_pairing(
            &mut attempt,
            peer_host_id,
            peer_identity.pubkey,
            peer_identity.name,
            reachability,
        )
        .await?;
        send_body(
            &outbound,
            wire::pb::pair_message::Body::PairingComplete(wire::pb::PairingComplete {}),
        )
        .await?;
        audit::pairing_success("spake2", peer_host_id);
        self.analytics.record(analytics::Event::PairingSucceeded {
            role: analytics::Role::Offerer,
            method,
            remote_host: peer_host_id,
        });
        Ok(())
    }
}

pub async fn commit_peer_trust(
    context: PeerTrustCommitContext,
    update: PeerTrustUpdate,
) -> Result<(), Status> {
    stage_peer_trust_update(context, update)
        .await?
        .commit()
        .await
}

#[cfg(test)]
pub(crate) async fn pair_initiator(
    client: &mut wire::pairing_service_client::PairingServiceClient<tonic::transport::Channel>,
    local_identity: &LocalPairingIdentity,
    local_name: &str,
    secret: &[u8],
) -> Result<SshPairingPeer, Status> {
    pair_initiator_with_timeout(
        client,
        local_identity,
        local_name,
        secret,
        PAIR_INITIATOR_TIMEOUT,
    )
    .await
}

pub(crate) struct PendingPairing {
    pub(crate) peer: wire::pb::PairingIdentity,
    tx: mpsc::Sender<wire::pb::PairMessage>,
    inbound: tonic::Streaming<wire::pb::PairMessage>,
    sealed_local_identity: Vec<u8>,
}

impl PendingPairing {
    pub(crate) async fn confirm(mut self) -> Result<SshPairingPeer, Status> {
        if Utc::now().timestamp_millis() >= self.peer.expires_at_ms {
            return Err(pairing_status(Code::PermissionDenied, "INVALID_PIN"));
        }
        send_client_pairing_body(
            &self.tx,
            wire::pb::pair_message::Body::SealedIdentity(self.sealed_local_identity),
        )
        .await?;
        let completion = self
            .inbound
            .message()
            .await?
            .ok_or_else(|| Status::unavailable("pairing stream closed"))?;
        match completion.body {
            Some(wire::pb::pair_message::Body::PairingComplete(_)) => Ok(SshPairingPeer {
                host_id: validate_pairing_identity(&self.peer).map_err(Status::invalid_argument)?,
                pubkey: self.peer.pubkey,
                name: self.peer.name,
            }),
            Some(wire::pb::pair_message::Body::Error(error)) => {
                Err(peer_pairing_error_status(error))
            }
            _ => Err(Status::invalid_argument("expected pairing completion")),
        }
    }

    pub(crate) async fn abandon(mut self) -> Result<(), Status> {
        send_client_pairing_body(
            &self.tx,
            wire::pb::pair_message::Body::Error(wire::pb::PairingError {
                reason: wire::pb::pairing_error::Reason::UserRejected as i32,
                detail: String::new(),
            }),
        )
        .await?;
        match self
            .inbound
            .message()
            .await?
            .and_then(|message| message.body)
        {
            Some(wire::pb::pair_message::Body::PairingAbandoned(_)) => Ok(()),
            Some(wire::pb::pair_message::Body::Error(error)) => {
                Err(peer_pairing_error_status(error))
            }
            _ => Err(Status::unavailable(
                "pairing stream closed before abandonment acknowledgement",
            )),
        }
    }
}

pub(crate) async fn begin_pair_initiator(
    client: &mut wire::pairing_service_client::PairingServiceClient<tonic::transport::Channel>,
    local_identity: &LocalPairingIdentity,
    local_name: &str,
    secret: &[u8],
) -> Result<PendingPairing, Status> {
    tokio::time::timeout(
        PAIR_INITIATOR_TIMEOUT,
        begin_pair_initiator_inner(client, local_identity, local_name, secret),
    )
    .await
    .map_err(|_| pairing_status(Code::DeadlineExceeded, "PAIRING_TIMEOUT"))?
}

async fn begin_pair_initiator_inner(
    client: &mut wire::pairing_service_client::PairingServiceClient<tonic::transport::Channel>,
    local_identity: &LocalPairingIdentity,
    local_name: &str,
    secret: &[u8],
) -> Result<PendingPairing, Status> {
    validate_name(local_name)?;
    let (tx, rx) = mpsc::channel(8);
    let outbound = stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|message| (message, rx))
    });
    let mut inbound = client
        .pair(tonic::Request::new(outbound))
        .await?
        .into_inner();

    let (spake, msg_b) = Spake2InitiatorState::start(secret)?;
    send_client_pairing_body(
        &tx,
        wire::pb::pair_message::Body::Spake2Message(msg_b.clone()),
    )
    .await?;

    let msg_a = match read_spake2_message(&mut inbound).await? {
        PairingRead::Expected(bytes) => bytes,
        PairingRead::PeerError(error) => return Err(peer_pairing_error_status(error)),
        PairingRead::Unexpected => {
            return Err(pairing_status(
                Code::InvalidArgument,
                "PROTOCOL_VIOLATION: expected SPAKE2 responder message",
            ));
        }
        PairingRead::Eof => return Err(Status::unavailable("pairing stream closed")),
    };
    let shared = spake
        .finish(&msg_a)
        .map_err(|_| pairing_status(Code::PermissionDenied, "INVALID_PIN"))?;
    let keys = derive_spake2_keys(&shared, &msg_b, &msg_a)?;
    send_client_pairing_body(
        &tx,
        wire::pb::pair_message::Body::KeyConfirmation(hmac_confirm(
            &keys.kc_b,
            PAIR_CONFIRM_B,
            &keys.transcript_hash,
        )),
    )
    .await?;

    let peer_confirmation = match read_key_confirmation(&mut inbound).await? {
        PairingRead::Expected(bytes) => bytes,
        PairingRead::PeerError(error) => return Err(peer_pairing_error_status(error)),
        PairingRead::Unexpected => {
            return Err(pairing_status(
                Code::InvalidArgument,
                "PROTOCOL_VIOLATION: expected key confirmation",
            ));
        }
        PairingRead::Eof => return Err(Status::unavailable("pairing stream closed")),
    };
    if !verify_hmac_confirm(
        &keys.kc_a,
        PAIR_CONFIRM_A,
        &keys.transcript_hash,
        &peer_confirmation,
    ) {
        return Err(pairing_status(Code::PermissionDenied, "INVALID_PIN"));
    }

    let sealed_peer_identity = match read_sealed_identity(&mut inbound).await? {
        PairingRead::Expected(bytes) => bytes,
        PairingRead::PeerError(error) => return Err(peer_pairing_error_status(error)),
        PairingRead::Unexpected => {
            return Err(pairing_status(
                Code::InvalidArgument,
                "PROTOCOL_VIOLATION: expected sealed identity",
            ));
        }
        PairingRead::Eof => return Err(Status::unavailable("pairing stream closed")),
    };
    let peer_identity = open_identity(
        &keys.aead_a_to_b,
        A_TO_B_DIRECTION,
        &keys.transcript_hash,
        sealed_peer_identity,
    )
    .map_err(|_| pairing_status(Code::PermissionDenied, "INVALID_PIN"))?;
    let peer_host_id = validate_pairing_identity(&peer_identity)
        .map_err(|error| pairing_status(Code::InvalidArgument, error))?;
    if peer_host_id == local_identity.host_id
        || peer_identity.pubkey.as_slice() == local_identity.pubkey.as_slice()
    {
        return Err(pairing_status(Code::InvalidArgument, "SELF_PAIRING"));
    }

    let local_pairing_identity = wire::pb::PairingIdentity {
        expires_at_ms: 0,
        host_id: local_identity.host_id.as_bytes().to_vec(),
        pubkey: local_identity.pubkey.clone(),
        name: local_name.to_string(),
    };
    let sealed_local_identity = seal_identity(
        &keys.aead_b_to_a,
        B_TO_A_DIRECTION,
        &keys.transcript_hash,
        &local_pairing_identity,
    )?;
    Ok(PendingPairing {
        peer: peer_identity,
        tx,
        inbound,
        sealed_local_identity,
    })
}

#[cfg(test)]
async fn pair_initiator_with_timeout(
    client: &mut wire::pairing_service_client::PairingServiceClient<tonic::transport::Channel>,
    local_identity: &LocalPairingIdentity,
    local_name: &str,
    secret: &[u8],
    timeout: Duration,
) -> Result<SshPairingPeer, Status> {
    match tokio::time::timeout(timeout, async {
        begin_pair_initiator_inner(client, local_identity, local_name, secret)
            .await?
            .confirm()
            .await
    })
    .await
    {
        Ok(result) => result,
        Err(_) => Err(pairing_status(Code::DeadlineExceeded, "PAIRING_TIMEOUT")),
    }
}

async fn stage_peer_trust_update(
    context: PeerTrustCommitContext,
    update: PeerTrustUpdate,
) -> Result<PeerTrustCommitGuard, Status> {
    let trust_commit_lock = context.trust_commit_lock.barrier().await;
    if context.trust_commit_lock.is_closed() {
        return Err(Status::failed_precondition("profile is unavailable"));
    }
    let host_id = update.host_id;
    let (before, mut staged, mut outcome) = {
        let store = context
            .trust_store
            .read()
            .map_err(|_| identity_status(IdentityError::TrustStorePoisoned))?;
        let before = store.clone();
        let mut staged = before.clone();
        let outcome = staged
            .upsert_paired_peer(
                host_id,
                update.pubkey.clone(),
                update.name.clone(),
                update.reachability.clone(),
                Utc::now(),
            )
            .map_err(identity_status)?;
        (before, staged, outcome)
    };
    let mut guard = PeerTrustCommitGuard::new(
        context.clone(),
        host_id,
        PeerTrustCommitState::new(before.clone(), staged.clone(), outcome.clone()),
        trust_commit_lock,
    );

    if outcome == TrustStorePairingUpdate::PubkeyReplacementRequired {
        {
            let mut store = context
                .trust_store
                .write()
                .map_err(|_| identity_status(IdentityError::TrustStorePoisoned))?;
            let _ = store
                .upsert_paired_peer(
                    host_id,
                    update.pubkey.clone(),
                    update.name.clone(),
                    update.reachability.clone(),
                    Utc::now(),
                )
                .map_err(identity_status)?;
        }
        guard = PeerTrustCommitGuard::new(
            context.clone(),
            host_id,
            PeerTrustCommitState::new(before, staged.clone(), outcome.clone()).finish_connection(),
            guard
                .trust_commit_lock
                .take()
                .expect("trust commit lock held"),
        );
        context.connections.teardown_host(host_id).await;
        outcome = staged
            .replace_paired_peer_after_teardown(
                host_id,
                update.pubkey,
                update.name,
                update.reachability,
                Utc::now(),
            )
            .map_err(identity_status)?;
        guard.outcome = outcome.clone();
        guard.staged = Some(staged);
    }
    if let Err(error) = guard.save_staged() {
        guard.rollback().await;
        return Err(error);
    }
    Ok(guard)
}

struct PeerTrustCommitGuard {
    trust_store: SharedTrustStore,
    connections: Arc<ConnectionManager>,
    data_dir: PathBuf,
    host_id: HostId,
    before: Option<TrustStore>,
    staged: Option<TrustStore>,
    outcome: TrustStorePairingUpdate,
    finish_connection: bool,
    trust_commit_lock: Option<crate::trust::TrustGateBarrier>,
}

struct PeerTrustCommitState {
    before: TrustStore,
    staged: TrustStore,
    outcome: TrustStorePairingUpdate,
    finish_connection: bool,
}

impl PeerTrustCommitState {
    fn new(before: TrustStore, staged: TrustStore, outcome: TrustStorePairingUpdate) -> Self {
        Self {
            before,
            staged,
            outcome,
            finish_connection: false,
        }
    }

    fn finish_connection(mut self) -> Self {
        self.finish_connection = true;
        self
    }
}

impl PeerTrustCommitGuard {
    fn new(
        context: PeerTrustCommitContext,
        host_id: HostId,
        state: PeerTrustCommitState,
        trust_commit_lock: crate::trust::TrustGateBarrier,
    ) -> Self {
        Self {
            trust_store: context.trust_store,
            connections: context.connections,
            data_dir: context.data_dir,
            host_id,
            before: Some(state.before),
            staged: Some(state.staged),
            outcome: state.outcome,
            finish_connection: state.finish_connection,
            trust_commit_lock: Some(trust_commit_lock),
        }
    }

    fn save_staged(&self) -> Result<(), Status> {
        let Some(staged) = self.staged.as_ref() else {
            return Ok(());
        };
        staged.save_in(&self.data_dir).map_err(identity_status)
    }

    async fn commit(mut self) -> Result<(), Status> {
        if let Some(staged) = self.staged.take() {
            let mut store = self
                .trust_store
                .write()
                .map_err(|_| identity_status(IdentityError::TrustStorePoisoned))?;
            *store = staged;
        }
        self.before = None;
        self.finish_replacement().await;
        self.connections.trust_renewed(self.host_id);
        audit::trust_pairing_update(self.host_id, &self.outcome);
        Ok(())
    }

    fn rollback_trust(&mut self) {
        let Some(before) = self.before.take() else {
            return;
        };
        if let Ok(mut store) = self.trust_store.write() {
            *store = before;
            let _ = store.save_in(&self.data_dir);
        }
        self.staged = None;
    }

    async fn finish_replacement(&mut self) {
        if self.finish_connection {
            self.connections.finish_host_replacement(self.host_id).await;
            self.finish_connection = false;
        }
    }

    async fn rollback(mut self) {
        self.rollback_trust();
        self.finish_replacement().await;
        self.before = None;
        self.staged = None;
    }
}

impl Drop for PeerTrustCommitGuard {
    fn drop(&mut self) {
        self.rollback_trust();
        if self.finish_connection {
            let connections = self.connections.clone();
            let host_id = self.host_id;
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    connections.finish_host_replacement(host_id).await;
                });
            }
        }
    }
}

#[tonic::async_trait]
impl wire::pairing_service_server::PairingService for PairingService {
    type PairStream = PairStream;

    async fn pair(
        &self,
        request: tonic::Request<tonic::Streaming<wire::pb::PairMessage>>,
    ) -> Result<tonic::Response<Self::PairStream>, Status> {
        audit::pairing_start("spake2");
        let reachability = pairing_request_reachability(&request).inspect_err(|error| {
            audit::pairing_failure("spake2", error);
        })?;
        let attempt = self
            .pair_mode
            .begin_attempt()
            .map_err(pair_mode_status)
            .inspect_err(|error| {
                audit::pairing_failure("spake2", error);
            })?;
        let (tx, rx) = mpsc::channel(8);
        let service = self.clone();
        let responder_timeout = self.spake2_responder_timeout.min(attempt.remaining());
        let method = attempt.method();
        let outcomes = self.clone();
        tokio::spawn(async move {
            let responder = service.run_spake2_responder(
                attempt,
                reachability,
                request.into_inner(),
                tx.clone(),
            );
            match tokio::time::timeout(responder_timeout, responder).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    audit::pairing_failure("spake2", &error);
                    outcomes.failed(method, analytics::PairingFailure::Other);
                    let _ = tx.send(Err(error)).await;
                }
                Err(_) => {
                    audit::pairing_failure("spake2", "PAIRING_TIMEOUT");
                    outcomes.failed(method, analytics::PairingFailure::TimedOut);
                    let _ = tx
                        .send(Err(pairing_status(
                            Code::DeadlineExceeded,
                            "PAIRING_TIMEOUT",
                        )))
                        .await;
                }
            }
        });
        Ok(tonic::Response::new(Box::pin(stream::unfold(
            rx,
            |mut rx| async move { rx.recv().await.map(|item| (item, rx)) },
        ))))
    }
}

fn validate_name(name: &str) -> Result<(), Status> {
    if name.len() <= MAX_PAIRING_NAME_BYTES {
        Ok(())
    } else {
        Err(pairing_status(
            Code::InvalidArgument,
            "PROTOCOL_VIOLATION: name is too long",
        ))
    }
}

fn validate_pairing_identity(identity: &wire::pb::PairingIdentity) -> Result<HostId, &'static str> {
    if identity.host_id.len() != HOST_ID_LEN {
        return Err("host_id must be 16 bytes");
    }
    if identity.pubkey.len() != PUBKEY_LEN {
        return Err("pubkey must be 32 bytes");
    }
    if identity.name.len() > MAX_PAIRING_NAME_BYTES {
        return Err("name is too long");
    }
    let mut host_id = [0_u8; HOST_ID_LEN];
    host_id.copy_from_slice(&identity.host_id);
    Ok(HostId::from_bytes(host_id))
}

fn pairing_request_reachability<T>(
    request: &tonic::Request<T>,
) -> Result<Option<Reachability>, Status> {
    request
        .extensions()
        .get::<BoxedGrpcConnectInfo>()
        .and_then(|info| match &info.auth {
            BoxedGrpcAuth::PreTrustPairing { reachability } => Some(match reachability {
                PreTrustPairingReachability::Cloud => Some(Reachability::Cloud),
                PreTrustPairingReachability::Direct(addr) => {
                    Some(Reachability::Direct { addrs: vec![*addr] })
                }
                PreTrustPairingReachability::NoReusableReachability => None,
            }),
            BoxedGrpcAuth::Trusted { .. } => None,
        })
        .ok_or_else(|| {
            Status::permission_denied("pairing RPC requires pre-trust pairing transport")
        })
}

struct Spake2ResponderState {
    x: Scalar,
    w: Scalar,
}

impl Spake2ResponderState {
    fn start(secret: &[u8]) -> Result<(Self, Vec<u8>), Status> {
        let x = random_spake2_scalar()?;
        let w = spake2_password_scalar(secret);
        let m = spake2_ed25519_point(&SPAKE2_ED25519_M)?;
        let msg_a = (ED25519_BASEPOINT_POINT * x + m * w)
            .compress()
            .to_bytes()
            .to_vec();
        Ok((Self { x, w }, msg_a))
    }

    fn finish(self, msg_b: &[u8]) -> Result<[u8; 32], ()> {
        let peer = spake2_peer_point(msg_b)?;
        let n = spake2_ed25519_point(&SPAKE2_ED25519_N).map_err(|_| ())?;
        let k = (peer - n * self.w) * self.x * Scalar::from(8_u8);
        if k.is_identity() {
            return Err(());
        }
        Ok(k.compress().to_bytes())
    }
}

struct Spake2InitiatorState {
    y: Scalar,
    w: Scalar,
}

impl Spake2InitiatorState {
    fn start(secret: &[u8]) -> Result<(Self, Vec<u8>), Status> {
        let y = random_spake2_scalar()?;
        let w = spake2_password_scalar(secret);
        let n = spake2_ed25519_point(&SPAKE2_ED25519_N)?;
        let msg_b = (ED25519_BASEPOINT_POINT * y + n * w)
            .compress()
            .to_bytes()
            .to_vec();
        Ok((Self { y, w }, msg_b))
    }

    fn finish(self, msg_a: &[u8]) -> Result<[u8; 32], ()> {
        let peer = spake2_peer_point(msg_a)?;
        let m = spake2_ed25519_point(&SPAKE2_ED25519_M).map_err(|_| ())?;
        let k = (peer - m * self.w) * self.y * Scalar::from(8_u8);
        if k.is_identity() {
            return Err(());
        }
        Ok(k.compress().to_bytes())
    }
}

fn random_spake2_scalar() -> Result<Scalar, Status> {
    let rng = SystemRandom::new();
    for _ in 0..8 {
        let mut bytes = [0_u8; 64];
        rng.fill(&mut bytes)
            .map_err(|_| Status::internal("failed to generate SPAKE2 scalar"))?;
        let scalar = Scalar::from_bytes_mod_order_wide(&bytes);
        if scalar != Scalar::ZERO {
            return Ok(scalar);
        }
    }
    Err(Status::internal("failed to generate nonzero SPAKE2 scalar"))
}

fn spake2_password_scalar(secret: &[u8]) -> Scalar {
    // The out-of-band secret (PIN digits or QR secret bytes) is the
    // RFC 9382 password input; this is the ciphersuite-required
    // hash-to-scalar step, not application stretching.
    let digest = digest::digest(&digest::SHA512, secret);
    let mut wide = [0_u8; 64];
    wide.copy_from_slice(digest.as_ref());
    Scalar::from_bytes_mod_order_wide(&wide)
}

fn spake2_ed25519_point(bytes: &[u8; 32]) -> Result<EdwardsPoint, Status> {
    spake2_peer_point(bytes).map_err(|_| Status::internal("invalid SPAKE2 constants"))
}

fn spake2_peer_point(bytes: &[u8]) -> Result<EdwardsPoint, ()> {
    if bytes.len() != 32 {
        return Err(());
    }
    let mut encoded = [0_u8; 32];
    encoded.copy_from_slice(bytes);
    let Some(point) = CompressedEdwardsY(encoded).decompress() else {
        return Err(());
    };
    if point.is_identity() || !point.is_torsion_free() {
        return Err(());
    }
    Ok(point)
}

struct PairSpake2Keys {
    transcript_hash: [u8; 32],
    kc_a: [u8; 32],
    kc_b: [u8; 32],
    confirm_a: Vec<u8>,
    aead_a_to_b: [u8; 32],
    aead_b_to_a: [u8; 32],
}

fn derive_spake2_keys(
    shared_secret: &[u8],
    msg_b: &[u8],
    msg_a: &[u8],
) -> Result<PairSpake2Keys, Status> {
    let transcript_hash = spake2_transcript_hash(msg_b, msg_a);
    let salt = hkdf::Salt::new(hkdf::HKDF_SHA256, PAIRING_HKDF_SALT);
    let prk = salt.extract(shared_secret);
    let kc_a = hkdf_expand_32(&prk, b"kc/A", &transcript_hash)?;
    let kc_b = hkdf_expand_32(&prk, b"kc/B", &transcript_hash)?;
    let aead_a_to_b = hkdf_expand_32(&prk, AEAD_A_TO_B_INFO, &transcript_hash)?;
    let aead_b_to_a = hkdf_expand_32(&prk, AEAD_B_TO_A_INFO, &transcript_hash)?;
    Ok(PairSpake2Keys {
        transcript_hash,
        kc_a,
        kc_b,
        confirm_a: hmac_confirm(&kc_a, PAIR_CONFIRM_A, &transcript_hash),
        aead_a_to_b,
        aead_b_to_a,
    })
}

fn spake2_transcript_hash(msg_b: &[u8], msg_a: &[u8]) -> [u8; 32] {
    let mut ctx = digest::Context::new(&digest::SHA256);
    ctx.update(&PROTOCOL_VERSION.to_be_bytes());
    update_len_prefixed(&mut ctx, msg_b);
    update_len_prefixed(&mut ctx, msg_a);
    let digest = ctx.finish();
    let mut hash = [0_u8; 32];
    hash.copy_from_slice(digest.as_ref());
    hash
}

fn update_len_prefixed(ctx: &mut digest::Context, bytes: &[u8]) {
    ctx.update(&(bytes.len() as u32).to_be_bytes());
    ctx.update(bytes);
}

struct Hkdf32;

impl hkdf::KeyType for Hkdf32 {
    fn len(&self) -> usize {
        32
    }
}

fn hkdf_expand_32(
    prk: &hkdf::Prk,
    label: &[u8],
    transcript_hash: &[u8; 32],
) -> Result<[u8; 32], Status> {
    let info = [label, transcript_hash.as_slice()];
    let okm = prk
        .expand(&info, Hkdf32)
        .map_err(|_| Status::internal("failed to expand pairing keys"))?;
    let mut out = [0_u8; 32];
    okm.fill(&mut out)
        .map_err(|_| Status::internal("failed to fill pairing keys"))?;
    Ok(out)
}

fn hmac_confirm(key: &[u8; 32], label: &[u8], transcript_hash: &[u8; 32]) -> Vec<u8> {
    let key = hmac::Key::new(hmac::HMAC_SHA256, key);
    let mut ctx = hmac::Context::with_key(&key);
    ctx.update(label);
    ctx.update(transcript_hash);
    ctx.sign().as_ref().to_vec()
}

fn verify_hmac_confirm(
    key: &[u8; 32],
    label: &[u8],
    transcript_hash: &[u8; 32],
    tag: &[u8],
) -> bool {
    let key = hmac::Key::new(hmac::HMAC_SHA256, key);
    let mut message = Vec::with_capacity(label.len() + transcript_hash.len());
    message.extend_from_slice(label);
    message.extend_from_slice(transcript_hash);
    hmac::verify(&key, &message, tag).is_ok()
}

fn seal_identity(
    key: &[u8; 32],
    direction: u8,
    transcript_hash: &[u8; 32],
    identity: &wire::pb::PairingIdentity,
) -> Result<Vec<u8>, Status> {
    let unbound = aead::UnboundKey::new(&aead::CHACHA20_POLY1305, key)
        .map_err(|_| Status::internal("failed to create pairing AEAD key"))?;
    let key = aead::LessSafeKey::new(unbound);
    let nonce = aead::Nonce::assume_unique_for_key([0_u8; 12]);
    let aad = pairing_identity_aad(direction, transcript_hash);
    let mut ciphertext = identity.encode_to_vec();
    key.seal_in_place_append_tag(nonce, aead::Aad::from(aad.as_slice()), &mut ciphertext)
        .map_err(|_| Status::internal("failed to seal pairing identity"))?;
    Ok(ciphertext)
}

fn open_identity(
    key: &[u8; 32],
    direction: u8,
    transcript_hash: &[u8; 32],
    mut ciphertext: Vec<u8>,
) -> Result<wire::pb::PairingIdentity, ()> {
    let unbound = aead::UnboundKey::new(&aead::CHACHA20_POLY1305, key).map_err(|_| ())?;
    let key = aead::LessSafeKey::new(unbound);
    let nonce = aead::Nonce::assume_unique_for_key([0_u8; 12]);
    let aad = pairing_identity_aad(direction, transcript_hash);
    let plaintext = key
        .open_in_place(nonce, aead::Aad::from(aad.as_slice()), &mut ciphertext)
        .map_err(|_| ())?;
    wire::pb::PairingIdentity::decode(&*plaintext).map_err(|_| ())
}

fn pairing_identity_aad(direction: u8, transcript_hash: &[u8; 32]) -> Vec<u8> {
    let mut aad = Vec::with_capacity(PAIR_ID_AAD.len() + 1 + transcript_hash.len());
    aad.extend_from_slice(PAIR_ID_AAD);
    aad.push(direction);
    aad.extend_from_slice(transcript_hash);
    aad
}

async fn read_spake2_message(
    inbound: &mut tonic::Streaming<wire::pb::PairMessage>,
) -> Result<PairingRead<Vec<u8>>, Status> {
    read_expected_body(inbound, |body| match body {
        wire::pb::pair_message::Body::Spake2Message(bytes) => Some(bytes),
        _ => None,
    })
    .await
}

async fn read_key_confirmation(
    inbound: &mut tonic::Streaming<wire::pb::PairMessage>,
) -> Result<PairingRead<Vec<u8>>, Status> {
    read_expected_body(inbound, |body| match body {
        wire::pb::pair_message::Body::KeyConfirmation(bytes) => Some(bytes),
        _ => None,
    })
    .await
}

async fn read_sealed_identity(
    inbound: &mut tonic::Streaming<wire::pb::PairMessage>,
) -> Result<PairingRead<Vec<u8>>, Status> {
    read_expected_body(inbound, |body| match body {
        wire::pb::pair_message::Body::SealedIdentity(bytes) => Some(bytes),
        _ => None,
    })
    .await
}

enum PairingRead<T> {
    Expected(T),
    PeerError(wire::pb::PairingError),
    Unexpected,
    Eof,
}

async fn read_expected_body<T>(
    inbound: &mut tonic::Streaming<wire::pb::PairMessage>,
    f: impl FnOnce(wire::pb::pair_message::Body) -> Option<T>,
) -> Result<PairingRead<T>, Status> {
    let Some(message) = inbound.message().await? else {
        return Ok(PairingRead::Eof);
    };
    let Some(body) = message.body else {
        return Ok(PairingRead::Unexpected);
    };
    if let wire::pb::pair_message::Body::Error(error) = body {
        return Ok(PairingRead::PeerError(error));
    }
    Ok(f(body).map_or(PairingRead::Unexpected, PairingRead::Expected))
}

async fn send_body(
    outbound: &mpsc::Sender<Result<wire::pb::PairMessage, Status>>,
    body: wire::pb::pair_message::Body,
) -> Result<(), Status> {
    outbound
        .send(Ok(wire::pb::PairMessage { body: Some(body) }))
        .await
        .map_err(|_| Status::cancelled("pairing stream closed"))
}

async fn send_client_pairing_body(
    outbound: &mpsc::Sender<wire::pb::PairMessage>,
    body: wire::pb::pair_message::Body,
) -> Result<(), Status> {
    outbound
        .send(wire::pb::PairMessage { body: Some(body) })
        .await
        .map_err(|_| Status::cancelled("pairing stream closed"))
}

async fn send_pairing_error(
    outbound: &mpsc::Sender<Result<wire::pb::PairMessage, Status>>,
    reason: wire::pb::pairing_error::Reason,
    detail: impl Into<String>,
) {
    let detail = detail.into();
    audit::pairing_failure("spake2", local_pairing_audit_reason(reason, &detail));
    let _ = send_body(
        outbound,
        wire::pb::pair_message::Body::Error(wire::pb::PairingError {
            reason: reason as i32,
            detail,
        }),
    )
    .await;
}

fn local_pairing_audit_reason(reason: wire::pb::pairing_error::Reason, detail: &str) -> String {
    match reason {
        wire::pb::pairing_error::Reason::InvalidPin => "invalid PIN".to_string(),
        wire::pb::pairing_error::Reason::SelfPairing => "self pairing is not allowed".to_string(),
        wire::pb::pairing_error::Reason::NotInPairingMode => "not in pairing mode".to_string(),
        wire::pb::pairing_error::Reason::Timeout => "pairing timed out".to_string(),
        wire::pb::pairing_error::Reason::UserRejected => "pairing rejected by peer".to_string(),
        wire::pb::pairing_error::Reason::ProtocolViolation => {
            if detail == "expected SPAKE2 initiator message"
                || detail == "expected key confirmation"
                || detail == "expected sealed identity"
                || detail == "host_id must be 16 bytes"
                || detail == "pubkey must be 32 bytes"
                || detail == "name is too long"
            {
                detail.to_string()
            } else {
                "pairing protocol violation".to_string()
            }
        }
        wire::pb::pairing_error::Reason::Unspecified => "pairing failed".to_string(),
    }
}

fn pair_mode_status(error: crate::pairing::PairModeError) -> Status {
    use crate::pairing::PairModeError;

    match error {
        PairModeError::NotActive => pairing_status(Code::PermissionDenied, "INVALID_PIN"),
        // The window is closed for good: saying so tells a guesser nothing
        // a closed window does not, and tells the person why the right PIN
        // no longer pairs.
        PairModeError::LockedOut => pairing_status(Code::PermissionDenied, "PAIRING_LOCKED"),
        PairModeError::AlreadyActive
        | PairModeError::InvalidPinFormat
        | PairModeError::SecretGeneration => pairing_status(Code::Internal, "PAIR_MODE_ERROR"),
    }
}

fn identity_status(error: IdentityError) -> Status {
    match error {
        IdentityError::DuplicatePubkey(_) => pairing_status(
            Code::InvalidArgument,
            "PROTOCOL_VIOLATION: pubkey is already trusted",
        ),
        other => Status::internal(other.to_string()),
    }
}

fn peer_pairing_error_status(error: wire::pb::PairingError) -> Status {
    use wire::pb::pairing_error::Reason;

    let reason = Reason::try_from(error.reason).unwrap_or(Reason::Unspecified);
    if matches!(reason, Reason::InvalidPin | Reason::NotInPairingMode) {
        return pairing_status(Code::PermissionDenied, "INVALID_PIN");
    }
    let code = match reason {
        Reason::NotInPairingMode => Code::FailedPrecondition,
        Reason::InvalidPin => Code::PermissionDenied,
        Reason::ProtocolViolation | Reason::SelfPairing | Reason::Unspecified => {
            Code::InvalidArgument
        }
        Reason::Timeout => Code::DeadlineExceeded,
        Reason::UserRejected => Code::Cancelled,
    };
    let label = match reason {
        Reason::Unspecified => "REASON_UNSPECIFIED",
        Reason::NotInPairingMode => "NOT_IN_PAIRING_MODE",
        Reason::InvalidPin => "INVALID_PIN",
        Reason::ProtocolViolation => "PROTOCOL_VIOLATION",
        Reason::Timeout => "TIMEOUT",
        Reason::UserRejected => "USER_REJECTED",
        Reason::SelfPairing => "SELF_PAIRING",
    };
    if error.detail.is_empty() {
        pairing_status(code, label)
    } else {
        pairing_status(code, format!("{label}: {}", error.detail))
    }
}

fn pairing_status(code: Code, reason: impl Into<String>) -> Status {
    Status::new(code, reason.into())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::Arc;
    use std::time::Duration;

    use tempfile::TempDir;
    use tokio::sync::mpsc;

    use super::*;
    use crate::routing::{
        Capabilities, Host, LinkCloseRequest, LinkId, LinkRole, Route, RoutingCore,
    };
    use crate::transport::{BoxedGrpcIo, in_process_channel, in_process_transport_pair};
    use crate::trust::{TrustEntry, TrustStore};

    fn service_fixture() -> (
        TempDir,
        PairingService,
        Arc<PairMode>,
        SharedTrustStore,
        DeviceIdentity,
        DeviceIdentity,
    ) {
        let data_dir = tempfile::tempdir().unwrap();
        let responder = DeviceIdentity::for_test(HostId::from_u128(1));
        let peer = DeviceIdentity::for_test(HostId::from_u128(2));
        let pair_mode = Arc::new(PairMode::new());
        let trust_store = Arc::new(std::sync::RwLock::new(TrustStore::default()));
        let routing = Arc::new(RoutingCore::new());
        let channels = Arc::new(crate::link::ChannelPool::new(Arc::new(
            crate::routing::LinkRegistry::default(),
        )));
        let connections = ConnectionManager::new(routing, channels, analytics::Analytics::off());
        let service = PairingService::new(
            pair_mode.clone(),
            LocalPairingIdentity::from_device_identity(&responder),
            "responder".to_string(),
            trust_store.clone(),
            Arc::default(),
            connections,
            data_dir.path().to_path_buf(),
        );
        (data_dir, service, pair_mode, trust_store, responder, peer)
    }

    async fn pairing_client_for(
        service: PairingService,
    ) -> (
        wire::pairing_service_client::PairingServiceClient<tonic::transport::Channel>,
        tokio::task::JoinHandle<()>,
    ) {
        pairing_client_for_reachability(service, PreTrustPairingReachability::Cloud).await
    }

    async fn pairing_client_for_reachability(
        service: PairingService,
        reachability: PreTrustPairingReachability,
    ) -> (
        wire::pairing_service_client::PairingServiceClient<tonic::transport::Channel>,
        tokio::task::JoinHandle<()>,
    ) {
        let (client_transport, server_transport) = in_process_transport_pair();
        let incoming = stream::once(async move {
            Ok::<_, std::io::Error>(BoxedGrpcIo::pre_trust_pairing(
                server_transport,
                reachability,
            ))
        });
        let task = tokio::spawn(async move {
            crate::transport::tonic_server_builder()
                .add_service(wire::pairing_service_server::PairingServiceServer::new(
                    service,
                ))
                .serve_with_incoming(incoming)
                .await
                .unwrap();
        });
        (
            wire::pairing_service_client::PairingServiceClient::new(in_process_channel(
                client_transport,
            )),
            task,
        )
    }

    async fn local_trusted_pairing_client_for(
        service: PairingService,
    ) -> (
        wire::pairing_service_client::PairingServiceClient<tonic::transport::Channel>,
        tokio::task::JoinHandle<()>,
    ) {
        let (client_transport, server_transport) = in_process_transport_pair();
        let incoming = stream::once(async move {
            Ok::<_, std::io::Error>(BoxedGrpcIo::trusted(
                server_transport,
                HostId::from_u128(0x7e57),
            ))
        });
        let task = tokio::spawn(async move {
            crate::transport::tonic_server_builder()
                .add_service(wire::pairing_service_server::PairingServiceServer::new(
                    service,
                ))
                .serve_with_incoming(incoming)
                .await
                .unwrap();
        });
        (
            wire::pairing_service_client::PairingServiceClient::new(in_process_channel(
                client_transport,
            )),
            task,
        )
    }

    #[derive(Clone)]
    struct ErrorOnlyPairingService {
        reason: wire::pb::pairing_error::Reason,
        detail: &'static str,
    }

    #[tonic::async_trait]
    impl wire::pairing_service_server::PairingService for ErrorOnlyPairingService {
        type PairStream = PairStream;

        async fn pair(
            &self,
            request: tonic::Request<tonic::Streaming<wire::pb::PairMessage>>,
        ) -> Result<tonic::Response<Self::PairStream>, Status> {
            let mut inbound = request.into_inner();
            tokio::spawn(
                async move { while inbound.message().await.is_ok_and(|m| m.is_some()) {} },
            );
            let message = wire::pb::PairMessage {
                body: Some(wire::pb::pair_message::Body::Error(
                    wire::pb::PairingError {
                        reason: self.reason as i32,
                        detail: self.detail.to_string(),
                    },
                )),
            };
            let output: PairStream = Box::pin(stream::once(async move { Ok(message) }));
            Ok(tonic::Response::new(output))
        }
    }

    #[derive(Clone)]
    struct HangingPairingService;

    #[tonic::async_trait]
    impl wire::pairing_service_server::PairingService for HangingPairingService {
        type PairStream = PairStream;

        async fn pair(
            &self,
            request: tonic::Request<tonic::Streaming<wire::pb::PairMessage>>,
        ) -> Result<tonic::Response<Self::PairStream>, Status> {
            let mut inbound = request.into_inner();
            tokio::spawn(
                async move { while inbound.message().await.is_ok_and(|m| m.is_some()) {} },
            );
            Ok(tonic::Response::new(Box::pin(stream::pending())))
        }
    }

    async fn hanging_pairing_client() -> (
        wire::pairing_service_client::PairingServiceClient<tonic::transport::Channel>,
        tokio::task::JoinHandle<()>,
    ) {
        let (client_transport, server_transport) = in_process_transport_pair();
        let incoming = stream::once(async move {
            Ok::<_, std::io::Error>(BoxedGrpcIo::pre_trust_pairing(
                server_transport,
                PreTrustPairingReachability::Cloud,
            ))
        });
        let task = tokio::spawn(async move {
            crate::transport::tonic_server_builder()
                .add_service(wire::pairing_service_server::PairingServiceServer::new(
                    HangingPairingService,
                ))
                .serve_with_incoming(incoming)
                .await
                .unwrap();
        });
        (
            wire::pairing_service_client::PairingServiceClient::new(in_process_channel(
                client_transport,
            )),
            task,
        )
    }

    async fn error_only_pairing_client_for(
        reason: wire::pb::pairing_error::Reason,
        detail: &'static str,
    ) -> (
        wire::pairing_service_client::PairingServiceClient<tonic::transport::Channel>,
        tokio::task::JoinHandle<()>,
    ) {
        let (client_transport, server_transport) = in_process_transport_pair();
        let incoming = stream::once(async move {
            Ok::<_, std::io::Error>(BoxedGrpcIo::pre_trust_pairing(
                server_transport,
                PreTrustPairingReachability::Cloud,
            ))
        });
        let task = tokio::spawn(async move {
            crate::transport::tonic_server_builder()
                .add_service(wire::pairing_service_server::PairingServiceServer::new(
                    ErrorOnlyPairingService { reason, detail },
                ))
                .serve_with_incoming(incoming)
                .await
                .unwrap();
        });
        (
            wire::pairing_service_client::PairingServiceClient::new(in_process_channel(
                client_transport,
            )),
            task,
        )
    }

    fn spake2_message(bytes: Vec<u8>) -> wire::pb::PairMessage {
        wire::pb::PairMessage {
            body: Some(wire::pb::pair_message::Body::Spake2Message(bytes)),
        }
    }

    fn key_confirmation(bytes: Vec<u8>) -> wire::pb::PairMessage {
        wire::pb::PairMessage {
            body: Some(wire::pb::pair_message::Body::KeyConfirmation(bytes)),
        }
    }

    fn sealed_identity(bytes: Vec<u8>) -> wire::pb::PairMessage {
        wire::pb::PairMessage {
            body: Some(wire::pb::pair_message::Body::SealedIdentity(bytes)),
        }
    }

    fn pairing_error(reason: wire::pb::pairing_error::Reason) -> wire::pb::PairMessage {
        wire::pb::PairMessage {
            body: Some(wire::pb::pair_message::Body::Error(
                wire::pb::PairingError {
                    reason: reason as i32,
                    detail: "peer abort".to_string(),
                },
            )),
        }
    }

    async fn next_spake2_body(
        stream: &mut tonic::Streaming<wire::pb::PairMessage>,
    ) -> wire::pb::pair_message::Body {
        stream
            .message()
            .await
            .unwrap()
            .expect("pairing stream ended")
            .body
            .expect("missing pairing body")
    }

    async fn start_spake2_client_stream(
        client: &mut wire::pairing_service_client::PairingServiceClient<tonic::transport::Channel>,
    ) -> (
        mpsc::Sender<wire::pb::PairMessage>,
        tonic::Streaming<wire::pb::PairMessage>,
    ) {
        let (tx, rx) = mpsc::channel(8);
        let outbound = stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|message| (message, rx))
        });
        let inbound = client
            .pair(tonic::Request::new(outbound))
            .await
            .unwrap()
            .into_inner();
        (tx, inbound)
    }

    #[test]
    fn spake2_ed25519_constants_are_valid_rfc9382_points() {
        assert!(spake2_ed25519_point(&SPAKE2_ED25519_M).is_ok());
        assert!(spake2_ed25519_point(&SPAKE2_ED25519_N).is_ok());
    }

    #[tokio::test]
    async fn pair_requires_active_pair_mode() {
        let (_dir, service, _pair_mode, _store, _responder, _peer) = service_fixture();
        let (mut client, task) = pairing_client_for(service).await;
        let (tx, rx) = mpsc::channel(1);
        drop(tx);
        let outbound = stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|message| (message, rx))
        });

        let error = client
            .pair(tonic::Request::new(outbound))
            .await
            .unwrap_err();
        assert_eq!(error.code(), Code::PermissionDenied);
        assert_eq!(error.message(), "INVALID_PIN");
        task.abort();
    }

    #[tokio::test]
    async fn pair_success_stores_peer_and_consumes_pin() {
        let (dir, service, pair_mode, trust_store, responder, peer) = service_fixture();
        pair_mode
            .start_pin_for_duration("123456".to_string(), Duration::from_secs(60))
            .unwrap();
        let (mut client, task) = pairing_client_for(service).await;
        let (tx, mut inbound) = start_spake2_client_stream(&mut client).await;
        let (spake_b, msg_b) = Spake2InitiatorState::start(b"123456").unwrap();

        tx.send(spake2_message(msg_b.clone())).await.unwrap();
        let msg_a = match next_spake2_body(&mut inbound).await {
            wire::pb::pair_message::Body::Spake2Message(bytes) => bytes,
            other => panic!("unexpected SPAKE2 body: {other:?}"),
        };
        let shared = spake_b.finish(&msg_a).unwrap();
        let keys = derive_spake2_keys(&shared, &msg_b, &msg_a).unwrap();
        tx.send(key_confirmation(hmac_confirm(
            &keys.kc_b,
            PAIR_CONFIRM_B,
            &keys.transcript_hash,
        )))
        .await
        .unwrap();
        match next_spake2_body(&mut inbound).await {
            wire::pb::pair_message::Body::KeyConfirmation(bytes) => {
                assert_eq!(bytes, keys.confirm_a)
            }
            other => panic!("unexpected key-confirmation body: {other:?}"),
        }
        let sealed_responder = match next_spake2_body(&mut inbound).await {
            wire::pb::pair_message::Body::SealedIdentity(bytes) => bytes,
            other => panic!("unexpected sealed-identity body: {other:?}"),
        };
        let responder_identity = open_identity(
            &keys.aead_a_to_b,
            A_TO_B_DIRECTION,
            &keys.transcript_hash,
            sealed_responder,
        )
        .unwrap();
        assert_eq!(responder_identity.host_id, responder.host_id.as_bytes());
        assert_eq!(responder_identity.pubkey, responder.public_key());
        assert_eq!(responder_identity.name, "responder");

        let initiator_identity = wire::pb::PairingIdentity {
            expires_at_ms: 0,
            host_id: peer.host_id.as_bytes().to_vec(),
            pubkey: peer.public_key().to_vec(),
            name: "peer".to_string(),
        };
        tx.send(sealed_identity(
            seal_identity(
                &keys.aead_b_to_a,
                B_TO_A_DIRECTION,
                &keys.transcript_hash,
                &initiator_identity,
            )
            .unwrap(),
        ))
        .await
        .unwrap();
        drop(tx);
        match next_spake2_body(&mut inbound).await {
            wire::pb::pair_message::Body::PairingComplete(_) => {}
            other => panic!("unexpected completion body: {other:?}"),
        }
        assert!(!pair_mode.is_active());

        let live = trust_store.read().unwrap();
        let entry = live.entry(peer.host_id).unwrap();
        assert_eq!(entry.pubkey, peer.public_key());
        assert_eq!(entry.name, "peer");
        assert_eq!(entry.reachabilities, vec![Reachability::Cloud]);
        drop(live);
        let persisted = TrustStore::load_or_create_in(dir.path()).unwrap();
        assert_eq!(
            persisted.entry(peer.host_id).unwrap().pubkey,
            peer.public_key()
        );
        task.abort();
    }

    #[tokio::test]
    async fn pair_initiator_helper_pairs_against_responder() {
        let (dir, service, pair_mode, trust_store, responder, peer) = service_fixture();
        pair_mode
            .start_pin_for_duration("123456".to_string(), Duration::from_secs(60))
            .unwrap();
        let (mut client, task) = pairing_client_for_reachability(
            service,
            PreTrustPairingReachability::NoReusableReachability,
        )
        .await;

        let peer_identity = LocalPairingIdentity::from_device_identity(&peer);
        let responder_peer = pair_initiator(&mut client, &peer_identity, "peer", b"123456")
            .await
            .unwrap();

        assert_eq!(responder_peer.host_id, responder.host_id);
        assert_eq!(responder_peer.pubkey, responder.public_key());
        assert_eq!(responder_peer.name, "responder");
        assert!(!pair_mode.is_active());
        let live = trust_store.read().unwrap();
        let entry = live.entry(peer.host_id).unwrap();
        assert!(entry.reachabilities.is_empty());
        drop(live);
        let persisted = TrustStore::load_or_create_in(dir.path()).unwrap();
        assert_eq!(
            persisted.entry(peer.host_id).unwrap().pubkey,
            peer.public_key()
        );
        task.abort();
    }

    #[tokio::test]
    async fn pair_initiator_preserves_peer_error_reason() {
        let (_dir, _service, _pair_mode, _trust_store, _responder, peer) = service_fixture();
        let (mut client, task) = error_only_pairing_client_for(
            wire::pb::pairing_error::Reason::ProtocolViolation,
            "bad frame",
        )
        .await;

        let peer_identity = LocalPairingIdentity::from_device_identity(&peer);
        let error = pair_initiator(&mut client, &peer_identity, "peer", b"123456")
            .await
            .unwrap_err();

        assert_eq!(error.code(), Code::InvalidArgument);
        assert_eq!(error.message(), "PROTOCOL_VIOLATION: bad frame");
        task.abort();
    }

    #[tokio::test]
    async fn pair_initiator_timeout_returns_pairing_timeout() {
        let (_dir, _service, _pair_mode, _trust_store, _responder, peer) = service_fixture();
        let (mut client, task) = hanging_pairing_client().await;

        let peer_identity = LocalPairingIdentity::from_device_identity(&peer);
        let error = pair_initiator_with_timeout(
            &mut client,
            &peer_identity,
            "peer",
            b"123456",
            Duration::from_millis(10),
        )
        .await
        .unwrap_err();

        assert_eq!(error.code(), Code::DeadlineExceeded);
        assert_eq!(error.message(), "PAIRING_TIMEOUT");
        task.abort();
    }

    #[tokio::test]
    async fn pair_invalid_pin_records_failure_without_consuming_pin() {
        let (_dir, service, pair_mode, _trust_store, _responder, _peer) = service_fixture();
        pair_mode
            .start_pin_for_duration("123456".to_string(), Duration::from_secs(60))
            .unwrap();
        let (mut client, task) = pairing_client_for(service).await;
        let (tx, mut inbound) = start_spake2_client_stream(&mut client).await;
        let (spake_b, msg_b) = Spake2InitiatorState::start(b"000000").unwrap();

        tx.send(spake2_message(msg_b.clone())).await.unwrap();
        let msg_a = match next_spake2_body(&mut inbound).await {
            wire::pb::pair_message::Body::Spake2Message(bytes) => bytes,
            other => panic!("unexpected SPAKE2 body: {other:?}"),
        };
        let shared = spake_b.finish(&msg_a).unwrap();
        let keys = derive_spake2_keys(&shared, &msg_b, &msg_a).unwrap();
        tx.send(key_confirmation(hmac_confirm(
            &keys.kc_b,
            PAIR_CONFIRM_B,
            &keys.transcript_hash,
        )))
        .await
        .unwrap();

        match next_spake2_body(&mut inbound).await {
            wire::pb::pair_message::Body::Error(error) => {
                assert_eq!(
                    error.reason,
                    wire::pb::pairing_error::Reason::InvalidPin as i32
                );
            }
            other => panic!("unexpected error body: {other:?}"),
        }
        assert!(pair_mode.is_active());
        drop(tx);
        task.abort();

        assert_eq!(
            pair_mode.start_qr_secret_for_duration([3_u8; 32], Duration::from_secs(60)),
            Err(crate::pairing::PairModeError::AlreadyActive)
        );
        let attempt = pair_mode.begin_attempt().unwrap();
        assert_eq!(attempt.secret(), b"123456");
    }

    #[tokio::test]
    async fn pair_peer_error_aborts_without_protocol_violation() {
        let (_dir, service, pair_mode, _trust_store, _responder, _peer) = service_fixture();
        pair_mode
            .start_pin_for_duration("123456".to_string(), Duration::from_secs(60))
            .unwrap();
        let (mut client, task) = pairing_client_for(service).await;
        let (tx, mut inbound) = start_spake2_client_stream(&mut client).await;

        tx.send(pairing_error(
            wire::pb::pairing_error::Reason::ProtocolViolation,
        ))
        .await
        .unwrap();
        assert!(inbound.message().await.unwrap().is_none());
        assert!(pair_mode.is_active());
        drop(tx);
        task.abort();
    }

    #[tokio::test]
    async fn pair_caps_in_flight_attempts() {
        let (_dir, service, pair_mode, _trust_store, _responder, _peer) = service_fixture();
        pair_mode
            .start_pin_for_duration("123456".to_string(), Duration::from_secs(60))
            .unwrap();
        let (mut client, task) = pairing_client_for(service).await;
        let mut held_attempts = Vec::new();
        for _ in 0..crate::pairing::PAIR_ATTEMPT_LIMIT {
            held_attempts.push(start_spake2_client_stream(&mut client).await);
        }

        let (tx, rx) = mpsc::channel(1);
        let outbound = stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|message| (message, rx))
        });
        let error = client
            .pair(tonic::Request::new(outbound))
            .await
            .unwrap_err();
        assert_eq!(error.code(), Code::PermissionDenied);
        assert_eq!(error.message(), "INVALID_PIN");

        drop(tx);
        drop(held_attempts);
        task.abort();
    }

    #[tokio::test]
    async fn pair_responder_timeout_releases_idle_attempts() {
        let (_dir, service, pair_mode, _trust_store, _responder, _peer) = service_fixture();
        let service = service.with_spake2_responder_timeout(Duration::from_millis(10));
        pair_mode
            .start_pin_for_duration("123456".to_string(), Duration::from_secs(60))
            .unwrap();
        let (mut client, task) = pairing_client_for(service).await;
        let mut held_attempts = Vec::new();
        for _ in 0..crate::pairing::PAIR_ATTEMPT_LIMIT {
            held_attempts.push(start_spake2_client_stream(&mut client).await);
        }

        let (_tx, _inbound) = tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let (tx, rx) = mpsc::channel(8);
                let outbound = stream::unfold(rx, |mut rx| async move {
                    rx.recv().await.map(|message| (message, rx))
                });
                match client.pair(tonic::Request::new(outbound)).await {
                    Ok(response) => return (tx, response.into_inner()),
                    Err(error)
                        if error.code() == Code::PermissionDenied
                            && error.message() == "INVALID_PIN" =>
                    {
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                    Err(error) => panic!("unexpected pairing error: {error}"),
                }
            }
        })
        .await
        .expect("idle attempts were not released after responder timeout");

        drop(held_attempts);
        task.abort();
    }

    #[tokio::test]
    async fn closed_profile_rejects_late_pairing_without_recreating_trust_files() {
        let (dir, service, _pair_mode, trust_store, _responder, peer) = service_fixture();
        let operation = service.trust_commit_lock.barrier().await;
        service.trust_commit_lock.close();
        std::fs::remove_dir_all(dir.path()).unwrap();
        drop(operation);
        let result = service
            .stage_peer_trust(
                peer.host_id,
                peer.public_key().to_vec(),
                "late peer".into(),
                Some(Reachability::Cloud),
            )
            .await;
        assert!(matches!(result, Err(error) if error.code() == Code::FailedPrecondition));
        assert!(trust_store.read().unwrap().entry(peer.host_id).is_none());
        assert!(!dir.path().exists());
    }

    #[tokio::test]
    async fn trust_commit_rolls_back_when_pin_pair_mode_is_cancelled_before_completion() {
        let (dir, service, pair_mode, trust_store, _responder, peer) = service_fixture();
        pair_mode
            .start_pin_for_duration("123456".to_string(), Duration::from_secs(60))
            .unwrap();
        let mut attempt = pair_mode.begin_attempt().unwrap();
        let mut commit = pair_mode.begin_commit(&mut attempt).unwrap();
        let guard = service
            .stage_peer_trust(
                peer.host_id,
                peer.public_key().to_vec(),
                "peer".to_string(),
                Some(Reachability::Cloud),
            )
            .await
            .unwrap();
        assert!(trust_store.read().unwrap().entry(peer.host_id).is_none());

        pair_mode.cancel();
        assert_eq!(
            pair_mode.complete_success(&mut commit),
            Err(crate::pairing::PairModeError::NotActive)
        );
        guard.rollback().await;

        assert!(trust_store.read().unwrap().entry(peer.host_id).is_none());
        let persisted = TrustStore::load_or_create_in(dir.path()).unwrap();
        assert!(persisted.entry(peer.host_id).is_none());
    }

    #[tokio::test]
    async fn dropped_replacement_guard_restores_trust_and_route_barrier() {
        let (_dir, service, _pair_mode, trust_store, _responder, peer) = service_fixture();
        let old_peer = DeviceIdentity::for_test(peer.host_id);
        trust_store.write().unwrap().insert_for_test(
            peer.host_id,
            TrustEntry {
                pubkey: old_peer.public_key().to_vec(),
                name: "old".to_string(),
                paired_at: chrono::DateTime::<Utc>::from_timestamp(100, 0).unwrap(),
                reachabilities: vec![Reachability::Cloud],
                signed_in: None,
                platform: None,
            },
        );
        let before = trust_store.read().unwrap().clone();
        assert_eq!(
            trust_store
                .write()
                .unwrap()
                .upsert_paired_peer(
                    peer.host_id,
                    peer.public_key().to_vec(),
                    "new".to_string(),
                    Reachability::Cloud,
                    Utc::now(),
                )
                .unwrap(),
            TrustStorePairingUpdate::PubkeyReplacementRequired
        );
        assert!(
            trust_store
                .read()
                .unwrap()
                .pubkey_for_host(peer.host_id)
                .is_none()
        );

        let guard = PeerTrustCommitGuard::new(
            PeerTrustCommitContext::new(
                trust_store.clone(),
                service.trust_commit_lock.clone(),
                service.connections.clone(),
                service.data_dir.clone(),
            ),
            peer.host_id,
            PeerTrustCommitState::new(
                before.clone(),
                before,
                TrustStorePairingUpdate::PubkeyReplacementRequired,
            )
            .finish_connection(),
            Arc::new(crate::trust::TrustGate::default()).barrier().await,
        );
        service.connections.teardown_host(peer.host_id).await;
        drop(guard);
        tokio::task::yield_now().await;

        assert_eq!(
            trust_store
                .read()
                .unwrap()
                .pubkey_for_host(peer.host_id)
                .unwrap(),
            old_peer.public_key()
        );
        // The route barrier is lifted: a fresh adjacency claim registers.
        let relay = HostId::from_u128(99);
        service
            .connections
            .routing()
            .apply_claim_up(
                relay,
                Host {
                    platform: None,
                    id: peer.host_id,
                    name: "peer".to_string(),
                    version: "test".to_string(),
                    capabilities: Capabilities {
                        cloud_relay: false,
                        kinds: vec![wire::Kind::ClaudeSdk as i32],
                    },
                    signed_in: Some(true),
                },
            )
            .await;
        assert_eq!(
            service.connections.known_routes(peer.host_id).await,
            vec![Route::Via(relay)]
        );
    }

    #[tokio::test]
    async fn trust_commits_are_serialized_and_keep_distinct_peers() {
        let (dir, service, _pair_mode, trust_store, _responder, peer_a) = service_fixture();
        let peer_b = DeviceIdentity::for_test(HostId::from_u128(3));
        let lock = service.trust_commit_lock.clone();

        let commit_a = commit_peer_trust(
            PeerTrustCommitContext::new(
                trust_store.clone(),
                lock.clone(),
                service.connections.clone(),
                service.data_dir.clone(),
            ),
            PeerTrustUpdate::new(
                peer_a.host_id,
                peer_a.public_key().to_vec(),
                "peer-a".to_string(),
                Some(Reachability::Cloud),
            ),
        );
        let commit_b = commit_peer_trust(
            PeerTrustCommitContext::new(
                trust_store.clone(),
                lock,
                service.connections.clone(),
                service.data_dir.clone(),
            ),
            PeerTrustUpdate::new(
                peer_b.host_id,
                peer_b.public_key().to_vec(),
                "peer-b".to_string(),
                Some(Reachability::Cloud),
            ),
        );

        let (result_a, result_b) = tokio::join!(commit_a, commit_b);
        result_a.unwrap();
        result_b.unwrap();

        let live = trust_store.read().unwrap();
        assert_eq!(live.entry(peer_a.host_id).unwrap().name, "peer-a");
        assert_eq!(live.entry(peer_b.host_id).unwrap().name, "peer-b");
        drop(live);
        let persisted = TrustStore::load_or_create_in(dir.path()).unwrap();
        assert_eq!(persisted.entry(peer_a.host_id).unwrap().name, "peer-a");
        assert_eq!(persisted.entry(peer_b.host_id).unwrap().name, "peer-b");
    }

    #[tokio::test]
    async fn pairing_service_rejects_non_pairing_transport() {
        let (_dir, service, pair_mode, _store, _responder, _peer) = service_fixture();
        pair_mode
            .start_pin_for_duration("123456".to_string(), Duration::from_secs(60))
            .unwrap();
        let (mut client, task) = local_trusted_pairing_client_for(service).await;
        let (tx, rx) = mpsc::channel::<wire::pb::PairMessage>(1);
        let outbound = stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|message| (message, rx))
        });

        let error = client
            .pair(tonic::Request::new(outbound))
            .await
            .unwrap_err();

        assert_eq!(error.code(), Code::PermissionDenied);
        assert_eq!(
            error.message(),
            "pairing RPC requires pre-trust pairing transport"
        );
        drop(tx);
        task.abort();
    }

    /// D9: the QR-carried 256-bit secret feeds the very same SPAKE2 stream
    /// the typed PIN does — same wire flow, same one-shot consumption. The
    /// secret never crosses the wire; only possession is proven.
    #[tokio::test]
    async fn the_qr_secret_drives_the_same_pairing_stream() {
        let (dir, service, pair_mode, trust_store, responder, peer) = service_fixture();
        let secret = [9_u8; 32];
        pair_mode
            .start_qr_secret_for_duration(secret, Duration::from_secs(60))
            .unwrap();
        let (mut client, task) = pairing_client_for(service).await;

        let peer_identity = LocalPairingIdentity::from_device_identity(&peer);
        let responder_peer = pair_initiator(&mut client, &peer_identity, "peer", &secret)
            .await
            .unwrap();

        assert_eq!(responder_peer.host_id, responder.host_id);
        assert_eq!(responder_peer.pubkey, responder.public_key());
        assert!(!pair_mode.is_active(), "the QR secret is one-shot");
        assert_eq!(
            trust_store
                .read()
                .unwrap()
                .entry(peer.host_id)
                .unwrap()
                .pubkey,
            peer.public_key()
        );
        let persisted = TrustStore::load_or_create_in(dir.path()).unwrap();
        assert_eq!(
            persisted.entry(peer.host_id).unwrap().pubkey,
            peer.public_key()
        );
        task.abort();
    }

    #[tokio::test]
    async fn a_wrong_qr_secret_records_a_failure_without_consuming_the_secret() {
        let (_dir, service, pair_mode, trust_store, _responder, peer) = service_fixture();
        pair_mode
            .start_qr_secret_for_duration([9_u8; 32], Duration::from_secs(60))
            .unwrap();
        let (mut client, task) = pairing_client_for(service).await;

        let peer_identity = LocalPairingIdentity::from_device_identity(&peer);
        let error = pair_initiator(&mut client, &peer_identity, "peer", &[0_u8; 32])
            .await
            .unwrap_err();

        assert_eq!(error.code(), Code::PermissionDenied);
        assert!(
            error.message().starts_with("INVALID_PIN"),
            "a wrong secret must fail with the opaque INVALID_PIN, got: {}",
            error.message()
        );
        assert!(
            pair_mode.is_active(),
            "a failed guess must not consume the secret"
        );
        assert!(trust_store.read().unwrap().entry(peer.host_id).is_none());
        task.abort();
    }

    #[tokio::test]
    async fn staged_trust_rejects_duplicate_pubkey_without_leaking_host_id() {
        let (_dir, service, _pair_mode, trust_store, _responder, peer) = service_fixture();
        trust_store.write().unwrap().insert_for_test(
            HostId::from_u128(99),
            TrustEntry {
                pubkey: peer.public_key().to_vec(),
                name: "other".to_string(),
                paired_at: chrono::DateTime::<Utc>::from_timestamp(100, 0).unwrap(),
                reachabilities: vec![Reachability::Cloud],
                signed_in: None,
                platform: None,
            },
        );

        let Err(error) = service
            .stage_peer_trust(
                peer.host_id,
                peer.public_key().to_vec(),
                "peer".to_string(),
                Some(Reachability::Cloud),
            )
            .await
        else {
            panic!("duplicate pubkey must not stage");
        };

        assert_eq!(error.code(), Code::InvalidArgument);
        assert_eq!(
            error.message(),
            "PROTOCOL_VIOLATION: pubkey is already trusted"
        );
        assert!(!error.message().contains(&HostId::from_u128(99).to_string()));
    }

    #[tokio::test]
    async fn trust_save_failure_aborts_the_secret_reservation() {
        let (dir, mut service, pair_mode, trust_store, _responder, peer) = service_fixture();
        let good_service = service.clone();
        let bad_data_dir = dir.path().join("not-a-directory");
        fs::write(&bad_data_dir, b"not a directory").unwrap();
        service.data_dir = bad_data_dir;
        pair_mode
            .start_pin_for_duration("123456".to_string(), Duration::from_secs(60))
            .unwrap();
        let peer_identity = LocalPairingIdentity::from_device_identity(&peer);

        let (mut bad_client, bad_task) = pairing_client_for(service).await;
        let error = pair_initiator(&mut bad_client, &peer_identity, "peer", b"123456")
            .await
            .unwrap_err();
        assert_eq!(error.code(), Code::Internal);
        assert!(
            pair_mode.is_active(),
            "a failed trust commit must release the secret reservation"
        );
        assert!(trust_store.read().unwrap().entry(peer.host_id).is_none());

        let (mut client, task) = pairing_client_for(good_service).await;
        pair_initiator(&mut client, &peer_identity, "peer", b"123456")
            .await
            .unwrap();
        assert!(!pair_mode.is_active());
        bad_task.abort();
        task.abort();
    }

    #[tokio::test]
    async fn replacement_save_failure_restores_trust_and_route_barrier() {
        let (dir, mut service, _pair_mode, trust_store, _responder, peer) = service_fixture();
        let old_peer = DeviceIdentity::for_test(peer.host_id);
        trust_store.write().unwrap().insert_for_test(
            peer.host_id,
            TrustEntry {
                pubkey: old_peer.public_key().to_vec(),
                name: "old".to_string(),
                paired_at: chrono::DateTime::<Utc>::from_timestamp(100, 0).unwrap(),
                reachabilities: vec![Reachability::Cloud],
                signed_in: None,
                platform: None,
            },
        );
        let bad_data_dir = dir.path().join("not-a-directory");
        fs::write(&bad_data_dir, b"not a directory").unwrap();
        service.data_dir = bad_data_dir;

        let Err(error) = service
            .stage_peer_trust(
                peer.host_id,
                peer.public_key().to_vec(),
                "new".to_string(),
                Some(Reachability::Cloud),
            )
            .await
        else {
            panic!("a failed trust save must not stage");
        };
        assert_eq!(error.code(), Code::Internal);
        assert_eq!(
            trust_store
                .read()
                .unwrap()
                .pubkey_for_host(peer.host_id)
                .unwrap(),
            old_peer.public_key()
        );

        // The route barrier is lifted: a fresh adjacency claim registers.
        let relay = HostId::from_u128(99);
        service
            .connections
            .routing()
            .apply_claim_up(
                relay,
                Host {
                    platform: None,
                    id: peer.host_id,
                    name: "peer".to_string(),
                    version: "test".to_string(),
                    capabilities: Capabilities {
                        cloud_relay: false,
                        kinds: vec![wire::Kind::ClaudeSdk as i32],
                    },
                    signed_in: Some(true),
                },
            )
            .await;
        assert_eq!(
            service.connections.known_routes(peer.host_id).await,
            vec![Route::Via(relay)]
        );
    }

    #[tokio::test]
    async fn pairing_replaces_existing_pubkey_after_teardown() {
        let (dir, service, pair_mode, trust_store, _responder, peer) = service_fixture();
        let old_peer = DeviceIdentity::for_test(peer.host_id);
        trust_store.write().unwrap().insert_for_test(
            peer.host_id,
            TrustEntry {
                pubkey: old_peer.public_key().to_vec(),
                name: "old".to_string(),
                paired_at: chrono::DateTime::<Utc>::from_timestamp(100, 0).unwrap(),
                reachabilities: vec![Reachability::Cloud],
                signed_in: None,
                platform: None,
            },
        );
        pair_mode
            .start_pin_for_duration("123456".to_string(), Duration::from_secs(60))
            .unwrap();
        let (mut client, task) = pairing_client_for(service).await;

        let peer_identity = LocalPairingIdentity::from_device_identity(&peer);
        pair_initiator(&mut client, &peer_identity, "new", b"123456")
            .await
            .unwrap();

        let live = trust_store.read().unwrap();
        let entry = live.entry(peer.host_id).unwrap();
        assert_eq!(entry.pubkey, peer.public_key());
        assert_eq!(entry.name, "new");
        drop(live);
        let persisted = TrustStore::load_or_create_in(dir.path()).unwrap();
        assert_eq!(
            persisted.entry(peer.host_id).unwrap().pubkey,
            peer.public_key()
        );
        task.abort();
    }

    /// Committing a same-host-id replacement tears down every existing link
    /// before the new key becomes active.
    #[tokio::test]
    async fn pairing_replacement_retires_the_existing_peer_link() {
        let data_dir = tempfile::tempdir().unwrap();
        let responder = DeviceIdentity::for_test(HostId::from_u128(1));
        let peer = DeviceIdentity::for_test(HostId::from_u128(2));
        let old_peer = DeviceIdentity::for_test(peer.host_id);
        let pair_mode = Arc::new(PairMode::new());
        let trust_store = Arc::new(std::sync::RwLock::new(TrustStore::default()));
        trust_store.write().unwrap().insert_for_test(
            peer.host_id,
            TrustEntry {
                pubkey: old_peer.public_key().to_vec(),
                name: "old".to_string(),
                paired_at: chrono::DateTime::<Utc>::from_timestamp(100, 0).unwrap(),
                reachabilities: vec![Reachability::Cloud],
                signed_in: None,
                platform: None,
            },
        );
        let routing = Arc::new(RoutingCore::new());
        let links = Arc::new(crate::routing::LinkRegistry::default());
        let (link_tx, _link_rx) = mpsc::channel(8);
        let peer_link = LinkId::new(peer.host_id);
        let mut close_rx = links
            .register(
                peer_link,
                Host {
                    id: peer.host_id,
                    name: "old".to_string(),
                    platform: None,
                    version: "test".to_string(),
                    capabilities: Capabilities::default(),
                    signed_in: Some(true),
                },
                link_tx,
                LinkRole::Peer,
                &[],
            )
            .await;
        let links_for_close = links.clone();
        tokio::spawn(async move {
            assert_eq!(close_rx.recv().await, Some(LinkCloseRequest::TrustReplaced));
            links_for_close.remove(&peer_link).await;
        });

        let connections = ConnectionManager::new(
            routing,
            Arc::new(crate::link::ChannelPool::new(links.clone())),
            analytics::Analytics::off(),
        );
        let service = PairingService::new(
            pair_mode.clone(),
            LocalPairingIdentity::from_device_identity(&responder),
            "responder".to_string(),
            trust_store.clone(),
            Arc::default(),
            connections,
            data_dir.path().to_path_buf(),
        );
        pair_mode
            .start_pin_for_duration("123456".to_string(), Duration::from_secs(60))
            .unwrap();
        let (mut client, task) = pairing_client_for(service).await;

        let peer_identity = LocalPairingIdentity::from_device_identity(&peer);
        pair_initiator(&mut client, &peer_identity, "new", b"123456")
            .await
            .unwrap();

        assert!(links.link_to_peer(peer.host_id).await.is_none());
        assert_eq!(
            trust_store
                .read()
                .unwrap()
                .entry(peer.host_id)
                .unwrap()
                .pubkey,
            peer.public_key()
        );
        task.abort();
    }
}
