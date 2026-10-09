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
use node::harness::{HostVia, ScriptedDiscovery, scripted_discovery};
use node::{
    ClientApi, Daemon, Edge, EdgeOptions, LanOptions, Launch, LoopbackLink, ProfileId,
    ProfileRuntime,
};
use provider_fakes::script::{INPUT_LOG_ENV, SCRIPT_ENV, Script, Step};
use serde::{Deserialize, Serialize};
use store::{AgentKey, Marker, Store as _};
use tokio::sync::mpsc;
use uuid::Uuid;
use wire::client_service_server::ClientService as _;
use wire::profile_service_client::ProfileServiceClient;
use wire::{
    AgentParent, ClaudeCreateConfig, ClaudePtyInput, ClaudeSdkInput, CodexCreateConfig, CodexInput,
    CreateAgentRequest, DeleteAgentRequest, DeleteAgentResponse, Input, PromptInput, StopMode,
    WithdrawQueued, claude_pty_input, claude_sdk_input, codex_input,
};

use crate::binaries::Binaries;
use crate::clock::DrivenClock;
use crate::gate::{Faults, UdpGate};
use crate::invariant::{self, BlockViolation};
use crate::observe::{self, InventoryObserver, Observer, ObserverOf, PATIENCE, Stuck};
use crate::relay::Relay;
use crate::topology::{AgentDecl, FakeKind, HostDecl, TierDecl, Topology, TopologyError, link_key};

/// The extension of the file beside an agent's script that its provider
/// logs every line it reads to.
const INPUT_LOG: &str = "input";

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
}

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
}

impl NetAgent {
    pub fn key(&self) -> AgentKey {
        AgentKey::new(
            self.host_id.as_bytes().to_vec(),
            self.id.as_bytes().to_vec(),
        )
    }

    pub fn parent(&self) -> AgentParent {
        AgentParent {
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
}

struct Link {
    /// Cut by a fault verb, as opposed to down because an end is.
    severed: bool,
    /// Behind a lock only so the net can be shared across tasks: a
    /// loopback carrier is not Sync.
    live: Option<std::sync::Mutex<LoopbackLink>>,
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
    /// A gate in front of a host's LAN listener, started on request: what a
    /// client dials instead of the host so its packets can be delayed or
    /// lost, the way a household network treats them.
    lan_gates: BTreeMap<String, UdpGate>,
    /// A gate in front of the relay's QUIC carrier, for a client outside
    /// the net, once one has asked for it.
    relay_gate: Option<UdpGate>,
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
        topology.validate()?;
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
        let mut net = Self {
            root,
            topology: topology.clone(),
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
            lan_gates: BTreeMap::new(),
            relay_gate: None,
        };
        if let Some(relay) = &topology.relay {
            net.relay = Some(Relay::start(relay, net.clock.clone()).await?);
        }
        for decl in &topology.hosts {
            net.create_host(decl)?;
            if let Some(script) = &decl.script {
                net.write_script(&format!("{}.host", decl.name), script.clone())?;
            }
            net.start_host(&decl.name).await?;
        }
        for decl in &topology.hosts {
            if let Some(account) = &decl.account {
                net.sign_in(&decl.name, account).await?;
            }
        }
        for link in &topology.links {
            let a = net.edge(&link.a)?;
            let b = net.edge(&link.b)?;
            let trust = |error: tonic::Status| NetError::Host {
                host: link.a.clone(),
                error: error.message().to_owned(),
            };
            a.trust(&b).await.map_err(trust)?;
            b.trust(&a).await.map_err(trust)?;
            net.links.insert(
                link_key(&link.a, &link.b),
                Link {
                    severed: false,
                    live: None,
                },
            );
        }
        net.link_all()?;
        for link in &topology.links {
            net.wait_link(&link.a, &link.b, true).await?;
        }
        for agent in &topology.agents {
            net.spawn(agent.clone()).await?;
            if topology.settle {
                net.settle(&agent.name).await?;
            }
        }
        Ok(net)
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

    pub fn is_up(&self, name: &str) -> bool {
        self.hosts
            .get(name)
            .is_some_and(|host| host.daemon.is_some())
    }

    /// The host's profile runtime. Hold it only as long as a call needs:
    /// a held runtime outlives a killed daemon.
    pub fn runtime(&self, name: &str) -> Result<Arc<ProfileRuntime>, NetError> {
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

    /// The installation's front door on `name`: profiles, pairing, peers
    /// and accounts, as a person's client calls them.
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
    /// provider is the fake of its kind playing its script. Relative wait
    /// paths in the script name files in [`Net::gates`].
    pub async fn spawn(&mut self, decl: AgentDecl) -> Result<wire::Agent, NetError> {
        if self.agents.contains_key(&decl.name) {
            return Err(NetError::AgentExists(decl.name));
        }
        let host = self.host(&decl.host)?.clone();
        let parent = match &decl.parent {
            Some(parent) => Some(self.agent(parent)?.parent()),
            None => None,
        };
        decl.check_branch()?;
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
        let request = CreateAgentRequest {
            agent_id: id.as_bytes().to_vec(),
            host: None,
            name: Some(decl.name.clone()),
            parent,
            cwd,
            kind: decl.kind.wire() as i32,
            config: create_config(&decl),
            initial_prompt: decl
                .prompt
                .as_deref()
                .map(|text| submission(b"testnet-first", text)),
            new_worktree: false,
        };
        let runtime = self.runtime(&decl.host)?;
        runtime.set_launch(self.launch(&host, Some((decl.kind, &script))));
        let spawned = runtime.spawn(request, None).await;
        runtime.set_launch(self.launch(&host, None));
        let agent = spawned?;
        self.agents.insert(
            decl.name.clone(),
            NetAgent {
                name: decl.name,
                host: host.name.clone(),
                host_id: host.host_id,
                id,
                kind: decl.kind,
            },
        );
        Ok(agent)
    }

    /// Waits until `name` has come to rest on its host: idle or needing
    /// the person, with nothing queued, so a creation prompt's turn has
    /// run. The fleet lists agents of one standing most recently active
    /// first, so starting each once the one before has settled fixes the
    /// order they are listed in.
    pub async fn settle(&self, name: &str) -> Result<(), NetError> {
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
                PATIENCE,
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
        let runtime = self.runtime(&agent.host)?;
        runtime.set_launch(self.launch(&host, Some((agent.kind, &script))));
        let bootstrap = text.map(|text| wire::Bootstrap {
            of: Some(wire::bootstrap::Of::Prompt(wire::PromptSubmission {
                input_id: Uuid::new_v4().as_bytes().to_vec(),
                prompt: Some(PromptInput {
                    text: text.to_owned(),
                    attachments: Vec::new(),
                }),
            })),
        });
        let resumed = runtime.resume(agent.id, bootstrap).await;
        runtime.set_launch(self.launch(&host, None));
        Ok(resumed?)
    }

    /// Sends the agent a prompt through its own host.
    pub async fn send(&self, name: &str, text: &str) -> Result<wire::SendInputResponse, NetError> {
        let agent = self.agent(name)?;
        let runtime = self.runtime(&agent.host)?;
        let input = prompt(agent.kind, Uuid::new_v4().as_bytes(), text);
        runtime
            .send_input(&wire::SendInputRequest {
                agent_id: agent.id.as_bytes().to_vec(),
                input: Some(input),
            })
            .await
            .map_err(|error| NetError::Host {
                host: agent.host.clone(),
                error: error.to_string(),
            })
    }

    /// Sends the agent any input through its own host: an answer, a
    /// withdrawal, an interrupt.
    pub async fn input(
        &self,
        name: &str,
        input: Input,
    ) -> Result<wire::SendInputResponse, NetError> {
        let agent = self.agent(name)?;
        let runtime = self.runtime(&agent.host)?;
        runtime
            .send_input(&wire::SendInputRequest {
                agent_id: agent.id.as_bytes().to_vec(),
                input: Some(input),
            })
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
        self.client(&agent.host)?
            .stop_agent(tonic::Request::new(wire::StopAgentRequest {
                agent_id: agent.id.as_bytes().to_vec(),
                mode: wire::StopMode::Graceful as i32,
            }))
            .await
            .map_err(|status| NetError::Refused(Box::new(status)))?;
        Ok(self.ack(format!("{name} stopped")))
    }

    /// What `name` offers, as a person on `host` fetches it: forwarded to
    /// the agent's own host when it lives elsewhere.
    pub async fn catalogue(&self, host: &str, name: &str) -> Result<wire::Catalogue, NetError> {
        let agent = self.agent(name)?;
        let request = wire::GetCatalogueRequest {
            of: Some(wire::get_catalogue_request::Of::AgentId(
                agent.id.as_bytes().to_vec(),
            )),
        };
        self.client(host)?
            .get_catalogue(tonic::Request::new(request))
            .await
            .map(tonic::Response::into_inner)
            .map_err(|status| NetError::Refused(Box::new(status)))
    }

    /// Deletes the agent on its own host.
    pub async fn delete(&mut self, name: &str) -> Result<Ack, NetError> {
        let answer = self.delete_family(name).await?;
        Ok(self.ack(format!(
            "{name} deleted with {} children; {} children unreachable",
            answer.removed_children.len(),
            answer.unreachable_children.len()
        )))
    }

    /// Deletes the agent on its own host, as a person does there, and
    /// answers what the cascade reached and what it could not. The net
    /// forgets every agent the cascade removed.
    pub async fn delete_family(&mut self, name: &str) -> Result<DeleteAgentResponse, NetError> {
        let agent = self.agent(name)?.clone();
        let answer = self
            .client(&agent.host)?
            .delete_agent(tonic::Request::new(DeleteAgentRequest {
                agent_id: agent.id.as_bytes().to_vec(),
            }))
            .await
            .map_err(|status| NetError::Refused(Box::new(status)))?
            .into_inner();
        self.agents.remove(name);
        for child in &answer.removed_children {
            self.agents
                .retain(|_, known| known.id.as_bytes() != child.agent_id.as_slice());
        }
        Ok(answer)
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
            kind: decl.kind.wire() as i32,
            config: create_config(&decl),
            ..CreateAgentRequest::default()
        };
        let tools = self.tools(parent)?;
        let target = self.runtime(&decl.host)?;
        target.set_launch(self.launch(&host, Some((decl.kind, &script))));
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
            .set_launch(self.launch(&host, Some((agent.kind, &path))));
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

    /// What a host's agents start with. With an agent, its fake and its
    /// script; without, the host's defaults: the host's declared script, or
    /// nothing.
    fn launch(&self, host: &HostInfo, agent: Option<(FakeKind, &Path)>) -> Launch {
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
            ..Launch::default()
        };
        launch.provider_env.insert(
            "CLAUDE_CONFIG_DIR".to_owned(),
            self.hosts[&host.name]
                .dir
                .join("claude")
                .to_string_lossy()
                .into_owned(),
        );
        let host_script = self.hosts[&host.name].decl.script.is_some().then(|| {
            self.root
                .path()
                .join("scripts")
                .join(format!("{}.host.json", host.name))
        });
        let agent = agent.or(host_script
            .as_deref()
            .map(|script| (FakeKind::ClaudeSdk, script)));
        if let Some((kind, script)) = agent {
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
        // Long enough to ride out a daemon restart, short enough that an
        // agent a failed run leaves behind goes away on its own.
        launch.agent.grace_secs = 20;
        launch.agent.drain_secs = 5;
        launch.stop_deadline_ms = 15_000;
        let own = &self.hosts[&host.name];
        if !own.decl.repositories.is_empty() {
            launch.repository_roots = vec![own.dir.join(REPOSITORIES)];
        }
        if let Some(hook) = &self.launch_hook {
            hook(&host.name, &mut launch);
        }
        launch
    }

    // --- hosts -------------------------------------------------------------

    fn create_host(&mut self, decl: &HostDecl) -> Result<(), NetError> {
        // Names that differ only in case share a directory on a
        // case-insensitive filesystem.
        let twins = self
            .hosts
            .keys()
            .filter(|name| name.eq_ignore_ascii_case(&decl.name))
            .count();
        let dir = match twins {
            0 => self.root.path().join(&decl.name),
            n => self.root.path().join(format!("{}-{n}", decl.name)),
        };
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
        let host_id = node::host_id(&node::profile_dir(&data_dir, profile))?;
        let front_door = dir.join("door.sock");
        let config = dir.join("installation.yaml");
        std::fs::write(
            &config,
            format!(
                "root: {}\nfront_door_socket: {}\nhost_name: {}\n",
                data_dir.display(),
                front_door.display(),
                decl.name,
            ),
        )?;
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
                },
                dir,
                daemon: None,
                runtime: Weak::new(),
                boot: 1,
                checkpoint: None,
                lan_handed: false,
            },
        );
        Ok(())
    }

    async fn start_host(&mut self, name: &str) -> Result<(), NetError> {
        if let Some(relay) = &self.relay
            && !self.udp.contains_key(name)
        {
            let gate = relay.gate().await?;
            self.udp.insert(name.to_owned(), gate);
        }
        let host = self
            .hosts
            .get(name)
            .ok_or_else(|| NetError::NoHost(name.to_owned()))?;
        if host.daemon.is_some() {
            return Err(NetError::Running(name.to_owned()));
        }
        let decl = &host.decl;
        let mut edge = EdgeOptions {
            host_name: decl.name.clone(),
            kinds: vec![
                wire::Kind::ClaudePty,
                wire::Kind::ClaudeSdk,
                wire::Kind::Codex,
            ],
            lan: decl.lan.then(|| LanOptions {
                bind: std::net::SocketAddr::from(([127, 0, 0, 1], 0)),
                socket: None,
            }),
            discovery: if decl.bonjour {
                Some(Arc::new(|| {
                    Ok(Arc::new(crate::bonjour::SystemBonjour::new())
                        as Arc<dyn node::discovery::Discovery>)
                }))
            } else {
                decl.discovery.then(|| scripted_discovery(&self.bus))
            },
            discovery_scope: decl
                .scope
                .clone()
                .unwrap_or_else(|| self.topology.scope.clone()),
            dial: decl.lan,
            link_socket: false,
            cloud: node::CloudOptions::default(),
        };
        if let Some(relay) = &self.relay {
            edge.cloud = relay.cloud_options(&self.udp[name]);
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
        Ok(())
    }

    /// Makes `a` trust `b` as pairing would, without linking them: they
    /// meet over whatever route they find.
    pub async fn trust(&mut self, a: &str, b: &str) -> Result<Ack, NetError> {
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
                profile_id: Some(profile.to_string()),
                cloud_url: relay.url().to_owned(),
                staged_refresh_token: Relay::login(account),
                // The person confirms adopting whatever the profile holds.
                adopt_non_pristine: true,
                ..wire::BindProfileRequest::default()
            })
            .await
            .map_err(|status| NetError::Refused(Box::new(status)))?;
        let edge = self.edge(host)?;
        observe::until(&format!("{host}'s relay link"), || {
            let observed = edge.observed();
            async move {
                matches!(observed, node::Observed::Connected { .. })
                    .then_some(())
                    .ok_or_else(|| format!("observed {observed:?}"))
            }
        })
        .await?;
        Ok(self.ack(format!("{host} signed in to {account}")))
    }

    /// Changes what `account` has bought, and has each of its hosts that is
    /// up ask for its entitlement at once, as the host where a purchase or
    /// a cancellation happened does.
    pub async fn set_tier(&self, account: &str, tier: TierDecl) -> Result<Ack, NetError> {
        self.relay()?.set_tier(account, tier.into());
        let signed_in: Vec<String> = self
            .hosts
            .values()
            .filter(|host| host.decl.account.as_deref() == Some(account))
            .map(|host| host.info.name.clone())
            .filter(|name| self.is_up(name))
            .collect();
        for host in &signed_in {
            self.edge(host)?
                .refresh_entitlement()
                .await
                .map_err(|error| NetError::Host {
                    host: host.clone(),
                    error,
                })?;
        }
        Ok(self.ack(format!("{account} is on {tier:?}")))
    }

    /// The address of a gate in front of `host`'s LAN listener, started
    /// the first time it is asked for. A client that dials it instead of
    /// the host reaches the host through a network the net can slow down
    /// or make lossy with [`Net::set_lan_faults`]; the host's own address
    /// stays as it was, so a pairing link has to be rewritten to name the
    /// gate. Only a running host with a LAN listener has one.
    pub async fn lan_gate(&mut self, host: &str) -> Result<SocketAddr, NetError> {
        if let Some(gate) = self.lan_gates.get(host) {
            return Ok(gate.addr());
        }
        let info = self.host(host)?;
        let port = std::fs::read_to_string(
            node::profile_dir(&info.data_dir, info.profile).join(node::LAN_PORT_FILE),
        )
        .ok()
        .and_then(|text| text.trim().parse::<u16>().ok())
        .ok_or_else(|| NetError::Host {
            host: host.to_owned(),
            error: "has no LAN listener to gate".to_owned(),
        })?;
        let gate = UdpGate::start(SocketAddr::from(([127, 0, 0, 1], port))).await?;
        let addr = gate.addr();
        self.lan_gates.insert(host.to_owned(), gate);
        Ok(addr)
    }

    /// What the gate in front of `host`'s LAN listener does to each
    /// datagram from now on: delay, loss, or neither.
    pub fn set_lan_faults(&self, host: &str, faults: Faults) -> Result<Ack, NetError> {
        let gate = self.lan_gates.get(host).ok_or_else(|| NetError::Host {
            host: host.to_owned(),
            error: "has no LAN gate; ask for one first".to_owned(),
        })?;
        gate.set_faults(faults);
        Ok(self.ack(format!(
            "{host}'s LAN gate delays {} ms and loses {}%",
            faults.delay.as_millis(),
            faults.loss_percent
        )))
    }

    /// A gate in front of the relay's QUIC carrier, which a client outside
    /// the net dials in the relay's place so its packets can be delayed or
    /// lost; the same gate on every ask.
    pub async fn relay_gate(&mut self) -> Result<SocketAddr, NetError> {
        if let Some(gate) = &self.relay_gate {
            return Ok(gate.addr());
        }
        let gate = self.relay()?.gate().await?;
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
        Ok(self.ack(format!(
            "the relay's gate delays {} ms and loses {}%",
            faults.delay.as_millis(),
            faults.loss_percent
        )))
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
        // A dead daemon's sockets die with its process, and the next one
        // listens where it did. In process, the dropped listener lets go of
        // its port only once its last task has, so wait for that. A socket
        // a test handed in is the test's to hold or let go.
        let lan_port = std::fs::read_to_string(
            node::profile_dir(&host.info.data_dir, host.info.profile).join(node::LAN_PORT_FILE),
        )
        .ok()
        .and_then(|text| text.trim().parse::<u16>().ok())
        .filter(|_| host.decl.lan && !host.lan_handed);
        if let Some(port) = lan_port {
            observe::until(&format!("{name}'s LAN port to be free"), || {
                let bound = std::net::UdpSocket::bind(("127.0.0.1", port));
                async move { bound.map(|_| ()).map_err(|error| error.to_string()) }
            })
            .await?;
        }
        Ok(())
    }

    // --- faults ------------------------------------------------------------

    /// Makes `a` fail every call to `b` before it goes out, as when no
    /// channel for it can be opened, while the link between them and the
    /// sessions it carries stay up. `refuse: false` lifts it.
    pub fn refuse_calls(&self, a: &str, b: &str, refuse: bool) -> Result<Ack, NetError> {
        let peer = self.host(b)?.host_id;
        self.edge(a)?.refuse_calls(peer, refuse);
        Ok(self.ack(format!(
            "{a} {} calls to {b}",
            if refuse { "refuses" } else { "makes" }
        )))
    }

    /// Cuts the link between two hosts the way a dead connection goes: no
    /// LinkClose, the carrier just stops.
    pub fn sever_link(&mut self, a: &str, b: &str) -> Result<Ack, NetError> {
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

    /// Links the two hosts again, if both are up, once both ends have seen
    /// the severed link go. An end still holding it would refuse the new
    /// link as a second dial from the same process, and an in-process link
    /// is never dialled again.
    pub async fn restore_link(&mut self, a: &str, b: &str) -> Result<Ack, NetError> {
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

    /// Crashes a host's daemon. Its agent processes keep running, in their
    /// grace, waiting for the next daemon.
    pub async fn kill_daemon(&mut self, host: &str) -> Result<Ack, NetError> {
        self.take_down(host, false).await?;
        Ok(self.ack(format!("{host}'s daemon killed")))
    }

    /// Shuts a host's daemon down cleanly: flushed, marked clean. Its agent
    /// processes keep running.
    pub async fn stop_daemon(&mut self, host: &str) -> Result<Ack, NetError> {
        self.take_down(host, true).await?;
        Ok(self.ack(format!("{host}'s daemon stopped")))
    }

    /// Starts a host's daemon again from what its installation holds, under
    /// the same boot, and relinks its declared links.
    pub async fn restart_daemon(&mut self, host: &str) -> Result<Ack, NetError> {
        self.start_host(host).await?;
        self.link_all()?;
        Ok(self.ack(format!("{host}'s daemon started")))
    }

    /// Records what has reached the drive now: the store flushed and its
    /// file as it stands. [`Net::rewind_host`] loses power back to here.
    pub async fn checkpoint_host(&mut self, host: &str) -> Result<Ack, NetError> {
        let runtime = self.runtime(host)?;
        let bytes = {
            // Held so nothing commits between the flush and the read.
            let store = runtime.store().await;
            store.flush_to_drive().map_err(|error| NetError::Host {
                host: host.to_owned(),
                error: error.to_string(),
            })?;
            std::fs::read(runtime.dir().join(node::STORE))?
        };
        drop(runtime);
        self.hosts.get_mut(host).expect("the host").checkpoint = Some(bytes);
        Ok(self.ack(format!("{host} checkpointed")))
    }

    /// Power loss without power: the daemon and every agent process on the
    /// host die at once; the store goes back to the last checkpoint with
    /// no WAL, each named journal is cut at its byte, and the machine comes
    /// back under a new boot id with the clean flag never set.
    pub async fn rewind_host(&mut self, host: &str, cuts: &[JournalCut]) -> Result<Ack, NetError> {
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
        let mut subscription = self.runtime(host)?.subscribe(id.as_bytes(), tail).await?;
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

    /// Whether both ends of a link route to each other directly now.
    pub async fn link_up(&self, a: &str, b: &str) -> bool {
        let (Ok(edge_a), Ok(edge_b), Ok(info_a), Ok(info_b)) =
            (self.edge(a), self.edge(b), self.host(a), self.host(b))
        else {
            return false;
        };
        edge_a.via(info_b.host_id).await == HostVia::Direct
            && edge_b.via(info_a.host_id).await == HostVia::Direct
    }

    /// Waits until the link between two hosts is up, or down on both ends.
    pub async fn wait_link(&self, a: &str, b: &str, up: bool) -> Result<(), NetError> {
        let (id_a, id_b) = (self.host(a)?.host_id, self.host(b)?.host_id);
        let what = format!("link {a} - {b} {}", if up { "up" } else { "down" });
        let wanted = if up {
            HostVia::Direct
        } else {
            HostVia::Offline
        };
        observe::until(&what, || async {
            let via_ab = match self.edge(a) {
                Ok(edge) => edge.via(id_b).await,
                Err(_) => HostVia::Offline,
            };
            let via_ba = match self.edge(b) {
                Ok(edge) => edge.via(id_a).await,
                Err(_) => HostVia::Offline,
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
                    && edge.via(far).await == HostVia::Direct
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
        let agent = self.agent(agent)?.clone();
        let key = agent.key();
        let replica = {
            let runtime = self.runtime(host)?;
            let store = runtime.store().await;
            invariant::block(&store, &key).map_err(|error| NetError::Host {
                host: host.to_owned(),
                error,
            })?
        };
        let origin = {
            let runtime = self.runtime(&agent.host)?;
            let store = runtime.store().await;
            invariant::block(&store, &key).map_err(|error| NetError::Host {
                host: agent.host.clone(),
                error,
            })?
        };
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
        mut probe: impl FnMut(&store::Sqlite) -> Result<T, String>,
    ) -> Result<T, NetError> {
        let runtime = self.runtime(host)?;
        let mut writes = runtime.committed();
        let deadline = tokio::time::Instant::now() + PATIENCE;
        loop {
            // Marked seen before the read, so a write landing between the
            // read and the wait is a change the wait returns on.
            writes.borrow_and_update();
            let seen = match tokio::time::timeout_at(deadline, runtime.store()).await {
                Ok(store) => match probe(&store) {
                    Ok(found) => return Ok(found),
                    Err(seen) => seen,
                },
                Err(_) => {
                    return Err(Stuck::Hung {
                        what: what.to_owned(),
                        waited: PATIENCE,
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
                        waited: PATIENCE,
                        seen,
                    }
                    .into());
                }
            }
        }
    }

    /// Waits until `host` holds `agent`'s row at `order`: its newest row
    /// is that one or a later one. After it, what the host holds through
    /// `order` is what its store reports, on the origin and on a replica
    /// alike.
    pub async fn fence(&self, host: &str, agent: &str, order: u64) -> Result<(), NetError> {
        let key = self.agent(agent)?.key();
        let what = format!("{host} to hold {agent}'s row at order {order}");
        self.on_write(host, &what, |store| {
            let newest = store
                .last_n(&key, 1)
                .map_err(|error| error.to_string())?
                .pop()
                .map(|item| item.order);
            match newest {
                Some(held) if held >= order => Ok(()),
                Some(held) => Err(format!("newest order {held}")),
                None => Err("no rows".to_owned()),
            }
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
        self.on_write(&at.host, &what, |store| {
            let mut ends: Vec<u64> = store
                .cut(&key, u32::MAX)
                .map_err(|error| error.to_string())?
                .held
                .iter()
                .filter(|item| item.key.starts_with("turn:"))
                .map(|item| item.order)
                .collect();
            ends.sort_unstable();
            ends.get(k - 1)
                .copied()
                .ok_or_else(|| format!("{} turns ended", ends.len()))
        })
        .await
    }

    /// Waits until the input `agent` accepted under `input_id` has
    /// settled on its own host: the row that reflects it exists. Returns
    /// that row's order. A rejected input never settles; rejection is
    /// the answer `send` and `input` return.
    pub async fn input_settled(&self, agent: &str, input_id: &[u8]) -> Result<u64, NetError> {
        let at = self.agent(agent)?.clone();
        let key = at.key();
        let what = format!("{agent}'s input {} to settle", hex(input_id));
        self.on_write(&at.host, &what, |store| {
            store
                .item_by_input(&key, input_id)
                .map_err(|error| error.to_string())?
                .map(|item| item.order)
                .ok_or_else(|| "no row carries it".to_owned())
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
                let store = runtime.store().await;
                store
                    .agent(&key)
                    .map_err(|error| error.to_string())?
                    .map(|row| row.next_revision - 1)
                    .ok_or_else(|| "no origin row".to_owned())?
            };
            let (cursor, marker) = {
                let runtime = self.runtime(host).map_err(|error| error.to_string())?;
                let store = runtime.store().await;
                let row = store
                    .agent(&key)
                    .map_err(|error| error.to_string())?
                    .ok_or_else(|| format!("no replica row; origin newest {newest}"))?;
                let marker = store
                    .cut(&key, 0)
                    .map_err(|error| error.to_string())?
                    .marker;
                (row.source_cursor, marker)
            };
            if cursor != newest || marker != Some(Marker::CaughtUp) {
                return Err(format!(
                    "replica cursor {cursor}, marker {marker:?}; origin newest {newest}"
                ));
            }
            self.assert_block_invariant(host, agent)
                .await
                .map_err(|error| error.to_string())
        })
        .await?;
        Ok(())
    }

    // --- teardown ----------------------------------------------------------

    /// Stops every agent and shuts every daemon down cleanly. The explicit
    /// end of a net; dropping one kills what it can and is for panics.
    pub async fn shutdown(mut self) -> Result<(), NetError> {
        let names: Vec<String> = self.hosts.keys().cloned().collect();
        for name in &names {
            if let Ok(runtime) = self.runtime(name) {
                for id in runtime.live() {
                    let _ = tokio::time::timeout(PATIENCE, runtime.stop(id, StopMode::Kill)).await;
                }
            }
        }
        for name in &names {
            if self.is_up(name) {
                self.take_down(name, true).await?;
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

/// SIGKILLs every agent process whose directory lies under `dir`, and
/// waits until each has released its directory's lock.
async fn kill_agents_under(dir: &Path) -> Result<(), NetError> {
    #[cfg(unix)]
    {
        // `amux agent <dir>`: the agent's own directory is on its command
        // line, and every agent of the net lives under its root.
        let _ = tokio::process::Command::new("pkill")
            .args(["-KILL", "-f"])
            .arg(dir)
            .status()
            .await;
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

/// The directory an agent starts in: its host's work directory, a folder
/// below it named relatively (made when missing), or an absolute path as
/// it is.
/// The provider settings an agent is created with, encoded for its kind:
/// its declared permission, nothing else.
fn create_config(decl: &AgentDecl) -> Vec<u8> {
    use prost::Message as _;
    let permission = decl.permission.clone();
    match decl.kind {
        FakeKind::Codex => CodexCreateConfig {
            permission,
            ..CodexCreateConfig::default()
        }
        .encode_to_vec(),
        _ => ClaudeCreateConfig {
            permission,
            ..ClaudeCreateConfig::default()
        }
        .encode_to_vec(),
    }
}

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
