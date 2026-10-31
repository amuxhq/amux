//! The thin per-kind layer: the only place the session model knows an agent
//! kind. It decodes item and snapshot bodies and reduces each item to the few
//! kind-neutral facts the model derives from (run membership, the activity
//! line, attachment references). Everything else about a body is left to the
//! views, which read the decoded body directly.

use prost::Message;
use wire::{
    Ask, BackgroundJobs, ClaudeItem, ClaudeSnapshot, CodexAsk, CodexItem, CodexSnapshot,
    ContextMeter, Kind, OfferedCommand, OfferedModel, SignIn, Task, TaskList, ToolServer,
    UsageState,
};

/// A decoded item body. `Undecodable` covers an empty or unreadable body and
/// an item kind this build does not know; views draw it as unrecognized.
#[derive(Clone, Debug, PartialEq)]
pub enum ItemBody {
    /// Either Claude kind's item.
    Claude(wire::claude_item::Kind),
    Codex(wire::codex_item::Kind),
    Undecodable,
}

impl ItemBody {
    pub fn decode(kind: Kind, body: &[u8]) -> ItemBody {
        let decoded = match kind {
            Kind::ClaudePty | Kind::ClaudeSdk => ClaudeItem::decode(body)
                .ok()
                .and_then(|item| item.kind)
                .map(ItemBody::Claude),
            Kind::Codex => CodexItem::decode(body)
                .ok()
                .and_then(|item| item.kind)
                .map(ItemBody::Codex),
            Kind::Unspecified => None,
        };
        decoded.unwrap_or(ItemBody::Undecodable)
    }

    /// The kind-neutral facts the model derives from.
    pub fn class(&self) -> ItemClass {
        use wire::claude_item::Kind as Claude;
        use wire::codex_item::Kind as Codex;
        match self {
            ItemBody::Claude(kind) => match kind {
                Claude::Prompt(_) => ItemClass::Prompt,
                Claude::Steer(_) => ItemClass::Steer,
                Claude::Message(text) => ItemClass::Prose {
                    complete: text.complete,
                },
                Claude::Thinking(thinking) => ItemClass::Thinking {
                    complete: thinking.complete,
                },
                Claude::Tool(tool) => claude_tool(tool),
                Claude::Turn(_) => ItemClass::Turn,
                Claude::ApiError(error) => api_error(error),
                Claude::Boundary(_) => ItemClass::Boundary,
                Claude::AgentMessage(_) => ItemClass::AgentMessage,
                Claude::Status(status) if status.status() == wire::SdkStatus::Compacting => {
                    ItemClass::Compacting
                }
                Claude::Status(_) => ItemClass::Silent,
                Claude::Task(_)
                | Claude::Slash(_)
                | Claude::Unrecognized(_)
                | Claude::ModelSwitch(_)
                | Claude::Compaction(_) => ItemClass::Other,
                Claude::Ask(_) | Claude::Plan(_) => ItemClass::Ask,
            },
            ItemBody::Codex(kind) => match kind {
                Codex::Prompt(_) => ItemClass::Prompt,
                Codex::SafetyCheck(check) if check.running => ItemClass::SafetyCheck,
                Codex::SafetyCheck(_) => ItemClass::Silent,
                Codex::CommandOutput(_) => ItemClass::Other,
                Codex::Steer(_) => ItemClass::Steer,
                Codex::Message(text) | Codex::WorkingNote(text) => ItemClass::Prose {
                    complete: text.complete,
                },
                Codex::Reasoning(reasoning) => ItemClass::Thinking {
                    complete: reasoning.complete,
                },
                Codex::Work(work) => codex_work(work),
                Codex::Turn(_) => ItemClass::Turn,
                Codex::Error(error) => api_error(error),
                Codex::Boundary(_) => ItemClass::Boundary,
                Codex::AgentMessage(_) => ItemClass::AgentMessage,
                Codex::Unrecognized(_) | Codex::Verdict(_) => ItemClass::Other,
                Codex::Ask(_) | Codex::Plan(_) => ItemClass::Ask,
            },
            ItemBody::Undecodable => ItemClass::Other,
        }
    }
}

/// What the session model needs to know about an item, whatever its kind.
#[derive(Clone, Debug, PartialEq)]
pub enum ItemClass {
    Prompt,
    Steer,
    Prose {
        complete: bool,
    },
    Thinking {
        complete: bool,
    },
    Tool(ToolFacts),
    Turn,
    /// The provider failed a request and says it will try again.
    Retrying {
        attempt: u32,
        max_attempts: u32,
        retry_at_ms: Option<i64>,
    },
    Error,
    Compacting,
    /// The provider holds the answer for a slower safety check.
    SafetyCheck,
    Boundary,
    AgentMessage,
    /// An ask that is the work, or a plan put to the person. It is a
    /// decision, not activity: the activity line reads past it to the work
    /// it interrupted.
    Ask,
    /// Drawn by no client: Claude's status bookkeeping, a finished safety
    /// check.
    Silent,
    Other,
}

/// How an item stands toward a run of tool steps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fold {
    /// A tool step: it is what a run is made of.
    Step,
    /// Inside a run it neither counts nor ends it: thinking, a retry
    /// notice, a subagent's own step, anything drawn by no client.
    Through,
    /// Anything else the agent or the person put in the transcript ends a
    /// run: its text, an ask, a prompt, a turn's end.
    Break,
}

impl ItemClass {
    pub fn fold(&self) -> Fold {
        match self {
            ItemClass::Tool(tool) if tool.child || tool.hidden => Fold::Through,
            ItemClass::Tool(_) => Fold::Step,
            ItemClass::Thinking { .. } | ItemClass::Retrying { .. } | ItemClass::Silent => {
                Fold::Through
            }
            _ => Fold::Break,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ToolFacts {
    /// Pending or running.
    pub in_flight: bool,
    /// A subagent call whose child has not finished.
    pub subagent: bool,
    /// A subagent's own step, drawn under its subagent's call.
    pub child: bool,
    /// Claude's task-list bookkeeping, drawn by no client: the task list
    /// shows what it did.
    pub hidden: bool,
}

fn in_flight(state: i32) -> bool {
    matches!(
        wire::ToolState::try_from(state),
        Ok(wire::ToolState::Pending | wire::ToolState::Running)
    )
}

fn claude_tool(tool: &wire::ToolCall) -> ItemClass {
    ItemClass::Tool(ToolFacts {
        in_flight: in_flight(tool.state),
        subagent: tool
            .subagent
            .as_ref()
            .is_some_and(|progress| !progress.finished)
            && in_flight(tool.state),
        child: !tool.parent_key.is_empty(),
        hidden: matches!(
            tool.input.as_ref().and_then(|input| input.of.as_ref()),
            Some(wire::tool_input::Of::TaskList(_))
        ),
    })
}

fn codex_work(work: &wire::Work) -> ItemClass {
    use wire::work::Of;
    ItemClass::Tool(ToolFacts {
        in_flight: in_flight(work.state),
        subagent: matches!(work.of, Some(Of::Collab(_))) && in_flight(work.state),
        child: false,
        hidden: false,
    })
}

fn api_error(error: &wire::ApiError) -> ItemClass {
    if error.will_retry {
        ItemClass::Retrying {
            attempt: error.attempt,
            max_attempts: error.max_attempts,
            retry_at_ms: error.retry_at_ms,
        }
    } else {
        ItemClass::Error
    }
}

/// An open ask in the provider's own shape; an answer names it by key.
#[derive(Clone, Debug, PartialEq)]
pub enum OpenAsk {
    Claude(Ask),
    Codex(CodexAsk),
}

impl OpenAsk {
    pub fn key(&self) -> &str {
        match self {
            OpenAsk::Claude(ask) => &ask.key,
            OpenAsk::Codex(ask) => &ask.key,
        }
    }

    /// The item the ask points at: the call it would let run, or the item
    /// of an ask that is the work.
    pub fn item_key(&self) -> &str {
        match self {
            OpenAsk::Claude(ask) => &ask.item_key,
            OpenAsk::Codex(ask) => &ask.item_key,
        }
    }
}

/// The agent as its newest Snapshot describes it: the envelope's phase
/// and queue, and the per-kind body decoded into the facts the
/// views draw. Every body field is present, at its explicit unknown until
/// the interpreter learns it: an unset optional string, a `known: false`
/// meter or list, an UNKNOWN state. An empty body, which is what Subscribe
/// answers before the first snapshot is ingested, decodes to all unknowns.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AgentState {
    pub kind: Kind,
    pub revision: u64,
    pub phase: wire::Phase,
    pub queue: Vec<wire::QueuedInput>,
    pub at_ms: i64,
    /// In the order the interpreter opened them.
    pub asks: Vec<OpenAsk>,
    pub model: Option<String>,
    /// The running model as a person reads it, as the interpreter names it.
    pub model_name: Option<String>,
    pub effort: Option<String>,
    /// The permission in force by its catalogue value: Claude's permission
    /// mode; for Codex, the named permission its settings make, None when
    /// they make none (see `approval_policy`).
    pub permission: Option<String>,
    /// The mode in force by its catalogue value (Codex's collaboration
    /// mode); None for Claude, which has none.
    pub mode: Option<String>,
    /// Codex's own approval policy, None until Codex says. Set with no
    /// `permission`, Codex's settings match no named permission.
    pub approval_policy: Option<String>,
    /// The hash of the catalogue the agent offers now; None until its
    /// provider says.
    pub catalogue: Option<Vec<u8>>,
    /// The models the agent's catalogue offers, each with its efforts;
    /// empty until the catalogue is fetched, and always for terminal Claude.
    pub models: Vec<OfferedModel>,
    /// The commands the agent's catalogue offers.
    pub commands: Vec<OfferedCommand>,
    /// The skills Codex's catalogue offers, mentioned as `$name`.
    pub skills: Vec<wire::OfferedSkill>,
    /// The permissions and modes the agent's catalogue offers.
    pub permissions: Vec<wire::OfferedPermission>,
    pub modes: Vec<wire::OfferedMode>,
    /// Claude's task list; Codex's plan.
    pub tasks: TaskList,
    pub context: ContextMeter,
    pub active_tasks: Vec<Task>,
    /// Each provider's usage limits in its own terms.
    pub usage: Usage,
    pub servers: Vec<ToolServer>,
    pub sign_in: SignIn,
    pub background: BackgroundJobs,
    pub provider_session: Option<String>,
    /// Terminal Claude's calls its hooks announced before their rows
    /// landed, oldest first.
    pub running_calls: Vec<wire::RunningCall>,
    /// Terminal Claude's own view resumed another conversation, this
    /// session: prompts are refused until it is back on the agent's own.
    pub switched_away: bool,
}

/// An agent's usage limits as its provider names them.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Usage {
    #[default]
    Unknown,
    Claude(wire::ClaudeUsage),
    Codex(wire::CodexUsage),
}

impl Usage {
    /// Whether the agent can work at all.
    pub fn state(&self) -> UsageState {
        match self {
            Usage::Unknown => UsageState::Unknown,
            Usage::Claude(usage) => usage.state(),
            Usage::Codex(usage) => usage.state(),
        }
    }
}

/// The kind an envelope names, when it is one this build knows.
pub(crate) fn named_kind(kind: i32) -> Option<Kind> {
    Kind::try_from(kind)
        .ok()
        .filter(|kind| *kind != Kind::Unspecified)
}

/// Decodes a snapshot body. A body that does not decode is drawn as nothing
/// known, the same as an empty one.
pub fn decode_snapshot(kind: Kind, body: &[u8]) -> AgentState {
    let mut state = AgentState {
        kind,
        ..AgentState::default()
    };
    if body.is_empty() {
        return state;
    }
    match kind {
        Kind::ClaudePty | Kind::ClaudeSdk => {
            let Ok(snapshot) = ClaudeSnapshot::decode(body) else {
                return state;
            };
            state.asks = snapshot.asks.into_iter().map(OpenAsk::Claude).collect();
            state.tasks = snapshot.tasks.unwrap_or_default();
            state.context = snapshot.context.unwrap_or_default();
            state.model = snapshot.model;
            state.model_name = snapshot.model_name;
            state.effort = snapshot.effort;
            state.permission = snapshot.permission;
            state.active_tasks = snapshot.active_tasks;
            state.provider_session = snapshot.provider_session;
            state.usage = snapshot.usage.map_or(Usage::Unknown, Usage::Claude);
            state.servers = snapshot.servers;
            state.sign_in = snapshot.sign_in.unwrap_or_default();
            state.background = snapshot.background_jobs.unwrap_or_default();
            state.running_calls = snapshot.running_calls;
            state.switched_away = snapshot.switched_away.unwrap_or(false);
        }
        Kind::Codex => {
            let Ok(snapshot) = CodexSnapshot::decode(body) else {
                return state;
            };
            state.asks = snapshot.asks.into_iter().map(OpenAsk::Codex).collect();
            state.tasks = snapshot.plan.unwrap_or_default();
            state.context = snapshot.context.unwrap_or_default();
            state.model = snapshot.model;
            state.model_name = snapshot.model_name;
            state.effort = snapshot.effort;
            state.permission = snapshot.permission;
            state.mode = snapshot.mode;
            state.approval_policy = snapshot.approval_policy;
            state.provider_session = snapshot.thread_id;
            state.usage = snapshot.usage.map_or(Usage::Unknown, Usage::Codex);
            state.servers = snapshot.servers;
            state.sign_in = snapshot.sign_in.unwrap_or_default();
            state.background = snapshot.background_jobs.unwrap_or_default();
        }
        Kind::Unspecified => {}
    }
    state
}

impl AgentState {
    /// The whole snapshot: envelope plus decoded body. The envelope's kind
    /// wins over the caller's when it names one this build knows.
    pub fn from_snapshot(kind: Kind, snapshot: &wire::Snapshot) -> AgentState {
        let kind = named_kind(snapshot.kind).unwrap_or(kind);
        AgentState {
            revision: snapshot.revision,
            phase: snapshot.phase(),
            queue: snapshot.queue.clone(),
            at_ms: snapshot.at_ms,
            catalogue: snapshot.catalogue.clone(),
            ..decode_snapshot(kind, &snapshot.body)
        }
    }
}
