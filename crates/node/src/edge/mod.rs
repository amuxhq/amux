//! A profile's network edge: its identity and trust, the links it holds to
//! paired hosts and to the relay, the LAN listener those hosts dial,
//! discovery, pairing, and the account binding the cloud link signs in with.
//!
//! Every link carries the same thing whichever carrier it rides: a control
//! stream (Hello, HelloAck, NeighborUp/Down, Reauth, LinkClose) and
//! application streams, each opened with a preface naming the host it is
//! for. Inside each stream the two hosts run a TLS handshake pinned to the
//! keys they exchanged when they paired; a trusted caller reaches the
//! profile's PeerService with its host id as the caller, and a caller that
//! presents no trusted key reaches only PairingService, and only while the
//! profile is in pairing mode.

pub(crate) mod account;
mod admin;
mod binding;
mod cloud;
mod peer;
mod relay_server;
mod status;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::task::{Context, Poll};
use std::time::Duration;

pub use account::ACCOUNT_FILE;
pub(crate) use account::{Account, AccountCredentials};
pub use cloud::{FREE_TIER_REFRESH_INTERVAL, UDP_BLOCKED_MEMORY};
use futures_util::stream;
pub use relay_server::{CloudLinkServer, JwtCloudLinkAuthenticator, RelayIdentity};
pub use status::{Observed, RelayCarrier};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use self::cloud::{CloudLink, CloudTransport, UdpBlockedMemory, establish_cloud_link};
use self::status::RuntimeStatus;
use crate::auth::CredentialProvider;
use crate::connection::ConnectionManager;
use crate::discovery::{Advertisement, Discovery, FoundHosts, local_pairing_addrs};
use crate::dispatcher::TunnelDispatcher;
use crate::driver::CoreLinks;
use crate::identity::{DeviceIdentity, IdentityError};
use crate::link::{CarrierKind, ChannelPool, MuxCarrier, MuxRole, run_link, serve_inbound_streams};
use crate::routing::{
    Capabilities, ConnectRole, LinkConnectorCtx, LinkCtx, LinkRegistry, LiveLocalHost, LiveRoute,
    RoutingCore, local_host,
};
use crate::runtime::ProfileRuntime;
use crate::services::{LocalPairingIdentity, PairingService, ReachabilityLinkConnector};
use crate::transport::{BoxedGrpcIo, tonic_server_builder};
use crate::trust::SharedTrustStore;
use crate::{Clock, HostId};

/// How long a peer has to finish its TLS handshake inside a stream.
const DEVICE_TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// The file a profile keeps its last LAN port in, so peers holding its
/// address find it again after a restart.
pub const LAN_PORT_FILE: &str = "lan_port";
/// The local socket an SSH relay hands a peer's link to.
pub const LINK_SOCKET: &str = "link.sock";
/// How long links get to carry their LinkClose before the edge stops.
const LINK_CLOSE_FLUSH: Duration = Duration::from_millis(200);

/// What an installation's edges share. The default serves nothing on the
/// network: identity, trust and pairing state exist, and a caller links
/// profiles in process.
#[derive(Clone)]
pub struct EdgeOptions {
    /// What this machine calls itself to its peers.
    pub host_name: String,
    /// The agent kinds this host runs, as its handshake advertises them.
    pub kinds: Vec<wire::Kind>,
    /// Listen for direct links on the LAN.
    pub lan: Option<LanOptions>,
    /// Advertise and browse the local network. Each profile gets its own
    /// advertisement from this factory.
    pub discovery: Option<DiscoveryFactory>,
    /// Discovery candidates are listed and dialled only when their scope
    /// equals this one.
    pub discovery_scope: String,
    /// Dial trusted peers at their known addresses and when discovery finds
    /// them.
    pub dial: bool,
    /// Serve the local socket SSH relays hand a peer's link to.
    pub link_socket: bool,
    /// Where the cloud link dials, when a test stands in for the cloud.
    pub cloud: CloudOptions,
}

pub type DiscoveryFactory = Arc<dyn Fn() -> Result<Arc<dyn Discovery>, String> + Send + Sync>;

/// The platform's local-network discovery: mDNS, except where only the
/// system may browse (the phone hands its results over instead). A debug
/// build can swap in a scripted stand-in with `AMUX_TEST_DISCOVERY_MODE`.
pub fn mdns_discovery() -> DiscoveryFactory {
    Arc::new(|| {
        #[cfg(all(debug_assertions, not(target_os = "ios")))]
        match std::env::var("AMUX_TEST_DISCOVERY_MODE").as_deref() {
            Ok("scripted" | "disabled") => {
                return Ok(
                    Arc::new(crate::discovery::ScriptedDiscovery::new()) as Arc<dyn Discovery>
                );
            }
            Ok("mdns") | Err(std::env::VarError::NotPresent) => {}
            Ok(mode) => return Err(format!("unknown test discovery mode {mode:?}")),
            Err(error) => return Err(format!("invalid test discovery mode: {error}")),
        }
        crate::discovery::MdnsDiscovery::new()
            .map(|discovery| Arc::new(discovery) as Arc<dyn Discovery>)
            .map_err(|error| error.to_string())
    })
}

impl Default for EdgeOptions {
    fn default() -> Self {
        Self {
            host_name: "amux".to_owned(),
            // What this platform hosts; a phone says it runs none.
            kinds: crate::routing::local_kinds(),
            lan: None,
            discovery: None,
            discovery_scope: String::new(),
            dial: false,
            link_socket: false,
            cloud: CloudOptions::default(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct LanOptions {
    /// Where the QUIC listener binds. Port zero reuses the profile's last
    /// port when it is free and asks for any port otherwise.
    pub bind: SocketAddr,
    /// Listen on this bound socket instead of binding one. The edge listens
    /// on a duplicate, so the port stays bound, answering nothing once the
    /// profile stops, for as long as the caller keeps its handle. Every
    /// profile's edge would share it: give it to one profile only.
    pub socket: Option<Arc<std::net::UdpSocket>>,
}

impl Default for LanOptions {
    fn default() -> Self {
        Self {
            bind: SocketAddr::from(([0, 0, 0, 0], 0)),
            socket: None,
        }
    }
}

/// Overrides for the cloud link, for tests that stand in for the cloud.
#[derive(Clone, Default)]
pub struct CloudOptions {
    /// Dial the relay's TCP carrier here, in plaintext, instead of the host
    /// and port the cloud names.
    pub relay_tcp: Option<SocketAddr>,
    /// Dial the relay's QUIC carrier here, trusting these roots, instead of
    /// the host and port the cloud names and the public roots.
    pub relay_quic: Option<RelayQuic>,
    /// How often a free account's link asks the cloud what the account
    /// buys; a purchase reaches the link within one interval.
    pub free_refresh_interval: Option<Duration>,
    /// A credential the embedder supplies instead of the profile's account
    /// file, for a host that owns its own sign-in.
    pub credentials: Option<Arc<dyn CredentialProvider>>,
}

/// Where a test's relay answers QUIC, and the client configuration that
/// trusts its certificate.
#[derive(Clone)]
pub struct RelayQuic {
    pub addr: SocketAddr,
    pub client: quinn::ClientConfig,
}

impl RelayQuic {
    /// A relay at `addr` whose certificate is `der`, and nothing else's.
    pub fn trusting(addr: SocketAddr, der: Vec<u8>) -> Result<Self, String> {
        let mut roots = rustls::RootCertStore::empty();
        roots
            .add(rustls::pki_types::CertificateDer::from(der))
            .map_err(|error| error.to_string())?;
        let client = crate::transport::relay_quic_client_config_with_roots(roots)
            .map_err(|error| error.to_string())?;
        Ok(Self { addr, client })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum EdgeError {
    #[error(transparent)]
    Identity(#[from] IdentityError),
    #[error("the account file: {0}")]
    Account(std::io::Error),
    #[error("the LAN listener: {0}")]
    Lan(std::io::Error),
    #[error("discovery: {0}")]
    Discovery(String),
    #[error("the link socket: {0}")]
    LinkSocket(std::io::Error),
}

/// One profile's network edge.
pub struct Edge {
    dir: PathBuf,
    host_name: String,
    identity: DeviceIdentity,
    trust: SharedTrustStore,
    runtime: Weak<ProfileRuntime>,
    local_host: LiveLocalHost,
    routing: Arc<RoutingCore>,
    channels: Arc<ChannelPool>,
    connections: Arc<ConnectionManager>,
    incoming_streams_tx: mpsc::Sender<crate::link::InboundStream>,
    pair_mode: Arc<crate::driver::pairing::PairWindow>,
    reachability: ReachabilityLinkConnector,
    discovery: Arc<dyn Discovery>,
    found: Arc<FoundHosts>,
    scope: String,
    lan_addr: Option<SocketAddr>,
    quic_endpoint: quinn::Endpoint,
    account: Arc<Account>,
    credentials_override: Option<Arc<dyn CredentialProvider>>,
    cloud_options: CloudOptions,
    cloud: tokio::sync::Mutex<Option<CloudLink>>,
    udp_blocked: Arc<UdpBlockedMemory>,
    status: RuntimeStatus,
    clock: Arc<dyn Clock>,
    shutdown: watch::Sender<bool>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
    analytics: analytics::Analytics,
}

impl Edge {
    /// Loads the profile's identity, trust and account, and serves what the
    /// options ask for. `runtime` is what trusted peers reach.
    pub(crate) async fn start(
        dir: &Path,
        options: &EdgeOptions,
        clock: Arc<dyn Clock>,
        runtime: Weak<ProfileRuntime>,
        analytics: analytics::Analytics,
    ) -> Result<Arc<Self>, EdgeError> {
        let identity = crate::identity::load_or_create_device_identity_in(dir)?;
        // The core holds the trust store; the edge reads it as the core
        // publishes it. A trust file that does not read stops the edge.
        let read = crate::trust::load_or_create_in(dir)?;
        let (trust, pair_mode): (SharedTrustStore, _) = match runtime.upgrade() {
            Some(runtime) => (runtime.driver().trust(), runtime.driver().pair_window()),
            None => (Arc::new(std::sync::RwLock::new(read)), Arc::default()),
        };
        let account = Account::open(dir).map_err(EdgeError::Account)?;
        let signed_in = options.cloud.credentials.is_some() || account.wants_cloud();
        let local_host = LiveLocalHost::new(local_host(
            identity.host_id,
            &options.host_name,
            Capabilities {
                cloud_relay: false,
                kinds: options.kinds.iter().map(|kind| *kind as i32).collect(),
            },
            signed_in,
        ));
        let routing = Arc::new(RoutingCore::with_trust_changes(
            trust.clone(),
            crate::routing::TrustChanges::to_core(runtime.clone()),
        ));
        let links = Arc::new(LinkRegistry::for_core(
            local_host.id(),
            CoreLinks::new(runtime.clone()),
        ));
        let channels = Arc::new(ChannelPool::with_device_tls(
            links,
            identity.clone(),
            trust.clone(),
            routing.revocations().clone(),
        ));
        let connections =
            ConnectionManager::new(routing.clone(), channels.clone(), analytics.clone());
        if let Some(runtime) = runtime.upgrade() {
            runtime
                .driver()
                .attach_connections(Arc::downgrade(&connections));
        }
        let (incoming_streams_tx, incoming_streams_rx) = mpsc::channel(64);
        let (trusted_tx, trusted_rx) = mpsc::channel(64);
        let (pairing_tx, pairing_rx) = mpsc::channel(64);
        let (shutdown, _) = watch::channel(false);

        let link_ctx = LinkCtx::new_live(
            local_host.clone(),
            routing.clone(),
            channels.link_registry(),
        )
        .with_incoming_streams(incoming_streams_tx.clone())
        .with_clock(clock.clone());
        let dispatcher = TunnelDispatcher::new(
            &identity,
            trust.clone(),
            pair_mode.clone(),
            connections.trusted_connections(),
            trusted_tx,
            pairing_tx,
            DEVICE_TLS_HANDSHAKE_TIMEOUT,
        )?
        .with_link_ctx(link_ctx.clone());

        let mut tasks = vec![
            serve_inbound_streams(Arc::new(dispatcher.clone()), incoming_streams_rx),
            peer::serve(runtime.clone(), trusted_rx, shutdown.subscribe()),
            serve_pairing(
                PairingService::new(
                    runtime.clone(),
                    LocalPairingIdentity::from_device_identity(&identity),
                    options.host_name.clone(),
                ),
                pairing_rx,
                shutdown.subscribe(),
            ),
        ];

        let quic_server = identity.quic_server_config(trust.clone())?;
        let (quic_endpoint, lan_addr) = match &options.lan {
            Some(lan) => {
                let endpoint = bind_lan(dir, lan, quic_server).map_err(EdgeError::Lan)?;
                let addr = endpoint.local_addr().map_err(EdgeError::Lan)?;
                let _ = crate::install::write_durably(
                    &dir.join(LAN_PORT_FILE),
                    addr.port().to_string().as_bytes(),
                );
                tasks.push(dispatcher.serve_quic_endpoint(endpoint.clone(), shutdown.subscribe()));
                tracing::info!(%addr, host = %identity.host_id, "listening for direct links");
                (endpoint, Some(addr))
            }
            None => (
                quinn::Endpoint::client(SocketAddr::from(([0, 0, 0, 0], 0)))
                    .map_err(EdgeError::Lan)?,
                None,
            ),
        };

        let platform = options
            .discovery
            .as_ref()
            .map(|factory| factory())
            .transpose()
            .map_err(EdgeError::Discovery)?;
        // Without a browser of its own the edge is handed what one outside
        // it found (the phone's system browses for the app). The handed set
        // plays as discovery events, so a trusted host that comes back is
        // dialled as one this edge found itself would be.
        let handed = platform.is_none();
        let discovery: Arc<dyn Discovery> =
            platform.unwrap_or_else(|| Arc::new(crate::discovery::ScriptedDiscovery::new()));
        let found = Arc::new(FoundHosts::default());
        let reachability = ReachabilityLinkConnector::new(
            identity.clone(),
            trust.clone(),
            local_host.clone(),
            routing.clone(),
            channels.clone(),
            connections.clone(),
            incoming_streams_tx.clone(),
        );
        reachability.configure(
            discovery.clone(),
            found.clone(),
            quic_endpoint.clone(),
            options.discovery_scope.clone(),
        );
        // A handed set is advertised nowhere: this edge has no advertiser.
        if let Some(addr) = lan_addr.filter(|_| !handed) {
            let addrs = if addr.ip().is_unspecified() {
                local_pairing_addrs(addr.port())
            } else {
                vec![addr]
            };
            discovery
                .advertise(Advertisement {
                    host_id: identity.host_id,
                    name: options.host_name.clone(),
                    version: crate::PROTOCOL_VERSION,
                    addrs,
                    scope: options.discovery_scope.clone(),
                })
                .map_err(|error| EdgeError::Discovery(error.to_string()))?;
        }
        tasks.push(reachability.spawn_dial_on_found(discovery.browse(), options.dial));
        discovery.requery();
        if options.dial {
            tasks.extend(reachability.spawn_startup_links());
        }
        if options.link_socket {
            tasks.push(serve_link_socket(dir, link_ctx, shutdown.subscribe())?);
        }

        let edge = Arc::new(Self {
            dir: dir.to_owned(),
            host_name: options.host_name.clone(),
            identity,
            trust,
            runtime: runtime.clone(),
            local_host,
            routing,
            channels,
            connections,
            incoming_streams_tx,
            pair_mode,
            reachability,
            discovery,
            found,
            scope: options.discovery_scope.clone(),
            lan_addr,
            quic_endpoint,
            account,
            credentials_override: options.cloud.credentials.clone(),
            cloud_options: options.cloud.clone(),
            cloud: tokio::sync::Mutex::new(None),
            udp_blocked: Arc::new(UdpBlockedMemory::new(UDP_BLOCKED_MEMORY, clock.clone())),
            status: RuntimeStatus::default(),
            clock,
            shutdown,
            tasks: Mutex::new(tasks),
            analytics,
        });
        edge.tasks
            .lock()
            .unwrap()
            .push(follow_plan(&edge.status, &edge.connections));
        if signed_in {
            edge.start_cloud().await;
        }
        Ok(edge)
    }

    pub fn host_id(&self) -> HostId {
        self.identity.host_id
    }

    pub fn public_key(&self) -> &[u8] {
        self.identity.public_key()
    }

    pub fn host_name(&self) -> &str {
        &self.host_name
    }

    /// What this host's handshake says it can do.
    pub fn capabilities(&self) -> wire::Capabilities {
        self.local_host.snapshot().capabilities
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Where the LAN listener answers, when there is one.
    pub fn lan_addr(&self) -> Option<SocketAddr> {
        self.lan_addr
    }

    /// The addresses a pairing invitation names: the LAN listener on every
    /// interface it is reachable on.
    pub fn pairing_addrs(&self) -> Vec<SocketAddr> {
        match self.lan_addr {
            Some(addr) if addr.ip().is_unspecified() => local_pairing_addrs(addr.port()),
            Some(addr) => vec![addr],
            None => Vec::new(),
        }
    }

    pub fn discovery_scope(&self) -> &str {
        &self.scope
    }

    /// Machines discovery has found in this profile's scope, newest first.
    pub fn candidates(&self) -> Vec<Advertisement> {
        self.found
            .candidates()
            .into_iter()
            .filter(|advert| advert.host_id != self.identity.host_id)
            .collect()
    }

    /// Hands this profile what an outside browser resolved: the phone,
    /// where only the system may browse the local network.
    pub fn hand_over_discovered(&self, found: Vec<Advertisement>) {
        let found = found
            .into_iter()
            .filter(|advert| advert.scope == self.scope)
            .collect::<Vec<_>>();
        self.reachability.hand_over_found(found.clone());
        self.discovery.hand_over(found);
    }

    /// The hosts this profile trusts: host id, name and key.
    pub fn trusted(&self) -> Vec<(HostId, String, Vec<u8>)> {
        self.trust
            .read()
            .map(|store| {
                store
                    .entries()
                    .map(|(host, entry)| (host, entry.name.clone(), entry.pubkey.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn is_trusted(&self, host: HostId) -> bool {
        self.trust
            .read()
            .is_ok_and(|store| store.entry(host).is_some())
    }

    /// How new calls to a host go now.
    pub async fn via(&self, host: HostId) -> LiveRoute {
        self.connections.via_for(host).await
    }

    /// Whether `host` was signed in to an account when it last said, kept
    /// with its trust entry so a host that is away still reads as it was.
    pub fn signed_in(&self, host: HostId) -> Option<bool> {
        self.routing.signed_in_for(host)
    }

    /// What kind of machine `host` last announced itself as.
    pub fn platform(&self, host: HostId) -> Option<String> {
        self.routing.platform_for(host)
    }

    /// `host`'s version and capabilities as its handshake gave them, while
    /// a link or a relay's claim makes it present.
    pub async fn described(&self, host: HostId) -> Option<(String, wire::Capabilities)> {
        self.routing
            .host_entry(host)
            .await
            .map(|host| (host.version, host.capabilities))
    }

    /// Whether `host` last said it no longer trusts this host, by closing
    /// a link or refusing a stream, and has not taken it back since.
    pub fn revoked(&self, host: HostId) -> bool {
        self.routing.revocations().contains(host)
    }

    /// Waits until calls to `host` have a route, or the timeout passes.
    pub async fn wait_for_route(&self, host: HostId, timeout: Duration) -> bool {
        tokio::time::timeout(timeout, async {
            loop {
                if self.via(host).await != LiveRoute::Offline {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .is_ok()
    }

    /// A PeerService client for a trusted host, over whichever route
    /// reaches it: a direct link, or the relay.
    pub async fn peer(&self, host: HostId) -> Result<crate::PeerClient, crate::link::ChannelError> {
        Ok(crate::link::peer_client(self.channel(host).await?))
    }

    /// A call to `host` could not open a channel on the route that lists
    /// it: the host reads offline until a channel to it opens again.
    pub(crate) async fn calls_failed(&self, host: HostId) {
        self.connections.calls_failed(host).await;
    }

    /// How `host` reads now, and the route its calls take while it is not
    /// offline.
    pub async fn presence(&self, host: HostId) -> (wire::Presence, LiveRoute) {
        self.connections.presence_for(host).await
    }

    /// From now on no channel to `host` opens, or opens again, while the
    /// link and the streams it already carries stay up: a call to it fails
    /// before it goes out. For a test network.
    #[doc(hidden)]
    pub async fn refuse_calls(&self, host: HostId, refuse: bool) {
        self.connections.refuse_calls(host, refuse);
    }

    /// The channel calls to a trusted host ride, for a caller that speaks
    /// some other service on it.
    pub async fn channel(
        &self,
        host: HostId,
    ) -> Result<tonic::transport::Channel, crate::link::ChannelError> {
        self.connections.channel_to(host).await
    }

    /// A PeerService client for a host's sessions, on a channel of their
    /// own so that their subscriptions neither wait behind nor hold up the
    /// host's other calls.
    pub async fn session_peer(
        &self,
        host: HostId,
    ) -> Result<crate::PeerClient, crate::link::ChannelError> {
        let channel = self.connections.session_channel_to(host).await?;
        Ok(crate::link::peer_client(channel))
    }

    /// A PeerService client for bulk transfers, blobs, apart from calls.
    pub async fn bulk_peer(
        &self,
        host: HostId,
    ) -> Result<crate::PeerClient, crate::link::ChannelError> {
        let channel = self.connections.bulk_channel_to(host).await?;
        Ok(crate::link::peer_client(channel))
    }

    /// Holds the next bulk transfer from `host` once its first data has
    /// arrived, until the returned hold is released or dropped.
    #[doc(hidden)]
    pub fn hold_next_bulk_response(&self, host: HostId) -> crate::link::BulkResponseHold {
        self.channels.hold_next_bulk_response(host)
    }

    /// Why the last dial to a host failed, until a route to it comes up.
    pub async fn last_dial_error(&self, host: HostId) -> Option<String> {
        self.connections.stored_reachability_error(host).await
    }

    /// Moves the LAN listener onto a new socket, for a device whose network
    /// changed under it: links migrate with the QUIC connection.
    pub fn rebind_lan(&self, socket: std::net::UdpSocket) -> std::io::Result<()> {
        socket.set_nonblocking(true)?;
        self.reachability.rebind_quic(socket)
    }

    /// Whether this profile remembers that UDP to `relay` was eaten, and so
    /// dials that relay over TCP only until the memory expires.
    pub fn remembers_udp_blocked(&self, relay: &str) -> bool {
        self.udp_blocked.holds(relay)
    }

    /// What the cloud link is doing.
    pub fn observed(&self) -> Observed {
        self.status.current()
    }

    pub fn subscribe_observed(&self) -> watch::Receiver<Observed> {
        self.status.subscribe()
    }

    pub(crate) fn account(&self) -> &Arc<Account> {
        &self.account
    }

    /// Whether this profile is signed in to the account it is bound to:
    /// None while it was never bound, so a profile that only ever worked
    /// on its own network never reads as signed out. A paused connection
    /// is still signed in; the person chose to stay off the relay.
    pub fn account_signed_in(&self) -> Option<bool> {
        match self.account.intent() {
            wire::Intent::Bound | wire::Intent::Paused => Some(true),
            wire::Intent::LoggedOut => Some(false),
            wire::Intent::Unbound | wire::Intent::Unspecified => None,
        }
    }

    /// The account service the profile is bound to, signed in or not.
    pub(crate) fn account_service(&self) -> Option<String> {
        self.account
            .service()
            .map(|service| service.as_str().to_owned())
    }

    /// Paired hosts by the route that reaches each now; offline ones are
    /// left out.
    pub(crate) async fn routes(&self) -> analytics::Counts<analytics::Route> {
        let hosts: Vec<HostId> = match self.trust.read() {
            Ok(store) => store.entries().map(|(host, _)| host).collect(),
            Err(_) => return analytics::Counts::default(),
        };
        let mut routes = analytics::Counts::default();
        for host in hosts {
            if let Some(route) = crate::telemetry::route(self.connections.via_for(host).await) {
                routes.add(route, 1);
            }
        }
        routes
    }

    pub(crate) fn analytics(&self) -> &analytics::Analytics {
        &self.analytics
    }

    /// A bearer for the account service, for a client that calls it on the
    /// account's behalf. The refresh token stays here: it may be single use,
    /// so only this process ever spends it.
    pub async fn access_token(&self) -> Result<crate::auth::AccessToken, crate::auth::AuthError> {
        match self.credentials() {
            Some(credentials) => credentials.access_token().await,
            None => Err(crate::auth::AuthError::Unauthenticated),
        }
    }

    fn credentials(&self) -> Option<Arc<dyn CredentialProvider>> {
        if let Some(credentials) = &self.credentials_override {
            return Some(credentials.clone());
        }
        self.account.wants_cloud().then(|| {
            Arc::new(AccountCredentials(self.account.clone())) as Arc<dyn CredentialProvider>
        })
    }

    /// Starts the cloud link if the profile is signed in and none runs.
    pub(crate) async fn start_cloud(&self) {
        let Some(credentials) = self.credentials() else {
            return;
        };
        let cloud_url = match self.account.service() {
            Some(service) => service.as_str().to_owned(),
            None if self.credentials_override.is_some() => String::new(),
            None => return,
        };
        let mut cloud = self.cloud.lock().await;
        if cloud.as_ref().is_some_and(|link| !link.is_finished()) {
            return;
        }
        if let Some(finished) = cloud.take() {
            finished.stop().await;
        }
        self.local_host.set_signed_in(true);
        self.status.report(Observed::Connecting);
        let connector: LinkConnectorCtx = LinkConnectorCtx::new_live(
            self.local_host.clone(),
            self.routing.clone(),
            self.channels.link_registry(),
        )
        .with_incoming_streams(self.incoming_streams_tx.clone())
        .with_clock(self.clock.clone());
        *cloud = Some(establish_cloud_link(
            cloud_url,
            credentials,
            connector,
            self.status.clone(),
            CloudTransport::new(
                self.quic_endpoint.clone(),
                self.udp_blocked.clone(),
                self.cloud_options.relay_tcp,
                self.cloud_options.relay_quic.clone(),
                self.cloud_options.free_refresh_interval,
            ),
        ));
    }

    /// Stops the cloud link; the profile works locally.
    pub(crate) async fn stop_cloud(&self) {
        if let Some(link) = self.cloud.lock().await.take() {
            link.stop().await;
        }
        self.local_host
            .set_signed_in(self.credentials_override.is_some() || self.account.wants_cloud());
        self.status.report(Observed::Local);
    }

    /// Asks the cloud what the account buys now, over the live cloud link.
    pub async fn refresh_entitlement(&self) -> Result<crate::Tier, String> {
        // The link's lock is held only to take the refresher: pausing,
        // signing out and shutting down take it too, and must not wait
        // behind the relay's answer.
        let refresher = self
            .cloud
            .lock()
            .await
            .as_ref()
            .and_then(|link| link.refresher());
        match refresher {
            Some(refresher) => refresher.refresh().await.map_err(|error| error.to_string()),
            None => Err("the profile has no cloud link".to_owned()),
        }
    }

    /// Links this edge to `other` in process, as a direct link over a
    /// loopback carrier. Both must already trust each other: the acceptor
    /// admits the connector as the host its trust store names, the way a
    /// pinned handshake would. The link lives until the returned handle is
    /// severed or dropped.
    pub fn link_in_process(self: &Arc<Self>, other: &Arc<Edge>) -> Result<LoopbackLink, String> {
        if !self.is_trusted(other.host_id()) || !other.is_trusted(self.host_id()) {
            return Err("an in-process link joins two hosts that trust each other".to_owned());
        }
        let (near, far) = tokio::io::duplex(1 << 20);
        let carried = Arc::new(AtomicU64::new(0));
        let near = Counted {
            inner: near,
            carried: carried.clone(),
        };
        let connector = Arc::new(MuxCarrier::new(near, MuxRole::Connector, CarrierKind::Quic));
        let acceptor = Arc::new(MuxCarrier::new(far, MuxRole::Acceptor, CarrierKind::Quic));
        let accept_ctx = other.link_ctx().with_authenticated_peer(self.host_id());
        let connect_ctx = self.link_ctx().with_expected_peer(other.host_id());
        tokio::spawn({
            let acceptor = acceptor.clone();
            async move {
                let _ = run_link(accept_ctx, acceptor, ConnectRole::Acceptor).await;
            }
        });
        let _ =
            crate::link::run::spawn_connector_with_establishment(connect_ctx, connector.clone());
        Ok(LoopbackLink {
            carriers: [connector, acceptor],
            carried,
        })
    }

    /// Opens a channel to a LAN listener without presenting a trusted key:
    /// what a stranger, or a device about to pair, reaches.
    pub async fn unpinned_channel(
        &self,
        addr: SocketAddr,
    ) -> Result<tonic::transport::Channel, String> {
        crate::transport::pairing_quic_channel(&self.quic_endpoint, addr)
            .await
            .map(|dial| dial.channel)
            .map_err(|error| error.to_string())
    }

    fn link_ctx(&self) -> LinkCtx {
        LinkCtx::new_live(
            self.local_host.clone(),
            self.routing.clone(),
            self.channels.link_registry(),
        )
        .with_incoming_streams(self.incoming_streams_tx.clone())
        .with_shutdown(self.shutdown.subscribe())
        .with_clock(self.clock.clone())
    }

    /// Dials a trusted peer's LAN listener directly.
    pub fn dial(&self, host: HostId, addr: SocketAddr) {
        self.reachability.spawn_pair_time_link(
            host,
            crate::trust::Reachability::Direct { addrs: vec![addr] },
        );
    }

    /// Closes every link, withdraws the advertisement and stops serving.
    /// Stops serving at once, without closing links politely: what a
    /// crash leaves behind.
    pub(crate) fn abort(&self) {
        self.shutdown.send_replace(true);
        self.quic_endpoint
            .close(quinn::VarInt::from_u32(0), b"profile stopping");
        if let Ok(mut cloud) = self.cloud.try_lock()
            && let Some(link) = cloud.take()
        {
            link.abort();
        }
        for task in std::mem::take(&mut *self.tasks.lock().unwrap()) {
            task.abort();
        }
    }

    pub(crate) async fn stop(&self, reason: wire::LinkCloseReason) {
        self.discovery.withdraw();
        self.channels
            .link_registry()
            .send_link_close_to_all(reason)
            .await;
        tokio::time::sleep(LINK_CLOSE_FLUSH).await;
        self.shutdown.send_replace(true);
        self.reachability.close_direct_links().await;
        if let Some(link) = self.cloud.lock().await.take() {
            link.stop().await;
        }
        self.quic_endpoint
            .close(quinn::VarInt::from_u32(0), b"profile stopping");
        let tasks = std::mem::take(&mut *self.tasks.lock().unwrap());
        for task in &tasks {
            task.abort();
        }
        for task in tasks {
            let _ = task.await;
        }
    }
}

impl Drop for Edge {
    fn drop(&mut self) {
        self.discovery.withdraw();
        self.shutdown.send_replace(true);
        for task in self.tasks.lock().unwrap().drain(..) {
            task.abort();
        }
    }
}

/// An in-process link between two edges.
pub struct LoopbackLink {
    carriers: [Arc<MuxCarrier>; 2],
    carried: Arc<AtomicU64>,
}

impl LoopbackLink {
    /// The bytes the link has carried so far, both ways, as a network
    /// between the hosts would see them: what a test reads to say what
    /// crossing from one host to the other costs.
    pub fn carried(&self) -> u64 {
        self.carried.load(Ordering::Relaxed)
    }

    /// Cuts the link the way a dead connection would: no LinkClose, the
    /// carrier just stops, and each side's link runtime cleans up as it
    /// does for any connection that went away.
    pub fn sever(&self) {
        for carrier in &self.carriers {
            crate::link::LinkCarrier::close(carrier.as_ref(), wire::LinkCloseReason::Unspecified);
        }
    }
}

/// One end of a loopback link's pipe, counting what passes it either way.
struct Counted<T> {
    inner: T,
    carried: Arc<AtomicU64>,
}

impl<T: AsyncRead + Unpin> AsyncRead for Counted<T> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let before = buf.filled().len();
        let result = Pin::new(&mut self.inner).poll_read(cx, buf);
        let read = buf.filled().len() - before;
        self.carried.fetch_add(read as u64, Ordering::Relaxed);
        result
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for Counted<T> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let result = Pin::new(&mut self.inner).poll_write(cx, buf);
        if let Poll::Ready(Ok(written)) = result {
            self.carried.fetch_add(written as u64, Ordering::Relaxed);
        }
        result
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

impl Drop for LoopbackLink {
    fn drop(&mut self) {
        self.sever();
    }
}

/// Tells the connections whenever the account's plan is seen to change,
/// from one connected tier to another: what the relay carries changes with
/// it, and hosts it refused or carried need not wait for a retry's backoff
/// or a call to read as they now are.
fn follow_plan(status: &RuntimeStatus, connections: &Arc<ConnectionManager>) -> JoinHandle<()> {
    let mut observed = status.subscribe();
    let connections = Arc::downgrade(connections);
    tokio::spawn(async move {
        let mut plan = None;
        while observed.changed().await.is_ok() {
            let Observed::Connected { tier, .. } = *observed.borrow_and_update() else {
                continue;
            };
            if plan.replace(tier).is_some_and(|before| before != tier) {
                let Some(connections) = connections.upgrade() else {
                    return;
                };
                connections.plan_changed();
            }
        }
    })
}

/// Binds the LAN listener, preferring the profile's last port.
fn bind_lan(
    dir: &Path,
    lan: &LanOptions,
    server: quinn::ServerConfig,
) -> std::io::Result<quinn::Endpoint> {
    if let Some(socket) = &lan.socket {
        let socket = socket.try_clone()?;
        socket.set_nonblocking(true)?;
        let runtime = quinn::default_runtime()
            .ok_or_else(|| std::io::Error::other("no async runtime for the LAN listener"))?;
        return quinn::Endpoint::new(
            quinn::EndpointConfig::default(),
            Some(server),
            socket,
            runtime,
        );
    }
    let bind = lan.bind;
    if bind.port() == 0
        && let Some(port) = std::fs::read_to_string(dir.join(LAN_PORT_FILE))
            .ok()
            .and_then(|text| text.trim().parse::<u16>().ok())
        && let Ok(endpoint) =
            quinn::Endpoint::server(server.clone(), SocketAddr::new(bind.ip(), port))
    {
        return Ok(endpoint);
    }
    quinn::Endpoint::server(server, bind)
}

/// Serves PairingService to callers that presented no trusted key.
fn serve_pairing(
    service: PairingService,
    incoming: mpsc::Receiver<BoxedGrpcIo>,
    shutdown: watch::Receiver<bool>,
) -> JoinHandle<()> {
    let incoming = stream::unfold(incoming, |mut incoming| async move {
        incoming
            .recv()
            .await
            .map(|io| (Ok::<_, std::io::Error>(io), incoming))
    });
    tokio::spawn(async move {
        let served = tonic_server_builder()
            .add_service(wire::pairing_service_server::PairingServiceServer::new(
                service,
            ))
            .serve_with_incoming_shutdown(incoming, wait_for(shutdown))
            .await;
        if let Err(error) = served {
            tracing::warn!(%error, "the pairing service stopped");
        }
    })
}

pub(crate) async fn wait_for(mut shutdown: watch::Receiver<bool>) {
    let _ = shutdown.wait_for(|stop| *stop).await;
}

/// Accepts links SSH relays hand over on the profile's link socket.
fn serve_link_socket(
    dir: &Path,
    ctx: LinkCtx,
    shutdown: watch::Receiver<bool>,
) -> Result<JoinHandle<()>, EdgeError> {
    let path = dir.join(LINK_SOCKET);
    let mut listener =
        agent_dir::local_socket::LocalListener::bind(&path).map_err(EdgeError::LinkSocket)?;
    let ctx = ctx
        .with_carrier(crate::routing::LinkCarrier::Ssh)
        .with_shutdown(shutdown);
    Ok(tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok(stream) => {
                    let carrier =
                        Arc::new(MuxCarrier::new(stream, MuxRole::Acceptor, CarrierKind::Ssh));
                    let ctx = ctx.clone();
                    tokio::spawn(async move {
                        if let Err(error) = run_link(ctx, carrier, ConnectRole::Acceptor).await {
                            tracing::warn!(%error, "an SSH link ended with an error");
                        }
                    });
                }
                Err(error) => {
                    tracing::warn!(%error, "the link socket stopped accepting");
                    break;
                }
            }
        }
    }))
}
