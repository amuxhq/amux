//! A running topology: one production daemon per host, in this process,
//! each on its own temporary installation, with real agent processes on
//! the fake providers.
//!
//! Verbs change the world and acknowledge that the change is installed;
//! observations wait for consequences and return what they saw. The net
//! distinguishes the three ways a daemon comes back: [`Net::stop_daemon`]
//! then [`Net::restart_daemon`] is a clean restart; [`Net::kill_daemon`]
//! then [`Net::restart_daemon`] is a crash with the agents still running;
//! [`Net::rewind_host`] is power loss, the store and journals back to what
//! reached the drive and the machine up under a new boot id.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};
use std::time::Duration;

use agent_dir::Clock;
use node::harness::{LiveRoute, ScriptedDiscovery, scripted_discovery};
use node::{
    ClientApi, Daemon, Edge, EdgeOptions, LanOptions, Launch, LoopbackLink, ProfileId,
    ProfileRuntime,
};
use provider_fakes::script::{INPUT_LOG_ENV, SCRIPT_ENV, Script, Step};
use serde::{Deserialize, Serialize};
use store::AgentKey;
use tokio::sync::mpsc;
use uuid::Uuid;
use wire::client_service_server::ClientService as _;
use wire::profile_admin_service_client::ProfileAdminServiceClient;
use wire::profile_service_client::ProfileServiceClient;
use wire::{
    AgentRef, ClaudeCreateConfig, ClaudePtyInput, ClaudeSdkInput, CodexCreateConfig, CodexInput,
    CreateAgentRequest, DeleteAgentRequest, Input, PromptInput, WithdrawQueued, claude_pty_input,
    claude_sdk_input, codex_input, create_agent_request,
};

use crate::binaries::Binaries;
use crate::clock::DrivenClock;
use crate::feed::Feed;
use crate::gate::{Faults, UdpGate};
use crate::invariant::{self, BlockViolation};
use crate::lan::{FrontAdvert, Gated, Lan};
use crate::observe::{self, InventoryObserver, Observer, ObserverOf, PATIENCE, Stuck};
use crate::probes;
use crate::process::ProcessHost;
use crate::providers::{self, Commands, RealTools, SMALL_CLAUDE, SMALL_CODEX};
use crate::relay::Relay;
use crate::router::{Router, RouterKind};
use crate::topology::{
    AccountDecl, AgentDecl, FakeKind, HostDecl, LinkDecl, Provider, RelayDecl, TierDecl, Topology,
    TopologyError, link_key,
};

/// The extension of the file beside an agent's script that its provider
/// logs every line it reads to.
const INPUT_LOG: &str = "input";
/// The relay's certificate in the net's root, for process hosts to trust.
const RELAY_CA: &str = "relay-ca.pem";
/// How long an imitated router keeps a mapping nothing uses: longer than a
/// link's keep-alive, so a quiet link keeps its port, as a home router's
/// half-minute or more does.
const ROUTER_IDLE: Duration = Duration::from_secs(30);

/// The version a supervised host starts on: older than anything a test
/// publishes.
pub const SUPERVISED_VERSION: &str = "0.0.1";

/// Lets a caller adjust a host's edge beyond what the topology declares,
/// such as pointing its cloud link at a stand-in relay.
pub type EdgeHook = Arc<dyn Fn(&str, &mut EdgeOptions) + Send + Sync>;

/// Lets a caller change a host's parameters, such as the tail size K or a
/// retention budget, on everything the host starts with.
pub type LaunchHook = Arc<dyn Fn(&str, &mut Launch) + Send + Sync>;

/// Which clock the daemons' policy timers run on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ClockMode {
    /// A [`DrivenClock`] the specification advances.
    #[default]
    Driven,
    /// Wall time, for a net served to a person or a journey.
    Wall,
}

#[derive(Clone, Default)]
pub struct NetOptions {
    pub clock: ClockMode,
    /// The driven clock to use, when something outside the net, such as a
    /// stand-in relay, must share it; a fresh one otherwise.
    pub driven: Option<DrivenClock>,
    pub edge: Option<EdgeHook>,
    pub launch: Option<LaunchHook>,
    /// Put the net's root at `<dir>/net` rather than at a fresh random
    /// temporary directory, so a client that draws the net's paths draws the
    /// same ones on every run. The directory must not already hold a `net`.
    pub root_in: Option<PathBuf>,
    /// What every host does with product analytics: nothing, unless a
    /// test hands a sink to record into.
    pub analytics: Option<node::Telemetry>,
    /// The size of each agent's facts ring, where it differs from the
    /// product's: a ring large enough not to rotate holds an agent's whole
    /// life for a dump.
    pub facts_ring_mib: Option<u64>,
    /// Where real providers' traffic is recorded, a folder per host and
    /// agent; under the net's root when none is given, which goes with it.
    pub recordings: Option<PathBuf>,
    /// Route the LAN between hosts, and from a client outside the net,
    /// through gates the net holds, so any pair of hosts can be cut and any
    /// host's network degraded however the hosts came to trust each other.
    /// Hosts then find each other at gate addresses rather than at their
    /// listeners'.
    pub gated_lan: bool,
    /// Where what a client outside this Mac dials listens: the gates in
    /// front of the hosts' LAN listeners and the relay, and the account
    /// service. A phone over its USB link names the Mac's address on that
    /// link; loopback when none is named.
    pub client_ip: Option<std::net::IpAddr>,
}

/// How long a wait on a real provider's agent may take: a real turn on a
/// small model is seconds, a slow one minutes.
pub const REAL_PATIENCE: Duration = Duration::from_secs(240);

/// How long a restart waits for its host's LAN port: past the two minutes
/// a client's idle connection to the dead listener may last.
pub(crate) const LAN_PORT_PATIENCE: Duration = Duration::from_secs(150);

/// How long a net's agents outlive their daemon: long enough to ride out a
/// daemon restart, short enough that an agent a failed run leaves behind
/// goes away on its own. A host down for longer ends every agent on it,
/// where a real installation waits five minutes.
pub(crate) const AGENT_GRACE_SECS: u64 = 20;

/// A verb's acknowledgement: what was installed, at what policy time.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ack {
    pub installed: String,
    pub at_ms: i64,
}

/// Where a host's journal is cut when it loses power.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JournalCut {
    pub agent: String,
    /// The journal's global byte offset the drive kept up to.
    pub byte: u64,
}

/// An agent the net knows by name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetAgent {
    pub name: String,
    pub host: String,
    pub host_id: Uuid,
    pub id: Uuid,
    pub kind: FakeKind,
    #[serde(default)]
    pub provider: Provider,
}

impl NetAgent {
    pub fn key(&self) -> AgentKey {
        AgentKey::new(
            self.host_id.as_bytes().to_vec(),
            self.id.as_bytes().to_vec(),
        )
    }

    pub fn parent(&self) -> AgentRef {
        AgentRef {
            host_id: self.host_id.as_bytes().to_vec(),
            agent_id: self.id.as_bytes().to_vec(),
        }
    }
}

/// The one repository root of a host that declares repositories, in its
/// directory.
const REPOSITORIES: &str = "repositories";

/// Everything a host is on disk and in process.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HostInfo {
    pub name: String,
    pub host_id: Uuid,
    pub profile: ProfileId,
    pub data_dir: PathBuf,
    /// The installation front door a client dials.
    pub front_door: PathBuf,
    /// An installation config naming the front door, for `amux --config`.
    pub config: PathBuf,
    /// Where this host's agents work.
    pub work: PathBuf,
    /// The host's own directory: its installation, its provider commands,
    /// and for a process host its logs.
    pub dir: PathBuf,
    /// Whether the host is a real `amux` process rather than a runtime in
    /// the net's.
    #[serde(default)]
    pub process: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum NetError {
    #[error(transparent)]
    Topology(#[from] TopologyError),
    #[error("no host {0:?}")]
    NoHost(String),
    #[error("no agent {0:?}")]
    NoAgent(String),
    #[error("agent {0:?} exists")]
    AgentExists(String),
    #[error("hosts {0:?} and {1:?} are not linked in this topology")]
    NoLink(String, String),
    #[error("host {0:?} is down")]
    Down(String),
    #[error("host {0:?} is running")]
    Running(String),
    #[error("host {0:?} has no checkpoint to lose power back to; take one first")]
    NoCheckpoint(String),
    #[error("the net runs on wall time; only a driven net advances its clock")]
    WallClock,
    #[error("host {host:?} failed to start: {error}")]
    Start { host: String, error: String },
    #[error("host {host:?}: {error}")]
    Host { host: String, error: String },
    #[error(transparent)]
    Registry(#[from] node::RegistryError),
    #[error(transparent)]
    Serve(#[from] node::ServeError),
    #[error("refused: {}", .0.message())]
    Refused(Box<tonic::Status>),
    #[error(transparent)]
    Stuck(#[from] Stuck),
    #[error(transparent)]
    Block(#[from] BlockViolation),
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0} is not supported on this platform")]
    Unsupported(&'static str),
    #[error("the topology declares no relay")]
    NoRelay,
    #[error("host {host:?} runs as a process: {why}")]
    Process { host: String, why: String },
    /// The simulated net broke a rule: a core asked for a dump or an
    /// invariant failed after a store write. The seed reproduces it.
    #[error("the simulated net failed under seed {seed}: {failure}")]
    Simulated { seed: u64, failure: String },
}

struct Host {
    decl: HostDecl,
    info: HostInfo,
    dir: PathBuf,
    /// Behind a lock only so the net can be shared across tasks: a daemon
    /// holding its supervisor pipe is not Sync.
    daemon: Option<std::sync::Mutex<Daemon>>,
    /// The running daemon's profile runtime, held weakly so a killed
    /// daemon's run can be seen to end.
    runtime: Weak<ProfileRuntime>,
    boot: u64,
    /// The store file as of the last checkpoint: what reached the drive.
    checkpoint: Option<Vec<u8>>,
    /// The running daemon listens on a LAN socket a test handed it, which
    /// stays bound for as long as the test holds it.
    lan_handed: bool,
    /// How a process host runs; none for one in the net's process.
    process: Option<ProcessHost>,
    /// A process host's daemon is up as far as the net's verbs know.
    running: bool,
    /// The host's `claude` and `codex` commands.
    commands: Commands,
}

struct Link {
    /// Cut by a fault verb, as opposed to down because an end is.
    severed: bool,
    /// Behind a lock only so the net can be shared across tasks: a
    /// loopback carrier is not Sync.
    live: Option<std::sync::Mutex<LoopbackLink>>,
}

/// One link a flap cuts and restores on its own, holding no lock of the
/// net's.
pub(crate) enum LinkFlip {
    // Weak, so a host killed or removed mid-flap still drops its edge,
    // which withdraws it from discovery.
    InProcess {
        a: Weak<Edge>,
        b: Weak<Edge>,
        id_a: Uuid,
        id_b: Uuid,
        live: Option<LoopbackLink>,
    },
    Lan {
        lan: Arc<Lan>,
        a: String,
        b: String,
    },
}

impl LinkFlip {
    pub(crate) fn cut(&mut self) {
        match self {
            // Dropping the link severs it.
            Self::InProcess { live, .. } => *live = None,
            Self::Lan { lan, a, b } => {
                lan.cut(a, b, true);
            }
        }
    }

    /// Lets traffic through again. An in-process link is dialled anew
    /// once both ends have seen the old one go, as a restore does; if they
    /// have not within a few seconds, it stays cut until the next period.
    pub(crate) async fn restore(&mut self) {
        match self {
            Self::InProcess {
                a,
                b,
                id_a,
                id_b,
                live,
            } => {
                let (Some(a), Some(b)) = (a.upgrade(), b.upgrade()) else {
                    return;
                };
                let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
                while a.via(*id_b).await == LiveRoute::Direct
                    || b.via(*id_a).await == LiveRoute::Direct
                {
                    if tokio::time::Instant::now() > deadline {
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                *live = a.link_in_process(&b).ok();
            }
            Self::Lan { lan, a, b } => {
                lan.cut(a, b, false);
            }
        }
    }
}

pub struct Net {
    root: tempfile::TempDir,
    topology: Topology,
    clock: Arc<dyn Clock>,
    driven: Option<DrivenClock>,
    binaries: Binaries,
    bus: ScriptedDiscovery,
    edge_hook: Option<EdgeHook>,
    launch_hook: Option<LaunchHook>,
    analytics: Option<node::Telemetry>,
    hosts: BTreeMap<String, Host>,
    links: BTreeMap<(String, String), Link>,
    agents: BTreeMap<String, NetAgent>,
    relay: Option<Relay>,
    /// Each host's way to the relay's QUIC carrier, kept across restarts.
    udp: BTreeMap<String, UdpGate>,
    /// A gate in front of the relay's QUIC carrier, for a client outside
    /// the net, once one has asked for it.
    relay_gate: Option<UdpGate>,
    /// The gates on the LAN between hosts, and in front of each.
    lan: Arc<Lan>,
    facts_ring_mib: Option<u64>,
    recordings: PathBuf,
    /// The real providers, found the first time a host or agent needs one.
    real: Option<RealTools>,
    /// The release feed supervised hosts take updates from, started with
    /// the first of them.
    feed: Option<Feed>,
    /// Faults set on the relay path as a whole, beside each host's own.
    relay_faults: Faults,
    gated_lan: bool,
    client_ip: Option<std::net::IpAddr>,
}

impl Net {
    /// Starts `topology` on a driven clock.
    pub async fn start(topology: Topology) -> Result<Self, NetError> {
        Self::start_with(topology, NetOptions::default()).await
    }

    /// Starts every host, trusts and links the declared pairs, waits for
    /// each link to carry traffic both ways, and spawns the declared agents
    /// in order, each returning once its process has said hello. When this
    /// returns the net is ready.
    pub async fn start_with(topology: Topology, options: NetOptions) -> Result<Self, NetError> {
        topology.check()?;
        crate::link_log::install();
        crate::ssh::install();
        let driven = match options.clock {
            ClockMode::Driven => Some(options.driven.unwrap_or_default()),
            ClockMode::Wall => None,
        };
        let clock: Arc<dyn Clock> = match &driven {
            Some(driven) => Arc::new(driven.clone()),
            None => Arc::new(agent_dir::SystemClock),
        };
        let root = match &options.root_in {
            Some(dir) => tempfile::Builder::new()
                .prefix("net")
                .rand_bytes(0)
                .tempdir_in(dir)?,
            None => tempfile::Builder::new().prefix("testnet").tempdir()?,
        };
        std::fs::create_dir_all(root.path().join("gates"))?;
        std::fs::create_dir_all(root.path().join("scripts"))?;
        let recordings = options
            .recordings
            .clone()
            .unwrap_or_else(|| root.path().join("providers"));
        let mut net = Self {
            root,
            topology: Topology {
                hosts: Vec::new(),
                links: Vec::new(),
                agents: Vec::new(),
                ..topology.clone()
            },
            clock,
            driven,
            binaries: Binaries::built().clone(),
            bus: ScriptedDiscovery::new(),
            edge_hook: options.edge,
            launch_hook: options.launch,
            analytics: options.analytics,
            hosts: BTreeMap::new(),
            links: BTreeMap::new(),
            agents: BTreeMap::new(),
            relay: None,
            udp: BTreeMap::new(),
            relay_gate: None,
            lan: Arc::new(Lan::default()),
            facts_ring_mib: options.facts_ring_mib,
            recordings,
            real: None,
            feed: None,
            relay_faults: Faults::default(),
            gated_lan: options.gated_lan,
            client_ip: options.client_ip,
        };
        if let Some(relay) = &topology.relay {
            net.relay = Some(Relay::start(relay, net.clock.clone(), net.client_ip).await?);
            net.write_relay_ca()?;
        }
        for decl in &topology.hosts {
            net.create_host(decl)?;
            net.start_host(&decl.name).await?;
        }
        for decl in &topology.hosts {
            if let Some(account) = &decl.account {
                net.sign_in(&decl.name, account).await?;
            }
        }
        for link in &topology.links {
            net.link_hosts(&link.a, &link.b).await?;
        }
        for agent in &topology.agents {
            net.spawn(agent.clone()).await?;
            if topology.settle {
                net.settle(&agent.name).await?;
            }
        }
        Ok(net)
    }

    // --- growing the net ---------------------------------------------------

    /// Adds a host to the running net, as a declared one starts: its
    /// installation made, its daemon started, signed in to its account.
    pub async fn add_host(&mut self, decl: HostDecl) -> Result<Ack, NetError> {
        let mut grown = self.topology.clone();
        grown.hosts.push(decl.clone());
        grown.check()?;
        if decl.account.is_some() && self.relay.is_none() {
            return Err(TopologyError::NoRelay {
                host: decl.name.clone(),
                account: decl.account.clone().unwrap_or_default(),
            }
            .into());
        }
        self.create_host(&decl)?;
        if let Err(error) = self.start_host(&decl.name).await {
            self.forget_host(&decl.name);
            return Err(error);
        }
        if let Some(account) = &decl.account {
            self.sign_in(&decl.name, account).await?;
        }
        let kind = if decl.supervised {
            "supervised process"
        } else if decl.process {
            "process"
        } else {
            "in-process"
        };
        Ok(self.ack(format!(
            "host {} ({kind}, {} providers) started",
            decl.name,
            match decl.provider {
                Provider::Real => "real",
                Provider::Fake => "fake",
            }
        )))
    }

    /// Takes a host out of the net: its agents killed, its daemon gone,
    /// every link and gate it had removed. Its peers still trust it, as
    /// they would a machine that was switched off for good.
    pub async fn remove_host(&mut self, name: &str) -> Result<Ack, NetError> {
        self.host(name)?;
        let links: Vec<(String, String)> = self
            .links
            .keys()
            .filter(|(a, b)| a == name || b == name)
            .cloned()
            .collect();
        for (a, b) in &links {
            self.links.remove(&(a.clone(), b.clone()));
        }
        let process = self.hosts[name].process.clone();
        let mut said = format!("host {name} removed");
        if self.is_up(name) {
            match &process {
                Some(process) => {
                    self.withdraw_process(name);
                    if let Err(why) = process.stop().await {
                        said = format!("{said}; it would not stop ({why}), so it was killed");
                    }
                }
                None => self.take_down(name, false).await?,
            }
        }
        let dir = self.hosts[name].dir.clone();
        kill_agents_under(&dir).await?;
        if process.is_some() {
            // A daemon or supervisor that outlived its stop dies here with
            // the agents: its command line names the host's folder. Nothing
            // of the host's may run on once the net has forgotten it.
            kill_all_under(name, &dir).await?;
        }
        self.forget_host(name);
        Ok(self.ack(said))
    }

    fn forget_host(&mut self, name: &str) {
        self.hosts.remove(name);
        self.agents.retain(|_, agent| agent.host != name);
        self.topology.hosts.retain(|host| host.name != name);
        self.topology
            .links
            .retain(|link| link.a != name && link.b != name);
        self.topology.agents.retain(|agent| agent.host != name);
        self.udp.remove(name);
        self.lan.forget(name);
    }

    /// Makes two hosts trust each other and links them, as a declared
    /// link does: in process when both run in the net's process, otherwise
    /// by real pairing over the LAN through the net's gates. Returns once
    /// the link carries traffic both ways.
    pub async fn add_link(&mut self, a: &str, b: &str) -> Result<Ack, NetError> {
        let mut grown = self.topology.clone();
        grown.links.push(LinkDecl {
            a: a.to_owned(),
            b: b.to_owned(),
        });
        grown.check()?;
        self.link_hosts(a, b).await?;
        Ok(self.ack(format!("link {a} - {b} up")))
    }

    async fn link_hosts(&mut self, a: &str, b: &str) -> Result<(), NetError> {
        self.host(a)?;
        self.host(b)?;
        if self.hosts[a].process.is_some() || self.hosts[b].process.is_some() {
            self.pair_for_real(a, b).await?;
        } else {
            let edge_a = self.edge(a)?;
            let edge_b = self.edge(b)?;
            let trust = |error: tonic::Status| NetError::Host {
                host: a.to_owned(),
                error: error.message().to_owned(),
            };
            edge_a.trust(&edge_b).await.map_err(trust)?;
            edge_b.trust(&edge_a).await.map_err(trust)?;
            self.links.insert(
                link_key(a, b),
                Link {
                    severed: false,
                    live: None,
                },
            );
            self.link_all()?;
            self.wait_link(a, b, true).await?;
        }
        self.topology.links.push(LinkDecl {
            a: a.to_owned(),
            b: b.to_owned(),
        });
        Ok(())
    }

    /// Pairs `a` with `b` the way a person does, through their profiles'
    /// sockets:
    /// `b` opens a pairing window, `a` dials it with the PIN at the gate the
    /// net keeps for the pair, so the link they make can be cut there.
    async fn pair_for_real(&mut self, a: &str, b: &str) -> Result<(), NetError> {
        let gate = self.pair_gate(a, b)?;
        let id_b = self.host(b)?.host_id;
        let refused = |status: tonic::Status| NetError::Refused(Box::new(status));
        let window = self
            .admin(b)
            .await?
            .start_pairing(wire::StartPairingRequest {
                method: Some(wire::start_pairing_request::Method::Pin(wire::Empty {})),
                ..wire::StartPairingRequest::default()
            })
            .await
            .map_err(refused)?
            .into_inner();
        let Some(wire::pairing_window::Secret::Pin(pin)) = window.secret else {
            return Err(NetError::Host {
                host: b.to_owned(),
                error: "its pairing window has no PIN".to_owned(),
            });
        };
        let mut admin_a = self.admin(a).await?;
        let pending = admin_a
            .begin_pair(wire::BeginPairRequest {
                host_id: id_b.as_bytes().to_vec(),
                secret: Some(wire::begin_pair_request::Secret::Pin(pin)),
                addrs: vec![gate.to_string()],
            })
            .await
            .map_err(refused)?
            .into_inner();
        admin_a
            .confirm_pair(wire::ConfirmPairRequest {
                token: pending.token,
            })
            .await
            .map_err(refused)?;
        self.wait_paired(a, b).await
    }

    /// Waits until each of two hosts lists the other as online in its
    /// inventory, which a process host's peers show only once a link to it
    /// is up.
    async fn wait_paired(&self, a: &str, b: &str) -> Result<(), NetError> {
        for (near, far) in [(a, b), (b, a)] {
            let far_id = self.host(far)?.host_id;
            let mut inventory = self.observe_inventory(near).await?;
            inventory
                .observe_until(
                    |events| {
                        observe::inventory_hosts(events).iter().any(|host| {
                            host.host_id() == far_id.as_bytes() && observe::host_online(host)
                        })
                    },
                    PATIENCE,
                )
                .await?;
        }
        Ok(())
    }

    /// Takes a link out: the two hosts forget each other, as unpairing
    /// does, and the net stops linking them.
    pub async fn remove_link(&mut self, a: &str, b: &str) -> Result<Ack, NetError> {
        let declared = self.links.remove(&link_key(a, b));
        let listed = self
            .topology
            .links
            .iter()
            .any(|link| link_key(&link.a, &link.b) == link_key(a, b));
        if declared.is_none() && !listed {
            return Err(NetError::NoLink(a.to_owned(), b.to_owned()));
        }
        drop(declared);
        self.untrust(a, b).await?;
        self.untrust(b, a).await?;
        self.topology
            .links
            .retain(|link| link_key(&link.a, &link.b) != link_key(a, b));
        Ok(self.ack(format!("link {a} - {b} removed; each forgot the other")))
    }

    /// Adds an account to the relay's cloud, starting the relay when none
    /// runs yet. A host that was already up when the relay started has no
    /// way to it until it restarts; declare a relay up front to avoid that.
    pub async fn add_account(&mut self, account: AccountDecl) -> Result<Ack, NetError> {
        if self.relay.is_none() {
            let relay =
                Relay::start(&RelayDecl::default(), self.clock.clone(), self.client_ip).await?;
            self.relay = Some(relay);
            self.topology.relay = Some(RelayDecl::default());
            self.write_relay_ca()?;
        }
        let relay = self.relay.as_ref().expect("the relay");
        if relay.user(&account.name).is_some() {
            return Err(TopologyError::DuplicateAccount(account.name).into());
        }
        relay.add_account(&account.name, account.tier.into());
        if let Some(declared) = self.topology.relay.as_mut() {
            declared.accounts.push(account.clone());
        }
        Ok(self.ack(format!("account {} on {:?}", account.name, account.tier)))
    }

    // --- resources ---------------------------------------------------------

    pub fn topology(&self) -> &Topology {
        &self.topology
    }

    /// The directory everything in the net lives under.
    pub fn root(&self) -> &Path {
        self.root.path()
    }

    /// The directory a script's relative wait paths resolve into.
    pub fn gates(&self) -> PathBuf {
        self.root.path().join("gates")
    }

    /// Creates the gate file `name`, releasing every step waiting on it.
    pub fn open_gate(&self, name: &str) -> Result<Ack, NetError> {
        std::fs::write(self.gates().join(name), b"")?;
        Ok(self.ack(format!("gate {name} open")))
    }

    /// Writes the relay's certificate where a process host's daemon is
    /// told to find it, so it can reach the relay as an in-process host
    /// does.
    fn write_relay_ca(&self) -> Result<(), NetError> {
        let relay = self.relay()?;
        std::fs::write(self.root.path().join(RELAY_CA), relay.quic_root_pem())?;
        Ok(())
    }

    /// The relay the topology declares.
    pub fn relay(&self) -> Result<&Relay, NetError> {
        self.relay.as_ref().ok_or(NetError::NoRelay)
    }

    /// The scripted local network the hosts with discovery advertise and
    /// browse on, for announcing what no host would.
    pub fn discovery(&self) -> &ScriptedDiscovery {
        &self.bus
    }

    pub fn binaries(&self) -> &Binaries {
        &self.binaries
    }

    /// The policy clock, when the net is driven.
    pub fn clock(&self) -> Option<&DrivenClock> {
        self.driven.as_ref()
    }

    /// Moves policy time on every host.
    pub fn advance(&self, by: Duration) -> Result<Ack, NetError> {
        let driven = self.driven.as_ref().ok_or(NetError::WallClock)?;
        driven.advance(by);
        Ok(self.ack(format!("policy time +{}ms", by.as_millis())))
    }

    pub fn now_ms(&self) -> i64 {
        self.clock.now_ms()
    }

    pub fn host_names(&self) -> Vec<String> {
        self.hosts.keys().cloned().collect()
    }

    pub fn host(&self, name: &str) -> Result<&HostInfo, NetError> {
        self.hosts
            .get(name)
            .map(|host| &host.info)
            .ok_or_else(|| NetError::NoHost(name.to_owned()))
    }

    /// What `host` has written to the link log, oldest first: each of its
    /// links coming up, going down or changing path, across restarts.
    pub fn link_log(&self, host: &str) -> Result<Vec<node::link_log::LinkEvent>, NetError> {
        let id = self.host(host)?.host_id;
        Ok(match self.process(host)? {
            Some(process) => crate::link_log::read(
                &std::fs::read_to_string(process.data_dir.join(node::DAEMON_LOG))
                    .unwrap_or_default(),
                id,
            ),
            None => crate::link_log::captured(id),
        })
    }

    pub fn is_up(&self, name: &str) -> bool {
        self.hosts
            .get(name)
            .is_some_and(|host| host.daemon.is_some() || host.running)
    }

    /// How a process host runs, or why `name` is not one.
    fn process(&self, name: &str) -> Result<Option<&ProcessHost>, NetError> {
        Ok(self
            .hosts
            .get(name)
            .ok_or_else(|| NetError::NoHost(name.to_owned()))?
            .process
            .as_ref())
    }

    /// Refuses a verb that reaches into a daemon's runtime, which a process
    /// host's daemon keeps to itself.
    fn in_process(&self, name: &str, verb: &str) -> Result<(), NetError> {
        match self.process(name)? {
            Some(_) => Err(NetError::Process {
                host: name.to_owned(),
                why: format!("{verb} reaches into a daemon in the net's own process"),
            }),
            None => Ok(()),
        }
    }

    /// The host's profile runtime. Hold it only as long as a call needs:
    /// a held runtime outlives a killed daemon.
    pub fn runtime(&self, name: &str) -> Result<Arc<ProfileRuntime>, NetError> {
        self.in_process(name, "this")?;
        let host = self
            .hosts
            .get(name)
            .ok_or_else(|| NetError::NoHost(name.to_owned()))?;
        if host.daemon.is_none() {
            return Err(NetError::Down(name.to_owned()));
        }
        host.runtime
            .upgrade()
            .ok_or_else(|| NetError::Down(name.to_owned()))
    }

    /// The host's generation as of its daemon's start.
    pub fn generation(&self, name: &str) -> Result<u64, NetError> {
        Ok(self.runtime(name)?.generation())
    }

    pub fn edge(&self, name: &str) -> Result<Arc<Edge>, NetError> {
        self.runtime(name)?.edge().ok_or_else(|| NetError::Host {
            host: name.to_owned(),
            error: "its edge is not running".to_owned(),
        })
    }

    /// Another profile the host's installation serves, such as one
    /// [`Net::create_profile`] made.
    pub fn profile_runtime(
        &self,
        name: &str,
        profile: ProfileId,
    ) -> Result<Arc<ProfileRuntime>, NetError> {
        let host = self
            .hosts
            .get(name)
            .ok_or_else(|| NetError::NoHost(name.to_owned()))?;
        let daemon = host
            .daemon
            .as_ref()
            .ok_or_else(|| NetError::Down(name.to_owned()))?;
        daemon
            .lock()
            .unwrap()
            .profile(profile)
            .ok_or_else(|| NetError::Host {
                host: name.to_owned(),
                error: format!("no profile {profile}"),
            })
    }

    pub fn profile_edge(&self, name: &str, profile: ProfileId) -> Result<Arc<Edge>, NetError> {
        self.profile_runtime(name, profile)?
            .edge()
            .ok_or_else(|| NetError::Host {
                host: name.to_owned(),
                error: format!("profile {profile}'s edge is not running"),
            })
    }

    /// The installation's front door on `name`: its profiles, as a
    /// person's client calls them.
    pub async fn front_door(
        &self,
        name: &str,
    ) -> Result<ProfileServiceClient<tonic::transport::Channel>, NetError> {
        let channel = local_channel(&self.host(name)?.front_door)
            .await
            .map_err(|error| NetError::Host {
                host: name.to_owned(),
                error: error.to_string(),
            })?;
        Ok(ProfileServiceClient::new(channel))
    }

    /// The administration of `name`'s profile on its own socket: its
    /// pairing, peers and account connection, as a person's client calls
    /// them.
    pub async fn admin(
        &self,
        name: &str,
    ) -> Result<ProfileAdminServiceClient<tonic::transport::Channel>, NetError> {
        self.admin_of(name, self.host(name)?.profile).await
    }

    /// The administration of another of `name`'s profiles.
    pub async fn admin_of(
        &self,
        name: &str,
        profile: node::ProfileId,
    ) -> Result<ProfileAdminServiceClient<tonic::transport::Channel>, NetError> {
        let host = self.host(name)?;
        let socket = node::profile_dir(&host.data_dir, profile).join(node::PROFILE_SOCKET);
        let channel = local_channel(&socket)
            .await
            .map_err(|error| NetError::Host {
                host: name.to_owned(),
                error: error.to_string(),
            })?;
        Ok(ProfileAdminServiceClient::new(channel))
    }

    pub fn agent(&self, name: &str) -> Result<&NetAgent, NetError> {
        self.agents
            .get(name)
            .ok_or_else(|| NetError::NoAgent(name.to_owned()))
    }

    pub fn agents(&self) -> impl Iterator<Item = &NetAgent> {
        self.agents.values()
    }

    /// The agent's directory on its host.
    pub fn agent_dir(&self, name: &str) -> Result<PathBuf, NetError> {
        let agent = self.agent(name)?;
        let host = self.host(&agent.host)?;
        Ok(node::profile_dir(&host.data_dir, host.profile)
            .join(node::AGENTS)
            .join(agent.id.to_string()))
    }

    /// Every line `name`'s provider has read from its agent process, over
    /// all its incarnations: what it was really sent, whatever the store
    /// made of it. Only the stdio fakes (headless Claude, Codex) log.
    pub fn provider_input(&self, name: &str) -> Result<Vec<String>, NetError> {
        self.agent(name)?;
        let path = self
            .root
            .path()
            .join("scripts")
            .join(format!("{name}.{INPUT_LOG}"));
        match std::fs::read_to_string(path) {
            Ok(text) => Ok(text.lines().map(str::to_owned).collect()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(error) => Err(error.into()),
        }
    }

    /// Where the agent's journal ends now.
    pub fn journal_end(&self, name: &str) -> Result<u64, NetError> {
        Ok(journal::end(
            &self.agent_dir(name)?.join(agent_dir::JOURNAL),
        )?)
    }

    fn ack(&self, installed: String) -> Ack {
        Ack {
            installed,
            at_ms: self.clock.now_ms(),
        }
    }

    // --- agents ------------------------------------------------------------

    /// Spawns an agent on its host: a real `amux agent` process whose
    /// provider is the fake of its kind playing its script, or the real one
    /// when the agent or its host asks for it. Relative wait paths in the
    /// script name files in [`Net::gates`].
    pub async fn spawn(&mut self, decl: AgentDecl) -> Result<wire::Agent, NetError> {
        if self.agents.contains_key(&decl.name) {
            return Err(NetError::AgentExists(decl.name));
        }
        if decl.journal.is_some() {
            return Err(NetError::Unsupported(
                "a recorded journal on a real net, whose agents' providers write their own",
            ));
        }
        let host = self.host(&decl.host)?.clone();
        let parent = match &decl.parent {
            Some(parent) => Some(self.agent(parent)?.parent()),
            None => None,
        };
        decl.check_branch()?;
        let provider = self.provider_of(&decl)?;
        let script = self.write_script(&decl.name, decl.script.clone().unwrap_or_default())?;
        let id = Uuid::new_v4();
        let cwd = match &decl.repository {
            Some(repository) => host
                .work
                .with_file_name(REPOSITORIES)
                .join(repository)
                .to_string_lossy()
                .into_owned(),
            None => work_dir(&host.work, decl.cwd.as_deref())?,
        };
        if let Some(branch) = &decl.branch {
            on_branch(&decl.host, Path::new(&cwd), branch)?;
        }
        providers::choose(&self.choices(), id, provider, decl.kind, &script)?;
        let request = CreateAgentRequest {
            agent_id: id.as_bytes().to_vec(),
            host: None,
            name: Some(decl.name.clone()),
            parent,
            cwd,
            agent: Some(create_config(&decl, provider)),
            initial_prompt: decl
                .prompt
                .as_deref()
                .map(|text| submission(b"testnet-first", text)),
            new_worktree: false,
        };
        let agent = match self.hosts[&decl.host].process.clone() {
            Some(process) => process
                .client()
                .await
                .map_err(|why| NetError::Process {
                    host: decl.host.clone(),
                    why,
                })?
                .create_agent(request)
                .await
                .map_err(|status| NetError::Refused(Box::new(status)))?
                .into_inner(),
            None => {
                let runtime = self.runtime(&decl.host)?;
                runtime.set_launch(self.launch(&host, Some((decl.kind, &script, provider))));
                let spawned = runtime.spawn(request, None).await;
                runtime.set_launch(self.launch(&host, None));
                spawned?
            }
        };
        self.agents.insert(
            decl.name.clone(),
            NetAgent {
                name: decl.name.clone(),
                host: host.name.clone(),
                host_id: host.host_id,
                id,
                kind: decl.kind,
                provider,
            },
        );
        self.topology.agents.push(decl);
        Ok(agent)
    }

    /// The provider an agent runs: its own choice, else its host's; a real
    /// one needs the real tools found and the host's commands able to run
    /// them.
    fn provider_of(&mut self, decl: &AgentDecl) -> Result<Provider, NetError> {
        let host = self
            .hosts
            .get(&decl.host)
            .ok_or_else(|| NetError::NoHost(decl.host.clone()))?;
        let provider = decl.provider.unwrap_or(host.decl.provider);
        if provider == Provider::Real {
            self.ensure_real(&decl.host)?;
        }
        Ok(provider)
    }

    /// Where the agents' provider choices are recorded for the hosts'
    /// commands to read.
    fn choices(&self) -> PathBuf {
        self.root.path().join("choices")
    }

    /// The real providers, found once, and `host`'s commands rewritten to
    /// reach them if they were written without.
    fn ensure_real(&mut self, host: &str) -> Result<(), NetError> {
        if self.real.is_none() {
            let found = RealTools::find(self.binaries.dir()).map_err(|error| NetError::Host {
                host: host.to_owned(),
                error,
            })?;
            self.real = Some(found);
        }
        let has_real = self.hosts[host].dir.join("codex-home").exists();
        if !has_real {
            let commands = self.write_commands(host)?;
            self.hosts.get_mut(host).expect("the host").commands = commands;
        }
        Ok(())
    }

    fn write_commands(&self, host: &str) -> Result<Commands, NetError> {
        let own = &self.hosts[host];
        let host_script = own.decl.script.is_some().then(|| {
            self.root
                .path()
                .join("scripts")
                .join(format!("{host}.host.json"))
        });
        providers::write(&providers::Wrappers {
            host,
            dir: &own.dir,
            default: own.decl.provider,
            choices: &self.choices(),
            recordings: &self.recordings.join(host),
            real: self.real.as_ref(),
            fakes: self.binaries.dir(),
            host_script: host_script.as_deref(),
        })
        .map_err(|error| NetError::Host {
            host: host.to_owned(),
            error,
        })
    }

    /// Waits until `name` has come to rest on its host: idle or needing
    /// the person, with nothing queued, so a creation prompt's turn has
    /// run. The fleet lists agents of one standing most recently active
    /// first, so starting each once the one before has settled fixes the
    /// order they are listed in.
    pub async fn settle(&self, name: &str) -> Result<(), NetError> {
        let patience = self.patience_for(self.agent(name)?);
        self.settle_within(name, patience).await
    }

    /// [`Net::settle`] with its own deadline.
    pub async fn settle_within(&self, name: &str, patience: Duration) -> Result<(), NetError> {
        let host = self.agent(name)?.host.clone();
        let mut session = self.observe(&host, name, 0).await?;
        session
            .observe_until(
                |events| {
                    let newest = events.iter().rev().find_map(|event| match &event.of {
                        Some(wire::session_event::Of::Snapshot(snapshot)) => Some(snapshot),
                        _ => None,
                    });
                    observe::caught_up(events)
                        && newest.is_some_and(|snapshot| {
                            snapshot.queue.is_empty()
                                && matches!(
                                    snapshot.phase(),
                                    wire::Phase::Idle | wire::Phase::NeedsYou
                                )
                        })
                },
                patience,
            )
            .await?;
        Ok(())
    }

    /// Starts an exited agent's next incarnation on the same script.
    pub async fn resume(
        &mut self,
        name: &str,
        text: Option<&str>,
    ) -> Result<wire::Agent, NetError> {
        let agent = self.agent(name)?.clone();
        let host = self.host(&agent.host)?.clone();
        let script = self
            .root
            .path()
            .join("scripts")
            .join(format!("{name}.json"));
        let submission = text.map(|text| wire::PromptSubmission {
            input_id: Uuid::new_v4().as_bytes().to_vec(),
            prompt: Some(PromptInput {
                text: text.to_owned(),
                attachments: Vec::new(),
            }),
        });
        if let Some(process) = self.hosts[&agent.host].process.clone() {
            return Ok(process
                .client()
                .await
                .map_err(|why| NetError::Process {
                    host: agent.host.clone(),
                    why,
                })?
                .resume_agent(wire::ResumeAgentRequest {
                    agent_id: agent.id.as_bytes().to_vec(),
                    initial_prompt: submission,
                })
                .await
                .map_err(|status| NetError::Refused(Box::new(status)))?
                .into_inner());
        }
        let runtime = self.runtime(&agent.host)?;
        runtime.set_launch(self.launch(&host, Some((agent.kind, &script, agent.provider))));
        let bootstrap = submission.map(|submission| wire::Bootstrap {
            of: Some(wire::bootstrap::Of::Prompt(submission)),
        });
        let resumed = runtime.resume(agent.id, bootstrap).await;
        runtime.set_launch(self.launch(&host, None));
        Ok(resumed?)
    }

    /// Sends the agent a prompt through its own host.
    pub async fn send(&self, name: &str, text: &str) -> Result<wire::SendInputResponse, NetError> {
        let kind = self.agent(name)?.kind;
        self.input(name, prompt(kind, Uuid::new_v4().as_bytes(), text))
            .await
    }

    /// Sends the agent any input through its own host: an answer, a
    /// withdrawal, an interrupt.
    pub async fn input(
        &self,
        name: &str,
        input: Input,
    ) -> Result<wire::SendInputResponse, NetError> {
        let agent = self.agent(name)?;
        let request = wire::SendInputRequest {
            agent_id: agent.id.as_bytes().to_vec(),
            input: Some(input),
        };
        if let Some(process) = &self.hosts[&agent.host].process {
            return Ok(process
                .client()
                .await
                .map_err(|why| NetError::Process {
                    host: agent.host.clone(),
                    why,
                })?
                .send_input(request)
                .await
                .map_err(|status| NetError::Refused(Box::new(status)))?
                .into_inner());
        }
        let runtime = self.runtime(&agent.host)?;
        runtime
            .send_input(&request)
            .await
            .map_err(|error| NetError::Host {
                host: agent.host.clone(),
                error: error.to_string(),
            })
    }

    /// Stops the agent on its own host, as a person does there: it exits
    /// and can be resumed.
    pub async fn stop(&self, name: &str) -> Result<Ack, NetError> {
        let agent = self.agent(name)?.clone();
        let request = wire::StopAgentRequest {
            agent_id: agent.id.as_bytes().to_vec(),
        };
        match &self.hosts[&agent.host].process {
            Some(process) => {
                process
                    .client()
                    .await
                    .map_err(|why| NetError::Process {
                        host: agent.host.clone(),
                        why,
                    })?
                    .stop_agent(request)
                    .await
            }
            None => {
                self.client(&agent.host)?
                    .stop_agent(tonic::Request::new(request))
                    .await
            }
        }
        .map_err(|status| NetError::Refused(Box::new(status)))?;
        Ok(self.ack(format!("{name} stopped")))
    }

    /// What `name` offers now, as a person on `host` fetches it: the blob
    /// its newest snapshot there names, read through GetBlob, which goes to
    /// the agent's own host when this one holds no copy. With the hash it
    /// was fetched by.
    pub async fn catalogue(
        &self,
        host: &str,
        name: &str,
    ) -> Result<(Vec<u8>, wire::Catalogue), NetError> {
        use prost::Message as _;
        let id = self.agent(name)?.id;
        let mut subscription = self
            .runtime(host)?
            .subscribe_session(id.as_bytes(), 1)
            .await?;
        let mut named = None;
        while let Some(event) = subscription.next().await {
            match &event.of {
                Some(wire::session_event::Of::Snapshot(snapshot)) => {
                    named.clone_from(&snapshot.catalogue);
                }
                Some(wire::session_event::Of::CaughtUp(_)) => break,
                _ => {}
            }
        }
        let refused =
            |message: String| NetError::Refused(Box::new(tonic::Status::not_found(message)));
        let hash = named.ok_or_else(|| refused(format!("{name} offers nothing yet at {host}")))?;
        let fetched = self
            .client(host)?
            .get_blob(tonic::Request::new(wire::GetBlobRequest {
                agent_id: id.as_bytes().to_vec(),
                hash: hash.clone(),
            }))
            .await
            .map_err(|status| NetError::Refused(Box::new(status)))?
            .into_inner();
        let catalogue = wire::Catalogue::decode(fetched.bytes.as_slice())
            .map_err(|error| refused(format!("{name}'s catalogue does not decode: {error}")))?;
        Ok((hash, catalogue))
    }

    /// Deletes the agent on its own host, as a person does there. Its
    /// children follow on their own hosts by the family rule, after this
    /// returns; the net forgets only this agent.
    pub async fn delete(&mut self, name: &str) -> Result<Ack, NetError> {
        let agent = self.agent(name)?.clone();
        let request = DeleteAgentRequest {
            agent_id: agent.id.as_bytes().to_vec(),
        };
        match self.process(&agent.host)?.cloned() {
            Some(process) => {
                process
                    .client()
                    .await
                    .map_err(|why| NetError::Process {
                        host: agent.host.clone(),
                        why,
                    })?
                    .delete_agent(request)
                    .await
            }
            None => {
                self.client(&agent.host)?
                    .delete_agent(tonic::Request::new(request))
                    .await
            }
        }
        .map_err(|status| NetError::Refused(Box::new(status)))?;
        self.agents.remove(name);
        Ok(self.ack(format!("{name} deleted")))
    }

    /// The client service a person reaches on `host`: the same calls a
    /// phone or terminal makes there, forwarded to another host where the
    /// agent lives there.
    pub fn client(&self, host: &str) -> Result<ClientApi, NetError> {
        Ok(ClientApi::new(&self.runtime(host)?, None))
    }

    /// The client service as `name`'s tools socket serves it: every call
    /// is made as that agent, which its host checks and forwards.
    pub fn tools(&self, name: &str) -> Result<ClientApi, NetError> {
        let agent = self.agent(name)?;
        Ok(ClientApi::new(&self.runtime(&agent.host)?, Some(agent.id)))
    }

    /// `parent` spawns `decl` through its tools socket, naming
    /// `decl.host` as the host: the parent's host resolves the name among
    /// the hosts it trusts and forwards the create there, where the child
    /// plays its script in that host's work directory.
    pub async fn spawn_child(
        &mut self,
        parent: &str,
        decl: AgentDecl,
    ) -> Result<wire::Agent, NetError> {
        if self.agents.contains_key(&decl.name) {
            return Err(NetError::AgentExists(decl.name));
        }
        if decl.branch.is_some() {
            return Err(NetError::Host {
                host: decl.host,
                error: format!(
                    "child {:?} names a branch; only the net's own spawns make one",
                    decl.name
                ),
            });
        }
        let host = self.host(&decl.host)?.clone();
        let script = self.write_script(&decl.name, decl.script.clone().unwrap_or_default())?;
        let id = Uuid::new_v4();
        let provider = self.provider_of(&decl)?;
        let request = CreateAgentRequest {
            agent_id: id.as_bytes().to_vec(),
            host: Some(wire::HostRef::named(decl.host.clone())),
            name: Some(decl.name.clone()),
            initial_prompt: decl
                .prompt
                .as_deref()
                .map(|text| submission(b"testnet-first", text)),
            cwd: decl
                .cwd
                .clone()
                .unwrap_or_else(|| host.work.to_string_lossy().into_owned()),
            agent: Some(create_config(&decl, provider)),
            ..CreateAgentRequest::default()
        };
        providers::choose(&self.choices(), id, provider, decl.kind, &script)?;
        let tools = self.tools(parent)?;
        let target = self.runtime(&decl.host)?;
        target.set_launch(self.launch(&host, Some((decl.kind, &script, provider))));
        let spawned = tools.create_agent(tonic::Request::new(request)).await;
        target.set_launch(self.launch(&host, None));
        let agent = spawned
            .map_err(|status| NetError::Refused(Box::new(status)))?
            .into_inner();
        self.agents.insert(
            decl.name.clone(),
            NetAgent {
                name: decl.name,
                host: host.name.clone(),
                host_id: host.host_id,
                id,
                kind: decl.kind,
                provider,
            },
        );
        Ok(agent)
    }

    /// The next process to start on `name`'s host, whoever starts it,
    /// plays `script` as `name`: for a resume the host makes on its own,
    /// such as a parent's message to its exited child.
    pub fn next_start(&self, name: &str, script: Script) -> Result<Ack, NetError> {
        let agent = self.agent(name)?.clone();
        let host = self.host(&agent.host)?.clone();
        let path = self.write_script(name, script)?;
        self.runtime(&agent.host)?
            .set_launch(self.launch(&host, Some((agent.kind, &path, agent.provider))));
        Ok(self.ack(format!("{name}'s next start plays its new script")))
    }

    /// Stops `name`'s process where it stands, as a machine too busy to
    /// schedule it would: it keeps its lock and its connection and answers
    /// nothing until thawed.
    pub fn freeze(&self, name: &str) -> Result<Ack, NetError> {
        self.signal(name, "-STOP")?;
        Ok(self.ack(format!("{name} frozen")))
    }

    /// Lets a frozen process run again.
    pub fn thaw(&self, name: &str) -> Result<Ack, NetError> {
        self.signal(name, "-CONT")?;
        Ok(self.ack(format!("{name} thawed")))
    }

    fn signal(&self, name: &str, signal: &str) -> Result<(), NetError> {
        let dir = self.agent_dir(name)?;
        #[cfg(unix)]
        {
            // `amux agent <dir>`: the directory is on its command line.
            let status = std::process::Command::new("pkill")
                .arg(signal)
                .arg("-f")
                .arg(format!("agent {}", dir.display()))
                .status()?;
            if status.success() {
                Ok(())
            } else {
                Err(NetError::Host {
                    host: self.agent(name)?.host.clone(),
                    error: format!("no process of {name} to signal"),
                })
            }
        }
        #[cfg(not(unix))]
        {
            let _ = (dir, signal);
            Err(NetError::Unsupported("signalling agent processes"))
        }
    }

    fn write_script(&self, name: &str, mut script: Script) -> Result<PathBuf, NetError> {
        let gates = self.gates();
        let resolve = |path: &mut PathBuf| {
            if path.is_relative() {
                *path = gates.join(&*path);
            }
        };
        for step in &mut script.steps {
            match step {
                Step::WaitFor { path } => resolve(path),
                Step::Tool(tool) => {
                    if let Some(path) = tool.wait_for.as_mut() {
                        resolve(path);
                    }
                }
                _ => {}
            }
        }
        let path = self
            .root
            .path()
            .join("scripts")
            .join(format!("{name}.json"));
        let text = serde_json::to_string_pretty(&script).map_err(std::io::Error::other)?;
        std::fs::write(&path, text)?;
        Ok(path)
    }

    /// What a host's agents start with. With an agent, its provider: the
    /// fake of its kind playing its script, or the real one. Without, the
    /// host's defaults: the host's declared script or nothing on a fake
    /// host, the real providers on a real one.
    fn launch(&self, host: &HostInfo, agent: Option<(FakeKind, &Path, Provider)>) -> Launch {
        let own = &self.hosts[&host.name];
        let mut launch = Launch {
            install_path: self.binaries.amux(),
            claude_command: self
                .binaries
                .fake(FakeKind::ClaudeSdk)
                .to_string_lossy()
                .into_owned(),
            codex_command: self
                .binaries
                .fake(FakeKind::Codex)
                .to_string_lossy()
                .into_owned(),
            step_budget: daemon_core::StepBudget::TEST,
            // An agent's core gets the daemon's test budget, for the same
            // reason: a loaded debug-build run stretches ordinary steps.
            agent_step_budget_micros: daemon_core::StepBudget::TEST.micros,
            ..Launch::default()
        };
        let provider = agent.map_or(own.decl.provider, |(_, _, provider)| provider);
        if provider == Provider::Real {
            // The host's own commands run the real provider through its
            // probe: a client's create or an agent's spawn tool carries no
            // arguments of the net's, and terminal Claude's version check
            // runs the command bare, so the command is the one place every
            // start passes.
            launch.claude_command = own.commands.claude.to_string_lossy().into_owned();
            launch.codex_command = own.commands.codex.to_string_lossy().into_owned();
        } else {
            launch.provider_env.insert(
                "CLAUDE_CONFIG_DIR".to_owned(),
                own.dir.join("claude").to_string_lossy().into_owned(),
            );
            let host_script = own.decl.script.is_some().then(|| {
                self.root
                    .path()
                    .join("scripts")
                    .join(format!("{}.host.json", host.name))
            });
            let fake = agent.map(|(kind, script, _)| (kind, script)).or(host_script
                .as_deref()
                .map(|script| (FakeKind::ClaudeSdk, script)));
            if let Some((kind, script)) = fake {
                if kind == FakeKind::ClaudePty {
                    launch.claude_command = self.binaries.fake(kind).to_string_lossy().into_owned();
                }
                launch
                    .provider_env
                    .insert(SCRIPT_ENV.to_owned(), script.to_string_lossy().into_owned());
                launch.provider_env.insert(
                    INPUT_LOG_ENV.to_owned(),
                    script
                        .with_extension(INPUT_LOG)
                        .to_string_lossy()
                        .into_owned(),
                );
            }
        }
        launch.agent.grace_secs = AGENT_GRACE_SECS;
        launch.agent.drain_secs = 5;
        if let Some(mib) = self.facts_ring_mib {
            launch.agent.facts_ring_mib = mib;
        }
        launch.stop_deadline_ms = 15_000;
        if !own.decl.repositories.is_empty() {
            launch.repository_roots = vec![own.dir.join(REPOSITORIES)];
        }
        if let Some(k) = own.decl.tail_rows {
            launch.tail_rows = k;
        }
        if let Some(hook) = &self.launch_hook {
            hook(&host.name, &mut launch);
        }
        launch
    }

    // --- hosts -------------------------------------------------------------

    fn create_host(&mut self, decl: &HostDecl) -> Result<(), NetError> {
        // A daemon in its own process keeps the machine's time, which a
        // driven clock cannot advance.
        if decl.runs_as_process() && self.driven.is_some() {
            return Err(NetError::Process {
                host: decl.name.clone(),
                why: "a process host keeps wall time: start the net with ClockMode::Wall"
                    .to_owned(),
            });
        }
        if decl.supervised && self.feed.is_none() {
            self.feed = Some(Feed::start(self.root.path().join("releases"))?);
        }
        // A host gets a directory nothing has used: a removed host's stays
        // behind with its profile, which a daemon would serve again as the
        // new host's, and names that differ only in case share a directory
        // on a case-insensitive filesystem.
        let mut dir = self.root.path().join(&decl.name);
        let mut n = 1;
        while dir.exists() {
            dir = self.root.path().join(format!("{}-{n}", decl.name));
            n += 1;
        }
        let data_dir = dir.join("data");
        let work = dir.join("work");
        std::fs::create_dir_all(&work)?;
        for repository in &decl.repositories {
            let path = dir.join(REPOSITORIES).join(repository);
            std::fs::create_dir_all(&path)?;
            let git = |args: &[&str]| git(&decl.name, &path, args);
            git(&["init", "--quiet"])?;
            let mut files = false;
            for (file, text) in &decl.files {
                if decl.repository_of(file) != Some(repository.as_str()) {
                    continue;
                }
                let target = dir.join(REPOSITORIES).join(file);
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&target, text)?;
                files = true;
            }
            if files {
                git(&["add", "--all"])?;
                git(&[
                    "commit",
                    "--quiet",
                    "--message",
                    "The files the topology declares",
                ])?;
            }
        }
        let profile = node::create_profile(&data_dir)?;
        let host_id = profile;
        let front_door = dir.join("door.sock");
        let config = dir.join("config.toml");
        std::fs::write(
            &config,
            self.installation_toml(decl, &dir, &data_dir, &front_door)?,
        )?;
        if let Some(script) = &decl.script {
            self.write_script(&format!("{}.host", decl.name), script.clone())?;
        }
        let process = decl.runs_as_process().then(|| ProcessHost {
            amux: if decl.supervised {
                dir.join("install").join("amux")
            } else {
                self.binaries.amux()
            },
            config: config.clone(),
            data_dir: data_dir.clone(),
            front_door: front_door.clone(),
            bin: dir.join("bin"),
            bonjour: decl.bonjour,
            relay_ca: self.root.path().join(RELAY_CA),
        });
        self.lan.learn(&decl.name, host_id);
        self.hosts.insert(
            decl.name.clone(),
            Host {
                decl: decl.clone(),
                info: HostInfo {
                    name: decl.name.clone(),
                    host_id,
                    profile,
                    data_dir,
                    front_door,
                    config,
                    work,
                    dir: dir.clone(),
                    process: decl.runs_as_process(),
                },
                dir,
                daemon: None,
                runtime: Weak::new(),
                boot: 1,
                checkpoint: None,
                lan_handed: false,
                process,
                running: false,
                commands: Commands {
                    claude: PathBuf::new(),
                    codex: PathBuf::new(),
                },
            },
        );
        let made = (|| {
            let host = &self.hosts[&decl.name];
            let amux = host
                .process
                .as_ref()
                .map_or_else(|| self.binaries.amux(), |process| process.amux.clone());
            crate::ssh::write_login(&host.dir, &amux, &host.info.config)?;
            if decl.provider == Provider::Real {
                self.ensure_real(&decl.name)?;
            } else {
                let commands = self.write_commands(&decl.name)?;
                self.hosts.get_mut(&decl.name).expect("the host").commands = commands;
            }
            if decl.supervised {
                self.install_supervised(&decl.name)?;
            }
            Ok::<(), NetError>(())
        })();
        if let Err(error) = made {
            self.hosts.remove(&decl.name);
            self.lan.forget(&decl.name);
            return Err(error);
        }
        self.topology.hosts.push(decl.clone());
        Ok(())
    }

    /// The host's installation config: what the terminal client and the
    /// CLI read on any host, and what a process host's daemon runs by.
    fn installation_toml(
        &self,
        decl: &HostDecl,
        dir: &Path,
        data_dir: &Path,
        front_door: &Path,
    ) -> Result<String, NetError> {
        let quoted = |path: &Path| format!("{:?}", path.display().to_string());
        // Only a supervised process host runs under a supervisor; a client
        // of an in-process host must never start one.
        let supervised = decl.runs_as_process() && decl.supervised;
        let mut text = format!(
            "root = {}\nfront_door_socket = {}\nhost_name = {:?}\nsupervisor = {supervised}\n\
             updates = \"manual\"\nkeep_awake = false\ntelemetry = false\n",
            quoted(data_dir),
            quoted(front_door),
            decl.name,
        );
        if decl.runs_as_process() {
            text.push_str(&format!("releases_url = {:?}\n", self.feed_url()));
            if !decl.repositories.is_empty() {
                text.push_str(&format!(
                    "repository_roots = [{}]\n",
                    quoted(&dir.join(REPOSITORIES))
                ));
            }
            let scope = decl
                .scope
                .clone()
                .unwrap_or_else(|| self.topology.scope.clone());
            text.push_str(&format!(
                "[discovery]\nscope = {scope:?}\n\
                 [agent]\ngrace_secs = {AGENT_GRACE_SECS}\ndrain_secs = 5\n"
            ));
            if let Some(mib) = self.facts_ring_mib {
                text.push_str(&format!("facts_ring_mib = {mib}\n"));
            }
        }
        if decl.provider == Provider::Real {
            // A person's own defaults for a new agent, so one made from the
            // terminal client starts on the small models; any client may
            // still choose another.
            text.push_str(&format!(
                "[new_agent.claude]\nmodel = \"{SMALL_CLAUDE}\"\neffort = \"medium\"\npermission = \"default\"\n\
                 [new_agent.codex]\nmodel = \"{SMALL_CODEX}\"\neffort = \"medium\"\npermission = \"default\"\n",
            ));
        }
        Ok(text)
    }

    async fn start_host(&mut self, name: &str) -> Result<(), NetError> {
        if let Some(relay) = &self.relay
            && !self.udp.contains_key(name)
        {
            let gate = relay.gate().await?;
            gate.set_faults(self.relay_faults.then(self.lan.faults(name)));
            self.udp.insert(name.to_owned(), gate);
        }
        let host = self
            .hosts
            .get(name)
            .ok_or_else(|| NetError::NoHost(name.to_owned()))?;
        if host.daemon.is_some() || host.running {
            return Err(NetError::Running(name.to_owned()));
        }
        if let Some(process) = host.process.clone() {
            process.start().await.map_err(|error| NetError::Start {
                host: name.to_owned(),
                error,
            })?;
            self.hosts.get_mut(name).expect("the host").running = true;
            self.advertise_process(name);
            return Ok(());
        }
        self.wait_lan_port(name).await?;
        let host = &self.hosts[name];
        let decl = &host.decl;
        let mut edge = EdgeOptions {
            host_name: decl.name.clone(),
            kinds: node::local_kinds(),
            lan: decl.lan.then(|| LanOptions {
                bind: SocketAddr::new(loopback(decl), 0),
                socket: None,
            }),
            discovery: if decl.bonjour && !self.gated_lan {
                Some(Arc::new(|| {
                    Ok(Arc::new(crate::bonjour::SystemBonjour::new())
                        as Arc<dyn node::discovery::Discovery>)
                }))
            } else if decl.bonjour {
                // A client outside the net finds the host at its front
                // gate, so the net can degrade the client's network.
                let (lan, me, on) = (self.lan.clone(), decl.name.clone(), self.client_ip);
                Some(Arc::new(move || {
                    Ok(Arc::new(FrontAdvert::new(
                        Arc::new(crate::bonjour::SystemBonjour::new()),
                        &me,
                        lan.clone(),
                        on,
                    ))
                        as Arc<dyn node::discovery::Discovery>)
                }))
            } else if !self.gated_lan {
                decl.discovery.then(|| scripted_discovery(&self.bus))
            } else {
                decl.discovery.then(|| {
                    // Each host finds its peers at the gates the net keeps
                    // for each pair, so any pair can be cut.
                    let (lan, me, bus) = (self.lan.clone(), decl.name.clone(), self.bus.clone());
                    let factory: node::DiscoveryFactory = Arc::new(move || {
                        let inner = scripted_discovery(&bus)()?;
                        Ok(Arc::new(Gated::new(inner, &me, lan.clone()))
                            as Arc<dyn node::discovery::Discovery>)
                    });
                    factory
                })
            },
            discovery_scope: decl
                .scope
                .clone()
                .unwrap_or_else(|| self.topology.scope.clone()),
            dial: decl.lan,
            link_socket: true,
            cloud: node::CloudOptions::default(),
            // An in-process host is reached on loopback alone, which it
            // never lists, and has no interfaces of its own to list after
            // a network change either.
            interfaces: Some(Vec::new()),
            liveness: Default::default(),
        };
        if let Some(relay) = &self.relay {
            edge.cloud = relay.cloud_options(&self.udp[name]);
            // The relay tells a host the address it sees its packets come
            // from, which a peer punches to: behind a router that must be
            // the router's public port, not the gate's own socket.
            if host.decl.router.is_some()
                && let Some(quic) = &mut edge.cloud.relay_quic
            {
                quic.addr = relay.quic();
            }
        }
        if let Some(hook) = &self.edge_hook {
            hook(name, &mut edge);
        }
        let handed = edge.lan.as_ref().is_some_and(|lan| lan.socket.is_some());
        let options = node::StartOptions {
            data_dir: host.info.data_dir.clone(),
            boot_id: Some(format!("boot-{}", host.boot)),
            launch: self.launch(&host.info, None),
            clock: self.clock.clone(),
            push: Arc::new(node::NoopSender),
            daemon_log: None,
            front_door: Some(host.info.front_door.clone()),
            edge,
            analytics: self.analytics.clone().unwrap_or(node::Telemetry::Off),
        };
        let daemon = node::start(options, None)
            .await
            .map_err(|error| NetError::Start {
                host: name.to_owned(),
                error: error.to_string(),
            })?;
        let runtime = daemon
            .profile(self.hosts[name].info.profile)
            .ok_or_else(|| NetError::Start {
                host: name.to_owned(),
                error: "the daemon does not host the profile".to_owned(),
            })?;
        let host = self.hosts.get_mut(name).expect("the host");
        host.runtime = Arc::downgrade(&runtime);
        host.daemon = Some(std::sync::Mutex::new(daemon));
        host.lan_handed = handed;
        if let Some(kind) = host.decl.router {
            self.put_behind_router(name, kind)?;
        }
        Ok(())
    }

    /// Makes `a` trust `b` as pairing would, without linking them: they
    /// meet over whatever route they find.
    pub async fn trust(&mut self, a: &str, b: &str) -> Result<Ack, NetError> {
        self.in_process(a, "trusting without pairing")?;
        self.in_process(b, "trusting without pairing")?;
        let far = self.edge(b)?;
        self.edge(a)?
            .trust(&far)
            .await
            .map_err(|error| NetError::Host {
                host: a.to_owned(),
                error: error.message().to_owned(),
            })?;
        Ok(self.ack(format!("{a} trusts {b}")))
    }

    /// Signs the host's profile in to `account` on the net's relay, through
    /// the front door as a person's login does, adopting the agents and
    /// peers it holds, and returns once its relay link is up.
    pub async fn sign_in(&self, host: &str, account: &str) -> Result<Ack, NetError> {
        let relay = self.relay()?;
        let profile = self.host(host)?.profile;
        self.front_door(host)
            .await?
            .bind_profile(wire::BindProfileRequest {
                profile_id: Some(profile.as_bytes().to_vec()),
                cloud_url: relay.url().to_owned(),
                staged_refresh_token: Relay::login(account),
                // The person confirms adopting whatever the profile holds.
                adopt_non_pristine: true,
                ..wire::BindProfileRequest::default()
            })
            .await
            .map_err(|status| NetError::Refused(Box::new(status)))?;
        if self.process(host)?.is_some() {
            let what = format!("{host}'s relay link");
            observe::until(&what, || async {
                let profiles = self
                    .front_door(host)
                    .await
                    .map_err(|error| error.to_string())?
                    .list_profiles(wire::ListProfilesRequest {})
                    .await
                    .map_err(|status| status.message().to_owned())?
                    .into_inner()
                    .profiles;
                let cloud = profiles.first().map(|info| &info.cloud);
                profiles
                    .first()
                    .and_then(wire::Profile::relay_connected)
                    .map(|_| ())
                    .ok_or_else(|| format!("cloud {cloud:?}"))
            })
            .await?;
            return Ok(self.ack(format!("{host} signed in to {account}")));
        }
        let edge = self.edge(host)?;
        observe::until(&format!("{host}'s relay link"), || {
            let relay = edge.relay();
            async move {
                matches!(relay, Some(node::Relay::Connected { .. }))
                    .then_some(())
                    .ok_or_else(|| format!("relay {relay:?}"))
            }
        })
        .await?;
        Ok(self.ack(format!("{host} signed in to {account}")))
    }

    /// Changes what `account` has bought, and has each host signed in to
    /// it learn that at once, as the host where a purchase or a
    /// cancellation happened does: an in-process host asks for its
    /// entitlement over its relay link. A process host's daemon offers no
    /// socket that asks it to, so its profile is paused, the relay token it
    /// keeps between launches forgotten, and the profile resumed, which
    /// dials the relay afresh for a credential of the new tier; its relay
    /// link drops for that moment.
    pub async fn set_tier(&mut self, account: &str, tier: TierDecl) -> Result<Ack, NetError> {
        self.relay()?.set_tier(account, tier.into());
        if let Some(declared) = self.topology.relay.as_mut() {
            for known in declared
                .accounts
                .iter_mut()
                .filter(|known| known.name == account)
            {
                known.tier = tier;
            }
        }
        let up: Vec<String> = self
            .hosts
            .keys()
            .filter(|name| self.is_up(name))
            .cloned()
            .collect();
        let wanted = match tier {
            TierDecl::Pro => wire::Tier::Pro,
            TierDecl::Free => wire::Tier::Free,
        };
        let mut told = Vec::new();
        for host in up {
            let profile = self.host(&host)?.profile;
            let info = self
                .front_door(&host)
                .await?
                .list_profiles(wire::ListProfilesRequest {})
                .await
                .map_err(|status| NetError::Refused(Box::new(status)))?
                .into_inner()
                .profiles
                .into_iter()
                .find(|info| info.id == profile.as_bytes());
            // Only a host whose relay link is up has anything to learn now;
            // any other learns the tier when its link comes up.
            if !info.is_some_and(|info| {
                info.account_subject == account && info.relay_connected().is_some()
            }) {
                continue;
            }
            if self.process(&host)?.is_none() {
                self.edge(&host)?
                    .refresh_entitlement()
                    .await
                    .map_err(|error| NetError::Host {
                        host: host.clone(),
                        error,
                    })?;
            } else {
                let mut admin = self.admin(&host).await?;
                admin
                    .pause_profile(wire::PauseProfileRequest {})
                    .await
                    .map_err(|status| NetError::Refused(Box::new(status)))?;
                let kept = node::profile_dir(&self.host(&host)?.data_dir, profile)
                    .join(node::RELAY_TOKEN_FILE);
                match std::fs::remove_file(&kept) {
                    Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                        return Err(NetError::Io(error));
                    }
                    _ => {}
                }
                admin
                    .resume_profile(wire::ResumeProfileRequest {})
                    .await
                    .map_err(|status| NetError::Refused(Box::new(status)))?;
            }
            let what = format!("{host}'s relay link on {tier:?}");
            observe::until(&what, || async {
                let info = self
                    .front_door(&host)
                    .await
                    .map_err(|error| error.to_string())?
                    .list_profiles(wire::ListProfilesRequest {})
                    .await
                    .map_err(|status| status.message().to_owned())?
                    .into_inner()
                    .profiles
                    .into_iter()
                    .find(|info| info.id == profile.as_bytes());
                let seen = info.as_ref().map(|info| &info.cloud);
                (info
                    .as_ref()
                    .and_then(wire::Profile::relay_connected)
                    .map(wire::RelayConnected::tier)
                    == Some(wanted))
                .then_some(())
                .ok_or_else(|| format!("cloud {seen:?}"))
            })
            .await?;
            told.push(host);
        }
        Ok(self.ack(format!(
            "{account} is on {tier:?}{}",
            if told.is_empty() {
                String::new()
            } else {
                format!("; {} learned it", told.join(", "))
            }
        )))
    }

    /// The address of a gate in front of `host`'s LAN listener, started
    /// the first time it is asked for. A client that dials it instead of
    /// the host reaches the host through a network the net can slow down
    /// or make lossy with [`Net::set_lan_faults`]; the host's own address
    /// stays as it was, so a pairing link has to be rewritten to name the
    /// gate. Only a running host with a LAN listener has one.
    pub async fn lan_gate(&mut self, host: &str) -> Result<SocketAddr, NetError> {
        let port = self.lan_port(host).map_err(|_| NetError::Host {
            host: host.to_owned(),
            error: "has no LAN listener to gate".to_owned(),
        })?;
        Ok(self.lan.front(
            host,
            Some(SocketAddr::new(loopback(&self.hosts[host].decl), port)),
            self.client_ip,
        )?)
    }

    /// Binds `host`'s socket to IPv6 loopback alone from its next start,
    /// so it reaches and is reached over IPv6 only. In-process hosts only.
    pub fn bind_ipv6_only(&mut self, host: &str) -> Result<Ack, NetError> {
        self.in_process(host, "binding IPv6 only")?;
        self.hosts
            .get_mut(host)
            .ok_or_else(|| NetError::NoHost(host.to_owned()))?
            .decl
            .ipv6_only = true;
        Ok(self.ack(format!("{host} binds IPv6 only from its next start")))
    }

    /// The SSH target that reaches `host`: give it to a peer as its way
    /// there. The stand-in on the PATH runs the far host's `amux` for it.
    pub fn ssh_target(&self, host: &str) -> Result<String, NetError> {
        Ok(self.host(host)?.dir.to_string_lossy().into_owned())
    }

    /// Puts an imitated router of `kind` in front of `host`'s link socket,
    /// now if it runs and on every start after; its links migrate onto the
    /// router's public port. In-process hosts only.
    pub fn set_router(&mut self, host: &str, kind: RouterKind) -> Result<Ack, NetError> {
        self.in_process(host, "a router in front of the link socket")?;
        self.hosts
            .get_mut(host)
            .ok_or_else(|| NetError::NoHost(host.to_owned()))?
            .decl
            .router = Some(kind);
        if self.is_up(host) {
            self.put_behind_router(host, kind)?;
        }
        Ok(self.ack(format!("{host} behind a {kind:?} router")))
    }

    fn put_behind_router(&self, host: &str, kind: RouterKind) -> Result<(), NetError> {
        let router = Router::new(kind, ROUTER_IDLE)?;
        self.edge(host)?.rebind(router)?;
        Ok(())
    }

    /// Moves `host` onto another network, as a phone leaving Wi-Fi does: its
    /// link socket is rebound to a fresh port, or behind a fresh router of
    /// the same kind with new public ports, its links migrate, and its path
    /// machines are told as a phone's are. In-process hosts that run only.
    pub fn change_network(&self, host: &str) -> Result<Ack, NetError> {
        self.in_process(host, "changing the network under the link socket")?;
        let edge = self.edge(host)?;
        let own = &self.hosts[host];
        let substrate: Arc<dyn node::socket::Datagrams> = match own.decl.router {
            Some(kind) => Router::new(kind, ROUTER_IDLE)?,
            None => node::socket::LinkSocket::bind_dual_stack(
                SocketAddr::new(loopback(&own.decl), 0),
                None,
            )?,
        };
        let to = substrate.local_addr()?;
        edge.rebind(substrate)?;
        // As a phone's runtime hears from its path monitor: every path
        // machine probes its paths again from the new network.
        let told = edge.network_changed()?;
        Ok(self.ack(format!(
            "{host} moved to another network, now at {to}; {} path machines told",
            told.machines_told
        )))
    }

    /// The path carrying `host`'s link to `peer` now, by `host`'s link log:
    /// the path its last line for the peer names, or none once that line
    /// says the link went down.
    pub fn carrying_path(
        &self,
        host: &str,
        peer: &str,
    ) -> Result<Option<node::link_log::PathKind>, NetError> {
        let peer = self.host(peer)?.host_id;
        Ok(self
            .link_log(host)?
            .iter()
            .rev()
            .find(|event| event.peer == peer)
            .and_then(|event| {
                (event.event != node::link_log::LinkTransition::Down).then_some(event.path)
            }))
    }

    /// What the gate in front of `host`'s LAN listener does to each
    /// datagram from now on: delay, loss, or neither.
    pub fn set_lan_faults(&self, host: &str, faults: Faults) -> Result<Ack, NetError> {
        if !self.lan.has_front(host) {
            return Err(NetError::Host {
                host: host.to_owned(),
                error: "has no LAN gate; ask for one first".to_owned(),
            });
        }
        self.lan.set_front_faults(host, faults);
        Ok(self.ack(format!("{host}'s LAN gate: {}", faults.describe())))
    }

    /// A gate in front of the relay's QUIC carrier, which a client outside
    /// the net dials in the relay's place so its packets can be delayed or
    /// lost; the same gate on every ask.
    pub async fn relay_gate(&mut self) -> Result<SocketAddr, NetError> {
        if let Some(gate) = &self.relay_gate {
            return Ok(gate.addr());
        }
        let gate = match self.client_ip {
            Some(ip) => self.relay()?.gate_on(ip)?,
            None => self.relay()?.gate().await?,
        };
        let addr = gate.addr();
        self.relay_gate = Some(gate);
        Ok(addr)
    }

    /// What the gate in front of the relay does to each datagram from now
    /// on: delay, loss, or neither.
    pub fn set_relay_faults(&self, faults: Faults) -> Result<Ack, NetError> {
        let gate = self.relay_gate.as_ref().ok_or_else(|| NetError::Host {
            host: "relay".to_owned(),
            error: "has no gate; ask for one first".to_owned(),
        })?;
        gate.set_faults(faults);
        Ok(self.ack(format!("the relay's gate: {}", faults.describe())))
    }

    /// Takes UDP away from the host's way to the relay, or gives it back:
    /// datagrams through its gate are dropped both ways while blocked.
    pub fn block_udp(&self, host: &str, blocked: bool) -> Result<Ack, NetError> {
        let gate = self
            .udp
            .get(host)
            .ok_or_else(|| NetError::NoHost(host.to_owned()))?;
        gate.block(blocked);
        Ok(self.ack(format!(
            "UDP to the relay {} for {host}",
            if blocked { "blocked" } else { "open" }
        )))
    }

    /// Makes `a` forget `b`, as unpairing does: `b`'s key leaves `a`'s
    /// trust store and the links between them close.
    pub async fn untrust(&mut self, a: &str, b: &str) -> Result<Ack, NetError> {
        let peer = self.host(b)?.host_id;
        if self.process(a)?.is_some() {
            self.admin(a)
                .await?
                .unpair(wire::UnpairRequest {
                    peer: Some(wire::HostRef {
                        of: Some(wire::host_ref::Of::HostId(peer.as_bytes().to_vec())),
                    }),
                    reason: "testnet".to_owned(),
                })
                .await
                .map_err(|status| NetError::Refused(Box::new(status)))?;
            return Ok(self.ack(format!("{a} no longer trusts {b}")));
        }
        self.edge(a)?
            .unpair(
                wire::HostRef {
                    of: Some(wire::host_ref::Of::HostId(peer.as_bytes().to_vec())),
                },
                "testnet".to_owned(),
            )
            .await
            .map_err(|error| NetError::Host {
                host: a.to_owned(),
                error: error.message().to_owned(),
            })?;
        Ok(self.ack(format!("{a} no longer trusts {b}")))
    }

    /// Takes a host's daemon down: `clean` shuts it down, otherwise it
    /// crashes. Its links drop as its sockets would, and this returns once
    /// nothing of that run is left in the process.
    async fn take_down(&mut self, name: &str, clean: bool) -> Result<(), NetError> {
        let host = self
            .hosts
            .get_mut(name)
            .ok_or_else(|| NetError::NoHost(name.to_owned()))?;
        let daemon = host
            .daemon
            .take()
            .ok_or_else(|| NetError::Down(name.to_owned()))?
            .into_inner()
            .unwrap_or_else(|poison| poison.into_inner());
        host.runtime = Weak::new();
        for ((a, b), link) in &mut self.links {
            if a == name || b == name {
                link.live = None;
            }
        }
        let runs: Vec<Weak<ProfileRuntime>> =
            daemon.profiles().iter().map(Arc::downgrade).collect();
        if clean {
            daemon.shutdown().await?;
        } else {
            drop(daemon);
        }
        observe::until(&format!("{name}'s runtime to be gone"), || {
            let live = runs.iter().filter(|run| run.upgrade().is_some()).count();
            async move {
                (live == 0)
                    .then_some(())
                    .ok_or_else(|| format!("{live} runtime references live"))
            }
        })
        .await?;
        Ok(())
    }

    // --- faults ------------------------------------------------------------

    /// Makes `a` fail every channel to `b` before it opens, as when the
    /// far end refuses each one, while the link between them and the
    /// streams it already carries stay up until a call fails on it. `b`
    /// then reads offline to `a`, as in production: `a` closes its own link
    /// to `b`, or drops the relay's channel and keeps trying a fresh one.
    /// `refuse: false` lifts it and has `a` try a channel at once; a link
    /// between them in this process that a failed call closed is linked
    /// again, as redialling would bring a real one back.
    pub async fn refuse_calls(&mut self, a: &str, b: &str, refuse: bool) -> Result<Ack, NetError> {
        self.in_process(a, "refusing calls")?;
        let peer = self.host(b)?.host_id;
        self.edge(a)?.refuse_calls(peer, refuse).await;
        if !refuse {
            self.redial(a, b).await?;
        }
        Ok(self.ack(format!(
            "{a} {} calls to {b}",
            if refuse { "refuses" } else { "makes" }
        )))
    }

    /// Links `a` and `b` again in this process if a failed call closed
    /// their link, as redialling brings a real one back, and returns once
    /// it is up. Nothing happens to a link that is up or was severed.
    pub async fn redial(&mut self, a: &str, b: &str) -> Result<Ack, NetError> {
        if self
            .links
            .get(&link_key(a, b))
            .is_some_and(|link| !link.severed && link.live.is_some())
            && !self.link_up(a, b).await
        {
            self.links.get_mut(&link_key(a, b)).expect("the link").live = None;
            self.wait_direct_gone(a, b).await?;
            self.link_all()?;
            self.wait_link(a, b, true).await?;
        }
        Ok(self.ack(format!("link {a} - {b} redialled")))
    }

    /// Cuts the link between two hosts the way a dead connection goes: no
    /// LinkClose, the carrier just stops.
    pub fn sever_link(&mut self, a: &str, b: &str) -> Result<Ack, NetError> {
        if !self.links.contains_key(&link_key(a, b)) {
            return self.cut_lan(a, b, true);
        }
        let link = self
            .links
            .get_mut(&link_key(a, b))
            .ok_or_else(|| NetError::NoLink(a.to_owned(), b.to_owned()))?;
        link.severed = true;
        if let Some(live) = link.live.take() {
            live.into_inner()
                .unwrap_or_else(|poison| poison.into_inner())
                .sever();
        }
        Ok(self.ack(format!("link {a} - {b} severed")))
    }

    /// Cuts a link and hands back what flips it from then on, without the
    /// net: a flap's period must not wait on a request that holds the net
    /// for minutes. Until [`Net::restore_link`] the net leaves the link to
    /// the flap. A flap holds the hosts as they were when it began: one
    /// restarted while it runs stays cut from the other until the flap
    /// stops and the link is restored.
    pub(crate) fn flap_link(&mut self, a: &str, b: &str) -> Result<LinkFlip, NetError> {
        if !self.links.contains_key(&link_key(a, b)) {
            self.cut_lan(a, b, true)?;
            return Ok(LinkFlip::Lan {
                lan: self.lan.clone(),
                a: a.to_owned(),
                b: b.to_owned(),
            });
        }
        let ends = (
            self.edge(a)?,
            self.edge(b)?,
            self.host(a)?.host_id,
            self.host(b)?.host_id,
        );
        self.sever_link(a, b)?;
        Ok(LinkFlip::InProcess {
            a: Arc::downgrade(&ends.0),
            b: Arc::downgrade(&ends.1),
            id_a: ends.2,
            id_b: ends.3,
            live: None,
        })
    }

    /// Links the two hosts again, if both are up, once both ends have seen
    /// the severed link go. An end still holding it would refuse the new
    /// link as a second dial from the same process, and an in-process link
    /// is never dialled again.
    pub async fn restore_link(&mut self, a: &str, b: &str) -> Result<Ack, NetError> {
        if !self.links.contains_key(&link_key(a, b)) {
            return self.cut_lan(a, b, false);
        }
        let severed = self
            .links
            .get(&link_key(a, b))
            .ok_or_else(|| NetError::NoLink(a.to_owned(), b.to_owned()))?
            .live
            .is_none();
        if severed {
            self.wait_direct_gone(a, b).await?;
        }
        self.links
            .get_mut(&link_key(a, b))
            .expect("the link")
            .severed = false;
        self.link_all()?;
        Ok(self.ack(format!("link {a} - {b} restored")))
    }

    /// Cuts the LAN between two hosts that are not linked in process, or
    /// lets it through again: every gate between them, now and any made
    /// later, so a pairing however it was made goes down as a dead
    /// connection does.
    fn cut_lan(&mut self, a: &str, b: &str, cut: bool) -> Result<Ack, NetError> {
        self.host(a)?;
        self.host(b)?;
        // Only on a gated LAN does every route between two hosts run
        // through the net's gates: elsewhere a host that found its peer by
        // discovery dials the peer's own listener, which no gate stands in
        // front of.
        if a == b || !self.gated_lan {
            return Err(NetError::NoLink(a.to_owned(), b.to_owned()));
        }
        let gates = self.lan.cut(a, b, cut);
        let what = if cut { "severed" } else { "restored" };
        Ok(self.ack(if gates == 0 {
            format!(
                "LAN {a} - {b} {what}, though no route between them runs through the net's \
                 gates yet: one they find later will"
            )
        } else {
            format!("LAN {a} - {b} {what} at {gates} gates")
        }))
    }

    /// Announces a process host with a LAN listener on the net's discovery
    /// bus, as its own daemon would if it could reach the bus: the bus lives
    /// in the net's process, and the daemon in its own. A daemon on a real
    /// LAN advertises whenever it listens, so this does not wait for the
    /// host to be declared with discovery; the hosts in the net's process
    /// that browse the bus then find it, pair with it by name, and dial it
    /// again after it restarts.
    fn advertise_process(&self, name: &str) {
        let Some(host) = self.hosts.get(name) else {
            return;
        };
        if host.process.is_none() || !host.decl.lan {
            return;
        }
        let Ok(port) = self.lan_port(name) else {
            tracing::warn!(host = name, "a process host has no LAN port to announce");
            return;
        };
        self.bus.announce(node::discovery::Advertisement {
            host_id: host.info.host_id,
            name: name.to_owned(),
            version: node::PROTOCOL_VERSION,
            addrs: vec![SocketAddr::from(([127, 0, 0, 1], port))],
            scope: host
                .decl
                .scope
                .clone()
                .unwrap_or_else(|| self.topology.scope.clone()),
        });
    }

    /// Withdraws what [`Net::advertise_process`] announced, as a daemon's
    /// goodbye would.
    fn withdraw_process(&self, name: &str) {
        if let Some(host) = self.hosts.get(name)
            && host.process.is_some()
            && host.decl.lan
        {
            self.bus.withdraw_host(host.info.host_id);
        }
    }

    /// The host ids the net's discovery bus advertises right now, as a
    /// browser starting now would learn them.
    pub fn advertised(&self) -> Vec<Uuid> {
        use node::discovery::{Discovery as _, DiscoveryEvent};
        let mut events = self.bus.browse();
        self.bus.requery();
        let mut ids = Vec::new();
        while let Ok(event) = events.try_recv() {
            if let DiscoveryEvent::Found(advert) = event {
                ids.push(advert.host_id);
            }
        }
        ids
    }

    /// The gate `from` dials to reach `to`'s LAN listener, made the first
    /// time it is asked for: give it to a pairing in place of `to`'s own
    /// address, and the pair can be cut and degraded there.
    pub fn pair_gate(&self, from: &str, to: &str) -> Result<SocketAddr, NetError> {
        self.host(from)?;
        let port = self.lan_port(to)?;
        Ok(self
            .lan
            .pair(from, to, SocketAddr::from(([127, 0, 0, 1], port)))?)
    }

    /// The port `host`'s LAN listener answers on, as its profile records.
    fn lan_port(&self, host: &str) -> Result<u16, NetError> {
        let info = self.host(host)?;
        std::fs::read_to_string(
            node::profile_dir(&info.data_dir, info.profile).join(node::LAN_PORT_FILE),
        )
        .ok()
        .and_then(|text| text.trim().parse::<u16>().ok())
        .ok_or_else(|| NetError::Host {
            host: host.to_owned(),
            error: "has no LAN listener".to_owned(),
        })
    }

    /// Makes `target`'s network bad, a host's or the relay's: every gate a
    /// host's datagrams cross (its way to the relay, its LAN pairings, its
    /// front gate for clients outside the net) or every way to the relay.
    /// A link made in process carries no datagrams and is untouched.
    pub fn set_net_faults(&mut self, target: &str, faults: Faults) -> Result<Ack, NetError> {
        let gates = if target == "relay" {
            self.relay()?;
            self.relay_faults = faults;
            if let Some(gate) = &self.relay_gate {
                gate.set_faults(faults);
            }
            for (host, gate) in &self.udp {
                gate.set_faults(faults.then(self.lan.faults(host)));
            }
            self.udp.len() + usize::from(self.relay_gate.is_some())
        } else {
            self.host(target)?;
            let mut gates = self.lan.set_faults(target, faults);
            if let Some(gate) = self.udp.get(target) {
                gate.set_faults(self.relay_faults.then(faults));
                gates += 1;
            }
            gates
        };
        Ok(self.ack(format!(
            "{target}'s network: {} ({gates} gates)",
            faults.describe()
        )))
    }

    /// Links every declared pair that is not severed, not live and has both
    /// ends up.
    fn link_all(&mut self) -> Result<(), NetError> {
        let wanted: Vec<(String, String)> = self
            .links
            .iter()
            .filter(|((a, b), link)| {
                !link.severed && link.live.is_none() && self.is_up(a) && self.is_up(b)
            })
            .map(|(key, _)| key.clone())
            .collect();
        for (a, b) in wanted {
            let live = self
                .edge(&a)?
                .link_in_process(&self.edge(&b)?)
                .map_err(|error| NetError::Host {
                    host: a.clone(),
                    error,
                })?;
            self.links.get_mut(&(a, b)).expect("the link").live = Some(std::sync::Mutex::new(live));
        }
        Ok(())
    }

    /// Waits until the LAN port `name`'s last daemon listened on is free
    /// again, so the next daemon listens where it did and every gate and
    /// stored address still reaches it. A dead daemon's sockets die with its
    /// process; in process, the dropped listener lets go of its port only
    /// once its last connection's task has, which a client outside the net
    /// (the phone) can hold until its connection times out. A socket a test
    /// handed in is the test's to hold or let go.
    async fn wait_lan_port(&self, name: &str) -> Result<(), NetError> {
        let host = &self.hosts[name];
        let port = std::fs::read_to_string(
            node::profile_dir(&host.info.data_dir, host.info.profile).join(node::LAN_PORT_FILE),
        )
        .ok()
        .and_then(|text| text.trim().parse::<u16>().ok())
        .filter(|_| host.decl.lan && !host.lan_handed);
        if let Some(port) = port {
            observe::until_within(
                &format!("{name}'s LAN port to be free"),
                LAN_PORT_PATIENCE,
                || {
                    let bound = std::net::UdpSocket::bind((loopback(&host.decl), port));
                    async move { bound.map(|_| ()).map_err(|error| error.to_string()) }
                },
            )
            .await?;
        }
        Ok(())
    }

    /// Crashes a host's daemon. Its agent processes keep running, in their
    /// grace, waiting for the next daemon.
    pub async fn kill_daemon(&mut self, host: &str) -> Result<Ack, NetError> {
        if let Some(process) = self.process(host)?.cloned() {
            if !self.is_up(host) {
                return Err(NetError::Down(host.to_owned()));
            }
            let pid = process.kill().map_err(|why| NetError::Process {
                host: host.to_owned(),
                why,
            })?;
            process.wait_down().await.map_err(|why| NetError::Process {
                host: host.to_owned(),
                why,
            })?;
            let supervised = self.hosts[host].decl.supervised;
            // A supervisor starts its daemon again on its own, as it does
            // after any crash; a bare daemon stays down.
            self.hosts.get_mut(host).expect("the host").running = supervised;
            if !supervised {
                self.withdraw_process(host);
            }
            return Ok(self.ack(format!(
                "{host}'s daemon (pid {pid}) killed{}",
                if supervised {
                    "; its supervisor will start it again"
                } else {
                    ""
                }
            )));
        }
        self.take_down(host, false).await?;
        Ok(self.ack(format!("{host}'s daemon killed")))
    }

    /// Shuts a host's daemon down cleanly: flushed, marked clean. Its agent
    /// processes keep running.
    pub async fn stop_daemon(&mut self, host: &str) -> Result<Ack, NetError> {
        if let Some(process) = self.process(host)?.cloned() {
            if !self.is_up(host) {
                return Err(NetError::Down(host.to_owned()));
            }
            process.stop().await.map_err(|why| NetError::Process {
                host: host.to_owned(),
                why,
            })?;
            self.hosts.get_mut(host).expect("the host").running = false;
            self.withdraw_process(host);
            return Ok(self.ack(format!("{host}'s daemon stopped with amux server stop")));
        }
        self.take_down(host, true).await?;
        Ok(self.ack(format!("{host}'s daemon stopped")))
    }

    /// Starts a host's daemon again from what its installation holds, under
    /// the same boot, and relinks its declared links.
    /// Gives a stopped host's installation a profile under `id`, another
    /// machine's, in place of its own: a machine starting under an id its
    /// peers already know, with a key of its own.
    pub fn replace_profile(&mut self, host: &str, id: ProfileId) -> Result<(), NetError> {
        let info = &mut self
            .hosts
            .get_mut(host)
            .ok_or_else(|| NetError::NoHost(host.to_owned()))?
            .info;
        std::fs::remove_dir_all(node::profile_dir(&info.data_dir, info.profile))?;
        node::create_profile_as(&info.data_dir, id)?;
        info.profile = id;
        info.host_id = id;
        Ok(())
    }

    pub async fn restart_daemon(&mut self, host: &str) -> Result<Ack, NetError> {
        if let Some(process) = self.process(host)?.cloned() {
            // A supervised host's daemon may already be back under its
            // supervisor; `amux server start` then says so and waits for it.
            process.start().await.map_err(|error| NetError::Start {
                host: host.to_owned(),
                error,
            })?;
            self.hosts.get_mut(host).expect("the host").running = true;
            self.advertise_process(host);
            return Ok(self.ack(format!("{host}'s daemon started with amux server start")));
        }
        self.start_host(host).await?;
        self.link_all()?;
        Ok(self.ack(format!("{host}'s daemon started")))
    }

    /// Records what has reached the drive now: the store flushed and its
    /// file as it stands. [`Net::rewind_host`] loses power back to here.
    pub async fn checkpoint_host(&mut self, host: &str) -> Result<Ack, NetError> {
        self.in_process(host, "a checkpoint of what reached the drive")?;
        let runtime = self.runtime(host)?;
        // Read on the core thread, so nothing commits between the flush and
        // the read.
        let path = runtime.dir().join(node::STORE);
        let bytes = runtime
            .read_store(move |store| {
                store.flush_to_drive().map_err(|error| error.to_string())?;
                std::fs::read(path).map_err(|error| error.to_string())
            })
            .await
            .map_err(|error| error.to_string())
            .flatten()
            .map_err(|error| NetError::Host {
                host: host.to_owned(),
                error,
            })?;
        drop(runtime);
        self.hosts.get_mut(host).expect("the host").checkpoint = Some(bytes);
        Ok(self.ack(format!("{host} checkpointed")))
    }

    /// Power loss without power: the daemon and every agent process on the
    /// host die at once; the store goes back to the last checkpoint with
    /// no WAL, each named journal is cut at its byte, and the machine comes
    /// back under a new boot id with the clean flag never set.
    pub async fn rewind_host(&mut self, host: &str, cuts: &[JournalCut]) -> Result<Ack, NetError> {
        self.in_process(host, "losing power back to a checkpoint")?;
        let checkpoint = self
            .hosts
            .get(host)
            .ok_or_else(|| NetError::NoHost(host.to_owned()))?
            .checkpoint
            .clone()
            .ok_or_else(|| NetError::NoCheckpoint(host.to_owned()))?;
        let mut journals = Vec::new();
        for cut in cuts {
            if self.agent(&cut.agent)?.host != host {
                return Err(NetError::Host {
                    host: host.to_owned(),
                    error: format!("agent {} runs elsewhere", cut.agent),
                });
            }
            journals.push((
                self.agent_dir(&cut.agent)?.join(agent_dir::JOURNAL),
                cut.byte,
            ));
        }
        if self.is_up(host) {
            self.take_down(host, false).await?;
        }
        let info = self.host(host)?.clone();
        let profile_dir = node::profile_dir(&info.data_dir, info.profile);
        kill_agents_under(&profile_dir.join(node::AGENTS)).await?;

        let db = profile_dir.join(node::STORE);
        std::fs::write(&db, checkpoint)?;
        for suffix in ["-wal", "-shm"] {
            let mut path = db.clone().into_os_string();
            path.push(suffix);
            match std::fs::remove_file(path) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                    return Err(error.into());
                }
                _ => {}
            }
        }
        for (journal, byte) in journals {
            journal::synthetic::cut(&journal, byte)?;
        }
        self.hosts.get_mut(host).expect("the host").boot += 1;
        self.start_host(host).await?;
        self.link_all()?;
        Ok(self.ack(format!("{host} lost power and came back")))
    }

    // --- observations ------------------------------------------------------

    /// Opens a Subscribe stream on `agent` at `host`, which may be its own
    /// host or one holding its replica, and records it.
    pub async fn observe(&self, host: &str, agent: &str, tail: u32) -> Result<Observer, NetError> {
        let id = self.agent(agent)?.id;
        self.observe_id(host, agent, id, tail).await
    }

    /// [`Net::observe`] for any agent the host holds, declared or not:
    /// one a client created is known only by its id.
    pub async fn observe_id(
        &self,
        host: &str,
        agent: &str,
        id: Uuid,
        tail: u32,
    ) -> Result<Observer, NetError> {
        if let Some(process) = self.process(host)?.cloned() {
            let mut stream = process
                .client()
                .await
                .map_err(|why| NetError::Process {
                    host: host.to_owned(),
                    why,
                })?
                .subscribe_session(wire::SubscribeSessionRequest {
                    agent_id: id.as_bytes().to_vec(),
                    tail,
                })
                .await
                .map_err(|status| NetError::Refused(Box::new(status)))?
                .into_inner();
            let (sender, incoming) = mpsc::unbounded_channel();
            let reader = tokio::spawn(async move {
                while let Ok(Some(event)) = stream.message().await {
                    if sender.send(event).is_err() {
                        return;
                    }
                }
            });
            return Ok(ObserverOf::new(
                format!("{agent} observed at {host}"),
                incoming,
                reader,
            ));
        }
        let mut subscription = self
            .runtime(host)?
            .subscribe_session(id.as_bytes(), tail)
            .await?;
        let (sender, incoming) = mpsc::unbounded_channel();
        let reader = tokio::spawn(async move {
            while let Some(event) = subscription.next().await {
                if sender.send((*event).clone()).is_err() {
                    return;
                }
            }
        });
        Ok(ObserverOf::new(
            format!("{agent} observed at {host}"),
            incoming,
            reader,
        ))
    }

    /// Opens a SubscribeInventory stream on `host` and records it.
    pub async fn observe_inventory(&self, host: &str) -> Result<InventoryObserver, NetError> {
        if let Some(process) = self.process(host)?.cloned() {
            let mut stream = process
                .client()
                .await
                .map_err(|why| NetError::Process {
                    host: host.to_owned(),
                    why,
                })?
                .subscribe_inventory(wire::SubscribeInventoryRequest {})
                .await
                .map_err(|status| NetError::Refused(Box::new(status)))?
                .into_inner();
            let (sender, incoming) = mpsc::unbounded_channel();
            let reader = tokio::spawn(async move {
                while let Ok(Some(event)) = stream.message().await {
                    if sender.send(event).is_err() {
                        return;
                    }
                }
            });
            return Ok(ObserverOf::new(
                format!("inventory at {host}"),
                incoming,
                reader,
            ));
        }
        let mut subscription = self.runtime(host)?.subscribe_inventory().await?;
        let (sender, incoming) = mpsc::unbounded_channel();
        let reader = tokio::spawn(async move {
            while let Some(event) = subscription.next().await {
                if sender.send((*event).clone()).is_err() {
                    return;
                }
            }
        });
        Ok(ObserverOf::new(
            format!("inventory at {host}"),
            incoming,
            reader,
        ))
    }

    /// The bytes an in-process link between `a` and `b` has carried, both
    /// ways, since it was last dialled: what crossing between the hosts
    /// costs on the wire. None while the link is down or not in process.
    pub fn link_carried(&self, a: &str, b: &str) -> Option<u64> {
        let link = self.links.get(&link_key(a, b))?;
        let live = link.live.as_ref()?;
        Some(live.lock().unwrap().carried())
    }

    /// Whether both ends of a link route to each other directly now.
    pub async fn link_up(&self, a: &str, b: &str) -> bool {
        if matches!(self.process(a), Ok(Some(_))) || matches!(self.process(b), Ok(Some(_))) {
            return self.sees_direct(a, b).await && self.sees_direct(b, a).await;
        }
        let (Ok(edge_a), Ok(edge_b), Ok(info_a), Ok(info_b)) =
            (self.edge(a), self.edge(b), self.host(a), self.host(b))
        else {
            return false;
        };
        edge_a.via(info_b.host_id).await == LiveRoute::Direct
            && edge_b.via(info_a.host_id).await == LiveRoute::Direct
    }

    /// Whether `near`'s inventory lists `far` online over a direct route,
    /// once it has caught up: what a process host's link looks like from
    /// outside it.
    async fn sees_direct(&self, near: &str, far: &str) -> bool {
        let (Ok(far_id), Ok(mut inventory)) = (
            self.host(far).map(|info| info.host_id),
            self.observe_inventory(near).await,
        ) else {
            return false;
        };
        let Ok(events) = inventory
            .observe_until(observe::inventory_caught_up, PATIENCE)
            .await
        else {
            return false;
        };
        observe::inventory_hosts(events).iter().any(|host| {
            host.host_id() == far_id.as_bytes() && host.online() == Some(wire::RouteKind::Direct)
        })
    }

    /// Waits until the link between two hosts is up, or down on both ends.
    pub async fn wait_link(&self, a: &str, b: &str, up: bool) -> Result<(), NetError> {
        let (id_a, id_b) = (self.host(a)?.host_id, self.host(b)?.host_id);
        let what = format!("link {a} - {b} {}", if up { "up" } else { "down" });
        let wanted = if up {
            LiveRoute::Direct
        } else {
            LiveRoute::Offline
        };
        observe::until(&what, || async {
            let via_ab = match self.edge(a) {
                Ok(edge) => edge.via(id_b).await,
                Err(_) => LiveRoute::Offline,
            };
            let via_ba = match self.edge(b) {
                Ok(edge) => edge.via(id_a).await,
                Err(_) => LiveRoute::Offline,
            };
            (via_ab == wanted && via_ba == wanted)
                .then_some(())
                .ok_or_else(|| format!("{a} -> {b} {via_ab:?}, {b} -> {a} {via_ba:?}"))
        })
        .await?;
        Ok(())
    }

    /// Waits until neither end of a link routes to the other over it. Unlike
    /// the link being down, this holds while a third host still reaches
    /// across.
    async fn wait_direct_gone(&self, a: &str, b: &str) -> Result<(), NetError> {
        let (id_a, id_b) = (self.host(a)?.host_id, self.host(b)?.host_id);
        let what = format!("the severed link {a} - {b} to go at both ends");
        observe::until(&what, || async {
            for (near, far, name) in [(a, id_b, a), (b, id_a, b)] {
                if let Ok(edge) = self.edge(near)
                    && edge.via(far).await == LiveRoute::Direct
                {
                    return Err(format!("{name} still routes direct"));
                }
            }
            Ok(())
        })
        .await?;
        Ok(())
    }

    /// Checks the replica block invariant for `agent` at `host` against
    /// its origin, at a settled moment: `host`'s rows for it are empty or
    /// one contiguous block ending at the origin's newest row, at the
    /// origin's revisions.
    pub async fn assert_block_invariant(&self, host: &str, agent: &str) -> Result<(), NetError> {
        self.in_process(host, "the block invariant reads both stores and")?;
        self.in_process(
            &self.agent(agent)?.host,
            "the block invariant reads both stores and",
        )?;
        let agent = self.agent(agent)?.clone();
        let key = agent.key();
        let block = |host: String| {
            let key = key.clone();
            async move {
                let runtime = self.runtime(&host)?;
                runtime
                    .read_store(move |store| invariant::block(store, &key))
                    .await
                    .map_err(|error| error.to_string())
                    .flatten()
                    .map_err(|error| NetError::Host { host, error })
            }
        };
        let replica = block(host.to_owned()).await?;
        let origin = block(agent.host.clone()).await?;
        invariant::check(&replica, &origin)?;
        Ok(())
    }

    // --- fences ------------------------------------------------------------
    //
    // A test asserts about a host only after the host's own cursors say
    // the step it triggered has landed there. These waits name the
    // condition in the runtime's terms and wake on the store write that
    // satisfies it, so nothing is inferred from time or from a proxy.

    /// Waits on `host`'s store, reading it again after every write, until
    /// `probe` answers what it found. Each `Err` is what the probe saw
    /// instead, and the last one is reported if [`PATIENCE`] passes.
    async fn on_write<T>(
        &self,
        host: &str,
        what: &str,
        patience: Duration,
        probe: impl Fn(&store::Sqlite) -> Result<T, String> + Send + Sync + 'static,
    ) -> Result<T, NetError>
    where
        T: Send + 'static,
    {
        let probe = std::sync::Arc::new(probe);
        let runtime = self.runtime(host)?;
        let mut writes = runtime.committed();
        let deadline = tokio::time::Instant::now() + patience;
        loop {
            // Marked seen before the read, so a write landing between the
            // read and the wait is a change the wait returns on.
            writes.borrow_and_update();
            let probe = probe.clone();
            let read = runtime.read_store(move |store| probe(store));
            let seen = match tokio::time::timeout_at(deadline, read).await {
                Ok(Ok(Ok(found))) => return Ok(found),
                Ok(Ok(Err(seen))) => seen,
                Ok(Err(stopped)) => {
                    return Err(Stuck::Closed {
                        what: what.to_owned(),
                        seen: stopped.to_string(),
                    }
                    .into());
                }
                Err(_) => {
                    return Err(Stuck::Hung {
                        what: what.to_owned(),
                        waited: patience,
                    }
                    .into());
                }
            };
            match tokio::time::timeout_at(deadline, writes.changed()).await {
                Ok(Ok(())) => {}
                Ok(Err(_)) => {
                    return Err(Stuck::Closed {
                        what: what.to_owned(),
                        seen,
                    }
                    .into());
                }
                Err(_) => {
                    return Err(Stuck::Deadline {
                        what: what.to_owned(),
                        waited: patience,
                        seen,
                    }
                    .into());
                }
            }
        }
    }

    /// [`Net::on_write`] for a process host, whose store the net cannot
    /// read: follows the agent's rows as a client does, on a Subscribe
    /// stream, filling in what its opening tail left out with Fetch, and
    /// asks `probe` again on every row. Each row arrives once, so this
    /// needs no polling; a stream that ends is opened again. Subscribing
    /// on a host that only holds a replica can start replicating, which
    /// the in-process wait does not.
    async fn on_rows<T>(
        &self,
        host: &str,
        agent: &NetAgent,
        what: &str,
        patience: Duration,
        mut probe: impl FnMut(&[wire::Item]) -> Result<T, String>,
    ) -> Result<T, NetError> {
        let process = self.process(host)?.cloned().expect("a process host");
        let deadline = tokio::time::Instant::now() + patience;
        let failed = |why: String| NetError::Process {
            host: host.to_owned(),
            why,
        };
        let mut seen = String::from("nothing yet");
        loop {
            let mut client = process.client().await.map_err(failed)?;
            let mut rows: BTreeMap<String, wire::Item> = BTreeMap::new();
            let mut stream = match tokio::time::timeout_at(
                deadline,
                client.subscribe_session(wire::SubscribeSessionRequest {
                    agent_id: agent.id.as_bytes().to_vec(),
                    tail: 200,
                }),
            )
            .await
            {
                Ok(Ok(stream)) => stream.into_inner(),
                Ok(Err(status)) => return Err(NetError::Refused(Box::new(status))),
                Err(_) => {
                    return Err(Stuck::Deadline {
                        what: what.to_owned(),
                        waited: patience,
                        seen,
                    }
                    .into());
                }
            };
            loop {
                let event = match tokio::time::timeout_at(deadline, stream.message()).await {
                    Ok(Ok(Some(event))) => event,
                    // The stream lagged, reset or closed: open it again and
                    // look afresh, which only moves forward.
                    Ok(_) => break,
                    Err(_) => {
                        return Err(Stuck::Deadline {
                            what: what.to_owned(),
                            waited: patience,
                            seen,
                        }
                        .into());
                    }
                };
                match event.of {
                    Some(wire::session_event::Of::Item(item)) => {
                        rows.insert(item.key.clone(), item);
                    }
                    Some(wire::session_event::Of::CaughtUp(_)) => {
                        // The opening is capped: everything older comes
                        // page by page.
                        let mut before = rows.values().map(|item| item.order).min();
                        while let Some(order) = before.filter(|order| *order > 1) {
                            let page = client
                                .list_items(wire::ListItemsRequest {
                                    agent_id: agent.id.as_bytes().to_vec(),
                                    before_order: Some(order),
                                    page_size: 500,
                                })
                                .await
                                .map_err(|status| NetError::Refused(Box::new(status)))?
                                .into_inner();
                            before = page.items.iter().map(|item| item.order).min();
                            for item in page.items {
                                rows.entry(item.key.clone()).or_insert(item);
                            }
                            if page.exhausted {
                                break;
                            }
                        }
                    }
                    _ => continue,
                }
                let mut ordered: Vec<wire::Item> = rows.values().cloned().collect();
                ordered.sort_by_key(|item| item.order);
                match probe(&ordered) {
                    Ok(found) => return Ok(found),
                    Err(now) => seen = now,
                }
            }
        }
    }

    /// How long to wait on `agent`: a real provider's turn takes far longer
    /// than a script's.
    fn patience_for(&self, agent: &NetAgent) -> Duration {
        match agent.provider {
            Provider::Real => REAL_PATIENCE,
            Provider::Fake => PATIENCE,
        }
    }

    /// Waits until `host` holds `agent`'s row at `order`: its newest row
    /// is that one or a later one. After it, what the host holds through
    /// `order` is what its store reports, on the origin and on a replica
    /// alike.
    pub async fn fence(&self, host: &str, agent: &str, order: u64) -> Result<(), NetError> {
        let at = self.agent(agent)?.clone();
        let key = at.key();
        let what = format!("{host} to hold {agent}'s row at order {order}");
        let patience = self.patience_for(&at);
        if self.process(host)?.is_some() {
            return self
                .on_rows(host, &at, &what, patience, |rows| {
                    match rows.last().map(|item| item.order) {
                        Some(held) if held >= order => Ok(()),
                        Some(held) => Err(format!("newest order {held}")),
                        None => Err("no rows".to_owned()),
                    }
                })
                .await;
        }
        self.on_write(host, &what, patience, move |store| {
            probes::holds_order(store, &key, order)
        })
        .await
    }

    /// Waits until the `k`th turn of `agent` (counting from one) has
    /// ended on its own host, and returns the order of the row that ended
    /// it, so another host can be fenced to it. Counts the turn-end rows
    /// in the block the host holds.
    pub async fn turn_ended(&self, agent: &str, k: usize) -> Result<u64, NetError> {
        assert!(k >= 1, "turns count from one");
        let at = self.agent(agent)?.clone();
        let key = at.key();
        let what = format!("{agent}'s turn {k} to end");
        let patience = self.patience_for(&at);
        if self.process(&at.host)?.is_some() {
            return self
                .on_rows(&at.host, &at, &what, patience, |rows| {
                    let ends: Vec<u64> = rows
                        .iter()
                        .filter(|item| item.key.starts_with("turn:"))
                        .map(|item| item.order)
                        .collect();
                    ends.get(k - 1)
                        .copied()
                        .ok_or_else(|| format!("{} turns ended", ends.len()))
                })
                .await;
        }
        self.on_write(&at.host, &what, patience, move |store| {
            probes::turn_end(store, &key, k)
        })
        .await
    }

    /// Waits until the input `agent` accepted under `input_id` has
    /// settled on its own host: the row that reflects it exists. Returns
    /// that row's order. A rejected input never settles; rejection is
    /// the answer `send` and `input` return.
    pub async fn input_settled(&self, agent: &str, input_id: &[u8]) -> Result<u64, NetError> {
        let patience = self.patience_of(agent)?;
        self.input_settled_within(agent, input_id, patience).await
    }

    /// How long a wait on `agent` allows by default: longer for a real
    /// provider's.
    pub fn patience_of(&self, agent: &str) -> Result<Duration, NetError> {
        Ok(self.patience_for(self.agent(agent)?))
    }

    /// [`Net::input_settled`] with its own deadline.
    pub async fn input_settled_within(
        &self,
        agent: &str,
        input_id: &[u8],
        patience: Duration,
    ) -> Result<u64, NetError> {
        let at = self.agent(agent)?.clone();
        let key = at.key();
        let what = format!("{agent}'s input {} to settle", hex(input_id));
        if self.process(&at.host)?.is_some() {
            return self
                .on_rows(&at.host, &at, &what, patience, |rows| {
                    rows.iter()
                        .find(|item| item.input_id == input_id)
                        .map(|item| item.order)
                        .ok_or_else(|| "no row carries it".to_owned())
                })
                .await;
        }
        let input_id = input_id.to_vec();
        self.on_write(&at.host, &what, patience, move |store| {
            probes::settled(store, &key, &input_id)
        })
        .await
    }

    /// Waits until `host` holds what the origin holds for `agent`: caught
    /// up through the origin's newest revision, with the block intact.
    /// Reads both hosts, so it looks again every poll rather than waking
    /// on one host's writes.
    pub async fn current(&self, host: &str, agent: &str) -> Result<(), NetError> {
        let at = self.agent(agent)?.clone();
        let key = at.key();
        let what = format!("{agent}'s replica at {host} to be current");
        observe::until(&what, || async {
            let newest = {
                let runtime = self.runtime(&at.host).map_err(|error| error.to_string())?;
                let key = key.clone();
                runtime
                    .read_store(move |store| probes::origin_newest(store, &key))
                    .await
                    .map_err(|error| error.to_string())??
            };
            {
                let runtime = self.runtime(host).map_err(|error| error.to_string())?;
                let key = key.clone();
                runtime
                    .read_store(move |store| probes::replica_current(store, &key, newest))
                    .await
                    .map_err(|error| error.to_string())??;
            }
            self.assert_block_invariant(host, agent)
                .await
                .map_err(|error| error.to_string())
        })
        .await?;
        Ok(())
    }

    // --- releases ----------------------------------------------------------

    /// The release feed's address, for a supervised host's config; a
    /// discard address when no host is supervised.
    fn feed_url(&self) -> String {
        self.feed
            .as_ref()
            .map_or_else(|| "http://127.0.0.1:9".to_owned(), Feed::url)
    }

    /// Gives a supervised host its own copy of the build under test,
    /// stamped as an old version so any release published later is newer.
    /// A copy, never a link: an update swaps files beside it.
    fn install_supervised(&self, host: &str) -> Result<(), NetError> {
        let install = self.hosts[host].dir.join("install");
        std::fs::create_dir_all(&install)?;
        let amux = install.join("amux");
        node::release::restamp(&self.binaries.amux(), &amux, SUPERVISED_VERSION)?;
        crate::feed::warm(&amux);
        Ok(())
    }

    /// Publishes the build under test as `version` on `channel`'s feed, to
    /// `rollout` percent of hosts (all when none), or a build that exits at
    /// once when `broken`, and has every supervised host check now rather
    /// than at its hourly check. Answers what each host's supervisor said.
    pub async fn publish_release(
        &self,
        version: &str,
        channel: &str,
        rollout: Option<u8>,
        broken: bool,
    ) -> Result<Ack, NetError> {
        let feed = self.feed.as_ref().ok_or_else(|| NetError::Host {
            host: "feed".to_owned(),
            error: "no host is supervised, so nothing takes releases".to_owned(),
        })?;
        feed.publish(&self.binaries.amux(), version, channel, rollout, broken)
            .map_err(|error| NetError::Host {
                host: "feed".to_owned(),
                error,
            })?;
        let mut answers = Vec::new();
        for (name, host) in &self.hosts {
            if !host.decl.supervised || !host.running {
                continue;
            }
            let data_dir = host.info.data_dir.clone();
            let answer = observe::until(&format!("{name}'s supervisor to take a check"), || {
                let data_dir = data_dir.clone();
                async move {
                    let said = node::supervisor::ask(
                        &data_dir,
                        node::supervisor::Request::Check,
                        Duration::from_secs(20),
                    )
                    .await
                    .map_err(|error| error.to_string())?;
                    // Busy installing or starting: it says to try again.
                    if said.contains("try again") {
                        Err(said)
                    } else {
                        Ok(said)
                    }
                }
            })
            .await?;
            answers.push(format!("{name}: {answer}"));
        }
        Ok(self.ack(format!(
            "{} {version} published on {channel}{}; {}",
            if broken { "a broken" } else { "build" },
            rollout.map(|n| format!(" to {n}%")).unwrap_or_default(),
            if answers.is_empty() {
                "no supervised host is up to check".to_owned()
            } else {
                answers.join("; ")
            }
        )))
    }

    // --- teardown ----------------------------------------------------------

    /// Stops every agent and shuts every daemon down cleanly. The explicit
    /// end of a net; dropping one kills what it can and is for panics.
    pub async fn shutdown(mut self) -> Result<(), NetError> {
        let names: Vec<String> = self.hosts.keys().cloned().collect();
        for name in &names {
            if let Ok(runtime) = self.runtime(name) {
                for id in runtime.live() {
                    let _ = tokio::time::timeout(PATIENCE, runtime.kill_agent(id)).await;
                }
            }
        }
        for name in &names {
            if !self.is_up(name) {
                continue;
            }
            match self.hosts[name].process.clone() {
                Some(process) => {
                    let _ = process.stop().await;
                    // A supervised host whose daemon was killed may still be
                    // starting it again; its supervisor goes with the stop.
                    if let Some(pid) = process.daemon_pid() {
                        let _ = crate::process::signal(pid, "-KILL");
                    }
                }
                None => self.take_down(name, true).await?,
            }
        }
        // Agents of hosts that were down at the end have no daemon to
        // stop them.
        kill_agents_under(self.root.path()).await?;
        Ok(())
    }
}

impl Drop for Net {
    fn drop(&mut self) {
        for host in self.hosts.values_mut() {
            drop(host.daemon.take());
        }
        #[cfg(unix)]
        let _ = std::process::Command::new("pkill")
            .args(["-KILL", "-f"])
            .arg(self.root.path())
            .status();
    }
}

/// A channel to a local socket: a front door or a profile's client socket.
pub async fn local_channel(
    path: &Path,
) -> Result<tonic::transport::Channel, tonic::transport::Error> {
    let path = path.to_owned();
    tonic::transport::Endpoint::from_static("http://amux.test")
        .connect_with_connector(tower::service_fn(move |_| {
            let path = path.clone();
            async move {
                agent_dir::local_socket::connect(&path)
                    .await
                    .map(hyper_util::rt::TokioIo::new)
            }
        }))
        .await
}

/// A first prompt or a message in the kind's own input arm.
/// A first prompt for a create or a resume.
pub fn submission(id: &[u8], text: &str) -> wire::PromptSubmission {
    wire::PromptSubmission {
        input_id: id.to_vec(),
        prompt: Some(PromptInput {
            text: text.to_owned(),
            attachments: Vec::new(),
        }),
    }
}

pub fn prompt(kind: FakeKind, id: &[u8], text: &str) -> Input {
    let prompt = PromptInput {
        text: text.to_owned(),
        attachments: Vec::new(),
    };
    Input::new(
        id.to_vec(),
        match kind {
            FakeKind::ClaudePty => wire::KindInput::ClaudePty(ClaudePtyInput {
                of: Some(claude_pty_input::Of::Prompt(prompt)),
            }),
            FakeKind::ClaudeSdk => wire::KindInput::ClaudeSdk(ClaudeSdkInput {
                of: Some(claude_sdk_input::Of::Prompt(prompt)),
            }),
            FakeKind::Codex => wire::KindInput::Codex(CodexInput {
                of: Some(codex_input::Of::Prompt(prompt)),
            }),
        },
    )
}

/// Withdraws a queued prompt, in the kind's own input arm.
pub fn withdraw(kind: FakeKind, id: &[u8], queued: &[u8]) -> Input {
    let withdraw = WithdrawQueued {
        queued_input_id: queued.to_vec(),
    };
    Input::new(
        id.to_vec(),
        match kind {
            FakeKind::ClaudePty => wire::KindInput::ClaudePty(ClaudePtyInput {
                of: Some(claude_pty_input::Of::Withdraw(withdraw)),
            }),
            FakeKind::ClaudeSdk => wire::KindInput::ClaudeSdk(ClaudeSdkInput {
                of: Some(claude_sdk_input::Of::Withdraw(withdraw)),
            }),
            FakeKind::Codex => wire::KindInput::Codex(CodexInput {
                of: Some(codex_input::Of::Withdraw(withdraw)),
            }),
        },
    )
}

/// Kills every process whose command line names `dir` until none is left:
/// a supervisor killed a moment after its daemon may have started another.
async fn kill_all_under(host: &str, dir: &Path) -> Result<(), NetError> {
    #[cfg(unix)]
    {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            kill_pids(&processes_under(dir));
            tokio::time::sleep(Duration::from_millis(200)).await;
            let left = processes_under(dir);
            if left.is_empty() {
                return Ok(());
            }
            if tokio::time::Instant::now() > deadline {
                return Err(NetError::Process {
                    host: host.to_owned(),
                    why: format!("processes of the host would not die: {left:?}"),
                });
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (host, dir);
        Err(NetError::Unsupported("killing a host's processes"))
    }
}

/// The processes whose command line names `dir` or a path below it, as a
/// whole path: `desk` must not catch `desk2`. The net's tmux server is left
/// out, though its command line names the folder its first pane started
/// in: it serves every host's panes.
#[cfg(unix)]
fn processes_under(dir: &Path) -> Vec<u32> {
    let dir = dir.to_string_lossy().into_owned();
    let listed = std::process::Command::new("ps")
        .args(["-axo", "pid=,command="])
        .output()
        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
        .unwrap_or_default();
    let me = std::process::id();
    listed
        .lines()
        .filter_map(|line| {
            let (pid, command) = line.trim_start().split_once(' ')?;
            let pid: u32 = pid.parse().ok()?;
            let program = command.split_whitespace().next().unwrap_or_default();
            let names_dir = command.match_indices(&dir).any(|(at, _)| {
                matches!(
                    command[at + dir.len()..].chars().next(),
                    None | Some('/') | Some(' ')
                )
            });
            (names_dir
                && pid != me
                && !program
                    .rsplit('/')
                    .next()
                    .is_some_and(|name| name == "tmux"))
            .then_some(pid)
        })
        .collect()
}

#[cfg(unix)]
fn kill_pids(pids: &[u32]) {
    for pid in pids {
        let _ = crate::process::signal(*pid, "-KILL");
    }
}

/// SIGKILLs every agent process whose directory lies under `dir`, and
/// waits until each has released its directory's lock.
async fn kill_agents_under(dir: &Path) -> Result<(), NetError> {
    #[cfg(unix)]
    {
        // `amux agent <dir>`: the agent's own directory is on its command
        // line, and every agent of the net lives under its root.
        kill_pids(&processes_under(dir));
        let locked: Vec<PathBuf> = agent_dirs(dir);
        observe::until("the killed agents to release their locks", || {
            let held: Vec<&PathBuf> = locked.iter().filter(|dir| agent_dir::locked(dir)).collect();
            async move {
                held.is_empty()
                    .then_some(())
                    .ok_or_else(|| format!("locks still held: {held:?}"))
            }
        })
        .await?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        Err(NetError::Unsupported("killing agent processes"))
    }
}

/// Every directory under `dir` that holds an agent's lock file.
#[cfg(unix)]
fn agent_dirs(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_owned()];
    while let Some(next) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&next) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path.join(agent_dir::LOCK).exists() {
                    found.push(path);
                } else {
                    pending.push(path);
                }
            }
        }
    }
    found
}

/// Runs git in `dir` for `host`'s setup.
fn git(host: &str, dir: &Path, args: &[&str]) -> Result<String, NetError> {
    // Named here, so a machine with no git identity commits too.
    let ran = std::process::Command::new("git")
        .args([
            "-c",
            "user.name=testnet",
            "-c",
            "user.email=testnet@localhost",
        ])
        .args(args)
        .current_dir(dir)
        .output()?;
    if ran.status.success() {
        Ok(String::from_utf8_lossy(&ran.stdout).trim().to_owned())
    } else {
        Err(NetError::Host {
            host: host.to_owned(),
            error: format!(
                "git {} in {} failed: {}",
                args.join(" "),
                dir.display(),
                String::from_utf8_lossy(&ran.stderr).trim()
            ),
        })
    }
}

/// Puts an agent's start folder on `branch`: a new repository there, or
/// one another agent's declaration made, which must be on the same branch.
fn on_branch(host: &str, dir: &Path, branch: &str) -> Result<(), NetError> {
    if !dir.join(".git").exists() {
        git(host, dir, &["init", "--quiet", "--initial-branch", branch])?;
        return Ok(());
    }
    let current = git(host, dir, &["symbolic-ref", "--short", "HEAD"])?;
    if current != branch {
        return Err(NetError::Host {
            host: host.to_owned(),
            error: format!("{} is on branch {current}, not {branch}", dir.display()),
        });
    }
    Ok(())
}

/// The kind an agent is created as, with its provider settings: its
/// declared permission, nothing else.
fn create_config(decl: &AgentDecl, provider: Provider) -> create_agent_request::Agent {
    use create_agent_request::Agent;
    // A real Claude starts asking, as a person's own default on a real
    // host has it: left unset, Claude would take the operator's own
    // default permission mode from outside the agent's settings.
    let permission = decl.permission.clone().or_else(|| {
        (provider == Provider::Real && decl.kind != FakeKind::Codex).then(|| "default".to_owned())
    });
    // A real provider starts on its small model unless told otherwise, as
    // a person's own defaults on a real host do: the provider's own default
    // is a large model, and every agent a test makes would run on it. A
    // fake plays its script's model and ignores this.
    let model = decl.model.clone().or_else(|| {
        (provider == Provider::Real).then(|| match decl.kind {
            FakeKind::Codex => SMALL_CODEX.to_owned(),
            FakeKind::ClaudePty | FakeKind::ClaudeSdk => SMALL_CLAUDE.to_owned(),
        })
    });
    let claude = ClaudeCreateConfig {
        permission: permission.clone(),
        model: model.clone(),
        ..ClaudeCreateConfig::default()
    };
    match decl.kind {
        FakeKind::Codex => Agent::Codex(CodexCreateConfig {
            permission,
            model,
            mode: decl.mode.clone(),
            ..CodexCreateConfig::default()
        }),
        FakeKind::ClaudePty => Agent::ClaudePty(claude),
        FakeKind::ClaudeSdk => Agent::ClaudeSdk(claude),
    }
}

/// The directory an agent starts in: its host's work directory, a folder
/// below it named relatively (made when missing), or an absolute path as
/// it is.
fn work_dir(work: &Path, cwd: Option<&str>) -> Result<String, NetError> {
    let Some(cwd) = cwd else {
        return Ok(work.to_string_lossy().into_owned());
    };
    if cwd.is_empty() || Path::new(cwd).is_absolute() {
        return Ok(cwd.to_owned());
    }
    let dir = work.join(cwd);
    std::fs::create_dir_all(&dir)?;
    Ok(dir.to_string_lossy().into_owned())
}

/// An input id as a failure message shows it.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The loopback address a host's socket binds to: IPv6's for a host bound
/// IPv6 only, IPv4's otherwise.
fn loopback(decl: &HostDecl) -> std::net::IpAddr {
    if decl.ipv6_only {
        std::net::Ipv6Addr::LOCALHOST.into()
    } else {
        std::net::Ipv4Addr::LOCALHOST.into()
    }
}
