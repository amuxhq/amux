//! Claude through the SDK: headless `claude -p` speaking stream-JSON. The
//! facts are the lines Claude writes to stdout, events and control
//! requests alike, and the process's exit. Text, thinking and tool input
//! stream; asks arrive as named control requests, so nothing is inferred.
//!
//! Every user message the interpreter hands to Claude carries a uuid made
//! from its input or envelope id. Claude echoes it on the message's replay,
//! on its `command_lifecycle` frames and as its transcript row's uuid, so a
//! reflection is matched to what was sent by id ([`CORRELATION`]); several
//! messages written while a turn runs fold into that turn, so arrival order
//! would mismatch them.
//!
//! What the agent offers comes from Claude, never from a client catalogue:
//! the answer to the agent process's `initialize` request lists the models
//! (each with its effort levels) and the slash commands, and every later
//! snapshot carries them.

mod facts;
mod recording;

use std::collections::BTreeMap;

use claude_protocol::stream::control::{
    ApplyFlagSettingsRequest, InterruptRequest, SetModelRequest, SetPermissionModeRequest,
};
use claude_protocol::stream::{
    self as claude_stream, ControlRequest, ControlRequestBody, ControlResponse, ElicitationResult,
    FlagSettings, PermissionMode, PermissionResult, PermissionUpdate, PermissionUpdateDestination,
};
use prost::Message as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use wire::{
    AgentSpec, Ask, AskClosed, Attachment, ClaudeAnswer, ClaudeGrant, ClaudeSdkItem,
    ClaudeSdkSnapshot, ClaudeUsage, ContextMeter, ContextShare, DecisionOutcome, Envelope,
    FormAction, Input, KindInput, OfferedCommand, OfferedModel, Refusal, SignIn, Step, ToolCall,
    ToolDecision, ToolServerHealth, ToolState, claude_answer, claude_sdk_input, claude_sdk_item,
    permission_answer, tool_decision,
};

use crate::claude_common::{
    Jobs, Task, Verdict, claude_grant, describe_asks, describe_claude_usage, describe_tasks,
    describe_tool, file_step_facts, launch_arg, or_dash, permission_scopes, plan_item,
    question_item, task_list,
};
use crate::{
    Carrier, Checkpoint, Effect, Emit, Event, FixtureInput, Interpreter, ItemDraft, ItemView,
    RedactTarget, SendOutcome, Shared, SnapshotView, Stepped, agent_message_body,
    agent_message_key, ask_item, claude_sdk_input, human, is_send_tool, sent_message, serde_pb,
    unknown,
};

/// The interpreter for kind `claude_sdk`.
pub struct ClaudeSdk;

/// How a reflection is matched to the input it reflects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Correlation {
    /// By the uuid the interpreter put on the message.
    ClientId,
    /// By arrival order, one message per turn.
    Fifo,
}

/// Decided by probing Claude 2.1.282: the uuid on a stdin user message is
/// echoed exactly, and messages written during a turn fold into it, so
/// arrival order cannot be trusted.
pub const CORRELATION: Correlation = Correlation::ClientId;

/// The uuid a user message for this input or envelope id carries. Claude
/// wants a well-formed uuid, so the id's first sixteen bytes (zero padded)
/// get version 4 and the RFC 4122 variant; the interpreter remembers the
/// mapping rather than reversing it.
pub fn client_uuid(id: &[u8]) -> String {
    let mut bytes = [0u8; 16];
    for (slot, byte) in bytes.iter_mut().zip(id) {
        *slot = *byte;
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex = crate::to_hex(&bytes);
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// A message handed to Claude, awaiting or past its reflection.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Client {
    #[serde(with = "serde_pb::bytes")]
    id: Vec<u8>,
    /// An agent message, which leaves the pending set when Claude takes it.
    message: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Tool {
    at_ms: i64,
    name: String,
    server: String,
    input: String,
    state: i32,
    outcome_text: String,
    outcome_json: String,
    class: i32,
    background: bool,
    decision: Option<ToolDecisionState>,
    ended_at_ms: Option<i64>,
    /// When the call began running: the moment it was allowed, when it
    /// waited for the person's approval.
    #[serde(default)]
    started_at_ms: Option<i64>,
    parent_key: String,
    hidden: bool,
    /// The task this call started and Claude reports on: a subagent, or a
    /// shell in the background. The call stays open until the task ends,
    /// since its own result only says the task was launched.
    task: Option<TaskProgress>,
    /// Images the tool read, by the blobs that hold them.
    #[serde(with = "serde_pb::msgs")]
    images: Vec<Attachment>,
    /// What amux's attach tool attached, from the element it answered.
    #[serde(default, with = "serde_pb::msgs")]
    attached: Vec<Attachment>,
    /// An ExitPlanMode call is drawn as its plan, with this verdict.
    plan: Option<Verdict>,
    /// An AskUserQuestion call is drawn as its question, closed with this
    /// record once decided.
    question: bool,
    #[serde(with = "serde_pb::opt_msg")]
    asked: Option<AskClosed>,
    #[serde(with = "serde_pb::item_body")]
    emitted: Vec<u8>,
}

/// What answering an ask decided, for the call it held.
enum Decided {
    Tool(ToolDecisionState),
    Plan(Verdict),
    Question(AskClosed),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct ToolDecisionState {
    outcome: i32,
    #[serde(with = "serde_pb::opt_msg")]
    grant: Option<ClaudeGrant>,
    note: String,
}

/// What answering an ask needs beyond the wire Ask.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct AskMeta {
    tool_use_id: String,
    input: String,
    /// Each offered scope choice as Claude sent it, to send back.
    suggestions: Vec<String>,
    shape: AskShape,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
enum AskShape {
    Permission,
    /// The permission the ask's switch choice moves to.
    Plan {
        switch: String,
    },
    /// The question texts, and each question's option labels.
    Question(Vec<(String, Vec<String>, bool)>),
    Form,
    Link,
}

/// A control request the interpreter sent, awaiting its response.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
enum Request {
    Interrupt,
    Model(Option<String>),
    Mode(String),
    /// None clears the session's own effort, back to the settings'.
    Effort(Option<String>),
    /// What Claude applied: the effort it runs at, which a model change
    /// can also move.
    Settings,
}

/// What a call's task has done so far, and whether it ended.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct TaskProgress {
    /// A subagent's steps; a shell in the background counts none.
    subagent: bool,
    tool_count: u32,
    last_tool: String,
    finished: bool,
}

/// A subagent or background task Claude reports.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct TaskState {
    at_ms: i64,
    description: String,
    state: i32,
    tool_count: u32,
    last_tool: String,
    tool_key: String,
    tokens: u64,
    /// A shell Claude runs as a task: a background job, never a subagent.
    shell: bool,
}

/// Everything the Claude SDK interpreter holds; its checkpoint.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct State {
    shared: Shared<wire::Ask>,
    incarnation: u32,
    session: Option<String>,
    version: Option<String>,
    model: Option<String>,
    /// The offered model amux last chose, which names the running one
    /// while it stands for it.
    chosen_model: Option<String>,
    effort: Option<String>,
    permission_mode: Option<String>,
    /// The models that take the auto permission, once Claude listed its
    /// models.
    auto_models: Option<Vec<String>>,
    /// Launched allowing the never-ask permission.
    never_ask: bool,
    /// The models the initialize response offers and the commands Claude
    /// last listed: the catalogue.
    #[serde(with = "serde_pb::msgs")]
    models: Vec<OfferedModel>,
    #[serde(with = "serde_pb::msgs")]
    commands: Vec<OfferedCommand>,
    inits: u32,
    exited: bool,
    clients: BTreeMap<String, Client>,
    /// The message being streamed, and its open blocks by index.
    stream: Option<String>,
    stream_tools: BTreeMap<u32, String>,
    /// Content blocks seen per assistant message, so whole blocks and
    /// streamed ones land on the same key.
    blocks: BTreeMap<String, u32>,
    tools: BTreeMap<String, Tool>,
    asks: BTreeMap<String, AskMeta>,
    requests: BTreeMap<String, Request>,
    next_request: u64,
    next_boundary: u64,
    /// The boundary drawn above a prompt submitted before the init that
    /// makes it: headless Claude reports its init only after it reads the
    /// first message. That init fills it in.
    early_boundary: Option<String>,
    tasks: Option<Vec<Task>>,
    active_tasks: BTreeMap<String, TaskState>,
    context_tokens: Option<u64>,
    context_window: Option<u64>,
    context_breakdown: Vec<(String, u64)>,
    #[serde(with = "serde_pb::opt_msg")]
    usage: Option<ClaudeUsage>,
    #[serde(with = "serde_pb::opt_msg")]
    servers: Option<ToolServerHealth>,
    #[serde(with = "serde_pb::opt_msg")]
    sign_in: Option<SignIn>,
    /// Claude's background jobs; the shared part publishes them.
    jobs: Jobs,
    /// Tasks Claude marks ambient, its own work rather than the person's.
    #[serde(default)]
    ambient: std::collections::BTreeSet<String>,
    /// The newest slash command sent, which a local command's output
    /// belongs to; taken by that output.
    slash: Option<String>,
    /// A built-in command's answer drawn before Claude named its command:
    /// its key and text, for the command's replay to name it.
    #[serde(default)]
    synthetic: Option<(String, String)>,
    /// An interrupt was sent in the running turn.
    interrupted: bool,
}

fn item_body(kind: claude_sdk_item::Kind) -> Vec<u8> {
    ClaudeSdkItem { kind: Some(kind) }.encode_to_vec()
}

/// A line for Claude's stdin.
fn write(input: &claude_stream::Input) -> Effect {
    Effect::ProviderWrite(claude_stream::encode(input))
}

/// AskUserQuestion's input with the person's answers: Claude hands it to
/// the tool.
#[derive(Serialize)]
struct AnsweringInput<'a> {
    #[serde(flatten)]
    input: serde_json::Map<String, Value>,
    /// The picked labels and typed answers, by each question's text.
    answers: BTreeMap<&'a str, String>,
    /// Notes per question, by its text.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    annotations: BTreeMap<&'a str, QuestionNote<'a>>,
}

/// The note a person wrote on a question, as Claude takes it.
#[derive(Serialize)]
struct QuestionNote<'a> {
    notes: &'a str,
}

/// The control response an answer sends.
enum SdkAnswer {
    Permission(PermissionResult),
    Elicitation(ElicitationResult),
}

/// What Claude offers on a host with no agent running, from its initialize
/// answer. Never-ask is listed: a new agent can be created allowing it.
pub fn host_catalogue(
    initialized: &claude_protocol::stream::InitializationResult,
) -> wire::Catalogue {
    use crate::claude_common::{auto_models, offered_commands, offered_models, permissions};
    wire::Catalogue {
        models: offered_models(&initialized.models),
        commands: offered_commands(&initialized.commands),
        permissions: permissions(Some(&auto_models(&initialized.models)), true, true),
        ..Default::default()
    }
}

impl State {
    /// The kind-neutral state: the agent process reads the pending
    /// agent-message set and quiescence from here.
    pub fn shared(&self) -> &Shared<Ask> {
        &self.shared
    }

    fn new(spec: &AgentSpec, producer_version: &str) -> Self {
        Self {
            shared: Shared::new(spec, ClaudeSdk::KIND, producer_version),
            incarnation: spec.incarnation,
            session: None,
            version: None,
            model: None,
            chosen_model: launch_arg(&spec.provider_args, "--model"),
            // The launch argument until Claude says what it applied.
            effort: launch_arg(&spec.provider_args, "--effort"),
            permission_mode: None,
            auto_models: None,
            never_ask: crate::claude_common::allows_never_ask(&spec.provider_args),
            models: Vec::new(),
            commands: Vec::new(),
            inits: 0,
            exited: false,
            clients: BTreeMap::new(),
            stream: None,
            stream_tools: BTreeMap::new(),
            blocks: BTreeMap::new(),
            tools: BTreeMap::new(),
            asks: BTreeMap::new(),
            requests: BTreeMap::new(),
            next_request: 0,
            next_boundary: 0,
            early_boundary: None,
            tasks: None,
            active_tasks: BTreeMap::new(),
            context_tokens: None,
            context_window: None,
            context_breakdown: Vec::new(),
            usage: None,
            servers: None,
            sign_in: None,
            jobs: Jobs::default(),
            slash: None,
            synthetic: None,
            ambient: Default::default(),
            interrupted: false,
        }
    }

    /// Whether Claude takes auto mode on this session's model. Claude lists
    /// its models by alias (`sonnet`, `opus[1m]`) and names the running one
    /// by id (`claude-sonnet-5`), so an alias names every id that holds it;
    /// before any model is named, the default one is running.
    fn offers_auto(&self) -> bool {
        let Some(models) = &self.auto_models else {
            return false;
        };
        let running = self.chosen_model.as_deref().or(self.model.as_deref());
        models.iter().any(|value| match running {
            None => value == "default",
            Some(running) => {
                let alias = value.trim_end_matches("[1m]");
                running.trim_end_matches("[1m]") == alias
                    || (alias != "default" && running.contains(alias))
            }
        })
    }

    /// The permissions Claude offers this session.
    fn permissions(&self) -> Vec<wire::OfferedPermission> {
        crate::claude_common::permissions(self.auto_models.as_deref(), self.never_ask, true)
    }

    fn body(&self) -> Vec<u8> {
        let context = match self.context_tokens {
            None => unknown::context_meter(),
            Some(used_tokens) => ContextMeter {
                known: true,
                used_tokens,
                window_tokens: self.context_window,
                breakdown: self
                    .context_breakdown
                    .iter()
                    .map(|(category, tokens)| ContextShare {
                        category: category.clone(),
                        tokens: *tokens,
                    })
                    .collect(),
            },
        };
        ClaudeSdkSnapshot {
            asks: self.shared.asks().open_asks().to_vec(),
            tasks: Some(task_list(&self.tasks)),
            context: Some(context),
            model: self.model.clone(),
            model_name: crate::shared::model_name(
                self.model.as_deref(),
                self.chosen_model.as_deref(),
                &self.models,
                crate::claude_common::tidy_model,
            ),
            effort: self.effort.clone(),
            permission: self.permission_mode.clone(),
            active_tasks: self
                .active_tasks
                .iter()
                .filter(|(_, task)| task.state == wire::TaskState::Running as i32 && !task.shell)
                .map(|(id, task)| task.to_wire(id))
                .collect(),
            usage: Some(self.usage.clone().unwrap_or_else(unknown::claude_usage)),
            servers: Some(
                self.servers
                    .clone()
                    .unwrap_or_else(unknown::tool_server_health),
            ),
            sign_in: Some(self.sign_in.clone().unwrap_or_else(unknown::sign_in)),
            background_jobs: Some(self.shared.jobs()),
            provider_session: self.session.clone(),
        }
        .encode_to_vec()
    }

    fn next_request_id(&mut self, request: Request) -> String {
        self.next_request += 1;
        let id = format!("amux-{}", self.next_request);
        self.requests.insert(id.clone(), request);
        id
    }

    fn control_request(&mut self, emit: &mut Emit, request: Request, body: ControlRequestBody) {
        let id = self.next_request_id(request);
        emit.effect(write(&claude_stream::Input::ControlRequest(
            ControlRequest::new(id, body),
        )));
    }

    // --- inputs ----------------------------------------------------------

    /// An immediate hand-off to Claude's own queue: a message sent while a
    /// turn runs folds into it.
    fn agent_message(&mut self, emit: &mut Emit, envelope: Envelope) {
        let envelope = crate::shared::worded(envelope);
        self.shared.message_accepted(&envelope.id);
        self.clients.insert(
            client_uuid(&envelope.id),
            Client {
                id: envelope.id.clone(),
                message: true,
            },
        );
        emit.effect(Effect::Inject {
            envelope: Box::new(Envelope {
                text: crate::shared::carried(&envelope),
                ..envelope.clone()
            }),
            via: Carrier::Stdin,
        });
        self.shared.item(
            emit,
            ItemDraft {
                key: agent_message_key(&envelope.id),
                text: envelope.text.clone(),
                attachments: envelope.attachments.clone(),
                input_id: envelope.id.clone(),
                body: item_body(claude_sdk_item::Kind::AgentMessage(agent_message_body(
                    &envelope,
                ))),
                complete: true,
                ..Default::default()
            },
        );
        self.shared.accept(emit, &envelope.id, true);
    }

    fn input(&mut self, emit: &mut Emit, input: Input) {
        let id = input.input_id.clone();
        // Another kind's input, or an arm a newer client sends that this
        // version cannot read.
        let arm = match input.decode_body() {
            Some(KindInput::ClaudeSdk(wire::ClaudeSdkInput { of: Some(arm) })) => arm,
            _ => return self.shared.reject(emit, &id, Refusal::Unsupported),
        };
        match arm {
            claude_sdk_input::Of::Prompt(prompt) => {
                self.dismiss_plan(emit);
                if let Some(entry) = self.shared.admit_prompt(emit, &id, prompt, human()) {
                    self.submit(emit, entry);
                }
            }
            claude_sdk_input::Of::Withdraw(withdraw) => {
                self.shared.withdraw(emit, &id, &withdraw.queued_input_id)
            }
            // A message at the default priority joins the running turn at
            // its next tool boundary. Priority "now" is not steering: it
            // abandons the rest of the turn.
            claude_sdk_input::Of::SendNow(send) => {
                let running = self.shared.is_busy();
                if let Some(entry) = self
                    .shared
                    .send_now(emit, &id, &send.queued_input_id, running)
                {
                    let uuid = client_uuid(&entry.input_id);
                    let text = crate::shared::provider_text(&entry);
                    self.clients.insert(
                        uuid.clone(),
                        Client {
                            id: entry.input_id,
                            message: false,
                        },
                    );
                    emit.effect(Effect::UserMessage {
                        uuid,
                        text,
                        attachments: entry.attachments,
                    });
                }
            }
            claude_sdk_input::Of::Interrupt(_) => {
                self.interrupt(emit);
                self.shared.accept(emit, &id, false);
            }
            claude_sdk_input::Of::Clear(_) => {
                let entry = wire::QueuedInput {
                    input_id: id.clone(),
                    text: "/clear".into(),
                    ..Default::default()
                };
                let prompt = wire::PromptInput {
                    text: entry.text,
                    ..Default::default()
                };
                if let Some(entry) = self.shared.admit_prompt(emit, &id, prompt, human()) {
                    self.submit(emit, entry);
                }
            }
            claude_sdk_input::Of::Permission(permission) => {
                let offered = self
                    .permissions()
                    .iter()
                    .any(|offered| offered.value == permission.value);
                if !offered {
                    return self.shared.reject(emit, &id, Refusal::Unsupported);
                }
                if permission.value != "plan" && self.leave_plan(emit, &permission.value) {
                    return self.shared.accept(emit, &id, false);
                }
                self.control_request(
                    emit,
                    Request::Mode(permission.value.clone()),
                    ControlRequestBody::SetPermissionMode(SetPermissionModeRequest {
                        mode: PermissionMode::parse(&permission.value),
                        extensions: Default::default(),
                    }),
                );
                self.shared.accept(emit, &id, false);
            }
            claude_sdk_input::Of::Model(model) => {
                let body = ControlRequestBody::SetModel(SetModelRequest {
                    model: model.model.clone(),
                    extensions: Default::default(),
                });
                self.control_request(emit, Request::Model(model.model), body);
                self.shared.accept(emit, &id, false);
            }
            claude_sdk_input::Of::Effort(effort) => {
                self.control_request(
                    emit,
                    Request::Effort(effort.effort.clone()),
                    ControlRequestBody::ApplyFlagSettings(ApplyFlagSettingsRequest {
                        settings: FlagSettings {
                            effort_level: Some(effort.effort),
                            extensions: Default::default(),
                        },
                        extensions: Default::default(),
                    }),
                );
                self.shared.accept(emit, &id, false);
            }
            claude_sdk_input::Of::Answer(answer) => self.answer(emit, &id, answer),
        }
    }

    /// The open plan ask, if any: its key and its call.
    fn plan_ask(&self) -> Option<(String, String)> {
        self.asks
            .iter()
            .find(|(_, meta)| matches!(meta.shape, AskShape::Plan { .. }))
            .map(|(key, meta)| (key.clone(), meta.tool_use_id.clone()))
    }

    /// Answers the open plan ask without the person deciding it.
    fn close_plan(
        &mut self,
        emit: &mut Emit,
        key: &str,
        tool_use_id: &str,
        result: PermissionResult,
        verdict: Verdict,
    ) {
        self.asks.remove(key);
        self.shared.close_ask(key);
        emit.effect(write(&claude_stream::Input::ControlResponse(
            ControlResponse::success(key, &result),
        )));
        self.decide(emit, tool_use_id, Decided::Plan(verdict));
    }

    /// A prompt typed instead of deciding the open plan: the plan is
    /// dismissed and its turn ends, so the prompt starts the next one.
    fn dismiss_plan(&mut self, emit: &mut Emit) {
        let Some((key, tool_use_id)) = self.plan_ask() else {
            return;
        };
        self.interrupted = true;
        let result = PermissionResult::Deny {
            message: REJECTED.to_owned(),
            interrupt: Some(true),
            tool_use_id: None,
        };
        let verdict = Verdict::of(wire::PlanVerdict::Dismissed);
        self.close_plan(emit, &key, &tool_use_id, result, verdict);
    }

    /// The person moved the agent out of plan while its plan waited: the
    /// plan is approved with that permission. False when no plan waited.
    fn leave_plan(&mut self, emit: &mut Emit, permission: &str) -> bool {
        let Some((key, tool_use_id)) = self.plan_ask() else {
            return false;
        };
        let input = self
            .asks
            .get(&key)
            .and_then(|meta| serde_json::from_str::<Value>(&meta.input).ok());
        let result = PermissionResult::Allow {
            updated_input: input,
            updated_permissions: Some(vec![PermissionUpdate::SetMode {
                mode: PermissionMode::parse(permission),
                destination: PermissionUpdateDestination::Session,
            }]),
            tool_use_id: None,
        };
        let verdict = Verdict::approved_for(Some(permission));
        self.close_plan(emit, &key, &tool_use_id, result, verdict);
        true
    }

    /// Cancels the running turn, which also drops an open ask's tool call.
    fn interrupt(&mut self, emit: &mut Emit) {
        if self.shared.is_busy() || !self.shared.asks().is_empty() {
            self.interrupted = true;
            self.control_request(
                emit,
                Request::Interrupt,
                ControlRequestBody::Interrupt(InterruptRequest::default()),
            );
        }
    }

    /// Hands a prompt to Claude. Its item is written now: the uuid it
    /// carries is the key Claude's own record of it uses.
    fn submit(&mut self, emit: &mut Emit, entry: wire::QueuedInput) {
        if self.early_boundary.is_none()
            && let Some(kind) = self.coming_boundary()
        {
            self.early_boundary = Some(self.boundary(emit, kind, wire::IncarnationEnd::default()));
        }
        let uuid = client_uuid(&entry.input_id);
        if entry.text.starts_with('/') {
            self.slash = Some(entry.text.clone());
        }
        self.clients.insert(
            uuid.clone(),
            Client {
                id: entry.input_id.clone(),
                message: false,
            },
        );
        emit.effect(Effect::UserMessage {
            uuid: uuid.clone(),
            text: crate::shared::provider_text(&entry),
            attachments: entry.attachments.clone(),
        });
        self.shared.item(
            emit,
            ItemDraft {
                key: uuid,
                text: entry.text,
                attachments: entry.attachments,
                input_id: entry.input_id,
                body: item_body(claude_sdk_item::Kind::Prompt(wire::Prompt {})),
                complete: true,
                ..Default::default()
            },
        );
    }

    /// An answer through amux: checked against the ask, then sent as the
    /// control response. The ask closes with the outcome it carries.
    fn answer(&mut self, emit: &mut Emit, id: &[u8], answer: ClaudeAnswer) {
        let key = answer.ask_key;
        if self.shared.asks().get(&key).is_none() {
            self.shared.answer(emit, id, &key);
            return;
        }
        let parsed = answer.of;
        let closed = match &parsed {
            Some(claude_answer::Of::Form(form)) => {
                let schema = match self
                    .shared
                    .asks()
                    .get(&key)
                    .and_then(|ask| ask.body.as_ref())
                {
                    Some(wire::ask::Body::Form(asked)) => asked.schema_json.as_slice(),
                    _ => &[],
                };
                Some(ask_item::form_sent(form, schema))
            }
            Some(claude_answer::Of::Link(link)) => Some(ask_item::link_answered(link.action)),
            _ => None,
        };
        let asked = match self
            .shared
            .asks()
            .get(&key)
            .and_then(|ask| ask.body.as_ref())
        {
            Some(wire::ask::Body::Question(asked)) => Some(asked.clone()),
            _ => None,
        };
        let Some((response, decision)) = self
            .asks
            .get(&key)
            .and_then(|meta| sdk_answer(meta, asked.as_ref(), parsed?))
        else {
            return self.shared.reject(emit, id, Refusal::Unsupported);
        };
        let ask = self.shared.answer(emit, id, &key);
        let meta = self.asks.remove(&key);
        emit.effect(write(&claude_stream::Input::ControlResponse(
            match response {
                SdkAnswer::Permission(result) => ControlResponse::success(&key, &result),
                SdkAnswer::Elicitation(result) => ControlResponse::success(&key, &result),
            },
        )));
        if let (Some(meta), Some(decided)) = (meta, decision) {
            self.decide(emit, &meta.tool_use_id, decided);
        }
        if let (Some(ask), Some(closed)) = (ask, closed) {
            self.emit_ask(emit, &ask, Some(closed));
        }
        self.shared.accept(emit, id, false);
    }

    /// Writes the item of a tool-server form or link, open or closed: an ask
    /// that is the work. A question or plan is drawn on its own tool call.
    fn emit_ask(&mut self, emit: &mut Emit, ask: &Ask, closed: Option<AskClosed>) {
        let asked = match &ask.body {
            Some(wire::ask::Body::Form(form)) => wire::ask_item::Ask::Form(form.clone()),
            Some(wire::ask::Body::Link(link)) => wire::ask_item::Ask::Link(link.clone()),
            _ => return,
        };
        let item = ask_item::opened(asked);
        let item = match closed {
            Some(closed) => ask_item::close(item, closed),
            None => item,
        };
        self.shared.item(
            emit,
            ItemDraft {
                key: ask.item_key.clone(),
                body: item_body(claude_sdk_item::Kind::Ask(item)),
                at_ms: Some(ask.opened_at_ms),
                complete: true,
                ..Default::default()
            },
        );
    }

    /// Closes every open ask without an answer: the turn or the provider
    /// ended under it.
    fn dismiss_asks(&mut self, emit: &mut Emit) {
        for ask in self.shared.close_all_asks() {
            self.dismissed(emit, &ask);
        }
    }

    /// Claude withdrew a request it had asked: an interrupt cancels an
    /// open permission request, and Claude refuses the call.
    pub(super) fn request_cancelled(&mut self, emit: &mut Emit, request_id: &str) {
        if let Some(ask) = self.shared.close_ask(request_id) {
            self.dismissed(emit, &ask);
        }
    }

    /// An ask closed with no answer. Its own item says so; a call it held
    /// that is still in flight will never run, so it reads cancelled until
    /// Claude says more.
    fn dismissed(&mut self, emit: &mut Emit, ask: &Ask) {
        let meta = self.asks.remove(&ask.key);
        self.emit_ask(emit, ask, Some(ask_item::dismissed()));
        let Some(id) = meta.map(|meta| meta.tool_use_id) else {
            return;
        };
        let Some(tool) = self.tools.get_mut(&id) else {
            return;
        };
        if let Some(plan) = tool.plan.as_mut().filter(|plan| plan.undecided()) {
            *plan = Verdict::of(wire::PlanVerdict::Dismissed);
        }
        if tool.question && tool.asked.is_none() {
            tool.asked = Some(ask_item::dismissed());
        }
        if matches!(
            wire::ToolState::try_from(tool.state),
            Ok(wire::ToolState::Pending | wire::ToolState::Running)
        ) {
            tool.state = wire::ToolState::Cancelled as i32;
            tool.ended_at_ms.get_or_insert(self.shared.now_ms());
        }
        self.emit_tool(emit, &id);
    }

    fn decide(&mut self, emit: &mut Emit, tool_use_id: &str, decided: Decided) {
        if let Some(tool) = self.tools.get_mut(tool_use_id) {
            match decided {
                // A question cannot be declined: one refused anyway closed
                // without answers.
                Decided::Tool(_) if tool.question => {
                    tool.asked.get_or_insert_with(ask_item::dismissed);
                }
                Decided::Tool(decision) => {
                    if decision.outcome == DecisionOutcome::Allowed as i32 {
                        tool.started_at_ms.get_or_insert(self.shared.now_ms());
                    }
                    tool.decision = Some(decision)
                }
                Decided::Plan(verdict) => tool.plan = Some(verdict),
                Decided::Question(closed) => tool.asked = Some(closed),
            }
            self.emit_tool(emit, tool_use_id);
        }
    }

    fn emit_tool(&mut self, emit: &mut Emit, id: &str) {
        let Some(tool) = self.tools.get_mut(id) else {
            return;
        };
        if tool.hidden {
            return;
        }
        if tool.question {
            let Some(item) = question_item(&tool.input, tool.asked.clone(), false) else {
                return;
            };
            let body = item_body(claude_sdk_item::Kind::Ask(item));
            if body == tool.emitted {
                return;
            }
            tool.emitted = body.clone();
            let at_ms = tool.at_ms;
            return self.shared.item(
                emit,
                ItemDraft {
                    key: id.to_owned(),
                    body,
                    at_ms: Some(at_ms),
                    complete: true,
                    ..Default::default()
                },
            );
        }
        if let Some(verdict) = &tool.plan {
            let Some((text, plan)) = plan_item(&tool.input, verdict) else {
                return;
            };
            let body = item_body(claude_sdk_item::Kind::Plan(plan));
            if body == tool.emitted {
                return;
            }
            tool.emitted = body.clone();
            let at_ms = tool.at_ms;
            return self.shared.item(
                emit,
                ItemDraft {
                    key: id.to_owned(),
                    text,
                    body,
                    at_ms: Some(at_ms),
                    complete: true,
                    ..Default::default()
                },
            );
        }
        if is_send_tool(&tool.server, &tool.name) {
            let (text, message) = sent_message(
                tool.input.as_bytes(),
                SendOutcome::of(
                    wire::ToolState::try_from(tool.state).unwrap_or_default(),
                    &tool.outcome_text,
                ),
            );
            let body = item_body(claude_sdk_item::Kind::AgentMessage(message));
            if body == tool.emitted {
                return;
            }
            tool.emitted = body.clone();
            let at_ms = tool.at_ms;
            // The message's files go as attachments, not their elements.
            let (text, attachments) = crate::shared::parse_reply(text);
            return self.shared.item(
                emit,
                ItemDraft {
                    key: id.to_owned(),
                    text,
                    attachments,
                    body,
                    at_ms: Some(at_ms),
                    complete: true,
                    ..Default::default()
                },
            );
        }
        let (created, line) = if tool.state == ToolState::Succeeded as i32 {
            file_step_facts(&tool.name, &tool.server, &tool.outcome_json)
        } else {
            (None, None)
        };
        let body = item_body(claude_sdk_item::Kind::Tool(ToolCall {
            name: tool.name.clone(),
            input_json: tool.input.clone().into_bytes(),
            state: tool.state,
            outcome_text: tool.outcome_text.clone(),
            outcome_json: tool.outcome_json.clone().into_bytes(),
            attachments: tool.images.clone(),
            class: tool.class,
            decision: tool.decision.as_ref().map(|decision| ToolDecision {
                outcome: decision.outcome,
                note: decision.note.clone(),
                elsewhere: false,
                granted: decision.grant.clone().map(tool_decision::Granted::Claude),
            }),
            background: tool.background,
            parent_key: tool.parent_key.clone(),
            subagent: tool.task.as_ref().filter(|task| task.subagent).map(|task| {
                wire::SubagentProgress {
                    tool_count: task.tool_count,
                    last_tool: task.last_tool.clone(),
                    finished: task.finished,
                }
            }),
            server: tool.server.clone(),
            exit_code: None,
            ended_at_ms: tool.ended_at_ms,
            created,
            line,
            started_at_ms: tool.started_at_ms,
        }));
        if body == tool.emitted {
            return;
        }
        tool.emitted = body.clone();
        let at_ms = tool.at_ms;
        let attachments = tool.attached.clone();
        self.shared.item(
            emit,
            ItemDraft {
                key: id.to_owned(),
                body,
                attachments,
                at_ms: Some(at_ms),
                complete: true,
                ..Default::default()
            },
        );
    }

    /// The boundary the next init makes when it is known before that init
    /// arrives: a new process's first, or a restart's.
    fn coming_boundary(&self) -> Option<wire::BoundaryKind> {
        if self.inits == 0 {
            Some(if self.incarnation > 1 {
                wire::BoundaryKind::Resumed
            } else {
                wire::BoundaryKind::Started
            })
        } else if self.exited {
            Some(wire::BoundaryKind::Restarted)
        } else {
            None
        }
    }

    /// Writes a new boundary item; returns its key.
    fn boundary(
        &mut self,
        emit: &mut Emit,
        kind: wire::BoundaryKind,
        exit: wire::IncarnationEnd,
    ) -> String {
        self.next_boundary += 1;
        let key = format!("boundary:{}", self.next_boundary);
        self.boundary_at(emit, key.clone(), kind, exit);
        key
    }

    fn boundary_at(
        &mut self,
        emit: &mut Emit,
        key: String,
        kind: wire::BoundaryKind,
        exit: wire::IncarnationEnd,
    ) {
        self.shared.item(
            emit,
            ItemDraft {
                key,
                body: item_body(claude_sdk_item::Kind::Boundary(wire::Boundary {
                    kind: kind as i32,
                    provider_session: self.session.clone().unwrap_or_default(),
                    provider_version: self.version.clone().unwrap_or_default(),
                    cause: exit.cause,
                    cause_detail: exit.detail,
                    keymap: String::new(),
                })),
                complete: true,
                ..Default::default()
            },
        );
    }
}

impl TaskState {
    fn to_wire(&self, id: &str) -> wire::Task {
        wire::Task {
            task_id: id.to_owned(),
            description: self.description.clone(),
            state: self.state,
            tool_count: self.tool_count,
            last_tool: self.last_tool.clone(),
            tokens: self.tokens,
        }
    }
}

/// Claude's own words for a tool use the person rejected.
const REJECTED: &str = "The user doesn't want to proceed with this tool use. The tool use was rejected (eg. if it was a file edit, the new_string was NOT written to the file).";

/// The control response an answer sends and the decision it puts on the
/// call's row, or None when the answer does not fit the ask.
fn sdk_answer(
    meta: &AskMeta,
    asked: Option<&wire::QuestionAsk>,
    answer: claude_answer::Of,
) -> Option<(SdkAnswer, Option<Decided>)> {
    let input = serde_json::from_str::<Value>(&meta.input).unwrap_or(Value::Null);
    let decided = |outcome: DecisionOutcome, grant: Option<ClaudeGrant>, note: &str| {
        Some(Decided::Tool(ToolDecisionState {
            outcome: outcome as i32,
            grant,
            note: note.to_owned(),
        }))
    };
    let allow = |input: Value, permissions: Option<Vec<PermissionUpdate>>| {
        SdkAnswer::Permission(PermissionResult::Allow {
            updated_input: Some(input),
            updated_permissions: permissions,
            tool_use_id: None,
        })
    };
    // Claude hands the message to the model as the tool's error. A bare
    // note there reads as a hook's refusal; worded as Claude words a
    // person's rejection in its own terminal, it reads as the person's.
    let deny = |note: &str, stop: bool| {
        let mut message = REJECTED.to_owned();
        if !note.is_empty() {
            message.push_str(" To tell you how to proceed, the user said:\n");
            message.push_str(note);
        }
        SdkAnswer::Permission(PermissionResult::Deny {
            message,
            interrupt: Some(stop),
            tool_use_id: None,
        })
    };
    match (&meta.shape, answer) {
        (AskShape::Permission, claude_answer::Of::Permission(permission)) => match permission.of? {
            permission_answer::Of::Allow(chosen) => {
                let mut permissions = None;
                let mut grant = None;
                if let Some(index) = chosen.scope {
                    let suggestion = serde_json::from_str::<PermissionUpdate>(
                        meta.suggestions.get(index as usize)?,
                    )
                    .ok()?;
                    let scopes = permission_scopes(std::slice::from_ref(&suggestion));
                    grant = scopes.first().map(claude_grant);
                    permissions = Some(vec![suggestion]);
                }
                Some((
                    allow(input, permissions),
                    decided(DecisionOutcome::Allowed, grant, ""),
                ))
            }
            permission_answer::Of::Deny(no) => Some((
                deny(&no.note, no.stop),
                decided(DecisionOutcome::Denied, None, &no.note),
            )),
        },
        (AskShape::Plan { switch }, claude_answer::Of::Plan(plan)) => {
            let start = |mode: PermissionMode, verdict: Verdict| {
                let update = PermissionUpdate::SetMode {
                    mode,
                    destination: PermissionUpdateDestination::Session,
                };
                Some((
                    allow(input.clone(), Some(vec![update])),
                    Some(Decided::Plan(verdict)),
                ))
            };
            match wire::PlanChoice::try_from(plan.choice).ok()? {
                wire::PlanChoice::Start => start(
                    PermissionMode::Default,
                    Verdict::of(wire::PlanVerdict::Approved),
                ),
                // The switch the ask offered, the one the person was shown.
                wire::PlanChoice::StartSwitching if !switch.is_empty() => {
                    start(PermissionMode::parse(switch), Verdict::switching(switch))
                }
                wire::PlanChoice::StartSwitching => None,
                wire::PlanChoice::KeepPlanning => {
                    let note = plan.note.unwrap_or_default();
                    Some((
                        deny(&note, false),
                        Some(Decided::Plan(Verdict::sent_back(&note))),
                    ))
                }
                wire::PlanChoice::Unspecified => None,
            }
        }
        (AskShape::Question(questions), claude_answer::Of::Question(answer)) => {
            if answer.answers.len() != questions.len() {
                return None;
            }
            // A skipped question is left out of the answers, as Claude's
            // own interface does; Claude takes notes per question, by its
            // text, whether or not it was answered.
            let mut answers = BTreeMap::new();
            let mut annotations = BTreeMap::new();
            for ((question, labels, multi), response) in questions.iter().zip(&answer.answers) {
                let mut picked = Vec::new();
                for index in &response.selected {
                    picked.push(labels.get(*index as usize)?.clone());
                }
                picked.extend(response.other.clone());
                if !multi && picked.len() > 1 {
                    return None;
                }
                if !picked.is_empty() {
                    answers.insert(question.as_str(), picked.join(", "));
                }
                if let Some(note) = response
                    .note
                    .as_deref()
                    .filter(|note| !note.trim().is_empty())
                {
                    annotations.insert(question.as_str(), QuestionNote { notes: note });
                }
            }
            let updated = AnsweringInput {
                input: match input {
                    Value::Object(input) => input,
                    _ => serde_json::Map::new(),
                },
                answers,
                annotations,
            };
            let closed = ask_item::answered(asked?, &answer)?;
            Some((
                allow(
                    serde_json::to_value(updated).expect("an answered input serializes"),
                    None,
                ),
                Some(Decided::Question(closed)),
            ))
        }
        // The question is refused with the person's own words, which
        // Claude hands the model as the call's result.
        (AskShape::Question(questions), claude_answer::Of::Reply(reply)) => {
            if reply.text.trim().is_empty()
                || (!reply.answers_so_far.is_empty()
                    && reply.answers_so_far.len() != questions.len())
            {
                return None;
            }
            let closed = ask_item::replied(asked?, &reply)?;
            Some((
                SdkAnswer::Permission(PermissionResult::Deny {
                    message: reply.text,
                    interrupt: Some(false),
                    tool_use_id: None,
                }),
                Some(Decided::Question(closed)),
            ))
        }
        (AskShape::Form, claude_answer::Of::Form(form)) => {
            let result = match FormAction::try_from(form.action).ok()? {
                FormAction::Accept => ElicitationResult::Accept {
                    content: Some(
                        serde_json::from_slice(&form.content_json)
                            .unwrap_or_else(|_| Value::Object(serde_json::Map::new())),
                    ),
                    extensions: Default::default(),
                },
                action => form_action(action)?,
            };
            Some((SdkAnswer::Elicitation(result), None))
        }
        (AskShape::Link, claude_answer::Of::Link(link)) => Some((
            SdkAnswer::Elicitation(form_action(FormAction::try_from(link.action).ok()?)?),
            None,
        )),
        _ => None,
    }
}

/// The answer an action gives a tool server's form or link, without content.
fn form_action(action: FormAction) -> Option<ElicitationResult> {
    let extensions = Default::default();
    Some(match action {
        FormAction::Accept => ElicitationResult::Accept {
            content: None,
            extensions,
        },
        FormAction::Decline => ElicitationResult::Decline { extensions },
        FormAction::Cancel => ElicitationResult::Cancel { extensions },
        FormAction::Unspecified => return None,
    })
}

impl Checkpoint for State {
    fn resume(mut self) -> (Self, Step) {
        let body = self.body();
        let step = self.shared.resume_step(body);
        (self, step)
    }
}

impl Interpreter for ClaudeSdk {
    type State = State;
    const KIND: wire::Kind = wire::Kind::ClaudeSdk;

    fn initial(spec: &AgentSpec, producer_version: &str) -> (State, Step) {
        let mut state = State::new(spec, producer_version);
        let step = state.shared.initial_step(Self::unknown_snapshot());
        (state, step)
    }

    fn reincarnate(mut state: State, spec: &AgentSpec, producer_version: &str) -> (State, Step) {
        state.shared.reincarnate(spec, producer_version);
        state.jobs.clear();
        // A new process: its first init is the incarnation's boundary.
        state.incarnation = spec.incarnation;
        state.inits = 0;
        state.exited = false;
        state.early_boundary = None;
        state.effort = launch_arg(&spec.provider_args, "--effort");
        state.resume()
    }

    fn step(state: &mut State, event: Event) -> Stepped {
        let mut emit = Emit::default();
        match event {
            Event::Tick { at_ms } => state.shared.tick(at_ms),
            Event::Fact(fact) => state.fact(&mut emit, fact),
            Event::Input(input) => state.input(&mut emit, input),
            Event::AgentMessage(envelope) => state.agent_message(&mut emit, envelope),
            // Claude has no name of its own to take.
            Event::Rename { .. } => {}
            Event::ProviderExit { code } => state.exited(&mut emit, crate::provider_exit(code)),
            Event::Exiting(exiting) => state.exited(&mut emit, exiting),
            Event::DaemonLost => {
                state.boundary(
                    &mut emit,
                    wire::BoundaryKind::DaemonLost,
                    wire::IncarnationEnd::default(),
                );
            }
            Event::StopRequested => {
                state.shared.stop_requested();
                state.interrupt(&mut emit);
            }
            Event::Git(git) => state.shared.set_git(git),
        }
        if let Some(entry) = state.shared.next_queued() {
            state.submit(&mut emit, entry);
        }
        let body = state.body();
        state.shared.finish(emit, body)
    }

    fn redact(target: RedactTarget) -> RedactTarget {
        crate::redact::redact_kind::<
            wire::ClaudeSdkItem,
            wire::ClaudeSdkSnapshot,
            wire::ClaudeSdkInput,
            State,
        >(target)
    }

    fn pending_messages(state: &Self::State) -> Vec<Vec<u8>> {
        state.shared.pending_messages().iter().cloned().collect()
    }

    fn typed_unrecorded(state: &Self::State) -> bool {
        state.shared.typed_unrecorded()
    }

    fn unknown_snapshot() -> Vec<u8> {
        unknown::claude_sdk().encode_to_vec()
    }

    fn describe_item(body: &[u8]) -> ItemView {
        describe_item(body)
    }

    fn describe_snapshot(body: &[u8]) -> SnapshotView {
        describe_snapshot(body)
    }

    fn fixture_input(input_id: Vec<u8>, input: &FixtureInput) -> Option<Input> {
        claude_sdk_input(input_id, input)
    }

    fn recording(format: &str, bytes: &[u8]) -> Result<Vec<Event>, String> {
        recording::read(format, bytes)
    }
}

fn describe_item(body: &[u8]) -> ItemView {
    use claude_sdk_item::Kind;
    let item = ClaudeSdkItem::decode(body).unwrap_or_default();
    let (arm, complete, text) = match item.kind {
        None => ("none", true, String::new()),
        Some(Kind::Prompt(_)) => ("prompt", true, String::new()),
        Some(Kind::Steer(_)) => ("steer", true, String::new()),
        Some(Kind::Message(text)) => ("message", text.complete, String::new()),
        Some(Kind::Thinking(thinking)) => ("thinking", thinking.complete, String::new()),
        Some(Kind::Tool(tool)) => ("tool", true, describe_tool(&tool)),
        Some(Kind::Ask(item)) => ("ask", true, ask_item::describe(&item)),
        Some(Kind::Plan(plan)) => ("plan", plan.complete, crate::shared::describe_plan(&plan)),
        Some(Kind::Task(task)) => (
            "task",
            true,
            format!(
                "{} {} {} tools={} last={} tokens={}",
                task.task_id,
                wire::TaskState::try_from(task.state).map_or("?", |state| state.as_str_name()),
                Value::String(task.description),
                task.tool_count,
                or_dash(&task.last_tool),
                task.tokens,
            ),
        ),
        Some(Kind::Turn(turn)) => (
            "turn",
            true,
            format!(
                "#{} {} started={} cost={}",
                turn.turn_id,
                wire::TurnOutcome::try_from(turn.outcome)
                    .map_or("?", |outcome| outcome.as_str_name()),
                turn.started_at_ms,
                turn.cost_usd
                    .map_or("-".to_owned(), |cost| format!("{cost:.6}"))
            ),
        ),
        Some(Kind::Status(status)) => (
            "status",
            true,
            format!(
                "{} {}",
                wire::SdkStatus::try_from(status.status).map_or("?", |status| status.as_str_name()),
                status.word
            ),
        ),
        Some(Kind::Boundary(boundary)) => (
            "boundary",
            true,
            format!(
                "{} session={} version={}{}",
                wire::BoundaryKind::try_from(boundary.kind).map_or("?", |kind| kind.as_str_name()),
                or_dash(&boundary.provider_session),
                or_dash(&boundary.provider_version),
                crate::claude_common::describe_cause(&boundary)
            ),
        ),
        Some(Kind::AgentMessage(message)) => (
            "agent_message",
            true,
            crate::shared::describe_agent_message(&message),
        ),
        Some(Kind::ApiError(error)) => (
            "api_error",
            true,
            format!(
                "{} {} retry={} {}/{}{}",
                error.error_kind,
                Value::String(error.message),
                error.will_retry,
                error.attempt,
                error.max_attempts,
                error
                    .retry_at_ms
                    .map_or(String::new(), |at| format!(" at={at}"))
            ),
        ),
        Some(Kind::Slash(slash)) => (
            "slash",
            true,
            format!("{} args={}", slash.command, Value::String(slash.args)),
        ),
        Some(Kind::Unrecognized(unrecognized)) => (
            "unrecognized",
            true,
            format!(
                "{} {}",
                unrecognized.fact_type,
                Value::String(unrecognized.summary)
            ),
        ),
        Some(Kind::ModelSwitch(switch)) => (
            "model_switch",
            true,
            format!(
                "{} -> {} {}",
                switch.from,
                switch.to,
                Value::String(switch.reason)
            ),
        ),
        Some(Kind::Compaction(compaction)) => (
            "compaction",
            true,
            format!(
                "{}→{}{}",
                compaction
                    .tokens_before
                    .map_or("?".to_owned(), |tokens| tokens.to_string()),
                compaction
                    .tokens_after
                    .map_or("?".to_owned(), |tokens| tokens.to_string()),
                if compaction.automatic { " auto" } else { "" }
            ),
        ),
    };
    ItemView {
        arm: arm.into(),
        complete,
        text,
    }
}

fn describe_snapshot(body: &[u8]) -> SnapshotView {
    let snapshot = ClaudeSdkSnapshot::decode(body).unwrap_or_default();
    let context = snapshot.context.unwrap_or_default();
    let usage = snapshot.usage.unwrap_or_default();
    let servers = snapshot.servers.unwrap_or_default();
    let sign_in = snapshot.sign_in.unwrap_or_default();
    SnapshotView {
        asks: snapshot
            .asks
            .iter()
            .map(|ask| (ask.key.clone(), ask.item_key.clone()))
            .collect(),
        text: format!(
            "asks=[{}] session={} model={} model_name={} effort={} mode={} context={} tasks={} active=[{}] usage={} servers={} sign_in={} background={}",
            describe_asks(&snapshot.asks),
            snapshot.provider_session.as_deref().unwrap_or("?"),
            snapshot.model.as_deref().unwrap_or("?"),
            crate::claude_common::describe_model_name(snapshot.model_name.as_deref()),
            snapshot.effort.as_deref().unwrap_or("?"),
            snapshot.permission.as_deref().unwrap_or("?"),
            if context.known {
                format!(
                    "{}/{}{}",
                    context.used_tokens,
                    context
                        .window_tokens
                        .map_or("?".to_owned(), |window| window.to_string()),
                    if context.breakdown.is_empty() {
                        String::new()
                    } else {
                        format!(" ({} categories)", context.breakdown.len())
                    }
                )
            } else {
                "?".into()
            },
            describe_tasks(&snapshot.tasks.unwrap_or_default()),
            snapshot
                .active_tasks
                .iter()
                .map(|task| task.task_id.clone())
                .collect::<Vec<_>>()
                .join(","),
            describe_claude_usage(&usage),
            match wire::HealthState::try_from(servers.state).unwrap_or_default() {
                wire::HealthState::Unknown => "?".to_owned(),
                state => format!(
                    "{}[{}]",
                    state.as_str_name(),
                    servers
                        .servers
                        .iter()
                        .map(|server| format!(
                            "{}:{}",
                            server.name,
                            wire::ToolServerStatus::try_from(server.status)
                                .map_or("?", |status| status.as_str_name())
                        ))
                        .collect::<Vec<_>>()
                        .join(",")
                ),
            },
            match wire::SignInState::try_from(sign_in.state).unwrap_or_default() {
                wire::SignInState::Unknown => "?".to_owned(),
                state => format!("{} {}", state.as_str_name(), or_dash(&sign_in.account)),
            },
            crate::shared::describe_jobs(snapshot.background_jobs.as_ref())
        ),
    }
}
