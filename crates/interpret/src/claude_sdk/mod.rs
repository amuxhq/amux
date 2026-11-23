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
mod streamed;
mod subagents;

use std::collections::{BTreeMap, BTreeSet};

use claude_protocol::stream::control::{
    ApplyFlagSettingsRequest, InterruptRequest, SetModelRequest, SetPermissionModeRequest,
};
use claude_protocol::stream::{
    self as claude_stream, ControlRequest, ControlRequestBody, ControlResponse, FlagSettings,
    GenerateSessionTitleRequest, PermissionMode, PermissionResult, PermissionUpdate,
    PermissionUpdateDestination,
};
use prost::Message as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use wire::{
    AgentSpec, Ask, AskClosed, ClaudeAnswer, ClaudeSnapshot, ClaudeUsage, Envelope, Input,
    KindInput, OfferedCommand, OfferedModel, Refusal, SignIn, Step, ToolServer, claude_answer,
    claude_item, claude_sdk_input,
};

use crate::claude_answers::{self, Answer, AnswerShape, Decided};
use crate::claude_common::{
    Jobs, Task, Verdict, agent_message_item, boundary_item, context_meter, item_body, launch_arg,
    task_list,
};
use crate::{
    Carrier, Checkpoint, Effect, Emit, Event, FixtureInput, Interpreter, ItemDraft, ItemView,
    Shared, SnapshotView, Stepped, ask_item, claude_sdk_input, human, serde_pb, unknown,
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

/// One subagent Claude's Agent tool started, by Claude's id for it (its
/// task id, the `agent_id` on its frames and asks). Kept after it ends:
/// Claude's model can continue it, and a continuation names the call that
/// first started it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Sub {
    /// The Agent call that first started it.
    call: String,
    /// The subagent whose chat holds that call, for a nested one; None for
    /// one the agent's own conversation started.
    parent: Option<String>,
    /// What Claude calls its task, the row's title.
    title: String,
    started_at_ms: i64,
    /// Its start is written and its end is not.
    running: bool,
    /// A stop was sent and Claude has not said it ended.
    stopping: bool,
    /// Claude's refusal of the last stop.
    stop_error: String,
    /// The prompt its latest run's `task_started` drew, until Claude
    /// streams the same text as the subagent's own prompt, which is then not
    /// drawn again. A checkpoint from before this field existed reads it as
    /// None.
    #[serde(default)]
    prompt_drawn: Option<String>,
    conversation: crate::claude_subagent::Conversation,
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
    /// A title for the session.
    Title,
}

/// Whether a call's task ended.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct TaskProgress {
    finished: bool,
}

/// A shell call whose input Claude is still writing: its id and the
/// input's JSON as far as it has arrived.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct StreamingCall {
    id: String,
    name: String,
    json: String,
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
    /// A title request is out or has been answered with a title; a prompt
    /// asks only while neither holds.
    title_asked: bool,
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
    /// The message being streamed.
    stream: Option<String>,
    /// The shell calls of the message being streamed whose input is still
    /// arriving, by block index. A checkpoint from before this field
    /// existed reads it as empty.
    #[serde(default)]
    streaming_calls: BTreeMap<u32, StreamingCall>,
    /// Content blocks seen per assistant message, so whole blocks and
    /// streamed ones land on the same key.
    blocks: BTreeMap<String, u32>,
    tools: BTreeMap<String, Tool>,
    asks: BTreeMap<String, AskMeta>,
    /// The subagents Claude's Agent tool started, by Claude's id for each,
    /// while each runs and while a call row still waits on it; nothing of
    /// one is kept past that. A checkpoint from before they were agents of
    /// their own has none.
    #[serde(default)]
    subagents: BTreeMap<String, Sub>,
    /// The tasks a subagent's calls started (the shells it runs), by task
    /// id, and the subagent each is for: kept past the subagent's end
    /// until the task's own notice, which can come after it.
    #[serde(default)]
    owned_tasks: BTreeMap<String, String>,
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
    #[serde(with = "serde_pb::msgs")]
    servers: Vec<ToolServer>,
    /// Launched without amux's tool server, by organization policy.
    tools_withheld: bool,
    #[serde(with = "serde_pb::opt_msg")]
    sign_in: Option<SignIn>,
    /// The item key of the run of failed API attempts under way.
    retry_key: Option<String>,
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
    /// The main conversation's prompt cache lives an hour, as the newest
    /// request that wrote to it said; None before one has.
    #[serde(default)]
    cache_hour: Option<bool>,
    /// The `/compact` amux sent while the agent sat idle, until its result.
    #[serde(default)]
    idle_compaction: Option<IdleCompaction>,
    /// How many idle compactions this agent has sent, which names the next.
    #[serde(default)]
    idle_compactions: u64,
    /// When the main conversation last sent Claude a request, which keeps
    /// its cache alive for the hour from then.
    #[serde(default)]
    last_request_ms: Option<i64>,
}

/// How long headless Claude sits idle before amux compacts it, as Claude's
/// own terminal does: just inside the hour its prompt cache lives, so the
/// summary is written from the cache and the next prompt sends the summary
/// rather than the whole conversation uncached. Headless Claude does not do
/// this itself.
const IDLE_COMPACT_AFTER_MS: i64 = 55 * 60 * 1000;
/// How long the cache lives past the last request: an idle compaction
/// later than this would read the conversation uncached, which saves
/// nothing.
const CACHE_HOUR_MS: i64 = 60 * 60 * 1000;
/// The smallest conversation Claude's terminal compacts while idle.
const IDLE_COMPACT_MIN_TOKENS: u64 = 200_000;
/// How long an idle compaction may hold the queue without its result
/// before amux stops waiting for it: a result that named no message would
/// otherwise hold it forever.
const IDLE_COMPACT_WAIT_MS: i64 = 10 * 60 * 1000;

/// The `/compact` amux sent while the agent sat idle: its uuid, which
/// Claude's result names, and when it was sent.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct IdleCompaction {
    uuid: String,
    at_ms: i64,
}

/// A line for Claude's stdin.
fn write(input: &claude_stream::Input) -> Effect {
    Effect::ProviderWrite(claude_stream::encode(input))
}

/// What Claude offers on a host with no agent running, from its initialize
/// answer, and what a new agent would start with by Claude's own settings,
/// from that and `applied`. Never-ask is listed: a new agent can be created
/// allowing it.
pub fn host_catalogue(
    initialized: &claude_protocol::stream::InitializationResult,
    applied: Option<&claude_protocol::stream::init::AppliedSettings>,
) -> wire::Catalogue {
    use crate::claude_common::{auto_models, offered_commands, offered_models, permissions};
    wire::Catalogue {
        models: offered_models(&initialized.models),
        commands: offered_commands(&initialized.commands),
        permissions: permissions(Some(&auto_models(&initialized.models)), true, true),
        new_agent: Some(wire::NewAgentDefaults {
            model: applied.and_then(|applied| applied.model.clone()),
            effort: applied.and_then(|applied| applied.effort.clone()),
            permission: initialized
                .current_permission_mode
                .as_ref()
                .map(|mode| mode.as_str().to_owned()),
        }),
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
            title_asked: false,
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
            streaming_calls: BTreeMap::new(),
            blocks: BTreeMap::new(),
            tools: BTreeMap::new(),
            asks: BTreeMap::new(),
            subagents: BTreeMap::new(),
            owned_tasks: BTreeMap::new(),
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
            servers: Vec::new(),
            tools_withheld: false,
            sign_in: None,
            retry_key: None,
            jobs: Jobs::default(),
            slash: None,
            synthetic: None,
            ambient: Default::default(),
            interrupted: false,
            cache_hour: None,
            idle_compaction: None,
            idle_compactions: 0,
            last_request_ms: None,
        }
    }

    /// When an idle compaction falls due: the conversation is big enough,
    /// on the hour-long cache, and the agent has done nothing since its
    /// last turn. None once the cache has run out.
    fn idle_compact_at(&self) -> Option<i64> {
        if let Some(sent) = &self.idle_compaction {
            return self
                .shared
                .is_housekeeping()
                .then_some(sent.at_ms + IDLE_COMPACT_WAIT_MS);
        }
        let big = self
            .context_tokens
            .is_some_and(|tokens| tokens >= IDLE_COMPACT_MIN_TOKENS);
        if !big || self.cache_hour != Some(true) || self.session.is_none() {
            return None;
        }
        let last = self.last_request_ms?;
        let at = self.shared.idle_wake(last, IDLE_COMPACT_AFTER_MS)?;
        (self.shared.now_ms() < last + CACHE_HOUR_MS).then_some(at)
    }

    /// Sends `/compact` when an idle compaction is due. It is no person's
    /// prompt: nothing draws it, and no turn starts or ends for it.
    fn compact_if_idle(&mut self, emit: &mut Emit) {
        let now = self.shared.now_ms();
        if self.idle_compact_at().is_none_or(|at| at > now) {
            return;
        }
        if let Some(sent) = self.idle_compaction.take() {
            // No result yet: the queue stops waiting, and a result that
            // comes later answers nothing of anybody's.
            self.unanswered.remove(&sent.uuid);
            return self.shared.housekeeping_ended();
        }
        self.shared.housekeeping_started();
        self.idle_compactions += 1;
        let uuid = client_uuid(format!("idle-compact:{}", self.idle_compactions).as_bytes());
        self.unanswered.insert(uuid.clone());
        self.idle_compaction = Some(IdleCompaction {
            uuid: uuid.clone(),
            at_ms: now,
        });
        emit.effect(Effect::UserMessage {
            uuid,
            text: "/compact".into(),
            attachments: Vec::new(),
        });
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
            // A subagent is an agent of its own, drawn by its own row.
            active_tasks: self
                .active_tasks
                .iter()
                .filter(|(id, task)| {
                    task.state == wire::TaskState::Running as i32
                        && !task.shell
                        && !self.subagents.contains_key(*id)
                })
                .map(|(id, task)| task.to_wire(id))
                .collect(),
            usage: Some(self.usage.clone().unwrap_or_else(unknown::claude_usage)),
            servers: if self.tools_withheld {
                crate::claude_common::amux_tools_withheld(self.servers.clone())
            } else {
                self.servers.clone()
            },
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

    /// Asks Claude to title the session from the first prompt that says
    /// what it is about. A terminal Claude does this by itself.
    fn ask_title(&mut self, emit: &mut Emit, prompt: &str) {
        let prompt = prompt.trim();
        if self.title_asked
            || !self.shared.wants_title()
            || prompt.is_empty()
            || prompt.starts_with('/')
        {
            return;
        }
        self.title_asked = true;
        self.control_request(
            emit,
            Request::Title,
            ControlRequestBody::GenerateSessionTitle(GenerateSessionTitleRequest {
                description: prompt.to_owned(),
                persist: true,
                extensions: Default::default(),
            }),
        );
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
                if self.shared.refuse_unreadable(emit, &id, &prompt) {
                    return;
                }
                self.dismiss_plan(emit);
                if let Some(entry) = self.shared.admit_prompt(emit, &id, prompt, human()) {
                    self.submit(emit, entry);
                }
            }
            claude_sdk_input::Of::Withdraw(withdraw) => {
                self.shared.withdraw(emit, &id, &withdraw.queued_input_id)
            }
            claude_sdk_input::Of::StopJob(stop) => self.stop_job(emit, &id, &stop.key, None),
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
                    let prompt = text.clone();
                    emit.effect(Effect::UserMessage {
                        uuid,
                        text,
                        attachments: entry.attachments,
                    });
                    self.ask_title(emit, &prompt);
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
            claude_sdk_input::Of::Answer(answer) => self.answer(emit, &id, answer, None),
        }
    }

    /// The open plan ask, if any: its key and its call.
    fn plan_ask(&self) -> Option<(String, String)> {
        self.asks
            .iter()
            .find(|(_, meta)| {
                meta.agent.is_none() && matches!(meta.shape, AnswerShape::Plan { .. })
            })
            .map(|(key, meta)| (key.clone(), meta.tool_use_id.clone()))
    }

    /// The agent's permission once an allowance moved Claude's mode, as
    /// terminal Claude's interpreter records it: at once, confirmed by
    /// Claude's next report, so the snapshot a resume starts from says it
    /// whether or not Claude reports it before the agent suspends.
    fn moved_by(&mut self, result: &claude_protocol::stream::PermissionResult) {
        if let Some(mode) = claude_answers::mode_set(result) {
            self.permission_mode = Some(mode);
        }
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
        if claude_answers::for_session(&result) {
            self.shared.granted_for_session();
        }
        self.moved_by(&result);
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
        // A stop cancels an idle compaction too; its result ends it, and no
        // turn of anybody's was interrupted.
        if self.shared.is_housekeeping() {
            return self.control_request(
                emit,
                Request::Interrupt,
                ControlRequestBody::Interrupt(InterruptRequest::default()),
            );
        }
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
        self.boundary_above_prompt(emit);
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
        self.ask_title(emit, &entry.text);
        if !self.shared.drawn_early(&entry.input_id) {
            self.prompt_row(emit, entry);
        }
    }

    /// Draws the first prompt while Claude is still starting.
    fn draw_opening(&mut self, emit: &mut Emit) {
        if let Some(entry) = self.shared.opening_to_draw() {
            self.boundary_above_prompt(emit);
            self.prompt_row(emit, entry);
        }
    }

    /// The boundary Claude's init will fill in, written now so a prompt
    /// that goes in first is drawn below it.
    fn boundary_above_prompt(&mut self, emit: &mut Emit) {
        if self.early_boundary.is_none()
            && let Some(kind) = self.coming_boundary()
        {
            self.early_boundary = Some(self.boundary(emit, kind, wire::IncarnationEnd::default()));
        }
    }

    fn prompt_row(&mut self, emit: &mut Emit, entry: wire::QueuedInput) {
        self.shared.item(
            emit,
            ItemDraft {
                key: client_uuid(&entry.input_id),
                text: entry.text,
                attachments: entry.attachments,
                input_id: entry.input_id,
                body: item_body(claude_item::Kind::Prompt(wire::Prompt {})),
                complete: true,
                ..Default::default()
            },
        );
    }

    /// An answer through amux, to the agent's own ask or, with `who`, to one
    /// its subagent raised: checked against the ask, then sent as the
    /// control response. The ask closes with the outcome it carries.
    fn answer(&mut self, emit: &mut Emit, id: &[u8], answer: ClaudeAnswer, who: Option<&str>) {
        let key = answer.ask_key;
        let Some(open) = self.open_ask(who, &key) else {
            self.answered(emit, who, id, &key);
            return;
        };
        let parsed = answer.of;
        let closed = match &parsed {
            Some(claude_answer::Of::Form(form)) => {
                let schema = match &open.body {
                    Some(wire::ask::Body::Form(asked)) => asked.schema_json.as_slice(),
                    _ => &[],
                };
                Some(ask_item::form_sent(form, schema))
            }
            Some(claude_answer::Of::Link(link)) => Some(ask_item::link_answered(link.action)),
            _ => None,
        };
        let asked = match &open.body {
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
            return self.reject_as(emit, who, id, Refusal::Unsupported);
        };
        let ask = self.answered(emit, who, id, &key);
        let meta = self.asks.remove(&key);
        if let Answer::Permission(result) = &response {
            if claude_answers::for_session(result) {
                self.shared.granted_for_session();
            }
            // Claude's mode is its session's, whichever thread asked.
            self.moved_by(result);
        }
        emit.effect(write(&claude_stream::Input::ControlResponse(
            match response {
                Answer::Permission(result) => ControlResponse::success(&key, &result),
                Answer::Elicitation(result) => ControlResponse::success(&key, &result),
            },
        )));
        if let (Some(meta), Some(decided)) = (meta, decision) {
            self.decide_as(emit, who, &meta.tool_use_id, decided);
        }
        if let (Some(ask), Some(closed)) = (ask, closed) {
            self.emit_ask(emit, who, &ask, Some(closed));
        }
        self.accept_as(emit, who, id);
    }

    /// The open ask keyed `key` of the agent, or of its subagent `who`.
    fn open_ask(&self, who: Option<&str>, key: &str) -> Option<Ask> {
        match who {
            None => self.shared.asks().get(key).cloned(),
            Some(child) => self.shared.subagent_asks(child)?.get(key).cloned(),
        }
    }

    /// The stale-answer rule for an ask of the agent's or its subagent's:
    /// an open one closes and is handed back, one that is not is refused.
    fn answered(
        &mut self,
        emit: &mut Emit,
        who: Option<&str>,
        id: &[u8],
        key: &str,
    ) -> Option<Ask> {
        match who {
            None => self.shared.answer(emit, id, key),
            Some(child) => self.shared.subagent(emit, child).answer(id, key),
        }
    }

    fn accept_as(&mut self, emit: &mut Emit, who: Option<&str>, id: &[u8]) {
        match who {
            None => self.shared.accept(emit, id, false),
            Some(child) => self.shared.subagent(emit, child).accept(id),
        }
    }

    fn reject_as(&mut self, emit: &mut Emit, who: Option<&str>, id: &[u8], refusal: Refusal) {
        match who {
            None => self.shared.reject(emit, id, refusal),
            Some(child) => self.shared.subagent(emit, child).reject(id, refusal),
        }
    }

    /// The running subagent that raised the ask keyed `key`; None for the
    /// agent's own.
    fn asker(&self, key: &str) -> Option<String> {
        self.asks
            .get(key)
            .and_then(|meta| meta.agent.clone())
            .filter(|child| self.shared.subagent_running(child))
    }

    /// Closes an open ask of whichever agent raised it, without letting go
    /// of what answering it needs.
    fn close_open_ask(&mut self, emit: &mut Emit, key: &str) -> Option<Ask> {
        match self.asker(key) {
            None => self.shared.close_ask(key),
            Some(child) => self.shared.subagent(emit, &child).close_ask(key),
        }
    }

    /// Writes the item of a tool-server form or link, open or closed, into
    /// the chat of the agent that raised it: an ask that is the work. A
    /// question or plan is drawn on its own tool call.
    fn emit_ask(
        &mut self,
        emit: &mut Emit,
        who: Option<&str>,
        ask: &Ask,
        closed: Option<AskClosed>,
    ) {
        if !matches!(
            ask.body,
            Some(wire::ask::Body::Form(_) | wire::ask::Body::Link(_))
        ) {
            return;
        }
        let Some(draft) = crate::claude_subagent::ask_draft(ask, closed) else {
            return;
        };
        match who {
            None => self.shared.item(emit, draft),
            Some(child) => self.shared.subagent(emit, child).item(draft),
        }
    }

    /// Closes every open ask of the agent's own without an answer: the
    /// turn they were raised in is over.
    fn dismiss_own_asks(&mut self, emit: &mut Emit) {
        for ask in self.shared.close_all_asks() {
            self.dismissed(emit, None, &ask);
        }
    }

    /// Closes every open ask of the subagent `child` without an answer: it
    /// is ending.
    fn dismiss_subagent_asks(&mut self, emit: &mut Emit, child: &str) {
        if !self.shared.subagent_running(child) {
            return;
        }
        for ask in self.shared.subagent(emit, child).close_all_asks() {
            self.dismissed(emit, Some(child), &ask);
        }
    }

    /// Claude withdrew a request it had asked: an interrupt cancels an
    /// open permission request, and Claude refuses the call.
    pub(super) fn request_cancelled(&mut self, emit: &mut Emit, request_id: &str) {
        let who = self.asker(request_id);
        if let Some(ask) = self.close_open_ask(emit, request_id) {
            self.dismissed(emit, who.as_deref(), &ask);
        }
    }

    /// An ask of the agent's or of its subagent `who` closed with no
    /// answer. Its own item says so; a call it held that is still in
    /// flight will never run, so it reads cancelled until Claude says more.
    fn dismissed(&mut self, emit: &mut Emit, who: Option<&str>, ask: &Ask) {
        let meta = self.asks.remove(&ask.key);
        self.emit_ask(emit, who, ask, Some(ask_item::dismissed()));
        let Some(id) = meta.map(|meta| meta.tool_use_id) else {
            return;
        };
        let now = self.shared.now_ms();
        if let Some(child) = who {
            if let Some(sub) = self.subagents.get_mut(child) {
                let mut view = self.shared.subagent(emit, child);
                sub.conversation.dismissed(&mut view, &id, now);
            }
            return;
        }
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
            tool.ended_at_ms.get_or_insert(now);
        }
        self.emit_tool(emit, &id);
    }

    fn decide(&mut self, emit: &mut Emit, tool_use_id: &str, decided: Decided) {
        self.decide_as(emit, None, tool_use_id, decided);
    }

    /// A decision on a call of the agent's own, or of its subagent `who`.
    fn decide_as(
        &mut self,
        emit: &mut Emit,
        who: Option<&str>,
        tool_use_id: &str,
        decided: Decided,
    ) {
        let now = self.shared.now_ms();
        if let Some(child) = who {
            if let Some(sub) = self.subagents.get_mut(child) {
                let mut view = self.shared.subagent(emit, child);
                sub.conversation
                    .decide(&mut view, tool_use_id, decided, now);
            }
            return;
        }
        if let Some(tool) = self.tools.get_mut(tool_use_id) {
            crate::claude_subagent::apply_decided(tool, decided, now);
            self.emit_tool(emit, tool_use_id);
        }
    }

    fn emit_tool(&mut self, emit: &mut Emit, id: &str) {
        if let Some(tool) = self.tools.get_mut(id) {
            tool.emit(&mut self.shared, emit, id);
        }
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
        state.title_asked = false;
        state.jobs.clear();
        state.forget_subagents();
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
            Event::Tick { at_ms } => {
                state.shared.tick(at_ms);
                state.compact_if_idle(&mut emit);
            }
            Event::Fact(fact) => state.fact(&mut emit, fact),
            Event::Input(input) => state.input(&mut emit, input),
            Event::ChildInput { agent_id, input } => state.child_input(&mut emit, &agent_id, input),
            Event::AgentMessage(envelope) => state.agent_message(&mut emit, envelope),
            Event::Rename { .. } => state.shared.renamed(),
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
        state.draw_opening(&mut emit);
        if let Some(entry) = state.shared.next_queued() {
            state.submit(&mut emit, entry);
        }
        state.forget_ended_subagents();
        state.subagent_snapshots(&mut emit);
        let body = state.body();
        let notice = state.shared.notice(crate::notice::claude_ask);
        state.shared.finish(emit, body, notice)
    }

    fn pending_messages(state: &Self::State) -> Vec<Vec<u8>> {
        state.shared.pending_messages().iter().cloned().collect()
    }

    /// An idle compaction falls due at a time its last tick may already
    /// have passed, when the turn that left the agent idle ended long after
    /// its last request: the next millisecond's tick starts it then.
    fn wake_at(state: &Self::State) -> Option<i64> {
        state
            .idle_compact_at()
            .map(|at| at.max(state.shared.now_ms().saturating_add(1)))
    }

    fn housekeeping(state: &Self::State) -> crate::Housekeeping {
        if state.shared.is_housekeeping() {
            crate::Housekeeping::Running
        } else {
            state
                .idle_compact_at()
                .map_or(crate::Housekeeping::Settled, |at_ms| {
                    crate::Housekeeping::Due { at_ms }
                })
        }
    }

    fn typed_unrecorded(state: &Self::State) -> bool {
        state.shared.typed_unrecorded()
    }

    fn holds(state: &Self::State) -> crate::Holds {
        state.shared.holds()
    }

    fn fact_shape(fact: &crate::Fact) -> crate::shape::FactShape {
        crate::shape::fact(fact)
    }

    fn input_shape(input: &Input) -> crate::shape::InputShape {
        crate::shape::claude_sdk_input(&input.body)
    }

    fn structure(state: &Self::State) -> crate::shape::Structure {
        state.shared.structure(|body| Self::item_shape(body).arm)
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
