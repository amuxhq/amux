//! The thin per-kind layer: the only place the session model knows an agent
//! kind. It decodes item and snapshot bodies and reduces each item to the few
//! kind-neutral facts the model derives from (run membership, the activity
//! line, attachment references). Everything else about a body is left to the
//! views, which read the decoded body directly.

use prost::Message;
use wire::{
    BackgroundJobs, ClaudeAsk, ClaudeItem, ClaudeSnapshot, ClaudeTask, CodexAsk, CodexItem,
    CodexSnapshot, ContextMeter, Kind, OfferedCommand, OfferedModel, SignIn, TaskList, ToolServer,
    UsageState,
};

/// A decoded item body. `Undecodable` covers an empty or unreadable body and
/// an item kind this build does not know; views draw it as unrecognized.
#[derive(Clone, Debug, PartialEq)]
pub enum ItemBody {
    /// Either Claude kind's item.
    Claude(wire::claude_item::Kind),
    Codex(wire::codex_item::Kind),
    /// Any ACP agent's item: one envelope serves them all.
    Acp(wire::acp_item::Kind),
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
            Kind::Acp => wire::AcpItem::decode(body)
                .ok()
                .and_then(|item| item.kind)
                .map(ItemBody::Acp),
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
                Codex::Unrecognized(_) | Codex::Verdict(_) | Codex::Compaction(_) => {
                    ItemClass::Other
                }
                Codex::Ask(_) | Codex::Plan(_) => ItemClass::Ask,
            },
            ItemBody::Acp(kind) => acp_class(kind),
            ItemBody::Undecodable => ItemClass::Other,
        }
    }
}

fn acp_class(kind: &wire::acp_item::Kind) -> ItemClass {
    use wire::acp_item::Kind as Acp;
    match kind {
        Acp::Prompt(_) => ItemClass::Prompt,
        Acp::Steer(_) => ItemClass::Steer,
        Acp::Message(text) => ItemClass::Prose {
            complete: text.complete,
        },
        Acp::Thought(thinking) => ItemClass::Thinking {
            complete: thinking.complete,
        },
        // ACP has no subagents of its own, and amux's spawn is drawn by
        // the child it names on its own row. A question tool's call is
        // drawn by its ask's row.
        Acp::Tool(tool) => ItemClass::Tool(ToolFacts {
            in_flight: in_flight(tool.state),
            subagent: false,
            child: false,
            opens: false,
            hidden: !tool.ask_item_key.is_empty(),
        }),
        Acp::Turn(_) => ItemClass::Turn,
        Acp::Error(error) => api_error(error),
        Acp::Boundary(_) => ItemClass::Boundary,
        Acp::AgentMessage(_) => ItemClass::AgentMessage,
        Acp::Ask(_) => ItemClass::Ask,
        Acp::Compaction(_) | Acp::Notice(_) | Acp::Unrecognized(_) => ItemClass::Other,
    }
}

/// An item body as a session's record carries it: the arm it decoded as,
/// and of its fields only those [`ItemBody::class`] reads, so the record
/// draws every conclusion the model draws and holds nothing anybody wrote.
/// Those fields are whether a message or thinking block is complete; a
/// tool call's state, its parent's key, whether its subagent finished,
/// whether its input is a task list or a subagent, and the agent it opens; an error's retry, attempt, limit and
/// time; headless Claude's status; whether Codex's safety check runs; and
/// the arm of Codex's work, with the agent a spawn opens and a collab
/// call's tool. Every other field, text and value is emptied.
/// A body this build cannot decode becomes an empty one, which reads the
/// same.
pub fn item_structure(kind: Kind, body: &[u8]) -> Vec<u8> {
    match ItemBody::decode(kind, body) {
        ItemBody::Claude(kind) => ClaudeItem {
            kind: Some(claude_structure(kind)),
        }
        .encode_to_vec(),
        ItemBody::Codex(kind) => CodexItem {
            kind: Some(codex_structure(kind)),
        }
        .encode_to_vec(),
        ItemBody::Acp(kind) => wire::AcpItem {
            kind: Some(acp_structure(kind)),
        }
        .encode_to_vec(),
        ItemBody::Undecodable => Vec::new(),
    }
}

fn acp_structure(kind: wire::acp_item::Kind) -> wire::acp_item::Kind {
    use wire::acp_item::Kind as Acp;
    match kind {
        Acp::Message(text) => Acp::Message(wire::Text {
            complete: text.complete,
        }),
        Acp::Thought(thinking) => Acp::Thought(wire::Thinking {
            complete: thinking.complete,
        }),
        Acp::Tool(tool) => Acp::Tool(wire::AcpTool {
            state: tool.state,
            ..Default::default()
        }),
        Acp::Error(error) => Acp::Error(api_error_structure(error)),
        Acp::Prompt(_) => Acp::Prompt(Default::default()),
        Acp::Turn(_) => Acp::Turn(Default::default()),
        Acp::Steer(_) => Acp::Steer(Default::default()),
        Acp::Compaction(_) => Acp::Compaction(Default::default()),
        Acp::Notice(_) => Acp::Notice(Default::default()),
        Acp::AgentMessage(_) => Acp::AgentMessage(Default::default()),
        Acp::Ask(_) => Acp::Ask(Default::default()),
        Acp::Boundary(_) => Acp::Boundary(Default::default()),
        Acp::Unrecognized(_) => Acp::Unrecognized(Default::default()),
    }
}

fn claude_structure(kind: wire::claude_item::Kind) -> wire::claude_item::Kind {
    use wire::claude_item::Kind as Claude;
    match kind {
        Claude::Message(text) => Claude::Message(wire::Text {
            complete: text.complete,
        }),
        Claude::Thinking(thinking) => Claude::Thinking(wire::Thinking {
            complete: thinking.complete,
        }),
        Claude::Tool(tool) => Claude::Tool(tool_structure(tool)),
        Claude::ApiError(error) => Claude::ApiError(api_error_structure(error)),
        Claude::Status(status) => Claude::Status(wire::ClaudeStatus {
            status: status.status,
            word: String::new(),
        }),
        Claude::Prompt(_) => Claude::Prompt(Default::default()),
        Claude::Turn(_) => Claude::Turn(Default::default()),
        Claude::Compaction(_) => Claude::Compaction(Default::default()),
        Claude::AgentMessage(_) => Claude::AgentMessage(Default::default()),
        Claude::Boundary(_) => Claude::Boundary(Default::default()),
        Claude::Slash(_) => Claude::Slash(Default::default()),
        Claude::Unrecognized(_) => Claude::Unrecognized(Default::default()),
        Claude::Steer(_) => Claude::Steer(Default::default()),
        Claude::Ask(_) => Claude::Ask(Default::default()),
        Claude::Plan(_) => Claude::Plan(Default::default()),
        Claude::Task(_) => Claude::Task(Default::default()),
        Claude::ModelSwitch(_) => Claude::ModelSwitch(Default::default()),
    }
}

fn codex_structure(kind: wire::codex_item::Kind) -> wire::codex_item::Kind {
    use wire::codex_item::Kind as Codex;
    match kind {
        Codex::Message(text) => Codex::Message(wire::Text {
            complete: text.complete,
        }),
        Codex::WorkingNote(text) => Codex::WorkingNote(wire::Text {
            complete: text.complete,
        }),
        Codex::Reasoning(reasoning) => Codex::Reasoning(wire::Reasoning {
            complete: reasoning.complete,
            summary: Vec::new(),
        }),
        Codex::Work(work) => Codex::Work(work_structure(work)),
        Codex::Error(error) => Codex::Error(api_error_structure(error)),
        Codex::SafetyCheck(check) => Codex::SafetyCheck(wire::SafetyCheck {
            running: check.running,
        }),
        Codex::Prompt(_) => Codex::Prompt(Default::default()),
        Codex::Turn(_) => Codex::Turn(Default::default()),
        Codex::Steer(_) => Codex::Steer(Default::default()),
        Codex::Boundary(_) => Codex::Boundary(Default::default()),
        Codex::AgentMessage(_) => Codex::AgentMessage(Default::default()),
        Codex::Unrecognized(_) => Codex::Unrecognized(Default::default()),
        Codex::Verdict(_) => Codex::Verdict(Default::default()),
        Codex::Ask(_) => Codex::Ask(Default::default()),
        Codex::Plan(_) => Codex::Plan(Default::default()),
        Codex::CommandOutput(_) => Codex::CommandOutput(Default::default()),
        Codex::Compaction(_) => Codex::Compaction(Default::default()),
    }
}

fn tool_structure(tool: wire::ClaudeToolCall) -> wire::ClaudeToolCall {
    use wire::claude_tool_input::Of;
    let input = match tool.input.as_ref().and_then(|input| input.of.as_ref()) {
        Some(Of::TaskList(_)) => Some(Of::TaskList(wire::Empty {})),
        Some(Of::Subagent(_)) => Some(Of::Subagent(Default::default())),
        _ => None,
    };
    wire::ClaudeToolCall {
        state: tool.state,
        parent_key: tool.parent_key,
        subagent: tool.subagent.map(|progress| wire::SubagentProgress {
            finished: progress.finished,
            ..Default::default()
        }),
        input: input.map(|of| wire::ClaudeToolInput {
            of: Some(of),
            ..Default::default()
        }),
        // An agent's id is made by amux, not written by anybody.
        child_agent_id: tool.child_agent_id,
        ..Default::default()
    }
}

fn work_structure(work: wire::Work) -> wire::Work {
    use wire::work::Of;
    wire::Work {
        of: work.of.map(|of| match of {
            Of::Command(_) => Of::Command(Default::default()),
            Of::FileChange(_) => Of::FileChange(Default::default()),
            // Agent ids are made by amux, and a collab tool's name is
            // Codex's own vocabulary: neither is anybody's writing.
            Of::Mcp(call) => Of::Mcp(wire::McpToolCall {
                child_agent_id: call.child_agent_id,
                ..Default::default()
            }),
            Of::WebSearch(_) => Of::WebSearch(Default::default()),
            Of::Image(_) => Of::Image(Default::default()),
            Of::Collab(collab) => Of::Collab(wire::CollabWork {
                tool: collab.tool,
                agent_ids: collab.agent_ids,
                ..Default::default()
            }),
        }),
        state: work.state,
        ..Default::default()
    }
}

fn api_error_structure(error: wire::ApiError) -> wire::ApiError {
    wire::ApiError {
        will_retry: error.will_retry,
        attempt: error.attempt,
        max_attempts: error.max_attempts,
        retry_at_ms: error.retry_at_ms,
        ..Default::default()
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
    /// check, an old Codex deprecation notice.
    Silent,
    Other,
}

/// How an item stands toward a run of tool steps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fold {
    /// A tool step: it is what a run is made of.
    Step,
    /// Inside a run it neither counts nor ends it: thinking, a retry
    /// notice, a subagent's own step from history, anything drawn by no
    /// client.
    Through,
    /// Anything else the agent or the person put in the transcript ends a
    /// run: its text, an ask, a prompt, a turn's end, a call that opens an
    /// agent.
    Break,
}

impl ItemClass {
    pub fn fold(&self) -> Fold {
        match self {
            // A call that opens an agent stands for that agent's work, which
            // the person reaches through it: it is not folded away with the
            // steps around it.
            ItemClass::Tool(tool) if tool.opens => Fold::Break,
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
    /// A subagent call still under way: the agent waits on its subagent.
    pub subagent: bool,
    /// A subagent's own step, drawn under its subagent's call: only in
    /// history, from before a subagent's steps went to its own chat.
    pub child: bool,
    /// The call opens an agent: the provider subagent or amux child it
    /// started or continues (see [`opens`]).
    pub opens: bool,
    /// Bookkeeping drawn by no client: Claude's task list, which the task
    /// list shows, Codex's calls on child threads that start none, and an
    /// ACP agent's question tool's call, which its ask's row stands for.
    pub hidden: bool,
}

fn in_flight(state: i32) -> bool {
    matches!(
        wire::ToolState::try_from(state),
        Ok(wire::ToolState::Pending | wire::ToolState::Running)
    )
}

fn claude_tool(tool: &wire::ClaudeToolCall) -> ItemClass {
    use wire::claude_tool_input::Of;
    let input = tool.input.as_ref().and_then(|input| input.of.as_ref());
    // History's background subagent outlived its call; its progress said
    // when it finished.
    let finished = tool
        .subagent
        .as_ref()
        .is_some_and(|progress| progress.finished);
    ItemClass::Tool(ToolFacts {
        in_flight: in_flight(tool.state),
        subagent: matches!(input, Some(Of::Subagent(_))) && !finished && in_flight(tool.state),
        child: !tool.parent_key.is_empty(),
        hidden: matches!(input, Some(Of::TaskList(_))),
        opens: !tool.child_agent_id.is_empty(),
    })
}

fn codex_work(work: &wire::Work) -> ItemClass {
    use wire::work::Of;
    ItemClass::Tool(ToolFacts {
        in_flight: in_flight(work.state),
        subagent: matches!(work.of, Some(Of::Collab(_))) && in_flight(work.state),
        child: false,
        // Waiting on, closing, interrupting or listing child threads draws
        // nothing: the rows that opened those children say how they stand.
        hidden: matches!(&work.of, Some(Of::Collab(collab))
            if !OPENING_COLLAB_TOOLS.contains(&collab.tool.as_str())),
        opens: codex_opened(work).is_some(),
    })
}

/// The agent a call opens: the provider subagent or amux child it started
/// or continues, once the interpreter knows it. Headless and terminal
/// Claude name it on the Agent call, on a SendMessage continuing it and on
/// amux's spawn; Codex on a collab call that starts or continues a child
/// thread, and on amux's spawn. A call from before amux knew it names none,
/// and draws as it always did.
pub fn opens(body: &ItemBody) -> Option<&[u8]> {
    match body {
        ItemBody::Claude(wire::claude_item::Kind::Tool(tool)) => {
            Some(tool.child_agent_id.as_slice()).filter(|id| !id.is_empty())
        }
        ItemBody::Codex(wire::codex_item::Kind::Work(work)) => codex_opened(work),
        _ => None,
    }
}

/// Codex's collab tools that start or continue one child thread; the rest
/// (waiting on, closing, interrupting or listing children) are the parent's
/// bookkeeping on agents its spawns already open.
const OPENING_COLLAB_TOOLS: &[&str] = &[
    "spawnAgent",
    "sendInput",
    "resumeAgent",
    "sendMessage",
    "followupTask",
];

fn codex_opened(work: &wire::Work) -> Option<&[u8]> {
    use wire::work::Of;
    let id = match &work.of {
        Some(Of::Collab(collab)) if OPENING_COLLAB_TOOLS.contains(&collab.tool.as_str()) => {
            collab.agent_ids.first()?
        }
        Some(Of::Mcp(call)) => &call.child_agent_id,
        _ => return None,
    };
    Some(id.as_slice()).filter(|id| !id.is_empty())
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
    Claude(ClaudeAsk),
    Codex(CodexAsk),
    /// Any ACP agent's: a permission with the options it offered, a form or
    /// a link.
    Acp(wire::AcpAsk),
}

impl OpenAsk {
    pub fn key(&self) -> &str {
        match self {
            OpenAsk::Claude(ask) => &ask.key,
            OpenAsk::Codex(ask) => &ask.key,
            OpenAsk::Acp(ask) => &ask.key,
        }
    }

    /// The item the ask points at: the call it would let run, or the item
    /// of an ask that is the work.
    pub fn item_key(&self) -> &str {
        match self {
            OpenAsk::Claude(ask) => &ask.item_key,
            OpenAsk::Codex(ask) => &ask.item_key,
            OpenAsk::Acp(ask) => &ask.item_key,
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
    /// What the agent wants of the person now.
    pub notice: Option<wire::Notice>,
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
    pub active_tasks: Vec<ClaudeTask>,
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
    /// What an ACP agent says its session has used: tokens and cost, never
    /// a limit.
    Acp(wire::AcpUsage),
}

impl Usage {
    /// Whether the agent can work at all.
    pub fn state(&self) -> UsageState {
        match self {
            Usage::Unknown => UsageState::Unspecified,
            Usage::Claude(usage) => usage.state(),
            Usage::Codex(usage) => usage.state(),
            Usage::Acp(_) => UsageState::Unspecified,
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
            state.active_tasks = snapshot.active_tasks;
            state.usage = snapshot.usage.map_or(Usage::Unknown, Usage::Claude);
            state.servers = snapshot.servers;
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
            state.approval_policy = snapshot.approval_policy;
            state.usage = snapshot.usage.map_or(Usage::Unknown, Usage::Codex);
            state.servers = snapshot.servers;
        }
        Kind::Acp => {
            let Ok(snapshot) = wire::AcpSnapshot::decode(body) else {
                return state;
            };
            state.asks = snapshot.asks.into_iter().map(OpenAsk::Acp).collect();
            state.usage = snapshot.usage.map_or(Usage::Unknown, Usage::Acp);
            state.tasks = snapshot.plan.unwrap_or_default();
            state.context = snapshot.context.unwrap_or_default();
            state.servers = snapshot.servers;
        }
        Kind::Unspecified => {}
    }
    state
}

/// A snapshot body as a session's record carries it: of its fields only
/// those the session model reads, an ask's key and the item it points at,
/// terminal Claude's announced calls by id and time, headless Claude's
/// tasks by id and state, and whether its view switched away. What a
/// client draws from the rest (the model's name, the meters, the task
/// list) is out of the record.
pub fn snapshot_structure(kind: Kind, body: &[u8]) -> Vec<u8> {
    match kind {
        Kind::ClaudePty | Kind::ClaudeSdk => {
            let Ok(snapshot) = ClaudeSnapshot::decode(body) else {
                return Vec::new();
            };
            ClaudeSnapshot {
                asks: snapshot
                    .asks
                    .into_iter()
                    .map(|ask| ClaudeAsk {
                        key: ask.key,
                        item_key: ask.item_key,
                        ..Default::default()
                    })
                    .collect(),
                running_calls: snapshot
                    .running_calls
                    .into_iter()
                    .map(|call| wire::RunningCall {
                        tool_use_id: call.tool_use_id,
                        since_ms: call.since_ms,
                        ..Default::default()
                    })
                    .collect(),
                active_tasks: snapshot
                    .active_tasks
                    .into_iter()
                    .map(|task| ClaudeTask {
                        task_id: task.task_id,
                        state: task.state,
                        ..Default::default()
                    })
                    .collect(),
                switched_away: snapshot.switched_away,
                ..Default::default()
            }
            .encode_to_vec()
        }
        Kind::Codex => {
            let Ok(snapshot) = CodexSnapshot::decode(body) else {
                return Vec::new();
            };
            CodexSnapshot {
                asks: snapshot
                    .asks
                    .into_iter()
                    .map(|ask| CodexAsk {
                        key: ask.key,
                        item_key: ask.item_key,
                        ..Default::default()
                    })
                    .collect(),
                ..Default::default()
            }
            .encode_to_vec()
        }
        Kind::Acp => {
            let Ok(snapshot) = wire::AcpSnapshot::decode(body) else {
                return Vec::new();
            };
            wire::AcpSnapshot {
                asks: snapshot
                    .asks
                    .into_iter()
                    .map(|ask| wire::AcpAsk {
                        key: ask.key,
                        item_key: ask.item_key,
                        ..Default::default()
                    })
                    .collect(),
                ..Default::default()
            }
            .encode_to_vec()
        }
        Kind::Unspecified => Vec::new(),
    }
}

impl AgentState {
    /// The whole snapshot: envelope plus decoded body. The envelope's kind
    /// wins over the caller's when it names one this build knows.
    pub fn from_snapshot(kind: Kind, snapshot: &wire::Snapshot) -> AgentState {
        let kind = named_kind(snapshot.kind).unwrap_or(kind);
        let settings = snapshot.settings();
        AgentState {
            revision: snapshot.revision,
            phase: snapshot.phase(),
            queue: snapshot.queue.clone(),
            at_ms: snapshot.at_ms,
            catalogue: snapshot.catalogue.clone(),
            notice: snapshot.notice.as_deref().cloned(),
            model: settings.model.clone(),
            model_name: snapshot.model_name.clone(),
            effort: settings.effort.clone(),
            permission: settings.permission.clone(),
            mode: settings.mode.clone(),
            sign_in: snapshot.sign_in.clone().unwrap_or_default(),
            background: snapshot.jobs.clone().unwrap_or_default(),
            provider_session: snapshot.conversation.clone(),
            ..decode_snapshot(kind, &snapshot.body)
        }
    }
}
