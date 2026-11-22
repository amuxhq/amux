//! amux's tool server: `amux internal mcp <dir>`, which the harness spawns from the
//! install path and speaks MCP to over stdio.
//!
//! Its identity is its agent's directory. Four fleet tools (list_agents,
//! list_hosts, message_agent, stop_subagent) dial the daemon at `<dir>/tools.sock`, the socket the
//! daemon listens on for this agent alone, so whoever answers knows the
//! caller is this agent from the connection, and nothing in a request
//! claims an id. What the tools read (the fleet, and the agent or host a
//! name means) the daemon answers from what it holds. What they do (a
//! message, a stop) is addressed to the host that owns the agent it acts
//! on, by that host's id as the call's authority: the daemon answers a
//! call for its own host and passes any other on, unread, to the host it
//! names, which applies every rule about the caller against its own row.
//!
//! start_subagent asks the agent process instead, on `<dir>/private/agent.sock`,
//! naming the host by the id the daemon resolved its name to, and the
//! agent process makes the create, addressed to that host, on the same
//! socket ([`create`]): an agent waits for the children it asked for, and
//! this way its process knows of each one before any daemon does, even one
//! made on another host. The other two never reach the daemon:
//! set_working_on returns ok and is observed by the interpreter as the provider's
//! tool-call fact, and attach_file writes the file into `<dir>/blobs/` and
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
use wire::agent_service_client::AgentServiceClient;
use wire::host_service_client::HostServiceClient;
use wire::inventory_service_client::InventoryServiceClient;
use wire::{
    Agent, AgentRef, Attachment, BlobRef, CreateAgentRequest, ExitCause, Filing, HostEntry,
    Lifecycle, PromptInput, PromptSubmission, ResolveAgentRequest, ResolveHostRequest,
    SendMessageRequest, SetFilingRequest, SubscribeInventoryRequest, attachment,
    create_agent_request, inventory_event,
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

/// Every tool this server offers, whether Claude may run it without asking,
/// and whether Claude lists it without a tool search. `attach_file` asks:
/// it reads any file the agent names and copies it into the blob folder,
/// which Claude may read without asking, so letting it through unasked
/// would read files the person's own permission rules deny. Claude's check
/// sees the call before the file is read, as for any read.
///
/// Claude defers a tool server's tools behind its tool search, so a model
/// knows a deferred tool by its name alone until it searches. Four are
/// always loaded: an agent that never loads attach_file writes a path in
/// prose, which no phone can open, and starting, messaging and finding
/// agents is what the server is for. stop_subagent is wanted only after a
/// start, when the agent already has the tools; set_working_on has the
/// provider's own title to fall back on; list_hosts is named in
/// start_subagent's description.
pub(crate) const TOOLS: [Tool; 7] = [
    Tool::new(wire::START_TOOL, true, true),
    Tool::new(wire::MESSAGE_TOOL, true, true),
    Tool::new(wire::AGENTS_TOOL, true, true),
    Tool::new(wire::ATTACH_TOOL, false, true),
    Tool::new(wire::STOP_TOOL, true, false),
    Tool::new(wire::HOSTS_TOOL, true, false),
    Tool::new(wire::WORKING_ON_TOOL, true, false),
];

/// One of the server's tools as [`TOOLS`] lists it.
#[derive(Clone, Copy)]
pub(crate) struct Tool {
    pub(crate) name: &'static str,
    /// Claude may run it without asking.
    pub(crate) unasked: bool,
    /// Claude lists it without a tool search.
    pub(crate) always_loaded: bool,
}

impl Tool {
    const fn new(name: &'static str, unasked: bool, always_loaded: bool) -> Self {
        Self {
            name,
            unasked,
            always_loaded,
        }
    }
}

/// The tools Claude may run without asking.
pub(crate) fn unasked() -> impl Iterator<Item = &'static str> {
    TOOLS
        .into_iter()
        .filter_map(|tool| tool.unasked.then_some(tool.name))
}

const AGENTS: &str = "List the amux fleet: every agent the person has not settled, with its \
name, provider (the agent program it runs: claude, codex or an ACP agent's id), host, whether \
it is live, what it is doing, what it declared it is working on (else its conversation's title), \
its model, effort, permission and branch when known, its parent, and which one is you.";
const HOSTS: &str = "List the hosts this fleet trusts: each one's name, whether it is online, \
which one is this host, and the agents each offers to start, with the models, efforts and \
permission levels each takes, its defaults, and whether it can fork your conversation. \
start_subagent picks from this.";
const SEND: &str = "Send a message to another amux agent by name. It answers once the \
recipient's provider has the message, or says why it could not. A message to one of your own \
subagents continues it, whether it is working or has finished: that is how you give it more \
work. A subagent that was stopped is started again only by its parent. When a message reaches \
you from an amux agent, reply with this tool to that agent's name.";
const SPAWN: &str = "Start a subagent: a child agent that does a task and reports back to you. \
Unlike your provider's own subagents, it shows in the person's fleet, can be claude, codex or \
another agent a host offers (list_hosts lists them with their models, efforts and permission \
levels), can run on another paired host, and outlives your turn. It works in your working \
directory on this host unless you give a cwd; on another host, give a folder there, and ask the \
person which one if you do not know one. Choose its model and effort for the work; permission \
is read-only, ask, auto or full, and defaults to yours, which is also the most you can give. \
new_workspace starts it in a new git worktree on a branch named after it. fork starts it from \
a copy of your conversation so far, with task as its next message: only for the same provider \
as yours, on this host. When it finishes, its last message arrives as a new message in the \
main conversation of the amux agent these tools belong to (a provider subagent of yours that \
calls this starts the child for that agent, and fork copies that agent's conversation, not \
the provider subagent's). Do not poll or wait for it. A later message_agent to it continues \
it.";
const STOP: &str = "Stop one of your own subagents now, done with it: its turn is \
cancelled, it ends, and so do the agents it started. Its history stays, and a later \
message_agent from you starts it again.";
const STATUS: &str = "Set what you are working on, so people and other agents can find the \
right collaborator. Until you set it, the fleet shows your conversation's title. Null clears \
it.";
const ATTACH: &str = "Attach a file from this host to your reply, so every viewer, the phone \
included, can open it: a path written in prose cannot be opened there. Returns an amux \
attachment element; put that element in your reply exactly as returned.";

/// What the server tells the model at initialize. Claude defers some MCP
/// tools behind its tool search, naming them only, but always shows a
/// server's instructions: these say what each tool is for in the words a
/// search for it would use, and stay short, since Claude caps them.
const INSTRUCTIONS: &str = "amux runs a fleet of coding agents, and you are one. Use these \
tools to work with other agents: start a subagent of any provider a host offers, with the \
model, effort, permission and workspace the task needs (start_subagent); message another \
agent, or reply to one that messaged you (message_agent); stop a subagent (stop_subagent); \
list the agents in the fleet (list_agents) and the hosts they run on with what each offers \
(list_hosts); set what you are working on (set_working_on); and attach a file to your reply \
(attach_file).";

/// What the answer to a start tells the parent to do next: a subagent's
/// report arrives as a new message, so a parent that polls only burns its
/// own turns.
const NEXT: &str = "It works on its own and messages you when it finishes: its report \
arrives as a new message in your conversation. Do not poll or wait for it; carry on with \
other work or end your turn.";

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
            wire::MESSAGE_TOOL => self.send(arguments).await,
            wire::START_TOOL => self.spawn(arguments).await,
            wire::STOP_TOOL => self.stop(arguments).await,
            wire::WORKING_ON_TOOL => status(arguments),
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
                    "provider": kind_word(agent),
                    "host": host_name(&agent.host_id),
                    "live": agent.lifecycle() == Lifecycle::Live,
                    "phase": word(agent.phase().as_str_name(), "PHASE_"),
                });
                // What it declared, else the title its provider gave the
                // conversation; never a prompt, which is the person's or
                // the parent's words.
                if let Some(working_on) = agent
                    .working_on
                    .as_ref()
                    .or(agent.title.as_ref())
                    .filter(|said| !said.is_empty())
                {
                    row["working_on"] = json!(working_on);
                }
                if let Some(settings) = &agent.settings {
                    settings_into(&mut row, settings);
                }
                if let Some(branch) = agent.git.as_ref().and_then(|git| git.branch.as_ref()) {
                    row["branch"] = json!(branch);
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
        let me = agents.iter().find(|agent| agent.agent_id == self.me);
        let here = me.map(|agent| agent.host_id.clone());
        let mut rows = Vec::new();
        for host in hosts.iter().filter(|host| host.trusted().is_some()) {
            let presence = match host.reach() {
                Some(wire::trusted::Reach::Online(_)) => "online",
                Some(wire::trusted::Reach::Away(_)) => "away",
                _ => "offline",
            };
            let this_host = here.as_deref() == Some(host.host_id());
            // What the host lists itself is what a start there can name:
            // Claude and Codex, and the ACP agents installed there. What
            // each offers is asked only of a host that can answer now, and
            // asked even when its entry names no copy yet: a host lists
            // Claude and Codex before anyone has asked what they offer.
            let mut offers = Vec::new();
            for provider in &host.providers {
                let catalogue = match this_host || presence == "online" {
                    true => self.catalogue(host.host_id(), &provider.provider_id).await,
                    false => None,
                };
                offers.push(offered(provider, catalogue.as_ref()));
            }
            let mut row = json!({
                "name": host.name(),
                "presence": presence,
                "agents": offers,
            });
            if this_host {
                row["this_host"] = json!(true);
            }
            rows.push(row);
        }
        let mut answer = json!({ "hosts": rows });
        // What a subagent gets when no permission is asked for, and the
        // most it can be given.
        if let Some(level) = me
            .and_then(|me| me.settings.as_ref())
            .and_then(|settings| settings.permission_level().word())
        {
            answer["your_permission"] = json!(level);
        }
        Ok(answer.to_string())
    }

    /// What `provider` offers on the host `host_id`, as that host's copy
    /// says; None when it cannot be read now. Asked of that host itself.
    async fn catalogue(&self, host_id: &[u8], provider: &str) -> Option<wire::Catalogue> {
        use prost::Message as _;
        let host = host_of(host_id).ok()?;
        let request = wire::GetCatalogueRequest {
            provider: provider.to_owned(),
        };
        let answer = dial(
            &self.dir,
            To::Host(host),
            |mut stubs| {
                let request = request.clone();
                async move { stubs.hosts.get_catalogue(request).await }
            },
            true,
        )
        .await
        .ok()?;
        wire::Catalogue::decode(answer.bytes.as_slice()).ok()
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
    /// The host also holds the permission to the caller's own: what is
    /// checked here is only what can be said better here.
    async fn spawn(&self, arguments: &Value) -> Answer {
        let provider = required(arguments, "agent")?;
        let model = optional(arguments, "model");
        let effort = optional(arguments, "effort");
        let agent = agent_named(&provider, model, effort)?;
        let prompt = required(arguments, "task")?;
        let name = optional(arguments, "name");
        // A name is never an address: the daemon resolves it against this
        // host and the hosts it trusts, and the create goes to the id.
        let host = match optional(arguments, "host") {
            Some(name) => Some(self.resolve_host(&name).await?),
            None => None,
        };
        let level = match optional(arguments, "permission") {
            None => wire::PermissionLevel::Unspecified,
            Some(word) => wire::PermissionLevel::from_word(&word).ok_or_else(|| {
                Refusal(format!(
                    "no permission {word:?}; use read-only, ask, auto or full"
                ))
            })?,
        };
        let new_workspace = flag(arguments, "new_workspace")?;
        let fork = flag(arguments, "fork")?;
        let level = match level {
            wire::PermissionLevel::Unspecified => match host {
                // A named host may be another, which may hold no copy of
                // this agent's row yet, so the start names this agent's own
                // level, the default a subagent gets. A host that holds the
                // row checks the start against it either way.
                Some(_) => self.own_level().await,
                None => level,
            },
            // The child's host refuses a level its provider cannot be
            // held to, in words, from what it offers.
            asked => asked,
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
            name,
            cwd,
            agent: Some(agent),
            initial_prompt: Some(PromptSubmission {
                input_id: uuid::Uuid::new_v4().as_bytes().to_vec(),
                prompt: Some(prompt),
            }),
            new_workspace,
            fork,
            permission_level: level as i32,
            ..CreateAgentRequest::default()
        };
        let host = host.map_or_else(Vec::new, |host| host.as_bytes().to_vec());
        let agent = match agent_sock::spawn(&self.dir, &host, &request).await {
            Ok(Spawned::Created(response)) => response.agent.ok_or(NO_AGENT)?,
            Ok(Spawned::Refused(why)) => return Err(Refusal(why)),
            Err(agent_sock::AskError::NotListening(_)) => return Err(NOT_TAKING_SPAWNS.into()),
            Err(agent_sock::AskError::Lost(error)) => {
                return Err(Refusal(format!(
                    "this agent's process went away before confirming the new agent ({error}); it may have asked the daemon for it, so the call was not made again"
                )));
            }
        };
        let mut answer = json!({
            "name": agent.name,
            "id": interpret::to_hex(&agent.agent_id),
            "provider": kind_word(&agent),
        });
        if !agent.cwd.is_empty() {
            answer["cwd"] = json!(agent.cwd);
        }
        if let Some(settings) = &agent.settings {
            settings_into(&mut answer, settings);
        }
        // A new workspace's branch is named after the agent, as its host
        // made it.
        if new_workspace {
            answer["branch"] = json!(agent.name);
        }
        if fork {
            answer["forked"] = json!(true);
        }
        answer["next"] = json!(NEXT);
        Ok(answer.to_string())
    }

    /// This agent's own permission level, as its row says; unspecified
    /// when it cannot be read now or has none.
    async fn own_level(&self) -> wire::PermissionLevel {
        let Ok((_, agents)) = self.inventory().await else {
            return wire::PermissionLevel::Unspecified;
        };
        agents
            .iter()
            .find(|agent| agent.agent_id == self.me)
            .and_then(|me| me.settings.as_ref())
            .map_or(wire::PermissionLevel::Unspecified, |settings| {
                settings.permission_level()
            })
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
        let attachment = store_file(&self.dir, &path, name)?;
        let hash = attachments::blobs(&attachment)
            .first()
            .map(|blob| blob.hash.clone())
            .unwrap_or_default();
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

/// Stores the file at `path` in the blob directory of the agent in `dir`
/// under its hash, as the attachment named `name` that refers to it: an
/// image where the bytes are one a provider reads natively, else a file.
/// Only a regular file is read, its links followed first: a device or a
/// pipe may never end, or may block the call until something writes it. A file over
/// [`wire::ATTACHMENT_SIZE_LIMIT`] is refused unread, and one that grows
/// past it while read is refused at the limit, so memory stays bounded.
fn store_file(dir: &Path, path: &Path, name: String) -> Result<Attachment, String> {
    use std::io::Read as _;

    use sha2::Digest as _;

    let reading = |error: std::io::Error| format!("reading {}: {error}", path.display());
    let limit = wire::ATTACHMENT_SIZE_LIMIT as u64;
    let resolved = std::fs::canonicalize(path).map_err(reading)?;
    let metadata = std::fs::metadata(&resolved).map_err(reading)?;
    if !metadata.is_file() {
        return Err(format!("{} is not a regular file", path.display()));
    }
    if metadata.len() > limit {
        return Err(too_large_words(&name));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(&resolved)
        .map_err(reading)?
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(reading)?;
    if bytes.len() as u64 > limit {
        return Err(too_large_words(&name));
    }
    let hash = sha2::Sha256::digest(&bytes).to_vec();
    dir::write_blob(dir, &hash, &bytes)
        .map_err(|error| format!("storing {}: {error}", path.display()))?;
    let image = attachments::native_image(&bytes);
    let blob = BlobRef {
        hash,
        name,
        mime: image.unwrap_or_else(|| mime_type(path)).to_owned(),
        size: bytes.len() as u64,
    };
    Ok(Attachment {
        of: Some(if image.is_some() {
            attachment::Of::Image(blob)
        } else {
            attachment::Of::File(blob)
        }),
        ..Default::default()
    })
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
/// asked the process for, addressed to the host the ask names by id (this
/// one when it names none) on the socket of the agent in `dir`, under the
/// rule of any call that acts. The daemon passes a create for another host
/// on to it, so the answer comes from the child's own host, which makes the
/// caller, this agent, its parent. The answer is the tool server's, in the
/// words the model reads; beside it, what the agent's core is to hear of
/// the child.
pub(crate) async fn create(
    dir: &Path,
    host_id: &[u8],
    request: CreateAgentRequest,
) -> (Spawned, Spawn) {
    let to = if host_id.is_empty() {
        To::Here
    } else {
        match host_of(host_id) {
            Ok(host) => To::Host(host),
            Err(Refusal(why)) => return (Spawned::Refused(why), Spawn::Refused),
        }
    };
    let made = once(dir, to, "the new agent".to_owned(), |mut stubs| {
        let request = request.clone();
        async move { stubs.agents.create_agent(request).await }
    })
    .await;
    match made {
        Ok(response) => match &response.agent {
            Some(agent) => {
                let incarnation = agent.incarnation;
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
    hosts: HostServiceClient<Channel>,
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
                    inventory: wire::inventory_service_client(channel.clone()),
                    agents: wire::agent_service_client(channel.clone()),
                    hosts: wire::host_service_client(channel),
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

/// An enum value as the tools say it: its protobuf name without the enum's
/// prefix, in lowercase, so the tools' words are the wire's own.
fn word(name: &str, prefix: &str) -> String {
    name.trim_start_matches(prefix).to_ascii_lowercase()
}

/// The agent program a row runs, as the tools say it: its provider id, the
/// one a start names. Both ways of driving Claude are `claude`, for the same
/// reason a parent cannot pick one.
fn kind_word(agent: &Agent) -> String {
    match agent.provider_id() {
        "" => "unknown".to_owned(),
        id => id.to_owned(),
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

/// The agent a start names by its provider id, on the model and effort it
/// names. A Claude child runs headless: people read a child in amux's chat,
/// so how its Claude is driven is no business of the parent's. Any other id
/// is an ACP agent's, which only the host it runs on knows whether it
/// offers, so that host refuses one it does not; an id no agent can have is
/// refused here. The host checks the model and effort against what it
/// offers.
fn agent_named(
    id: &str,
    model: Option<String>,
    effort: Option<String>,
) -> Result<create_agent_request::Agent, Refusal> {
    use create_agent_request::Agent;
    match id {
        wire::agents::CLAUDE => Ok(Agent::ClaudeSdk(wire::ClaudeCreateConfig {
            model,
            effort,
            ..Default::default()
        })),
        wire::agents::CODEX => Ok(Agent::Codex(wire::CodexCreateConfig {
            model,
            effort,
            ..Default::default()
        })),
        _ if wire::agents::shape_problem(id).is_none() => Ok(Agent::Acp(wire::AcpCreateConfig {
            agent: id.to_owned(),
            model,
            effort,
            mode: None,
        })),
        _ => Err(format!(
            "no agent {id:?}; use claude, codex or another agent a host offers, by the id list_hosts lists"
        )
        .into()),
    }
}

/// How an agent runs, into a listing's row or a start's answer: its
/// model, effort and permission, the permission by its level where it has
/// one.
fn settings_into(row: &mut Value, settings: &wire::AgentSettings) {
    if let Some(model) = &settings.model {
        row["model"] = json!(model);
    }
    if let Some(effort) = &settings.effort {
        row["effort"] = json!(effort);
    }
    match (settings.permission_level().word(), &settings.permission) {
        (Some(level), _) => row["permission"] = json!(level),
        (None, Some(value)) => row["permission"] = json!(value),
        (None, None) => {}
    }
}

/// One agent a host offers, as list_hosts says it: its id and name, and
/// from the host's catalogue, when it could be read, its models with their
/// efforts, the permission levels it takes and what it starts on.
fn offered(provider: &wire::ProviderOnHost, catalogue: Option<&wire::Catalogue>) -> Value {
    let id = provider.provider_id.as_str();
    let mut row = json!({ "id": id });
    if let Some(descriptor) = &provider.descriptor
        && !descriptor.name.is_empty()
    {
        row["name"] = json!(descriptor.name);
    }
    if provider.catalogue.is_some() && !provider.signed_in {
        row["signed_in"] = json!(false);
    }
    let forks = provider
        .capabilities
        .as_ref()
        .is_some_and(|capabilities| capabilities.forks);
    row["forks"] = json!(forks);
    let levels: Vec<&str> = wire::levels::levels(&wire::levels::offered(id, catalogue))
        .into_iter()
        .filter_map(wire::PermissionLevel::word)
        .collect();
    row["permissions"] = json!(levels);
    let Some(catalogue) = catalogue else {
        return row;
    };
    let models: Vec<Value> = catalogue
        .models
        .iter()
        .map(|model| {
            let mut entry = json!({ "model": model.value });
            if !model.efforts.is_empty() {
                entry["efforts"] = json!(model.efforts);
            }
            if let Some(effort) = &model.default_effort {
                entry["default_effort"] = json!(effort);
            }
            entry
        })
        .collect();
    row["models"] = json!(models);
    if let Some(defaults) = &catalogue.new_agent {
        if let Some(model) = &defaults.model {
            row["default_model"] = json!(model);
        }
        if let Some(effort) = &defaults.effort {
            row["default_effort"] = json!(effort);
        }
    }
    row
}

/// A yes-or-no argument; absent is no.
fn flag(arguments: &Value, name: &str) -> Result<bool, Refusal> {
    match &arguments[name] {
        Value::Null => Ok(false),
        Value::Bool(value) => Ok(*value),
        _ => Err(Refusal(format!("{name} is true or false"))),
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

/// The seven tools as MCP lists them. A tool Claude lists without a tool
/// search carries Claude's `anthropic/alwaysLoad` metadata; other
/// providers ignore it.
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
    let mut tools = json!([
        { "name": wire::START_TOOL, "description": SPAWN, "inputSchema": object(json!({
            "agent": { "type": "string", "description": "The agent to start: claude, codex, or the id of another agent the host offers (list_hosts lists them)." },
            "task": { "type": "string", "description": "What it is to do: its first message, or with fork its next one." },
            "name": { "type": "string", "pattern": "^[a-z0-9][a-z0-9-]*$", "maxLength": wire::AGENT_NAME_MOST, "description": "Lowercase letters, digits and hyphens; one is chosen when omitted." },
            "host": { "type": "string", "description": "A trusted host's name; this host when omitted." },
            "cwd": { "type": "string", "description": "A path on the host the child runs on; required when that is another host." },
            "model": { "type": "string", "description": "A model the agent offers on that host, as list_hosts names it; the host's default when omitted." },
            "effort": { "type": "string", "description": "An effort the model takes, as list_hosts names it; the host's default when omitted." },
            "permission": { "type": "string", "enum": ["read-only", "ask", "auto", "full"], "description": "How much it may do without asking; yours when omitted, and never more than yours." },
            "new_workspace": { "type": "boolean", "description": "Start it in a new git worktree of cwd's repository, on a branch named after it." },
            "fork": { "type": "boolean", "description": "Start it from a copy of your conversation, with task as its next message. Only for your own provider, on this host." },
          }), &["agent", "task"]) },
        { "name": wire::MESSAGE_TOOL, "description": SEND, "inputSchema": object(json!({
            "to": { "type": "string", "description": "The recipient agent's name." },
            "text": { "type": "string" },
            "context": { "type": "string", "description": "An optional thread label the recipient sees with the message." },
          }), &["to", "text"]) },
        { "name": wire::AGENTS_TOOL, "description": AGENTS, "inputSchema": object(json!({}), &[]),
          "annotations": read_only },
        { "name": wire::ATTACH_TOOL, "description": ATTACH, "inputSchema": object(json!({
            "path": { "type": "string" },
            "name": { "type": "string", "description": "The name viewers see; the file's own name when omitted." },
          }), &["path"]) },
        { "name": wire::STOP_TOOL, "description": STOP, "inputSchema": object(json!({
            "name": { "type": "string" },
          }), &["name"]) },
        { "name": wire::HOSTS_TOOL, "description": HOSTS, "inputSchema": object(json!({}), &[]),
          "annotations": read_only },
        { "name": wire::WORKING_ON_TOOL, "description": STATUS, "inputSchema": object(json!({
            "working_on": { "type": ["string", "null"] },
          }), &["working_on"]) },
    ]);
    for (definition, tool) in tools.as_array_mut().into_iter().flatten().zip(TOOLS) {
        if tool.always_loaded {
            definition["_meta"] = json!({ "anthropic/alwaysLoad": true });
        }
    }
    tools
}

#[cfg(test)]
mod tests {
    /// The table names exactly the tools the server lists, in its order,
    /// so a new tool cannot be offered without deciding whether it asks
    /// first and whether Claude loads it unsearched.
    #[test]
    fn every_listed_tool_is_tabled_and_attach_asks() {
        let definitions = super::definitions();
        let listed: Vec<&str> = definitions
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        let tabled: Vec<&str> = super::TOOLS.iter().map(|tool| tool.name).collect();
        assert_eq!(listed, tabled);
        assert!(!super::unasked().any(|name| name == wire::ATTACH_TOOL));
        let loaded: Vec<&str> = definitions
            .as_array()
            .unwrap()
            .iter()
            .filter(|tool| tool["_meta"]["anthropic/alwaysLoad"] == true)
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            loaded,
            [
                wire::START_TOOL,
                wire::MESSAGE_TOOL,
                wire::AGENTS_TOOL,
                wire::ATTACH_TOOL
            ]
        );
    }
}
