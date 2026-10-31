//! Claude through the SDK: headless `claude -p` speaking stream-JSON. The
//! facts are the lines Claude writes to stdout, events and control
//! requests alike, and the process's exit. Text, thinking and tool input
//! stream; asks arrive as named control requests, so nothing is inferred.
//!
//! Every user message the interpreter hands to Claude carries a uuid made
//! from its input or envelope id. Claude echoes it on the message's replay,
//! on its `command_lifecycle` frames and as its transcript row's uuid, so a
//! reflection is matched to what was sent by id; several messages written
//! while a turn runs fold into that turn, so arrival order would mismatch
//! them (Claude 2.1.282, probed).
//!
//! What the agent offers comes from Claude, never from a client catalogue:
//! the answer to the agent process's `initialize` request lists the models
//! (each with its effort levels) and the slash commands, and every later
//! snapshot carries them.

mod facts;
mod recording;

use std::collections::{BTreeMap, BTreeSet};

use claude_protocol::stream::control::{
    ApplyFlagSettingsRequest, InterruptRequest, SetModelRequest, SetPermissionModeRequest,
    StopTaskRequest,
};
use claude_protocol::stream::{
    self as claude_stream, ControlRequest, ControlRequestBody, ControlResponse, FlagSettings,
    PermissionMode, PermissionResult, PermissionUpdate, PermissionUpdateDestination,
};
use prost::Message as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use wire::{
    AgentSpec, Ask, AskClosed, ClaudeAnswer, ClaudeSnapshot, ClaudeUsage, DecisionOutcome,
    Envelope, Input, KindInput, OfferedCommand, OfferedModel, Refusal, SignIn, Step,
    ToolServerHealth, claude_answer, claude_item, claude_sdk_input,
};

use crate::claude_answers::{self, Answer, AnswerShape, Decided};
use crate::claude_common::{
    Jobs, Task, Verdict, agent_message_item, boundary_item, context_meter, item_body, launch_arg,
    task_list,
};
use crate::{
    Carrier, Checkpoint, Effect, Emit, Event, FixtureInput, Interpreter, ItemDraft, ItemView,
    RedactTarget, Shared, SnapshotView, Stepped, ask_item, claude_sdk_input, human, serde_pb,
    unknown,
};

/// The interpreter for kind `claude_sdk`.
pub struct ClaudeSdk;

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

type Tool = crate::claude_common::Tool<Call>;

/// What the stream says of a call beside what is drawn.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct Call {
    /// The task this call started and Claude reports on: a subagent, or a
    /// shell in the background. The call stays open until the task ends,
    /// since its own result only says the task was launched.
    task: Option<TaskProgress>,
}

/// What answering an ask needs beyond the wire Ask.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct AskMeta {
    tool_use_id: String,
    input: String,
    /// Each offered scope choice as Claude sent it, to send back.
    suggestions: Vec<String>,
    shape: AnswerShape,
    /// The subagent that asked; None for the main thread.
    agent: Option<String>,
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
    /// A stop for one background job, by its task id.
    StopJob(String),
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
    /// The folder Claude was launched in, which it may work in without
    /// being granted it.
    cwd: String,
    /// The models the initialize response offers and the commands Claude
    /// last listed: the catalogue.
    #[serde(with = "serde_pb::msgs")]
    models: Vec<OfferedModel>,
    #[serde(with = "serde_pb::msgs")]
    commands: Vec<OfferedCommand>,
    inits: u32,
    exited: bool,
    clients: BTreeMap<String, Client>,
    /// The uuids of the messages amux handed Claude that no result has
    /// answered yet.
    unanswered: BTreeSet<String>,
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
    /// The cleared boundary drawn at a reset, until the next init names the
    /// session it began: Claude's reset frame names an id it made up
    /// before it made the session.
    cleared: Option<String>,
    tasks: Option<Vec<Task>>,
    active_tasks: BTreeMap<String, TaskState>,
    context_tokens: Option<u64>,
    context_window: Option<u64>,
    #[serde(with = "serde_pb::opt_msg")]
    usage: Option<ClaudeUsage>,
    #[serde(with = "serde_pb::opt_msg")]
    servers: Option<ToolServerHealth>,
    /// Launched without amux's tool server, by organization policy.
    tools_withheld: bool,
    #[serde(with = "serde_pb::opt_msg")]
    sign_in: Option<SignIn>,
    /// Claude's background jobs; the shared part publishes them.
    jobs: Jobs,
    /// Tasks Claude marks ambient, its own work rather than the person's.
    ambient: std::collections::BTreeSet<String>,
    /// The newest slash command sent, which a local command's output
    /// belongs to; taken by that output.
    slash: Option<String>,
    /// A built-in command's answer drawn before Claude named its command:
    /// its key and text, for the command's replay to name it.
    synthetic: Option<(String, String)>,
    /// An interrupt was sent in the running turn.
    interrupted: bool,
}

/// A line for Claude's stdin.
fn write(input: &claude_stream::Input) -> Effect {
    Effect::ProviderWrite(claude_stream::encode(input))
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

    fn new(spec: &AgentSpec) -> Self {
        Self {
            shared: Shared::new(spec, ClaudeSdk::KIND),
            incarnation: spec.incarnation,
            session: None,
            model: None,
            chosen_model: launch_arg(&spec.provider_args, "--model"),
            // The launch argument until Claude says what it applied.
            effort: launch_arg(&spec.provider_args, "--effort"),
            permission_mode: None,
            auto_models: None,
            never_ask: crate::claude_common::allows_never_ask(&spec.provider_args),
            cwd: spec.cwd.clone(),
            models: Vec::new(),
            commands: Vec::new(),
            inits: 0,
            exited: false,
            clients: BTreeMap::new(),
            unanswered: BTreeSet::new(),
            stream: None,
            stream_tools: BTreeMap::new(),
            blocks: BTreeMap::new(),
            tools: BTreeMap::new(),
            asks: BTreeMap::new(),
            requests: BTreeMap::new(),
            next_request: 0,
            next_boundary: 0,
            early_boundary: None,
            cleared: None,
            tasks: None,
            active_tasks: BTreeMap::new(),
            context_tokens: None,
            context_window: None,
            usage: None,
            servers: None,
            tools_withheld: false,
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
        ClaudeSnapshot {
            asks: self.shared.asks().open_asks().to_vec(),
            tasks: Some(task_list(&self.tasks)),
            context: Some(context_meter(self.context_tokens, self.context_window)),
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
            servers: Some({
                let servers = self
                    .servers
                    .clone()
                    .unwrap_or_else(unknown::tool_server_health);
                if self.tools_withheld {
                    crate::claude_common::amux_tools_withheld(servers)
                } else {
                    servers
                }
            }),
            sign_in: Some(self.sign_in.clone().unwrap_or_else(unknown::sign_in)),
            background_jobs: Some(self.shared.jobs()),
            provider_session: self.session.clone(),
            running_calls: Vec::new(),
            switched_away: None,
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
        if self.shared.message_repeated(emit, &envelope.id) {
            return;
        }
        self.shared.message_accepted(&envelope.id);
        let item = agent_message_item(&envelope);
        self.clients.insert(
            client_uuid(&envelope.id),
            Client {
                id: envelope.id.clone(),
                message: true,
            },
        );
        self.unanswered.insert(client_uuid(&envelope.id));
        self.shared.message_handed();
        emit.effect(Effect::Inject {
            id: envelope.id.clone(),
            text: crate::shared::carried(&envelope),
            attachments: item.attachments.clone(),
            via: Carrier::Stdin,
        });
        self.shared.item(emit, item);
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
            // The job reads stopping until Claude says it ended, as a task
            // notification or a list without it.
            claude_sdk_input::Of::StopJob(stop) => {
                let Some(jobs) = self.jobs.stop_sent(&stop.key) else {
                    return self.shared.reject(emit, &id, Refusal::Unsupported);
                };
                self.shared.set_jobs(jobs);
                self.control_request(
                    emit,
                    Request::StopJob(stop.key.clone()),
                    ControlRequestBody::StopTask(StopTaskRequest {
                        task_id: stop.key,
                        extensions: Default::default(),
                    }),
                );
                self.shared.accept(emit, &id, false);
            }
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
                    self.unanswered.insert(uuid.clone());
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
            .find(|(_, meta)| matches!(meta.shape, AnswerShape::Plan { .. }))
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
        let verdict = Verdict::of(wire::PlanVerdict::Dismissed);
        self.close_plan(
            emit,
            &key,
            &tool_use_id,
            claude_answers::plan_dismissed(),
            verdict,
        );
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
        self.unanswered.insert(uuid.clone());
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
                body: item_body(claude_item::Kind::Prompt(wire::Prompt {})),
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
        let Some((response, decision)) = self.asks.get(&key).and_then(|meta| {
            claude_answers::decide(
                &meta.shape,
                &meta.input,
                &meta.suggestions,
                asked.as_ref(),
                parsed?,
            )
        }) else {
            return self.shared.reject(emit, id, Refusal::Unsupported);
        };
        let ask = self.shared.answer(emit, id, &key);
        let meta = self.asks.remove(&key);
        emit.effect(write(&claude_stream::Input::ControlResponse(
            match response {
                Answer::Permission(result) => ControlResponse::success(&key, &result),
                Answer::Elicitation(result) => ControlResponse::success(&key, &result),
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
                body: item_body(claude_item::Kind::Ask(item)),
                at_ms: Some(ask.opened_at_ms),
                complete: true,
                ..Default::default()
            },
        );
    }

    /// Closes every open ask without an answer: the provider ended under
    /// it.
    fn dismiss_asks(&mut self, emit: &mut Emit) {
        for ask in self.shared.close_all_asks() {
            self.dismissed(emit, &ask);
        }
    }

    /// Closes the asks a turn's end is the end of, and those `ended` says
    /// are over, without an answer. A subagent still running in the
    /// background keeps the asks it raised.
    fn dismiss_asks_but_background(&mut self, emit: &mut Emit) {
        let keys: Vec<String> = self
            .shared
            .asks()
            .open_asks()
            .iter()
            .map(|ask| ask.key.clone())
            .filter(|key| !self.asked_in_background(key))
            .collect();
        for key in keys {
            if let Some(ask) = self.shared.close_ask(&key) {
                self.dismissed(emit, &ask);
            }
        }
    }

    /// Whether the ask keyed `key` is a subagent's that still runs: one
    /// naming a task still running, or whose call hangs under one.
    fn asked_in_background(&self, key: &str) -> bool {
        let Some(meta) = self.asks.get(key) else {
            return false;
        };
        meta.agent.as_ref().is_some_and(|agent| {
            self.active_tasks
                .get(agent)
                .is_some_and(|task| task.state == wire::TaskState::Running as i32)
        }) || self.under_running_task(&meta.tool_use_id)
    }

    /// Whether the call `id`, or a call it hangs under, is one whose task
    /// still runs.
    fn under_running_task(&self, id: &str) -> bool {
        self.call_and_parents(id).iter().any(|id| {
            self.tools[*id]
                .of
                .task
                .as_ref()
                .is_some_and(|task| !task.finished)
        })
    }

    /// The keys of the call `id` and of each call it hangs under, nearest
    /// first, as far as they are held.
    fn call_and_parents<'a>(&'a self, id: &'a str) -> Vec<&'a str> {
        let mut chain = Vec::new();
        let mut next = id;
        // A chain of nested calls is short; the bound guards a cycle.
        while let Some(tool) = self.tools.get(next)
            && chain.len() < 16
        {
            chain.push(next);
            next = &tool.parent_key;
        }
        chain
    }

    /// The task `task` started by the call `call` ended: the asks its
    /// subagent raised are over, without an answer.
    fn task_asks_over(&mut self, emit: &mut Emit, task: &str, call: &str) {
        let keys: Vec<String> = self
            .shared
            .asks()
            .open_asks()
            .iter()
            .map(|ask| ask.key.clone())
            .filter(|key| {
                self.asks.get(key).is_some_and(|meta| {
                    meta.agent.as_deref() == Some(task)
                        || (!call.is_empty()
                            && meta.tool_use_id != call
                            && self.call_and_parents(&meta.tool_use_id).contains(&call))
                })
            })
            .collect();
        for key in keys {
            if let Some(ask) = self.shared.close_ask(&key) {
                self.dismissed(emit, &ask);
            }
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
                    tool.decision = Some(decision.to_wire())
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
        let subagent = tool
            .of
            .task
            .as_ref()
            .filter(|task| task.subagent)
            .map(|task| wire::SubagentProgress {
                tool_count: task.tool_count,
                last_tool: task.last_tool.clone(),
                finished: task.finished,
            });
        tool.emit(&mut self.shared, emit, id, subagent);
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
        self.shared.item(emit, boundary_item(key, kind, exit));
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

    fn initial(spec: &AgentSpec) -> (State, Step) {
        let mut state = State::new(spec);
        let step = state.shared.initial_step(Self::unknown_snapshot());
        (state, step)
    }

    fn reincarnate(mut state: State, spec: &AgentSpec) -> (State, Step) {
        state.shared.reincarnate(spec);
        state.jobs.clear();
        // A new process: its first init is the incarnation's boundary.
        state.incarnation = spec.incarnation;
        state.cwd = spec.cwd.clone();
        state.inits = 0;
        state.exited = false;
        state.early_boundary = None;
        // The agent says again at each launch whether policy withholds
        // amux's tools.
        state.tools_withheld = false;
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
                state.shared.leaving();
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
            wire::ClaudeItem,
            wire::ClaudeSnapshot,
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
        unknown::claude().encode_to_vec()
    }

    fn describe_item(body: &[u8]) -> ItemView {
        crate::claude_common::describe_item(body)
    }

    fn describe_snapshot(body: &[u8]) -> SnapshotView {
        crate::claude_common::describe_snapshot(body)
    }

    fn fixture_input(input_id: Vec<u8>, input: &FixtureInput) -> Option<Input> {
        claude_sdk_input(input_id, input)
    }

    fn recording(format: &str, bytes: &[u8]) -> Result<Vec<Event>, String> {
        recording::read(format, bytes)
    }
}
