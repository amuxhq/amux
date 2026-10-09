//! What a person does to a profile's edge through the front door: pairing
//! in both directions, the paired hosts, and the account the profile signs
//! in with.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::Utc;
use ring::rand::SecureRandom as _;
use tonic::Status;
use uuid::Uuid;
use wire::pairing_window::Secret;
use wire::start_pairing_request::Method;
use wire::{
    BeginPairRequest, DeviceIdentity, HostRef, PairingIdentity, PairingWindow, PeerEntry,
    RouteKind, StartPairingRequest, TrustSshPeerRequest,
};

use super::Edge;
use super::account::AccountRecord;
use crate::auth::AccessToken;
use crate::pairing::ssh::SshPairingPeer;
use crate::pairing::{PAIR_MODE_TTL, PairModeError, QR_SECRET_LEN};
use crate::services::{
    LocalPairingIdentity, PAIR_INITIATOR_TIMEOUT, PeerTrustCommitContext, PeerTrustUpdate,
    PendingPairing, begin_pair_initiator, commit_peer_trust,
};
use crate::trust::{Reachability, TrustEntry, TrustStore};
use crate::{HostId, audit};

const PUBKEY_LEN: usize = 32;
const MAX_PAIRING_NAME_BYTES: usize = 256;
/// How long a pairing dial to one address may take before the next is tried.
const PAIRING_QUIC_DIAL_TIMEOUT: Duration = Duration::from_secs(2);
/// A demo window is a standing shared secret; bound how long any window
/// can live.
const PAIR_MODE_MAX_TTL: Duration = Duration::from_secs(90 * 86_400);
/// Pending pairings a profile holds at once, awaiting confirmation.
const MAX_PENDING_PAIRS: usize = 32;

/// Pairings this device began and a person has yet to confirm or abandon.
#[derive(Default, Clone)]
pub(crate) struct PendingPairs(Arc<Mutex<HashMap<Uuid, PendingPair>>>);

struct PendingPair {
    pairing: PendingPairing,
    reachability: Reachability,
    via: RouteKind,
    method: analytics::Method,
}

/// Every refusal a guessing peer could learn from reads the same.
fn invalid_pin() -> Status {
    Status::permission_denied("INVALID_PIN")
}

fn opaque_pairing_status(error: Status) -> Status {
    match error.code() {
        tonic::Code::Unavailable | tonic::Code::Internal => error,
        tonic::Code::InvalidArgument if error.message().contains("SELF_PAIRING") => error,
        _ => invalid_pin(),
    }
}

/// Why a pairing failed, as analytics reads a refusal.
fn pairing_failure(status: &Status) -> analytics::PairingFailure {
    use analytics::PairingFailure;
    match status.code() {
        tonic::Code::PermissionDenied => PairingFailure::WrongSecret,
        tonic::Code::InvalidArgument if status.message().contains("SELF_PAIRING") => {
            PairingFailure::SelfPairing
        }
        tonic::Code::Unavailable => PairingFailure::Unreachable,
        tonic::Code::DeadlineExceeded => PairingFailure::TimedOut,
        _ => PairingFailure::Other,
    }
}

fn pair_mode_status(error: PairModeError) -> Status {
    match error {
        PairModeError::AlreadyActive => Status::failed_precondition("PAIR_MODE_ALREADY_ACTIVE"),
        PairModeError::InvalidPinFormat => {
            Status::invalid_argument("PIN must be six decimal digits")
        }
        PairModeError::SecretGeneration | PairModeError::NotActive => {
            Status::internal("PAIR_MODE_ERROR")
        }
    }
}

/// How long a pairing window stays open: what the request asks, else the
/// default, for every method alike.
fn pairing_ttl(ttl_seconds: Option<u64>) -> Result<Duration, Status> {
    let ttl = ttl_seconds.map_or(PAIR_MODE_TTL, Duration::from_secs);
    if ttl.is_zero() || ttl > PAIR_MODE_MAX_TTL {
        return Err(Status::invalid_argument(format!(
            "pairing ttl must be between 1 second and {} days",
            PAIR_MODE_MAX_TTL.as_secs() / 86_400
        )));
    }
    Ok(ttl)
}

fn host_from_bytes(field: &str, bytes: &[u8]) -> Result<HostId, Status> {
    Uuid::from_slice(bytes)
        .map_err(|error| Status::invalid_argument(format!("{field} is invalid: {error}")))
}

pub(crate) fn peer_entry_to_wire(host_id: HostId, entry: &TrustEntry) -> PeerEntry {
    PeerEntry {
        host_id: host_id.as_bytes().to_vec(),
        name: entry.name.clone(),
        pubkey: entry.pubkey.clone(),
        paired_at_ms: entry.paired_at.timestamp_millis(),
        reachabilities: entry
            .reachabilities
            .iter()
            .map(|reachability| wire::PeerReachability {
                kind: Some(match reachability {
                    Reachability::Cloud => wire::peer_reachability::Kind::Cloud(wire::Empty {}),
                    Reachability::Ssh { target, profile } => {
                        wire::peer_reachability::Kind::SshTarget(wire::SshTarget {
                            target: target.clone(),
                            profile_id: profile.as_bytes().to_vec(),
                        })
                    }
                    Reachability::Direct { addrs } => {
                        wire::peer_reachability::Kind::Direct(wire::DirectReachability {
                            addrs: addrs.iter().map(ToString::to_string).collect(),
                        })
                    }
                }),
            })
            .collect(),
    }
}

fn resolve_peer(store: &TrustStore, peer: HostRef) -> Result<(HostId, TrustEntry), Status> {
    match peer.of {
        None => Err(Status::invalid_argument("HostRef.of is required")),
        Some(wire::host_ref::Of::HostId(bytes)) => {
            let host = host_from_bytes("HostRef.host_id", &bytes)?;
            let entry = store
                .entry(host)
                .cloned()
                .ok_or_else(|| Status::not_found(format!("peer {host} is not trusted")))?;
            Ok((host, entry))
        }
        Some(wire::host_ref::Of::Name(name)) => {
            if name.is_empty() {
                return Err(Status::invalid_argument("HostRef.name must not be empty"));
            }
            let matches = store
                .entries()
                .filter(|(_, entry)| entry.name == name)
                .map(|(host, entry)| (host, entry.clone()))
                .collect::<Vec<_>>();
            match matches.as_slice() {
                [(host, entry)] => Ok((*host, entry.clone())),
                [] => Err(Status::not_found(format!(
                    "peer named {name} is not trusted"
                ))),
                _ => Err(Status::invalid_argument(format!(
                    "peer name {name} is ambiguous; use host_id"
                ))),
            }
        }
    }
}

impl Edge {
    fn trust_context(&self) -> PeerTrustCommitContext {
        PeerTrustCommitContext::new(
            self.trust.clone(),
            self.trust_gate.clone(),
            self.connections.clone(),
            self.dir.clone(),
        )
    }

    pub fn device_identity(&self) -> DeviceIdentity {
        DeviceIdentity {
            host_id: self.host_id().as_bytes().to_vec(),
            name: self.host_name.clone(),
            pubkey: self.public_key().to_vec(),
        }
    }

    /// Puts the profile in pairing mode with a fresh one-shot secret, or a
    /// reusable demo PIN, and says how a peer reaches it.
    pub async fn start_pairing(
        &self,
        request: StartPairingRequest,
    ) -> Result<PairingWindow, Status> {
        let _operation = self.trust_gate.barrier().await;
        if self.trust_gate.is_closed() {
            return Err(Status::failed_precondition("profile is unavailable"));
        }
        let method = request
            .method
            .ok_or_else(|| Status::invalid_argument("StartPairingRequest.method is required"))?;
        if self.host_name.len() > MAX_PAIRING_NAME_BYTES {
            return Err(Status::invalid_argument(
                "host_name is too long for pairing",
            ));
        }
        let ttl = pairing_ttl(request.ttl_seconds)?;
        self.reachability.requery();
        let (method, secret) = match method {
            Method::Pin(_) => {
                let pin = format!("{:06}", Uuid::new_v4().as_u128() % 1_000_000);
                self.pair_mode
                    .start_pin_for_duration(pin.clone(), ttl)
                    .map_err(pair_mode_status)?;
                ("pin", Secret::Pin(pin))
            }
            Method::Qr(_) => {
                let mut secret = [0_u8; QR_SECRET_LEN];
                ring::rand::SystemRandom::new()
                    .fill(&mut secret)
                    .map_err(|_| pair_mode_status(PairModeError::SecretGeneration))?;
                self.pair_mode
                    .start_qr_secret_for_duration(secret, ttl)
                    .map_err(pair_mode_status)?;
                ("qr", Secret::QrSecret(secret.to_vec()))
            }
            Method::DemoPin(pin) => {
                self.pair_mode
                    .start_demo_pin(pin.clone(), ttl)
                    .map_err(pair_mode_status)
                    .inspect_err(|error| audit::pairing_failure("demo", error))?;
                tracing::warn!(
                    ttl_seconds = ttl.as_secs(),
                    "demo pairing active: a reusable fixed PIN pairs any device that presents it"
                );
                ("demo", Secret::Pin(pin))
            }
        };
        audit::pairing_start(method);
        self.analytics.record(analytics::Event::PairingStarted {
            role: analytics::Role::Offerer,
            method: match method {
                "qr" => analytics::Method::Qr,
                _ => analytics::Method::Pin,
            },
        });
        Ok(PairingWindow {
            identity: Some(PairingIdentity {
                expires_at_ms: 0,
                host_id: self.host_id().as_bytes().to_vec(),
                pubkey: self.public_key().to_vec(),
                name: self.host_name.clone(),
            }),
            ttl_seconds: ttl.as_secs(),
            addrs: self
                .pairing_addrs()
                .into_iter()
                .map(|addr| addr.to_string())
                .collect(),
            cloud_url: self
                .account
                .service()
                .filter(|_| self.account.wants_cloud())
                .map(|service| service.as_str().to_owned()),
            secret: Some(secret),
        })
    }

    pub fn pairing_active(&self) -> bool {
        self.pair_mode.is_active()
    }

    pub async fn cancel_pairing(&self) -> Result<(), Status> {
        let _operation = self.trust_gate.barrier().await;
        if self.trust_gate.is_closed() {
            return Err(Status::failed_precondition("profile is unavailable"));
        }
        if self.pair_mode.cancel() {
            audit::pairing_cancel("admin");
        }
        Ok(())
    }

    /// Runs the initiator's half of pairing against a host in pairing mode:
    /// its LAN addresses first, then the relay. The result is held until a
    /// person confirms the peer it names.
    pub async fn begin_pair(&self, request: BeginPairRequest) -> Result<wire::PendingPair, Status> {
        let method = match &request.secret {
            Some(wire::begin_pair_request::Secret::QrSecret(_)) => analytics::Method::Qr,
            _ => analytics::Method::Pin,
        };
        self.analytics.record(analytics::Event::PairingStarted {
            role: analytics::Role::Joiner,
            method,
        });
        let begun = self.reach_pairing_host(request, method).await;
        if let Err(status) = &begun {
            self.pairing_failed(method, status);
        }
        begun
    }

    fn pairing_failed(&self, method: analytics::Method, status: &Status) {
        self.analytics.record(analytics::Event::PairingFailed {
            role: analytics::Role::Joiner,
            method,
            reason: pairing_failure(status),
        });
    }

    async fn reach_pairing_host(
        &self,
        request: BeginPairRequest,
        method: analytics::Method,
    ) -> Result<wire::PendingPair, Status> {
        if self.trust_gate.is_closed() {
            return Err(Status::failed_precondition("profile is unavailable"));
        }
        let requested_host = if request.host_id.is_empty() {
            None
        } else {
            Some(
                host_from_bytes("BeginPairRequest.host_id", &request.host_id)
                    .map_err(|_| invalid_pin())?,
            )
        };
        if requested_host == Some(self.host_id()) {
            return Err(Status::invalid_argument("SELF_PAIRING"));
        }
        let secret = match request.secret {
            Some(wire::begin_pair_request::Secret::Pin(pin))
                if pin.len() == 6 && pin.bytes().all(|byte| byte.is_ascii_digit()) =>
            {
                pin.into_bytes()
            }
            Some(wire::begin_pair_request::Secret::QrSecret(secret))
                if secret.len() == QR_SECRET_LEN =>
            {
                secret
            }
            _ => return Err(invalid_pin()),
        };
        let requested_addrs = request
            .addrs
            .into_iter()
            .map(|addr| addr.parse::<SocketAddr>().map_err(|_| invalid_pin()))
            .collect::<Result<Vec<_>, _>>()?;
        let found_addrs = requested_host
            .map(|host| self.reachability.found_addrs(host))
            .unwrap_or_default();
        let mut direct_addrs = found_addrs.clone();
        for addr in requested_addrs {
            if !direct_addrs.contains(&addr) {
                direct_addrs.push(addr);
            }
        }
        let identity = LocalPairingIdentity::from_device_identity(&self.identity);
        let mut last_unreachable = None;
        let mut selected = None;
        // A host with no pairing window open refuses the connection before
        // reading the secret; that refusal answers like any other secret
        // failure, whether it ends the dial or the call that follows it.
        for addr in direct_addrs {
            let dial = match tokio::time::timeout(
                PAIRING_QUIC_DIAL_TIMEOUT,
                crate::transport::pairing_quic_channel(&self.quic_endpoint, addr),
            )
            .await
            {
                Ok(Ok(dial)) => dial,
                Ok(Err(crate::transport::TransportError::NoPairingWindow)) => {
                    return Err(invalid_pin());
                }
                Ok(Err(error)) => {
                    last_unreachable = Some(error.to_string());
                    continue;
                }
                Err(_) => {
                    last_unreachable = Some(format!("pairing QUIC dial to {addr} timed out"));
                    continue;
                }
            };
            let result = begin_pair_initiator(
                &mut wire::pairing_service_client::PairingServiceClient::new(dial.channel.clone()),
                &identity,
                &self.host_name,
                &secret,
            )
            .await;
            match result {
                Err(_) if dial.refused() => return Err(invalid_pin()),
                Ok(pending) => {
                    selected = Some((
                        pending,
                        Reachability::Direct { addrs: vec![addr] },
                        RouteKind::Direct,
                        found_addrs.contains(&addr),
                    ));
                    break;
                }
                Err(error)
                    if matches!(
                        error.code(),
                        tonic::Code::Unavailable | tonic::Code::Internal
                    ) =>
                {
                    last_unreachable = Some(error.to_string());
                }
                Err(error) => return Err(opaque_pairing_status(error)),
            }
        }
        if selected.is_none()
            && let Some(host) = requested_host
            && self.connections.has_cloud_route(host).await
        {
            let channel = self
                .connections
                .cloud_pairing_channel_to(host)
                .await
                .map_err(|error| Status::unavailable(error.to_string()))?;
            let pending = begin_pair_initiator(
                &mut wire::pairing_service_client::PairingServiceClient::new(channel),
                &identity,
                &self.host_name,
                &secret,
            )
            .await
            .map_err(opaque_pairing_status)?;
            selected = Some((pending, Reachability::Cloud, RouteKind::Relay, false));
        }
        let (pending, reachability, via, from_discovery) =
            selected.ok_or_else(|| {
                Status::unavailable(last_unreachable.unwrap_or_else(|| {
                    crate::link::ChannelError::CloudPairingUnavailable.to_string()
                }))
            })?;
        let peer_host = host_from_bytes("PairingIdentity.host_id", &pending.peer.host_id)
            .map_err(|_| invalid_pin())?;
        if peer_host == self.host_id() || pending.peer.pubkey == self.public_key() {
            return Err(Status::invalid_argument("SELF_PAIRING"));
        }
        if requested_host.is_some_and(|host| host != peer_host) {
            return Err(invalid_pin());
        }
        // A machine discovery found that already has a key here must present
        // that key: a stranger answering at its address does not replace it.
        if from_discovery
            && let Ok(store) = self.trust.read()
            && let Some(pinned) = store.pubkey_for_host(peer_host)
            && pinned != pending.peer.pubkey.as_slice()
        {
            return Err(invalid_pin());
        }
        let remaining_ms = pending
            .peer
            .expires_at_ms
            .checked_sub(Utc::now().timestamp_millis())
            .filter(|remaining| *remaining > 0)
            .ok_or_else(invalid_pin)?;
        let token = Uuid::new_v4();
        let response = wire::PendingPair {
            token: token.as_bytes().to_vec(),
            peer: Some(pending.peer.clone()),
            via: via as i32,
        };
        {
            let mut pending_pairs = self.pending.0.lock().unwrap();
            if pending_pairs.len() >= MAX_PENDING_PAIRS {
                return Err(Status::resource_exhausted("too many pending pairings"));
            }
            pending_pairs.insert(
                token,
                PendingPair {
                    pairing: pending,
                    reachability,
                    via,
                    method,
                },
            );
        }
        let pending_pairs = Arc::downgrade(&self.pending.0);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(remaining_ms as u64)).await;
            if let Some(pending_pairs) = pending_pairs.upgrade() {
                pending_pairs.lock().unwrap().remove(&token);
            }
        });
        Ok(response)
    }

    fn take_pending(&self, token: &[u8]) -> Result<PendingPair, Status> {
        let token = Uuid::from_slice(token).map_err(|_| invalid_pin())?;
        self.pending
            .0
            .lock()
            .unwrap()
            .remove(&token)
            .ok_or_else(invalid_pin)
    }

    /// Finishes a pairing a person confirmed: both sides commit the other's
    /// key, and this side dials the peer at the route pairing found.
    pub async fn confirm_pair(&self, token: &[u8]) -> Result<PeerEntry, Status> {
        let pending = self.take_pending(token)?;
        let method = pending.method;
        let confirmed = self.confirm_pending(pending).await;
        match &confirmed {
            Ok(host) => self.analytics.record(analytics::Event::PairingSucceeded {
                role: analytics::Role::Joiner,
                method,
                remote_host: *host,
            }),
            Err(status) => self.pairing_failed(method, status),
        }
        let host = confirmed?;
        self.peer_entry(HostRef {
            of: Some(wire::host_ref::Of::HostId(host.as_bytes().to_vec())),
        })
    }

    async fn confirm_pending(&self, pending: PendingPair) -> Result<HostId, Status> {
        let peer = tokio::time::timeout(PAIR_INITIATOR_TIMEOUT, pending.pairing.confirm())
            .await
            .map_err(|_| invalid_pin())?
            .map_err(opaque_pairing_status)?;
        let host = peer.host_id;
        // Secret-based pairing reaches its peer directly or through the
        // relay; SSH pairings commit through `trust_ssh_peer`.
        let method = match pending.via {
            RouteKind::Relay => "cloud",
            _ => "direct_pin",
        };
        self.commit_peer(peer, Some(pending.reachability), method)
            .await?;
        Ok(host)
    }

    pub async fn abandon_pair(&self, token: &[u8]) -> Result<(), Status> {
        let pending = self.take_pending(token)?;
        self.analytics.record(analytics::Event::PairingFailed {
            role: analytics::Role::Joiner,
            method: pending.method,
            reason: analytics::PairingFailure::Abandoned,
        });
        tokio::time::timeout(PAIR_INITIATOR_TIMEOUT, pending.pairing.abandon())
            .await
            .map_err(|_| invalid_pin())?
            .map_err(opaque_pairing_status)
    }

    /// Trusts a peer whose identity an SSH exchange carried: SSH already
    /// authenticated it, so no secret is exchanged.
    pub async fn trust_ssh_peer(&self, request: TrustSshPeerRequest) -> Result<(), Status> {
        let identity = request
            .peer
            .ok_or_else(|| Status::invalid_argument("TrustSshPeerRequest.peer is required"))?;
        let host = host_from_bytes("PairingIdentity.host_id", &identity.host_id)?;
        if identity.pubkey.len() != PUBKEY_LEN {
            return Err(Status::invalid_argument(
                "PairingIdentity.pubkey must be 32 bytes",
            ));
        }
        if identity.name.len() > MAX_PAIRING_NAME_BYTES {
            return Err(Status::invalid_argument("PairingIdentity.name is too long"));
        }
        let reachability = request
            .ssh_target
            .map(|target| {
                if target.target.trim().is_empty() || target.target.starts_with('-') {
                    return Err(Status::invalid_argument(
                        "TrustSshPeerRequest.ssh_target is invalid",
                    ));
                }
                let profile = Uuid::from_slice(&target.profile_id).map_err(|_| {
                    Status::invalid_argument(
                        "TrustSshPeerRequest.ssh_target.profile_id must be 16 bytes",
                    )
                })?;
                Ok(Reachability::Ssh {
                    target: target.target,
                    profile,
                })
            })
            .transpose()?;
        // The side that dialled knows how to reach the other; SSH pairings
        // reach the daemon only once the exchange worked, so a start and
        // its success are counted together.
        let role = match reachability {
            Some(_) => analytics::Role::Joiner,
            None => analytics::Role::Offerer,
        };
        self.commit_peer(
            SshPairingPeer {
                host_id: host,
                pubkey: identity.pubkey,
                name: identity.name,
            },
            reachability,
            "ssh",
        )
        .await?;
        let method = analytics::Method::Ssh;
        self.analytics
            .record(analytics::Event::PairingStarted { role, method });
        self.analytics.record(analytics::Event::PairingSucceeded {
            role,
            method,
            remote_host: host,
        });
        Ok(())
    }

    /// Commits trust in a peer and dials it at the route that proved it.
    pub(crate) async fn commit_peer(
        &self,
        peer: SshPairingPeer,
        reachability: Option<Reachability>,
        method: &'static str,
    ) -> Result<(), Status> {
        if peer.host_id == self.host_id() || peer.pubkey == self.public_key() {
            return Err(Status::invalid_argument("SELF_PAIRING"));
        }
        audit::pairing_start(method);
        commit_peer_trust(
            self.trust_context(),
            PeerTrustUpdate::new(peer.host_id, peer.pubkey, peer.name, reachability.clone()),
        )
        .await?;
        audit::pairing_success(method, peer.host_id);
        if let Some(reachability) = reachability {
            self.reachability
                .spawn_pair_time_link(peer.host_id, reachability);
        }
        Ok(())
    }

    /// Trusts another edge directly, as a finished pairing would, without
    /// the secret exchange: for harnesses that build topologies of hosts
    /// that are already paired.
    #[doc(hidden)]
    pub async fn trust(&self, peer: &Edge) -> Result<(), Status> {
        self.commit_peer(
            SshPairingPeer {
                host_id: peer.host_id(),
                pubkey: peer.public_key().to_vec(),
                name: peer.host_name().to_owned(),
            },
            None,
            "harness",
        )
        .await
    }

    /// Paired hosts, by name.
    pub fn list_peers(&self) -> Result<Vec<PeerEntry>, Status> {
        let store = self
            .trust
            .read()
            .map_err(|_| Status::internal("trust store lock is poisoned"))?;
        let mut entries = store
            .entries()
            .map(|(host, entry)| (host, entry.clone()))
            .collect::<Vec<_>>();
        entries.sort_unstable_by(|(left_id, left), (right_id, right)| {
            left.name
                .cmp(&right.name)
                .then_with(|| left_id.cmp(right_id))
        });
        Ok(entries
            .iter()
            .map(|(host, entry)| peer_entry_to_wire(*host, entry))
            .collect())
    }

    pub fn peer_entry(&self, peer: HostRef) -> Result<PeerEntry, Status> {
        let store = self
            .trust
            .read()
            .map_err(|_| Status::internal("trust store lock is poisoned"))?;
        let (host, entry) = resolve_peer(&store, peer)?;
        Ok(peer_entry_to_wire(host, &entry))
    }

    /// Forgets a paired host: its key leaves the trust store, its links
    /// close with USER_REVOKED, and every connection it holds here ends.
    pub async fn unpair(&self, peer: HostRef, reason: String) -> Result<PeerEntry, Status> {
        let reason = match reason.trim() {
            "" => "user".to_owned(),
            reason => reason.to_owned(),
        };
        let (host, removed) = {
            let _operation = self.trust_gate.barrier().await;
            if self.trust_gate.is_closed() {
                return Err(Status::failed_precondition("profile is unavailable"));
            }
            let (host, removed, staged) = {
                let store = self
                    .trust
                    .read()
                    .map_err(|_| Status::internal("trust store lock is poisoned"))?;
                let (host, _) = resolve_peer(&store, peer)?;
                let mut staged = store.clone();
                let removed = staged
                    .remove(host)
                    .ok_or_else(|| Status::not_found(format!("peer {host} is not trusted")))?;
                staged
                    .save_in(&self.dir)
                    .map_err(|error| Status::internal(error.to_string()))?;
                (host, removed, staged)
            };
            *self
                .trust
                .write()
                .map_err(|_| Status::internal("trust store lock is poisoned"))? = staged;
            (host, removed)
        };
        self.connections.close_host_access(host).await;
        audit::trust_remove(host, &removed.name, removed.paired_at, Utc::now(), &reason);
        Ok(peer_entry_to_wire(host, &removed))
    }

    /// Binds the profile to an account, or signs a bound profile back in,
    /// and starts its cloud link unless the connection is paused.
    pub(crate) async fn bind_account(
        &self,
        record: AccountRecord,
        access: AccessToken,
    ) -> std::io::Result<()> {
        self.stop_cloud().await;
        self.account.bind(record, access).await?;
        self.start_cloud().await;
        Ok(())
    }

    pub(crate) async fn sign_out(&self) -> std::io::Result<()> {
        self.account.sign_out().await?;
        self.stop_cloud().await;
        Ok(())
    }

    pub(crate) async fn set_paused(&self, paused: bool) -> std::io::Result<()> {
        self.account.set_paused(paused).await?;
        if paused {
            self.stop_cloud().await;
        } else {
            self.start_cloud().await;
        }
        Ok(())
    }

    /// Whether the profile has nothing an account would adopt: no paired
    /// hosts. Agents are counted by the caller, who holds the registry.
    pub(crate) fn has_no_peers(&self) -> bool {
        self.trust
            .read()
            .is_ok_and(|store| store.entries().next().is_none())
    }
}
