//! The daemon, hosted in the phone's own process.
//!
//! The phone runs no agents, but it is a host like any other to the
//! machines it pairs with. It is one installation with one profile per
//! account, exactly as a desktop is: each profile has its own identity and
//! trust, its own store of replica rows, direct links on the local network
//! and a relay link once an account is bound. The daemon's profile registry
//! creates, binds, signs out and deletes them; every call here names
//! the profile it is for. Chats read a profile's store through the same
//! client service the terminal dials over a socket, called in process here;
//! no client opens the store.
//!
//! Which replica agents keep a source open is switchable per profile: every
//! listed agent for the profile in front of somebody, and only the ones a
//! chat asks for otherwise.

mod log;
mod phone;

use std::collections::VecDeque;
use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use app_runtime::values::{
    AccountBinding, AccountView, Bearer, BillingInterval, Found, Identity, PairedPeer, PaywallFrom,
    PendingPair, RelayLink, Roster,
};
pub use app_runtime::values::{PairRequest, ProfileView, StartConfig, UsageEvent};
use client::{Client, Clock, InProcess};
use node::{
    ClientApi, Cloud, CloudOptions, Daemon, DiscoveryFactory, EdgeOptions, LanOptions,
    ProfileRefusal, ProfileRuntime, ProfileState, ProfileSubscription, Registry, Relay, SignIn,
    SourcePolicy, StartError, StartOptions, Tier,
};
pub use node::{Cores, ProfileId};
pub use phone::Phone;
use sha2::{Digest, Sha256};
use wire::{
    BeginPairRequest, Empty, HostRef, StartPairingRequest, begin_pair_request, host_ref,
    pairing_window, start_pairing_request,
};

/// What a test or a driving build changes about the network edge.
#[derive(Clone)]
pub struct EdgeOverrides {
    /// Where the relay link dials instead of where the cloud names.
    pub cloud: CloudOptions,
    /// Local-network discovery; the phone's system browses and hands its
    /// results over, so there is none by default.
    pub discovery: Option<DiscoveryFactory>,
    /// Where direct links listen: every interface of both families by
    /// default.
    pub lan_bind: SocketAddr,
    /// Whether this is the app as published, which sends product
    /// analytics; tests and driving builds are not, and send none unless
    /// `AMUX_ANALYTICS_URL` names a server.
    pub published: bool,
    /// Where profiles record analytics instead of sending it: for tests.
    pub recording: Option<Arc<dyn analytics::Sink>>,
}

impl Default for EdgeOverrides {
    fn default() -> Self {
        EdgeOverrides {
            cloud: CloudOptions::default(),
            discovery: None,
            lan_bind: node::DUAL_STACK_ANY,
            published: false,
            recording: None,
        }
    }
}

/// A client opening again within this long is the same visit.
const CLIENT_OPENED_EVERY: Duration = Duration::from_secs(60 * 60);
/// How long a flush as the app leaves the screen may take.
const BACKGROUND_FLUSH: Duration = Duration::from_secs(3);

/// What only the app sees, as the event it is sent as.
fn usage_event(event: UsageEvent) -> analytics::Event {
    match event {
        UsageEvent::PaywallViewed { from } => analytics::Event::PaywallViewed {
            from: match from {
                PaywallFrom::Agents => analytics::PaywallFrom::Agents,
                PaywallFrom::Hosts => analytics::PaywallFrom::Hosts,
                PaywallFrom::You => analytics::PaywallFrom::You,
            },
        },
        UsageEvent::PurchaseStarted { interval } => analytics::Event::PurchaseStarted {
            interval: match interval {
                BillingInterval::Monthly => analytics::Interval::Monthly,
                BillingInterval::Yearly => analytics::Interval::Yearly,
            },
        },
    }
}

#[derive(Debug, thiserror::Error)]
pub enum EmbeddedError {
    #[error(transparent)]
    Start(#[from] StartError),
    #[error("the installation hosts no profile {0}")]
    NoProfile(ProfileId),
    #[error("{}", .0.message())]
    Refused(#[from] tonic::Status),
    #[error("{0}")]
    Profile(#[from] ProfileRefusal),
    #[error(transparent)]
    Link(#[from] node::QrPairingError),
    #[error("{0}")]
    Account(#[from] node::AuthError),
    #[error("{0}")]
    Relay(String),
    #[error("checkpointing profile {0}: {1}")]
    Checkpoint(ProfileId, String),
    #[error("binding profile {0}'s link socket afresh: {1}")]
    Rebind(ProfileId, io::Error),
}

/// What a network change did across every profile.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NetworkChangeReport {
    /// The profiles whose link socket was bound afresh.
    pub profiles_rebound: usize,
    /// The path machines told, one per peer of every profile.
    pub machines_told: usize,
    /// The device's interface addresses now.
    pub addrs: Vec<std::net::IpAddr>,
}

/// One installation with every profile it hosts, in process.
pub struct EmbeddedRuntime {
    // Held for its shutdown and to find a profile's runtime.
    daemon: Mutex<Option<Daemon>>,
    registry: Registry,
    data_dir: PathBuf,
    clock: Arc<dyn Clock>,
    /// The app's telemetry setting, read before every upload.
    telemetry: Arc<AtomicBool>,
    /// The app's automatic-reports setting, read before every dump is sent.
    auto_reports: Arc<AtomicBool>,
    analytics: Option<analytics::Flusher>,
}

/// A change to the profile list, as the registry announces it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProfileEvent {
    Upserted(ProfileView),
    Removed(String),
    /// Everything that was already there has been announced.
    CaughtUp,
}

/// The registry's changes, from a snapshot of every profile on.
pub struct ProfileEvents {
    opening: VecDeque<ProfileEvent>,
    watch: ProfileSubscription,
}

impl ProfileEvents {
    /// The next change, or none once the daemon stops or the watch falls
    /// too far behind, when the list has to be read again.
    pub async fn next(&mut self) -> Option<ProfileEvent> {
        if let Some(event) = self.opening.pop_front() {
            return Some(event);
        }
        Some(match self.watch.next().await?.ok()? {
            node::ProfileEvent::Upserted(state) => ProfileEvent::Upserted(profile_view(&state)),
            node::ProfileEvent::Removed(id) => ProfileEvent::Removed(id.to_string()),
        })
    }
}

impl EmbeddedRuntime {
    /// Starts the installation under `config.data_dir`, creating its first
    /// profile on first start. Its profiles' cores have no threads: whoever
    /// rings for `cores` steps them.
    ///
    /// A relaunch starts each core from its store as every start does,
    /// never from the checkpoint a suspend left: the rest of a checkpoint is
    /// links, channels, calls, jobs, timers and pairings bound to the
    /// process that ended, and what outlives it is read back from the
    /// store, the drafts file, the trust store and the settings.
    pub async fn start_with(
        config: &StartConfig,
        overrides: EdgeOverrides,
        clock: Arc<dyn Clock>,
        cores: Cores,
    ) -> Result<EmbeddedRuntime, EmbeddedError> {
        if let Some(path) = &config.log_path {
            // A log that cannot be opened costs the dump its log, never the
            // phone its runtime.
            let _ = log::write_to(path);
        }
        let edge = EdgeOptions {
            host_name: config.device_name.clone(),
            // The phone runs no agents.
            kinds: Vec::new(),
            lan: config.lan.then(|| LanOptions {
                bind: overrides.lan_bind,
                socket: None,
            }),
            discovery: overrides.discovery,
            discovery_scope: config.discovery_scope.clone(),
            dial: config.lan,
            link_socket: false,
            cloud: overrides.cloud,
            interfaces: None,
            liveness: Default::default(),
        };
        let telemetry = Arc::new(AtomicBool::new(config.telemetry));
        let auto_reports = Arc::new(AtomicBool::new(config.auto_reports));
        let analytics = match overrides.recording {
            Some(sink) => node::Telemetry::Record(sink),
            None => match analytics::Endpoint::resolve(overrides.published) {
                Some(endpoint) => node::Telemetry::Upload {
                    endpoint,
                    channel: analytics::Channel::Stable,
                    gate: {
                        let telemetry = telemetry.clone();
                        Arc::new(move || telemetry.load(Ordering::SeqCst))
                    },
                },
                None => node::Telemetry::Off,
            },
        };
        let options = StartOptions {
            data_dir: config.data_dir.clone(),
            // The phone has no boot id to read; each start is its own boot.
            boot_id: Some(uuid::Uuid::new_v4().to_string()),
            launch: node::Launch {
                cores: Some(cores),
                ..node::Launch::default()
            },
            clock: clock.clone(),
            push: Arc::new(node::NoopSender),
            daemon_log: config.log_path.clone(),
            supervisor_log: None,
            front_door: None,
            edge,
            analytics,
            auto_reports: node::AutoReports::new({
                let auto_reports = auto_reports.clone();
                Arc::new(move || auto_reports.load(Ordering::SeqCst))
            }),
        };
        let daemon = node::start(options, None).await?;
        Ok(EmbeddedRuntime {
            registry: daemon.registry(),
            data_dir: daemon.data_dir().to_owned(),
            analytics: daemon.analytics_flusher(),
            daemon: Mutex::new(Some(daemon)),
            clock,
            telemetry,
            auto_reports,
        })
    }

    fn hosted(&self) -> Vec<Arc<ProfileRuntime>> {
        self.daemon
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .as_ref()
            .map(Daemon::profiles)
            .unwrap_or_default()
    }

    // --- analytics -----------------------------------------------------------

    /// The app's telemetry setting changed: off stops what is waiting from
    /// going, on lets it go again.
    pub fn set_telemetry(&self, on: bool) {
        self.telemetry.store(on, Ordering::SeqCst);
    }

    /// The app's automatic-reports setting changed; the next dump a check
    /// asks for reads it.
    pub fn set_auto_reports(&self, on: bool) {
        self.auto_reports.store(on, Ordering::SeqCst);
    }

    /// The app came to the front: counted for every profile, at most once
    /// an hour. Revalidate existing paths and recover a lost relay at once.
    pub fn client_opened(&self) {
        for runtime in self.hosted() {
            if let Some(edge) = runtime.edge() {
                edge.foreground();
            }
            runtime.analytics().record_at_most_every(
                CLIENT_OPENED_EVERY,
                analytics::Event::ClientOpened {
                    client: analytics::Client::Phone,
                },
            );
        }
    }

    /// Records what only the app sees, on the profile it concerns, or on
    /// every profile when it names none it hosts.
    pub fn record(&self, profile: Option<ProfileId>, event: UsageEvent) {
        let event = usage_event(event);
        let hosted = self.hosted();
        let named: Vec<_> = hosted
            .iter()
            .filter(|runtime| Some(runtime.profile()) == profile)
            .collect();
        let targets = if named.is_empty() {
            hosted.iter().collect()
        } else {
            named
        };
        for runtime in targets {
            runtime.analytics().record(event.clone());
        }
    }

    /// Sends what is waiting, as the app leaves the screen and may be
    /// suspended.
    pub async fn flush_analytics(&self) {
        if let Some(flusher) = &self.analytics {
            flusher.flush(BACKGROUND_FLUSH).await;
        }
    }

    /// Checkpoints every profile's core, each at a passive WAL checkpoint
    /// of its store, and waits until every one is on the drive: as the app
    /// is about to be suspended.
    pub async fn checkpoint(&self) -> Result<(), EmbeddedError> {
        for runtime in self.hosted() {
            runtime
                .checkpoint()
                .await
                .map_err(|error| EmbeddedError::Checkpoint(runtime.profile(), error))?;
        }
        Ok(())
    }

    fn runtime(&self, profile: ProfileId) -> Result<Arc<ProfileRuntime>, EmbeddedError> {
        self.daemon
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .as_ref()
            .and_then(|daemon| daemon.profile(profile))
            .ok_or(EmbeddedError::NoProfile(profile))
    }

    fn edge(&self, profile: ProfileId) -> Result<Arc<node::Edge>, EmbeddedError> {
        self.runtime(profile)?
            .edge()
            .ok_or(EmbeddedError::NoProfile(profile))
    }

    // --- the registry ------------------------------------------------------

    /// Every profile, oldest first.
    pub fn profiles(&self) -> Result<Vec<ProfileView>, EmbeddedError> {
        Ok(self.registry.list()?.iter().map(profile_view).collect())
    }

    /// One profile as the registry lists it.
    pub fn profile(&self, profile: ProfileId) -> Result<ProfileView, EmbeddedError> {
        Ok(profile_view(&self.registry.profile(profile)?))
    }

    /// Every profile as it is now, then each change as it happens.
    pub fn subscribe_profiles(&self) -> Result<ProfileEvents, EmbeddedError> {
        let mut watch = self.registry.subscribe()?;
        let opening = std::mem::take(&mut watch.listed)
            .iter()
            .map(|state| ProfileEvent::Upserted(profile_view(state)))
            .chain([ProfileEvent::CaughtUp])
            .collect();
        Ok(ProfileEvents { opening, watch })
    }

    /// A new profile nobody has signed in on.
    pub async fn create_profile(&self, label: Option<&str>) -> Result<ProfileView, EmbeddedError> {
        Ok(profile_view(&self.registry.create(label).await?))
    }

    /// Deletes a profile: its key, the machines it trusts and its store.
    /// The installation keeps at least one.
    pub async fn delete_profile(&self, profile: ProfileId) -> Result<(), EmbeddedError> {
        Ok(self.registry.delete(profile).await?)
    }

    /// Binds a profile to an account with the refresh token the app's
    /// sign-in obtained; the relay link comes up from there. Named, the
    /// profile is adopted even when it paired machines before; unnamed,
    /// the registry picks the profile already bound to that account, else
    /// one that holds nothing, else a new one. `client_id` is the OAuth
    /// client the token was issued to, which is the only one it refreshes
    /// under.
    pub async fn bind(
        &self,
        profile: Option<ProfileId>,
        cloud_url: &str,
        client_id: &str,
        refresh_token: &str,
    ) -> Result<ProfileView, EmbeddedError> {
        let bound = self
            .registry
            .bind(SignIn {
                profile,
                cloud_url: cloud_url.to_owned(),
                refresh_token: refresh_token.to_owned(),
                client_id: client_id.to_owned(),
                adopt: profile.is_some(),
            })
            .await?;
        Ok(profile_view(&bound))
    }

    /// Signs a profile out: its relay link goes down and its refresh token
    /// is forgotten, but it stays tied to its account.
    pub async fn sign_out(&self, profile: ProfileId) -> Result<ProfileView, EmbeddedError> {
        Ok(profile_view(&self.registry.sign_out(profile).await?))
    }

    /// Every profile whose trust store holds `host_id`, oldest first.
    pub fn trusting(&self, host_id: &[u8]) -> Result<Vec<ProfileId>, EmbeddedError> {
        let Ok(host) = node::HostId::from_slice(host_id) else {
            return Ok(Vec::new());
        };
        Ok(self
            .registry
            .list()?
            .into_iter()
            .map(|state| state.id)
            .filter(|profile| self.edge(*profile).is_ok_and(|edge| edge.is_trusted(host)))
            .collect())
    }

    // --- one profile ---------------------------------------------------------

    /// A profile's client services in process, as its chats and fleet call
    /// them, with its administration for the dump.
    pub fn client(&self, profile: ProfileId) -> Result<Arc<dyn Client>, EmbeddedError> {
        let runtime = self.runtime(profile)?;
        let admin = self
            .daemon
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .as_ref()
            .map(|daemon| daemon.profile_admin(profile))
            .ok_or(EmbeddedError::NoProfile(profile))?;
        Ok(Arc::new(
            InProcess::new(ClientApi::new(&runtime, None)).with_admin(Arc::new(admin)),
        ))
    }

    /// Runs `work` where the profile's core steps, between two of its
    /// steps: how a check finds out which queue that is.
    pub async fn between_steps<T: Send + 'static>(
        &self,
        profile: ProfileId,
        work: impl FnOnce() -> T + Send + 'static,
    ) -> Result<T, EmbeddedError> {
        self.runtime(profile)?
            .read_store(move |_| work())
            .await
            .map_err(|_| EmbeddedError::NoProfile(profile))
    }

    pub fn clock(&self) -> Arc<dyn Clock> {
        self.clock.clone()
    }

    /// A profile's host id, as its peers pin it.
    pub fn host_id(&self, profile: ProfileId) -> Result<Vec<u8>, EmbeddedError> {
        Ok(self.runtime(profile)?.host().as_bytes().to_vec())
    }

    /// `Listed`: every listed agent keeps a source, for the profile in
    /// front of somebody. `OnDemand`: only the agents a chat asks for.
    /// Every listed agent of the profile on screen keeps a source open;
    /// every other profile's, and every profile's when none is shown,
    /// opens sources only for the chats that open.
    pub fn list_sources(&self, shown: Option<ProfileId>) {
        for runtime in self.hosted() {
            runtime.set_source_policy(if Some(runtime.profile()) == shown {
                SourcePolicy::Listed
            } else {
                SourcePolicy::OnDemand
            });
        }
    }

    pub fn source_policy(&self, profile: ProfileId) -> Result<SourcePolicy, EmbeddedError> {
        Ok(self.runtime(profile)?.source_policy())
    }

    /// Reaches a machine by the PIN its person reads out or the link its QR
    /// code carries, and authenticates it; nothing is trusted until the
    /// attempt is confirmed.
    pub async fn begin_pair(
        &self,
        profile: ProfileId,
        request: &PairRequest,
    ) -> Result<PendingPair, EmbeddedError> {
        let pairing = match request {
            PairRequest::Pin {
                host_id,
                pin,
                addrs,
            } => BeginPairRequest {
                host_id: host_id.clone(),
                secret: Some(begin_pair_request::Secret::Pin(
                    pin.chars().filter(char::is_ascii_digit).collect(),
                )),
                addrs: addrs.clone(),
            },
            PairRequest::Link(link) => {
                let invitation = node::parse_pair_link(link)?;
                BeginPairRequest {
                    host_id: invitation.host_id.as_bytes().to_vec(),
                    secret: Some(begin_pair_request::Secret::QrSecret(invitation.secret)),
                    addrs: invitation.addrs.iter().map(ToString::to_string).collect(),
                }
            }
        };
        let pending = self.edge(profile)?.begin_pair(pairing).await?;
        let via = pending.via();
        let peer = pending.peer.unwrap_or_default();
        Ok(PendingPair {
            token: pending.token,
            host_id: peer.host_id,
            name: peer.name,
            fingerprint: fingerprint(&peer.pubkey),
            expires_at_ms: peer.expires_at_ms,
            via,
        })
    }

    /// Opens pairing mode on a profile and answers the link its QR code
    /// would carry: whoever pairs with it by that link is trusted. The phone
    /// shows no pairing code of its own; a driving build uses this to pair
    /// two of its profiles.
    pub async fn offer_pairing(&self, profile: ProfileId) -> Result<String, EmbeddedError> {
        let started = self
            .edge(profile)?
            .start_pairing(StartPairingRequest {
                method: Some(start_pairing_request::Method::Qr(Empty {})),
                ttl_seconds: None,
            })
            .await?;
        let Some(pairing_window::Secret::QrSecret(secret)) = &started.secret else {
            return Err(EmbeddedError::Relay(
                "pairing mode opened without a link secret".into(),
            ));
        };
        let host = node::HostId::from_slice(
            &started
                .identity
                .as_ref()
                .map(|identity| identity.host_id.clone())
                .unwrap_or_default(),
        )
        .map_err(|_| EmbeddedError::NoProfile(profile))?;
        let addrs: Vec<SocketAddr> = started
            .addrs
            .iter()
            .filter_map(|addr| addr.parse().ok())
            .collect();
        let invitation =
            node::encode_qr_pairing_invitation(host, &addrs, started.cloud_url.as_deref(), secret)?;
        Ok(node::pair_link(&invitation))
    }

    /// Trusts the machine an attempt reached.
    pub async fn confirm_pair(
        &self,
        profile: ProfileId,
        token: &[u8],
    ) -> Result<wire::Peer, EmbeddedError> {
        Ok(self.edge(profile)?.confirm_pair(token).await?)
    }

    /// Turns away the machine an attempt reached, telling it so.
    pub async fn abandon_pair(
        &self,
        profile: ProfileId,
        token: &[u8],
    ) -> Result<(), EmbeddedError> {
        Ok(self.edge(profile)?.abandon_pair(token).await?)
    }

    /// Pairs in one step, trusting whoever the secret reaches.
    pub async fn pair(
        &self,
        profile: ProfileId,
        request: &PairRequest,
    ) -> Result<wire::Peer, EmbeddedError> {
        let pending = self.begin_pair(profile, request).await?;
        self.confirm_pair(profile, &pending.token).await
    }

    /// A profile's identity and the machines it trusts, sorted by name.
    pub fn roster(&self, profile: ProfileId) -> Result<Roster, EmbeddedError> {
        let edge = self.edge(profile)?;
        let identity = edge.device_identity();
        let mut peers: Vec<PairedPeer> = edge
            .list_peers()?
            .into_iter()
            .map(|peer| PairedPeer {
                fingerprint: fingerprint(&peer.pubkey),
                host_id: peer.host_id,
                name: peer.name,
            })
            .collect();
        peers.sort_by_key(|peer| peer.name.to_lowercase());
        Ok(Roster {
            identity: Identity {
                fingerprint: fingerprint(&identity.pubkey),
                host_id: identity.host_id,
                name: identity.name,
            },
            peers,
        })
    }

    /// Hands over the whole set the phone's own browser found on the local
    /// network to every profile; only the system may browse there.
    pub fn discovered(&self, found: Vec<Found>) {
        let found: Vec<_> = found
            .into_iter()
            .filter_map(|found| {
                Some(node::harness::Advertisement {
                    host_id: node::HostId::from_slice(&found.host_id).ok()?,
                    name: found.name,
                    version: found.version,
                    addrs: found
                        .addrs
                        .iter()
                        .filter_map(|addr| addr.parse().ok())
                        .collect(),
                    scope: found.scope,
                })
            })
            .collect();
        for runtime in self.hosted() {
            if let Some(edge) = runtime.edge() {
                edge.hand_over_discovered(found.clone());
            }
        }
    }

    /// The phone's network changed under it: every profile's link socket
    /// is bound afresh, so its links move to the new network with their
    /// QUIC connections, and every path machine probes its paths again.
    /// Every profile is told even when one fails to rebind; the first
    /// failure is the answer.
    pub async fn network_changed(&self) -> Result<NetworkChangeReport, EmbeddedError> {
        let mut report = NetworkChangeReport::default();
        let mut failed = None;
        for runtime in self.hosted() {
            let Some(edge) = runtime.edge() else {
                continue;
            };
            match edge.network_changed() {
                Ok(change) => {
                    report.profiles_rebound += usize::from(change.rebound);
                    report.machines_told += change.machines_told;
                    report.addrs = change.addrs;
                }
                Err(error) => {
                    failed.get_or_insert(EmbeddedError::Rebind(runtime.profile(), error));
                }
            }
        }
        failed.map_or(Ok(report), Err)
    }

    /// The account a profile is bound to, and its relay link.
    pub fn account(&self, profile: ProfileId) -> Result<AccountView, EmbeddedError> {
        Ok(self.profile(profile)?.account)
    }

    /// A bearer for the account service, refreshed by the profile when it
    /// is about to expire.
    pub async fn access_token(&self, profile: ProfileId) -> Result<Bearer, EmbeddedError> {
        let token = self.edge(profile)?.access_token().await?;
        Ok(Bearer {
            bearer: token.bearer,
            expires_at_ms: token.expires_at.and_then(|at| {
                at.duration_since(std::time::UNIX_EPOCH)
                    .ok()
                    .map(|since| since.as_millis() as i64)
            }),
        })
    }

    /// Asks the account service again what the account buys, over the
    /// relay link, so a purchase lifts the relay's tier now rather than at
    /// the link's next scheduled refresh.
    pub async fn refresh_entitlement(&self, profile: ProfileId) -> Result<(), EmbeddedError> {
        self.edge(profile)?
            .refresh_entitlement()
            .await
            .map(|_| ())
            .map_err(EmbeddedError::Relay)
    }

    /// Stops trusting a paired machine, telling it so where it can be
    /// reached.
    pub async fn unpair(&self, profile: ProfileId, host_id: &[u8]) -> Result<(), EmbeddedError> {
        self.edge(profile)?
            .unpair(HostRef {
                of: Some(host_ref::Of::HostId(host_id.to_vec())),
            })
            .await?;
        Ok(())
    }

    /// Where reports are written.
    pub fn reports(&self) -> PathBuf {
        self.data_dir.join(node::REPORTS)
    }

    /// Stops serving and flushes the store; the installation is marked
    /// clean.
    pub async fn shutdown(self) -> io::Result<()> {
        let daemon = self
            .daemon
            .into_inner()
            .unwrap_or_else(|poison| poison.into_inner());
        match daemon {
            Some(daemon) => daemon.shutdown().await,
            None => Ok(()),
        }
    }
}

/// A key as a person compares it: hex of its SHA-256.
fn fingerprint(pubkey: &[u8]) -> String {
    Sha256::digest(pubkey)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn profile_view(state: &ProfileState) -> ProfileView {
    let account = state.account.clone().unwrap_or_default();
    ProfileView {
        id: state.id.to_string(),
        label: state.label.clone(),
        subject: account.subject,
        account: AccountView {
            binding: match state.cloud {
                Cloud::Unbound => AccountBinding::Unbound,
                Cloud::SignedIn(_) => AccountBinding::SignedIn,
                Cloud::SignedOut => AccountBinding::SignedOut,
            },
            email: account.email.unwrap_or_default(),
            name: account.name.unwrap_or_default(),
            picture: account.picture.unwrap_or_default(),
            pro: state.cloud.connected().map(|(tier, _)| tier == Tier::Pro),
            relay: match &state.cloud {
                Cloud::Unbound | Cloud::SignedOut => RelayLink::Off,
                Cloud::SignedIn(Relay::Connecting) => RelayLink::Connecting,
                Cloud::SignedIn(Relay::Connected { .. }) => RelayLink::Connected,
                Cloud::SignedIn(Relay::Retrying) => RelayLink::Retrying,
                Cloud::SignedIn(Relay::SignInAgain) => RelayLink::SignInAgain,
                Cloud::SignedIn(Relay::VersionMismatch) => RelayLink::VersionMismatch,
            },
        },
    }
}
