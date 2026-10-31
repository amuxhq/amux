use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use codex_protocol::client::{
    CommandApprovalResponse, DynamicTool, InjectedContent, InjectedItem, InjectedMessage,
    ThreadListParams, ThreadResumeParams, ThreadStartParams, ToolCallResponse, TurnStartParams,
};
use codex_protocol::items::{MessagePhase, TextContent, ThreadItem, ToolOutputContent};
use codex_protocol::server::{
    CommandDecision, Decision, ErrorNotification, ServerNotification, ServerRequest,
};
use codex_protocol::thread::{ApprovalPolicy, AskForApproval, SandboxMode, TurnStatus};
use codex_protocol::{ClientResponse, Extra, Turn};
use replay_support::{ReplayAdvance, SpecEntry, StrictReplay};
use semver::Version;

use crate::client::{Codex, CodexConfig, Event, Thread, ThreadEvent, ThreadEventStream};

pub const MINIMUM_SUPPORTED: &str = "0.150.1";
pub const CAPTURE_MODEL: &str = "gpt-5.6-luna";
const ALLOWED_MODELS: &[&str] = &[CAPTURE_MODEL];
const EVENT_TIMEOUT: Duration = Duration::from_secs(300);
const LIVE_IO_FILE: &str = "spec.io.jsonl";

#[path = "decisions.rs"]
mod decisions;
#[path = "rows.rs"]
mod rows;
#[path = "two_clients.rs"]
mod two_clients;

pub use decisions::SPEC_TOOL_SERVER;
pub use two_clients::TWO_CLIENTS;

/// Specifications that run with no signed-in account; capture gives them a
/// Codex home without credentials.
pub const SIGNED_OUT: &[&str] = &["signed_out"];

/// Specifications whose Codex home registers the spec tool server.
pub const WITH_TOOL_SERVER: &[&str] = &["tool_server_form"];

const REGISTRY: &[SpecEntry] = &[
    entry("initialize_and_start"),
    entry("turn_round_trip"),
    entry("approval_allow"),
    entry("approval_deny"),
    entry("interrupt"),
    entry("thread_list_and_resume"),
    entry("dynamic_tools"),
    entry("inject_idle"),
    entry("inject_busy"),
    entry("two_assistant_messages"),
    entry("inject_drain"),
    entry("reasoning_summary"),
    entry("exploring"),
    entry("failing_command"),
    entry("file_changes"),
    entry("image"),
    entry("compaction"),
    entry("plan_mode"),
    entry("turn_error"),
    entry("signed_out"),
    entry("web_search"),
    entry("file_moved"),
    entry("subagent"),
    entry("background_terminal"),
    entry("tool_server_form"),
    entry("access_grant"),
    entry("automatic_review"),
    entry("approval_scopes"),
    entry("turn_retries"),
    entry("two_clients_prompt"),
    entry("two_clients_approval"),
    entry("two_clients_steer"),
    entry("two_clients_join_fresh"),
];

const fn entry(name: &'static str) -> SpecEntry {
    SpecEntry {
        name,
        recording: name,
        allowed_models: ALLOWED_MODELS,
    }
}

pub fn registry() -> &'static [SpecEntry] {
    REGISTRY
}

pub fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/runtime")
}

/// Live captures amux's own interpreter drove, which no specification
/// replays.
pub fn live_fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/live")
}

pub fn live_io_path(codex_home: &Path) -> PathBuf {
    codex_home.join(LIVE_IO_FILE)
}

pub enum SpecSource {
    Live { codex_home: PathBuf, model: String },
    Recorded(StrictReplay),
}

#[derive(Debug, thiserror::Error)]
#[error("specification {spec} failed: {claim}")]
pub struct SpecFailure {
    pub spec: String,
    pub claim: String,
}

#[derive(Clone, Debug)]
pub struct RunReport {
    pub provider_version: Option<Version>,
    pub server_model: String,
    pub session_ids: Vec<String>,
}

struct ScenarioReport {
    server_model: String,
    session_ids: Vec<String>,
}

struct Runtime {
    codex: Codex,
    /// The second client, for the two-client specifications.
    other: Option<Codex>,
    #[cfg(unix)]
    server: Option<two_clients::live::Server>,
    model: String,
    project: PathBuf,
    replay_driver: Option<tokio::task::JoinHandle<()>>,
}

pub async fn run(spec: &SpecEntry, source: SpecSource) -> Result<(), SpecFailure> {
    execute(spec, source).await.map(|_| ())
}

/// Runs a specification and returns capture metadata used by `codex-probe`.
#[doc(hidden)]
pub async fn execute(spec: &SpecEntry, source: SpecSource) -> Result<RunReport, SpecFailure> {
    if !REGISTRY.contains(spec) {
        return Err(failure(spec, "specification is not in the Codex registry"));
    }

    let mut runtime = open_runtime(spec, source).await?;
    let initialization = runtime.codex.initialization_result().cloned();
    let scenario = match &runtime.other {
        Some(other) => {
            two_clients::run(
                spec.name,
                &runtime.codex,
                other,
                &runtime.model,
                &runtime.project,
            )
            .await
        }
        None => run_scenario(spec.name, &runtime.codex, &runtime.model, &runtime.project).await,
    };
    let replay_exhausted = if let Some(mut driver) = runtime.replay_driver.take() {
        let exhausted = tokio::time::timeout(Duration::from_secs(5), &mut driver)
            .await
            .is_ok();
        if !exhausted {
            driver.abort();
        }
        exhausted
    } else {
        true
    };
    runtime.codex.close().await;
    if let Some(other) = runtime.other.take() {
        other.close().await;
    }
    #[cfg(unix)]
    if let Some(server) = runtime.server.take() {
        server.stop().await;
    }
    if !replay_exhausted {
        return Err(failure(
            spec,
            "strict replay driver did not reach exhaustion",
        ));
    }

    let scenario = scenario.map_err(|claim| failure(spec, claim))?;
    let provider_version = initialization
        .as_ref()
        .and_then(|result| version_from_user_agent(&result.user_agent));

    Ok(RunReport {
        provider_version,
        server_model: scenario.server_model,
        session_ids: scenario.session_ids,
    })
}

async fn open_runtime(spec: &SpecEntry, source: SpecSource) -> Result<Runtime, SpecFailure> {
    match source {
        SpecSource::Live { codex_home, model } => {
            if !spec.allowed_models.contains(&model.as_str()) {
                return Err(failure(
                    spec,
                    format!("model {model} is not allowed; expected {CAPTURE_MODEL}"),
                ));
            }
            let project = codex_home.parent().unwrap_or(&codex_home).join("project");
            std::fs::create_dir_all(&project).map_err(|error| failure(spec, error))?;
            let io_path = live_io_path(&codex_home);
            if io_path.exists() {
                std::fs::remove_file(&io_path).map_err(|error| failure(spec, error))?;
            }
            if TWO_CLIENTS.contains(&spec.name) {
                return open_two_clients(spec, &codex_home, model, project, io_path).await;
            }
            let mut env = HashMap::new();
            env.insert(
                "CODEX_HOME".to_string(),
                codex_home.to_string_lossy().into_owned(),
            );
            let codex = crate::client::connect(CodexConfig {
                model: Some(model.clone()),
                cwd: Some(project.clone()),
                env: Some(env),
                record_io: Some(io_path.clone()),
                client_name: "amux-codex-spec".to_string(),
                ..CodexConfig::default()
            })
            .await
            .map_err(|error| failure(spec, format!("model {model}: {error}")))?;
            Ok(Runtime {
                codex,
                other: None,
                #[cfg(unix)]
                server: None,
                model,
                project,
                replay_driver: None,
            })
        }
        SpecSource::Recorded(mut replay) => {
            let other = if TWO_CLIENTS.contains(&spec.name) {
                Some(
                    replay
                        .transports
                        .remove(two_clients::OTHER)
                        .ok_or_else(|| failure(spec, "recording has no other client"))?,
                )
            } else {
                None
            };
            let transport = if let Some(amux) = replay.transports.remove(two_clients::AMUX) {
                Some(amux)
            } else if replay.transports.len() == 1 {
                replay
                    .transports
                    .pop_first()
                    .map(|(_, transport)| transport)
            } else {
                replay.transports.remove("<default>")
            }
            .ok_or_else(|| failure(spec, "recording has no default app-server transport"))?;
            let controller = replay.controller.clone();
            let driver = tokio::spawn(async move {
                while let ReplayAdvance::Advanced { .. } | ReplayAdvance::BlockedOnWrite =
                    controller.advance_one().await
                {
                    tokio::task::yield_now().await;
                }
            });
            let codex = Codex::from_io(
                transport.reader,
                transport.writer,
                CodexConfig {
                    model: Some(CAPTURE_MODEL.to_string()),
                    client_name: "amux-codex-spec".to_string(),
                    ..CodexConfig::default()
                },
            )
            .await
            .map_err(|error| failure(spec, error))?;
            let other = match other {
                Some(transport) => Some(
                    Codex::from_io(
                        transport.reader,
                        transport.writer,
                        CodexConfig {
                            model: Some(CAPTURE_MODEL.to_string()),
                            client_name: "codex-spec-other".to_string(),
                            ..CodexConfig::default()
                        },
                    )
                    .await
                    .map_err(|error| failure(spec, error))?,
                ),
                None => None,
            };
            Ok(Runtime {
                codex,
                other,
                #[cfg(unix)]
                server: None,
                model: CAPTURE_MODEL.to_string(),
                project: PathBuf::from("<MACHINE_PATH>"),
                replay_driver: Some(driver),
            })
        }
    }
}

/// Codex's server on a socket with amux and another client, both recorded.
#[cfg(unix)]
async fn open_two_clients(
    spec: &SpecEntry,
    codex_home: &Path,
    model: String,
    project: PathBuf,
    io_path: PathBuf,
) -> Result<Runtime, SpecFailure> {
    let (codex, other, server) = two_clients::live::open(codex_home, &model, &project, &io_path)
        .await
        .map_err(|error| failure(spec, format!("model {model}: {error}")))?;
    Ok(Runtime {
        codex,
        other: Some(other),
        server: Some(server),
        model,
        project,
        replay_driver: None,
    })
}

#[cfg(not(unix))]
async fn open_two_clients(
    spec: &SpecEntry,
    _: &Path,
    _: String,
    _: PathBuf,
    _: PathBuf,
) -> Result<Runtime, SpecFailure> {
    Err(failure(
        spec,
        "two clients share a Codex server only on a Unix socket",
    ))
}

async fn run_scenario(
    name: &str,
    codex: &Codex,
    model: &str,
    project: &Path,
) -> Result<ScenarioReport, String> {
    match name {
        "initialize_and_start" => initialize_and_start(codex, model, project).await,
        "turn_round_trip" => turn_round_trip(codex, model, project).await,
        "approval_allow" => approval(codex, model, project, true).await,
        "approval_deny" => approval(codex, model, project, false).await,
        "interrupt" => interrupt(codex, model, project).await,
        "thread_list_and_resume" => thread_list_and_resume(codex, model, project).await,
        "dynamic_tools" => dynamic_tools(codex, model, project).await,
        "inject_idle" => inject_idle(codex, model, project).await,
        "inject_busy" => inject_busy(codex, model, project).await,
        "two_assistant_messages" => two_assistant_messages(codex, model, project).await,
        "inject_drain" => inject_drain(codex, model, project).await,
        "reasoning_summary" => rows::reasoning_summary(codex, model, project).await,
        "exploring" => rows::exploring(codex, model, project).await,
        "failing_command" => rows::failing_command(codex, model, project).await,
        "file_changes" => rows::file_changes(codex, model, project).await,
        "image" => rows::image(codex, model, project).await,
        "compaction" => rows::compaction(codex, model, project).await,
        "plan_mode" => rows::plan_mode(codex, model, project).await,
        "turn_error" => rows::turn_error(codex, model, project).await,
        "signed_out" => rows::signed_out(codex, model, project).await,
        "turn_retries" => rows::turn_retries(codex, model, project).await,
        "web_search" => rows::web_search(codex, model, project).await,
        "file_moved" => rows::file_moved(codex, model, project).await,
        "subagent" => rows::subagent(codex, model, project).await,
        "background_terminal" => rows::background_terminal(codex, model, project).await,
        "tool_server_form" => decisions::tool_server_form(codex, model, project).await,
        "access_grant" => decisions::access_grant(codex, model, project).await,
        "automatic_review" => decisions::automatic_review(codex, model, project).await,
        "approval_scopes" => decisions::approval_scopes(codex, model, project).await,
        other => Err(format!("unknown registered specification {other}")),
    }
}

fn thread_config(model: &str, project: &Path) -> ThreadStartParams {
    ThreadStartParams {
        model: Some(model.to_string()),
        cwd: Some(project.to_string_lossy().into_owned()),
        approval_policy: Some(AskForApproval::Named(ApprovalPolicy::OnRequest)),
        sandbox: Some(SandboxMode::WorkspaceWrite),
        ..ThreadStartParams::default()
    }
}

async fn start_thread(codex: &Codex, model: &str, project: &Path) -> Result<Thread, String> {
    codex
        .start_thread(thread_config(model, project))
        .await
        .map_err(|error| format!("model {model}: thread/start failed: {error}"))
}

fn report(thread: &Thread) -> ScenarioReport {
    ScenarioReport {
        server_model: thread.session().model.clone(),
        session_ids: vec![thread.id().to_string()],
    }
}

fn notification(event: &ThreadEvent) -> Option<&ServerNotification> {
    event.notification()
}

/// The item an `item/started` carries.
fn started(event: &ThreadEvent) -> Option<&ThreadItem> {
    match notification(event)? {
        ServerNotification::ItemStarted(started) => Some(&started.item),
        _ => None,
    }
}

/// The item an `item/completed` carries.
fn completed(event: &ThreadEvent) -> Option<&ThreadItem> {
    match notification(event)? {
        ServerNotification::ItemCompleted(completed) => Some(&completed.item),
        _ => None,
    }
}

fn turn_started(event: &ThreadEvent) -> Option<&Turn> {
    match notification(event)? {
        ServerNotification::TurnStarted(started) => Some(&started.turn),
        _ => None,
    }
}

fn turn_completed(event: &ThreadEvent) -> Option<&Turn> {
    match notification(event)? {
        ServerNotification::TurnCompleted(completed) => Some(&completed.turn),
        _ => None,
    }
}

fn error_of(event: &ThreadEvent) -> Option<&ErrorNotification> {
    match notification(event)? {
        ServerNotification::Error(error) => Some(error),
        _ => None,
    }
}

fn decision(decision: Decision) -> ClientResponse {
    ClientResponse::CommandApproval(CommandApprovalResponse {
        decision: CommandDecision::Plain(decision),
        extra: Extra::new(),
    })
}

async fn initialize_and_start(
    codex: &Codex,
    model: &str,
    project: &Path,
) -> Result<ScenarioReport, String> {
    let thread = start_thread(codex, model, project).await?;
    Ok(report(&thread))
}

async fn turn_round_trip(
    codex: &Codex,
    model: &str,
    project: &Path,
) -> Result<ScenarioReport, String> {
    let thread = start_thread(codex, model, project).await?;
    let mut events = thread.events().await.map_err(stringify)?;
    thread
        .say("Reply with exactly CODEX_SPEC_PONG and nothing else.")
        .await
        .map_err(stringify)?;
    let messages = wait_for_completion(&mut events).await?;
    if !messages
        .iter()
        .any(|(text, _)| text.contains("CODEX_SPEC_PONG"))
    {
        return Err("turn completed without CODEX_SPEC_PONG".to_string());
    }
    Ok(report(&thread))
}

async fn approval(
    codex: &Codex,
    model: &str,
    project: &Path,
    allow: bool,
) -> Result<ScenarioReport, String> {
    let mut config = thread_config(model, project);
    config.sandbox = Some(SandboxMode::ReadOnly);
    let thread = codex
        .start_thread(config)
        .await
        .map_err(|error| format!("model {model}: thread/start failed: {error}"))?;
    let mut events = thread.events().await.map_err(stringify)?;
    let file = project.join(if allow {
        "approval-allowed.txt"
    } else {
        "approval-denied.txt"
    });
    let command = if project == Path::new("<MACHINE_PATH>") {
        // The sanitizer replaces the complete path token, including its trailing
        // punctuation, so replay must produce the canonical recorded sentence.
        "Run this exact shell command and no substitute: /usr/bin/touch <MACHINE_PATH> Then say DONE."
            .to_string()
    } else {
        format!(
            "Run this exact shell command and no substitute: /usr/bin/touch {}. Then say DONE.",
            file.display()
        )
    };
    thread.say(command).await.map_err(stringify)?;

    loop {
        let event = next_event(&mut events, "approval request").await?;
        if let Event::Request {
            id,
            request: ServerRequest::CommandApproval(_) | ServerRequest::FileChangeApproval(_),
        } = event.event
        {
            let answer = if allow {
                Decision::Accept
            } else {
                Decision::Decline
            };
            thread
                .respond(id, decision(answer))
                .await
                .map_err(stringify)?;
            break;
        }
        if turn_completed(&event).is_some() {
            return Err("turn completed before requesting approval".to_string());
        }
    }
    wait_for_completion(&mut events).await?;
    if project.is_absolute() && file.exists() != allow {
        return Err(format!(
            "approval world assertion failed for {}",
            file.display()
        ));
    }
    Ok(report(&thread))
}

async fn interrupt(codex: &Codex, model: &str, project: &Path) -> Result<ScenarioReport, String> {
    let thread = start_thread(codex, model, project).await?;
    let mut events = thread.events().await.map_err(stringify)?;
    let turn = thread
        .say("Count slowly from one to one hundred, one number per line.")
        .await
        .map_err(stringify)?;
    while turn_started(&next_event(&mut events, "turn start before interrupt").await?).is_none() {}
    thread.interrupt(&turn.id).await.map_err(stringify)?;
    wait_for_completion(&mut events).await?;
    Ok(report(&thread))
}

async fn thread_list_and_resume(
    codex: &Codex,
    model: &str,
    project: &Path,
) -> Result<ScenarioReport, String> {
    let thread = start_thread(codex, model, project).await?;
    let original = report(&thread);
    let mut events = thread.events().await.map_err(stringify)?;
    thread
        .say("Reply with exactly CODEX_SPEC_RESUME and nothing else.")
        .await
        .map_err(stringify)?;
    wait_for_completion(&mut events).await?;
    drop(events);
    codex
        .rename_thread(thread.id(), "codex-spec-resume")
        .await
        .map_err(stringify)?;
    let listed = codex
        .list_threads(ThreadListParams::default())
        .await
        .map_err(stringify)?;
    if !listed.data.iter().any(|item| item.id == thread.id()) {
        return Err("thread/list omitted the newly started thread".to_string());
    }
    let id = thread.id().to_string();
    drop(thread);
    let config = thread_config(model, project);
    let resumed = codex
        .resume_thread(ThreadResumeParams {
            thread_id: id.clone(),
            cwd: config.cwd,
            model: config.model,
            approval_policy: config.approval_policy,
            sandbox: config.sandbox,
            exclude_turns: None,
            extra: Extra::new(),
        })
        .await
        .map_err(stringify)?;
    if resumed.id() != id {
        return Err("thread/resume changed thread identity".to_string());
    }
    Ok(ScenarioReport {
        server_model: resumed.session().model.clone(),
        session_ids: original.session_ids,
    })
}

async fn dynamic_tools(
    codex: &Codex,
    model: &str,
    project: &Path,
) -> Result<ScenarioReport, String> {
    let mut config = thread_config(model, project);
    config.dynamic_tools = Some(vec![DynamicTool {
        name: "send".to_string(),
        description: "Send a short message to another agent.".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {"to": {"type": "string"}, "text": {"type": "string"}},
            "required": ["to", "text"]
        }),
        defer_loading: None,
        extra: Extra::new(),
    }]);
    let thread = codex.start_thread(config).await.map_err(stringify)?;
    let mut events = thread.events().await.map_err(stringify)?;
    thread
        .say("Call the send tool exactly once with to=probe and text=CODEX_SPEC_SENT. Do not use any other tool.")
        .await
        .map_err(stringify)?;
    let mut called = false;
    loop {
        let event = next_event(&mut events, "dynamic tool call and completion").await?;
        if let Some(turn) = turn_completed(&event) {
            if turn.status != TurnStatus::Completed || !called {
                return Err("dynamic tool turn completed without the required call".to_string());
            }
            break;
        }
        let Event::Request {
            id,
            request: ServerRequest::ToolCall(call),
        } = event.event
        else {
            continue;
        };
        if call.tool != "send" {
            continue;
        }
        if call.arguments != serde_json::json!({"to": "probe", "text": "CODEX_SPEC_SENT"}) {
            return Err(format!(
                "dynamic tool arguments differed: {}",
                call.arguments
            ));
        }
        called = true;
        thread
            .respond(
                id,
                ClientResponse::ToolCall(ToolCallResponse {
                    content_items: vec![ToolOutputContent::Text(TextContent {
                        text: "sent".to_string(),
                        extra: Extra::new(),
                    })],
                    success: true,
                    extra: Extra::new(),
                }),
            )
            .await
            .map_err(stringify)?;
    }
    Ok(report(&thread))
}

fn injected_item(text: &str) -> InjectedItem {
    InjectedItem::Message(InjectedMessage {
        role: "user".to_string(),
        content: vec![InjectedContent::InputText(TextContent {
            text: text.to_string(),
            extra: Extra::new(),
        })],
        extra: Extra::new(),
    })
}

async fn inject_idle(codex: &Codex, model: &str, project: &Path) -> Result<ScenarioReport, String> {
    let thread = start_thread(codex, model, project).await?;
    let mut events = thread.events().await.map_err(stringify)?;
    thread
        .inject_items(vec![injected_item(
            "Reply with exactly CODEX_SPEC_INJECT_IDLE and nothing else.",
        )])
        .await
        .map_err(stringify)?;
    // A turn with no input of its own consumes what was injected.
    thread
        .start_turn(TurnStartParams::default())
        .await
        .map_err(stringify)?;
    let messages = wait_for_completion(&mut events).await?;
    if !messages
        .iter()
        .any(|(text, _)| text.contains("CODEX_SPEC_INJECT_IDLE"))
    {
        return Err("idle injected item was not reflected in the response".to_string());
    }
    Ok(report(&thread))
}

async fn inject_busy(codex: &Codex, model: &str, project: &Path) -> Result<ScenarioReport, String> {
    let thread = start_thread(codex, model, project).await?;
    let mut events = thread.events().await.map_err(stringify)?;
    thread
        .say("Think briefly, then reply exactly CODEX_SPEC_INITIAL.")
        .await
        .map_err(stringify)?;
    while turn_started(&next_event(&mut events, "busy turn start").await?).is_none() {}
    thread
        .inject_items(vec![injected_item(
            "Reply with exactly CODEX_SPEC_INJECT_BUSY and nothing else.",
        )])
        .await
        .map_err(stringify)?;
    let messages = wait_for_completion(&mut events).await?;
    let texts = messages
        .iter()
        .map(|(text, _)| text.as_str())
        .collect::<Vec<_>>();
    if texts != ["CODEX_SPEC_INITIAL", "CODEX_SPEC_INJECT_BUSY"] {
        return Err(format!("busy injected message order differed: {texts:?}"));
    }
    Ok(report(&thread))
}

/// An item injected while a turn runs is drained by that turn: the model
/// samples again with the item in context and answers it under the same turn
/// id, so nothing waits for a next turn. The inject is acknowledged at once
/// and produces no item of its own, so a client that wants the injected
/// message in its transcript must write it there itself.
async fn inject_drain(
    codex: &Codex,
    model: &str,
    project: &Path,
) -> Result<ScenarioReport, String> {
    let mut config = thread_config(model, project);
    config.approval_policy = Some(AskForApproval::Named(ApprovalPolicy::Never));
    let thread = codex
        .start_thread(config)
        .await
        .map_err(|error| format!("model {model}: thread/start failed: {error}"))?;
    let mut events = thread.events().await.map_err(stringify)?;
    thread
        .say(
            "Run the shell command `sleep 5`, then reply with exactly CODEX_SPEC_DONE_A and \
             nothing else.",
        )
        .await
        .map_err(stringify)?;
    let turn_id = loop {
        if let Some(turn) = turn_started(&next_event(&mut events, "turn start").await?) {
            break turn.id.clone();
        }
    };
    while !matches!(
        started(&next_event(&mut events, "the command").await?),
        Some(ThreadItem::CommandExecution(_))
    ) {}
    thread
        .inject_items(vec![injected_item(
            "Ignore the earlier instruction about the final reply. Reply with exactly \
             CODEX_SPEC_INJECT_DRAIN and nothing else.",
        )])
        .await
        .map_err(stringify)?;

    let mut messages = Vec::new();
    loop {
        let event = next_event(&mut events, "turn completion").await?;
        if matches!(
            started(&event).or(completed(&event)),
            Some(ThreadItem::UserMessage(_))
        ) {
            return Err("the injected item surfaced as a userMessage item".to_string());
        }
        if let Some(turn) = turn_started(&event) {
            return Err(format!(
                "turn {} started before the running turn completed",
                turn.id
            ));
        }
        if let Some(ThreadItem::AgentMessage(message)) = completed(&event) {
            messages.push(message.text.clone());
        }
        if let Some(turn) = turn_completed(&event) {
            if turn.id != turn_id || turn.status != TurnStatus::Completed {
                return Err(format!(
                    "expected turn {turn_id} to complete, got {} with status {:?}",
                    turn.id, turn.status
                ));
            }
            break;
        }
        if let Some(error) = error_of(&event) {
            return Err(format!("turn error: {}", error.error.message));
        }
    }
    if !messages
        .iter()
        .any(|text| text.contains("CODEX_SPEC_INJECT_DRAIN"))
    {
        return Err(format!(
            "the running turn did not answer the injected item: {messages:?}"
        ));
    }
    Ok(report(&thread))
}

async fn two_assistant_messages(
    codex: &Codex,
    model: &str,
    project: &Path,
) -> Result<ScenarioReport, String> {
    let thread = start_thread(codex, model, project).await?;
    let mut events = thread.events().await.map_err(stringify)?;
    thread
        .say("Send two separate assistant messages in this turn: first exactly CODEX_SPEC_FIRST in commentary, then exactly CODEX_SPEC_SECOND as the final answer.")
        .await
        .map_err(stringify)?;
    let messages = wait_for_completion(&mut events).await?;
    if messages
        != [
            (
                "CODEX_SPEC_FIRST".to_string(),
                Some(MessagePhase::Commentary),
            ),
            (
                "CODEX_SPEC_SECOND".to_string(),
                Some(MessagePhase::FinalAnswer),
            ),
        ]
    {
        return Err(format!(
            "assistant message order or phases differed: {messages:?}"
        ));
    }
    Ok(report(&thread))
}

async fn wait_for_completion(
    events: &mut ThreadEventStream,
) -> Result<Vec<(String, Option<MessagePhase>)>, String> {
    let mut messages = Vec::new();
    loop {
        let event = next_event(events, "turn completion").await?;
        if let Some(ThreadItem::AgentMessage(message)) = completed(&event) {
            messages.push((message.text.clone(), message.phase.clone()));
        }
        if let Some(turn) = turn_completed(&event) {
            if turn.status != TurnStatus::Completed && turn.status != TurnStatus::Interrupted {
                return Err(format!("turn completed with status {:?}", turn.status));
            }
            return Ok(messages);
        }
        if let Some(error) = error_of(&event) {
            return Err(format!("turn error: {}", error.error.message));
        }
    }
}

async fn next_event(events: &mut ThreadEventStream, what: &str) -> Result<ThreadEvent, String> {
    tokio::time::timeout(EVENT_TIMEOUT, events.next())
        .await
        .map_err(|_| format!("timed out waiting for {what}"))?
        .map_err(stringify)?
        .ok_or_else(|| format!("event stream closed while waiting for {what}"))
}

fn version_from_user_agent(user_agent: &str) -> Option<Version> {
    user_agent
        .split(|character: char| !(character.is_ascii_digit() || character == '.'))
        .filter(|part| part.matches('.').count() >= 2)
        .find_map(|part| Version::parse(part).ok())
}

fn stringify(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn failure(spec: &SpecEntry, claim: impl std::fmt::Display) -> SpecFailure {
    SpecFailure {
        spec: spec.name.to_string(),
        claim: claim.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_names_the_provider_side_of_the_c_suite() {
        assert_eq!(registry().len(), 33);
        assert_eq!(registry()[0].name, "initialize_and_start");
        assert_eq!(registry()[9].name, "two_assistant_messages");
        assert!(
            registry()
                .iter()
                .all(|entry| entry.allowed_models == [CAPTURE_MODEL])
        );
    }

    #[test]
    fn user_agent_version_is_semantic() {
        assert_eq!(
            version_from_user_agent("codex-cli/0.150.1"),
            Some(Version::parse("0.150.1").unwrap())
        );
    }
}
