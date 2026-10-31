//! What a person does to a profile's edge through its socket: pairing
//! in both directions, the paired hosts, and the account the profile signs
//! in with. The pairing window, the exchange and every trust change are the
//! profile's core's; what is left here is reading the request, dialling the
//! host in pairing mode, and what follows on the connections.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use daemon_core::pairing::{
    Initiate, Method as CoreMethod, PairFailure, WindowMethod, WindowSecret,
};
use daemon_core::payload::{CallError, ClientRequest, Response};
use daemon_core::trust::{TrustChange, TrustChanged, TrustError};
use tonic::Status;
use uuid::Uuid;
use wire::pairing_window::Secret;
use wire::start_pairing_request::Method;
use wire::{
    BeginPairRequest, DeviceIdentity, HostRef, PairingIdentity, PairingWindow, RouteKind,
    StartPairingRequest, TrustSshPeerRequest,
};

use super::Edge;
use super::account::AccountRecord;
use crate::HostId;
use crate::auth::AccessToken;
use crate::driver::pairing::status as pairing_status;
use crate::pairing::PAIR_MODE_TTL;
use crate::runtime::ProfileRuntime;
use crate::services::LocalPairingIdentity;
use crate::trust::{Reachability, TrustEntry, TrustStore};

const QR_SECRET_LEN: usize = daemon_core::pairing::QR_SECRET_LEN;
/// A demo window is a standing shared secret; bound how long any window
/// can live.
const PAIR_MODE_MAX_TTL: Duration = Duration::from_secs(90 * 86_400);

/// Every refusal a guessing peer could learn from reads the same.
fn invalid_pin() -> Status {
    Status::permission_denied("INVALID_PIN")
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

/// A call the core refused, as the front door says it.
fn core_status(error: CallError) -> Status {
    use daemon_core::pairing::WindowRefusal;
    match error {
        CallError::Window(WindowRefusal::AlreadyActive) => {
            Status::failed_precondition("PAIR_MODE_ALREADY_ACTIVE")
        }
        CallError::Window(WindowRefusal::InvalidPinFormat) => {
            Status::invalid_argument("PIN must be six decimal digits")
        }
        CallError::Window(WindowRefusal::NoEntropy) => Status::internal("PAIR_MODE_ERROR"),
        CallError::Pairing(failure) => pairing_status(&failure),
        CallError::Trust(TrustError::NotTrusted(host)) => {
            Status::not_found(format!("peer {host} is not trusted"))
        }
        CallError::Trust(TrustError::DuplicatePubkey(_)) => {
            Status::invalid_argument("PROTOCOL_VIOLATION: pubkey is already trusted")
        }
        CallError::Trust(error) => Status::internal(error.to_string()),
        CallError::Store(_) => Status::failed_precondition("profile is unavailable"),
        other => Status::internal(format!("{other:?}")),
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

/// The refusal a person is shown for a pairing this host began.
fn opaque(error: Status) -> Status {
    match error.code() {
        tonic::Code::ResourceExhausted => error,
        _ => pairing_status(&crate::driver::pairing::failure(&error).opaque()),
    }
}

pub(crate) fn peer_entry_to_wire(host_id: HostId, entry: &TrustEntry) -> wire::Peer {
    wire::Peer {
        host_id: host_id.as_bytes().to_vec(),
        name: entry.name.clone(),
        pubkey: entry.pubkey.clone(),
        ssh_target: entry.ssh.clone().map(|target| wire::SshTarget { target }),
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
    /// The profile's runtime, whose core pairs and holds trust.
    fn profile(&self) -> Result<Arc<ProfileRuntime>, Status> {
        self.runtime
            .upgrade()
            .ok_or_else(|| Status::failed_precondition("profile is unavailable"))
    }

    /// Hands a call to the profile's core and waits for its answer.
    async fn ask(&self, request: ClientRequest) -> Result<Option<Response>, Status> {
        self.profile()?
            .driver()
            .ask(request)
            .await
            .map_err(core_status)
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
        let method = request
            .method
            .ok_or_else(|| Status::invalid_argument("StartPairingRequest.method is required"))?;
        let ttl = pairing_ttl(request.ttl_seconds)?;
        self.reachability.requery();
        let (label, method) = match method {
            Method::Pin(_) => ("pin", WindowMethod::Pin),
            Method::Qr(_) => ("qr", WindowMethod::Qr),
            Method::DemoPin(pin) => ("demo", WindowMethod::Demo(pin)),
        };
        let opened = self
            .ask(ClientRequest::StartPairing {
                method,
                ttl_ms: u64::try_from(ttl.as_millis()).unwrap_or(u64::MAX),
            })
            .await?;
        let secret = match opened {
            Some(Response::PairingWindow(WindowSecret::Pin(pin))) => Secret::Pin(pin),
            Some(Response::PairingWindow(WindowSecret::Qr(secret))) => Secret::QrSecret(secret),
            other => return Err(Status::internal(format!("the core opened {other:?}"))),
        };
        if label == "demo" {
            tracing::warn!(
                ttl_seconds = ttl.as_secs(),
                "demo pairing active: a reusable fixed PIN pairs any device that presents it"
            );
        }
        self.analytics.record(analytics::Event::PairingStarted {
            role: analytics::Role::Offerer,
            method: match label {
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

    /// Whether too many wrong secrets closed the last pairing window.
    pub fn pairing_locked_out(&self) -> bool {
        self.pair_mode.is_locked_out()
    }

    pub async fn cancel_pairing(&self) -> Result<(), Status> {
        if let Some(Response::PairingCancelled { was_active: true }) =
            self.ask(ClientRequest::CancelPairing).await?
        {}
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
            self.analytics.record(analytics::Event::PairingFailed {
                role: analytics::Role::Joiner,
                method,
                reason: pairing_failure(status),
            });
        }
        begun
    }

    async fn reach_pairing_host(
        &self,
        request: BeginPairRequest,
        method: analytics::Method,
    ) -> Result<wire::PendingPair, Status> {
        let runtime = self.profile()?;
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
        // Which addresses, how long each, and the forwarders after them,
        // are the core's.
        let plan = runtime
            .driver()
            .look(move |daemon| {
                daemon.pairing_dial(
                    requested_host.map(|host| daemon_core::HostId(*host.as_bytes())),
                    &requested_addrs,
                )
            })
            .await
            .ok_or_else(|| Status::unavailable("the profile is stopping"))?;
        let local =
            LocalPairingIdentity::from_device_identity(&self.identity).named(&self.host_name);
        let initiate = |found: bool, reachability: Reachability| Initiate {
            local: local.clone(),
            secret: secret.clone(),
            expect: requested_host.map(|host| daemon_core::HostId(*host.as_bytes())),
            found,
            reachability,
            method: match method {
                analytics::Method::Qr => CoreMethod::Qr,
                _ => CoreMethod::Pin,
            },
        };
        let refused = |failure: PairFailure| pairing_status(&failure);
        let mut last_unreachable = None;
        let mut selected = None;
        // A host with no pairing window open refuses the connection before
        // reading the secret; that refusal answers like any other secret
        // failure, whether it ends the dial or the call that follows it.
        let mut dials = PairingDials::start(&self.quic_endpoint, &plan);
        while let Some((addr, found, dial)) = dials.next().await {
            let dial = match dial {
                Ok(dial) => dial,
                Err(DialMiss::NoPairingWindow) => return Err(invalid_pin()),
                Err(DialMiss::Unreachable(error)) => {
                    last_unreachable = Some(error);
                    continue;
                }
            };
            let result = runtime
                .driver()
                .initiate(
                    &mut wire::pairing_service_client::PairingServiceClient::new(
                        dial.channel.clone(),
                    ),
                    initiate(found, Reachability::Direct { addrs: vec![addr] }),
                )
                .await
                .map_err(refused);
            match result {
                Err(_) if dial.refused() => return Err(invalid_pin()),
                Ok(pending) => {
                    selected = Some((pending, RouteKind::Direct));
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
                Err(error) => return Err(opaque(error)),
            }
        }
        drop(dials);
        // No address answered: every neighbor announcing the host is tried,
        // paired hosts first, since they forward on any plan and the cloud
        // relay only on a paid one.
        let mut unpaid = false;
        if selected.is_none()
            && let Some(host) = requested_host
        {
            for (forwarder, relay) in plan.forwarders.iter().copied() {
                let forwarder = crate::routing::host_from_core(forwarder);
                let channel = match self
                    .connections
                    .pairing_channel_through(
                        host,
                        forwarder,
                        Duration::from_millis(plan.deadline_ms),
                    )
                    .await
                {
                    Ok(channel) => channel,
                    Err(crate::link::ChannelError::Refused(
                        wire::ForwardRefusal::PaymentRequired,
                    )) => {
                        unpaid = true;
                        continue;
                    }
                    Err(error) => {
                        last_unreachable = Some(error.to_string());
                        continue;
                    }
                };
                let pending = runtime
                    .driver()
                    .initiate(
                        &mut wire::pairing_service_client::PairingServiceClient::new(channel),
                        initiate(
                            false,
                            if relay {
                                Reachability::Cloud
                            } else {
                                Reachability::Forwarded
                            },
                        ),
                    )
                    .await
                    .map_err(refused)
                    .map_err(opaque)?;
                selected = Some((pending, RouteKind::Relay));
                break;
            }
        }
        if selected.is_none() && unpaid {
            return Err(crate::grpc::status(wire::Error::because(
                wire::ErrorReason::PaymentRequired,
                "the other host can be reached only through the relay, and this account's free \
                 plan carries no connection between hosts",
            )));
        }
        // The other host not answering is this daemon's answer, not this
        // daemon going away: Unavailable would read as the latter.
        let (pending, via) = selected.ok_or_else(|| {
            crate::grpc::status(wire::Error::because(
                wire::ErrorReason::HostUnreachable,
                format!(
                    "the other host could not be reached: {}",
                    last_unreachable.unwrap_or_else(|| {
                        crate::link::ChannelError::NoPairingRoute.to_string()
                    })
                ),
            ))
        })?;
        Ok(wire::PendingPair {
            token: pending.token,
            peer: Some(pending.peer),
            via: via as i32,
        })
    }

    /// Finishes a pairing a person confirmed: both sides commit the other's
    /// key, and the core dials the peer at the route pairing found.
    pub async fn confirm_pair(&self, token: &[u8]) -> Result<wire::Peer, Status> {
        let answer = self
            .ask(ClientRequest::ConfirmPair {
                token: token.to_vec(),
            })
            .await?;
        let Some(Response::Paired { host }) = answer else {
            return Err(Status::internal(format!("the core confirmed {answer:?}")));
        };
        let host = Uuid::from_bytes(host.0);
        self.peer_entry(HostRef {
            of: Some(wire::host_ref::Of::HostId(host.as_bytes().to_vec())),
        })
    }

    pub async fn abandon_pair(&self, token: &[u8]) -> Result<(), Status> {
        self.ask(ClientRequest::AbandonPair {
            token: token.to_vec(),
        })
        .await
        .map(|_| ())
    }

    /// Trusts a peer whose identity an SSH exchange carried: SSH already
    /// authenticated it, so no secret is exchanged.
    pub async fn trust_ssh_peer(&self, request: TrustSshPeerRequest) -> Result<(), Status> {
        let identity = request
            .peer
            .ok_or_else(|| Status::invalid_argument("TrustSshPeerRequest.peer is required"))?;
        let reachability = request
            .ssh_target
            .map(|target| {
                if target.target.trim().is_empty() || target.target.starts_with('-') {
                    return Err(Status::invalid_argument(
                        "TrustSshPeerRequest.ssh_target is invalid",
                    ));
                }
                Ok(Reachability::Ssh(target.target))
            })
            .transpose()?;
        self.ask(ClientRequest::PairOverSsh {
            local: LocalPairingIdentity::from_device_identity(&self.identity)
                .named(&self.host_name),
            peer: identity,
            reachability,
        })
        .await?;
        Ok(())
    }

    /// Trusts another edge directly, as a finished pairing would, without
    /// the secret exchange: for harnesses that build topologies of hosts
    /// that are already paired.
    #[doc(hidden)]
    pub async fn trust(&self, peer: &Edge) -> Result<(), Status> {
        if peer.host_id() == self.host_id() || peer.public_key() == self.public_key() {
            return Err(Status::invalid_argument("SELF_PAIRING"));
        }
        self.ask(ClientRequest::ChangeTrust(TrustChange::Pair {
            host: peer.host_id(),
            pubkey: peer.public_key().to_vec(),
            name: peer.host_name().to_owned(),
            reachability: None,
        }))
        .await?;
        Ok(())
    }

    /// Paired hosts, by name.
    pub fn list_peers(&self) -> Result<Vec<wire::Peer>, Status> {
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

    pub fn peer_entry(&self, peer: HostRef) -> Result<wire::Peer, Status> {
        let store = self
            .trust
            .read()
            .map_err(|_| Status::internal("trust store lock is poisoned"))?;
        let (host, entry) = resolve_peer(&store, peer)?;
        Ok(peer_entry_to_wire(host, &entry))
    }

    /// Forgets a paired host: its key leaves the trust store, its links
    /// close with USER_REVOKED, and every connection it holds here ends.
    pub async fn unpair(&self, peer: HostRef, _reason: String) -> Result<wire::Peer, Status> {
        // The reason the client gave stays on the wire for a client that
        // may show it; nothing here records it.
        let host = {
            let store = self
                .trust
                .read()
                .map_err(|_| Status::internal("trust store lock is poisoned"))?;
            resolve_peer(&store, peer)?.0
        };
        let removed = match self
            .ask(ClientRequest::ChangeTrust(TrustChange::Remove { host }))
            .await?
        {
            Some(Response::Trust(TrustChanged::Removed(entry))) => entry,
            other => return Err(Status::internal(format!("the core unpaired {other:?}"))),
        };
        self.connections.close_host_access(host).await;
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

/// Why a pairing dial to one address came to nothing.
enum DialMiss {
    /// The host has no pairing window open.
    NoPairingWindow,
    Unreachable(String),
}

/// The pairing dials of one plan, all at once, handed out in the order the
/// plan prefers: an address that answered waits while any earlier one is
/// still within its short try.
struct PairingDials {
    dials: futures_util::stream::FuturesUnordered<
        futures_util::future::BoxFuture<
            'static,
            (usize, Result<crate::transport::PairingQuic, DialMiss>),
        >,
    >,
    addrs: Vec<(SocketAddr, bool)>,
    /// What each dial came to, until it is handed out: none while it runs.
    ended: Vec<Option<Result<crate::transport::PairingQuic, DialMiss>>>,
    handed: Vec<bool>,
    preferred_until: tokio::time::Instant,
}

impl PairingDials {
    fn start(endpoint: &crate::link::LinkEndpoint, plan: &daemon_core::reach::PairingDial) -> Self {
        let deadline = Duration::from_millis(plan.deadline_ms);
        let dials = plan
            .addrs
            .iter()
            .enumerate()
            .map(|(index, &(addr, _))| {
                let endpoint = endpoint.clone();
                Box::pin(async move {
                    let dial = match tokio::time::timeout(
                        deadline,
                        crate::transport::pairing_quic_channel(&endpoint, addr),
                    )
                    .await
                    {
                        Ok(Ok(dial)) => Ok(dial),
                        Ok(Err(crate::transport::TransportError::NoPairingWindow)) => {
                            Err(DialMiss::NoPairingWindow)
                        }
                        Ok(Err(error)) => Err(DialMiss::Unreachable(error.to_string())),
                        Err(_) => Err(DialMiss::Unreachable(format!("nothing answered at {addr}"))),
                    };
                    (index, dial)
                }) as futures_util::future::BoxFuture<'static, _>
            })
            .collect();
        let count = plan.addrs.len();
        Self {
            dials,
            addrs: plan.addrs.clone(),
            ended: (0..count).map(|_| None).collect(),
            handed: vec![false; count],
            preferred_until: tokio::time::Instant::now() + Duration::from_millis(plan.short_try_ms),
        }
    }

    /// The next dial to take, with its address and whether discovery found
    /// the host there; none once every one is handed out.
    async fn next(
        &mut self,
    ) -> Option<(
        SocketAddr,
        bool,
        Result<crate::transport::PairingQuic, DialMiss>,
    )> {
        use futures_util::StreamExt as _;
        loop {
            if let Some(index) = self.ready() {
                self.handed[index] = true;
                let (addr, found) = self.addrs[index];
                let dial = self.ended[index].take().expect("an ended dial");
                return Some((addr, found, dial));
            }
            if self.handed.iter().all(|handed| *handed) {
                return None;
            }
            let preferring = tokio::time::sleep_until(self.preferred_until);
            tokio::select! {
                ended = self.dials.next() => {
                    let (index, dial) = ended?;
                    self.ended[index] = Some(dial);
                }
                () = preferring, if tokio::time::Instant::now() < self.preferred_until => {}
            }
        }
    }

    /// The first dial to hand out: the earliest that ended, once every
    /// earlier one has ended, failed, or had its short try.
    fn ready(&self) -> Option<usize> {
        let preferring = tokio::time::Instant::now() < self.preferred_until;
        for index in 0..self.addrs.len() {
            if self.handed[index] {
                continue;
            }
            match &self.ended[index] {
                // A failure is handed out at once: it says only that this
                // address is no way to the host.
                Some(_) => return Some(index),
                // Still dialling: a later answer waits for it while it is
                // within its short try.
                None if preferring => return None,
                None => continue,
            }
        }
        None
    }
}
