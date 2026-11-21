//! What the agent writes: answers, notifications and requests.

use acp_protocol::agent::{
    AuthState, InitializeResponse, NewSessionResponse, PermissionOptionKind, PromptResponse,
    RequestPermissionRequest, SessionSetup, SetSessionConfigOptionResponse, StopReason,
};
use acp_protocol::elicitation::CreateElicitationRequest;
use acp_protocol::session::SessionConfigOption;
use acp_protocol::update::{
    CompactionStatus, ContentChunk, NoticeSeverity, PlanEntryStatus, SessionUpdate, ToolCall,
    ToolCallContent, ToolCallLocation, ToolCallStatus, ToolCallUpdate,
};
use acp_protocol::{
    AgentMessage, AgentNotification, AgentRequest, ContentBlock, Extension, RequestId, RpcError,
};
use serde::Deserialize;
use serde_json::Value;
use wire::{AcpAsk, AcpTool, acp_ask, acp_item};

use super::{
    Category, INITIALIZE_REQUEST, LOAD_REQUEST, NEW_SESSION_REQUEST, RESUME_REQUEST, Request,
    State, Streamed, choices, finished, item_body, permission_kind, tool_class, tool_kind,
};
use crate::{Channel, Effect, Emit, Fact, ItemDraft, ask_item, unknown};

/// Grok's own session events, beside ACP's `session/update`.
const XAI_SESSION_NOTIFICATION: &str = "_x.ai/session_notification";

/// Grok's report of a compaction it ran on its own (`grok-compact`).
#[derive(Deserialize)]
struct XaiNotification {
    update: XaiUpdate,
}

#[derive(Deserialize)]
struct XaiUpdate {
    #[serde(rename = "sessionUpdate")]
    session_update: String,
    #[serde(default)]
    tokens_before: Option<u64>,
    #[serde(default)]
    tokens_after: Option<u64>,
}

/// The input of Grok's `use_tool` call, which runs a tool server's tool
/// by its name (`grok-elicitation`).
#[derive(Deserialize)]
struct UseTool {
    tool_name: String,
}

/// Grok's turn tokens, in its prompt answer's `_meta` (`grok-prompt`).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MetaTokens {
    #[serde(default)]
    input_tokens: Option<u64>,
    #[serde(default)]
    output_tokens: Option<u64>,
    #[serde(default)]
    cached_read_tokens: Option<u64>,
    #[serde(default)]
    reasoning_tokens: Option<u64>,
    #[serde(default)]
    total_tokens: Option<u64>,
}

fn tool_state(status: Option<&ToolCallStatus>) -> wire::ToolState {
    match status {
        Some(ToolCallStatus::Pending) => wire::ToolState::Pending,
        Some(ToolCallStatus::Completed) => wire::ToolState::Succeeded,
        Some(ToolCallStatus::Failed) => wire::ToolState::Failed,
        // Grok starts a call with no status at all (`grok-read`): it runs.
        Some(ToolCallStatus::InProgress | ToolCallStatus::Other(_)) | None => {
            wire::ToolState::Running
        }
    }
}

fn block_text(block: &ContentBlock) -> Option<&str> {
    match block {
        ContentBlock::Text(text) => Some(&text.text),
        ContentBlock::ResourceLink(link) => Some(&link.uri),
        ContentBlock::Image(_)
        | ContentBlock::Audio(_)
        | ContentBlock::Resource(_)
        | ContentBlock::Unknown(_) => None,
    }
}

/// A call's content as its row holds it: what it printed, and its diffs.
fn tool_content(content: &[ToolCallContent]) -> (String, Vec<wire::AcpDiff>) {
    let mut texts = Vec::new();
    let mut diffs = Vec::new();
    for part in content {
        match part {
            ToolCallContent::Content(content) => texts.extend(block_text(&content.content)),
            ToolCallContent::Diff(diff) => diffs.push(wire::AcpDiff {
                path: diff.path.clone(),
                old_text: diff.old_text.clone().flatten(),
                new_text: diff.new_text.clone(),
            }),
            // amux offers the agent no terminals, so none is shown.
            ToolCallContent::Terminal(_) | ToolCallContent::Unknown(_) => {}
        }
    }
    let output = texts.join("\n");
    let cut = crate::shared::output_cut(&output);
    (output[cut..].to_owned(), diffs)
}

fn locations(locations: &[ToolCallLocation]) -> Vec<wire::AcpLocation> {
    locations
        .iter()
        .map(|location| wire::AcpLocation {
            path: location.path.clone(),
            line: location.line.flatten(),
        })
        .collect()
}

fn signed(state: wire::SignInState) -> wire::SignIn {
    wire::SignIn {
        state: state as i32,
        ..unknown::sign_in()
    }
}

impl State {
    pub(super) fn fact(&mut self, emit: &mut Emit, fact: Fact) {
        if fact.channel == Channel::Agent
            && let Ok(super::Stderr { stderr }) = serde_json::from_slice(&fact.payload)
        {
            return self.log(emit, format!("{} on stderr: {stderr}", self.agent));
        }
        if fact.channel != Channel::Rpc {
            return self.unrecognized(
                emit,
                &format!("{:?}", fact.channel),
                "a fact on a channel ACP agents do not use",
            );
        }
        let Ok(message) = acp_protocol::decode(&fact.payload) else {
            return self.unrecognized(emit, "unparsed", "a line that is not a JSON object");
        };
        match message {
            AgentMessage::Notification { notification, .. } => {
                self.notification(emit, notification)
            }
            AgentMessage::Request { id, request, .. } => {
                self.agent_request(emit, id, request, &fact.payload)
            }
            AgentMessage::Response { id, result, .. } => self.response(emit, id, result),
            AgentMessage::Extension { extension, .. } => self.extension(emit, extension),
            AgentMessage::Unknown(unknown) => self.unknown(emit, unknown),
        }
    }

    fn log(&self, emit: &mut Emit, line: String) {
        emit.effect(Effect::Log { line });
    }

    fn unrecognized(&mut self, emit: &mut Emit, fact_type: &str, summary: &str) {
        let key = self.local_key("unrecognized");
        self.emit_item(
            emit,
            ItemDraft {
                key,
                body: item_body(acp_item::Kind::Unrecognized(wire::Unrecognized {
                    fact_type: fact_type.to_owned(),
                    summary: summary.to_owned(),
                })),
                complete: true,
                ..Default::default()
            },
        );
    }

    /// Whether something naming `session` is about this agent's session.
    /// Before this process's session answer, the agent may speak of the
    /// session it is opening before naming it (Grok, `grok-prompt`), so
    /// anything counts.
    fn ours(&self, session: &str) -> bool {
        !self.opened || self.session_id.as_deref() == Some(session)
    }

    // --- notifications -----------------------------------------------------

    fn notification(&mut self, emit: &mut Emit, notification: AgentNotification) {
        match notification {
            AgentNotification::SessionUpdate(update) => {
                if !self.ours(&update.session_id) {
                    return self.log(emit, "an update for another session".to_owned());
                }
                self.update(emit, update.update)
            }
            // The page was answered when the person answered its ask.
            AgentNotification::ElicitationComplete(_) => {}
            AgentNotification::AuthStatusUpdate(status) => match status.state {
                AuthState::Authenticated | AuthState::NotRequired => {
                    self.sign_in = Some(signed(wire::SignInState::SignedIn));
                    self.shared.signed_in();
                }
                AuthState::Unauthenticated => {
                    self.sign_in = Some(signed(wire::SignInState::SignedOut))
                }
                AuthState::Unknown | AuthState::Other(_) => {}
            },
            // The agent withdrew a request of its own: its ask closes.
            AgentNotification::CancelRequest(cancel) => {
                let id = super::request_id_key(&cancel.request_id);
                let key = self
                    .ask_requests
                    .iter()
                    .find(|(_, request)| **request == id)
                    .map(|(key, _)| key.clone());
                if let Some(key) = key {
                    self.ask_requests.remove(&key);
                    if let Some(ask) = self.shared.close_ask(&key) {
                        self.dismiss(emit, &ask);
                    }
                }
            }
        }
    }

    fn update(&mut self, emit: &mut Emit, update: SessionUpdate) {
        let kind = update.kind().to_owned();
        match update {
            // ACP sends the person's words back only replaying a loaded
            // session, and amux drew each prompt as it sent it.
            SessionUpdate::UserMessageChunk(_) => {}
            SessionUpdate::AgentMessageChunk(chunk) if self.opened => {
                self.chunk(emit, Streamed::Message, &chunk)
            }
            SessionUpdate::AgentThoughtChunk(chunk) if self.opened => {
                self.chunk(emit, Streamed::Thought, &chunk)
            }
            SessionUpdate::ToolCall(call) if self.opened => {
                self.close_stream(emit);
                self.tool_call(emit, call);
            }
            SessionUpdate::ToolCallUpdate(update) if self.opened => {
                self.close_stream(emit);
                self.tool_update(emit, &update);
            }
            SessionUpdate::Notice(notice) if self.opened => {
                self.close_stream(emit);
                let severity = match notice.severity {
                    NoticeSeverity::Info => wire::AcpNoticeSeverity::Info,
                    NoticeSeverity::Warning => wire::AcpNoticeSeverity::Warning,
                    NoticeSeverity::Error => wire::AcpNoticeSeverity::Error,
                    NoticeSeverity::Other(_) => wire::AcpNoticeSeverity::Unspecified,
                };
                self.notice(
                    emit,
                    wire::AcpNoticeKind::Agent,
                    severity,
                    notice.title,
                    notice.description.flatten().unwrap_or_default(),
                );
            }
            SessionUpdate::CompactionUpdate(compaction) if self.opened => match compaction.status {
                CompactionStatus::Completed => {
                    self.close_stream(emit);
                    self.compaction(emit, &compaction.compaction_id, None, None, false);
                }
                CompactionStatus::Failed => self.log(
                    emit,
                    format!(
                        "compaction failed: {}",
                        compaction.error.flatten().unwrap_or_default()
                    ),
                ),
                CompactionStatus::InProgress
                | CompactionStatus::Cancelled
                | CompactionStatus::Other(_) => {}
            },
            // What comes before this process's session answer replays a
            // loaded session (`agy-load`, `opencode-load`, `grok-load`),
            // which the chat already holds; a compaction's summary is the
            // model's, not the person's to read.
            SessionUpdate::AgentMessageChunk(_)
            | SessionUpdate::AgentThoughtChunk(_)
            | SessionUpdate::ToolCall(_)
            | SessionUpdate::ToolCallUpdate(_)
            | SessionUpdate::Notice(_)
            | SessionUpdate::CompactionUpdate(_)
            | SessionUpdate::CompactionSummaryChunk(_) => {}
            SessionUpdate::Plan(plan) => {
                self.plan = Some(
                    plan.entries
                        .into_iter()
                        .map(|entry| {
                            let status = match entry.status {
                                PlanEntryStatus::Pending => wire::TaskListStatus::Pending,
                                PlanEntryStatus::InProgress => wire::TaskListStatus::InProgress,
                                PlanEntryStatus::Completed => wire::TaskListStatus::Completed,
                                PlanEntryStatus::Other(_) => wire::TaskListStatus::Unspecified,
                            };
                            (entry.content, status as i32)
                        })
                        .collect(),
                )
            }
            SessionUpdate::AvailableCommandsUpdate(update) => {
                self.commands = update
                    .available_commands
                    .into_iter()
                    .map(|command| wire::OfferedCommand {
                        name: command.name,
                        description: command.description,
                        argument_hint: command
                            .input
                            .flatten()
                            .map(|input| input.hint)
                            .unwrap_or_default(),
                        source: String::new(),
                    })
                    .collect();
                self.offers_changed = true;
            }
            SessionUpdate::CurrentModeUpdate(update) => {
                if let Some(choice) = self
                    .choices
                    .iter_mut()
                    .find(|choice| choice.category == Category::Mode)
                {
                    choice.current = update.current_mode_id;
                }
            }
            SessionUpdate::ConfigOptionUpdate(update) => {
                if !self.setting_pending() {
                    self.merge_options(&update.config_options);
                }
            }
            SessionUpdate::SessionInfoUpdate(info) => {
                if let Some(Some(title)) = info.title
                    && self.shared.wants_title()
                {
                    self.shared.provider_titled(&title);
                }
            }
            SessionUpdate::UsageUpdate(update) => {
                self.context = Some((update.used, update.size));
                if let Some(Some(cost)) = update.cost {
                    let usage = self.usage.get_or_insert_with(unknown::acp_usage);
                    usage.cost = cost.amount.as_f64();
                    usage.currency = cost.currency;
                }
            }
            SessionUpdate::Unknown(_) if self.opened => self.unrecognized(
                emit,
                &format!("session/update {kind}"),
                "an update amux does not read",
            ),
            SessionUpdate::Unknown(_) => {}
        }
    }

    /// Replaces the config options, keeping an older mode or model list
    /// for a category no option covers.
    fn merge_options(&mut self, options: &[SessionConfigOption]) {
        let mut merged = choices(Some(options), None, None);
        for legacy in self.choices.iter().filter(|choice| choice.id.is_empty()) {
            if !merged
                .iter()
                .any(|choice| choice.category == legacy.category)
            {
                merged.push(legacy.clone());
            }
        }
        merged.sort_by_key(|choice| choice.category);
        self.choices = merged;
        self.offers_changed = true;
    }

    fn chunk(&mut self, emit: &mut Emit, kind: Streamed, chunk: &ContentChunk) {
        match block_text(&chunk.content) {
            Some(text) => self.stream(emit, kind, text),
            None => self.log(
                emit,
                format!("a {} block in the agent's text", chunk.content.kind()),
            ),
        }
    }

    fn notice(
        &mut self,
        emit: &mut Emit,
        kind: wire::AcpNoticeKind,
        severity: wire::AcpNoticeSeverity,
        title: String,
        text: String,
    ) {
        let key = self.local_key("notice");
        self.emit_item(
            emit,
            ItemDraft {
                key,
                text,
                body: item_body(acp_item::Kind::Notice(wire::AcpNotice {
                    kind: kind as i32,
                    severity: severity as i32,
                    title,
                })),
                complete: true,
                ..Default::default()
            },
        );
    }

    fn compaction(
        &mut self,
        emit: &mut Emit,
        id: &str,
        tokens_before: Option<u64>,
        tokens_after: Option<u64>,
        automatic: bool,
    ) {
        self.emit_item(
            emit,
            ItemDraft {
                key: format!("compaction:{id}"),
                body: item_body(acp_item::Kind::Compaction(wire::Compaction {
                    tokens_before,
                    tokens_after,
                    automatic,
                    idle: false,
                })),
                complete: true,
                ..Default::default()
            },
        );
    }

    fn tool_call(&mut self, emit: &mut Emit, call: ToolCall) {
        let id = call.tool_call_id.clone();
        if self.tool_closed(&id) {
            return;
        }
        let kind = tool_kind(call.kind.as_ref());
        let (output, diffs) = tool_content(call.content.as_deref().unwrap_or_default());
        let state = tool_state(call.status.as_ref());
        let decision = self.tools.get(&id).and_then(|tool| tool.decision.clone());
        let tool = AcpTool {
            title: call.title,
            kind: kind as i32,
            state: state as i32,
            class: tool_class(kind) as i32,
            decision,
            locations: locations(call.locations.as_deref().unwrap_or_default()),
            diffs,
            input_json: call
                .raw_input
                .as_ref()
                .map(|input| serde_json::to_vec(input).unwrap_or_default())
                .unwrap_or_default(),
            ended_at_ms: finished(state as i32).then(|| self.shared.now_ms()),
        };
        self.tools.insert(id.clone(), tool);
        self.outputs.insert(id.clone(), output);
        self.emit_tool(emit, &id);
    }

    /// A call's change: only the fields it carries change. A call never
    /// seen starts from it.
    fn tool_update(&mut self, emit: &mut Emit, update: &ToolCallUpdate) {
        let id = update.tool_call_id.clone();
        if self.tool_closed(&id) {
            return;
        }
        let now = self.shared.now_ms();
        let tool = self.tools.entry(id.clone()).or_insert_with(|| AcpTool {
            state: wire::ToolState::Running as i32,
            class: wire::ToolClass::Consequential as i32,
            ..Default::default()
        });
        if let Some(Some(title)) = &update.title {
            tool.title = title.clone();
        }
        if let Some(kind) = &update.kind {
            let kind = tool_kind(kind.as_ref());
            tool.kind = kind as i32;
            tool.class = tool_class(kind) as i32;
        }
        if let Some(Some(status)) = &update.status {
            tool.state = tool_state(Some(status)) as i32;
        }
        if let Some(Some(found)) = &update.locations {
            tool.locations = locations(found);
        }
        if let Some(input) = &update.raw_input {
            tool.input_json = serde_json::to_vec(input).unwrap_or_default();
        }
        if finished(tool.state) && tool.ended_at_ms.is_none() {
            tool.ended_at_ms = Some(now);
        }
        if let Some(Some(content)) = &update.content {
            let (output, diffs) = tool_content(content);
            tool.diffs = diffs;
            self.outputs.insert(id.clone(), output);
        }
        self.emit_tool(emit, &id);
    }

    // --- the agent's requests ----------------------------------------------

    fn agent_request(
        &mut self,
        emit: &mut Emit,
        id: RequestId,
        request: AgentRequest,
        payload: &[u8],
    ) {
        // An ask that comes while amux is cancelling the turn would only be
        // answered cancelled, as every open ask was: so it is, at once.
        let asks = matches!(
            request,
            AgentRequest::RequestPermission(_)
                | AgentRequest::ElicitationCreate(
                    CreateElicitationRequest::Form(_) | CreateElicitationRequest::Url(_)
                )
        );
        if asks && self.cancelled {
            self.asks_seen += 1;
            let response = match request {
                AgentRequest::RequestPermission(_) => {
                    acp_protocol::ClientResponse::RequestPermission(
                        acp_protocol::client::RequestPermissionResponse::cancelled(),
                    )
                }
                _ => acp_protocol::ClientResponse::ElicitationCreate(
                    acp_protocol::elicitation::CreateElicitationResponse::Cancel(
                        acp_protocol::Empty::default(),
                    ),
                ),
            };
            return self.write(emit, &acp_protocol::ClientMessage::response(id, response));
        }
        match request {
            AgentRequest::RequestPermission(request) => self.permission(emit, &id, request),
            AgentRequest::ElicitationCreate(CreateElicitationRequest::Form(form)) => {
                let schema_json =
                    crate::shared::json_as_written(payload, &["params", "requestedSchema"])
                        .unwrap_or_default();
                self.work_ask(
                    emit,
                    &id,
                    acp_ask::Body::Form(wire::FormAsk {
                        server: String::new(),
                        message: form.message,
                        schema_json,
                    }),
                );
            }
            AgentRequest::ElicitationCreate(CreateElicitationRequest::Url(page)) => self.work_ask(
                emit,
                &id,
                acp_ask::Body::Link(wire::LinkAsk {
                    server: String::new(),
                    message: page.message,
                    url: page.url,
                }),
            ),
            AgentRequest::ElicitationCreate(CreateElicitationRequest::Unknown(_)) => {
                self.refuse_request(emit, id, "elicitation/create");
                self.unrecognized(
                    emit,
                    "elicitation/create",
                    "an elicitation amux cannot show",
                );
            }
            // amux offers the agent neither files nor terminals at
            // `initialize`, so only an agent ignoring that asks.
            AgentRequest::ReadTextFile(_)
            | AgentRequest::WriteTextFile(_)
            | AgentRequest::TerminalCreate(_)
            | AgentRequest::TerminalOutput(_)
            | AgentRequest::TerminalRelease(_)
            | AgentRequest::TerminalKill(_)
            | AgentRequest::TerminalWaitForExit(_) => {
                let method = request.method();
                self.log(
                    emit,
                    format!(
                        "{} asked for {method}, which amux does not offer",
                        self.agent
                    ),
                );
                self.refuse_request(emit, id, method);
            }
        }
    }

    /// A call waiting for the person: its row is drawn, or updated from
    /// what the request says of it, and the ask offers exactly the agent's
    /// options.
    fn permission(&mut self, emit: &mut Emit, id: &RequestId, request: RequestPermissionRequest) {
        self.close_stream(emit);
        let call = request.tool_call.tool_call_id.clone();
        let amux_tool = self.amux_tool(&request.tool_call);
        self.tool_update(emit, &request.tool_call);
        if amux_tool {
            // amux's own tools never ask, on any kind: the call is allowed
            // once, which a new process allows again. Allowing always would
            // be a grant the session holds that nobody chose, so an agent
            // that offers no one-time allow asks as for any other call.
            let allow = request
                .options
                .iter()
                .find(|option| option.kind == PermissionOptionKind::AllowOnce);
            if let Some(allow) = allow {
                let response = acp_protocol::ClientResponse::RequestPermission(
                    acp_protocol::client::RequestPermissionResponse::selected(&allow.option_id),
                );
                self.write(
                    emit,
                    &acp_protocol::ClientMessage::response(id.clone(), response),
                );
                return self.decide(
                    emit,
                    &super::tool_key(&call),
                    wire::DecisionOutcome::Allowed,
                );
            }
        }
        let tool = self.tools.get(&call).cloned().unwrap_or_default();
        let ask = AcpAsk {
            key: self.ask_for(id),
            item_key: super::tool_key(&call),
            body: Some(acp_ask::Body::Permission(wire::AcpPermission {
                title: tool.title,
                kind: tool.kind,
                options: request
                    .options
                    .iter()
                    .map(|option| wire::AcpPermissionOption {
                        option_id: option.option_id.clone(),
                        name: option.name.clone(),
                        kind: permission_kind(&option.kind) as i32,
                    })
                    .collect(),
            })),
        };
        self.shared.open_ask(ask);
    }

    /// Whether a call is to one of amux's own tools that never ask, by the
    /// names the agent gives it: its title so far, and the title, name and
    /// input the permission request carries. OpenCode titles a call by its
    /// tool and its permission request by what it does (`opencode-allow`),
    /// so the call's earlier title counts too.
    fn amux_tool(&self, call: &ToolCallUpdate) -> bool {
        let naming = self.quirks().tool_naming;
        let tracked = self.tools.get(&call.tool_call_id);
        let used: Option<UseTool> = tracked
            .and_then(|tool| serde_json::from_slice(&tool.input_json).ok())
            .or_else(|| {
                call.raw_input
                    .clone()
                    .and_then(|input| serde_json::from_value(input).ok())
            });
        let names = [
            tracked.map(|tool| tool.title.clone()),
            call.title.clone().flatten(),
            call.name.clone().flatten(),
            used.map(|used| used.tool_name),
        ];
        names
            .iter()
            .flatten()
            .any(|name| naming.amux_tool(name).is_some())
    }

    fn work_ask(&mut self, emit: &mut Emit, id: &RequestId, body: acp_ask::Body) {
        self.close_stream(emit);
        let key = self.ask_for(id);
        let ask = AcpAsk {
            item_key: ask_item::key(&key),
            key,
            body: Some(body),
        };
        self.shared.open_ask(ask.clone());
        self.emit_ask_item(emit, &ask, None);
    }

    /// A vendor's own method. A request is refused at once, so the agent
    /// does not wait on amux for it: Grok's questions and tool-server
    /// forms (`grok-question`, `grok-elicitation`) included, as the answer
    /// it would take was never recorded. A notification on the known list
    /// is dropped by name, Grok's compaction report aside; one not on it
    /// is logged, so a new one is seen without being drawn.
    fn extension(&mut self, emit: &mut Emit, extension: Extension) {
        if let Some(id) = extension.id.clone() {
            self.refuse_request(emit, id, &extension.method);
        }
        if extension.method == XAI_SESSION_NOTIFICATION
            && self.opened
            && let Ok(notification) =
                serde_json::from_value::<XaiNotification>(extension.params.clone())
            && notification.update.session_update == "auto_compact_completed"
        {
            self.close_stream(emit);
            let id = self.local_key("auto");
            self.compaction(
                emit,
                &id,
                notification.update.tokens_before,
                notification.update.tokens_after,
                true,
            );
        }
        if !extension.is_known() {
            self.log(
                emit,
                format!(
                    "{} sent {}, which amux does not read",
                    self.agent, extension.method
                ),
            );
        }
    }

    fn unknown(&mut self, emit: &mut Emit, unknown: acp_protocol::Unknown) {
        match (unknown.method.clone(), unknown.id.clone()) {
            (Some(method), Some(id)) => {
                self.refuse_request(emit, id, &method);
                self.unrecognized(emit, &method, "a request amux cannot read");
            }
            (Some(method), None) => {
                if unknown
                    .session_id()
                    .is_some_and(|session| !self.ours(&session))
                {
                    return;
                }
                self.unrecognized(emit, &method, "a notification amux cannot read");
            }
            (None, Some(id)) => {
                let unreadable = RpcError {
                    code: 0,
                    message: "an answer amux cannot read".to_owned(),
                    data: None,
                    extra: acp_protocol::Extra::new(),
                };
                self.response(emit, id, Err(unreadable));
            }
            (None, None) => self.unrecognized(emit, "message", "neither a request nor a response"),
        }
    }

    // --- answers -----------------------------------------------------------

    fn response(&mut self, emit: &mut Emit, id: RequestId, result: Result<Value, RpcError>) {
        let RequestId::String(id) = id else {
            return self.log(
                emit,
                format!("an answer to request {id}, which amux did not make"),
            );
        };
        match id.as_str() {
            INITIALIZE_REQUEST => self.initialized(emit, result),
            NEW_SESSION_REQUEST | RESUME_REQUEST | LOAD_REQUEST => {
                self.session_opened(emit, &id, result)
            }
            _ => match self.requests.remove(&id) {
                Some(Request::Prompt {
                    input_id,
                    message,
                    steer,
                }) => self.prompt_answered(emit, &id, input_id, message, steer, result),
                Some(Request::Set {
                    category,
                    value,
                    apply,
                }) => self.set_answered(emit, category, value, apply, result),
                // A request of an earlier process's, or of an earlier build.
                None if id.starts_with("amux-") => {}
                None => self.log(
                    emit,
                    format!("an answer to request {id}, which amux did not make"),
                ),
            },
        }
    }

    fn initialized(&mut self, emit: &mut Emit, result: Result<Value, RpcError>) {
        let init = match result {
            Ok(result) => serde_json::from_value::<InitializeResponse>(result),
            Err(error) => {
                return emit.effect(Effect::Exit {
                    cause: format!("{} refused to start: {}", self.agent, error.message),
                });
            }
        };
        // The agent process opens no session on an answer it cannot read
        // either, so the agent would wait for good.
        let Ok(init) = init else {
            return emit.effect(Effect::Exit {
                cause: format!(
                    "{} answered initialize in a way amux cannot read",
                    self.agent
                ),
            });
        };
        // An agent may answer with an older version than amux's, whose
        // messages amux cannot be sure to read; the agent process opens no
        // session for it either.
        if init.protocol_version != acp_protocol::client::PROTOCOL_VERSION {
            return emit.effect(Effect::Exit {
                cause: format!(
                    "{} speaks version {} of the Agent Client Protocol; amux speaks version {}",
                    self.agent,
                    init.protocol_version,
                    acp_protocol::client::PROTOCOL_VERSION
                ),
            });
        }
        self.reopens = Some(super::Reopens::of(&init));
        self.takes = Some(super::Takes::of(&init));
        self.say_capabilities();
        // A conversation this process cannot reopen ends here; the agent
        // process sends no session request for it.
        if super::opening(&init, self.session_id.as_deref()) == super::Opening::Unresumable {
            return emit.effect(Effect::Exit {
                cause: super::UNRESUMABLE.to_owned(),
            });
        }
        if let Some(method) = self.quirks().signed_in_method {
            let present = init
                .auth_methods
                .iter()
                .flatten()
                .any(|offered| offered.id() == Some(method));
            if present {
                self.sign_in = Some(signed(wire::SignInState::SignedIn));
                self.shared.signed_in();
            } else {
                self.sign_in = Some(signed(wire::SignInState::SignedOut));
            }
        }
    }

    /// The agent opened the session, or refused to. A refusal ends the
    /// incarnation with the agent's words: waiting would leave the agent
    /// starting for good, and a refused reopening is never covered by a new
    /// session, which would pass a fresh conversation off as the old one.
    /// The session id never changes under an agent, so nothing keyed by the
    /// agent's ids (its calls, its requests) needs the session in its key.
    fn session_opened(&mut self, emit: &mut Emit, id: &str, result: Result<Value, RpcError>) {
        let reopened = id != NEW_SESSION_REQUEST;
        // A new session where a conversation was had: the agent process
        // lost the conversation's id, and a fresh conversation is never
        // passed off as the old one reopened.
        if !reopened && self.session_id.is_some() {
            return emit.effect(Effect::Exit {
                cause: format!(
                    "the id of {}'s conversation was lost, so the conversation cannot be reopened; amux does not start a new one in its place",
                    self.agent
                ),
            });
        }
        let result = match result {
            Ok(result) => result,
            Err(error) => {
                if error.code == acp_protocol::error_code::AUTH_REQUIRED {
                    self.sign_in = Some(signed(wire::SignInState::SignedOut));
                    self.shared.stopped_by(crate::notice::signed_out());
                }
                let what = if reopened {
                    "reopen its conversation"
                } else {
                    "open a session"
                };
                return emit.effect(Effect::Exit {
                    cause: format!("{} refused to {what}: {}", self.agent, error.message),
                });
            }
        };
        let setup = if reopened {
            serde_json::from_value::<SessionSetup>(result).ok()
        } else {
            // A new session must name itself.
            serde_json::from_value::<NewSessionResponse>(result)
                .ok()
                .map(|answer| SessionSetup {
                    session_id: Some(answer.session_id),
                    modes: answer.modes,
                    models: answer.models,
                    config_options: answer.config_options,
                    meta: answer.meta,
                    extra: answer.extra,
                })
        };
        let Some(setup) = setup else {
            return emit.effect(Effect::Exit {
                cause: format!("{} opened a session amux cannot read", self.agent),
            });
        };
        if let Some(session) = setup.session_id
            && self.session_id.as_ref() != Some(&session)
        {
            emit.effect(Effect::ProviderSession {
                id: session.clone(),
            });
            self.session_id = Some(session);
        }
        self.choices = choices(
            setup.config_options.flatten().as_deref(),
            setup.modes.flatten().as_ref(),
            setup.models.as_ref(),
        );
        self.offers_changed = true;
        if !reopened || self.starting.is_empty() {
            self.starting = self
                .choices
                .iter()
                .map(|choice| (choice.category, choice.current.clone()))
                .collect();
        }
        // An agent that gives a session needs no sign-in it lacks.
        self.sign_in = Some(signed(wire::SignInState::SignedIn));
        self.shared.signed_in();
        self.opened = true;
        self.start_boundary(
            emit,
            if reopened {
                wire::BoundaryKind::Resumed
            } else {
                wire::BoundaryKind::Started
            },
        );
        // The agent takes input once its settings are restored.
        self.apply_wanted(emit);
    }

    /// A setting's answer. What amux wants follows the newest pick of each
    /// setting: an answer to an older one, refused or not, changes it only
    /// while no newer pick of the same setting is in flight.
    fn set_answered(
        &mut self,
        emit: &mut Emit,
        category: Category,
        value: String,
        apply: bool,
        result: Result<Value, RpcError>,
    ) {
        let newer = self.requests.values().any(|request| {
            matches!(request, Request::Set { category: pending, .. } if *pending == category)
        });
        match result {
            Err(error) => {
                self.log(
                    emit,
                    format!("{} refused a setting: {}", self.agent, error.message),
                );
                if !newer {
                    // A person's pick it refused is not kept for later
                    // agents either.
                    self.picking.remove(&category);
                    // The session keeps what it has.
                    match self.current(category) {
                        Some(current) if !apply => {
                            self.wanted.insert(category, current);
                        }
                        _ => {
                            self.wanted.remove(&category);
                        }
                    }
                }
            }
            Ok(result) => {
                match serde_json::from_value::<SetSessionConfigOptionResponse>(result) {
                    Ok(answer) => self.merge_options(&answer.config_options),
                    // `session/set_mode` answers `{}` (`agy-config`): the
                    // session has the mode now.
                    Err(_) => {
                        if let Some(choice) = self
                            .choices
                            .iter_mut()
                            .find(|choice| choice.category == category)
                        {
                            choice.current = value.clone();
                        }
                    }
                }
                if !newer {
                    // The newest pick of the setting, taken: its host keeps
                    // a person's model or effort, never a mode.
                    if self.picking.remove(&category) {
                        match category {
                            Category::Model => emit.picked_model(value.clone()),
                            Category::Effort => emit.picked_effort(value.clone()),
                            Category::Mode => {}
                        }
                    }
                    self.wanted.insert(category, value);
                }
            }
        }
        if apply {
            self.apply_wanted(emit);
        }
    }

    fn prompt_answered(
        &mut self,
        emit: &mut Emit,
        id: &str,
        input_id: Vec<u8>,
        message: bool,
        steer: bool,
        result: Result<Value, RpcError>,
    ) {
        self.prompts.retain(|prompt| prompt != id);
        if message {
            self.shared.message_consumed(&input_id);
        }
        if steer && let Err(error) = &result {
            // The agent would not take the prompt into its turn: it waits
            // again, and the refusal says why.
            self.shared.steer_refused(&input_id);
            let key = self.local_key("error");
            self.emit_item(
                emit,
                ItemDraft {
                    key,
                    body: item_body(acp_item::Kind::Error(wire::ApiError {
                        error_kind: error.code.to_string(),
                        message: error.message.clone(),
                        ..Default::default()
                    })),
                    complete: true,
                    ..Default::default()
                },
            );
            if !self.prompts.is_empty() {
                return;
            }
        }
        self.read_usage(&result);
        if steer && result.is_ok() {
            self.steer_reflected(emit, &input_id);
        }
        // A later prompt the agent folded into this turn: the turn goes on
        // under it (`agy-mid_turn`).
        if let Some(later) = self.prompts.first().cloned() {
            if let Some(Request::Prompt {
                input_id,
                steer: true,
                ..
            }) = self.requests.get(&later).cloned()
            {
                self.steer_reflected(emit, &input_id);
            }
            return;
        }
        self.turn_over(emit, result);
    }

    fn steer_reflected(&mut self, emit: &mut Emit, input_id: &[u8]) {
        let Some(entry) = self
            .shared
            .steer_reflected(|entry| entry.input_id == input_id)
        else {
            return;
        };
        self.close_stream(emit);
        self.emit_item(
            emit,
            ItemDraft {
                key: super::prompt_key(&entry.input_id),
                text: entry.text,
                attachments: entry.attachments,
                input_id: entry.input_id,
                body: item_body(acp_item::Kind::Steer(wire::Steer {})),
                complete: true,
                ..Default::default()
            },
        );
    }

    fn read_usage(&mut self, result: &Result<Value, RpcError>) {
        let Ok(result) = result else {
            return;
        };
        let Ok(answer) = serde_json::from_value::<PromptResponse>(result.clone()) else {
            return;
        };
        let from_meta = quirk_meta(self.quirks().usage_in_answer_meta, answer.meta.clone());
        let usage = self.usage.get_or_insert_with(unknown::acp_usage);
        if let Some(Some(tokens)) = answer.usage {
            usage.input_tokens = Some(tokens.input_tokens);
            usage.output_tokens = Some(tokens.output_tokens);
            usage.total_tokens = Some(tokens.total_tokens);
            usage.thought_tokens = tokens.thought_tokens.flatten();
            usage.cached_read_tokens = tokens.cached_read_tokens.flatten();
            usage.cached_write_tokens = tokens.cached_write_tokens.flatten();
        } else if let Some(tokens) = from_meta {
            usage.input_tokens = tokens.input_tokens;
            usage.output_tokens = tokens.output_tokens;
            usage.total_tokens = tokens.total_tokens;
            usage.thought_tokens = tokens.reasoning_tokens;
            usage.cached_read_tokens = tokens.cached_read_tokens;
            usage.cached_write_tokens = None;
        } else {
            // The turn's tokens are unknown; the session's cost stands.
            usage.input_tokens = None;
            usage.output_tokens = None;
            usage.total_tokens = None;
            usage.thought_tokens = None;
            usage.cached_read_tokens = None;
            usage.cached_write_tokens = None;
        }
    }

    /// The turn is over: the answer to its last prompt came.
    fn turn_over(&mut self, emit: &mut Emit, result: Result<Value, RpcError>) {
        use wire::{AcpNoticeKind as Notice, TurnOutcome as Outcome};
        self.close_stream(emit);
        self.cancelled = false;
        let ended = |kind, words: &str| (kind, words.to_owned());
        let (outcome, notice) = match result {
            Ok(result) => match serde_json::from_value::<PromptResponse>(result) {
                Ok(answer) => match answer.stop_reason {
                    StopReason::EndTurn => (Outcome::Completed, None),
                    StopReason::Cancelled => (Outcome::Interrupted, None),
                    StopReason::Refusal => (
                        Outcome::Completed,
                        Some(ended(Notice::Refusal, "The model refused to go on.")),
                    ),
                    StopReason::MaxTokens => (
                        Outcome::Failed,
                        Some(ended(
                            Notice::MaxTokens,
                            "The turn reached the model's token limit.",
                        )),
                    ),
                    StopReason::MaxTurnRequests => (
                        Outcome::Failed,
                        Some(ended(
                            Notice::MaxTurnRequests,
                            "The turn reached the agent's limit on model requests.",
                        )),
                    ),
                    StopReason::Other(reason) => {
                        self.log(emit, format!("a turn ended {reason:?}"));
                        (Outcome::Completed, None)
                    }
                },
                Err(_) => {
                    self.log(emit, "a prompt answer amux cannot read".to_owned());
                    (Outcome::Completed, None)
                }
            },
            Err(error) => {
                if error.code == acp_protocol::error_code::AUTH_REQUIRED {
                    self.sign_in = Some(signed(wire::SignInState::SignedOut));
                    self.shared.stopped_by(crate::notice::signed_out());
                } else {
                    self.shared
                        .stopped_by(crate::notice::failed(&error.message));
                }
                let key = self.local_key("error");
                self.emit_item(
                    emit,
                    ItemDraft {
                        key,
                        body: item_body(acp_item::Kind::Error(wire::ApiError {
                            error_kind: error.code.to_string(),
                            message: error.message,
                            ..Default::default()
                        })),
                        complete: true,
                        ..Default::default()
                    },
                );
                (Outcome::Failed, None)
            }
        };
        if let Some((kind, words)) = notice {
            self.notice(
                emit,
                kind,
                wire::AcpNoticeSeverity::Warning,
                String::new(),
                words,
            );
        }
        for ask in self.shared.close_all_asks() {
            self.dismiss(emit, &ask);
        }
        self.settle_tools(emit);
        self.shared.steers_lost();
        if let Some(ended) = self.shared.turn_ended(emit, outcome) {
            // As the turn's end records it: a stopped agent's turn ends
            // stopped, however the agent words it.
            let outcome = emit
                .step
                .turn_end
                .as_ref()
                .map_or(outcome as i32, |end| end.outcome);
            self.emit_item(
                emit,
                ItemDraft {
                    key: format!("turn:{}", ended.id),
                    body: item_body(acp_item::Kind::Turn(wire::Turn {
                        turn_id: ended.id,
                        outcome,
                        started_at_ms: ended.started_at_ms,
                    })),
                    complete: true,
                    ..Default::default()
                },
            );
        }
    }
}

/// The turn's tokens in a prompt answer's `_meta`, for an agent that puts
/// them there.
fn quirk_meta(reads: bool, meta: Option<Value>) -> Option<MetaTokens> {
    if !reads {
        return None;
    }
    serde_json::from_value::<MetaTokens>(meta?).ok()
}
