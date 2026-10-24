//! One open chat as the phone holds it: the agent's session, borrowed from
//! the fleet and widened while the chat is open, its changes gathered for
//! the host's next turn, and the views the host asks for by row key.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use client::Client;
use model::AgentKey;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use ui_runtime::{Fleet, InputError, PageError, Session};
use ui_state::{InputOutcome, InputState, Key, Refusal, RefusalReason, SessionState};
use ui_view::{
    AskCard, ChatOptions, Comparison, FormValue, Overview, QuestionResponse, Row, SettingChange,
    SettingsView, ToolRows,
};
use wire::{BlobRef, send_input_response};

use crate::coalesce::{Coalescer, WakeFn};
use crate::delta::Sent;
use crate::values::{
    ActOutcome, AskMoved, ChatDelta, ChatFrame, CommandOutcome, Draft, FrozenReview, PageOutcome,
    SendOutcome,
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
    changes: Coalescer<Key>,
    watcher: OnceLock<JoinHandle<()>>,
    clock: Arc<dyn client::Clock>,
    /// The changed files the overview last fetched.
    changed_files: Mutex<Option<wire::Diff>>,
    /// Where each of this client's prompts was first drawn: true at the
    /// feed's end. A prompt stays where it was first drawn until it lands.
    first_drawn: Mutex<HashMap<Vec<u8>, bool>>,
    /// The runs the reader opened, each held by one of its steps: rows
    /// fold every other run to its newest step.
    open_runs: Mutex<HashSet<Key>>,
    /// The draft this device's daemon kept for the agent when the chat
    /// opened.
    kept: Option<Draft>,
    /// The draft as it last stood, for the saver to hand the daemon.
    draft: watch::Sender<Option<wire::ComposerDraft>>,
    /// What the host was last handed; None until its model opens on the
    /// chat, and no delta is computed before.
    sent: Mutex<Option<Sent>>,
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
        client: Arc<dyn Client>,
        kept: Option<Draft>,
    ) -> Arc<Chat> {
        // Subscribed here, the receiver has seen the opening: its rows are
        // the host's first read, not news. Anything after is, even a change
        // that lands before the watcher first runs, so the watcher must not
        // mark the channel seen itself.
        let mut changed = session.changed();
        let (draft, saving) = watch::channel(None);
        tokio::spawn(save_drafts(client, agent.agent.clone(), saving));
        let chat = Arc::new(Chat {
            id,
            agent,
            session,
            fleet,
            changes: Coalescer::new(wake),
            watcher: OnceLock::new(),
            clock,
            changed_files: Mutex::new(None),
            first_drawn: Mutex::new(HashMap::new()),
            open_runs: Mutex::new(HashSet::new()),
            kept,
            draft,
            sent: Mutex::new(None),
        });
        // Spawned once the chat exists, so every change it sees finds the
        // chat; it ends when the chat is dropped or the session ends.
        let watched = Arc::downgrade(&chat);
        let watcher = tokio::spawn(async move {
            while changed.changed().await.is_ok() {
                let Some(chat) = watched.upgrade() else {
                    return;
                };
                let changes = chat.session.take_changes();
                chat.changes
                    .push(changes.keys, changes.reloaded, changes.session);
            }
        });
        let _ = chat.watcher.set(watcher);
        chat
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
        self.draft
            .send_replace(Some(wire::ComposerDraft { text, attachments }));
    }

    /// Where the reader is: at the newest row, or in history.
    pub fn follow(&self, following: bool) {
        self.session.follow(following);
    }

    /// Rows for these keys, in order, skipping keys not held, each run
    /// folded to its newest step unless the reader opened it.
    pub fn rows_for(&self, keys: &[Key]) -> Vec<Row> {
        let open = self.open_runs.lock().unwrap();
        let opts = ChatOptions {
            tools: ToolRows::Collapse { open: &open },
        };
        ui_view::chat_rows_for(&self.session.state(), keys, &opts)
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

    /// Fetches the files changed for `comparison`, without a patch, and
    /// answers the overview with them. A branch with no known base has no
    /// changes to list.
    pub async fn open_overview(
        &self,
        comparison: Comparison,
    ) -> Result<Overview, client::RpcError> {
        let base = ui_view::diff_base(&self.session.state(), comparison);
        let files = match base {
            Some(base) => Some(self.session.changed_files(base).await?),
            None => None,
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
        let mut first_drawn = self.first_drawn.lock().unwrap();
        note_drawn(&state, &mut first_drawn);
        frame(&state, self.clock.now_ms(), ended, &first_drawn)
    }

    /// The chat whole, for the host's model opening on it: every key, the
    /// frame and the views around it. Every delta after it says what moved
    /// since.
    pub fn opening(&self) -> ChatDelta {
        let mut sent = self.sent.lock().unwrap();
        let sent = sent.insert(Sent::default());
        let batch = self.changes.take();
        self.moved(sent, true, &batch.keys)
    }

    /// What moved since the host last heard, or None when nothing did or
    /// its model has not opened on the chat; the next change wakes the host
    /// again.
    pub fn delta(&self) -> Option<ChatDelta> {
        let mut sent = self.sent.lock().unwrap();
        let sent = sent.as_mut()?;
        let batch = self.changes.take();
        let delta = self.moved(sent, batch.reloaded, &batch.keys);
        let moved = delta.keys.is_some()
            || !delta.rows.is_empty()
            || delta.frame.is_some()
            || delta.ask.is_some()
            || delta.overview.is_some()
            || delta.settings.is_some();
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
        let (keys, wanted) = sent.keys_moved(state.transcript(), reloaded, changed);
        let opts = ChatOptions {
            tools: ToolRows::Collapse { open: &open },
        };
        let rows = ui_view::chat_rows_for(&state, &wanted, &opts);
        let frame = {
            let mut first_drawn = self.first_drawn.lock().unwrap();
            note_drawn(&state, &mut first_drawn);
            frame(&state, self.clock.now_ms(), ended, &first_drawn)
        };
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
        }
    }

    pub async fn send(&self, draft: &Draft) -> SendOutcome {
        let (text, attachments) = draft.to_prompt();
        let sent = self.session.send_prompt(&text, attachments).await;
        let state = self
            .session
            .state()
            .input_state(&sent.id)
            .unwrap_or_else(|| outcome_state(&sent.outcome));
        SendOutcome {
            input_id: sent.id,
            state,
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
            ui_view::Slash::Refused { command, offered } => {
                CommandOutcome::Refused { command, offered }
            }
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
            None => ActOutcome::Rejected(Refusal::of(RefusalReason::Unsupported)),
        }
    }

    /// Sends a pick from the settings view; the frame shows the new value
    /// once the agent reports it.
    pub async fn change_setting(&self, change: &SettingChange) -> ActOutcome {
        let kind = self.session.state().kind();
        match ui_view::setting_input(kind, change) {
            Some(input) => acted(self.session.answer(input).await),
            None => ActOutcome::Rejected(Refusal::of(RefusalReason::Unsupported)),
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

    /// Takes the agent's own view back to its conversation after it
    /// switched to another; the composer says so (`switched_away`).
    pub async fn switch_back(&self) -> ActOutcome {
        acted(self.session.switch_back().await)
    }

    /// Sends a not-confirmed input again under a new id, forgetting the
    /// old one.
    pub async fn resend(&self, input_id: &[u8]) -> Option<SendOutcome> {
        let mut input = self
            .session
            .state()
            .inputs()
            .get(input_id)
            .map(|sent| sent.input.clone())?;
        self.session.discard(input_id);
        input.input_id.clear();
        let sent = self.session.send(input).await;
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

    /// The agent's diff for `comparison` and its patch, frozen for a
    /// review. A branch with no known base is reviewed as its uncommitted
    /// work.
    pub async fn review(&self, comparison: Comparison) -> Result<FrozenReview, client::RpcError> {
        let base = ui_view::diff_base(&self.session.state(), comparison)
            .or_else(|| ui_view::diff_base(&self.session.state(), Comparison::Uncommitted))
            .expect("the working tree is always a base");
        let (diff, patch) = self.session.review(base).await?;
        Ok(FrozenReview { diff, patch })
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
}

impl Drop for Chat {
    fn drop(&mut self) {
        if let Some(watcher) = self.watcher.get() {
            watcher.abort();
        }
        if let Some(fleet) = self.fleet.upgrade() {
            fleet.close(&self.agent);
        }
    }
}

/// Hands the daemon each draft the chat keeps, newest last, until the chat
/// is gone and its last draft saved. A save that fails loses only what the
/// phone still shows.
async fn save_drafts(
    client: Arc<dyn Client>,
    agent_id: Vec<u8>,
    mut drafts: watch::Receiver<Option<wire::ComposerDraft>>,
) {
    while drafts.changed().await.is_ok() {
        let Some(draft) = drafts.borrow_and_update().clone() else {
            continue;
        };
        let request = wire::SaveDraftRequest {
            agent_id: agent_id.clone(),
            draft: Some(draft),
        };
        if let Err(error) = client.save_draft(request).await {
            tracing::warn!(%error, "cannot keep the draft");
        }
    }
}

/// Notes where each prompt on its way is drawn as a frame first sees it,
/// and forgets the ones no longer on their way.
pub(crate) fn note_drawn(state: &SessionState, first_drawn: &mut HashMap<Vec<u8>, bool>) {
    let to_feed = ui_view::sends_to_feed(state);
    let mut underway = HashSet::new();
    for sent in state.inputs().iter() {
        if matches!(
            sent.state,
            InputState::Sent | InputState::Queued | InputState::Uncertain
        ) {
            underway.insert(sent.id.clone());
            first_drawn
                .entry(sent.id.clone())
                .or_insert(to_feed && sent.state != InputState::Queued);
        }
    }
    first_drawn.retain(|id, _| underway.contains(id));
}

pub(crate) fn frame(
    state: &SessionState,
    now_ms: i64,
    ended: Option<String>,
    first_drawn: &HashMap<Vec<u8>, bool>,
) -> ChatFrame {
    let agent = state.agent();
    ChatFrame {
        agent: ui_state::agent_key(agent),
        name: agent.name.clone(),
        kind: state.kind(),
        phase: state.phase(),
        composer: ui_view::composer(state, now_ms),
        waiting: ui_view::waiting(state),
        controls: ui_view::controls(state),
        git: agent.git.clone(),
        context: ui_view::context(state),
        sign_in: ui_view::sign_in(state),
        connection: state.connection(),
        caught_up: state.caught_up(),
        has_older: state.transcript().has_older(),
        arrivals_held: state.arrivals_held(),
        queue: ui_view::queue_rows(state),
        underway: ui_view::prompts_underway(state, |id| {
            first_drawn.get(id).copied().unwrap_or(false)
        }),
        refused: ui_view::refused_prompts(state),
        ask_input: ui_view::ask_card(state)
            .and_then(|card| state.answering(&card.key).map(|sent| sent.id.clone())),
        ended,
    }
}

fn outcome_state(outcome: &InputOutcome) -> InputState {
    match outcome {
        InputOutcome::Lost => InputState::Uncertain,
        InputOutcome::Held => InputState::Held,
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
        Err(InputError::Held) => ActOutcome::Held,
    }
}

/// The ask the person answered is no longer the one waiting: it closed.
fn moved_on() -> ActOutcome {
    ActOutcome::Rejected(Refusal::of(RefusalReason::ClosedAsk))
}

#[cfg(test)]
mod tests {
    use ui_state::Msg;
    use wire::{Kind, Phase, SessionEvent, session_event};

    use super::*;

    fn snapshot(phase: Phase) -> Msg {
        Msg::Event(SessionEvent {
            of: Some(session_event::Of::Snapshot(wire::Snapshot {
                kind: Kind::Codex as i32,
                phase: phase as i32,
                ..wire::Snapshot::default()
            })),
        })
    }

    fn prompt(id: &[u8]) -> Msg {
        Msg::Send(wire::Input::new(
            id.to_vec(),
            wire::CodexInput {
                of: Some(wire::codex_input::Of::Prompt(wire::PromptInput {
                    text: "go".into(),
                    attachments: vec![],
                })),
            },
        ))
    }

    /// A prompt sent to an idle agent is drawn at the feed's end and stays
    /// there once the agent starts work; one sent while it works waits in
    /// the queue.
    #[test]
    fn a_prompt_stays_where_it_was_first_drawn() {
        let agent = wire::Agent {
            agent_id: b"agent".to_vec(),
            host_id: b"host".to_vec(),
            kind: Kind::Codex as i32,
            lifecycle: wire::Lifecycle::Live as i32,
            incarnation: 1,
            ..wire::Agent::default()
        };
        let mut state = SessionState::new(agent, 50);
        state.update(snapshot(Phase::Idle));
        state.update(Msg::Event(SessionEvent {
            of: Some(session_event::Of::CaughtUp(wire::CaughtUp { revision: 0 })),
        }));
        let mut first_drawn = HashMap::new();
        state.update(prompt(b"p1"));
        note_drawn(&state, &mut first_drawn);
        state.update(snapshot(Phase::Working));
        state.update(prompt(b"p2"));
        note_drawn(&state, &mut first_drawn);
        let lands: Vec<(Vec<u8>, ui_view::Lands)> = frame(&state, 0, None, &first_drawn)
            .underway
            .into_iter()
            .map(|prompt| (prompt.input_id, prompt.lands))
            .collect();
        assert_eq!(
            lands,
            vec![
                (b"p1".to_vec(), ui_view::Lands::Feed),
                (b"p2".to_vec(), ui_view::Lands::Queue),
            ]
        );
    }
}
