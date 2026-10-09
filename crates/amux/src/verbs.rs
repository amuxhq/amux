//! The agent verbs: each is one call on the selected profile's client
//! service, and `ls` one read of the inventory to its CaughtUp.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Result, anyhow, bail};
use prost::Message as _;
use tonic::transport::Channel;
use uuid::Uuid;
use wire::client_service_client::ClientServiceClient;
use wire::{
    Agent, ClaudeCreateConfig, ClaudePtyInput, ClaudeSdkInput, CodexCreateConfig, CodexInput,
    CreateAgentRequest, DeleteAgentRequest, DumpRequest, Empty, HostEntry, Input, Kind, Lifecycle,
    Phase, PromptInput, PromptSubmission, RenameAgentRequest, ResolveAgentRequest,
    ResumeAgentRequest, SendInputRequest, StopAgentRequest, StopMode, claude_pty_input,
    claude_sdk_input, codex_input, inventory_event, send_input_response,
};

type Client = ClientServiceClient<Channel>;

/// Every host entry and agent row, read to the inventory's CaughtUp.
pub async fn inventory(client: &mut Client) -> Result<(Vec<HostEntry>, Vec<Agent>)> {
    let mut stream = client
        .subscribe_inventory(Empty {})
        .await
        .map_err(crate::plain)?
        .into_inner();
    let mut hosts = BTreeMap::new();
    let mut agents = BTreeMap::new();
    while let Some(event) = stream.message().await.map_err(crate::plain)? {
        match event.of {
            Some(inventory_event::Of::Host(host)) => {
                hosts.insert(host.host_id.clone(), host);
            }
            Some(inventory_event::Of::HostRemoved(removed)) => {
                hosts.remove(&removed.host_id);
            }
            Some(inventory_event::Of::Agent(agent)) => {
                agents.insert((agent.host_id.clone(), agent.agent_id.clone()), agent);
            }
            Some(inventory_event::Of::AgentRemoved(removed)) => {
                agents.remove(&(removed.host_id, removed.agent_id));
            }
            Some(inventory_event::Of::CaughtUp(_)) => {
                return Ok((
                    hosts.into_values().collect(),
                    agents.into_values().collect(),
                ));
            }
            None => {}
        }
    }
    bail!("the inventory ended before it caught up")
}

/// The agent a reference names: its id, or its name.
pub async fn resolve(client: &mut Client, reference: &str) -> Result<Agent> {
    if let Ok(id) = Uuid::parse_str(reference) {
        let (_, agents) = inventory(client).await?;
        return agents
            .into_iter()
            .find(|agent| agent.agent_id == id.as_bytes())
            .ok_or_else(|| anyhow!("no agent {id}"));
    }
    client
        .resolve_agent(ResolveAgentRequest {
            name: reference.to_owned(),
        })
        .await
        .map(tonic::Response::into_inner)
        .map_err(crate::plain)
}

fn short_id(id: &[u8]) -> String {
    Uuid::from_slice(id)
        .map(|id| id.simple().to_string()[..8].to_owned())
        .unwrap_or_default()
}

/// A kind as the listing says it: the enum value's own name, lowercase,
/// without the enum's prefix.
fn kind_word(kind: i32) -> String {
    match Kind::try_from(kind).unwrap_or(Kind::Unspecified) {
        Kind::Unspecified => "unknown".to_owned(),
        known => known
            .as_str_name()
            .trim_start_matches("KIND_")
            .to_ascii_lowercase(),
    }
}

fn state(agent: &Agent) -> String {
    if Lifecycle::try_from(agent.lifecycle) == Ok(Lifecycle::Exited) {
        let cause = wire::exit_words(agent.exit_cause, agent.exit_detail.as_deref());
        return match cause.as_str() {
            "" => "exited".to_owned(),
            cause => format!("exited: {cause}"),
        };
    }
    let phase = match Phase::try_from(agent.phase).unwrap_or(Phase::Starting) {
        Phase::Starting => "starting",
        Phase::Idle => "idle",
        Phase::Working => "working",
        Phase::NeedsYou => "needs you",
    };
    match agent.working_on.as_deref() {
        Some(text) if !text.is_empty() => format!("{phase}: {text}"),
        _ => phase.to_owned(),
    }
}

/// `amux ls`: every agent, children beneath their parent.
pub async fn ls(client: &mut Client) -> Result<()> {
    let (hosts, agents) = inventory(client).await?;
    if agents.is_empty() {
        println!("No agents.");
        return Ok(());
    }
    let host_name = |id: &[u8]| {
        hosts
            .iter()
            .find(|host| host.host_id == id)
            .map(|host| host.name.clone())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| short_id(id))
    };
    let several_hosts = agents
        .iter()
        .any(|agent| agent.host_id != agents[0].host_id);
    let is_listed = |parent: &wire::AgentRef| {
        agents
            .iter()
            .any(|agent| agent.host_id == parent.host_id && agent.agent_id == parent.agent_id)
    };
    let mut roots: Vec<&Agent> = agents
        .iter()
        .filter(|agent| !agent.parent.as_ref().is_some_and(is_listed))
        .collect();
    roots.sort_by_key(|agent| agent.created_at_ms);
    let mut rows = Vec::new();
    fn walk<'a>(
        agent: &'a Agent,
        depth: usize,
        all: &'a [Agent],
        rows: &mut Vec<(usize, &'a Agent)>,
    ) {
        rows.push((depth, agent));
        let mut children: Vec<&Agent> = all
            .iter()
            .filter(|child| {
                child.parent.as_ref().is_some_and(|parent| {
                    parent.host_id == agent.host_id && parent.agent_id == agent.agent_id
                })
            })
            .collect();
        children.sort_by_key(|child| child.created_at_ms);
        for child in children {
            walk(child, depth + 1, all, rows);
        }
    }
    for root in roots {
        walk(root, 0, &agents, &mut rows);
    }
    let names: Vec<String> = rows
        .iter()
        .map(|(depth, agent)| {
            let name = agent.name.clone();
            let name = if several_hosts {
                format!("{}/{name}", host_name(&agent.host_id))
            } else {
                name
            };
            format!("{}{name}", "  ".repeat(*depth))
        })
        .collect();
    let width = names.iter().map(String::len).max().unwrap_or(4).max(4);
    println!("{:width$}  {:8}  {:10}  STATE", "NAME", "ID", "KIND");
    for (name, (_, agent)) in names.iter().zip(&rows) {
        println!(
            "{name:width$}  {:8}  {:10}  {}",
            short_id(&agent.agent_id),
            kind_word(agent.kind),
            state(agent),
        );
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub enum CliKind {
    /// Claude in a terminal.
    #[value(name = "claude_pty", alias = "claude-pty", alias = "claude")]
    ClaudePty,
    /// Headless Claude.
    #[value(name = "claude_sdk", alias = "claude-sdk")]
    ClaudeSdk,
    Codex,
}

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub enum CliStopMode {
    /// Let the current turn finish, then exit.
    Graceful,
    /// Cancel the current turn and exit.
    Abort,
    /// End the process group at once.
    Kill,
}

/// A prompt in the input arm the agent's kind takes.
/// A first prompt for a create or a resume.
fn submission(text: &str) -> PromptSubmission {
    PromptSubmission {
        input_id: Uuid::new_v4().as_bytes().to_vec(),
        prompt: Some(PromptInput {
            text: text.to_owned(),
            attachments: Vec::new(),
        }),
    }
}

pub fn prompt(kind: i32, text: &str) -> Input {
    let prompt = PromptInput {
        text: text.to_owned(),
        attachments: Vec::new(),
    };
    let of = match Kind::try_from(kind).unwrap_or(Kind::Unspecified) {
        Kind::Codex => wire::KindInput::Codex(CodexInput {
            of: Some(codex_input::Of::Prompt(prompt)),
        }),
        Kind::ClaudeSdk => wire::KindInput::ClaudeSdk(ClaudeSdkInput {
            of: Some(claude_sdk_input::Of::Prompt(prompt)),
        }),
        Kind::ClaudePty | Kind::Unspecified => wire::KindInput::ClaudePty(ClaudePtyInput {
            of: Some(claude_pty_input::Of::Prompt(prompt)),
        }),
    };
    Input::new(Uuid::new_v4().as_bytes().to_vec(), of)
}

pub struct Create {
    pub kind: CliKind,
    pub name: Option<String>,
    pub cwd: Option<PathBuf>,
    pub model: Option<String>,
    pub prompt: Option<String>,
    pub args: Vec<String>,
}

pub async fn create(client: &mut Client, create: Create) -> Result<()> {
    let cwd = match create.cwd {
        Some(cwd) => std::path::absolute(cwd)?,
        None => std::env::current_dir()?,
    };
    let kind = match create.kind {
        CliKind::ClaudePty => Kind::ClaudePty,
        CliKind::ClaudeSdk => Kind::ClaudeSdk,
        CliKind::Codex => Kind::Codex,
    };
    let config = match kind {
        Kind::Codex => {
            if !create.args.is_empty() {
                bail!("codex agents take no extra provider arguments");
            }
            CodexCreateConfig {
                model: create.model,
                ..CodexCreateConfig::default()
            }
            .encode_to_vec()
        }
        _ => ClaudeCreateConfig {
            args: create.args,
            model: create.model,
            ..ClaudeCreateConfig::default()
        }
        .encode_to_vec(),
    };
    let agent = client
        .create_agent(CreateAgentRequest {
            agent_id: Uuid::new_v4().as_bytes().to_vec(),
            name: create.name,
            initial_prompt: create.prompt.as_deref().map(submission),
            cwd: cwd.to_string_lossy().into_owned(),
            kind: kind as i32,
            config,
            ..CreateAgentRequest::default()
        })
        .await
        .map_err(crate::plain)?
        .into_inner();
    println!(
        "Created {} ({}): {}",
        agent.name,
        Uuid::from_slice(&agent.agent_id)?,
        state(&agent)
    );
    Ok(())
}

pub async fn send(client: &mut Client, reference: &str, text: &str) -> Result<()> {
    let agent = resolve(client, reference).await?;
    let response = client
        .send_input(SendInputRequest {
            agent_id: agent.agent_id.clone(),
            input: Some(prompt(agent.kind, text)),
        })
        .await
        .map_err(crate::plain)?
        .into_inner();
    match response.of {
        Some(send_input_response::Of::Accepted(_)) => {
            println!("Sent to {}.", agent.name);
            Ok(())
        }
        Some(send_input_response::Of::Rejected(rejected)) => Err(anyhow!(
            "{} did not take it: {}",
            agent.name,
            rejected.detail.clone().unwrap_or_else(|| rejected
                .refusal()
                .as_str_name()
                .trim_start_matches("REFUSAL_")
                .to_ascii_lowercase())
        )),
        None => Err(anyhow!("the daemon gave no verdict")),
    }
}

pub async fn stop(client: &mut Client, reference: &str, mode: CliStopMode) -> Result<()> {
    let agent = resolve(client, reference).await?;
    let mode = match mode {
        CliStopMode::Graceful => StopMode::Graceful,
        CliStopMode::Abort => StopMode::Abort,
        CliStopMode::Kill => StopMode::Kill,
    };
    client
        .stop_agent(StopAgentRequest {
            agent_id: agent.agent_id.clone(),
            mode: mode as i32,
        })
        .await
        .map_err(crate::plain)?;
    println!("Stopped {}.", agent.name);
    Ok(())
}

pub async fn delete(client: &mut Client, reference: &str) -> Result<()> {
    let agent = resolve(client, reference).await?;
    let response = client
        .delete_agent(DeleteAgentRequest {
            agent_id: agent.agent_id.clone(),
        })
        .await
        .map_err(crate::plain)?
        .into_inner();
    println!("Deleted {}.", agent.name);
    for child in &response.removed_children {
        println!("Deleted its child {}.", child.name);
    }
    for child in &response.unreachable_children {
        println!(
            "Could not reach its child {}; it stays listed on its own.",
            child.name
        );
    }
    Ok(())
}

pub async fn resume(client: &mut Client, reference: &str, text: Option<&str>) -> Result<()> {
    let agent = resolve(client, reference).await?;
    let resumed = client
        .resume_agent(ResumeAgentRequest {
            agent_id: agent.agent_id.clone(),
            initial_prompt: text.map(submission),
        })
        .await
        .map_err(crate::plain)?
        .into_inner();
    println!("Resumed {}: {}", resumed.name, state(&resumed));
    Ok(())
}

pub async fn rename(client: &mut Client, reference: &str, name: &str) -> Result<()> {
    let agent = resolve(client, reference).await?;
    let renamed = client
        .rename_agent(RenameAgentRequest {
            agent_id: agent.agent_id.clone(),
            name: name.to_owned(),
        })
        .await
        .map_err(crate::plain)?
        .into_inner();
    println!("Renamed {} to {}.", agent.name, renamed.name);
    Ok(())
}

pub async fn dump(
    client: &mut Client,
    references: &[String],
    reason: Option<String>,
) -> Result<()> {
    let mut agent_ids = Vec::new();
    for reference in references {
        agent_ids.push(resolve(client, reference).await?.agent_id);
    }
    let response = client
        .dump(DumpRequest {
            agent_ids,
            reason: reason.unwrap_or_default(),
            automatic: false,
        })
        .await
        .map_err(crate::plain)?
        .into_inner();
    println!("{}", response.report_path);
    Ok(())
}
