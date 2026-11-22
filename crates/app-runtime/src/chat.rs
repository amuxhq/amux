//! One open chat as the phone holds it: the agent's session, borrowed from
//! the fleet and widened while the chat is open, its changes gathered for
//! the host's next turn, and the views the host asks for by row key.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};

use tokio::task::JoinHandle;
use ui_runtime::{Fleet, InputError, PageError, Session};
use ui_state::{AgentKey, InputOutcome, InputState, InputWhat, Key, Refusal, SessionState};
use ui_view::{
    AskCard, Comparison, FormValue, Overview, QuestionResponse, Row, SettingChange, SettingsView,
    ToolRows,
};
use wire::{BlobRef, GetChangesResponse, send_input_response};

use crate::coalesce::{Coalescer, WakeFn};
use crate::delta::Sent;
use crate::review::ReviewSession;
use crate::values::{
    ActOutcome, AskMoved, ChatDelta, ChatFrame, CommandOutcome, Draft, PageOutcome, ProviderLabel,
    ReviewChange, ReviewOutcome, SendOutcome,
};

/// An open chat. The host hears what moved as [`ChatDelta`]s: the window's
/// keys at their edges, the rows that changed, and the views around them,
/// each only when it moved.
pub struct Chat {
    id: u64,
    agent: AgentKey,
    session: Arc<Session>,
    /// Told when the chat closes, so the session narrows again.
    fleet: Weak<Fleet>,
    changes: Arc<Coalescer<Key>>,
    watcher: JoinHandle<()>,
    clock: Arc<dyn client::Clock>,
    /// The changed files the overview last fetched.
    changed_files: Mutex<Option<GetChangesResponse>>,
    /// The runs the reader opened, each held by one of its steps: rows
    /// fold every other run to its newest step.
    open_runs: Mutex<HashSet<Key>>,
    /// Whether the host folds each finished turn to its answer, as the Mac
    /// does, rather than only its runs.
    folds_turns: AtomicBool,
    /// The finished turns the reader opened, each held by its end.
    open_turns: Mutex<HashSet<Key>>,
    /// The draft this device's daemon kept for the agent when the chat
    /// opened.
    kept: Option<Draft>,
    /// What the host was last handed; None until its model opens on the
    /// chat, and no delta is computed before.
    sent: Mutex<Option<Sent>>,
    /// The review page open on the chat.
    review: Mutex<Option<Arc<ReviewSession>>>,
}

impl Chat {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn start(
        id: u64,
        agent: AgentKey,
        session: Arc<Session>,
        fleet: Weak<Fleet>,
        wake: WakeFn,
        clock: Arc<dyn client::Clock>,
        kept: Option<Draft>,
    ) -> Arc<Chat> {
        // Subscribed here, the receiver has seen the opening: its rows are
        // the host's first read, not news. Anything after is, even a change
        // that lands before the watcher first runs, so the watcher must not
        // mark the channel seen itself.
        let mut changed = session.changed();
        // The fleet moving changes no row, but it moves how each agent a
        // row opens stands, which the delta carries.
        let mut fleet_changed = fleet.upgrade().map(|fleet| fleet.changed());
        let changes = Arc::new(Coalescer::new(wake));
        // The watcher holds only weak references to the coalescer and session,
        // never to `Chat` itself, so it cannot delay `Drop` (and `fleet.close`)
        // past the caller dropping its handle.
        let watched_changes = Arc::downgrade(&changes);
        let watched_session = Arc::downgrade(&session);
        let watcher = tokio::spawn(async move {
            loop {
                let fleet_moved = async {
                    match fleet_changed.as_mut() {
                        Some(fleet) => fleet.changed().await.is_ok(),
                        None => std::future::pending().await,
                    }
                };
                tokio::select! {
                    moved = changed.changed() => {
                        if moved.is_err() {
                            return;
                        }
                        let (Some(changes), Some(session)) =
                            (watched_changes.upgrade(), watched_session.upgrade())
                        else {
                            return;
                        };
                        let batch = session.take_changes();
                        changes.push(batch.keys, batch.reloaded, batch.session);
                    }
                    moved = fleet_moved => {
                        if !moved {
                            fleet_changed = None;
                            continue;
                        }
                        let (Some(changes), Some(session)) =
                            (watched_changes.upgrade(), watched_session.upgrade())
                        else {
                            return;
                        };
                        if opens_agents(&session) {
                            changes.push(Vec::new(), false, true);
                        }
                    }
                }
            }
        });
        Arc::new(Chat {
            id,
            agent,
            session,
            fleet,
            changes,
            watcher,
            clock,
            changed_files: Mutex::new(None),
            open_runs: Mutex::new(HashSet::new()),
            folds_turns: AtomicBool::new(false),
            open_turns: Mutex::new(HashSet::new()),
            kept,
            sent: Mutex::new(None),
            review: Mutex::new(None),
        })
    }

    /// The id the host's wake names this chat by.
    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn session(&self) -> &Session {
        &self.session
    }

    /// The held window's keys, oldest first.
    pub fn keys(&self) -> Vec<Key> {
        self.session.state().transcript().keys().cloned().collect()
    }

    /// The draft this device kept for the agent when the chat opened.
    pub fn kept_draft(&self) -> Option<Draft> {
        self.kept.clone()
    }

    /// Keeps the draft as it now stands with this device's daemon, so it
    /// outlives the chat and the app; an empty one is forgotten. Saves go
    /// in order, and one overtaken before it is sent is never sent.
    pub fn keep_draft(&self, draft: &Draft) {
        let (text, attachments) = draft.to_prompt();
        if let Some(fleet) = self.fleet.upgrade() {
            fleet.keep_draft(&self.agent, wire::Draft { text, attachments });
        }
    }

    /// Where the reader is: at the newest row, or in history.
    pub fn follow(&self, following: bool) {
        self.session.follow(following);
    }

    /// Rows for these keys, in order, skipping keys not held, each run
    /// folded to its newest step unless the reader opened it, and with
    /// [`Chat::fold_turns`] each finished turn folded to its answer unless
    /// the reader opened it.
    pub fn rows_for(&self, keys: &[Key]) -> Vec<Row> {
        let runs = self.open_runs.lock().unwrap();
        let turns = self.open_turns.lock().unwrap();
        ui_view::chat_rows_for(&self.session.state(), keys, self.tools(&runs, &turns))
    }

    fn tools<'a>(&self, runs: &'a HashSet<Key>, turns: &'a HashSet<Key>) -> ToolRows<'a> {
        if self.folds_turns.load(Ordering::Relaxed) {
            ToolRows::Turns { runs, turns }
        } else {
            ToolRows::Collapse { open: runs }
        }
    }

    /// Folds each finished turn to its answer, or only its runs; the next
    /// delta carries every row drawn so.
    pub fn fold_turns(&self, on: bool) {
        if self.folds_turns.swap(on, Ordering::Relaxed) != on {
            self.changes.push(self.keys(), false, false);
        }
    }

    /// Opens or closes the finished turn `end` closes; the next delta
    /// carries its rows drawn so.
    pub fn toggle_turn(&self, end: &str) {
        let end = end.to_owned();
        {
            let mut open = self.open_turns.lock().unwrap();
            if !open.remove(&end) {
                open.insert(end.clone());
            }
        }
        let keys = ui_view::turn_keys(&self.session.state(), &end);
        self.changes.push(keys, false, false);
    }

    /// How each agent the held rows open stands, and each group of them,
    /// from the fleet; empty once the fleet is gone.
    pub fn agents(&self) -> ui_view::ChatAgents {
        match self.fleet.upgrade() {
            Some(fleet) => ui_view::chat_agents(&self.session.state(), &fleet.state()),
            None => ui_view::ChatAgents::default(),
        }
    }

    /// Opens or closes the group of calls that opened agents `member`
    /// belongs to; the next delta carries the group's rows drawn so. A
    /// group is held open in the same set as runs.
    pub fn toggle_group(&self, member: &str) {
        let calls: Vec<Key> = {
            let mut open = self.open_runs.lock().unwrap();
            let state = self.session.state();
            let member = member.to_owned();
            ui_view::toggle_group(&state, &member, &mut open);
            std::iter::once(member.clone())
                .chain(ui_view::subagent_rows_moved(&state, &[member]))
                .collect()
        };
        self.changes.push(calls, false, false);
    }

    /// Opens or closes the run `member` belongs to; the next delta carries
    /// its rows drawn so, and every row read after draws it so.
    pub fn toggle_run(&self, member: &str) {
        let steps: Vec<Key> = {
            let mut open = self.open_runs.lock().unwrap();
            let state = self.session.state();
            ui_view::toggle_run(&state, &member.to_owned(), &mut open);
            let transcript = state.transcript();
            transcript
                .get(member)
                .and_then(|held| transcript.run_span(held.item.order))
                .map(|span| {
                    transcript
                        .range(span)
                        .map(|held| held.item.key.clone())
                        .collect()
                })
                .unwrap_or_default()
        };
        self.changes.push(steps, false, false);
    }

    pub fn ask_card(&self) -> Option<AskCard> {
        ui_view::ask_card(&self.session.state())
    }

    /// The overview, with the changed files [`Chat::open_overview`] last
    /// fetched.
    pub fn overview(&self) -> Overview {
        let files = self.changed_files.lock().unwrap().clone();
        ui_view::overview(&self.session.state(), files.as_ref())
    }

    /// Asks the agent's host which files changed for `comparison`, and
    /// answers the overview with them: the uncommitted work against HEAD,
    /// or everything on the branch from the base the host's own rule picks.
    /// A provider subagent, which has no folder of its own, lists none.
    pub async fn open_overview(
        &self,
        comparison: Comparison,
    ) -> Result<Overview, client::RpcError> {
        let files = if ui_view::lists_changes(&self.session.state()) {
            Some(self.session.changes(comparison.base()).await?)
        } else {
            None
        };
        *self.changed_files.lock().unwrap() = files;
        Ok(self.overview())
    }

    /// What the agent offers to change, with the current values marked.
    pub fn settings(&self) -> SettingsView {
        ui_view::settings(&self.session.state())
    }

    pub fn frame(&self) -> ChatFrame {
        let ended = self.session.ended().map(|error| error.to_string());
        let state = self.session.state();
        frame(&state, self.clock.now_ms(), ended)
    }

    /// The chat whole, for the host's model opening on it: every key, the
    /// frame and the views around it. Every delta after it says what moved
    /// since.
    pub fn opening(&self) -> ChatDelta {
        let mut sent = self.sent.lock().unwrap();
        let sent = sent.insert(Sent::default());
        let batch = self.changes.take();
        let mut opening = self.moved(sent, true, &batch.keys);
        opening.review = self.review_page().and_then(|review| review.hand_over(true));
        opening
    }

    /// What moved since the host last heard, or None when nothing did or
    /// its model has not opened on the chat; the next change wakes the host
    /// again.
    pub fn delta(&self) -> Option<ChatDelta> {
        let mut sent = self.sent.lock().unwrap();
        let sent = sent.as_mut()?;
        let batch = self.changes.take();
        let mut delta = self.moved(sent, batch.reloaded, &batch.keys);
        delta.review = self
            .review_page()
            .and_then(|review| review.hand_over(false));
        let moved = delta.keys.is_some()
            || !delta.rows.is_empty()
            || delta.frame.is_some()
            || delta.ask.is_some()
            || delta.overview.is_some()
            || delta.settings.is_some()
            || delta.agents.is_some()
            || delta.review.is_some();
        moved.then_some(delta)
    }

    /// Everything that moved against `sent`, from one read of the session.
    fn moved(&self, sent: &mut Sent, reloaded: bool, changed: &[Key]) -> ChatDelta {
        let ended = self.session.ended().map(|error| error.to_string());
        let files = self.changed_files.lock().unwrap().clone();
        let mut open = self.open_runs.lock().unwrap();
        let state = self.session.state();
        // Each open run is re-held on its newest step before rows are read,
        // so it stays open as it grows, older steps page in, or the window
        // trims it.
        if !open.is_empty() && (reloaded || !changed.is_empty()) {
            ui_view::keep_open_runs(&state, &mut open);
        }
        // Folding turns, a change moves the place in its turn of rows it
        // does not touch itself; and a call joining a group, or a cut
        // taking its first, moves the group's other calls.
        let widened: Vec<Key>;
        let changed = if !reloaded && !changed.is_empty() {
            let turns = if self.folds_turns.load(Ordering::Relaxed) {
                ui_view::turn_rows_moved(&state, changed)
            } else {
                Vec::new()
            };
            widened = changed
                .iter()
                .cloned()
                .chain(turns)
                .chain(ui_view::subagent_rows_moved(&state, changed))
                .collect();
            &widened[..]
        } else {
            changed
        };
        let (keys, wanted) = sent.keys_moved(state.transcript(), reloaded, changed);
        let turns = self.open_turns.lock().unwrap();
        let rows = ui_view::chat_rows_for(&state, &wanted, self.tools(&open, &turns));
        let frame = frame(&state, self.clock.now_ms(), ended);
        ChatDelta {
            chat: self.id,
            keys,
            rows,
            frame: sent.frame(frame),
            ask: sent
                .ask(ui_view::ask_card(&state))
                .map(|card| AskMoved { card }),
            overview: sent.overview(ui_view::overview(&state, files.as_ref())),
            settings: sent.settings(ui_view::settings(&state)),
            agents: sent.agents(match self.fleet.upgrade() {
                Some(fleet) => ui_view::chat_agents(&state, &fleet.state()),
                None => ui_view::ChatAgents::default(),
            }),
            review: None,
        }
    }

    pub async fn send(&self, draft: &Draft) -> SendOutcome {
        let (text, attachments) = draft.to_prompt();
        let sent = self.session.send_prompt(&text, attachments).await;
        let session = self.session.state();
        let state = match session
            .input_state(&sent.id)
            .unwrap_or_else(|| outcome_state(&sent.outcome))
        {
            InputState::Rejected(refusal) => {
                InputState::Rejected(worded(&session, &sent.id, refusal))
            }
            state => state,
        };
        SendOutcome {
            input_id: sent.id,
            state,
        }
    }

    /// Sends `draft` with the review's drafts after its words: the review's
    /// text attachment, naming exactly the comments its text holds, each at
    /// the revision it holds, so the agent's host marks those sent when the
    /// agent takes the message and no draft written or edited meanwhile.
    /// Until then they are still drafts there. The review is read again
    /// first, without changing it, so what is sent is what the host holds;
    /// nothing is sent when it cannot be read or holds no drafts.
    pub async fn send_with_review(&self, draft: &Draft) -> Result<SendOutcome, String> {
        let review = self.reviews();
        let summary = review.read().await.map_err(|error| error.to_string())?;
        let (text, attachments) = with_review(draft, &summary)?;
        let sent = self.session.send_prompt(&text, attachments).await;
        let state = self
            .session
            .state()
            .input_state(&sent.id)
            .unwrap_or_else(|| outcome_state(&sent.outcome));
        Ok(SendOutcome {
            input_id: sent.id,
            state,
        })
    }

    /// Opens the agent's review page on the chat, or opens the open one
    /// again: `base_branch` picks its base anew, and `refresh` takes a
    /// snapshot even with drafts, moving them onto it. The page follows
    /// the agent's row from then until it closes, and moves in the chat's
    /// deltas.
    pub async fn open_review(
        self: &Arc<Self>,
        base_branch: Option<String>,
        refresh: bool,
    ) -> ReviewOutcome {
        let page = {
            let mut open = self.review.lock().unwrap();
            open.get_or_insert_with(|| {
                let session = self.session.clone();
                let watched = Arc::downgrade(self);
                ReviewSession::start(
                    Arc::new(self.session.reviews()),
                    self.session.changed(),
                    Arc::new(move || wire::Agent {
                        experimental: session.state().agent().experimental.clone(),
                        ..wire::Agent::default()
                    }),
                    Arc::new(move || {
                        if let Some(chat) = watched.upgrade() {
                            chat.changes.push(Vec::new(), false, true);
                        }
                    }),
                )
            })
            .clone()
        };
        page.open(base_branch, refresh).await
    }

    /// The review page open on the chat.
    pub fn review_page(&self) -> Option<Arc<ReviewSession>> {
        self.review.lock().unwrap().clone()
    }

    /// Closes the review page: it stops following the agent's row, and no
    /// delta carries it again.
    pub fn close_review(&self) {
        drop(self.review.lock().unwrap().take());
    }

    /// Makes `changes` to the agent's review, all of them or none: on the
    /// open page, or with none open, as the chip's ✕ discards drafts.
    pub async fn edit_review(&self, changes: Vec<ReviewChange>) -> ReviewOutcome {
        match self.review_page() {
            Some(page) => page.edit(changes).await,
            None => crate::review::edit_closed(&self.session.reviews(), changes).await,
        }
    }

    /// The page of the round a sent review in the chat closed, named by its
    /// review and the input that sent it: its whole diff and the comments
    /// it sent. None when that input closed no round, or the agent's host
    /// no longer holds it, and the chat shows the text the agent read
    /// instead. It is read apart from the review page open on the chat, so
    /// the snapshot that page shows never moves under it.
    pub async fn review_round(
        &self,
        review_id: &[u8],
        input_id: &[u8],
    ) -> Option<ui_view::ReviewPage> {
        crate::review::round_page(&self.session.reviews(), review_id, input_id).await
    }

    /// The agent's review: the open page's, or one read for a send.
    fn reviews(&self) -> Arc<ui_runtime::review::Review> {
        match self.review_page() {
            Some(page) => page.review().clone(),
            None => Arc::new(self.session.reviews()),
        }
    }

    /// Runs a draft that is a command the client handles itself, the same
    /// on every client: `/model` and `/effort` set the setting (bare, they
    /// ask for amux's picker), `/rename` renames the agent, and a command
    /// the agent does not run is refused. Anything else comes back as
    /// `Send`, for the draft to go as typed. A draft with attachments is
    /// always a prompt.
    pub async fn command(&self, draft: &Draft) -> CommandOutcome {
        let (text, attachments) = draft.to_prompt();
        if !attachments.is_empty() {
            return CommandOutcome::Send;
        }
        let slash = ui_view::slash(&self.session.state(), &text);
        match slash {
            ui_view::Slash::Prompt => CommandOutcome::Send,
            ui_view::Slash::Pick(command) => CommandOutcome::Pick(command),
            ui_view::Slash::Rename(name) if name.is_empty() => CommandOutcome::AskName,
            ui_view::Slash::Rename(name) => {
                CommandOutcome::Acted(match self.session.rename(&name).await {
                    Ok(_) => ActOutcome::Done,
                    Err(error) => ActOutcome::Failed(error.to_string()),
                })
            }
            ui_view::Slash::Set(input) => {
                CommandOutcome::Acted(acted(self.session.answer(input).await))
            }
            ui_view::Slash::Typed(typed) => {
                let sent = self.session.send_prompt(&typed, Vec::new()).await;
                let state = self
                    .session
                    .state()
                    .input_state(&sent.id)
                    .unwrap_or_else(|| outcome_state(&sent.outcome));
                CommandOutcome::Sent(SendOutcome {
                    input_id: sent.id,
                    state,
                })
            }
            ui_view::Slash::Refused {
                command,
                offered,
                hint,
            } => CommandOutcome::Refused {
                command,
                offered,
                hint,
            },
            ui_view::Slash::Unknown {
                command,
                value,
                offered,
            } => CommandOutcome::Unknown {
                command,
                value,
                offered,
            },
        }
    }

    /// Answers the head ask with its choice at `index`; `note` goes back
    /// with a choice that takes one.
    pub async fn answer_choice(&self, ask_key: &str, index: usize, note: &str) -> ActOutcome {
        let input = {
            let Some(card) = self.card_for(ask_key) else {
                return moved_on();
            };
            let Some(choice) = card.choices.get(index) else {
                return ActOutcome::Failed(format!("the ask has no choice {index}"));
            };
            ui_view::answer_input(&card, &choice.answer, note)
        };
        self.answer(input).await
    }

    /// Submits the head form ask with the person's values, one per field
    /// in the form's order, checked and encoded by the shared view. A form
    /// with a problem is not sent: the phone holds Submit back until the
    /// same check passes, so reaching here with one is a caller's mistake.
    pub async fn answer_form(&self, ask_key: &str, values: &[FormValue]) -> ActOutcome {
        let input = {
            let Some(card) = self.card_for(ask_key) else {
                return moved_on();
            };
            match ui_view::form_answer(&card, values) {
                Ok(Some(answer)) => ui_view::answer_input(&card, &answer, ""),
                Ok(None) => return ActOutcome::Failed("the ask is not a form".into()),
                Err(problems) => {
                    return ActOutcome::Failed(format!(
                        "the form's fields have problems: {problems:?}"
                    ));
                }
            }
        };
        self.answer(input).await
    }

    /// Answers the head question ask: one response per question, in
    /// order, a skip where the person left one unanswered.
    pub async fn answer_questions(&self, ask_key: &str, given: &[QuestionResponse]) -> ActOutcome {
        let input = {
            let Some(card) = self.card_for(ask_key) else {
                return moved_on();
            };
            let answer = ui_view::question_answer(&card, given);
            ui_view::answer_input(&card, &answer, "")
        };
        self.answer(input).await
    }

    /// Replies to the head question ask in the person's own words instead
    /// of answering, with what they had answered so far.
    pub async fn reply_instead(
        &self,
        ask_key: &str,
        text: &str,
        so_far: &[QuestionResponse],
    ) -> ActOutcome {
        let input = {
            let Some(card) = self.card_for(ask_key) else {
                return moved_on();
            };
            let answer = ui_view::reply_answer(&card, text, so_far);
            ui_view::answer_input(&card, &answer, "")
        };
        self.answer(input).await
    }

    fn card_for(&self, ask_key: &str) -> Option<AskCard> {
        self.ask_card().filter(|card| card.key == ask_key)
    }

    async fn answer(&self, input: Option<wire::Input>) -> ActOutcome {
        match input {
            Some(input) => acted(self.session.answer(input).await),
            None => ActOutcome::Rejected(Refusal::of(wire::Refusal::Unsupported)),
        }
    }

    /// Sends a pick from the settings view; the frame shows the new value
    /// once the agent reports it.
    pub async fn change_setting(&self, change: &SettingChange) -> ActOutcome {
        let kind = self.session.state().kind();
        match ui_view::setting_input(kind, change) {
            Some(input) => acted(self.session.answer(input).await),
            None => ActOutcome::Rejected(Refusal::of(wire::Refusal::Unsupported)),
        }
    }

    /// A queued or sent prompt as the draft it came from: its words and
    /// every attachment, whole, for a withdraw or an edit to put back.
    pub fn draft_of(&self, input_id: &[u8]) -> Option<Draft> {
        let state = self.session.state();
        if let Some(row) = state
            .queue()
            .into_iter()
            .find(|row| row.entry.input_id == input_id)
        {
            return Some(Draft::from_queued(row.entry));
        }
        state.inputs().get(input_id).and_then(Draft::from_sent)
    }

    pub async fn withdraw(&self, input_id: &[u8]) -> ActOutcome {
        acted(self.session.withdraw(input_id).await)
    }

    pub async fn send_now(&self, input_id: &[u8]) -> ActOutcome {
        acted(self.session.send_now(input_id).await)
    }

    pub async fn interrupt(&self) -> ActOutcome {
        acted(self.session.interrupt().await)
    }

    /// Stops one background job by the key its `JobStop` gives.
    pub async fn stop_job(&self, key: &str) -> ActOutcome {
        acted(self.session.stop_job(key).await)
    }

    /// Takes the agent's own view back to its conversation after it
    /// switched to another; the composer says so (`switched_away`).
    pub async fn switch_back(&self) -> ActOutcome {
        acted(self.session.switch_back().await)
    }

    /// Sends a not-confirmed input again under a new id, forgetting the
    /// old one.
    pub async fn resend(&self, input_id: &[u8]) -> Option<SendOutcome> {
        let sent = self.session.resend(input_id).await?;
        let state = self
            .session
            .state()
            .input_state(&sent.id)
            .unwrap_or_else(|| outcome_state(&sent.outcome));
        Some(SendOutcome {
            input_id: sent.id,
            state,
        })
    }

    pub fn discard(&self, input_id: &[u8]) {
        self.session.discard(input_id);
    }

    /// The exited composer's one tap: the draft is the new incarnation's
    /// first prompt.
    pub async fn resume_with(&self, draft: &Draft) -> ActOutcome {
        let (text, attachments) = draft.to_prompt();
        let prompt = ui_runtime::inputs::submission(&text, attachments);
        match self.session.resume_with(prompt).await {
            Ok(_) => ActOutcome::Done,
            Err(error) if error.is_transport() => ActOutcome::NotConfirmed,
            Err(error) => ActOutcome::Failed(error.to_string()),
        }
    }

    pub async fn page_older(&self, n: u32) -> PageOutcome {
        match self.session.page_older(n).await {
            Ok(count) => PageOutcome::Arrived(count as u32),
            Err(PageError::OriginUnreachable) => PageOutcome::OriginUnreachable,
            Err(PageError::Rpc(error)) => PageOutcome::Failed(error.to_string()),
        }
    }

    /// Why an attachment would be refused before anything of it is read or
    /// stored, in words naming the file: over the size every host takes, or
    /// a picture or file the agent takes none of, which would have the
    /// whole prompt refused. None when it may go.
    pub fn attachment_refusal(&self, name: &str, mime: &str, size: u64) -> Option<String> {
        attachment_refusal(&self.session.state(), name, mime, size)
    }

    /// Stores bytes to attach to a prompt.
    pub async fn put_blob(
        &self,
        name: &str,
        mime: &str,
        bytes: Vec<u8>,
    ) -> Result<BlobRef, client::RpcError> {
        self.session.put_blob(name, mime, bytes).await
    }

    /// An attachment's bytes once fetched; asking starts the fetch, and the
    /// rows that show it change when it lands.
    pub fn blob(&self, hash: &[u8]) -> Option<Arc<[u8]>> {
        self.session.blob(hash)
    }

    /// Whether fetching an attachment's bytes failed.
    pub fn blob_failed(&self, hash: &[u8]) -> bool {
        matches!(
            self.session.state().blob(hash),
            ui_state::BlobStatus::Failed(_)
        )
    }

    /// Fetches an attachment's bytes again, after a failure.
    pub fn fetch_blob(&self, hash: &[u8]) {
        self.session.fetch_blob(hash);
    }
}

impl Drop for Chat {
    fn drop(&mut self) {
        self.watcher.abort();
        self.close_review();
        if let Some(fleet) = self.fleet.upgrade() {
            fleet.close(&self.agent);
        }
    }
}

/// Whether any held row opens an agent, whose state a fleet change may
/// have moved.
fn opens_agents(session: &Session) -> bool {
    session
        .state()
        .transcript()
        .iter()
        .any(|held| ui_state::opens(&held.body).is_some())
}

pub(crate) fn frame(state: &SessionState, now_ms: i64, ended: Option<String>) -> ChatFrame {
    let agent = state.agent();
    ChatFrame {
        agent: ui_state::agent_key(agent),
        name: ui_state::shown_name(agent).to_owned(),
        provider: provider_label(state),
        capabilities: state.capabilities().into(),
        phase: state.phase(),
        composer: ui_view::composer(state, now_ms),
        controls: ui_view::controls(state),
        git: agent.git.clone(),
        context: ui_view::context(state),
        sign_in: ui_view::sign_in(state),
        connection: state.connection(),
        caught_up: state.caught_up(),
        has_older: state.transcript().has_older(),
        arrivals_held: state.arrivals_held(),
        queue: ui_view::queue_rows(state),
        underway: ui_view::prompts_underway(state),
        refused: ui_view::refused_prompts(state)
            .into_iter()
            .map(|refused| ui_view::RefusedPrompt {
                reason: worded(state, &refused.input_id, refused.reason),
                ..refused
            })
            .collect(),
        ask_input: ui_view::ask_card(state)
            .and_then(|card| state.answering(&card.key).map(|sent| sent.id.clone())),
        ended,
        review_chip: ui_view::review_chip(agent),
    }
}

/// A prompt's refusal with words where it carries none of its own: one
/// refused for an attachment the agent takes none of says which, by the
/// agent's name, so the draft that comes back says why.
fn worded(state: &SessionState, input_id: &[u8], refusal: Refusal) -> Refusal {
    if refusal.reason != wire::Refusal::Unsupported || refusal.detail.is_some() {
        return refusal;
    }
    let untaken = state
        .inputs()
        .iter()
        .find(|sent| sent.id.as_slice() == input_id)
        .and_then(|sent| match &sent.what {
            InputWhat::Prompt { attachments, .. } => ui_view::untaken(state, attachments),
            _ => None,
        });
    match untaken {
        Some(what) => Refusal {
            detail: Some(cannot_take(state, what)),
            ..refusal
        },
        None => refusal,
    }
}

/// "OpenCode can’t take images".
fn cannot_take(state: &SessionState, what: ui_view::Untaken) -> String {
    let what = match what {
        ui_view::Untaken::Images => "images",
        ui_view::Untaken::Files => "files",
    };
    format!("{} can’t take {what}", state.provider_name())
}

fn attachment_refusal(state: &SessionState, name: &str, mime: &str, size: u64) -> Option<String> {
    let limit = ui_view::ATTACHMENT_SIZE_LIMIT as u64;
    if size > limit {
        let megabytes = limit / (1024 * 1024);
        return Some(format!(
            "{name} is not attached because it is over the {megabytes} MB limit for attachments."
        ));
    }
    let what = ui_view::untaken_file(state, mime)?;
    Some(format!(
        "{name} is not attached: {}.",
        cannot_take(state, what)
    ))
}

/// The agent's program as its host names it; by its id while the host has
/// not said, which is what the host would name it by.
fn provider_label(state: &SessionState) -> ProviderLabel {
    ProviderLabel {
        provider: state.agent().provider_id().to_owned(),
        name: state.provider_name(),
    }
}

/// The prompt that sends `draft` with the drafts of `summary` after its
/// words, as the review's text attachment.
fn with_review(
    draft: &Draft,
    summary: &wire::experimental::ReviewSummary,
) -> Result<(String, Vec<wire::Attachment>), String> {
    let review = crate::review::sent_review(summary)
        .ok_or_else(|| "the review holds no drafts to send".to_owned())?;
    let (mut text, mut attachments) = draft.to_prompt();
    text.push(ui_view::PLACEHOLDER);
    attachments.push(review);
    Ok((text, attachments))
}

fn outcome_state(outcome: &InputOutcome) -> InputState {
    match outcome {
        InputOutcome::Lost => InputState::Uncertain,
        InputOutcome::Reply(reply) => match &reply.of {
            Some(send_input_response::Of::Accepted(accepted)) if accepted.queued => {
                InputState::Queued
            }
            Some(send_input_response::Of::Accepted(_)) => InputState::Settled,
            Some(send_input_response::Of::Rejected(rejected)) => {
                InputState::Rejected(ui_state::refusal(rejected))
            }
            None => InputState::Uncertain,
        },
    }
}

fn acted(result: Result<(), InputError>) -> ActOutcome {
    match result {
        Ok(()) => ActOutcome::Done,
        Err(InputError::Rejected(reason)) => ActOutcome::Rejected(reason),
        Err(InputError::Uncertain) => ActOutcome::NotConfirmed,
    }
}

/// The ask the person answered is no longer the one waiting: it closed.
fn moved_on() -> ActOutcome {
    ActOutcome::Rejected(Refusal::of(wire::Refusal::ClosedAsk))
}

#[cfg(test)]
mod tests {
    use wire::experimental::{REVIEW_ROW, ReviewRowEntry, ReviewSummary, ReviewerComment};

    use super::*;
    use crate::values::{DraftAttachment, DraftPart};

    /// A send with the review puts its attachment after the draft's words
    /// and attachments, each in its place; with no drafts there is nothing
    /// to send.
    #[test]
    fn a_send_with_the_review_puts_it_after_the_draft() {
        let draft = Draft {
            parts: vec![
                DraftPart::Text("look at ".into()),
                DraftPart::Attachment(DraftAttachment::Text {
                    name: "log".into(),
                    text: "failed".into(),
                }),
                DraftPart::Text(" and the review".into()),
            ],
        };
        let mut summary = ReviewSummary {
            id: b"review".to_vec(),
            snapshot: "5".repeat(40),
            ..ReviewSummary::default()
        };
        assert!(with_review(&draft, &summary).is_err());
        summary.comments.push(ReviewerComment {
            id: b"one".to_vec(),
            text: "Keep it.".into(),
            ..ReviewerComment::default()
        });
        let (text, attachments) = with_review(&draft, &summary).unwrap();
        let placeholder = ui_view::PLACEHOLDER;
        assert_eq!(
            text,
            format!("look at {placeholder} and the review{placeholder}")
        );
        assert_eq!(attachments.len(), 2);
        let entry = ui_view::sent_review::entry(&attachments[1]).expect("the review last");
        assert_eq!(
            entry
                .comments
                .iter()
                .map(|comment| comment.id.clone())
                .collect::<Vec<_>>(),
            vec![b"one".to_vec()]
        );
    }

    /// The composer's chip comes from the review's entry on the agent's
    /// row, with the frame: there while drafts wait, gone with them.
    #[test]
    fn the_chip_comes_with_the_frame_from_the_agents_row() {
        let mut agent = wire::Agent {
            agent_id: b"agent".to_vec(),
            kind: wire::Kind::Codex as i32,
            ..wire::Agent::default()
        };
        let entry = |drafts| ReviewRowEntry {
            drafts,
            counter: 4,
            moved: true,
            review_id: b"review".to_vec(),
        };
        REVIEW_ROW.put(&mut agent.experimental, &entry(3));
        let chip = frame(&SessionState::new(agent.clone(), 3), 0, None).review_chip;
        assert_eq!(
            chip,
            Some(ui_view::ReviewChip {
                comments: 3,
                moved: true
            })
        );
        REVIEW_ROW.put(&mut agent.experimental, &entry(0));
        assert_eq!(
            frame(&SessionState::new(agent, 3), 0, None).review_chip,
            None
        );
    }
}
