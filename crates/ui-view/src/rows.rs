//! Chat rows: one row per item, the row id the item key. A run of tool
//! steps is an attribute on its members (see [`crate::run`]).

use std::collections::HashSet;

#[cfg(feature = "schema")]
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ui_state::{Fold, Held, ItemBody, Key, PhaseView, SessionState};
use wire::{
    Attachment, BlobRef, BoundaryKind, DecisionOutcome, EnvelopeKind, FileChangeKind, SendState,
    ToolCall, ToolState, TurnOutcome, tool_input,
};

use crate::ask::{QuestionView, Scope, lift_rule, lifted, question, scope_of};
use crate::code::LineKind;
use crate::run::Run;
use crate::segments::{Segment, segments};
use crate::subagents::{Groups, SubagentGroup};
use crate::text::{terminal_text, within, without_direction_controls};
use crate::turn::{TurnPart, TurnPlace, Turns};

/// How many lines of a command's output a row carries.
pub const OUTPUT_HEAD_LINES: usize = 3;
/// The output's last lines a command row carries.
pub const OUTPUT_TAIL_LINES: usize = 12;

#[derive(Clone, Debug, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
pub struct Row {
    pub id: Key,
    pub order: u64,
    pub at_ms: i64,
    pub kind: RowKind,
    pub run: Option<Run>,
    /// The client skips a collapsed row: inside a folded run, or a tool row
    /// when tool rows are hidden, or a row that carries nothing to draw.
    pub collapsed: bool,
    /// A permission decision, meta on the tool call's own row.
    pub decision: Option<Decision>,
    /// Drawn in the attention ink: an open ask points at it, or it failed.
    pub attention: bool,
    /// A subagent's own step from history: collapsed under the subagent's
    /// row, which opens to it. Steps written since a subagent became an
    /// agent of its own are in its own chat, and carry none.
    pub parent: Option<Key>,
    /// The agent this call started or continues, by its id: a provider
    /// subagent or a child amux's spawn made. Every client opens that
    /// agent's chat from the row, a gesture apart from expanding it, and
    /// reads how the agent stands from its fleet row
    /// ([`crate::opened_agent`]), never from the call.
    pub opens: Option<Vec<u8>>,
    /// The calls that opened agents in this row's turn, when they opened
    /// more than one: they fold into one row (see [`SubagentGroup`]).
    pub group: Option<SubagentGroup>,
    /// Where it stands in its finished turn, when the client folds turns
    /// ([`ToolRows::Turns`]); None in the turn under way and for every
    /// other fold.
    pub turn: Option<TurnPlace>,
}

/// Whether tool steps show, hide, or fold into their runs.
#[derive(Clone, Copy, Debug)]
pub enum ToolRows<'a> {
    ShowAll,
    /// Every step hidden but a failure its turn left unresolved.
    Hide,
    /// Each run folds to its newest step, where its one line goes, keeping
    /// a failure its turn left unresolved; under way it shows its newest
    /// step and any still running. A run `open` holds a step of shows every
    /// step (see [`crate::run_is_open`]).
    Collapse {
        open: &'a HashSet<Key>,
    },
    /// Each finished turn folds to its prompt, the line over its work, and
    /// its answer, keeping in view what the reader must see; one `turns`
    /// holds the end of shows its work, each run folded as `Collapse`
    /// folds it. The turn under way folds as `Collapse` does.
    Turns {
        runs: &'a HashSet<Key>,
        turns: &'a HashSet<Key>,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
pub enum RowKind {
    Prompt {
        text: Vec<Segment>,
        steered: bool,
    },
    Prose {
        text: Vec<Segment>,
        streaming: bool,
        working_note: bool,
    },
    /// "Thought for 8s"; the text when the provider records it.
    Thinking {
        text: String,
        open: bool,
        duration_ms: Option<i64>,
    },
    ToolCall {
        server: String,
        tool: String,
        fact: String,
        state: CallPhase,
        result: String,
    },
    FileChange {
        files: Vec<FileRow>,
        state: CallPhase,
    },
    /// amux's own call that starts or stops a subagent: "Started
    /// payment-tests on build · Codex". `host` is empty for the caller's own
    /// host, and `kind` empty when the call does not say it.
    Child {
        action: ChildAction,
        name: String,
        host: String,
        kind: String,
        state: CallPhase,
        result: String,
    },
    Command {
        command: String,
        /// What the agent said the command is for, as it wrote it; empty
        /// when it said nothing, as Codex never does.
        description: String,
        state: CallPhase,
        exit_code: Option<i32>,
        output_head: Vec<String>,
        more_lines: usize,
        /// The output's start was dropped to keep a long command's output
        /// bounded, so its first lines here are not the command's first.
        output_trimmed: bool,
        /// The output's last lines, for a step opened to show how it
        /// ended; the lines before them number `more_lines` plus the head,
        /// less these.
        output_tail: Vec<String>,
        duration_ms: Option<i64>,
    },
    Explore {
        verb: ExploreVerb,
        subject: String,
        state: CallPhase,
        /// Why it failed, or what a search, listing or fetch found, in the
        /// provider's words; empty when it said nothing. A read's result is
        /// the file itself, which the row does not carry.
        result: String,
    },
    /// A call that started or continued a subagent its provider runs:
    /// Claude's Agent call or a SendMessage to its subagent, Codex's spawn
    /// or follow-up of a child thread. The row opens the subagent's chat
    /// ([`Row::opens`]); how the subagent stands is its fleet row's, since a
    /// call can return while its subagent runs on. Only a call from history
    /// opens nothing, and then `state` is all there is to say.
    Subagent {
        /// What the call asked for: a new subagent's description, or the
        /// message a continuation sent; empty when the provider gave none
        /// (Codex), when the subagent's own title names it.
        description: String,
        /// It continued a subagent started earlier.
        continued: bool,
        state: CallPhase,
        /// What the subagent handed back, once the call returned it.
        answer: String,
        duration_ms: Option<i64>,
    },
    Background {
        command: String,
        /// What the agent said the command is for; empty when it said
        /// nothing.
        description: String,
        running: bool,
        /// How long it ran, once it ended.
        duration_ms: Option<i64>,
    },
    Image {
        image: Option<BlobRef>,
        path: String,
        generated: bool,
    },
    SlashOutput {
        command: String,
        args: String,
        output: String,
    },
    /// An ask that is the work: resolves in place.
    Ask(AskRow),
    TurnEnd {
        duration_ms: Option<i64>,
        failed: bool,
    },
    Stopped,
    Compaction {
        tokens_before: Option<u64>,
        tokens_after: Option<u64>,
        automatic: bool,
        /// amux compacted the conversation while the agent sat idle.
        idle: bool,
    },
    Error {
        error_kind: String,
        message: String,
        attempts: u32,
        gave_up: bool,
    },
    ModelSwitch {
        to: String,
        reason: String,
    },
    Boundary {
        kind: BoundaryKind,
        /// For an exit: its cause in words, then the cause's own.
        cause: String,
    },
    AgentMessage {
        from: String,
        kind: EnvelopeKind,
        /// Its words, with its attachments where their placeholders stand.
        text: Vec<Segment>,
        /// A message this agent sent: the recipient as the model named it,
        /// and how the send went; empty and unspecified on one it received.
        to: String,
        sent: SendState,
        rejection: String,
    },
    /// Codex's automatic reviewer on an action. A client words the status,
    /// and says `status_word` for one this build does not know.
    AutoReview {
        status: wire::ReviewStatus,
        status_word: String,
        risk: String,
        rationale: String,
        subject: Key,
    },
    Unrecognized {
        what: String,
        summary: String,
    },
    /// An item that belongs to the overview or the activity line, never a row.
    Hidden,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
pub enum AskRow {
    /// A plan the agent proposed, and what the person decided: its title,
    /// lifted from an opening heading that names it, and the rest as its
    /// body. While its first line is still being written there is no title
    /// yet and nothing of that line in the body.
    Plan {
        title: Option<String>,
        body: String,
        verdict: PlanVerdict,
        /// Approved switching: the permission switched to, and its name as
        /// the agent's catalogue gives it. Empty otherwise.
        mode: String,
        mode_name: String,
        /// Why it was sent back, when the person said.
        note: Option<String>,
        /// Still being written: it grows as it streams.
        writing: bool,
    },
    /// Questions asked as the work, then the answers sent: "answered 2 of
    /// 3", or the person's own words when they replied instead.
    Questions {
        questions: Vec<QuestionView>,
        /// One per question once answered, in the ask's order; with a
        /// reply, the answers given before it.
        answers: Vec<AnswerView>,
        /// How many of the answers are skips.
        skipped: u32,
        /// What the person wrote instead of answering.
        reply: Option<String>,
        resolution: Resolution,
    },
    /// A tool server's form: "Sent 3 fields to github".
    Form {
        server: String,
        message: String,
        /// The names of the fields sent.
        fields: Vec<String>,
        resolution: Resolution,
    },
    /// A tool server's link to open.
    Link {
        server: String,
        message: String,
        url: String,
        resolution: Resolution,
    },
    /// Files and network beyond the sandbox: "Granted write target/ this
    /// turn".
    Grant {
        reason: String,
        read: Vec<String>,
        write: Vec<String>,
        network: bool,
        hosts: Vec<String>,
        granted: Option<Granted>,
        resolution: Resolution,
    },
    /// A dialog the provider showed that this build could not read: it was
    /// answered in the provider's own terminal or ended by a stop.
    Unanswerable {
        reason: String,
        resolution: Resolution,
    },
}

/// How an ask that is the work closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
pub enum Resolution {
    Open,
    /// Answered, sent, opened or granted.
    Answered,
    /// Declined, or a grant that granted nothing.
    Declined,
    /// The person replied in their own words instead of answering.
    Replied,
    Cancelled,
    /// Closed by a fact that did not say how.
    Dismissed,
}

/// One question's answer: the picked options, a typed answer, or a secret
/// answer that reads "answered (hidden)"; skipped when it is none of them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
pub struct AnswerView {
    /// "(Recommended)" lifted off, as on the card.
    pub picked: Vec<String>,
    pub other: Option<String>,
    pub hidden: bool,
    /// The person's note on this question.
    pub note: Option<String>,
    pub skipped: bool,
}

/// What an access grant granted, and for how long.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
pub struct Granted {
    pub read: Vec<String>,
    pub write: Vec<String>,
    pub network: bool,
    pub for_session: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
pub enum PlanVerdict {
    Open,
    Approved,
    /// Approved, switching to another permission from then on.
    ApprovedSwitching,
    SentBack,
    Dismissed,
}

/// Where a call stands for the person: one answer both clients word and
/// branch on. Asking: an open ask points at it and nothing has decided it
/// yet, so it runs only if the person allows it. Pending: announced, not
/// started, and asking nobody. Denied: refused, by the person or by the
/// agent's own rules, whatever the call reports after. The rest are the
/// call's own state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
pub enum CallPhase {
    Asking,
    Pending,
    Running,
    Succeeded,
    Failed,
    Denied,
    Cancelled,
}

impl CallPhase {
    /// Not yet done: asking, pending or running.
    pub fn in_flight(self) -> bool {
        matches!(
            self,
            CallPhase::Asking | CallPhase::Pending | CallPhase::Running
        )
    }
}

impl Row {
    /// The call's phase, for a row that is a call with one.
    pub fn call_phase(&self) -> Option<CallPhase> {
        match &self.kind {
            RowKind::ToolCall { state, .. }
            | RowKind::Child { state, .. }
            | RowKind::Subagent { state, .. }
            | RowKind::FileChange { state, .. }
            | RowKind::Command { state, .. }
            | RowKind::Explore { state, .. } => Some(*state),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
pub enum ChildAction {
    Start,
    Stop,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
pub enum ExploreVerb {
    Read,
    Search,
    List,
    Fetch,
    WebSearch,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
pub struct FileRow {
    pub path: String,
    pub change: FileChangeView,
    pub added: u32,
    pub removed: u32,
    /// The line the edit's first change landed on, once it landed.
    pub line: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
pub enum FileChangeView {
    Edited,
    Created {
        lines: u32,
    },
    Deleted,
    Moved {
        to: String,
    },
    /// A whole-file write not yet done: whether it makes the file or
    /// replaces one is known only once it is.
    Writing {
        lines: u32,
    },
}

/// A permission decision: allowed or denied, with what it granted and a
/// note when the provider says them, and where it was answered.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
pub struct Decision {
    pub outcome: DecisionView,
    /// What an allowance granted beyond this one call.
    pub granted: Option<PermissionGrant>,
    pub note: Option<String>,
    /// Answered in the provider's own interface.
    pub elsewhere: bool,
}

/// What a permission allowed from then on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
pub enum PermissionGrant {
    /// Claude's standing permission: what it allows ("cargo test", as on
    /// the card), folders added, the permission it switched to, and the
    /// grant's destination: where Claude keeps it.
    Claude {
        subjects: Vec<String>,
        directories: Vec<String>,
        /// The permission switched to, by its value; empty when none.
        mode: String,
        /// That permission by the catalogue's name; empty when the
        /// catalogue does not name it.
        mode_name: String,
        destination: Scope,
    },
    /// Codex: approvals like this one, for the rest of the session.
    Session,
    /// Codex: calls like this one, saved in the person's Codex config for
    /// every project.
    Always,
    /// Codex: commands starting with these words.
    CommandPrefix { words: Vec<String> },
    /// Codex: network access to these hosts.
    NetworkHosts { hosts: Vec<String> },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
pub enum DecisionView {
    Allowed,
    Denied,
    AutoApproved,
    /// Closed by a fact that did not say how.
    Dismissed,
}

/// Rows for these keys, in order, skipping keys not held.
pub fn chat_rows_for(state: &SessionState, keys: &[Key], tools: ToolRows) -> Vec<Row> {
    let transcript = state.transcript();
    let mut held: Vec<&Held> = keys.iter().filter_map(|key| transcript.get(key)).collect();
    held.sort_by_key(|held| held.item.order);
    held.dedup_by_key(|held| held.item.order);
    let mut turns = Turns::default();
    let mut groups = Groups::default();
    held.into_iter()
        .map(|held| row(state, held, tools, &mut turns, &mut groups))
        .collect()
}

fn row(
    state: &SessionState,
    held: &Held,
    tools: ToolRows,
    turns: &mut Turns,
    groups: &mut Groups,
) -> Row {
    let item = &held.item;
    let (kind, decision, failed) = kind_of(state, held);
    let mut run = crate::run::run_of(state, held, &kind, failed);
    if let (ToolRows::Collapse { open } | ToolRows::Turns { runs: open, .. }, Some(run)) =
        (tools, run.as_mut())
    {
        run.open = crate::run::run_is_open(state, item.order, open);
    }
    let step = held.class.fold() == Fold::Step;
    let kept = run.as_ref().is_some_and(|run| run.unresolved_failure);
    let run_folded = || {
        run.as_ref().is_some_and(|run| {
            let shown = if run.live {
                run.live_shown
            } else {
                run.is_last() || kept
            };
            !shown && !run.open
        })
    };
    let turn = match tools {
        ToolRows::Turns { turns: open, .. } => turns.place(state, held, &kind, kept, open),
        _ => None,
    };
    let collapsed = matches!(kind, RowKind::Hidden)
        || match tools {
            ToolRows::ShowAll => false,
            ToolRows::Hide => step && !kept,
            ToolRows::Collapse { .. } => run_folded(),
            ToolRows::Turns { .. } => match &turn {
                Some(TurnPlace {
                    open: false,
                    part: TurnPart::Work,
                    ..
                }) => true,
                // Closed, a turn keeps its failure in view without the
                // run that held it.
                Some(TurnPlace {
                    open: false,
                    part: TurnPart::Kept,
                    ..
                }) => false,
                _ => run_folded(),
            },
        };
    let asked = asked(state, held);
    let parent = parent_of(held);
    let opens = ui_state::opens(&held.body).map(<[u8]>::to_vec);
    let group = opens.as_ref().and_then(|_| groups.of(state, held, tools));
    // A folded group draws as its first call; the rest wait for it to open.
    let grouped_away = group
        .as_ref()
        .is_some_and(|group| !group.open && group.first != item.key);
    let collapsed = collapsed || parent.is_some() || grouped_away;
    Row {
        id: item.key.clone(),
        order: item.order,
        at_ms: item.at_ms,
        kind,
        run,
        collapsed,
        decision,
        attention: asked || failed,
        parent,
        opens,
        group,
        turn,
    }
}

/// Whether an open ask points at the item.
fn asked(state: &SessionState, held: &Held) -> bool {
    state
        .open_asks()
        .iter()
        .any(|ask| ask.item_key() == held.item.key)
}

/// The subagent call a step belongs to, when it is a subagent's own step.
pub(crate) fn parent_of(held: &Held) -> Option<Key> {
    match &held.body {
        ItemBody::Claude(wire::claude_item::Kind::Tool(tool)) if !tool.parent_key.is_empty() => {
            Some(tool.parent_key.clone())
        }
        _ => None,
    }
}

/// What a step acted on: a redo of a failed step acts on the same.
pub(crate) fn subject_of(held: &Held) -> String {
    match &held.body {
        ItemBody::Claude(wire::claude_item::Kind::Tool(tool)) => read_input(tool).0.to_owned(),
        ItemBody::Codex(wire::codex_item::Kind::Work(work)) => match &work.of {
            Some(wire::work::Of::Command(command)) => command.command.clone(),
            Some(wire::work::Of::WebSearch(search)) => web_step(search).1,
            _ => String::new(),
        },
        _ => String::new(),
    }
}

/// The call's phase from its state, its decision, and whether an open ask
/// points at it. A refusal decides it whatever state the call reports, and
/// a call its agent exited in the middle of is cancelled: nothing runs it
/// on, whatever its last report said.
fn call_phase(
    session: &SessionState,
    held: &Held,
    state: i32,
    decision: Option<&Decision>,
) -> CallPhase {
    let phase = reported_phase(state, decision, asked(session, held));
    if exited(session) && phase.in_flight() {
        CallPhase::Cancelled
    } else {
        phase
    }
}

fn exited(state: &SessionState) -> bool {
    matches!(state.phase(), PhaseView::Exited { .. })
}

fn reported_phase(state: i32, decision: Option<&Decision>, asked: bool) -> CallPhase {
    let denied = decision.is_some_and(|decision| decision.outcome == DecisionView::Denied);
    match ToolState::try_from(state).unwrap_or(ToolState::Unspecified) {
        _ if denied => CallPhase::Denied,
        ToolState::Unspecified | ToolState::Pending | ToolState::Running
            if asked && decision.is_none() =>
        {
            CallPhase::Asking
        }
        ToolState::Unspecified | ToolState::Pending => CallPhase::Pending,
        ToolState::Running => CallPhase::Running,
        ToolState::Succeeded => CallPhase::Succeeded,
        ToolState::Failed => CallPhase::Failed,
        ToolState::Denied => CallPhase::Denied,
        ToolState::Cancelled => CallPhase::Cancelled,
    }
}

fn succeeded(state: i32) -> bool {
    ToolState::try_from(state) == Ok(ToolState::Succeeded)
}

fn decision_view(
    decision: Option<&wire::ToolDecision>,
    offered: &[wire::OfferedPermission],
) -> Option<Decision> {
    let decision = decision?;
    let outcome = match DecisionOutcome::try_from(decision.outcome).ok()? {
        DecisionOutcome::Unspecified => return None,
        DecisionOutcome::Allowed => DecisionView::Allowed,
        DecisionOutcome::Denied => DecisionView::Denied,
        DecisionOutcome::AutoApproved => DecisionView::AutoApproved,
        DecisionOutcome::Unknown => DecisionView::Dismissed,
    };
    let text = |s: &str| (!s.is_empty()).then(|| s.to_owned());
    let granted = match &decision.granted {
        Some(wire::tool_decision::Granted::Claude(grant)) => {
            let mode = grant.mode.clone().unwrap_or_default();
            Some(PermissionGrant::Claude {
                subjects: grant.rules.iter().map(|rule| lift_rule(rule)).collect(),
                directories: grant.directories.clone(),
                mode_name: offered
                    .iter()
                    .find(|permission| !mode.is_empty() && permission.value == mode)
                    .map(|permission| permission.display_name.clone())
                    .unwrap_or_default(),
                mode,
                destination: scope_of(grant),
            })
        }
        Some(wire::tool_decision::Granted::Codex(grant)) => match &grant.of {
            Some(wire::codex_grant::Of::Session(_)) => Some(PermissionGrant::Session),
            Some(wire::codex_grant::Of::Always(_)) => Some(PermissionGrant::Always),
            Some(wire::codex_grant::Of::CommandPrefix(prefix)) => {
                Some(PermissionGrant::CommandPrefix {
                    words: crate::ask::shown_prefix(&prefix.words),
                })
            }
            Some(wire::codex_grant::Of::NetworkHosts(hosts)) => {
                Some(PermissionGrant::NetworkHosts {
                    hosts: hosts.hosts.clone(),
                })
            }
            None => None,
        },
        None => None,
    };
    Some(Decision {
        outcome,
        granted,
        note: text(&decision.note),
        elsewhere: decision.elsewhere,
    })
}

/// How long a call ran: from when it started running, else from its
/// item's time, to when it ended.
fn duration(started: i64, ended: Option<i64>) -> Option<i64> {
    ended.map(|ended| (ended - started).max(0))
}

/// When a Claude call started running: after any wait for the person's
/// approval, where the provider says, else when its item was written.
fn claude_started(held: &Held, tool: &ToolCall) -> i64 {
    tool.started_at_ms.unwrap_or(held.item.at_ms)
}

/// The row kind, the decision meta, and whether it failed.
pub(crate) fn kind_of(state: &SessionState, held: &Held) -> (RowKind, Option<Decision>, bool) {
    use wire::claude_item::Kind as Claude;
    use wire::codex_item::Kind as Codex;
    let item = &held.item;
    let text = || chat_segments(held);
    let plain = |kind| (kind, None, false);
    match &held.body {
        ItemBody::Claude(kind) => match kind {
            Claude::Prompt(_) if wire::is_continued_input(&item.input_id) => plain(continued()),
            Claude::Prompt(_) => plain(RowKind::Prompt {
                text: text(),
                steered: false,
            }),
            Claude::Steer(_) => plain(RowKind::Prompt {
                text: text(),
                steered: true,
            }),
            Claude::Message(m) => plain(prose(held, m.complete, false)),
            Claude::Thinking(t) => plain(thinking(state, held, t.complete)),
            Claude::Tool(tool) => claude_tool(state, held, tool),
            // History: a task headless Claude reported without its call.
            Claude::Task(task) => plain(RowKind::Subagent {
                description: task.description.clone(),
                continued: false,
                state: match task.state() {
                    wire::TaskState::Running if !exited(state) => CallPhase::Running,
                    wire::TaskState::Running => CallPhase::Cancelled,
                    wire::TaskState::Failed => CallPhase::Failed,
                    wire::TaskState::Stopped => CallPhase::Cancelled,
                    _ => CallPhase::Succeeded,
                },
                answer: String::new(),
                duration_ms: None,
            }),
            Claude::Turn(turn) => plain(turn_row(state, held, turn)),
            Claude::Status(_) => plain(RowKind::Hidden),
            Claude::Boundary(b) => plain(boundary(b, Provider::Claude)),
            Claude::AgentMessage(m) => plain(agent_message(held, m)),
            Claude::ApiError(e) => error(e),
            Claude::Slash(s) => plain(slash(held, s)),
            Claude::Unrecognized(u) => plain(unrecognized(u)),
            Claude::Plan(plan) => plain(plan_row(state, held, plan)),
            Claude::ModelSwitch(m) => plain(model_switch(m)),
            Claude::Compaction(c) => plain(compaction(c)),
            Claude::Ask(ask) => plain(ask_row(ask)),
        },
        ItemBody::Codex(kind) => match kind {
            Codex::CommandOutput(s) => plain(slash(held, s)),
            Codex::SafetyCheck(_) => plain(RowKind::Hidden),
            Codex::Prompt(_) if wire::is_continued_input(&item.input_id) => plain(continued()),
            Codex::Prompt(_) => plain(RowKind::Prompt {
                text: text(),
                steered: false,
            }),
            Codex::Steer(_) => plain(RowKind::Prompt {
                text: text(),
                steered: true,
            }),
            Codex::Message(m) => plain(prose(held, m.complete, false)),
            Codex::WorkingNote(m) => plain(prose(held, m.complete, true)),
            Codex::Plan(plan) => plain(plan_row(state, held, plan)),
            Codex::Reasoning(r) => {
                let text = if item.text.is_empty() {
                    r.summary.join("\n")
                } else {
                    item.text.clone()
                };
                plain(RowKind::Thinking {
                    text,
                    open: !r.complete,
                    duration_ms: thinking_duration(state, held, r.complete),
                })
            }
            Codex::Work(work) => codex_work(state, held, work),
            Codex::Turn(turn) => plain(turn_row(state, held, turn)),
            Codex::Boundary(b) => plain(boundary(b, Provider::Codex)),
            Codex::AgentMessage(m) => plain(agent_message(held, m)),
            // Codex telling a client it will stop supporting something the
            // client sends: words for amux's developers, which the agent
            // logs, not for the person chatting. Earlier builds kept them
            // as error items, so a history may still hold one.
            Codex::Error(e) if e.error_kind == ui_state::body::CODEX_DEPRECATION_NOTICE => {
                plain(RowKind::Hidden)
            }
            Codex::Error(e) => error(e),
            Codex::Unrecognized(u) => plain(unrecognized(u)),
            Codex::Verdict(v) => plain(RowKind::AutoReview {
                status: v.status(),
                status_word: v.status_word.clone(),
                risk: v.risk.clone(),
                rationale: v.rationale.clone(),
                subject: v.item_key.clone(),
            }),
            Codex::Ask(ask) => plain(ask_row(ask)),
        },
        ItemBody::Undecodable => plain(RowKind::Unrecognized {
            what: match wire::Kind::try_from(item.kind) {
                Ok(kind) => kind
                    .as_str_name()
                    .trim_start_matches("KIND_")
                    .to_ascii_lowercase(),
                Err(_) => format!("kind {}", item.kind),
            },
            summary: String::new(),
        }),
    }
}

/// An ask that is the work, drawn from its own item.
fn ask_row(item: &wire::AskItem) -> RowKind {
    use wire::ask_item::Ask;
    let closed = item.closed.clone().unwrap_or_default();
    let resolution = match item.closed.as_ref().map(|closed| closed.outcome()) {
        None => Resolution::Open,
        Some(wire::AskOutcome::Answered) => Resolution::Answered,
        Some(wire::AskOutcome::Declined) => Resolution::Declined,
        Some(wire::AskOutcome::Replied) => Resolution::Replied,
        Some(wire::AskOutcome::Cancelled) => Resolution::Cancelled,
        Some(wire::AskOutcome::Dismissed | wire::AskOutcome::Unspecified) => Resolution::Dismissed,
    };
    RowKind::Ask(match &item.ask {
        Some(Ask::Question(asked)) => {
            let answers: Vec<AnswerView> = closed
                .answers
                .iter()
                .map(|answer| AnswerView {
                    picked: answer
                        .picked
                        .iter()
                        .map(|label| lifted(label, "", "", false).label)
                        .collect(),
                    other: answer.other.clone(),
                    hidden: answer.hidden,
                    note: answer.note.clone(),
                    skipped: answer.picked.is_empty() && answer.other.is_none() && !answer.hidden,
                })
                .collect();
            AskRow::Questions {
                questions: asked.questions.iter().map(question).collect(),
                skipped: answers.iter().filter(|answer| answer.skipped).count() as u32,
                answers,
                reply: (!closed.reply.is_empty()).then_some(closed.reply),
                resolution,
            }
        }
        Some(Ask::Form(form)) => AskRow::Form {
            server: form.server.clone(),
            message: form.message.clone(),
            fields: closed.fields,
            resolution,
        },
        Some(Ask::Link(link)) => AskRow::Link {
            server: link.server.clone(),
            message: link.message.clone(),
            url: link.url.clone(),
            resolution,
        },
        Some(Ask::Access(access)) => AskRow::Grant {
            reason: access.reason.clone(),
            read: access.read.clone(),
            write: access.write.clone(),
            network: access.network,
            hosts: access.network_hosts.clone(),
            granted: closed.grant.map(|grant| Granted {
                read: grant.read,
                write: grant.write,
                network: grant.network,
                for_session: grant.for_session,
            }),
            resolution,
        },
        Some(Ask::Unanswerable(unanswerable)) => AskRow::Unanswerable {
            reason: unanswerable.reason.clone(),
            resolution,
        },
        None => {
            return RowKind::Unrecognized {
                what: "ask".into(),
                summary: String::new(),
            };
        }
    })
}

/// A plan item: the plan is the item's text.
fn plan_row(state: &SessionState, held: &Held, plan: &wire::Plan) -> RowKind {
    let verdict = match plan.verdict() {
        wire::PlanVerdict::Undecided => PlanVerdict::Open,
        wire::PlanVerdict::Approved => PlanVerdict::Approved,
        wire::PlanVerdict::ApprovedSwitching => PlanVerdict::ApprovedSwitching,
        wire::PlanVerdict::SentBack => PlanVerdict::SentBack,
        // A verdict from a newer build reads as closed, never as a call to act.
        wire::PlanVerdict::Dismissed | wire::PlanVerdict::Unspecified => PlanVerdict::Dismissed,
    };
    let (title, body) = plan_parts(&held.item.text, !plan.complete);
    RowKind::Ask(AskRow::Plan {
        title,
        body,
        verdict,
        mode_name: permission_name(&state.agent_state().permissions, &plan.permission),
        mode: plan.permission.clone(),
        note: plan.note.clone(),
        writing: !plan.complete,
    })
}

/// A permission's name as the agent's catalogue gives it; empty where the
/// catalogue does not name it, so a client words the value itself.
pub(crate) fn permission_name(offered: &[wire::OfferedPermission], mode: &str) -> String {
    offered
        .iter()
        .find(|permission| !mode.is_empty() && permission.value == mode)
        .map(|permission| permission.display_name.clone())
        .unwrap_or_default()
}

/// A plan's title and body. An opening heading of any level is lifted out
/// as the title, unless it only says "Plan", which names nothing beyond
/// the plan's own landmark; the body is what follows it. While the first
/// line is still being written it may yet become a heading, so there is
/// no title yet and nothing of that line in the body.
pub(crate) fn plan_parts(text: &str, writing: bool) -> (Option<String>, String) {
    let text = text.trim_start_matches(['\n', '\r', ' ']);
    let (first, rest) = match text.split_once('\n') {
        Some(split) => split,
        None if writing && (text.is_empty() || text.starts_with('#')) => {
            return (None, String::new());
        }
        None => (text, ""),
    };
    let heading = first.trim_start();
    if !heading.starts_with('#') {
        return (None, text.to_owned());
    }
    let title = heading.trim_start_matches('#').trim();
    let named = !title.is_empty() && !title.eq_ignore_ascii_case("plan");
    (named.then(|| title.to_owned()), rest.to_owned())
}

/// A message's text and attachments as a row draws them. Direction
/// controls are dropped from the text, so a name quoted in it reads the
/// same on every client and cannot turn the rest of the message around.
fn chat_segments(held: &Held) -> Vec<Segment> {
    segments(
        &without_direction_controls(&held.item.text),
        &held.item.attachments,
        &held.item.input_id,
    )
}

fn prose(held: &Held, complete: bool, working_note: bool) -> RowKind {
    RowKind::Prose {
        text: chat_segments(held),
        streaming: !complete,
        working_note,
    }
}

fn thinking(state: &SessionState, held: &Held, complete: bool) -> RowKind {
    RowKind::Thinking {
        text: held.item.text.clone(),
        open: !complete,
        duration_ms: thinking_duration(state, held, complete),
    }
}

/// "Thought for 8s": the thinking item's time minus the item before it,
/// passing over headless Claude's status items. Claude records one as each
/// request goes out, and timing from it would leave out the time between
/// the person's prompt or the last tool result and that request. None when
/// there is no item before it: terminal Claude's thinking row lands with
/// the item before it, so the two times say nothing about how long it
/// thought.
fn thinking_duration(state: &SessionState, held: &Held, complete: bool) -> Option<i64> {
    if !complete {
        return None;
    }
    let before = state
        .transcript()
        .range(0..=held.item.order.saturating_sub(1))
        .rev()
        .find(|before| !is_status(before))?;
    Some(held.item.at_ms - before.item.at_ms).filter(|ms| *ms > 0)
}

fn is_status(held: &Held) -> bool {
    matches!(
        held.body,
        ItemBody::Claude(wire::claude_item::Kind::Status(_))
    )
}

/// A turn's end. A turn interrupted so the person's reply could go instead
/// of answering a question ended for that reply, not because anyone
/// stopped it, so it ends as a turn does.
fn turn_row(state: &SessionState, held: &Held, turn: &wire::Turn) -> RowKind {
    let duration_ms =
        (turn.started_at_ms > 0).then(|| (held.item.at_ms - turn.started_at_ms).max(0));
    match TurnOutcome::try_from(turn.outcome).unwrap_or(TurnOutcome::Unspecified) {
        TurnOutcome::Interrupted if replied_instead(state, held) => RowKind::TurnEnd {
            duration_ms,
            failed: false,
        },
        TurnOutcome::Interrupted | TurnOutcome::Stopped => RowKind::Stopped,
        TurnOutcome::Failed => RowKind::TurnEnd {
            duration_ms,
            failed: true,
        },
        TurnOutcome::Completed | TurnOutcome::Unspecified => RowKind::TurnEnd {
            duration_ms,
            failed: false,
        },
    }
}

/// Whether the turn ending at `end` held a question the person replied to
/// in their own words instead of answering.
fn replied_instead(state: &SessionState, end: &Held) -> bool {
    use wire::claude_item::Kind as Claude;
    use wire::codex_item::Kind as Codex;
    for held in state
        .transcript()
        .range(0..=end.item.order.saturating_sub(1))
        .rev()
    {
        match &held.body {
            ItemBody::Claude(Claude::Ask(ask)) | ItemBody::Codex(Codex::Ask(ask))
                if ask
                    .closed
                    .as_ref()
                    .is_some_and(|closed| closed.outcome() == wire::AskOutcome::Replied) =>
            {
                return true;
            }
            ItemBody::Claude(Claude::Turn(_) | Claude::Prompt(_) | Claude::Steer(_))
            | ItemBody::Codex(Codex::Turn(_) | Codex::Prompt(_) | Codex::Steer(_)) => {
                return false;
            }
            _ => {}
        }
    }
    false
}

fn compaction(c: &wire::Compaction) -> RowKind {
    RowKind::Compaction {
        tokens_before: c.tokens_before,
        tokens_after: c.tokens_after,
        automatic: c.automatic,
        idle: c.idle,
    }
}

fn error(e: &wire::ApiError) -> (RowKind, Option<Decision>, bool) {
    let gave_up = !e.will_retry;
    (
        RowKind::Error {
            error_kind: e.error_kind.clone(),
            message: e.message.clone(),
            attempts: e.attempt,
            gave_up,
        },
        None,
        gave_up,
    )
}

/// Which provider wrote a boundary, for the one boundary they report
/// differently.
#[derive(Clone, Copy, PartialEq)]
enum Provider {
    Claude,
    Codex,
}

/// A boundary is drawn only where the chat above it no longer holds: a clear,
/// after which the agent remembers none of it, and a lost daemon. A start, a
/// resume, a restart, a fork or an exit carries on the same conversation, and
/// a rule across it for each said nothing the person acts on; how an exited
/// agent ended is the composer's to say. Claude writes its compaction as a
/// row of its own beside the boundary, with the counts; Codex writes only the
/// boundary, so its compaction is drawn as that row without counts.
fn boundary(b: &wire::Boundary, provider: Provider) -> RowKind {
    match b.kind() {
        BoundaryKind::Cleared | BoundaryKind::DaemonLost => RowKind::Boundary {
            kind: b.kind(),
            cause: ui_state::exit_words(b.cause, b.cause_detail.as_deref()),
        },
        BoundaryKind::Compacted if provider == Provider::Codex => RowKind::Compaction {
            tokens_before: None,
            tokens_after: None,
            automatic: false,
            idle: b.idle,
        },
        BoundaryKind::Unspecified
        | BoundaryKind::Started
        | BoundaryKind::Compacted
        | BoundaryKind::Resumed
        | BoundaryKind::Forked
        | BoundaryKind::Restarted
        | BoundaryKind::Exited => RowKind::Hidden,
    }
}

/// The prompt an agent was resumed with after the machine restarted under
/// its turn: nobody sent it, so it is drawn as the line saying the agent
/// carried on, not as the person's words.
fn continued() -> RowKind {
    RowKind::Boundary {
        kind: BoundaryKind::Resumed,
        cause: CONTINUED_WORDS.to_owned(),
    }
}

/// What the line over a continuation after a restart says, after the
/// boundary's own word.
pub const CONTINUED_WORDS: &str = "after the device restarted";

fn agent_message(held: &Held, m: &wire::AgentMessage) -> RowKind {
    let from = match m.from.as_ref().and_then(|from| from.value.as_ref()) {
        Some(wire::sender::Value::Agent(agent)) => agent.name.clone(),
        Some(wire::sender::Value::Human(_)) | None => String::new(),
    };
    RowKind::AgentMessage {
        from,
        kind: m.kind(),
        text: chat_segments(held),
        to: m.to.clone(),
        sent: m.send_state(),
        rejection: m.rejection.clone(),
    }
}

fn slash(held: &Held, s: &wire::SlashOutput) -> RowKind {
    RowKind::SlashOutput {
        command: s.command.clone(),
        args: s.args.clone(),
        output: held.item.text.clone(),
    }
}

fn model_switch(m: &wire::ModelSwitch) -> RowKind {
    RowKind::ModelSwitch {
        to: m.to.clone(),
        reason: m.reason.clone(),
    }
}

fn unrecognized(u: &wire::Unrecognized) -> RowKind {
    RowKind::Unrecognized {
        what: u.fact_type.clone(),
        summary: u.summary.clone(),
    }
}

fn lines(text: &str) -> u32 {
    if text.is_empty() {
        0
    } else {
        text.lines().count() as u32
    }
}

/// What the interpreter read from a call's input: what it names, and the
/// arm the views draw it by.
fn read_input(tool: &ToolCall) -> (&str, Option<&tool_input::Of>) {
    tool.input.as_ref().map_or(("", None), |input| {
        (input.subject.as_str(), input.of.as_ref())
    })
}

/// What a step that only looks says about how it went: why it failed, or
/// what it found. A read found the file, which is not news on its row.
fn explore_result(verb: ExploreVerb, phase: CallPhase, outcome: &str) -> String {
    match phase {
        CallPhase::Failed => terminal_text(outcome).trim().to_owned(),
        CallPhase::Succeeded if verb != ExploreVerb::Read => {
            terminal_text(outcome).trim().to_owned()
        }
        _ => String::new(),
    }
}

fn output_head(text: &str) -> (Vec<String>, usize) {
    let all: Vec<&str> = text.lines().collect();
    let head = all
        .iter()
        .take(OUTPUT_HEAD_LINES)
        .map(|line| (*line).to_owned())
        .collect();
    (head, all.len().saturating_sub(OUTPUT_HEAD_LINES))
}

fn output_tail(text: &str) -> Vec<String> {
    let all: Vec<&str> = text.lines().collect();
    all[all.len().saturating_sub(OUTPUT_TAIL_LINES)..]
        .iter()
        .map(|line| (*line).to_owned())
        .collect()
}

fn image_of(attachments: &[Attachment]) -> Option<BlobRef> {
    attachments
        .iter()
        .find_map(|attachment| match &attachment.of {
            Some(wire::attachment::Of::Image(blob)) => Some(blob.clone()),
            _ => None,
        })
}

fn claude_tool(
    state: &SessionState,
    held: &Held,
    tool: &ToolCall,
) -> (RowKind, Option<Decision>, bool) {
    let decision = decision_view(tool.decision.as_ref(), &state.agent_state().permissions);
    let view = call_phase(state, held, tool.state, decision.as_ref());
    let failed = view == CallPhase::Failed;
    let (named, of) = read_input(tool);
    // Paths in the agent's folder are named from it.
    let subject = within(&state.agent().cwd, named);
    let duration_ms = duration(claude_started(held, tool), tool.ended_at_ms);
    let kind = if let Some(image) = image_of(&tool.attachments) {
        RowKind::Image {
            image: Some(image),
            path: subject,
            generated: false,
        }
    } else if !tool.server.is_empty() {
        // Any other tool's input is Claude's own JSON, which amux's calls
        // are read from.
        let input = match of {
            Some(tool_input::Of::OtherJson(json)) => {
                serde_json::from_slice(json).unwrap_or(Value::Null)
            }
            _ => Value::Null,
        };
        amux_call(&tool.server, &tool.name, &input, &tool.outcome_text, view).unwrap_or_else(|| {
            RowKind::ToolCall {
                server: tool.server.clone(),
                tool: tool.name.clone(),
                fact: named.to_owned(),
                state: view,
                result: tool.outcome_text.clone(),
            }
        })
    } else if let Some(verb) = explore_verb(tool.class) {
        RowKind::Explore {
            verb,
            subject,
            state: view,
            result: explore_result(verb, view, &tool.outcome_text),
        }
    } else {
        match of {
            Some(tool_input::Of::Shell(shell)) if tool.background => RowKind::Background {
                command: named.to_owned(),
                description: shell.description.clone(),
                running: view.in_flight(),
                duration_ms,
            },
            Some(tool_input::Of::Shell(shell)) => {
                let output = terminal_text(&tool.outcome_text);
                let (output_head, more_lines) = output_head(&output);
                RowKind::Command {
                    command: named.to_owned(),
                    description: shell.description.clone(),
                    state: view,
                    exit_code: tool.exit_code,
                    output_head,
                    more_lines,
                    output_trimmed: false,
                    output_tail: output_tail(&output),
                    duration_ms,
                }
            }
            Some(tool_input::Of::Edit(edit)) if edit.write => {
                let content = edit
                    .replacements
                    .first()
                    .map_or(0, |write| lines(&write.new));
                let (change, (added, removed)) = match tool.created {
                    Some(true) => (FileChangeView::Created { lines: content }, (content, 0)),
                    Some(false) => (FileChangeView::Edited, claude_counts(tool, edit)),
                    None => (FileChangeView::Writing { lines: content }, (content, 0)),
                };
                RowKind::FileChange {
                    files: vec![FileRow {
                        path: subject,
                        change,
                        added,
                        removed,
                        line: tool.line,
                    }],
                    state: view,
                }
            }
            Some(tool_input::Of::Edit(edit)) => {
                let (added, removed) = claude_counts(tool, edit);
                RowKind::FileChange {
                    files: vec![FileRow {
                        path: subject,
                        change: FileChangeView::Edited,
                        added,
                        removed,
                        line: tool.line,
                    }],
                    state: view,
                }
            }
            Some(tool_input::Of::Subagent(_)) => RowKind::Subagent {
                description: named.to_owned(),
                continued: false,
                state: view,
                answer: tool.outcome_text.clone(),
                duration_ms,
            },
            Some(tool_input::Of::TaskList(_)) => RowKind::Hidden,
            // A SendMessage to a subagent it started: the call that
            // continues it, opening the same chat.
            _ if !tool.child_agent_id.is_empty() => RowKind::Subagent {
                description: named.to_owned(),
                continued: true,
                state: view,
                answer: tool.outcome_text.clone(),
                duration_ms,
            },
            _ => RowKind::ToolCall {
                server: String::new(),
                tool: tool.name.clone(),
                fact: named.to_owned(),
                state: view,
                result: tool.outcome_text.clone(),
            },
        }
    };
    (kind, decision, failed)
}

/// A string field of a JSON object, empty when it is missing.
fn field(value: &Value, name: &str) -> String {
    value
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// amux's own calls that a person reads better as what they did than as
/// a tool call: a subagent started or stopped. Listing the fleet or the hosts
/// is the agent looking around, and hidden. None for any other call.
fn amux_call(
    server: &str,
    tool: &str,
    input: &Value,
    result: &str,
    state: CallPhase,
) -> Option<RowKind> {
    if server != wire::AMUX_TOOL_SERVER {
        return None;
    }
    let action = match tool {
        wire::AGENTS_TOOL | wire::HOSTS_TOOL => return Some(RowKind::Hidden),
        wire::SPAWN_TOOL => ChildAction::Start,
        wire::STOP_TOOL => ChildAction::Stop,
        _ => return None,
    };
    // A spawn may leave the name to amux; its answer then names the child,
    // as `{"name", "id"}` or, from Codex, inside the tool result's text.
    let answered = || {
        let answer: Value = serde_json::from_str(result).ok()?;
        let answer = match answer["content"][0]["text"].as_str() {
            Some(text) => serde_json::from_str(text).ok()?,
            None => answer,
        };
        answer["name"].as_str().map(str::to_owned)
    };
    let name = field(input, "name");
    let name = if name.is_empty() {
        answered().unwrap_or_default()
    } else {
        name
    };
    let kind = match field(input, "kind").as_str() {
        "claude" => crate::fleet::kind_name(wire::Kind::ClaudeSdk),
        "codex" => crate::fleet::kind_name(wire::Kind::Codex),
        _ => "",
    };
    Some(RowKind::Child {
        action,
        name,
        host: field(input, "host"),
        kind: kind.to_owned(),
        state,
        result: if state == CallPhase::Failed {
            result.to_owned()
        } else {
            String::new()
        },
    })
}

/// Added and removed lines: from the patch Claude reported when it gave
/// one, else from the replaced and replacing text.
fn claude_counts(tool: &ToolCall, edit: &wire::EditInput) -> (u32, u32) {
    if !tool.patch.is_empty() {
        let mut added = 0;
        let mut removed = 0;
        for line in tool.patch.iter().flat_map(|hunk| &hunk.lines) {
            match line.chars().next() {
                Some('+') => added += 1,
                Some('-') => removed += 1,
                _ => {}
            }
        }
        return (added, removed);
    }
    edit.replacements
        .iter()
        .fold((0, 0), |(added, removed), replacement| {
            (
                added + lines(&replacement.new),
                removed + lines(&replacement.old),
            )
        })
}

/// The verb a call that only looks is drawn with; none for one drawn its
/// own way.
fn explore_verb(class: i32) -> Option<ExploreVerb> {
    match wire::ToolClass::try_from(class) {
        Ok(wire::ToolClass::Read) => Some(ExploreVerb::Read),
        Ok(wire::ToolClass::Search) => Some(ExploreVerb::Search),
        Ok(wire::ToolClass::List) => Some(ExploreVerb::List),
        Ok(wire::ToolClass::Fetch) => Some(ExploreVerb::Fetch),
        Ok(wire::ToolClass::WebSearch) => Some(ExploreVerb::WebSearch),
        Ok(
            wire::ToolClass::Look | wire::ToolClass::Unspecified | wire::ToolClass::Consequential,
        )
        | Err(_) => None,
    }
}

fn codex_work(
    state: &SessionState,
    held: &Held,
    work: &wire::Work,
) -> (RowKind, Option<Decision>, bool) {
    use wire::work::Of;
    let decision = decision_view(work.decision.as_ref(), &state.agent_state().permissions);
    let view = call_phase(state, held, work.state, decision.as_ref());
    let failed = view == CallPhase::Failed;
    let duration_ms = duration(
        work.started_at_ms.unwrap_or(held.item.at_ms),
        work.ended_at_ms,
    );
    let kind = match (&work.of, explore_verb(work.class)) {
        (Some(Of::Command(command)), Some(verb)) => RowKind::Explore {
            verb,
            subject: command.command.clone(),
            state: view,
            result: explore_result(verb, view, &held.item.text),
        },
        (of, _) => match of {
            Some(Of::Command(command)) if command.background => RowKind::Background {
                command: command.command.clone(),
                description: String::new(),
                running: view.in_flight(),
                duration_ms,
            },
            Some(Of::Command(command)) => {
                let output = terminal_text(&held.item.text);
                let (output_head, more_lines) = output_head(&output);
                RowKind::Command {
                    command: command.command.clone(),
                    description: String::new(),
                    state: view,
                    exit_code: command.exit_code,
                    output_head,
                    more_lines,
                    output_trimmed: command.output_dropped_bytes > 0,
                    output_tail: output_tail(&output),
                    duration_ms,
                }
            }
            Some(Of::FileChange(change)) => {
                // A patch that did not land wrote nothing: a file it would
                // have added is not said to be created.
                let landed = view.in_flight() || view == CallPhase::Succeeded;
                RowKind::FileChange {
                    files: change
                        .changes
                        .iter()
                        .map(|change| codex_file(&state.agent().cwd, change, landed))
                        .collect(),
                    state: view,
                }
            }
            Some(Of::Mcp(call)) => {
                let input = serde_json::from_slice(&call.arguments_json).unwrap_or(Value::Null);
                let result = if call.error.is_empty() {
                    String::from_utf8_lossy(&call.result_json).into_owned()
                } else {
                    call.error.clone()
                };
                amux_call(&call.server, &call.tool, &input, &result, view).unwrap_or_else(|| {
                    RowKind::ToolCall {
                        server: call.server.clone(),
                        tool: call.tool.clone(),
                        fact: call.subject.clone(),
                        state: view,
                        result,
                    }
                })
            }
            Some(Of::WebSearch(search)) => {
                let (verb, subject) = web_step(search);
                RowKind::Explore {
                    verb,
                    subject,
                    state: view,
                    result: explore_result(verb, view, &held.item.text),
                }
            }
            Some(Of::Image(image)) => RowKind::Image {
                image: image_of(&held.item.attachments),
                path: image.path.clone(),
                generated: image.generated,
            },
            // Waiting on, closing, interrupting or listing child threads is
            // the parent's bookkeeping on agents its spawns open; a spawn
            // from history that names no agent still draws.
            Some(Of::Collab(collab))
                if ui_state::opens(&held.body).is_some() || collab.tool == "spawnAgent" =>
            {
                let spawns = collab.tool == "spawnAgent";
                // A spawn that opens its child is named by the child's
                // title; the task Codex also puts on the call opens the
                // child's own chat. A follow-up says what it sent. Each
                // call returns at once while the child works on, so its
                // own time is not the child's.
                let named_by_child = spawns && ui_state::opens(&held.body).is_some();
                RowKind::Subagent {
                    description: if named_by_child {
                        String::new()
                    } else {
                        collab.prompt.clone()
                    },
                    continued: !spawns,
                    state: view,
                    answer: String::new(),
                    duration_ms: None,
                }
            }
            Some(Of::Collab(_)) => RowKind::Hidden,
            None => RowKind::Unrecognized {
                what: "work".into(),
                summary: String::new(),
            },
        },
    };
    (kind, decision, failed)
}

/// What a Codex web step looked at: the page it opened, the words it
/// looked for within one, or what it searched the web for.
fn web_step(search: &wire::WebSearch) -> (ExploreVerb, String) {
    let first = |a: &str, b: &str| if a.is_empty() { b } else { a }.to_owned();
    match search.action() {
        wire::WebAction::OpenPage => (ExploreVerb::Fetch, first(&search.url, &search.query)),
        wire::WebAction::FindInPage => (ExploreVerb::Search, first(&search.pattern, &search.url)),
        wire::WebAction::Search | wire::WebAction::Unspecified => {
            (ExploreVerb::WebSearch, first(&search.query, &search.url))
        }
    }
}

/// A Codex file change as a row's file, named from the agent's folder
/// `cwd`. One that did not `land` (failed, denied or cancelled) made no
/// file, so a file it would have added reads as a write that never
/// happened.
pub(crate) fn codex_file(cwd: &str, change: &wire::FileChange, landed: bool) -> FileRow {
    let (added, removed) = counts(&change_hunks(change));
    let change_view =
        match FileChangeKind::try_from(change.kind).unwrap_or(FileChangeKind::Unspecified) {
            FileChangeKind::Add if landed => FileChangeView::Created { lines: added },
            FileChangeKind::Add => FileChangeView::Writing { lines: added },
            FileChangeKind::Delete => FileChangeView::Deleted,
            _ if !change.move_to.is_empty() => FileChangeView::Moved {
                to: within(cwd, &change.move_to),
            },
            _ => FileChangeView::Edited,
        };
    FileRow {
        path: within(cwd, &change.path),
        change: change_view,
        added,
        removed,
        line: change.line,
    }
}

/// A Codex file change's patch as hunks. Codex gives a file it adds or
/// deletes as the file's text, not as a diff, so each of its lines is added
/// or removed whole, in one hunk.
pub(crate) fn change_hunks(change: &wire::FileChange) -> Vec<PatchHunk> {
    let whole = |kind| {
        let lines: Vec<PatchLine> = change
            .patch
            .lines()
            .enumerate()
            .map(|(i, text)| PatchLine {
                number: Some(i as u32 + 1),
                kind,
                text: text.to_owned(),
            })
            .collect();
        if lines.is_empty() {
            Vec::new()
        } else {
            vec![PatchHunk { start: None, lines }]
        }
    };
    let diff = change.patch.lines().any(|line| line.starts_with("@@"));
    match FileChangeKind::try_from(change.kind).unwrap_or(FileChangeKind::Unspecified) {
        FileChangeKind::Add if !diff => whole(LineKind::Added),
        FileChangeKind::Delete if !diff => whole(LineKind::Removed),
        _ => unified_patch(&change.patch),
    }
}

/// Added and removed lines among hunks.
pub(crate) fn counts<'a>(hunks: impl IntoIterator<Item = &'a PatchHunk>) -> (u32, u32) {
    hunks
        .into_iter()
        .flat_map(|hunk| &hunk.lines)
        .fold((0, 0), |(added, removed), line| match line.kind {
            LineKind::Added => (added + 1, removed),
            LineKind::Removed => (added, removed + 1),
            LineKind::Context => (added, removed),
        })
}

/// One line of a patch, numbered on the side it belongs to (the new file
/// for context and added lines, the old for removed ones) when the provider
/// said where its hunk starts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
pub struct PatchLine {
    pub number: Option<u32>,
    pub kind: LineKind,
    pub text: String,
}

/// A stretch of a patch whose lines are neighbours in the file: one
/// hunk of a diff, or one of the places a Claude edit replaces text.
///
/// A file's change is always carried as its hunks, by an edit ask's card
/// and by a landed edit's head alike, because the lines of two hunks are
/// not neighbours. Kept apart in the data itself, nothing that reads the
/// lines as code (the highlighting, which a comment or string one hunk
/// leaves open would otherwise run into the next) can join them, whether
/// or not a client draws where each starts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
pub struct PatchHunk {
    /// Where the hunk starts, when the provider said: a diff's hunk header
    /// does, a Claude edit's replacement is only text and does not.
    pub start: Option<HunkStart>,
    pub lines: Vec<PatchLine>,
}

/// A hunk's first line in the old file and in the new.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
pub struct HunkStart {
    pub old: u32,
    pub new: u32,
}

/// One file's part of an edit ask's patch, as hunks. A file's own header
/// lines are not carried. Each client decides whether to draw a hunk's
/// start.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
pub struct FilePatch {
    /// In a patch of several files, the file these hunks are in, named
    /// from the agent's folder, and what happens to it (added, deleted,
    /// moved or edited), with its counts, for the client to draw where its
    /// lines start. None in an edit of one file, which the card names
    /// already.
    pub file: Option<FileRow>,
    pub hunks: Vec<PatchHunk>,
}

/// The head of a landed file change: its first file's first lines, hunk
/// by hunk, and how many lines of that file's patch follow them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
pub struct PatchHead {
    pub hunks: Vec<PatchHunk>,
    pub more: usize,
}

impl PatchHead {
    /// Every line of the head, hunk after hunk.
    pub fn lines(&self) -> impl Iterator<Item = &PatchLine> {
        self.hunks.iter().flat_map(|hunk| &hunk.lines)
    }
}

/// The first `max` lines of the patch a landed file change made, for a
/// chat to show under its row; None for a row that changed no file, has
/// not landed, or carries no patch.
pub fn patch_head(state: &SessionState, key: &Key, max: usize) -> Option<PatchHead> {
    let held = state.transcript().get(key)?;
    let hunks = match &held.body {
        ItemBody::Claude(wire::claude_item::Kind::Tool(tool)) => {
            if !succeeded(tool.state) {
                return None;
            }
            claude_patch(tool)?
        }
        ItemBody::Codex(wire::codex_item::Kind::Work(work)) => {
            if !succeeded(work.state) {
                return None;
            }
            match &work.of {
                Some(wire::work::Of::FileChange(change)) => change_hunks(change.changes.first()?),
                _ => return None,
            }
        }
        _ => return None,
    };
    let total: usize = hunks.iter().map(|hunk| hunk.lines.len()).sum();
    if total == 0 {
        return None;
    }
    let mut left = max;
    let hunks = hunks
        .into_iter()
        .map_while(|mut hunk| {
            if left == 0 {
                return None;
            }
            hunk.lines.truncate(left);
            left -= hunk.lines.len();
            Some(hunk)
        })
        .filter(|hunk| !hunk.lines.is_empty())
        .collect();
    Some(PatchHead {
        hunks,
        more: total.saturating_sub(max),
    })
}

fn claude_patch(tool: &ToolCall) -> Option<Vec<PatchHunk>> {
    let (_, Some(tool_input::Of::Edit(edit))) = read_input(tool) else {
        return None;
    };
    if !tool.patch.is_empty() {
        return Some(
            tool.patch
                .iter()
                .map(|hunk| {
                    let (mut old, mut new) = (Some(hunk.old_start), Some(hunk.new_start));
                    PatchHunk {
                        start: Some(HunkStart {
                            old: hunk.old_start,
                            new: hunk.new_start,
                        }),
                        lines: hunk
                            .lines
                            .iter()
                            .map(|line| numbered(line, &mut old, &mut new))
                            .collect(),
                    }
                })
                .collect(),
        );
    }
    if edit.write {
        let content = edit.replacements.first().map_or("", |write| &write.new);
        return Some(vec![PatchHunk {
            start: None,
            lines: content
                .lines()
                .enumerate()
                .map(|(i, text)| PatchLine {
                    number: Some(i as u32 + 1),
                    kind: LineKind::Added,
                    text: text.to_owned(),
                })
                .collect(),
        }]);
    }
    Some(replaced_hunks(edit))
}

/// What a Claude edit replaces and writes, as unnumbered hunks, one for
/// each replacement, since each is a place of its own in the file: the
/// replaced text removed, then its replacement added. There is no file to
/// number them against.
pub(crate) fn replaced_hunks(edit: &wire::EditInput) -> Vec<PatchHunk> {
    edit.replacements
        .iter()
        .map(|replacement| PatchHunk {
            start: None,
            lines: replacement_lines(replacement),
        })
        .filter(|hunk| !hunk.lines.is_empty())
        .collect()
}

/// One replacement's lines: the replaced text removed, then its
/// replacement added.
fn replacement_lines(replacement: &wire::Replacement) -> Vec<PatchLine> {
    [
        (&replacement.old, LineKind::Removed),
        (&replacement.new, LineKind::Added),
    ]
    .into_iter()
    .flat_map(|(text, kind)| {
        text.lines().map(move |text| PatchLine {
            number: None,
            kind,
            text: text.to_owned(),
        })
    })
    .collect()
}

/// One patch line, numbered from the running old and new positions.
fn numbered(line: &str, old: &mut Option<u32>, new: &mut Option<u32>) -> PatchLine {
    let (kind, text) = match line.chars().next() {
        Some('+') => (LineKind::Added, &line[1..]),
        Some('-') => (LineKind::Removed, &line[1..]),
        Some(' ') => (LineKind::Context, &line[1..]),
        _ => (LineKind::Context, line),
    };
    let number = if kind == LineKind::Removed {
        *old
    } else {
        *new
    };
    let step = |at: &mut Option<u32>| {
        if let Some(at) = at.as_mut() {
            *at += 1;
        }
    };
    match kind {
        LineKind::Removed => step(old),
        LineKind::Added => step(new),
        LineKind::Context => {
            step(old);
            step(new);
        }
    }
    PatchLine {
        number,
        kind,
        text: text.to_owned(),
    }
}

/// A unified diff's hunks, their lines numbered from each hunk's header. A
/// file's header lines ("--- ", "+++ ", "index " and the rest) come only
/// outside a hunk: inside one, the header's counts say how many lines are
/// the hunk's, so a removed "-- comment" or an added "++i" is a line of
/// the patch. A hunk header without counts runs to the next header, and
/// has no start. Every header, and every file's `diff --git` line, starts
/// a hunk; lines before the first form one of their own.
pub(crate) fn unified_patch(patch: &str) -> Vec<PatchHunk> {
    let mut hunks = vec![PatchHunk {
        start: None,
        lines: Vec::new(),
    }];
    let (mut old, mut new) = (None, None);
    // Lines each side of the open hunk still holds, from its header.
    let (mut old_left, mut new_left) = (0u32, 0u32);
    let mut uncounted = false;
    fn push(hunks: &mut [PatchHunk], line: PatchLine) {
        hunks
            .last_mut()
            .expect("one hunk at least")
            .lines
            .push(line);
    }
    for line in patch.lines() {
        if line.starts_with('\\') {
            continue;
        }
        if old_left > 0 || new_left > 0 {
            match line.chars().next() {
                Some('-') => old_left = old_left.saturating_sub(1),
                Some('+') => new_left = new_left.saturating_sub(1),
                _ => {
                    old_left = old_left.saturating_sub(1);
                    new_left = new_left.saturating_sub(1);
                }
            }
            push(&mut hunks, numbered(line, &mut old, &mut new));
            continue;
        }
        if let Some(header) = line.strip_prefix("@@") {
            let range = |sign: char| {
                let range = header
                    .split_whitespace()
                    .find_map(|part| part.strip_prefix(sign))?;
                let mut parts = range.split(',');
                let start = parts.next()?.parse().ok()?;
                let count = parts.next().map_or(Some(1), |count| count.parse().ok())?;
                Some((start, count))
            };
            let (from, to) = (range('-'), range('+'));
            (old, new) = (from.map(|(start, _)| start), to.map(|(start, _)| start));
            (old_left, new_left) = (
                from.map_or(0, |(_, count)| count),
                to.map_or(0, |(_, count)| count),
            );
            uncounted = from.is_none() || to.is_none();
            hunks.push(PatchHunk {
                start: from
                    .zip(to)
                    .map(|((old, _), (new, _))| HunkStart { old, new }),
                lines: Vec::new(),
            });
            continue;
        }
        if line.starts_with("diff --git") {
            uncounted = false;
            (old, new) = (None, None);
            hunks.push(PatchHunk {
                start: None,
                lines: Vec::new(),
            });
            continue;
        }
        if !uncounted
            && (line.starts_with("index ")
                || line.starts_with("--- ")
                || line.starts_with("+++ ")
                || line.starts_with("new file")
                || line.starts_with("deleted file"))
        {
            continue;
        }
        push(&mut hunks, numbered(line, &mut old, &mut new));
    }
    hunks.retain(|hunk| !hunk.lines.is_empty());
    hunks
}

/// The new file's line a file's first change lands on: an added line's
/// own, or for a removal the new-side line where it was, counting from its
/// hunk's start past the context before it.
pub(crate) fn first_change(hunks: &[PatchHunk]) -> Option<u32> {
    for hunk in hunks {
        let mut at = hunk.start.map(|start| start.new);
        for line in &hunk.lines {
            match line.kind {
                LineKind::Added => return line.number.or(at).map(|n| n.max(1)),
                LineKind::Removed => return at.map(|n| n.max(1)),
                LineKind::Context => at = line.number.map(|n| n + 1),
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(number: u32, kind: LineKind, text: &str) -> PatchLine {
        PatchLine {
            number: Some(number),
            kind,
            text: text.to_owned(),
        }
    }

    /// Inside a hunk its header's counts say which lines are the patch's,
    /// so a removed SQL comment and an added increment are lines of it, not
    /// a file's header lines; the file's own headers before it are dropped.
    #[test]
    fn a_patch_keeps_lines_that_look_like_file_headers_inside_its_hunks() {
        let patch = "--- a/q.sql\n+++ b/q.sql\n@@ -3,3 +3,3 @@\n select 1;\n--- old note\n+++i;\n select 2;\n";
        let hunks = unified_patch(patch);
        assert_eq!(
            hunks,
            vec![PatchHunk {
                start: Some(HunkStart { old: 3, new: 3 }),
                lines: vec![
                    line(3, LineKind::Context, "select 1;"),
                    line(4, LineKind::Removed, "-- old note"),
                    line(4, LineKind::Added, "++i;"),
                    line(5, LineKind::Context, "select 2;"),
                ],
            }]
        );
        assert_eq!(counts(&hunks), (1, 1));
        assert_eq!(first_change(&hunks), Some(4), "past the hunk's context");
    }

    /// Every hunk header starts a hunk of its own, one without counts
    /// too, and so does each file of a patch: no two places in a file, or
    /// two files, share one.
    #[test]
    fn every_hunk_header_and_file_starts_a_hunk() {
        let patch = "@@ -1 +1 @@\n-a\n+b\n@@\n+c\n@@\n+d\ndiff --git a/e b/e\n+e\n";
        let hunks = unified_patch(patch);
        let shape: Vec<(Option<HunkStart>, Vec<&str>)> = hunks
            .iter()
            .map(|hunk| {
                let texts = hunk.lines.iter().map(|line| line.text.as_str()).collect();
                (hunk.start, texts)
            })
            .collect();
        assert_eq!(
            shape,
            vec![
                (Some(HunkStart { old: 1, new: 1 }), vec!["a", "b"]),
                (None, vec!["c"]),
                (None, vec!["d"]),
                (None, vec!["e"]),
            ]
        );
    }

    /// Each of a Claude edit's replacements is a place of its own in the
    /// file, so each is its own hunk.
    #[test]
    fn each_replacement_of_a_claude_edit_is_its_own_hunk() {
        let edit = wire::EditInput {
            replacements: vec![
                wire::Replacement {
                    old: "a".into(),
                    new: "/* b".into(),
                },
                wire::Replacement {
                    old: "c".into(),
                    new: "d".into(),
                },
            ],
            ..Default::default()
        };
        let hunks = replaced_hunks(&edit);
        let texts: Vec<Vec<(LineKind, &str)>> = hunks
            .iter()
            .map(|hunk| {
                hunk.lines
                    .iter()
                    .map(|line| (line.kind, line.text.as_str()))
                    .collect()
            })
            .collect();
        assert_eq!(
            texts,
            vec![
                vec![(LineKind::Removed, "a"), (LineKind::Added, "/* b")],
                vec![(LineKind::Removed, "c"), (LineKind::Added, "d")],
            ]
        );
        assert!(hunks.iter().all(|hunk| hunk.start.is_none()));
        assert_eq!(counts(&hunks), (2, 2));
    }

    #[test]
    fn a_plan_lifts_an_opening_heading_that_names_it() {
        let parts = |text, writing| plan_parts(text, writing);
        assert_eq!(
            parts("\n## Ship it\n\n- one", false),
            (Some("Ship it".to_owned()), "\n- one".to_owned())
        );
        // "Plan" names nothing the landmark does not.
        assert_eq!(parts("# Plan\n- one", false), (None, "- one".to_owned()));
        assert_eq!(parts("- one", false), (None, "- one".to_owned()));
        // A first line still being written may yet be a heading.
        assert_eq!(parts("## Shi", true), (None, String::new()));
        assert_eq!(parts("Do", true), (None, "Do".to_owned()));
        assert_eq!(
            parts("# Ship it", false),
            (Some("Ship it".to_owned()), String::new())
        );
    }
}
