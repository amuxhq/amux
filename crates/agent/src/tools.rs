//! amux's tool server: `amux internal mcp <dir>`, which the harness spawns from the
//! install path and speaks MCP to over stdio.
//!
//! Its identity is its agent's directory. Four fleet tools (agents, hosts,
//! send, stop) dial the daemon at `<dir>/tools.sock`, the socket the
//! daemon listens on for this agent alone, so whoever answers knows the
//! caller is this agent from the connection, and nothing in a request
//! claims an id. What the tools read (the fleet, and the agent or host a
//! name means) the daemon answers from what it holds. What they do (a
//! message, a stop) is addressed to the host that owns the agent it acts
//! on, by that host's id as the call's authority: the daemon answers a
//! call for its own host and passes any other on, unread, to the host it
//! names, which applies every rule about the caller against its own row.
//!
//! Spawn asks the agent process instead, on `<dir>/private/agent.sock`,
//! naming the host by the id the daemon resolved its name to, and the
//! agent process makes the create, addressed to that host, on the same
//! socket ([`create`]): an agent waits for the children it asked for, and
//! this way its process knows of each one before any daemon does, even one
//! made on another host. The other two never reach the daemon: status
//! returns ok and is observed by the interpreter as the provider's
//! tool-call fact, and attach writes the file into `<dir>/blobs/` and
//! returns the element the model puts in its reply. Nothing here writes
//! the journal; the agent process is its one writer.
//!
//! A fleet call made while the daemon is being replaced retries for a few
//! seconds; with no daemon at all it says so plainly, and so does a spawn
//! whose agent process is not listening.

use std::path::{Path, PathBuf};
use std::time::Duration;

use agent_core::Spawn;
pub use agent_dir::TOOLS_SOCK;
use agent_dir::agent_sock::{self, Spawned};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tonic::transport::{Channel, Endpoint};
use tonic::{Code, Status};
use uuid::Uuid;
use wire::amux::staging::agent_service_client::AgentServiceClient;
use wire::amux::staging::inventory_service_client::InventoryServiceClient;
use wire::amux::staging::{
    self, ResolveAgentRequest, ResolveHostRequest, SendMessageRequest, SetFilingRequest,
    SubscribeInventoryRequest,
};
use wire::{
    Agent, AgentRef, Attachment, BlobRef, CreateAgentRequest, ExitCause, Filing, HostEntry,
    HostRef, Kind, Lifecycle, PromptInput, PromptSubmission, attachment, create_agent_request,
    host_ref, inventory_event,
};

use crate::dir;

/// What a fleet call answers when no daemon is listening.
pub const NOT_RUNNING: &str = "amux daemon isn't running";

/// What a spawn answers when its agent process has no socket for its tool
/// server: the process was started by a build from before spawns went
/// through it.
pub const NOT_TAKING_SPAWNS: &str = "this agent's process is not taking spawn requests";

/// How long a fleet call keeps redialling a daemon that is not answering:
/// long enough to cover an update's restart.
pub const RETRY_WINDOW: Duration = Duration::from_secs(5);

/// What a call answers when the daemon's answer holds no agent row.
const NO_AGENT: &str = "the daemon's answer named no agent";

/// What a call answers when the daemon's answer names no host it could be
/// addressed to.
const NO_HOST: &str = "the daemon's answer named no host";

/// The authority of a call answered by the daemon serving the socket, for
/// itself: anything that is not a host id is.
const HERE: &str = "amux-tools";

const PROTOCOL_VERSION: &str = "2025-06-18";

/// Every tool this server offers, and whether Claude may run it without
/// asking. `attach` asks: it reads any file the agent names and copies it
/// into the blob folder, which Claude may read without asking, so letting it
/// through unasked would read files the person's own permission rules deny.
/// Claude's check sees the call before the file is read, as for any read.
pub(crate) const TOOLS: [(&str, bool); 7] = [
    (wire::AGENTS_TOOL, true),
    (wire::HOSTS_TOOL, true),
    (wire::SEND_TOOL, true),
    (wire::SPAWN_TOOL, true),
    (wire::STOP_TOOL, true),
    (wire::STATUS_TOOL, true),
    (wire::ATTACH_TOOL, false),
];

/// The tools Claude may run without asking.
pub(crate) fn unasked() -> impl Iterator<Item = &'static str> {
    TOOLS
        .into_iter()
        .filter_map(|(name, unasked)| unasked.then_some(name))
}

const AGENTS: &str = "List the amux fleet: every agent the person has not settled, with its \
name, kind, host, whether it is live, what it is doing and working on, its parent, and which \
one is you.";
const HOSTS: &str = "List the hosts this fleet trusts: each one's name, whether it is online, \
and which one is this host. Spawn on another host by its name.";
const SEND: &str = "Send a message to another amux agent by name. It answers once the \
recipient's provider has the message, or says why it could not. When a message reaches you \
from an amux agent, reply with this tool to that agent's name.";
const SPAWN: &str = "Start a child agent with a first prompt: claude or codex. It runs on \
this host unless you name \
another trusted host, in your working directory unless you give one. On another host, give \
a folder there: ask the person which one if you do not know one. The child does \
its work and stops. When it finishes, its last message arrives as a new message in the main \
conversation of the amux agent these tools belong to: a subagent that spawns one is not sent \
it, its top-level agent is. A later send to the child resumes it.";
const STOP: &str = "Stop one of your own child agents now, done with it: its turn is \
cancelled, it ends, and so do the agents it started. Its history stays, and a later send from \
you resumes it.";
const STATUS: &str = "Say what you are working on, so people and other agents can find the \
right collaborator. Null clears it.";
const ATTACH: &str = "Attach a file from this host to your reply. Returns an amux attachment \
element; put that element in your reply exactly as returned so every viewer can open the \
file.";

/// What the server tells the model at initialize. Claude defers MCP tools
/// behind its tool search, naming them only, but always shows a server's
/// instructions: these say what each tool is for in the words a search
/// for it would use, and stay short, since Claude caps them.
const INSTRUCTIONS: &str = "amux runs a fleet of coding agents, and you are one. Use these \
tools to work with other agents: send a message to another agent, or reply to one that \
messaged you (send); start a child agent, claude or codex, on this or another host (spawn); \
stop a child agent (stop); list the agents in the fleet (agents) and the hosts they run on \
(hosts); say what you are working on (status); and attach a file to your reply (attach).";

#[derive(Debug, thiserror::Error)]
pub enum ToolsError {
    #[error("reading the harness: {0}")]
    Read(std::io::Error),
    #[error("writing to the harness: {0}")]
    Write(std::io::Error),
}

/// `amux internal mcp <dir>`: serves the harness on stdin and stdout until it
/// closes stdin.
pub async fn serve_tools(dir: PathBuf) -> Result<(), ToolsError> {
    serve_tools_on(dir, tokio::io::stdin(), tokio::io::stdout()).await
}

/// Serves one harness on `reader` and `writer`: newline-delimited JSON-RPC,
/// one response per request, in order.
pub async fn serve_tools_on<R, W>(dir: PathBuf, reader: R, mut writer: W) -> Result<(), ToolsError>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let server = Server::new(dir);
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
    fn new(dir: PathBuf) -> Self {
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
                    "instructions": INSTRUCTIONS,
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
            wire::AGENTS_TOOL => self.agents().await,
            wire::HOSTS_TOOL => self.hosts().await,
            wire::SEND_TOOL => self.send(arguments).await,
            wire::SPAWN_TOOL => self.spawn(arguments).await,
            wire::STOP_TOOL => self.stop(arguments).await,
            wire::STATUS_TOOL => status(arguments),
            wire::ATTACH_TOOL => self.attach(arguments),
            _ => return None,
        })
    }

    // --- fleet tools -----------------------------------------------------

    async fn agents(&self) -> Answer {
        let (hosts, agents) = self.inventory().await?;
        let host_name = |id: &[u8]| {
            hosts
                .iter()
                .find(|host| host.host_id() == id)
                .map_or_else(|| interpret::to_hex(id), |host| host.name().to_owned())
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
        // The person is done with a closed agent, so it is not offered as
        // one to work with; naming it still answers why it will not take a
        // message. A provider subagent (Claude's Agent tool, a Codex child
        // thread) takes no messages at all: only its parent's model reaches
        // it, through the provider's own tools, so it is not listed.
        let rows = agents
            .iter()
            .filter(|agent| agent.filing() != Filing::Closed && agent.native.is_none())
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
            .filter(|host| host.trusted().is_some())
            .map(|host| {
                let presence = match host.reach() {
                    Some(wire::trusted::Reach::Online(_)) => "online",
                    Some(wire::trusted::Reach::Away(_)) => "away",
                    _ => "offline",
                };
                let mut row = json!({
                    "name": host.name(),
                    "presence": presence,
                });
                if here.as_deref() == Some(host.host_id()) {
                    row["this_host"] = json!(true);
                }
                row
            })
            .collect::<Vec<_>>();
        Ok(json!({ "hosts": rows }).to_string())
    }

    /// `{"id": "<hex>"}` once the recipient's provider has the message: the
    /// shape the interpreters read the send's envelope id from. The message
    /// goes to the recipient's host, and names no sender: the caller is.
    async fn send(&self, arguments: &Value) -> Answer {
        let to = required(arguments, "to")?;
        let text = required(arguments, "text")?;
        let context = arguments["context"].as_str().map(|context| context.into());
        let recipient = self.resolve(&to).await?;
        let host = host_of(&recipient.host_id)?;
        // A file the model attached (the attach tool's element) travels as
        // an attachment, not as text naming a path on this host: the
        // recipient's host fetches its bytes and its agent reads them at a
        // path of its own.
        let positioned = attachments::parse(&text).positioned;
        // Measured as stored, not as the element says: the model wrote the
        // element, and the recipient's host fetches every byte.
        for attachment in &positioned.attachments {
            self.within_limit(attachment)?;
        }
        let request = SendMessageRequest {
            id: Uuid::new_v4().as_bytes().to_vec(),
            agent_id: recipient.agent_id,
            context,
            message: Some(wire::MessageBody {
                text: positioned.text,
                attachments: positioned.attachments,
            }),
        };
        let sent = once(
            &self.dir,
            To::Host(host),
            format!("the message to {to}"),
            |mut stubs| {
                let request = request.clone();
                async move { stubs.agents.send_message(request).await }
            },
        )
        .await
        .map_err(|refusal| refusal.or_else(|status| refused(&status, &to)))?;
        let id = if sent.envelope_id.is_empty() {
            request.id
        } else {
            sent.envelope_id
        };
        Ok(json!({ "id": interpret::to_hex(&id) }).to_string())
    }

    /// Asked of the agent process, which asks the child's host. The parent
    /// is not in the request: the host sets it from the caller, this agent,
    /// as it sets a message's sender.
    async fn spawn(&self, arguments: &Value) -> Answer {
        let kind = required(arguments, "kind")?;
        let Some(kind) = kind_named(&kind) else {
            let [first, last] = SPAWNABLE.map(kind_word);
            return Err(format!("no agent kind {kind:?}; use {first} or {last}").into());
        };
        let prompt = required(arguments, "prompt")?;
        let name = optional(arguments, "name");
        // A name is never an address: the daemon resolves it against this
        // host and the hosts it trusts, and the create goes to the id.
        let host = match optional(arguments, "host") {
            Some(name) => Some(self.resolve_host(&name).await?),
            None => None,
        };
        let cwd = optional(arguments, "cwd").unwrap_or_else(|| match host {
            // A named host may be this one or another, and the host decides:
            // this agent's directory here, a refusal asking for a folder
            // elsewhere.
            Some(_) => String::new(),
            None => self.cwd.clone(),
        });
        let prompt = PromptInput {
            text: prompt,
            attachments: Vec::new(),
        };
        let request = CreateAgentRequest {
            agent_id: uuid::Uuid::new_v4().as_bytes().to_vec(),
            host: host.map(|host| HostRef::id(host.as_bytes().to_vec())),
            name,
            parent: None,
            cwd,
            agent: create_agent_request::Agent::defaults(kind),
            initial_prompt: Some(PromptSubmission {
                input_id: uuid::Uuid::new_v4().as_bytes().to_vec(),
                prompt: Some(prompt),
            }),
            ..CreateAgentRequest::default()
        };
        let agent = match agent_sock::spawn(&self.dir, &request).await {
            Ok(Spawned::Created(response)) => response.agent.ok_or(NO_AGENT)?,
            Ok(Spawned::Refused(why)) => return Err(Refusal(why)),
            Err(agent_sock::AskError::NotListening(_)) => return Err(NOT_TAKING_SPAWNS.into()),
            Err(agent_sock::AskError::Lost(error)) => {
                return Err(Refusal(format!(
                    "this agent's process went away before confirming the new agent ({error}); it may have asked the daemon for it, so the call was not made again"
                )));
            }
        };
        Ok(json!({
            "name": agent.name,
            "id": interpret::to_hex(&agent.agent_id),
        })
        .to_string())
    }

    /// A real stop, as a person's close: the child ends now, filed away as
    /// done with, and its host closes its descendants on every host with
    /// it. Only a direct child: the child's host checks the lineage against
    /// its own row and refuses the rest. Answers once the child has ended.
    async fn stop(&self, arguments: &Value) -> Answer {
        let name = required(arguments, "name")?;
        let child = self.resolve(&name).await?;
        let host = host_of(&child.host_id)?;
        let request = SetFilingRequest {
            agent_id: child.agent_id.clone(),
            filing: Filing::Closed as i32,
        };
        once(
            &self.dir,
            To::Host(host),
            format!("the stop for {name}"),
            |mut stubs| {
                let request = request.clone();
                async move { stubs.agents.set_filing(request).await }
            },
        )
        .await
        .map_err(|refusal| {
            refusal.or_else(|status| Refusal(format!("{name} did not stop: {}", plain(&status))))
        })?;
        Ok("{}".to_owned())
    }

    async fn resolve(&self, name: &str) -> Result<Agent, Refusal> {
        let request = ResolveAgentRequest {
            name: name.to_owned(),
        };
        self.daemon(|mut stubs| {
            let request = request.clone();
            async move { stubs.inventory.resolve_agent(request).await }
        })
        .await
        .map_err(|status| refused(&status, name))?
        .agent
        .ok_or_else(|| NO_AGENT.into())
    }

    /// The host a person's name for it means, among this host and the
    /// hosts the daemon trusts. The daemon's refusal names the hosts it
    /// knows.
    async fn resolve_host(&self, name: &str) -> Result<Uuid, Refusal> {
        let request = ResolveHostRequest {
            name: name.to_owned(),
        };
        let host = self
            .daemon(|mut stubs| {
                let request = request.clone();
                async move { stubs.inventory.resolve_host(request).await }
            })
            .await
            .map_err(|status| Refusal(plain(&status)))?
            .host
            .ok_or(NO_HOST)?;
        host_of(host.host_id())
    }

    /// The inventory read to CaughtUp: every host entry and agent row.
    async fn inventory(&self) -> Result<(Vec<HostEntry>, Vec<Agent>), Refusal> {
        self.daemon(|mut stubs| async move {
            let mut stream = stubs
                .inventory
                .subscribe_inventory(SubscribeInventoryRequest {})
                .await?
                .into_inner();
            let (mut hosts, mut agents) = (Vec::<HostEntry>::new(), Vec::<Agent>::new());
            while let Some(response) = stream.message().await? {
                match response.event.and_then(|event| event.of) {
                    Some(inventory_event::Of::Host(host)) => {
                        hosts.retain(|known| known.host_id() != host.host_id());
                        hosts.push(host);
                    }
                    Some(inventory_event::Of::HostRemoved(removed)) => {
                        hosts.retain(|known| known.host_id() != removed.host_id);
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

    /// Runs a read of what the daemon holds, dialling afresh each attempt.
    /// A daemon that is not there, or goes away mid-call, is retried until
    /// the window closes; then the call reports that no daemon is running.
    async fn daemon<T, F, Fut>(&self, call: F) -> Result<T, Status>
    where
        F: Fn(Stubs) -> Fut,
        Fut: std::future::Future<Output = Result<tonic::Response<T>, Status>>,
    {
        dial(&self.dir, To::Here, call, true)
            .await
            .map_err(|(status, _)| status)
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
        let name = optional(arguments, "name").unwrap_or_else(|| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default()
        });
        let size = std::fs::metadata(&path)
            .map_err(|error| format!("reading {}: {error}", path.display()))?
            .len();
        if size > wire::ATTACHMENT_SIZE_LIMIT as u64 {
            return Err(too_large_words(&name).into());
        }
        let bytes =
            std::fs::read(&path).map_err(|error| format!("reading {}: {error}", path.display()))?;
        let hash = sha2::Sha256::digest(&bytes).to_vec();
        dir::write_blob(&self.dir, &hash, &bytes)
            .map_err(|error| format!("storing {}: {error}", path.display()))?;
        let image = attachments::native_image(&bytes);
        let blob = BlobRef {
            hash: hash.clone(),
            name,
            mime: image.unwrap_or_else(|| mime_type(&path)).to_owned(),
            size: bytes.len() as u64,
        };
        let attachment = Attachment {
            of: Some(if image.is_some() {
                attachment::Of::Image(blob)
            } else {
                attachment::Of::File(blob)
            }),
        };
        let stored = self.dir.join(dir::BLOBS).join(interpret::to_hex(&hash));
        Ok(attachments::element(&attachment, Some(&stored)))
    }

    /// Refuses an attachment whose stored blob in this agent's directory is
    /// over [`wire::ATTACHMENT_SIZE_LIMIT`], in the words a person's
    /// over-large file gets.
    fn within_limit(&self, attachment: &Attachment) -> Result<(), Refusal> {
        for blob in attachments::blobs(attachment) {
            let stored = self
                .dir
                .join(dir::BLOBS)
                .join(interpret::to_hex(&blob.hash));
            let size = std::fs::metadata(&stored).map_or(0, |meta| meta.len());
            if size > wire::ATTACHMENT_SIZE_LIMIT as u64 {
                let name = if blob.name.is_empty() {
                    "the attachment"
                } else {
                    &blob.name
                };
                return Err(too_large_words(name).into());
            }
        }
        Ok(())
    }
}

/// Why a file is not attached: it is over the limit, in the unit a person
/// reads (the limit is 64 MiB of the file's own bytes).
fn too_large_words(name: &str) -> String {
    format!(
        "{name} is too large to attach: files can be at most {} MB",
        wire::ATTACHMENT_SIZE_LIMIT / (1024 * 1024)
    )
}

/// The status call is the provider's fact; there is nothing to do but
/// accept it.
fn status(arguments: &Value) -> Answer {
    match arguments.get("working_on") {
        Some(Value::String(_) | Value::Null) => Ok("ok".to_owned()),
        _ => Err("working_on must be a string, or null to clear it".into()),
    }
}

/// The agent process's half of a spawn: makes the child its tool server
/// asked the process for, addressed to the host the ask names (this one
/// when it names none) on the socket of the agent in `dir`, under the rule
/// of any call that acts. The daemon passes a create for another host on to
/// it, so the answer comes from the child's own host, which makes the
/// caller, this agent, its parent. The answer is the tool server's, in the
/// words the model reads; beside it, what the agent's core is to hear of
/// the child.
pub(crate) async fn create(dir: &Path, request: CreateAgentRequest) -> (Spawned, Spawn) {
    let to = match request.host.as_ref().and_then(|host| host.of.as_ref()) {
        None => To::Here,
        Some(host_ref::Of::HostId(host)) => match host_of(host) {
            Ok(host) => To::Host(host),
            Err(Refusal(why)) => return (Spawned::Refused(why), Spawn::Refused),
        },
        // The tool server resolves a host's name before it asks: a name is
        // never an address.
        Some(host_ref::Of::Name(name)) => {
            return (
                Spawned::Refused(format!(
                    "a spawn names its host by id, not as {name:?}: the tool server resolves the name"
                )),
                Spawn::Refused,
            );
        }
    };
    let request = reshaped(request);
    let made = once(dir, to, "the new agent".to_owned(), |mut stubs| {
        let request = request.clone();
        async move { stubs.agents.create_agent(request).await }
    })
    .await;
    match made {
        Ok(response) => match response.agent {
            Some(agent) => {
                let incarnation = agent.incarnation;
                let response = wire::CreateAgentResponse { agent: Some(agent) };
                (
                    Spawned::Created(Box::new(response)),
                    Spawn::Created { incarnation },
                )
            }
            None => (Spawned::Refused(NO_AGENT.to_owned()), Spawn::Refused),
        },
        Err(Settled::Refused(status)) => (Spawned::Refused(plain(&status)), Spawn::Refused),
        Err(Settled::Unconfirmed(text)) => (Spawned::Refused(text), Spawn::Unconfirmed),
    }
}

/// The create an ask describes, as its host takes it: no host, since the
/// call's address is the host, and no parent, since the caller is.
fn reshaped(request: CreateAgentRequest) -> staging::CreateAgentRequest {
    use staging::create_agent_request::Agent as Asked;
    staging::CreateAgentRequest {
        agent_id: request.agent_id,
        name: request.name,
        cwd: request.cwd,
        agent: request.agent.map(|agent| match agent {
            create_agent_request::Agent::ClaudePty(config) => Asked::ClaudePty(config),
            create_agent_request::Agent::ClaudeSdk(config) => Asked::ClaudeSdk(config),
            create_agent_request::Agent::Codex(config) => Asked::Codex(config),
        }),
        initial_prompt: request.initial_prompt,
        new_workspace: request.new_workspace,
        remember_model: request.remember_model,
        remember_effort: request.remember_effort,
    }
}

/// The host an id names, as a call to it is addressed.
fn host_of(host_id: &[u8]) -> Result<Uuid, Refusal> {
    Uuid::from_slice(host_id).map_err(|_| NO_HOST.into())
}

/// Whom a call on the tools socket is addressed to.
#[derive(Clone, Copy, Debug)]
enum To {
    /// The daemon serving the socket, for what it holds and for its own
    /// host.
    Here,
    /// A host by its id: the daemon's own answers it, and any other is
    /// passed the call by the daemon.
    Host(Uuid),
}

impl To {
    fn authority(self) -> String {
        match self {
            To::Here => HERE.to_owned(),
            To::Host(host) => wire::authority(&host),
        }
    }
}

/// The services a tool calls, on one connection to the tools socket whose
/// calls are all addressed to one host.
#[derive(Clone, Debug)]
struct Stubs {
    inventory: InventoryServiceClient<Channel>,
    agents: AgentServiceClient<Channel>,
}

/// Runs a call that acts (a send, spawn or stop) at most once. Only
/// dialling is retried over the window: once the call has gone out, the
/// daemon or the host it passed the call to may have acted on it even if
/// the answer never came, so it is never made again on the model's behalf.
async fn once<T, F, Fut>(dir: &Path, to: To, what: String, call: F) -> Result<T, Settled>
where
    F: Fn(Stubs) -> Fut,
    Fut: std::future::Future<Output = Result<tonic::Response<T>, Status>>,
{
    match dial(dir, to, call, false).await {
        Ok(answer) => Ok(answer),
        Err((status, true)) if lost(&status) => Err(Settled::Unconfirmed(format!(
            "the daemon went away before confirming {what}; it may have taken it, so the call was not made again"
        ))),
        Err((status, _)) => Err(Settled::Refused(status)),
    }
}

/// Dials the daemon serving the agent in `dir` afresh each attempt, until
/// the window closes, and makes `call` addressed `to`. The error says
/// whether the call reached the daemon.
async fn dial<T, F, Fut>(dir: &Path, to: To, call: F, resend: bool) -> Result<T, (Status, bool)>
where
    F: Fn(Stubs) -> Fut,
    Fut: std::future::Future<Output = Result<tonic::Response<T>, Status>>,
{
    let deadline = tokio::time::Instant::now() + RETRY_WINDOW;
    let mut backoff = Duration::from_millis(50);
    loop {
        let failed = match connect(&dir.join(TOOLS_SOCK), to).await {
            Ok(channel) => {
                let stubs = Stubs {
                    inventory: staging::inventory_service_client(channel.clone()),
                    agents: staging::agent_service_client(channel),
                };
                match call(stubs).await {
                    Ok(response) => return Ok(response.into_inner()),
                    Err(status) if resend && lost(&status) => (status, true),
                    Err(status) => return Err((status, true)),
                }
            }
            Err(_) => (Status::unavailable(NOT_RUNNING), false),
        };
        if tokio::time::Instant::now() + backoff > deadline {
            return Err(failed);
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(Duration::from_millis(500));
    }
}

/// A connection to the socket at `sock` whose calls are addressed `to`.
/// A channel gives every call on it the authority it was built for, so a
/// call to another host takes a channel of its own on the same socket.
async fn connect(sock: &Path, to: To) -> std::io::Result<Channel> {
    let sock = sock.to_owned();
    Endpoint::from_shared(format!("http://{}", to.authority()))
        .map_err(std::io::Error::other)?
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

/// A daemon refusal as the model reads it. The daemon's own words say
/// which name was unknown, or for a name agents on two hosts share, the
/// `name@host` forms that pick each.
fn refused(status: &Status, name: &str) -> Refusal {
    match (status.code(), status.message()) {
        (Code::NotFound, "") => Refusal(format!("no agent named {name}")),
        _ => Refusal(plain(status)),
    }
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

/// The kinds an agent may spawn. A Claude child runs headless: people read
/// a child in amux's chat, so how its Claude is driven is no business of
/// the parent's.
const SPAWNABLE: [Kind; 2] = [Kind::ClaudeSdk, Kind::Codex];

/// An enum value as the tools say it: its protobuf name without the enum's
/// prefix, in lowercase, so the tools' words are the wire's own.
fn word(name: &str, prefix: &str) -> String {
    name.trim_start_matches(prefix).to_ascii_lowercase()
}

/// A kind as the tools say it: `claude` or `codex`. Both ways of driving
/// Claude are `claude`, for the same reason a parent cannot pick one.
fn kind_word(kind: Kind) -> String {
    match kind {
        Kind::Unspecified => "unknown".to_owned(),
        Kind::ClaudePty | Kind::ClaudeSdk => "claude".to_owned(),
        Kind::Codex => "codex".to_owned(),
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

fn required(arguments: &Value, name: &str) -> Result<String, Refusal> {
    optional(arguments, name).ok_or_else(|| Refusal(format!("{name} is required")))
}

fn optional(arguments: &Value, name: &str) -> Option<String> {
    arguments[name]
        .as_str()
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
}

/// The type a viewer opens a file that is not a native image as, from its
/// extension.
fn mime_type(path: &Path) -> &'static str {
    let extension = path
        .extension()
        .map(|extension| extension.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    match extension.as_str() {
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
        { "name": wire::AGENTS_TOOL, "description": AGENTS, "inputSchema": object(json!({}), &[]),
          "annotations": read_only },
        { "name": wire::HOSTS_TOOL, "description": HOSTS, "inputSchema": object(json!({}), &[]),
          "annotations": read_only },
        { "name": wire::SEND_TOOL, "description": SEND, "inputSchema": object(json!({
            "to": { "type": "string", "description": "The recipient agent's name." },
            "text": { "type": "string" },
            "context": { "type": "string", "description": "An optional thread label the recipient sees with the message." },
          }), &["to", "text"]) },
        { "name": wire::SPAWN_TOOL, "description": SPAWN, "inputSchema": object(json!({
            "kind": { "type": "string", "enum": SPAWNABLE.map(kind_word) },
            "prompt": { "type": "string" },
            "name": { "type": "string", "pattern": "^[a-z0-9][a-z0-9-]*$", "maxLength": wire::AGENT_NAME_MOST, "description": "Lowercase letters, digits and hyphens; one is chosen when omitted." },
            "cwd": { "type": "string", "description": "A path on the host the child runs on; required when that is another host." },
            "host": { "type": "string", "description": "A trusted host's name; this host when omitted." },
          }), &["kind", "prompt"]) },
        { "name": wire::STOP_TOOL, "description": STOP, "inputSchema": object(json!({
            "name": { "type": "string" },
          }), &["name"]) },
        { "name": wire::STATUS_TOOL, "description": STATUS, "inputSchema": object(json!({
            "working_on": { "type": ["string", "null"] },
          }), &["working_on"]) },
        { "name": wire::ATTACH_TOOL, "description": ATTACH, "inputSchema": object(json!({
            "path": { "type": "string" },
            "name": { "type": "string", "description": "The name viewers see; the file's own name when omitted." },
          }), &["path"]) },
    ])
}

#[cfg(test)]
mod tests {
    /// The approval table names exactly the tools the server lists, so a new
    /// tool cannot be offered without deciding whether it asks first.
    #[test]
    fn every_listed_tool_has_an_approval_and_attach_asks() {
        let listed: Vec<String> = super::definitions()
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap().to_owned())
            .collect();
        let tabled: Vec<String> = super::TOOLS
            .iter()
            .map(|(name, _)| (*name).to_owned())
            .collect();
        assert_eq!(listed, tabled);
        assert!(!super::unasked().any(|name| name == wire::ATTACH_TOOL));
    }
}
