//! Claude in a terminal. The facts are rows of Claude's transcript file,
//! hook payloads on the agent's hooks socket, the agent process's own launch
//! fact and the PTY's exit. Rows arrive whole; a reply streams from the
//! MessageDisplay hook until its row lands.
//!
//! The transcript is the source of rows and hooks fill only its gaps. Every
//! row, tool calls included, is emitted from its transcript row in
//! transcript order: Claude writes a call's tool_use row as the call starts
//! and its tool_result row as it ends, so a running call is live and the
//! text that introduced a call sits above it. A prompt's row precedes the
//! rows of the turn it began, so nothing waits for it. Hooks carry what the
//! transcript never does: SessionStart names the transcript and the
//! session, PermissionRequest and Elicitation open asks Claude waits on for
//! amux's decision, PermissionDenied says why auto mode refused a call,
//! MessageDisplay draws a reply as Claude writes it, PostModelSwitch names
//! the model Claude moved to, Stop closes what the turn left open, and
//! SubagentStop ends a background subagent. PreToolUse and PostToolUse
//! mark a call running, for the activity line while its row is on the way,
//! and PreToolUse names the call an ask that follows is raised for; they
//! make no row but a plan's. PostToolUse's duration says when the call
//! began running, after any wait for approval, which the call's row time
//! includes.
//!
//! Claude's asks are answered on their hooks, but a person can also answer
//! one in Claude's own terminal, which says nothing until the call's row or
//! result lands, so how an ask ends is inferred, and every inference rule
//! lives in this module:
//!
//! - An ask opens on the PermissionRequest hook, which names no tool-use id,
//!   as a card carrying the hook's tool and input; it is raised for the call
//!   the PreToolUse before it announced. Claude writes a gated call's row
//!   only once the call is decided (AskUserQuestion's once it is answered),
//!   so the card points at no row until that call's row lands, and then at
//!   that row.
//! - An answer through amux goes back on the hook, built as headless
//!   Claude's control reply is, and closes the ask with the outcome it
//!   carries. An ask that closes any other way releases its hook with no
//!   decision, so Claude carries on with its own dialog: it closes on the
//!   call's tool result (answered in the terminal: a refusal is a deny,
//!   anything else an allow), or on a later fact that proves the call is
//!   over without saying how: a new prompt, an interruption, the turn's
//!   end, a session change, the provider exiting, or a row from a later
//!   assistant message (outcome unknown, drawn dismissed, until the call's
//!   result says how it went). Nothing else closes one; a tick never does.
//! - A subagent's calls reach the hooks socket carrying its agent id and
//!   never reach the followed transcript; they are its steps, counted on
//!   the Agent row that started it, never rows of their own. An ask a
//!   subagent raises points at that Agent row and closes on the subagent's
//!   own PostToolUse for the call. A subagent Claude runs in the background
//!   answers the Agent call at once with launch metadata; its row stays
//!   running, across the end of the turn that launched it, until a user row
//!   of task-notification origin carries its result.
//! - A tool server's form or link reaches amux on the Elicitation hook,
//!   which Claude waits on before drawing its own dialog. It opens an ask
//!   with an item of its own, pointing at the call that was running (a tool
//!   server's, when one was). When that call's row has not landed yet, the
//!   item waits for it, so it sits below the call and the rows before it.
//!   An interrupt through amux closes it cancelled, cancelling it on the
//!   hook too; that call's result, a row from a later assistant message,
//!   and the facts that close every ask close it dismissed. Someone
//!   watching Claude's own terminal must see the dialog, so while a
//!   terminal client shows its screen the hook is let go at once, and one
//!   that comes to it lets go of the hook that waits: Claude draws its dialog, and
//!   the ask says it is answered there. Its item keeps what was asked.
//! - A reply's batches open a message item keyed by the hook's message id,
//!   which is not the row's. Claude writes rows in the order it draws
//!   replies, so the next text row takes over the oldest reply not yet
//!   written; a row that lands before its reply's first batch keeps its own
//!   key and the late batch draws nothing. Hooks and rows travel apart, so
//!   a reply's last batch can also come after the row that ended its turn,
//!   even after the next prompt; every hook names the prompt its turn
//!   answers, and a batch for a prompt whose turn is over draws nothing.
//! - Claude's first screen may ask whether its folder is trusted; the
//!   agent process reports it before Claude counts as ready. It opens a
//!   question with its own item, answered through amux with keys (no hook
//!   runs before it) or closed by the session starting (trusted in the
//!   terminal) or Claude exiting.
//! - A plan ask's switch is the permission the first entry of Claude's own
//!   plan menu moves to, which follows from how Claude was launched and
//!   what its model offers: never ask where Claude was launched allowing
//!   it, else auto mode where the host's Claude offers it for the running
//!   model, else accept edits. The answer agreeing to the switch names no
//!   permission: it is the ask's, fixed when the ask was made.
//! - In manual mode, where Claude offers auto mode, a permission ask offers
//!   one more scope after the suggestions, switching to auto.
//! - The permission is the one amux last cycled Claude to (amux knows
//!   Claude's cycle: default, accept edits, plan, then never ask and auto
//!   mode where they are on offer, and back to default), confirmed by the
//!   permission Claude next records on a row or hook. Nothing is read off
//!   the screen.
//! - A turn starts when Claude records its prompt, not when amux types it:
//!   keys typed into a terminal may land in one of Claude's own screens and
//!   never reach the conversation.
//! - An agent is its conversation. A SessionStart resuming another session
//!   means Claude's own view now shows a conversation that is not this
//!   agent's: the snapshot names it, prompts are refused, the other
//!   session's hooks are ignored, and switching back types `/resume` with
//!   this agent's session. A clear is this agent starting afresh.
//!
//! Terminal Claude offers no list of models, efforts or commands a program
//! could read, so it takes no model or effort input: a person types
//! `/model <name>` or `/effort <level>` as any other prompt. The model is
//! what Claude's newest assistant row ran (or what PostModelSwitch names,
//! until a row does), and the effort is the one that row ran with. The
//! permission mode changes only by cycling.

mod facts;
mod recording;

use std::collections::{BTreeMap, VecDeque};
use std::fmt;

use prost::Message as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use wire::{
    AgentSpec, Ask, AskClosed, Attachment, Boundary, BoundaryKind, ClaudeAnswer, ClaudeGrant,
    ClaudeItem, ClaudeSnapshot, ContextMeter, DecisionOutcome, Envelope, Input, KindInput, Refusal,
    RunningCall, Step, SubagentProgress, ToolCall, ToolDecision, ToolState, claude_answer,
    claude_item, claude_pty_input, tool_decision,
};

use crate::claude_answers::{self, AnswerShape, Decided};
use crate::claude_common::{
    FileStep, Jobs, Task, Verdict, describe_asks, describe_tasks, file_step, plan_item,
    question_item, same_json, task_list, tool_input,
};
use crate::{
    Carrier, Checkpoint, Effect, Emit, Event, FixtureInput, Interpreter, ItemDraft, ItemView,
    RedactTarget, SendOutcome, Shared, SnapshotView, Stepped, agent_message_body,
    agent_message_key, ask_item, claude_pty_input, human, is_send_tool, sent_message, serde_pb,
    unknown,
};

/// The interpreter for kind `claude_pty`.
pub struct ClaudePty;

/// A semantic input for the agent process to type into Claude's terminal.
/// Asks are answered through the hooks Claude waits on, not typed; what
/// is typed is a prompt, the keys that interrupt and cycle the permission
/// mode, and the answer to the folder-trust dialog, which Claude shows
/// before any hook runs.
#[derive(Clone, Debug, PartialEq)]
pub enum TerminalInput {
    /// Paste the text and submit it.
    Prompt {
        text: String,
        attachments: Vec<Attachment>,
    },
    /// Type a queued prompt into the running turn, which Claude hands the
    /// model at its next tool boundary.
    SendNow {
        text: String,
        attachments: Vec<Attachment>,
    },
    /// Stop the running turn.
    Interrupt,
    /// Start a new conversation: /clear.
    Clear,
    /// Cycle Claude's permission mode, as its own key for that does.
    CyclePermissionMode,
    /// Answer the folder-trust dialog: trust the folder, or exit.
    Trust { trust: bool },
}

impl fmt::Display for TerminalInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Prompt { text, attachments } => {
                write!(f, "prompt {}", Value::String(text.clone()))?;
                if !attachments.is_empty() {
                    write!(f, " attachments={}", attachments.len())?;
                }
                Ok(())
            }
            Self::SendNow { text, attachments } => {
                write!(f, "send now {}", Value::String(text.clone()))?;
                if !attachments.is_empty() {
                    write!(f, " attachments={}", attachments.len())?;
                }
                Ok(())
            }
            Self::Interrupt => f.write_str("interrupt"),
            Self::Clear => f.write_str("clear"),
            Self::CyclePermissionMode => f.write_str("cycle permission mode"),
            Self::Trust { trust } => write!(f, "trust {trust}"),
        }
    }
}

/// What the agent process tells the interpreter about Claude's terminal on
/// the agent channel. These are amux's own facts, not Claude's.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum AgentFact {
    /// Sent before the provider's first fact, and again after the agent
    /// process relaunches it.
    Launch,
    /// Claude's terminal first drew with bracketed paste on: its input is
    /// live, so a prompt can be typed. Some versions report their session
    /// start only after the first prompt, so nothing else says a new
    /// terminal takes input.
    Ready,
    /// Claude's first screen is its folder-trust dialog; sent before
    /// [`AgentFact::Ready`].
    TrustDialog,
    /// Whether a terminal client shows Claude's own terminal: sent when
    /// the first comes to it and when the last leaves it.
    Watched { watched: bool },
    /// The agent launched Claude without amux's tool server, which an
    /// organization's managed MCP config forbids. Headless Claude's
    /// interpreter reads this one too.
    ToolsWithheld,
}

impl AgentFact {
    fn encode(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("an agent fact serializes")
    }
}

/// The launch fact, as the agent process sends it.
pub fn launch_fact() -> Vec<u8> {
    AgentFact::Launch.encode()
}

/// The fact that Claude's terminal takes input.
pub fn ready_fact() -> Vec<u8> {
    AgentFact::Ready.encode()
}

/// The fact that Claude's first screen is its folder-trust dialog.
pub fn trust_dialog_fact() -> Vec<u8> {
    AgentFact::TrustDialog.encode()
}

/// The fact that some terminal client shows Claude's own terminal, or
/// that none any longer does.
pub fn watched_fact(watched: bool) -> Vec<u8> {
    AgentFact::Watched { watched }.encode()
}

/// The fact, for either Claude kind, that Claude was launched without
/// amux's tool server because the organization manages its MCP servers.
pub fn tools_withheld_fact() -> Vec<u8> {
    AgentFact::ToolsWithheld.encode()
}

/// What the interpreter knows about the Claude process and its session.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct Provider {
    session: Option<String>,
    transcript: Option<String>,
    launches: u32,
    /// Launched again after an earlier launch; the next session start is a
    /// restart whatever its source says.
    relaunched: bool,
    model: Option<String>,
    /// The reasoning effort Claude's newest assistant row ran with.
    effort: Option<String>,
    permission_mode: Option<String>,
    /// Launched allowing the never-ask permission.
    never_ask: bool,
    /// The permission Claude was launched in, from its arguments.
    launched_permission: Option<String>,
    /// The model Claude was launched with (`--model`), which runs until
    /// Claude names the running one: it starts its session only once a
    /// prompt arrives.
    launched_model: Option<String>,
    /// Claude's own view resumed another conversation and still shows one
    /// that is not this agent's.
    switched_away: bool,
    /// Launched without amux's tool server, by organization policy.
    tools_withheld: bool,
    /// What the host's Claude offers, from the spec: a terminal offers
    /// nothing a program can read.
    #[serde(with = "serde_pb::opt_msg")]
    offered: Option<wire::Catalogue>,
}

/// A tool call as its transcript rows report it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Tool {
    /// Arrival order, so the newest running call can be found.
    seq: u64,
    at_ms: i64,
    name: String,
    server: String,
    input: String,
    state: i32,
    outcome_text: String,
    outcome_json: String,
    class: i32,
    background: bool,
    decision: Option<Decision>,
    ended_at_ms: Option<i64>,
    /// When the call began running, after any wait for approval: its
    /// PostToolUse hook's time less the duration Claude gives.
    started_at_ms: Option<i64>,
    /// The assistant message the call's row belongs to.
    message_id: String,
    /// Its tool result row arrived.
    finished: bool,
    /// Drawn elsewhere than as a tool row: the status tool sets working_on
    /// and the task tools the task list.
    hidden: bool,
    /// An Agent call's subagent: the steps it has taken so far.
    subagent: Option<Subagent>,
    /// A subagent launched in the background: the call returned, and the
    /// task notification carries the answer.
    awaiting_notification: bool,
    /// The body last emitted, so an unchanged revision is not re-emitted.
    /// Images the tool read, by the blobs that hold them.
    #[serde(with = "serde_pb::msgs")]
    images: Vec<Attachment>,
    /// What amux's attach tool attached, from the element it answered.
    #[serde(with = "serde_pb::msgs")]
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

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct Subagent {
    tool_count: u32,
    last_tool: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Decision {
    outcome: i32,
    /// What allowing granted; None when it allowed only the call, and when
    /// it was answered in Claude's own terminal, which does not say.
    #[serde(with = "serde_pb::opt_msg")]
    grant: Option<ClaudeGrant>,
    note: String,
    elsewhere: bool,
    /// How a plan ask closed, which its plan carries instead.
    verdict: Option<Verdict>,
    /// How a question closed, which its item carries instead.
    #[serde(with = "serde_pb::opt_msg")]
    record: Option<AskClosed>,
}

impl From<Decided> for Decision {
    fn from(decided: Decided) -> Self {
        match decided {
            Decided::Tool(tool) => Self {
                outcome: tool.outcome,
                grant: tool.grant,
                note: tool.note,
                ..Self::unknown()
            },
            Decided::Plan(verdict) => Self::plan(verdict),
            Decided::Question(record) => Self::question(record),
        }
    }
}

impl Decision {
    /// Auto mode refused the call, for the rule it names.
    fn refused(reason: &str) -> Self {
        Self {
            outcome: DecisionOutcome::Denied as i32,
            note: reason.to_owned(),
            ..Self::unknown()
        }
    }

    fn elsewhere(outcome: DecisionOutcome) -> Self {
        Self {
            outcome: outcome as i32,
            grant: None,
            note: String::new(),
            elsewhere: true,
            verdict: None,
            record: None,
        }
    }

    fn unknown() -> Self {
        Self {
            outcome: DecisionOutcome::Unknown as i32,
            grant: None,
            note: String::new(),
            elsewhere: false,
            verdict: None,
            record: None,
        }
    }

    fn plan(verdict: Verdict) -> Self {
        Self {
            verdict: Some(verdict),
            ..Self::unknown()
        }
    }

    fn question(record: AskClosed) -> Self {
        Self {
            record: Some(record),
            ..Self::unknown()
        }
    }

    /// The plan's verdict this decision stands for, when it closed a plan
    /// ask: an approval reads the permission Claude left plan for.
    fn plan_verdict(&self, permission: Option<&str>) -> Verdict {
        if let Some(verdict) = &self.verdict {
            return verdict.clone();
        }
        match DecisionOutcome::try_from(self.outcome) {
            Ok(DecisionOutcome::Allowed) => Verdict::approved_for(permission),
            Ok(DecisionOutcome::Denied) => Verdict::sent_back(&self.note),
            _ => Verdict::of(wire::PlanVerdict::Dismissed),
        }
    }

    fn to_wire(&self) -> ToolDecision {
        ToolDecision {
            outcome: self.outcome,
            note: self.note.clone(),
            elsewhere: self.elsewhere,
            granted: self.grant.clone().map(tool_decision::Granted::Claude),
        }
    }
}

/// What the interpreter keeps about an ask beside the wire Ask: the call
/// it was raised for, and the hook Claude waits on for its answer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct AskMeta {
    tool_name: String,
    input: String,
    /// Each scope choice offered, as Claude sends a permission change: the
    /// hook's suggestions, then the switch to auto mode where Claude's own
    /// menu has one.
    suggestions: Vec<String>,
    shape: AskShape,
    /// The call it was raised for: the one its PreToolUse hook announced,
    /// or for a tool server's form, the call that was running.
    call: Option<String>,
    /// The call whose row it points at, once that row is drawn.
    bound: Option<String>,
    /// Closed before its call's row was drawn: the decision waits for it.
    closed: Option<Decision>,
    /// Raised inside the subagent with this agent id: never bound to a
    /// call of the main conversation.
    agent: Option<String>,
    /// The hook Claude waits on for the answer, by the number the agent
    /// process gave it; None once answered or released, and for a payload
    /// that came without one.
    hook: Option<u64>,
    /// A form or link left to Claude's own dialog, as the tool server
    /// asked it: the open ask only says it is answered in the terminal,
    /// and its item keeps what was asked.
    #[serde(with = "serde_pb::opt_msg")]
    asked: Option<Ask>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
enum AskShape {
    /// Answered through its hook, with the decision headless Claude would
    /// be handed.
    Answer(AnswerShape),
    /// Claude's folder-trust dialog, answered by typing.
    Trust,
}

/// An ask whose hook cannot be answered: it was left to Claude's own
/// dialog, or its payload named no hook.
fn no_hook_for_answer() -> wire::Rejected {
    crate::reply::unsupported_saying("Claude's terminal is asking this; answer it there")
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct PendingMessage {
    #[serde(with = "serde_pb::bytes")]
    id: Vec<u8>,
    /// The text Claude was handed, its sender's label included.
    text: String,
    /// The message's own words, without the label.
    words: String,
}

/// The newest slash command's item, which its output row fills in.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Slash {
    key: String,
    command: String,
    args: String,
    #[serde(with = "serde_pb::bytes")]
    input_id: Vec<u8>,
    at_ms: i64,
    output: Option<String>,
}

/// A call a PreToolUse hook announced, until its result.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Running {
    id: String,
    name: String,
    since_ms: i64,
}

/// Everything the Claude PTY interpreter holds; its checkpoint.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct State {
    shared: Shared<Ask>,
    provider: Provider,
    tools: BTreeMap<String, Tool>,
    asks: BTreeMap<String, AskMeta>,
    seq: u64,
    next_ask: u64,
    next_boundary: u64,
    tasks: Option<Vec<Task>>,
    context_tokens: Option<u64>,
    /// Claude's background jobs; the shared part publishes them.
    jobs: Jobs,
    messages: Vec<PendingMessage>,
    /// Each subagent's agent id and the tool-use id of its Agent call.
    agents: BTreeMap<String, String>,
    slash: Option<Slash>,
    /// The running turn began with a local command, not a model request.
    local_turn: bool,
    /// The row that ended the last turn as interrupted. Claude follows it
    /// with that turn's duration row, which must not end the turn begun
    /// since (a reply typed the moment the interruption landed).
    interruption: Option<String>,
    running: Vec<Running>,
    /// When calls whose PostToolUse hook came before their row began
    /// running, by tool-use id, until the row lands.
    ran_from: BTreeMap<String, i64>,
    /// The text of each prompt typed through amux and not yet reflected,
    /// by input id: a prompt Claude folds into a running turn is reflected
    /// by its text alone.
    submitted: Vec<PendingMessage>,
    /// Calls a hook opens before Claude writes their row (a plan, and a
    /// call that waits on the person), not yet drawn: they wait for the row
    /// of the prompt that began their turn.
    hook_rows: Vec<HookRow>,
    /// The calls PreToolUse hooks announced, until they end: an ask is
    /// raised for one of them.
    announced: Vec<Announced>,
    /// The replies Claude is drawing (MessageDisplay), oldest first, until
    /// the transcript rows they become land.
    streams: Vec<Stream>,
    /// Replies whose rows landed before Claude drew a line of them: the
    /// stream that follows for each is already drawn.
    unstreamed: u32,
    /// The prompts the running turn answers, and those of the last turns
    /// over. Hooks and rows travel apart, so a reply's last batch can come
    /// after the row that ended its turn, even after the next prompt; a
    /// batch for a prompt whose turn is over draws nothing.
    turn_prompts: Vec<String>,
    ended_prompts: VecDeque<String>,
    /// A new turn's prompt its `UserPromptSubmit` announced, until the
    /// prompt's row lands, and the reply's batches that came before it:
    /// drawn first, they would sit above the prompt. The hook can come
    /// after the row too: `prompt_seen` is the newest prompt row's.
    prompt_unseen: Option<String>,
    prompt_seen: Option<String>,
    /// Calls whose PostToolUse came before their row. Claude wrote the row
    /// before the call ran, so a reply drawn after the hook is a later
    /// message's: drawn before the row lands, it would sit above the call.
    calls_unseen: Vec<String>,
    /// The batches held while a prompt's or a call's row is owed.
    held_displays: Vec<HeldDisplay>,
    /// Why auto mode refused a call whose row has not landed yet, by
    /// tool-use id.
    refused: BTreeMap<String, String>,
    /// The step of a `!command` typed in Claude's own view, until its
    /// output row.
    bang: Option<String>,
    /// A terminal client shows Claude's own terminal, so a tool server's
    /// form is left to Claude's dialog.
    watched: bool,
}

/// A call a hook opens before its row: its id, tool name and input.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct HookRow {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) input: serde_json::Value,
}

/// A call its PreToolUse hook announced.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct Announced {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) input: String,
    pub(super) agent: Option<String>,
}

/// A reply's batch held until its prompt's row lands.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct HeldDisplay {
    pub(super) message_id: String,
    pub(super) delta: String,
}

/// A reply Claude is drawing, by its display id, under the key its item
/// has; the transcript row it becomes takes the key over.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct Stream {
    pub(super) message_id: String,
    pub(super) key: String,
    /// Its row already landed: what Claude draws of it is not drawn again.
    pub(super) landed: bool,
}

fn item_body(kind: claude_item::Kind) -> Vec<u8> {
    ClaudeItem { kind: Some(kind) }.encode_to_vec()
}

impl State {
    /// The kind-neutral state: the agent process reads the pending
    /// agent-message set and quiescence from here.
    pub fn shared(&self) -> &Shared<Ask> {
        &self.shared
    }

    fn new(spec: &AgentSpec) -> Self {
        let mut shared = Shared::new(spec, ClaudePty::KIND);
        // Keys typed into a terminal may land in one of Claude's own
        // screens and never reach the conversation: a turn starts when
        // Claude records the prompt.
        shared.start_turns_on_reflection();
        Self {
            shared,
            provider: Provider {
                never_ask: crate::claude_common::allows_never_ask(&spec.provider_args),
                launched_permission: crate::claude_common::launched_permission(&spec.provider_args),
                launched_model: crate::claude_common::launch_arg(&spec.provider_args, "--model"),
                offered: spec.offered.clone(),
                ..Provider::default()
            },
            tools: BTreeMap::new(),
            asks: BTreeMap::new(),
            seq: 0,
            next_ask: 0,
            next_boundary: 0,
            tasks: None,
            context_tokens: None,
            jobs: Jobs::default(),
            messages: Vec::new(),
            agents: BTreeMap::new(),
            slash: None,
            local_turn: false,
            interruption: None,
            running: Vec::new(),
            ran_from: BTreeMap::new(),
            submitted: Vec::new(),
            hook_rows: Vec::new(),
            announced: Vec::new(),
            streams: Vec::new(),
            unstreamed: 0,
            turn_prompts: Vec::new(),
            ended_prompts: VecDeque::new(),
            prompt_unseen: None,
            prompt_seen: None,
            calls_unseen: Vec::new(),
            held_displays: Vec::new(),
            refused: BTreeMap::new(),
            bang: None,
            watched: false,
        }
    }

    fn next_seq(&mut self) -> u64 {
        self.seq += 1;
        self.seq
    }

    fn body(&self) -> Vec<u8> {
        let tasks = task_list(&self.tasks);
        let context = match self.context_tokens {
            None => unknown::context_meter(),
            Some(used_tokens) => ContextMeter {
                known: true,
                used_tokens,
                window_tokens: None,
            },
        };
        ClaudeSnapshot {
            asks: self.shared.asks().open_asks().to_vec(),
            tasks: Some(tasks),
            context: Some(context),
            model: self.provider.model.clone(),
            // Terminal Claude lists no models of its own to name it from.
            model_name: self
                .provider
                .model
                .as_deref()
                .filter(|model| !model.is_empty())
                .map(crate::claude_common::tidy_model),
            effort: self.provider.effort.clone(),
            switched_away: Some(self.provider.switched_away),
            permission: self.provider.permission_mode.clone(),
            provider_session: self.provider.session.clone(),
            background_jobs: Some(self.shared.jobs()),
            servers: self
                .provider
                .tools_withheld
                .then(|| crate::claude_common::amux_tools_withheld(unknown::tool_server_health())),
            running_calls: self
                .running
                .iter()
                .map(|running| RunningCall {
                    tool_use_id: running.id.clone(),
                    tool_name: running.name.clone(),
                    since_ms: running.since_ms,
                })
                .collect(),
            ..unknown::claude()
        }
        .encode_to_vec()
    }

    // --- tools -----------------------------------------------------------

    /// Emits a tool call's item if it is drawn and changed since the last
    /// emission.
    fn emit_tool(&mut self, emit: &mut Emit, id: &str) {
        let Some(tool) = self.tools.get_mut(id) else {
            return;
        };
        if tool.hidden {
            return;
        }
        if tool.question {
            let Some(item) = question_item(&tool.input, tool.asked.clone()) else {
                return;
            };
            let body = item_body(claude_item::Kind::Ask(item));
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
            let body = item_body(claude_item::Kind::Plan(plan));
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
            let body = item_body(claude_item::Kind::AgentMessage(message));
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
        let step = if tool.state == ToolState::Succeeded as i32 {
            file_step(&tool.name, &tool.server, &tool.outcome_json)
        } else {
            FileStep::default()
        };
        let input = serde_json::from_str(&tool.input).unwrap_or(Value::Null);
        let body = item_body(claude_item::Kind::Tool(ToolCall {
            name: tool.name.clone(),
            input: Some(tool_input(&tool.name, &tool.server, &input, "")),
            state: tool.state,
            outcome_text: tool.outcome_text.clone(),
            attachments: tool.images.clone(),
            class: tool.class,
            decision: tool.decision.as_ref().map(Decision::to_wire),
            background: tool.background,
            parent_key: String::new(),
            subagent: tool.subagent.as_ref().map(|subagent| SubagentProgress {
                tool_count: subagent.tool_count,
                last_tool: subagent.last_tool.clone(),
                finished: tool.finished,
            }),
            server: tool.server.clone(),
            exit_code: None,
            ended_at_ms: tool.ended_at_ms,
            created: step.created,
            line: step.line,
            started_at_ms: tool.started_at_ms,
            patch: step.patch,
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

    // --- asks ------------------------------------------------------------

    /// Points an ask at a call's row: the wire ask gains its item key, or a
    /// decision reached before the row was drawn goes onto the call.
    fn bind(&mut self, emit: &mut Emit, ask_key: &str, tool_id: &str) {
        let Some(meta) = self.asks.get_mut(ask_key) else {
            return;
        };
        meta.bound = Some(tool_id.to_owned());
        match meta.closed.clone() {
            Some(decision) => {
                self.asks.remove(ask_key);
                self.decide(emit, tool_id, decision);
            }
            None => {
                if let Some(open) = self.shared.asks().get(ask_key) {
                    let ask = Ask {
                        item_key: tool_id.to_owned(),
                        ..open.clone()
                    };
                    self.shared.open_ask(ask);
                }
            }
        }
    }

    /// A call's row is drawn: the asks raised for it point at it.
    fn bind_asks_for(&mut self, emit: &mut Emit, tool_id: &str) {
        if !self.tools.get(tool_id).is_some_and(|tool| !tool.hidden) {
            return;
        }
        let keys = self
            .asks
            .iter()
            .filter(|(_, meta)| {
                meta.agent.is_none()
                    && meta.bound.is_none()
                    && meta.call.as_deref() == Some(tool_id)
                    && matches!(meta.shape, AskShape::Answer(ref shape) if !shape.is_dialog())
            })
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        for key in keys {
            self.bind(emit, &key, tool_id);
        }
    }

    fn decide(&mut self, emit: &mut Emit, tool_id: &str, decision: Decision) {
        if let Some(tool) = self.tools.get_mut(tool_id) {
            if tool.question {
                // Answered in Claude's own terminal, the question's result
                // says how; anything else closed it without answers.
                match (decision.record, DecisionOutcome::try_from(decision.outcome)) {
                    (Some(record), _) => tool.asked = Some(record),
                    (None, Ok(DecisionOutcome::Allowed)) => {}
                    (None, _) => tool.asked = Some(ask_item::dismissed()),
                }
            } else {
                match &mut tool.plan {
                    Some(plan) => *plan = decision.plan_verdict(None),
                    None => tool.decision = Some(decision),
                }
            }
            self.emit_tool(emit, tool_id);
        }
    }

    /// Lets go of the hook Claude waits on for an ask that is over: Claude
    /// decided it itself, or it no longer matters.
    fn release(&mut self, emit: &mut Emit, ask_key: &str) {
        if let Some(id) = self.asks.get_mut(ask_key).and_then(|meta| meta.hook.take()) {
            emit.effect(Effect::HookAnswer {
                id,
                answer: Vec::new(),
            });
        }
    }

    /// Closes an ask with the outcome the closing fact carries.
    fn close(&mut self, emit: &mut Emit, ask_key: &str, mut decision: Decision) {
        self.release(emit, ask_key);
        match self.asks.get(ask_key).map(|meta| &meta.shape) {
            Some(AskShape::Trust) => {
                return self.close_ask_item(emit, ask_key, ask_item::dismissed());
            }
            Some(AskShape::Answer(shape)) if shape.is_dialog() => {
                return self.close_ask_item(emit, ask_key, ask_item::dismissed());
            }
            // Read now: the permission Claude is in when the plan closes.
            Some(AskShape::Answer(AnswerShape::Plan { .. })) => {
                let permission = self.provider.permission_mode.as_deref();
                decision.verdict = Some(decision.plan_verdict(permission));
            }
            _ => {}
        }
        self.shared.close_ask(ask_key);
        let Some(meta) = self.asks.get_mut(ask_key) else {
            return;
        };
        match meta.bound.clone() {
            Some(tool_id) => {
                self.asks.remove(ask_key);
                self.decide(emit, &tool_id, decision);
            }
            None if meta.agent.is_some() || meta.call.is_none() => {
                self.asks.remove(ask_key);
            }
            None => meta.closed = Some(decision),
        }
    }

    fn open_ask_keys(&self) -> Vec<String> {
        self.shared
            .asks()
            .open_asks()
            .iter()
            .map(|ask| ask.key.clone())
            .collect()
    }

    /// The open plan ask, if any.
    fn plan_ask(&self) -> Option<String> {
        self.open_ask_keys().into_iter().find(|key| {
            self.asks.get(key).is_some_and(|meta| {
                matches!(meta.shape, AskShape::Answer(AnswerShape::Plan { .. }))
            })
        })
    }

    /// Claude's permission is now `permission`: leaving plan while its
    /// plan waited approves the plan, answered in Claude's own terminal.
    pub(super) fn permission_seen(&mut self, emit: &mut Emit, permission: &str) {
        let left_plan =
            self.provider.permission_mode.as_deref() == Some("plan") && permission != "plan";
        self.provider.permission_mode = Some(permission.to_owned());
        if left_plan && let Some(key) = self.plan_ask() {
            self.close(emit, &key, Decision::elsewhere(DecisionOutcome::Allowed));
        }
    }

    /// Whether Claude's permission menu has "Yes, and switch to auto mode":
    /// in manual mode, where Claude offers auto mode for the running model.
    pub(super) fn menu_offers_auto(&self) -> bool {
        let current = self
            .provider
            .permission_mode
            .as_deref()
            .or(self.provider.launched_permission.as_deref())
            .unwrap_or("default");
        current == "default" && !self.provider.never_ask && self.offers_auto()
    }

    /// Whether Claude offers auto mode for the running model, as the
    /// host's Claude says (its models' `supportsAutoMode`).
    fn offers_auto(&self) -> bool {
        // Before Claude names its model, the one it was launched with runs,
        // else its default.
        let model = self
            .provider
            .model
            .as_deref()
            .or(self.provider.launched_model.as_deref())
            .unwrap_or("default");
        self.provider.offered.as_ref().is_some_and(|offered| {
            offered.permissions.iter().any(|permission| {
                permission.value == "auto"
                    && (permission.models.is_empty()
                        || permission.models.iter().any(|named| named == model))
            })
        })
    }

    /// The permission Claude's cycle key moves to from the one it is in,
    /// as Claude's own cycle goes: default, accept edits, plan, then never
    /// ask where Claude was launched allowing it and auto mode where its
    /// model offers it, and back to default.
    fn next_permission(&self) -> String {
        let current = self
            .provider
            .permission_mode
            .clone()
            .or_else(|| self.provider.launched_permission.clone())
            .unwrap_or_else(|| "default".to_owned());
        let never_ask = self.provider.never_ask;
        let auto = self.offers_auto();
        match current.as_str() {
            "default" => "acceptEdits",
            "acceptEdits" => "plan",
            "plan" if never_ask => crate::claude_common::NEVER_ASK,
            "plan" | "bypassPermissions" if auto => "auto",
            _ => "default",
        }
        .to_owned()
    }

    /// A prompt typed instead of deciding the open plan: the plan is
    /// dismissed and the turn ends, as headless Claude's does, so the
    /// prompt is typed once it has.
    fn dismiss_plan(&mut self, emit: &mut Emit) {
        let Some(key) = self.plan_ask() else {
            return;
        };
        match self.asks.get_mut(&key).and_then(|meta| meta.hook.take()) {
            Some(id) => emit.effect(Effect::HookAnswer {
                id,
                answer: hook_output(claude_answers::Answer::Permission(
                    claude_protocol::stream::PermissionResult::Deny {
                        message: claude_answers::REJECTED.to_owned(),
                        interrupt: Some(true),
                        tool_use_id: None,
                    },
                )),
            }),
            None => emit.effect(Effect::Terminal(TerminalInput::Interrupt)),
        }
        self.close(
            emit,
            &key,
            Decision::plan(Verdict::of(wire::PlanVerdict::Dismissed)),
        );
    }

    /// A fact that proves every open ask is over without saying how.
    fn close_all_unknown(&mut self, emit: &mut Emit) {
        for key in self.open_ask_keys() {
            self.close(emit, &key, Decision::unknown());
        }
    }

    /// A fact that proves the main thread's asks are over without saying
    /// how. A subagent Claude still runs in the background keeps the dialog
    /// it raised open in Claude's own view: so does amux, until that
    /// subagent stops.
    fn close_main_threads_asks(&mut self, emit: &mut Emit) {
        for key in self.open_ask_keys() {
            let in_background = self
                .asks
                .get(&key)
                .and_then(|meta| meta.agent.as_ref())
                .is_some_and(|agent| self.jobs.running(agent));
            if !in_background {
                self.close(emit, &key, Decision::unknown());
            }
        }
    }

    /// Writes the own item of an ask that is not a call's (a tool server's
    /// form or link, the trust question), open or closed.
    fn emit_ask_item(&mut self, emit: &mut Emit, ask: &Ask, closed: Option<AskClosed>) {
        let item = match &ask.body {
            Some(wire::ask::Body::Form(form)) => wire::ask_item::Ask::Form(form.clone()),
            Some(wire::ask::Body::Link(link)) => wire::ask_item::Ask::Link(link.clone()),
            Some(wire::ask::Body::Question(question)) => {
                wire::ask_item::Ask::Question(question.clone())
            }
            _ => return,
        };
        let item = ask_item::opened(item);
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

    fn close_ask_item(&mut self, emit: &mut Emit, ask_key: &str, closed: AskClosed) {
        self.release(emit, ask_key);
        if let Some(ask) = self.shared.close_ask(ask_key) {
            let ask = self.as_asked(ask);
            self.emit_closed_ask_item(emit, ask_key, ask, closed);
        }
        self.asks.remove(ask_key);
    }

    /// An open ask as its item records it: what the tool server asked,
    /// for one left to Claude's dialog.
    fn as_asked(&self, ask: Ask) -> Ask {
        match self.asks.get(&ask.key).and_then(|meta| meta.asked.as_ref()) {
            Some(asked) => Ask {
                body: asked.body.clone(),
                ..ask
            },
            None => ask,
        }
    }

    /// Leaves a tool server's form or link to Claude's own dialog for
    /// someone at its terminal: the hook is let go, so Claude draws it,
    /// and the open ask says where it is answered.
    pub(super) fn leave_to_terminal(&mut self, emit: &mut Emit, ask_key: &str) {
        let Some(meta) = self.asks.get(ask_key) else {
            return;
        };
        if meta.hook.is_none() || meta.asked.is_some() {
            return;
        }
        let Some(open) = self.shared.asks().get(ask_key).cloned() else {
            return;
        };
        let reason = match &open.body {
            Some(wire::ask::Body::Link(link)) => format!(
                "{} is showing a link in Claude's terminal; open it there, or stop the agent.",
                link.server
            ),
            Some(wire::ask::Body::Form(form)) => format!(
                "{} is showing a form in Claude's terminal; answer it there, or stop the agent.",
                form.server
            ),
            _ => return,
        };
        self.release(emit, ask_key);
        self.shared.open_ask(Ask {
            body: Some(wire::ask::Body::Unanswerable(wire::UnanswerableAsk {
                reason,
            })),
            ..open.clone()
        });
        if let Some(meta) = self.asks.get_mut(ask_key) {
            meta.asked = Some(open);
        }
    }

    /// An ask closed before the row it waited for lands now, under its
    /// own key.
    fn emit_closed_ask_item(
        &mut self,
        emit: &mut Emit,
        ask_key: &str,
        mut ask: Ask,
        closed: AskClosed,
    ) {
        if ask.item_key.is_empty() {
            ask.item_key = ask_item::key(ask_key);
        }
        self.emit_ask_item(emit, &ask, Some(closed));
    }

    /// The open trust question, if any.
    pub(super) fn trust_ask(&self) -> Option<String> {
        self.asks
            .iter()
            .find(|(_, meta)| matches!(meta.shape, AskShape::Trust))
            .map(|(key, _)| key.clone())
    }

    /// The session started with the trust question open: the folder was
    /// trusted in Claude's own terminal.
    fn close_trust_answered_elsewhere(&mut self, emit: &mut Emit) {
        let Some(key) = self.trust_ask() else {
            return;
        };
        let closed = AskClosed {
            outcome: wire::AskOutcome::Answered as i32,
            answers: vec![wire::AnsweredQuestion {
                picked: vec![facts::TRUST_YES.to_owned()],
                ..Default::default()
            }],
            ..Default::default()
        };
        self.close_ask_item(emit, &key, closed);
    }

    /// The open forms and links of tool servers, and the call each was
    /// raised under.
    fn dialog_asks(&self) -> Vec<(String, Option<String>)> {
        self.asks
            .iter()
            .filter(|(_, meta)| matches!(&meta.shape, AskShape::Answer(shape) if shape.is_dialog()))
            .map(|(key, meta)| (key.clone(), meta.call.clone()))
            .collect()
    }

    /// Draws the dialogs that waited for this call's row, below it.
    pub(super) fn emit_dialog_items_for(&mut self, emit: &mut Emit, tool_id: &str) {
        for (key, call) in self.dialog_asks() {
            if call.as_deref() != Some(tool_id) {
                continue;
            }
            let Some(open) = self.shared.asks().get(&key) else {
                continue;
            };
            if !open.item_key.is_empty() {
                continue;
            }
            let ask = Ask {
                item_key: ask_item::key(&key),
                ..open.clone()
            };
            self.shared.open_ask(ask.clone());
            let ask = self.as_asked(ask);
            self.emit_ask_item(emit, &ask, None);
        }
    }

    /// A call's result ends the form raised while it ran, or one raised
    /// while no call ran.
    fn close_dialogs_for_tool(&mut self, emit: &mut Emit, tool_id: &str) {
        for (key, call) in self.dialog_asks() {
            if call.as_deref().is_none_or(|call| call == tool_id) {
                self.close_ask_item(emit, &key, ask_item::dismissed());
            }
        }
    }

    /// The call's own result closes the asks raised for it.
    fn close_for_tool(&mut self, emit: &mut Emit, tool_id: &str, outcome: DecisionOutcome) {
        for key in self.open_ask_keys() {
            let meta = self.asks.get(&key);
            if meta.and_then(|meta| meta.call.as_deref()) == Some(tool_id)
                && meta.is_some_and(
                    |meta| matches!(&meta.shape, AskShape::Answer(shape) if !shape.is_dialog()),
                )
            {
                self.close(emit, &key, Decision::elsewhere(outcome));
            }
        }
    }

    /// The call an ask for this tool and input was raised for: the newest
    /// its PreToolUse hook announced, in the same conversation, with equal
    /// input, that no other ask was raised for. Claude runs that hook
    /// before it asks.
    fn announced_call(&self, name: &str, input: &Value, agent: Option<&str>) -> Option<String> {
        let asked = self
            .asks
            .values()
            .filter_map(|meta| meta.call.as_deref())
            .collect::<Vec<_>>();
        self.announced
            .iter()
            .rev()
            .find(|call| {
                call.name == name
                    && call.agent.as_deref() == agent
                    && !asked.contains(&call.id.as_str())
                    && same_json(&call.input, input)
            })
            .map(|call| call.id.clone())
    }

    // --- items -----------------------------------------------------------

    /// Cancels the running turn with the interrupt key, which also
    /// dismisses an open ask's dialog.
    fn interrupt(&mut self, emit: &mut Emit) {
        if self.shared.is_busy() || !self.shared.asks().is_empty() {
            emit.effect(Effect::Terminal(TerminalInput::Interrupt));
        }
        // The interrupt key cancels a tool server's form whether or not
        // the turn goes on, and nothing else reports that. A form whose
        // hook still waits is cancelled there too, so Claude never draws it.
        for (key, _) in self.dialog_asks() {
            if let Some(id) = self.asks.get_mut(&key).and_then(|meta| meta.hook.take()) {
                let cancel = claude_protocol::stream::ElicitationResult::Cancel {
                    extensions: Default::default(),
                };
                emit.effect(Effect::HookAnswer {
                    id,
                    answer: hook_output(claude_answers::Answer::Elicitation(cancel)),
                });
            }
            self.close_ask_item(emit, &key, ask_item::outcome(wire::AskOutcome::Cancelled));
        }
    }

    fn boundary(&mut self, emit: &mut Emit, kind: BoundaryKind, exit: wire::IncarnationEnd) {
        self.next_boundary += 1;
        let key = format!("boundary:{}", self.next_boundary);
        self.shared.item(
            emit,
            ItemDraft {
                key,
                body: item_body(claude_item::Kind::Boundary(Boundary {
                    kind: kind as i32,
                    cause: exit.cause,
                    cause_detail: exit.detail,
                })),
                complete: true,
                ..Default::default()
            },
        );
    }

    fn emit_slash(&mut self, emit: &mut Emit) {
        let Some(slash) = &self.slash else {
            return;
        };
        let draft = ItemDraft {
            key: slash.key.clone(),
            text: slash.output.clone().unwrap_or_default(),
            input_id: slash.input_id.clone(),
            body: item_body(claude_item::Kind::Slash(wire::SlashOutput {
                command: slash.command.clone(),
                args: slash.args.clone(),
            })),
            at_ms: Some(slash.at_ms),
            complete: true,
            ..Default::default()
        };
        self.shared.item(emit, draft);
    }

    // --- inputs ----------------------------------------------------------

    /// Types a prompt submitted from the queue or at once, remembering its
    /// text until its reflection.
    fn typed(&mut self, emit: &mut Emit, entry: wire::QueuedInput) {
        // Another agent's words go in labelled, and Claude's transcript
        // reflects them so; the prompt's row keeps the words alone.
        let text = crate::shared::provider_text(&entry);
        let words = if text == entry.text {
            String::new()
        } else {
            entry.text
        };
        self.submitted.push(PendingMessage {
            id: entry.input_id,
            text: text.clone(),
            words,
        });
        emit.effect(Effect::Terminal(TerminalInput::Prompt {
            text,
            attachments: entry.attachments,
        }));
    }

    /// An agent message goes to Claude's messaging socket and is accepted
    /// at once; its item is the record that it arrived.
    fn agent_message(&mut self, emit: &mut Emit, envelope: Envelope) {
        if self.shared.message_repeated(emit, &envelope.id) {
            return;
        }
        let words = crate::shared::words(&envelope);
        let attachments = crate::shared::attachments(&envelope).to_vec();
        self.shared.message_accepted(&envelope.id);
        // Claude shows the model the text as carried, label and all, and
        // that is what its transcript reflects.
        let carried = crate::shared::carried(&envelope);
        self.messages.push(PendingMessage {
            id: envelope.id.clone(),
            text: carried.clone(),
            words: words.clone(),
        });
        self.shared.message_handed();
        emit.effect(Effect::Inject {
            id: envelope.id.clone(),
            text: carried,
            attachments: attachments.clone(),
            via: Carrier::MessagingSocket,
        });
        self.shared.item(
            emit,
            ItemDraft {
                key: agent_message_key(&envelope.id),
                text: words,
                attachments,
                input_id: envelope.id.clone(),
                body: item_body(claude_item::Kind::AgentMessage(agent_message_body(
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
            Some(KindInput::ClaudePty(wire::ClaudePtyInput { of: Some(arm) })) => arm,
            _ => return self.shared.reject(emit, &id, Refusal::Unsupported),
        };
        match arm {
            // Claude's own view shows a conversation that is not this
            // agent's: nothing is typed into it.
            claude_pty_input::Of::Prompt(_) if self.provider.switched_away => {
                self.shared
                    .reject(emit, &id, Refusal::ConversationElsewhere)
            }
            claude_pty_input::Of::Prompt(prompt) => {
                self.dismiss_plan(emit);
                if let Some(entry) = self.shared.admit_prompt(emit, &id, prompt, human()) {
                    self.typed(emit, entry);
                }
            }
            claude_pty_input::Of::Withdraw(withdraw) => {
                self.shared.withdraw(emit, &id, &withdraw.queued_input_id)
            }
            // Back to this agent's conversation: Claude's own resume of it.
            claude_pty_input::Of::SwitchBack(_) => {
                let ours = self.provider.session.clone();
                match ours.filter(|_| self.provider.switched_away) {
                    Some(session) => {
                        emit.effect(Effect::Terminal(TerminalInput::Prompt {
                            text: format!("/resume {session}"),
                            attachments: Vec::new(),
                        }));
                        self.shared.accept(emit, &id, false);
                    }
                    None => self.shared.reject(emit, &id, Refusal::Unsupported),
                }
            }
            // Typed while the turn runs, a prompt joins it at the next tool
            // boundary, as Codex's steer and headless Claude's mid-turn
            // message do. Claude's send-now chord is not used: it moves a
            // running command to the background or cancels the reply being
            // written. Nothing is typed over an open menu.
            claude_pty_input::Of::SendNow(send) => {
                let running = self.shared.is_busy() && self.shared.asks().is_empty();
                if let Some(entry) = self
                    .shared
                    .send_now(emit, &id, &send.queued_input_id, running)
                {
                    emit.effect(Effect::Terminal(TerminalInput::SendNow {
                        text: crate::shared::provider_text(&entry),
                        attachments: entry.attachments,
                    }));
                }
            }
            claude_pty_input::Of::Interrupt(_) => {
                self.interrupt(emit);
                self.shared.accept(emit, &id, false);
            }
            claude_pty_input::Of::Clear(_) => {
                emit.effect(Effect::Terminal(TerminalInput::Clear));
                self.shared.accept(emit, &id, false);
            }
            // amux knows Claude's cycle, so the permission it moves to is
            // recorded at once; Claude's next record of it confirms it.
            claude_pty_input::Of::CyclePermissionMode(_) => {
                emit.effect(Effect::Terminal(TerminalInput::CyclePermissionMode));
                let next = self.next_permission();
                self.permission_seen(emit, &next);
                self.shared.accept(emit, &id, false);
            }
            claude_pty_input::Of::Answer(answer) => self.answer(emit, &id, answer),
        }
    }

    /// An answer through amux: checked against the open ask's shape, then
    /// handed to Claude as the output of the hook it waits on, the decision
    /// headless Claude would be handed. The ask closes with the outcome the
    /// answer carries.
    fn answer(&mut self, emit: &mut Emit, id: &[u8], answer: ClaudeAnswer) {
        let key = answer.ask_key;
        if self.shared.asks().get(&key).is_none() {
            self.shared.answer(emit, id, &key);
            return;
        }
        let parsed = answer.of;
        let Some(meta) = self.asks.get(&key).cloned() else {
            return self.shared.reject(emit, id, Refusal::Unsupported);
        };
        let shape = match meta.shape {
            AskShape::Trust => return self.answer_trust(emit, id, &key, parsed),
            AskShape::Answer(shape) => shape,
        };
        let body = self
            .shared
            .asks()
            .get(&key)
            .and_then(|ask| ask.body.clone());
        let asked = match &body {
            Some(wire::ask::Body::Question(asked)) => Some(asked.clone()),
            _ => None,
        };
        // A tool server's form or link is the ask's own item, closed with
        // what was sent.
        let dialog = match (&parsed, &body) {
            (Some(claude_answer::Of::Form(form)), Some(wire::ask::Body::Form(asked))) => {
                Some(ask_item::form_sent(form, &asked.schema_json))
            }
            (Some(claude_answer::Of::Link(link)), _) => Some(ask_item::link_answered(link.action)),
            _ => None,
        };
        let Some((decision, decided)) = parsed.and_then(|parsed| {
            claude_answers::decide(
                &shape,
                &meta.input,
                &meta.suggestions,
                asked.as_ref(),
                parsed,
            )
        }) else {
            return self.shared.reject(emit, id, Refusal::Unsupported);
        };
        let Some(hook) = meta.hook else {
            return self.shared.refuse(emit, id, no_hook_for_answer());
        };
        // amux knows where an allowance moves Claude's permission, as it
        // knows Claude's cycle: recorded at once, confirmed by Claude's
        // next record.
        let switched_to = match &decision {
            claude_answers::Answer::Permission(
                claude_protocol::stream::PermissionResult::Allow {
                    updated_permissions: Some(updates),
                    ..
                },
            ) => updates.iter().find_map(|update| match update {
                claude_protocol::stream::PermissionUpdate::SetMode { mode, .. } => {
                    Some(mode.as_str().to_owned())
                }
                _ => None,
            }),
            _ => None,
        };
        if let Some(meta) = self.asks.get_mut(&key) {
            meta.hook = None;
        }
        emit.effect(Effect::HookAnswer {
            id: hook,
            answer: hook_output(decision),
        });
        let answered = self.shared.answer(emit, id, &key);
        match (dialog, decided) {
            (Some(closed), _) => {
                self.asks.remove(&key);
                if let Some(ask) = answered {
                    self.emit_closed_ask_item(emit, &key, ask, closed);
                }
            }
            (None, Some(decided)) => self.close(emit, &key, Decision::from(decided)),
            (None, None) => self.close(emit, &key, Decision::unknown()),
        }
        if let Some(permission) = switched_to {
            self.permission_seen(emit, &permission);
        }
        self.shared.accept(emit, id, false);
    }
}

impl State {
    /// The trust question answered through amux: its first answer trusts
    /// the folder, its second exits.
    fn answer_trust(
        &mut self,
        emit: &mut Emit,
        id: &[u8],
        key: &str,
        parsed: Option<claude_answer::Of>,
    ) {
        let trust = match &parsed {
            Some(claude_answer::Of::Question(answer))
                if answer
                    .answers
                    .iter()
                    .all(|response| response.note.is_none())
                    && answer.answers.len() == 1
                    && answer.answers[0].other.is_none() =>
            {
                match answer.answers[0].selected.as_slice() {
                    [0] => Some(true),
                    [1] => Some(false),
                    _ => None,
                }
            }
            _ => None,
        };
        let (Some(trust), Some(claude_answer::Of::Question(answer))) = (trust, parsed) else {
            return self.shared.reject(emit, id, Refusal::Unsupported);
        };
        let Some(ask) = self.shared.answer(emit, id, key) else {
            return;
        };
        self.asks.remove(key);
        let closed = match &ask.body {
            Some(wire::ask::Body::Question(asked)) => ask_item::answered(asked, &answer),
            _ => None,
        };
        self.emit_ask_item(emit, &ask, closed);
        emit.effect(Effect::Terminal(TerminalInput::Trust { trust }));
        self.shared.accept(emit, id, false);
    }
}

/// The output of the hook Claude waits on that hands it `answer`.
fn hook_output(answer: claude_answers::Answer) -> Vec<u8> {
    use claude_protocol::hooks::{HookDecision, HookOutput};
    let hook_specific_output = match answer {
        claude_answers::Answer::Permission(decision) => {
            HookDecision::PermissionRequest { decision }
        }
        claude_answers::Answer::Elicitation(result) => HookDecision::Elicitation(result),
    };
    serde_json::to_vec(&HookOutput {
        hook_specific_output,
    })
    .expect("a hook's output serializes")
}

impl Checkpoint for State {
    fn resume(mut self) -> (Self, Step) {
        let body = self.body();
        let step = self.shared.resume_step(body);
        (self, step)
    }
}

impl Interpreter for ClaudePty {
    type State = State;
    const KIND: wire::Kind = wire::Kind::ClaudePty;

    fn initial(spec: &AgentSpec) -> (State, Step) {
        let mut state = State::new(spec);
        let step = state.shared.initial_step(Self::unknown_snapshot());
        (state, step)
    }

    fn reincarnate(mut state: State, spec: &AgentSpec) -> (State, Step) {
        state.shared.reincarnate(spec);
        state.jobs.clear();
        // The launch fact of the new process is its first, not a relaunch:
        // the session it resumes is a resume, not a restart.
        state.provider.launches = 0;
        state.provider.relaunched = false;
        // The new process's transcript is followed anew, from wherever the
        // agent process last read it.
        state.provider.transcript = None;
        // The agent says again at each launch whether policy withholds
        // amux's tools.
        state.provider.tools_withheld = false;
        state.provider.offered = spec.offered.clone();
        // The new process counts its own terminal clients, each of which
        // says so again: one attached when the last process ended never
        // said it left.
        state.watched = false;
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
            state.typed(&mut emit, entry);
        }
        let awaiting = state.shared.awaiting_reflection();
        state
            .submitted
            .retain(|prompt| awaiting.contains(&prompt.id));
        let body = state.body();
        state.shared.finish(emit, body)
    }

    fn redact(target: RedactTarget) -> RedactTarget {
        crate::redact::redact_kind::<
            wire::ClaudeItem,
            wire::ClaudeSnapshot,
            wire::ClaudePtyInput,
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
        describe_snapshot(body)
    }

    fn fixture_input(input_id: Vec<u8>, input: &FixtureInput) -> Option<Input> {
        claude_pty_input(input_id, input)
    }

    fn recording(format: &str, bytes: &[u8]) -> Result<Vec<Event>, String> {
        recording::read(format, bytes)
    }
}

fn describe_snapshot(body: &[u8]) -> SnapshotView {
    let snapshot = ClaudeSnapshot::decode(body).unwrap_or_default();
    let context = snapshot.context.unwrap_or_default();
    SnapshotView {
        asks: snapshot
            .asks
            .iter()
            .map(|ask| (ask.key.clone(), ask.item_key.clone()))
            .collect(),
        text: format!(
            "asks=[{}] session={} model={} model_name={} mode={} context={} tasks={} background={}",
            describe_asks(&snapshot.asks),
            snapshot.provider_session.as_deref().unwrap_or("?"),
            snapshot.model.as_deref().unwrap_or("?"),
            crate::claude_common::describe_model_name(snapshot.model_name.as_deref()),
            snapshot.permission.as_deref().unwrap_or("?"),
            if context.known {
                context.used_tokens.to_string()
            } else {
                "?".into()
            },
            describe_tasks(&snapshot.tasks.unwrap_or_default()),
            crate::shared::describe_jobs(snapshot.background_jobs.as_ref())
        ) + &snapshot
            .running_calls
            .iter()
            .map(|call| {
                format!(
                    " running={} {} since={}",
                    call.tool_use_id, call.tool_name, call.since_ms
                )
            })
            .collect::<String>(),
    }
}
