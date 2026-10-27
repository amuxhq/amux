//! The served door: a running net behind a loopback control socket, for a
//! driver in another process (the terminal journeys, the simulator).
//!
//! [`serve`] starts the topology and returns once it is ready: every
//! daemon's front door answers, every declared link carries traffic and
//! every declared agent has said hello. Only then is the [`Readiness`]
//! published, as one JSON line on the binary's standard output. A driver
//! then sends [`Control`] requests, one JSON value per line, and reads one
//! [`Reply`] line per request, in order.
//!
//! Every verb is a net capability or a declared composition of two; the
//! map is [`CAPABILITIES`], and a test holds every verb to it.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, watch};
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::gate::Faults;
use crate::net::{ClockMode, JournalCut, Net, NetError, NetOptions};
use crate::observe;
use crate::router::RouterKind;
use crate::topology::{AccountDecl, AgentDecl, FakeKind, HostDecl, Provider, TierDecl, Topology};

/// A request to the door.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum Control {
    Sever {
        a: String,
        b: String,
    },
    Restore {
        a: String,
        b: String,
    },
    Link {
        a: String,
        b: String,
    },
    Trust {
        a: String,
        b: String,
    },
    Untrust {
        a: String,
        b: String,
    },
    RefuseCalls {
        a: String,
        b: String,
        refuse: bool,
    },
    SetTier {
        account: String,
        tier: TierDecl,
    },
    SignIn {
        host: String,
        account: String,
    },
    KillDaemon {
        host: String,
    },
    StopDaemon {
        host: String,
    },
    RestartDaemon {
        host: String,
    },
    Checkpoint {
        host: String,
    },
    Rewind {
        host: String,
        #[serde(default)]
        cuts: Vec<JournalCut>,
    },
    Advance {
        ms: u64,
    },
    Spawn {
        agent: Box<AgentDecl>,
    },
    Resume {
        agent: String,
        #[serde(default)]
        text: Option<String>,
    },
    Send {
        agent: String,
        text: String,
    },
    Stop {
        agent: String,
    },
    Freeze {
        agent: String,
    },
    Thaw {
        agent: String,
    },
    OpenGate {
        name: String,
    },
    /// A gate in front of the host's LAN listener, for a client to dial in
    /// place of the host; answers its address.
    LanGate {
        host: String,
    },
    /// What that gate does to every datagram from now on.
    LanFaults {
        host: String,
        #[serde(default)]
        delay_ms: u64,
        #[serde(default)]
        loss_percent: u32,
        #[serde(default)]
        jitter_ms: u64,
        #[serde(default)]
        reorder_percent: u32,
        #[serde(default)]
        rate_bytes_per_sec: u64,
    },
    /// A gate in front of the relay's QUIC carrier, for a client outside
    /// the net to dial in the relay's place; answers its address.
    RelayGate {},
    /// What that gate does to every datagram from now on.
    RelayFaults {
        #[serde(default)]
        delay_ms: u64,
        #[serde(default)]
        loss_percent: u32,
        #[serde(default)]
        jitter_ms: u64,
        #[serde(default)]
        reorder_percent: u32,
        #[serde(default)]
        rate_bytes_per_sec: u64,
    },
    /// Makes a host's network, or the relay's, bad: every gate its
    /// datagrams cross.
    NetFaults {
        target: String,
        #[serde(default)]
        delay_ms: u64,
        #[serde(default)]
        loss_percent: u32,
        #[serde(default)]
        jitter_ms: u64,
        #[serde(default)]
        reorder_percent: u32,
        #[serde(default)]
        rate_bytes_per_sec: u64,
    },
    /// Drops every datagram on the host's way to the relay, or stops.
    BlockUdp {
        host: String,
        blocked: bool,
    },
    /// Puts an imitated router in front of a host's link socket, now and
    /// on every start after.
    SetRouter {
        host: String,
        kind: RouterKind,
    },
    /// Moves a host onto another network: its link socket rebinds and its
    /// links migrate.
    ChangeNetwork {
        host: String,
    },
    /// Binds a host to IPv6 loopback alone from its next start.
    BindIpv6Only {
        host: String,
    },
    /// The path carrying a host's link to a peer, by the host's link log;
    /// null while the link is down.
    CarryingPath {
        host: String,
        peer: String,
    },
    /// Every line the host has written to the link log, oldest first.
    LinkLog {
        host: String,
    },
    /// Severs a link and restores it on a period until stopped.
    Flap {
        a: String,
        b: String,
        down_ms: u64,
        up_ms: u64,
    },
    /// Stops every flapping link, leaving each restored.
    FlapStop {},
    /// The gate `from` dials to reach `to`'s LAN listener: give it to a
    /// pairing in place of `to`'s address; answers its address.
    PairGate {
        from: String,
        to: String,
    },
    /// Adds a host to the running net.
    AddHost {
        host: Box<HostDecl>,
    },
    RemoveHost {
        host: String,
    },
    /// Two hosts trust each other and link, as a declared link.
    AddLink {
        a: String,
        b: String,
    },
    /// Two hosts forget each other.
    RemoveLink {
        a: String,
        b: String,
    },
    /// An account the relay's cloud knows, starting the relay if none runs.
    AddAccount {
        account: AccountDecl,
    },
    /// Waits until an agent rests: idle or needing the person, nothing
    /// queued; after the input named, when one is.
    Settle {
        agent: String,
        #[serde(default)]
        input: Option<String>,
        #[serde(default)]
        timeout_ms: Option<u64>,
    },
    /// Publishes the build under test on the release feed and has every
    /// supervised host check for it.
    Release {
        version: String,
        #[serde(default)]
        channel: Option<String>,
        #[serde(default)]
        rollout: Option<u8>,
        #[serde(default)]
        broken: bool,
    },
    /// The net as it stands: hosts, agents, links, and the topology that
    /// would start it again.
    Status {},
    Inventory {
        host: String,
    },
    Block {
        host: String,
        agent: String,
    },
    Chat {
        host: String,
        agent: String,
    },
    ProviderInput {
        agent: String,
    },
    Shutdown,
}

/// Door verb to the harness capability it runs. A verb that composes two
/// capabilities, or adds an effect at the process boundary, says so.
pub const CAPABILITIES: &[(&str, &str)] = &[
    ("Sever", "Net::sever_link"),
    ("Restore", "Net::restore_link"),
    ("Link", "Net::link_up"),
    ("Trust", "Net::trust"),
    ("Untrust", "Net::untrust"),
    ("RefuseCalls", "Net::refuse_calls"),
    ("SetTier", "Net::set_tier"),
    ("SignIn", "Net::sign_in"),
    ("KillDaemon", "Net::kill_daemon"),
    ("StopDaemon", "Net::stop_daemon"),
    (
        "RestartDaemon",
        "Net::kill_daemon when running + Net::restart_daemon",
    ),
    ("Checkpoint", "Net::checkpoint_host"),
    ("Rewind", "Net::rewind_host"),
    ("Advance", "Net::advance"),
    ("Spawn", "Net::spawn"),
    ("Resume", "Net::resume"),
    ("Send", "Net::send"),
    ("Stop", "Net::stop"),
    ("Freeze", "Net::freeze"),
    ("Thaw", "Net::thaw"),
    ("OpenGate", "Net::open_gate"),
    ("LanGate", "Net::lan_gate"),
    ("LanFaults", "Net::set_lan_faults"),
    ("RelayGate", "Net::relay_gate"),
    ("RelayFaults", "Net::set_relay_faults"),
    ("NetFaults", "Net::set_net_faults"),
    ("BlockUdp", "Net::block_udp"),
    ("SetRouter", "Net::set_router"),
    ("ChangeNetwork", "Net::change_network"),
    ("BindIpv6Only", "Net::bind_ipv6_only"),
    ("CarryingPath", "Net::carrying_path"),
    ("LinkLog", "Net::link_log"),
    ("Flap", "Net::sever_link + Net::restore_link on a period"),
    ("FlapStop", "Net::restore_link for each flapping link"),
    ("PairGate", "Net::pair_gate"),
    ("AddHost", "Net::add_host"),
    ("RemoveHost", "Net::remove_host"),
    ("AddLink", "Net::add_link"),
    ("RemoveLink", "Net::remove_link"),
    ("AddAccount", "Net::add_account"),
    (
        "Settle",
        "Net::input_settled when an input is named + Net::settle_within",
    ),
    ("Release", "Net::publish_release"),
    ("Status", "Net::topology + Net::host"),
    (
        "Inventory",
        "Net::observe_inventory + observe_until(CaughtUp)",
    ),
    ("Block", "Net::assert_block_invariant"),
    (
        "Chat",
        "Net::observe or Net::observe_id + observe_until(CaughtUp)",
    ),
    ("ProviderInput", "Net::provider_input"),
    ("Shutdown", "Net::shutdown + closes the control socket"),
];

impl Control {
    /// The verb's name, as it is spelled on the wire and in [`CAPABILITIES`].
    pub fn verb(&self) -> &'static str {
        match self {
            Self::Sever { .. } => "Sever",
            Self::Restore { .. } => "Restore",
            Self::Link { .. } => "Link",
            Self::Trust { .. } => "Trust",
            Self::Untrust { .. } => "Untrust",
            Self::RefuseCalls { .. } => "RefuseCalls",
            Self::SetTier { .. } => "SetTier",
            Self::SignIn { .. } => "SignIn",
            Self::KillDaemon { .. } => "KillDaemon",
            Self::StopDaemon { .. } => "StopDaemon",
            Self::RestartDaemon { .. } => "RestartDaemon",
            Self::Checkpoint { .. } => "Checkpoint",
            Self::Rewind { .. } => "Rewind",
            Self::Advance { .. } => "Advance",
            Self::Spawn { .. } => "Spawn",
            Self::Resume { .. } => "Resume",
            Self::Send { .. } => "Send",
            Self::Stop { .. } => "Stop",
            Self::Freeze { .. } => "Freeze",
            Self::Thaw { .. } => "Thaw",
            Self::OpenGate { .. } => "OpenGate",
            Self::LanGate { .. } => "LanGate",
            Self::LanFaults { .. } => "LanFaults",
            Self::RelayGate {} => "RelayGate",
            Self::RelayFaults { .. } => "RelayFaults",
            Self::NetFaults { .. } => "NetFaults",
            Self::BlockUdp { .. } => "BlockUdp",
            Self::SetRouter { .. } => "SetRouter",
            Self::ChangeNetwork { .. } => "ChangeNetwork",
            Self::BindIpv6Only { .. } => "BindIpv6Only",
            Self::CarryingPath { .. } => "CarryingPath",
            Self::LinkLog { .. } => "LinkLog",
            Self::Flap { .. } => "Flap",
            Self::FlapStop {} => "FlapStop",
            Self::PairGate { .. } => "PairGate",
            Self::AddHost { .. } => "AddHost",
            Self::RemoveHost { .. } => "RemoveHost",
            Self::AddLink { .. } => "AddLink",
            Self::RemoveLink { .. } => "RemoveLink",
            Self::AddAccount { .. } => "AddAccount",
            Self::Settle { .. } => "Settle",
            Self::Release { .. } => "Release",
            Self::Status {} => "Status",
            Self::Inventory { .. } => "Inventory",
            Self::Block { .. } => "Block",
            Self::Chat { .. } => "Chat",
            Self::ProviderInput { .. } => "ProviderInput",
            Self::Shutdown => "Shutdown",
        }
    }
}

/// One reply line: the verb's result or a typed error.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reply {
    Ok(Value),
    Error { kind: ErrorKind, message: String },
}

/// What went wrong, so a driver can tell its own mistake from the net's
/// refusal and from a wait that ran out.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    /// The request did not parse or named nothing the net has.
    Invalid,
    /// The net refused: a host down or running, no checkpoint, wall time.
    Refused,
    /// An observation did not see its consequence in time.
    Stuck,
    /// The replica block invariant does not hold.
    Violation,
    /// The door is shutting down.
    Closed,
}

/// What a driver needs to reach the net, published once it is ready.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Readiness {
    pub control: SocketAddr,
    pub root: PathBuf,
    pub gates: PathBuf,
    pub hosts: Vec<ReadyHost>,
    pub agents: Vec<ReadyAgent>,
    /// The account service a client signs in to, when the topology has a
    /// relay: a refresh token `refresh-<account>` is its login.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cloud_url: Option<String>,
    /// The relay's plaintext TCP carrier, for a client outside the net.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relay_tcp: Option<SocketAddr>,
    /// The relay's QUIC carrier, for a client outside the net, which trusts
    /// it by `relay_root`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relay_quic: Option<SocketAddr>,
    /// The relay's self-signed certificate, DER as hex.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relay_root: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReadyHost {
    pub name: String,
    pub host_id: Uuid,
    pub profile: Uuid,
    pub front_door: PathBuf,
    pub config: PathBuf,
    /// The host's own directory: its installation, provider commands and,
    /// for a process host, its logs.
    #[serde(default)]
    pub dir: PathBuf,
    #[serde(default)]
    pub process: bool,
    #[serde(default)]
    pub supervised: bool,
    #[serde(default)]
    pub provider: Provider,
    #[serde(default)]
    pub up: bool,
    /// The port its LAN listener answers on, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lan_port: Option<u16>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReadyAgent {
    pub name: String,
    pub host: String,
    pub id: Uuid,
    pub kind: FakeKind,
    #[serde(default)]
    pub provider: Provider,
}

#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    #[error("starting the net: {0}")]
    Net(#[from] NetError),
    #[error("binding the control socket: {0}")]
    Bind(std::io::Error),
}

/// A net being served.
pub struct Served {
    pub readiness: Readiness,
    net: Arc<Mutex<Option<Net>>>,
    flaps: Flaps,
    closed: watch::Receiver<bool>,
    accept: JoinHandle<()>,
}

/// The links being cut and restored on a period, by their ends.
type Flaps = Arc<std::sync::Mutex<std::collections::BTreeMap<(String, String), JoinHandle<()>>>>;

/// Starts `topology` on wall time and serves its door on `door`.
pub async fn serve(topology: Topology, door: SocketAddr) -> Result<Served, ServeError> {
    serve_with(
        topology,
        door,
        NetOptions {
            clock: ClockMode::Wall,
            ..NetOptions::default()
        },
    )
    .await
}

pub async fn serve_with(
    topology: Topology,
    door: SocketAddr,
    options: NetOptions,
) -> Result<Served, ServeError> {
    let listener = TcpListener::bind(door).await.map_err(ServeError::Bind)?;
    let control = listener.local_addr().map_err(ServeError::Bind)?;
    let net = Net::start_with(topology, options).await?;
    let readiness = readiness(&net, control)?;
    let net = Arc::new(Mutex::new(Some(net)));
    let flaps = Flaps::default();
    let (close, closed) = watch::channel(false);
    let close = Arc::new(close);
    let accept = tokio::spawn({
        let (net, flaps) = (net.clone(), flaps.clone());
        async move {
            let mut closing = close.subscribe();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let Ok((stream, _)) = accepted else { return };
                        tokio::spawn(connection(stream, net.clone(), flaps.clone(), close.clone()));
                    }
                    _ = closing.wait_for(|closed| *closed) => return,
                }
            }
        }
    });
    Ok(Served {
        readiness,
        net,
        flaps,
        closed,
        accept,
    })
}

impl Served {
    /// Resolves once a driver has sent Shutdown.
    pub async fn closed(&mut self) {
        let _ = self.closed.wait_for(|closed| *closed).await;
    }

    /// Stops serving and shuts the net down.
    pub async fn shutdown(self) -> Result<(), NetError> {
        self.accept.abort();
        let _ = self.accept.await;
        for (_, flap) in std::mem::take(&mut *self.flaps.lock().unwrap()) {
            flap.abort();
        }
        let net = self.net.lock().await.take();
        match net {
            Some(net) => net.shutdown().await,
            None => Ok(()),
        }
    }
}

fn readiness(net: &Net, control: SocketAddr) -> Result<Readiness, NetError> {
    let mut hosts = Vec::new();
    for name in net.host_names() {
        let info = net.host(&name)?;
        let decl = net
            .topology()
            .hosts
            .iter()
            .find(|host| host.name == name)
            .cloned()
            .unwrap_or_default();
        hosts.push(ReadyHost {
            name: info.name.clone(),
            host_id: info.host_id,
            profile: info.profile,
            front_door: info.front_door.clone(),
            config: info.config.clone(),
            dir: info.dir.clone(),
            process: info.process,
            supervised: decl.supervised,
            provider: decl.provider,
            up: net.is_up(&name),
            lan_port: std::fs::read_to_string(
                node::profile_dir(&info.data_dir, info.profile).join(node::LAN_PORT_FILE),
            )
            .ok()
            .and_then(|text| text.trim().parse().ok()),
        });
    }
    Ok(Readiness {
        control,
        root: net.root().to_owned(),
        gates: net.gates(),
        hosts,
        agents: net
            .agents()
            .map(|agent| ReadyAgent {
                name: agent.name.clone(),
                host: agent.host.clone(),
                id: agent.id,
                kind: agent.kind,
                provider: agent.provider,
            })
            .collect(),
        cloud_url: net.relay().ok().map(|relay| relay.client_url().to_owned()),
        relay_tcp: net.relay().ok().map(|relay| relay.tcp()),
        relay_quic: net.relay().ok().map(|relay| relay.quic()),
        relay_root: net.relay().ok().map(|relay| {
            relay
                .quic_root()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect()
        }),
    })
}

/// One driver's connection: requests answered one at a time, in order.
async fn connection(
    stream: TcpStream,
    net: Arc<Mutex<Option<Net>>>,
    flaps: Flaps,
    close: Arc<watch::Sender<bool>>,
) {
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if line.trim().is_empty() {
            continue;
        }
        let (reply, shutting_down) = match serde_json::from_str::<Control>(&line) {
            Ok(Control::Flap {
                a,
                b,
                down_ms,
                up_ms,
            }) => (flap(&net, &flaps, a, b, down_ms, up_ms).await, false),
            Ok(Control::FlapStop {}) => (flap_stop(&net, &flaps).await, false),
            Ok(Control::Shutdown) => {
                for (_, flap) in std::mem::take(&mut *flaps.lock().unwrap()) {
                    flap.abort();
                }
                let taken = net.lock().await.take();
                let reply = match taken {
                    Some(net) => match net.shutdown().await {
                        Ok(()) => Reply::Ok(json!("shut down")),
                        Err(error) => refusal(&error),
                    },
                    None => closed(),
                };
                (reply, true)
            }
            Ok(control) => {
                let mut guard = net.lock().await;
                let reply = match guard.as_mut() {
                    Some(net) => match dispatch(net, control).await {
                        Ok(value) => Reply::Ok(value),
                        Err(error) => refusal(&error),
                    },
                    None => closed(),
                };
                (reply, false)
            }
            Err(error) => (
                Reply::Error {
                    kind: ErrorKind::Invalid,
                    message: format!("not a control request: {error}"),
                },
                false,
            ),
        };
        let mut text = serde_json::to_string(&reply).expect("a reply serializes");
        text.push('\n');
        if write.write_all(text.as_bytes()).await.is_err() {
            return;
        }
        if shutting_down {
            close.send_replace(true);
            return;
        }
    }
}

/// Starts cutting the link between `a` and `b` for `down_ms` and restoring
/// it for `up_ms`, over and over, until stopped: a connection that keeps
/// dropping. The first cut happens before this answers.
async fn flap(
    net: &Arc<Mutex<Option<Net>>>,
    flaps: &Flaps,
    a: String,
    b: String,
    down_ms: u64,
    up_ms: u64,
) -> Reply {
    if down_ms == 0 || up_ms == 0 {
        return Reply::Error {
            kind: ErrorKind::Invalid,
            message: "a flap needs a down and an up period".to_owned(),
        };
    }
    // The net is held only to cut the link and take what flips it: a
    // request holding the net for minutes (a wait on a real agent) must not
    // hold a two-second cut for as long.
    let mut flip = {
        let mut guard = net.lock().await;
        let Some(held) = guard.as_mut() else {
            return closed();
        };
        match held.flap_link(&a, &b) {
            Ok(flip) => flip,
            Err(error) => return refusal(&error),
        }
    };
    let task = tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(down_ms)).await;
            flip.restore().await;
            tokio::time::sleep(Duration::from_millis(up_ms)).await;
            flip.cut();
        }
    });
    if let Some(earlier) = flaps
        .lock()
        .unwrap()
        .insert(crate::topology::link_key(&a, &b), task)
    {
        earlier.abort();
    }
    Reply::Ok(json!({
        "installed": format!("link {a} - {b} flapping: {down_ms} ms down, {up_ms} ms up"),
    }))
}

/// Stops every flap and restores each link, so none is left cut.
async fn flap_stop(net: &Arc<Mutex<Option<Net>>>, flaps: &Flaps) -> Reply {
    let stopped: Vec<(String, String)> = {
        let mut flaps = flaps.lock().unwrap();
        let stopped = flaps.keys().cloned().collect();
        for (_, flap) in std::mem::take(&mut *flaps) {
            flap.abort();
        }
        stopped
    };
    let mut guard = net.lock().await;
    let Some(held) = guard.as_mut() else {
        return closed();
    };
    for (a, b) in &stopped {
        if let Err(error) = held.restore_link(a, b).await {
            return refusal(&error);
        }
    }
    Reply::Ok(json!({
        "installed": if stopped.is_empty() {
            "nothing was flapping".to_owned()
        } else {
            format!(
                "stopped flapping and restored {}",
                stopped
                    .iter()
                    .map(|(a, b)| format!("{a} - {b}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        },
    }))
}

fn closed() -> Reply {
    Reply::Error {
        kind: ErrorKind::Closed,
        message: "the net has shut down".to_owned(),
    }
}

fn refusal(error: &NetError) -> Reply {
    let kind = match error {
        NetError::NoHost(_)
        | NetError::NoAgent(_)
        | NetError::AgentExists(_)
        | NetError::NoLink(..)
        | NetError::Topology(_) => ErrorKind::Invalid,
        NetError::Stuck(_) => ErrorKind::Stuck,
        NetError::Block(_) => ErrorKind::Violation,
        _ => ErrorKind::Refused,
    };
    Reply::Error {
        kind,
        message: error.to_string(),
    }
}

/// Runs one verb against the net.
pub async fn dispatch(net: &mut Net, control: Control) -> Result<Value, NetError> {
    let value = |ack| serde_json::to_value(ack).expect("an ack serializes");
    Ok(match control {
        Control::Sever { a, b } => value(net.sever_link(&a, &b)?),
        Control::Restore { a, b } => value(net.restore_link(&a, &b).await?),
        Control::RefuseCalls { a, b, refuse } => value(net.refuse_calls(&a, &b, refuse).await?),
        Control::Link { a, b } => {
            net.host(&a)?;
            net.host(&b)?;
            json!({ "up": net.link_up(&a, &b).await })
        }
        Control::Trust { a, b } => value(net.trust(&a, &b).await?),
        Control::Untrust { a, b } => value(net.untrust(&a, &b).await?),
        Control::SetTier { account, tier } => value(net.set_tier(&account, tier).await?),
        Control::SignIn { host, account } => value(net.sign_in(&host, &account).await?),
        Control::KillDaemon { host } => value(net.kill_daemon(&host).await?),
        Control::StopDaemon { host } => value(net.stop_daemon(&host).await?),
        Control::RestartDaemon { host } => {
            if net.is_up(&host) {
                net.kill_daemon(&host).await?;
            }
            value(net.restart_daemon(&host).await?)
        }
        Control::Checkpoint { host } => value(net.checkpoint_host(&host).await?),
        Control::Rewind { host, cuts } => value(net.rewind_host(&host, &cuts).await?),
        Control::Advance { ms } => value(net.advance(Duration::from_millis(ms))?),
        Control::Spawn { agent } => {
            let spawned = net.spawn(*agent).await?;
            json!({ "id": Uuid::from_slice(&spawned.agent_id).unwrap_or_default() })
        }
        Control::Resume { agent, text } => {
            let resumed = net.resume(&agent, text.as_deref()).await?;
            json!({ "incarnation": resumed.incarnation })
        }
        Control::Send { agent, text } => {
            let kind = net.agent(&agent)?.kind;
            let id = Uuid::new_v4();
            let verdict = net
                .input(&agent, crate::net::prompt(kind, id.as_bytes(), &text))
                .await?;
            json!({ "verdict": format!("{:?}", verdict.of), "input_id": hex(id.as_bytes()) })
        }
        Control::Stop { agent } => value(net.stop(&agent).await?),
        Control::Freeze { agent } => value(net.freeze(&agent)?),
        Control::Thaw { agent } => value(net.thaw(&agent)?),
        Control::OpenGate { name } => value(net.open_gate(&name)?),
        Control::LanGate { host } => json!({ "addr": net.lan_gate(&host).await?.to_string() }),
        Control::LanFaults {
            host,
            delay_ms,
            loss_percent,
            jitter_ms,
            reorder_percent,
            rate_bytes_per_sec,
        } => value(net.set_lan_faults(
            &host,
            faults(
                delay_ms,
                loss_percent,
                jitter_ms,
                reorder_percent,
                rate_bytes_per_sec,
            ),
        )?),
        Control::RelayGate {} => json!({ "addr": net.relay_gate().await?.to_string() }),
        Control::RelayFaults {
            delay_ms,
            loss_percent,
            jitter_ms,
            reorder_percent,
            rate_bytes_per_sec,
        } => value(net.set_relay_faults(faults(
            delay_ms,
            loss_percent,
            jitter_ms,
            reorder_percent,
            rate_bytes_per_sec,
        ))?),
        Control::NetFaults {
            target,
            delay_ms,
            loss_percent,
            jitter_ms,
            reorder_percent,
            rate_bytes_per_sec,
        } => value(net.set_net_faults(
            &target,
            faults(
                delay_ms,
                loss_percent,
                jitter_ms,
                reorder_percent,
                rate_bytes_per_sec,
            ),
        )?),
        Control::BlockUdp { host, blocked } => value(net.block_udp(&host, blocked)?),
        Control::SetRouter { host, kind } => value(net.set_router(&host, kind)?),
        Control::ChangeNetwork { host } => value(net.change_network(&host)?),
        Control::BindIpv6Only { host } => value(net.bind_ipv6_only(&host)?),
        Control::CarryingPath { host, peer } => json!({
            "path": net.carrying_path(&host, &peer)?.map(|path| path.to_string()),
        }),
        Control::LinkLog { host } => Value::Array(
            net.link_log(&host)?
                .iter()
                .map(|event| {
                    json!({
                        "peer": event.peer,
                        "event": event.event.to_string(),
                        "path": event.path.to_string(),
                        "resumed": event.resumed,
                        "early_data": event.early_data.to_string(),
                        "first_packet_to_catch_up_ms": event.first_packet_to_catch_up_ms,
                    })
                })
                .collect(),
        ),
        Control::Flap { .. } | Control::FlapStop {} => {
            unreachable!("the connection handles flaps")
        }
        Control::PairGate { from, to } => json!({ "addr": net.pair_gate(&from, &to)?.to_string() }),
        Control::AddHost { host } => value(net.add_host(*host).await?),
        Control::RemoveHost { host } => value(net.remove_host(&host).await?),
        Control::AddLink { a, b } => value(net.add_link(&a, &b).await?),
        Control::RemoveLink { a, b } => value(net.remove_link(&a, &b).await?),
        Control::AddAccount { account } => value(net.add_account(account).await?),
        Control::Settle {
            agent,
            input,
            timeout_ms,
        } => {
            let patience = match timeout_ms {
                Some(ms) => Duration::from_millis(ms),
                None => net.patience_of(&agent)?,
            };
            if let Some(input) = input {
                let bytes = unhex(&input).ok_or_else(|| NetError::Host {
                    host: agent.clone(),
                    error: format!("{input:?} is not an input id in hex"),
                })?;
                net.input_settled_within(&agent, &bytes, patience).await?;
            }
            net.settle_within(&agent, patience).await?;
            json!({ "installed": format!("{agent} is at rest") })
        }
        Control::Release {
            version,
            channel,
            rollout,
            broken,
        } => value(
            net.publish_release(
                &version,
                channel.as_deref().unwrap_or("stable"),
                rollout,
                broken,
            )
            .await?,
        ),
        Control::Status {} => {
            let control = "0.0.0.0:0".parse().expect("an address");
            let mut status =
                serde_json::to_value(readiness(net, control)?).expect("readiness serializes");
            status["topology"] = serde_json::to_value(net.topology()).expect("a topology");
            if let Some(object) = status.as_object_mut() {
                object.remove("control");
            }
            status
        }
        Control::Inventory { host } => {
            let mut inventory = net.observe_inventory(&host).await?;
            let events = inventory
                .observe_until(observe::inventory_caught_up, observe::PATIENCE)
                .await?;
            // The hosts beside the agents, with their trust: a client
            // shows only the agents of hosts it trusts, and the inventory
            // also keeps rows a host it no longer trusts left behind.
            let hosts: Vec<Value> = observe::inventory_hosts(events)
                .iter()
                .map(|host| {
                    json!({
                        "name": host.name(),
                        "id": Uuid::from_slice(host.host_id()).unwrap_or_default(),
                        "trust": if host.trusted().is_some() { "TRUSTED" } else { "CANDIDATE" },
                        "presence": match host.reach() {
                            Some(wire::trusted::Reach::Online(_)) | None => "ONLINE",
                            Some(wire::trusted::Reach::Away(_)) => "AWAY",
                            Some(wire::trusted::Reach::Offline(_)) => "OFFLINE",
                        },
                    })
                })
                .collect();
            let agents: Vec<Value> = observe::inventory_agents(events)
                .iter()
                .map(|agent| {
                    json!({
                        "name": agent.name,
                        "id": Uuid::from_slice(&agent.agent_id).unwrap_or_default(),
                        "host_id": Uuid::from_slice(&agent.host_id).unwrap_or_default(),
                        "lifecycle": agent.lifecycle,
                        "phase": agent.phase,
                        "cwd": agent.cwd,
                        "branch": agent.git.as_ref().and_then(|git| git.branch.clone()),
                        "kind": agent.kind().as_str_name().trim_start_matches("KIND_"),
                        "lifecycle_name": agent.lifecycle().as_str_name().trim_start_matches("LIFECYCLE_"),
                        "phase_name": agent.phase().as_str_name().trim_start_matches("PHASE_"),
                        "working_on": agent.working_on,
                        "incarnation": agent.incarnation,
                        "exit_cause": (agent.lifecycle() == wire::Lifecycle::Exited)
                            .then(|| agent.exit_cause().as_str_name().trim_start_matches("EXIT_CAUSE_").to_owned()),
                        "exit_detail": agent.exit_detail,
                    })
                })
                .collect();
            json!({ "agents": agents, "hosts": hosts })
        }
        Control::Block { host, agent } => {
            net.assert_block_invariant(&host, &agent).await?;
            json!("holds")
        }
        Control::Chat { host, agent } => {
            // Everything the host holds of the chat: a tail long enough for
            // any journey, read to its first CaughtUp.
            // An agent a client created has no declared name: its id.
            let mut chat = match Uuid::parse_str(&agent) {
                Ok(id) if net.agent(&agent).is_err() => {
                    net.observe_id(&host, &agent, id, 1_000).await?
                }
                _ => net.observe(&host, &agent, 1_000).await?,
            };
            let events = chat
                .observe_until(observe::caught_up, observe::PATIENCE)
                .await?;
            let mut items: Vec<Value> = Vec::new();
            let mut phase = Value::Null;
            let mut model = Value::Null;
            let mut asks = Value::Array(Vec::new());
            for event in events {
                match &event.of {
                    Some(wire::session_event::Of::Item(item)) => {
                        items.retain(|held| held["key"] != item.key.as_str());
                        items.push(json!({
                            "key": item.key,
                            "order": item.order,
                            "text": item.text,
                            "input_id": hex(&item.input_id),
                            "attachments": item.attachments.iter().map(attachment).collect::<Vec<_>>(),
                            "body": item_body(item),
                        }));
                    }
                    Some(wire::session_event::Of::Snapshot(snapshot)) => {
                        phase = json!(snapshot.phase().as_str_name().trim_start_matches("PHASE_"));

                        model = json!(running_model(snapshot));
                        asks = json!(pending_asks(snapshot));
                    }
                    _ => {}
                }
            }
            items.sort_by_key(|item| item["order"].as_u64());
            json!({ "items": items, "phase": phase, "model": model, "asks": asks })
        }
        Control::ProviderInput { agent } => json!({ "lines": net.provider_input(&agent)? }),
        Control::Shutdown => unreachable!("the connection handles shutdown"),
    })
}

/// The model a snapshot says the agent runs, from its per-kind body.
fn running_model(snapshot: &wire::Snapshot) -> Option<String> {
    use prost::Message;
    let body = snapshot.body.as_slice();
    match wire::Kind::try_from(snapshot.kind).ok()? {
        wire::Kind::ClaudeSdk => wire::ClaudeSdkSnapshot::decode(body).ok()?.model,
        wire::Kind::ClaudePty => wire::ClaudePtySnapshot::decode(body).ok()?.model,
        wire::Kind::Codex => wire::CodexSnapshot::decode(body).ok()?.model,
        wire::Kind::Unspecified => None,
    }
}

/// What an item is, as its per-kind body decodes, shortened: enough to
/// tell a tool call from an ask's record from a plan when comparing the
/// host's record with what a client draws.
fn item_body(item: &wire::Item) -> String {
    use prost::Message;
    let body = item.body.as_slice();
    let text = match wire::Kind::try_from(item.kind) {
        Ok(wire::Kind::ClaudeSdk) => {
            wire::ClaudeSdkItem::decode(body).map(|item| format!("{:?}", item.kind))
        }
        Ok(wire::Kind::ClaudePty) => {
            wire::ClaudePtyItem::decode(body).map(|item| format!("{:?}", item.kind))
        }
        Ok(wire::Kind::Codex) => {
            wire::CodexItem::decode(body).map(|item| format!("{:?}", item.kind))
        }
        _ => return String::new(),
    }
    .unwrap_or_else(|error| format!("undecodable: {error}"));
    shorten(text, 600)
}

/// The asks waiting on a person, each as its body decodes.
fn pending_asks(snapshot: &wire::Snapshot) -> Vec<String> {
    use prost::Message;
    let body = snapshot.body.as_slice();
    let asks: Vec<String> = match wire::Kind::try_from(snapshot.kind) {
        Ok(wire::Kind::ClaudeSdk) => wire::ClaudeSdkSnapshot::decode(body)
            .map(|snapshot| snapshot.asks.iter().map(|ask| format!("{ask:?}")).collect())
            .unwrap_or_default(),
        Ok(wire::Kind::ClaudePty) => wire::ClaudePtySnapshot::decode(body)
            .map(|snapshot| snapshot.asks.iter().map(|ask| format!("{ask:?}")).collect())
            .unwrap_or_default(),
        Ok(wire::Kind::Codex) => wire::CodexSnapshot::decode(body)
            .map(|snapshot| snapshot.asks.iter().map(|ask| format!("{ask:?}")).collect())
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    asks.into_iter().map(|ask| shorten(ask, 1_200)).collect()
}

fn shorten(mut text: String, most: usize) -> String {
    if text.len() > most {
        let mut cut = most;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text.truncate(cut);
        text.push_str(" …");
    }
    text
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    text.len()
        .is_multiple_of(2)
        .then(|| {
            (0..text.len())
                .step_by(2)
                .map(|at| u8::from_str_radix(text.get(at..at + 2)?, 16).ok())
                .collect()
        })
        .flatten()
}

fn faults(
    delay_ms: u64,
    loss_percent: u32,
    jitter_ms: u64,
    reorder_percent: u32,
    rate_bytes_per_sec: u64,
) -> Faults {
    Faults {
        loss_percent,
        delay: Duration::from_millis(delay_ms),
        jitter: Duration::from_millis(jitter_ms),
        reorder_percent,
        rate_bytes_per_sec,
    }
}

/// An attachment as a journey checks it: what it is, its name, and the
/// bytes or the hash that identify its content.
fn attachment(attachment: &wire::Attachment) -> Value {
    use wire::attachment::Of;
    let blob = |kind: &str, blob: &wire::BlobRef| {
        json!({
            "kind": kind,
            "name": blob.name,
            "mime": blob.mime,
            "size": blob.size,
            "hash": hex(&blob.hash),
        })
    };
    match &attachment.of {
        Some(Of::Image(image)) => blob("image", image),
        Some(Of::File(file)) => blob("file", file),
        Some(Of::Text(text)) => json!({ "kind": "text", "name": text.name, "text": text.text }),
        Some(Of::Review(review)) => {
            let patch = review.diff.as_ref().and_then(|diff| diff.patch.as_ref());
            json!({
                "kind": "review",
                "patch": patch.map(|patch| hex(&patch.hash)),
                "comments": review.comments.iter().map(|comment| json!({
                    "path": comment.path,
                    "line": comment.line,
                    "old_line": comment.old_line,
                    "text": comment.text,
                })).collect::<Vec<_>>(),
            })
        }
        None => json!({ "kind": "none" }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::topology::{HostDecl, Topology};

    /// A request can hold the net for minutes (a wait on a real agent); the
    /// flap's periods go on regardless.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_flap_flips_while_a_request_holds_the_net() {
        let mut started = Net::start_with(Topology::new(), crate::NetOptions::default())
            .await
            .unwrap();
        for name in ["desk", "laptop"] {
            started
                .add_host(HostDecl {
                    name: name.to_owned(),
                    ..HostDecl::default()
                })
                .await
                .unwrap();
        }
        started.add_link("desk", "laptop").await.unwrap();
        let net = Arc::new(Mutex::new(Some(started)));
        let flaps = Flaps::default();
        let reply = flap(&net, &flaps, "desk".into(), "laptop".into(), 300, 300).await;
        assert!(matches!(reply, Reply::Ok(_)));

        // Held as a long request holds it, and read only through that hold.
        let held = net.lock().await;
        let held = held.as_ref().unwrap();
        let mut seen = std::collections::BTreeSet::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        while seen.len() < 2 && tokio::time::Instant::now() < deadline {
            seen.insert(held.link_up("desk", "laptop").await);
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(
            seen.len(),
            2,
            "the link went both down and up while the net was held"
        );
        for (_, task) in std::mem::take(&mut *flaps.lock().unwrap()) {
            task.abort();
        }
    }
}
