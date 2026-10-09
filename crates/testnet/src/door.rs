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

use crate::net::{ClockMode, JournalCut, Net, NetError, NetOptions};
use crate::observe;
use crate::topology::{AgentDecl, FakeKind, TierDecl, Topology};

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
    },
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
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReadyAgent {
    pub name: String,
    pub host: String,
    pub id: Uuid,
    pub kind: FakeKind,
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
    closed: watch::Receiver<bool>,
    accept: JoinHandle<()>,
}

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
    let (close, closed) = watch::channel(false);
    let close = Arc::new(close);
    let accept = tokio::spawn({
        let net = net.clone();
        async move {
            let mut closing = close.subscribe();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let Ok((stream, _)) = accepted else { return };
                        tokio::spawn(connection(stream, net.clone(), close.clone()));
                    }
                    _ = closing.wait_for(|closed| *closed) => return,
                }
            }
        }
    });
    Ok(Served {
        readiness,
        net,
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
        hosts.push(ReadyHost {
            name: info.name.clone(),
            host_id: info.host_id,
            profile: info.profile,
            front_door: info.front_door.clone(),
            config: info.config.clone(),
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
            })
            .collect(),
        cloud_url: net.relay().ok().map(|relay| relay.url().to_owned()),
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
    close: Arc<watch::Sender<bool>>,
) {
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if line.trim().is_empty() {
            continue;
        }
        let (reply, shutting_down) = match serde_json::from_str::<Control>(&line) {
            Ok(Control::Shutdown) => {
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
        Control::RefuseCalls { a, b, refuse } => value(net.refuse_calls(&a, &b, refuse)?),
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
            let verdict = net.send(&agent, &text).await?;
            json!({ "verdict": format!("{:?}", verdict.of) })
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
        } => value(net.set_lan_faults(
            &host,
            crate::Faults {
                loss_percent,
                delay: Duration::from_millis(delay_ms),
            },
        )?),
        Control::RelayGate {} => json!({ "addr": net.relay_gate().await?.to_string() }),
        Control::RelayFaults {
            delay_ms,
            loss_percent,
        } => value(net.set_relay_faults(crate::Faults {
            loss_percent,
            delay: Duration::from_millis(delay_ms),
        })?),
        Control::Inventory { host } => {
            let mut inventory = net.observe_inventory(&host).await?;
            let events = inventory
                .observe_until(observe::inventory_caught_up, observe::PATIENCE)
                .await?;
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
                    })
                })
                .collect();
            json!({ "agents": agents })
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
                        }));
                    }
                    Some(wire::session_event::Of::Snapshot(snapshot)) => {
                        phase = json!(snapshot.phase().as_str_name().trim_start_matches("PHASE_"));

                        model = json!(running_model(snapshot));
                    }
                    _ => {}
                }
            }
            items.sort_by_key(|item| item["order"].as_u64());
            json!({ "items": items, "phase": phase, "model": model })
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

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
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
