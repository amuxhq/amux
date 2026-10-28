//! What the app server writes: responses, requests and notifications.

use std::collections::BTreeSet;

use codex_protocol::client::{
    BackgroundTerminalsListParams, CommandApprovalResponse, ElicitationAction, ModelListParams,
    SkillsListParams, ThreadIdParams, ThreadReadParams, ThreadResumeParams, ThreadSetNameParams,
    ToolCallResponse, TurnInterruptParams,
};
use codex_protocol::items::{
    CommandAction, DynamicToolCallItem, FileUpdateChange, McpToolCallItem, MessagePhase,
    PatchChangeKind, TextContent, ThreadItem, ToolOutputContent, UserInput, WebSearchAction,
    WebSearchItem,
};
use codex_protocol::server::{
    AccountReadResponse, AutoApprovalReview, BackgroundTerminalsResponse, CommandApprovalParams,
    CommandDecision, Decision as Offered, DeprecationNotice, ElicitationParams, ErrorNotification,
    McpServerStatusUpdated, Model, ModelListResponse, SkillsListResponse, ThreadResponse,
    TurnStartResponse,
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
    TaskListStatus, ToolClass, ToolDecision, ToolServer, ToolServerHealth, ToolServerStatus,
    ToolState, Turn, TurnOutcome, UsageMeter, UsageState, Work, codex_ask, codex_item, work,
};

use super::{
    AskMeta, DROPPED_NOTIFICATIONS, FRESH_THREAD_REQUEST, InjectConsumption, ListedSkill, Proposed,
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
fn error_kind(error: &TurnError) -> String {
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
fn error_message(error: &TurnError) -> String {
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
                    data: None,
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
                    data: None,
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

    fn error_item(&mut self, emit: &mut Emit, key: String, error: ApiError) {
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
    fn thread_answered(
        &mut self,
        emit: &mut Emit,
        result: &Value,
        resumed: bool,
        fresh: bool,
    ) -> bool {
        let Ok(response) = codex_protocol::result::<ThreadResponse>(result) else {
            return false;
        };
        if fresh {
            self.thread_id = None;
        }
        self.thread_started(emit, &response.thread, Some(&response), fresh);
        if self.thread_id.as_ref() == Some(&response.thread.id) {
            self.name_thread(emit);
            // A resume leaves the turns out, so an empty list says nothing
            // about a resumed thread, which Codex has already written.
            if !resumed && response.thread.turns.is_empty() {
                self.persist_thread(emit);
            }
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
            if !self.thread_answered(emit, &result, resumed, false) {
                self.account_answered(&result);
            }
            return;
        };
        match request {
            Request::Unarchive | Request::Resume | Request::Fresh => {
                return match result {
                    Ok(_) if request == Request::Unarchive => self.resume(emit),
                    Ok(result) => {
                        let fresh = request == Request::Fresh;
                        self.thread_answered(emit, &result, !fresh, fresh);
                    }
                    Err(error) => self.refused_to_start(emit, &error),
                };
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
        if let Request::Jobs { page, listed } = request {
            return self.jobs_listed(emit, page, listed, result.ok().as_ref());
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
                    Request::Steer { .. } => {}
                    Request::Inject { envelope_id, .. } => {
                        self.shared.message_consumed(&envelope_id);
                    }
                    Request::Interrupt
                    | Request::Models { .. }
                    | Request::Name
                    | Request::Persist
                    | Request::Skills
                    | Request::Jobs { .. }
                    | Request::Unarchive
                    | Request::Resume
                    | Request::Fresh
                    | Request::Unsubscribe => {}
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
                if during_turn && self.consumption == InjectConsumption::DrainedMidTurn {
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
            | Request::Models { .. }
            | Request::Name
            | Request::Persist
            | Request::Skills
            | Request::Jobs { .. }
            | Request::Unarchive
            | Request::Resume
            | Request::Fresh
            | Request::Unsubscribe => {}
        }
    }

    /// The child threads the agent's thread spawned, from its subagent
    /// calls.
    fn spawned_threads(&self) -> Vec<String> {
        let mut threads: Vec<String> = self
            .works
            .values()
            .filter_map(|state| match state.work.as_ref()?.of.as_ref()? {
                work::Of::Collab(collab) => Some(collab.thread_ids.clone()),
                _ => None,
            })
            .flatten()
            .collect();
        threads.sort();
        threads.dedup();
        threads
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
        // spawned, as Codex's own app does when it switches threads.
        let left: Vec<String> = self
            .thread_id
            .take()
            .into_iter()
            .chain(self.spawned_threads())
            .collect();
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
        self.persist_thread(emit);
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
    fn list_jobs(
        &mut self,
        emit: &mut Emit,
        page: u32,
        cursor: Option<String>,
        listed: Vec<BackgroundJob>,
    ) {
        if page == 1 {
            self.jobs_asked += 1;
        }
        let bytes = self.request_as(
            format!("amux-jobs-{}-{page}", self.jobs_asked),
            ClientRequest::BackgroundTerminalsList(BackgroundTerminalsListParams {
                thread_id: self.thread(),
                cursor,
                limit: None,
                extra: Default::default(),
            }),
            Request::Jobs { page, listed },
        );
        emit.effect(Effect::ProviderWrite(bytes));
    }

    /// A page of Codex's background commands: each is a job of the
    /// command item it names. The last page publishes the list.
    fn jobs_listed(
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
        listed.extend(page.data.iter().map(|terminal| {
            BackgroundJob {
                item_key: terminal.item_id.clone(),
                command: terminal.command.clone(),
                started_at_ms: self
                    .works
                    .get(&terminal.item_id)
                    .map_or(now, |work| work.at_ms),
                kind: wire::BackgroundKind::Shell as i32,
                ..Default::default()
            }
        }));
        match some_of(page.next_cursor.as_deref()) {
            Some(cursor) if !page.data.is_empty() => {
                self.list_jobs(emit, number + 1, Some(cursor), listed)
            }
            _ => self.shared.set_jobs(listed),
        }
    }

    /// A command Codex ran in the background finished: it leaves the list.
    fn job_ended(&mut self, key: &str) {
        let jobs = self.shared.jobs();
        if jobs.known && jobs.jobs.iter().any(|job| job.item_key == key) {
            let mut jobs = jobs.jobs;
            jobs.retain(|job| job.item_key != key);
            self.shared.set_jobs(jobs);
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
    /// amux does. Naming a thread that has not run a turn lets another
    /// client resume it plainly, but not Codex's own app, which resumes
    /// from history on disk; `persist_thread` is what lets the app attach.
    pub(super) fn name_thread(&mut self, emit: &mut Emit) {
        if self.name.is_empty() {
            return;
        }
        self.names_set += 1;
        let bytes = self.request_as(
            format!("amux-name-{}", self.names_set),
            ClientRequest::ThreadSetName(ThreadSetNameParams {
                thread_id: self.thread(),
                name: self.name.clone(),
                extra: Default::default(),
            }),
            Request::Name,
        );
        emit.effect(Effect::ProviderWrite(bytes));
    }

    /// Has Codex write a thread that has run no turn to disk. Codex's own
    /// app resumes a thread from its history on disk, which Codex writes at
    /// the first turn, so until then the app cannot attach; reading the
    /// loaded thread with its turns makes Codex write it.
    fn persist_thread(&mut self, emit: &mut Emit) {
        let bytes = self.request_as(
            "amux-persist".into(),
            ClientRequest::ThreadRead(ThreadReadParams {
                thread_id: self.thread(),
                include_turns: Some(true),
                extra: Default::default(),
            }),
            Request::Persist,
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
        if let Some(version) = some_of(thread.cli_version.as_deref()) {
            self.version = Some(version);
        }
        self.shared.provider_started();
        let kind = if some_of(thread.forked_from_id.as_deref()).is_some() {
            wire::BoundaryKind::Forked
        } else if !fresh && (self.incarnation > 1 || !thread.turns.is_empty()) {
            wire::BoundaryKind::Resumed
        } else {
            wire::BoundaryKind::Started
        };
        self.boundary(emit, kind, wire::IncarnationEnd::default());
        self.release_held(emit);
    }

    // --- requests from the server ----------------------------------------

    fn server_request(
        &mut self,
        emit: &mut Emit,
        id: &RequestId,
        request: ServerRequest,
        payload: &[u8],
    ) {
        let key = ask_key(id);
        let method = request.method();
        let thread = request.thread_id().to_owned();
        // A child thread's request is the child's: its work is no row of the
        // agent's turn, so the ask points at none.
        let child = self.is_child(&thread);
        let (item_key, body, decisions) = match request {
            ServerRequest::CommandApproval(params) => self.command_approval(emit, params, child),
            ServerRequest::FileChangeApproval(params) => {
                let changes = if child {
                    self.child_changes
                        .get(&params.item_id)
                        .map(|work| work.changes.clone())
                        .unwrap_or_default()
                } else {
                    match self
                        .works
                        .get(&params.item_id)
                        .and_then(|state| state.work.as_ref())
                        .and_then(|work| work.of.as_ref())
                    {
                        Some(work::Of::FileChange(change)) => change.changes.clone(),
                        _ => Vec::new(),
                    }
                };
                (
                    if child { String::new() } else { params.item_id },
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
                return self.elicitation(emit, key, method, params, payload, child);
            }
            ServerRequest::ToolCall(_) => {
                // Tools amux offers Codex are served by amux's tool server;
                // a client-side tool is nothing this agent hosts.
                let refused = ToolCallResponse {
                    content_items: vec![ToolOutputContent::Text(TextContent {
                        text: "This client hosts no dynamic tools.".into(),
                        extra: Default::default(),
                    })],
                    success: false,
                    extra: Default::default(),
                };
                return self.respond(emit, &key, Ok(ClientResponse::ToolCall(refused)));
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
        child: bool,
    ) -> (String, codex_ask::Body, Vec<CommandDecision>) {
        let item_id = if child { String::new() } else { params.item_id };
        let command = shown_command(&params.command.unwrap_or_default());
        let cwd = params.cwd.unwrap_or_default();
        if !child && !self.works.contains_key(&item_id) {
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
        self.shared.open_ask(ask);
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
    fn is_child(&self, thread: &str) -> bool {
        !thread.is_empty() && self.thread_id.as_deref().is_some_and(|ours| ours != thread)
    }

    fn elicitation(
        &mut self,
        emit: &mut Emit,
        key: String,
        method: &str,
        params: ElicitationParams,
        payload: &[u8],
        child: bool,
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
        let (item_key, call_server, tool) = match self.running_tool_call(&server, &named_tool) {
            Some(call) if !child => call,
            _ => (String::new(), server.clone(), named_tool),
        };
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
    /// when amux interrupted its turn, else answered by another client.
    fn ask_resolved(&mut self, emit: &mut Emit, key: &str) {
        let Some(ask) = self.shared.close_ask(key) else {
            return;
        };
        let meta = self.asks.remove(key);
        if self.interrupted_here {
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
        // A child thread's activity on the same server is its own; the
        // collaboration item reports it here. Its file changes are kept for
        // its approvals to show.
        if let (Some(ours), Some(theirs)) = (self.thread_id.as_deref(), about_thread(&notification))
            && ours != theirs
        {
            if let ServerNotification::ItemStarted(started)
            | ServerNotification::ItemCompleted(started) = &notification
                && let ThreadItem::FileChange(change) = &started.item
            {
                self.child_changes.insert(
                    change.id.clone(),
                    wire::FileChangeWork {
                        changes: file_changes(&change.changes, false),
                    },
                );
            }
            return;
        }
        match notification {
            ServerNotification::ThreadStarted(started) => {
                self.thread_started(emit, &started.thread, None, false);
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
            ServerNotification::TurnDiffUpdated(updated) => {
                let key = format!("diff:{}", updated.turn_id);
                let diff = (key.clone(), updated.diff);
                if self.last_diff.as_ref() == Some(&diff) {
                    return;
                }
                let patch = diff.1.clone();
                self.last_diff = Some(diff);
                self.emit_item(
                    emit,
                    ItemDraft {
                        key,
                        body: item_body(codex_item::Kind::TurnDiff(wire::TurnDiff { patch })),
                        complete: true,
                        ..Default::default()
                    },
                );
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
            ServerNotification::McpServerStatusUpdated(updated) => {
                self.server_status(emit, updated)
            }
            // A reroute covers one request and Codex keeps the chosen model
            // for the next; it also sends a warning saying so, with a link,
            // which is what the person sees.
            ServerNotification::ModelRerouted(_) => {}
            ServerNotification::Error(error) => self.api_error(emit, error),
            ServerNotification::AutoApprovalReviewStarted(review) => {
                self.review(emit, review, false)
            }
            ServerNotification::AutoApprovalReviewCompleted(review) => {
                self.review(emit, review, true)
            }
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
            | ServerNotification::ThreadNameUpdated(_)
            | ServerNotification::ThreadCompacted(_)
            | ServerNotification::ThreadArchived(_)
            | ServerNotification::ThreadUnarchived(_)
            | ServerNotification::ThreadGoalUpdated(_)
            | ServerNotification::ThreadGoalCleared(_)
            | ServerNotification::ThreadReadStateChanged(_)
            | ServerNotification::TurnModerationMetadata(_)
            | ServerNotification::RemoteControlStatusChanged(_)
            | ServerNotification::FileChangeOutputDelta(_)
            | ServerNotification::TerminalInteraction(_) => {}
            ServerNotification::SkillsChanged(_) => self.skills_changed(emit),
            // Configuration Codex found and did not apply: the person
            // wants to know, in Codex's words.
            ServerNotification::ConfigWarning(warning) => {
                self.warning(emit, "configWarning", notice_words(warning))
            }
            // Codex's own app shows each of these as a warning line.
            ServerNotification::DeprecationNotice(notice) => {
                self.warning(emit, "deprecationNotice", notice_words(notice))
            }
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
            ServerNotification::HookStarted(_) | ServerNotification::HookCompleted(_) => {}
            // Codex closes a thread only once no client is subscribed, so
            // the agent's own thread closing means it is gone; the next
            // incarnation resumes it.
            ServerNotification::ThreadClosed(_) => emit.effect(Effect::Exit {
                cause: "Codex closed the agent's thread".into(),
            }),
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

    fn server_status(&mut self, emit: &mut Emit, updated: McpServerStatusUpdated) {
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
        let health = self.servers.get_or_insert_with(ToolServerHealth::default);
        let entry = ToolServer {
            name: name.clone(),
            status: status as i32,
            error: error.clone(),
        };
        match health.servers.iter_mut().find(|server| server.name == name) {
            Some(server) => *server = entry,
            None => health.servers.push(entry),
        }
        let failed = health.servers.iter().any(|server| {
            server.status == ToolServerStatus::Failed as i32
                || server.status == ToolServerStatus::NeedsAuth as i32
        });
        health.state = if failed {
            wire::HealthState::Degraded
        } else {
            wire::HealthState::Healthy
        } as i32;
        if matches!(
            status,
            ToolServerStatus::Failed | ToolServerStatus::NeedsAuth
        ) {
            self.emit_item(
                emit,
                ItemDraft {
                    key: format!("mcp:{name}"),
                    body: item_body(codex_item::Kind::McpStartup(wire::McpStartup {
                        server: name,
                        status: status as i32,
                        error,
                    })),
                    complete: true,
                    ..Default::default()
                },
            );
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
        if unauthorized(&error) {
            self.sign_in = Some(SignIn {
                state: SignInState::Failed as i32,
                account: String::new(),
                message: message.clone(),
            });
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
        self.final_error = (!will_retry).then_some(turn);
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
            ThreadItem::Plan(plan) => {
                self.streamed.insert(id.clone(), Streamed::Plan);
                let text = match self.shared.open_item(&id) {
                    Some(open) if !completed => open.text.clone(),
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
                self.emit_plan(emit);
                if completed {
                    self.streamed.remove(&id);
                    self.shared.note_message(&id);
                }
            }
            ThreadItem::AgentMessage(message) => {
                let kind = self.streamed.get(&id).cloned().unwrap_or(
                    if message.phase == Some(MessagePhase::Commentary) {
                        Streamed::WorkingNote
                    } else {
                        Streamed::Message
                    },
                );
                self.streamed.insert(id.clone(), kind.clone());
                let complete = |complete| wire::Text { complete };
                let body = match kind {
                    Streamed::WorkingNote => codex_item::Kind::WorkingNote(complete(completed)),
                    _ => codex_item::Kind::Message(complete(completed)),
                };
                let (text, attachments) = match self.shared.open_item(&id) {
                    Some(open) if !completed => (open.text.clone(), Vec::new()),
                    _ if completed => crate::shared::parse_reply(message.text.clone()),
                    _ => (message.text.clone(), Vec::new()),
                };
                self.emit_item(
                    emit,
                    ItemDraft {
                        key: id.clone(),
                        text,
                        attachments,
                        body: item_body(body),
                        at_ms,
                        complete: completed,
                        ..Default::default()
                    },
                );
                if completed && kind == Streamed::Message {
                    self.shared.note_message(&id);
                }
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
                    self.boundary(
                        emit,
                        wire::BoundaryKind::Compacted,
                        wire::IncarnationEnd::default(),
                    );
                }
            }
            ThreadItem::CommandExecution(_)
            | ThreadItem::FileChange(_)
            | ThreadItem::McpToolCall(_)
            | ThreadItem::DynamicToolCall(_)
            | ThreadItem::WebSearch(_)
            | ThreadItem::ImageView(_)
            | ThreadItem::ImageGeneration(_)
            | ThreadItem::CollabAgentToolCall(_) => {
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
            // Read for what they carry elsewhere: a hook's prompt reaches the
            // conversation, a child's activity its own subagent row; a sleep
            // or a raw call output shows nothing in Codex's own app.
            ThreadItem::HookPrompt(_)
            | ThreadItem::SubAgentActivity(_)
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
        let text = full_text.unwrap_or_else(|| {
            self.shared
                .open_item(key)
                .map(|open| open.text.clone())
                .unwrap_or_default()
        });
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
        let first_in_turn = !std::mem::replace(&mut self.turn_prompted, true);
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
    fn work_item(
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
            if is_status_tool(&call.server, &call.tool) {
                let arguments = compact_json(&call.arguments);
                if let Some(working_on) = status_working_on(arguments.as_bytes()) {
                    self.shared.set_working_on(working_on);
                }
                return;
            }
            if is_send_tool(&call.server, &call.tool) {
                return self.sent_message(emit, id, call, at_ms, completed);
            }
        }
        let prior = self.works.get(id).cloned();
        let status = match item {
            ThreadItem::CommandExecution(command) => command.status.as_str(),
            ThreadItem::FileChange(change) => change.status.as_str(),
            ThreadItem::McpToolCall(call) => call.status.as_str(),
            ThreadItem::DynamicToolCall(call) => call.status.as_str(),
            ThreadItem::CollabAgentToolCall(call) => call.status.as_str(),
            ThreadItem::ImageGeneration(image) => image.status.as_str(),
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
            ThreadItem::DynamicToolCall(call) => {
                if completed && call.success == Some(false) {
                    state = ToolState::Failed;
                }
                (work::Of::Mcp(dynamic_work(call)), ToolClass::Consequential)
            }
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
            ThreadItem::DynamicToolCall(call) => call.duration_ms,
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
        let Some(state) = self.works.get_mut(id) else {
            return self.extend(emit, id, delta);
        };
        state.text.push_str(delta);
        match self.shared.append_output(emit, id, delta) {
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
        self.shared.item(
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
        // Work whose approval was still open never ran. A child thread's
        // asks are the child's, and stay open past the agent's turn.
        let mut unapproved = BTreeSet::new();
        let own: Vec<String> = self
            .shared
            .asks()
            .open_asks()
            .iter()
            .map(|ask| ask.key.clone())
            .filter(|key| {
                self.asks
                    .get(key)
                    .is_none_or(|meta| !self.is_child(&meta.thread))
            })
            .collect();
        for key in own {
            let Some(ask) = self.shared.close_ask(&key) else {
                continue;
            };
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
        if self.consumption == InjectConsumption::ParkedUntilNextTurn {
            self.turn_over_parked(emit);
        }
    }

    /// At turn end nothing streams any more. A command still running
    /// carries on in the background when the turn completed or was
    /// interrupted (a command Codex did end reports its own end), else it
    /// was cut short, as is one still waiting for its approval
    /// (`unapproved`, by item key), which never ran; any other open item
    /// is finished as it stands.
    fn settle_open(&mut self, emit: &mut Emit, completed: bool, unapproved: &BTreeSet<String>) {
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
            let Some(open) = self.shared.open_item(&key).cloned() else {
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
            self.shared.item(
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
    }
}

/// A dynamic tool call's work, drawn as a tool-server call.
fn dynamic_work(call: &DynamicToolCallItem) -> wire::McpToolCall {
    wire::McpToolCall {
        server: call.namespace.clone().unwrap_or_default(),
        tool: call.tool.clone(),
        arguments_json: compact_json(&call.arguments).into_bytes(),
        result_json: compact_json(&as_written(&call.content_items)).into_bytes(),
        error: String::new(),
        subject: crate::shared::first_string(&call.arguments),
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
