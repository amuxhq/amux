//! Chat rows: one row per item, the row id the item key. A run of tool
//! steps is an attribute on its members (see [`crate::run`]).

use std::collections::HashSet;
use std::ops::RangeInclusive;

use schemars::JsonSchema;
use serde::Serialize;
use serde_json::Value;
use ui_state::{Fold, Held, ItemBody, Key, SessionState};
use wire::{
    Attachment, BlobRef, BoundaryKind, DecisionOutcome, EnvelopeKind, FileChangeKind, SendState,
    ToolCall, ToolState, TurnOutcome,
};

use crate::ask::{QuestionView, Scope, lift_rule, lifted, question, scope_of};
use crate::review::LineKind;
use crate::run::Run;
use crate::segments::{Segment, segments};

/// How many lines of a command's output a row carries.
pub const OUTPUT_HEAD_LINES: usize = 3;
/// The output's last lines a command row carries.
pub const OUTPUT_TAIL_LINES: usize = 12;

#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
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
    /// A subagent's own step: collapsed under the subagent's row, which
    /// opens to it.
    pub parent: Option<Key>,
}

/// Whether tool steps show, hide, or fold into their runs.
#[derive(Clone, Copy, Debug)]
pub enum ToolRows<'a> {
    ShowAll,
    /// Every step hidden but a failure its turn left unresolved.
    Hide,
    /// Each run folds to its newest step, where its one line goes, keeping
    /// a failure its turn left unresolved; under way it shows its newest
    /// few steps. A run `open` holds a step of shows every step (see
    /// [`crate::run_is_open`]).
    Collapse {
        open: &'a HashSet<Key>,
    },
}

#[derive(Clone, Copy, Debug)]
pub struct ChatOptions<'a> {
    pub tools: ToolRows<'a>,
}

impl Default for ChatOptions<'_> {
    fn default() -> Self {
        ChatOptions {
            tools: ToolRows::ShowAll,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
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
    Command {
        command: String,
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
    },
    Subagent {
        description: String,
        running: bool,
        tool_count: u32,
        last_tool: String,
        answer: String,
        duration_ms: Option<i64>,
    },
    Background {
        command: String,
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
        cost_usd: Option<f64>,
        failed: bool,
    },
    Stopped,
    Compaction {
        tokens_before: Option<u64>,
        tokens_after: Option<u64>,
        automatic: bool,
    },
    Error {
        error_kind: String,
        message: String,
        attempts: u32,
        gave_up: bool,
    },
    ModelSwitch {
        from: String,
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
        text: String,
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

#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
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
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
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
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Granted {
    pub read: Vec<String>,
    pub write: Vec<String>,
    pub network: bool,
    pub for_session: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
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
            | RowKind::FileChange { state, .. }
            | RowKind::Command { state, .. }
            | RowKind::Explore { state, .. } => Some(*state),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub enum ExploreVerb {
    Read,
    Search,
    List,
    Fetch,
    WebSearch,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct FileRow {
    pub path: String,
    pub change: FileChangeView,
    pub added: u32,
    pub removed: u32,
    /// The line the edit's first change landed on, once it landed.
    pub line: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
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
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Decision {
    pub outcome: DecisionView,
    /// What an allowance granted beyond this one call.
    pub granted: Option<PermissionGrant>,
    pub note: Option<String>,
    /// Answered in the provider's own interface.
    pub elsewhere: bool,
}

/// What a permission allowed from then on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
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
    /// Codex: commands starting with these words.
    CommandPrefix { words: Vec<String> },
    /// Codex: network access to these hosts.
    NetworkHosts { hosts: Vec<String> },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub enum DecisionView {
    Allowed,
    Denied,
    AutoApproved,
    /// Closed by a fact that did not say how.
    Dismissed,
}

/// Rows for the held items whose order falls in `range`: the terminal's
/// call, for what is on screen.
pub fn chat_rows(state: &SessionState, range: RangeInclusive<u64>, opts: &ChatOptions) -> Vec<Row> {
    if range.start() > range.end() {
        return Vec::new();
    }
    state
        .transcript()
        .range(range)
        .map(|held| row(state, held, opts))
        .collect()
}

/// Rows for these keys, in order, skipping keys not held: the phone's call,
/// for the cells an update changed.
pub fn chat_rows_for(state: &SessionState, keys: &[Key], opts: &ChatOptions) -> Vec<Row> {
    let transcript = state.transcript();
    let mut held: Vec<&Held> = keys.iter().filter_map(|key| transcript.get(key)).collect();
    held.sort_by_key(|held| held.item.order);
    held.dedup_by_key(|held| held.item.order);
    held.into_iter()
        .map(|held| row(state, held, opts))
        .collect()
}

fn row(state: &SessionState, held: &Held, opts: &ChatOptions) -> Row {
    let item = &held.item;
    let (kind, decision, failed) = kind_of(state, held);
    let run = crate::run::run_of(state, held, &kind, failed);
    let step = held.class.fold() == Fold::Step;
    let kept = run.as_ref().is_some_and(|run| run.unresolved_failure);
    let collapsed = matches!(kind, RowKind::Hidden)
        || match opts.tools {
            ToolRows::ShowAll => false,
            ToolRows::Hide => step && !kept,
            ToolRows::Collapse { open } => run.as_ref().is_some_and(|run| {
                let shown = if run.live {
                    run.shows_live()
                } else {
                    run.is_last() || kept
                };
                !shown && !crate::run::run_is_open(state, item.order, open)
            }),
        };
    let asked = asked(state, held);
    let parent = parent_of(held);
    let collapsed = collapsed || parent.is_some();
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
        ItemBody::ClaudePty(wire::claude_pty_item::Kind::Tool(tool))
        | ItemBody::ClaudeSdk(wire::claude_sdk_item::Kind::Tool(tool))
            if !tool.parent_key.is_empty() =>
        {
            Some(tool.parent_key.clone())
        }
        _ => None,
    }
}

/// What a step acted on: a redo of a failed step acts on the same.
pub(crate) fn subject_of(held: &Held) -> String {
    match &held.body {
        ItemBody::ClaudePty(wire::claude_pty_item::Kind::Tool(tool))
        | ItemBody::ClaudeSdk(wire::claude_sdk_item::Kind::Tool(tool)) => claude_subject(tool),
        ItemBody::Codex(wire::codex_item::Kind::Work(work)) => match &work.of {
            Some(wire::work::Of::Command(command)) => command.command.clone(),
            Some(wire::work::Of::WebSearch(search)) => search.query.clone(),
            _ => String::new(),
        },
        _ => String::new(),
    }
}

/// The call's phase from its state, its decision, and whether an open ask
/// points at it. A refusal decides it whatever state the call reports.
fn call_phase(state: i32, decision: Option<&Decision>, asked: bool) -> CallPhase {
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
        DecisionOutcome::None => return None,
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
            Some(wire::codex_grant::Of::CommandPrefix(prefix)) => {
                Some(PermissionGrant::CommandPrefix {
                    words: prefix.words.clone(),
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

fn duration(held: &Held, ended: Option<i64>) -> Option<i64> {
    ended.map(|ended| (ended - held.item.at_ms).max(0))
}

/// The row kind, the decision meta, and whether it failed.
pub(crate) fn kind_of(state: &SessionState, held: &Held) -> (RowKind, Option<Decision>, bool) {
    use wire::claude_pty_item::Kind as Pty;
    use wire::claude_sdk_item::Kind as Sdk;
    use wire::codex_item::Kind as Codex;
    let item = &held.item;
    let text = || segments(&item.text, &item.attachments);
    let plain = |kind| (kind, None, false);
    match &held.body {
        ItemBody::ClaudePty(kind) => match kind {
            Pty::Prompt(_) => plain(RowKind::Prompt {
                text: text(),
                steered: false,
            }),
            Pty::Steer(_) => plain(RowKind::Prompt {
                text: text(),
                steered: true,
            }),
            Pty::Message(m) => plain(prose(held, m.complete, false)),
            Pty::Thinking(t) => plain(thinking(state, held, t.complete)),
            Pty::Tool(tool) => claude_tool(state, held, tool),
            Pty::Turn(turn) => plain(turn_row(held, turn)),
            Pty::Compaction(c) => plain(compaction(c)),
            Pty::CompactSummary(_) | Pty::Interruption(_) => plain(RowKind::Hidden),
            Pty::AgentMessage(m) => plain(agent_message(held, m)),
            Pty::ApiError(e) => error(e),
            Pty::Boundary(b) => plain(boundary(b)),
            Pty::Slash(s) => plain(slash(held, s)),
            Pty::Unrecognized(u) => plain(unrecognized(u)),
            Pty::Ask(ask) => plain(ask_row(ask)),
            Pty::Plan(plan) => plain(plan_row(state, held, plan)),
        },
        ItemBody::ClaudeSdk(kind) => match kind {
            Sdk::Prompt(_) => plain(RowKind::Prompt {
                text: text(),
                steered: false,
            }),
            Sdk::Steer(_) => plain(RowKind::Prompt {
                text: text(),
                steered: true,
            }),
            Sdk::Message(m) => plain(prose(held, m.complete, false)),
            Sdk::Thinking(t) => plain(thinking(state, held, t.complete)),
            Sdk::Tool(tool) => claude_tool(state, held, tool),
            Sdk::Task(task) => plain(RowKind::Subagent {
                description: task.description.clone(),
                running: task.state() == wire::TaskState::Running,
                tool_count: task.tool_count,
                last_tool: task.last_tool.clone(),
                answer: String::new(),
                duration_ms: None,
            }),
            Sdk::Turn(turn) => plain(turn_row(held, turn)),
            Sdk::Status(_) => plain(RowKind::Hidden),
            Sdk::Boundary(b) => plain(boundary(b)),
            Sdk::AgentMessage(m) => plain(agent_message(held, m)),
            Sdk::ApiError(e) => error(e),
            Sdk::Slash(s) => plain(slash(held, s)),
            Sdk::Unrecognized(u) => plain(unrecognized(u)),
            Sdk::Plan(plan) => plain(plan_row(state, held, plan)),
            Sdk::ModelSwitch(m) => plain(model_switch(m)),
            Sdk::Compaction(c) => plain(compaction(c)),
            Sdk::Ask(ask) => plain(ask_row(ask)),
        },
        ItemBody::Codex(kind) => match kind {
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
            Codex::McpStartup(_) | Codex::TurnDiff(_) => plain(RowKind::Hidden),
            Codex::Turn(turn) => plain(turn_row(held, turn)),
            Codex::Boundary(b) => plain(boundary(b)),
            Codex::AgentMessage(m) => plain(agent_message(held, m)),
            Codex::Error(e) => error(e),
            Codex::Reroute(m) => plain(model_switch(m)),
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
        wire::PlanVerdict::Dismissed => PlanVerdict::Dismissed,
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

fn prose(held: &Held, complete: bool, working_note: bool) -> RowKind {
    RowKind::Prose {
        text: segments(&held.item.text, &held.item.attachments),
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
        ItemBody::ClaudeSdk(wire::claude_sdk_item::Kind::Status(_))
    )
}

fn turn_row(held: &Held, turn: &wire::Turn) -> RowKind {
    let duration_ms =
        (turn.started_at_ms > 0).then(|| (held.item.at_ms - turn.started_at_ms).max(0));
    match TurnOutcome::try_from(turn.outcome).unwrap_or(TurnOutcome::Unspecified) {
        TurnOutcome::Interrupted => RowKind::Stopped,
        TurnOutcome::Failed => RowKind::TurnEnd {
            duration_ms,
            cost_usd: turn.cost_usd,
            failed: true,
        },
        TurnOutcome::Completed | TurnOutcome::Unspecified => RowKind::TurnEnd {
            duration_ms,
            cost_usd: turn.cost_usd,
            failed: false,
        },
    }
}

fn compaction(c: &wire::Compaction) -> RowKind {
    RowKind::Compaction {
        tokens_before: c.tokens_before,
        tokens_after: c.tokens_after,
        automatic: c.automatic,
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

fn boundary(b: &wire::Boundary) -> RowKind {
    RowKind::Boundary {
        kind: b.kind(),
        cause: ui_state::exit_words(b.cause, b.cause_detail.as_deref()),
    }
}

fn agent_message(held: &Held, m: &wire::AgentMessage) -> RowKind {
    let from = match m.from.as_ref().and_then(|from| from.value.as_ref()) {
        Some(wire::sender::Value::Agent(agent)) => agent.name.clone(),
        Some(wire::sender::Value::Human(_)) | None => String::new(),
    };
    RowKind::AgentMessage {
        from,
        kind: m.kind(),
        text: held.item.text.clone(),
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
        from: m.from.clone(),
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

fn input(tool: &ToolCall) -> Value {
    serde_json::from_slice(&tool.input_json).unwrap_or(Value::Null)
}

fn field(value: &Value, name: &str) -> String {
    value
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn lines(text: &str) -> u32 {
    if text.is_empty() {
        0
    } else {
        text.lines().count() as u32
    }
}

/// What a Claude call is about, for its row and a run's summary.
fn claude_subject(tool: &ToolCall) -> String {
    let input = input(tool);
    for name in [
        "file_path",
        "pattern",
        "url",
        "query",
        "path",
        "command",
        "description",
        "notebook_path",
    ] {
        let value = field(&input, name);
        if !value.is_empty() {
            return value;
        }
    }
    String::new()
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
    let view = call_phase(tool.state, decision.as_ref(), asked(state, held));
    let failed = view == CallPhase::Failed;
    let input = input(tool);
    let subject = claude_subject(tool);
    let duration_ms = duration(held, tool.ended_at_ms);
    let kind = if let Some(image) = image_of(&tool.attachments) {
        RowKind::Image {
            image: Some(image),
            path: subject,
            generated: false,
        }
    } else if !tool.server.is_empty() {
        RowKind::ToolCall {
            server: tool.server.clone(),
            tool: tool.name.clone(),
            fact: first_fact(&input),
            state: view,
            result: tool.outcome_text.clone(),
        }
    } else if let Some(verb) = explore_verb(tool.class) {
        RowKind::Explore {
            verb,
            subject,
            state: view,
        }
    } else {
        match tool.name.as_str() {
            "Bash" if tool.background => RowKind::Background {
                command: field(&input, "command"),
                running: view.in_flight(),
                duration_ms,
            },
            "Bash" => {
                let (output_head, more_lines) = output_head(&tool.outcome_text);
                RowKind::Command {
                    command: field(&input, "command"),
                    state: view,
                    exit_code: tool.exit_code,
                    output_head,
                    more_lines,
                    output_trimmed: false,
                    output_tail: output_tail(&tool.outcome_text),
                    duration_ms,
                }
            }
            "Edit" | "MultiEdit" | "NotebookEdit" => {
                let (added, removed) = claude_counts(tool, &input);
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
            "Write" => {
                let content = lines(&field(&input, "content"));
                let (change, (added, removed)) = match tool.created {
                    Some(true) => (FileChangeView::Created { lines: content }, (content, 0)),
                    Some(false) => (FileChangeView::Edited, claude_counts(tool, &input)),
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
            "Agent" | "Task" => subagent(held, tool, &input, view),
            name if ui_state::is_task_tool(name) => RowKind::Hidden,
            name => RowKind::ToolCall {
                server: String::new(),
                tool: name.to_owned(),
                fact: first_fact(&input),
                state: view,
                result: tool.outcome_text.clone(),
            },
        }
    };
    (kind, decision, failed)
}

/// The first string argument, as the one fact a generic tool row shows.
fn first_fact(input: &Value) -> String {
    input
        .as_object()
        .and_then(|object| object.values().find_map(Value::as_str))
        .unwrap_or_default()
        .to_owned()
}

/// Added and removed lines: from the provider's patch when it gave one,
/// else from the replaced and replacing text.
fn claude_counts(tool: &ToolCall, input: &Value) -> (u32, u32) {
    let out: Value = serde_json::from_slice(&tool.outcome_json).unwrap_or(Value::Null);
    if let Some(hunks) = out.get("structuredPatch").and_then(Value::as_array) {
        let mut added = 0;
        let mut removed = 0;
        for line in hunks
            .iter()
            .filter_map(|hunk| hunk.get("lines")?.as_array())
            .flatten()
        {
            match line.as_str().and_then(|line| line.chars().next()) {
                Some('+') => added += 1,
                Some('-') => removed += 1,
                _ => {}
            }
        }
        return (added, removed);
    }
    let edits = input
        .get("edits")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| vec![input.clone()]);
    edits.iter().fold((0, 0), |(added, removed), edit| {
        (
            added + lines(&field(edit, "new_string")),
            removed + lines(&field(edit, "old_string")),
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

fn subagent(held: &Held, tool: &ToolCall, input: &Value, view: CallPhase) -> RowKind {
    let progress = tool.subagent.clone().unwrap_or_default();
    RowKind::Subagent {
        description: field(input, "description"),
        running: view.in_flight() || (tool.background && !progress.finished),
        tool_count: progress.tool_count,
        last_tool: progress.last_tool,
        answer: tool.outcome_text.clone(),
        duration_ms: duration(held, tool.ended_at_ms),
    }
}

fn codex_work(
    state: &SessionState,
    held: &Held,
    work: &wire::Work,
) -> (RowKind, Option<Decision>, bool) {
    use wire::work::Of;
    let decision = decision_view(work.decision.as_ref(), &state.agent_state().permissions);
    let view = call_phase(work.state, decision.as_ref(), asked(state, held));
    let failed = view == CallPhase::Failed;
    let kind = match (&work.of, explore_verb(work.class)) {
        (Some(Of::Command(command)), Some(verb)) => RowKind::Explore {
            verb,
            subject: command.command.clone(),
            state: view,
        },
        (of, _) => match of {
            Some(Of::Command(command)) if command.background => RowKind::Background {
                command: command.command.clone(),
                running: view.in_flight(),
                duration_ms: duration(held, work.ended_at_ms),
            },
            Some(Of::Command(command)) => {
                let (output_head, more_lines) = output_head(&held.item.text);
                RowKind::Command {
                    command: command.command.clone(),
                    state: view,
                    exit_code: command.exit_code,
                    output_head,
                    more_lines,
                    output_trimmed: command.output_dropped_bytes > 0,
                    output_tail: output_tail(&held.item.text),
                    duration_ms: duration(held, work.ended_at_ms),
                }
            }
            Some(Of::FileChange(change)) => RowKind::FileChange {
                files: change.changes.iter().map(codex_file).collect(),
                state: view,
            },
            Some(Of::Mcp(call)) => RowKind::ToolCall {
                server: call.server.clone(),
                tool: call.tool.clone(),
                fact: first_fact(
                    &serde_json::from_slice(&call.arguments_json).unwrap_or(Value::Null),
                ),
                state: view,
                result: if call.error.is_empty() {
                    String::from_utf8_lossy(&call.result_json).into_owned()
                } else {
                    call.error.clone()
                },
            },
            Some(Of::WebSearch(search)) => RowKind::Explore {
                verb: ExploreVerb::WebSearch,
                subject: search.query.clone(),
                state: view,
            },
            Some(Of::Image(image)) => RowKind::Image {
                image: image_of(&held.item.attachments),
                path: image.path.clone(),
                generated: image.generated,
            },
            Some(Of::Collab(collab)) => RowKind::Subagent {
                description: collab.prompt.clone(),
                running: view.in_flight(),
                tool_count: 0,
                last_tool: collab.tool.clone(),
                answer: String::new(),
                duration_ms: duration(held, work.ended_at_ms),
            },
            None => RowKind::Unrecognized {
                what: "work".into(),
                summary: String::new(),
            },
        },
    };
    (kind, decision, failed)
}

fn codex_file(change: &wire::FileChange) -> FileRow {
    let (added, removed) = counts(&change_lines(change));
    let change_view =
        match FileChangeKind::try_from(change.kind).unwrap_or(FileChangeKind::Unspecified) {
            FileChangeKind::Add => FileChangeView::Created { lines: added },
            FileChangeKind::Delete => FileChangeView::Deleted,
            _ if !change.move_to.is_empty() => FileChangeView::Moved {
                to: change.move_to.clone(),
            },
            _ => FileChangeView::Edited,
        };
    FileRow {
        path: change.path.clone(),
        change: change_view,
        added,
        removed,
        line: change.line,
    }
}

/// A Codex file change's patch lines. Codex gives a file it adds or
/// deletes as the file's text, not as a diff, so each of its lines is added
/// or removed whole.
pub(crate) fn change_lines(change: &wire::FileChange) -> Vec<EditLine> {
    let whole = |kind| {
        change
            .patch
            .lines()
            .enumerate()
            .map(|(i, text)| {
                EditLine::Line(PatchLine {
                    number: Some(i as u32 + 1),
                    kind,
                    text: text.to_owned(),
                })
            })
            .collect()
    };
    let diff = change.patch.lines().any(|line| line.starts_with("@@"));
    match FileChangeKind::try_from(change.kind).unwrap_or(FileChangeKind::Unspecified) {
        FileChangeKind::Add if !diff => whole(LineKind::Added),
        FileChangeKind::Delete if !diff => whole(LineKind::Removed),
        _ => unified_patch(&change.patch),
    }
}

/// Added and removed lines among parsed patch lines.
pub(crate) fn counts(lines: &[EditLine]) -> (u32, u32) {
    lines
        .iter()
        .fold((0, 0), |(added, removed), line| match line {
            EditLine::Line(PatchLine {
                kind: LineKind::Added,
                ..
            }) => (added + 1, removed),
            EditLine::Line(PatchLine {
                kind: LineKind::Removed,
                ..
            }) => (added, removed + 1),
            _ => (added, removed),
        })
}

/// One line of a patch, numbered on the side it belongs to (the new file
/// for context and added lines, the old for removed ones) when the provider
/// said where its hunk starts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct PatchLine {
    pub number: Option<u32>,
    pub kind: LineKind,
    pub text: String,
}

/// One line of an edit's patch as an ask carries it: where a hunk starts,
/// by its first line in the old file and the new, or a line of the patch.
/// A file's own header lines are not carried. Each client decides whether
/// to draw a hunk's start.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub enum EditLine {
    Hunk { old_start: u32, new_start: u32 },
    Line(PatchLine),
}

/// The head of a landed file change: its first file's first lines, and
/// how many lines of that file's patch follow them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct PatchHead {
    pub lines: Vec<PatchLine>,
    pub more: usize,
}

/// The first `max` lines of the patch a landed file change made, for a
/// chat to show under its row; None for a row that changed no file, has
/// not landed, or carries no patch.
pub fn patch_head(state: &SessionState, key: &Key, max: usize) -> Option<PatchHead> {
    let held = state.transcript().get(key)?;
    let lines = match &held.body {
        ItemBody::ClaudePty(wire::claude_pty_item::Kind::Tool(tool))
        | ItemBody::ClaudeSdk(wire::claude_sdk_item::Kind::Tool(tool)) => {
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
                Some(wire::work::Of::FileChange(change)) => {
                    patch_lines(change_lines(change.changes.first()?))
                }
                _ => return None,
            }
        }
        _ => return None,
    };
    if lines.is_empty() {
        return None;
    }
    let more = lines.len().saturating_sub(max);
    Some(PatchHead {
        lines: lines.into_iter().take(max).collect(),
        more,
    })
}

/// The lines of a parsed patch, without where its hunks start.
fn patch_lines(lines: Vec<EditLine>) -> Vec<PatchLine> {
    lines
        .into_iter()
        .filter_map(|line| match line {
            EditLine::Line(line) => Some(line),
            EditLine::Hunk { .. } => None,
        })
        .collect()
}

fn claude_patch(tool: &ToolCall) -> Option<Vec<PatchLine>> {
    if !matches!(
        tool.name.as_str(),
        "Edit" | "MultiEdit" | "NotebookEdit" | "Write"
    ) {
        return None;
    }
    let out: Value = serde_json::from_slice(&tool.outcome_json).unwrap_or(Value::Null);
    if let Some(hunks) = out.get("structuredPatch").and_then(Value::as_array)
        && !hunks.is_empty()
    {
        let mut lines = Vec::new();
        for hunk in hunks {
            let start = |name: &str| hunk.get(name).and_then(Value::as_u64).map(|n| n as u32);
            let (mut old, mut new) = (start("oldStart"), start("newStart"));
            for line in hunk
                .get("lines")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                lines.push(numbered(line, &mut old, &mut new));
            }
        }
        return Some(lines);
    }
    let input = input(tool);
    if tool.name == "Write" {
        return Some(
            field(&input, "content")
                .lines()
                .enumerate()
                .map(|(i, text)| PatchLine {
                    number: Some(i as u32 + 1),
                    kind: LineKind::Added,
                    text: text.to_owned(),
                })
                .collect(),
        );
    }
    Some(patch_lines(replaced_lines(&input)))
}

/// What a Claude edit replaces and writes, as unnumbered patch lines: each
/// edit's old text removed, then its new text (or a write's content)
/// added. There is no file to number them against.
pub(crate) fn replaced_lines(input: &Value) -> Vec<EditLine> {
    let edits = input
        .get("edits")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| vec![input.clone()]);
    let mut lines = Vec::new();
    for edit in &edits {
        for (name, kind) in [
            ("old_string", LineKind::Removed),
            ("new_string", LineKind::Added),
            ("content", LineKind::Added),
        ] {
            lines.extend(field(edit, name).lines().map(|text| {
                EditLine::Line(PatchLine {
                    number: None,
                    kind,
                    text: text.to_owned(),
                })
            }));
        }
    }
    lines
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

/// A unified diff's lines, numbered from its hunk headers, with where each
/// hunk starts. A file's header lines ("--- ", "+++ ", "index " and the
/// rest) come only outside a hunk: inside one, the header's counts say how
/// many lines are the hunk's, so a removed "-- comment" or an added "++i"
/// is a line of the patch. A hunk header without counts runs to the next
/// header.
pub(crate) fn unified_patch(patch: &str) -> Vec<EditLine> {
    let mut lines = Vec::new();
    let (mut old, mut new) = (None, None);
    // Lines each side of the open hunk still holds, from its header.
    let (mut old_left, mut new_left) = (0u32, 0u32);
    let mut uncounted = false;
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
            lines.push(EditLine::Line(numbered(line, &mut old, &mut new)));
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
            if let (Some((old_start, _)), Some((new_start, _))) = (from, to) {
                lines.push(EditLine::Hunk {
                    old_start,
                    new_start,
                });
            }
            continue;
        }
        if line.starts_with("diff --git") {
            uncounted = false;
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
        lines.push(EditLine::Line(numbered(line, &mut old, &mut new)));
    }
    lines
}

/// The new file's line the patch's first change lands on: an added line's
/// own, or for a removal the new-side line where it was, counting from its
/// hunk's start past the context before it.
pub(crate) fn first_change(lines: &[EditLine]) -> Option<u32> {
    let mut at = None;
    for line in lines {
        match line {
            EditLine::Hunk { new_start, .. } => at = Some(*new_start),
            EditLine::Line(line) => match line.kind {
                LineKind::Added => return line.number.or(at).map(|n| n.max(1)),
                LineKind::Removed => return at.map(|n| n.max(1)),
                LineKind::Context => at = line.number.map(|n| n + 1),
            },
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(number: u32, kind: LineKind, text: &str) -> EditLine {
        EditLine::Line(PatchLine {
            number: Some(number),
            kind,
            text: text.to_owned(),
        })
    }

    /// Inside a hunk its header's counts say which lines are the patch's,
    /// so a removed SQL comment and an added increment are lines of it, not
    /// a file's header lines; the file's own headers before it are dropped.
    #[test]
    fn a_patch_keeps_lines_that_look_like_file_headers_inside_its_hunks() {
        let patch = "--- a/q.sql\n+++ b/q.sql\n@@ -3,3 +3,3 @@\n select 1;\n--- old note\n+++i;\n select 2;\n";
        let lines = unified_patch(patch);
        assert_eq!(
            lines,
            vec![
                EditLine::Hunk {
                    old_start: 3,
                    new_start: 3
                },
                line(3, LineKind::Context, "select 1;"),
                line(4, LineKind::Removed, "-- old note"),
                line(4, LineKind::Added, "++i;"),
                line(5, LineKind::Context, "select 2;"),
            ]
        );
        assert_eq!(counts(&lines), (1, 1));
        assert_eq!(first_change(&lines), Some(4), "past the hunk's context");
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
