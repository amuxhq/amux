//! The agent verbs: each is one call on the selected profile's client
//! service, and `ls` one read of the inventory to its CaughtUp.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Result, anyhow, bail};
use settings::{ChatInSetting, InstallationConfig};
use uuid::Uuid;
use wire::{
    Agent, AgentInput, ClaudeCreateConfig, CodexCreateConfig, CreateAgentRequest,
    DeleteAgentRequest, DumpRequest, HostEntry, Input, Kind, Lifecycle, Phase, PromptInput,
    PromptSubmission, Refusal, RenameAgentRequest, ResolveAgentRequest, ResumeAgentRequest,
    SendInputRequest, StopAgentRequest, agent_input, create_agent_request, inventory_event,
    send_input_response,
};

type Client = wire::ClientStubs;

/// Every host entry and agent row, read to the inventory's CaughtUp.
pub async fn inventory(client: &mut Client) -> Result<(Vec<HostEntry>, Vec<Agent>)> {
    let mut stream = client
        .agents
        .subscribe_inventory(wire::Empty {})
        .await
        .map_err(crate::plain)?
        .into_inner();
    let mut hosts = BTreeMap::new();
    let mut agents = BTreeMap::new();
    while let Some(event) = stream.message().await.map_err(crate::plain)? {
        match event.of {
            Some(inventory_event::Of::Host(host)) => {
                hosts.insert(host.host_id().to_vec(), host);
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
            // The opening ends at CaughtUp, so a lag cannot come first; if
            // it did, the stream ends here.
            Some(inventory_event::Of::Lagged(_)) | None => {}
        }
    }
    bail!("the inventory ended before it caught up")
}

/// The agent a reference names: any form the daemon resolves (its id, the
/// short id `amux ls` prints, its name, or `name@host`).
pub async fn resolve(client: &mut Client, reference: &str) -> Result<Agent> {
    client
        .agents
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

/// A kind as the listing says it: the agent a person chose, whichever
/// interface it runs for.
fn kind_word(kind: i32) -> &'static str {
    match Kind::try_from(kind).unwrap_or(Kind::Unspecified) {
        Kind::Unspecified => "unknown",
        Kind::ClaudePty | Kind::ClaudeSdk => "claude",
        Kind::Codex => "codex",
    }
}

/// Why an exited agent ended, as `amux ls` says it after "exited", with
/// the cause's own words; None when the cause is unknown and has none.
fn exit_words(agent: &Agent) -> Option<String> {
    use wire::ExitCause;
    let words = match agent.exit_cause() {
        ExitCause::Unspecified => "",
        ExitCause::Finished => "finished",
        ExitCause::Stopped => "stopped",
        ExitCause::Killed => "killed",
        ExitCause::ProviderExited => "the provider exited",
        ExitCause::DaemonLost | ExitCause::EndedWhileAway => "while the daemon was away",
        ExitCause::Orphaned => "orphaned while waiting for you",
        ExitCause::CouldNotStart => "could not start",
        ExitCause::DirectoryFailed => "its directory failed",
        ExitCause::EndedUnexpectedly => "ended unexpectedly",
    };
    match agent
        .exit_detail
        .as_deref()
        .filter(|detail| !detail.is_empty())
    {
        Some(detail) if words.is_empty() => Some(detail.to_owned()),
        Some(detail) => Some(format!("{words}: {detail}")),
        None => (!words.is_empty()).then(|| words.to_owned()),
    }
}

fn state(agent: &Agent) -> String {
    if Lifecycle::try_from(agent.lifecycle) == Ok(Lifecycle::Exited) {
        return match exit_words(agent) {
            Some(cause) => format!("exited: {cause}"),
            None => "exited".to_owned(),
        };
    }
    let phase = match Phase::try_from(agent.phase).unwrap_or(Phase::Unspecified) {
        Phase::Unspecified => "unknown",
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
    print!("{}", ls_table(&hosts, &agents));
    Ok(())
}

fn ls_table(hosts: &[HostEntry], agents: &[Agent]) -> String {
    use std::fmt::Write as _;
    if agents.is_empty() {
        return "No agents.\n".to_owned();
    }
    let host_name = |id: &[u8]| {
        hosts
            .iter()
            .find(|host| host.host_id() == id)
            .map(|host| host.name().to_owned())
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
        walk(root, 0, agents, &mut rows);
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
    let mut out = String::new();
    let _ = writeln!(out, "{:width$}  {:8}  {:7}  STATE", "NAME", "ID", "KIND");
    for (name, (_, agent)) in names.iter().zip(&rows) {
        let _ = writeln!(
            out,
            "{name:width$}  {:8}  {:7}  {}",
            short_id(&agent.agent_id),
            kind_word(agent.kind),
            listed_state(agent, hosts, &host_name(&agent.host_id)),
        );
    }
    out
}

/// A live agent's state as this machine can vouch for it. One whose host
/// is out of reach reads as that, as the clients' rows do: the phase last
/// heard from the host is not what the agent is doing now.
fn listed_state(agent: &Agent, hosts: &[HostEntry], host_name: &str) -> String {
    let live = Lifecycle::try_from(agent.lifecycle) != Ok(Lifecycle::Exited);
    match hosts.iter().find(|host| host.host_id() == agent.host_id) {
        Some(host) if live && host.revoked() => {
            format!("{host_name} no longer trusts this machine")
        }
        Some(host) if live && host.online().is_none() => {
            format!("{host_name} is away")
        }
        _ => state(agent),
    }
}

/// The agents a person chooses between.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum CliAgent {
    Claude,
    Codex,
}

/// What a new agent runs on. Claude runs headless for amux's chat and in a
/// terminal for its own; Codex is the same agent for both.
fn kind_for(agent: CliAgent, interface: ChatInSetting) -> Kind {
    match (agent, interface) {
        (CliAgent::Claude, ChatInSetting::Amux) => Kind::ClaudeSdk,
        (CliAgent::Claude, ChatInSetting::Native) => Kind::ClaudePty,
        (CliAgent::Codex, _) => Kind::Codex,
    }
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

pub fn prompt(text: &str) -> Input {
    let of = AgentInput {
        of: Some(agent_input::Of::Prompt(PromptInput {
            text: text.to_owned(),
            attachments: Vec::new(),
        })),
    };
    Input::new(Uuid::new_v4().as_bytes().to_vec(), of)
}

pub struct Create {
    pub agent: CliAgent,
    pub interface: Option<ChatInSetting>,
    pub name: Option<String>,
    pub cwd: Option<PathBuf>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub permission: Option<String>,
    pub prompt: Option<String>,
    pub args: Vec<String>,
}

/// `amux new`: an agent as the terminal app starts one, with its starting
/// model, effort and permission from `new_agent` where none is given. In
/// the agent's own interface those are its own to set, as in the app.
pub async fn create(
    client: &mut Client,
    config: &InstallationConfig,
    create: Create,
) -> Result<()> {
    let cwd = match create.cwd {
        Some(cwd) => std::path::absolute(cwd)?,
        None => std::env::current_dir()?,
    };
    let interface = create.interface.unwrap_or(config.ui.chat_in);
    let defaults = match create.agent {
        CliAgent::Claude => &config.new_agent.claude,
        CliAgent::Codex => &config.new_agent.codex,
    };
    let configured = |given: &Option<String>, default: &str| {
        given
            .clone()
            .or_else(|| (interface == ChatInSetting::Amux).then(|| default.to_owned()))
    };
    let model = configured(&create.model, &defaults.model);
    let effort = configured(&create.effort, &defaults.effort);
    let permission = configured(&create.permission, &defaults.permission);
    let claude = || ClaudeCreateConfig {
        args: create.args.clone(),
        model: model.clone(),
        effort: effort.clone(),
        permission: permission.clone(),
    };
    let config = match kind_for(create.agent, interface) {
        Kind::ClaudePty => create_agent_request::Agent::ClaudePty(claude()),
        Kind::ClaudeSdk => create_agent_request::Agent::ClaudeSdk(claude()),
        _ => {
            if !create.args.is_empty() {
                bail!("codex agents take no extra provider arguments");
            }
            create_agent_request::Agent::Codex(CodexCreateConfig {
                model: model.clone(),
                effort: effort.clone(),
                permission: permission.clone(),
                mode: None,
            })
        }
    };
    let agent = client
        .agents
        .create_agent(CreateAgentRequest {
            agent_id: Uuid::new_v4().as_bytes().to_vec(),
            name: create.name,
            initial_prompt: create.prompt.as_deref().map(submission),
            cwd: cwd.to_string_lossy().into_owned(),
            agent: Some(config),
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
        .sessions
        .send_input(SendInputRequest {
            agent_id: agent.agent_id.clone(),
            input: Some(prompt(text)),
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
            rejected
                .detail
                .clone()
                .filter(|detail| !detail.is_empty())
                .unwrap_or_else(|| refusal_words(&agent.name, rejected.refusal()))
        )),
        None => Err(anyhow!("the daemon gave no verdict")),
    }
}

/// Why an agent did not take an input, as a sentence about it.
fn refusal_words(name: &str, refusal: Refusal) -> String {
    match refusal {
        Refusal::Unspecified => format!("{name} refused it"),
        Refusal::ClosedAsk => "the question it answers is no longer open".to_owned(),
        Refusal::NotQueued => "the message it names is no longer waiting".to_owned(),
        Refusal::Unsupported => format!("{name} cannot take that input"),
        Refusal::Draining => format!("{name} is finishing its last turn and takes nothing new"),
        Refusal::Exiting => format!("{name} is stopping"),
        Refusal::Exited => {
            format!("{name} has stopped; start it again with `amux resume {name} <text>`")
        }
        Refusal::HostUnreachable => format!("{name}'s host cannot be reached"),
        Refusal::ConversationElsewhere => format!(
            "Claude's own view switched to another conversation, so {name}'s conversation is \
             not the one on its screen"
        ),
        Refusal::TooLong => format!("the prompt is longer than {name} takes"),
    }
}

pub async fn stop(client: &mut Client, reference: &str) -> Result<()> {
    let agent = resolve(client, reference).await?;
    client
        .agents
        .stop_agent(StopAgentRequest {
            agent_id: agent.agent_id.clone(),
        })
        .await
        .map_err(crate::plain)?;
    println!("Stopped {}.", agent.name);
    Ok(())
}

pub async fn delete(client: &mut Client, reference: &str) -> Result<()> {
    let agent = resolve(client, reference).await?;
    client
        .agents
        .delete_agent(DeleteAgentRequest {
            agent_id: agent.agent_id.clone(),
        })
        .await
        .map_err(crate::plain)?;
    println!("Deleted {}.", agent.name);
    Ok(())
}

pub async fn resume(client: &mut Client, reference: &str, text: Option<&str>) -> Result<()> {
    let agent = resolve(client, reference).await?;
    let resumed = client
        .agents
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
        .agents
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
    admin: &mut crate::pairing::Admin,
    references: &[String],
    reason: Option<String>,
    send: bool,
) -> Result<()> {
    let mut agent_ids = Vec::new();
    for reference in references {
        agent_ids.push(resolve(client, reference).await?.agent_id);
    }
    let reason = reason.unwrap_or_default();
    let response = admin
        .dump(DumpRequest {
            agent_ids,
            reason: reason.clone(),
            automatic: false,
            // The command line holds no session open.
            sessions: Vec::new(),
            ..DumpRequest::default()
        })
        .await
        .map_err(crate::plain)?
        .into_inner();
    if !send {
        println!("{}", response.report_path);
        return Ok(());
    }
    let header = serde_json::json!({
        "build": node::release::version(),
        "note": reason,
        "parts": {
            "frame": {"absent": {"reason": "sent from the command line, which has no screen"}},
            "trace": {"absent": {"reason": "sent from the command line, which has no screen"}},
        },
    });
    let sent = admin
        .submit_report(wire::SubmitReportRequest {
            dump_path: response.report_path,
            files: vec![wire::DumpFile {
                name: "report.json".to_owned(),
                contents: serde_json::to_vec_pretty(&header)?,
            }],
        })
        .await
        .map_err(crate::plain)?
        .into_inner();
    println!("{}", sent.report_path);
    if sent.not_sent.is_empty() {
        eprintln!("Sent as report {}.", sent.id);
        Ok(())
    } else {
        anyhow::bail!("not sent: {}", sent.not_sent)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An agent whose host is out of reach reads as its host away, as the
    /// clients' rows do: the last phase this machine heard ("working") is
    /// not what the agent is doing now.
    #[test]
    fn ls_says_an_agent_on_an_away_host_is_away_not_its_last_phase() {
        let host = |id: u8, name: &str, reach: wire::trusted::Reach| {
            HostEntry::trusted_host(vec![id; 16], name, reach)
        };
        let agent = |id: u8, host: u8, name: &str| Agent {
            agent_id: vec![id; 16],
            host_id: vec![host; 16],
            name: name.into(),
            kind: Kind::Codex as i32,
            lifecycle: Lifecycle::Live as i32,
            phase: Phase::Working as i32,
            working_on: Some("Reply with only the word hello.".into()),
            ..Agent::default()
        };
        let table = ls_table(
            &[
                host(
                    1,
                    "pdesk",
                    wire::trusted::Reach::Online(wire::RouteKind::Direct as i32),
                ),
                host(2, "plap", wire::trusted::Reach::Away(wire::Empty {})),
            ],
            &[agent(1, 1, "here"), agent(2, 2, "c1")],
        );
        let line = |name: &str| {
            table
                .lines()
                .find(|line| line.contains(name))
                .unwrap_or_else(|| panic!("{name} is not listed:\n{table}"))
                .to_owned()
        };
        assert!(
            line("pdesk/here").ends_with("working: Reply with only the word hello."),
            "{table}"
        );
        assert!(line("plap/c1").ends_with("plap is away"), "{table}");
    }
}
