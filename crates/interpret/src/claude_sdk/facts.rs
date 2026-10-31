//! Stream-JSON lines from headless Claude, read into state and items.

use std::collections::{BTreeMap, BTreeSet};

use claude_protocol::stream::control::{CanUseToolRequest, ControlOutcome};
use claude_protocol::stream::init::SlashCommand;
use claude_protocol::stream::{
    self, AssistantMessage, CompactTrigger, ContentBlock, ControlRequest, ControlRequestBody,
    ControlResponse, InitializationResult, Message, MessageContent, ModelUsage, Output,
    RateLimitInfo, ResultCommon, ResultError, ResultMessage, SettingsResult, StreamDelta,
    StreamEvent, SystemInitMessage, TaskUsage, Usage, UserMessageOutput,
};
use prost::Message as _;
use serde::Deserialize;
use serde_json::Value;
use wire::claude_item::Kind;
use wire::{
    BoundaryKind, ClaudeUsage, ClaudeUsageWindow, DecisionOutcome, FormAsk, HealthState, Limit,
    LinkAsk, PermissionAsk, SignIn, SignInState, TaskState as WireTaskState, ToolServer,
    ToolServerHealth, ToolServerStatus, ToolState, Turn, TurnOutcome, UsageMeter, UsageState,
};

use super::{AskMeta, Call, Request, State, TaskState, Tool};
use crate::claude_answers::{AnswerShape, Decided, ToolDecisionState};
use crate::claude_common::{
    AnsweredResult, JobInput, PLAN_TOOL, QUESTION_TOOL, auto_models, blocks_text, claude_limit,
    item_body, job_kind, message_text, offered_commands, offered_models, old_source,
    permission_scopes, plan_ask, question_ask, question_ask_text, split_tool_name, tool_input,
    tool_result_text, without_terminal_codes,
};
use crate::shared::json_as_written;
use crate::{Channel, Effect, Emit, Fact, ItemDraft, ask_item, is_status_tool, status_working_on};

const INTERRUPTED: &str = "[Request interrupted by user";

/// Text Claude wrote, when it wrote any.
fn written(text: &str) -> Option<String> {
    (!text.is_empty()).then(|| text.to_owned())
}

fn written_opt(text: Option<&str>) -> Option<String> {
    text.and_then(written)
}

/// The error Claude reports when the API rejects its credential.
const AUTHENTICATION_FAILED: &str = "authentication_failed";

/// What one of Claude's task frames says about a task: its start, an update,
/// progress, or the notification that it ended.
#[derive(Default)]
struct TaskReport<'a> {
    task_id: &'a str,
    description: Option<&'a str>,
    tool_use_id: Option<&'a str>,
    last_tool_name: Option<&'a str>,
    usage: Option<&'a TaskUsage>,
    status: Option<&'a str>,
    summary: Option<&'a str>,
    task_type: Option<&'a str>,
    backgrounded: bool,
}

impl State {
    /// Whether a path is in the folder Claude was launched in. An agent
    /// whose folder is not known has none to be in.
    fn in_folder(&self, path: &str) -> bool {
        !self.cwd.is_empty() && std::path::Path::new(path).starts_with(&self.cwd)
    }

    pub(super) fn fact(&mut self, emit: &mut Emit, fact: Fact) {
        if fact.channel == Channel::Agent {
            if let Ok(crate::claude_pty::AgentFact::ToolsWithheld) =
                serde_json::from_slice(&fact.payload)
            {
                self.tools_withheld = true;
            }
            return;
        }
        if fact.channel != Channel::Stream {
            return;
        }
        let Ok(output) = stream::decode(&fact.payload) else {
            return;
        };
        match output {
            Output::Message(message) => self.message(emit, message),
            Output::ControlRequest(request) => {
                self.control_request_in(emit, request, &fact.payload)
            }
            Output::ControlCancelRequest(cancel) => {
                self.request_cancelled(emit, &cancel.request_id)
            }
            Output::ControlResponse(response) => self.control_response_in(emit, response),
            Output::Unknown(_) => {}
        }
    }

    fn message(&mut self, emit: &mut Emit, message: Message) {
        match message {
            Message::System(init) => self.init(emit, &init),
            Message::StreamEvent(event) => self.stream_event(
                emit,
                &event.event,
                event.parent_tool_use_id.as_deref().unwrap_or_default(),
            ),
            Message::Assistant(assistant) => self.assistant(emit, &assistant),
            Message::User(user) => self.user(emit, &user),
            Message::UserReplay(replay) => {
                let whole = message_text(&replay.message.content);
                if let Some(output) = local_output(&whole) {
                    return self.slash_output(emit, replay.uuid, &output);
                }
                if let Some(command) = command_name(&whole)
                    && let Some((key, text)) = self.synthetic.take()
                {
                    self.slash_item(emit, key, text, &command);
                }
                self.steer_replayed(emit, &replay.uuid);
                self.taken(&replay.uuid);
            }
            Message::Result(result) => self.result(emit, &result),
            Message::CommandLifecycle(lifecycle) => {
                if lifecycle.state == "started" {
                    self.taken(&lifecycle.command_uuid);
                }
            }
            Message::RateLimit(event) => self.rate_limit(&event.rate_limit_info),
            Message::CommandsChanged(changed) => self.commands_changed(emit, &changed.commands),
            Message::AuthStatus(status) => {
                self.sign_in = Some(match written_opt(status.error.as_deref()) {
                    Some(error) => SignIn {
                        state: SignInState::Failed as i32,
                        account: String::new(),
                        message: error,
                    },
                    None if status.is_authenticating => SignIn {
                        state: SignInState::SignedOut as i32,
                        ..Default::default()
                    },
                    None => SignIn {
                        state: SignInState::SignedIn as i32,
                        ..Default::default()
                    },
                })
            }
            // Every reset, whatever started it, is a new session the next
            // init names.
            Message::ConversationReset(_) => {
                self.session = None;
                self.context_tokens = None;
                self.cleared = Some(self.boundary(
                    emit,
                    BoundaryKind::Cleared,
                    wire::IncarnationEnd::default(),
                ));
            }
            Message::Status(status) => {
                if let Some(mode) = status
                    .permission_mode
                    .as_ref()
                    .and_then(|mode| written(mode.as_str()))
                {
                    self.permission_mode = Some(mode);
                }
                // The activity line reads "Compacting" from this item until
                // the compaction row lands; a status of null only ends one.
                if let Some(word) = status.status.as_deref().map(without_terminal_codes) {
                    let typed = match word.as_str() {
                        "compacting" => wire::SdkStatus::Compacting,
                        _ => wire::SdkStatus::Unspecified,
                    };
                    self.shared.item(
                        emit,
                        ItemDraft {
                            key: status.uuid,
                            body: item_body(Kind::Status(wire::Status {
                                status: typed as i32,
                                word,
                            })),
                            complete: true,
                            ..Default::default()
                        },
                    );
                }
            }
            Message::CompactBoundary(compact) => {
                let metadata = &compact.compact_metadata;
                let after = metadata.post_tokens;
                if after.is_some() {
                    self.context_tokens = after;
                }
                self.shared.item(
                    emit,
                    ItemDraft {
                        key: compact.uuid,
                        body: item_body(Kind::Compaction(wire::Compaction {
                            tokens_before: Some(metadata.pre_tokens),
                            tokens_after: after,
                            automatic: metadata.trigger == CompactTrigger::Auto,
                        })),
                        complete: true,
                        ..Default::default()
                    },
                );
                self.boundary(
                    emit,
                    BoundaryKind::Compacted,
                    wire::IncarnationEnd::default(),
                );
            }
            Message::ApiRetry(retry) => {
                let error = retry.error.as_str();
                if error == AUTHENTICATION_FAILED {
                    self.sign_in_failed(error);
                }
                let status = retry
                    .error_status
                    .map(|status| format!(" ({status})"))
                    .unwrap_or_default();
                let retry_at_ms = Some(self.shared.now_ms() + retry.retry_delay_ms as i64);
                self.shared.item(
                    emit,
                    ItemDraft {
                        key: retry.uuid.clone(),
                        body: item_body(Kind::ApiError(wire::ApiError {
                            error_kind: error.to_owned(),
                            message: format!("{error}{status}"),
                            will_retry: true,
                            attempt: retry.attempt,
                            max_attempts: retry.max_retries,
                            retry_at_ms,
                        })),
                        complete: true,
                        ..Default::default()
                    },
                );
            }
            Message::ModelRefusalFallback(fallback) => {
                self.model = Some(fallback.fallback_model.clone());
                self.chosen_model = None;
                let reason = without_terminal_codes(
                    &written_opt(fallback.api_refusal_explanation.as_deref())
                        .or_else(|| written_opt(fallback.api_refusal_category.as_deref()))
                        .unwrap_or(fallback.content),
                );
                self.shared.item(
                    emit,
                    ItemDraft {
                        key: fallback.uuid,
                        body: item_body(Kind::ModelSwitch(wire::ModelSwitch {
                            from: fallback.original_model,
                            to: fallback.fallback_model,
                            reason,
                        })),
                        complete: true,
                        ..Default::default()
                    },
                );
            }
            Message::ModelRefusalNoFallback(refusal) => self.shared.item(
                emit,
                ItemDraft {
                    key: refusal.uuid,
                    body: item_body(Kind::ApiError(wire::ApiError {
                        error_kind: "refusal".into(),
                        message: without_terminal_codes(
                            &written_opt(refusal.api_refusal_explanation.as_deref())
                                .unwrap_or(refusal.content),
                        ),
                        ..Default::default()
                    })),
                    complete: true,
                    ..Default::default()
                },
            ),
            Message::LocalCommandOutput(output) => {
                self.slash_output(emit, output.uuid, &without_terminal_codes(&output.content))
            }
            // Claude's own ambient work (memory consolidation and the
            // like) is nothing the person started: no row, no job.
            Message::TaskStarted(task) if task.ambient == Some(true) => {
                self.ambient.insert(task.task_id);
            }
            Message::TaskStarted(task) => self.task_event(
                emit,
                TaskReport {
                    task_id: &task.task_id,
                    description: Some(&task.description),
                    tool_use_id: task.tool_use_id.as_deref(),
                    task_type: task.task_type.as_deref(),
                    backgrounded: task.is_backgrounded == Some(true),
                    ..Default::default()
                },
            ),
            // A task sent to the background after it started says so only
            // here, as a patch.
            Message::TaskUpdated(task) => self.task_event(
                emit,
                TaskReport {
                    task_id: &task.task_id,
                    description: task.patch.description.as_deref(),
                    status: task.patch.status.as_deref(),
                    backgrounded: task.patch.is_backgrounded == Some(true),
                    ..Default::default()
                },
            ),
            Message::TaskProgress(task) => self.task_event(
                emit,
                TaskReport {
                    task_id: &task.task_id,
                    description: Some(&task.description),
                    tool_use_id: task.tool_use_id.as_deref(),
                    last_tool_name: task.last_tool_name.as_deref(),
                    usage: Some(&task.usage),
                    summary: task.summary.as_deref(),
                    ..Default::default()
                },
            ),
            Message::TaskNotification(task) => self.task_event(
                emit,
                TaskReport {
                    task_id: &task.task_id,
                    tool_use_id: task.tool_use_id.as_deref(),
                    usage: task.usage.as_ref(),
                    status: Some(&task.status),
                    summary: Some(&task.summary),
                    ..Default::default()
                },
            ),
            Message::BackgroundTasksChanged(changed) => {
                if let Some(tasks) = changed.tasks {
                    let now = self.shared.now_ms();
                    let ambient = &mut self.ambient;
                    let jobs = self.jobs.listed(
                        tasks.into_iter().filter_map(|task| {
                            if task.ambient == Some(true) {
                                ambient.insert(task.task_id);
                                return None;
                            }
                            let kind = job_kind(&task.task_type);
                            Some((task.task_id, task.description, kind))
                        }),
                        now,
                    );
                    self.shared.set_jobs(jobs);
                }
            }
            Message::PermissionDenied(denied) => {
                let note = without_terminal_codes(
                    &written_opt(denied.decision_reason.as_deref()).unwrap_or(denied.message),
                );
                self.decide(
                    emit,
                    &denied.tool_use_id,
                    Decided::Tool(ToolDecisionState {
                        outcome: DecisionOutcome::Denied as i32,
                        grant: None,
                        note,
                    }),
                );
            }
            Message::ElicitationComplete(complete) => {
                let elicitation = complete.elicitation_id;
                for key in self.open_ask_keys() {
                    let matches = self.shared.asks().get(&key).is_some_and(|ask| {
                        matches!(&ask.body, Some(wire::ask::Body::Link(link)) if link.url.contains(&elicitation))
                    }) || key == elicitation;
                    if matches && let Some(ask) = self.shared.close_ask(&key) {
                        self.asks.remove(&key);
                        self.emit_ask(
                            emit,
                            &ask,
                            Some(ask_item::outcome(wire::AskOutcome::Answered)),
                        );
                    }
                }
            }
            // Tool progress, summaries, suggestions and the rest are
            // Claude's own interface; they stay in the facts ring.
            _ => {}
        }
    }

    pub(super) fn exited(&mut self, emit: &mut Emit, exit: wire::IncarnationEnd) {
        emit.end_incarnation(exit.clone());
        self.dismiss_asks(emit);
        self.boundary(emit, BoundaryKind::Exited, exit);
        self.shared.provider_exited();
        self.jobs.clear();
        self.exited = true;
        self.unanswered.clear();
        self.early_boundary = None;
        self.stream = None;
    }

    fn open_ask_keys(&self) -> Vec<String> {
        self.shared
            .asks()
            .open_asks()
            .iter()
            .map(|ask| ask.key.clone())
            .collect()
    }

    /// Claude took the message with this uuid: an agent message is
    /// consumed.
    fn taken(&mut self, uuid: &str) {
        if let Some(client) = self.clients.get(uuid)
            && client.message
        {
            let id = client.id.clone();
            self.clients.remove(uuid);
            self.shared.message_consumed(&id);
            self.shared.turn_started();
        }
    }

    // --- system ----------------------------------------------------------

    fn init(&mut self, emit: &mut Emit, init: &SystemInitMessage) {
        self.shared.provider_started();
        let session = written(&init.session_id);
        let previous = self.session.clone();
        self.session = session.clone().or(previous.clone());
        if let Some(model) = written(&init.model) {
            self.model = Some(model);
        }
        if let Some(mode) = written(init.permission_mode.as_str()) {
            self.permission_mode = Some(mode);
        }
        if let Some(effort) = &init.effort {
            self.effort = effort.clone();
        }
        if let Some(servers) = &init.mcp_servers {
            self.servers =
                Some(server_health(servers.iter().map(|server| {
                    (server.name.as_str(), server.status.as_str(), "")
                })));
        }
        let changed = session.is_some() && session != previous;
        // A reset's boundary names the session it began once one does.
        let cleared = self.cleared.take();
        // The session the process launched under is saved already; one it
        // moved to (a reset's) is the one the next incarnation resumes.
        if changed
            && (previous.is_some() || cleared.is_some())
            && let Some(id) = &session
        {
            emit.effect(Effect::ProviderSession { id: id.clone() });
        }
        if let Some(key) = &cleared {
            self.boundary_at(
                emit,
                key.clone(),
                BoundaryKind::Cleared,
                wire::IncarnationEnd::default(),
            );
        }
        // Claude repeats its init at every turn; only a new process or a
        // new session it did not reset to is a boundary.
        let kind = self
            .coming_boundary()
            .or_else(|| (changed && cleared.is_none()).then_some(BoundaryKind::Forked));
        self.exited = false;
        self.inits += 1;
        let Some(kind) = kind else {
            return;
        };
        // A prompt that went in first already drew this boundary above it.
        match self.early_boundary.take() {
            Some(key) if kind != BoundaryKind::Forked => {
                self.boundary_at(emit, key, kind, wire::IncarnationEnd::default())
            }
            _ => {
                self.boundary(emit, kind, wire::IncarnationEnd::default());
            }
        }
    }

    /// Claude reports one window's status at a time, with every window's
    /// use: the named window takes the status, and every other keeps the
    /// last status stated for it until it resets. A window whose reported
    /// reset is later than the one held has reset, so the status stated for
    /// it is dropped (back to unknown) before the report is applied: a
    /// window stated near or reached reads so only until it resets. The
    /// overall state is the report's own.
    fn rate_limit(&mut self, info: &RateLimitInfo) {
        let state = match info.status.as_str() {
            "allowed" => UsageState::Ok,
            "allowed_warning" => UsageState::NearLimit,
            "rejected" => UsageState::Blocked,
            _ => UsageState::Unknown,
        };
        let usage = self.usage.get_or_insert_with(crate::unknown::claude_usage);
        usage.state = state as i32;
        for (name, window) in info.unified_windows.iter().flatten() {
            report_window(
                claude_window(usage, name),
                window.utilization,
                window.resets_at.map(|at| at * 1000),
            );
        }
        if let Some(name) = info
            .rate_limit_type
            .as_deref()
            .filter(|name| !name.is_empty())
        {
            let listed = info
                .unified_windows
                .as_ref()
                .is_some_and(|windows| windows.contains_key(name));
            let meter = claude_window(usage, name);
            if !listed {
                report_window(
                    meter,
                    info.utilization,
                    info.resets_at.map(|at| at as i64 * 1000),
                );
            }
            meter.state = state as i32;
        }
    }

    fn task_event(&mut self, emit: &mut Emit, report: TaskReport) {
        let id = report.task_id.to_owned();
        if id.is_empty() || self.ambient.contains(&id) {
            return;
        }
        let now = self.shared.now_ms();
        let task = self.active_tasks.entry(id.clone()).or_insert(TaskState {
            at_ms: now,
            description: String::new(),
            state: WireTaskState::Running as i32,
            tool_count: 0,
            last_tool: String::new(),
            tool_key: String::new(),
            tokens: 0,
            shell: false,
        });
        if report.task_type == Some("local_bash") {
            task.shell = true;
        }
        if let Some(description) = written_opt(report.description) {
            task.description = description;
        }
        if let Some(tool) = written_opt(report.tool_use_id) {
            task.tool_key = tool;
        }
        if let Some(last) = written_opt(report.last_tool_name) {
            task.last_tool = last;
        }
        if let Some(usage) = report.usage {
            task.tool_count = usage.tool_uses;
            task.tokens = usage.total_tokens;
        }
        if let Some(status) = report.status {
            task.state = match status {
                "completed" => WireTaskState::Completed,
                "failed" => WireTaskState::Failed,
                "killed" | "stopped" | "cancelled" => WireTaskState::Stopped,
                _ => WireTaskState::Running,
            } as i32;
        }
        let wire = task.to_wire(&id);
        let at_ms = task.at_ms;
        let shell = task.shell;
        let summary = written_opt(report.summary);
        let key = task.tool_key.clone();
        // A subagent that ended asks nothing more, and a job that ended
        // leaves the list, whether or not its call was shown.
        if wire.state != WireTaskState::Running as i32 {
            self.task_asks_over(emit, &id, &key);
            if let Some(jobs) = self.jobs.ended(&id) {
                self.shared.set_jobs(jobs);
            }
        }
        // A task of a call this interpreter showed is progress on that call,
        // which stays open until the task ends: its own result only said the
        // task was launched. The notification carries the task's answer.
        if let Some(tool) = self.tools.get_mut(&key) {
            let finished = wire.state != WireTaskState::Running as i32;
            let subagent = tool
                .of
                .task
                .as_ref()
                .map_or(report.task_type == Some("local_agent"), |task| {
                    task.subagent
                });
            tool.of.task = Some(super::TaskProgress {
                subagent,
                tool_count: wire.tool_count,
                last_tool: wire.last_tool.clone(),
                finished,
            });
            if report.backgrounded && !finished {
                tool.background = true;
                let kind = if shell {
                    wire::BackgroundKind::Shell
                } else if subagent {
                    wire::BackgroundKind::Subagent
                } else {
                    wire::BackgroundKind::Other
                };
                let (command, at_ms) = (JobInput::command(&tool.input), tool.at_ms);
                self.jobs.launched(&id, &key, command, at_ms, kind);
                if let Some(jobs) = self.jobs.current() {
                    self.shared.set_jobs(jobs);
                }
            }
            if !finished {
                tool.state = ToolState::Running as i32;
                tool.ended_at_ms = None;
            } else if tool.background {
                tool.state = match WireTaskState::try_from(wire.state) {
                    Ok(WireTaskState::Completed) => ToolState::Succeeded,
                    Ok(WireTaskState::Failed) => ToolState::Failed,
                    _ => ToolState::Cancelled,
                } as i32;
                tool.ended_at_ms.get_or_insert(now);
                if let Some(summary) = summary {
                    tool.outcome_text = summary;
                }
            }
            return self.emit_tool(emit, &key);
        }
        self.shared.item(
            emit,
            ItemDraft {
                key: format!("task:{id}"),
                text: summary.unwrap_or_default(),
                body: item_body(Kind::Task(wire)),
                at_ms: Some(at_ms),
                complete: true,
                ..Default::default()
            },
        );
        // A finished task's summary stays on its item; only running ones
        // are drawn in the strip.
    }

    // --- streams ---------------------------------------------------------

    fn stream_event(&mut self, emit: &mut Emit, event: &StreamEvent, parent: &str) {
        // A subagent's stream is its own; its whole assistant frame carries
        // the calls it makes.
        if !parent.is_empty() {
            return;
        }
        match event {
            StreamEvent::MessageStart { message, .. } => {
                self.stream = written(&message.id);
                self.stream_tools.clear();
                self.usage_seen(&message.usage);
                self.shared.turn_started();
            }
            StreamEvent::ContentBlockStart {
                index,
                content_block,
                ..
            } => {
                let Some(message) = self.stream.clone() else {
                    return;
                };
                let key = format!("{message}:{index}");
                match content_block {
                    ContentBlock::Text { text, .. } => self.open_block(emit, key, text, false),
                    ContentBlock::Thinking { thinking, .. } => {
                        self.open_block(emit, key, thinking, true)
                    }
                    ContentBlock::ToolUse { id, name, .. } => {
                        self.stream_tools.insert(*index, id.clone());
                        self.tool_seen(emit, id, name, None, "");
                    }
                    _ => {}
                }
            }
            StreamEvent::ContentBlockDelta { index, delta, .. } => {
                let Some(message) = self.stream.clone() else {
                    return;
                };
                let key = format!("{message}:{index}");
                let appended = match delta {
                    StreamDelta::Text { text, .. } => text,
                    StreamDelta::Thinking { thinking, .. } => thinking,
                    _ => return,
                };
                self.shared.append(emit, &key, appended);
            }
            StreamEvent::ContentBlockStop { index, .. } => {
                let Some(message) = self.stream.clone() else {
                    return;
                };
                let key = format!("{message}:{index}");
                if let Some(open) = self.shared.open_item(&key).cloned() {
                    let thinking = matches!(
                        wire::ClaudeItem::decode(open.body.as_slice()).map(|item| item.kind),
                        Ok(Some(Kind::Thinking(_)))
                    );
                    self.close_block(emit, key, open.text, thinking);
                }
            }
            StreamEvent::MessageDelta { usage, .. } => self.context_seen(
                usage.input_tokens,
                usage.cache_creation_input_tokens,
                usage.cache_read_input_tokens,
            ),
            StreamEvent::MessageStop { .. } => self.stream = None,
            StreamEvent::Unknown(_) => {}
        }
    }

    fn open_block(&mut self, emit: &mut Emit, key: String, text: &str, thinking: bool) {
        let body = if thinking {
            item_body(Kind::Thinking(wire::Thinking { complete: false }))
        } else {
            item_body(Kind::Message(wire::Text { complete: false }))
        };
        self.shared.item(
            emit,
            ItemDraft {
                key,
                text: text.to_owned(),
                body,
                ..Default::default()
            },
        );
    }

    fn close_block(&mut self, emit: &mut Emit, key: String, text: String, thinking: bool) {
        let (body, (text, attachments)) = if thinking {
            (
                item_body(Kind::Thinking(wire::Thinking { complete: true })),
                (text, Vec::new()),
            )
        } else {
            (
                item_body(Kind::Message(wire::Text { complete: true })),
                crate::shared::parse_reply(text),
            )
        };
        self.shared.item(
            emit,
            ItemDraft {
                key: key.clone(),
                text,
                attachments,
                body,
                complete: true,
                ..Default::default()
            },
        );
        if !thinking {
            self.shared.note_message(&key);
        }
    }

    fn usage_seen(&mut self, usage: &Usage) {
        self.context_seen(
            Some(usage.input_tokens),
            usage.cache_creation_input_tokens,
            usage.cache_read_input_tokens,
        );
    }

    /// The tokens a request sent: what the context holds now.
    fn context_seen(&mut self, input: Option<u64>, created: Option<u64>, read: Option<u64>) {
        let tokens = [input, created, read].into_iter().flatten().sum::<u64>();
        if tokens > 0 {
            self.context_tokens = Some(tokens);
        }
    }

    /// Claude rejected the credential: the sign-in problem the strip
    /// shows until Claude reports an account again.
    fn sign_in_failed(&mut self, message: &str) {
        let account = self
            .sign_in
            .as_ref()
            .map(|sign_in| sign_in.account.clone())
            .unwrap_or_default();
        self.sign_in = Some(SignIn {
            state: SignInState::Failed as i32,
            account,
            message: message.to_owned(),
        });
    }

    // --- whole messages --------------------------------------------------

    fn assistant(&mut self, emit: &mut Emit, assistant: &AssistantMessage) {
        let message = &assistant.message;
        // A subagent's frame is its own: no turn, model, context, text or
        // error of the main thread's, only its calls, under the call that
        // started it.
        if let Some(parent) = written_opt(assistant.parent_tool_use_id.as_deref()) {
            for block in &message.content {
                if let ContentBlock::ToolUse {
                    id, name, input, ..
                } = block
                {
                    self.tool_seen(emit, id, name, Some(input), &parent);
                }
            }
            return;
        }
        self.shared.turn_started();
        let message_id = message.id.clone();
        if let Some(model) = written(&message.model)
            && model != "<synthetic>"
        {
            self.model = Some(model);
        }
        self.usage_seen(&message.usage);
        let blocks = &message.content;
        if let Some(error) = assistant
            .error
            .as_ref()
            .and_then(|error| written(error.as_str()))
        {
            let text = blocks_text(blocks);
            if error == AUTHENTICATION_FAILED {
                self.sign_in_failed(&text);
            }
            self.shared.item(
                emit,
                ItemDraft {
                    key: assistant.uuid.clone(),
                    body: item_body(Kind::ApiError(wire::ApiError {
                        error_kind: error,
                        message: text,
                        ..Default::default()
                    })),
                    complete: true,
                    ..Default::default()
                },
            );
            return;
        }
        let base = self.blocks.get(&message_id).copied().unwrap_or(0);
        // A built-in command Claude runs itself answers as the `<synthetic>`
        // model: the answer is the command's output, not a reply.
        if message.model == "<synthetic>"
            && !blocks.is_empty()
            && blocks
                .iter()
                .all(|block| matches!(block, ContentBlock::Text { .. }))
        {
            self.blocks.insert(message_id.clone(), base + 1);
            let key = format!("{message_id}:{base}");
            let text = blocks_text(blocks);
            match self.slash.take() {
                Some(slash) => self.slash_item(emit, key, text, &slash),
                None => {
                    self.synthetic = Some((key.clone(), text.clone()));
                    self.slash_item(emit, key, text, "");
                }
            }
            return;
        }
        self.blocks
            .insert(message_id.clone(), base + blocks.len() as u32);
        for (offset, block) in blocks.iter().enumerate() {
            let key = format!("{message_id}:{}", base + offset as u32);
            match block {
                ContentBlock::Text { text, .. } => self.close_block(emit, key, text.clone(), false),
                ContentBlock::Thinking { thinking, .. } => {
                    self.close_block(emit, key, thinking.clone(), true)
                }
                // Claude keeps a redacted block's thinking to itself.
                ContentBlock::RedactedThinking { .. } => {
                    self.close_block(emit, key, String::new(), true)
                }
                ContentBlock::ToolUse {
                    id, name, input, ..
                } => {
                    self.tool_seen(emit, id, name, Some(input), "");
                }
                other => self.shared.item(
                    emit,
                    ItemDraft {
                        key,
                        body: item_body(Kind::Unrecognized(wire::Unrecognized {
                            fact_type: format!("assistant/{}", other.kind()),
                            summary: String::new(),
                        })),
                        complete: true,
                        ..Default::default()
                    },
                ),
            }
        }
    }

    fn tool_seen(
        &mut self,
        emit: &mut Emit,
        id: &str,
        name: &str,
        input: Option<&Value>,
        parent: &str,
    ) {
        if id.is_empty() {
            return;
        }
        let (server, tool_name) = split_tool_name(name);
        let status = is_status_tool(&server, &tool_name);
        if status
            && let Some(input) = input
            && let Some(working_on) = status_working_on(input.to_string().as_bytes())
        {
            self.shared.set_working_on(working_on);
        }
        let now = self.shared.now_ms();
        let planning = self.permission_mode.as_deref();
        let tool = self.tools.entry(id.to_owned()).or_insert_with(|| Tool {
            parent_key: parent.to_owned(),
            ..Tool::running(name, now, planning, Call::default())
        });
        if let Some(input) = input {
            tool.set_input(input);
        }
        self.emit_tool(emit, id);
    }

    fn user(&mut self, emit: &mut Emit, user: &UserMessageOutput) {
        let content = &user.message.content;
        if let MessageContent::Blocks(blocks) = content {
            for block in blocks {
                if let ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                    ..
                } = block
                {
                    self.tool_result(
                        emit,
                        tool_use_id,
                        content.as_ref(),
                        is_error.unwrap_or(false),
                        user.tool_use_result.as_ref(),
                    );
                }
            }
        }
        // The compaction summary Claude writes back is its own; the SDK's
        // compaction row carries the counts.
        if user.parent_tool_use_id.as_deref().is_none_or(str::is_empty)
            && message_text(content).trim_start().starts_with(INTERRUPTED)
        {
            self.interrupted = true;
        }
    }

    /// Claude replays a prompt sent into the running turn when it takes it:
    /// before the turn's result it joined that turn at a tool boundary and
    /// its item is marked steered; after it, no boundary was left and it
    /// opened a turn of its own as an ordinary prompt.
    fn steer_replayed(&mut self, emit: &mut Emit, uuid: &str) {
        let Some(client) = self.clients.get(uuid).filter(|client| !client.message) else {
            return;
        };
        let id = client.id.clone();
        let Some(entry) = self.shared.steer_reflected(|entry| entry.input_id == id) else {
            return;
        };
        self.clients.remove(uuid);
        let body = if self.shared.is_busy() {
            Kind::Steer(wire::Steer {})
        } else {
            self.shared.turn_started();
            Kind::Prompt(wire::Prompt {})
        };
        self.shared.item(
            emit,
            ItemDraft {
                key: uuid.to_owned(),
                text: entry.text,
                attachments: entry.attachments,
                input_id: entry.input_id,
                body: item_body(body),
                complete: true,
                ..Default::default()
            },
        );
    }

    fn tool_result(
        &mut self,
        emit: &mut Emit,
        id: &str,
        content: Option<&stream::ToolResultBody>,
        is_error: bool,
        result: Option<&Value>,
    ) {
        let output = tool_result_text(content);
        let now = self.shared.now_ms();
        let Some(tool) = self.tools.get_mut(id) else {
            return;
        };
        let denied = tool
            .decision
            .as_ref()
            .is_some_and(|decision| decision.outcome == DecisionOutcome::Denied as i32);
        if tool.background && tool.of.task.as_ref().is_some_and(|task| !task.finished) {
            // The result only tells the model the task was launched, and
            // the call stays open until the task's notification: nothing a
            // reader sees changes, so the item is not sent again for it.
            return;
        }
        tool.state = if denied {
            ToolState::Denied
        } else if is_error {
            ToolState::Failed
        } else {
            ToolState::Succeeded
        } as i32;
        // A denial's result is the rejection amux wrote; the person's note
        // is on the decision.
        tool.outcome_text = if denied && output.starts_with(crate::claude_answers::REJECTED) {
            String::new()
        } else {
            output
        };
        tool.take_result(emit, content, result);
        // A question nothing here closed: Claude's own record says how.
        if tool.question && tool.asked.is_none() {
            tool.asked = Some(match result.map(AnsweredResult::deserialize) {
                Some(Ok(answered)) if !is_error => ask_item::recorded_by_claude(
                    &question_ask_text(&tool.input),
                    &answered.answers,
                    &answered.annotations,
                ),
                _ => ask_item::dismissed(),
            });
        }
        tool.ended_at_ms.get_or_insert(now);
        tool.apply_to_tasks(&mut self.tasks, result.unwrap_or(&Value::Null));
        self.emit_tool(emit, id);
    }

    /// A local command's output. Output no command typed through amux
    /// asked for is Claude's note on a setting amux changed (its "Set model
    /// to" after a model chosen from the settings), which the snapshot
    /// already shows: it draws nothing.
    fn slash_output(&mut self, emit: &mut Emit, key: String, output: &str) {
        let output = local_output(output).unwrap_or_else(|| output.to_owned());
        let Some(slash) = self.slash.take() else {
            return;
        };
        self.slash_item(emit, key, output, &slash);
    }

    /// A slash command's output item, the command as typed (`/name args`).
    fn slash_item(&mut self, emit: &mut Emit, key: String, output: String, slash: &str) {
        let (command, args) = slash.split_once(char::is_whitespace).unwrap_or((slash, ""));
        let (command, args) = (command.to_owned(), args.trim().to_owned());
        self.shared.item(
            emit,
            ItemDraft {
                key,
                text: output,
                body: item_body(Kind::Slash(wire::SlashOutput { command, args })),
                complete: true,
                ..Default::default()
            },
        );
    }

    fn result(&mut self, emit: &mut Emit, result: &ResultMessage) {
        // A result of a kind this build does not know still carries the
        // fields every result does.
        let unknown = match result {
            ResultMessage::Unknown(raw) => ResultError::deserialize(&raw.raw).ok(),
            _ => None,
        };
        let (common, errors): (Option<&ResultCommon>, &[String]) = match result {
            ResultMessage::Success(success) => (Some(&success.common), &[]),
            ResultMessage::ErrorDuringExecution(error)
            | ResultMessage::ErrorMaxTurns(error)
            | ResultMessage::ErrorMaxBudgetUsd(error)
            | ResultMessage::ErrorMaxStructuredOutputRetries(error) => {
                (Some(&error.common), &error.errors)
            }
            ResultMessage::Unknown(_) => match &unknown {
                Some(error) => (Some(&error.common), &error.errors),
                None => (None, &[]),
            },
        };
        // Any result states the window, the main thread's model's.
        if let Some(window) = common
            .and_then(|common| common.model_usage.as_ref())
            .and_then(|models| context_window(models, self.model.as_deref().unwrap_or_default()))
        {
            self.context_window = Some(window);
        }
        if common.is_some_and(|common| self.foreign(common)) {
            return;
        }
        self.dismiss_asks_but_background(emit);
        // A command that ran a turn (a custom command or a skill) wrote no
        // output of its own; a later note is not its.
        self.slash = None;
        let subtype = result.subtype();
        let aborted = common
            .and_then(|common| common.terminal_reason.as_deref())
            .is_some_and(|reason| reason.starts_with("aborted"));
        // An API failure ends the turn with subtype success and is_error;
        // the assistant row before it already carried the error.
        let api_failure = common.is_some_and(|common| common.is_error);
        let outcome = if std::mem::take(&mut self.interrupted) || aborted {
            TurnOutcome::Interrupted
        } else if subtype == "success" && !api_failure {
            TurnOutcome::Completed
        } else {
            TurnOutcome::Failed
        };
        let uuid = common
            .map(|common| common.uuid.as_str())
            .unwrap_or_default();
        if outcome == TurnOutcome::Failed && subtype != "success" {
            self.shared.item(
                emit,
                ItemDraft {
                    key: format!("{uuid}:error"),
                    body: item_body(Kind::ApiError(wire::ApiError {
                        error_kind: subtype.to_owned(),
                        message: errors.join("; "),
                        ..Default::default()
                    })),
                    complete: true,
                    ..Default::default()
                },
            );
        }
        let Some(turn) = self.shared.turn_ended(emit, outcome) else {
            return;
        };
        let at_ms = self.shared.now_ms();
        let started_at_ms = common.map_or(turn.started_at_ms, |common| {
            at_ms - common.duration_ms as i64
        });
        self.shared.item(
            emit,
            ItemDraft {
                key: format!("turn:{}", turn.id),
                body: item_body(Kind::Turn(Turn {
                    turn_id: turn.id,
                    outcome: outcome as i32,
                    started_at_ms,
                })),
                at_ms: Some(at_ms),
                complete: true,
                ..Default::default()
            },
        );
        // A call whose task outlives the turn is still open: its task's
        // events are progress on it until the notification ends it. So are
        // the calls of a subagent it runs, which finish after the turn.
        let open: BTreeSet<String> = self
            .tools
            .keys()
            .filter(|id| self.under_running_task(id))
            .cloned()
            .collect();
        self.tools.retain(|id, _| open.contains(id));
        self.blocks.clear();
    }

    /// Whether `result` ends work of Claude's own while a message amux
    /// handed it is unanswered: it names none of amux's messages, or names
    /// none at all and is not a person's. Such a result changes nothing of
    /// the person's turn. Any other answers the messages it names, or all
    /// of them when it names none.
    fn foreign(&mut self, common: &ResultCommon) -> bool {
        let named: BTreeSet<&str> = common
            .user_message_uuids
            .iter()
            .flatten()
            .chain(&common.user_message_uuid)
            .map(String::as_str)
            .collect();
        if !self.unanswered.is_empty() {
            let foreign = if named.is_empty() {
                common
                    .origin
                    .as_ref()
                    .and_then(|origin| origin.kind.as_deref())
                    .is_some_and(|kind| kind != "human")
            } else {
                !named.iter().any(|uuid| self.unanswered.contains(*uuid))
            };
            if foreign {
                return true;
            }
        }
        if named.is_empty() {
            self.unanswered.clear();
        } else {
            self.unanswered
                .retain(|uuid| !named.contains(uuid.as_str()));
        }
        false
    }

    // --- control ---------------------------------------------------------

    fn control_request_in(&mut self, emit: &mut Emit, request: ControlRequest, payload: &[u8]) {
        let request_id = request.request_id;
        match request.request {
            ControlRequestBody::CanUseTool(asked) => self.can_use_tool(emit, &request_id, asked),
            ControlRequestBody::Elicitation(elicitation) => {
                let server = elicitation.mcp_server_name;
                let message = without_terminal_codes(&elicitation.message);
                let (body, shape) = if elicitation.mode.as_deref() == Some("url") {
                    (
                        wire::ask::Body::Link(LinkAsk {
                            server,
                            message,
                            url: elicitation.url.unwrap_or_default(),
                        }),
                        AnswerShape::Link,
                    )
                } else {
                    (
                        wire::ask::Body::Form(FormAsk {
                            server,
                            message,
                            // The schema as Claude wrote it, key order kept.
                            schema_json: json_as_written(payload, &["request", "requested_schema"])
                                .unwrap_or_default(),
                        }),
                        AnswerShape::Form,
                    )
                };
                self.asks.insert(
                    request_id.clone(),
                    AskMeta {
                        tool_use_id: String::new(),
                        input: String::new(),
                        suggestions: Vec::new(),
                        shape,
                        agent: None,
                    },
                );
                let ask = wire::Ask {
                    item_key: ask_item::key(&request_id),
                    key: request_id,
                    body: Some(body),
                    opened_at_ms: self.shared.now_ms(),
                };
                self.emit_ask(emit, &ask, None);
                self.shared.open_ask(ask);
            }
            other => emit.effect(super::write(&stream::Input::ControlResponse(
                ControlResponse::error(
                    request_id,
                    format!("amux does not handle {}", other.kind()),
                ),
            ))),
        }
    }

    fn can_use_tool(&mut self, emit: &mut Emit, request_id: &str, asked: CanUseToolRequest) {
        let name = &asked.tool_name;
        let input = &asked.input;
        let tool_use_id = asked.tool_use_id.clone();
        // The call's row may not have been written yet: the request names
        // it, so the ask always has an item to point at, under the call
        // that started the subagent asking.
        let parent = asked
            .agent_id
            .as_ref()
            .and_then(|agent| self.active_tasks.get(agent))
            .map(|task| task.tool_key.clone())
            .unwrap_or_default();
        self.tool_seen(emit, &tool_use_id, name, Some(input), &parent);
        let (server, tool) = split_tool_name(name);
        // An ask Claude says may offer no persistent rule offers no scope:
        // a rule sent back would be narrowed or dropped, and amux would
        // record a grant Claude never applied.
        let suggestions = if asked.suppress_always_allow_rule == Some(true) {
            Vec::new()
        } else {
            asked.permission_suggestions.clone().unwrap_or_default()
        };
        let (body, shape) = match tool.as_str() {
            QUESTION_TOOL if server.is_empty() => {
                let question = question_ask(input);
                let questions = question
                    .questions
                    .iter()
                    .map(|question| {
                        (
                            question.question.clone(),
                            question
                                .options
                                .iter()
                                .map(|option| option.label.clone())
                                .collect(),
                            question.multi_select,
                        )
                    })
                    .collect();
                (
                    wire::ask::Body::Question(question),
                    AnswerShape::Question(questions),
                )
            }
            PLAN_TOOL if server.is_empty() => {
                let switch = crate::claude_common::plan_switch(self.offers_auto(), self.never_ask)
                    .to_owned();
                (
                    wire::ask::Body::Plan(plan_ask(&switch)),
                    AnswerShape::Plan { switch },
                )
            }
            _ => {
                let reason = match &asked.decision_reason {
                    Some(reason) => without_terminal_codes(reason),
                    // Claude names the path a command would write even
                    // inside its folder, where it asks only because writes
                    // need asking; only a path beyond the folder is the
                    // reason.
                    None => written_opt(asked.blocked_path.as_deref())
                        .filter(|path| !self.in_folder(path))
                        .map(|path| format!("outside the allowed directories: {path}"))
                        .unwrap_or_default(),
                };
                (
                    wire::ask::Body::Permission(PermissionAsk {
                        tool_name: tool.clone(),
                        input: Some(tool_input(
                            &tool,
                            &server,
                            input,
                            old_source(&asked.extensions),
                        )),
                        scopes: permission_scopes(&suggestions),
                        reason,
                        description: without_terminal_codes(
                            &written_opt(asked.description.as_deref())
                                .or_else(|| written_opt(asked.title.as_deref()))
                                .unwrap_or_default(),
                        ),
                        deny_stops: false,
                        deny_can_stop: true,
                        default_to_no: asked.default_to_no == Some(true),
                        server: server.clone(),
                    }),
                    AnswerShape::Permission,
                )
            }
        };
        self.asks.insert(
            request_id.to_owned(),
            AskMeta {
                tool_use_id: tool_use_id.clone(),
                input: input.to_string(),
                suggestions: suggestions
                    .iter()
                    .map(|suggestion| {
                        serde_json::to_value(suggestion)
                            .expect("a permission change serializes")
                            .to_string()
                    })
                    .collect(),
                shape,
                agent: asked.agent_id.clone(),
            },
        );
        let drawn = self
            .tools
            .get(&tool_use_id)
            .is_some_and(|tool| !tool.hidden);
        self.shared.open_ask(wire::Ask {
            key: request_id.to_owned(),
            item_key: if drawn { tool_use_id } else { String::new() },
            body: Some(body),
            opened_at_ms: self.shared.now_ms(),
        });
    }

    /// Asks Claude what it applied from its settings and this session's
    /// changes; the answer carries the effort it runs at.
    fn ask_settings(&mut self, emit: &mut Emit) {
        self.control_request(
            emit,
            Request::Settings,
            ControlRequestBody::GetSettings(Default::default()),
        );
    }

    /// Responses to requests: the interpreter's own, matched by id, and
    /// the agent process's initialize, recognised by its shape.
    fn control_response_in(&mut self, emit: &mut Emit, response: ControlResponse) {
        let ok = response.response.subtype == ControlOutcome::Success;
        match self.requests.remove(response.request_id()) {
            Some(Request::Model(model)) if ok => {
                self.chosen_model.clone_from(&model);
                self.model = model.or(self.model.take());
                self.ask_settings(emit);
            }
            Some(Request::Mode(mode)) if ok => self.permission_mode = Some(mode),
            Some(Request::StopJob(task)) if !ok => {
                let error = response.response.error.as_deref().unwrap_or_default();
                if let Some(jobs) = self.jobs.stop_refused(&task, without_terminal_codes(error)) {
                    self.shared.set_jobs(jobs);
                }
            }
            Some(Request::Effort(Some(effort))) if ok => self.effort = Some(effort),
            Some(Request::Effort(None)) if ok => self.ask_settings(emit),
            Some(Request::Settings) if ok => {
                let Some(Ok(settings)) = response.result::<SettingsResult>() else {
                    return;
                };
                if let Some(effort) = settings.applied.effort {
                    self.effort = Some(effort);
                }
                // Claude names its model in its init only once a turn
                // starts; until then the applied settings say which it is.
                if self.model.is_none()
                    && let Some(model) = settings.applied.model
                {
                    self.model = Some(model);
                }
            }
            Some(_) => {}
            None => self.agent_answer(emit, ok, &response),
        }
    }

    /// An answer to one of the agent process's own requests.
    fn agent_answer(&mut self, emit: &mut Emit, ok: bool, response: &ControlResponse) {
        if let Some(Ok(initialized)) = response.result::<InitializationResult>() {
            self.models = offered_models(&initialized.models);
            self.auto_models = Some(auto_models(&initialized.models));
            self.commands_listed(emit, ok, &initialized.commands);
            let account = &initialized.account;
            self.sign_in = Some(SignIn {
                state: SignInState::SignedIn as i32,
                account: written_opt(account.email.as_deref())
                    .or_else(|| written_opt(account.subscription_type.as_deref()))
                    .unwrap_or_default(),
                message: String::new(),
            });
        }
    }
}

impl State {
    /// Initialize's answer, which lists Claude's commands. Claude reports
    /// its init only once the first message arrives, so this answer is
    /// what says it takes input; without it a queued first prompt would
    /// wait for an init that only a prompt can bring. What it applies may
    /// have changed with it.
    fn commands_listed(&mut self, emit: &mut Emit, ok: bool, commands: &[SlashCommand]) {
        if ok {
            self.shared.provider_started();
            self.ask_settings(emit);
        }
        self.commands_changed(emit, commands);
    }

    /// Claude listed its commands, in an answer or unasked: the catalogue
    /// is rebuilt with them.
    fn commands_changed(&mut self, emit: &mut Emit, commands: &[SlashCommand]) {
        self.commands = offered_commands(commands);
        self.shared.set_catalogue(
            emit,
            wire::Catalogue {
                models: self.models.clone(),
                commands: self.commands.clone(),
                permissions: self.permissions(),
                ..Default::default()
            },
        );
    }
}

/// The context window the turn's model ran with: the named model's, or the
/// first Claude reports.
fn context_window(models: &BTreeMap<String, ModelUsage>, model: &str) -> Option<u64> {
    models
        .get(model)
        .or_else(|| models.values().next())
        .map(|usage| usage.context_window)
}

/// The command a slash command's replay names, with its arguments, as
/// typed.
fn command_name(text: &str) -> Option<String> {
    let tag = |name: &str| {
        let open = format!("<{name}>");
        let close = format!("</{name}>");
        let start = text.find(&open)? + open.len();
        let end = text[start..].find(&close)? + start;
        Some(text[start..end].trim().to_owned())
    };
    let command = tag("command-name")?;
    Some(match tag("command-args").filter(|args| !args.is_empty()) {
        Some(args) => format!("{command} {args}"),
        None => command,
    })
}

/// A local command's output as Claude replays it.
fn local_output(text: &str) -> Option<String> {
    let text = text.trim();
    for tag in ["local-command-stdout", "local-command-stderr"] {
        let open = format!("<{tag}>");
        let close = format!("</{tag}>");
        if let Some(rest) = text.strip_prefix(&open) {
            return Some(
                rest.split(&close)
                    .next()
                    .unwrap_or_default()
                    .trim()
                    .to_owned(),
            );
        }
    }
    None
}

fn server_health<'a>(
    servers: impl Iterator<Item = (&'a str, &'a str, &'a str)>,
) -> ToolServerHealth {
    let servers = servers
        .map(|(name, status, error)| ToolServer {
            name: name.to_owned(),
            status: match status {
                "connected" => ToolServerStatus::Ready,
                "pending" => ToolServerStatus::Starting,
                "needs-auth" => ToolServerStatus::NeedsAuth,
                "failed" => ToolServerStatus::Failed,
                _ => ToolServerStatus::Unspecified,
            } as i32,
            error: error.to_owned(),
        })
        .collect::<Vec<_>>();
    // A server waiting for sign-in is the person's choice, not a failure.
    let degraded = servers
        .iter()
        .any(|server| server.status == ToolServerStatus::Failed as i32);
    ToolServerHealth {
        state: if degraded {
            HealthState::Degraded
        } else {
            HealthState::Healthy
        } as i32,
        servers,
    }
}

/// One window's use and reset as reported; a reset later than the one held
/// means the window has reset, which ends the status stated for it. A use
/// or reset the report leaves out keeps the one held.
fn report_window(meter: &mut UsageMeter, utilization: Option<f64>, resets_at_ms: Option<i64>) {
    if let (Some(held), Some(reported)) = (meter.resets_at_ms, resets_at_ms)
        && reported > held
    {
        meter.state = UsageState::Unknown as i32;
    }
    if let Some(utilization) = utilization {
        meter.used_percent = utilization * 100.0;
        meter.used_unknown = false;
    }
    if resets_at_ms.is_some() {
        meter.resets_at_ms = resets_at_ms;
    }
}

/// The meter of the window Claude calls `name`, added the first time it
/// is named, its use unknown until a report gives it.
fn claude_window<'a>(usage: &'a mut ClaudeUsage, name: &str) -> &'a mut UsageMeter {
    let at = match usage
        .windows
        .iter()
        .position(|window| window.provider_name == name)
    {
        Some(at) => at,
        None => {
            let (limit, model) = claude_limit(name).unwrap_or((Limit::Unspecified, None));
            usage.windows.push(ClaudeUsageWindow {
                limit: limit as i32,
                model: model.map(str::to_owned),
                provider_name: name.to_owned(),
                meter: Some(UsageMeter {
                    used_unknown: true,
                    ..Default::default()
                }),
            });
            usage.windows.len() - 1
        }
    };
    usage.windows[at].meter.get_or_insert_default()
}
