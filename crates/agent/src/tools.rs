//! amux's tool server: `amux mcp <dir>`, which the harness spawns from the
//! install path and speaks MCP to over stdio.
//!
//! Its identity is its agent's directory. The fleet tools (agents, hosts,
//! send, spawn, stop) dial the daemon at `<dir>/tools.sock`, the socket the
//! daemon listens on for this agent alone, so the daemon knows who is
//! calling from the connection and nothing in a request claims an id. The
//! other two never reach the daemon: status returns ok and is observed by
//! the interpreter as the provider's tool-call fact, and attach writes the
//! file into `<dir>/blobs/` and returns the element the model puts in its
//! reply. Nothing here writes the journal; the agent process is its one
//! writer.
//!
//! A fleet call made while the daemon is being replaced retries for a few
//! seconds; with no daemon at all it says so plainly.

use std::path::{Path, PathBuf};
use std::time::Duration;

pub use agent_dir::TOOLS_SOCK;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tonic::transport::{Channel, Endpoint};
use tonic::{Code, Status};
use wire::client_service_client::ClientServiceClient;
use wire::{
    Agent, AgentRef, AmbiguousAgentName, Attachment, BlobRef, CreateAgentRequest, Empty, ExitCause,
    HostEntry, HostRef, Input, Interrupt, Kind, Lifecycle, PromptInput, PromptSubmission,
    ResolveAgentRequest, SendInputRequest, SendMessageRequest, Trust, attachment,
    create_agent_request, host_ref, inventory_event, send_input_response,
};

use crate::dir;

/// What a fleet call answers when no daemon is listening.
pub const NOT_RUNNING: &str = "amux daemon isn't running";

/// How long a fleet call keeps redialling a daemon that is not answering:
/// long enough to cover an update's restart.
pub const RETRY_WINDOW: Duration = Duration::from_secs(5);

const PROTOCOL_VERSION: &str = "2025-06-18";

const AGENTS: &str = "List the amux fleet: every agent's name, kind, host, whether it is live, \
what it is doing and working on, its parent, and which one is you.";
const HOSTS: &str = "List the hosts this fleet trusts: each one's name, whether it is online, \
and which one is this host. Spawn on another host by its name.";
const SEND: &str = "Send a message to another amux agent by name. It answers once the \
recipient's provider has the message, or says why it could not. When a message reaches you \
from an amux agent, reply with this tool to that agent's name.";
const SPAWN: &str = "Start a child agent of any kind with a first prompt: claude_pty (Claude \
in a terminal), claude_sdk (headless Claude) or codex. It runs on this host unless you name \
another trusted host, in your working directory unless you give one there. The child does \
its work and stops; you are sent its last message when it finishes, and a later send to it \
resumes it.";
const STOP: &str = "Interrupt the turn one of your own child agents is running. Its history \
stays.";
const STATUS: &str = "Say what you are working on, so people and other agents can find the \
right collaborator. Null clears it.";
const ATTACH: &str = "Attach a file from this host to your reply. Returns an amux attachment \
element; put that element in your reply exactly as returned so every viewer can open the \
file.";

/// How the tool server reaches the daemon; the defaults are the product's.
#[derive(Clone, Debug)]
pub struct ToolsConfig {
    pub retry_window: Duration,
}

impl Default for ToolsConfig {
    fn default() -> Self {
        Self {
            retry_window: RETRY_WINDOW,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ToolsError {
    #[error("reading the harness: {0}")]
    Read(std::io::Error),
    #[error("writing to the harness: {0}")]
    Write(std::io::Error),
}

/// `amux mcp <dir>`: serves the harness on stdin and stdout until it
/// closes stdin.
pub async fn serve_tools(dir: PathBuf) -> Result<(), ToolsError> {
    serve_tools_on(
        dir,
        tokio::io::stdin(),
        tokio::io::stdout(),
        ToolsConfig::default(),
    )
    .await
}

/// Serves one harness on `reader` and `writer`: newline-delimited JSON-RPC,
/// one response per request, in order.
pub async fn serve_tools_on<R, W>(
    dir: PathBuf,
    reader: R,
    mut writer: W,
    config: ToolsConfig,
) -> Result<(), ToolsError>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let server = Server::new(dir, config);
    let mut lines = BufReader::new(reader).lines();
    while let Some(line) = lines.next_line().await.map_err(ToolsError::Read)? {
        if line.trim().is_empty() {
            continue;
        }
        let Some(response) = server.handle(&line).await else {
            continue;
        };
        let mut bytes = serde_json::to_vec(&response).expect("JSON values serialize");
        bytes.push(b'\n');
        writer.write_all(&bytes).await.map_err(ToolsError::Write)?;
        writer.flush().await.map_err(ToolsError::Write)?;
    }
    Ok(())
}

struct Server {
    dir: PathBuf,
    config: ToolsConfig,
    /// This agent's id, from its newest spec; the directory's name is the
    /// same id, but the spec says it in the wire's own bytes.
    me: Vec<u8>,
    /// Where a spawn on this host starts when no cwd is given.
    cwd: String,
}

/// Why a tool call failed, as the model reads it.
struct Refusal(String);

impl From<String> for Refusal {
    fn from(text: String) -> Self {
        Self(text)
    }
}

impl From<&str> for Refusal {
    fn from(text: &str) -> Self {
        Self(text.to_owned())
    }
}

type Answer = Result<String, Refusal>;

impl Server {
    fn new(dir: PathBuf, config: ToolsConfig) -> Self {
        let dir = std::path::absolute(&dir).unwrap_or(dir);
        let spec = dir::newest_spec(&dir).ok().flatten().map(|(_, spec)| spec);
        let cwd = spec
            .as_ref()
            .map(|spec| spec.cwd.clone())
            .filter(|cwd| !cwd.is_empty())
            .or_else(|| {
                std::env::current_dir()
                    .ok()
                    .map(|cwd| cwd.display().to_string())
            })
            .unwrap_or_default();
        Self {
            dir,
            config,
            me: spec.map(|spec| spec.agent_id).unwrap_or_default(),
            cwd,
        }
    }

    /// One JSON-RPC message in; the response, or nothing for a
    /// notification.
    async fn handle(&self, line: &str) -> Option<Value> {
        let request: Value = match serde_json::from_str(line) {
            Ok(request) => request,
            Err(error) => {
                return Some(rpc_error(
                    Value::Null,
                    -32700,
                    &format!("parse error: {error}"),
                ));
            }
        };
        let id = request.get("id")?.clone();
        let params = request.get("params").cloned().unwrap_or(Value::Null);
        Some(match request["method"].as_str().unwrap_or_default() {
            "initialize" => rpc_result(
                id,
                json!({
                    "protocolVersion": params["protocolVersion"]
                        .as_str()
                        .unwrap_or(PROTOCOL_VERSION),
                    "capabilities": { "tools": { "listChanged": false } },
                    "serverInfo": { "name": interpret::AMUX_TOOL_SERVER, "version": crate::VERSION },
                }),
            ),
            "ping" => rpc_result(id, json!({})),
            "tools/list" => rpc_result(id, json!({ "tools": definitions() })),
            "tools/call" => {
                let name = params["name"].as_str().unwrap_or_default();
                let arguments = match &params["arguments"] {
                    Value::Null => json!({}),
                    arguments => arguments.clone(),
                };
                match self.call(name, &arguments).await {
                    Some(answer) => tool_result(id, answer),
                    None => rpc_error(id, -32602, &format!("no tool named {name}")),
                }
            }
            method => rpc_error(id, -32601, &format!("method not found: {method}")),
        })
    }

    async fn call(&self, name: &str, arguments: &Value) -> Option<Answer> {
        Some(match name {
            "agents" => self.agents().await,
            "hosts" => self.hosts().await,
            "send" => self.send(arguments).await,
            "spawn" => self.spawn(arguments).await,
            "stop" => self.stop(arguments).await,
            interpret::STATUS_TOOL => status(arguments),
            "attach" => self.attach(arguments),
            _ => return None,
        })
    }

    // --- fleet tools -----------------------------------------------------

    async fn agents(&self) -> Answer {
        let (hosts, agents) = self.inventory().await?;
        let host_name = |id: &[u8]| {
            hosts
                .iter()
                .find(|host| host.host_id == id)
                .map_or_else(|| interpret::to_hex(id), |host| host.name.clone())
        };
        let agent_name = |parent: &AgentRef| {
            agents
                .iter()
                .find(|agent| agent.agent_id == parent.agent_id)
                .map_or_else(
                    || interpret::to_hex(&parent.agent_id),
                    |agent| agent.name.clone(),
                )
        };
        let rows = agents
            .iter()
            .map(|agent| {
                let mut row = json!({
                    "name": agent.name,
                    "id": interpret::to_hex(&agent.agent_id),
                    "kind": kind_word(agent.kind()),
                    "host": host_name(&agent.host_id),
                    "live": agent.lifecycle() == Lifecycle::Live,
                    "phase": word(agent.phase().as_str_name(), "PHASE_"),
                });
                if let Some(working_on) = &agent.working_on {
                    row["working_on"] = json!(working_on);
                }
                if let Some(cause) = exit_words(agent) {
                    row["exit_cause"] = json!(cause);
                }
                if let Some(parent) = &agent.parent {
                    row["parent"] = json!(agent_name(parent));
                }
                if agent.agent_id == self.me {
                    row["you"] = json!(true);
                }
                row
            })
            .collect::<Vec<_>>();
        Ok(json!({ "agents": rows }).to_string())
    }

    async fn hosts(&self) -> Answer {
        let (hosts, agents) = self.inventory().await?;
        let here = agents
            .iter()
            .find(|agent| agent.agent_id == self.me)
            .map(|agent| agent.host_id.clone());
        let rows = hosts
            .iter()
            .filter(|host| host.trust() == Trust::Trusted)
            .map(|host| {
                let mut row = json!({
                    "name": host.name,
                    "presence": word(host.presence().as_str_name(), "PRESENCE_"),
                });
                if here.as_deref() == Some(host.host_id.as_slice()) {
                    row["this_host"] = json!(true);
                }
                row
            })
            .collect::<Vec<_>>();
        Ok(json!({ "hosts": rows }).to_string())
    }

    /// `{"id": "<hex>"}` once the recipient's provider has the message: the
    /// shape the interpreters read the send's envelope id from.
    async fn send(&self, arguments: &Value) -> Answer {
        let to = required(arguments, "to")?;
        let text = required(arguments, "text")?;
        let context = arguments["context"].as_str().map(|context| context.into());
        let recipient = self.resolve(&to).await?;
        let request = SendMessageRequest {
            id: uuid::Uuid::new_v4().as_bytes().to_vec(),
            to: Some(AgentRef {
                host_id: recipient.host_id,
                agent_id: recipient.agent_id,
            }),
            text,
            context,
            attachments: Vec::new(),
        };
        let sent = self
            .daemon_once(format!("the message to {to}"), |mut client| {
                let request = request.clone();
                async move { client.send_message(request).await }
            })
            .await
            .map_err(|refusal| refusal.or_else(|status| refused(&status, &to)))?;
        let id = if sent.envelope_id.is_empty() {
            request.id
        } else {
            sent.envelope_id
        };
        Ok(json!({ "id": interpret::to_hex(&id) }).to_string())
    }

    /// The parent is not in the request: the daemon sets it from the
    /// socket the call came in on, as it sets a message's sender.
    async fn spawn(&self, arguments: &Value) -> Answer {
        let kind = required(arguments, "kind")?;
        let Some(kind) = kind_named(&kind) else {
            let [first, second, last] = SPAWNABLE.map(kind_word);
            return Err(format!("no agent kind {kind:?}; use {first}, {second} or {last}").into());
        };
        let prompt = required(arguments, "prompt")?;
        let name = optional(arguments, "name");
        // The daemon resolves the name against the hosts it trusts.
        let host = optional(arguments, "host");
        let cwd = optional(arguments, "cwd").unwrap_or_else(|| match host {
            // A named host may be this one or another; the daemon picks:
            // this agent's directory here, the home directory elsewhere.
            Some(_) => String::new(),
            None => self.cwd.clone(),
        });
        let prompt = PromptInput {
            text: prompt,
            attachments: Vec::new(),
        };
        let request = CreateAgentRequest {
            agent_id: uuid::Uuid::new_v4().as_bytes().to_vec(),
            host: host.map(|name| HostRef {
                of: Some(host_ref::Of::Name(name)),
            }),
            name,
            parent: None,
            cwd,
            agent: create_agent_request::Agent::defaults(kind),
            initial_prompt: Some(PromptSubmission {
                input_id: uuid::Uuid::new_v4().as_bytes().to_vec(),
                prompt: Some(prompt),
            }),
            new_worktree: false,
        };
        let agent = self
            .daemon_once("the new agent".to_owned(), |mut client| {
                let request = request.clone();
                async move { client.create_agent(request).await }
            })
            .await
            .map_err(|refusal| refusal.or_else(|status| Refusal(plain(&status))))?;
        Ok(json!({
            "name": agent.name,
            "id": interpret::to_hex(&agent.agent_id),
        })
        .to_string())
    }

    /// Only a direct child: the daemon checks the lineage and refuses the
    /// rest.
    async fn stop(&self, arguments: &Value) -> Answer {
        let name = required(arguments, "name")?;
        let child = self.resolve(&name).await?;
        let request = SendInputRequest {
            agent_id: child.agent_id.clone(),
            input: Some(Input::new(
                uuid::Uuid::new_v4().as_bytes().to_vec(),
                interrupt_input(child.kind()),
            )),
        };
        let response = self
            .daemon_once(format!("the stop for {name}"), |mut client| {
                let request = request.clone();
                async move { client.send_input(request).await }
            })
            .await
            .map_err(|refusal| refusal.or_else(|status| refused(&status, &name)))?;
        match response.of {
            Some(send_input_response::Of::Rejected(rejected)) => {
                let why = rejected
                    .detail
                    .clone()
                    .unwrap_or_else(|| word(rejected.refusal().as_str_name(), "REFUSAL_"));
                Err(format!("{name} did not stop: {why}").into())
            }
            _ => Ok("{}".to_owned()),
        }
    }

    async fn resolve(&self, name: &str) -> Result<Agent, Refusal> {
        let request = ResolveAgentRequest {
            name: name.to_owned(),
        };
        self.daemon(|mut client| {
            let request = request.clone();
            async move { client.resolve_agent(request).await }
        })
        .await
        .map_err(|status| refused(&status, name))
    }

    /// The inventory read to CaughtUp: every host entry and agent row.
    async fn inventory(&self) -> Result<(Vec<HostEntry>, Vec<Agent>), Refusal> {
        self.daemon(|mut client| async move {
            let mut stream = client.subscribe_inventory(Empty {}).await?.into_inner();
            let (mut hosts, mut agents) = (Vec::<HostEntry>::new(), Vec::<Agent>::new());
            while let Some(event) = stream.message().await? {
                match event.of {
                    Some(inventory_event::Of::Host(host)) => {
                        hosts.retain(|known| known.host_id != host.host_id);
                        hosts.push(host);
                    }
                    Some(inventory_event::Of::HostRemoved(removed)) => {
                        hosts.retain(|known| known.host_id != removed.host_id);
                    }
                    Some(inventory_event::Of::Agent(agent)) => {
                        agents.retain(|known| {
                            (&known.host_id, &known.agent_id) != (&agent.host_id, &agent.agent_id)
                        });
                        agents.push(agent);
                    }
                    Some(inventory_event::Of::AgentRemoved(removed)) => {
                        agents.retain(|known| {
                            (&known.host_id, &known.agent_id)
                                != (&removed.host_id, &removed.agent_id)
                        });
                    }
                    Some(inventory_event::Of::CaughtUp(_)) => {
                        return Ok(tonic::Response::new((hosts, agents)));
                    }
                    // The opening ends at CaughtUp, so a lag cannot come
                    // first; if it did, the stream ends here.
                    Some(inventory_event::Of::Lagged(_)) | None => {}
                }
            }
            Err(Status::unavailable(
                "the inventory ended before it caught up",
            ))
        })
        .await
        .map_err(|status| Refusal(plain(&status)))
    }

    /// Runs a read against the daemon, dialling afresh each attempt. A
    /// daemon that is not there, or goes away mid-call, is retried until
    /// the window closes; then the call reports that no daemon is running.
    async fn daemon<T, F, Fut>(&self, call: F) -> Result<T, Status>
    where
        F: Fn(ClientServiceClient<Channel>) -> Fut,
        Fut: std::future::Future<Output = Result<tonic::Response<T>, Status>>,
    {
        self.dial(call, true).await.map_err(|(status, _)| status)
    }

    /// Runs a call the daemon acts on (a send, spawn or stop) at most
    /// once. Only dialling is retried over the window: once the call has
    /// gone out, the daemon may have acted on it even if the answer never
    /// came, so it is never made again on the model's behalf. `Ok(Err)`
    /// is the daemon's own refusal.
    async fn daemon_once<T, F, Fut>(&self, what: String, call: F) -> Result<T, Settled>
    where
        F: Fn(ClientServiceClient<Channel>) -> Fut,
        Fut: std::future::Future<Output = Result<tonic::Response<T>, Status>>,
    {
        match self.dial(call, false).await {
            Ok(answer) => Ok(answer),
            Err((status, true)) if lost(&status) => Err(Settled::Unconfirmed(format!(
                "the daemon went away before confirming {what}; it may have taken it, so the call was not made again"
            ))),
            Err((status, _)) => Err(Settled::Refused(status)),
        }
    }

    /// The error says whether the call reached the daemon.
    async fn dial<T, F, Fut>(&self, call: F, resend: bool) -> Result<T, (Status, bool)>
    where
        F: Fn(ClientServiceClient<Channel>) -> Fut,
        Fut: std::future::Future<Output = Result<tonic::Response<T>, Status>>,
    {
        let deadline = tokio::time::Instant::now() + self.config.retry_window;
        let mut backoff = Duration::from_millis(50);
        loop {
            let failed = match connect(&self.dir.join(TOOLS_SOCK)).await {
                Ok(channel) => match call(wire::client_service_client(channel)).await {
                    Ok(response) => return Ok(response.into_inner()),
                    Err(status) if resend && lost(&status) => (status, true),
                    Err(status) => return Err((status, true)),
                },
                Err(_) => (Status::unavailable(NOT_RUNNING), false),
            };
            if tokio::time::Instant::now() + backoff > deadline {
                return Err(failed);
            }
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(Duration::from_millis(500));
        }
    }

    // --- local tools -----------------------------------------------------

    /// Stores the file in this agent's directory under its hash and answers
    /// the element that refers to it.
    fn attach(&self, arguments: &Value) -> Answer {
        use sha2::Digest as _;

        let path = PathBuf::from(required(arguments, "path")?);
        let path = if path.is_absolute() {
            path
        } else {
            Path::new(&self.cwd).join(path)
        };
        let bytes =
            std::fs::read(&path).map_err(|error| format!("reading {}: {error}", path.display()))?;
        let hash = sha2::Sha256::digest(&bytes).to_vec();
        dir::write_blob(&self.dir, &hash, &bytes)
            .map_err(|error| format!("storing {}: {error}", path.display()))?;
        let name = optional(arguments, "name").unwrap_or_else(|| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default()
        });
        let mime = mime_type(&path);
        let blob = BlobRef {
            hash: hash.clone(),
            name,
            mime: mime.to_owned(),
            size: bytes.len() as u64,
        };
        let attachment = Attachment {
            of: Some(if mime.starts_with("image/") {
                attachment::Of::Image(blob)
            } else {
                attachment::Of::File(blob)
            }),
        };
        let stored = self.dir.join(dir::BLOBS).join(interpret::to_hex(&hash));
        Ok(attachments::element(&attachment, Some(&stored)))
    }
}

/// The status call is the provider's fact; there is nothing to do but
/// accept it.
fn status(arguments: &Value) -> Answer {
    match arguments.get("working_on") {
        Some(Value::String(_) | Value::Null) => Ok("ok".to_owned()),
        _ => Err("working_on must be a string, or null to clear it".into()),
    }
}

async fn connect(sock: &Path) -> std::io::Result<Channel> {
    let sock = sock.to_owned();
    Endpoint::from_static("http://amux-tools")
        .connect_with_connector(tower::service_fn(move |_| {
            let sock = sock.clone();
            async move {
                crate::local_socket::connect(&sock)
                    .await
                    .map(hyper_util::rt::TokioIo::new)
            }
        }))
        .await
        .map_err(std::io::Error::other)
}

/// How a call the daemon acts on ended without an answer.
enum Settled {
    /// The daemon refused it.
    Refused(Status),
    /// It went out and no answer came back.
    Unconfirmed(String),
}

impl Settled {
    fn or_else(self, refused: impl FnOnce(Status) -> Refusal) -> Refusal {
        match self {
            Settled::Refused(status) => refused(status),
            Settled::Unconfirmed(text) => Refusal(text),
        }
    }
}

/// The daemon went away: it said it is unavailable, or the connection
/// failed under the call.
fn lost(status: &Status) -> bool {
    status.code() == Code::Unavailable
        || std::error::Error::source(status)
            .is_some_and(|source| source.is::<tonic::transport::Error>())
}

/// A daemon refusal as the model reads it; an unknown or ambiguous name
/// says which.
fn refused(status: &Status, name: &str) -> Refusal {
    if status.code() == Code::NotFound {
        return Refusal(format!("no agent named {name}"));
    }
    match ambiguous(status) {
        Some(ambiguous) if !ambiguous.candidates.is_empty() => Refusal(format!(
            "{name} names several agents: {}",
            ambiguous
                .candidates
                .iter()
                .map(|agent| format!("{} ({})", agent.name, interpret::to_hex(&agent.agent_id)))
                .collect::<Vec<_>>()
                .join(", ")
        )),
        _ => Refusal(plain(status)),
    }
}

/// The candidates an ambiguous name matched, from the status's details: an
/// encoded Error whose details name their message types.
fn ambiguous(status: &Status) -> Option<AmbiguousAgentName> {
    use prost::{Message as _, Name as _};

    wire::Error::decode(status.details())
        .ok()?
        .details
        .into_iter()
        .find(|detail| detail.r#type == AmbiguousAgentName::full_name())
        .and_then(|detail| AmbiguousAgentName::decode(detail.value.as_slice()).ok())
}

fn plain(status: &Status) -> String {
    if status.code() == Code::Unavailable {
        return NOT_RUNNING.to_owned();
    }
    match status.message() {
        "" => status.code().description().to_owned(),
        message => message.to_owned(),
    }
}

/// The kinds an agent may spawn.
const SPAWNABLE: [Kind; 3] = [Kind::ClaudePty, Kind::ClaudeSdk, Kind::Codex];

/// An enum value as the tools say it: its protobuf name without the enum's
/// prefix, in lowercase, so the tools' words are the wire's own.
fn word(name: &str, prefix: &str) -> String {
    name.trim_start_matches(prefix).to_ascii_lowercase()
}

/// A kind as the tools say it: `claude_pty`, `claude_sdk`, `codex`.
fn kind_word(kind: Kind) -> String {
    match kind {
        Kind::Unspecified => "unknown".to_owned(),
        known => word(known.as_str_name(), "KIND_"),
    }
}

/// How an exited agent ended, as the tools say it: the cause's wire name,
/// then its own words when it has any (`provider_exited: exit code 1`).
/// None while it runs, and for a cause this build does not know with no
/// words of its own. A row still live can already hold the cause its
/// journal ended with, until its daemon marks it exited.
fn exit_words(agent: &Agent) -> Option<String> {
    if agent.lifecycle() == Lifecycle::Live {
        return None;
    }
    let cause = match agent.exit_cause() {
        ExitCause::Unspecified => None,
        known => Some(word(known.as_str_name(), "EXIT_CAUSE_")),
    };
    let detail = agent
        .exit_detail
        .clone()
        .filter(|detail| !detail.is_empty());
    match (cause, detail) {
        (Some(cause), Some(detail)) => Some(format!("{cause}: {detail}")),
        (cause, detail) => cause.or(detail),
    }
}

/// The kind an agent named in a spawn call.
fn kind_named(word: &str) -> Option<Kind> {
    SPAWNABLE.into_iter().find(|kind| kind_word(*kind) == word)
}

fn interrupt_input(kind: Kind) -> wire::KindInput {
    match kind {
        Kind::Codex => wire::KindInput::Codex(wire::CodexInput {
            of: Some(wire::codex_input::Of::Interrupt(Interrupt {})),
        }),
        Kind::ClaudeSdk => wire::KindInput::ClaudeSdk(wire::ClaudeSdkInput {
            of: Some(wire::claude_sdk_input::Of::Interrupt(Interrupt {})),
        }),
        _ => wire::KindInput::ClaudePty(wire::ClaudePtyInput {
            of: Some(wire::claude_pty_input::Of::Interrupt(Interrupt {})),
        }),
    }
}

fn required(arguments: &Value, name: &str) -> Result<String, Refusal> {
    optional(arguments, name).ok_or_else(|| Refusal(format!("{name} is required")))
}

fn optional(arguments: &Value, name: &str) -> Option<String> {
    arguments[name]
        .as_str()
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
}

/// The type a viewer opens the file as, from its extension.
fn mime_type(path: &Path) -> &'static str {
    let extension = path
        .extension()
        .map(|extension| extension.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    match extension.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "pdf" => "application/pdf",
        "json" => "application/json",
        "html" | "htm" => "text/html",
        "csv" => "text/csv",
        "md" => "text/markdown",
        "diff" | "patch" => "text/x-diff",
        "txt" | "log" | "rs" | "py" | "js" | "ts" | "swift" | "go" | "toml" | "yaml" | "yml"
        | "sh" | "c" | "h" | "cpp" | "java" | "kt" | "rb" | "css" | "xml" => "text/plain",
        _ => "application/octet-stream",
    }
}

fn tool_result(id: Value, answer: Answer) -> Value {
    let (text, is_error) = match answer {
        Ok(text) => (text, false),
        Err(Refusal(text)) => (text, true),
    };
    rpc_result(
        id,
        json!({ "content": [{ "type": "text", "text": text }], "isError": is_error }),
    )
}

fn rpc_result(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// The seven tools as MCP lists them.
fn definitions() -> Value {
    let object = |properties: Value, required: &[&str]| {
        json!({
            "type": "object",
            "properties": properties,
            "required": required,
            "additionalProperties": false,
        })
    };
    let read_only = json!({ "readOnlyHint": true });
    json!([
        { "name": "agents", "description": AGENTS, "inputSchema": object(json!({}), &[]),
          "annotations": read_only },
        { "name": "hosts", "description": HOSTS, "inputSchema": object(json!({}), &[]),
          "annotations": read_only },
        { "name": interpret::SEND_TOOL, "description": SEND, "inputSchema": object(json!({
            "to": { "type": "string", "description": "The recipient agent's name." },
            "text": { "type": "string" },
            "context": { "type": "string", "description": "An optional thread label the recipient sees with the message." },
          }), &["to", "text"]) },
        { "name": "spawn", "description": SPAWN, "inputSchema": object(json!({
            "kind": { "type": "string", "enum": SPAWNABLE.map(kind_word) },
            "prompt": { "type": "string" },
            "name": { "type": "string", "pattern": "^[a-z0-9][a-z0-9-]*$", "maxLength": wire::AGENT_NAME_MOST, "description": "Lowercase letters, digits and hyphens; one is chosen when omitted." },
            "cwd": { "type": "string", "description": "A path on the host the child runs on." },
            "host": { "type": "string", "description": "A trusted host's name; this host when omitted." },
          }), &["kind", "prompt"]) },
        { "name": "stop", "description": STOP, "inputSchema": object(json!({
            "name": { "type": "string" },
          }), &["name"]) },
        { "name": interpret::STATUS_TOOL, "description": STATUS, "inputSchema": object(json!({
            "working_on": { "type": ["string", "null"] },
          }), &["working_on"]) },
        { "name": "attach", "description": ATTACH, "inputSchema": object(json!({
            "path": { "type": "string" },
            "name": { "type": "string", "description": "The name viewers see; the file's own name when omitted." },
          }), &["path"]) },
    ])
}
