//! Terminal Claude's subagents as agents of their own.
//!
//! Terminal Claude has no stream: a subagent's conversation is the rows of
//! its own transcript file, `<session>/subagents/agent-<id>.jsonl`, which the
//! agent process follows once asked to and delivers on the subagent
//! transcript channel, each row naming its subagent (`agentId`). Its start,
//! asks and end come from hooks and the parent's rows, so how they line up
//! is inferred, and every rule for it lives here:
//!
//! - **Following.** Claude's `SubagentStart` hook names a subagent (an ask
//!   naming an id not seen before does too); the interpreter asks the agent
//!   process to follow its file. A hook naming no agent type is one of
//!   Claude's own internal helpers, which has no file and makes no agent.
//!   Before the first row the agent process reports the subagent's
//!   `meta.json`, which names the call that started it (`toolUseId`) and,
//!   for a nested one, its parent subagent (`parentAgentId`).
//! - **Start.** The subagent starts once the call that started it is drawn
//!   (in the agent's chat, or its parent subagent's), in that call's step,
//!   or at its first row or ask if those come first.
//! - **Continuation.** Nothing of a subagent is kept past its end, so its
//!   parent's model continuing it with SendMessage meets a subagent amux
//!   holds no record of: `SubagentStart` (or an ask) naming it while a
//!   SendMessage call to it is open. It starts again at once under that
//!   call, in the chat of whoever made it, as the same agent (its id comes
//!   from Claude's), and its rows go on in the same file.
//! - **End.** `SubagentStop` is no end: Claude writes a subagent's last row
//!   after it, and a subagent stopped, interrupted or killed with its host
//!   gets none. Its end is its parent's record of it: the task notification
//!   in the parent's transcript (its queued copy or its prompt row), or the
//!   result of the Agent call it ran under in the foreground, or the host's
//!   exit. On that the agent process is asked to read the subagent's file
//!   to its end, waiting a bounded moment for a completed subagent's last
//!   reply (the text the notification carries), and says when it has; the
//!   subagent ends then, or as soon as that reply is read, or once the wait
//!   is over if the agent process never says. Its record goes with its end.
//!   Only how the run ended stays, while the SendMessage that continued it
//!   waits for Claude's result, which can be read after the notice and then
//!   hands it back.
//! - **Asks** a subagent raises arrive on the hooks naming it, open in its
//!   own snapshot and are answered on the hook. A tool server's form names
//!   no agent: it is the one agent's with a call to that server running.
//! - **Status.** The PreToolUse hook names the subagent before its call to
//!   amux's status tool, so what it says it works on is its own.
//! - **Stop.** amux has no per-job stop for terminal Claude: a subagent's
//!   snapshot lists no job of its own.

use std::collections::BTreeMap;

use claude_protocol::stream::{ContentBlock, MessageContent};
use claude_protocol::transcript::{self, Row};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use wire::{BackgroundJob, DecisionOutcome, KindInput, NativeStart, Refusal, ToolState};

use super::{Decision, State};
use crate::claude_common::{JobInput, item_body, timestamp_ms, tool_result_text};
use crate::claude_subagent::{
    AGENT_TOOL, Conversation, SEND_MESSAGE_TOOL, call_state, end_cause, returned_status,
};
use crate::{Effect, Emit, ItemDraft};

/// How long a subagent's end waits for its file to be read to its end
/// before it ends anyway: Claude writes a completed subagent's last reply
/// about a tenth of a second after the parent learns of its end. The end
/// usually comes from reading that reply, here or in the agent process,
/// which says the file is drained as soon as it has read it and gives up
/// after the same time.
pub(super) const DRAIN_WAIT_MS: i64 = 2_000;

/// Who a subagent's prompt came from when its parent's SendMessage
/// continued it, as Claude 2.1.326 marks the row.
const COORDINATOR: &str = "coordinator";
/// The lines Claude 2.1.326 wraps a continuing message in, for the model.
pub(crate) const CONTINUED_PREAMBLE: &str =
    "The coordinator sent a message while you were working:\n";
pub(crate) const CONTINUED_CLOSING: &str = "\n\nAddress this before completing your current task.";

/// The message a SendMessage continued a subagent with, from the row
/// Claude writes for it in the subagent's file: a note to the model from
/// the coordinator, wrapped in Claude's own lines, which are left out. A
/// row of any other note is None.
fn continuation_message(user: &transcript::UserRow) -> Option<String> {
    if user.origin.as_ref()?.kind != COORDINATOR {
        return None;
    }
    let text = crate::claude_common::message_text(&user.message.content);
    let message = text
        .trim_start()
        .strip_prefix(CONTINUED_PREAMBLE)
        .unwrap_or(&text);
    let message = message.strip_suffix(CONTINUED_CLOSING).unwrap_or(message);
    Some(message.to_owned())
}

/// Whether `said`, what a subagent last said, is `reply`, the last reply
/// its parent's notice names.
pub(super) fn is_last_reply(said: &str, reply: &str) -> bool {
    said.trim() == reply.trim()
}

/// What a row of a subagent's own transcript says, as its conversation
/// takes it for what the subagent last said: an assistant row's text, when
/// it has any.
pub(super) fn row_said(row: &[u8]) -> Option<String> {
    let Ok(Row::Assistant(assistant)) = transcript::decode(row) else {
        return None;
    };
    if assistant.is_api_error_message == Some(true) {
        return None;
    }
    Some(crate::claude_common::blocks_text(
        &assistant.message.content,
    ))
    .filter(|said| !said.is_empty())
}

/// Where a subagent is in its life; it has no record once it ended.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(super) enum Phase {
    /// Claude named it; it starts once its call is drawn.
    #[default]
    Waiting,
    Running,
}

/// One subagent Claude's Agent tool started, by Claude's agent id for it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(super) struct Sub {
    /// The call that started this run of it: the Agent call its
    /// `meta.json` names, or the SendMessage that continued it. None until
    /// one is known.
    pub(super) call: Option<String>,
    /// Its parent subagent, for a nested one.
    pub(super) parent: Option<String>,
    /// What Claude calls its task: the row's title.
    pub(super) title: String,
    pub(super) phase: Phase,
    /// This incarnation's agent process follows its file.
    pub(super) followed: bool,
    /// Its parent learned it ended, and its file is being read to its end.
    pub(super) ending: Option<Ending>,
    /// How this run ended, as its parent's notice said, while its file is
    /// read to its end: what a call that continued it hands back when
    /// Claude's result of that call is read only after the notice. None
    /// before the notice.
    #[serde(default)]
    pub(super) outcome: Option<Outcome>,
    pub(super) conversation: Conversation,
}

/// How a subagent's run ended, for the call that ran it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct Outcome {
    /// The call's `ToolState`.
    pub(super) state: i32,
    pub(super) text: String,
}

/// A subagent's end its parent reported, waiting for its last rows.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct Ending {
    /// Why it ends, as an `IncarnationEnd`'s cause and detail.
    pub(super) cause: i32,
    pub(super) detail: Option<String>,
    /// The last reply a completed subagent's file still owes.
    pub(super) last_reply: Option<String>,
    /// When it ends whatever has been read.
    pub(super) by_ms: i64,
}

impl Ending {
    fn end(&self) -> wire::IncarnationEnd {
        wire::IncarnationEnd {
            cause: self.cause,
            detail: self.detail.clone(),
        }
    }
}

/// What every subagent transcript row says beside its row: whose it is.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Tagged {
    #[serde(default)]
    agent_id: String,
}

/// A subagent's `meta.json`, as far as amux reads it.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Meta {
    #[serde(default)]
    tool_use_id: String,
    #[serde(default)]
    parent_agent_id: Option<String>,
    #[serde(default)]
    description: String,
}

/// A task notification's fields, as Claude writes them in its markup.
struct Notified<'a> {
    task: &'a str,
    status: &'a str,
    call: Option<&'a str>,
    result: Option<&'a str>,
    summary: &'a str,
}

/// The text between `<tag>` and `</tag>`, trimmed.
fn between<'a>(text: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = text.find(&open)? + open.len();
    let end = text[start..].find(&close)? + start;
    Some(text[start..end].trim())
}

fn notified(text: &str) -> Option<Notified<'_>> {
    if !text.trim_start().starts_with("<task-notification>") {
        return None;
    }
    Some(Notified {
        task: between(text, "task-id")?,
        status: between(text, "status").unwrap_or_default(),
        call: between(text, "tool-use-id"),
        // The result may itself hold markup: it runs to its last close.
        result: text.find("<result>").and_then(|start| {
            let body = &text[start + "<result>".len()..];
            body.rfind("</result>").map(|end| body[..end].trim())
        }),
        summary: between(text, "summary").unwrap_or_default(),
    })
}

/// What a SendMessage call's result says it did, as far as amux reads it.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Resumed {
    #[serde(default)]
    resumed_agent_id: Option<String>,
}

/// A SendMessage call's input, as far as amux reads it.
#[derive(Deserialize)]
struct SendMessageInput {
    #[serde(default)]
    to: Option<String>,
}

/// An Agent call's immediate result, as far as amux reads it.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Launch {
    #[serde(default)]
    is_async: bool,
    #[serde(default)]
    status: Option<String>,
}

/// A Bash call's immediate result, as far as amux reads it.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ShellLaunch {
    #[serde(default)]
    background_task_id: Option<String>,
}

/// The subagent a SendMessage call continued, from its result or the text
/// of it, else the input that named it.
fn continued(input: &str, result: Option<&Value>, output: &str) -> Option<String> {
    result
        .and_then(|result| Resumed::deserialize(result).ok())
        .or_else(|| serde_json::from_str::<Resumed>(output).ok())
        .and_then(|resumed| resumed.resumed_agent_id)
        .or_else(|| serde_json::from_str::<SendMessageInput>(input).ok()?.to)
        .filter(|id| !id.is_empty())
}

/// Whether a call's result says only that it launched a subagent in the
/// background.
fn launched(result: Option<&Value>, output: &str) -> bool {
    result
        .and_then(|result| Launch::deserialize(result).ok())
        .is_some_and(|launch| launch.is_async || launch.status.as_deref() == Some("async_launched"))
        || output.starts_with("Async agent launched")
}

/// The task id of a shell a call started in the background, from its
/// result.
fn background_shell(result: Option<&Value>) -> Option<String> {
    ShellLaunch::deserialize(result?)
        .ok()?
        .background_task_id
        .filter(|id| !id.is_empty())
}

impl State {
    /// A person's input to one of the subagents: an answer to an ask it
    /// raised. Terminal Claude has no per-job stop, so nothing else reaches
    /// a subagent.
    pub(super) fn child_input(
        &mut self,
        emit: &mut Emit,
        agent_id: &[u8],
        id: Vec<u8>,
        of: wire::child_input::Of,
    ) {
        let Some(child) = self.shared.subagent_child(agent_id).map(str::to_owned) else {
            return self.shared.reject(emit, &id, Refusal::Unsupported);
        };
        let wire::child_input::Of::Input(input) = of else {
            return self
                .shared
                .subagent(emit, &child)
                .reject(&id, Refusal::Unsupported);
        };
        match input.decode_body() {
            Some(KindInput::ClaudePty(wire::ClaudePtyInput {
                of: Some(wire::claude_pty_input::Of::Answer(answer)),
            })) => self.answer(emit, &id, answer, Some(&child)),
            _ => self
                .shared
                .subagent(emit, &child)
                .reject(&id, Refusal::Unsupported),
        }
    }

    /// The subagent `child` raised an ask, so it runs: one amux holds no
    /// record of starts, its file followed from now on, under the
    /// SendMessage that continued it when one did, before its start hook
    /// said so. False when the ask cannot be its own.
    pub(super) fn subagent_asking(&mut self, emit: &mut Emit, child: &str) -> bool {
        if !self.subagents.contains_key(child) {
            if !self.subagent_named(emit, child, "subagent") {
                return false;
            }
            self.continued_by_call(child);
        }
        self.start_subagent(emit, child);
        self.shared.subagent_running(child)
    }

    /// Claude named the subagent `child` (its start hook, or an ask): its
    /// file is followed from now on. False for one of Claude's internal
    /// helpers, which makes no agent.
    pub(super) fn subagent_named(
        &mut self,
        emit: &mut Emit,
        child: &str,
        agent_type: &str,
    ) -> bool {
        if child.is_empty() || (agent_type.is_empty() && !self.subagents.contains_key(child)) {
            return false;
        }
        let sub = self.subagents.entry(child.to_owned()).or_default();
        if !sub.followed {
            sub.followed = true;
            emit.effect(Effect::FollowSubagent {
                agent_id: child.to_owned(),
            });
        }
        true
    }

    /// Claude's SubagentStart hook: a subagent starts, or its parent's model
    /// continued one that ended, which starts again at once under the
    /// SendMessage that continued it.
    pub(super) fn subagent_start_hook(&mut self, emit: &mut Emit, child: &str, agent_type: &str) {
        let known = self.subagents.contains_key(child);
        let followed = self.subagents.get(child).is_some_and(|sub| sub.followed);
        if !self.subagent_named(emit, child, agent_type) {
            return;
        }
        // Its file is followed already, as a running subagent's is when its
        // nested one's notice wakes it: asked again, the agent process wakes
        // a follower that polls the idle file slowly. Following the file of
        // a continued one, whose record went with its end, also starts anew
        // a follower that ended on a file it could not read.
        if followed {
            emit.effect(Effect::FollowSubagent {
                agent_id: child.to_owned(),
            });
        }
        if !known && self.continued_by_call(child) {
            self.start_subagent(emit, child);
        }
    }

    /// The open SendMessage call that continues the subagent `child`, and
    /// the subagent whose call it is (None for the agent's own): the one its
    /// PreToolUse hook announced, else the newest drawn and not yet
    /// answered. None when no open call names it, as for a subagent never
    /// seen before.
    fn continuing_call(&self, child: &str) -> Option<(String, Option<String>)> {
        let names = |name: &str, input: &str| {
            name == SEND_MESSAGE_TOOL && continued(input, None, "").as_deref() == Some(child)
        };
        if let Some(call) = self
            .announced
            .iter()
            .rev()
            .find(|call| names(&call.name, &call.input))
        {
            return Some((call.id.clone(), call.agent.clone()));
        }
        if let Some((id, _)) = self
            .tools
            .iter()
            .filter(|(_, tool)| !tool.of.finished && names(&tool.name, &tool.input))
            .max_by_key(|(_, tool)| tool.of.seq)
        {
            return Some((id.clone(), None));
        }
        self.subagents
            .iter()
            .filter(|(_, sub)| sub.phase == Phase::Running)
            .find_map(|(holder, sub)| {
                sub.conversation
                    .tools
                    .iter()
                    .find(|(_, tool)| names(&tool.name, &tool.input))
                    .map(|(id, _)| (id.clone(), Some(holder.clone())))
            })
    }

    /// The subagent `child`, whose record was just made, is one an open
    /// SendMessage continues: its run is that call's, in the chat that holds
    /// it. False when no open call names it.
    fn continued_by_call(&mut self, child: &str) -> bool {
        let Some((call, parent)) = self.continuing_call(child) else {
            return false;
        };
        let Some(sub) = self.subagents.get_mut(child) else {
            return false;
        };
        sub.call = Some(call);
        sub.parent = parent;
        true
    }

    /// Whether a SendMessage of the agent's own that continued the subagent
    /// `child` still waits for Claude's result.
    fn continuation_pending(&self, child: &str) -> bool {
        let agent = self.shared.subagent_id(child);
        self.tools.values().any(|tool| {
            tool.name == SEND_MESSAGE_TOOL
                && !tool.of.finished
                && (tool.child_agent_id == agent
                    || continued(&tool.input, None, "").as_deref() == Some(child))
        })
    }

    /// Lets go of how each finished subagent's run ended once the
    /// SendMessage that continued it has settled.
    pub(super) fn drop_settled_runs(&mut self) {
        let settled: Vec<String> = self
            .ended_runs
            .keys()
            .filter(|child| !self.continuation_pending(child))
            .cloned()
            .collect();
        for child in settled {
            self.ended_runs.remove(&child);
        }
    }

    /// The subagent's `meta.json`, which the agent process reads before its
    /// first row: the call that started it, its parent and its title.
    pub(super) fn subagent_meta(&mut self, emit: &mut Emit, child: &str, meta: Meta) {
        let Some(sub) = self.subagents.get_mut(child) else {
            return;
        };
        // Claude rewrites it as the subagent runs; the first says it all.
        if sub.call.is_none() && !meta.tool_use_id.is_empty() {
            sub.call = Some(meta.tool_use_id);
            sub.parent = meta.parent_agent_id.filter(|parent| !parent.is_empty());
        }
        if sub.title.is_empty() {
            sub.title = meta.description;
        }
        self.start_waiting(emit);
    }

    /// Starts each waiting subagent whose call is drawn now, in the step
    /// that draws it, and links each running one whose call row is only
    /// drawn now: a subagent's file can start before the parent's
    /// transcript writes the call that launched it.
    pub(super) fn start_waiting(&mut self, emit: &mut Emit) {
        let ready: Vec<(String, String, Phase)> = self
            .subagents
            .iter()
            .filter_map(|(child, sub)| {
                let call = sub.call.clone()?;
                self.call_holder(&call)?;
                Some((child.clone(), call, sub.phase))
            })
            .collect();
        for (child, call, phase) in ready {
            match phase {
                Phase::Waiting => self.start_subagent(emit, &child),
                Phase::Running => self.link(emit, &call, &child),
            }
        }
    }

    /// Claude's id for the subagent whose agent id is `agent_id`: one amux
    /// holds a record of, or one whose last run's end a SendMessage still
    /// waits to hand back.
    pub(super) fn subagent_opened(&self, agent_id: &[u8]) -> Option<String> {
        self.subagents
            .keys()
            .chain(self.ended_runs.keys())
            .find(|child| self.shared.subagent_id(child) == agent_id)
            .cloned()
    }

    /// How the subagent `child`'s latest run ended, as its notice said:
    /// None before the notice.
    pub(super) fn run_outcome(&self, child: &str) -> Option<Outcome> {
        self.subagents
            .get(child)
            .and_then(|sub| sub.outcome.clone())
            .or_else(|| self.ended_runs.get(child).cloned())
    }

    /// Whose chat holds the call `call`: Some(None) for the agent's own,
    /// Some(Some(child)) for a running subagent's.
    fn call_holder(&self, call: &str) -> Option<Option<String>> {
        if self.tools.contains_key(call) {
            return Some(None);
        }
        self.subagents
            .iter()
            .find(|(_, sub)| sub.phase == Phase::Running && sub.conversation.holds(call))
            .map(|(child, _)| Some(child.clone()))
    }

    /// Starts the subagent `child` and rewrites the call that started it to
    /// open its chat.
    pub(super) fn start_subagent(&mut self, emit: &mut Emit, child: &str) {
        let Some(sub) = self.subagents.get_mut(child) else {
            return;
        };
        if sub.phase == Phase::Running {
            return;
        }
        sub.phase = Phase::Running;
        sub.ending = None;
        sub.outcome = None;
        self.ended_runs.remove(child);
        let call_agent_id = match &sub.parent {
            Some(parent) => self.shared.subagent_id(parent),
            None => self.shared.agent_id().to_vec(),
        };
        let start = NativeStart {
            call_agent_id,
            call_key: sub.call.clone().unwrap_or_default(),
            title: sub.title.clone(),
            ..Default::default()
        };
        let call = sub.call.clone();
        self.shared.subagent(emit, child).start(start);
        if let Some(call) = call {
            self.link(emit, &call, child);
        }
    }

    /// The call `call` opens the subagent `child`'s chat, in whichever
    /// chat holds it.
    fn link(&mut self, emit: &mut Emit, call: &str, child: &str) {
        let agent_id = self.shared.subagent_id(child);
        match self.call_holder(call) {
            Some(None) => {
                if let Some(tool) = self.tools.get_mut(call)
                    && tool.child_agent_id != agent_id
                {
                    tool.child_agent_id = agent_id;
                    self.emit_tool(emit, call);
                }
            }
            Some(Some(holder)) => {
                if let Some(sub) = self.subagents.get_mut(&holder) {
                    let mut view = self.shared.subagent(emit, &holder);
                    sub.conversation.link(&mut view, call, &agent_id);
                }
            }
            None => {}
        }
    }

    /// The result of a call of the agent's own or of a subagent's
    /// (`holder`): the end of a subagent that ran under it in the
    /// foreground, the subagent a SendMessage continued, or a shell a
    /// subagent sent to the background.
    pub(super) fn call_returned(
        &mut self,
        emit: &mut Emit,
        holder: Option<&str>,
        id: &str,
        is_error: bool,
        output: &str,
        result: Option<&Value>,
    ) {
        let input = match holder {
            None => self
                .tools
                .get(id)
                .map(|tool| (tool.name.clone(), tool.input.clone())),
            Some(holder) => self
                .subagents
                .get(holder)
                .and_then(|sub| sub.conversation.tools.get(id))
                .map(|tool| (tool.name.clone(), tool.input.clone())),
        };
        let Some((name, input)) = input else {
            return;
        };
        if name == SEND_MESSAGE_TOOL
            && !is_error
            && let Some(child) = continued(&input, result, output)
            && (self.subagents.contains_key(&child) || self.ended_runs.contains_key(&child))
        {
            self.link(emit, id, &child);
        }
        if name == AGENT_TOOL && !launched(result, output) {
            let ran = self
                .subagents
                .iter()
                .find(|(_, sub)| sub.call.as_deref() == Some(id) && sub.phase == Phase::Running)
                .map(|(child, _)| child.clone());
            if let Some(child) = ran {
                let status = returned_status(is_error, output);
                self.begin_end(emit, &child, end_cause(status, output), None);
            }
        }
        if let (Some(holder), Some(task)) = (holder, background_shell(result)) {
            let (command, at_ms) = self
                .subagents
                .get(holder)
                .and_then(|sub| sub.conversation.tools.get(id))
                .map_or((String::new(), self.shared.now_ms()), |tool| {
                    (JobInput::command(&tool.input), tool.at_ms)
                });
            self.jobs
                .launched(&task, id, command, at_ms, wire::BackgroundKind::Shell);
            self.owned_tasks.insert(task, holder.to_owned());
        }
    }

    /// A task notification in the parent's transcript, as its queued copy or
    /// its prompt row: for a subagent, its end.
    pub(super) fn notification_seen(&mut self, emit: &mut Emit, text: &str) {
        let Some(notified) = notified(text) else {
            return;
        };
        let task = notified.task.to_owned();
        if let Some(owner) = self.owned_tasks.remove(&task) {
            // A shell a subagent ran in the background ended.
            if let Some(jobs) = self.jobs.ended(&task) {
                let jobs = self.host_jobs(jobs);
                self.shared.set_jobs(jobs);
            }
            let now = self.shared.now_ms();
            if let (Some(call), Some(sub)) = (notified.call, self.subagents.get_mut(&owner))
                && sub.phase == Phase::Running
            {
                let mut view = self.shared.subagent(emit, &owner);
                sub.conversation.finish_launched(
                    &mut view,
                    call,
                    call_state(notified.status),
                    Some(notified.summary),
                    now,
                );
            }
            return;
        }
        let running = self
            .subagents
            .get(&task)
            .is_some_and(|sub| sub.phase == Phase::Running && sub.ending.is_none());
        if !running {
            return;
        }
        // Its last reply is the result a completed subagent reports.
        let reply = (notified.status == "completed")
            .then(|| notified.result.map(str::to_owned))
            .flatten();
        let end = end_cause(notified.status, notified.summary);
        let state = call_state(notified.status);
        let detail = notified.result.unwrap_or(notified.summary).to_owned();
        if let Some(sub) = self.subagents.get_mut(&task) {
            sub.outcome = Some(Outcome {
                state: state as i32,
                text: detail.clone(),
            });
        }
        self.finish_calls(emit, &task, notified.call, state, &detail);
        // Claude answers the notice with a turn of its own, there at once
        // when the agent is idle, and its queued copy can land before the
        // row that opens that turn: the agent works from this step, so a
        // host that waits for its subagents does not leave before that turn.
        // A nested subagent's notice goes to the subagent that started it.
        if self
            .subagents
            .get(&task)
            .is_some_and(|sub| sub.parent.is_none())
        {
            self.shared.turn_started();
        }
        self.begin_end(emit, &task, end, reply);
    }

    /// The calls waiting on the subagent `child` end with it, as `state`
    /// with `text`: the one that first started it, in whichever chat holds
    /// it, `latest`, the one the notice names, and any continuation still
    /// waiting on it, which the notice need not name.
    fn finish_calls(
        &mut self,
        emit: &mut Emit,
        child: &str,
        latest: Option<&str>,
        state: ToolState,
        text: &str,
    ) {
        let now = self.shared.now_ms();
        let mut calls: Vec<String> = self
            .subagents
            .get(child)
            .and_then(|sub| sub.call.clone())
            .into_iter()
            .collect();
        let agent = self.shared.subagent_id(child);
        let waiting = self
            .tools
            .iter()
            .filter(|(_, tool)| {
                tool.child_agent_id == agent && tool.of.awaiting_notification && !tool.of.finished
            })
            .map(|(id, _)| id.clone())
            .chain(self.subagents.values().flat_map(|sub| {
                sub.conversation
                    .tools
                    .iter()
                    .filter(|(_, tool)| tool.child_agent_id == agent && tool.of.launched)
                    .map(|(id, _)| id.clone())
            }))
            .collect::<Vec<_>>();
        for call in latest.map(str::to_owned).into_iter().chain(waiting) {
            if !calls.contains(&call) {
                calls.push(call);
            }
        }
        for call in calls {
            match self.call_holder(&call) {
                Some(None) => {
                    let Some(tool) = self
                        .tools
                        .get_mut(&call)
                        .filter(|tool| tool.of.awaiting_notification && !tool.of.finished)
                    else {
                        continue;
                    };
                    tool.of.finished = true;
                    tool.state = state as i32;
                    text.clone_into(&mut tool.outcome_text);
                    tool.ended_at_ms.get_or_insert(now);
                    self.emit_tool(emit, &call);
                }
                Some(Some(holder)) => {
                    if let Some(sub) = self.subagents.get_mut(&holder) {
                        let mut view = self.shared.subagent(emit, &holder);
                        sub.conversation
                            .finish_launched(&mut view, &call, state, Some(text), now);
                    }
                }
                None => {}
            }
        }
    }

    /// The subagent `child`'s parent learned it ended. It ends once its
    /// file is read to its end: now, if its last reply `reply` is already
    /// read, else when the agent process says it has read it all, its last
    /// reply arrives, or the wait is over.
    pub(super) fn begin_end(
        &mut self,
        emit: &mut Emit,
        child: &str,
        end: wire::IncarnationEnd,
        reply: Option<String>,
    ) {
        let now = self.shared.now_ms();
        let Some(sub) = self.subagents.get_mut(child) else {
            return;
        };
        if sub.phase != Phase::Running || sub.ending.is_some() {
            return;
        }
        if reply
            .as_deref()
            .is_some_and(|reply| is_last_reply(&sub.conversation.last_said, reply))
        {
            return self.end_subagent(emit, child, end);
        }
        sub.ending = Some(Ending {
            cause: end.cause,
            detail: end.detail,
            last_reply: reply.clone(),
            by_ms: now + DRAIN_WAIT_MS,
        });
        emit.effect(Effect::DrainSubagent {
            agent_id: child.to_owned(),
            last_reply: reply,
        });
    }

    /// The agent process read the subagent's file to its end: an end that
    /// waited for it ends it.
    pub(super) fn subagent_drained(&mut self, emit: &mut Emit, child: &str) {
        if let Some(ending) = self.subagents.get(child).and_then(|sub| sub.ending.clone()) {
            self.end_subagent(emit, child, ending.end());
        }
    }

    /// The agent process cannot read the subagent's file: its chat says so,
    /// rather than stopping without a word.
    pub(super) fn subagent_unreadable(&mut self, emit: &mut Emit, child: &str, error: &str) {
        if !self.shared.subagent_running(child) {
            return;
        }
        let key = format!("unreadable:{}", self.next_seq());
        self.shared.subagent(emit, child).item(ItemDraft {
            key,
            body: item_body(wire::claude_item::Kind::ApiError(wire::ApiError {
                error_kind: "transcript".into(),
                message: format!("amux can't read this subagent's transcript: {error}"),
                ..Default::default()
            })),
            complete: true,
            ..Default::default()
        });
    }

    /// Ends the subagents whose wait for their last rows is over.
    pub(super) fn endings_due(&mut self, emit: &mut Emit) {
        let now = self.shared.now_ms();
        let due: Vec<(String, wire::IncarnationEnd)> = self
            .subagents
            .iter()
            .filter_map(|(child, sub)| {
                let ending = sub.ending.as_ref()?;
                (ending.by_ms <= now).then(|| (child.clone(), ending.end()))
            })
            .collect();
        for (child, end) in due {
            self.end_subagent(emit, &child, end);
        }
    }

    /// When the soonest wait for a subagent's last rows is over.
    pub(super) fn ending_at(&self) -> Option<i64> {
        self.subagents
            .values()
            .filter_map(|sub| sub.ending.as_ref().map(|ending| ending.by_ms))
            .min()
    }

    /// One row of a subagent's own transcript.
    pub(super) fn subagent_row(&mut self, emit: &mut Emit, payload: &[u8]) {
        let Ok(tagged) = serde_json::from_slice::<Tagged>(payload) else {
            return;
        };
        let child = tagged.agent_id;
        let Ok(row) = transcript::decode(payload) else {
            return;
        };
        match self.subagents.get(&child).map(|sub| sub.phase) {
            // A file nothing asked to follow, a row of an agent Claude never
            // said started, or a late row of one that ended: nothing amux
            // shows.
            None => return,
            Some(Phase::Waiting) => self.start_subagent(emit, &child),
            Some(Phase::Running) => {}
        }
        let at_ms = row
            .timestamp()
            .and_then(timestamp_ms)
            .unwrap_or_else(|| self.shared.now_ms());
        match &row {
            Row::User(user) => {
                // Claude's own notes to the model, but for the message a
                // SendMessage continued the subagent with, which Claude
                // writes as one: that is the prompt its run answers.
                let continued_with = (user.is_meta == Some(true))
                    .then(|| continuation_message(user))
                    .flatten();
                if (user.is_meta == Some(true) && continued_with.is_none())
                    || user.is_compact_summary == Some(true)
                {
                    return;
                }
                let continuing;
                let content = match continued_with {
                    Some(message) => {
                        continuing = MessageContent::Text(message);
                        &continuing
                    }
                    None => &user.message.content,
                };
                if let MessageContent::Blocks(blocks) = content {
                    for block in blocks {
                        if let ContentBlock::ToolResult {
                            tool_use_id,
                            content,
                            is_error,
                            ..
                        } = block
                        {
                            let output = tool_result_text(content.as_ref());
                            self.subagent_call_returned(
                                emit,
                                &child,
                                tool_use_id,
                                is_error.unwrap_or(false),
                                &output,
                                user.tool_use_result.as_ref(),
                            );
                        }
                    }
                }
                let Some(sub) = self.subagents.get_mut(&child) else {
                    return;
                };
                let mut view = self.shared.subagent(emit, &child);
                // Terminal Claude writes each subagent's rows to a file of
                // its own, read apart from this one, so a nested subagent's
                // last words may not be read yet: its report comes out of
                // Claude's frame instead.
                sub.conversation.user(
                    &mut view,
                    &user.envelope.uuid,
                    content,
                    user.tool_use_result.as_ref(),
                    &BTreeMap::new(),
                    &mut self.tasks,
                    at_ms,
                );
            }
            Row::Assistant(assistant) => {
                let message = &assistant.message;
                if assistant.is_api_error_message == Some(true) {
                    self.shared.subagent(emit, &child).item(ItemDraft {
                        key: assistant.envelope.uuid.clone(),
                        body: item_body(wire::claude_item::Kind::ApiError(wire::ApiError {
                            error_kind: assistant.error.clone().unwrap_or_default(),
                            message: crate::claude_common::blocks_text(&message.content),
                            ..Default::default()
                        })),
                        at_ms: Some(at_ms),
                        complete: true,
                        ..Default::default()
                    });
                    return;
                }
                let Some(sub) = self.subagents.get_mut(&child) else {
                    return;
                };
                let mut view = self.shared.subagent(emit, &child);
                sub.conversation.assistant(
                    &mut view,
                    &message.id,
                    &message.model,
                    &message.content,
                    at_ms,
                );
                let calls: Vec<String> = message
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::ToolUse { id, .. } => Some(id.clone()),
                        _ => None,
                    })
                    .collect();
                for call in calls {
                    if let Some(reason) = self.refused.remove(&call) {
                        self.decide_as(emit, Some(&child), &call, Decision::refused(&reason));
                    }
                    self.bind_subagent_asks(emit, &child, &call);
                }
                let replied = self.subagents.get(&child).and_then(|sub| {
                    let ending = sub.ending.as_ref()?;
                    let reply = ending.last_reply.as_deref()?;
                    is_last_reply(&sub.conversation.last_said, reply).then(|| ending.end())
                });
                if let Some(end) = replied {
                    self.end_subagent(emit, &child, end);
                }
            }
            _ => {}
        }
        self.start_waiting(emit);
    }

    /// A call of the subagent `child` returned, before its conversation
    /// draws the result: the asks and forms raised for it are over, and it
    /// may end a nested subagent or start a background shell.
    fn subagent_call_returned(
        &mut self,
        emit: &mut Emit,
        child: &str,
        id: &str,
        is_error: bool,
        output: &str,
        result: Option<&Value>,
    ) {
        for (key, call) in self.dialog_asks() {
            if call.as_deref() == Some(id) {
                self.close_ask_item(emit, &key, crate::ask_item::dismissed());
            }
        }
        let refused = is_error && output.starts_with(crate::claude_answers::REJECTED);
        let outcome = if refused {
            DecisionOutcome::Denied
        } else {
            DecisionOutcome::Allowed
        };
        let asked: Vec<String> = self
            .asks
            .iter()
            .filter(|(_, meta)| {
                meta.agent.as_deref() == Some(child) && meta.call.as_deref() == Some(id)
            })
            .map(|(key, _)| key.clone())
            .collect();
        for key in asked {
            if self.wire_ask(&key).is_some() {
                self.close(emit, &key, Decision::elsewhere(outcome));
            }
        }
        self.call_returned(emit, Some(child), id, is_error, output, result);
    }

    /// The subagent `child`'s call `call` drew its row: the asks it raised
    /// for that call point at it.
    fn bind_subagent_asks(&mut self, emit: &mut Emit, child: &str, call: &str) {
        let drawn = self
            .subagents
            .get(child)
            .and_then(|sub| sub.conversation.tools.get(call))
            .is_some_and(|tool| !tool.hidden);
        if !drawn {
            return;
        }
        let keys: Vec<String> = self
            .asks
            .iter()
            .filter(|(_, meta)| {
                meta.agent.as_deref() == Some(child)
                    && meta.bound.is_none()
                    && meta.call.as_deref() == Some(call)
                    && matches!(&meta.shape, super::AskShape::Answer(shape) if !shape.is_dialog())
            })
            .map(|(key, _)| key.clone())
            .collect();
        for key in keys {
            self.bind(emit, &key, call);
        }
    }

    /// The subagent `child` ended, for `end`'s cause: its asks close
    /// unanswered, a call still running ends cancelled, its last snapshot
    /// lists nothing, and its part ends. Its record goes with it; how the
    /// run ended stays only while a SendMessage that continued it waits for
    /// Claude's result.
    pub(super) fn end_subagent(&mut self, emit: &mut Emit, child: &str, end: wire::IncarnationEnd) {
        let running = self.shared.subagent_running(child);
        let keys: Vec<String> = self
            .asks
            .iter()
            .filter(|(_, meta)| meta.agent.as_deref() == Some(child))
            .map(|(key, _)| key.clone())
            .collect();
        for key in keys {
            if self.wire_ask(&key).is_some() {
                self.close(emit, &key, Decision::unknown());
            }
            self.asks.remove(&key);
        }
        let now = self.shared.now_ms();
        let Some(mut sub) = self.subagents.remove(child) else {
            return;
        };
        if let Some(outcome) = sub.outcome.take()
            && self.continuation_pending(child)
        {
            self.ended_runs.insert(child.to_owned(), outcome);
        }
        if !running {
            return;
        }
        let mut view = self.shared.subagent(emit, child);
        sub.conversation.close(&mut view, now);
        sub.conversation.snapshot(
            &mut view,
            Vec::new(),
            wire::AgentCapabilities::claude_pty().stops_job,
        );
        view.end(end);
        let owned: Vec<String> = self
            .owned_tasks
            .iter()
            .filter(|(_, owner)| owner.as_str() == child)
            .map(|(task, _)| task.clone())
            .collect();
        for task in owned {
            self.owned_tasks.remove(&task);
            self.jobs.ended(&task);
        }
    }

    /// Every running subagent ends with the process that runs them.
    pub(super) fn end_all_subagents(&mut self, emit: &mut Emit, end: &wire::IncarnationEnd) {
        let named: Vec<String> = self.subagents.keys().cloned().collect();
        for child in named {
            self.end_subagent(emit, &child, end.clone());
        }
        self.ended_runs.clear();
        self.owned_tasks.clear();
    }

    /// A new process runs none of the last one's subagents (those it never
    /// ended took the end of its incarnation) and follows no file yet, so
    /// nothing of them is kept. One Claude continues starts again when its
    /// start hook fires.
    pub(super) fn forget_subagents(&mut self) {
        self.subagents.clear();
        self.ended_runs.clear();
        self.owned_tasks.clear();
    }

    /// The agent's own jobs out of Claude's list: no subagent, and no
    /// command a subagent started.
    pub(super) fn host_jobs(&self, jobs: Vec<BackgroundJob>) -> Vec<BackgroundJob> {
        jobs.into_iter()
            .filter(|job| {
                job.kind != wire::BackgroundKind::Subagent as i32
                    && !self.subagents.contains_key(&job.key)
                    && !self.owned_tasks.contains_key(&job.key)
            })
            .collect()
    }

    /// Each running subagent's snapshot, written when it changed: its asks,
    /// what it works on, and the background commands it started.
    pub(super) fn subagent_snapshots(&mut self, emit: &mut Emit) {
        let published = self.jobs.current().unwrap_or_default();
        let running: Vec<String> = self
            .subagents
            .iter()
            .filter(|(child, sub)| {
                sub.phase == Phase::Running && self.shared.subagent_running(child)
            })
            .map(|(child, _)| child.clone())
            .collect();
        for child in running {
            let Some(sub) = self.subagents.get(&child) else {
                continue;
            };
            let jobs = published
                .iter()
                .filter(|job| self.owned_tasks.get(&job.key) == Some(&child))
                .cloned()
                .collect();
            let mut view = self.shared.subagent(emit, &child);
            sub.conversation.snapshot(
                &mut view,
                jobs,
                wire::AgentCapabilities::claude_pty().stops_job,
            );
        }
    }

    /// A call to amux's status tool a subagent is about to make, which its
    /// PreToolUse hook names: what it says it works on is its own.
    pub(super) fn subagent_status(&mut self, child: &str, input: &Value) {
        if let Some(sub) = self.subagents.get_mut(child)
            && let Some(working_on) = crate::status_working_on(input.to_string().as_bytes())
        {
            sub.conversation.working_on = working_on;
        }
    }

    /// The agent a tool server's form is for. Claude names none on it: it
    /// is the one agent's with a call to that server running (announced by
    /// its PreToolUse hook, whose row may not have landed yet); when the
    /// agent's own and a subagent's both have one, or several subagents do,
    /// the agent's own.
    pub(super) fn form_owner(&self, server: &str) -> Option<String> {
        let prefix = format!("mcp__{server}__");
        let mut callers: Vec<Option<String>> = self
            .announced
            .iter()
            .filter(|call| call.name.starts_with(&prefix))
            .map(|call| call.agent.clone())
            .collect();
        callers.extend(
            self.subagents
                .iter()
                .filter(|(_, sub)| {
                    sub.phase == Phase::Running && sub.conversation.calls_server(server)
                })
                .map(|(child, _)| Some(child.clone())),
        );
        callers.sort();
        callers.dedup();
        match callers.as_slice() {
            [Some(child)] if self.shared.subagent_running(child) => Some(child.clone()),
            _ => None,
        }
    }
}
