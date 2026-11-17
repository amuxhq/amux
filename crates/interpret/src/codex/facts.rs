//! What the app server writes: responses, requests and notifications.

use std::collections::BTreeSet;

use codex_protocol::client::{
    BackgroundTerminalsListParams, BackgroundTerminalsTerminateParams, CommandApprovalResponse,
    ElicitationAction, ModelListParams, SkillsListParams, ThreadIdParams, ThreadResumeParams,
    ThreadSetNameParams, TurnInterruptParams,
};
use codex_protocol::items::{
    CommandAction, FileUpdateChange, McpToolCallItem, MessagePhase, PatchChangeKind,
    SubAgentActivityKind, ThreadItem, UserInput, WebSearchAction, WebSearchItem,
};
use codex_protocol::server::{
    AccountReadResponse, AutoApprovalReview, BackgroundTerminalTerminateResponse,
    BackgroundTerminalsResponse, CommandApprovalParams, CommandDecision, Decision as Offered,
    DeprecationNotice, ElicitationParams, ErrorNotification, McpServerStatusUpdated, Model,
    ModelListResponse, SkillsListResponse, ThreadResponse, TurnStartResponse,
};
use codex_protocol::thread::{
    AskForApproval, CodexErrorInfo, RateLimitSnapshot, ReasoningEffort, SandboxPolicy, TurnError,
};
use codex_protocol::{
    ClientRequest, ClientResponse, RequestId, RpcError, ServerMessage, ServerNotification,
    ServerRequest, Thread, Unknown,
};
use serde_json::Value;
use wire::{
    AccessGrant, ApiError, BackgroundJob, CodexAsk, CodexUsage, CodexUsageWindow, CommandApproval,
    Decision, DecisionOutcome, FileChangeApproval, FormAsk, Limit, LinkAsk, McpToolApproval,
    OfferedModel, Question, QuestionAsk, QuestionOption, ReviewerVerdict, SignIn, SignInState,
    TaskListStatus, ToolClass, ToolDecision, ToolServer, ToolServerStatus, ToolState, Turn,
    TurnOutcome, UsageMeter, UsageState, Work, codex_ask, codex_item, work,
};

use super::{
    AskMeta, DROPPED_NOTIFICATIONS, FRESH_THREAD_REQUEST, IdleCompaction, ListedSkill, Proposed,
    RESUME_REQUEST, Request, State, Streamed, WorkState, ask_key, client_message_id,
    elicitation_response, item_body, work_ask, work_complete,
};
use crate::claude_common::compact_json;
use crate::shared::{Output, json_as_written, patch_first_change};
use crate::{
    AMUX_TOOL_SERVER, Channel, Effect, Emit, Fact, ItemDraft, SendOutcome, ask_item, is_send_tool,
    is_status_tool, sent_message, status_working_on,
};

/// The answers a command approval offers when Codex names none.
const DEFAULT_DECISIONS: [Offered; 4] = [
    Offered::Accept,
    Offered::AcceptForSession,
    Offered::Decline,
    Offered::Cancel,
];

fn default_decisions() -> Vec<CommandDecision> {
    DEFAULT_DECISIONS
        .into_iter()
        .map(CommandDecision::Plain)
        .collect()
}

/// A string that says something.
fn some(text: &str) -> Option<String> {
    (!text.is_empty()).then(|| text.to_owned())
}

fn some_of(text: Option<&str>) -> Option<String> {
    text.and_then(some)
}

/// A command as the model wrote it. Codex runs a model's command through
/// the user's shell and reports the wrapped form (`/bin/zsh -lc "…"`), with
/// the quoting the wrapper needs; Codex's own view shows the script inside,
/// and so does amux.
fn shown_command(command: &str) -> String {
    let Ok(words) = shell_words::split(command) else {
        return command.to_owned();
    };
    match words.as_slice() {
        [shell, flag, script]
            if matches!(flag.as_str(), "-lc" | "-c")
                && matches!(
                    shell.rsplit('/').next(),
                    Some("bash" | "zsh" | "sh" | "dash" | "ksh" | "fish")
                ) =>
        {
            script.clone()
        }
        _ => command.to_owned(),
    }
}

/// A web search item: what it searched for, or the page it opened or looked
/// within.
fn web_search(search: &WebSearchItem) -> wire::WebSearch {
    let (action, url, pattern, query) = match &search.action {
        Some(WebSearchAction::Search(action)) => (
            wire::WebAction::Search,
            None,
            None,
            some_of(action.query.as_deref()),
        ),
        Some(WebSearchAction::OpenPage(page)) => {
            (wire::WebAction::OpenPage, page.url.as_deref(), None, None)
        }
        Some(WebSearchAction::FindInPage(find)) => (
            wire::WebAction::FindInPage,
            find.url.as_deref(),
            find.pattern.as_deref(),
            None,
        ),
        Some(WebSearchAction::Other(_) | WebSearchAction::Unknown(_)) | None => {
            (wire::WebAction::Unspecified, None, None, None)
        }
    };
    wire::WebSearch {
        query: some(&search.query).or(query).unwrap_or_default(),
        action: action as i32,
        url: url.unwrap_or_default().to_owned(),
        pattern: pattern.unwrap_or_default().to_owned(),
    }
}

/// A sandbox policy as the mode name inputs use.
fn sandbox_mode(policy: &SandboxPolicy) -> Option<String> {
    match policy.mode() {
        Some(mode) => Some(mode.as_str().to_owned()),
        None => some(policy.kind()),
    }
}

/// An approval policy as inputs name it; a granular policy has no name.
fn approval_name(policy: &AskForApproval) -> Option<String> {
    match policy {
        AskForApproval::Named(policy) => some(policy.as_str()),
        AskForApproval::Granular { .. } => None,
    }
}

/// The named permission a turn's overrides make, if they make one.
pub(super) fn named_permission(
    approval: &AskForApproval,
    sandbox: &SandboxPolicy,
    reviewer: Option<&str>,
) -> Option<&'static str> {
    super::permission_of(&approval_name(approval)?, &sandbox_mode(sandbox)?, reviewer)
}

fn effort_name(effort: Option<&ReasoningEffort>) -> Option<String> {
    some_of(effort.map(ReasoningEffort::as_str))
}

fn tool_state(status: &str) -> ToolState {
    match status {
        "inProgress" => ToolState::Running,
        "completed" => ToolState::Succeeded,
        "failed" => ToolState::Failed,
        "declined" => ToolState::Denied,
        _ => ToolState::Running,
    }
}

/// An offered approval decision as the wire's, and the response it sends.
fn offered_decision(offered: &CommandDecision) -> Option<(Decision, String)> {
    let decision = match offered {
        CommandDecision::Plain(Offered::Accept) => Decision::Approve,
        CommandDecision::Plain(Offered::AcceptForSession) => Decision::ApproveSession,
        CommandDecision::Plain(Offered::Decline) => Decision::Deny,
        CommandDecision::Plain(Offered::Cancel) => Decision::Abort,
        CommandDecision::Plain(Offered::Other(_)) => return None,
        CommandDecision::AcceptWithExecpolicyAmendment { .. } => Decision::ApproveSimilar,
        CommandDecision::ApplyNetworkPolicyAmendment { .. } => Decision::ApproveNetwork,
    };
    Some((decision, response_text(&decision_response(offered.clone()))))
}

fn decision_response(decision: CommandDecision) -> ClientResponse {
    ClientResponse::CommandApproval(CommandApprovalResponse {
        decision,
        extra: Default::default(),
    })
}

/// An answer as an ask keeps it until the person picks it.
fn response_text(response: &ClientResponse) -> String {
    serde_json::to_string(response).expect("protocol types serialize")
}

/// Whether `id` is one of the agent process's own requests, which it
/// names with an `agent-` prefix: its handshake with Codex.
fn is_agent_request(id: &RequestId) -> bool {
    matches!(id, RequestId::String(id) if id.starts_with("agent-"))
}

/// The empty form content an approval accepts with.
fn no_content() -> Option<Value> {
    Some(Value::Object(Default::default()))
}

/// The skills `skills/list` answers.
fn skills(result: &Value) -> Vec<ListedSkill> {
    codex_protocol::result::<SkillsListResponse>(result)
        .map(listed_skills)
        .unwrap_or_default()
}

/// The skills a `skills/list` answer names, across every folder it lists,
/// in Codex's order; a skill the person turned off is left out.
pub(super) fn listed_skills(listed: SkillsListResponse) -> Vec<ListedSkill> {
    listed
        .data
        .into_iter()
        .flat_map(|folder| folder.skills)
        .filter(|skill| skill.enabled)
        .map(|skill| ListedSkill {
            name: skill.name,
            description: skill.description,
            source: skill.scope,
            path: skill.path,
        })
        .collect()
}

/// The models a page of `model/list` offers; a hidden one is left out.
pub(crate) fn offered_models(page: &[Model]) -> Vec<OfferedModel> {
    page.iter()
        .filter(|model| !model.hidden)
        .map(|model| OfferedModel {
            value: model.id.clone(),
            display_name: model.display_name.clone(),
            description: model.description.clone(),
            efforts: model
                .supported_reasoning_efforts
                .iter()
                .filter_map(|effort| some(effort.reasoning_effort.as_str()))
                .collect(),
            default_effort: some(model.default_reasoning_effort.as_str()),
            resolved_model: model.model.clone(),
        })
        .collect()
}

/// "Reconnecting... 2/5" as attempt and maximum.
fn attempts(message: &str) -> (u32, u32) {
    let Some(tail) = message.rsplit(' ').next() else {
        return (0, 0);
    };
    let mut parts = tail.split('/');
    match (
        parts.next().and_then(|n| n.parse().ok()),
        parts.next().and_then(|n| n.parse().ok()),
    ) {
        (Some(attempt), Some(max)) => (attempt, max),
        _ => (0, 0),
    }
}

/// Codex's typed error kind: the name of its error-info variant.
/// The kind Codex gives a turn refused for a spent limit.
const USAGE_LIMIT_EXCEEDED: &str = "usageLimitExceeded";

pub(super) fn error_kind(error: &TurnError) -> String {
    error
        .codex_error_info
        .as_ref()
        .map(|info| info.kind().to_owned())
        .unwrap_or_default()
}

fn unauthorized(error: &TurnError) -> bool {
    match &error.codex_error_info {
        Some(CodexErrorInfo::Named(kind)) => kind == "unauthorized",
        Some(info) => info.http_status_code() == Some(401),
        None => false,
    }
}

/// What an error says: its details when it has them, else its headline.
pub(super) fn error_message(error: &TurnError) -> String {
    some_of(error.additional_details.as_deref()).unwrap_or_else(|| error.message.clone())
}

/// The thread a notification is about, for telling a child thread's
/// activity from this one's. `thread/started` is read whichever thread it
/// names: the thread it starts may be this agent's own.
fn about_thread(notification: &ServerNotification) -> Option<&str> {
    match notification {
        ServerNotification::ThreadStarted(_) => None,
        notification => notification.thread_id(),
    }
}

impl State {
    pub(super) fn fact(&mut self, emit: &mut Emit, fact: Fact) {
        if fact.channel != Channel::Rpc {
            return self.unrecognized(
                emit,
                &format!("{:?}", fact.channel),
                "a fact on a channel Codex does not use",
            );
        }
        let message = match codex_protocol::decode(&fact.payload) {
            Ok(message) => message,
            Err(_) if serde_json::from_slice::<serde::de::IgnoredAny>(&fact.payload).is_ok() => {
                return self.unrecognized(emit, "message", "neither a request nor a response");
            }
            Err(_) => return self.unrecognized(emit, "unparsed", "a line that is not JSON"),
        };
        // What a child says while its start waits for its title waits too.
        let about = match &message {
            ServerMessage::Request { request, .. } => Some(request.thread_id()),
            ServerMessage::Notification {
                notification: ServerNotification::ServerRequestResolved(_),
                ..
            } => None,
            ServerMessage::Notification { notification, .. } => about_thread(notification),
            _ => None,
        };
        if let Some(thread) = about.filter(|thread| self.thread_id.as_deref() != Some(*thread))
            && self.hold(emit, thread, &fact.payload)
        {
            return;
        }
        match message {
            ServerMessage::Request { id, request, .. } => {
                self.server_request(emit, &id, request, &fact.payload)
            }
            ServerMessage::Notification { notification, .. } => {
                self.notification(emit, notification)
            }
            ServerMessage::Response { id, result, .. } => self.response(emit, &id, result),
            ServerMessage::Unknown(unknown) => self.unknown(emit, unknown),
        }
    }

    /// A line this interpreter cannot read. A request is refused, so the
    /// server does not wait on it; an answer it cannot read did not do what
    /// was asked.
    fn unknown(&mut self, emit: &mut Emit, unknown: Unknown) {
        match (unknown.method.as_deref(), unknown.id.as_ref()) {
            (Some(method), Some(id)) => {
                let refusal = RpcError {
                    code: -32601,
                    message: format!("amux does not handle {method}"),
                    extra: Default::default(),
                };
                self.respond(emit, &ask_key(id), Err(refusal));
                if !unknown
                    .thread_id()
                    .is_some_and(|thread| self.is_child(&thread))
                {
                    self.unrecognized(emit, method, "a request amux cannot answer");
                }
            }
            (Some(method), None) => {
                // A child thread's activity on the same server is its own.
                if let (Some(ours), Some(theirs)) = (self.thread_id.as_deref(), unknown.thread_id())
                    && ours != theirs
                {
                    return;
                }
                if DROPPED_NOTIFICATIONS.contains(&method) {
                    return;
                }
                self.unrecognized(emit, method, "a notification amux does not read");
            }
            (None, Some(id)) => {
                let refused = RpcError {
                    code: 0,
                    message: String::new(),
                    extra: Default::default(),
                };
                self.response(emit, id, Err(refused));
            }
            (None, None) => self.unrecognized(emit, "message", "neither a request nor a response"),
        }
    }

    fn local_key(&mut self, prefix: &str) -> String {
        self.next_boundary += 1;
        format!("{prefix}:{}", self.next_boundary)
    }

    fn unrecognized(&mut self, emit: &mut Emit, fact_type: &str, summary: &str) {
        let key = self.local_key("unrecognized");
        self.emit_item(
            emit,
            ItemDraft {
                key,
                body: item_body(codex_item::Kind::Unrecognized(wire::Unrecognized {
                    fact_type: fact_type.to_owned(),
                    summary: summary.to_owned(),
                })),
                complete: true,
                ..Default::default()
            },
        );
    }

    /// When the spent usage windows reset.
    fn spent_until(&self) -> Option<i64> {
        self.usage.as_ref().and_then(|usage| {
            crate::notice::spent_until(
                usage
                    .windows
                    .iter()
                    .filter_map(|window| window.meter.as_ref()),
            )
        })
    }

    /// How an error the turn can't go past stops the agent.
    fn blocked_by(&self, error: &TurnError) -> wire::BlockedNotice {
        if unauthorized(error) {
            crate::notice::signed_out()
        } else if error_kind(error) == USAGE_LIMIT_EXCEEDED {
            crate::notice::usage_spent(self.spent_until())
        } else {
            crate::notice::failed(&error_message(error))
        }
    }

    /// Codex stopped on this error. Only here does a 401 mean the sign-in
    /// failed: one Codex is still retrying can recover by itself.
    fn gave_up(&mut self, error: &TurnError) {
        if unauthorized(error) {
            self.sign_in = Some(SignIn {
                state: SignInState::Failed as i32,
                account: String::new(),
                message: error_message(error),
            });
        }
        let why = self.blocked_by(error);
        self.shared.stopped_by(why);
    }

    pub(super) fn error_item(&mut self, emit: &mut Emit, key: String, error: ApiError) {
        self.emit_item(
            emit,
            ItemDraft {
                key,
                body: item_body(codex_item::Kind::Error(error)),
                complete: true,
                ..Default::default()
            },
        );
    }

    // --- responses -------------------------------------------------------

    /// The account the handshake's `account/read` names.
    fn account_answered(&mut self, result: &Value) {
        if let Ok(response) = codex_protocol::result::<AccountReadResponse>(result) {
            self.sign_in = Some(match response.account {
                None => SignIn {
                    state: SignInState::SignedOut as i32,
                    ..Default::default()
                },
                Some(account) => SignIn {
                    state: SignInState::SignedIn as i32,
                    account: match &account {
                        codex_protocol::server::Account::Chatgpt(chatgpt) => {
                            some_of(chatgpt.email.as_deref())
                        }
                        _ => None,
                    }
                    .unwrap_or_else(|| account.kind().to_owned()),
                    message: String::new(),
                },
            });
        }
    }

    /// A thread answer: the handshake's, the resume after an unarchive, or
    /// the new thread started in place of one with no history. False when
    /// the answer names no thread.
    fn thread_answered(&mut self, emit: &mut Emit, result: &Value, fresh: bool) -> bool {
        let Ok(response) = codex_protocol::result::<ThreadResponse>(result) else {
            return false;
        };
        if fresh {
            self.thread_id = None;
        }
        self.thread_started(emit, &response.thread, Some(&response), fresh);
        if self.thread_id.as_ref() == Some(&response.thread.id) {
            self.name_thread(emit);
        }
        self.list_offers(emit);
        true
    }

    /// Codex refused the resume of the agent's thread. An archived thread
    /// is unarchived and resumed, as Codex's refusal asks; one whose history
    /// is gone is replaced by a new thread, with a notice. False for any
    /// other refusal.
    fn resume_refused(&mut self, emit: &mut Emit, error: &RpcError) -> bool {
        if error.message.contains(" is archived")
            && let Some(thread_id) = self.thread_id.clone()
        {
            let bytes = self.request_as(
                "amux-unarchive".into(),
                ClientRequest::ThreadUnarchive(ThreadIdParams {
                    thread_id,
                    extra: Default::default(),
                }),
                Request::Unarchive,
            );
            emit.effect(Effect::ProviderWrite(bytes));
            return true;
        }
        if error.message.starts_with("no rollout found") {
            self.warning(
                emit,
                "missingHistory",
                "Codex has no history left for this conversation, so a new one started.".to_owned(),
            );
            let params = self.new_thread();
            let bytes = self.request_as(
                FRESH_THREAD_REQUEST.into(),
                ClientRequest::ThreadStart(params),
                Request::Fresh,
            );
            emit.effect(Effect::ProviderWrite(bytes));
            return true;
        }
        false
    }

    /// Resumes the agent's thread, without its turns.
    fn resume(&mut self, emit: &mut Emit) {
        let params = ThreadResumeParams {
            thread_id: self.thread(),
            cwd: (!self.cwd.is_empty()).then(|| self.cwd.clone()),
            model: self.model.clone(),
            exclude_turns: Some(true),
            ..Default::default()
        };
        let bytes = self.request_as(
            "amux-resume".into(),
            ClientRequest::ThreadResume(params),
            Request::Resume,
        );
        emit.effect(Effect::ProviderWrite(bytes));
    }

    /// Codex refused to give the agent a thread. Waiting would leave the
    /// agent starting for good and swallow every prompt, so the incarnation
    /// ends with Codex's own words.
    fn refused_to_start(&mut self, emit: &mut Emit, error: &RpcError) {
        emit.effect(Effect::Exit {
            cause: format!("Codex refused to start: {}", error.message),
        });
    }

    fn response(&mut self, emit: &mut Emit, id: &RequestId, result: Result<Value, RpcError>) {
        let tracked = match id {
            RequestId::String(id) => self
                .requests
                .remove(id)
                .map(|request| (id.clone(), request)),
            RequestId::Integer(_) => None,
        };
        let Some((id, request)) = tracked else {
            // Every request the interpreter makes is tracked until answered,
            // so an answer to one of its own ids it does not track is to a
            // request an earlier build made and this one no longer does, as
            // a recording of that build holds: nothing of this agent's.
            if matches!(id, RequestId::String(id) if id.starts_with("amux-")) {
                return;
            }
            // The agent process's own handshake.
            let resumed = matches!(id, RequestId::String(id) if id == RESUME_REQUEST);
            let result = match result {
                Ok(result) => result,
                Err(error) if resumed && self.resume_refused(emit, &error) => return,
                // Refused, it never brings a thread: Codex holding the
                // thread for another server ("already has an active
                // writer"), a thread it cannot find.
                Err(error) if is_agent_request(id) => return self.refused_to_start(emit, &error),
                Err(_) => return,
            };
            if !self.thread_answered(emit, &result, false) {
                self.account_answered(&result);
            }
            return;
        };
        match request {
            Request::Unarchive | Request::Resume | Request::Fresh => {
                return match result {
                    Ok(_) if request == Request::Unarchive => self.resume(emit),
                    Ok(result) => {
                        self.thread_answered(emit, &result, request == Request::Fresh);
                    }
                    Err(error) => self.refused_to_start(emit, &error),
                };
            }
            _ => {}
        }
        match request {
            Request::TitleConfig => return self.title_config_read(emit, result),
            Request::TitleThread => return self.title_thread_started(emit, result),
            Request::TitleTurn => {
                if result.is_err() {
                    self.title_turn_refused(emit);
                }
                return;
            }
            _ => {}
        }
        if let Request::Models { page, listed } = request {
            return self.models_listed(emit, page, listed, result.ok().as_ref());
        }
        if let Request::Skills = request {
            if let Ok(result) = &result {
                self.skills = skills(result);
                self.publish_catalogue(emit);
            }
            if std::mem::take(&mut self.skills_stale) {
                self.list_skills(emit);
            }
            return;
        }
        if let Request::Jobs {
            page,
            listed,
            child,
        } = request
        {
            let page_result = result.ok();
            return match child {
                Some(child) if self.children.contains_key(&child) => {
                    let lists = |state: &Self| {
                        state
                            .requests
                            .values()
                            .filter(|request| matches!(request, Request::Jobs { child: Some(asked), .. } if *asked == child))
                            .count()
                    };
                    let before = lists(self);
                    self.as_child(&child, |state| {
                        state.jobs_listed(emit, page, listed, page_result.as_ref())
                    });
                    // Whole unless this page asked for the next.
                    if lists(self) == before {
                        self.child_jobs_listed(emit, &child);
                    }
                }
                Some(_) => {}
                None => self.jobs_listed(emit, page, listed, page_result.as_ref()),
            };
        }
        match request {
            Request::ChildRead { thread } => return self.child_read(emit, &thread, result.ok()),
            Request::ChildInterrupt { thread } => {
                let result = result.map(drop).map_err(|error| error.message);
                return self.child_interrupted(emit, &thread, result);
            }
            Request::StopNote {
                thread,
                name,
                steered,
            } => {
                if result.is_err() && steered {
                    let name = if name.is_empty() {
                        thread.clone()
                    } else {
                        name
                    };
                    self.send_stop_note(emit, &thread, name, false);
                }
                return;
            }
            _ => {}
        }
        if let Request::StopJob { key, child } = request {
            // Ended, the command's item completes and takes the job off
            // the list; refused, the job runs on and says why.
            let refused = match result {
                Ok(answer) => {
                    codex_protocol::result::<BackgroundTerminalTerminateResponse>(&answer)
                        .ok()
                        .filter(|answer| !answer.terminated)
                        .map(|_| "Codex is not running it.".to_owned())
                }
                Err(error) => Some(error.message),
            };
            if let Some(error) = refused {
                match child {
                    Some(child) if self.children.contains_key(&child) => {
                        self.as_child(&child, |state| state.stop_refused(&key, error.clone()));
                        if let Some(child) = self.children.get_mut(&child) {
                            child.stop_error = error;
                        }
                    }
                    Some(_) => {}
                    None => self.stop_refused(&key, error),
                }
            }
            return;
        }
        let result = match result {
            Ok(result) => result,
            Err(error) => {
                // A steer that lost the race with the turn's end is not a
                // failure: the prompt waits for the next turn.
                if let Request::Steer { input_id } = &request {
                    self.shared.steer_refused(input_id);
                    return;
                }
                self.error_item(
                    emit,
                    format!("error:rpc:{id}"),
                    ApiError {
                        error_kind: "request".into(),
                        message: error.message,
                        ..Default::default()
                    },
                );
                match request {
                    Request::Turn { consumes } => {
                        // An interrupt asked for while this turn was starting
                        // had this turn in mind, never the next one.
                        self.interrupt_pending = false;
                        self.shared.reflect_prompt();
                        self.shared.turn_abandoned();
                        // Nothing will consume them now.
                        for envelope in consumes {
                            self.shared.message_consumed(&envelope);
                        }
                    }
                    Request::Compact | Request::Review | Request::Clear => {
                        self.interrupt_pending = false;
                        self.shared.turn_abandoned();
                    }
                    Request::IdleCompact => self.idle_compaction_over(emit),
                    Request::Steer { .. } => {}
                    Request::Inject { envelope_id, .. } => {
                        self.shared.message_consumed(&envelope_id);
                    }
                    Request::Interrupt
                    | Request::Models { .. }
                    | Request::Name
                    | Request::TitleConfig
                    | Request::TitleThread
                    | Request::TitleTurn
                    | Request::Persist
                    | Request::Skills
                    | Request::Jobs { .. }
                    | Request::StopJob { .. }
                    | Request::Unarchive
                    | Request::Resume
                    | Request::Fresh
                    | Request::Unsubscribe
                    | Request::ChildRead { .. }
                    | Request::ChildInterrupt { .. }
                    | Request::StopNote { .. } => {}
                }
                return;
            }
        };
        match request {
            Request::Clear => {
                if let Ok(response) = codex_protocol::result::<ThreadResponse>(&result) {
                    self.cleared(emit, &response);
                }
            }
            Request::Review => self.turn_answered(&result),
            Request::Turn { consumes } => {
                self.turn_answered(&result);
                for envelope in consumes {
                    self.shared.message_consumed(&envelope);
                }
            }
            Request::Inject {
                envelope_id,
                during_turn,
                turn_over,
            } => {
                // A running turn samples again with injected items in
                // context before it ends (codex-cli 0.157.0 onwards), so a
                // message injected mid-turn is consumed at the inject's
                // acknowledgement and needs no turn of its own.
                if during_turn {
                    if turn_over && !self.shared.is_busy() {
                        // The turn it was sent into ended first: nothing
                        // answers it until a turn starts.
                        self.kick(emit, vec![envelope_id]);
                    } else {
                        // Still running, or a later turn took the recorded
                        // item into its context.
                        self.shared.message_consumed(&envelope_id);
                    }
                }
            }
            Request::Steer { .. }
            | Request::Interrupt
            | Request::Compact
            | Request::IdleCompact
            | Request::Models { .. }
            | Request::Name
            | Request::TitleConfig
            | Request::TitleThread
            | Request::TitleTurn
            | Request::Persist
            | Request::Skills
            | Request::Jobs { .. }
            | Request::StopJob { .. }
            | Request::Unarchive
            | Request::Resume
            | Request::Fresh
            | Request::Unsubscribe
            | Request::ChildRead { .. }
            | Request::ChildInterrupt { .. }
            | Request::StopNote { .. } => {}
        }
    }

    /// `/clear`'s thread: from now on it is this agent's conversation.
    /// The old thread's turn state and jobs stay with it.
    fn cleared(&mut self, emit: &mut Emit, response: &ThreadResponse) {
        let Some(id) = some(&response.thread.id) else {
            self.shared.turn_abandoned();
            return;
        };
        // Codex keeps a thread loaded, with its tool servers, while a client
        // is subscribed: the agent leaves the old thread and the children it
        // spawned in this incarnation, running or ended, as Codex's own app
        // does when it switches threads. Those still running end here, since
        // nothing more will be heard of them.
        let mut children: BTreeSet<String> = self.children.keys().cloned().collect();
        self.leave_children(emit);
        children.append(&mut self.ended_children);
        let left: Vec<String> = self.thread_id.take().into_iter().chain(children).collect();
        self.left.extend(left.iter().cloned());
        for thread_id in left {
            let bytes = self.request_as(
                format!("amux-unsubscribe-{thread_id}"),
                ClientRequest::ThreadUnsubscribe(ThreadIdParams {
                    thread_id,
                    extra: Default::default(),
                }),
                Request::Unsubscribe,
            );
            emit.effect(Effect::ProviderWrite(bytes));
        }
        self.thread_id = Some(id);
        if let Some(model) = some(&response.model) {
            self.model = Some(model);
        }
        if let Some(policy) = response.approval_policy.as_ref().and_then(approval_name) {
            self.approval = Some(policy);
        }
        if let Some(sandbox) = response.sandbox.as_ref().and_then(sandbox_mode) {
            self.sandbox = Some(sandbox);
        }
        if let Some(reviewer) = &response.approvals_reviewer {
            self.reviewer = some(reviewer.as_str());
        }
        // The new thread's first turn carries the mode and effort the
        // agent ran with.
        if let Some(mode) = self.collaboration.clone() {
            self.overrides.mode = Some(mode);
        }
        self.overrides.effort = Some(self.effort.clone());
        self.active_turn = None;
        self.context_tokens = None;
        self.background = None;
        self.shared.set_jobs(Vec::new());
        self.shared.turn_abandoned();
        self.boundary(
            emit,
            wire::BoundaryKind::Cleared,
            wire::IncarnationEnd::default(),
        );
        self.name_thread(emit);
    }

    /// A page of `model/list`: the next page is asked for while the server
    /// names one, and the list is taken whole from the last. A refused page
    /// drops the list; it is what the person picks from, and half of it
    /// would mislead.
    fn models_listed(
        &mut self,
        emit: &mut Emit,
        number: u32,
        mut listed: Vec<OfferedModel>,
        page: Option<&Value>,
    ) {
        let Some(page) = page else {
            return;
        };
        let page = codex_protocol::result::<ModelListResponse>(page).ok();
        let data = page
            .as_ref()
            .map(|page| page.data.as_slice())
            .unwrap_or_default();
        listed.extend(offered_models(data));
        match page
            .as_ref()
            .and_then(|page| some_of(page.next_cursor.as_deref()))
        {
            Some(cursor) if !data.is_empty() => {
                self.list_models(emit, number + 1, Some(cursor), listed)
            }
            _ => {
                self.models = listed;
                self.publish_catalogue(emit);
            }
        }
    }

    /// The catalogue from what Codex last listed; written only when it
    /// differs from the last one.
    fn publish_catalogue(&mut self, emit: &mut Emit) {
        self.shared.set_catalogue(
            emit,
            wire::Catalogue {
                models: self.models.clone(),
                commands: super::offered_commands(),
                permissions: super::offered_permissions(),
                new_agent: None,
                modes: super::offered_modes(),
                skills: self.skills.iter().map(ListedSkill::offered).collect(),
            },
        );
    }

    /// Codex's skills changed: ask for the list again. While an ask is
    /// out, one more after its answer covers every change since it was
    /// sent.
    fn skills_changed(&mut self, emit: &mut Emit) {
        if self
            .requests
            .values()
            .any(|request| *request == Request::Skills)
        {
            self.skills_stale = true;
        } else {
            self.list_skills(emit);
        }
    }

    /// Asks for the skills, the same way each time.
    fn list_skills(&mut self, emit: &mut Emit) {
        self.skills_asked += 1;
        let id = match self.skills_asked {
            1 => "amux-skills".to_owned(),
            asked => format!("amux-skills-{asked}"),
        };
        let skills = self.request_as(
            id,
            ClientRequest::SkillsList(SkillsListParams::default()),
            Request::Skills,
        );
        emit.effect(Effect::ProviderWrite(skills));
    }

    /// Asks Codex which commands it runs in the background, the first page
    /// without a cursor.
    pub(super) fn list_jobs(
        &mut self,
        emit: &mut Emit,
        page: u32,
        cursor: Option<String>,
        listed: Vec<BackgroundJob>,
    ) {
        // A child's lists are named apart, so the agent's own keep the ids
        // they always had.
        let id = match self.speaking.clone() {
            None => {
                if page == 1 {
                    self.jobs_asked += 1;
                }
                format!("amux-jobs-{}-{page}", self.jobs_asked)
            }
            Some(child) => {
                if page == 1 {
                    let asked = self.child_request_id("child-jobs");
                    if let Some(child) = self.children.get_mut(&child) {
                        child.jobs_list = asked;
                    }
                }
                let list = self
                    .children
                    .get(&child)
                    .map(|child| child.jobs_list.clone())
                    .unwrap_or_default();
                format!("{list}-{page}")
            }
        };
        let bytes = self.request_as(
            id,
            ClientRequest::BackgroundTerminalsList(BackgroundTerminalsListParams {
                thread_id: self.thread_here(),
                cursor,
                extra: Default::default(),
            }),
            Request::Jobs {
                page,
                listed,
                child: self.speaking.clone(),
            },
        );
        emit.effect(Effect::ProviderWrite(bytes));
    }

    /// A page of Codex's background commands: each is a job of the
    /// command item it names. The last page publishes the list.
    pub(super) fn jobs_listed(
        &mut self,
        emit: &mut Emit,
        number: u32,
        mut listed: Vec<BackgroundJob>,
        page: Option<&Value>,
    ) {
        let Some(page) =
            page.and_then(|page| codex_protocol::result::<BackgroundTerminalsResponse>(page).ok())
        else {
            return;
        };
        let now = self.shared.now_ms();
        // A stop sent or refused stands across a new list of the same job.
        let held = self.jobs().jobs;
        listed.extend(page.data.iter().map(|terminal| {
            let before = held.iter().find(|job| job.key == terminal.process_id);
            BackgroundJob {
                item_key: terminal.item_id.clone(),
                command: terminal.command.clone(),
                started_at_ms: self
                    .works
                    .get(&terminal.item_id)
                    .map_or(now, |work| work.at_ms),
                kind: wire::BackgroundKind::Shell as i32,
                key: terminal.process_id.clone(),
                stopping: before.is_some_and(|job| job.stopping),
                stop_error: before.map(|job| job.stop_error.clone()).unwrap_or_default(),
            }
        }));
        match some_of(page.next_cursor.as_deref()) {
            Some(cursor) if !page.data.is_empty() => {
                self.list_jobs(emit, number + 1, Some(cursor), listed)
            }
            _ => self.set_jobs(listed),
        }
    }

    /// The person stops one background command by its process id: it
    /// reads stopping until its item completes.
    pub(super) fn stop_job(&mut self, emit: &mut Emit, id: &[u8], key: String) {
        let mut jobs = self.shared.jobs().jobs;
        let Some(job) = jobs
            .iter_mut()
            .find(|job| !key.is_empty() && job.key == key)
        else {
            return self.shared.reject(emit, id, wire::Refusal::Unsupported);
        };
        job.stopping = true;
        job.stop_error.clear();
        self.shared.set_jobs(jobs);
        let bytes = self.request_as(
            format!("amux-stop-{key}"),
            ClientRequest::BackgroundTerminalsTerminate(BackgroundTerminalsTerminateParams {
                thread_id: self.thread(),
                process_id: key.clone(),
                extra: Default::default(),
            }),
            Request::StopJob { key, child: None },
        );
        emit.effect(Effect::ProviderWrite(bytes));
        self.shared.accept(emit, id, false);
    }

    /// Codex did not stop the job keyed `key`, saying `error`.
    pub(super) fn stop_refused(&mut self, key: &str, error: String) {
        let mut jobs = self.jobs().jobs;
        if let Some(job) = jobs.iter_mut().find(|job| job.key == key) {
            job.stopping = false;
            job.stop_error = error;
            self.set_jobs(jobs);
        }
    }

    /// A command Codex ran in the background finished: it leaves the list
    /// of the thread being read.
    fn job_ended(&mut self, key: &str) {
        let jobs = self.jobs();
        if jobs.known && jobs.jobs.iter().any(|job| job.item_key == key) {
            let mut jobs = jobs.jobs;
            jobs.retain(|job| job.item_key != key);
            self.set_jobs(jobs);
        }
    }

    /// Asks once per server what it offers. Sent when the handshake's
    /// thread answer arrives: the agent process writes the handshake beside
    /// the interpreter until then, and the order of what reaches the server
    /// would depend on which wrote first.
    fn list_offers(&mut self, emit: &mut Emit) {
        if std::mem::replace(&mut self.offers_asked, true) {
            return;
        }
        self.list_models(emit, 1, None, Vec::new());
        self.list_skills(emit);
    }

    /// Gives the thread the agent's name, so Codex's own app shows the name
    /// amux does. Naming also has Codex write a thread that has run no turn
    /// to disk (codex-cli 0.161 onwards), which Codex's own app resumes
    /// from, so the app can attach before the first turn.
    pub(super) fn name_thread(&mut self, emit: &mut Emit) {
        if self.name.is_empty() {
            return;
        }
        let Some(name) = self.shared.thread_name(&self.name) else {
            return;
        };
        self.names_set += 1;
        let bytes = self.request_as(
            format!("amux-name-{}", self.names_set),
            ClientRequest::ThreadSetName(ThreadSetNameParams {
                thread_id: self.thread(),
                name,
                extra: Default::default(),
            }),
            Request::Name,
        );
        emit.effect(Effect::ProviderWrite(bytes));
    }

    /// Asks for one page of `model/list`, the first without a cursor.
    fn list_models(
        &mut self,
        emit: &mut Emit,
        page: u32,
        cursor: Option<String>,
        listed: Vec<OfferedModel>,
    ) {
        let params = ModelListParams {
            cursor,
            extra: Default::default(),
        };
        let bytes = self.request_as(
            format!("amux-models-{page}"),
            ClientRequest::ModelList(params),
            Request::Models { page, listed },
        );
        emit.effect(Effect::ProviderWrite(bytes));
    }

    /// The thread, from the handshake's response or `thread/started`.
    fn thread_started(
        &mut self,
        emit: &mut Emit,
        thread: &Thread,
        response: Option<&ThreadResponse>,
        fresh: bool,
    ) {
        let Some(id) = some(&thread.id) else {
            return;
        };
        if let Some(response) = response {
            if let Some(model) = some(&response.model) {
                self.launch_model.get_or_insert(model.clone());
                self.model = Some(model);
            }
            if let Some(policy) = response.approval_policy.as_ref().and_then(approval_name) {
                self.approval = Some(policy);
            }
            if let Some(sandbox) = response.sandbox.as_ref().and_then(sandbox_mode) {
                self.sandbox = Some(sandbox);
            }
            if let Some(reviewer) = &response.approvals_reviewer {
                self.reviewer = some(reviewer.as_str());
            }
            if let Some(collaboration) = &response.collaboration_mode {
                self.collaboration = some(collaboration.mode.as_str());
            }
            if let Some(effort) = effort_name(response.reasoning_effort.as_ref()) {
                self.effort = Some(effort);
            }
        }
        if let Some(known) = &self.thread_id {
            // The agent process resumed the thread it had, after a restart
            // or on its own; a thread started anywhere else is a child's.
            if response.is_some() && *known == id {
                self.shared.provider_started();
                self.boundary(
                    emit,
                    wire::BoundaryKind::Resumed,
                    wire::IncarnationEnd::default(),
                );
                self.release_held(emit);
            }
            return;
        }
        self.thread_id = Some(id);
        self.shared.provider_started();
        let kind = if some_of(thread.forked_from_id.as_deref()).is_some() {
            wire::BoundaryKind::Forked
        } else if !fresh && (self.incarnation > 1 || !thread.turns.is_empty()) {
            wire::BoundaryKind::Resumed
        } else {
            wire::BoundaryKind::Started
        };
        if !(kind == wire::BoundaryKind::Started && std::mem::take(&mut self.started_drawn)) {
            self.boundary(emit, kind, wire::IncarnationEnd::default());
        }
        self.release_held(emit);
    }

    // --- requests from the server ----------------------------------------

    /// A request from the server: an ask of the agent's own thread, or of
    /// a child thread's, which opens in that subagent's chat and points at
    /// its own work.
    fn server_request(
        &mut self,
        emit: &mut Emit,
        id: &RequestId,
        request: ServerRequest,
        payload: &[u8],
    ) {
        let thread = request.thread_id().to_owned();
        if !self.is_child(&thread) {
            return self.thread_request(emit, id, request, payload);
        }
        if self.child_heard(emit, &thread, true) {
            self.begin_child(emit, &thread);
            return self.as_child(&thread, |state| {
                state.thread_request(emit, id, request, payload)
            });
        }
        // A thread the agent left: nobody will answer here.
        let refusal = RpcError {
            code: -32600,
            message: "amux left this thread".to_owned(),
            extra: Default::default(),
        };
        self.respond(emit, &ask_key(id), Err(refusal));
    }

    fn thread_request(
        &mut self,
        emit: &mut Emit,
        id: &RequestId,
        request: ServerRequest,
        payload: &[u8],
    ) {
        let key = ask_key(id);
        let method = request.method();
        let thread = request.thread_id().to_owned();
        let (item_key, body, decisions) = match request {
            ServerRequest::CommandApproval(params) => self.command_approval(emit, params),
            ServerRequest::FileChangeApproval(params) => {
                let changes = match self
                    .works
                    .get(&params.item_id)
                    .and_then(|state| state.work.as_ref())
                    .and_then(|work| work.of.as_ref())
                {
                    Some(work::Of::FileChange(change)) => change.changes.clone(),
                    _ => Vec::new(),
                };
                (
                    params.item_id,
                    codex_ask::Body::FileChange(FileChangeApproval {
                        reason: params.reason.unwrap_or_default(),
                        grant_root: params.grant_root.unwrap_or_default(),
                        changes,
                    }),
                    default_decisions(),
                )
            }
            ServerRequest::PermissionsApproval(params) => {
                let files = params.permissions.file_system.flatten().unwrap_or_default();
                let network = params.permissions.network.flatten().unwrap_or_default();
                (
                    String::new(),
                    codex_ask::Body::Access(AccessGrant {
                        reason: params.reason.unwrap_or_default(),
                        read: files.read.unwrap_or_default(),
                        write: files.write.unwrap_or_default(),
                        network: network.enabled.unwrap_or(false),
                        network_hosts: Vec::new(),
                    }),
                    Vec::new(),
                )
            }
            ServerRequest::RequestUserInput(params) => {
                let mut shapes = Vec::new();
                let questions = params
                    .questions
                    .into_iter()
                    .map(|question| {
                        let options = question.options.unwrap_or_default();
                        shapes.push((
                            question.id,
                            options.iter().map(|option| option.label.clone()).collect(),
                        ));
                        Question {
                            header: question.header,
                            question: question.question,
                            multi_select: false,
                            options: options
                                .into_iter()
                                .map(|option| QuestionOption {
                                    recommended: option.label.trim_end().ends_with("(Recommended)"),
                                    label: option.label,
                                    description: option.description,
                                    preview: String::new(),
                                })
                                .collect(),
                            allow_other: question.is_other.unwrap_or(false),
                            secret: question.is_secret.unwrap_or(false),
                        }
                    })
                    .collect();
                return self.open(
                    emit,
                    CodexAsk {
                        key: key.clone(),
                        item_key: String::new(),
                        body: Some(codex_ask::Body::Question(QuestionAsk {
                            questions,
                            provider_dialog: false,
                        })),
                        decisions: Vec::new(),
                    },
                    AskMeta {
                        id: key,
                        method: method.to_owned(),
                        responses: Vec::new(),
                        questions: shapes,
                        at_ms: 0,
                        thread,
                    },
                );
            }
            ServerRequest::Elicitation(params) => {
                return self.elicitation(emit, key, method, params, payload);
            }
        };
        let (decisions, responses) = decisions
            .iter()
            .filter_map(offered_decision)
            .map(|(decision, response)| (decision as i32, response))
            .unzip();
        self.open(
            emit,
            CodexAsk {
                key: key.clone(),
                item_key,
                body: Some(body),
                decisions,
            },
            AskMeta {
                id: key,
                method: method.to_owned(),
                responses,
                questions: Vec::new(),
                at_ms: 0,
                thread,
            },
        );
    }

    /// A command approval: the command's work, if Codex has not reported
    /// it yet, and what may be answered.
    fn command_approval(
        &mut self,
        emit: &mut Emit,
        params: CommandApprovalParams,
    ) -> (String, codex_ask::Body, Vec<CommandDecision>) {
        let item_id = params.item_id;
        let command = shown_command(&params.command.unwrap_or_default());
        let cwd = params.cwd.unwrap_or_default();
        if !self.works.contains_key(&item_id) {
            let at_ms = params.started_at_ms.unwrap_or(self.shared.now_ms());
            self.works.insert(
                item_id.clone(),
                WorkState {
                    at_ms,
                    work: Some(Work {
                        of: Some(work::Of::Command(wire::CommandWork {
                            command: command.clone(),
                            cwd: cwd.clone(),
                            ..Default::default()
                        })),
                        state: ToolState::Pending as i32,
                        class: ToolClass::Consequential as i32,
                        ..Default::default()
                    }),
                    text: String::new(),
                    turn: params.turn_id,
                },
            );
            self.emit_work(emit, &item_id);
        }
        let mut network_hosts = Vec::new();
        if let Some(context) = params.network_approval_context {
            network_hosts.push(context.host);
        }
        let mut offered = params.available_decisions.unwrap_or_else(default_decisions);
        // Codex honours decline even when its offer leaves it out, and
        // a person must always be able to refuse a command.
        let decline = CommandDecision::Plain(Offered::Decline);
        if !offered.contains(&decline) {
            let at = offered
                .iter()
                .position(|decision| *decision == CommandDecision::Plain(Offered::Cancel))
                .unwrap_or(offered.len());
            offered.insert(at, decline);
        }
        for decision in &offered {
            if let CommandDecision::ApplyNetworkPolicyAmendment {
                apply_network_policy_amendment: choice,
            } = decision
            {
                network_hosts.push(choice.network_policy_amendment.host.clone());
            }
        }
        network_hosts.dedup();
        (
            item_id,
            codex_ask::Body::Command(CommandApproval {
                command,
                cwd,
                reason: params.reason.unwrap_or_default(),
                allow_prefix: params.proposed_execpolicy_amendment.unwrap_or_default(),
                network_hosts,
            }),
            offered,
        )
    }

    /// Opens an ask; one that is the work gets its own item, which the ask
    /// points at.
    fn open(&mut self, emit: &mut Emit, mut ask: CodexAsk, mut meta: AskMeta) {
        meta.at_ms = self.shared.now_ms();
        if work_ask(&ask).is_some() {
            ask.item_key = ask_item::key(&ask.key);
            self.emit_ask(emit, &ask, meta.at_ms, None);
        }
        self.asks.insert(ask.key.clone(), meta);
        self.open_ask(emit, ask);
    }

    /// The running call an elicitation of `server` belongs to, and its tool:
    /// that server's call of `tool` when Codex names it, else the server's
    /// newest, else, when no running call is that server's, the newest
    /// running call.
    fn running_tool_call(&self, server: &str, tool: &str) -> Option<(String, String, String)> {
        let running = |of_server: bool| {
            self.works
                .iter()
                .filter_map(|(key, state)| match state.work.as_ref() {
                    Some(Work {
                        of: Some(work::Of::Mcp(call)),
                        state: running,
                        ..
                    }) if *running == ToolState::Running as i32
                        && (!of_server
                            || (call.server == server
                                && (tool.is_empty() || call.tool == tool))) =>
                    {
                        Some((state.at_ms, key, call.server.clone(), call.tool.clone()))
                    }
                    _ => None,
                })
                .max_by_key(|(at_ms, ..)| *at_ms)
                .map(|(_, key, server, tool)| (key.clone(), server, tool))
        };
        running(true).or_else(|| running(false))
    }

    /// Whether `thread` is a child thread on the agent's server, not the
    /// agent's own.
    pub(super) fn is_child(&self, thread: &str) -> bool {
        !thread.is_empty() && self.thread_id.as_deref().is_some_and(|ours| ours != thread)
    }

    fn elicitation(
        &mut self,
        emit: &mut Emit,
        key: String,
        method: &str,
        params: ElicitationParams,
        payload: &[u8],
    ) {
        let thread = params.thread_id.clone();
        let meta = params.meta.unwrap_or_default();
        let server = params.server_name;
        let approval = meta.codex_approval_kind.as_deref() == Some("mcp_tool_call");
        let offers = |scope: &str| {
            meta.persist
                .as_ref()
                .is_some_and(|persist| persist.offers(scope))
        };
        let (session, always) = (offers("session"), offers("always"));
        if approval && server == AMUX_TOOL_SERVER {
            // amux's own tools never ask, as the Claude launch settings
            // pre-approve them: the call is approved at once, for the rest
            // of the session when Codex offers that.
            let accept = elicitation_response(
                ElicitationAction::Accept,
                no_content(),
                session.then_some("session"),
            );
            return self.respond(emit, &key, Ok(accept));
        }
        let message = params.message.unwrap_or_default();
        let named_tool = meta.tool_name.clone().unwrap_or_default();
        let (item_key, call_server, tool) = self
            .running_tool_call(&server, &named_tool)
            .unwrap_or_else(|| (String::new(), server.clone(), named_tool));
        let (body, offered): (_, Vec<(Decision, ClientResponse)>) = if approval {
            let mut offered = vec![(
                Decision::Approve,
                elicitation_response(ElicitationAction::Accept, no_content(), None),
            )];
            if session {
                offered.push((
                    Decision::ApproveSession,
                    elicitation_response(ElicitationAction::Accept, no_content(), Some("session")),
                ));
            }
            if always {
                offered.push((
                    Decision::ApproveAlways,
                    elicitation_response(ElicitationAction::Accept, no_content(), Some("always")),
                ));
            }
            offered.push((
                Decision::Deny,
                elicitation_response(ElicitationAction::Decline, None, None),
            ));
            offered.push((
                Decision::Abort,
                elicitation_response(ElicitationAction::Cancel, None, None),
            ));
            (
                codex_ask::Body::McpTool(McpToolApproval {
                    server: call_server,
                    tool,
                    arguments_json: compact_json(meta.tool_params.as_ref().unwrap_or(&Value::Null))
                        .into_bytes(),
                }),
                offered,
            )
        } else if params.mode == "url" {
            (
                codex_ask::Body::McpLink(LinkAsk {
                    server,
                    message,
                    url: params.url.unwrap_or_default(),
                }),
                Vec::new(),
            )
        } else {
            (
                codex_ask::Body::McpForm(FormAsk {
                    server,
                    message,
                    // The schema as the server wrote it: its key order is
                    // the order the form asks in, which decoding loses.
                    schema_json: json_as_written(payload, &["params", "requestedSchema"])
                        .unwrap_or_default(),
                }),
                Vec::new(),
            )
        };
        let (decisions, responses) = offered
            .into_iter()
            .map(|(decision, response)| (decision as i32, response_text(&response)))
            .unzip();
        self.open(
            emit,
            CodexAsk {
                key: key.clone(),
                item_key,
                body: Some(body),
                decisions,
            },
            AskMeta {
                id: key,
                method: method.to_owned(),
                responses,
                questions: Vec::new(),
                at_ms: 0,
                thread,
            },
        );
    }

    /// An ask the server resolved without an answer from here: withdrawn
    /// when amux interrupted its turn, else answered by another client. A
    /// child thread's is its subagent's.
    fn ask_resolved(&mut self, emit: &mut Emit, key: &str) {
        let child = self
            .asks
            .get(key)
            .map(|meta| meta.thread.clone())
            .filter(|thread| self.is_child(thread) && self.children.contains_key(thread));
        match child {
            Some(child) => self.as_child(&child, |state| state.thread_ask_resolved(emit, key)),
            None => self.thread_ask_resolved(emit, key),
        }
    }

    fn thread_ask_resolved(&mut self, emit: &mut Emit, key: &str) {
        let Some(ask) = self.close_ask(emit, key) else {
            return;
        };
        let meta = self.asks.remove(key);
        let withdrawn = match &self.speaking {
            None => self.interrupted_here,
            Some(child) => self.children.get(child).is_some_and(|child| child.stopping),
        };
        if withdrawn {
            self.dismiss(emit, &ask, meta);
        } else {
            self.answered_elsewhere(emit, &ask, meta);
        }
    }

    // --- notifications ---------------------------------------------------

    fn notification(&mut self, emit: &mut Emit, notification: ServerNotification) {
        // Codex says an ask was answered on whichever thread asked it.
        if let ServerNotification::ServerRequestResolved(resolved) = &notification {
            return self.ask_resolved(emit, &ask_key(&resolved.request_id));
        }
        if self.title_notified(emit, &notification) {
            return;
        }
        // A child thread's activity on the same server is its subagent's.
        if let (Some(ours), Some(theirs)) = (self.thread_id.as_deref(), about_thread(&notification))
            && ours != theirs
        {
            let theirs = theirs.to_owned();
            return self.child_notification(emit, &theirs, notification);
        }
        match notification {
            // Not the agent's name and not the title amux gave it: a person
            // renamed the thread in Codex's own interface.
            ServerNotification::ThreadNameUpdated(updated) => {
                if let Some(name) = updated.thread_name
                    && !self.name.is_empty()
                    && name != self.name
                    && Some(name.as_str()) != self.shared.title()
                {
                    self.shared.person_titled(&name);
                }
            }
            ServerNotification::ThreadStarted(started) => {
                self.thread_started(emit, &started.thread, None, false);
            }
            // The turn Codex runs an idle compaction in is nobody's: no
            // turn of the agent's starts or ends for it.
            ServerNotification::TurnStarted(started)
                if matches!(self.idle_compaction, Some(IdleCompaction::Requested { .. }))
                    && !self.shared.is_busy() =>
            {
                self.idle_compaction = Some(IdleCompaction::Running {
                    turn: started.turn.id.clone(),
                });
            }
            ServerNotification::TurnCompleted(completed)
                if matches!(
                    &self.idle_compaction,
                    Some(IdleCompaction::Running { turn }) if *turn == completed.turn.id
                ) =>
            {
                self.idle_compaction_over(emit);
            }
            ServerNotification::TurnStarted(started) => {
                self.plan_overtaken(emit);
                self.shared.turn_started();
                self.turn_prompted = false;
                self.interrupted_here = false;
                self.active_turn = some(&started.turn.id).or(self.active_turn.take());
                if std::mem::take(&mut self.interrupt_pending)
                    && let Some(turn) = self.active_turn.clone()
                {
                    self.request(
                        emit,
                        ClientRequest::TurnInterrupt(TurnInterruptParams {
                            thread_id: self.thread(),
                            turn_id: turn,
                            extra: Default::default(),
                        }),
                        Request::Interrupt,
                    );
                }
            }
            ServerNotification::TurnCompleted(completed) => {
                self.turn_completed(emit, &completed.turn);
            }
            notification @ (ServerNotification::ItemStarted(_)
            | ServerNotification::ItemCompleted(_)
            | ServerNotification::AgentMessageDelta(_)
            | ServerNotification::PlanDelta(_)
            | ServerNotification::ReasoningTextDelta(_)
            | ServerNotification::CommandOutputDelta(_)
            | ServerNotification::ReasoningSummaryPartAdded(_)
            | ServerNotification::ReasoningSummaryTextDelta(_)
            | ServerNotification::Error(_)
            | ServerNotification::AutoApprovalReviewStarted(_)
            | ServerNotification::AutoApprovalReviewCompleted(_)) => {
                self.thread_notification(emit, notification)
            }
            ServerNotification::TurnPlanUpdated(updated) => {
                self.plan = Some(
                    updated
                        .plan
                        .into_iter()
                        .map(|step| {
                            let status = match step.status.as_str() {
                                "completed" => TaskListStatus::Completed,
                                "inProgress" => TaskListStatus::InProgress,
                                _ => TaskListStatus::Pending,
                            };
                            (step.step, status as i32)
                        })
                        .collect(),
                );
            }
            ServerNotification::ThreadTokenUsageUpdated(updated) => {
                self.last_request_ms = Some(self.shared.now_ms());
                let usage = updated.token_usage;
                self.context_tokens = Some(usage.last.total_tokens as u64);
                if let Some(window) = usage.model_context_window {
                    self.context_window = Some(window as u64);
                }
            }
            // Codex reports each of its limits under its own id; the meter
            // is the `codex` limit's, as in Codex's own app, and an update
            // without an id is that one.
            ServerNotification::AccountRateLimitsUpdated(updated)
                if updated
                    .rate_limits
                    .limit_id
                    .as_deref()
                    .is_none_or(|id| id == "codex") =>
            {
                self.usage = Some(codex_usage(&updated.rate_limits));
            }
            ServerNotification::AccountRateLimitsUpdated(_) => {}
            ServerNotification::AccountUpdated(updated) => {
                self.sign_in = Some(
                    match some_of(updated.auth_mode.as_ref().map(|m| m.as_str())) {
                        None => SignIn {
                            state: SignInState::SignedOut as i32,
                            ..Default::default()
                        },
                        Some(mode) => SignIn {
                            state: SignInState::SignedIn as i32,
                            account: match some_of(updated.plan_type.as_ref().map(|p| p.as_str())) {
                                Some(plan) => format!("{mode} {plan}"),
                                None => mode,
                            },
                            message: String::new(),
                        },
                    },
                );
                if self.sign_in.as_ref().is_some_and(|sign_in| {
                    sign_in.state == SignInState::SignedIn as i32
                }) {
                    self.shared.signed_in();
                }
            }
            ServerNotification::AccountLoginCompleted(completed) => {
                if !completed.success {
                    self.sign_in = Some(SignIn {
                        state: SignInState::Failed as i32,
                        account: String::new(),
                        message: completed.error.unwrap_or_default(),
                    });
                }
            }
            ServerNotification::McpServerStatusUpdated(updated) => self.server_status(updated),
            ServerNotification::ThreadSettingsUpdated(updated) => {
                let settings = updated.thread_settings;
                if let Some(model) = some(&settings.model) {
                    self.model = Some(model);
                }
                if let Some(policy) = approval_name(&settings.approval_policy) {
                    self.approval = Some(policy);
                }
                if let Some(sandbox) = sandbox_mode(&settings.sandbox_policy) {
                    self.sandbox = Some(sandbox);
                }
                if let Some(reviewer) = &settings.approvals_reviewer {
                    self.reviewer = some(reviewer.as_str());
                }
                self.collaboration = some(settings.collaboration_mode.mode.as_str());
                self.effort = settings.effort.map(|effort| effort.as_str().to_owned());
                self.left_plan(emit);
            }
            ServerNotification::ServerRequestResolved(_) => {}
            // Nothing a client draws, or what another fact already covers.
            ServerNotification::ThreadStatusChanged(_)
            | ServerNotification::ThreadGoalCleared(_)
            | ServerNotification::RemoteControlStatusChanged(_)
            | ServerNotification::TerminalInteraction(_)
            | ServerNotification::TurnDiffUpdated(_) => {}
            ServerNotification::SkillsChanged(_) => self.skills_changed(emit),
            // Configuration Codex found and did not apply: the person
            // wants to know, in Codex's words.
            ServerNotification::ConfigWarning(warning) => {
                self.warning(emit, "configWarning", notice_words(warning))
            }
            // Codex's own app shows each of these as a warning line.
            // Codex will stop supporting something amux sends: news for
            // amux's developers, logged where a debug report finds it.
            // Codex's own app keeps it out of the chat too.
            ServerNotification::DeprecationNotice(notice) => emit.effect(Effect::Log {
                line: format!("Codex deprecation notice: {}", notice_words(notice)),
            }),
            ServerNotification::Warning(warning) => self.warning(emit, "warning", warning.message),
            // An approved review's warning repeats its verdict row, so
            // Codex's own app leaves it out too.
            ServerNotification::GuardianWarning(warning)
                if warning
                    .message
                    .starts_with("Automatic approval review approved (") => {}
            ServerNotification::GuardianWarning(warning) => {
                self.warning(emit, "guardianWarning", warning.message)
            }
            // Codex's own app shows only this check, in these words.
            ServerNotification::ModelVerification(verified) => {
                if verified
                    .verifications
                    .iter()
                    .any(|check| check == "trustedAccessForCyber")
                {
                    self.warning(emit, "modelVerification", CYBER_CHECKS.to_owned());
                }
            }
            ServerNotification::StrictReviewRequired(_) => self.warning(
                emit,
                "strictReviewRequired",
                "This request requires additional safety checks, some tool calls might take extra time"
                    .to_owned(),
            ),
            ServerNotification::SafetyBufferingUpdated(update) => {
                self.safety_check(emit, &update.turn_id, update.show_buffering_ui)
            }
            // Codex closes a thread only once no client is subscribed, so
            // the agent's own thread closing means it is gone; the next
            // incarnation resumes it.
            ServerNotification::ThreadClosed(_) => emit.effect(Effect::Exit {
                cause: "Codex closed the agent's thread".into(),
            }),
        }
    }

    /// What a thread's own conversation says: its items and their
    /// streams, its errors and the reviews of its actions. Read alike for
    /// the agent's own thread and, with its item state swapped in, a child
    /// thread's.
    pub(super) fn thread_notification(
        &mut self,
        emit: &mut Emit,
        notification: ServerNotification,
    ) {
        match notification {
            ServerNotification::ItemStarted(started) => self.item_event(
                emit,
                &started.item,
                &started.turn_id,
                started.started_at_ms,
                started.completed_at_ms,
                false,
            ),
            ServerNotification::ItemCompleted(completed) => self.item_event(
                emit,
                &completed.item,
                &completed.turn_id,
                completed.started_at_ms,
                completed.completed_at_ms,
                true,
            ),
            ServerNotification::AgentMessageDelta(delta)
            | ServerNotification::PlanDelta(delta)
            | ServerNotification::ReasoningTextDelta(delta) => {
                self.extend(emit, &delta.item_id, &delta.delta);
            }
            ServerNotification::CommandOutputDelta(delta) => {
                self.command_output(emit, &delta.item_id, &delta.delta);
            }
            ServerNotification::ReasoningSummaryPartAdded(part) => {
                self.summary_delta(emit, &part.item_id, part.summary_index, "");
            }
            ServerNotification::ReasoningSummaryTextDelta(delta) => {
                self.summary_delta(emit, &delta.item_id, delta.summary_index, &delta.delta);
            }
            ServerNotification::Error(error) => self.api_error(emit, error),
            ServerNotification::AutoApprovalReviewStarted(review) => {
                self.review(emit, review, false)
            }
            ServerNotification::AutoApprovalReviewCompleted(review) => {
                self.review(emit, review, true)
            }
            _ => {}
        }
    }

    /// A line Codex's own app prints for a command amux runs for it.
    fn review_line(&mut self, emit: &mut Emit, key: String, at_ms: Option<i64>, line: String) {
        self.emit_item(
            emit,
            ItemDraft {
                key,
                text: line,
                body: item_body(codex_item::Kind::CommandOutput(wire::SlashOutput {
                    command: "review".into(),
                    args: String::new(),
                })),
                at_ms,
                complete: true,
                ..Default::default()
            },
        );
    }

    /// Codex holding the turn's answer for a slower safety check: the turn's
    /// status while it runs.
    fn safety_check(&mut self, emit: &mut Emit, turn: &str, running: bool) {
        self.emit_item(
            emit,
            ItemDraft {
                key: format!("safety:{turn}"),
                text: "Our systems are thinking a bit more about this request before responding."
                    .into(),
                body: item_body(codex_item::Kind::SafetyCheck(wire::SafetyCheck { running })),
                complete: !running,
                ..Default::default()
            },
        );
    }

    /// A warning row in Codex's words, under the kind Codex sent it as.
    fn warning(&mut self, emit: &mut Emit, kind: &str, words: String) {
        let key = self.local_key("warning");
        self.emit_item(
            emit,
            ItemDraft {
                key,
                text: words.clone(),
                body: item_body(codex_item::Kind::Error(ApiError {
                    error_kind: kind.into(),
                    message: words,
                    ..Default::default()
                })),
                complete: true,
                ..Default::default()
            },
        );
    }

    /// A reasoning summary's part, started or extended.
    fn summary_delta(&mut self, emit: &mut Emit, key: &str, index: i64, delta: &str) {
        let index = index as usize;
        if let Some(Streamed::Reasoning(summary)) = self.streamed.get_mut(key) {
            if summary.len() <= index {
                summary.resize(index + 1, String::new());
            }
            summary[index].push_str(delta);
            if !delta.is_empty() {
                self.emit_reasoning(emit, key, None, false);
            }
        }
    }

    fn server_status(&mut self, updated: McpServerStatusUpdated) {
        let name = updated.name;
        let status = match updated.status.as_str() {
            "starting" => ToolServerStatus::Starting,
            "ready" => ToolServerStatus::Ready,
            "failed" | "cancelled" => ToolServerStatus::Failed,
            "needsAuth" | "notLoggedIn" => ToolServerStatus::NeedsAuth,
            _ => ToolServerStatus::Unspecified,
        };
        let error = some_of(updated.error.as_deref())
            .or_else(|| some_of(updated.failure_reason.as_deref()))
            .unwrap_or_default();
        let entry = ToolServer {
            name: name.clone(),
            status: status as i32,
            error,
        };
        match self.servers.iter_mut().find(|server| server.name == name) {
            Some(server) => *server = entry,
            None => self.servers.push(entry),
        }
    }

    fn api_error(&mut self, emit: &mut Emit, notification: ErrorNotification) {
        let error = notification.error;
        let will_retry = notification.will_retry;
        let (attempt, max_attempts) = if will_retry {
            attempts(&error.message)
        } else {
            (0, 0)
        };
        let message = error_message(&error);
        // A subagent's error is its own: the agent goes on.
        let own = self.speaking.is_none();
        if !will_retry && own {
            self.gave_up(&error);
        }
        let turn = notification.turn_id;
        self.error_item(
            emit,
            format!("error:{turn}"),
            ApiError {
                error_kind: error_kind(&error),
                message,
                will_retry,
                attempt,
                max_attempts,
                retry_at_ms: None,
            },
        );
        if own {
            self.final_error = (!will_retry).then_some(turn);
        }
    }

    fn review(&mut self, emit: &mut Emit, review: AutoApprovalReview, completed: bool) {
        let target = review.target_item_id.unwrap_or_default();
        let status = match &review.review.status {
            codex_protocol::server::ReviewStatus::InProgress => wire::ReviewStatus::InProgress,
            codex_protocol::server::ReviewStatus::Approved => wire::ReviewStatus::Approved,
            codex_protocol::server::ReviewStatus::Denied => wire::ReviewStatus::Denied,
            codex_protocol::server::ReviewStatus::TimedOut => wire::ReviewStatus::Timeout,
            codex_protocol::server::ReviewStatus::Aborted => wire::ReviewStatus::Abort,
            codex_protocol::server::ReviewStatus::Other(_) => wire::ReviewStatus::Unspecified,
        };
        self.emit_item(
            emit,
            ItemDraft {
                key: format!("review:{}", review.review_id),
                body: item_body(codex_item::Kind::Verdict(ReviewerVerdict {
                    status: status as i32,
                    status_word: review.review.status.as_str().to_owned(),
                    risk: review.review.risk_level.unwrap_or_default(),
                    rationale: review.review.rationale.unwrap_or_default(),
                    item_key: target.clone(),
                })),
                at_ms: review.started_at_ms,
                complete: true,
                ..Default::default()
            },
        );
        if !completed {
            return;
        }
        let outcome = match status {
            wire::ReviewStatus::Approved => DecisionOutcome::AutoApproved,
            wire::ReviewStatus::Denied => DecisionOutcome::Denied,
            _ => return,
        };
        let verdict = ToolDecision {
            outcome: outcome as i32,
            ..Default::default()
        };
        if self.works.contains_key(&target) {
            self.decide(emit, &target, verdict);
        } else {
            self.reviewed.insert(target, verdict);
        }
    }

    // --- items -----------------------------------------------------------

    fn item_event(
        &mut self,
        emit: &mut Emit,
        item: &ThreadItem,
        turn: &str,
        at_ms: Option<i64>,
        ended_at_ms: Option<i64>,
        completed: bool,
    ) {
        let id = item.id().to_owned();
        match item {
            ThreadItem::UserMessage(message) => {
                if completed {
                    self.user_message(emit, &id, message.client_id.as_deref(), &message.content);
                }
            }
            // A plan is the agent's own business with the person; a
            // subagent's reads as what it wrote.
            ThreadItem::Plan(plan) if self.speaking.is_some() => {
                self.streamed.insert(id.clone(), Streamed::Message);
                self.message_item(emit, &id, &plan.text, Streamed::Message, at_ms, completed);
            }
            ThreadItem::Plan(plan) => {
                self.streamed.insert(id.clone(), Streamed::Plan);
                let text = match self.open_item(emit, &id) {
                    Some(open) if !completed => open.text,
                    _ => plan.text.clone(),
                };
                let proposed = self.proposed.take().filter(|proposed| proposed.key == id);
                self.proposed = Some(Proposed {
                    key: id.clone(),
                    turn: turn.to_owned(),
                    at_ms: proposed
                        .as_ref()
                        .map(|proposed| proposed.at_ms)
                        .or(at_ms)
                        .unwrap_or_else(|| self.shared.now_ms()),
                    text,
                    complete: completed,
                    verdict: wire::PlanVerdict::Undecided as i32,
                    note: None,
                });
                if let Some(proposed) = &self.proposed {
                    self.shared.plan_written(&proposed.text);
                }
                self.emit_plan(emit);
                if completed {
                    self.streamed.remove(&id);
                    self.shared.note_message(&id);
                }
            }
            ThreadItem::AgentMessage(message) => {
                crate::notice::model_answered(&mut self.sign_in);
                let kind = self.streamed.get(&id).cloned().unwrap_or(
                    if message.phase == Some(MessagePhase::Commentary) {
                        Streamed::WorkingNote
                    } else {
                        Streamed::Message
                    },
                );
                self.streamed.insert(id.clone(), kind.clone());
                self.message_item(emit, &id, &message.text, kind, at_ms, completed);
            }
            ThreadItem::Reasoning(reasoning) => {
                let summary = reasoning.summary.clone().unwrap_or_default();
                let entry = self
                    .streamed
                    .entry(id.clone())
                    .or_insert_with(|| Streamed::Reasoning(Vec::new()));
                if completed || !summary.is_empty() {
                    *entry = Streamed::Reasoning(summary);
                }
                let content = reasoning.content.clone().unwrap_or_default().join("\n");
                self.emit_reasoning(emit, &id, completed.then_some(content), completed);
            }
            ThreadItem::ContextCompaction(_) => {
                if completed {
                    let idle = matches!(
                        &self.idle_compaction,
                        Some(IdleCompaction::Running { turn: running }) if running == turn
                    );
                    self.boundary_item(
                        emit,
                        wire::Boundary {
                            kind: wire::BoundaryKind::Compacted as i32,
                            idle,
                            ..Default::default()
                        },
                    );
                }
            }
            ThreadItem::CollabAgentToolCall(call) => {
                self.collab_call(emit, &id, call, completed);
                self.work_item(emit, &id, item, turn, at_ms, ended_at_ms, completed)
            }
            ThreadItem::CommandExecution(_)
            | ThreadItem::FileChange(_)
            | ThreadItem::McpToolCall(_)
            | ThreadItem::WebSearch(_)
            | ThreadItem::ImageView(_)
            | ThreadItem::ImageGeneration(_) => {
                self.work_item(emit, &id, item, turn, at_ms, ended_at_ms, completed)
            }
            // Codex's own app prints a review's start and end as lines of
            // their own; the review's findings arrive as an agent message.
            ThreadItem::EnteredReviewMode(review) => self.review_line(
                emit,
                id,
                at_ms,
                format!("Code review started: {}", review.review),
            ),
            ThreadItem::ExitedReviewMode(_) => {
                self.review_line(emit, id, at_ms, "Code review finished".to_owned())
            }
            ThreadItem::SubAgentActivity(activity) => {
                self.subagent_activity(emit, activity, turn, at_ms, ended_at_ms, completed)
            }
            // Read for what they carry elsewhere: a hook's prompt reaches the
            // conversation; a sleep or a raw call output shows nothing in
            // Codex's own app.
            ThreadItem::HookPrompt(_)
            | ThreadItem::Sleep(_)
            | ThreadItem::FunctionCallOutput(_) => {}
            other => {
                let other = other.kind().to_owned();
                self.emit_item(
                    emit,
                    ItemDraft {
                        key: id,
                        body: item_body(codex_item::Kind::Unrecognized(wire::Unrecognized {
                            fact_type: other,
                            summary: "an item amux does not read".into(),
                        })),
                        at_ms,
                        complete: true,
                        ..Default::default()
                    },
                );
            }
        }
    }

    /// A message the thread being read wrote: a working note or an
    /// answer, streaming or whole. The agent's own complete answer is its
    /// turn's newest message and the line its row quotes; a subagent's is
    /// the line its row quotes.
    fn message_item(
        &mut self,
        emit: &mut Emit,
        id: &str,
        whole: &str,
        kind: Streamed,
        at_ms: Option<i64>,
        completed: bool,
    ) {
        let complete = |complete| wire::Text { complete };
        let body = match kind {
            Streamed::WorkingNote => codex_item::Kind::WorkingNote(complete(completed)),
            _ => codex_item::Kind::Message(complete(completed)),
        };
        let (text, attachments) = match self.open_item(emit, id) {
            Some(open) if !completed => (open.text, Vec::new()),
            _ if completed => crate::shared::parse_reply(whole.to_owned()),
            _ => (whole.to_owned(), Vec::new()),
        };
        self.emit_item(
            emit,
            ItemDraft {
                key: id.to_owned(),
                text: text.clone(),
                attachments: attachments.clone(),
                body: item_body(body),
                at_ms,
                complete: completed,
                ..Default::default()
            },
        );
        if completed && kind == Streamed::Message {
            match self.speaking.clone() {
                None => {
                    self.shared.note_message(id);
                    self.shared.reply_said(emit, id);
                }
                Some(child) => self.child_said(&child, &text, &attachments),
            }
        }
    }

    fn emit_reasoning(
        &mut self,
        emit: &mut Emit,
        key: &str,
        full_text: Option<String>,
        complete: bool,
    ) {
        let Some(Streamed::Reasoning(summary)) = self.streamed.get(key).cloned() else {
            return;
        };
        let text = match full_text {
            Some(text) => text,
            None => self
                .open_item(emit, key)
                .map(|open| open.text)
                .unwrap_or_default(),
        };
        self.emit_item(
            emit,
            ItemDraft {
                key: key.to_owned(),
                text,
                body: item_body(codex_item::Kind::Reasoning(wire::Reasoning {
                    complete,
                    summary,
                })),
                complete,
                ..Default::default()
            },
        );
    }

    /// A prompt as Codex reflects it. One this interpreter sent is already
    /// an item; one typed into an attached terminal becomes one.
    /// A user message Codex reports: the echo of amux's own prompt or
    /// steer, matched by the client message id it was sent with, or another
    /// client's, drawn as amux's own would be. Without an id (a Codex that
    /// echoes none) order is the only evidence: the oldest prompt awaiting
    /// its reflection, then the oldest steer.
    fn user_message(
        &mut self,
        emit: &mut Emit,
        id: &str,
        client_id: Option<&str>,
        content: &[UserInput],
    ) {
        // A subagent's task comes from its parent's model, never a person:
        // drawn as the prompt it is, unless it is the echo of the prompt
        // its parent's call already drew in its chat.
        if let Some(thread) = self.speaking.clone() {
            let text = content
                .iter()
                .filter_map(|part| match part {
                    UserInput::Text(text) => Some(text.text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            if let Some(child) = self.children.get_mut(&thread)
                && child.prompt_drawn.as_deref() == Some(text.as_str())
            {
                child.prompt_drawn = None;
                return;
            }
            return self.emit_item(
                emit,
                ItemDraft {
                    key: id.to_owned(),
                    text,
                    body: item_body(codex_item::Kind::Prompt(wire::Prompt {})),
                    complete: true,
                    ..Default::default()
                },
            );
        }
        let first_in_turn = !std::mem::replace(&mut self.turn_prompted, true);
        let typed = content
            .iter()
            .filter_map(|input| match input {
                UserInput::Text(text) => Some(text.text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" ");
        self.ask_title(emit, &typed);
        let ours =
            |input_id: &[u8]| client_id.is_none_or(|sent| sent == client_message_id(input_id));
        let prompt = self
            .shared
            .awaiting_reflection()
            .iter()
            .find(|input_id| ours(input_id))
            .cloned();
        if let Some(prompt) = prompt {
            self.shared.reflect_prompt_id(&prompt);
            return;
        }
        if let Some(entry) = self.shared.steer_reflected(|entry| ours(&entry.input_id)) {
            self.emit_item(
                emit,
                ItemDraft {
                    key: format!("steer:{}", crate::serde_pb::to_hex(&entry.input_id)),
                    text: entry.text,
                    attachments: entry.attachments,
                    input_id: entry.input_id,
                    body: item_body(codex_item::Kind::Steer(wire::Steer {})),
                    complete: true,
                    ..Default::default()
                },
            );
            return;
        }
        let text = content
            .iter()
            .filter_map(|part| match part {
                UserInput::Text(text) => Some(text.text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        // Another client's: the prompt that started the turn, or a steer.
        let kind = if first_in_turn {
            codex_item::Kind::Prompt(wire::Prompt {})
        } else {
            codex_item::Kind::Steer(wire::Steer {})
        };
        self.emit_item(
            emit,
            ItemDraft {
                key: id.to_owned(),
                text,
                body: item_body(kind),
                complete: true,
                ..Default::default()
            },
        );
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn work_item(
        &mut self,
        emit: &mut Emit,
        id: &str,
        item: &ThreadItem,
        turn: &str,
        at_ms: Option<i64>,
        ended_at_ms: Option<i64>,
        completed: bool,
    ) {
        if let ThreadItem::McpToolCall(call) = item {
            // Called on a child's thread, it is the subagent saying what it
            // works on, not the agent.
            if is_status_tool(&call.server, &call.tool) {
                let arguments = compact_json(&call.arguments);
                if let Some(working_on) = status_working_on(arguments.as_bytes()) {
                    match self
                        .speaking
                        .clone()
                        .and_then(|child| self.children.get_mut(&child))
                    {
                        Some(child) => child.working_on = working_on,
                        None => self.shared.set_working_on(working_on),
                    }
                }
                return;
            }
            if is_send_tool(&call.server, &call.tool) {
                return self.sent_message(emit, id, call, at_ms, completed);
            }
        }
        let prior = self.works.get(id).cloned();
        if completed && prior.is_none() {
            self.shared.result_without_call("item/completed");
        }
        let status = match item {
            ThreadItem::CommandExecution(command) => command.status.as_str(),
            ThreadItem::FileChange(change) => change.status.as_str(),
            ThreadItem::McpToolCall(call) => call.status.as_str(),
            ThreadItem::CollabAgentToolCall(call) => call.status.as_str(),
            ThreadItem::ImageGeneration(image) => image.status.as_str(),
            // The spawn or follow-up returns at once; the subagent's own
            // state is its row's.
            ThreadItem::SubAgentActivity(_) => "",
            _ => "",
        };
        let mut state = match status {
            "" => ToolState::Succeeded,
            status => tool_state(status),
        };
        if !completed && state != ToolState::Running {
            state = ToolState::Running;
        }
        let (of, class) = match item {
            ThreadItem::CommandExecution(command) => {
                // A command that only looks takes the verb of its widest
                // action: a read among searches is a read.
                let actions = &command.command_actions;
                let looking = !actions.is_empty()
                    && actions.iter().all(|action| {
                        matches!(
                            action,
                            CommandAction::Read(_)
                                | CommandAction::Search(_)
                                | CommandAction::ListFiles(_)
                        )
                    });
                let class = if !looking {
                    ToolClass::Consequential
                } else if actions.iter().any(|a| matches!(a, CommandAction::Read(_))) {
                    ToolClass::Read
                } else if actions
                    .iter()
                    .any(|a| matches!(a, CommandAction::Search(_)))
                {
                    ToolClass::Search
                } else {
                    ToolClass::List
                };
                let exit_code = command.exit_code.map(|code| code as i32);
                if completed && state == ToolState::Succeeded && exit_code.is_some_and(|c| c != 0) {
                    state = ToolState::Failed;
                }
                (
                    work::Of::Command(wire::CommandWork {
                        command: shown_command(&command.command),
                        cwd: command.cwd.clone(),
                        exit_code,
                        background: false,
                        output_dropped_bytes: 0,
                    }),
                    class,
                )
            }
            ThreadItem::FileChange(change) => (
                work::Of::FileChange(wire::FileChangeWork {
                    changes: file_changes(&change.changes, state == ToolState::Succeeded),
                }),
                ToolClass::Consequential,
            ),
            ThreadItem::McpToolCall(call) => (
                work::Of::Mcp(mcp_work(call)),
                if call.read_only_hint == Some(true) {
                    ToolClass::Look
                } else {
                    ToolClass::Consequential
                },
            ),
            ThreadItem::WebSearch(search) => (
                work::Of::WebSearch(web_search(search)),
                ToolClass::WebSearch,
            ),
            ThreadItem::ImageView(image) => (
                work::Of::Image(wire::ImageWork {
                    generated: false,
                    path: image.path.clone(),
                }),
                ToolClass::Look,
            ),
            ThreadItem::ImageGeneration(image) => (
                work::Of::Image(wire::ImageWork {
                    generated: true,
                    path: image.saved_path.clone().unwrap_or_default(),
                }),
                ToolClass::Look,
            ),
            ThreadItem::CollabAgentToolCall(call) => (
                work::Of::Collab(wire::CollabWork {
                    tool: call.tool.as_str().to_owned(),
                    thread_ids: call.receiver_thread_ids.clone(),
                    prompt: call.prompt.clone().unwrap_or_default(),
                    agent_ids: call
                        .receiver_thread_ids
                        .iter()
                        .map(|thread| self.shared.subagent_id(thread))
                        .collect(),
                }),
                ToolClass::Consequential,
            ),
            // Codex 0.166.0's spawn and follow-up: the call row that opens
            // the subagent's chat.
            ThreadItem::SubAgentActivity(activity) => (
                work::Of::Collab(wire::CollabWork {
                    tool: match activity.kind {
                        SubAgentActivityKind::Interacted => "sendInput",
                        _ => "spawnAgent",
                    }
                    .to_owned(),
                    thread_ids: vec![activity.agent_thread_id.clone()],
                    prompt: String::new(),
                    agent_ids: vec![self.shared.subagent_id(&activity.agent_thread_id)],
                }),
                ToolClass::Consequential,
            ),
            _ => return,
        };
        let prior_work = prior.as_ref().and_then(|prior| prior.work.clone());
        let mut decision = prior_work
            .as_ref()
            .and_then(|work| work.decision.clone())
            .or_else(|| self.reviewed.remove(id));
        // An approval answered by another client: how the work ended says
        // what was decided.
        if let Some(decision) = &mut decision
            && decision.elsewhere
            && decision.outcome == DecisionOutcome::Unknown as i32
            && completed
        {
            decision.outcome = if state == ToolState::Denied {
                DecisionOutcome::Denied
            } else {
                DecisionOutcome::Allowed
            } as i32;
        }
        let (background, dropped) = prior_work
            .as_ref()
            .and_then(|work| match &work.of {
                Some(work::Of::Command(command)) => {
                    Some((command.background, command.output_dropped_bytes))
                }
                _ => None,
            })
            .unwrap_or((false, 0));
        let mut of = match of {
            work::Of::Command(mut command) => {
                command.background = background;
                command.output_dropped_bytes = dropped;
                work::Of::Command(command)
            }
            of => of,
        };
        let started = prior.as_ref().map(|prior| prior.at_ms);
        let mut text = match (item, completed) {
            (ThreadItem::CommandExecution(command), true) => {
                match some_of(command.aggregated_output.as_deref()) {
                    // The whole output: what is kept of it is counted anew.
                    Some(output) => {
                        if let work::Of::Command(command) = &mut of {
                            command.output_dropped_bytes = 0;
                        }
                        output
                    }
                    None => prior
                        .as_ref()
                        .map(|prior| prior.text.clone())
                        .unwrap_or_default(),
                }
            }
            // A page opened or searched within is named by its address,
            // which is all Codex says of it.
            (ThreadItem::WebSearch(_), _) => match &of {
                work::Of::WebSearch(web) => web.url.clone(),
                _ => String::new(),
            },
            _ => prior.as_ref().map(|p| p.text.clone()).unwrap_or_default(),
        };
        if let work::Of::Command(command) = &mut of {
            let cut = crate::shared::output_cut(&text);
            text.drain(..cut);
            command.output_dropped_bytes += cut as u64;
        }
        let duration_ms = match item {
            ThreadItem::CommandExecution(command) => command.duration_ms,
            ThreadItem::McpToolCall(call) => call.duration_ms,
            _ => None,
        };
        let at_ms = started.or(at_ms).unwrap_or(self.shared.now_ms());
        let ended_at_ms = if completed {
            ended_at_ms
                .or_else(|| duration_ms.map(|duration| at_ms + duration))
                .or(Some(self.shared.now_ms()))
        } else {
            None
        };
        // Codex announces a call before any approval it waits for, and its
        // duration is the run alone: the run began that long before the end.
        let started_at_ms = ended_at_ms
            .zip(duration_ms)
            .map(|(ended, duration)| (ended - duration).max(at_ms));
        self.works.insert(
            id.to_owned(),
            WorkState {
                at_ms,
                work: Some(Work {
                    of: Some(of),
                    state: state as i32,
                    class: class as i32,
                    decision,
                    ended_at_ms,
                    started_at_ms,
                }),
                text,
                turn: prior.map_or_else(|| turn.to_owned(), |prior| prior.turn),
            },
        );
        if completed {
            if let Some(background) = &mut self.background {
                background.remove(id);
            }
            self.job_ended(id);
        }
        self.emit_work(emit, id);
    }

    /// A running command's new output: an append, or, past twice the cap,
    /// the command again whole with only its newest output and the bytes
    /// dropped counted in its body.
    fn command_output(&mut self, emit: &mut Emit, id: &str, delta: &str) {
        if delta.is_empty() {
            return;
        }
        if !self.works.contains_key(id) {
            return self.extend(emit, id, delta);
        }
        let output = self.append_output(emit, id, delta);
        let Some(state) = self.works.get_mut(id) else {
            return;
        };
        state.text.push_str(delta);
        match output {
            Output::Appended => {}
            Output::Cut { dropped } => {
                state.text.drain(..dropped as usize);
                if let Some(Work {
                    of: Some(work::Of::Command(command)),
                    ..
                }) = &mut state.work
                {
                    command.output_dropped_bytes += dropped;
                }
                self.emit_work(emit, id);
            }
            // Nothing shows output after its command ended; the state only
            // stays bounded.
            Output::NotOpen => {
                let cut = crate::shared::output_cut(&state.text);
                state.text.drain(..cut);
            }
        }
    }

    /// amux's send tool, drawn as the message it sent. Started and
    /// completed each emit the whole item on the call's key.
    fn sent_message(
        &mut self,
        emit: &mut Emit,
        id: &str,
        call: &McpToolCallItem,
        at_ms: Option<i64>,
        completed: bool,
    ) {
        let arguments = compact_json(&call.arguments);
        let returned = call
            .result
            .as_ref()
            .map(|result| result.texts().collect::<Vec<_>>().join("\n"))
            .unwrap_or_default();
        let error = call
            .error
            .as_ref()
            .map(|error| error.message.clone())
            .unwrap_or_default();
        let outcome = match (completed, tool_state(call.status.as_str())) {
            (false, _) => SendOutcome::Running,
            (true, ToolState::Succeeded) => SendOutcome::Returned(&returned),
            (true, _) if error.is_empty() => SendOutcome::Failed(&returned),
            (true, _) => SendOutcome::Failed(&error),
        };
        let (message_text, message) = sent_message(arguments.as_bytes(), outcome);
        // The message's files go as attachments, not their elements.
        let (message_text, attachments) = crate::shared::parse_reply(message_text);
        let at_ms = self
            .sent
            .entry(id.to_owned())
            .or_insert_with(|| at_ms.unwrap_or_else(|| self.shared.now_ms()));
        let at_ms = *at_ms;
        self.emit_item(
            emit,
            ItemDraft {
                key: id.to_owned(),
                text: message_text,
                attachments,
                body: item_body(codex_item::Kind::AgentMessage(message)),
                at_ms: Some(at_ms),
                complete: true,
                ..Default::default()
            },
        );
    }

    // --- turn end and exit -----------------------------------------------

    /// The answer to a `turn/start` or `review/start` names the turn it
    /// began; it records that turn unless Codex already named one running or
    /// the turn has already ended.
    fn turn_answered(&mut self, result: &Value) {
        if self.active_turn.is_some() {
            return;
        }
        let turn = codex_protocol::result::<TurnStartResponse>(result)
            .ok()
            .and_then(|response| some(&response.turn.id));
        if turn != self.ended_turn {
            self.active_turn = turn;
        }
    }

    fn turn_completed(&mut self, emit: &mut Emit, turn: &codex_protocol::Turn) {
        let outcome = match turn.status.as_str() {
            "completed" => TurnOutcome::Completed,
            "interrupted" => TurnOutcome::Interrupted,
            "failed" => TurnOutcome::Failed,
            _ => TurnOutcome::Unspecified,
        };
        let turn_id = turn.id.clone();
        if let Some(error) = &turn.error
            && self.final_error.as_deref() != Some(turn_id.as_str())
        {
            self.gave_up(error);
            self.error_item(
                emit,
                format!("error:{turn_id}"),
                ApiError {
                    error_kind: error_kind(error),
                    message: error_message(error),
                    ..Default::default()
                },
            );
        }
        self.final_error = None;
        self.begin_children(emit);
        // Work whose approval was still open never ran.
        let mut unapproved = BTreeSet::new();
        for ask in self.shared.close_all_asks() {
            if let Some(codex_ask::Body::Plan(_)) = ask.body {
                self.decide_plan(emit, wire::PlanVerdict::Dismissed, None);
                continue;
            }
            unapproved.insert(ask.item_key.clone());
            let meta = self.asks.remove(&ask.key);
            self.dismiss(emit, &ask, meta);
        }
        // Codex's interrupt ends the turn but not the commands it runs as
        // background terminals: they run on, as after a completed turn.
        self.settle_open(
            emit,
            matches!(outcome, TurnOutcome::Completed | TurnOutcome::Interrupted),
            &unapproved,
        );
        // Codex lists what carries on in the background; with nothing
        // still running there is nothing to ask.
        if self.background.as_ref().is_some_and(|set| !set.is_empty()) {
            self.list_jobs(emit, 1, None, Vec::new());
        } else {
            self.shared.set_jobs(Vec::new());
        }
        // A steer lives in the turn it was sent into. One Codex has not
        // answered yet reached it after the turn ended and will be refused
        // (Codex answers a steer it took before announcing the turn's end),
        // so it waits in the queue again.
        let unanswered = self
            .requests
            .values()
            .filter_map(|request| match request {
                Request::Steer { input_id } => Some(input_id.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        for input_id in &unanswered {
            self.shared.steer_refused(input_id);
        }
        self.shared.steers_lost();
        for request in self.requests.values_mut() {
            if let Request::Inject { turn_over, .. } = request {
                *turn_over = true;
            }
        }
        let at_ms = self.shared.now_ms();
        let duration = turn.duration_ms;
        if let Some(ended) = self.shared.turn_ended(emit, outcome) {
            let started_at_ms = duration.map_or(ended.started_at_ms, |duration| at_ms - duration);
            self.emit_item(
                emit,
                ItemDraft {
                    key: format!("turn:{}", ended.id),
                    body: item_body(codex_item::Kind::Turn(Turn {
                        turn_id: ended.id,
                        outcome: outcome as i32,
                        started_at_ms,
                    })),
                    at_ms: Some(at_ms),
                    complete: true,
                    ..Default::default()
                },
            );
        }
        self.ended_turn = some(&turn_id).or(self.active_turn.take());
        self.active_turn = None;
        self.interrupt_pending = false;
        if outcome == TurnOutcome::Completed {
            self.open_plan_ask(&turn_id);
        }
    }

    /// At turn end nothing streams any more. A command still running
    /// carries on in the background when the turn completed or was
    /// interrupted (a command Codex did end reports its own end), else it
    /// was cut short, as is one still waiting for its approval
    /// (`unapproved`, by item key), which never ran; any other open item
    /// is finished as it stands.
    pub(super) fn settle_open(
        &mut self,
        emit: &mut Emit,
        completed: bool,
        unapproved: &BTreeSet<String>,
    ) {
        let open = self
            .works
            .iter()
            .filter(|(_, state)| state.work.as_ref().is_some_and(|work| !work_complete(work)))
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        for key in open {
            let Some(work) = self
                .works
                .get_mut(&key)
                .and_then(|state| state.work.as_mut())
            else {
                continue;
            };
            match (&mut work.of, completed) {
                (Some(work::Of::Command(command)), true)
                    if work.state == ToolState::Running as i32 && !unapproved.contains(&key) =>
                {
                    if command.background {
                        continue;
                    }
                    command.background = true;
                    self.background.get_or_insert_default().insert(key.clone());
                }
                _ => work.state = ToolState::Cancelled as i32,
            }
            self.emit_work(emit, &key);
        }
        self.works.retain(|key, state| {
            state.work.as_ref().is_some_and(|work| !work_complete(work))
                || self
                    .background
                    .as_ref()
                    .is_some_and(|set| set.contains(key))
        });
        for (key, kind) in std::mem::take(&mut self.streamed) {
            let Some(open) = self.open_item(emit, &key) else {
                continue;
            };
            let (body, (text, attachments)) = match kind {
                Streamed::Plan => {
                    if let Some(proposed) = self.proposed.as_mut().filter(|plan| plan.key == key) {
                        proposed.text = open.text;
                        proposed.complete = true;
                    }
                    self.emit_plan(emit);
                    continue;
                }
                Streamed::Message => (
                    codex_item::Kind::Message(wire::Text { complete: true }),
                    crate::shared::parse_reply(open.text),
                ),
                Streamed::WorkingNote => (
                    codex_item::Kind::WorkingNote(wire::Text { complete: true }),
                    crate::shared::parse_reply(open.text),
                ),
                Streamed::Reasoning(summary) => (
                    codex_item::Kind::Reasoning(wire::Reasoning {
                        complete: true,
                        summary,
                    }),
                    (open.text, Vec::new()),
                ),
            };
            self.emit_item(
                emit,
                ItemDraft {
                    key,
                    text,
                    attachments,
                    body: item_body(body),
                    at_ms: Some(open.at_ms),
                    complete: true,
                    ..Default::default()
                },
            );
        }
    }

    pub(super) fn exited(&mut self, emit: &mut Emit, exit: wire::IncarnationEnd) {
        emit.end_incarnation(exit.clone());
        // The subagents went with the process that ran them.
        self.end_children(emit, &exit);
        self.idle_compaction = None;
        self.offers_asked = false;
        // The next server is asked afresh; this one will answer nothing.
        self.skills_asked = 0;
        self.skills_stale = false;
        self.requests
            .retain(|_, request| *request != Request::Skills);
        for ask in self.shared.close_all_asks() {
            if let Some(codex_ask::Body::Plan(_)) = ask.body {
                self.decide_plan(emit, wire::PlanVerdict::Dismissed, None);
            }
            if let Some(meta) = self.asks.remove(&ask.key) {
                self.emit_ask(emit, &ask, meta.at_ms, Some(ask_item::dismissed()));
            }
        }
        self.settle_open(emit, false, &BTreeSet::new());
        // No answer follows a safety check the server died holding.
        for key in self.shared.open_keys("safety:") {
            if let Some(id) = key.strip_prefix("safety:") {
                self.safety_check(emit, id, false);
            }
        }
        self.background = None;
        self.shared.provider_exited();
        self.active_turn = None;
        self.interrupt_pending = false;
        self.boundary(emit, wire::BoundaryKind::Exited, exit);
    }
}

/// Codex's limits as it reports them: each window named from its length,
/// the only description Codex gives, and blocked when fully used. Codex
/// says a limit was reached without saying which, so that alone blocks
/// the whole.
fn codex_usage(limits: &RateLimitSnapshot) -> CodexUsage {
    let windows: Vec<CodexUsageWindow> = [&limits.primary, &limits.secondary]
        .into_iter()
        .flatten()
        .map(|window| {
            let minutes = window.window_duration_mins.unwrap_or(0).max(0) as u32;
            let used = window.used_percent as f64;
            CodexUsageWindow {
                limit: match minutes {
                    300 => Limit::FiveHour,
                    10_080 => Limit::Weekly,
                    _ => Limit::Unspecified,
                } as i32,
                window_minutes: minutes,
                meter: Some(UsageMeter {
                    used_percent: used,
                    resets_at_ms: window.resets_at.map(|at| at * 1000),
                    state: window_state(used) as i32,
                    used_unknown: false,
                }),
            }
        })
        .collect();
    let worst = windows
        .iter()
        .filter_map(|window| window.meter.as_ref())
        .map(|meter| meter.state())
        .max_by_key(|state| *state as i32)
        .unwrap_or(UsageState::Ok);
    let state = if limits.rate_limit_reached_type.is_some() {
        UsageState::Blocked
    } else {
        worst
    };
    CodexUsage {
        state: state as i32,
        windows,
        credits: limits.credits.as_ref().and_then(|credits| {
            if credits.unlimited {
                Some("unlimited".to_owned())
            } else {
                some_of(credits.balance.as_deref())
            }
        }),
    }
}

/// A window's state from its use alone: blocked when full, near from 80%.
fn window_state(used_percent: f64) -> UsageState {
    if used_percent >= 100.0 {
        UsageState::Blocked
    } else if used_percent >= 80.0 {
        UsageState::NearLimit
    } else {
        UsageState::Ok
    }
}

/// A tool-server call's work.
fn mcp_work(call: &McpToolCallItem) -> wire::McpToolCall {
    wire::McpToolCall {
        server: call.server.clone(),
        tool: call.tool.clone(),
        arguments_json: compact_json(&call.arguments).into_bytes(),
        result_json: compact_json(&as_written(&call.result)).into_bytes(),
        error: call
            .error
            .as_ref()
            .map(|error| error.message.clone())
            .unwrap_or_default(),
        subject: crate::shared::first_string(&call.arguments),
        child_agent_id: Vec::new(),
    }
}

/// A decoded part of a message as the JSON Codex wrote.
fn as_written(value: &impl serde::Serialize) -> Value {
    serde_json::to_value(value).expect("protocol types serialize")
}

/// A notice's summary, then its details on their own lines when it has any.
fn notice_words(notice: DeprecationNotice) -> String {
    match some_of(notice.details.as_deref()) {
        Some(details) => format!("{}\n{details}", notice.summary),
        None => notice.summary,
    }
}

/// Codex's words for its cybersecurity checks, as its own app shows them.
const CYBER_CHECKS: &str = "Your conversations have multiple flags for possible cybersecurity risk. Responses may take longer because extra safety checks are on. To get authorized for security work, join the Trusted Access for Cyber program: https://chatgpt.com/cyber";

/// A patch's changes as the wire carries them; the first changed line only
/// once the patch applied.
fn file_changes(changes: &[FileUpdateChange], applied: bool) -> Vec<wire::FileChange> {
    changes
        .iter()
        .map(|change| wire::FileChange {
            path: change.path.clone(),
            kind: match &change.kind {
                PatchChangeKind::Add(_) => wire::FileChangeKind::Add,
                PatchChangeKind::Delete(_) => wire::FileChangeKind::Delete,
                PatchChangeKind::Update(_) => wire::FileChangeKind::Update,
                PatchChangeKind::Unknown(_) => wire::FileChangeKind::Unspecified,
            } as i32,
            move_to: match &change.kind {
                PatchChangeKind::Update(update) => update.move_path.clone().unwrap_or_default(),
                _ => String::new(),
            },
            patch: change.diff.clone(),
            line: match &change.kind {
                PatchChangeKind::Update(_) if applied => patch_first_change(&change.diff),
                _ => None,
            },
        })
        .collect()
}
