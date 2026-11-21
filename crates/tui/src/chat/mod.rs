//! One open chat: the client's view state over a session, the keys that
//! act on it, and the frame it draws.
//!
//! Everything here is ephemeral and the client's own: the anchor, the
//! expansion set keyed by item keys, the focused row, the draft, the ask
//! card's picks. Keys turn into [`ChatEffect`]s the event loop carries out
//! against the session; nothing in this module does I/O.

pub mod ask;
pub mod changes;
pub mod commands;
pub mod composer;
pub mod feed;
pub mod layout;
pub mod pane;
pub mod review;
pub mod rows;
pub mod selection;
pub mod thumbs;

mod draw;
mod input;
mod pointer;
mod words;

use std::collections::{HashMap, HashSet};

pub(crate) use composer::resets;
use crossterm::event::{KeyCode, KeyEvent, MouseEvent};
use draw::{ChipSpot, ROUNDED, StopAsk, change_totals, framed, line_places, review_counter};
#[cfg(feature = "fixtures")]
use draw::{boxed_composer, hint_line, marked_edge_in};
use ratatui::layout::Rect;
use ratatui::text::Line;
use ui_state::{Composer, InputState, InputWhat, Key, SessionState};
pub use ui_view::Comparison;
use ui_view::{AskCard, CardState, Reach, ask_card, overview, queue_rows};
use wire::{Attachment, GetChangesResponse, get_changes_request};
pub(crate) use words::{FREE_PLAN_AWAY, Keys, activity_words, keys, next_control, refusal_words};
use words::{
    Hint, edge_title, ends_with_exit, exit_words, has_draft, key_event, native_box, native_hint,
    problem_words, quiet_activity, refused_command_words, settled, sign_in_notice, turn_hint,
    unchangeable_words, withdraw_effect,
};
#[cfg(feature = "fixtures")]
pub(crate) use words::{composer_lines, native_composer_lines, sign_in_composer_lines};

use self::ask::AskUi;
use self::composer::QueueEntry;
use self::feed::FeedHit;
pub use self::layout::ToolSteps;
use self::layout::{Anchor, Frame, Laid, Toggle};
use self::selection::{FeedSelection, Point, ScreenCells, ScreenSelection};
use crate::clipboard::{Clip, ClipboardContent};
use crate::editor::Editor;
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
    /// Open the agent's review again; `refresh` takes a new snapshot even
    /// with drafts, moving them onto it.
    ReviewOpen {
        refresh: bool,
    },
    /// Make these edits to the agent's review, all or none, and open it
    /// again afterwards when `reopen` says.
    ReviewEdit {
        edits: Vec<wire::experimental::ReviewEdit>,
        reopen: bool,
    },
    /// Read these lines for more context on the review page.
    ReviewContext(ui_view::ReviewContextAsk),
    /// A prompt that carries the review's drafts, which the agent's host
    /// marks sent when it accepts the message.
    PromptWithReview {
        text: String,
        attachments: Vec<Attachment>,
    },
    /// Open a link from the agent's text in the person's browser.
    OpenUrl(String),
    /// Open an attachment in the app the system picks for it.
    OpenAttachment(wire::BlobRef),
    /// Show the round a sent review in the chat closed, read from the
    /// agent's host; `text`, what the agent read, when the host no longer
    /// holds it.
    ReviewRound {
        review: ui_view::SentReviewRef,
        text: String,
    },
    /// Fetch an attachment again after a failure.
    RetryBlob(Vec<u8>),
    /// Back to home: the header's [Home].
    Home,
    /// A key a click stands for: a hint, the composer's mode. The app
    /// handles it as if pressed, so the leader and its panel work too.
    Press(KeyEvent),
    /// A leader chord picked from the which-key panel.
    Chord(char),
    /// Say this in the notice line.
    Notice(String),
    /// Put this on the clipboard.
    Copy(String),
    /// amux's rename of this chat's agent: `/rename <name>`.
    Rename(String),
    /// Open the chat of an agent a call in this one started.
    OpenAgent(ui_state::AgentKey),
}

/// What the reader has dragged over.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Selection {
    Feed(FeedSelection),
    /// In the side pane or on the review page.
    Screen(ScreenSelection),
}

/// What the mouse went down on.
#[derive(Clone, Debug)]
enum Pressed {
    Feed {
        anchor: Option<Point>,
        hit: Option<FeedHit>,
    },
    /// The side pane, on this item if on one.
    Pane(Option<pane::PaneItem>),
    /// The review page's stream.
    Stream(Rect),
}

/// The left button held down.
#[derive(Clone, Debug)]
struct Press {
    at: (u16, u16),
    pointer: (u16, u16),
    dragged: bool,
    pressed: Pressed,
}

impl Press {
    fn new(event: MouseEvent, pressed: Pressed) -> Press {
        let at = (event.column, event.row);
        Press {
            at,
            pointer: at,
            dragged: false,
            pressed,
        }
    }
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

/// The changed files of one comparison, as the agent's host answered.
#[derive(Debug)]
pub struct Fetched {
    pub key: FetchKey,
    pub changes: GetChangesResponse,
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
    /// The review page of the agent's review, kept while the chat is open
    /// so returning to it draws at once.
    pub review_page: Option<review::Page>,
    pub review_page_open: bool,
    /// A sent review's round, over the chat and its review page while
    /// open, with the review it names.
    pub round: Option<(ui_view::SentReviewRef, review::Page)>,
    /// The window asking before the review chip's drafts are discarded.
    discard_ask: Option<review::Asking>,
    /// The review chip on the composer's edge on the last frame: its row,
    /// its columns, and the columns of its ✕.
    chip_spot: Option<ChipSpot>,
    /// The review entry's counter when this composer last sent the drafts:
    /// the chip waits for the row to move past it, so the same drafts are
    /// not sent twice while the host marks them.
    review_sent: Option<u64>,
    /// Whether the agent's own interface can be attached from here.
    pub attach: bool,
    /// How this machine reaches the agent's host, kept current by the app
    /// from the fleet.
    pub reach: Reach,
    /// How the agents this chat's rows started stand, kept current by the
    /// app from the fleet before each frame.
    pub agents: ui_view::ChatAgents,
    /// The feed's images, on terminals that can draw them.
    pub thumbs: thumbs::Thumbs,
    /// The app is drawing something of its own over the chat this frame.
    pub covered: bool,
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
    /// What the reader has dragged over.
    selection: Option<Selection>,
    /// The mouse button held since it went down on something a drag selects
    /// or a click acts on, which waits for the release to know which it was.
    press: Option<Press>,
    /// The selection is on the clipboard.
    copied: bool,
    /// Releasing a drag copies it. The app sets this from the person's
    /// choice in Settings.
    pub copy_on_select: bool,
    /// A drag held past the feed's top (-1) or bottom (1) keeps scrolling.
    edge: Option<isize>,
    /// The side pane's list, where a drag selects text.
    pane_body: Option<Rect>,
    /// What the side pane's list or the review page last drew.
    screen_cells: Option<ScreenCells>,
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
    /// When each of this client's prompts on their way was first drawn,
    /// for the clock a pending prompt in the feed shows.
    sending: HashMap<Vec<u8>, i64>,
    /// Rejected prompts whose words have gone back to the composer.
    rejected_seen: HashSet<Vec<u8>>,
    /// Words stayed in or came back to the composer because they could not
    /// go: the client saves the draft to this machine's daemon at once, so
    /// a terminal killed before the next close or quit still has them.
    draft_owed: bool,
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
    /// The lit command and whether the commands' flyover was closed.
    menu: commands::Menu,
    /// The commands' flyover as last drawn, for clicks; None when it was
    /// not drawn.
    menu_flyover: Option<crate::setup::Flyover>,
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
            review_page: None,
            review_page_open: false,
            round: None,
            discard_ask: None,
            chip_spot: None,
            review_sent: None,
            attach,
            // Nothing is known against the host until the app reads the
            // fleet, before the chat is first drawn.
            reach: Reach::Online(wire::RouteKind::Unspecified),
            agents: ui_view::ChatAgents::default(),
            leader: 'a',
            open_runs: HashSet::new(),
            tools: ToolSteps::Collapse,
            local: true,
            comparison: Comparison::Uncommitted,
            changes: None,
            changes_asked: None,
            header_spots: Vec::new(),
            hover: None,
            selection: None,
            press: None,
            copied: false,
            copy_on_select: true,
            edge: None,
            pane_body: None,
            screen_cells: None,
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
            draft_owed: false,
            not_sent: None,
            feed_tail: Vec::new(),
            setting_prefix: false,
            picker: None,
            flyover: crate::setup::Flyover::default(),
            menu: commands::Menu::default(),
            menu_flyover: None,
            feed_origin: (0, 0),
            thumbs: thumbs::Thumbs::new(crate::terminal::image_picker()),
            covered: false,
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
            thumbnails: self.thumbs.enabled() && self.feed.0 >= thumbs::LEAST_FEED_WIDTH,
            agents: &self.agents,
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
        for prompt in ui_view::prompts_underway(state) {
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
            });
        }
        entries
    }

    /// Notes this client's prompts as the frame first sees them: when a
    /// prompt on its way was first drawn, and, for one the agent refused,
    /// its words back in the composer (after any draft) and why on the
    /// box's edge.
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
                self.take_back(text, attachments.clone());
                self.draft_owed = true;
            }
            self.not_sent = Some(format!("not sent: {}", refusal_words(&refused.reason)));
        }
        for sent in state.inputs().iter() {
            if !matches!(sent.what, InputWhat::Prompt { .. }) {
                continue;
            }
            match &sent.state {
                InputState::Sent | InputState::Queued | InputState::Uncertain => {
                    self.sending.entry(sent.id.clone()).or_insert(now_ms);
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

    /// Whether the draft is owed a save since it was last asked; asking
    /// clears it.
    pub(crate) fn take_draft_owed(&mut self) -> bool {
        std::mem::take(&mut self.draft_owed)
    }

    pub fn field_text(&self, state: &SessionState) -> bool {
        if self.round.is_some() {
            return false;
        }
        if self.review_page_open {
            return self.review_page.as_ref().is_some_and(review::Page::editing);
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
            && self.round.is_none()
            && !self.review_page_open
            && self.live_card(state).is_none()
            && self.tray.is_none()
            && self.editor.is_empty()
    }

    /// Ctrl+C on a field with text: clears it as a kill. Only the field
    /// with the keys: a draft hidden behind a card stays.
    pub fn kill_field(&mut self, state: &SessionState) -> bool {
        if self.round.is_some() {
            return false;
        }
        if self.review_page_open {
            return self
                .review_page
                .as_mut()
                .is_some_and(review::Page::kill_field);
        }
        if let Some(card) = self.card_takes_keys(state) {
            return self.ask.in_box_note(&card) && self.ask.kill_box_note();
        }
        self.tray.is_none() && self.editor.kill_all()
    }

    /// A bracketed paste goes to the field with the keys, and nowhere when
    /// none has them.
    pub fn paste_text(&mut self, state: &SessionState, text: &str) {
        if self.round.is_some() {
            return;
        }
        if self.review_page_open {
            if let Some(page) = &mut self.review_page {
                page.paste(text);
            }
            return;
        }
        if let Some(card) = self.card_takes_keys(state) {
            self.ask.sync(&card);
            self.ask.paste_box_note(&card, text);
            return;
        }
        if self.tray.is_none() && has_draft(state) {
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

    /// The overview with the changed files fetched for the comparison
    /// shown, while they still answer it.
    fn overview(&self, state: &SessionState) -> ui_view::Overview {
        let key = self.fetch_key(state);
        let changed = self
            .changes
            .as_ref()
            .filter(|fetched| fetched.key.comparison == key.comparison)
            .map(|fetched| &fetched.changes);
        overview(state, changed)
    }

    /// The fetch the overview needs now: the comparison shown, as of the
    /// row's totals for it.
    fn fetch_key(&self, state: &SessionState) -> FetchKey {
        FetchKey {
            comparison: self.comparison,
            totals: change_totals(state, self.comparison),
        }
    }

    /// The comparison's changed files to ask the agent's host for, with
    /// the base to ask for: while the overview shows, once for each
    /// comparison and again whenever the row's totals for it move. Never on
    /// every frame.
    pub fn wants_changes(
        &mut self,
        state: &SessionState,
    ) -> Option<(FetchKey, Option<get_changes_request::Base>)> {
        if !self.pane_open
            || !matches!(state.composer(), Composer::Send | Composer::Resume)
            || !ui_view::lists_changes(state)
        {
            return None;
        }
        let key = self.fetch_key(state);
        if self.changes_asked.as_ref() == Some(&key) {
            return None;
        }
        self.changes_asked = Some(key.clone());
        let base = key.comparison.base();
        Some((key, base))
    }

    /// Takes in a fetch of the changed files, unless the person has moved
    /// to another comparison since it was asked for.
    pub fn changes_fetched(&mut self, key: FetchKey, changes: GetChangesResponse) {
        if key.comparison == self.comparison {
            self.changes = Some(Fetched { key, changes });
        }
    }

    /// The branch the changes are counted against, for the header to name:
    /// none for the uncommitted work. On the branch it is the base the
    /// agent's host reported for the last list fetched, since the host
    /// picks it, as from the branch's upstream when the agent works on its
    /// base branch; until one is fetched, the base the row names.
    fn compared_with<'a>(&'a self, state: &'a SessionState) -> Option<&'a str> {
        if self.comparison == Comparison::Uncommitted {
            return None;
        }
        self.changes
            .as_ref()
            .filter(|fetched| fetched.key.comparison == Comparison::OnBranch)
            .map(|fetched| fetched.changes.base_branch.as_str())
            .filter(|base| !base.is_empty())
            .or_else(|| {
                state
                    .agent()
                    .git
                    .as_ref()
                    .and_then(|git| git.base_branch.as_deref())
            })
    }

    /// Switches the comparison the header and the overview count against:
    /// uncommitted changes, or everything on this branch. False when the
    /// branch has no base to compare with, and nothing changes.
    pub fn switch_comparison(&mut self, state: &SessionState) -> bool {
        let next = match self.comparison {
            Comparison::Uncommitted => Comparison::OnBranch,
            Comparison::OnBranch => Comparison::Uncommitted,
        };
        if !next.offered(state) {
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

    /// The attachment `<leader> o` opens: the focused row's first, unless
    /// that row has something of its own to unfold.
    pub fn focused_attachment(&self) -> Option<ChatEffect> {
        let focus = self.focus.as_ref()?;
        let block = self.laid.blocks.iter().find(|block| &block.key == focus)?;
        let hits = || block.hits.iter().flatten().map(|(_, hit)| hit);
        if hits().any(|hit| matches!(hit, FeedHit::Step(_))) {
            return None;
        }
        hits().find_map(|hit| match hit {
            FeedHit::Attachment(blob) | FeedHit::Thumbnail { blob, .. } => {
                Some(ChatEffect::OpenAttachment(blob.clone()))
            }
            FeedHit::Round { review, text } => Some(ChatEffect::ReviewRound {
                review: review.clone(),
                text: text.clone(),
            }),
            _ => None,
        })
    }

    /// `<leader> o`: a folded run or group of subagents opens, an open
    /// one's first row folds it again, and any other step opens its detail.
    /// With no row focused, the newest run or group on screen opens or
    /// folds, so neither is ever out of the keyboard's reach.
    pub fn toggle_expanded(&mut self, state: &SessionState) {
        let Some(key) = self.focus.clone() else {
            let newest = self
                .laid
                .blocks
                .iter()
                .rev()
                .find_map(|block| match &block.toggle {
                    Toggle::Run(id) => Some(FeedHit::Run(id.clone())),
                    Toggle::Group(id) => Some(FeedHit::Group(id.clone())),
                    _ => None,
                });
            if let Some(hit) = newest {
                self.feed_hit(state, hit);
            }
            return;
        };
        if let Some(block) = self.laid.blocks.iter().find(|block| block.key == key) {
            match block.toggle.clone() {
                Toggle::Row => {}
                Toggle::Group(member) => {
                    self.feed_hit(state, FeedHit::Group(member));
                    return;
                }
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

    /// `<leader> enter`: the chat of the agent the focused row's call
    /// started, or with no row focused the newest such row's on screen. A
    /// folded group stands for several agents, so it unfolds instead, to
    /// pick one from. Opening an agent and opening a row in place are kept
    /// apart: `<leader> o` opens the row.
    pub fn open_focused_agent(&mut self, state: &SessionState) -> Vec<ChatEffect> {
        let block =
            match &self.focus {
                Some(key) => self.laid.blocks.iter().find(|block| &block.key == key),
                None => self.laid.blocks.iter().rev().find(|block| {
                    block.opens.is_some() || matches!(block.toggle, Toggle::Group(_))
                }),
            };
        let Some(block) = block else {
            return vec![];
        };
        if let Some(agent) = &block.opens {
            return vec![ChatEffect::OpenAgent(agent.clone())];
        }
        if let Toggle::Group(member) = block.toggle.clone() {
            self.feed_hit(state, FeedHit::Group(member));
        }
        vec![]
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
        match crate::clipboard::clip(content)? {
            Clip::File { name, mime, bytes } => {
                self.attaching.push(name.clone());
                Ok(Some(ChatEffect::Attach { name, mime, bytes }))
            }
            Clip::Text(text) => {
                self.editor.paste(&text);
                Ok(None)
            }
            Clip::Nothing => Ok(None),
        }
    }

    /// A stored blob, attached at the cursor as an image or a file.
    /// A file stored for the draft: its chip goes in. When Enter waited
    /// for it and it was the last on its way, the draft is sent now.
    pub fn attach_blob(&mut self, state: &SessionState, blob: wire::BlobRef) -> Vec<ChatEffect> {
        self.landed(&blob.name);
        self.editor.attach(crate::clipboard::blob_attachment(blob));
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

    /// The agent's review page, made when first wanted, shown at the file
    /// `at` when one is named.
    pub fn open_review_page(&mut self, at: Option<&str>) -> &mut review::Page {
        self.review_page_open = true;
        let page = self.review_page.get_or_insert_with(review::Page::new);
        if let Some(path) = at {
            page.show_file(path);
        }
        page
    }

    /// A sent review's round over the chat, while it is read from the
    /// agent's host.
    pub fn open_round(&mut self, review: &ui_view::SentReviewRef) {
        self.round = Some((review.clone(), review::Page::round(review.round)));
    }

    /// The round `review` names, read: shown when it is still the one
    /// open; none, or a host out of reach with none of it loaded, shows
    /// `text`, what the agent read. A round read from what was loaded with
    /// the host out of reach says so.
    pub fn round_read(
        &mut self,
        review: &ui_view::SentReviewRef,
        read: Result<Option<ui_runtime::review::Round>, ui_runtime::review::ReviewError>,
        text: &str,
    ) {
        if let Some((open, page)) = &mut self.round
            && open == review
        {
            page.round_read(read, text);
        }
    }

    /// The review chip the composer shows: from the agent's row, unless
    /// this composer has just sent those drafts.
    fn review_chip(&self, state: &SessionState) -> Option<ui_view::ReviewChip> {
        let chip = ui_view::review_chip(state.agent())?;
        (self.review_sent.is_none() || self.review_sent != review_counter(state)).then_some(chip)
    }

    /// A send of the review's drafts did not go: the chip is back.
    pub fn review_unsent(&mut self) {
        self.review_sent = None;
    }

    /// A prompt that did not reach the agent, refused, withdrawn from the
    /// queue or never sent, comes back to the composer: its words and
    /// attachments after any draft there, but not a review it carried.
    /// That review's comments are still drafts on the agent's host, so the
    /// chip is back and the next send reads them afresh: the agent gets
    /// one review, in the words the drafts hold then.
    pub fn take_back(&mut self, text: &str, attachments: Vec<Attachment>) {
        let attachments = attachments
            .into_iter()
            .filter(|attachment| ui_view::sent_review::entry(attachment).is_none())
            .collect();
        self.editor.restore(text, attachments);
        self.review_sent = None;
    }

    /// A page was asked for; another waits until the window grows.
    pub fn page_sent(&mut self, state: &SessionState) {
        self.page_asked = state.oldest_order();
    }
}

#[cfg(test)]
mod tests {
    use ui_state::ActivityKind;

    use super::*;

    fn retrying_line(since_ms: i64, elapsed_ms: i64, retry_at_ms: Option<i64>) -> String {
        let activity = ui_state::Activity {
            kind: ActivityKind::Retrying {
                attempt: 6,
                max_attempts: 10,
                retry_at_ms,
            },
            since_ms,
            elapsed_ms,
            turn_elapsed_ms: elapsed_ms,
        };
        quiet_activity(
            &activity,
            &ui_view::GroupState::default(),
            80,
            Theme::default(),
        )
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>()
        .trim()
        .to_owned()
    }

    /// A retry counts down to its next attempt, and once that moment has
    /// passed says only that it is trying.
    #[test]
    fn a_retry_says_when_the_next_attempt_is() {
        assert_eq!(
            retrying_line(1_000, 2_000, Some(22_000)),
            "Retrying · attempt 6 of 10 · in 19s"
        );
        assert_eq!(
            retrying_line(1_000, 21_000, Some(22_000)),
            "Retrying · attempt 6 of 10"
        );
        assert_eq!(
            retrying_line(1_000, 2_000, None),
            "Retrying · attempt 6 of 10"
        );
    }
}
