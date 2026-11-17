//! Headless Claude's subagents as agents of their own.
//!
//! With the two forwarding options the agent process sets in its
//! `initialize` request, Claude streams every subagent's conversation on
//! the agent's own stream, nested ones included: each frame is tagged with
//! the subagent's id (`agent_id`, its task id) and the call that started it
//! (`parent_tool_use_id`). Its start (`task_started`), its end
//! (`task_notification`) and its asks (`can_use_tool` naming its
//! `agent_id`) arrive on the same ordered channel, so a subagent's end
//! always comes after its last frame.
//!
//! - **Start.** A `task_started` of a `local_agent` task starts the
//!   subagent, in the step that rewrites the call that started it with the
//!   subagent's agent id. A nested subagent's call is in its parent
//!   subagent's chat (`parent_task_id`). Claude's model continues a
//!   subagent with `SendMessage`: a second `task_started` for the same
//!   task, naming the SendMessage call, starts it again, as the same agent
//!   (its id comes from Claude's), under the SendMessage call, which opens
//!   its chat too.
//! - **End.** Its `task_notification`: completed is finished; stopped or
//!   killed is stopped; failed ends it as the provider failing it. A
//!   resumed Claude reports a subagent of the previous process as stopped,
//!   which ends nothing: that subagent ended with its process. Claude sends
//!   no notice for a subagent run in the foreground whose call an interrupt
//!   cut: the result of the Agent call that started it ends one still
//!   running. Where a notice comes it comes before that result, so it is
//!   the end, and the result ends nothing more.
//! - **Forgetting.** A subagent's record lasts while it runs and while a
//!   call row still waits on it: an Agent call in the foreground whose
//!   result hands back its last words, or a background row its notice
//!   finishes, as a resumed Claude's notice does for one that ended with
//!   the previous process. Then it goes, so a long-lived agent's checkpoint
//!   does not grow with every subagent it ran. A frame tagged with a
//!   subagent amux holds no record of, under a call that already opens its
//!   chat or that no chat holds, is a late frame of one that ended (a
//!   stopped task flushing the refused result of the call it was in), and
//!   draws nothing; a stop of it, or an answer to it, is refused as one of
//!   any agent not running.
//! - **Asks** a subagent raises are its own, and a person's answer reaches
//!   it as a child input, answered with Claude's control response. A tool
//!   server's form names no agent: it is the one agent's with a call to that
//!   server running, else the agent's own.
//! - **Stop.** Each running subagent's snapshot lists its own job, keyed by
//!   its task id, foreground or background, beside the background commands
//!   it started; a stop sends `stop_task`. The agent's own snapshot lists
//!   no subagent as a job: each has its row.

use std::collections::BTreeMap;

use claude_protocol::stream::control::StopTaskRequest;
use claude_protocol::stream::{
    AssistantMessage, ContentBlock, ControlRequestBody, MessageContent, TaskNotificationMessage,
    TaskStartedMessage, UserMessageOutput,
};
use wire::{BackgroundJob, Input, KindInput, NativeStart, Refusal, ToolState, claude_sdk_input};

use super::{Request, State, Sub, TaskProgress};
use crate::Emit;
use crate::claude_subagent::{
    Conversation, call_state, end_cause, launched_in_background, returned_status,
};

/// Claude's task type for a subagent its Agent tool started.
const AGENT_TASK: &str = "local_agent";

impl State {
    /// A person's input to one of the subagents: an answer to an ask it
    /// raised, or a stop of its own job or of a command it started. Nothing
    /// else reaches a subagent.
    pub(super) fn child_input(&mut self, emit: &mut Emit, agent_id: &[u8], input: Input) {
        let id = input.input_id.clone();
        let Some(child) = self.shared.subagent_child(agent_id).map(str::to_owned) else {
            return self.shared.reject(emit, &id, Refusal::Unsupported);
        };
        match input.decode_body() {
            Some(KindInput::ClaudeSdk(wire::ClaudeSdkInput {
                of: Some(claude_sdk_input::Of::Answer(answer)),
            })) => self.answer(emit, &id, answer, Some(&child)),
            Some(KindInput::ClaudeSdk(wire::ClaudeSdkInput {
                of: Some(claude_sdk_input::Of::StopJob(stop)),
            })) => self.stop_job(emit, &id, &stop.key, Some(&child)),
            _ => self
                .shared
                .subagent(emit, &child)
                .reject(&id, Refusal::Unsupported),
        }
    }

    /// Stops one job by its key, the way Claude stops any task: a running
    /// subagent (its own job) or a background command. It reads stopping
    /// until Claude says it ended. `who` is the subagent whose chat the
    /// stop came from; None for the agent's own.
    pub(super) fn stop_job(&mut self, emit: &mut Emit, id: &[u8], key: &str, who: Option<&str>) {
        let subagent = self
            .subagents
            .get_mut(key)
            .filter(|sub| sub.running && (who.is_none() || who == Some(key)));
        if let Some(sub) = subagent {
            sub.stopping = true;
            sub.stop_error.clear();
        } else {
            let Some(jobs) = self.jobs.stop_sent(key) else {
                return self.reject_as(emit, who, id, Refusal::Unsupported);
            };
            let jobs = self.host_jobs(jobs);
            self.shared.set_jobs(jobs);
        }
        self.control_request(
            emit,
            Request::StopJob(key.to_owned()),
            ControlRequestBody::StopTask(StopTaskRequest {
                task_id: key.to_owned(),
                extensions: Default::default(),
            }),
        );
        self.accept_as(emit, who, id);
    }

    /// Claude refused to stop `key`: the job runs on and says why.
    pub(super) fn stop_refused(&mut self, key: &str, error: String) {
        if let Some(sub) = self.subagents.get_mut(key).filter(|sub| sub.running) {
            sub.stopping = false;
            sub.stop_error = error;
        } else if let Some(jobs) = self.jobs.stop_refused(key, error) {
            let jobs = self.host_jobs(jobs);
            self.shared.set_jobs(jobs);
        }
    }

    /// The agent's own jobs out of Claude's list: no subagent, and no
    /// command a subagent started, which their own snapshots list.
    pub(super) fn host_jobs(&self, jobs: Vec<BackgroundJob>) -> Vec<BackgroundJob> {
        jobs.into_iter()
            .filter(|job| {
                job.kind != wire::BackgroundKind::Subagent as i32
                    && !self.subagents.contains_key(&job.key)
                    && !self.owned_tasks.contains_key(&job.key)
            })
            .collect()
    }

    /// The subagent a frame is from: the one its `agent_id` names, or the
    /// one the call it names started. None for the agent's own frames, and
    /// for a frame of a subagent nothing started, which draws nothing.
    pub(super) fn frame_subagent(&self, agent: Option<&str>, call: Option<&str>) -> Option<String> {
        if let Some(agent) = agent.filter(|agent| !agent.is_empty()) {
            return Some(agent.to_owned());
        }
        let call = call.filter(|call| !call.is_empty())?;
        self.subagents
            .iter()
            .find(|(_, sub)| sub.call == call)
            .map(|(child, _)| child.clone())
    }

    /// A tagged frame of a subagent no `task_started` began: it starts now,
    /// under the call the frame names, in the chat that holds it. A frame of
    /// one that already ended is dropped: a stopped task still flushes the
    /// refused result of the call it was in, and only a new `task_started`
    /// (a continuation) runs it again. Its record may be gone by then: a
    /// call that already opens its chat, or one no chat holds, is the call
    /// of a run that ended.
    fn ensure_running(&mut self, emit: &mut Emit, child: &str, call: Option<&str>) -> bool {
        if let Some(sub) = self.subagents.get(child) {
            return sub.running;
        }
        let Some(call) = call.filter(|call| !call.is_empty()).map(str::to_owned) else {
            return false;
        };
        let Some(parent) = self.holder(&call) else {
            return false;
        };
        if self.call_opens(&call, &self.shared.subagent_id(child)) {
            return false;
        }
        self.start_subagent(emit, child, &call, parent, "", &call);
        true
    }

    /// Whether the open call `call`, in whichever chat holds it, opens the
    /// chat of the subagent whose agent id is `agent_id`.
    fn call_opens(&self, call: &str, agent_id: &[u8]) -> bool {
        if let Some(tool) = self.tools.get(call) {
            return tool.child_agent_id == agent_id;
        }
        self.subagents.values().any(|sub| {
            sub.running
                && sub
                    .conversation
                    .tools
                    .get(call)
                    .is_some_and(|tool| tool.child_agent_id == agent_id)
        })
    }

    /// Whether a call row still waits on the subagent `child`: one of the
    /// agent's own that opens its chat and still runs (an Agent call in the
    /// foreground until its result, one in the background until its
    /// notice), or an open call of a running subagent's that opens it.
    fn awaited(&self, child: &str) -> bool {
        let agent_id = self.shared.subagent_id(child);
        self.tools
            .values()
            .any(|tool| tool.child_agent_id == agent_id && tool.state == ToolState::Running as i32)
            || self.subagents.values().any(|sub| {
                sub.running
                    && sub
                        .conversation
                        .tools
                        .values()
                        .any(|tool| tool.child_agent_id == agent_id)
            })
    }

    /// Lets go of each subagent that ended and that no call row waits on
    /// any more: nothing more of it can arrive that needs its record.
    pub(super) fn forget_ended_subagents(&mut self) {
        let ended: Vec<String> = self
            .subagents
            .iter()
            .filter(|(child, sub)| !sub.running && !self.awaited(child))
            .map(|(child, _)| child.clone())
            .collect();
        for child in ended {
            self.subagents.remove(&child);
        }
    }

    /// Whether the subagent `child` runs, so an ask it raised is its own.
    pub(super) fn subagent_runs(&self, child: &str) -> bool {
        self.subagents.get(child).is_some_and(|sub| sub.running)
    }

    /// One of a subagent's assistant frames.
    pub(super) fn subagent_assistant(
        &mut self,
        emit: &mut Emit,
        child: &str,
        assistant: &AssistantMessage,
    ) {
        if !self.ensure_running(emit, child, assistant.parent_tool_use_id.as_deref()) {
            return;
        }
        let now = self.shared.now_ms();
        let message = &assistant.message;
        let Some(sub) = self.subagents.get_mut(child) else {
            return;
        };
        // Claude streams a run's prompt, when it does, before its first word.
        sub.prompt_drawn = None;
        let mut view = self.shared.subagent(emit, child);
        // An error Claude met on the subagent's request is the subagent's,
        // not the agent's sign-in or turn.
        if let Some(error) = assistant
            .error
            .as_ref()
            .map(|error| error.as_str().to_owned())
            && !error.is_empty()
        {
            view.item(crate::ItemDraft {
                key: assistant.uuid.clone(),
                body: crate::claude_common::item_body(wire::claude_item::Kind::ApiError(
                    wire::ApiError {
                        error_kind: error,
                        message: crate::claude_common::blocks_text(&message.content),
                        ..Default::default()
                    },
                )),
                complete: true,
                ..Default::default()
            });
            return;
        }
        sub.conversation.assistant(
            &mut view,
            &message.id,
            &message.model,
            &message.content,
            now,
        );
    }

    /// One of a subagent's user frames: its prompt, a message continuing
    /// it, or its calls' results.
    pub(super) fn subagent_user(&mut self, emit: &mut Emit, child: &str, user: &UserMessageOutput) {
        if !self.ensure_running(emit, child, user.parent_tool_use_id.as_deref()) {
            return;
        }
        let now = self.shared.now_ms();
        let said: BTreeMap<String, String> = self
            .subagents
            .get(child)
            .map(|sub| sub.conversation.ran_subagents(&user.message.content))
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(call, agent)| Some((call, self.last_said(&agent)?)))
            .collect();
        let Some(sub) = self.subagents.get_mut(child) else {
            return;
        };
        // The prompt its run's start drew already, which Claude streams as
        // well for a subagent it runs in the foreground.
        let echo = sub.prompt_drawn.as_deref().is_some_and(|drawn| {
            only_text(&user.message.content)
                && crate::claude_common::message_text(&user.message.content).trim() == drawn.trim()
        });
        if echo {
            sub.prompt_drawn = None;
            return;
        }
        let key = user.uuid.clone().unwrap_or_default();
        let mut view = self.shared.subagent(emit, child);
        sub.conversation.user(
            &mut view,
            &key,
            &user.message.content,
            user.tool_use_result.as_ref(),
            &said,
            &mut self.tasks,
            now,
        );
        if let MessageContent::Blocks(blocks) = &user.message.content {
            for block in blocks {
                if let ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                    ..
                } = block
                {
                    self.call_returned(
                        emit,
                        tool_use_id,
                        is_error.unwrap_or(false),
                        &crate::claude_common::tool_result_text(content.as_ref()),
                        user.tool_use_result.as_ref(),
                    );
                }
            }
        }
    }

    /// The result of the call `call` returned, in the agent's own chat or a
    /// subagent's. When it is the Agent call that started a subagent in the
    /// foreground and that subagent still runs, no notice ended it, as
    /// happens when an interrupt cuts the call: it ends now, stopped when
    /// the call was cut or refused. A result saying only that a subagent
    /// was launched in the background ends nothing; its notice does.
    pub(super) fn call_returned(
        &mut self,
        emit: &mut Emit,
        call: &str,
        is_error: bool,
        output: &str,
        result: Option<&serde_json::Value>,
    ) {
        if (!is_error && launched_in_background(result, output))
            || self.tools.get(call).is_some_and(|tool| tool.background)
        {
            return;
        }
        let Some((child, said)) = self
            .subagents
            .iter()
            .find(|(_, sub)| sub.running && sub.call == call)
            .map(|(child, sub)| (child.clone(), sub.conversation.last_said.clone()))
        else {
            return;
        };
        let end = end_cause(returned_status(is_error, output), &said);
        self.end_subagent(emit, &child, end);
    }

    /// What the subagent whose agent id is `agent_id` last said. Headless
    /// Claude sends a subagent's frames on the one ordered stream that
    /// carries the result of the call that ran it, all of them first, so at
    /// that result these are its last words.
    pub(super) fn last_said(&self, agent_id: &[u8]) -> Option<String> {
        self.subagents
            .iter()
            .find(|(child, _)| self.shared.subagent_id(child) == agent_id)
            .map(|(_, sub)| sub.conversation.last_said.clone())
            .filter(|said| !said.is_empty())
    }

    /// Whose chat holds the open call `call`: Some(None) for the agent's
    /// own, Some(Some(child)) for a running subagent's; None when no chat
    /// holds it.
    pub(super) fn holder(&self, call: &str) -> Option<Option<String>> {
        if self.tools.contains_key(call) {
            return Some(None);
        }
        self.subagents
            .iter()
            .find(|(_, sub)| sub.running && sub.conversation.holds(call))
            .map(|(child, _)| Some(child.clone()))
    }

    /// A `task_started` for a subagent: it starts, or Claude's model
    /// continued it. False for any other task, which is a job. A continued
    /// subagent amux kept no record of starts afresh; either way its start
    /// names the call that continued it. A record a waiting row kept still
    /// names that row as its first call, which the run's end finishes.
    pub(super) fn task_started(&mut self, emit: &mut Emit, task: &TaskStartedMessage) -> bool {
        if task.task_type.as_deref() != Some(AGENT_TASK)
            && !self.subagents.contains_key(&task.task_id)
        {
            return false;
        }
        let call = task.tool_use_id.clone().unwrap_or_default();
        let parent = task
            .parent_task_id
            .clone()
            .filter(|parent| self.subagents.contains_key(parent));
        let first = self
            .subagents
            .get(&task.task_id)
            .map_or(call.clone(), |sub| sub.call.clone());
        self.start_subagent(
            emit,
            &task.task_id,
            &first,
            parent,
            &task.description,
            &call,
        );
        if let Some(prompt) = task
            .prompt
            .as_deref()
            .filter(|prompt| !prompt.trim().is_empty())
        {
            self.draw_prompt(emit, &task.task_id, &call, prompt);
        }
        if task.is_backgrounded == Some(true) {
            self.backgrounded(emit, &call);
        }
        true
    }

    /// Draws `prompt`, what the call `call` asked of the subagent `child`,
    /// as the prompt its run answers: the first run's and each
    /// continuation's, which Claude streams as a frame of the subagent's
    /// own only for some runs (one in the foreground), and never for a
    /// continuation.
    fn draw_prompt(&mut self, emit: &mut Emit, child: &str, call: &str, prompt: &str) {
        let now = self.shared.now_ms();
        let Some(sub) = self.subagents.get_mut(child).filter(|sub| sub.running) else {
            return;
        };
        sub.prompt_drawn = Some(prompt.to_owned());
        let mut view = self.shared.subagent(emit, child);
        sub.conversation.user(
            &mut view,
            &format!("prompt:{call}"),
            &MessageContent::Text(prompt.to_owned()),
            None,
            &BTreeMap::new(),
            &mut self.tasks,
            now,
        );
    }

    /// Starts the subagent `child`, or starts it again, under its first
    /// call `call` in `parent`'s chat (the agent's own for None), and
    /// rewrites the call `linking` that started or continued it to open its
    /// chat, in the same step. Its start names `linking`, in the chat that
    /// holds it: the call that started this run.
    fn start_subagent(
        &mut self,
        emit: &mut Emit,
        child: &str,
        call: &str,
        parent: Option<String>,
        title: &str,
        linking: &str,
    ) {
        let now = self.shared.now_ms();
        let linked_in = self.holder(linking);
        let sub = self
            .subagents
            .entry(child.to_owned())
            .or_insert_with(|| Sub {
                call: call.to_owned(),
                parent: parent.clone(),
                title: title.to_owned(),
                started_at_ms: now,
                running: false,
                stopping: false,
                stop_error: String::new(),
                prompt_drawn: None,
                conversation: Conversation::default(),
            });
        if !title.is_empty() && sub.title.is_empty() {
            title.clone_into(&mut sub.title);
        }
        let agent_id = self.shared.subagent_id(child);
        if !sub.running {
            sub.running = true;
            sub.started_at_ms = now;
            sub.stopping = false;
            sub.stop_error.clear();
            let (call_key, in_chat) = match linked_in {
                Some(holder) if !linking.is_empty() => (linking.to_owned(), holder),
                _ => (sub.call.clone(), sub.parent.clone()),
            };
            let call_agent_id = match &in_chat {
                Some(parent) => self.shared.subagent_id(parent),
                None => self.shared.agent_id().to_vec(),
            };
            let start = NativeStart {
                call_agent_id,
                call_key,
                title: sub.title.clone(),
                ..Default::default()
            };
            self.shared.subagent(emit, child).start(start);
        }
        self.link(emit, linking, &agent_id);
    }

    /// The call `call` opens the subagent `agent_id`'s chat, in whichever
    /// chat holds it.
    fn link(&mut self, emit: &mut Emit, call: &str, agent_id: &[u8]) {
        if call.is_empty() {
            return;
        }
        if let Some(tool) = self.tools.get_mut(call) {
            if tool.child_agent_id != agent_id {
                tool.child_agent_id = agent_id.to_vec();
                self.emit_tool(emit, call);
            }
            return;
        }
        let holders: Vec<String> = self
            .subagents
            .iter()
            .filter(|(_, sub)| sub.running && sub.conversation.holds(call))
            .map(|(child, _)| child.clone())
            .collect();
        for holder in holders {
            if let Some(sub) = self.subagents.get_mut(&holder) {
                let mut view = self.shared.subagent(emit, &holder);
                sub.conversation.link(&mut view, call, agent_id);
            }
        }
    }

    /// A subagent's call of the agent's own runs in the background: its
    /// result only says it launched, and the row stays running until the
    /// subagent's end.
    pub(super) fn backgrounded(&mut self, emit: &mut Emit, call: &str) {
        let Some(tool) = self.tools.get_mut(call) else {
            return;
        };
        tool.background = true;
        tool.of.task = Some(TaskProgress { finished: false });
        tool.state = ToolState::Running as i32;
        tool.ended_at_ms = None;
        self.emit_tool(emit, call);
    }

    /// A `task_notification` for a subagent: it ended. False for any other
    /// task.
    pub(super) fn task_notified(
        &mut self,
        emit: &mut Emit,
        task: &TaskNotificationMessage,
    ) -> bool {
        let Some(sub) = self.subagents.get(&task.task_id) else {
            return false;
        };
        let now = self.shared.now_ms();
        let (first, running, nested) = (sub.call.clone(), sub.running, sub.parent.is_some());
        let state = call_state(&task.status);
        // The calls waiting on it end with it: the one that started it and
        // the one that last continued it.
        let mut calls = vec![first];
        if let Some(call) = task
            .tool_use_id
            .clone()
            .filter(|call| !calls.contains(call))
        {
            calls.push(call);
        }
        for call in calls {
            match self.holder(&call) {
                Some(None) => {
                    let Some(tool) = self
                        .tools
                        .get_mut(&call)
                        .filter(|tool| tool.of.task.as_ref().is_some_and(|task| !task.finished))
                    else {
                        continue;
                    };
                    tool.of.task = Some(TaskProgress { finished: true });
                    if tool.background {
                        tool.state = state as i32;
                        tool.ended_at_ms.get_or_insert(now);
                        if !task.summary.is_empty() {
                            tool.outcome_text.clone_from(&task.summary);
                        }
                    }
                    self.emit_tool(emit, &call);
                }
                Some(Some(holder)) => {
                    if let Some(holding) = self.subagents.get_mut(&holder) {
                        let mut view = self.shared.subagent(emit, &holder);
                        holding.conversation.finish_launched(
                            &mut view,
                            &call,
                            state,
                            Some(&task.summary),
                            now,
                        );
                    }
                }
                None => {}
            }
        }
        if let Some(jobs) = self.jobs.ended(&task.task_id) {
            let jobs = self.host_jobs(jobs);
            self.shared.set_jobs(jobs);
        }
        self.active_tasks.remove(&task.task_id);
        // A subagent of an earlier process ended with that process; Claude
        // resumed reports it only so the model knows.
        if running {
            let end = end_cause(&task.status, &task.summary);
            self.end_subagent(emit, &task.task_id, end);
            // Claude answers the notice with a turn of its own, there at
            // once when the agent is idle: the agent works from this step,
            // so its end and its subagent's are never read apart, and a
            // host that waits for both does not leave before that turn. A
            // nested subagent's notice goes to the subagent that started it.
            if !nested {
                self.shared.turn_started();
            }
        }
        true
    }

    /// A task a subagent's call started (a shell it runs): a job of that
    /// subagent's while it runs in the background, and nothing of the
    /// agent's own.
    pub(super) fn owned_task_started(&mut self, task: &TaskStartedMessage) -> bool {
        let Some(owner) = task
            .parent_task_id
            .clone()
            .filter(|parent| self.subagents.contains_key(parent))
        else {
            return false;
        };
        self.owned_tasks.insert(task.task_id.clone(), owner);
        if task.is_backgrounded == Some(true) {
            self.owned_backgrounded(
                &task.task_id,
                task.tool_use_id.as_deref(),
                &task.description,
            );
        }
        true
    }

    /// A subagent's shell went to the background: it is a job of that
    /// subagent's.
    pub(super) fn owned_backgrounded(
        &mut self,
        task_id: &str,
        call: Option<&str>,
        description: &str,
    ) {
        let Some(owner) = self.owned_tasks.get(task_id) else {
            return;
        };
        let call = call.unwrap_or_default();
        let (command, at_ms) = self
            .subagents
            .get(owner)
            .and_then(|sub| sub.conversation.tools.get(call))
            .map_or((description.to_owned(), self.shared.now_ms()), |tool| {
                (
                    crate::claude_common::JobInput::command(&tool.input),
                    tool.at_ms,
                )
            });
        self.jobs
            .launched(task_id, call, command, at_ms, wire::BackgroundKind::Shell);
    }

    /// A subagent's shell task ended: it leaves that subagent's jobs, and
    /// its call, if it waited, ends with it.
    pub(super) fn owned_task_notified(
        &mut self,
        emit: &mut Emit,
        task: &TaskNotificationMessage,
    ) -> bool {
        let Some(owner) = self.owned_tasks.remove(&task.task_id) else {
            return false;
        };
        self.jobs.ended(&task.task_id);
        let now = self.shared.now_ms();
        if let (Some(call), Some(sub)) =
            (task.tool_use_id.as_deref(), self.subagents.get_mut(&owner))
            && sub.running
        {
            let mut view = self.shared.subagent(emit, &owner);
            sub.conversation.finish_launched(
                &mut view,
                call,
                call_state(&task.status),
                Some(&task.summary),
                now,
            );
        }
        true
    }

    /// The subagent `child` ended, for `end`'s cause: its asks close
    /// unanswered, a call still running ends cancelled, its last snapshot
    /// lists nothing, and its part ends.
    pub(super) fn end_subagent(&mut self, emit: &mut Emit, child: &str, end: wire::IncarnationEnd) {
        if !self.shared.subagent_running(child) {
            if let Some(sub) = self.subagents.get_mut(child) {
                sub.running = false;
            }
            return;
        }
        self.dismiss_subagent_asks(emit, child);
        let now = self.shared.now_ms();
        let Some(sub) = self.subagents.get_mut(child) else {
            return;
        };
        sub.running = false;
        sub.stopping = false;
        sub.stop_error.clear();
        let mut view = self.shared.subagent(emit, child);
        sub.conversation.close(&mut view, now);
        sub.conversation.snapshot(&mut view, Vec::new());
        view.end(end);
        // Its commands went with it. Each stays known as the subagent's
        // until its own notice comes: a stopped subagent's notice comes
        // before its command's, which would otherwise read as a task of
        // the agent's own and draw in its chat.
        let owned: Vec<String> = self
            .owned_tasks
            .iter()
            .filter(|(_, owner)| owner.as_str() == child)
            .map(|(task, _)| task.clone())
            .collect();
        for task in owned {
            self.jobs.ended(&task);
        }
    }

    /// Every running subagent ends with the process that runs them.
    pub(super) fn end_all_subagents(&mut self, emit: &mut Emit, end: &wire::IncarnationEnd) {
        let running: Vec<String> = self
            .subagents
            .iter()
            .filter(|(_, sub)| sub.running)
            .map(|(child, _)| child.clone())
            .collect();
        for child in running {
            self.end_subagent(emit, &child, end.clone());
        }
        self.owned_tasks.clear();
    }

    /// A new process runs none of the last one's subagents: those it never
    /// ended took the end of its incarnation. One a SendMessage continues
    /// starts again. A record a waiting row keeps stays for the resumed
    /// Claude's notice, which finishes that row.
    pub(super) fn forget_subagents(&mut self) {
        for sub in self.subagents.values_mut() {
            sub.running = false;
            sub.stopping = false;
        }
        self.owned_tasks.clear();
    }

    /// Each running subagent's snapshot, written when it changed: its asks,
    /// what it works on, and its jobs, its own first.
    pub(super) fn subagent_snapshots(&mut self, emit: &mut Emit) {
        let published = self.jobs.current().unwrap_or_default();
        let running: Vec<String> = self
            .subagents
            .iter()
            .filter(|(child, sub)| sub.running && self.shared.subagent_running(child))
            .map(|(child, _)| child.clone())
            .collect();
        for child in running {
            let Some(sub) = self.subagents.get(&child) else {
                continue;
            };
            let mut jobs = vec![BackgroundJob {
                item_key: String::new(),
                command: sub.title.clone(),
                started_at_ms: sub.started_at_ms,
                kind: wire::BackgroundKind::Subagent as i32,
                key: child.clone(),
                stopping: sub.stopping,
                stop_error: sub.stop_error.clone(),
            }];
            jobs.extend(
                published
                    .iter()
                    .filter(|job| self.owned_tasks.get(&job.key) == Some(&child))
                    .cloned(),
            );
            let mut view = self.shared.subagent(emit, &child);
            sub.conversation.snapshot(&mut view, jobs);
        }
    }

    /// The agent a tool server's form is for. Claude names none on it: it
    /// is the one agent's with a call to that server running; when the
    /// agent's own and a subagent's both have one, or several subagents
    /// do, the agent's own.
    pub(super) fn form_owner(&self, server: &str) -> Option<String> {
        let own = self.tools.values().any(|tool| {
            tool.server == server && tool.state == ToolState::Running as i32 && !tool.background
        });
        let mut calling = self
            .subagents
            .iter()
            .filter(|(child, sub)| {
                sub.running
                    && self.shared.subagent_running(child)
                    && sub.conversation.calls_server(server)
            })
            .map(|(child, _)| child.clone());
        match (own, calling.next(), calling.next()) {
            (false, Some(child), None) => Some(child),
            _ => None,
        }
    }
}

/// Whether a user frame is only text: a prompt, not a call's results.
fn only_text(content: &MessageContent) -> bool {
    match content {
        MessageContent::Text(_) => true,
        MessageContent::Blocks(blocks) => blocks
            .iter()
            .all(|block| matches!(block, ContentBlock::Text { .. })),
    }
}
