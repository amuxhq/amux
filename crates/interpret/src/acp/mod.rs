//! Agents that speak the Agent Client Protocol (ACP): JSON-RPC over the
//! agent's stdio, one session per agent. One interpreter serves every ACP
//! agent, built in or added by a person; what tells them apart is
//! [`Quirks`], chosen by the agent's id.
//!
//! The agent process does the handshake, as it does Codex's: `initialize`
//! under [`INITIALIZE_REQUEST`], then the session under
//! [`NEW_SESSION_REQUEST`], [`RESUME_REQUEST`] or [`LOAD_REQUEST`], picked
//! by [`opening`]. Everything the agent writes reaches the interpreter as a
//! fact, the answers to the handshake included; the interpreter writes the
//! rest: prompts, cancels, config changes and the answers to the agent's
//! asks.
//!
//! The interpreter holds the person's queue itself. ACP has one prompt
//! running per session, and agents differ on a second sent mid-turn (one
//! folds it into the turn, one runs both in one loop and answers both
//! together, one queues it), so a prompt waits in amux's queue until the
//! running prompt's answer, and only an agent known to fold one in is sent
//! one early.

mod facts;
mod offer;
mod recording;

use std::collections::BTreeMap;

use acp_protocol::agent::{InitializeResponse, PermissionOptionKind};
use acp_protocol::client::{
    PromptRequest, SessionRequest, SetSessionConfigOptionRequest, SetSessionModeRequest,
};
use acp_protocol::elicitation::{CreateElicitationResponse, ElicitationAccept, ElicitationValue};
use acp_protocol::session::{
    SessionConfigOption, SessionConfigOptionCategory, SessionModeState, SessionModelState,
};
use acp_protocol::update::ToolKind;
use acp_protocol::xai::{
    AskUserQuestionResponse, ExtensionAnswer, QuestionAnswer, QuestionOutcome,
};
use acp_protocol::{
    ClientMessage, ClientNotification, ClientRequest, ClientResponse, ContentBlock, Empty, Extra,
    RequestId, RpcError,
};
pub use offer::{HostOffer, host_offer};
use prost::Message as _;
use serde::{Deserialize, Serialize};
pub use wire::agents::{BUILT_INS, BuiltIn, built_in};
use wire::{
    AcpAnswer, AcpAsk, AcpItem, AcpSnapshot, AcpTool, AgentCapabilities, AgentSpec, AskItem,
    AskOutcome, Catalogue, ContextMeter, Envelope, FormAction, Input, KindInput, OfferedCommand,
    OfferedMode, OfferedModel, QueuedInput, Refusal, Step, TaskList, TaskListEntry, acp_answer,
    acp_ask, acp_input, acp_item, ask_item as wire_ask_item,
};

use crate::claude_common::{clip, describe_cause, or_dash};
use crate::serde_pb::{self, to_hex};
use crate::shared::{
    AMUX_TOOL_SERVER, EarlyBoundary, agent_message_body, agent_message_key, provider_text,
};
use crate::{
    Checkpoint, Effect, Emit, Event, FixtureInput, Interpreter, ItemDraft, ItemView, Shared,
    SnapshotView, ask_item, unknown,
};

/// The id the agent process gives its `initialize` request.
pub const INITIALIZE_REQUEST: &str = "agent-initialize";
/// The id of the agent process's `session/new`.
pub const NEW_SESSION_REQUEST: &str = "agent-session";
/// The id of the agent process's `session/resume`.
pub const RESUME_REQUEST: &str = "agent-resume";
/// The id of the agent process's `session/load`.
pub const LOAD_REQUEST: &str = "agent-load";

/// A prompt's request id: its input id, so an answer names the input it
/// ends.
pub fn prompt_request(input_id: &[u8]) -> String {
    format!("amux-prompt-{}", to_hex(input_id))
}

/// The id of the person's `n`th setting change in this agent's life.
pub fn set_request(n: u64) -> String {
    format!("amux-set-{n}")
}

/// The id of a setting amux applies itself when a session opens (the
/// agent's create config, or a pick an agent forgot on reopening): one per
/// config option, `mode` for a legacy mode.
pub fn apply_request(config_id: &str) -> String {
    format!("amux-apply-{config_id}")
}

/// How a new process opens the agent's conversation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Opening {
    /// The agent has no conversation yet: `session/new`.
    New,
    /// `session/resume`: the agent reopens it and replays nothing.
    Resume(String),
    /// `session/load`: the agent reopens it and replays it all first.
    Load(String),
    /// The agent has a conversation and cannot reopen it in a new process.
    /// The agent process exits for good with [`UNRESUMABLE`] as the cause:
    /// a fresh conversation is never passed off as the old one resumed.
    Unresumable,
}

/// The exit cause of an agent whose conversation no new process can
/// reopen.
pub const UNRESUMABLE: &str = "the agent cannot reopen its conversation in a new process";

/// How the agent process opens the session after `initialize`: the saved
/// one by `session/resume` where the agent offers it, which replays
/// nothing, else by `session/load`. With neither, a saved conversation is
/// over ([`Opening::Unresumable`]), which [`Interpreter::holds`] reports
/// while its process runs, so the agent never suspends on its own.
pub fn opening(init: &InitializeResponse, saved: Option<&str>) -> Opening {
    let Some(saved) = saved else {
        return Opening::New;
    };
    let reopens = Reopens::of(init);
    if reopens.resume {
        Opening::Resume(saved.to_owned())
    } else if reopens.load {
        Opening::Load(saved.to_owned())
    } else {
        Opening::Unresumable
    }
}

/// What an ACP agent can do, from what its process said at `initialize`
/// (None before it has answered, which reads as cannot) and its quirks.
fn capabilities(agent: &str, reopens: Option<Reopens>, takes: Option<Takes>) -> AgentCapabilities {
    AgentCapabilities::acp(
        reopens.is_some_and(|reopens| reopens.resume || reopens.load),
        takes.is_some_and(|takes| takes.images),
        quirks(agent).steers,
    )
}

/// What tells one ACP agent from another, where the protocol leaves room.
/// Chosen by the agent's id; an agent a person added gets the default,
/// which assumes nothing beyond ACP itself.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Quirks {
    /// A prompt sent while one runs is folded into the running turn: the
    /// running prompt is answered `end_turn` at once and what follows
    /// belongs to the later one. Only such an agent is sent a queued prompt
    /// early (send-now). Seen in `agy-mid_turn`; OpenCode answers both
    /// prompts together (`opencode-mid_turn`) and Grok queues the second
    /// itself (`grok-mid_turn`), so neither steers.
    pub steers: bool,
    /// The turn's tokens come in the `session/prompt` answer's `_meta`
    /// rather than ACP's `usage`. Seen in `grok-prompt`.
    pub usage_in_answer_meta: bool,
    /// The auth method `initialize` lists only while the agent holds a
    /// sign-in, so its presence says signed in and its absence signed out.
    /// Grok lists `cached_token` signed in (`grok-catalogue`) and not
    /// signed out (`grok-signed_out`).
    pub signed_in_method: Option<&'static str>,
    /// `initialize`'s `_meta` lists the models, with their efforts, and the
    /// commands a session would offer, so the host's copy has them without
    /// opening a session. Seen in `grok-catalogue`.
    pub offers_in_initialize: bool,
    /// How the agent names a call to a tool server's tool, by which a call
    /// to amux's own server is told from the rest.
    pub tool_naming: ToolNaming,
    /// The agent's question tool asks outside ACP, with Grok's
    /// `_x.ai/ask_user_question`: questions with options, answered by what
    /// was picked or typed under each question's text
    /// ([`acp_protocol::xai`]). Seen in `grok-question`; the answer's shape
    /// is what Grok took when answered live.
    pub xai_questions: bool,
}

/// How an agent names a tool server's tool on its calls. ACP gives a call
/// only a title, so each agent's own way is read; an agent a person added
/// is taken to use any of the ways seen, each of which names the server.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ToolNaming {
    /// Any of the ways below.
    #[default]
    Any,
    /// `mcp_<server>_<tool>` in the call's `name`, and `Run <that>` as its
    /// title (Antigravity, `agy-elicitation`).
    RunMcp,
    /// `<server>_<tool>` as the call's title (OpenCode,
    /// `opencode-elicitation`).
    Underscore,
    /// `<server>__<tool>`, as the `tool_name` of the `use_tool` call that
    /// runs it and then as the call's title (Grok, `grok-elicitation`).
    DoubleUnderscore,
}

impl ToolNaming {
    /// The tool of amux's own server that a call by this name runs, if it
    /// runs one.
    fn amux_tool(self, name: &str) -> Option<&'static str> {
        let server = AMUX_TOOL_SERVER;
        let forms: &[fn(&str, &str, &str) -> bool] = match self {
            Self::Any => &[run_mcp, underscore, double_underscore],
            Self::RunMcp => &[run_mcp],
            Self::Underscore => &[underscore],
            Self::DoubleUnderscore => &[double_underscore],
        };
        AMUX_TOOLS_UNASKED
            .iter()
            .copied()
            .find(|tool| forms.iter().any(|form| form(name, server, tool)))
    }
}

fn run_mcp(name: &str, server: &str, tool: &str) -> bool {
    let name = name.strip_prefix("Run ").unwrap_or(name);
    name == format!("mcp_{server}_{tool}")
}

fn underscore(name: &str, server: &str, tool: &str) -> bool {
    name == format!("{server}_{tool}")
}

fn double_underscore(name: &str, server: &str, tool: &str) -> bool {
    name == format!("{server}__{tool}")
}

/// amux's own tools that run without asking. `attach` asks, as it does in
/// Claude: it copies any file the agent names into the agent's blob folder,
/// which the agent then reads unasked, so a read the agent's own settings
/// would ask about goes through them first.
const AMUX_TOOLS_UNASKED: [&str; 6] = [
    wire::AGENTS_TOOL,
    wire::HOSTS_TOOL,
    wire::SEND_TOOL,
    wire::SPAWN_TOOL,
    wire::STOP_TOOL,
    wire::STATUS_TOOL,
];

/// The quirks of the agent with this id.
pub fn quirks(agent: &str) -> Quirks {
    match agent {
        "antigravity" => Quirks {
            steers: true,
            tool_naming: ToolNaming::RunMcp,
            ..Quirks::default()
        },
        "grok" => Quirks {
            usage_in_answer_meta: true,
            signed_in_method: Some("cached_token"),
            offers_in_initialize: true,
            tool_naming: ToolNaming::DoubleUnderscore,
            xai_questions: true,
            ..Quirks::default()
        },
        "opencode" => Quirks {
            tool_naming: ToolNaming::Underscore,
            ..Quirks::default()
        },
        _ => Quirks::default(),
    }
}

/// Whether the agent can reopen a session in a new process.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Reopens {
    resume: bool,
    load: bool,
}

impl Reopens {
    fn of(init: &InitializeResponse) -> Self {
        let Some(caps) = &init.agent_capabilities else {
            return Self::default();
        };
        // Antigravity writes each capability twice, once in snake case
        // (`agy-allow`): either spelling counts.
        let session = caps
            .session_capabilities
            .as_ref()
            .or(caps.session_capabilities_again.as_ref());
        Self {
            resume: session.is_some_and(|session| matches!(session.resume, Some(Some(_)))),
            load: caps.load_session.or(caps.load_session_again) == Some(true)
                || session.is_some_and(|session| session.load.is_some()),
        }
    }
}

/// What a prompt may carry beyond text, as the agent last said at
/// `initialize`. An agent's answer can change with its version (Grok took
/// no images at 1.0.50 and takes them at 1.0.89, `grok-image`), so it is
/// read again from every process.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Takes {
    images: bool,
}

impl Takes {
    fn of(init: &InitializeResponse) -> Self {
        let prompt = init
            .agent_capabilities
            .as_ref()
            .and_then(|caps| caps.prompt_capabilities.as_ref());
        Self {
            images: prompt.is_some_and(|prompt| prompt.image == Some(true)),
        }
    }
}

/// A line an ACP agent wrote on stderr, as the agent process hands it on.
#[derive(Serialize, Deserialize)]
struct Stderr {
    stderr: String,
}

/// The fact the agent process gives the interpreter for a line the agent
/// wrote on stderr. Agents write there freely (Grok an error for each remote
/// tool server it cannot reach, at every start), so the line is logged and
/// never drawn or taken as a failure.
pub fn stderr_fact(line: &str) -> Vec<u8> {
    serde_json::to_vec(&Stderr {
        stderr: line.to_owned(),
    })
    .expect("a string serializes")
}

/// The settings amux can change, by the category ACP gives their option.
/// Agents name the options differently (a thought level is
/// `thought_level`, `effort` or `reasoning_effort` in `agy-config`,
/// `opencode-config` and `grok-config`), so only the category is read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
enum Category {
    Model,
    Effort,
    Mode,
}

impl Category {
    fn of(category: &SessionConfigOptionCategory) -> Option<Self> {
        match category {
            SessionConfigOptionCategory::Model => Some(Self::Model),
            SessionConfigOptionCategory::ThoughtLevel => Some(Self::Effort),
            SessionConfigOptionCategory::Mode => Some(Self::Mode),
            SessionConfigOptionCategory::ModelConfig
            | SessionConfigOptionCategory::Speed
            | SessionConfigOptionCategory::Other(_) => None,
        }
    }
}

/// One setting the session offers, and the value it has.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Choice {
    category: Category,
    /// The config option's id; empty for ACP's older `modes` and `models`,
    /// which only `session/set_mode` changes, and only for a mode.
    id: String,
    current: String,
    values: Vec<Offered>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Offered {
    value: String,
    name: String,
    description: String,
}

/// The settings a session offers: its config options, and the older modes
/// and models for a category no option covers.
fn choices(
    options: Option<&[SessionConfigOption]>,
    modes: Option<&SessionModeState>,
    models: Option<&SessionModelState>,
) -> Vec<Choice> {
    let mut choices: Vec<Choice> = Vec::new();
    for option in options.into_iter().flatten() {
        let SessionConfigOption::Select(select) = option else {
            continue;
        };
        let Some(category) = select
            .category
            .as_ref()
            .and_then(|category| category.as_ref().and_then(Category::of))
        else {
            continue;
        };
        if choices.iter().any(|choice| choice.category == category) {
            continue;
        }
        choices.push(Choice {
            category,
            id: select.id.clone(),
            current: select.current_value.clone(),
            values: select
                .options
                .values()
                .into_iter()
                .map(|value| Offered {
                    value: value.value.clone(),
                    name: value.name.clone(),
                    description: value.description.clone().flatten().unwrap_or_default(),
                })
                .collect(),
        });
    }
    let has =
        |choices: &[Choice], category| choices.iter().any(|choice| choice.category == category);
    if let Some(modes) = modes
        && !has(&choices, Category::Mode)
    {
        choices.push(Choice {
            category: Category::Mode,
            id: String::new(),
            current: modes.current_mode_id.clone(),
            values: modes
                .available_modes
                .iter()
                .map(|mode| Offered {
                    value: mode.id.clone(),
                    name: mode.name.clone(),
                    description: mode.description.clone().flatten().unwrap_or_default(),
                })
                .collect(),
        });
    }
    if let Some(models) = models
        && !has(&choices, Category::Model)
    {
        choices.push(Choice {
            category: Category::Model,
            id: String::new(),
            current: models.current_model_id.clone(),
            values: models
                .available_models
                .iter()
                .map(|model| Offered {
                    value: model.model_id.clone(),
                    name: model.name.clone(),
                    description: model.description.clone().unwrap_or_default(),
                })
                .collect(),
        });
    }
    choices.sort_by_key(|choice| choice.category);
    choices
}

/// A request amux made and has not had answered.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
enum Request {
    /// A prompt: a person's, or an agent message's (`message`), whose
    /// delivery its answer settles. `steer` when it was sent into a
    /// running turn.
    Prompt {
        #[serde(with = "serde_pb::bytes")]
        input_id: Vec<u8>,
        message: bool,
        steer: bool,
    },
    /// A setting changed; the answer says what the session now has.
    /// `apply` when amux restores it itself as the session opens, which
    /// holds the first prompt until the settings settle.
    Set {
        category: Category,
        value: String,
        apply: bool,
    },
}

/// The agent's text being streamed, by its item key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Streamed {
    Message,
    Thought,
}

pub struct Acp;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct State {
    shared: Shared<AcpAsk>,
    /// The agent's id: the spec's provider.
    agent: String,
    incarnation: u32,
    session_id: Option<String>,
    /// What the agent last said at `initialize` about reopening sessions;
    /// None before any `initialize` answered.
    reopens: Option<Reopens>,
    /// What its prompts may carry, from the same answer.
    #[serde(default)]
    takes: Option<Takes>,
    /// This process's session answer came. Until it does, what the agent
    /// says about the conversation is a replay of a loaded session, which
    /// the chat already holds; what it says about the session's state
    /// (commands, settings, usage, title) still counts.
    opened: bool,
    choices: Vec<Choice>,
    /// The values the agent was created with or a person picked, which a
    /// reopened session is given again where it lost them.
    wanted: BTreeMap<Category, String>,
    /// The values the first session started with, which a pick of none
    /// returns to.
    starting: BTreeMap<Category, String>,
    /// The agent refused to open the session until the person signs in;
    /// a prompt asks it again, and `reopening` says an ask is in flight.
    #[serde(default)]
    awaiting_sign_in: bool,
    #[serde(default)]
    reopening: bool,
    /// A prompt came while an ask was in flight: the person may have
    /// signed in after it went, so a refusal of it is asked once more.
    #[serde(default)]
    reopen_again: bool,
    /// The settings a person picked that the agent has not yet said it
    /// took: once it does, the step says so, and its host keeps the pick.
    #[serde(default)]
    picking: std::collections::BTreeSet<Category>,
    requests: BTreeMap<String, Request>,
    /// The prompt requests unanswered, oldest first.
    prompts: Vec<String>,
    /// How many setting changes the person made, which names each.
    sets: u64,
    /// amux sent `session/cancel` for the running turn.
    cancelled: bool,
    stream: Option<(Streamed, String)>,
    next_item: u64,
    next_boundary: u64,
    early_boundary: Option<EarlyBoundary>,
    /// Each tool call by the agent's id for it, and what it printed.
    #[serde(with = "serde_pb::msg_map")]
    tools: BTreeMap<String, AcpTool>,
    outputs: BTreeMap<String, String>,
    /// The calls a form, link or question ask named as its own, by call,
    /// with the key of that ask's item: the call is how the agent asks, so
    /// it is drawn naming the ask, which stands for it.
    #[serde(default)]
    folded: BTreeMap<String, String>,
    context: Option<(u64, u64)>,
    #[serde(with = "serde_pb::opt_msg")]
    usage: Option<wire::AcpUsage>,
    #[serde(with = "serde_pb::opt_msg")]
    sign_in: Option<wire::SignIn>,
    plan: Option<Vec<(String, i32)>>,
    #[serde(with = "serde_pb::msgs")]
    commands: Vec<OfferedCommand>,
    /// How many permissions and elicitations the agent asked for, which
    /// numbers each ask's key.
    asks_seen: u64,
    /// The agent's request id behind each open ask, by ask key, for the
    /// answer and for the agent withdrawing it.
    ask_requests: BTreeMap<String, String>,
    /// What the session offers changed in this step, so the catalogue is
    /// published again.
    #[serde(skip)]
    offers_changed: bool,
}

fn item_body(kind: acp_item::Kind) -> Vec<u8> {
    AcpItem { kind: Some(kind) }.encode_to_vec()
}

/// The `n`th ask's key, counting every permission and elicitation the
/// agent asked for in the agent's life.
pub fn ask_key(n: u64) -> String {
    format!("ask-{n}")
}

/// A request id as the agent sent it, in JSON.
fn request_id_key(id: &RequestId) -> String {
    serde_json::to_string(id).expect("a request id serializes")
}

fn tool_key(tool_call_id: &str) -> String {
    format!("tool:{tool_call_id}")
}

fn prompt_key(input_id: &[u8]) -> String {
    format!("prompt:{}", to_hex(input_id))
}

fn tool_kind(kind: Option<&ToolKind>) -> wire::AcpToolKind {
    use wire::AcpToolKind as Kind;
    match kind {
        None => Kind::Unspecified,
        Some(ToolKind::Read) => Kind::Read,
        Some(ToolKind::Edit) => Kind::Edit,
        Some(ToolKind::Delete) => Kind::Delete,
        Some(ToolKind::Move) => Kind::Move,
        Some(ToolKind::Search) => Kind::Search,
        Some(ToolKind::Execute) => Kind::Execute,
        Some(ToolKind::Think) => Kind::Think,
        Some(ToolKind::Fetch) => Kind::Fetch,
        Some(ToolKind::SwitchMode) => Kind::SwitchMode,
        Some(ToolKind::Generic) => Kind::Other,
        Some(ToolKind::Other(_)) => Kind::Unspecified,
    }
}

/// How a call's row groups with others: what only looks is told apart
/// from what changes something, and an unknown kind is taken to change
/// something.
fn tool_class(kind: wire::AcpToolKind) -> wire::ToolClass {
    use wire::AcpToolKind as Kind;
    match kind {
        Kind::Read => wire::ToolClass::Read,
        Kind::Search => wire::ToolClass::Search,
        Kind::Fetch => wire::ToolClass::Fetch,
        Kind::Think => wire::ToolClass::Look,
        Kind::Edit
        | Kind::Delete
        | Kind::Move
        | Kind::Execute
        | Kind::SwitchMode
        | Kind::Other
        | Kind::Unspecified => wire::ToolClass::Consequential,
    }
}

fn permission_kind(kind: &PermissionOptionKind) -> wire::AcpPermissionKind {
    use wire::AcpPermissionKind as Kind;
    match kind {
        PermissionOptionKind::AllowOnce => Kind::AllowOnce,
        PermissionOptionKind::AllowAlways => Kind::AllowAlways,
        PermissionOptionKind::RejectOnce => Kind::RejectOnce,
        PermissionOptionKind::RejectAlways => Kind::RejectAlways,
        PermissionOptionKind::Other(_) => Kind::Unspecified,
    }
}

fn finished(state: i32) -> bool {
    use wire::ToolState;
    matches!(
        ToolState::try_from(state).unwrap_or(ToolState::Unspecified),
        ToolState::Succeeded | ToolState::Failed | ToolState::Denied | ToolState::Cancelled
    )
}

/// The ask item a form or link ask draws, closed by `closed`.
fn work_item(ask: &AcpAsk, closed: Option<wire::AskClosed>) -> Option<AskItem> {
    let asked = match &ask.body {
        Some(acp_ask::Body::Form(form)) => wire_ask_item::Ask::Form(form.clone()),
        Some(acp_ask::Body::Link(link)) => wire_ask_item::Ask::Link(link.clone()),
        Some(acp_ask::Body::Question(question)) => wire_ask_item::Ask::Question(question.clone()),
        Some(acp_ask::Body::Permission(_)) | None => return None,
    };
    let item = ask_item::opened(asked);
    Some(match closed {
        Some(closed) => ask_item::close(item, closed),
        None => item,
    })
}

impl State {
    /// The kind-neutral state.
    pub fn shared(&self) -> &Shared<AcpAsk> {
        &self.shared
    }

    fn new(spec: &AgentSpec) -> Self {
        let config = spec.config.clone().unwrap_or_default();
        let wanted = [
            (Category::Model, config.model),
            (Category::Effort, config.effort),
            (Category::Mode, config.mode),
        ]
        .into_iter()
        .filter_map(|(category, value)| Some((category, value?)))
        .collect();
        let mut shared = Shared::new(spec, KIND);
        shared.set_capabilities(capabilities(&spec.provider, None, None));
        Self {
            shared,
            agent: spec.provider.clone(),
            incarnation: spec.incarnation,
            session_id: None,
            reopens: None,
            takes: None,
            opened: false,
            choices: Vec::new(),
            wanted,
            starting: BTreeMap::new(),
            picking: Default::default(),
            awaiting_sign_in: false,
            reopening: false,
            reopen_again: false,
            requests: BTreeMap::new(),
            prompts: Vec::new(),
            sets: 0,
            cancelled: false,
            stream: None,
            next_item: 0,
            next_boundary: 0,
            early_boundary: None,
            tools: BTreeMap::new(),
            outputs: BTreeMap::new(),
            folded: BTreeMap::new(),
            context: None,
            usage: None,
            sign_in: None,
            plan: None,
            commands: Vec::new(),
            asks_seen: 0,
            ask_requests: BTreeMap::new(),
            offers_changed: false,
        }
    }

    fn quirks(&self) -> Quirks {
        quirks(&self.agent)
    }

    /// Says on the snapshot what the agent can do, as its process last said.
    fn say_capabilities(&mut self) {
        let capabilities = capabilities(&self.agent, self.reopens, self.takes);
        self.shared.set_capabilities(capabilities);
    }

    fn choice(&self, category: Category) -> Option<&Choice> {
        self.choices
            .iter()
            .find(|choice| choice.category == category)
    }

    fn current(&self, category: Category) -> Option<String> {
        self.choice(category).map(|choice| choice.current.clone())
    }

    fn offered_models(&self) -> Vec<OfferedModel> {
        let efforts: Vec<String> = self
            .choice(Category::Effort)
            .map(|choice| {
                choice
                    .values
                    .iter()
                    .map(|value| value.value.clone())
                    .collect()
            })
            .unwrap_or_default();
        self.choice(Category::Model)
            .map(|choice| {
                choice
                    .values
                    .iter()
                    .map(|value| OfferedModel {
                        value: value.value.clone(),
                        display_name: value.name.clone(),
                        description: value.description.clone(),
                        efforts: efforts.clone(),
                        ..Default::default()
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn catalogue(&self) -> Catalogue {
        // ACP names no normal mode. The one the first session started in is
        // the agent's own default, from its own settings, so clients leave
        // it unsaid, as they do Claude's and Codex's normal modes.
        let normal = self.starting.get(&Category::Mode);
        Catalogue {
            models: self.offered_models(),
            commands: self.commands.clone(),
            modes: self
                .choice(Category::Mode)
                .map(|choice| {
                    choice
                        .values
                        .iter()
                        .map(|value| OfferedMode {
                            value: value.value.clone(),
                            display_name: value.name.clone(),
                            normal: normal == Some(&value.value),
                            settable: true,
                        })
                        .collect()
                })
                .unwrap_or_default(),
            ..Default::default()
        }
    }

    fn body(&self) -> Vec<u8> {
        let model = self.current(Category::Model);
        AcpSnapshot {
            agent: Some(self.agent.clone()),
            asks: self.shared.asks().open_asks().to_vec(),
            context: Some(match self.context {
                None => unknown::context_meter(),
                Some((used_tokens, window)) => ContextMeter {
                    known: true,
                    used_tokens,
                    window_tokens: Some(window),
                },
            }),
            model_name: crate::shared::model_name(
                model.as_deref(),
                None,
                &self.offered_models(),
                str::to_owned,
            ),
            model,
            effort: self.current(Category::Effort),
            mode: self.current(Category::Mode),
            usage: Some(self.usage.clone().unwrap_or_else(unknown::acp_usage)),
            sign_in: Some(self.sign_in.clone().unwrap_or_else(unknown::sign_in)),
            plan: Some(match &self.plan {
                None => unknown::task_list(),
                Some(entries) => TaskList {
                    known: true,
                    entries: entries
                        .iter()
                        .enumerate()
                        .map(|(index, (subject, status))| TaskListEntry {
                            id: (index + 1).to_string(),
                            subject: subject.clone(),
                            status: *status,
                            active_form: String::new(),
                        })
                        .collect(),
                },
            }),
            session_id: self.session_id.clone(),
            // What the daemon reads to know whether a new process could
            // carry the conversation on; resume wins, as `opening` picks.
            reopen: self.reopens.map(|reopens| {
                (if reopens.resume {
                    wire::AcpReopen::Resume
                } else if reopens.load {
                    wire::AcpReopen::Load
                } else {
                    wire::AcpReopen::None
                }) as i32
            }),
        }
        .encode_to_vec()
    }

    // --- writing to the agent ------------------------------------------------

    fn session(&self) -> String {
        self.session_id.clone().unwrap_or_default()
    }

    fn write(&mut self, emit: &mut Emit, message: &ClientMessage) {
        emit.effect(Effect::ProviderWrite(acp_protocol::encode(message)));
    }

    fn request(&mut self, emit: &mut Emit, id: String, message: ClientRequest, request: Request) {
        self.requests.insert(id.clone(), request);
        self.write(
            emit,
            &ClientMessage::request(RequestId::String(id), message),
        );
    }

    /// Answers the agent's request an ask key names, once.
    fn respond(&mut self, emit: &mut Emit, key: &str, response: ClientResponse) {
        self.respond_as(emit, key, Response::Acp(response));
    }

    fn respond_as(&mut self, emit: &mut Emit, key: &str, response: Response) {
        let Some(id) = self
            .ask_requests
            .remove(key)
            .and_then(|id| serde_json::from_str::<RequestId>(&id).ok())
        else {
            return;
        };
        let message = match response {
            Response::Acp(response) => ClientMessage::response(id, response),
            Response::Vendor(answer) => ClientMessage::ExtensionAnswer {
                id,
                answer,
                extra: Extra::new(),
            },
        };
        self.write(emit, &message);
    }

    /// A key for the ask the agent's request `id` opens. Keys are amux's
    /// own and never repeat, while an agent may give a later request an id
    /// an earlier process used: an answer to an ask from before a restart
    /// must never reach a new one.
    fn ask_for(&mut self, id: &RequestId) -> String {
        self.asks_seen += 1;
        let key = ask_key(self.asks_seen);
        self.ask_requests.insert(key.clone(), request_id_key(id));
        key
    }

    /// Refuses a request of the agent's at once, so it does not wait on
    /// amux for something amux does not do.
    fn refuse_request(&mut self, emit: &mut Emit, id: RequestId, method: &str) {
        let error = RpcError {
            code: acp_protocol::error_code::METHOD_NOT_FOUND,
            message: format!("amux does not handle {method}"),
            data: None,
            extra: Extra::new(),
        };
        self.write(emit, &ClientMessage::error(id, error));
    }

    /// Sends a setting to the session under `id`: by its config option, or
    /// a legacy mode by `session/set_mode`, which is called only where the
    /// agent offers modes (Grok answers it without offering any,
    /// `grok-config`). False when the session does not offer `value`.
    fn send_setting(
        &mut self,
        emit: &mut Emit,
        id: String,
        category: Category,
        value: &str,
        apply: bool,
    ) -> bool {
        let Some(choice) = self.choice(category).cloned() else {
            return false;
        };
        if !choice.values.iter().any(|offered| offered.value == value) {
            return false;
        }
        let request = if !choice.id.is_empty() {
            ClientRequest::SessionSetConfigOption(SetSessionConfigOptionRequest::select(
                &self.session(),
                &choice.id,
                value,
            ))
        } else if category == Category::Mode {
            ClientRequest::SessionSetMode(SetSessionModeRequest {
                session_id: self.session(),
                mode_id: value.to_owned(),
                meta: None,
                extra: Extra::new(),
            })
        } else {
            // ACP's older model list has no stable way to change it.
            return false;
        };
        let value = value.to_owned();
        self.request(
            emit,
            id,
            request,
            Request::Set {
                category,
                value,
                apply,
            },
        );
        true
    }

    /// A setting change is in flight. Antigravity's `config_option_update`
    /// can echo the value from before it (`agy-config`, `agy-prompt`), so
    /// updates are not read until its answer, which says what the session
    /// has.
    fn setting_pending(&self) -> bool {
        self.requests
            .values()
            .any(|request| matches!(request, Request::Set { .. }))
    }

    /// Gives a session the values amux wants it to have where it does not
    /// have them: the create config on a new session, and on a reopened one
    /// whatever the agent forgot (Antigravity reopens a session on its
    /// defaults, `agy-load`, where Grok keeps them, `grok-load`). One at a
    /// time, the model first: what thought levels and modes a session
    /// offers can change with its model, so each is checked against what
    /// the answer before it offers. Once nothing is left to restore the
    /// agent takes its first prompt.
    fn apply_wanted(&mut self, emit: &mut Emit) {
        if self.applying() {
            return;
        }
        let wanted = self.wanted.clone();
        for (category, value) in wanted {
            if self.current(category).as_deref() == Some(value.as_str()) {
                continue;
            }
            let id = self
                .choice(category)
                .map(|choice| match choice.id.as_str() {
                    "" => apply_request("mode"),
                    id => apply_request(id),
                })
                .unwrap_or_default();
            if self.send_setting(emit, id, category, &value, true) {
                return;
            }
            // Not offered here: the agent's own value stands.
            self.wanted.remove(&category);
        }
        if !self.shared.is_started() {
            self.shared.provider_started();
        }
    }

    /// A setting amux restores as the session opens is in flight.
    fn applying(&self) -> bool {
        self.requests
            .values()
            .any(|request| matches!(request, Request::Set { apply: true, .. }))
    }

    // --- items -----------------------------------------------------------

    fn local_key(&mut self, prefix: &str) -> String {
        self.next_item += 1;
        format!("{prefix}:{}", self.next_item)
    }

    fn emit_item(&mut self, emit: &mut Emit, draft: ItemDraft) {
        self.shared.item(emit, draft);
    }

    /// The agent's text, appended to the stream of its kind running, else
    /// starting one: another kind of update between two chunks ends the
    /// first stream.
    fn stream(&mut self, emit: &mut Emit, kind: Streamed, text: &str) {
        if text.is_empty() {
            return;
        }
        if let Some((streaming, key)) = self.stream.clone()
            && streaming == kind
            && self.shared.append(emit, &key, text)
        {
            return;
        }
        self.close_stream(emit);
        let key = self.local_key(match kind {
            Streamed::Message => "message",
            Streamed::Thought => "thought",
        });
        self.emit_item(
            emit,
            ItemDraft {
                key: key.clone(),
                text: text.to_owned(),
                body: text_body(kind, false),
                ..Default::default()
            },
        );
        self.stream = Some((kind, key));
    }

    fn close_stream(&mut self, emit: &mut Emit) {
        let Some((kind, key)) = self.stream.take() else {
            return;
        };
        let Some(item) = self.shared.open_item(&key).cloned() else {
            return;
        };
        self.emit_item(
            emit,
            ItemDraft {
                key: key.clone(),
                text: item.text,
                attachments: item.attachments,
                body: text_body(kind, true),
                complete: true,
                ..Default::default()
            },
        );
        if kind == Streamed::Message {
            self.shared.note_message(&key);
            self.shared.reply_said(emit, &key);
        }
    }

    fn emit_tool(&mut self, emit: &mut Emit, id: &str) {
        let Some(mut tool) = self.tools.get(id).cloned() else {
            return;
        };
        if let Some(ask) = self.folded.get(id) {
            tool.ask_item_key = ask.clone();
        }
        let complete = finished(tool.state);
        self.emit_item(
            emit,
            ItemDraft {
                key: tool_key(id),
                text: self.outputs.get(id).cloned().unwrap_or_default(),
                body: item_body(acp_item::Kind::Tool(tool)),
                complete,
                ..Default::default()
            },
        );
    }

    /// Whether the call's row was drawn finished, after which nothing more
    /// about it is drawn.
    fn tool_closed(&self, id: &str) -> bool {
        self.tools.get(id).is_some_and(|tool| finished(tool.state))
            && self.shared.open_item(&tool_key(id)).is_none()
    }

    fn decide(&mut self, emit: &mut Emit, item_key: &str, outcome: wire::DecisionOutcome) {
        let Some(id) = item_key.strip_prefix("tool:").map(str::to_owned) else {
            return;
        };
        if self.tool_closed(&id) {
            return;
        }
        if let Some(tool) = self.tools.get_mut(&id) {
            tool.decision = Some(wire::ToolDecision {
                outcome: outcome as i32,
                ..Default::default()
            });
            if outcome == wire::DecisionOutcome::Denied && !finished(tool.state) {
                tool.state = wire::ToolState::Denied as i32;
                tool.ended_at_ms = Some(self.shared.now_ms());
            }
        }
        self.emit_tool(emit, &id);
    }

    /// Ends every call still running: its turn is over or its process gone.
    fn settle_tools(&mut self, emit: &mut Emit) {
        let open: Vec<String> = self
            .tools
            .iter()
            .filter(|(_, tool)| !finished(tool.state))
            .map(|(id, _)| id.clone())
            .collect();
        let now = self.shared.now_ms();
        for id in open {
            if let Some(tool) = self.tools.get_mut(&id) {
                tool.state = wire::ToolState::Cancelled as i32;
                tool.ended_at_ms = Some(now);
            }
            self.emit_tool(emit, &id);
        }
        // A finished call's row never changes again.
        self.tools.retain(|_, tool| !finished(tool.state));
        let tools = &self.tools;
        self.outputs.retain(|id, _| tools.contains_key(id));
    }

    fn emit_ask_item(&mut self, emit: &mut Emit, ask: &AcpAsk, closed: Option<wire::AskClosed>) {
        let Some(item) = work_item(ask, closed.clone()) else {
            return;
        };
        self.emit_item(
            emit,
            ItemDraft {
                key: ask.item_key.clone(),
                body: item_body(acp_item::Kind::Ask(item)),
                complete: closed.is_some(),
                ..Default::default()
            },
        );
    }

    /// Closes an ask nobody answered: its row says so, and a permission's
    /// call is left undecided.
    fn dismiss(&mut self, emit: &mut Emit, ask: &AcpAsk) {
        match &ask.body {
            Some(acp_ask::Body::Permission(_)) => {
                self.decide(emit, &ask.item_key, wire::DecisionOutcome::Unknown)
            }
            _ => self.emit_ask_item(emit, ask, Some(ask_item::dismissed())),
        }
    }

    fn boundary(&mut self, emit: &mut Emit, kind: wire::BoundaryKind, exit: wire::IncarnationEnd) {
        self.boundary_item(
            emit,
            wire::Boundary {
                kind: kind as i32,
                cause: exit.cause,
                cause_detail: exit.detail,
                idle: false,
            },
        );
    }

    fn boundary_item(&mut self, emit: &mut Emit, boundary: wire::Boundary) -> String {
        self.next_boundary += 1;
        let key = format!("boundary:{}", self.next_boundary);
        self.boundary_at(emit, key.clone(), boundary);
        key
    }

    fn boundary_at(&mut self, emit: &mut Emit, key: String, boundary: wire::Boundary) {
        self.emit_item(
            emit,
            ItemDraft {
                key,
                body: item_body(acp_item::Kind::Boundary(boundary)),
                complete: true,
                ..Default::default()
            },
        );
    }

    /// The session opened: the boundary a first prompt drawn ahead of it
    /// placed above itself is filled in, else one is drawn.
    fn start_boundary(&mut self, emit: &mut Emit, kind: wire::BoundaryKind) {
        let boundary = wire::Boundary {
            kind: kind as i32,
            ..Default::default()
        };
        match self.early_boundary.take() {
            Some(early) => {
                if let Some(key) = early.refill(kind) {
                    self.boundary_at(emit, key, boundary);
                }
            }
            None => {
                self.boundary_item(emit, boundary);
            }
        }
    }

    fn draw_opening(&mut self, emit: &mut Emit) {
        if let Some(entry) = self.shared.opening_to_draw() {
            let kind = if self.incarnation > 1 {
                wire::BoundaryKind::Resumed
            } else {
                wire::BoundaryKind::Started
            };
            let boundary = wire::Boundary {
                kind: kind as i32,
                ..Default::default()
            };
            let key = self.boundary_item(emit, boundary);
            self.early_boundary = Some(EarlyBoundary {
                key,
                kind: kind as i32,
            });
            self.prompt_row(emit, entry);
        }
    }

    fn prompt_row(&mut self, emit: &mut Emit, entry: QueuedInput) {
        self.emit_item(
            emit,
            ItemDraft {
                key: prompt_key(&entry.input_id),
                text: entry.text,
                attachments: entry.attachments,
                input_id: entry.input_id,
                body: item_body(acp_item::Kind::Prompt(wire::Prompt {})),
                complete: true,
                ..Default::default()
            },
        );
    }

    // --- prompts and the queue -----------------------------------------------

    /// Sends a prompt. A person's draws its row now, since ACP reflects no
    /// prompt back; an agent message's row was drawn when it arrived.
    fn submit(&mut self, emit: &mut Emit, entry: QueuedInput, steer: bool) {
        let message = self.shared.pending_messages().contains(&entry.input_id);
        self.shared.reflect_prompt_id(&entry.input_id);
        let id = prompt_request(&entry.input_id);
        let request = ClientRequest::SessionPrompt(PromptRequest {
            session_id: self.session(),
            prompt: vec![ContentBlock::text(provider_text(&entry))],
            meta: None,
            extra: Extra::new(),
        });
        self.requests.insert(
            id.clone(),
            Request::Prompt {
                input_id: entry.input_id.clone(),
                message,
                steer,
            },
        );
        self.prompts.push(id.clone());
        let bytes = acp_protocol::encode(&ClientMessage::request(RequestId::String(id), request));
        emit.effect(if entry.attachments.is_empty() {
            Effect::ProviderWrite(bytes)
        } else {
            Effect::AcpPrompt {
                request: bytes,
                attachments: entry.attachments.clone(),
            }
        });
        if !message && !steer && !self.shared.drawn_early(&entry.input_id) {
            self.prompt_row(emit, entry);
        }
    }

    /// An agent message arrives as a prompt: its row now, then it waits in
    /// the queue like a person's prompt and is consumed when its prompt is
    /// answered.
    fn agent_message(&mut self, emit: &mut Emit, envelope: Envelope) {
        let id = envelope.id.clone();
        if self.shared.message_repeated(emit, &id) {
            return;
        }
        self.shared.message_accepted(&id);
        self.emit_item(
            emit,
            ItemDraft {
                key: agent_message_key(&envelope.id),
                text: crate::shared::words(&envelope),
                attachments: crate::shared::attachments(&envelope).to_vec(),
                input_id: envelope.id.clone(),
                body: item_body(acp_item::Kind::AgentMessage(agent_message_body(&envelope))),
                complete: true,
                ..Default::default()
            },
        );
        let prompt = wire::PromptInput {
            text: crate::shared::words(&envelope),
            attachments: crate::shared::attachments(&envelope).to_vec(),
        };
        let from = envelope.from.clone().unwrap_or_default();
        if let Some(entry) = self.shared.admit_prompt(emit, &id, prompt, from) {
            self.submit(emit, entry, false);
        }
        self.reopen_after_sign_in(emit);
    }

    /// A prompt came while the agent would not open its session for want
    /// of a sign-in: the person may have signed in since, so the agent
    /// process asks it again, by the very request it was refused (never a
    /// new session in place of a refused reopening). The prompt waits in
    /// the queue meanwhile, and goes once the session opens.
    fn reopen_after_sign_in(&mut self, emit: &mut Emit) {
        if !self.awaiting_sign_in {
            return;
        }
        if self.reopening {
            self.reopen_again = true;
        } else {
            self.reopening = true;
            emit.effect(Effect::AcpReopen);
        }
    }

    /// Cancels the running turn the ACP way: a `session/cancel`
    /// notification, after which the agent may go on streaming until it
    /// answers the prompt `cancelled` (`agy-cancel`, `grok-cancel`). Every
    /// ask of the turn is answered cancelled now, as ACP requires.
    fn interrupt(&mut self, emit: &mut Emit) {
        if self.prompts.is_empty() || self.cancelled {
            return;
        }
        self.cancelled = true;
        let cancel = ClientNotification::SessionCancel(SessionRequest {
            session_id: self.session(),
            meta: None,
            extra: Extra::new(),
        });
        self.write(emit, &ClientMessage::notification(cancel));
        for ask in self.shared.close_all_asks() {
            self.respond_as(emit, &ask.key, cancelled(ask.body.as_ref()));
            self.dismiss(emit, &ask);
        }
    }

    // --- inputs ----------------------------------------------------------

    fn input(&mut self, emit: &mut Emit, input: Input) {
        let id = input.input_id.clone();
        let arm = match input.decode_body() {
            Some(KindInput::Acp(wire::AcpInput { of: Some(arm) })) => arm,
            _ => return self.shared.reject(emit, &id, Refusal::Unsupported),
        };
        match arm {
            acp_input::Of::Prompt(prompt) => {
                if self.shared.refuse_unreadable(emit, &id, &prompt)
                    || self.refuse_untaken(emit, &id, &prompt)
                {
                    return;
                }
                if let Some(entry) =
                    self.shared
                        .admit_prompt(emit, &id, prompt, crate::shared::human())
                {
                    self.submit(emit, entry, false);
                }
                self.reopen_after_sign_in(emit);
            }
            // An agent's message is its sender's to take back, not a
            // person's.
            acp_input::Of::Withdraw(withdraw)
                if self
                    .shared
                    .pending_messages()
                    .contains(&withdraw.queued_input_id) =>
            {
                self.shared.reject(emit, &id, Refusal::Unsupported)
            }
            acp_input::Of::Withdraw(withdraw) => {
                self.shared.withdraw(emit, &id, &withdraw.queued_input_id)
            }
            acp_input::Of::SendNow(send) => {
                if !self.quirks().steers {
                    return self.shared.reject(emit, &id, Refusal::Unsupported);
                }
                let running = !self.prompts.is_empty() && !self.cancelled;
                if let Some(entry) = self
                    .shared
                    .send_now(emit, &id, &send.queued_input_id, running)
                {
                    self.submit(emit, entry, true);
                }
            }
            acp_input::Of::Interrupt(_) => {
                self.interrupt(emit);
                self.shared.accept(emit, &id, false);
            }
            acp_input::Of::Model(set) => {
                let value = set
                    .model
                    .or_else(|| self.starting.get(&Category::Model).cloned());
                self.setting(emit, &id, Category::Model, value)
            }
            acp_input::Of::Effort(set) => {
                let value = set
                    .effort
                    .or_else(|| self.starting.get(&Category::Effort).cloned());
                self.setting(emit, &id, Category::Effort, value)
            }
            acp_input::Of::Mode(set) => self.setting(emit, &id, Category::Mode, Some(set.value)),
            acp_input::Of::Answer(answer) => self.answer(emit, &id, answer),
        }
    }

    /// Refuses a prompt carrying what the agent said its prompts do not
    /// take: an image, for an agent without image support. Until the agent
    /// has said, the prompt waits, and an image it turns out not to take
    /// reaches it as the element naming the file.
    fn refuse_untaken(&mut self, emit: &mut Emit, id: &[u8], prompt: &wire::PromptInput) -> bool {
        let images = prompt
            .attachments
            .iter()
            .any(|attachment| matches!(attachment.of, Some(wire::attachment::Of::Image(_))));
        if !images || self.takes.is_none_or(|takes| takes.images) {
            return false;
        }
        let name = built_in(&self.agent).map_or("this agent", |agent| agent.name);
        self.shared.refuse(
            emit,
            id,
            wire::Rejected {
                refusal: Refusal::Unsupported as i32,
                detail: Some(format!("{name} does not take images")),
            },
        );
        true
    }

    /// A person's pick of a setting: sent now when the session is open,
    /// else kept for when it opens. A value the session does not offer is
    /// refused.
    fn setting(&mut self, emit: &mut Emit, id: &[u8], category: Category, value: Option<String>) {
        let Some(value) = value else {
            // Nothing to return to yet: the session has not said.
            return self.shared.accept(emit, id, false);
        };
        if !self.opened {
            let offered = self
                .choice(category)
                .is_none_or(|choice| choice.values.iter().any(|offered| offered.value == value));
            if !offered {
                return self.shared.reject(emit, id, Refusal::Unsupported);
            }
            self.wanted.insert(category, value);
            self.picking.insert(category);
            return self.shared.accept(emit, id, false);
        }
        self.sets += 1;
        let request = set_request(self.sets);
        if !self.send_setting(emit, request, category, &value, false) {
            return self.shared.reject(emit, id, Refusal::Unsupported);
        }
        self.wanted.insert(category, value);
        self.picking.insert(category);
        self.shared.accept(emit, id, false);
    }

    /// A person's answer to an ask. One that does not fit the ask (an
    /// option it did not offer, a form for a permission) is refused and
    /// leaves the ask open.
    fn answer(&mut self, emit: &mut Emit, id: &[u8], answer: AcpAnswer) {
        let key = answer.ask_key;
        let Some(ask) = self.shared.asks().get(&key).cloned() else {
            return self.shared.reject(emit, id, Refusal::ClosedAsk);
        };
        let Some(of) = answer.of else {
            return self.shared.reject(emit, id, Refusal::Unsupported);
        };
        match (&ask.body, of) {
            (Some(acp_ask::Body::Permission(permission)), acp_answer::Of::Option(option)) => {
                let Some(chosen) = permission
                    .options
                    .iter()
                    .find(|offered| offered.option_id == option)
                    .cloned()
                else {
                    return self.shared.reject(emit, id, Refusal::Unsupported);
                };
                if self.shared.answer(emit, id, &key).is_none() {
                    return;
                }
                let response = ClientResponse::RequestPermission(
                    acp_protocol::client::RequestPermissionResponse::selected(&option),
                );
                self.respond(emit, &key, response);
                use wire::AcpPermissionKind as Kind;
                let kind = Kind::try_from(chosen.kind).unwrap_or(Kind::Unspecified);
                if kind == Kind::AllowAlways {
                    // The agent remembers it for as long as its process
                    // runs (OpenCode, `opencode-allow_always`), which no
                    // reopened session restores.
                    self.shared.granted_for_session();
                }
                let outcome = match kind {
                    Kind::AllowOnce | Kind::AllowAlways => wire::DecisionOutcome::Allowed,
                    Kind::RejectOnce | Kind::RejectAlways => wire::DecisionOutcome::Denied,
                    Kind::Unspecified => wire::DecisionOutcome::Unknown,
                };
                self.decide(emit, &ask.item_key, outcome);
                self.shared.accept(emit, id, false);
            }
            (Some(acp_ask::Body::Form(form)), acp_answer::Of::Form(sent)) => {
                let Some(response) = form_response(&sent, &form.schema_json) else {
                    return self.shared.reject(emit, id, Refusal::Unsupported);
                };
                if self.shared.answer(emit, id, &key).is_none() {
                    return;
                }
                self.respond(emit, &key, ClientResponse::ElicitationCreate(response));
                let closed = ask_item::form_sent(&sent, &form.schema_json);
                self.emit_ask_item(emit, &ask, Some(closed));
                self.shared.accept(emit, id, false);
            }
            (Some(acp_ask::Body::Link(_)), acp_answer::Of::Link(sent)) => {
                let Some(response) = action_response(sent.action, None) else {
                    return self.shared.reject(emit, id, Refusal::Unsupported);
                };
                if self.shared.answer(emit, id, &key).is_none() {
                    return;
                }
                self.respond(emit, &key, ClientResponse::ElicitationCreate(response));
                self.emit_ask_item(emit, &ask, Some(ask_item::link_answered(sent.action)));
                self.shared.accept(emit, id, false);
            }
            (Some(acp_ask::Body::Question(asked)), acp_answer::Of::Question(answer)) => {
                let (Some(response), Some(closed)) = (
                    question_response(asked, &answer),
                    ask_item::answered(asked, &answer),
                ) else {
                    return self.shared.reject(emit, id, Refusal::Unsupported);
                };
                if self.shared.answer(emit, id, &key).is_none() {
                    return;
                }
                self.respond_as(emit, &key, Response::Vendor(response));
                self.emit_ask_item(emit, &ask, Some(closed));
                self.shared.accept(emit, id, false);
            }
            // The person's words instead of an answer: the ask is
            // withdrawn and the turn cancelled, which closes every other ask
            // of it, and the words are the next prompt.
            (Some(body), acp_answer::Of::Reply(reply)) => {
                if self.shared.answer(emit, id, &key).is_none() {
                    return;
                }
                self.respond_as(emit, &key, cancelled(Some(body)));
                match body {
                    acp_ask::Body::Permission(_) => {
                        self.decide(emit, &ask.item_key, wire::DecisionOutcome::Denied)
                    }
                    acp_ask::Body::Question(asked) => {
                        let closed = ask_item::replied(asked, &reply)
                            .unwrap_or_else(|| ask_item::outcome(AskOutcome::Replied));
                        self.emit_ask_item(emit, &ask, Some(closed));
                    }
                    _ => {
                        self.emit_ask_item(emit, &ask, Some(ask_item::outcome(AskOutcome::Replied)))
                    }
                }
                self.interrupt(emit);
                let prompt = wire::PromptInput {
                    text: reply.text,
                    attachments: Vec::new(),
                };
                if let Some(entry) =
                    self.shared
                        .admit_prompt(emit, id, prompt, crate::shared::human())
                {
                    self.submit(emit, entry, false);
                }
            }
            _ => self.shared.reject(emit, id, Refusal::Unsupported),
        }
    }

    pub(super) fn exited(&mut self, emit: &mut Emit, exit: wire::IncarnationEnd) {
        emit.end_incarnation(exit.clone());
        self.shared.provider_exited_as(&exit);
        self.close_stream(emit);
        for ask in self.shared.close_all_asks() {
            self.dismiss(emit, &ask);
        }
        self.settle_tools(emit);
        // A message whose prompt the agent was given went with the process
        // that had it: delivered, never sent again.
        let in_flight: Vec<Vec<u8>> = self
            .requests
            .values()
            .filter_map(|request| match request {
                Request::Prompt {
                    input_id,
                    message: true,
                    ..
                } => Some(input_id.clone()),
                _ => None,
            })
            .collect();
        for envelope in in_flight {
            self.shared.message_consumed(&envelope);
        }
        self.forget_process();
        self.boundary(emit, wire::BoundaryKind::Exited, exit);
    }

    /// What lived only in the agent's process.
    fn forget_process(&mut self) {
        self.opened = false;
        // A new process asks to open the session itself.
        self.awaiting_sign_in = false;
        self.reopening = false;
        self.reopen_again = false;
        self.shared.awaiting_sign_in(false);
        self.requests.clear();
        self.prompts.clear();
        self.cancelled = false;
        self.stream = None;
        self.ask_requests.clear();
    }
}

fn text_body(kind: Streamed, complete: bool) -> Vec<u8> {
    match kind {
        Streamed::Message => item_body(acp_item::Kind::Message(wire::Text { complete })),
        Streamed::Thought => item_body(acp_item::Kind::Thought(wire::Thinking { complete })),
    }
}

/// The answer to a form: what it holds when accepted, read as ACP's
/// values, each choice the constant the agent's schema asked for. None when
/// the action is none or the content is not a form's.
fn form_response(sent: &wire::FormAnswer, schema: &[u8]) -> Option<CreateElicitationResponse> {
    let content = if sent.content_json.is_empty() {
        None
    } else {
        let content = ask_item::constants_for_titles(&sent.content_json, schema);
        Some(serde_json::from_slice::<BTreeMap<String, ElicitationValue>>(&content).ok()?)
    };
    action_response(sent.action, content)
}

fn action_response(
    action: i32,
    content: Option<BTreeMap<String, ElicitationValue>>,
) -> Option<CreateElicitationResponse> {
    Some(
        match FormAction::try_from(action).unwrap_or(FormAction::Unspecified) {
            FormAction::Accept => CreateElicitationResponse::Accept(ElicitationAccept {
                content: content.map(Some),
                ..Default::default()
            }),
            FormAction::Decline => CreateElicitationResponse::Decline(Empty::default()),
            FormAction::Cancel => CreateElicitationResponse::Cancel(Empty::default()),
            FormAction::Unspecified => return None,
        },
    )
}

/// What amux writes to answer one of the agent's requests: an ACP answer,
/// or one to a vendor's own request.
enum Response {
    Acp(ClientResponse),
    Vendor(ExtensionAnswer),
}

/// The answer that closes an ask nobody answered, as its request takes it.
fn cancelled(body: Option<&acp_ask::Body>) -> Response {
    match body {
        Some(acp_ask::Body::Permission(_)) => Response::Acp(ClientResponse::RequestPermission(
            acp_protocol::client::RequestPermissionResponse::cancelled(),
        )),
        Some(acp_ask::Body::Question(_)) => Response::Vendor(ExtensionAnswer::AskUserQuestion(
            AskUserQuestionResponse::new(QuestionOutcome::Cancelled, None),
        )),
        _ => Response::Acp(ClientResponse::ElicitationCreate(
            CreateElicitationResponse::Cancel(Empty::default()),
        )),
    }
}

/// Grok's answer to its questions: under each question's text, the label
/// picked, the labels picked and any words typed of one that takes
/// several, or the words typed. A question with nothing picked or typed is
/// left out, which Grok reads as unanswered. With none answered the
/// questions are declined (`cancelled`: Grok goes on with its best
/// judgment); Grok's own skip is for its plan interview, and tells it to
/// finish the plan. None when a pick names no option.
fn question_response(
    asked: &wire::QuestionAsk,
    answer: &wire::QuestionAnswer,
) -> Option<ExtensionAnswer> {
    let mut answers = BTreeMap::new();
    for (question, response) in asked.questions.iter().zip(&answer.answers) {
        let mut said = response
            .selected
            .iter()
            .map(|at| {
                question
                    .options
                    .get(*at as usize)
                    .map(|option| option.label.clone())
            })
            .collect::<Option<Vec<_>>>()?;
        said.extend(
            response
                .other
                .clone()
                .filter(|other| !other.trim().is_empty()),
        );
        let answer = match (question.multi_select, said.len()) {
            (_, 0) => continue,
            (false, 1) => QuestionAnswer::One(said.remove(0)),
            (false, _) => QuestionAnswer::One(said.join(", ")),
            (true, _) => QuestionAnswer::Many(said),
        };
        answers.insert(question.question.clone(), answer);
    }
    let response = if answers.is_empty() {
        AskUserQuestionResponse::new(QuestionOutcome::Cancelled, None)
    } else {
        AskUserQuestionResponse::new(QuestionOutcome::Accepted, Some(answers))
    };
    Some(ExtensionAnswer::AskUserQuestion(response))
}

impl Checkpoint for State {
    fn resume(mut self) -> (Self, Step) {
        let body = self.body();
        let step = self.shared.resume_step(body);
        (self, step)
    }
}

const KIND: wire::Kind = wire::Kind::Acp;

impl Interpreter for Acp {
    type State = State;
    const KIND: wire::Kind = KIND;

    fn initial(spec: &AgentSpec) -> (State, Step) {
        let mut state = State::new(spec);
        let step = state.shared.initial_step(Self::unknown_snapshot());
        (state, step)
    }

    fn reincarnate(mut state: State, spec: &AgentSpec) -> (State, Step) {
        state.shared.reincarnate(spec);
        state.incarnation = spec.incarnation;
        state.forget_process();
        state.resume()
    }

    fn step(state: &mut State, event: Event) -> crate::Stepped {
        let mut emit = Emit::default();
        match event {
            Event::Tick { at_ms } => state.shared.tick(at_ms),
            Event::Fact(fact) => state.fact(&mut emit, fact),
            Event::Input(input) => state.input(&mut emit, input),
            // An ACP agent's own subagents are not amux agents: nothing
            // addresses one.
            Event::ChildInput { input, .. } => {
                state
                    .shared
                    .reject(&mut emit, &input.input_id, Refusal::Unsupported)
            }
            Event::AgentMessage(envelope) => state.agent_message(&mut emit, envelope),
            // ACP has no way to name a session, so the name stays amux's.
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
        if state.opened
            && let Some(entry) = state.shared.next_queued()
        {
            state.submit(&mut emit, entry, false);
        }
        if std::mem::take(&mut state.offers_changed) {
            let catalogue = state.catalogue();
            state.shared.set_catalogue(&mut emit, catalogue);
        }
        let body = state.body();
        let notice = state.shared.notice(crate::notice::acp_ask);
        state.shared.finish(emit, body, notice)
    }

    fn pending_messages(state: &State) -> Vec<Vec<u8>> {
        state.shared.pending_messages().iter().cloned().collect()
    }

    fn typed_unrecorded(state: &State) -> bool {
        state.shared.typed_unrecorded()
    }

    /// The shared holds, and one of ACP's own: a session the agent cannot
    /// reopen in a new process ends with this one.
    fn holds(state: &State) -> crate::Holds {
        let mut holds = state.shared.holds();
        holds.unresumable = state.session_id.is_some()
            && state
                .reopens
                .is_none_or(|reopens| !reopens.resume && !reopens.load);
        holds
    }

    fn fact_shape(fact: &crate::Fact) -> crate::shape::FactShape {
        match fact.channel {
            // A stderr line is the agent's own words: none of it is kept.
            crate::Channel::Agent => crate::shape::FactShape {
                variant: "stderr".into(),
                ..Default::default()
            },
            _ => crate::shape::acp_fact(fact),
        }
    }

    fn input_shape(input: &Input) -> crate::shape::InputShape {
        crate::shape::acp_input(&input.body)
    }

    fn structure(state: &State) -> crate::shape::Structure {
        state.shared.structure(|body| Self::item_shape(body).arm)
    }

    fn unknown_snapshot() -> Vec<u8> {
        unknown::acp().encode_to_vec()
    }

    fn describe_item(body: &[u8]) -> ItemView {
        describe_item(body)
    }

    fn describe_snapshot(body: &[u8]) -> SnapshotView {
        describe_snapshot(body)
    }

    fn fixture_input(input_id: Vec<u8>, input: &FixtureInput) -> Option<Input> {
        crate::golden::acp_input(input_id, input)
    }

    fn recording(format: &str, bytes: &[u8]) -> Result<Vec<Event>, String> {
        recording::read(format, bytes)
    }
}

fn describe_tool(tool: &AcpTool) -> String {
    let mut text = format!(
        "{} {} {}",
        tool.kind()
            .as_str_name()
            .trim_start_matches("ACP_TOOL_KIND_"),
        tool.state().as_str_name().trim_start_matches("TOOL_STATE_"),
        clip(&tool.title, 60),
    );
    if let Some(decision) = &tool.decision {
        text.push_str(&format!(
            " decision={}",
            decision
                .outcome()
                .as_str_name()
                .trim_start_matches("DECISION_OUTCOME_")
        ));
    }
    if !tool.ask_item_key.is_empty() {
        text.push_str(&format!(" asks={}", tool.ask_item_key));
    }
    for location in &tool.locations {
        text.push_str(&format!(" at={}", location.path));
        if let Some(line) = location.line {
            text.push_str(&format!(":{line}"));
        }
    }
    for diff in &tool.diffs {
        text.push_str(&format!(
            " diff={}{} +{}",
            diff.path,
            if diff.old_text.is_none() { "(new)" } else { "" },
            diff.new_text.lines().count()
        ));
    }
    if !tool.input_json.is_empty() {
        text.push_str(&format!(
            " input={}",
            clip(&String::from_utf8_lossy(&tool.input_json), 80)
        ));
    }
    text
}

fn describe_item(body: &[u8]) -> ItemView {
    use acp_item::Kind;
    let item = AcpItem::decode(body).unwrap_or_default();
    let (arm, complete, text) = match item.kind {
        None => ("none", true, String::new()),
        Some(Kind::Prompt(_)) => ("prompt", true, String::new()),
        Some(Kind::Message(text)) => ("message", text.complete, String::new()),
        Some(Kind::Thought(text)) => ("thought", text.complete, String::new()),
        Some(Kind::Tool(tool)) => ("tool", finished(tool.state), describe_tool(&tool)),
        Some(Kind::Turn(turn)) => (
            "turn",
            true,
            format!(
                "#{} {} started={}",
                turn.turn_id,
                wire::TurnOutcome::try_from(turn.outcome)
                    .map_or("?", |outcome| outcome.as_str_name()),
                turn.started_at_ms,
            ),
        ),
        Some(Kind::Steer(_)) => ("steer", true, String::new()),
        Some(Kind::Compaction(compaction)) => (
            "compaction",
            true,
            format!(
                "before={} after={} automatic={}",
                compaction
                    .tokens_before
                    .map_or("?".to_owned(), |tokens| tokens.to_string()),
                compaction
                    .tokens_after
                    .map_or("?".to_owned(), |tokens| tokens.to_string()),
                compaction.automatic
            ),
        ),
        Some(Kind::Notice(notice)) => (
            "notice",
            true,
            format!(
                "{} {} {}",
                notice
                    .kind()
                    .as_str_name()
                    .trim_start_matches("ACP_NOTICE_KIND_"),
                notice
                    .severity()
                    .as_str_name()
                    .trim_start_matches("ACP_NOTICE_SEVERITY_"),
                or_dash(&notice.title)
            ),
        ),
        Some(Kind::AgentMessage(message)) => (
            "agent_message",
            true,
            crate::shared::describe_agent_message(&message),
        ),
        Some(Kind::Error(error)) => (
            "error",
            true,
            format!("{} {}", error.error_kind, clip(&error.message, 80)),
        ),
        Some(Kind::Ask(item)) => ("ask", item.closed.is_some(), ask_item::describe(&item)),
        Some(Kind::Boundary(boundary)) => (
            "boundary",
            true,
            format!(
                "{}{}",
                wire::BoundaryKind::try_from(boundary.kind).map_or("?", |kind| kind.as_str_name()),
                describe_cause(&boundary)
            ),
        ),
        Some(Kind::Unrecognized(unrecognized)) => (
            "unrecognized",
            true,
            format!(
                "{} {}",
                unrecognized.fact_type,
                serde_json::Value::String(unrecognized.summary)
            ),
        ),
    };
    ItemView {
        arm: arm.into(),
        complete,
        text,
    }
}

fn describe_ask(ask: &AcpAsk) -> String {
    match &ask.body {
        Some(acp_ask::Body::Permission(permission)) => format!(
            "{}:permission[{}]",
            ask.key,
            permission
                .options
                .iter()
                .map(|option| format!(
                    "{}={}",
                    option.option_id,
                    option
                        .kind()
                        .as_str_name()
                        .trim_start_matches("ACP_PERMISSION_KIND_")
                ))
                .collect::<Vec<_>>()
                .join(",")
        ),
        Some(acp_ask::Body::Form(form)) => format!("{}:form {}", ask.key, clip(&form.message, 40)),
        Some(acp_ask::Body::Link(link)) => format!("{}:link {}", ask.key, clip(&link.message, 40)),
        Some(acp_ask::Body::Question(question)) => format!(
            "{}:question {}",
            ask.key,
            question
                .questions
                .iter()
                .map(|question| clip(&question.question, 40))
                .collect::<Vec<_>>()
                .join(" | ")
        ),
        None => format!("{}:?", ask.key),
    }
}

fn describe_snapshot(body: &[u8]) -> SnapshotView {
    let snapshot = AcpSnapshot::decode(body).unwrap_or_default();
    let context = snapshot.context.unwrap_or_default();
    let usage = snapshot.usage.clone().unwrap_or_default();
    let sign_in = snapshot.sign_in.clone().unwrap_or_default();
    let opt = |value: &Option<String>| value.clone().unwrap_or_else(|| "?".to_owned());
    let tokens = |value: Option<u64>| value.map_or("?".to_owned(), |tokens| tokens.to_string());
    SnapshotView {
        asks: snapshot
            .asks
            .iter()
            .map(|ask| (ask.key.clone(), ask.item_key.clone()))
            .collect(),
        text: format!(
            "agent={} session={} asks=[{}] model={} model_name={} effort={} mode={} context={} plan={} usage=in:{}/out:{}/thought:{}/total:{} cost={} sign_in={} reopen={}",
            opt(&snapshot.agent),
            opt(&snapshot.session_id),
            snapshot
                .asks
                .iter()
                .map(describe_ask)
                .collect::<Vec<_>>()
                .join(","),
            opt(&snapshot.model),
            opt(&snapshot.model_name),
            opt(&snapshot.effort),
            opt(&snapshot.mode),
            if context.known {
                format!("{}/{}", context.used_tokens, tokens(context.window_tokens))
            } else {
                "?".to_owned()
            },
            snapshot
                .plan
                .as_ref()
                .map_or("?".to_owned(), crate::claude_common::describe_tasks),
            tokens(usage.input_tokens),
            tokens(usage.output_tokens),
            tokens(usage.thought_tokens),
            tokens(usage.total_tokens),
            usage
                .cost
                .map_or("?".to_owned(), |cost| format!("{cost}{}", usage.currency)),
            wire::SignInState::try_from(sign_in.state)
                .map_or("?", |state| state.as_str_name())
                .trim_start_matches("SIGN_IN_STATE_"),
            snapshot
                .reopen
                .and_then(|reopen| wire::AcpReopen::try_from(reopen).ok())
                .map_or("?", |reopen| reopen.as_str_name())
                .trim_start_matches("ACP_REOPEN_"),
        ),
    }
}
