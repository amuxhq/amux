//! A Claude subagent's own conversation, drawn into its own chat.
//!
//! Claude's Agent tool starts subagents inside the agent's own Claude
//! process; each is an amux agent of its own (see `crate::subagent`), and
//! its chat holds its prompt, thinking, text, tool calls and results. Both
//! Claude kinds read that conversation in the same message shapes: headless
//! Claude forwards each subagent's frames on its stream, tagged with the
//! subagent's id, and terminal Claude writes them as rows of the
//! subagent's own transcript file. So one builder serves both, and a
//! Claude subagent draws the same whichever kind runs it.
//!
//! What stays with each kind is what differs between them: how it learns
//! that a subagent started, which call started it, how it ended, how its
//! asks are answered and whether it can be stopped.

use std::collections::BTreeMap;

use claude_protocol::stream::{ContentBlock, MessageContent, ToolResultBody};
use prost::Message as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use wire::{
    Ask, AskClosed, BackgroundJob, ClaudeSnapshot, DecisionOutcome, ToolState, claude_item,
};

use crate::claude_answers::Decided;
use crate::claude_common::{
    AnsweredResult, Task, Tool, item_body, message_text, question_ask_text, split_tool_name,
    tool_result_text,
};
use crate::{ItemDraft, Subagent, ask_item, is_status_tool, status_working_on, unknown};

/// The text Claude writes when a person stops a call or a subagent.
const INTERRUPTED: &str = "[Request interrupted by user";

/// The tool that starts a Claude subagent.
pub(crate) const AGENT_TOOL: &str = "Agent";

/// The tool a Claude model continues a subagent with.
pub(crate) const SEND_MESSAGE_TOOL: &str = "SendMessage";

/// What a subagent's own call keeps beside what is drawn.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct SubCall {
    /// Its result returned only that a task was launched in the background
    /// (a nested subagent, a shell): the row stays running until that
    /// task's end finishes it.
    pub(crate) launched: bool,
}

/// One Claude subagent's conversation as its chat draws it: its calls by
/// tool-use id while they can still change, and the blocks of each
/// assistant message seen so far, which key its items.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct Conversation {
    pub(crate) tools: BTreeMap<String, Tool<SubCall>>,
    blocks: BTreeMap<String, u32>,
    /// The model its newest assistant message ran.
    pub(crate) model: Option<String>,
    /// What it last said it is working on, with amux's status tool: a
    /// subagent's own `status` call is about itself.
    pub(crate) working_on: Option<String>,
}

/// Whether a user message's text is Claude's own note to the model rather
/// than something its parent said to the subagent: a background task's
/// notification, a reminder, or the marker of an interruption.
fn claude_note(text: &str) -> bool {
    let text = text.trim_start();
    text.is_empty()
        || text.starts_with(INTERRUPTED)
        || text.starts_with("<task-notification>")
        || text.starts_with("[SYSTEM NOTIFICATION")
        || text.starts_with("<system-reminder>")
}

/// Whether a call's result says only that it launched work in the
/// background: an Agent call whose subagent runs on, or a shell sent to
/// the background. The work's own end says how it went.
fn launched_in_background(result: Option<&Value>, output: &str) -> bool {
    let Some(result) = result else {
        return output.starts_with("Async agent launched");
    };
    result.get("isAsync").and_then(Value::as_bool) == Some(true)
        || result.get("status").and_then(Value::as_str) == Some("async_launched")
        || result
            .get("backgroundTaskId")
            .and_then(Value::as_str)
            .is_some_and(|id| !id.is_empty())
}

/// A decision on a call, as either kind applies one to a call's row: an
/// allowance, a denial, a plan's verdict or a question's record.
pub(crate) fn apply_decided<T>(tool: &mut Tool<T>, decided: Decided, now_ms: i64) {
    match decided {
        // A question cannot be declined: one refused anyway closed
        // without answers.
        Decided::Tool(_) if tool.question => {
            tool.asked.get_or_insert_with(ask_item::dismissed);
        }
        Decided::Tool(decision) => {
            if decision.outcome == DecisionOutcome::Allowed as i32 {
                tool.started_at_ms.get_or_insert(now_ms);
            }
            tool.decision = Some(decision.to_wire());
        }
        Decided::Plan(verdict) => tool.plan = Some(verdict),
        Decided::Question(closed) => tool.asked = Some(closed),
    }
}

impl Conversation {
    /// Writes the call's row into the subagent's part, if it changed.
    pub(crate) fn emit_tool(&mut self, sub: &mut Subagent<'_, Ask>, id: &str) {
        if let Some(draft) = self.tools.get_mut(id).and_then(|tool| tool.draft(id)) {
            sub.item(draft);
        }
    }

    /// A call the subagent makes: its row opens running, or takes the input
    /// that arrived since. An ask for a call can come before the call's own
    /// frame, so either may open it.
    pub(crate) fn tool_seen(
        &mut self,
        sub: &mut Subagent<'_, Ask>,
        id: &str,
        name: &str,
        input: Option<&Value>,
        at_ms: i64,
    ) {
        if id.is_empty() {
            return;
        }
        let (server, tool_name) = split_tool_name(name);
        if is_status_tool(&server, &tool_name)
            && let Some(input) = input
            && let Some(working_on) = status_working_on(input.to_string().as_bytes())
        {
            self.working_on = working_on;
        }
        let tool = self
            .tools
            .entry(id.to_owned())
            .or_insert_with(|| Tool::running(name, at_ms, None, SubCall::default()));
        if let Some(input) = input {
            tool.set_input(input);
        }
        self.emit_tool(sub, id);
    }

    /// One assistant message's blocks, or the next of them: Claude writes a
    /// message a block at a time on both of its channels, so each block is
    /// keyed by its message and its place in it.
    pub(crate) fn assistant(
        &mut self,
        sub: &mut Subagent<'_, Ask>,
        message_id: &str,
        model: &str,
        blocks: &[ContentBlock],
        at_ms: i64,
    ) {
        if !model.is_empty() && model != "<synthetic>" {
            self.model = Some(model.to_owned());
        }
        let base = self.blocks.get(message_id).copied().unwrap_or(0);
        self.blocks
            .insert(message_id.to_owned(), base + blocks.len() as u32);
        for (offset, block) in blocks.iter().enumerate() {
            let key = format!("{message_id}:{}", base + offset as u32);
            let text = |kind, text: &str| ItemDraft {
                key: key.clone(),
                text: text.to_owned(),
                body: item_body(kind),
                at_ms: Some(at_ms),
                complete: true,
                ..Default::default()
            };
            match block {
                ContentBlock::Text { text: said, .. } => {
                    let (said, attachments) = crate::shared::parse_reply(said.clone());
                    sub.item(ItemDraft {
                        attachments,
                        ..text(
                            claude_item::Kind::Message(wire::Text { complete: true }),
                            &said,
                        )
                    });
                }
                ContentBlock::Thinking { thinking, .. } => sub.item(text(
                    claude_item::Kind::Thinking(wire::Thinking { complete: true }),
                    thinking,
                )),
                // Claude keeps a redacted block's thinking to itself.
                ContentBlock::RedactedThinking { .. } => sub.item(text(
                    claude_item::Kind::Thinking(wire::Thinking { complete: true }),
                    "",
                )),
                ContentBlock::ToolUse {
                    id, name, input, ..
                } => self.tool_seen(sub, id, name, Some(input), at_ms),
                other => sub.item(text(
                    claude_item::Kind::Unrecognized(wire::Unrecognized {
                        fact_type: format!("assistant/{}", other.kind()),
                        summary: String::new(),
                    }),
                    "",
                )),
            }
        }
    }

    /// One user message of the subagent's: what its parent's model asked of
    /// it (its prompt, or a message continuing it), or its calls' results.
    /// `key` keys a prompt's row; `result` is the message's structured
    /// result of a call. `tasks` is the session's task list: Claude keeps
    /// one for a host and all its subagents, so a subagent's task tools
    /// change the list its host's snapshot shows.
    pub(crate) fn user(
        &mut self,
        sub: &mut Subagent<'_, Ask>,
        key: &str,
        content: &MessageContent,
        result: Option<&Value>,
        tasks: &mut Option<Vec<Task>>,
        at_ms: i64,
    ) {
        if let MessageContent::Blocks(blocks) = content {
            let mut results = false;
            for block in blocks {
                if let ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                    ..
                } = block
                {
                    results = true;
                    if let Some(tool) = self.tools.get(tool_use_id) {
                        tool.apply_to_tasks(tasks, result.unwrap_or(&Value::Null));
                    }
                    self.tool_result(
                        sub,
                        tool_use_id,
                        content.as_ref(),
                        is_error.unwrap_or(false),
                        result,
                        at_ms,
                    );
                }
            }
            if results {
                return;
            }
        }
        let text = message_text(content);
        if claude_note(&text) || key.is_empty() {
            return;
        }
        let (text, attachments) = crate::shared::parse_reply(text);
        sub.item(ItemDraft {
            key: key.to_owned(),
            text,
            attachments,
            body: item_body(claude_item::Kind::Prompt(wire::Prompt {})),
            at_ms: Some(at_ms),
            complete: true,
            ..Default::default()
        });
    }

    fn tool_result(
        &mut self,
        sub: &mut Subagent<'_, Ask>,
        id: &str,
        content: Option<&ToolResultBody>,
        is_error: bool,
        result: Option<&Value>,
        at_ms: i64,
    ) {
        let output = tool_result_text(content);
        let Some(tool) = self.tools.get_mut(id) else {
            return;
        };
        if !is_error && launched_in_background(result, &output) {
            tool.of.launched = true;
            tool.background = true;
            return self.emit_tool(sub, id);
        }
        let denied = tool
            .decision
            .as_ref()
            .is_some_and(|decision| decision.outcome == DecisionOutcome::Denied as i32);
        let refused = is_error && output.starts_with(crate::claude_answers::REJECTED);
        tool.state = if denied {
            ToolState::Denied
        } else if refused || (is_error && output.starts_with(INTERRUPTED)) {
            ToolState::Cancelled
        } else if is_error {
            ToolState::Failed
        } else {
            ToolState::Succeeded
        } as i32;
        tool.outcome_text = if denied && refused {
            String::new()
        } else {
            output
        };
        for write in tool.take_result(content, result) {
            sub.effect(write);
        }
        if tool.question && tool.asked.is_none() {
            tool.asked = Some(match result.map(AnsweredResult::deserialize) {
                Some(Ok(answered)) if !is_error && !answered.answers.is_empty() => {
                    ask_item::recorded_by_claude(
                        &question_ask_text(&tool.input),
                        &answered.answers,
                        &answered.annotations,
                    )
                }
                _ => ask_item::dismissed(),
            });
        }
        tool.ended_at_ms.get_or_insert(at_ms);
        self.emit_tool(sub, id);
        self.tools.remove(id);
    }

    /// A decision on one of its calls, reached on an ask it raised.
    pub(crate) fn decide(
        &mut self,
        sub: &mut Subagent<'_, Ask>,
        id: &str,
        decided: Decided,
        now_ms: i64,
    ) {
        if let Some(tool) = self.tools.get_mut(id) {
            apply_decided(tool, decided, now_ms);
            self.emit_tool(sub, id);
        }
    }

    /// An ask it raised closed with no answer: a call it held that is still
    /// in flight will never run as asked, so it reads cancelled until
    /// Claude says more.
    pub(crate) fn dismissed(&mut self, sub: &mut Subagent<'_, Ask>, call: &str, now_ms: i64) {
        let Some(tool) = self.tools.get_mut(call) else {
            return;
        };
        if let Some(plan) = tool.plan.as_mut().filter(|plan| plan.undecided()) {
            *plan = crate::claude_common::Verdict::of(wire::PlanVerdict::Dismissed);
        }
        if tool.question && tool.asked.is_none() {
            tool.asked = Some(ask_item::dismissed());
        }
        if matches!(
            ToolState::try_from(tool.state),
            Ok(ToolState::Pending | ToolState::Running)
        ) {
            tool.state = ToolState::Cancelled as i32;
            tool.ended_at_ms.get_or_insert(now_ms);
        }
        self.emit_tool(sub, call);
    }

    /// One of its calls now opens the provider subagent `child`: the
    /// Agent call that started it, or a SendMessage that continued it.
    /// False when the call is not one of its own.
    pub(crate) fn link(&mut self, sub: &mut Subagent<'_, Ask>, call: &str, child: &[u8]) -> bool {
        let Some(tool) = self.tools.get_mut(call) else {
            return false;
        };
        if tool.child_agent_id != child {
            tool.child_agent_id = child.to_vec();
            self.emit_tool(sub, call);
        }
        true
    }

    /// Whether one of its calls is `call`, still open.
    pub(crate) fn holds(&self, call: &str) -> bool {
        self.tools.contains_key(call)
    }

    /// A call it launched into the background finished: a nested
    /// subagent's end, or its shell's. `text` is what the task reported.
    pub(crate) fn finish_launched(
        &mut self,
        sub: &mut Subagent<'_, Ask>,
        call: &str,
        state: ToolState,
        text: Option<&str>,
        at_ms: i64,
    ) {
        let Some(tool) = self.tools.get_mut(call).filter(|tool| tool.of.launched) else {
            return;
        };
        tool.state = state as i32;
        if let Some(text) = text.filter(|text| !text.is_empty()) {
            text.clone_into(&mut tool.outcome_text);
        }
        tool.ended_at_ms.get_or_insert(at_ms);
        self.emit_tool(sub, call);
        self.tools.remove(call);
    }

    /// Whether it has a call to tool server `server` still running: a form
    /// that server raises mid-call is that call's.
    pub(crate) fn calls_server(&self, server: &str) -> bool {
        self.tools.values().any(|tool| {
            tool.server == server && tool.state == ToolState::Running as i32 && !tool.of.launched
        })
    }

    /// It is ending: a call still running will not finish, so it ends as
    /// cancelled, with `at_ms` as its end.
    pub(crate) fn close(&mut self, sub: &mut Subagent<'_, Ask>, at_ms: i64) {
        let open: Vec<String> = self.tools.keys().cloned().collect();
        for id in open {
            if let Some(tool) = self.tools.get_mut(&id)
                && matches!(
                    ToolState::try_from(tool.state),
                    Ok(ToolState::Pending | ToolState::Running)
                )
            {
                tool.state = ToolState::Cancelled as i32;
                tool.ended_at_ms.get_or_insert(at_ms);
            }
            self.emit_tool(sub, &id);
        }
        self.tools.clear();
        self.blocks.clear();
    }

    /// Its snapshot: its open asks, the jobs it lists (its own, where it
    /// can be stopped, and the background commands it started), the model
    /// it runs and what it says it is working on. Its title is left out:
    /// the start carries it, and the daemon keeps it from there.
    pub(crate) fn snapshot(&self, sub: &mut Subagent<'_, Ask>, jobs: Vec<BackgroundJob>) {
        // What its row counts as running in the background leaves out its
        // own job, which its chat stops and which is the subagent itself.
        let count = jobs
            .iter()
            .filter(|job| job.kind != wire::BackgroundKind::Subagent as i32)
            .count() as u32;
        let asks = sub.asks().open_asks().to_vec();
        // What it asks of the person, for its row to say, as a Codex
        // child's and every agent's snapshot carries.
        let notice = asks
            .first()
            .map(|head| Box::new(crate::notice::claude_ask(head, asks.len() as u32 - 1, "")));
        let body = ClaudeSnapshot {
            asks,
            model: self.model.clone(),
            model_name: self.model.as_deref().map(crate::claude_common::tidy_model),
            background_jobs: Some(wire::BackgroundJobs { known: true, jobs }),
            ..unknown::claude()
        }
        .encode_to_vec();
        sub.snapshot(wire::Snapshot {
            phase: sub.phase() as i32,
            body,
            working_on: self.working_on.clone(),
            background_jobs: count,
            notice,
            ..Default::default()
        });
    }
}

/// Writes the own item of an ask that is not a call's (a tool server's
/// form or link), open or closed, into whichever part `item` lands in.
pub(crate) fn ask_draft(ask: &Ask, closed: Option<AskClosed>) -> Option<ItemDraft> {
    let asked = match &ask.body {
        Some(wire::ask::Body::Form(form)) => wire::ask_item::Ask::Form(form.clone()),
        Some(wire::ask::Body::Link(link)) => wire::ask_item::Ask::Link(link.clone()),
        Some(wire::ask::Body::Question(question)) => {
            wire::ask_item::Ask::Question(question.clone())
        }
        _ => return None,
    };
    let item = ask_item::opened(asked);
    let item = match closed {
        Some(closed) => ask_item::close(item, closed),
        None => item,
    };
    Some(ItemDraft {
        key: ask.item_key.clone(),
        body: item_body(claude_item::Kind::Ask(item)),
        at_ms: Some(ask.opened_at_ms),
        complete: true,
        ..Default::default()
    })
}

/// The cause a Claude task's final status names for a subagent's end.
pub(crate) fn end_cause(status: &str, summary: &str) -> wire::IncarnationEnd {
    let (cause, detail) = match status {
        "completed" => (wire::ExitCause::Finished, None),
        "failed" => (
            wire::ExitCause::ProviderExited,
            Some(if summary.is_empty() {
                "failed".to_owned()
            } else {
                format!("failed: {summary}")
            }),
        ),
        _ => (wire::ExitCause::Stopped, None),
    };
    wire::IncarnationEnd {
        cause: cause as i32,
        detail,
    }
}

/// How the call row that started a subagent ends with the subagent.
pub(crate) fn call_state(status: &str) -> ToolState {
    match status {
        "completed" => ToolState::Succeeded,
        "failed" => ToolState::Failed,
        _ => ToolState::Cancelled,
    }
}
