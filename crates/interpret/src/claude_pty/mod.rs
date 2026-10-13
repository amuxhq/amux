//! Claude in a terminal. The facts are rows of Claude's transcript file,
//! hook payloads on the agent's hooks socket, the agent process's own launch
//! fact and the PTY's exit. Rows arrive whole, so nothing here streams.
//!
//! The transcript is the source of rows and hooks fill only its gaps. Every
//! row, tool calls included, is emitted from its transcript row in
//! transcript order: Claude writes a call's tool_use row as the call starts
//! and its tool_result row as it ends, so a running call is live and the
//! text that introduced a call sits above it. A prompt's row precedes the
//! rows of the turn it began, so nothing waits for it. Hooks carry what the
//! transcript never does: SessionStart names the transcript and the
//! session, PermissionRequest opens an ask, Stop closes what the turn left
//! open, SubagentStop ends a background subagent, and a Notification
//! announces a dialog only the terminal can answer. PreToolUse and
//! PostToolUse only mark a call running, for the activity line while its
//! row is on the way; they never make a row.
//!
//! Terminal Claude has no protocol for asks, so this is the one interpreter
//! that infers them, and every inference rule lives in this module:
//!
//! - An ask opens on the PermissionRequest hook, which names no tool-use id,
//!   as a card carrying the hook's tool and input. Claude writes a gated
//!   call's row only once the call is decided (AskUserQuestion's once it is
//!   answered), so the card points at no row until the row with that tool
//!   and input lands, and then at that row.
//! - An ask closes on an answer sent through amux (outcome known), on the
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
//! - A tool server's form or link that Claude shows in its own terminal is
//!   announced only by a Notification hook, which no hook can answer. It
//!   opens an unanswerable ask with an item of its own, the one item a hook
//!   makes besides SessionStart's session boundary, pointing at the call that was running (a tool server's, when
//!   one was). When that call's row has not landed yet, the item waits for
//!   it, so it sits below the call and the rows before it. An interrupt through amux closes it cancelled; that call's
//!   result, a row from a later assistant message, and the facts that close
//!   every ask close it dismissed.
//! - Claude's first screen may ask whether its folder is trusted; the
//!   agent process reports it before Claude counts as ready. It opens a
//!   question with its own item, answered through amux or closed by the
//!   session starting (trusted in the terminal) or Claude exiting.
//! - A plan ask's switch is the permission the first entry of Claude's own
//!   plan menu moves to, which follows from how Claude was launched and
//!   what its model offers: never ask where Claude was launched allowing
//!   it, else auto mode where the host's Claude offers it for the running
//!   model, else accept edits. A switch answered for any other permission
//!   is refused: the menu's first entry would not do it.
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
//! what Claude's newest assistant row ran (or what `/model`'s output says
//! it set, until a row names it), and the effort is the one that row ran
//! with. The permission mode changes only by cycling.

mod facts;
mod recording;

use std::collections::BTreeMap;
use std::fmt;

use prost::Message as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use wire::{
    AgentSpec, Ask, AskClosed, Attachment, Boundary, BoundaryKind, ClaudeAnswer, ClaudeGrant,
    ClaudePtyItem, ClaudePtySnapshot, ContextMeter, DecisionOutcome, Envelope, Input, KindInput,
    Refusal, RunningCall, Step, SubagentProgress, ToolCall, ToolDecision, ToolState, claude_answer,
    claude_pty_input, claude_pty_item, permission_answer, tool_decision,
};

use crate::claude_common::{
    Jobs, Task, Verdict, describe_asks, describe_tasks, describe_tool, file_step_facts, or_dash,
    plan_item, question_item, same_json, split_tool_name, task_list,
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
/// The agent process turns it into bytes with the keymap it resolved for
/// the Claude version it launched; the shapes here are what that keymap's
/// programs need to know about the ask being answered.
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
    /// Answer the permission menu. `suggestions` is how many scope choices
    /// the menu offers.
    Permission {
        suggestions: u32,
        choice: PermissionChoice,
    },
    /// Answer the plan menu.
    Plan(PlanChoice),
    /// Fill in the question form, one answer per question in order.
    Question {
        questions: Vec<QuestionShape>,
        answers: Vec<QuestionChoice>,
    },
    /// Answer the folder-trust dialog: trust the folder, or exit.
    Trust { trust: bool },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PermissionChoice {
    AllowOnce,
    /// The scope choice at this index of the ask's offer.
    AllowScoped {
        suggestion: u32,
    },
    /// Terminal Claude always ends the turn on a deny.
    Deny {
        note: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlanChoice {
    /// The menu's first entry: approve, switching to the permission it
    /// names.
    ApproveSwitching,
    Approve,
    SendBack {
        note: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionShape {
    pub options: u32,
    pub multi_select: bool,
    /// Its options carry previews: Claude draws them side by side, with no
    /// row for a typed answer.
    pub previews: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuestionChoice {
    pub selected: Vec<u32>,
    pub other: Option<String>,
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
            Self::Permission {
                suggestions,
                choice,
            } => write!(f, "permission of {suggestions} {choice:?}"),
            Self::Plan(choice) => write!(f, "plan {choice:?}"),
            Self::Question { questions, answers } => {
                write!(f, "question")?;
                for (shape, answer) in questions.iter().zip(answers) {
                    if answer.selected.is_empty() && answer.other.is_none() {
                        write!(f, " [skipped of {}]", shape.options)?;
                        continue;
                    }
                    write!(
                        f,
                        " [{} of {}{}{}{}]",
                        answer
                            .selected
                            .iter()
                            .map(u32::to_string)
                            .collect::<Vec<_>>()
                            .join(","),
                        shape.options,
                        if shape.multi_select { " multi" } else { "" },
                        if shape.previews { " previews" } else { "" },
                        answer
                            .other
                            .as_ref()
                            .map(|other| format!(" other={}", Value::String(other.clone())))
                            .unwrap_or_default()
                    )?;
                }
                Ok(())
            }
            Self::Trust { trust } => write!(f, "trust {trust}"),
        }
    }
}

/// The permission menus the agent's keymap can type, by how many
/// suggestions the PermissionRequest hook carries. An allowance on any
/// other menu is refused before it closes the ask; a deny is Escape, which
/// every menu takes.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionMenus {
    /// Menus with one scoped entry per suggestion.
    pub per_suggestion: Vec<u32>,
    /// Menus that fold every suggestion into one scoped entry.
    pub folded: Vec<u32>,
}

/// What the agent process tells the interpreter about Claude's terminal on
/// the agent channel. These are amux's own facts, not Claude's.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum AgentFact {
    /// Sent before the provider's first fact, and again after the agent
    /// process relaunches it. `send_now_refused` is why the keymap cannot
    /// type send now for this Claude, when it cannot. `asking`: the agent
    /// is asking Claude in its own folder what it offers, so the catalogue
    /// waits for [`AgentFact::Offered`].
    Launch {
        #[serde(default)]
        version: String,
        #[serde(default)]
        keymap: String,
        #[serde(default)]
        permission_menus: PermissionMenus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        send_now_refused: Option<String>,
        #[serde(default)]
        asking: bool,
    },
    /// Claude's terminal first drew with bracketed paste on: its input is
    /// live, so a prompt can be typed. Some versions report their session
    /// start only after the first prompt, so nothing else says a new
    /// terminal takes input.
    Ready,
    /// Claude's first screen is its folder-trust dialog; sent before
    /// [`AgentFact::Ready`].
    TrustDialog,
    /// What Claude offers asked from the agent's own folder, which names
    /// the project's own commands the host's copy cannot; none when Claude
    /// could not say, and the host's copy stands.
    Offered {
        #[serde(with = "serde_pb::opt_msg")]
        catalogue: Option<wire::Catalogue>,
    },
}

impl AgentFact {
    fn encode(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("an agent fact serializes")
    }
}

/// The launch fact, as the agent process sends it.
pub fn launch_fact(
    version: &str,
    keymap: &str,
    permission_menus: &PermissionMenus,
    send_now_refused: Option<&str>,
    asking: bool,
) -> Vec<u8> {
    AgentFact::Launch {
        version: version.to_owned(),
        keymap: keymap.to_owned(),
        permission_menus: permission_menus.clone(),
        send_now_refused: send_now_refused.map(str::to_owned),
        asking,
    }
    .encode()
}

/// The fact that Claude's terminal takes input.
pub fn ready_fact() -> Vec<u8> {
    AgentFact::Ready.encode()
}

/// The fact that Claude's first screen is its folder-trust dialog.
pub fn trust_dialog_fact() -> Vec<u8> {
    AgentFact::TrustDialog.encode()
}

/// The fact that Claude, asked in the agent's folder, offers `catalogue`,
/// or could not say.
pub fn offered_fact(catalogue: Option<wire::Catalogue>) -> Vec<u8> {
    AgentFact::Offered { catalogue }.encode()
}

/// What the interpreter knows about the Claude process and its session.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct Provider {
    session: Option<String>,
    transcript: Option<String>,
    version: Option<String>,
    keymap: String,
    permission_menus: PermissionMenus,
    /// Why this Claude's keymap cannot type send now, when it cannot.
    send_now_refused: Option<String>,
    launches: u32,
    /// Launched again after an earlier launch; the next session start is a
    /// restart whatever its source says.
    relaunched: bool,
    model: Option<String>,
    /// The model as Claude named it when it was set in Claude's own view
    /// ("Set model to `Haiku 5.5`"), until a row names the model's id.
    #[serde(default)]
    model_name: Option<String>,
    /// The reasoning effort Claude's newest assistant row ran with.
    #[serde(default)]
    effort: Option<String>,
    permission_mode: Option<String>,
    /// Launched allowing the never-ask permission.
    never_ask: bool,
    /// The permission Claude was launched in, from its arguments.
    #[serde(default)]
    launched_permission: Option<String>,
    /// The other conversation Claude's own view resumed, while it shows
    /// one that is not this agent's.
    #[serde(default)]
    switched_away_to: Option<String>,
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

impl Decision {
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

/// What the interpreter keeps about an ask beside the wire Ask: how to
/// find its tool call and how the terminal's menu is shaped.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct AskMeta {
    seq: u64,
    tool_name: String,
    input: String,
    shape: AskShape,
    /// The tool-use id of the call it points at.
    bound: Option<String>,
    /// Closed before its call's row landed: the decision waits for it.
    closed: Option<Decision>,
    /// Raised inside the subagent with this agent id: never bound to a
    /// call of the main conversation.
    agent: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
enum AskShape {
    Permission {
        /// What each scope choice grants, in offer order.
        #[serde(with = "serde_pb::msgs")]
        grants: Vec<ClaudeGrant>,
        /// How many suggestions the hook carried: what the keymap reads
        /// the terminal's menu by.
        suggestions: u32,
    },
    /// The permission the ask's switch choice moves to: what the first
    /// entry of Claude's plan menu switches to, known from how Claude was
    /// launched and what its model offers.
    Plan {
        switch: String,
    },
    Question {
        questions: Vec<QuestionShape>,
    },
    /// A dialog only Claude's own terminal can answer, raised while this
    /// call ran.
    Unanswerable {
        call: Option<String>,
    },
    /// Claude's folder-trust dialog.
    Trust,
}

/// A scope the terminal's menu has no entry for, refused in words the card
/// shows.
fn no_key_for_answer() -> wire::Rejected {
    crate::reply::unsupported_saying(
        "Claude's terminal menu has no entry amux can type for this answer",
    )
}

/// An answer that does not fit the ask.
fn unsupported() -> wire::Rejected {
    wire::Rejected {
        refusal: Refusal::Unsupported as i32,
        detail: None,
    }
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
    /// The text of each prompt typed through amux and not yet reflected,
    /// by input id: a prompt Claude folds into a running turn is reflected
    /// by its text alone.
    submitted: Vec<PendingMessage>,
    /// Plans announced by their hook and not yet drawn: they wait for the
    /// row of the prompt that began their turn.
    announced_plans: Vec<AnnouncedPlan>,
    /// The step of a `!command` typed in Claude's own view, until its
    /// output row.
    #[serde(default)]
    bang: Option<String>,
}

/// A plan its PreToolUse hook announced: the call's id, tool name and the
/// input that holds the plan.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct AnnouncedPlan {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) input: serde_json::Value,
}

fn item_body(kind: claude_pty_item::Kind) -> Vec<u8> {
    ClaudePtyItem { kind: Some(kind) }.encode_to_vec()
}

impl State {
    /// The kind-neutral state: the agent process reads the pending
    /// agent-message set and quiescence from here.
    pub fn shared(&self) -> &Shared<Ask> {
        &self.shared
    }

    fn new(spec: &AgentSpec, producer_version: &str) -> Self {
        let mut shared = Shared::new(spec, ClaudePty::KIND, producer_version);
        // Keys typed into a terminal may land in one of Claude's own
        // screens and never reach the conversation: a turn starts when
        // Claude records the prompt.
        shared.start_turns_on_reflection();
        Self {
            shared,
            provider: Provider {
                never_ask: crate::claude_common::allows_never_ask(&spec.provider_args),
                launched_permission: crate::claude_common::launched_permission(&spec.provider_args),
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
            submitted: Vec::new(),
            announced_plans: Vec::new(),
            bang: None,
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
                breakdown: Vec::new(),
            },
        };
        ClaudePtySnapshot {
            asks: self.shared.asks().open_asks().to_vec(),
            tasks: Some(tasks),
            context: Some(context),
            model: self.provider.model.clone(),
            // Terminal Claude lists no models of its own to name it from.
            model_name: self.provider.model_name.clone().or_else(|| {
                self.provider
                    .model
                    .as_deref()
                    .filter(|model| !model.is_empty())
                    .map(crate::claude_common::tidy_model)
            }),
            effort: self.provider.effort.clone(),
            switched_away_to: self.provider.switched_away_to.clone(),
            permission: self.provider.permission_mode.clone(),
            provider_session: self.provider.session.clone(),
            background_jobs: Some(self.shared.jobs()),
            running_calls: self
                .running
                .iter()
                .map(|running| RunningCall {
                    tool_use_id: running.id.clone(),
                    tool_name: running.name.clone(),
                    since_ms: running.since_ms,
                })
                .collect(),
            ..unknown::claude_pty()
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
            let Some(item) = question_item(&tool.input, tool.asked.clone(), true) else {
                return;
            };
            let body = item_body(claude_pty_item::Kind::Ask(item));
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
            let body = item_body(claude_pty_item::Kind::Plan(plan));
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
            let body = item_body(claude_pty_item::Kind::AgentMessage(message));
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
        let (created, line) = if tool.state == ToolState::Succeeded as i32 {
            file_step_facts(&tool.name, &tool.server, &tool.outcome_json)
        } else {
            (None, None)
        };
        let body = item_body(claude_pty_item::Kind::Tool(ToolCall {
            name: tool.name.clone(),
            input_json: tool.input.clone().into_bytes(),
            state: tool.state,
            outcome_text: tool.outcome_text.clone(),
            outcome_json: tool.outcome_json.clone().into_bytes(),
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
            created,
            line,
            started_at_ms: None,
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

    /// Points an ask at a call: the wire ask gains its item key, or a
    /// decision reached before the call's row landed goes onto the call.
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

    /// Closes an ask with the outcome the closing fact carries.
    fn close(&mut self, emit: &mut Emit, ask_key: &str, mut decision: Decision) {
        match self.asks.get(ask_key).map(|meta| &meta.shape) {
            Some(AskShape::Unanswerable { .. } | AskShape::Trust) => {
                return self.close_ask_item(emit, ask_key, ask_item::dismissed());
            }
            // Read now: the permission Claude is in when the plan closes.
            Some(AskShape::Plan { .. }) => {
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
            None if meta.agent.is_some() => {
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
            self.asks
                .get(key)
                .is_some_and(|meta| matches!(meta.shape, AskShape::Plan { .. }))
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

    /// Whether Claude offers auto mode for the running model, as the
    /// host's Claude says (its models' `supportsAutoMode`).
    fn offers_auto(&self) -> bool {
        let model = self.provider.model.as_deref().unwrap_or_default();
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
    /// dismissed and the interrupt key cancels Claude's plan menu, so the
    /// prompt is typed once the turn ends.
    fn dismiss_plan(&mut self, emit: &mut Emit) {
        let Some(key) = self.plan_ask() else {
            return;
        };
        self.close(
            emit,
            &key,
            Decision::plan(Verdict::of(wire::PlanVerdict::Dismissed)),
        );
        emit.effect(Effect::Terminal(TerminalInput::Interrupt));
    }

    /// A fact that proves every open ask is over without saying how.
    fn close_all_unknown(&mut self, emit: &mut Emit) {
        for key in self.open_ask_keys() {
            self.close(emit, &key, Decision::unknown());
        }
    }

    /// Writes the own item of an ask that is not a call's (an unanswerable
    /// dialog, the trust question), open or closed.
    fn emit_ask_item(&mut self, emit: &mut Emit, ask: &Ask, closed: Option<AskClosed>) {
        let item = match &ask.body {
            Some(wire::ask::Body::Unanswerable(unanswerable)) => {
                wire::ask_item::Ask::Unanswerable(unanswerable.clone())
            }
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
                body: item_body(claude_pty_item::Kind::Ask(item)),
                at_ms: Some(ask.opened_at_ms),
                complete: true,
                ..Default::default()
            },
        );
    }

    fn close_ask_item(&mut self, emit: &mut Emit, ask_key: &str, closed: AskClosed) {
        self.asks.remove(ask_key);
        if let Some(mut ask) = self.shared.close_ask(ask_key) {
            // Closed before the row it waited for: it lands now.
            if ask.item_key.is_empty() {
                ask.item_key = ask_item::key(ask_key);
            }
            self.emit_ask_item(emit, &ask, Some(closed));
        }
    }

    /// A call's row landed: the unanswerable asks raised under it before
    /// then get their items, below it.
    fn emit_unanswerable_items_for(&mut self, emit: &mut Emit, tool_id: &str) {
        for (key, call) in self.unanswerable_asks() {
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
            self.emit_ask_item(emit, &ask, None);
        }
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

    /// The open unanswerable asks and the call each was raised under.
    fn unanswerable_asks(&self) -> Vec<(String, Option<String>)> {
        self.asks
            .iter()
            .filter_map(|(key, meta)| match &meta.shape {
                AskShape::Unanswerable { call } => Some((key.clone(), call.clone())),
                _ => None,
            })
            .collect()
    }

    /// A call's result ends the dialog raised while it ran, or one raised
    /// while no call ran.
    fn close_unanswerable_for_tool(&mut self, emit: &mut Emit, tool_id: &str) {
        for (key, call) in self.unanswerable_asks() {
            if call.as_deref().is_none_or(|call| call == tool_id) {
                self.close_ask_item(emit, &key, ask_item::dismissed());
            }
        }
    }

    /// The call's own result closes the asks that point at it.
    fn close_for_tool(&mut self, emit: &mut Emit, tool_id: &str, outcome: DecisionOutcome) {
        for key in self.open_ask_keys() {
            if self.asks.get(&key).and_then(|meta| meta.bound.as_deref()) == Some(tool_id) {
                self.close(emit, &key, Decision::elsewhere(outcome));
            }
        }
    }

    /// The running, undecided call whose row already landed that an ask
    /// for this tool and input points at: the newest with equal input.
    /// Claude writes a gated call's row at its decision, so there is
    /// usually none, and a call of the same tool with other input is
    /// another call.
    fn tool_for_ask(&self, name: &str, input: &Value) -> Option<String> {
        let bound = self
            .asks
            .values()
            .filter_map(|meta| meta.bound.as_deref())
            .collect::<Vec<_>>();
        self.tools
            .iter()
            .filter(|(id, tool)| {
                tool.name_matches(name)
                    && !tool.hidden
                    && !tool.finished
                    && tool.decision.is_none()
                    && tool.plan.as_ref().is_none_or(Verdict::undecided)
                    && !bound.contains(&id.as_str())
                    && same_json(&tool.input, input)
            })
            .max_by_key(|(_, tool)| tool.seq)
            .map(|(id, _)| id.clone())
    }

    /// The unbound ask a newly seen call answers to: the oldest with equal
    /// input, else the oldest for the tool.
    fn ask_for_tool(&self, name: &str, input: &Value) -> Option<String> {
        let candidates = self
            .asks
            .iter()
            .filter(|(_, meta)| {
                meta.bound.is_none() && meta.agent.is_none() && meta.tool_name == name
            })
            .collect::<Vec<_>>();
        let oldest = |exact: bool| {
            candidates
                .iter()
                .filter(|(_, meta)| !exact || same_json(&meta.input, input))
                .min_by_key(|(_, meta)| meta.seq)
                .map(|(key, _)| (*key).clone())
        };
        oldest(true).or_else(|| oldest(false))
    }

    // --- items -----------------------------------------------------------

    /// Cancels the running turn with the interrupt key, which also
    /// dismisses an open ask's dialog.
    fn interrupt(&mut self, emit: &mut Emit) {
        if self.shared.is_busy() || !self.shared.asks().is_empty() {
            emit.effect(Effect::Terminal(TerminalInput::Interrupt));
        }
        // The interrupt key cancels a dialog Claude shows in its terminal
        // whether or not the turn goes on, and nothing else reports that.
        for (key, _) in self.unanswerable_asks() {
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
                body: item_body(claude_pty_item::Kind::Boundary(Boundary {
                    kind: kind as i32,
                    provider_session: self.provider.session.clone().unwrap_or_default(),
                    provider_version: self.provider.version.clone().unwrap_or_default(),
                    cause: exit.cause,
                    cause_detail: exit.detail,
                    keymap: self.provider.keymap.clone(),
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
            body: item_body(claude_pty_item::Kind::Slash(wire::SlashOutput {
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
        let envelope = crate::shared::worded(envelope);
        self.shared.message_accepted(&envelope.id);
        // Claude shows the model the text as carried, label and all, and
        // that is what its transcript reflects.
        let carried = crate::shared::carried(&envelope);
        self.messages.push(PendingMessage {
            id: envelope.id.clone(),
            text: carried.clone(),
            words: envelope.text.clone(),
        });
        emit.effect(Effect::Inject {
            envelope: Box::new(Envelope {
                text: carried,
                ..envelope.clone()
            }),
            via: Carrier::MessagingSocket,
        });
        self.shared.item(
            emit,
            ItemDraft {
                key: agent_message_key(&envelope.id),
                text: envelope.text.clone(),
                input_id: envelope.id.clone(),
                body: item_body(claude_pty_item::Kind::AgentMessage(agent_message_body(
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
            claude_pty_input::Of::Prompt(_) if self.provider.switched_away_to.is_some() => self
                .shared
                .reject(emit, &id, Refusal::ConversationElsewhere),
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
                match ours.filter(|_| self.provider.switched_away_to.is_some()) {
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
            // message do; the keymap's send-now row says from which Claude
            // that holds. Claude's send-now chord is not used: it moves a
            // running command to the background or cancels the reply being
            // written. Nothing is typed over an open menu.
            claude_pty_input::Of::SendNow(send) => {
                if let Some(reason) = self.provider.send_now_refused.clone() {
                    return self
                        .shared
                        .refuse(emit, &id, crate::reply::unsupported_saying(reason));
                }
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
    /// typed into the terminal's menu. The ask closes with the outcome the
    /// answer carries.
    fn answer(&mut self, emit: &mut Emit, id: &[u8], answer: ClaudeAnswer) {
        let key = answer.ask_key;
        if self.shared.asks().get(&key).is_none() {
            self.shared.answer(emit, id, &key);
            return;
        }
        let parsed = answer.of;
        if let Some(AskShape::Trust) = self.asks.get(&key).map(|meta| &meta.shape) {
            return self.answer_trust(emit, id, &key, parsed);
        }
        let asked = match self
            .shared
            .asks()
            .get(&key)
            .and_then(|ask| ask.body.as_ref())
        {
            Some(wire::ask::Body::Question(asked)) => Some(asked.clone()),
            _ => None,
        };
        if let (Some(asked), Some(claude_answer::Of::Reply(reply))) = (&asked, &parsed) {
            return self.reply_instead(emit, id, &key, asked, reply);
        }
        let answered = match (self.asks.get(&key), parsed) {
            (Some(meta), Some(parsed)) => terminal_answer(&meta.shape, asked.as_ref(), parsed),
            _ => Err(unsupported()),
        };
        let (terminal, decision) = match answered {
            Ok(answered) => answered,
            Err(why) => return self.shared.refuse(emit, id, why),
        };
        self.shared.answer(emit, id, &key);
        self.close(emit, &key, decision);
        emit.effect(Effect::Terminal(terminal));
        self.shared.accept(emit, id, false);
    }
}

impl State {
    /// The person replied in their own words instead of answering: Claude's
    /// question menu is cancelled with the interrupt key, which ends the
    /// turn, and the words are typed as the next prompt.
    fn reply_instead(
        &mut self,
        emit: &mut Emit,
        id: &[u8],
        key: &str,
        asked: &wire::QuestionAsk,
        reply: &wire::ReplyInstead,
    ) {
        let fits =
            reply.answers_so_far.is_empty() || reply.answers_so_far.len() == asked.questions.len();
        let Some(record) =
            ask_item::replied(asked, reply).filter(|_| fits && !reply.text.trim().is_empty())
        else {
            return self.shared.reject(emit, id, Refusal::Unsupported);
        };
        self.shared.answer(emit, id, key);
        self.close(emit, key, Decision::question(record));
        emit.effect(Effect::Terminal(TerminalInput::Interrupt));
        let prompt = wire::PromptInput {
            text: reply.text.clone(),
            ..Default::default()
        };
        if let Some(entry) = self.shared.admit_prompt(emit, id, prompt, human()) {
            self.typed(emit, entry);
        }
    }

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

impl Tool {
    fn name_matches(&self, name: &str) -> bool {
        let (server, tool) = split_tool_name(name);
        self.name == tool && self.server == server
    }
}

/// The keystrokes an answer stands for and the decision it records, or
/// why it is refused: it does not fit the ask, or it names a scope the
/// terminal's menu has no entry for.
fn terminal_answer(
    shape: &AskShape,
    asked: Option<&wire::QuestionAsk>,
    answer: claude_answer::Of,
) -> Result<(TerminalInput, Decision), wire::Rejected> {
    let decision = |outcome: DecisionOutcome, grant: Option<ClaudeGrant>, note: String| Decision {
        outcome: outcome as i32,
        grant,
        note,
        elsewhere: false,
        verdict: None,
        record: None,
    };
    match (shape, answer) {
        (
            AskShape::Permission {
                grants,
                suggestions,
            },
            claude_answer::Of::Permission(permission),
        ) => {
            let suggestions = *suggestions;
            match permission.of.ok_or_else(unsupported)? {
                permission_answer::Of::Allow(allow) => match allow.scope {
                    None => Ok((
                        TerminalInput::Permission {
                            suggestions,
                            choice: PermissionChoice::AllowOnce,
                        },
                        decision(DecisionOutcome::Allowed, None, String::new()),
                    )),
                    Some(index) => Ok((
                        TerminalInput::Permission {
                            suggestions,
                            choice: PermissionChoice::AllowScoped { suggestion: index },
                        },
                        decision(
                            DecisionOutcome::Allowed,
                            Some(
                                grants
                                    .get(index as usize)
                                    .ok_or_else(no_key_for_answer)?
                                    .clone(),
                            ),
                            String::new(),
                        ),
                    )),
                },
                permission_answer::Of::Deny(deny) => Ok((
                    TerminalInput::Permission {
                        suggestions,
                        choice: PermissionChoice::Deny {
                            note: deny.note.clone(),
                        },
                    },
                    decision(DecisionOutcome::Denied, None, deny.note),
                )),
            }
        }
        (AskShape::Plan { switch }, claude_answer::Of::Plan(plan)) => {
            let (choice, verdict) = match wire::PlanChoice::try_from(plan.choice) {
                Ok(wire::PlanChoice::Start) => (
                    PlanChoice::Approve,
                    Verdict::of(wire::PlanVerdict::Approved),
                ),
                // The menu's first entry switches to the permission the ask
                // names; an answer agreeing to another is not what that key
                // would do.
                Ok(wire::PlanChoice::StartSwitching)
                    if !switch.is_empty() && plan.permission == *switch =>
                {
                    (PlanChoice::ApproveSwitching, Verdict::switching(switch))
                }
                Ok(wire::PlanChoice::KeepPlanning) => {
                    let note = plan.note.unwrap_or_default();
                    let verdict = Verdict::sent_back(&note);
                    (PlanChoice::SendBack { note }, verdict)
                }
                _ => return Err(unsupported()),
            };
            Ok((TerminalInput::Plan(choice), Decision::plan(verdict)))
        }
        (AskShape::Question { questions }, claude_answer::Of::Question(answer)) => {
            // Claude's form has nowhere to type a note. A question is
            // skipped by moving past it to Claude's review screen, which
            // only a form of several questions has: a lone question is
            // submitted by answering it.
            if answer.answers.len() != questions.len()
                || answer
                    .answers
                    .iter()
                    .any(|response| response.note.is_some())
            {
                return Err(unsupported());
            }
            let record = asked
                .and_then(|asked| ask_item::answered(asked, &answer))
                .ok_or_else(unsupported)?;
            let mut answers = Vec::new();
            for (shape, response) in questions.iter().zip(answer.answers) {
                let picks = response.selected.len() + usize::from(response.other.is_some());
                let fits = response.selected.iter().all(|index| *index < shape.options)
                    && (picks > 0 || questions.len() > 1)
                    && (shape.multi_select || picks <= 1)
                    && !(shape.previews && response.other.is_some());
                if !fits {
                    return Err(unsupported());
                }
                answers.push(QuestionChoice {
                    selected: response.selected,
                    other: response.other,
                });
            }
            Ok((
                TerminalInput::Question {
                    questions: questions.clone(),
                    answers,
                },
                Decision::question(record),
            ))
        }
        _ => Err(unsupported()),
    }
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

    fn initial(spec: &AgentSpec, producer_version: &str) -> (State, Step) {
        let mut state = State::new(spec, producer_version);
        let step = state.shared.initial_step(Self::unknown_snapshot());
        (state, step)
    }

    fn reincarnate(mut state: State, spec: &AgentSpec, producer_version: &str) -> (State, Step) {
        state.shared.reincarnate(spec, producer_version);
        state.jobs.clear();
        // The launch fact of the new process is its first, not a relaunch:
        // the session it resumes is a resume, not a restart.
        state.provider.launches = 0;
        state.provider.relaunched = false;
        // The new process's transcript is followed anew, from wherever the
        // agent process last read it.
        state.provider.transcript = None;
        state.provider.offered = spec.offered.clone();
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
            Event::DaemonLost => state.boundary(
                &mut emit,
                wire::BoundaryKind::DaemonLost,
                wire::IncarnationEnd::default(),
            ),
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
            wire::ClaudePtyItem,
            wire::ClaudePtySnapshot,
            wire::ClaudePtyInput,
            State,
        >(target)
    }

    fn pending_messages(state: &Self::State) -> Vec<Vec<u8>> {
        state.shared.pending_messages().iter().cloned().collect()
    }

    fn unknown_snapshot() -> Vec<u8> {
        unknown::claude_pty().encode_to_vec()
    }

    fn describe_item(body: &[u8]) -> ItemView {
        describe_item(body)
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

fn describe_item(body: &[u8]) -> ItemView {
    use claude_pty_item::Kind;
    let item = ClaudePtyItem::decode(body).unwrap_or_default();
    let (arm, complete, text) = match item.kind {
        None => ("none", true, String::new()),
        Some(Kind::Prompt(_)) => ("prompt", true, String::new()),
        Some(Kind::Steer(_)) => ("steer", true, String::new()),
        Some(Kind::Message(text)) => ("message", text.complete, String::new()),
        Some(Kind::Thinking(thinking)) => ("thinking", thinking.complete, String::new()),
        Some(Kind::Tool(tool)) => ("tool", true, describe_tool(&tool)),
        Some(Kind::Turn(turn)) => (
            "turn",
            true,
            format!(
                "#{} {} started={}",
                turn.turn_id,
                wire::TurnOutcome::try_from(turn.outcome)
                    .map_or("?", |outcome| outcome.as_str_name()),
                turn.started_at_ms
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
        Some(Kind::CompactSummary(_)) => ("compact_summary", true, String::new()),
        Some(Kind::Interruption(_)) => ("interruption", true, String::new()),
        Some(Kind::AgentMessage(message)) => (
            "agent_message",
            true,
            crate::shared::describe_agent_message(&message),
        ),
        Some(Kind::ApiError(error)) => (
            "api_error",
            true,
            format!(
                "{} {} retry={} {}/{}",
                error.error_kind,
                Value::String(error.message),
                error.will_retry,
                error.attempt,
                error.max_attempts
            ),
        ),
        Some(Kind::Boundary(boundary)) => (
            "boundary",
            true,
            format!(
                "{} session={} version={} keymap={}{}",
                BoundaryKind::try_from(boundary.kind).map_or("?", |kind| kind.as_str_name()),
                or_dash(&boundary.provider_session),
                or_dash(&boundary.provider_version),
                or_dash(&boundary.keymap),
                crate::claude_common::describe_cause(&boundary)
            ),
        ),
        Some(Kind::Slash(slash)) => (
            "slash",
            true,
            format!("{} args={}", slash.command, Value::String(slash.args)),
        ),
        Some(Kind::Ask(item)) => ("ask", true, ask_item::describe(&item)),
        Some(Kind::Plan(plan)) => ("plan", plan.complete, crate::shared::describe_plan(&plan)),
        Some(Kind::Unrecognized(unrecognized)) => (
            "unrecognized",
            true,
            format!(
                "{} {}",
                unrecognized.fact_type,
                Value::String(unrecognized.summary)
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
    let snapshot = ClaudePtySnapshot::decode(body).unwrap_or_default();
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
