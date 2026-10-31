//! One open chat: the client's view state over a session, the keys that
//! act on it, and the frame it draws.
//!
//! Everything here is ephemeral and the client's own: the anchor, the
//! expansion set keyed by item keys, the focused row, the draft, the ask
//! card's picks. Keys turn into [`ChatEffect`]s the event loop carries out
//! against the session; nothing in this module does I/O.

pub mod ask;
pub mod changes;
pub mod composer;
pub mod feed;
pub mod layout;
pub mod pane;
pub mod review;
pub mod rows;

use std::collections::{HashMap, HashSet};

pub(crate) use composer::resets;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Frame as Paint;
use ratatui::layout::{Position, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ui_state::{
    ActivityKind, Composer, InputState, InputWhat, Key, PhaseView, Refusal, SessionState, Waiting,
};
pub use ui_view::Comparison;
use ui_view::{
    AskBody, AskCard, CardState, Changeable, ChatOptions, ClientCommand, ExitCause, FamilyHeader,
    Offline, Reach, RowKind, Slash, ToolRows, ask_card, chat_rows_for, composer, overview,
    queue_rows,
};
use wire::{Attachment, attachment};

use self::ask::{AskAction, AskUi};
use self::composer::{COMPOSER_LINES, QueueEntry, edge_row, editor_lines};
use self::feed::FeedHit;
pub use self::layout::ToolSteps;
use self::layout::{Anchor, Frame, Laid, Toggle};
use self::review::{ReviewAction, ReviewPage};
use crate::clipboard::ClipboardContent;
use crate::editor::Editor;
use crate::text::{self, push};
use crate::theme::Theme;
use crate::wheel::Direction;

/// The in-flight pane beside the chat: about a third of the terminal,
/// within these widths.
const PANE_MIN: usize = 36;
const PANE_MAX: usize = 56;
/// The narrowest the chat gets beside the pane; narrower, the pane lies
/// over the feed instead.
const CHAT_MIN: usize = 60;

/// The lines one wheel event scrolls, signed for the feed's arithmetic.
fn wheel_lines(direction: Direction) -> isize {
    crate::wheel::lines(direction) as isize
}

/// How long an empty chat waits before saying it is loading.
pub const LOADING_HINT_MS: i64 = 300;

/// How long an ask must be on screen with no key pressed before it takes
/// keys. A person typing as an ask opens has not read it yet, and a digit or
/// Enter among their words would answer it unseen; every key they press
/// restarts the wait, so steady typing never reaches the ask.
pub const ASK_ARMS_MS: i64 = 500;

/// What a key asks the event loop to do with the session.
#[derive(Clone, Debug, PartialEq)]
pub enum ChatEffect {
    Prompt {
        text: String,
        attachments: Vec<Attachment>,
    },
    /// The exited composer's one act: the draft is the new incarnation's
    /// first prompt.
    Resume {
        text: String,
        attachments: Vec<Attachment>,
    },
    Answer(wire::Input),
    /// Stop: ends the turn; the agent stays live.
    Interrupt,
    /// Stops one background job by its key; the agent stays live.
    StopJob {
        key: String,
    },
    /// Takes a queued prompt back; its words return to the composer.
    Withdraw {
        id: Vec<u8>,
        text: String,
        attachments: Vec<Attachment>,
    },
    SendNow {
        id: Vec<u8>,
    },
    /// Sends an unconfirmed input again under a new id.
    Resend {
        id: Vec<u8>,
    },
    Discard {
        id: Vec<u8>,
    },
    /// Takes Claude's own view back to this agent's conversation.
    SwitchBack,
    /// Takes a held prompt back before it goes; its words return to the
    /// composer.
    Cancel {
        id: Vec<u8>,
    },
    Page(u32),
    /// Ctrl+V: read the clipboard and attach what it holds.
    Paste,
    /// Store these bytes and attach them at the cursor.
    Attach {
        name: String,
        mime: String,
        bytes: Vec<u8>,
    },
    /// Hand the terminal to the agent's own interface.
    RawAttach,
    /// Open the working tree's diff on the review page.
    Review,
    /// The review page, at this changed file.
    ReviewAt(String),
    /// Open a link from the agent's text in the person's browser.
    OpenUrl(String),
    /// Back to home: the header's [Home].
    Home,
    /// A key a click stands for: a hint, the composer's mode. The app
    /// handles it as if pressed, so the leader and its panel work too.
    Press(KeyEvent),
    /// A leader chord picked from the which-key panel.
    Chord(char),
    /// Say this in the notice line.
    Notice(String),
    /// amux's rename of this chat's agent: `/rename <name>`.
    Rename(String),
}

/// Why a file was not attached: it is over the limit, said in the unit a
/// person reads (the limit is 64 MiB of the file's own bytes).
fn too_large_words(name: &str) -> String {
    format!(
        "{name} is too large to attach: files can be at most {} MB",
        wire::ATTACHMENT_SIZE_LIMIT / (1024 * 1024)
    )
}

/// Why a slash command was not sent: the agent does not run it, and what it
/// does run.
fn refused_command_words(
    state: &SessionState,
    command: &str,
    offered: &[String],
    hint: Option<&str>,
) -> String {
    let agent = match state.kind() {
        wire::Kind::Codex => "Codex",
        wire::Kind::ClaudePty | wire::Kind::ClaudeSdk => "Claude",
        wire::Kind::Unspecified => "This agent",
    };
    if let Some(hint) = hint {
        return format!("{agent} takes a skill as a mention: type {hint}");
    }
    let commands: Vec<String> = offered.iter().map(|name| format!("/{name}")).collect();
    if commands.is_empty() {
        format!("{agent} does not run /{command} here")
    } else {
        format!(
            "{agent} does not run /{command} here; it runs {}",
            commands.join(" ")
        )
    }
}

/// Takes a queued prompt back; its words return to the composer (after any
/// draft there, on a new line).
fn withdraw_effect(state: &SessionState, input_id: &[u8]) -> Option<ChatEffect> {
    state
        .queue()
        .into_iter()
        .find(|row| row.entry.input_id == input_id)
        .map(|row| ChatEffect::Withdraw {
            id: input_id.to_vec(),
            text: row.entry.text.clone(),
            attachments: row.entry.attachments.clone(),
        })
}

/// A line of the queue block above the composer: its screen row, its place
/// among the block's entries, and where its controls are, in order (the
/// first is what Enter does, the second what Backspace does).
#[derive(Clone, Debug)]
struct QueuedSpot {
    row: u16,
    index: usize,
    controls: Vec<(u16, u16)>,
}

/// When this client's prompt was first drawn on its way, and where: in the
/// feed (the agent was idle) or in the queue block (it was busy). A prompt
/// that loses its connection stays where it was until catching up.
#[derive(Clone, Copy, Debug)]
struct Sending {
    at_ms: i64,
    in_feed: bool,
}

/// Where the composer's words were drawn, for a click to place the cursor:
/// the screen cell of the first wrapped line's first column, how many
/// wrapped lines show, how many are scrolled off above, and the width the
/// draft wraps at.
#[derive(Clone, Copy, Debug, Default)]
struct ComposerSpot {
    x: u16,
    y: u16,
    rows: u16,
    skip: usize,
    wrap: usize,
}

/// A line of the which-key flyover: its group ("amux", "this chat"), the
/// key, what it does, and the chord's key, for clicks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PanelEntry {
    pub group: &'static str,
    pub key: String,
    pub label: String,
    pub chord: char,
}

/// A control in the chat's header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HeaderControl {
    Diff,
    Home,
}

/// Where each header control was drawn: its columns, and what it does.
type HeaderSpots = Vec<((usize, usize), HeaderControl)>;

/// The changed files of one comparison, fetched without a patch.
#[derive(Debug)]
pub struct Fetched {
    pub key: FetchKey,
    pub diff: wire::Diff,
}

/// What a fetch of the changed files answers: a comparison, as of the
/// totals the agent's row carried when it was asked for. New totals mean
/// the files changed and the list is fetched again.
#[derive(Clone, Debug, PartialEq)]
pub struct FetchKey {
    pub comparison: Comparison,
    pub totals: Option<wire::ChangeTotals>,
}

/// The client's state for one open chat.
#[derive(Debug)]
pub struct ChatView {
    pub agent_id: Vec<u8>,
    pub anchor: Anchor,
    pub expanded: HashSet<Key>,
    pub focus: Option<Key>,
    pub editor: Editor,
    pub ask: AskUi,
    /// The selected entry of the queue block while it has the keys.
    pub tray: Option<usize>,
    /// The review page, kept behind its draft token while the chat shows.
    pub review: Option<ReviewPage>,
    pub review_open: bool,
    /// Whether the agent's own interface can be attached from here.
    pub attach: bool,
    /// How this machine reaches the agent's host, kept current by the app
    /// from the fleet.
    pub reach: Reach,
    /// The leader key, for the keys the chat names.
    pub leader: char,
    /// Runs of steps the reader opened, by their ids.
    pub open_runs: HashSet<Key>,
    /// Whether tool steps fold, all show or hide: the person's choice,
    /// kept by the app for this chat.
    pub tools: ToolSteps,
    /// Whether the agent runs on this machine; the header names its host
    /// only when it does not.
    pub local: bool,
    /// What the changes are counted against, in the header and the
    /// overview: the person's choice, kept by the app across chats.
    pub comparison: Comparison,
    /// The changed files the overview lists, as last fetched.
    pub changes: Option<Fetched>,
    /// The fetch last asked for, so one is asked for once.
    changes_asked: Option<FetchKey>,
    /// The header's controls on the last frame: row, columns, control.
    header_spots: Vec<(u16, (u16, u16), HeaderControl)>,
    /// The header control under the mouse.
    hover: Option<HeaderControl>,
    /// The which-key panel's entries while it shows; the app sets them
    /// before each frame.
    pub panel: Option<Vec<PanelEntry>>,
    /// Clickable places drawn on the last frame, as (row, columns, what):
    /// hints and panel lines stand for keys.
    hint_spots: Vec<(u16, (u16, u16), KeyEvent)>,
    panel_spots: Vec<(u16, (u16, u16), char)>,
    /// The pinned prompt's rows and its key.
    pin_spot: Option<((u16, u16), Key)>,
    /// A page down moved the feed: the next frame keeps the lines the pin
    /// would cover below it, so paging skips none.
    paged: bool,
    /// The overview pane: the row above the composer, unfolded. Whether it
    /// shows is the client's layout, kept across chats by the app.
    pub pane_open: bool,
    /// Whether the pane has the keys rather than the composer.
    pub pane_keys: bool,
    /// The item the pane's keys are on.
    pane_focus: Option<pane::PaneItem>,
    /// The Stop window asking before one background job is stopped.
    stop_ask: Option<StopAsk>,
    /// The pane's folded sections: the client's layout for this chat, kept
    /// by the app.
    pub pane_folds: HashSet<pane::Section>,
    /// The pane body's first line on screen.
    pane_scroll: usize,
    /// The keys moved the focus: bring it into view on the next frame.
    pane_follow: bool,
    /// The pane body's height on the last frame, for paging.
    pane_page: usize,
    /// The item under the mouse; hover and focus are one highlight.
    pane_hover: Option<pane::PaneItem>,
    /// Where the pane was drawn: a click inside gives it the keys.
    pane_rect: Option<Rect>,
    /// A step was just scrolled to from the pane: if it sits within the
    /// last screenful, the next frame follows the newest row instead.
    revealed: bool,
    pane_spots: Vec<(u16, (u16, u16), pane::PaneHit)>,
    /// The row above the composer, which opens the pane.
    row_spot: Option<(u16, (u16, u16))>,
    /// Tool servers the row has told about. It counts the failing ones
    /// when the chat opens, until a send or the pane opening, and again
    /// only once another one fails; the pane keeps the full list.
    servers_told: HashSet<String>,
    /// The tool servers failing on the last frame.
    servers_failing: Vec<String>,
    /// A boxed ask's choices and controls.
    ask_spots: Vec<(u16, (u16, u16), ask::BoxSpot)>,
    /// The step the boxed ask points at, which the feed leaves out.
    asking: Option<Key>,
    /// The last plan the feed opened at, so it opens there only once.
    plan_seen: Option<Key>,
    /// The jump-to-bottom control.
    jump_spot: Option<(u16, (u16, u16))>,
    /// The mode on the composer's edge.
    mode_spot: Option<(u16, (u16, u16))>,
    composer_spot: Option<ComposerSpot>,
    /// Queued prompts above the composer: their rows and their controls.
    queued_spots: Vec<QueuedSpot>,
    /// The queued prompt under the pointer.
    queued_hover: Option<usize>,
    /// The first queued prompt shown, when more wait than show.
    queued_from: usize,
    /// This client's prompts on their way, as first drawn.
    sending: HashMap<Vec<u8>, Sending>,
    /// Rejected prompts whose words have gone back to the composer.
    rejected_seen: HashSet<Vec<u8>>,
    /// Why the last prompt was not sent, on the composer's edge until the
    /// draft changes or is sent.
    not_sent: Option<String>,
    /// The running turn's live end after the newest row, for the layout.
    feed_tail: Vec<Line<'static>>,
    /// Ctrl+S was pressed: the next letter names a setting to change.
    setting_prefix: bool,
    /// A setting being chosen, above the composer.
    picker: Option<crate::setup::Picker>,
    /// The picker's flyover as last drawn, for clicks.
    flyover: crate::setup::Flyover,
    /// Where the feed's first line was drawn, for clicks.
    feed_origin: (u16, u16),
    epoch: u64,
    opened_at_ms: i64,
    /// The head ask's key and when it was first drawn, for the time it
    /// leaves keys with the draft.
    ask_drawn: Option<(String, i64)>,
    /// Files on their way into the draft, by name: a prompt sent before
    /// they land waits for them.
    attaching: Vec<String>,
    /// Enter was pressed while files were attaching: the draft goes once
    /// the last of them is in.
    send_when_attached: bool,
    /// When the last timed key was pressed: an ask arms only after
    /// [`ASK_ARMS_MS`] with no key.
    last_key_at: Option<i64>,
    /// The ask that has armed. Once it takes keys it keeps them, so moving
    /// through its choices quickly never hands keys back to the draft.
    ask_armed: Option<String>,
    /// The last frame's layout, for scrolling and focus.
    laid: Laid,
    feed: (usize, usize),
    /// Whether the session was last told the reader follows the newest row.
    told_following: bool,
    page_asked: Option<u64>,
}

impl ChatView {
    pub fn new(agent_id: Vec<u8>, now_ms: i64, attach: bool) -> ChatView {
        ChatView {
            agent_id,
            anchor: Anchor::Bottom,
            expanded: HashSet::new(),
            focus: None,
            editor: Editor::default(),
            ask: AskUi::default(),
            tray: None,
            review: None,
            review_open: false,
            attach,
            // Nothing is known against the host until the app reads the
            // fleet, before the chat is first drawn.
            reach: Reach::Online(wire::RouteKind::Unspecified),
            leader: 'a',
            open_runs: HashSet::new(),
            tools: ToolSteps::Collapse,
            local: true,
            comparison: Comparison::Uncommitted,
            changes: None,
            changes_asked: None,
            header_spots: Vec::new(),
            hover: None,
            panel: None,
            hint_spots: Vec::new(),
            panel_spots: Vec::new(),
            pin_spot: None,
            paged: false,
            pane_open: false,
            pane_keys: false,
            pane_focus: None,
            stop_ask: None,
            pane_folds: HashSet::new(),
            pane_scroll: 0,
            pane_follow: false,
            pane_page: 0,
            pane_hover: None,
            pane_rect: None,
            revealed: false,
            pane_spots: Vec::new(),
            row_spot: None,
            servers_told: HashSet::new(),
            servers_failing: Vec::new(),
            ask_spots: Vec::new(),
            asking: None,
            plan_seen: None,
            jump_spot: None,
            mode_spot: None,
            composer_spot: None,
            queued_spots: Vec::new(),
            queued_hover: None,
            queued_from: 0,
            sending: HashMap::new(),
            rejected_seen: HashSet::new(),
            not_sent: None,
            feed_tail: Vec::new(),
            setting_prefix: false,
            picker: None,
            flyover: crate::setup::Flyover::default(),
            feed_origin: (0, 0),
            epoch: 0,
            opened_at_ms: now_ms,
            ask_drawn: None,
            attaching: Vec::new(),
            send_when_attached: false,
            last_key_at: None,
            ask_armed: None,
            laid: Laid::default(),
            feed: (0, 0),
            told_following: true,
            page_asked: None,
        }
    }

    fn frame<'a>(&'a self, theme: Theme) -> Frame<'a> {
        Frame {
            anchor: &self.anchor,
            expanded: &self.expanded,
            focus: self.focus.as_ref(),
            width: self.feed.0,
            height: self.feed.1,
            theme,
            leader: self.leader,
            open_runs: &self.open_runs,
            tools: self.tools,
            asking: self.asking.as_ref(),
            tail: &self.feed_tail,
        }
    }

    /// The card to draw, synced with this client's picks. With no ask
    /// open, the last one's picks and fields go.
    fn card(&mut self, state: &SessionState) -> Option<AskCard> {
        let Some(card) = self.live_card(state) else {
            self.ask = AskUi::default();
            return None;
        };
        self.ask.sync(&card);
        Some(card)
    }

    /// The queue block's entries, in the order they will run: the agent's
    /// queue, then this client's prompts on their way to it, then those
    /// that may not have arrived. `host` names what a prompt waits for
    /// while the link is down.
    fn queue_entries(&self, state: &SessionState, host: &str) -> Vec<QueueEntry> {
        let mut entries: Vec<QueueEntry> = queue_rows(state)
            .into_iter()
            .map(QueueEntry::Queued)
            .collect();
        let underway = ui_view::prompts_underway(state, |id| self.in_feed(state, id));
        for prompt in underway {
            if prompt.lands != ui_view::Lands::Queue {
                continue;
            }
            entries.push(match prompt.underway {
                ui_view::Underway::Sending { waiting } => QueueEntry::Sending {
                    input_id: prompt.input_id,
                    text: prompt.text,
                    waiting: waiting.then(|| host.to_owned()),
                },
                ui_view::Underway::MayNotHaveArrived => QueueEntry::Unconfirmed {
                    input_id: prompt.input_id,
                    text: prompt.text,
                },
                ui_view::Underway::Held => QueueEntry::Held {
                    input_id: prompt.input_id,
                    text: prompt.text,
                    host: host.to_owned(),
                },
            });
        }
        entries
    }

    /// Whether this client's prompt draws in the feed rather than the queue
    /// block: where it was first drawn, or, not drawn yet, where it would
    /// be now.
    fn in_feed(&self, state: &SessionState, id: &[u8]) -> bool {
        self.sending
            .get(id)
            .map_or_else(|| ui_view::sends_to_feed(state), |sending| sending.in_feed)
    }

    /// Notes this client's prompts as the frame first sees them: where a
    /// prompt on its way draws, and, for one the agent refused, its words
    /// back in the composer (after any draft) and why on the box's edge.
    fn note_sent(&mut self, state: &SessionState, now_ms: i64) {
        for refused in ui_view::refused_prompts(state) {
            if !self.rejected_seen.insert(refused.input_id.clone()) {
                continue;
            }
            if let Some(InputWhat::Prompt { text, attachments }) = state
                .inputs()
                .iter()
                .find(|sent| sent.id == refused.input_id)
                .map(|sent| &sent.what)
            {
                self.editor.restore(text, attachments.clone());
            }
            self.not_sent = Some(format!("not sent: {}", refusal_words(&refused.reason)));
        }
        let to_feed = ui_view::sends_to_feed(state);
        for sent in state.inputs().iter() {
            if !matches!(sent.what, InputWhat::Prompt { .. }) {
                continue;
            }
            match &sent.state {
                InputState::Sent
                | InputState::Queued
                | InputState::Uncertain
                | InputState::Held => {
                    self.sending.entry(sent.id.clone()).or_insert(Sending {
                        at_ms: now_ms,
                        in_feed: to_feed
                            && !matches!(sent.state, InputState::Queued | InputState::Held),
                    });
                }
                InputState::Rejected(_) | InputState::Settled => {}
            }
        }
    }

    /// Whether a text field has the keys and holds something, for Ctrl+C.
    /// Whether a refused prompt's words have gone back to this composer.
    pub(crate) fn took_back(&self, input_id: &[u8]) -> bool {
        self.rejected_seen.contains(input_id)
    }

    pub fn field_text(&self, state: &SessionState) -> bool {
        if self.review_open {
            return self.review.as_ref().is_some_and(ReviewPage::editing);
        }
        if self.pane_open && self.pane_keys {
            return false;
        }
        if let Some(card) = self.card_takes_keys(state) {
            return self.ask.box_note_text(&card);
        }
        self.tray.is_none() && !self.editor.is_empty()
    }

    /// Whether `key` opens the key help: '?' while the empty composer has
    /// the keys. Every other field that is open, a review comment or an
    /// answer among them, types it.
    pub fn opens_help(&self, state: &SessionState, key: KeyEvent) -> bool {
        key.code == KeyCode::Char('?')
            && !self.review_open
            && self.live_card(state).is_none()
            && self.tray.is_none()
            && self.editor.is_empty()
    }

    /// Ctrl+C on a field with text: clears it as a kill. Only the field
    /// with the keys: a draft hidden behind a card stays.
    pub fn kill_field(&mut self, state: &SessionState) -> bool {
        if self.review_open {
            return self.review.as_mut().is_some_and(ReviewPage::kill_field);
        }
        if let Some(card) = self.card_takes_keys(state) {
            return self.ask.in_box_note(&card) && self.ask.kill_box_note();
        }
        self.tray.is_none() && self.editor.kill_all()
    }

    /// A bracketed paste goes to the field with the keys, and nowhere when
    /// none has them.
    pub fn paste_text(&mut self, state: &SessionState, text: &str) {
        if self.review_open {
            if let Some(page) = &mut self.review {
                page.paste(text);
            }
            return;
        }
        if let Some(card) = self.card_takes_keys(state) {
            self.ask.sync(&card);
            self.ask.paste_box_note(&card, text);
            return;
        }
        if self.tray.is_none() {
            self.editor.paste(text);
            self.focus = None;
        }
    }

    /// The head ask.
    fn live_card(&self, state: &SessionState) -> Option<AskCard> {
        ask_card(state)
    }

    fn card_takes_keys(&self, state: &SessionState) -> Option<AskCard> {
        let card = self.live_card(state)?;
        (card.state != CardState::Dismissed).then_some(card)
    }

    /// One key with no time of its own: every ask open has long been on
    /// screen.
    pub fn key(&mut self, state: &SessionState, key: KeyEvent, theme: Theme) -> Vec<ChatEffect> {
        self.timed_key(state, key, theme, None)
    }

    /// One key, pressed at `now_ms`. Ctrl+C and the leader are the app's
    /// and never reach here.
    pub fn key_at(
        &mut self,
        state: &SessionState,
        key: KeyEvent,
        theme: Theme,
        now_ms: i64,
    ) -> Vec<ChatEffect> {
        self.timed_key(state, key, theme, Some(now_ms))
    }

    fn timed_key(
        &mut self,
        state: &SessionState,
        key: KeyEvent,
        theme: Theme,
        now_ms: Option<i64>,
    ) -> Vec<ChatEffect> {
        let quiet_since = self.last_key_at;
        if now_ms.is_some() {
            self.last_key_at = now_ms;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if let Some(ask) = self.stop_ask.take() {
            return self.stop_ask_key(ask, key);
        }
        if self.review_open
            && let Some(page) = &mut self.review
        {
            match page.key(key) {
                ReviewAction::None => {}
                ReviewAction::Close => self.review_open = false,
                ReviewAction::Comments => self.sync_review_token(),
            }
            return vec![];
        }
        if key.code == KeyCode::Char('o') && ctrl {
            self.pane_toggle_key();
            return vec![];
        }
        if let Some(effects) = self.setting_key(state, key) {
            return effects;
        }
        match key.code {
            KeyCode::Char('b') if ctrl => {
                // Takes Claude's own view back to this agent's conversation.
                return if ui_view::switched_away(state) {
                    vec![ChatEffect::SwitchBack]
                } else {
                    vec![]
                };
            }
            KeyCode::Char('x') if ctrl => {
                let stops = ui_view::ask_card(state).is_some_and(|card| card.stops_turn);
                return if state.phase() == PhaseView::Working || stops {
                    vec![ChatEffect::Interrupt]
                } else {
                    vec![]
                };
            }
            KeyCode::PageUp => {
                // The pin covers the top of the page above too.
                let pin = self
                    .pin_spot
                    .as_ref()
                    .map_or(0, |((from, to), _)| usize::from(to - from) + 1);
                let page = self.feed.1.saturating_sub(2 + pin).max(1) as isize;
                if !self.scroll_box_diff(state, -page) {
                    self.scroll(state, -page, theme);
                }
                return vec![];
            }
            KeyCode::PageDown => {
                let page = self.feed.1.saturating_sub(2).max(1) as isize;
                if !self.scroll_box_diff(state, page) {
                    self.scroll(state, page, theme);
                    self.paged = true;
                }
                return vec![];
            }
            KeyCode::End if ctrl => {
                self.follow();
                return vec![];
            }
            KeyCode::Home if ctrl => {
                if let Some(oldest) = state.transcript().iter().next() {
                    self.anchor = Anchor::Top {
                        key: oldest.item.key.clone(),
                        offset: 0,
                    };
                }
                return vec![];
            }
            _ => {}
        }
        // The pane has the keys: it takes the ones it uses and ignores the
        // rest; Esc hands the keys back to the composer.
        if self.pane_open && self.pane_keys {
            return self.pane_key(state, key);
        }
        if let Some(card) = self.card_takes_keys(state) {
            if !self.arm_ask(&card, now_ms, quiet_since) {
                // Words typed as the ask appears are still the draft's; Enter
                // neither sends the draft nor answers what was not read.
                if key.code != KeyCode::Enter && self.editor.key(key) {
                    self.not_sent = None;
                }
                return vec![];
            }
            self.ask.sync(&card);
            // An ask takes over the composer's box, and Esc there never
            // leaves the ask.
            let action = self.ask.box_key(&card, key);
            return self.ask_effects(state, &card, action);
        }
        // Signing in happens elsewhere; the draft waits behind the box.
        if ui_view::sign_in(state).is_some() {
            return vec![];
        }
        if let Some(selected) = self.tray {
            return self.tray_key(state, selected, key);
        }
        self.composer_key(state, key)
    }

    /// Whether `card` takes a key pressed at `now_ms`, the key before it
    /// having been pressed at `last_key`: once it has been on screen for
    /// [`ASK_ARMS_MS`] with no key pressed. An ask not drawn yet has not been
    /// seen at all. Once armed it stays armed.
    fn arm_ask(&mut self, card: &AskCard, now_ms: Option<i64>, last_key: Option<i64>) -> bool {
        let Some(now_ms) = now_ms else {
            return true;
        };
        if self.ask_armed.as_deref() == Some(card.key.as_str()) {
            return true;
        }
        let armed = match &self.ask_drawn {
            Some((key, drawn)) if *key == card.key => {
                let quiet_from = last_key.map_or(*drawn, |at| at.max(*drawn));
                now_ms.saturating_sub(quiet_from) >= ASK_ARMS_MS
            }
            _ => false,
        };
        if armed {
            self.ask_armed = Some(card.key.clone());
        }
        armed
    }

    /// What the chat does for an action on the ask card.
    fn ask_effects(
        &mut self,
        state: &SessionState,
        card: &AskCard,
        action: AskAction,
    ) -> Vec<ChatEffect> {
        match action {
            AskAction::None => vec![],
            AskAction::Attach => vec![ChatEffect::RawAttach],
            AskAction::OpenUrl(url) => vec![ChatEffect::OpenUrl(url)],
            AskAction::Answer(input) => {
                // The plan was read from its top; once it is answered, the
                // feed follows the work it sets going.
                if matches!(card.body, AskBody::Plan { .. }) {
                    self.follow();
                    self.expanded.remove(&card.item_key);
                }
                vec![ChatEffect::Answer(*input)]
            }
            AskAction::Resend => state
                .answering(&card.key)
                .map(|sent| {
                    vec![ChatEffect::Resend {
                        id: sent.id.clone(),
                    }]
                })
                .unwrap_or_default(),
            AskAction::Discard => state
                .answering(&card.key)
                .map(|sent| {
                    vec![ChatEffect::Discard {
                        id: sent.id.clone(),
                    }]
                })
                .unwrap_or_default(),
        }
    }

    /// A key for the composer, when nothing else takes it.
    fn composer_key(&mut self, state: &SessionState, key: KeyEvent) -> Vec<ChatEffect> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => {
                self.escape();
                vec![]
            }
            KeyCode::Enter if !key.modifiers.contains(KeyModifiers::SHIFT) => self.submit(state),
            KeyCode::Char('v') if ctrl => vec![ChatEffect::Paste],
            KeyCode::BackTab if state.composer() == Composer::Send => {
                next_control(&ui_view::settings(state))
                    .and_then(|change| ui_view::setting_input(state.kind(), &change))
                    .map(ChatEffect::Answer)
                    .into_iter()
                    .collect()
            }
            KeyCode::Up if self.editor.is_empty() && !self.queue_entries(state, "").is_empty() => {
                self.tray = Some(self.queue_entries(state, "").len() - 1);
                vec![]
            }
            // Nothing queued: the chat's last prompt comes back to send
            // again or edit, as a shell's history does.
            KeyCode::Up if self.editor.is_empty() => {
                if let Some(last) = state.transcript().iter().rev().find(|held| {
                    matches!(
                        held.class,
                        ui_state::ItemClass::Prompt | ui_state::ItemClass::Steer
                    )
                }) {
                    let words: String = last
                        .item
                        .text
                        .chars()
                        .filter(|c| *c != attachments::PLACEHOLDER)
                        .collect();
                    self.editor.set(words.trim(), Vec::new());
                }
                vec![]
            }
            _ => {
                if self.editor.key(key) {
                    self.focus = None;
                    // Editing answers why the last one was not sent.
                    self.not_sent = None;
                }
                vec![]
            }
        }
    }

    fn tray_key(
        &mut self,
        state: &SessionState,
        selected: usize,
        key: KeyEvent,
    ) -> Vec<ChatEffect> {
        let entries = self.queue_entries(state, "");
        let Some(entry) = entries.get(selected.min(entries.len().saturating_sub(1))) else {
            self.tray = None;
            return vec![];
        };
        match key.code {
            KeyCode::Esc => self.tray = None,
            KeyCode::Up => self.tray = Some(selected.saturating_sub(1)),
            KeyCode::Down if selected + 1 >= entries.len() => self.tray = None,
            KeyCode::Down => self.tray = Some(selected + 1),
            _ => {
                let effect = match (entry, key.code) {
                    // Typing on an entry goes back to the composer.
                    (_, KeyCode::Char(_))
                        if !key
                            .modifiers
                            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                    {
                        self.tray = None;
                        if self.editor.key(key) {
                            self.not_sent = None;
                        }
                        None
                    }
                    (_, KeyCode::Enter) => self.queue_act(state, entry, 0),
                    (_, KeyCode::Delete | KeyCode::Backspace) => self.queue_act(state, entry, 1),
                    _ => None,
                };
                if let Some(effect) = effect {
                    // Acting on an entry hands the keys back to the
                    // composer: a withdrawn prompt's words are there to
                    // edit, and a second Enter must not act on the next.
                    self.tray = None;
                    return vec![effect];
                }
            }
        }
        vec![]
    }

    /// An entry's control by place: 0 is its first ("[Send Now]",
    /// "[Resend]"), 1 its second ("[Withdraw]", "[Discard]").
    fn queue_act(
        &self,
        state: &SessionState,
        entry: &QueueEntry,
        control: usize,
    ) -> Option<ChatEffect> {
        match (entry, control) {
            (QueueEntry::Queued(queued), 0) if queued.can_send_now => Some(ChatEffect::SendNow {
                id: queued.input_id.clone(),
            }),
            (QueueEntry::Queued(queued), 1) if queued.can_withdraw => {
                withdraw_effect(state, &queued.input_id)
            }
            (QueueEntry::Unconfirmed { input_id, .. }, 0) => Some(ChatEffect::Resend {
                id: input_id.clone(),
            }),
            (QueueEntry::Unconfirmed { input_id, .. }, 1) => Some(ChatEffect::Discard {
                id: input_id.clone(),
            }),
            (QueueEntry::Held { input_id, .. }, 0) => Some(ChatEffect::Cancel {
                id: input_id.clone(),
            }),
            _ => None,
        }
    }

    /// Enter in the composer: send when caught up and live, resume an
    /// exited agent with the draft, and otherwise keep the draft.
    fn submit(&mut self, state: &SessionState) -> Vec<ChatEffect> {
        // Enter on an exited agent resumes it, with or without a message.
        // A draft of only blanks is no message: it would spend a turn.
        let blank = self.editor.attachments().is_empty() && self.editor.text().trim().is_empty();
        if blank && state.composer() != Composer::Resume {
            return vec![];
        }
        match state.composer() {
            // A prompt never goes without a file still on its way into it.
            Composer::Send | Composer::Holds(_) | Composer::Resume
                if !self.attaching.is_empty() =>
            {
                self.send_when_attached = true;
                vec![ChatEffect::Notice(format!(
                    "Sends once {} is attached",
                    self.attaching.join(", ")
                ))]
            }
            // Not current, the client library holds what is sent and sends
            // it once the chat is current again.
            Composer::Send | Composer::Holds(_) => {
                if let Some(effects) = self.slash_command(state) {
                    return effects;
                }
                let (text, attachments) = self.editor.take_prompt();
                self.not_sent = None;
                self.follow();
                self.servers_seen();
                vec![ChatEffect::Prompt { text, attachments }]
            }
            Composer::Resume => {
                let (text, attachments) = self.editor.take_prompt();
                self.not_sent = None;
                self.follow();
                vec![ChatEffect::Resume { text, attachments }]
            }
        }
    }

    /// A draft that is a command the client handles, or one this agent
    /// does not run: what it does instead of going as a prompt. None sends
    /// the draft as typed.
    fn slash_command(&mut self, state: &SessionState) -> Option<Vec<ChatEffect>> {
        use crate::setup::{Item, Picker};
        let text = self.editor.text().to_owned();
        if !self.editor.attachments().is_empty() {
            return None;
        }
        let effects = match ui_view::slash(state, &text) {
            Slash::Prompt => return None,
            Slash::Typed(typed) => vec![ChatEffect::Prompt {
                text: typed,
                attachments: Vec::new(),
            }],
            Slash::Set(input) => vec![ChatEffect::Answer(input)],
            Slash::Pick(command) => {
                let view = ui_view::settings(state);
                let (item, title, choices) = match command {
                    ClientCommand::Effort => {
                        (Item::Effort, "Effort", crate::setup::effort_choices(&view))
                    }
                    _ => (Item::Model, "Model", crate::setup::model_choices(&view)),
                };
                self.picker = Some(Picker::new(item, title, choices, false));
                vec![]
            }
            Slash::Rename(name) if name.is_empty() => {
                return Some(vec![ChatEffect::Notice(
                    "Name the agent after the command: /rename <name>".into(),
                )]);
            }
            // A name the rule refuses is refused here, the draft kept to
            // fix, in words that fit the key line.
            Slash::Rename(name) => match wire::agent_name_problem(&name) {
                Some(problem) => {
                    return Some(vec![ChatEffect::Notice(format!(
                        "not renamed: {}",
                        crate::words::name_problem(problem)
                    ))]);
                }
                None => vec![ChatEffect::Rename(name)],
            },
            // The draft stays, so the person can fix it.
            Slash::Refused {
                command,
                offered,
                hint,
            } => {
                return Some(vec![ChatEffect::Notice(refused_command_words(
                    state,
                    &command,
                    &offered,
                    hint.as_deref(),
                ))]);
            }
            Slash::Unknown {
                command,
                value,
                offered,
            } => {
                let what = match command {
                    ClientCommand::Effort => "effort",
                    _ => "model",
                };
                return Some(vec![ChatEffect::Notice(format!(
                    "No {what} {value} here; it offers {}",
                    offered.join(", ")
                ))]);
            }
        };
        self.editor.take_prompt();
        self.not_sent = None;
        Some(effects)
    }

    /// Ctrl+S and what follows it: a running chat's model, effort and
    /// permission, where the agent lets a client change them. None when the key is
    /// not this.
    fn setting_key(&mut self, state: &SessionState, key: KeyEvent) -> Option<Vec<ChatEffect>> {
        use crate::setup::{Item, Picker};
        // An open ask holds the composer's box, where a picker would be
        // drawn: a picker opened under it would take keys unseen, so the ask
        // wins and the settings wait until it is answered.
        if self.card_takes_keys(state).is_some() {
            self.picker = None;
            self.setting_prefix = false;
            let settings_key =
                key.code == KeyCode::Char('s') && key.modifiers.contains(KeyModifiers::CONTROL);
            return settings_key.then(|| {
                vec![ChatEffect::Notice(
                    "Answer the ask first; settings change once it is answered".into(),
                )]
            });
        }
        if let Some(picker) = &mut self.picker {
            let pick = picker.key(key);
            return Some(self.picked(state, pick));
        }
        if std::mem::take(&mut self.setting_prefix) {
            let view = ui_view::settings(state);
            let (setting, item, title) = match key.code {
                KeyCode::Char('m') => (&view.changeable.model, Item::Model, "Model"),
                KeyCode::Char('e') => (&view.changeable.effort, Item::Effort, "Effort"),
                KeyCode::Char('p') => (&view.changeable.permission, Item::Permission, "Permission"),
                // Esc takes the settings key back; any other key was never
                // meant for it and goes on as typed.
                KeyCode::Esc => return Some(vec![]),
                _ => return None,
            };
            if *setting != Changeable::Pick {
                // A setting this agent can't change from here says why
                // rather than doing nothing.
                return Some(vec![ChatEffect::Notice(unchangeable_words(
                    &view, setting, item,
                ))]);
            }
            let choices = match item {
                Item::Model => crate::setup::model_choices(&view),
                Item::Effort => crate::setup::effort_choices(&view),
                _ => crate::setup::permission_choices(&view),
            };
            self.picker = Some(Picker::new(item, title, choices, false));
            return Some(vec![]);
        }
        if key.code == KeyCode::Char('s') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.setting_prefix = true;
            return Some(vec![]);
        }
        None
    }

    /// What a model or effort flyover's key or click did.
    fn picked(&mut self, state: &SessionState, pick: crate::setup::Pick) -> Vec<ChatEffect> {
        use crate::setup::{Item, Pick};
        let Some(item) = self.picker.as_ref().map(|picker| picker.item) else {
            return vec![];
        };
        match pick {
            Pick::None => vec![],
            Pick::Close => {
                self.picker = None;
                vec![]
            }
            Pick::Value(value) => {
                self.picker = None;
                let change = match item {
                    Item::Effort => ui_view::SettingChange::Effort(value),
                    Item::Permission => ui_view::SettingChange::Permission(value),
                    _ => ui_view::SettingChange::Model(value),
                };
                match ui_view::setting_input(state.kind(), &change) {
                    Some(input) => vec![ChatEffect::Answer(input)],
                    // An agent that changes it only by its own command
                    // (terminal Claude) gets that command typed.
                    None => ui_view::typed_setting(&change)
                        .map(|text| ChatEffect::Prompt {
                            text,
                            attachments: Vec::new(),
                        })
                        .into_iter()
                        .collect(),
                }
            }
        }
    }

    /// Scrolls an opened diff in the ask's box; false when there is none.
    fn scroll_box_diff(&mut self, state: &SessionState, lines: isize) -> bool {
        match self.card_takes_keys(state) {
            Some(card) => {
                self.ask.sync(&card);
                self.ask.scroll_diff(&card, lines)
            }
            None => false,
        }
    }

    /// Whether the pointer is under the feed: on the composer or its box.
    fn below_feed(&self, event: MouseEvent) -> bool {
        event.row >= self.feed_origin.1 + self.feed.1 as u16
    }

    /// Esc, view-only: clear focus, then follow the newest row. It never
    /// answers and never interrupts.
    fn escape(&mut self) {
        if self.focus.take().is_some() {
            return;
        }
        self.follow();
    }

    pub fn follow(&mut self) {
        self.anchor = Anchor::Bottom;
    }

    /// Ctrl+O: closed, the pane opens with the keys; open with the
    /// composer holding the keys, the pane takes them; holding them, it
    /// closes.
    pub fn pane_toggle_key(&mut self) {
        if !self.pane_open {
            self.open_pane();
        } else if !self.pane_keys {
            self.focus_pane();
        } else {
            self.close_pane();
        }
    }

    /// Opens the pane with the keys.
    pub fn open_pane(&mut self) {
        self.pane_open = true;
        self.servers_seen();
        self.focus_pane();
    }

    /// The failing tool servers on screen have been seen: the row stops
    /// counting them.
    fn servers_seen(&mut self) {
        self.servers_told.extend(self.servers_failing.drain(..));
    }

    pub fn close_pane(&mut self) {
        self.pane_open = false;
        self.pane_keys = false;
        self.pane_hover = None;
    }

    /// Gives the pane the keys; the item they land on is settled when the
    /// pane is next drawn.
    fn focus_pane(&mut self) {
        self.pane_keys = true;
        self.pane_follow = true;
    }

    /// The pane's items in its order, as this frame would draw them.
    fn pane_items(&self, state: &SessionState) -> Vec<pane::PaneItem> {
        let overview = self.overview(state);
        pane::Contents {
            overview: &overview,
            folded: &self.pane_folds,
        }
        .items()
    }

    /// Where the pane's keys are among `items`: the focused item while it
    /// is still listed, else the first job, else the first item.
    fn pane_at(&self, items: &[pane::PaneItem]) -> Option<usize> {
        self.pane_focus
            .as_ref()
            .and_then(|focus| items.iter().position(|item| item.same(focus)))
            .or_else(|| {
                items
                    .iter()
                    .position(|item| matches!(item, pane::PaneItem::Job(_)))
            })
            .or((!items.is_empty()).then_some(0))
    }

    /// A key while the pane has the keys: j/k and the arrows move across
    /// its headings, jobs and changed files as one list; Enter folds a
    /// heading, opens a job's step in the chat or a file on the review
    /// page; Esc hands the keys back to the composer, the pane staying
    /// open. It ignores the rest.
    fn pane_key(&mut self, state: &SessionState, key: KeyEvent) -> Vec<ChatEffect> {
        let items = self.pane_items(state);
        let at = self.pane_at(&items);
        let last = items.len().saturating_sub(1);
        match key.code {
            KeyCode::Esc => self.pane_keys = false,
            KeyCode::Char('j') | KeyCode::Down if key.modifiers.is_empty() => {
                self.pane_focus = at.and_then(|at| items.get((at + 1).min(last))).cloned();
                self.pane_follow = true;
            }
            KeyCode::Char('k') | KeyCode::Up if key.modifiers.is_empty() => {
                self.pane_focus = at.and_then(|at| items.get(at.saturating_sub(1))).cloned();
                self.pane_follow = true;
            }
            KeyCode::Enter => {
                if let Some(item) = at.and_then(|at| items.get(at)).cloned() {
                    self.pane_focus = Some(item.clone());
                    return self.open_item(state, item);
                }
            }
            // A job its provider can stop alone: the Stop window asks first.
            KeyCode::Char('x') if key.modifiers.is_empty() => {
                if let Some(pane::PaneItem::Job(job)) = at.and_then(|at| items.get(at))
                    && let Some(job_key) = pane::stop_key(job)
                {
                    self.stop_ask = Some(StopAsk {
                        key: job_key.to_owned(),
                        command: text::first_line(&job.command).to_owned(),
                        stop: true,
                    });
                }
            }
            _ => {}
        }
        vec![]
    }

    /// A key while the Stop window is open. Enter presses the focused
    /// button, never a letter; the arrows and Tab move it; Esc cancels.
    fn stop_ask_key(&mut self, mut ask: StopAsk, key: KeyEvent) -> Vec<ChatEffect> {
        match key.code {
            KeyCode::Enter if ask.stop => return vec![ChatEffect::StopJob { key: ask.key }],
            KeyCode::Enter | KeyCode::Esc => return vec![],
            KeyCode::Left => ask.stop = true,
            KeyCode::Right => ask.stop = false,
            KeyCode::Tab | KeyCode::BackTab => ask.stop = !ask.stop,
            _ => {}
        }
        self.stop_ask = Some(ask);
        vec![]
    }

    /// A heading folds or unfolds; a job opens its step in the chat; a
    /// file, the review page at it.
    fn open_item(&mut self, state: &SessionState, item: pane::PaneItem) -> Vec<ChatEffect> {
        match item {
            pane::PaneItem::Heading(section) => {
                if !self.pane_folds.remove(&section) {
                    self.pane_folds.insert(section);
                }
                self.pane_follow = true;
                vec![]
            }
            pane::PaneItem::Job(job) => {
                if let Some(key) = &job.step {
                    self.reveal_step(state, key);
                }
                vec![]
            }
            pane::PaneItem::File(path) => vec![ChatEffect::ReviewAt(path)],
        }
    }

    fn over_pane(&self, event: MouseEvent) -> bool {
        self.pane_rect
            .is_some_and(|rect| rect.contains(Position::new(event.column, event.row)))
    }

    /// Scrolls the chat to a step and opens it, and the run it sits in.
    fn reveal_step(&mut self, state: &SessionState, key: &Key) {
        let transcript = state.transcript();
        let Some(order) = transcript.get(key).map(|held| held.item.order) else {
            return;
        };
        if !ui_view::run_is_open(state, order, &self.open_runs) {
            ui_view::toggle_run(state, key, &mut self.open_runs);
        }
        self.expanded.insert(key.clone());
        self.anchor = Anchor::Top {
            key: key.clone(),
            offset: 0,
        };
        self.revealed = true;
    }

    /// Whether the reader follows the newest row, when that changed since
    /// the session was last told: the one thing about the view the session
    /// needs, to trim its window while following and hold arrivals while
    /// the reader is in history.
    pub fn following_moved(&mut self) -> Option<bool> {
        let following = self.anchor == Anchor::Bottom;
        (following != self.told_following).then(|| {
            self.told_following = following;
            following
        })
    }

    fn scroll(&mut self, state: &SessionState, delta: isize, theme: Theme) {
        // Following the newest, there is nothing further down.
        if self.anchor == Anchor::Bottom && delta >= 0 {
            return;
        }
        self.anchor = self.frame(theme).scrolled(state, &self.laid, delta);
    }

    pub fn mouse(
        &mut self,
        state: &SessionState,
        event: MouseEvent,
        theme: Theme,
    ) -> Vec<ChatEffect> {
        if self.review_open
            && let Some(page) = &mut self.review
        {
            match event.kind {
                MouseEventKind::ScrollUp => page.scroll_by(-wheel_lines(Direction::Up)),
                MouseEventKind::ScrollDown => page.scroll_by(wheel_lines(Direction::Down)),
                MouseEventKind::Down(MouseButton::Left) => page.click(event.column, event.row),
                _ => {}
            }
            return vec![];
        }
        // A flyover is open: a click on a choice picks it, anywhere else
        // closes it.
        if self.picker.is_some() && event.kind == MouseEventKind::Down(MouseButton::Left) {
            let (x, y) = (event.column, event.row);
            let pick = match self.flyover.choice_at(x, y) {
                Some(at) => self
                    .picker
                    .as_mut()
                    .map_or(crate::setup::Pick::None, |picker| picker.click(at)),
                None if self.flyover.covers(x, y) => crate::setup::Pick::None,
                None => crate::setup::Pick::Close,
            };
            return self.picked(state, pick);
        }
        let control = self
            .header_spots
            .iter()
            .find(|(y, (from, to), _)| *y == event.row && (*from..*to).contains(&event.column))
            .map(|(_, _, control)| *control);
        match event.kind {
            MouseEventKind::Moved => {
                self.hover = control;
                self.queued_hover = self
                    .queued_spots
                    .iter()
                    .find(|spot| spot.row == event.row)
                    .map(|spot| spot.index);
                self.pane_hover = self
                    .pane_spots
                    .iter()
                    .find(|(row, (from, to), _)| {
                        *row == event.row && (*from..*to).contains(&event.column)
                    })
                    .and_then(|(_, _, hit)| match hit {
                        pane::PaneHit::Item(item) => Some(item.clone()),
                        pane::PaneHit::Close | pane::PaneHit::Page { .. } => None,
                    });
            }
            // The wheel scrolls what is under the pointer; over the pane it
            // leaves the keys where they are.
            MouseEventKind::ScrollUp if self.over_pane(event) => {
                self.pane_scroll = self
                    .pane_scroll
                    .saturating_sub(crate::wheel::lines(Direction::Up));
                self.pane_hover = None;
            }
            MouseEventKind::ScrollDown if self.over_pane(event) => {
                self.pane_scroll += crate::wheel::lines(Direction::Down);
                self.pane_hover = None;
            }
            // Under the feed, an opened diff in the ask's box takes the
            // wheel.
            MouseEventKind::ScrollUp
                if self.below_feed(event)
                    && self.scroll_box_diff(state, -wheel_lines(Direction::Up)) => {}
            MouseEventKind::ScrollDown
                if self.below_feed(event)
                    && self.scroll_box_diff(state, wheel_lines(Direction::Down)) => {}
            MouseEventKind::ScrollUp => self.scroll(state, -wheel_lines(Direction::Up), theme),
            MouseEventKind::ScrollDown => self.scroll(state, wheel_lines(Direction::Down), theme),
            MouseEventKind::Down(MouseButton::Left) => {
                match control {
                    Some(HeaderControl::Diff) => return vec![ChatEffect::Review],
                    Some(HeaderControl::Home) => return vec![ChatEffect::Home],
                    None => {}
                }
                let (x, y) = (event.column, event.row);
                let on = |row: u16, (from, to): (u16, u16)| row == y && (from..to).contains(&x);
                if let Some(spot) = self
                    .ask_spots
                    .iter()
                    .find(|(row, cols, _)| on(*row, *cols))
                    .map(|(_, _, spot)| *spot)
                    && let Some(card) = self.card_takes_keys(state)
                {
                    self.ask.sync(&card);
                    let action = self.ask.box_click(&card, spot);
                    return self.ask_effects(state, &card, action);
                }
                if let Some((_, _, hit)) = self
                    .pane_spots
                    .iter()
                    .find(|(row, cols, _)| on(*row, *cols))
                    .cloned()
                {
                    match hit {
                        pane::PaneHit::Close => self.close_pane(),
                        pane::PaneHit::Page { up } => {
                            let page = self.pane_page.saturating_sub(2).max(1);
                            self.pane_scroll = if up {
                                self.pane_scroll.saturating_sub(page)
                            } else {
                                self.pane_scroll + page
                            };
                        }
                        pane::PaneHit::Item(item) => {
                            self.pane_keys = true;
                            self.pane_focus = Some(item.clone());
                            return self.open_item(state, item);
                        }
                    }
                    return vec![];
                }
                // A click anywhere else in the pane gives it the keys.
                if self
                    .pane_rect
                    .is_some_and(|rect| rect.contains(Position::new(x, y)))
                {
                    self.focus_pane();
                    return vec![];
                }
                if self.row_spot.is_some_and(|(row, cols)| on(row, cols)) {
                    self.open_pane();
                    return vec![];
                }
                if let Some((_, _, chord)) = self
                    .panel_spots
                    .iter()
                    .find(|(row, cols, _)| on(*row, *cols))
                {
                    return vec![ChatEffect::Chord(*chord)];
                }
                if let Some((_, _, key)) = self
                    .hint_spots
                    .iter()
                    .find(|(row, cols, _)| on(*row, *cols))
                {
                    return vec![ChatEffect::Press(*key)];
                }
                if self.jump_spot.is_some_and(|(row, cols)| on(row, cols)) {
                    self.follow();
                    return vec![];
                }
                if let Some(((from, to), key)) = &self.pin_spot
                    && (*from..*to).contains(&y)
                {
                    self.anchor = Anchor::Top {
                        key: key.clone(),
                        offset: 0,
                    };
                    return vec![];
                }
                if self.mode_spot.is_some_and(|(row, cols)| on(row, cols)) {
                    return vec![ChatEffect::Press(KeyEvent::new(
                        KeyCode::BackTab,
                        KeyModifiers::SHIFT,
                    ))];
                }
                // A queued prompt's controls act; a click elsewhere on it
                // highlights it.
                if let Some(spot) = self.queued_spots.iter().find(|spot| spot.row == y).cloned() {
                    let entries = self.queue_entries(state, "");
                    let Some(entry) = entries.get(spot.index) else {
                        return vec![];
                    };
                    if let Some(control) = spot.controls.iter().position(|cols| on(spot.row, *cols))
                        && let Some(effect) = self.queue_act(state, entry, control)
                    {
                        self.tray = None;
                        return vec![effect];
                    }
                    self.tray = Some(spot.index);
                    return vec![];
                }

                if let Some(spot) = self.composer_spot
                    && self.tray.is_none()
                    && (spot.y..spot.y + spot.rows).contains(&y)
                    && x >= spot.x.saturating_sub(2)
                {
                    self.pane_keys = false;
                    let row = usize::from(y - spot.y) + spot.skip;
                    let col = usize::from(x.saturating_sub(spot.x));
                    let at = composer::cursor_at(&self.editor, spot.wrap, row, col);
                    self.editor.set_cursor_chars(at);
                    return vec![];
                }
                if let Some(hit) = self.hit_at(event.column, event.row) {
                    return self.feed_hit(state, hit);
                }
            }
            _ => {}
        }
        vec![]
    }

    /// The overview with the changed files fetched for the comparison
    /// shown, while they still answer it.
    fn overview(&self, state: &SessionState) -> ui_view::Overview {
        let key = self.fetch_key(state);
        let diff = self
            .changes
            .as_ref()
            .filter(|fetched| fetched.key.comparison == key.comparison)
            .map(|fetched| &fetched.diff);
        overview(state, diff)
    }

    /// The fetch the overview needs now: the comparison shown, as of the
    /// row's totals for it.
    fn fetch_key(&self, state: &SessionState) -> FetchKey {
        FetchKey {
            comparison: self.comparison,
            totals: change_totals(state, self.comparison),
        }
    }

    /// The comparison's changed files to fetch, without a patch: while the
    /// overview shows, once for each comparison and again whenever the
    /// row's totals for it move. Never on every frame.
    pub fn wants_changes(&mut self, state: &SessionState) -> Option<(FetchKey, wire::DiffBase)> {
        if !self.pane_open || !matches!(state.composer(), Composer::Send | Composer::Resume) {
            return None;
        }
        let key = self.fetch_key(state);
        if self.changes_asked.as_ref() == Some(&key) {
            return None;
        }
        let base = ui_view::diff_base(state, key.comparison)?;
        self.changes_asked = Some(key.clone());
        Some((key, base))
    }

    /// Takes in a fetch of the changed files, unless the person has moved
    /// to another comparison since it was asked for.
    pub fn changes_fetched(&mut self, key: FetchKey, diff: wire::Diff) {
        if key.comparison == self.comparison {
            self.changes = Some(Fetched { key, diff });
        }
    }

    /// Switches the comparison the header and the overview count against:
    /// uncommitted changes, or everything on this branch. False when the
    /// branch has no base to compare with, and nothing changes.
    pub fn switch_comparison(&mut self, state: &SessionState) -> bool {
        let next = match self.comparison {
            Comparison::Uncommitted => Comparison::OnBranch,
            Comparison::OnBranch => Comparison::Uncommitted,
        };
        if ui_view::diff_base(state, next).is_none() {
            return false;
        }
        self.comparison = next;
        true
    }

    /// Switches how tool steps draw, round from collapse through show all
    /// and hide, and returns the new choice.
    pub fn switch_tools(&mut self) -> ToolSteps {
        self.tools = self.tools.next();
        self.tools
    }

    /// What the feed line under a click does there.
    fn hit_at(&self, column: u16, row: u16) -> Option<FeedHit> {
        let (x, y) = self.feed_origin;
        let line = usize::from(row.checked_sub(y)?);
        let column = usize::from(column.checked_sub(x)?);
        self.laid
            .hits
            .get(line)?
            .iter()
            .find(|(span, _)| span.is_none_or(|(from, to)| (from..to).contains(&column)))
            .map(|(_, hit)| hit.clone())
    }

    fn feed_hit(&mut self, state: &SessionState, hit: FeedHit) -> Vec<ChatEffect> {
        // Scrolled up, opening or folding something keeps what is on screen
        // where it is: the feed holds its top line, so an opened fold grows
        // downward from the line that was clicked. Following the newest row,
        // it keeps following.
        if matches!(hit, FeedHit::Run(_) | FeedHit::Step(_))
            && self.anchor != Anchor::Bottom
            && let Some(top) = self.laid.blocks.first()
        {
            self.anchor = Anchor::Top {
                key: top.key.clone(),
                offset: self.laid.top_offset,
            };
        }
        match hit {
            FeedHit::Link(url) => return vec![ChatEffect::OpenUrl(url)],
            FeedHit::Run(member) => ui_view::toggle_run(state, &member, &mut self.open_runs),
            FeedHit::Step(key) => {
                // A run opens from any member and closes as a whole.
                let transcript = state.transcript();
                let run = transcript
                    .get(&key)
                    .and_then(|held| transcript.run_at(held.item.order));
                let members: Vec<Key> = match run {
                    Some(run) => transcript
                        .range(run.oldest..=run.newest)
                        .map(|held| held.item.key.clone())
                        .collect(),
                    None => vec![key.clone()],
                };
                if members.iter().any(|member| self.expanded.contains(member)) {
                    for member in &members {
                        self.expanded.remove(member);
                    }
                } else {
                    self.expanded.insert(key);
                }
            }
        }
        vec![]
    }

    /// `<leader> k` / `<leader> j`: focus the older or newer drawn row and
    /// keep it on screen.
    pub fn move_focus(&mut self, state: &SessionState, older: bool, theme: Theme) {
        let count = self.laid.blocks.len();
        if count == 0 {
            return;
        }
        let at = self
            .focus
            .as_ref()
            .and_then(|key| self.laid.blocks.iter().position(|b| &b.key == key));
        if at == Some(0) && older {
            // The focused row is the top one: bring the row above it in
            // and focus that.
            let above =
                self.frame(theme)
                    .scrolled(state, &self.laid, -(self.laid.top_offset as isize) - 1);
            if let Anchor::Top { key, .. } = &above {
                self.focus = Some(key.clone());
                self.anchor = Anchor::Top {
                    key: key.clone(),
                    offset: 0,
                };
            }
            return;
        }
        let next = match (at, older) {
            (None, _) => count - 1,
            (Some(i), true) => i - 1,
            (Some(i), false) => (i + 1).min(count - 1),
        };
        let key = self.laid.blocks[next].key.clone();
        if next == 0 && self.laid.top_offset > 0 {
            self.anchor = Anchor::Top {
                key: key.clone(),
                offset: 0,
            };
        }
        self.focus = Some(key);
    }

    /// `<leader> o`: a folded run opens, an open run's first step folds it
    /// again, and any other step opens its detail. With no row focused, the
    /// newest run on screen opens or folds, so a run is never out of the
    /// keyboard's reach.
    pub fn toggle_expanded(&mut self, state: &SessionState) {
        let Some(key) = self.focus.clone() else {
            let newest = self
                .laid
                .blocks
                .iter()
                .rev()
                .find_map(|block| match &block.toggle {
                    Toggle::Run(id) => Some(id.clone()),
                    _ => None,
                });
            if let Some(id) = newest {
                self.feed_hit(state, FeedHit::Run(id));
            }
            return;
        };
        if let Some(block) = self.laid.blocks.iter().find(|block| block.key == key) {
            match block.toggle.clone() {
                Toggle::Row => {}
                Toggle::Run(id) => {
                    self.feed_hit(state, FeedHit::Run(id));
                    return;
                }
                Toggle::Step(step) => {
                    let header =
                        block
                            .hits
                            .first()
                            .and_then(|hits| hits.first())
                            .and_then(|(_, hit)| match hit {
                                FeedHit::Run(id) => Some(id.clone()),
                                _ => None,
                            });
                    match header {
                        Some(id) => self.feed_hit(state, FeedHit::Run(id)),
                        None => self.feed_hit(state, FeedHit::Step(step)),
                    };
                    return;
                }
            }
        }
        let transcript = state.transcript();
        let run = transcript
            .get(&key)
            .and_then(|held| transcript.run_at(held.item.order));
        match run {
            Some(run) => {
                let members: Vec<Key> = transcript
                    .range(run.oldest..=run.newest)
                    .map(|held| held.item.key.clone())
                    .collect();
                let open = members.iter().any(|member| self.expanded.contains(member));
                if open {
                    for member in &members {
                        self.expanded.remove(member);
                    }
                    self.focus = Some(run.newest_key);
                } else {
                    self.expanded.insert(key);
                }
            }
            None => {
                if !self.expanded.remove(&key) {
                    self.expanded.insert(key);
                }
            }
        }
    }

    /// `<leader> y`: the focused row's words, or the newest row's.
    pub fn copy_text(&self) -> Option<String> {
        let block = match &self.focus {
            Some(key) => self.laid.blocks.iter().find(|b| &b.key == key),
            None => self.laid.blocks.last(),
        }?;
        let text: Vec<String> = block
            .lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
                    .trim_start_matches(['▌', '▎'])
                    .trim()
                    .to_owned()
            })
            .collect();
        Some(text.join("\n").trim().to_owned())
    }

    /// What Ctrl+V found: text follows the paste rules, an image or a file
    /// is stored and attached.
    pub fn paste(&mut self, content: ClipboardContent) -> Result<Option<ChatEffect>, String> {
        match content {
            ClipboardContent::Image { mime, bytes } => {
                let name = "clipboard.png".to_owned();
                if bytes.len() > wire::ATTACHMENT_SIZE_LIMIT {
                    return Err(too_large_words(&name));
                }
                self.attaching.push(name.clone());
                Ok(Some(ChatEffect::Attach { name, mime, bytes }))
            }
            ClipboardContent::Path(path) => {
                let name = path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string());
                // Refused before it is read or sent: the daemon would refuse
                // it after the whole upload.
                let size = std::fs::metadata(&path)
                    .map_err(|error| format!("{name} could not be read: {error}"))?
                    .len();
                if size > wire::ATTACHMENT_SIZE_LIMIT as u64 {
                    return Err(too_large_words(&name));
                }
                let bytes = std::fs::read(&path)
                    .map_err(|error| format!("{name} could not be read: {error}"))?;
                self.attaching.push(name.clone());
                Ok(Some(ChatEffect::Attach {
                    mime: attachments::native_image(&bytes)
                        .unwrap_or_else(|| mime_of(&name))
                        .to_owned(),
                    name,
                    bytes,
                }))
            }
            ClipboardContent::Text(text) => {
                self.editor.paste(&text);
                Ok(None)
            }
            ClipboardContent::Empty => Ok(None),
        }
    }

    /// A stored blob, attached at the cursor as an image or a file.
    /// A file stored for the draft: its chip goes in. When Enter waited
    /// for it and it was the last on its way, the draft is sent now.
    pub fn attach_blob(&mut self, state: &SessionState, blob: wire::BlobRef) -> Vec<ChatEffect> {
        self.landed(&blob.name);
        let of = if blob.mime.starts_with("image/") {
            attachment::Of::Image(blob)
        } else {
            attachment::Of::File(blob)
        };
        self.editor.attach(Attachment { of: Some(of) });
        if self.attaching.is_empty() && std::mem::take(&mut self.send_when_attached) {
            return self.submit(state);
        }
        vec![]
    }

    /// A file could not be stored: it is not in the draft, a send waiting
    /// for it waits no longer, and why stays on the composer's edge until
    /// the draft changes.
    pub fn attach_failed(&mut self, name: &str, why: &str) {
        self.landed(name);
        self.send_when_attached = false;
        self.not_sent = Some(format!("could not attach {name}: {why}"));
    }

    fn landed(&mut self, name: &str) {
        match self.attaching.iter().position(|pending| pending == name) {
            Some(at) => {
                self.attaching.remove(at);
            }
            None => {
                self.attaching.pop();
            }
        }
    }

    /// The files still on their way into the draft.
    pub fn attaching(&self) -> &[String] {
        &self.attaching
    }

    /// `<leader> r` with a review already in the draft: back to its page.
    /// False when there is none, and a fresh diff is wanted.
    pub fn resume_review(&mut self) -> bool {
        let held = self
            .review
            .as_ref()
            .is_some_and(|page| self.editor.find_attachment(|a| page.owns(a)).is_some());
        self.review_open = held;
        held
    }

    /// The review page, at the file `at` when one is named.
    pub fn open_review_at(&mut self, diff: wire::Diff, patch: String, at: Option<&str>) {
        let mut page = ReviewPage::new(diff, patch);
        if let Some(path) = at {
            page.show_file(path);
        }
        self.review = Some(page);
        self.review_open = true;
    }

    /// The review page already held, turned to the file `at`.
    pub fn review_file(&mut self, at: &str) {
        if let Some(page) = &mut self.review {
            page.show_file(at);
        }
    }

    /// Keeps the draft's Review token in step with the page: the first
    /// comment inserts it at the cursor, later ones update it where it
    /// sits, and the last deletion takes it out.
    fn sync_review_token(&mut self) {
        let Some(page) = &self.review else {
            return;
        };
        let held = self.editor.find_attachment(|a| page.owns(a));
        match (held, page.comments().is_empty()) {
            (Some(index), false) => self.editor.replace_attachment(index, page.attachment()),
            (Some(index), true) => self.editor.remove_attachment(index),
            (None, false) => self.editor.attach(page.attachment()),
            (None, true) => {}
        }
    }

    /// A page was asked for; another waits until the window grows.
    pub fn page_sent(&mut self, state: &SessionState) {
        self.page_asked = state.oldest_order();
    }

    /// Lays the chat out in `area` and paints it. Returns the page to ask
    /// for, if the reader is within a page of the oldest held row.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &mut self,
        paint: &mut Paint<'_>,
        area: Rect,
        state: &SessionState,
        family: Option<&FamilyHeader>,
        footer: Option<Line<'static>>,
        now_ms: i64,
        theme: Theme,
    ) -> Option<u32> {
        if state.epoch() != self.epoch {
            // A Reset swapped the transcript in: scroll to the newest, the
            // one whole-list reload.
            self.epoch = state.epoch();
            self.follow();
            self.focus = None;
            self.page_asked = None;
        }
        if self.review_open
            && let Some(page) = &mut self.review
        {
            // Its header names the agent and where it works, as the chat's.
            let name = state.agent().name.clone();
            let cwd = &state.agent().cwd;
            let place = if self.local {
                text::tilde(cwd)
            } else {
                cwd.clone()
            };
            page.set_owner(&name, &place);
            page.draw(paint, area, footer, theme);
            return None;
        }
        let page = self.draw_turns(paint, area, state, family, footer, now_ms, theme);
        if let Some(ask) = &self.stop_ask {
            stop_window(paint, area, ask, theme);
        }
        page
    }
}

/// The question before one background job is stopped: which job, and the
/// button Enter presses.
#[derive(Clone, Debug)]
struct StopAsk {
    key: String,
    command: String,
    /// Stop has the focus, else Cancel.
    stop: bool,
}

/// The Stop window, centred over the chat: "Stop cargo test? The agent
/// keeps running.", then Stop and Cancel.
fn stop_window(paint: &mut Paint<'_>, area: Rect, ask: &StopAsk, theme: Theme) {
    let width = usize::from(area.width);
    let (buttons, _) = crate::panel::buttons(
        &[("Stop", true), ("Cancel", false)],
        usize::from(!ask.stop),
        theme,
    );
    let legend = crate::panel::legend(
        &[("←→", "choose"), ("enter", "press"), ("esc", "cancel")],
        theme,
    );
    let rest = "? The agent keeps running.";
    let inner = (text::str_width("Stop ") + text::str_width(&ask.command) + text::str_width(rest))
        .max(crate::panel::content_width(std::slice::from_ref(&legend)))
        .clamp(36, 60)
        .min(width.saturating_sub(8));
    let room = inner.saturating_sub(text::str_width("Stop ") + text::str_width(rest));
    let sentence = Line::from(vec![
        Span::styled("Stop ", theme.text()),
        Span::styled(text::ellipsize(&ask.command, room.max(8)), theme.code()),
        Span::styled(rest, theme.text()),
    ]);
    let rows = vec![Line::default(), sentence, Line::default(), buttons, legend];
    let lines = crate::panel::bordered("Stop", rows, inner, &[], None, theme);
    crate::panel::centre(paint, lines, area);
}

impl ChatView {
    /// The chat: a top line naming the agent and where it
    /// stands, the feed as turns, and the composer boxed with the model and
    /// mode on its bottom edge. Returns the page to ask for, as `draw` does.
    #[allow(clippy::too_many_arguments)]
    fn draw_turns(
        &mut self,
        paint: &mut Paint<'_>,
        area: Rect,
        state: &SessionState,
        family: Option<&FamilyHeader>,
        footer: Option<Line<'static>>,
        now_ms: i64,
        theme: Theme,
    ) -> Option<u32> {
        // The in-flight pane beside the chat narrows everything under the
        // header, unless the chat would be left too narrow: then it lies
        // over the feed's right side instead.
        let full = usize::from(area.width);
        let side = self.pane_open;
        let pane_width = (full / 3).clamp(PANE_MIN, PANE_MAX).min(full);
        let beside = side && full.saturating_sub(pane_width) >= CHAT_MIN;
        let width = if beside { full - pane_width } else { full };
        // Lying over the feed, the pane leaves the feed the columns to its
        // left, and the feed's words wrap there rather than hide under it.
        let feed_width = if side { full - pane_width } else { full };
        let name = state.agent().name.clone();
        let host = state
            .host()
            .map(|host| host.name().to_owned())
            .unwrap_or_else(|| "its host".into());

        let overview = self.overview(state);
        let sign_in = ui_view::sign_in(state);
        let summary = ui_view::controls(state);
        let items = pane::Contents {
            overview: &overview,
            folded: &self.pane_folds,
        }
        .items();
        let pane_keys = self.pane_open && self.pane_keys;
        if pane_keys {
            self.pane_focus = self.pane_at(&items).map(|at| items[at].clone());
        }
        // The item drawn highlighted, in the pane and, for a job, at its
        // step in the feed: the one under the mouse, else the one the
        // pane's keys are on.
        let lit = self
            .pane_hover
            .clone()
            .filter(|hovered| items.contains(hovered))
            .or(self.pane_focus.clone().filter(|_| pane_keys));
        let (header, controls) = self.top_line(state, &name, full, theme);
        // A blank line above the header keeps it off the terminal's edge.
        self.header_spots = controls
            .into_iter()
            .map(|((from, to), control)| {
                (
                    area.y + 1,
                    (area.x + from as u16, area.x + to as u16),
                    control,
                )
            })
            .collect();
        let mut top: Vec<Line<'static>> = vec![Line::default(), header];
        if let Some(family) = family {
            let mut line = family_line(family, width, theme);
            for span in &mut line.spans {
                if span.style == theme.muted() {
                    span.style = theme.faint();
                }
            }
            top.push(line);
        }
        top.push(Line::default());

        let mut bottom: Vec<Line<'static>> = Vec::new();
        let view = composer(state, now_ms);
        self.note_sent(state, now_ms);
        // The running turn's live end, after its newest row and scrolled
        // with it: a prompt on its way while the agent was idle, then what
        // the agent is doing, where "Worked 6m" stands once the turn ends.
        let mut tail: Vec<Line<'static>> = Vec::new();
        for prompt in ui_view::prompts_underway(state, |id| self.in_feed(state, id)) {
            if prompt.lands != ui_view::Lands::Feed {
                continue;
            }
            let ui_view::Underway::Sending { waiting } = prompt.underway else {
                continue;
            };
            let Some(seen) = self.sending.get(&prompt.input_id) else {
                continue;
            };
            let when = if waiting {
                format!("waiting for {host}…")
            } else {
                feed::clock(seen.at_ms)
            };
            tail.extend(feed::pending_prompt(&prompt.text, &when, feed_width, theme));
        }
        if let Some(activity) = &view.activity {
            tail.push(quiet_activity(activity, feed_width, theme));
            tail.push(Line::default());
        }
        // An exited agent's feed ends saying so, like a session boundary,
        // unless the agent wrote that boundary itself.
        if let PhaseView::Exited { cause } = state.phase()
            && !ends_with_exit(state)
        {
            tail.push(rows::rule(&exit_words(&cause), feed_width, theme));
            tail.push(Line::default());
        }
        self.feed_tail = tail;
        let mut row_at = None;
        // Prompts queued behind the running turn wait just above the
        // composer, one line each in the order they will run. At most
        // QUEUED_SHOWN show, the newest unless walking up through them
        // brings earlier ones in; one line says how many more wait, on the
        // side they are on, so the block's height never changes as it moves.
        const QUEUED_SHOWN: usize = 3;
        let entries = self.queue_entries(state, &host);
        if let Some(selected) = self.tray {
            if entries.is_empty() {
                self.tray = None;
            } else if selected >= entries.len() {
                self.tray = Some(entries.len() - 1);
            }
        }
        let mut queued_at: Vec<(usize, usize, feed::Queued)> = Vec::new();
        if !entries.is_empty() {
            let latest = entries.len().saturating_sub(QUEUED_SHOWN);
            let mut from = self.queued_from.min(latest);
            match self.tray {
                Some(i) if i < from => from = i,
                Some(i) if i >= from + QUEUED_SHOWN => from = i + 1 - QUEUED_SHOWN,
                Some(_) => {}
                None => from = latest,
            }
            self.queued_from = from;
            let more = entries.len().saturating_sub(QUEUED_SHOWN);
            let more_line = || {
                Line::from(Span::styled(
                    format!("{}+{more} more queued", " ".repeat(feed::WORDS)),
                    theme.faint(),
                ))
            };
            // Earlier ones hidden: the line goes above; only later ones
            // hidden, it goes below.
            let more_above = more > 0 && from > 0;
            if more_above {
                bottom.push(more_line());
            }
            for (i, entry) in entries.iter().enumerate().skip(from).take(QUEUED_SHOWN) {
                let lit = self.tray == Some(i) || self.queued_hover == Some(i);
                let drawn = feed::queued_line(entry, lit, width, theme);
                bottom.push(drawn.line.clone());
                queued_at.push((bottom.len() - 1, i, drawn));
            }
            if more > 0 && !more_above {
                bottom.push(more_line());
            }
        }
        // The row rests on the composer's box; a blank line sets it apart
        // from whatever is above. It is the pane folded: while the pane is
        // open it takes the row's place.
        let running = overview.jobs.len();
        self.servers_failing = overview
            .failed_servers
            .iter()
            .map(|server| server.name.clone())
            .collect();
        let servers_new = self
            .servers_failing
            .iter()
            .any(|name| !self.servers_told.contains(name));
        if !side
            && let Some(line) = edge_row(
                &overview,
                servers_new,
                running,
                !self.editor.is_empty(),
                now_ms,
                width,
                theme,
            )
        {
            if !bottom.is_empty() {
                bottom.push(Line::default());
            }
            row_at = Some((bottom.len(), text::line_width(&line)));
            bottom.push(line);
        }

        let mut cursor = None;
        let card = self.card(state);
        match &card {
            Some(card)
                if self
                    .ask_drawn
                    .as_ref()
                    .is_none_or(|(key, _)| *key != card.key) =>
            {
                self.ask_drawn = Some((card.key.clone(), now_ms));
            }
            Some(_) => {}
            None => self.ask_drawn = None,
        }
        // A plan stays in the feed, as the agent's text, while the box asks.
        self.asking = card
            .as_ref()
            .filter(|card| ask::boxed(card) && !matches!(card.body, AskBody::Plan { .. }))
            .map(|card| card.item_key.clone());
        // A plan arriving while the reader follows opens at its first line,
        // to be read from the top; one that fits leaves the feed following.
        let plan_arrived = match card.as_ref() {
            Some(card)
                if matches!(card.body, AskBody::Plan { .. })
                    && card.state == CardState::Open
                    && self.plan_seen.as_ref() != Some(&card.item_key) =>
            {
                self.plan_seen = Some(card.item_key.clone());
                if self.anchor == Anchor::Bottom {
                    self.anchor = Anchor::Top {
                        key: card.item_key.clone(),
                        offset: 0,
                    };
                    self.revealed = true;
                    true
                } else {
                    false
                }
            }
            _ => false,
        };
        // Where the composer's box sits among the bottom lines, once drawn.
        let mut boxed_at: Option<(usize, Boxed)> = None;
        let mut composer_box = |bottom: &mut Vec<Line<'static>>,
                                cursor: &mut Option<(usize, usize)>,
                                editor: &Editor,
                                takes_keys: bool| {
            // Kept from sending, or exited, the box's edge says why; the
            // empty field still invites the draft.
            let invite = format!("Message {name}");
            let boxed = boxed_composer(editor, &invite, &summary, !pane_keys, width, theme);
            if takes_keys {
                *cursor = Some((bottom.len() + boxed.cursor.0, boxed.cursor.1));
            }
            let at = bottom.len();
            bottom.extend(boxed.lines.iter().cloned());
            boxed_at = Some((at, boxed));
        };
        let legend = || {
            if pane_keys {
                let stoppable = matches!(
                    &self.pane_focus,
                    Some(pane::PaneItem::Job(job)) if pane::stop_key(job).is_some()
                );
                Hint::Keys(if stoppable {
                    keys(&[
                        ("j/k", "move"),
                        ("enter", "open"),
                        ("x", "stop"),
                        ("esc", "back"),
                        ("ctrl+o", "close"),
                    ])
                } else {
                    keys(&[
                        ("j/k", "move"),
                        ("enter", "open"),
                        ("esc", "back"),
                        ("ctrl+o", "close"),
                    ])
                })
            } else {
                turn_hint(state, &self.editor, self.reach, self.leader)
            }
        };
        // The app's footer, when it has one, takes the line over.
        let mut hint;
        // Where the plain composer's box starts, to mark its top edge.
        let mut plain_box: Option<usize> = None;
        // A permission ask takes over the composer's box: its edge in the
        // attention ink, the draft kept behind it.
        let mut ask_box: Option<(usize, Vec<ask::Spot>)> = None;
        let mut ask_mode: Option<(usize, (usize, usize))> = None;
        match &card {
            Some(card) if ask::boxed(card) => {
                const MARGIN: usize = 2;
                let inner = width.saturating_sub(2 * MARGIN + 4).max(1);
                self.ask.set_room(usize::from(area.height), self.attach);
                let drawn = self.ask.box_lines(card, inner, theme);
                let (lines, mode, _) =
                    framed(drawn.lines, theme.attention(), &summary, width, theme);
                let at = bottom.len();
                if let Some((row, col)) = drawn.cursor {
                    cursor = Some((at + 1 + row, MARGIN + 2 + col));
                }
                if let Some(cols) = mode {
                    ask_mode = Some((at + lines.len() - 1, cols));
                }
                ask_box = Some((at, drawn.spots));
                bottom.extend(lines);
                let stop: &[(&str, &str)] = if card.stops_turn {
                    &[("ctrl+x", "interrupt")]
                } else {
                    &[]
                };
                let more = format!("ctrl+{}", self.leader);
                let more = &[(more.as_str(), "more")];
                hint = Hint::Keys(match self.ask.question_hint(card, self.leader) {
                    Some(question) => question,
                    None if self.ask.in_box_note(card) => {
                        keys(&[&[("enter", "send"), ("esc", "clear")], stop].concat())
                    }
                    None if !matches!(card.state, CardState::Open | CardState::Rejected(_)) => {
                        keys(&[stop, more].concat())
                    }
                    None if self.ask.on_noted_deny(card) => {
                        keys(&[&[("enter", "choose"), ("tab", "note")], stop, more].concat())
                    }
                    None => keys(&[&[("enter", "choose")], stop, more].concat()),
                });
            }
            // The agent's account needs signing in: the box says how, as an
            // ask would, and keeps the draft for afterwards. An ask the
            // agent exited with leaves the composer to say it exited.
            _ if let Some(sign_in) = &sign_in => {
                let (lines, mode) =
                    sign_in_box(state.kind(), &host, sign_in, &summary, width, theme);
                if let Some(cols) = mode {
                    ask_mode = Some((bottom.len() + lines.len() - 1, cols));
                }
                bottom.extend(lines);
                hint = Hint::Keys(keys(&[(&format!("ctrl+{}", self.leader), "more")]));
            }
            _ => {
                plain_box = Some(bottom.len());
                composer_box(
                    &mut bottom,
                    &mut cursor,
                    &self.editor,
                    self.tray.is_none() && self.picker.is_none(),
                );
                hint = match self.tray.and_then(|i| entries.get(i)) {
                    Some(entry) => Hint::Keys(keys(entry.hint())),
                    None => legend(),
                };
            }
        }
        // Ctrl+S: its legend, at once; a picker: its keys.
        if let Some(picker) = &self.picker {
            hint = Hint::Keys(keys(&picker.hints()));
            if footer.is_none() {
                cursor = None;
            }
        } else if self.setting_prefix {
            let view = ui_view::settings(state);
            let mut pairs = Vec::new();
            for (setting, pair) in [
                (&view.changeable.model, ("m", "model")),
                (&view.changeable.effort, ("e", "effort")),
                (&view.changeable.permission, ("p", "permission")),
            ] {
                if *setting == Changeable::Pick {
                    pairs.push(pair);
                }
            }
            hint = if pairs.is_empty() {
                Hint::Sentence(
                    crate::words::by_typing_words(&view.changeable.model, &view.changeable.effort)
                        .unwrap_or_else(|| "Nothing here can change from amux".to_owned()),
                )
            } else {
                pairs.push(("esc", "back"));
                Hint::Keys(keys(&pairs))
            };
        }
        // The plain composer's top edge says what stands in the way of
        // sending, most pressing first: the last prompt refused (error
        // ink), Claude's own view on another conversation (warning ink),
        // the link to the host down, an exited agent that Enter resumes, a
        // usage limit reached (warning ink; sending stays open).
        if let Some(at) = plain_box {
            let words = edge_title(
                self.not_sent.as_deref(),
                ui_view::switched_away(state),
                &state.composer(),
                &host,
                self.reach,
                composer::limit_reached(&overview, now_ms),
                theme,
            );
            if let Some((words, ink)) = words
                && let Some(top) = bottom.get_mut(at)
            {
                *top = marked_edge_in(&words, ink, width, theme);
            }
        }
        // A blank line lets the composer's box breathe above the keys.
        bottom.push(Line::default());
        let (hint, hint_keys) = match footer {
            Some(footer) => (footer, Vec::new()),
            None => hint_line(&hint, width, theme),
        };
        let hint_row = bottom.len();
        bottom.push(hint);

        let height = usize::from(area.height);
        let feed_height = height.saturating_sub(top.len() + bottom.len());
        self.feed = (feed_width, feed_height);
        self.feed_origin = (area.x, area.y + top.len() as u16);
        let feed_top = top.len();
        let mut lines = top;
        self.pin_spot = None;
        self.jump_spot = None;
        self.panel_spots.clear();
        if state.transcript().is_empty() {
            lines.extend(empty_feed(
                state,
                self.reach,
                feed_height,
                now_ms - self.opened_at_ms,
                width,
                theme,
            ));
            self.laid = Laid::default();
        } else {
            // A run grown since the last frame is held by its newest step
            // again, so trimming its oldest steps never folds it.
            ui_view::keep_open_runs(state, &mut self.open_runs);
            let mut laid = self.frame(theme).layout(state);
            // A step revealed within the last screenful cannot reach the
            // top; the feed simply shows the newest row.
            if std::mem::take(&mut self.revealed) && laid.at_bottom {
                self.anchor = Anchor::Bottom;
            }
            // Held at a line from which the rest no longer fills the feed
            // (a fold just closed, say), the feed shows the newest row: it
            // follows again.
            if laid.at_bottom {
                self.anchor = Anchor::Bottom;
            }
            // The pinned prompt would cover the plan's first lines, or the
            // lines a page down brought to the top: they start just below
            // it instead.
            if (plan_arrived || std::mem::take(&mut self.paged))
                && !laid.at_bottom
                && let Some((pinned, _)) = pinned(state, &laid, feed_height, feed_width, theme)
            {
                self.anchor = self
                    .frame(theme)
                    .scrolled(state, &laid, -(pinned.len() as isize));
                laid = self.frame(theme).layout(state);
            }
            let mut feed: Vec<Line<'static>> = laid.lines.clone();
            text::drop_cut_padding(&mut feed);
            if let Some((pinned, key)) = pinned(state, &laid, feed_height, feed_width, theme) {
                let rows = pinned.len();
                // The blank line under the block is not the pin's to click.
                let block_rows = rows.saturating_sub(1);
                for (at, line) in pinned.into_iter().enumerate() {
                    if let Some(slot) = feed.get_mut(at) {
                        *slot = line;
                    }
                }
                let y = area.y + feed_top as u16;
                self.pin_spot = Some(((y, y + block_rows as u16), key));
                // The line under the pin is the feed's new edge: a cut
                // block's padding there would read as a stray band.
                if let Some(rest) = feed.get_mut(rows..) {
                    text::drop_cut_padding(rest);
                }
            }
            // The lit job's step, when the feed shows it, carries the same
            // card, so the pane's line and the feed's line read as one.
            if let Some(pane::PaneItem::Job(job)) = lit.as_ref()
                && let Some(block) = laid
                    .blocks
                    .iter()
                    .position(|block| job.step.as_ref() == Some(&block.key))
            {
                let owners = line_owners(&laid, feed.len());
                // Exactly the step's own lines, flat: a block may end in
                // the blank line that sets it apart, which is not the step's.
                let rows: Vec<usize> = owners
                    .iter()
                    .enumerate()
                    .filter(|(at, owner)| {
                        **owner == Some(block) && text::line_width(&feed[*at]) > 0
                    })
                    .map(|(at, _)| at)
                    .collect();
                if let (Some(first), Some(last)) = (rows.first(), rows.last()) {
                    pane::card_highlight(
                        &mut feed,
                        *first..*last + 1,
                        2,
                        width.saturating_sub(2),
                        false,
                        theme,
                    );
                }
            }
            if self.anchor != Anchor::Bottom
                && let Some(last) = feed.len().checked_sub(1)
            {
                // The control sits in the blank line that sets the feed
                // apart from what is below it, never over history.
                let (control, from) = jump_control(state, width, theme);
                let control_width = text::line_width(&control);
                feed[last] = text::overlay(&Line::default(), from, control, width);
                text::drop_cut_padding(&mut feed[..last]);
                self.jump_spot = Some((
                    area.y + (feed_top + last) as u16,
                    (area.x + from as u16, area.x + (from + control_width) as u16),
                ));
            }
            lines.extend(feed);
            self.laid = laid;
        }
        let bottom_start = lines.len();
        let bottom_y = |row: usize| area.y + (bottom_start + row) as u16;
        self.hint_spots = hint_keys
            .into_iter()
            .map(|((from, to), key)| {
                (
                    bottom_y(hint_row),
                    (area.x + from as u16, area.x + to as u16),
                    key,
                )
            })
            .collect();
        self.queued_spots = queued_at
            .into_iter()
            .map(|(at, index, drawn)| QueuedSpot {
                row: bottom_y(at),
                index,
                controls: drawn
                    .controls
                    .iter()
                    .map(|(from, to)| (area.x + *from as u16, area.x + *to as u16))
                    .collect(),
            })
            .collect();
        self.row_spot = match row_at {
            Some((at, row_width)) if !self.pane_open => {
                Some((bottom_y(at), (area.x + 2, area.x + row_width as u16)))
            }
            _ => None,
        };
        self.pane_spots.clear();
        self.pane_rect = None;
        self.mode_spot = None;
        self.composer_spot = None;
        self.ask_spots = match ask_box {
            Some((at, spots)) => spots
                .into_iter()
                .map(|(row, (from, to), spot)| {
                    (
                        bottom_y(at + 1 + row),
                        (area.x + (4 + from) as u16, area.x + (4 + to) as u16),
                        spot,
                    )
                })
                .collect(),
            None => Vec::new(),
        };
        if let Some((row, (from, to))) = ask_mode {
            self.mode_spot = Some((bottom_y(row), (area.x + from as u16, area.x + to as u16)));
        }
        if let Some((at, boxed)) = &boxed_at {
            if let Some((from, to)) = boxed.mode {
                self.mode_spot = Some((
                    bottom_y(at + boxed.lines.len() - 1),
                    (area.x + from as u16, area.x + to as u16),
                ));
            }
            self.composer_spot = Some(ComposerSpot {
                x: area.x + boxed.text_x as u16,
                y: bottom_y(at + 1),
                rows: boxed.lines.len().saturating_sub(2) as u16,
                skip: boxed.skip,
                wrap: boxed.wrap,
            });
        }
        lines.extend(bottom);
        paint.render_widget(Paragraph::new(lines), area);
        if side {
            // Beside the chat from the feed's first line down to the line
            // above the keys; over the feed alone when it lies on top.
            let x = area.x + (full - pane_width) as u16;
            let y = area.y + feed_top as u16;
            let bottom_edge = if beside {
                area.y + area.height.saturating_sub(1)
            } else {
                y + feed_height as u16
            };
            let rect = Rect {
                x,
                y,
                width: pane_width as u16,
                height: bottom_edge.saturating_sub(y),
            };
            if !beside {
                // Lying over the feed, it clears a column more than it
                // draws, so the feed's cut text never touches the rule.
                let cleared = Rect {
                    x: rect.x.saturating_sub(1),
                    width: rect.width + 1,
                    ..rect
                };
                paint.render_widget(ratatui::widgets::Clear, cleared);
            }
            self.pane_rect = Some(rect);
            self.draw_side_pane(paint, rect, &overview, lit.as_ref(), now_ms, theme);
        }
        // A model or effort being chosen: its flyover rises from the model's
        // words on the composer's edge, over the box (and the pane).
        let mut flyover_cursor = None;
        if let (Some(picker), Some((at, boxed))) = (&self.picker, &boxed_at) {
            let anchor = area.x + boxed.label.unwrap_or(4) as u16;
            let above = (area.y as usize + bottom_start + at) as u16;
            self.flyover = crate::setup::draw_flyover(paint, picker, anchor, above, area, theme);
            flyover_cursor = self.flyover.cursor;
        }
        if let Some(at) = flyover_cursor {
            paint.set_cursor_position(at);
        }
        // The leader held: its keys rise from the hint line's `ctrl+a
        // more`, in a flyover like the settings', over the feed.
        if let Some(entries) = self.panel.clone() {
            let leader = KeyEvent::new(KeyCode::Char(self.leader), KeyModifiers::CONTROL);
            let (anchor, above) = self
                .hint_spots
                .iter()
                .find(|(_, _, key)| *key == leader)
                .map_or(
                    (area.x + 2, area.y + area.height.saturating_sub(1)),
                    |(y, (from, _), _)| (*from, *y),
                );
            let (lines, chords) = which_key_panel(&entries, self.leader, width, theme);
            if let Some((rect, skip)) = crate::panel::rise(paint, lines, anchor, above, area) {
                for (i, chord) in chords.into_iter().enumerate().skip(skip) {
                    if let Some(chord) = chord {
                        self.panel_spots.push((
                            rect.y + (i - skip) as u16,
                            (rect.x, rect.x + rect.width),
                            chord,
                        ));
                    }
                }
            }
        }
        // While the pane has the keys, the composer shows no cursor.
        if let Some((row, col)) = cursor.filter(|_| !pane_keys && self.picker.is_none()) {
            let y = area.y as usize + bottom_start + row;
            let x = area.x as usize + col.min(width.saturating_sub(1));
            if y < (area.y + area.height) as usize {
                paint.set_cursor_position(Position::new(x as u16, y as u16));
            }
        }
        let page = if state.transcript().is_empty() {
            state.transcript().has_older().then_some(layout::PAGE)
        } else {
            self.laid.page
        };
        match page {
            Some(n) if self.page_asked != state.oldest_order() || self.page_asked.is_none() => {
                Some(n)
            }
            _ => None,
        }
    }

    /// The pane beside the chat: a hairline rule down its left edge, then
    /// its lines past the margin, cut at the rect's foot.
    #[allow(clippy::too_many_arguments)]
    fn draw_side_pane(
        &mut self,
        paint: &mut Paint<'_>,
        rect: Rect,
        overview: &ui_view::Overview,
        lit: Option<&pane::PaneItem>,
        now_ms: i64,
        theme: Theme,
    ) {
        // The rule, a blank column, then the pane's lines out to the outer
        // margin: a highlighted item's tint spans them, its words sit two
        // columns further in, as on home.
        const LEFT: u16 = 2;
        const RIGHT: u16 = 2;
        // The title line and the blank under it stay; the rest scrolls.
        const TITLE: u16 = 2;
        let inner = rect.width.saturating_sub(LEFT + RIGHT);
        let focused = self.pane_keys;
        let content = pane::pane_lines(
            &pane::Contents {
                overview,
                folded: &self.pane_folds,
            },
            lit,
            focused,
            now_ms,
            usize::from(inner),
            theme,
        );
        // Which area has the keys shows on the frame, never the content:
        // the rule turns grey while the pane has them.
        let rule_ink = if focused {
            theme.muted()
        } else {
            theme.hairline()
        };
        let rule: Vec<Line<'static>> = (0..rect.height)
            .map(|_| Line::from(Span::styled("│", rule_ink)))
            .collect();
        paint.render_widget(
            Paragraph::new(rule),
            Rect {
                width: 1.min(rect.width),
                ..rect
            },
        );
        let x = rect.x + LEFT;
        for ((from, to), hit) in content.title_hits.iter().cloned() {
            self.pane_spots
                .push((rect.y, (x + from as u16, x + to as u16), hit));
        }
        paint.render_widget(
            Paragraph::new(vec![content.title.clone()]),
            Rect {
                x,
                width: inner,
                height: 1.min(rect.height),
                ..rect
            },
        );

        // The body: one list, scrolled so the keys' item keeps a line of
        // room from each edge when they move it.
        let height = usize::from(rect.height.saturating_sub(TITLE));
        self.pane_page = height;
        let len = content.body.len();
        let max = len.saturating_sub(height);
        if self.pane_follow {
            self.pane_follow = false;
            let focus_line = self.pane_focus.as_ref().and_then(|focus| {
                content
                    .item_lines
                    .iter()
                    .find(|(item, _)| item == focus)
                    .map(|(_, line)| *line)
            });
            if let Some(line) = focus_line.filter(|_| self.pane_keys) {
                // One line of room, one more for an edge's count and, at
                // the top, one more for a pinned directory.
                const ROOM: usize = 2;
                const TOP_ROOM: usize = ROOM + 1;
                if line < self.pane_scroll + TOP_ROOM {
                    self.pane_scroll = line.saturating_sub(TOP_ROOM);
                } else if line + ROOM + 1 > self.pane_scroll + height {
                    self.pane_scroll = (line + ROOM + 1).saturating_sub(height);
                }
                // Scrolled only past a heading, the list starts at its top,
                // as long as the item still clears the bottom edge's line.
                if pane::hidden(&content, self.pane_scroll, height, 0)
                    .0
                    .is_none()
                    && line + 2 <= height
                {
                    self.pane_scroll = 0;
                }
            }
        }
        self.pane_scroll = self.pane_scroll.min(max);
        let top = self.pane_scroll;
        let (mut above, below) = pane::hidden(&content, top, height, 0);
        // Scrolled into a group, its directory stays pinned on the first
        // line under the top edge, over the row there, so nothing moves.
        let pin_row = top + usize::from(above.is_some());
        let pin = (pin_row < top + height)
            .then(|| content.pin(pin_row))
            .flatten();
        if pin.is_some() {
            above = pane::hidden(&content, top, height, 1).0.or(above);
        }
        let mut shown: Vec<Line<'static>> = content
            .body
            .iter()
            .skip(top)
            .take(height)
            .cloned()
            .collect();
        let body_y = rect.y + TITLE;
        for (row, (from, to), hit) in content.hits {
            if (top..top + height).contains(&row) {
                self.pane_spots.push((
                    body_y + (row - top) as u16,
                    (x + from as u16, x + to as u16),
                    hit,
                ));
            }
        }
        // Cut content says how much lies past each edge, on the edge's own
        // line; clicking it turns a page.
        if let Some(above) = above
            && !shown.is_empty()
        {
            shown[0] = pane::more_line(true, above, theme);
            self.pane_spots.retain(|(row, _, _)| *row != body_y);
            self.pane_spots
                .push((body_y, (x, x + inner), pane::PaneHit::Page { up: true }));
        }
        if let Some(dir) = pin
            && let Some(slot) = shown.get_mut(pin_row - top)
        {
            *slot = content.body[dir].clone();
            let row = body_y + (pin_row - top) as u16;
            self.pane_spots.retain(|(each, _, _)| *each != row);
        }
        if let Some(below) = below
            && let Some(last) = shown.len().checked_sub(1)
        {
            shown[last] = pane::more_line(false, below, theme);
            let row = body_y + last as u16;
            self.pane_spots.retain(|(each, _, _)| *each != row);
            self.pane_spots
                .push((row, (x, x + inner), pane::PaneHit::Page { up: false }));
        }
        paint.render_widget(
            Paragraph::new(shown),
            Rect {
                x,
                y: body_y,
                width: inner,
                height: height as u16,
            },
        );
    }

    /// The header. At the left the agent's name, then faint where it runs:
    /// its project, and its host when that is not this machine. At the
    /// right how much of its context is used, the working tree's `[Diff]`
    /// and `[Home]`. Where the chat stands is not repeated here, since the
    /// feed shows work and asks; only a problem is (host away,
    /// reconnecting, catching up, exited). Returns the line and the
    /// controls' column ranges.
    fn top_line(
        &self,
        state: &SessionState,
        name: &str,
        width: usize,
        theme: Theme,
    ) -> (Line<'static>, HeaderSpots) {
        let host = state
            .host()
            .map(|host| host.name().to_owned())
            .unwrap_or_default();
        // The right side, built first so the left knows its room.
        let mut right: Vec<Span<'static>> = Vec::new();
        // Groups on both sides are split by the same faint bar.
        let bar = || Span::styled(" │ ", theme.faint());
        let gap = |right: &mut Vec<Span<'static>>| {
            if !right.is_empty() {
                right.push(bar());
            }
        };
        if let Some((words, style)) = problem_words(state, self.reach, &host, theme) {
            right.push(Span::styled(words, style));
        }
        if let Some(context) = ui_view::context(state) {
            gap(&mut right);
            let used = tokens_short(context.used_tokens);
            let words = match context.window_tokens {
                Some(window) => format!("{used} / {}", tokens_short(window)),
                None => used,
            };
            let style = if context.near_full {
                theme.warning()
            } else {
                theme.faint()
            };
            right.push(Span::styled(words, style));
        }
        let diff = diff_words(state, self.comparison);
        let mut controls = Vec::new();
        for (words, control) in [
            (diff, HeaderControl::Diff),
            ("[Home]".to_owned(), HeaderControl::Home),
        ] {
            gap(&mut right);
            let style = if self.hover == Some(control) {
                theme.emphasis()
            } else {
                theme.muted()
            };
            controls.push((right.len(), text::str_width(&words), control));
            right.push(Span::styled(words, style));
        }
        let right_width: usize = right
            .iter()
            .map(|span| text::str_width(&span.content))
            .sum();
        let end = width.saturating_sub(2);
        let right_at = end.saturating_sub(right_width);

        let mut line = Line::from(Span::raw("  "));
        let room = right_at.saturating_sub(2);
        push(&mut line, name, theme.bright(), room);
        // Where it works: its branch and its path as the person would write
        // it (`main ~/source/amux`), and its host when that is not this
        // machine.
        let cwd = &state.agent().cwd;
        let mut place = if self.local {
            text::tilde(cwd)
        } else {
            cwd.clone()
        };
        if let Some(branch) = state
            .agent()
            .git
            .as_ref()
            .and_then(|git| git.branch.as_deref())
        {
            place = format!("{branch} {place}");
        }
        if !self.local && !host.is_empty() {
            place = format!("{place} · {host}");
        }
        if !place.is_empty() && text::line_width(&line) + 6 < room {
            line.spans.push(bar());
            push(&mut line, place, theme.faint(), room);
        }
        let mut spots = Vec::new();
        if text::line_width(&line) + 2 <= right_at {
            text::pad_to(&mut line, right_at);
            let mut at = right_at;
            for (i, span) in right.into_iter().enumerate() {
                if let Some((_, span_width, control)) =
                    controls.iter().find(|(index, _, _)| *index == i)
                {
                    spots.push(((at, at + span_width), *control));
                }
                at += text::str_width(&span.content);
                line.spans.push(span);
            }
        }
        (line, spots)
    }
}

/// The row's change totals for `comparison`, as of the last turn end.
fn change_totals(state: &SessionState, comparison: Comparison) -> Option<wire::ChangeTotals> {
    let git = state.agent().git.as_ref()?;
    match comparison {
        Comparison::Uncommitted => git.uncommitted,
        Comparison::OnBranch => git.on_branch,
    }
}

/// The header's way to the review page, with the change totals of the
/// comparison shown: "[Diff +42 −7]" for uncommitted changes, "[Diff vs
/// main +120 −30]" for everything on the branch, a bare "[Diff]" with
/// nothing to count.
fn diff_words(state: &SessionState, comparison: Comparison) -> String {
    let against = match comparison {
        Comparison::Uncommitted => None,
        Comparison::OnBranch => state
            .agent()
            .git
            .as_ref()
            .and_then(|git| git.base_branch.as_deref()),
    };
    let mut words = "[Diff".to_owned();
    if let Some(base) = against {
        words.push_str(&format!(" vs {base}"));
    }
    if let Some(totals) = change_totals(state, comparison)
        .filter(|totals| totals.added + totals.removed > 0 || totals.files > 0)
    {
        words.push_str(&format!(" +{} −{}", totals.added, totals.removed));
    }
    words.push(']');
    words
}

/// "41K", "1.2M": a token count at a glance.
fn tokens_short(count: u64) -> String {
    match count {
        0..1_000 => count.to_string(),
        1_000..1_000_000 => format!("{}K", (count + 500) / 1_000),
        _ => {
            let tenths = (count + 50_000) / 100_000;
            if tenths.is_multiple_of(10) {
                format!("{}M", tenths / 10)
            } else {
                format!("{}.{}M", tenths / 10, tenths % 10)
            }
        }
    }
}

/// Where the chat stands, only when that is a problem the person should
/// see: the host away, reconnecting, catching up, a rebuilt history on its
/// way, or the agent exited. Working, idle and needs-you show in the feed.
fn problem_words(
    state: &SessionState,
    reach: Reach,
    host: &str,
    theme: Theme,
) -> Option<(String, ratatui::style::Style)> {
    let away_words = || {
        let host = if host.is_empty() { "host" } else { host };
        match reach {
            Reach::Offline(Offline::SignedOut) => {
                format!("{host} away · this machine is signed out")
            }
            Reach::Offline(Offline::Revoked) => format!("{host} no longer trusts this machine"),
            Reach::Away => format!("{host} away · {FREE_PLAN_AWAY}"),
            Reach::Online(_) | Reach::Offline(Offline::Plain) => {
                format!("{host} away · not current")
            }
        }
    };
    Some(match (state.composer(), state.phase()) {
        // Resuming asks the host, so why it is away matters more than why
        // the agent ended.
        (_, PhaseView::Exited { cause }) if !reach.online() => {
            let ended = match cause {
                ExitCause::Finished => "finished",
                ExitCause::Ended | ExitCause::Failed(_) => "exited",
            };
            (format!("{ended} · {}", away_words()), theme.warning())
        }
        (_, PhaseView::Exited { cause }) => (exit_words(&cause), theme.faint()),
        (Composer::Holds(Waiting::Detached), _) => (away_words(), theme.warning()),
        (Composer::Holds(Waiting::Reconnecting), _) => ("reconnecting".to_owned(), theme.warning()),
        (Composer::Holds(Waiting::CatchingUp), _) => ("catching up".to_owned(), theme.faint()),
        _ if state.reset_pending() => ("refreshing".to_owned(), theme.faint()),
        _ => return None,
    })
}

/// Why a setting cannot change from here, as a notice: how it does
/// change, or that the agent has not offered any yet.
fn unchangeable_words(
    view: &ui_view::SettingsView,
    setting: &Changeable,
    item: crate::setup::Item,
) -> String {
    use crate::setup::Item;
    match setting {
        Changeable::ByTyping(_) => {
            crate::words::by_typing_words(&view.changeable.model, &view.changeable.effort)
                .unwrap_or_default()
        }
        Changeable::Cycle => {
            "Terminal Claude changes permission only by cycling through its permissions.".to_owned()
        }
        Changeable::Pick | Changeable::NotOffered => {
            let what = match item {
                Item::Model => "models",
                Item::Effort => "efforts",
                _ => "permissions",
            };
            format!("the agent has not offered its {what} yet")
        }
    }
}

/// "finished", "exited", or "exited · its directory failed: it is gone":
/// how an exited agent's feed ends.
fn exit_words(cause: &ExitCause) -> String {
    match cause {
        ExitCause::Finished => "finished".to_owned(),
        ExitCause::Ended => "exited".to_owned(),
        ExitCause::Failed(cause) => format!("exited · {cause}"),
    }
}

/// Whether the agent's newest row is its own exit boundary.
fn ends_with_exit(state: &SessionState) -> bool {
    let transcript = state.transcript();
    let Some(held) = transcript.head().and_then(|head| transcript.at(head)) else {
        return false;
    };
    let everything = ChatOptions {
        tools: ToolRows::ShowAll,
    };
    chat_rows_for(state, std::slice::from_ref(&held.item.key), &everything)
        .last()
        .is_some_and(|row| {
            matches!(
                row.kind,
                RowKind::Boundary {
                    kind: wire::BoundaryKind::Exited,
                    ..
                }
            )
        })
}

/// The sign-in box in the composer's place, framed with the composer's
/// edge, and the columns of the mode's words on that edge.
fn sign_in_box(
    kind: wire::Kind,
    host: &str,
    sign_in: &ui_view::SignInView,
    controls: &ui_view::ControlsSummary,
    width: usize,
    theme: Theme,
) -> (Vec<Line<'static>>, Option<(usize, usize)>) {
    const MARGIN: usize = 2;
    let inner = width.saturating_sub(2 * MARGIN + 4).max(1);
    let body = sign_in_lines(kind, host, sign_in, inner, theme);
    let (lines, mode, _) = framed(body, theme.attention(), controls, width, theme);
    (lines, mode)
}

/// The sign-in box alone, its edge naming `model`, as the component
/// catalogue draws it.
#[cfg(feature = "fixtures")]
pub(crate) fn sign_in_composer_lines(
    kind: wire::Kind,
    host: &str,
    sign_in: &ui_view::SignInView,
    model: &str,
    width: usize,
    theme: Theme,
) -> Vec<Line<'static>> {
    let edge = ui_view::ControlsSummary {
        model: Some(model.to_owned()),
        ..Default::default()
    };
    sign_in_box(kind, host, sign_in, &edge, width, theme).0
}

/// The box for an agent whose account needs signing in: who, the account
/// and what went wrong, then how to sign in. Nothing here can do it for
/// you, so there are no choices.
fn sign_in_lines(
    kind: wire::Kind,
    host: &str,
    sign_in: &ui_view::SignInView,
    width: usize,
    theme: Theme,
) -> Vec<Line<'static>> {
    let (who, steps): (&str, Vec<(&str, bool)>) = match kind {
        wire::Kind::Codex => (
            "Codex",
            vec![("Run ", false), ("codex login", true), (" on ", false)],
        ),
        _ => (
            "Claude",
            vec![
                ("Run ", false),
                ("claude", true),
                (" and sign in with ", false),
                ("/login", true),
                (" on ", false),
            ],
        ),
    };
    let mut lines = Vec::new();
    let mut head = Line::from(Span::styled("● ", theme.attention()));
    push(
        &mut head,
        format!("{who} needs you to sign in"),
        theme.text(),
        width,
    );
    lines.push(head);
    lines.push(Line::default());
    let what = match sign_in.state {
        wire::SignInState::Expired => "Sign-in expired",
        wire::SignInState::Failed => "Sign-in failed",
        _ => "Signed out",
    };
    let mut status = Line::default();
    push(&mut status, what, theme.muted(), width);
    if !sign_in.account.is_empty() {
        push(
            &mut status,
            format!(" · {}", sign_in.account),
            theme.muted(),
            width,
        );
    }
    lines.push(status);
    for part in text::wrap(&sign_in.message, width.max(1)) {
        if part.is_empty() {
            continue;
        }
        let mut line = Line::default();
        push(&mut line, part, theme.faint(), width);
        lines.push(line);
    }
    lines.push(Line::default());
    let mut how = Line::default();
    for (words, code) in steps {
        push(
            &mut how,
            words,
            if code { theme.code() } else { theme.text() },
            width,
        );
    }
    push(&mut how, format!("{host}."), theme.text(), width);
    lines.push(how);
    lines
}

/// Why a host is away when its daemon lists it so: the relay alone sees
/// it, and on the free plan the relay opens no connection between hosts.
pub(crate) const FREE_PLAN_AWAY: &str = "the free plan does not relay between hosts";

/// Why the composer cannot send while the link to the agent's host is
/// down, for the box's edge; None when it can.
fn waiting_words(composer: &Composer, host: &str, reach: Reach) -> Option<String> {
    Some(match composer {
        Composer::Holds(Waiting::Detached) => match reach {
            Reach::Offline(Offline::Revoked) => format!("{host} no longer trusts this machine"),
            Reach::Offline(Offline::SignedOut) => {
                format!("{host} is away · this machine is signed out")
            }
            Reach::Away => format!("{host} is away · {FREE_PLAN_AWAY}"),
            Reach::Online(_) | Reach::Offline(Offline::Plain) => format!("{host} is away"),
        },
        // The client talks only to this machine's daemon: reconnecting is
        // that daemon gone, never the agent's host, as home says it.
        Composer::Holds(Waiting::Reconnecting) => "reconnecting to amux".to_owned(),
        Composer::Holds(Waiting::CatchingUp) => "catching up".to_owned(),
        Composer::Send | Composer::Resume => return None,
    })
}

/// The composer's top edge while terminal Claude's own view is on another
/// conversation: prompts are refused until it is back on this one.
const SWITCHED_AWAY_WORDS: &str =
    "Claude's own view switched to another conversation · ctrl+b switch back";

/// What the plain composer's top edge says stands in the way of sending,
/// most pressing first, with its ink: the last prompt refused, Claude's own
/// view on another conversation, the link to the host down, an exited agent
/// that Enter resumes, a usage limit reached (sending stays open).
fn edge_title(
    not_sent: Option<&str>,
    switched_away: bool,
    composer: &Composer,
    host: &str,
    reach: Reach,
    limit: Option<String>,
    theme: Theme,
) -> Option<(String, ratatui::style::Style)> {
    if let Some(reason) = not_sent {
        return Some((reason.to_owned(), theme.error()));
    }
    if switched_away {
        return Some((SWITCHED_AWAY_WORDS.to_owned(), theme.warning()));
    }
    waiting_words(composer, host, reach)
        .map(|words| (words, theme.faint()))
        .or_else(|| {
            (*composer == Composer::Resume).then(|| ("Enter resumes".to_owned(), theme.muted()))
        })
        .or_else(|| limit.map(|words| (words, theme.warning())))
}

/// The plain composer as the chat draws it for `name` on `host`: the draft
/// or the invitation in its box, and its top edge saying what stands in the
/// way. For the vocabulary.
#[cfg(feature = "fixtures")]
pub(crate) fn composer_lines(
    editor: &Editor,
    composer: &Composer,
    name: &str,
    host: &str,
    reach: Reach,
    width: usize,
    theme: Theme,
) -> Vec<Line<'static>> {
    let invite = format!("Message {name}");
    let edge = ui_view::ControlsSummary::default();
    let mut lines = boxed_composer(editor, &invite, &edge, true, width, theme).lines;
    if let Some((words, ink)) = edge_title(None, false, composer, host, reach, None, theme)
        && let Some(top) = lines.first_mut()
    {
        *top = marked_edge_in(&words, ink, width, theme);
    }
    lines
}

/// Why the agent or its host refused an input, in words: the refusal's own
/// when it has them.
pub(crate) fn refusal_words(refusal: &Refusal) -> String {
    if let Some(detail) = &refusal.detail {
        return detail.clone();
    }
    match refusal.reason {
        wire::Refusal::Exited => "it had exited",
        wire::Refusal::Exiting => "it was exiting",
        wire::Refusal::Draining => "its host is shutting down",
        wire::Refusal::HostUnreachable => "its host can't be reached",
        wire::Refusal::Unsupported => "this agent can't take it",
        wire::Refusal::ClosedAsk => "the ask had already closed",
        wire::Refusal::NotQueued => "it was no longer queued",
        wire::Refusal::ConversationElsewhere => {
            "Claude's own view switched to another conversation"
        }
        wire::Refusal::TooLong => "it is longer than the agent takes",
        wire::Refusal::Unspecified => "it was refused",
    }
    .to_owned()
}

/// What an agent is doing, as the activity line names it.
pub(crate) fn activity_words(kind: &ActivityKind) -> String {
    match kind {
        ActivityKind::Thinking => "Thinking".to_owned(),
        ActivityKind::Subagents { count } => format!(
            "{count} subagent{} working",
            if *count == 1 { "" } else { "s" }
        ),
        ActivityKind::Compacting => "Compacting".to_owned(),
        ActivityKind::SafetyCheck => "Running safety checks".to_owned(),
        ActivityKind::Retrying { attempt, .. } => format!("Retrying · attempt {attempt}"),
        ActivityKind::Working | ActivityKind::Running { .. } => "Working".to_owned(),
    }
}

/// While the agent works, one quiet line that it is, and for how long, in
/// whole seconds once there is one. What it is doing shows as live steps in
/// the feed, so a call in flight is the turn's work, timed with the turn.
fn quiet_activity(activity: &ui_state::Activity, width: usize, theme: Theme) -> Line<'static> {
    let ms = match activity.kind {
        ActivityKind::Working | ActivityKind::Running { .. } => activity.turn_elapsed_ms,
        _ => activity.elapsed_ms,
    };
    let words = activity_words(&activity.kind);
    let mut line = Line::from(Span::raw("    "));
    push(&mut line, words, theme.muted(), width);
    if ms >= 1_000 {
        let elapsed = text::duration(ms - ms % 1_000);
        push(&mut line, format!(" · {elapsed}"), theme.faint(), width);
    }
    line
}

/// What the composer's bottom edge says, in the words a person reads: the
/// model by its name, its effort, and the permission and mode.
/// The composer in its box, as drawn: its lines, the cursor's (line,
/// column), the mode's columns on the bottom edge, where the draft's words
/// start, how many wrapped lines are scrolled off above, and the width the
/// draft wraps at.
struct Boxed {
    lines: Vec<Line<'static>>,
    cursor: (usize, usize),
    mode: Option<(usize, usize)>,
    /// Where the model's words start on the bottom edge: a model or effort
    /// flyover rises from there.
    label: Option<usize>,
    text_x: usize,
    skip: usize,
    wrap: usize,
}

/// The composer in its box, the model, effort and mode on the bottom edge.
fn boxed_composer(
    editor: &Editor,
    placeholder: &str,
    controls: &ui_view::ControlsSummary,
    focused: bool,
    width: usize,
    theme: Theme,
) -> Boxed {
    const MARGIN: usize = 2;
    let inner = width.saturating_sub(2 * MARGIN + 4).max(1);
    let (mut body, (row, col)) = editor_lines(editor, placeholder, inner, theme);
    // The box already says where to type, so the edge mark becomes a prompt.
    for (i, line) in body.iter_mut().enumerate() {
        if let Some(first) = line.spans.first_mut() {
            *first = Span::styled(if i == 0 { "› " } else { "  " }, theme.muted());
        }
    }
    let skip = (row + 1).saturating_sub(COMPOSER_LINES);
    // Holding the keys, the box's edge is grey; otherwise it recedes to a
    // hairline. The words on its edge keep their own inks.
    let edge = if focused {
        theme.muted()
    } else {
        theme.hairline()
    };
    let body = body.into_iter().skip(skip).take(COMPOSER_LINES).collect();
    let (lines, mode, label) = framed(body, edge, controls, width, theme);
    Boxed {
        lines,
        cursor: (1 + row - skip, MARGIN + 2 + col),
        mode,
        label,
        text_x: MARGIN + 4,
        skip,
        wrap: inner,
    }
}

/// The composer box's top edge with `words` on it in `ink`, after a short
/// run of the edge.
fn marked_edge_in(
    words: &str,
    ink: ratatui::style::Style,
    width: usize,
    theme: Theme,
) -> Line<'static> {
    const MARGIN: usize = 2;
    let edge = theme.muted();
    let span = width.saturating_sub(2 * MARGIN + 2);
    let mut top = Line::from(Span::raw(" ".repeat(MARGIN)));
    push(&mut top, "╭─ ", edge, width);
    push(&mut top, words, ink, width);
    push(&mut top, " ", edge, width);
    let used = text::line_width(&top) - MARGIN - 1;
    push(&mut top, "─".repeat(span.saturating_sub(used)), edge, width);
    push(&mut top, "╮", edge, width);
    top
}

/// `body` in the composer's box at the margin, its edge in `edge`, with
/// the model, effort and mode on the bottom edge. Returns the lines, the
/// mode's columns on the last line, for clicks, and where the edge's words
/// start.
fn framed(
    body: Vec<Line<'static>>,
    edge: ratatui::style::Style,
    controls: &ui_view::ControlsSummary,
    width: usize,
    theme: Theme,
) -> (Vec<Line<'static>>, Option<(usize, usize)>, Option<usize>) {
    const MARGIN: usize = 2;
    let span = width.saturating_sub(2 * MARGIN + 2);
    let mut out = Vec::new();
    let mut top = Line::from(Span::raw(" ".repeat(MARGIN)));
    push(&mut top, "╭", edge, width);
    push(&mut top, "─".repeat(span), edge, width);
    push(&mut top, "╮", edge, width);
    out.push(top);
    for line in body {
        let mut boxed = Line::from(Span::raw(" ".repeat(MARGIN)));
        push(&mut boxed, "│ ", edge, width);
        boxed.spans.extend(line.spans);
        text::pad_to(&mut boxed, width - MARGIN - 1);
        push(&mut boxed, "│", edge, width);
        out.push(boxed);
    }
    // "Opus (high) · accept edits": the model faint by its name, its
    // effort beside it, and the permission and mode, which Shift+Tab (or a
    // click) changes, in the reading ink; normal ones are left unsaid.
    let known = |fact: &Option<String>| fact.clone().filter(|fact| !fact.is_empty());
    let mut label: Vec<Span<'static>> = Vec::new();
    match (known(&controls.model), known(&controls.effort)) {
        (Some(model), Some(effort)) => {
            label.push(Span::styled(format!("{model} ({effort})"), theme.faint()))
        }
        (Some(model), None) => label.push(Span::styled(model, theme.faint())),
        (None, Some(effort)) => label.push(Span::styled(format!("({effort})"), theme.faint())),
        (None, None) => {}
    }
    let mode_words = crate::words::control_words(controls).filter(|words| !words.is_empty());
    if let Some(mode) = &mode_words {
        if !label.is_empty() {
            label.push(Span::styled(" · ", theme.faint()));
        }
        label.push(Span::styled(mode.clone(), theme.text()));
    }
    let label_width: usize = label
        .iter()
        .map(|part| text::str_width(&part.content))
        .sum();
    let mut bottom = Line::from(Span::raw(" ".repeat(MARGIN)));
    push(&mut bottom, "╰", edge, width);
    let mut mode = None;
    let mut label_at = None;
    if label.is_empty() || label_width + 6 > span {
        push(&mut bottom, "─".repeat(span), edge, width);
    } else {
        let rule = span.saturating_sub(label_width + 3).max(1);
        push(&mut bottom, "─".repeat(rule), edge, width);
        push(&mut bottom, " ", edge, width);
        label_at = Some(text::line_width(&bottom));
        bottom.spans.extend(label);
        if let Some(words) = &mode_words {
            let to = text::line_width(&bottom);
            mode = Some((to - text::str_width(words), to));
        }
        push(&mut bottom, " ─", edge, width);
    }
    text::pad_to(&mut bottom, width - MARGIN - 1);
    push(&mut bottom, "╯", edge, width);
    out.push(bottom);
    (out, mode, label_at)
}

/// The keys under the composer, by what is happening, few enough to read
/// at a glance and always ending with the way to more (the leader's
/// panel). What everyone knows (Enter sends, pasting attaches) is not
/// said; Enter is named only when it does something else.
fn turn_hint(state: &SessionState, editor: &Editor, reach: Reach, leader: char) -> Hint {
    let working = state.phase() == PhaseView::Working;
    let shift_tab = next_control(&ui_view::settings(state)).map(|change| match change {
        ui_view::SettingChange::Mode(_) => ("shift+tab", "mode"),
        _ => ("shift+tab", "permission"),
    });
    let more = format!("ctrl+{leader}");
    let mut pairs = Vec::new();
    // A send now is held, shown pending with Cancel, and goes by itself.
    let holds = |when| Hint::Keys(keys(&[("enter", when)]));
    match state.composer() {
        Composer::Send => {
            if working {
                pairs.push(("enter", "queue"));
                pairs.push(("ctrl+x", "interrupt"));
            } else if !editor.is_empty() && !crate::terminal::shift_enter_reported() {
                // Shift+Enter is the newline everyone expects; where the
                // terminal cannot tell it from Enter, say the key that works.
                pairs.push(("ctrl+j", "newline"));
            }
            pairs.extend(shift_tab);
        }
        Composer::Resume => pairs.push(("enter", "resume")),
        Composer::Holds(Waiting::Detached) if reach == Reach::Offline(Offline::SignedOut) => {
            return holds("holds it · it sends once this machine signs in");
        }
        Composer::Holds(Waiting::Detached) if reach == Reach::Offline(Offline::Revoked) => {
            return holds("holds it · it sends once you pair again");
        }
        Composer::Holds(Waiting::Detached) => {
            return holds("holds it · it sends when the host is back");
        }
        Composer::Holds(Waiting::Reconnecting) => {
            return holds("holds it · it sends once amux is back");
        }
        Composer::Holds(Waiting::CatchingUp) => {
            return holds("holds it · it sends once caught up");
        }
    }
    pairs.push((&more, "more"));
    Hint::Keys(keys(&pairs))
}

/// A key legend as (key, action) pairs, most needed first.
pub(crate) type Keys = Vec<(String, String)>;

pub(crate) fn keys(pairs: &[(&str, &str)]) -> Keys {
    pairs
        .iter()
        .map(|(key, action)| ((*key).to_owned(), (*action).to_owned()))
        .collect()
}

/// What the line under the composer says: a key legend, or words that are
/// not one.
enum Hint {
    Keys(Keys),
    Sentence(String),
}

/// Where each drawn key pair sits on its line, and the key it presses.
type KeySpots = Vec<((usize, usize), KeyEvent)>;

/// The hint line in home's style (see [`text::push_hints`]); a sentence
/// reads faint. Returns the line and, for each pair drawn that names a
/// key, its columns and the key, so a click can press it.
fn hint_line(hint: &Hint, width: usize, theme: Theme) -> (Line<'static>, KeySpots) {
    let mut line = Line::from(Span::raw("  "));
    let pairs = match hint {
        Hint::Sentence(words) => {
            push(&mut line, words, theme.faint(), width);
            return (line, Vec::new());
        }
        Hint::Keys(pairs) => pairs,
    };
    let borrowed: Vec<(&str, &str)> = pairs
        .iter()
        .map(|(key, action)| (key.as_str(), action.as_str()))
        .collect();
    let spots = text::push_hints(&mut line, &borrowed, width.saturating_sub(2), theme)
        .into_iter()
        .filter_map(|(at, columns)| Some((columns, key_event(borrowed[at].0)?)))
        .collect();
    (line, spots)
}

/// The key a legend names, as the event pressing it sends: `enter`,
/// `shift+tab`, `ctrl+x`, a letter.
fn key_event(key: &str) -> Option<KeyEvent> {
    let (modifiers, name) = match key.split_once('+') {
        Some(("ctrl", name)) => (KeyModifiers::CONTROL, name),
        Some(("shift", "tab")) => {
            return Some(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT));
        }
        Some(_) => return None,
        None => (KeyModifiers::NONE, key),
    };
    let code = match name {
        "enter" => KeyCode::Enter,
        "esc" => KeyCode::Esc,
        "end" => KeyCode::End,
        "↑" => KeyCode::Up,
        name if name.chars().count() == 1 => KeyCode::Char(name.chars().next()?),
        _ => return None,
    };
    Some(KeyEvent::new(code, modifiers))
}

/// The prompt to pin under the header, and its key: the prompt of the turn
/// that owns the feed's first line under the pin. Best effort: nothing when
/// that turn's prompt is not held (older than the window; a page will bring
/// it), when the line belongs to no turn, or when the feed is too short to
/// spare the rows. Faint while the next turn's prompt is about to take the
/// pin.
fn pinned(
    state: &SessionState,
    laid: &Laid,
    height: usize,
    width: usize,
    theme: Theme,
) -> Option<(Vec<Line<'static>>, Key)> {
    // The pin's one line and the blank line under it.
    const ROWS: usize = 2;
    // The next prompt within this many lines of the pin fades it.
    const FADE: usize = 3;
    if height < ROWS + 6 {
        return None;
    }
    let owners = line_owners(laid, height);
    let owner = (*owners.get(ROWS)?)?;
    let order = laid.blocks.get(owner)?.order;
    let transcript = state.transcript();
    let oldest = transcript.oldest_held()?;
    let mut prompt = None;
    for held in transcript.range(oldest..=order).rev() {
        match held.class {
            ui_state::ItemClass::Prompt => {
                prompt = Some(held.item.key.clone());
                break;
            }
            // Another turn's end: the line belongs to no turn.
            ui_state::ItemClass::Turn if held.item.order != order => return None,
            _ => {}
        }
    }
    let key = prompt?;
    // A prompt whose first line of words is still on screen is its own
    // landmark: pinning it too would draw it over itself. Only the block's
    // top padding line may have scrolled off.
    if let Some(first) = owners
        .iter()
        .position(|block| block.is_some_and(|block| laid.blocks[block].key == key))
        && (first > 0 || laid.top_offset <= 1)
    {
        return None;
    }
    let row = chat_rows_for(
        state,
        std::slice::from_ref(&key),
        &ChatOptions {
            tools: ToolRows::ShowAll,
        },
    )
    .pop()?;
    let next = owners
        .iter()
        .enumerate()
        .skip(ROWS + 1)
        .find(|(_, block)| {
            block.is_some_and(|block| {
                block != owner && matches!(laid.blocks[block].row.kind, RowKind::Prompt { .. })
            })
        })
        .map(|(at, _)| at);
    let faint = next.is_some_and(|at| at - ROWS <= FADE);
    let mut lines = feed::pinned_prompt(&row, faint, width, theme);
    if lines.is_empty() {
        return None;
    }
    lines.push(Line::default());
    Some((lines, key))
}

/// Which drawn block each of the feed's `height` lines belongs to, top
/// first; `None` for the blank lines that pad a short chat.
fn line_owners(laid: &Laid, height: usize) -> Vec<Option<usize>> {
    let mut owners: Vec<Option<usize>> = laid
        .blocks
        .iter()
        .enumerate()
        .flat_map(|(i, block)| std::iter::repeat_n(Some(i), block.lines.len()))
        .skip(laid.top_offset)
        .take(height)
        .collect();
    if owners.len() < height {
        let mut padded = vec![None; height - owners.len()];
        padded.append(&mut owners);
        owners = padded;
    }
    owners
}

/// The control that returns to the newest row while the reader is in
/// history, centred: "↓ Jump to Bottom  ctrl+end", or how many rows arrived
/// meanwhile. Returns it and its first column.
fn jump_control(state: &SessionState, width: usize, theme: Theme) -> (Line<'static>, usize) {
    let surface = ratatui::style::Style {
        bg: theme.user_surface().bg,
        ..Default::default()
    };
    let arrived = state.held_arrivals();
    let words = if arrived > 0 {
        format!("↓ {arrived} new")
    } else if state.arrivals_held() {
        "↓ New Activity".to_owned()
    } else {
        "↓ Jump to Bottom".to_owned()
    };
    let line = Line::from(vec![
        Span::styled(format!(" {words}  "), theme.text().patch(surface)),
        Span::styled("ctrl+end ", theme.faint().patch(surface)),
    ]);
    let from = width.saturating_sub(text::line_width(&line)) / 2;
    (line, from)
}

/// The leader's flyover: every next key and what it does, in groups (what
/// is about amux, then what is about this chat), with each line's chord for
/// clicks. A legend, not a list to move through: no pointer, no highlight;
/// the key bright, its words faint with the key's letter picked out where
/// the word holds it.
fn which_key_panel(
    entries: &[PanelEntry],
    leader: char,
    width: usize,
    theme: Theme,
) -> (Vec<Line<'static>>, Vec<Option<char>>) {
    let key_width = entries
        .iter()
        .map(|entry| text::str_width(&entry.key))
        .max()
        .unwrap_or(1);
    let mut rows = Vec::new();
    let mut chords = Vec::new();
    let mut group = "";
    for entry in entries {
        if entry.group != group {
            if !group.is_empty() {
                rows.push(Line::default());
                chords.push(None);
            }
            group = entry.group;
            rows.push(Line::from(Span::styled(group.to_owned(), theme.faint())));
            chords.push(None);
        }
        let pad = key_width.saturating_sub(text::str_width(&entry.key));
        let mut line = Line::from(vec![
            Span::raw("  "),
            Span::styled(entry.key.clone(), theme.emphasis()),
            Span::raw(" ".repeat(pad + 2)),
        ]);
        let letter = entry
            .key
            .chars()
            .next()
            .filter(|_| entry.key.chars().count() == 1);
        match letter {
            Some(letter) => line
                .spans
                .extend(crate::panel::mnemonic(&entry.label, letter, theme)),
            None => line
                .spans
                .push(Span::styled(entry.label.clone(), theme.faint())),
        }
        rows.push(line);
        chords.push(Some(entry.chord));
    }
    let title = format!("ctrl+{leader}");
    let inner = crate::panel::content_width(&rows)
        .max(text::str_width(&title) + 3)
        .min(width.saturating_sub(8).max(8));
    let lines = crate::panel::bordered(&title, rows, inner, &[], None, theme);
    let chords = std::iter::once(None).chain(chords).chain([None]).collect();
    (lines, chords)
}

/// The type of a pasted file that is not a native image, from its name.
fn mime_of(name: &str) -> &'static str {
    let extension = name.rsplit('.').next().unwrap_or_default().to_lowercase();
    match extension.as_str() {
        "txt" | "md" | "log" => "text/plain",
        "json" => "application/json",
        _ => "application/octet-stream",
    }
}

/// "↑ planner · 3 subagents · 1 needs you"
fn family_line(family: &FamilyHeader, width: usize, theme: Theme) -> Line<'static> {
    let mut parts = Vec::new();
    if let Some(parent) = &family.parent {
        parts.push(format!("↑ {}", parent.name));
    }
    let count = family.children.len();
    if count > 0 {
        parts.push(format!(
            "{count} subagent{}",
            if count == 1 { "" } else { "s" }
        ));
    }
    let waiting = family.need_you;
    let mut line = Line::from(Span::raw("  "));
    push(&mut line, parts.join(" · "), theme.muted(), width);
    if waiting > 0 {
        push(
            &mut line,
            format!(
                " · {waiting} need{} you",
                if waiting == 1 { "s" } else { "" }
            ),
            theme.attention(),
            width,
        );
    }
    line
}

/// What Shift+Tab changes: the next mode where the agent offers modes;
/// else its own next permission where it only cycles; else the next
/// settable permission that still asks before acting. None where neither
/// can change from here.
pub(crate) fn next_control(view: &ui_view::SettingsView) -> Option<ui_view::SettingChange> {
    let modes: Vec<(&str, bool, bool)> = view
        .modes
        .iter()
        .filter(|mode| mode.settable)
        .map(|mode| (mode.value.as_str(), mode.current, mode.normal))
        .collect();
    if view.changeable.mode == Changeable::Pick
        && let Some(value) = next_of(&modes)
    {
        return Some(ui_view::SettingChange::Mode(value));
    }
    match view.changeable.permission {
        Changeable::Cycle => return Some(ui_view::SettingChange::CyclePermission),
        Changeable::Pick => {}
        _ => return None,
    }
    let permissions: Vec<(&str, bool, bool)> = view
        .permissions
        .iter()
        .filter(|permission| permission.settable && (!permission.never_asks || permission.current))
        .map(|permission| {
            (
                permission.value.as_str(),
                permission.current,
                permission.normal,
            )
        })
        .collect();
    next_of(&permissions).map(ui_view::SettingChange::Permission)
}

/// The value after the current one, (value, current, normal) in order,
/// counting from the normal one when none is current; None with fewer than
/// two.
fn next_of(offered: &[(&str, bool, bool)]) -> Option<String> {
    if offered.len() < 2 {
        return None;
    }
    let at = offered
        .iter()
        .position(|(_, current, _)| *current)
        .or_else(|| offered.iter().position(|(_, _, normal)| *normal));
    let next = at.map_or(0, |at| (at + 1) % offered.len());
    Some(offered[next].0.to_owned())
}

fn empty_feed(
    state: &SessionState,
    reach: Reach,
    height: usize,
    waited_ms: i64,
    width: usize,
    theme: Theme,
) -> Vec<Line<'static>> {
    let mut lines = vec![Line::default(); height];
    let away = matches!(state.composer(), Composer::Holds(Waiting::Detached)) || !reach.online();
    // A caught-up chat is current from its host, which loses its source
    // the moment the host does: the fleet's word on the host comes on
    // another stream and may still say away just after the host is back.
    let words = if state.has_snapshot() && state.caught_up() {
        Some("Nothing here yet.")
    } else if away {
        Some("Its host is away, and nothing of this chat is held here yet.")
    } else if waited_ms >= LOADING_HINT_MS {
        Some("Loading…")
    } else {
        None
    };
    if let (Some(words), Some(line)) = (words, lines.get_mut(height / 2)) {
        let mut centred = Line::from(Span::raw(" ".repeat(width.saturating_sub(words.len()) / 2)));
        push(&mut centred, words, theme.muted(), width);
        *line = centred;
    }
    lines
}
