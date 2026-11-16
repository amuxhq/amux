//! Child threads: the provider subagents Codex runs for this agent.
//!
//! Codex's `spawn_agent` starts a subagent in a child thread on the
//! agent's own server. Codex 0.166.0 reports the spawn, and each follow-up
//! the parent sends it, as a `subAgentActivity` item on the parent's
//! thread, and the child's own turns, items and asks arrive on the same
//! connection naming the child's thread; no child sends `thread/started`.
//! Each child thread is an amux agent of its own (see [`crate::subagent`]):
//! the parent's chat draws the spawn as one collab row naming the child's
//! agent, and the child's chat draws what the child does.
//!
//! The interpreter keeps a copy of the per-thread item state for each
//! child and reads a child's notifications with that copy swapped in
//! ([`State::as_child`]), so one set of item rules serves every thread and
//! what they write goes to that child's part of the step.
//!
//! A child ends at its own `turn/completed` (an interrupted one gets no
//! `subAgentActivity completed`), unless commands it started still run in
//! the background: an interrupt leaves them running, as it does the
//! agent's own, so the child stays, listing them, until the last ends.
//! A follow-up starts it again under the same agent.
//!
//! A subagent's row takes its title from its start alone, and the title is
//! Codex's nickname for the child, which only `thread/read` gives. So the
//! start waits for the read's answer, and whatever Codex says of the child
//! meanwhile waits with it, to be read once the start is out. The answer
//! comes within milliseconds, before the child's first word. If it does
//! not come within [`HOLD_MS`], or the parent's turn ends first (a host
//! that leaves at its turn end must have journaled its subagents), the
//! child starts without it, titled by the task name its parent gave it
//! when Codex says one. A replay of a recording made before amux read
//! children is one such case.
//!
//! Codex 0.166.0 never shows a client what a parent asked its child: the
//! `spawn_agent` and `followup_task` message, and the child's copy of it,
//! reach the client only encrypted. A child's chat opens with its prompt
//! only where Codex says it, as an older Codex's collab call does.

use std::collections::{BTreeMap, BTreeSet};

use codex_protocol::client::{
    InjectItemsParams, InjectedContent, InjectedItem, InjectedMessage, ThreadReadParams,
    TurnInterruptParams, TurnSteerParams,
};
use codex_protocol::items::{
    CollabAgentTool, CollabAgentToolCallItem, SubAgentActivityItem, SubAgentActivityKind,
    TextContent, ThreadItem, UserInput,
};
use codex_protocol::server::ThreadResponse;
use codex_protocol::{ClientRequest, ServerNotification};
use prost::Message as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use wire::{
    ApiError, BackgroundJob, CodexSnapshot, ExitCause, IncarnationEnd, Input, KindInput,
    NativeStart, Refusal, Snapshot, codex_input,
};

use super::facts::{error_kind, error_message};
use super::{Request, State, Streamed, WorkState, item_body};
use crate::{Channel, Effect, Emit, Fact, ItemDraft, serde_pb};

/// What the interpreter keeps of one child thread, from the first word of
/// it until the agent leaves it: past its end, so a follow-up continues the
/// same subagent from the call that started it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(super) struct Child {
    /// The child thread whose chat holds the call that started this one,
    /// for a child's own child; None when the agent's thread started it.
    pub(super) parent: Option<String>,
    /// That call's item key. A follow-up keeps the spawn's.
    pub(super) call_key: String,
    /// Codex's nickname for it, from `thread/read`; empty until that
    /// answers, and its row takes it from the next snapshot.
    pub(super) title: String,
    /// `thread/read` was asked of it.
    pub(super) read: bool,
    /// It was started or continued and its start waits for its title (see
    /// the module's notes); it is live meanwhile.
    #[serde(default)]
    pub(super) pending: bool,
    /// What Codex said of it while its start waited, each line as it came,
    /// to be read once the start is out.
    #[serde(default)]
    pub(super) held: Vec<String>,
    /// The task name its parent gave it, from its place in Codex's team
    /// (`/root/<name>`): its title when the nickname does not come.
    #[serde(default)]
    pub(super) task: String,
    /// What its parent asked it, where Codex says, and the key of the item
    /// that shows it: its chat's first item once it starts.
    #[serde(default)]
    pub(super) prompt: Option<(String, String)>,
    /// The text of the prompt its call drew, until Codex echoes it as the
    /// child's own message, which then draws nothing more.
    #[serde(default)]
    pub(super) prompt_drawn: Option<String>,
    pub(super) model: Option<String>,
    pub(super) effort: Option<String>,
    /// When it last started or was continued: when its own job began.
    pub(super) started_at_ms: i64,
    /// The turn it runs now.
    pub(super) turn: Option<String>,
    /// How its last turn ended, while commands that turn started run on in
    /// the background: it ends so once they have.
    #[serde(with = "serde_pb::opt_msg")]
    pub(super) over: Option<IncarnationEnd>,
    /// What it said it works on, with amux's `status` tool on its thread.
    pub(super) working_on: Option<String>,
    /// The line its row quotes from its newest answer.
    pub(super) said: Option<String>,
    /// The person stopped it, and it has not ended yet.
    pub(super) stopping: bool,
    /// Its parent was told of the stop.
    pub(super) told: bool,
    /// Why Codex refused the last stop of one of its commands.
    pub(super) stop_error: String,

    /// The commands it runs in the background, as Codex lists them on its
    /// thread.
    #[serde(with = "serde_pb::msgs")]
    pub(super) jobs: Vec<BackgroundJob>,
    /// The id its newest list of commands goes by, each page's id this and
    /// the page's number.
    #[serde(default)]
    pub(super) jobs_list: String,
    /// Its item state while another thread's is read; see
    /// [`State::as_child`].
    pub(super) items: ThreadItems,
}

/// How long a child's start waits for its nickname, from its spawn: well
/// past the few milliseconds Codex takes to answer, and short enough that a
/// child's chat is not seen to lag when it does not.
pub(super) const HOLD_MS: i64 = 3_000;

/// One thread's item state: its units of work, its streaming texts, its
/// amux sends, and the commands it left running.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(super) struct ThreadItems {
    works: BTreeMap<String, WorkState>,
    streamed: BTreeMap<String, Streamed>,
    sent: BTreeMap<String, i64>,
    background: Option<BTreeSet<String>>,
}

/// How a child's turn ends it: completed is finished; interrupted, by the
/// person here or by another client, is stopped; one that failed finished
/// in Codex's words.
fn child_end(turn: &codex_protocol::Turn) -> IncarnationEnd {
    match (turn.status.as_str(), &turn.error) {
        ("interrupted", _) => IncarnationEnd {
            cause: ExitCause::Stopped as i32,
            detail: None,
        },
        (_, Some(error)) => IncarnationEnd {
            cause: ExitCause::Finished as i32,
            detail: Some(error_message(error)),
        },
        _ => IncarnationEnd {
            cause: ExitCause::Finished as i32,
            detail: None,
        },
    }
}

impl State {
    /// Runs `read` as the child thread `thread`: its item state in the
    /// fields the item rules read, and every item, ask and job written going
    /// to its subagent. A child's own child may start while the child is
    /// read: the swaps nest, each putting back what it took.
    pub(super) fn as_child<R>(&mut self, thread: &str, read: impl FnOnce(&mut Self) -> R) -> R {
        debug_assert!(
            self.speaking.as_deref() != Some(thread),
            "a thread is read as itself once"
        );
        let mut items = self
            .children
            .get_mut(thread)
            .map(|child| std::mem::take(&mut child.items))
            .unwrap_or_default();
        self.swap_items(&mut items);
        let speaking = self.speaking.replace(thread.to_owned());
        let answer = read(self);
        self.speaking = speaking;
        self.swap_items(&mut items);
        if let Some(child) = self.children.get_mut(thread) {
            child.items = items;
        }
        answer
    }

    fn swap_items(&mut self, items: &mut ThreadItems) {
        std::mem::swap(&mut self.works, &mut items.works);
        std::mem::swap(&mut self.streamed, &mut items.streamed);
        std::mem::swap(&mut self.sent, &mut items.sent);
        std::mem::swap(&mut self.background, &mut items.background);
    }

    fn child_running(&self, thread: &str) -> bool {
        self.shared.running_subagents().any(|child| child == thread)
    }

    /// Running, or started with its start held for its title.
    fn child_live(&self, thread: &str) -> bool {
        self.child_running(thread) || self.children.get(thread).is_some_and(|child| child.pending)
    }

    /// Writes the held start of child `thread`, if one is held: before
    /// anything else of the subagent's part, whose row comes from it. Its
    /// prompt is its first item, and what Codex said of it meanwhile is
    /// read after, in the order it came.
    pub(super) fn begin_child(&mut self, emit: &mut Emit, thread: &str) {
        let Some(child) = self.children.get_mut(thread).filter(|child| child.pending) else {
            return;
        };
        child.pending = false;
        let held = std::mem::take(&mut child.held);
        let prompt = child.prompt.take();
        let title = if child.title.is_empty() {
            child.task.clone()
        } else {
            child.title.clone()
        };
        let call_agent_id = match &child.parent {
            Some(parent) => self.shared.subagent_id(parent),
            None => self.shared.agent_id().to_vec(),
        };
        let start = NativeStart {
            call_agent_id,
            call_key: child.call_key.clone(),
            title,
            ..NativeStart::default()
        };
        self.shared.subagent(emit, thread).start(start);
        if let Some((key, text)) = prompt {
            if let Some(child) = self.children.get_mut(thread) {
                child.prompt_drawn = Some(text.clone());
            }
            self.as_child(thread, |state| {
                state.emit_item(
                    emit,
                    ItemDraft {
                        key,
                        text,
                        body: item_body(wire::codex_item::Kind::Prompt(wire::Prompt {})),
                        complete: true,
                        ..Default::default()
                    },
                )
            });
            self.settle_child(emit, thread);
        }
        for line in held {
            self.fact(
                emit,
                Fact {
                    channel: Channel::Rpc,
                    payload: line.into_bytes(),
                },
            );
        }
    }

    /// Whether what Codex said (`payload`, about `thread`) waits with the
    /// child's held start, which it then joins. Past [`HOLD_MS`] the start
    /// stops waiting and goes out, and this is read now.
    pub(super) fn hold(&mut self, emit: &mut Emit, thread: &str, payload: &[u8]) -> bool {
        let now = self.shared.now_ms();
        let Some(child) = self.children.get_mut(thread).filter(|child| child.pending) else {
            return false;
        };
        if now < child.started_at_ms + HOLD_MS
            && let Ok(line) = std::str::from_utf8(payload)
        {
            child.held.push(line.to_owned());
            return true;
        }
        self.begin_child(emit, thread);
        false
    }

    /// Writes each start that has waited [`HOLD_MS`] for its nickname.
    pub(super) fn begin_overdue(&mut self, emit: &mut Emit) {
        let now = self.shared.now_ms();
        let overdue: Vec<String> = self
            .children
            .iter()
            .filter(|(_, child)| child.pending && now >= child.started_at_ms + HOLD_MS)
            .map(|(thread, _)| thread.clone())
            .collect();
        for thread in overdue {
            self.begin_child(emit, &thread);
        }
    }

    /// Writes every held start: the parent's turn is ending, and a host
    /// that leaves at its turn end must have journaled its subagents' rows.
    pub(super) fn begin_children(&mut self, emit: &mut Emit) {
        let held: Vec<String> = self
            .children
            .iter()
            .filter(|(_, child)| child.pending)
            .map(|(thread, _)| thread.clone())
            .collect();
        for thread in held {
            self.begin_child(emit, &thread);
        }
    }

    /// The child thread `thread` was heard from: whether it runs, starting
    /// it when what was heard is the start of its work (`activity`: a turn,
    /// an item or a request). A thread first heard from without a call
    /// naming it gets a subagent whose call is unknown; one heard from
    /// again after its end was continued. A thread the agent left is no
    /// longer its business.
    pub(super) fn child_heard(&mut self, emit: &mut Emit, thread: &str, activity: bool) -> bool {
        if self.left.contains(thread) {
            return false;
        }
        if self.child_live(thread) {
            return true;
        }
        if !activity {
            return false;
        }
        self.start_child(emit, thread, None, String::new(), None);
        true
    }

    /// The subagent of child thread `thread` starts, or is continued, asked
    /// `prompt` (its item's key and text) where Codex says what. Its
    /// nickname is asked for once, and its first start is held until the
    /// answer (see the module's notes).
    fn start_child(
        &mut self,
        emit: &mut Emit,
        thread: &str,
        parent: Option<String>,
        call_key: String,
        prompt: Option<(String, String)>,
    ) {
        let now = self.shared.now_ms();
        let child = self
            .children
            .entry(thread.to_owned())
            .or_insert_with(|| Child {
                parent,
                ..Child::default()
            });
        if child.call_key.is_empty() {
            child.call_key = call_key;
        }
        child.started_at_ms = now;
        child.over = None;
        child.stopping = false;
        child.told = false;
        child.stop_error.clear();
        child.pending = true;
        child.prompt = prompt;
        // A continued subagent keeps the title its first start gave it, so
        // its start has nothing to wait for.
        if std::mem::replace(&mut child.read, true) {
            return self.begin_child(emit, thread);
        }
        let id = self.child_request_id("read");
        let bytes = self.request_as(
            id,
            ClientRequest::ThreadRead(ThreadReadParams {
                thread_id: thread.to_owned(),
                include_turns: Some(false),
                extra: Default::default(),
            }),
            Request::ChildRead {
                thread: thread.to_owned(),
            },
        );
        emit.effect(Effect::ProviderWrite(bytes));
    }

    /// `thread/read` on a child answered: its nickname is its title, and
    /// its held start goes out with it. A refused read starts it untitled.
    pub(super) fn child_read(&mut self, emit: &mut Emit, thread: &str, result: Option<Value>) {
        let nickname = result
            .as_ref()
            .and_then(|result| codex_protocol::result::<ThreadResponse>(result).ok())
            .and_then(|response| response.thread.agent_nickname)
            .filter(|nickname| !nickname.is_empty());
        if let (Some(nickname), Some(child)) = (nickname, self.children.get_mut(thread)) {
            child.title = nickname;
        }
        self.begin_child(emit, thread);
    }

    /// The subagent of the thread being read answered: the line its row
    /// quotes.
    pub(super) fn child_said(
        &mut self,
        thread: &str,
        text: &str,
        attachments: &[wire::Attachment],
    ) {
        if let (Some(said), Some(child)) = (
            preview::said_line(text, attachments),
            self.children.get_mut(thread),
        ) {
            child.said = Some(said);
        }
    }

    // --- the parent's side ---------------------------------------------

    /// Codex 0.166.0's word on the parent's thread (the agent's, or a
    /// child's for a child's child) that it started a subagent or sent it a
    /// follow-up: the subagent starts or is continued, and the call row
    /// that opens its chat is drawn in the parent's. Its completion says
    /// nothing a row needs: the subagent's own turn end ends it.
    pub(super) fn subagent_activity(
        &mut self,
        emit: &mut Emit,
        activity: &SubAgentActivityItem,
        turn: &str,
        at_ms: Option<i64>,
        ended_at_ms: Option<i64>,
        completed: bool,
    ) {
        if !matches!(
            activity.kind,
            SubAgentActivityKind::Started | SubAgentActivityKind::Interacted
        ) {
            return;
        }
        let thread = activity.agent_thread_id.clone();
        if thread.is_empty() {
            return;
        }
        let id = activity.id.as_str();
        // `/root/<task>`, a level deeper under a subagent parent.
        let task = activity
            .agent_path
            .as_deref()
            .and_then(|path| path.strip_prefix("/root/"))
            .and_then(|path| path.rsplit('/').next())
            .filter(|task| !task.is_empty());
        // Its start or follow-up begins at the item's start. The item's
        // completion can come after the child's turn is over, so it starts
        // only a child it is the first word of.
        let begins = !completed
            || (activity.kind == SubAgentActivityKind::Started
                && !self.children.contains_key(&thread));
        if begins && !self.child_live(&thread) && !self.left.contains(&thread) {
            let parent = self.speaking.clone();
            self.start_child(emit, &thread, parent, id.to_owned(), None);
        }
        // Read when the start goes out: it waits for the nickname first.
        if let (Some(task), Some(child)) = (task, self.children.get_mut(&thread)) {
            child.task = task.to_owned();
        }
        if let Some(child) = self.children.get_mut(&thread) {
            let named = |field: &str| {
                activity
                    .extra
                    .get(field)
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                    .map(str::to_owned)
            };
            if let Some(model) = named("model") {
                child.model = Some(model);
            }
            if let Some(effort) = named("reasoningEffort") {
                child.effort = Some(effort);
            }
        }
        self.work_item(
            emit,
            id,
            &ThreadItem::SubAgentActivity(activity.clone()),
            turn,
            at_ms,
            ended_at_ms,
            completed,
        );
    }

    /// An older Codex's collab call naming the child threads it spawned or
    /// sent a follow-up: each starts or is continued, its call this one.
    /// A spawn may name its threads only at its completion; a follow-up
    /// continues one at its start, so a child that ended between the two
    /// is not brought back.
    pub(super) fn collab_call(
        &mut self,
        emit: &mut Emit,
        id: &str,
        call: &CollabAgentToolCallItem,
        completed: bool,
    ) {
        let spawns = call.tool == CollabAgentTool::SpawnAgent;
        let continues = matches!(
            call.tool,
            CollabAgentTool::SendInput
                | CollabAgentTool::ResumeAgent
                | CollabAgentTool::FollowupTask
                | CollabAgentTool::SendMessage
        );
        for thread in &call.receiver_thread_ids {
            let known = self.children.contains_key(thread);
            let start = !thread.is_empty()
                && !self.left.contains(thread)
                && !self.child_live(thread)
                && ((spawns && !known) || (continues && !completed));
            if start {
                let parent = self.speaking.clone();
                let prompt = call
                    .prompt
                    .clone()
                    .filter(|prompt| !prompt.is_empty())
                    .map(|prompt| (format!("prompt:{id}"), prompt));
                self.start_child(emit, thread, parent, id.to_owned(), prompt);
            }
        }
    }

    // --- the child's side ----------------------------------------------

    /// A notification about child thread `thread`: its turns, and what its
    /// conversation says, read as the agent's own are. Its settings, token
    /// use, tool servers and status are the agent's process's business and
    /// draw nothing on the subagent.
    pub(super) fn child_notification(
        &mut self,
        emit: &mut Emit,
        thread: &str,
        notification: ServerNotification,
    ) {
        let activity = match &notification {
            ServerNotification::TurnStarted(_) | ServerNotification::ItemStarted(_) => true,
            ServerNotification::TurnCompleted(_)
            | ServerNotification::ItemCompleted(_)
            | ServerNotification::AgentMessageDelta(_)
            | ServerNotification::PlanDelta(_)
            | ServerNotification::ReasoningTextDelta(_)
            | ServerNotification::CommandOutputDelta(_)
            | ServerNotification::ReasoningSummaryPartAdded(_)
            | ServerNotification::ReasoningSummaryTextDelta(_)
            | ServerNotification::Error(_)
            | ServerNotification::AutoApprovalReviewStarted(_)
            | ServerNotification::AutoApprovalReviewCompleted(_) => false,
            _ => return,
        };
        if !self.child_heard(emit, thread, activity) {
            return;
        }
        // A turn starting writes nothing of its own; anything else may.
        if !matches!(notification, ServerNotification::TurnStarted(_)) {
            self.begin_child(emit, thread);
        }
        match notification {
            ServerNotification::TurnStarted(started) => {
                let turn = Some(started.turn.id.clone()).filter(|turn| !turn.is_empty());
                if let Some(child) = self.children.get_mut(thread) {
                    child.turn.clone_from(&turn);
                    child.over = None;
                    // Stopped before its turn had an id: the interrupt
                    // waited for this.
                    if child.stopping
                        && let Some(turn) = turn
                    {
                        self.interrupt_child(emit, thread, turn);
                    }
                }
            }
            ServerNotification::TurnCompleted(completed) => {
                self.child_turn_completed(emit, thread, &completed.turn)
            }
            notification => self.as_child(thread, |state| {
                state.thread_notification(emit, notification)
            }),
        }
        self.settle_child(emit, thread);
    }

    /// A child's turn ended. Its asks close with it (the work they held
    /// never ran), what streamed is finished as it stands, and a command
    /// still running carries on in the background, listed on the child.
    fn child_turn_completed(&mut self, emit: &mut Emit, thread: &str, turn: &codex_protocol::Turn) {
        let end = child_end(turn);
        let carries_on = matches!(turn.status.as_str(), "completed" | "interrupted");
        self.as_child(thread, |state| {
            if let Some(error) = &turn.error {
                state.error_item(
                    emit,
                    format!("error:{}", turn.id),
                    ApiError {
                        error_kind: error_kind(error),
                        message: error_message(error),
                        ..Default::default()
                    },
                );
            }
            let mut unapproved = BTreeSet::new();
            for ask in state.close_all_asks(emit) {
                unapproved.insert(ask.item_key.clone());
                let meta = state.asks.remove(&ask.key);
                state.dismiss(emit, &ask, meta);
            }
            state.settle_open(emit, carries_on, &unapproved);
            if state.background.as_ref().is_some_and(|set| !set.is_empty()) {
                state.list_jobs(emit, 1, None, Vec::new());
            } else {
                state.set_jobs(Vec::new());
            }
        });
        if let Some(child) = self.children.get_mut(thread) {
            child.turn = None;
            child.over = Some(end);
        }
    }

    /// Ends the child's subagent once its turn is over and nothing it
    /// started runs on.
    fn settle_child(&mut self, emit: &mut Emit, thread: &str) {
        if !self.child_running(thread) {
            return;
        }
        let Some(child) = self.children.get(thread) else {
            return;
        };
        let running_on = child
            .items
            .background
            .as_ref()
            .is_some_and(|set| !set.is_empty());
        if child.turn.is_some() || running_on {
            return;
        }
        let Some(end) = child.over.clone() else {
            return;
        };
        // Stopped with nothing left running, it ends before its command
        // list answers: its parent is told now, as that answer would have.
        if let Some(child) = self.children.get_mut(thread)
            && child.stopping
            && !std::mem::replace(&mut child.told, true)
        {
            self.tell_parent(emit, thread, true);
        }
        self.end_child(emit, thread, end);
    }

    /// The child's subagent ends for `end`: what it holds open is closed as
    /// it stands, its last snapshot lists nothing running, and the end is
    /// its part's last word.
    fn end_child(&mut self, emit: &mut Emit, thread: &str, end: IncarnationEnd) {
        self.begin_child(emit, thread);
        // What its held start held may have ended it already.
        if !self.child_running(thread) {
            return;
        }
        self.as_child(thread, |state| {
            for ask in state.close_all_asks(emit) {
                let meta = state.asks.remove(&ask.key);
                state.dismiss(emit, &ask, meta);
            }
            state.settle_open(emit, false, &BTreeSet::new());
        });
        if let Some(child) = self.children.get_mut(thread) {
            child.turn = None;
            child.over = None;
            child.stopping = false;
            child.told = false;
            child.stop_error.clear();
            child.jobs.clear();
            child.items = ThreadItems::default();
        }
        self.child_snapshot(emit, thread, true);
        self.shared.subagent(emit, thread).end(end);
    }

    /// Every running subagent ends for `end`: the provider process that ran
    /// them is going.
    pub(super) fn end_children(&mut self, emit: &mut Emit, end: &IncarnationEnd) {
        let live: Vec<String> = self
            .children
            .keys()
            .filter(|thread| self.child_live(thread))
            .cloned()
            .collect();
        for thread in live {
            self.end_child(emit, &thread, end.clone());
        }
    }

    /// The agent leaves its children's threads with its own (`/clear`):
    /// nothing more will be heard of them, so their subagents end.
    pub(super) fn leave_children(&mut self, emit: &mut Emit) {
        self.end_children(
            emit,
            &IncarnationEnd {
                cause: ExitCause::Stopped as i32,
                detail: Some("the agent's conversation was cleared".to_owned()),
            },
        );
    }

    /// A new incarnation begins: an earlier one's process that went without
    /// a word took its subagents with it, and their rows were closed with
    /// it. What this state still holds of them running is let go, without
    /// a part, as nothing is left to say of them.
    pub(super) fn children_went(&mut self) {
        for child in self.children.values_mut() {
            child.pending = false;
            child.held.clear();
            child.prompt = None;
        }
        let running: Vec<String> = self.shared.running_subagents().map(str::to_owned).collect();
        if running.is_empty() {
            return;
        }
        let mut gone = Emit::default();
        for thread in &running {
            self.end_child(
                &mut gone,
                thread,
                IncarnationEnd {
                    cause: ExitCause::EndedUnexpectedly as i32,
                    detail: None,
                },
            );
        }
    }

    // --- snapshots -----------------------------------------------------

    /// Every running subagent's snapshot, carried by the step when it
    /// changed.
    pub(super) fn child_snapshots(&mut self, emit: &mut Emit) {
        let running: Vec<String> = self.shared.running_subagents().map(str::to_owned).collect();
        for thread in running {
            self.child_snapshot(emit, &thread, false);
        }
    }

    /// A subagent's snapshot: its asks, what it works on, and while it runs
    /// its own job, which its Stop names, beside the commands it left
    /// running. Its title is its start's alone.
    fn child_snapshot(&mut self, emit: &mut Emit, thread: &str, ending: bool) {
        let Some(child) = self.children.get(thread) else {
            return;
        };
        let mut jobs = Vec::new();
        if !ending {
            jobs.push(BackgroundJob {
                item_key: String::new(),
                command: if child.title.is_empty() {
                    "subagent".to_owned()
                } else {
                    child.title.clone()
                },
                started_at_ms: child.started_at_ms,
                kind: wire::BackgroundKind::Subagent as i32,
                key: thread.to_owned(),
                stopping: child.stopping,
                stop_error: child.stop_error.clone(),
            });
            jobs.extend(child.jobs.iter().cloned());
        }
        let model = child.model.clone();
        let effort = child.effort.clone();
        let working_on = child.working_on.clone();
        let said = child.said.clone();
        let mut sub = self.shared.subagent(emit, thread);
        let asks = sub.asks().open_asks().to_vec();
        let notice = asks
            .first()
            .map(|head| crate::notice::codex_ask(head, asks.len() as u32 - 1, ""));
        let body = CodexSnapshot {
            asks,
            model,
            effort,
            thread_id: Some(thread.to_owned()),
            background_jobs: Some(wire::BackgroundJobs {
                known: true,
                jobs: jobs.clone(),
            }),
            ..crate::unknown::codex()
        };
        let phase = sub.phase() as i32;
        sub.snapshot(Snapshot {
            phase,
            body: body.encode_to_vec(),
            working_on,
            // Its row counts the commands it runs in the background, not
            // its own job, which its chat stops and which is the child.
            background_jobs: child_jobs(&jobs),
            notice: notice.map(Box::new),
            last_said: said,
            ..Snapshot::default()
        });
    }

    // --- a person's input ----------------------------------------------

    /// A person's input to one of the subagents: an answer to one of its
    /// asks, or a stop of it or of a command it left running. Nothing else
    /// reaches a subagent, which takes no messages.
    pub(super) fn child_input(&mut self, emit: &mut Emit, agent_id: &[u8], input: Input) {
        let id = input.input_id.clone();
        let Some(thread) = self.shared.subagent_child(agent_id).map(str::to_owned) else {
            return self.shared.reject(emit, &id, Refusal::Unsupported);
        };
        let arm = match input.decode_body() {
            Some(KindInput::Codex(wire::CodexInput { of: Some(arm) })) => arm,
            _ => return self.shared.reject(emit, &id, Refusal::Unsupported),
        };
        match arm {
            codex_input::Of::Answer(answer) => {
                self.as_child(&thread, |state| state.answer(emit, &id, answer))
            }
            codex_input::Of::StopJob(stop) if stop.key == thread => {
                self.stop_child(emit, &id, &thread)
            }
            codex_input::Of::StopJob(stop) => self.stop_child_job(emit, &id, &thread, stop.key),
            _ => self.shared.reject(emit, &id, Refusal::Unsupported),
        }
    }

    /// The person stops a subagent, in the three steps Codex 0.166.0 needs:
    /// its turn is interrupted, which leaves the command it runs running;
    /// its background commands are listed on its thread and each ended;
    /// then its parent, whose `wait` would otherwise wait on until its own
    /// timeout, is told. Its own job reads stopping until it ends.
    fn stop_child(&mut self, emit: &mut Emit, id: &[u8], thread: &str) {
        let Some(child) = self.children.get_mut(thread) else {
            return self.shared.reject(emit, id, Refusal::Unsupported);
        };
        if !child.stopping {
            child.stopping = true;
            child.stop_error.clear();
            match (child.turn.clone(), child.over.is_some()) {
                (Some(turn), _) => self.interrupt_child(emit, thread, turn),
                // Its turn is over and only its commands run on.
                (None, true) => {
                    self.as_child(thread, |state| state.list_jobs(emit, 1, None, Vec::new()))
                }
                // Started, its turn not yet named: interrupted once it is.
                (None, false) => {}
            }
        }
        self.shared.subagent(emit, thread).accept(id);
    }

    fn interrupt_child(&mut self, emit: &mut Emit, thread: &str, turn: String) {
        let id = self.child_request_id("interrupt");
        let bytes = self.request_as(
            id,
            ClientRequest::TurnInterrupt(TurnInterruptParams {
                thread_id: thread.to_owned(),
                turn_id: turn,
                extra: Default::default(),
            }),
            Request::ChildInterrupt {
                thread: thread.to_owned(),
            },
        );
        emit.effect(Effect::ProviderWrite(bytes));
    }

    /// The person stops one command a subagent left running, by Codex's
    /// process id, on the subagent's thread.
    fn stop_child_job(&mut self, emit: &mut Emit, id: &[u8], thread: &str, key: String) {
        let Some(child) = self.children.get_mut(thread) else {
            return self.shared.reject(emit, id, Refusal::Unsupported);
        };
        let Some(job) = child
            .jobs
            .iter_mut()
            .find(|job| !key.is_empty() && job.key == key)
        else {
            return self
                .shared
                .subagent(emit, thread)
                .reject(id, Refusal::Unsupported);
        };
        job.stopping = true;
        job.stop_error.clear();
        self.terminate_on(emit, thread, key);
        self.shared.subagent(emit, thread).accept(id);
    }

    fn terminate_on(&mut self, emit: &mut Emit, thread: &str, key: String) {
        let bytes = self.request_as(
            format!("amux-stop-{key}"),
            ClientRequest::BackgroundTerminalsTerminate(
                codex_protocol::client::BackgroundTerminalsTerminateParams {
                    thread_id: thread.to_owned(),
                    process_id: key.clone(),
                    extra: Default::default(),
                },
            ),
            Request::StopJob {
                key,
                child: Some(thread.to_owned()),
            },
        );
        emit.effect(Effect::ProviderWrite(bytes));
    }

    /// Codex answered the interrupt of a stopped subagent's turn: what its
    /// turn left running is listed next, to be ended. Refused, the subagent
    /// runs on, its job saying why, and can be stopped again.
    pub(super) fn child_interrupted(
        &mut self,
        emit: &mut Emit,
        thread: &str,
        result: Result<(), String>,
    ) {
        let Some(child) = self.children.get_mut(thread).filter(|child| child.stopping) else {
            return;
        };
        match result {
            Ok(()) => self.as_child(thread, |state| state.list_jobs(emit, 1, None, Vec::new())),
            Err(error) => {
                child.stopping = false;
                child.stop_error = error;
            }
        }
    }

    /// A child's command list is whole. For a subagent being stopped, each
    /// command not yet ending is ended, and its parent is told once.
    pub(super) fn child_jobs_listed(&mut self, emit: &mut Emit, thread: &str) {
        let Some(child) = self.children.get_mut(thread) else {
            return;
        };
        if !child.stopping {
            return;
        }
        let mut ending = Vec::new();
        for job in &mut child.jobs {
            if !job.stopping {
                job.stopping = true;
                ending.push(job.key.clone());
            }
        }
        let tell = !std::mem::replace(&mut child.told, true);
        for key in ending {
            self.terminate_on(emit, thread, key);
        }
        if tell {
            self.tell_parent(emit, thread, true);
        }
    }

    /// Tells the parent the person stopped a subagent, so its `wait` ends:
    /// steered into its running turn, as a queued prompt sent now is
    /// (`steer`), else recorded in its thread for the next turn. Either
    /// ended a parent's wait within milliseconds when recorded; without it
    /// the parent waits on its own timeout. Only the agent's own thread
    /// takes input, so a child's child's parent is not told.
    pub(super) fn tell_parent(&mut self, emit: &mut Emit, thread: &str, steer: bool) {
        // A child thread takes no input, so only the agent's own thread
        // can be told; a child's child's parent is not.
        let Some(child) = self
            .children
            .get(thread)
            .filter(|child| child.parent.is_none())
        else {
            return;
        };
        let name = if child.title.is_empty() {
            thread
        } else {
            child.title.as_str()
        };
        let text = format!(
            "I stopped the agent {name} you started; it will not answer. Stop waiting for it and go on without it."
        );
        let host = self.thread();
        let note = self.child_request_id("told");
        let (id, message, steered) = match self.active_turn.clone().filter(|_| steer) {
            Some(turn) => (
                note.clone(),
                ClientRequest::TurnSteer(TurnSteerParams {
                    thread_id: host,
                    expected_turn_id: turn,
                    input: vec![UserInput::text(&text)],
                    client_user_message_id: Some(note),
                    ..Default::default()
                }),
                true,
            ),
            None => (
                format!("{note}-inject"),
                ClientRequest::ThreadInjectItems(InjectItemsParams {
                    thread_id: host,
                    items: vec![InjectedItem::Message(InjectedMessage {
                        role: "user".into(),
                        content: vec![InjectedContent::InputText(TextContent {
                            text,
                            extra: Default::default(),
                        })],
                        extra: Default::default(),
                    })],
                    extra: Default::default(),
                }),
                false,
            ),
        };
        let bytes = self.request_as(
            id,
            message,
            Request::StopNote {
                thread: thread.to_owned(),
                steered,
            },
        );
        emit.effect(Effect::ProviderWrite(bytes));
    }
}

/// The jobs a child's row counts: the commands it runs in the background,
/// leaving out its own job.
fn child_jobs(jobs: &[BackgroundJob]) -> u32 {
    jobs.iter()
        .filter(|job| job.kind != wire::BackgroundKind::Subagent as i32)
        .count() as u32
}
