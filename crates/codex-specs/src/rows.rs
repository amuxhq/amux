//! Specifications for the chat rows a Codex turn produces beyond a plain
//! answer: reasoning summaries, exploring commands, a failing command, file
//! changes, images, compaction, plan mode with its question,
//! a turn that errors, a signed-out account, web search, a moved file, a
//! subagent and a background terminal session.
//!
//! Live capture seeds the project with `config.txt` (`VALUE=1`), `old.txt`
//! and `square.png`, a 32×32 red PNG. Prompts name files relative to the
//! project so replay, which has no project, sends the same bytes.

use std::path::Path;

use codex_protocol::client::{AccountReadParams, TurnStartParams, UserInputResponse};
use codex_protocol::items::{CommandAction, ImageInput, PatchChangeKind, ThreadItem, UserInput};
use codex_protocol::server::{ServerNotification, ServerRequest};
use codex_protocol::thread::{
    ApprovalPolicy, AskForApproval, CollaborationMode, CollaborationSettings, ModeKind,
    ReasoningEffort, ReasoningSummary, TurnStatus,
};
use codex_protocol::{ClientResponse, Extra};

use super::decisions::drive_turn;
use super::{
    ScenarioReport, completed, error_of, next_event, report, started, stringify, thread_config,
    turn_completed,
};
use crate::client::{Codex, Event, ThreadEvent, ThreadEventStream, text_input};

/// The seed image, attached to a prompt as a data URL.
pub(crate) const SQUARE_PNG: &[u8] = include_bytes!("../assets/square.png");

/// Every event of one turn, read until it completes.
async fn collect_turn(events: &mut ThreadEventStream) -> Result<Vec<ThreadEvent>, String> {
    let mut seen = Vec::new();
    loop {
        let event = next_event(events, "turn completion").await?;
        let done = turn_completed(&event).is_some();
        seen.push(event);
        if done {
            return Ok(seen);
        }
    }
}

fn completed_items(events: &[ThreadEvent]) -> Vec<&ThreadItem> {
    events.iter().filter_map(completed).collect()
}

fn answers(events: &[ThreadEvent]) -> String {
    completed_items(events)
        .into_iter()
        .filter_map(|item| match item {
            ThreadItem::AgentMessage(message) => Some(message.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn final_status(events: &[ThreadEvent]) -> Option<TurnStatus> {
    events
        .iter()
        .rev()
        .find_map(|event| Some(turn_completed(event)?.status.clone()))
}

fn full_auto_config(model: &str, project: &Path) -> codex_protocol::client::ThreadStartParams {
    let mut config = thread_config(model, project);
    config.approval_policy = Some(AskForApproval::Named(ApprovalPolicy::Never));
    config
}

async fn full_auto(
    codex: &Codex,
    model: &str,
    project: &Path,
) -> Result<(crate::client::Thread, ThreadEventStream), String> {
    let config = full_auto_config(model, project);
    let thread = codex
        .start_thread(config)
        .await
        .map_err(|error| format!("model {model}: thread/start failed: {error}"))?;
    let events = thread.events().await.map_err(stringify)?;
    Ok((thread, events))
}

/// With a detailed summary requested, reasoning items carry summary text.
pub(super) async fn reasoning_summary(
    codex: &Codex,
    model: &str,
    project: &Path,
) -> Result<ScenarioReport, String> {
    let (thread, mut events) = full_auto(codex, model, project).await?;
    thread
        .start_turn(TurnStartParams {
            input: vec![text_input(
                "Think carefully about which is larger, 17 * 23 or 19 * 21, then reply with only \
                 the larger product.",
            )],
            effort: Some(Some(ReasoningEffort::High)),
            summary: Some(ReasoningSummary::Detailed),
            ..TurnStartParams::default()
        })
        .await
        .map_err(stringify)?;
    let seen = collect_turn(&mut events).await?;
    let summaries = completed_items(&seen)
        .into_iter()
        .filter_map(|item| match item {
            ThreadItem::Reasoning(reasoning) => reasoning.summary.as_ref().map(|s| s.join("")),
            _ => None,
        })
        .filter(|summary| !summary.is_empty())
        .count();
    if summaries == 0 {
        return Err("no reasoning item carried summary text".to_string());
    }
    Ok(report(&thread))
}

/// Codex classifies the commands it runs: reading a file, listing files and
/// searching come back as typed command actions.
pub(super) async fn exploring(
    codex: &Codex,
    model: &str,
    project: &Path,
) -> Result<ScenarioReport, String> {
    let (thread, mut events) = full_auto(codex, model, project).await?;
    thread
        .say(
            "Run these three shell commands separately, one tool call each: `cat config.txt`, \
             `ls`, and `grep -n VALUE config.txt`. Then reply DONE.",
        )
        .await
        .map_err(stringify)?;
    let seen = collect_turn(&mut events).await?;
    let mut kinds = Vec::new();
    for item in completed_items(&seen) {
        if let ThreadItem::CommandExecution(command) = item {
            for action in &command.command_actions {
                kinds.push(match action {
                    CommandAction::Read(_) => "read",
                    CommandAction::ListFiles(_) => "list",
                    CommandAction::Search(_) => "search",
                    _ => "other",
                });
            }
        }
    }
    for kind in ["read", "list", "search"] {
        if !kinds.contains(&kind) {
            return Err(format!("no {kind} command action among {kinds:?}"));
        }
    }
    Ok(report(&thread))
}

/// A command that exits non-zero completes as a failed command execution
/// with its exit code and output.
pub(super) async fn failing_command(
    codex: &Codex,
    model: &str,
    project: &Path,
) -> Result<ScenarioReport, String> {
    let (thread, mut events) = full_auto(codex, model, project).await?;
    thread
        .say(
            "Run exactly this shell command once: ls does-not-exist. Then tell me whether it failed.",
        )
        .await
        .map_err(stringify)?;
    let seen = collect_turn(&mut events).await?;
    let failed = completed_items(&seen).into_iter().any(|item| {
        matches!(
            item,
            ThreadItem::CommandExecution(command)
                if command.exit_code.is_some_and(|code| code != 0)
                    && command
                        .aggregated_output
                        .as_deref()
                        .is_some_and(|output| output.contains("does-not-exist"))
        )
    });
    if !failed {
        return Err("no command completed with a non-zero exit code".to_string());
    }
    Ok(report(&thread))
}

/// Edits made through apply_patch arrive as file-change items whose changes
/// say whether each file was added, modified or deleted.
pub(super) async fn file_changes(
    codex: &Codex,
    model: &str,
    project: &Path,
) -> Result<ScenarioReport, String> {
    let (thread, mut events) = full_auto(codex, model, project).await?;
    thread
        .say(
            "Use apply_patch for every change, one patch per step: first create notes.txt \
             containing the line one; then change that line to two; then delete old.txt. \
             Then reply DONE.",
        )
        .await
        .map_err(stringify)?;
    let seen = collect_turn(&mut events).await?;
    let mut kinds = Vec::new();
    for item in completed_items(&seen) {
        if let ThreadItem::FileChange(change) = item {
            kinds.extend(change.changes.iter().map(|change| match change.kind {
                PatchChangeKind::Add(_) => "add",
                PatchChangeKind::Update(_) => "update",
                PatchChangeKind::Delete(_) => "delete",
                PatchChangeKind::Unknown(_) => "unknown",
            }));
        }
    }
    for kind in ["add", "update", "delete"] {
        if !kinds.contains(&kind) {
            return Err(format!("no {kind} change among {kinds:?}"));
        }
    }
    Ok(report(&thread))
}

/// An image goes in attached to the message, and Codex can look at one on
/// disk with its image viewer.
pub(super) async fn image(
    codex: &Codex,
    model: &str,
    project: &Path,
) -> Result<ScenarioReport, String> {
    let (thread, mut events) = full_auto(codex, model, project).await?;
    thread
        .start_turn(TurnStartParams {
            input: vec![
                UserInput::Image(ImageInput {
                    url: format!("data:image/png;base64,{}", base64(SQUARE_PNG)),
                    extra: Extra::new(),
                }),
                text_input(
                    "Name the color of the attached image in one word. Then look at square.png \
                     in the working directory with your image viewing tool and say whether it \
                     is the same image.",
                ),
            ],
            ..TurnStartParams::default()
        })
        .await
        .map_err(stringify)?;
    let seen = collect_turn(&mut events).await?;
    if !completed_items(&seen)
        .into_iter()
        .any(|item| matches!(item, ThreadItem::ImageView(_)))
    {
        return Err("Codex did not view the image on disk".to_string());
    }
    if !answers(&seen).to_lowercase().contains("red") {
        return Err(format!(
            "the attached image did not reach the model: {}",
            answers(&seen)
        ));
    }
    Ok(report(&thread))
}

/// Compacting a thread on request produces a context-compaction item.
pub(super) async fn compaction(
    codex: &Codex,
    model: &str,
    project: &Path,
) -> Result<ScenarioReport, String> {
    let (thread, mut events) = full_auto(codex, model, project).await?;
    thread
        .say("Reply with exactly CODEX_SPEC_BEFORE_COMPACT and nothing else.")
        .await
        .map_err(stringify)?;
    collect_turn(&mut events).await?;
    thread.compact().await.map_err(stringify)?;
    let mut compacted = false;
    loop {
        let event = next_event(&mut events, "compaction").await?;
        if let Some(ThreadItem::ContextCompaction(_)) = completed(&event) {
            compacted = true;
        }
        if let Some(turn) = turn_completed(&event) {
            if turn.status != TurnStatus::Completed {
                return Err(format!("compaction ended with {:?}", turn.status));
            }
            break;
        }
        if let Some(error) = error_of(&event) {
            return Err(format!("compaction error: {}", error.error.message));
        }
    }
    if !compacted {
        return Err("no context-compaction item completed".to_string());
    }
    Ok(report(&thread))
}

/// In plan mode Codex asks its questions through the request-user-input tool,
/// which reaches the client as a server request, and delivers its plan as a
/// plan item.
pub(super) async fn plan_mode(
    codex: &Codex,
    model: &str,
    project: &Path,
) -> Result<ScenarioReport, String> {
    let (thread, mut events) = full_auto(codex, model, project).await?;
    thread
        .start_turn(TurnStartParams {
            input: vec![text_input(
                "Before planning, ask me exactly one question with your tool for asking the \
                 user: which color should color.txt hold, with the options Red and Blue. Then \
                 propose a short plan to write my answer into color.txt.",
            )],
            collaboration_mode: Some(CollaborationMode {
                mode: ModeKind::Plan,
                settings: CollaborationSettings {
                    model: model.to_string(),
                    reasoning_effort: None,
                    developer_instructions: None,
                    extra: Extra::new(),
                },
                extra: Extra::new(),
            }),
            ..TurnStartParams::default()
        })
        .await
        .map_err(stringify)?;
    let mut asked = false;
    let mut planned = false;
    loop {
        let event = next_event(&mut events, "plan mode turn").await?;
        if let Some(ThreadItem::Plan(plan)) = completed(&event)
            && !plan.text.is_empty()
        {
            planned = true;
        }
        if let Some(turn) = turn_completed(&event) {
            if turn.status != TurnStatus::Completed {
                return Err(format!("plan turn ended with {:?}", turn.status));
            }
            break;
        }
        if let Some(error) = error_of(&event) {
            return Err(format!("turn error: {}", error.error.message));
        }
        if let Event::Request {
            id,
            request: ServerRequest::RequestUserInput(params),
        } = event.event
        {
            asked = true;
            let answers = params
                .questions
                .iter()
                .map(|question| {
                    let answer = codex_protocol::client::Answer {
                        answers: vec!["Blue".to_string()],
                        extra: Extra::new(),
                    };
                    (question.id.clone(), answer)
                })
                .collect();
            thread
                .respond(
                    id,
                    ClientResponse::UserInput(UserInputResponse {
                        answers,
                        extra: Extra::new(),
                    }),
                )
                .await
                .map_err(stringify)?;
        }
    }
    if !asked {
        return Err("Codex did not ask through request-user-input".to_string());
    }
    if !planned {
        return Err("no plan item completed".to_string());
    }
    Ok(report(&thread))
}

/// A turn against a model the account cannot use fails with an error event
/// and a failed turn.
pub(super) async fn turn_error(
    codex: &Codex,
    model: &str,
    project: &Path,
) -> Result<ScenarioReport, String> {
    let (thread, mut events) = full_auto(codex, model, project).await?;
    thread
        .start_turn(TurnStartParams {
            input: vec![text_input("Reply with exactly CODEX_SPEC_UNREACHABLE.")],
            model: Some("codex-spec-no-such-model".to_string()),
            ..TurnStartParams::default()
        })
        .await
        .map_err(stringify)?;
    let seen = collect_turn(&mut events).await?;
    if !seen.iter().any(|event| error_of(event).is_some()) {
        return Err("no error event arrived".to_string());
    }
    if final_status(&seen) != Some(TurnStatus::Failed) {
        return Err(format!("the turn ended {:?}", final_status(&seen)));
    }
    Ok(report(&thread))
}

/// A provider that cannot be reached makes Codex retry: each attempt arrives
/// as an error that says it will retry, then the turn fails.
pub(super) async fn turn_retries(
    codex: &Codex,
    model: &str,
    project: &Path,
) -> Result<ScenarioReport, String> {
    let mut config = full_auto_config(model, project);
    config.model_provider = Some("unreachable".to_string());
    config.config = Some(
        [
            (
                "features.unbounded_connection_retries".to_owned(),
                serde_json::json!(false),
            ),
            (
                "model_providers.unreachable".to_owned(),
                serde_json::json!({
                    "name": "unreachable",
                    "base_url": "http://127.0.0.1:9/v1",
                    "wire_api": "responses",
                    "request_max_retries": 2,
                    "stream_max_retries": 2,
                }),
            ),
        ]
        .into_iter()
        .collect(),
    );
    let thread = codex.start_thread(config).await.map_err(stringify)?;
    let mut events = thread.events().await.map_err(stringify)?;
    thread
        .say("Reply with exactly CODEX_SPEC_UNREACHABLE.")
        .await
        .map_err(stringify)?;
    let seen = collect_turn(&mut events).await?;
    let retries = seen
        .iter()
        .filter(|event| error_of(event).is_some_and(|error| error.will_retry))
        .count();
    if retries == 0 {
        return Err(format!("no error said it would retry: {seen:?}"));
    }
    if final_status(&seen) != Some(TurnStatus::Failed) {
        return Err(format!("the turn ended {:?}", final_status(&seen)));
    }
    Ok(report(&thread))
}

/// Without credentials the account reads as signed out and a turn fails.
pub(super) async fn signed_out(
    codex: &Codex,
    model: &str,
    project: &Path,
) -> Result<ScenarioReport, String> {
    let account = codex
        .read_account(AccountReadParams::default())
        .await
        .map_err(stringify)?;
    if account.account.is_some() {
        return Err(format!("an account was signed in: {account:?}"));
    }
    let (thread, mut events) = full_auto(codex, model, project).await?;
    thread
        .say("Reply with exactly CODEX_SPEC_SIGNED_OUT.")
        .await
        .map_err(stringify)?;
    let seen = collect_turn(&mut events).await?;
    if final_status(&seen) != Some(TurnStatus::Failed) {
        return Err(format!(
            "the signed-out turn ended {:?}",
            final_status(&seen)
        ));
    }
    Ok(report(&thread))
}

/// With live web search on, a search runs as its own web-search item.
pub(super) async fn web_search(
    codex: &Codex,
    model: &str,
    project: &Path,
) -> Result<ScenarioReport, String> {
    let mut config = full_auto_config(model, project);
    config.config = Some(
        [("web_search".to_owned(), serde_json::json!("live"))]
            .into_iter()
            .collect(),
    );
    let thread = codex.start_thread(config).await.map_err(stringify)?;
    let mut events = thread.events().await.map_err(stringify)?;
    thread
        .say(
            "Use your web search tool to look up the capital city of Australia, then reply with \
             the city name only.",
        )
        .await
        .map_err(stringify)?;
    let seen = collect_turn(&mut events).await?;
    if !completed_items(&seen)
        .into_iter()
        .any(|item| matches!(item, ThreadItem::WebSearch(_)))
    {
        return Err("no web-search item completed".to_string());
    }
    Ok(report(&thread))
}

/// A patch that renames a file arrives as a file change whose kind carries
/// the path it moved to.
pub(super) async fn file_moved(
    codex: &Codex,
    model: &str,
    project: &Path,
) -> Result<ScenarioReport, String> {
    let (thread, mut events) = full_auto(codex, model, project).await?;
    thread
        .say(
            "Use apply_patch to rename old.txt to moved.txt, with a patch that has a Move to \
             line. Do not use shell commands. Then reply DONE.",
        )
        .await
        .map_err(stringify)?;
    let seen = drive_turn(&thread, &mut events, &mut |_| None).await?;
    let moved = completed_items(&seen).into_iter().any(|item| {
        let ThreadItem::FileChange(change) = item else {
            return false;
        };
        change.changes.iter().any(|change| {
            matches!(&change.kind, PatchChangeKind::Update(update) if update.move_path.is_some())
        })
    });
    if !moved {
        return Err(format!(
            "no file change carried a move: {:?}",
            items_of(&seen, &["fileChange", "commandExecution"])
        ));
    }
    Ok(report(&thread))
}

/// Codex's multi-agent tools start a child thread and wait for it; each call
/// is a collab-agent tool-call item on the parent thread.
pub(super) async fn subagent(
    codex: &Codex,
    model: &str,
    project: &Path,
) -> Result<ScenarioReport, String> {
    let (thread, mut events) = full_auto(codex, model, project).await?;
    thread
        .say(
            "Call your spawn_agent tool directly, without reading any file or using any skill, to \
             start exactly one agent whose task is: reply with exactly CODEX_SPEC_CHILD. Then \
             wait for it to finish and reply with what it answered.",
        )
        .await
        .map_err(stringify)?;
    let seen = drive_turn(&thread, &mut events, &mut |_| None).await?;
    let tools = completed_items(&seen)
        .into_iter()
        .filter_map(|item| match item {
            ThreadItem::CollabAgentToolCall(call) => Some(call.tool.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    if tools.is_empty() {
        return Err("no collab-agent tool call completed".to_string());
    }
    // The owner's own skills are visible to the capture; reading one would
    // put its text into the recording.
    if seen
        .iter()
        .any(|event| matches!(started(event), Some(ThreadItem::CommandExecution(_))))
    {
        return Err("the turn ran a command instead of only spawning".to_string());
    }
    Ok(report(&thread))
}

/// The completed items of the given types, as JSON, for a failure message.
pub(super) fn items_of(seen: &[ThreadEvent], types: &[&str]) -> Vec<String> {
    completed_items(seen)
        .into_iter()
        .filter(|item| types.contains(&item.kind()))
        .map(|item| serde_json::to_string(item).unwrap_or_default())
        .collect()
}

/// A long-running command started without waiting keeps running as a
/// terminal session; each later read of it arrives as a terminal interaction
/// on the command's item.
pub(super) async fn background_terminal(
    codex: &Codex,
    model: &str,
    project: &Path,
) -> Result<ScenarioReport, String> {
    let (thread, mut events) = full_auto(codex, model, project).await?;
    thread
        .say(
            "Start the shell command `read LINE; echo CODEX_SPEC_GOT $LINE` with a yield time \
             of 1000 ms, so the call returns while it waits for input. Then write the text \
             hello followed by a newline to that same running session, read its output, and \
             reply DONE.",
        )
        .await
        .map_err(stringify)?;
    let seen = drive_turn(&thread, &mut events, &mut |_| None).await?;
    let backgrounded = seen.iter().any(|event| {
        matches!(
            event.notification(),
            Some(ServerNotification::TerminalInteraction(_))
        )
    });
    if !backgrounded {
        return Err(format!(
            "no command ran as a terminal session: {:?}",
            seen.iter()
                .map(|event| event.event.method())
                .collect::<Vec<_>>()
        ));
    }
    Ok(report(&thread))
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let word = chunk.iter().enumerate().fold(0u32, |word, (index, byte)| {
            word | u32::from(*byte) << (16 - 8 * index)
        });
        for index in 0..4 {
            if index <= chunk.len() {
                encoded.push(ALPHABET[(word >> (18 - 6 * index) & 63) as usize] as char);
            } else {
                encoded.push('=');
            }
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    #[test]
    fn base64_pads_short_tails() {
        assert_eq!(super::base64(b"Man"), "TWFu");
        assert_eq!(super::base64(b"Ma"), "TWE=");
        assert_eq!(super::base64(b"M"), "TQ==");
    }
}
