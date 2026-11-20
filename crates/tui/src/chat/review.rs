//! The review page: an agent's changes as one snapshot of its working tree
//! shows them, with the person's comments placed on their lines.
//!
//! The page is ui-view's [`review_page`], composed from what the agent's
//! review last fetched and the person's own choices; this module draws its
//! rows exactly as that composition places them and owns only the cursor,
//! the range being selected, the scroll, the comment editor and the windows
//! that ask before discarding drafts or starting over. Nothing here does
//! I/O: keys answer [`ReviewAction`]s, which the chat hands to the app to
//! carry out against the review, and what the review fetches comes back
//! through [`Page::update`].
//!
//! The same page shows, to read only, the round a review sent into the chat
//! closed: its whole diff, from the base commit the round was recorded with
//! to its snapshot, with the comments it sent on their lines; each comment
//! with the code stored with it when the host no longer keeps the round's
//! snapshot; or, when the agent's host no longer holds the round, the text
//! the agent read ([`Page::round`]).

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame as Paint;
use ratatui::layout::{Position, Rect};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ui_view::{
    LineKind, ReviewAnchor, ReviewBanner, ReviewCodeLine, ReviewCommentAt, ReviewCommentRow,
    ReviewCommentState, ReviewContextAsk, ReviewEmpty, ReviewFileFlag, ReviewFileHead, ReviewGap,
    ReviewHeader, ReviewNoLines, ReviewNotice, ReviewPage, ReviewPageState, ReviewRow, ReviewRowOf,
    ReviewView, review_page_of,
};
use wire::experimental::{
    ReviewEdit, ReviewFileDiff, ReviewLine, ReviewPutComment, ReviewSummary, review_edit,
};

use super::composer::editor_lines;
use crate::editor::Editor;
use crate::text::{self, push};
use crate::theme::Theme;

/// What the page draws from, as the agent's review last fetched it.
#[derive(Clone, Debug, Default)]
pub struct Fetched {
    pub summary: Option<Arc<ReviewSummary>>,
    /// The bodies fetched for each view of the summary, by path.
    pub all: BTreeMap<String, Arc<ReviewFileDiff>>,
    pub since_last: BTreeMap<String, Arc<ReviewFileDiff>>,
    /// The answers to the page's asks for more context.
    pub context: Vec<(ReviewContextAsk, Vec<ReviewLine>)>,
}

/// What a key on the page asks of the chat.
#[derive(Clone, Debug, PartialEq)]
pub enum ReviewAction {
    None,
    /// Back to the chat; the review stays, its drafts on the agent's host.
    Close,
    /// Open the review again; `refresh` takes a new snapshot even with
    /// drafts, moving them onto it.
    Open {
        refresh: bool,
    },
    /// Make these edits at the agent's host, all or none; `reopen` opens
    /// the review afterwards, as after starting over.
    Edit {
        edits: Vec<ReviewEdit>,
        reopen: bool,
    },
    /// Read these lines for more context.
    Context(ReviewContextAsk),
    /// Say this on the notice line.
    Notice(&'static str),
}

/// What a window asks before it goes ahead.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Confirm {
    /// Delete every draft; sent comments stay.
    Discard { drafts: u32 },
    /// Delete the review: the next look is a first look.
    StartOver,
}

/// A window asking, and the button Enter presses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Asking {
    pub what: Confirm,
    /// Enter goes ahead rather than cancelling.
    pub go: bool,
}

impl Asking {
    pub fn new(what: Confirm) -> Asking {
        Asking { what, go: true }
    }

    /// A key in the window: Some(true) goes ahead, Some(false) cancels,
    /// None keeps it open.
    pub fn key(&mut self, key: KeyEvent) -> Option<bool> {
        match key.code {
            KeyCode::Enter => return Some(self.go),
            KeyCode::Esc => return Some(false),
            KeyCode::Left => self.go = true,
            KeyCode::Right => self.go = false,
            KeyCode::Tab | KeyCode::BackTab => self.go = !self.go,
            _ => {}
        }
        None
    }

    /// The edit going ahead makes.
    pub fn edit(&self) -> ReviewEdit {
        ReviewEdit {
            of: Some(match self.what {
                Confirm::Discard { .. } => review_edit::Of::DiscardDrafts(wire::Empty {}),
                Confirm::StartOver => review_edit::Of::StartOver(wire::Empty {}),
            }),
        }
    }

    /// The window, centred over `area`.
    pub fn draw(&self, paint: &mut Paint<'_>, area: Rect, theme: Theme) {
        let (title, act, sentence, rest) = match self.what {
            Confirm::Discard { drafts } => (
                "Discard",
                "Discard",
                match drafts {
                    1 => "Discard your draft comment?".to_owned(),
                    n => format!("Discard your {n} draft comments?"),
                },
                "They go without being sent. Sent comments stay.",
            ),
            Confirm::StartOver => (
                "Start over",
                "Start over",
                "Start the review over?".to_owned(),
                "Its comments and rounds go; the next look is a first look.",
            ),
        };
        let (buttons, _) = crate::panel::buttons(
            &[(act, true), ("Cancel", false)],
            usize::from(!self.go),
            theme,
        );
        let legend = crate::panel::legend(
            &[("←→", "choose"), ("enter", "press"), ("esc", "cancel")],
            theme,
        );
        let inner = text::str_width(rest)
            .max(crate::panel::content_width(std::slice::from_ref(&legend)))
            .clamp(36, 64)
            .min(usize::from(area.width).saturating_sub(8));
        let mut rows = vec![Line::default()];
        rows.push(Line::from(Span::styled(sentence, theme.text())));
        for part in text::wrap(rest, inner.max(1)) {
            rows.push(Line::from(Span::styled(part, theme.muted())));
        }
        rows.extend([Line::default(), buttons, legend]);
        let lines = crate::panel::bordered(title, rows, inner, &[], None, theme);
        crate::panel::centre(paint, lines, area);
    }
}

/// A comment being written or edited under the row it is on.
#[derive(Debug)]
struct Composing {
    /// The row the editor opens under, by its key.
    under: String,
    anchor: wire::experimental::ReviewAnchor,
    /// The comment's id: made now for a new one.
    id: Vec<u8>,
    /// An existing draft's words are being changed.
    editing: bool,
    /// Where it goes, for its placeholder: lines, or the whole file.
    whole_file: bool,
    editor: Editor,
    /// Sent to the agent's host, its answer awaited.
    saving: bool,
    /// Why the last save did not land.
    failed: Option<String>,
}

/// A sent review's round, shown to read only.
#[derive(Debug)]
struct Round {
    /// The round's number: the chip's until the host's is read.
    number: u32,
    /// What the agent read, once the host is found to hold none of the
    /// round, or cannot be reached with none of it loaded: the page shows
    /// it in place of the round.
    gone: Option<String>,
    /// The host could not be reached, rather than holding none of it.
    unreachable: bool,
}

#[derive(Debug, Default)]
pub struct Page {
    fetched: Fetched,
    /// Showing a sent round rather than the review as it stands.
    round: Option<Round>,
    state: ReviewPageState,
    page: ReviewPage,
    /// The row the keys are on, by index and by key, so it stays on its
    /// row as the page is composed again.
    cursor: usize,
    cursor_key: Option<String>,
    /// Where a range being selected started, by key.
    mark: Option<String>,
    /// The first body line on screen.
    scroll: usize,
    /// The body's height at the last draw, for paging.
    height: usize,
    composing: Option<Composing>,
    asking: Option<Asking>,
    /// Whose changes these are, for the header: the agent's name and where
    /// it works, as the chat's header says them.
    owner: (String, String),
    /// Where the body was last drawn, the columns of each of its rows that
    /// are a diff's gutter rather than code, and the page row each body
    /// line on screen belongs to.
    stream_area: Rect,
    stream_gutters: Vec<u16>,
    rows_on_screen: Vec<Option<usize>>,
}

impl Page {
    pub fn new() -> Page {
        let mut page = Page::default();
        page.compose();
        page
    }

    /// The page of a review's round `number`, as a sent review's chip
    /// names it, while the round is read from the agent's host.
    pub fn round(number: u32) -> Page {
        let mut page = Page {
            round: Some(Round {
                number,
                gone: None,
                unreachable: false,
            }),
            ..Page::default()
        };
        page.compose();
        page
    }

    /// The round, as `Review::round` read it from the agent's review; when
    /// the host holds none of it, or cannot be reached and none of it was
    /// loaded, the page shows `text`, what the agent read.
    pub fn round_read(
        &mut self,
        read: Result<Option<ui_runtime::review::Round>, ui_runtime::review::ReviewError>,
        text: &str,
    ) {
        let Some(round) = &mut self.round else {
            return;
        };
        let read = match read {
            Ok(read) => read,
            Err(error) => {
                round.unreachable = ui_runtime::review::unreachable(&error);
                None
            }
        };
        match read {
            Some(read) => {
                if let Some(number) = read.summary.comments.iter().filter_map(|c| c.round).min() {
                    round.number = number;
                }
                // Every file of a past round opens, a flagged one too.
                for notice in read.state.notices {
                    if !self.state.notices.contains(&notice) {
                        self.state.notices.push(notice);
                    }
                }
                self.state.toggled_files = read.state.toggled_files;
                self.update(Fetched {
                    summary: Some(Arc::new(read.summary)),
                    all: read
                        .files
                        .into_iter()
                        .map(|file| (file.path.clone(), Arc::new(file)))
                        .collect(),
                    ..Fetched::default()
                });
            }
            None => {
                round.gone = Some(text.to_owned());
                self.scroll = 0;
            }
        }
    }

    /// The text the agent read, shown when the round is gone.
    fn gone(&self) -> Option<&str> {
        self.round.as_ref().and_then(|round| round.gone.as_deref())
    }

    /// The page as composed now.
    pub fn page(&self) -> &ReviewPage {
        &self.page
    }

    /// What the page was last handed.
    pub fn fetched(&self) -> &Fetched {
        &self.fetched
    }

    /// The row the keys are on.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// The person's choices on it.
    pub fn state(&self) -> &ReviewPageState {
        &self.state
    }

    /// The review's fetches changed: compose the page again from them.
    pub fn update(&mut self, fetched: Fetched) {
        self.fetched = fetched;
        self.compose();
    }

    /// Says or stops saying something the summary does not.
    pub fn notice(&mut self, notice: ReviewNotice, shown: bool) {
        let at = self.state.notices.iter().position(|held| *held == notice);
        match (at, shown) {
            (None, true) => self.state.notices.push(notice),
            (Some(at), false) => {
                self.state.notices.remove(at);
            }
            _ => return,
        }
        self.compose();
    }

    /// The view the page shows, as composing it decided.
    pub fn view(&self) -> ReviewView {
        self.page
            .header
            .as_ref()
            .map_or(ReviewView::All, |header| header.view)
    }

    /// The files the view shown still needs the bodies of, in page order.
    pub fn to_fetch(&self) -> &[String] {
        &self.page.to_fetch
    }

    /// Every ask for more context made on the page, answered or not.
    pub fn asks(&self) -> &[ReviewContextAsk] {
        &self.state.asked
    }

    /// An ask for more context that could not be answered: the gap offers
    /// it again.
    pub fn unask(&mut self, ask: &ReviewContextAsk) {
        self.state.asked.retain(|asked| asked != ask);
        self.compose();
    }

    fn compose(&mut self) {
        let fetched = &self.fetched;
        self.page = review_page_of(
            fetched.summary.as_deref(),
            |view| {
                match view {
                    ReviewView::All => &fetched.all,
                    ReviewView::SinceLast => &fetched.since_last,
                }
                .values()
                .cloned()
                .collect()
            },
            &fetched.context,
            &self.state,
        );
        let stops = self.stops();
        let kept = self
            .cursor_key
            .as_ref()
            .and_then(|key| self.row_index(key))
            .filter(|at| stops.contains(at));
        self.cursor = match kept {
            Some(at) => at,
            // A page just come in starts on its first change: that is what
            // it is for.
            None => self.first_change().unwrap_or(0),
        };
        self.cursor_key = self.page.rows.get(self.cursor).map(|row| row.key.clone());
        if self
            .mark
            .as_ref()
            .is_some_and(|mark| self.row_index(mark).is_none())
        {
            self.mark = None;
        }
    }

    fn row_index(&self, key: &str) -> Option<usize> {
        self.page.rows.iter().position(|row| row.key == key)
    }

    /// The rows the cursor stops on: what a key can act on.
    fn stops(&self) -> Vec<usize> {
        self.page
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| {
                matches!(
                    row.of,
                    ReviewRowOf::File(_)
                        | ReviewRowOf::Line(_)
                        | ReviewRowOf::Comment(_)
                        | ReviewRowOf::Gap(_)
                )
            })
            .map(|(at, _)| at)
            .collect()
    }

    fn first_change(&self) -> Option<usize> {
        let stops = self.stops();
        stops
            .iter()
            .copied()
            .find(|at| {
                matches!(&self.page.rows[*at].of, ReviewRowOf::Line(line) if line.kind != LineKind::Context)
            })
            .or_else(|| stops.first().copied())
    }

    fn set_cursor(&mut self, at: usize) {
        self.cursor = at;
        self.cursor_key = self.page.rows.get(at).map(|row| row.key.clone());
    }

    /// Moves the cursor `by` stops; over the text of a gone round, scrolls
    /// it `by` lines.
    fn step(&mut self, by: isize) {
        if self.gone().is_some() {
            self.scroll = self.scroll.saturating_add_signed(by);
            return;
        }
        let stops = self.stops();
        if stops.is_empty() {
            return;
        }
        let here = stops
            .iter()
            .position(|at| *at >= self.cursor)
            .unwrap_or(stops.len() - 1);
        let next = here.saturating_add_signed(by).min(stops.len() - 1);
        self.set_cursor(stops[next]);
    }

    /// Moves to the next or previous stop after a row `starts` picks.
    fn jump(&mut self, forward: bool, starts: impl Fn(&ReviewRowOf) -> bool) {
        let stops = self.stops();
        // Each group's first stop: the row that starts it, or the first
        // stop after a row the cursor cannot stop on.
        let groups: Vec<usize> = self
            .page
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| starts(&row.of))
            .filter_map(|(at, _)| stops.iter().copied().find(|stop| *stop >= at))
            .collect();
        let found = if forward {
            groups.into_iter().find(|at| *at > self.cursor)
        } else {
            groups.into_iter().rev().find(|at| *at < self.cursor)
        };
        if let Some(at) = found {
            self.set_cursor(at);
        }
    }

    /// The selected rows, first and last: the range being selected, or the
    /// cursor's row.
    fn selected(&self) -> (usize, usize) {
        let mark = self
            .mark
            .as_ref()
            .and_then(|key| self.row_index(key))
            .unwrap_or(self.cursor);
        (mark.min(self.cursor), mark.max(self.cursor))
    }

    fn header(&self) -> Option<&ReviewHeader> {
        self.page.header.as_ref()
    }

    fn row(&self) -> Option<&ReviewRow> {
        self.page.rows.get(self.cursor)
    }

    /// Whether a comment editor has the keys and holds text, for Ctrl+C.
    pub fn editing(&self) -> bool {
        self.composing
            .as_ref()
            .is_some_and(|composing| !composing.editor.is_empty())
    }

    /// Ctrl+C in the comment editor clears it as a kill.
    pub fn kill_field(&mut self) -> bool {
        self.composing
            .as_mut()
            .is_some_and(|composing| composing.editor.kill_all())
    }

    /// A bracketed paste types into the comment being written.
    pub fn paste(&mut self, text: &str) {
        if let Some(composing) = self.composing.as_mut().filter(|c| !c.saving) {
            composing.editor.insert_str(text);
        }
    }

    /// The answer to the last edit sent: a comment being saved closes, or
    /// stays open with its words and why it did not land.
    pub fn edited(&mut self, result: Result<(), String>) {
        let Some(composing) = self.composing.as_mut().filter(|c| c.saving) else {
            return;
        };
        match result {
            Ok(()) => self.composing = None,
            Err(why) => {
                composing.saving = false;
                composing.failed = Some(why);
            }
        }
    }

    /// Takes the cursor to the head of the file at `path`, when the page
    /// lists it.
    pub fn show_file(&mut self, path: &str) {
        let key = format!("f/{path}");
        if let Some(at) = self.row_index(&key) {
            self.mark = None;
            self.set_cursor(at);
        }
    }

    /// Whose changes these are: the agent's name and its place, for the
    /// header.
    pub fn set_owner(&mut self, name: &str, place: &str) {
        self.owner = (name.to_owned(), place.to_owned());
    }

    pub fn scroll_by(&mut self, delta: isize) {
        self.step(delta);
    }

    /// Where a drag selects text.
    pub fn stream_area(&self) -> Rect {
        self.stream_area
    }

    /// The columns of each of the body's rows on screen that are not code.
    pub fn stream_gutters(&self) -> &[u16] {
        &self.stream_gutters
    }

    /// A click on the page puts the cursor on the row under it.
    pub fn click(&mut self, x: u16, y: u16) {
        if self.asking.is_some()
            || self.composing.is_some()
            || !self.stream_area.contains(Position::new(x, y))
        {
            return;
        }
        let line = usize::from(y - self.stream_area.y);
        if let Some(Some(at)) = self.rows_on_screen.get(line).copied()
            && self.stops().contains(&at)
        {
            self.mark = None;
            self.set_cursor(at);
        }
    }

    /// Asks before discarding the drafts, when there are any.
    pub fn ask_discard(&mut self) -> bool {
        let drafts = self.header().map_or(0, |header| header.drafts);
        if drafts == 0 {
            return false;
        }
        self.asking = Some(Asking::new(Confirm::Discard { drafts }));
        true
    }

    pub fn key(&mut self, key: KeyEvent) -> ReviewAction {
        if let Some(mut asking) = self.asking.take() {
            return match asking.key(key) {
                Some(true) => ReviewAction::Edit {
                    edits: vec![asking.edit()],
                    reopen: asking.what == Confirm::StartOver,
                },
                Some(false) => ReviewAction::None,
                None => {
                    self.asking = Some(asking);
                    ReviewAction::None
                }
            };
        }
        if self.composing.is_some() {
            return self.composing_key(key);
        }
        if self.round.is_some() {
            return self.round_key(key);
        }
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        match key.code {
            KeyCode::Esc if self.mark.is_some() => self.mark = None,
            KeyCode::Char('q') | KeyCode::Esc => return ReviewAction::Close,
            KeyCode::Char('r') => return ReviewAction::Open { refresh: true },
            KeyCode::Tab => return self.switch_view(),
            KeyCode::Char('D') => {
                if !self.ask_discard() {
                    return ReviewAction::Notice("no drafts to discard");
                }
            }
            KeyCode::Char('S') if self.header().is_some_and(|header| header.start_over) => {
                self.asking = Some(Asking::new(Confirm::StartOver));
            }
            KeyCode::Down | KeyCode::Up if shift => {
                if self.mark.is_none() {
                    self.mark = self.cursor_key.clone();
                }
                self.step(if key.code == KeyCode::Down { 1 } else { -1 });
            }
            KeyCode::Char('v') => {
                self.mark = match self.mark {
                    Some(_) => None,
                    None => self.cursor_key.clone(),
                };
            }
            KeyCode::Char('j') | KeyCode::Down => self.step(1),
            KeyCode::Char('k') | KeyCode::Up => self.step(-1),
            KeyCode::PageDown | KeyCode::Char(' ') => {
                self.step(self.height.saturating_sub(2).max(1) as isize)
            }
            KeyCode::PageUp => self.step(-(self.height.saturating_sub(2).max(1) as isize)),
            KeyCode::Char('g') | KeyCode::Home => self.step(isize::MIN / 2),
            KeyCode::Char('G') | KeyCode::End => self.step(isize::MAX / 2),
            KeyCode::Char('J') => self.jump(true, |row| matches!(row, ReviewRowOf::Hunk(_))),
            KeyCode::Char('K') => self.jump(false, |row| matches!(row, ReviewRowOf::Hunk(_))),
            KeyCode::Char(']') => self.jump(true, |row| matches!(row, ReviewRowOf::File(_))),
            KeyCode::Char('[') => self.jump(false, |row| matches!(row, ReviewRowOf::File(_))),
            KeyCode::Char('f') => self.fold_here(),
            KeyCode::Char('c') => return self.compose_new(),
            KeyCode::Char('e') => return self.expand(false),
            KeyCode::Char('d') => return self.delete(),
            KeyCode::Enter => return self.enter(),
            _ => {}
        }
        ReviewAction::None
    }

    /// A key on a sent round's page: it moves, folds and goes back, and
    /// changes nothing.
    fn round_key(&mut self, key: KeyEvent) -> ReviewAction {
        let page = self.height.saturating_sub(2).max(1) as isize;
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return ReviewAction::Close,
            KeyCode::Char('j') | KeyCode::Down => self.step(1),
            KeyCode::Char('k') | KeyCode::Up => self.step(-1),
            KeyCode::PageDown | KeyCode::Char(' ') => self.step(page),
            KeyCode::PageUp => self.step(-page),
            KeyCode::Char('g') | KeyCode::Home => self.step(isize::MIN / 2),
            KeyCode::Char('G') | KeyCode::End => self.step(isize::MAX / 2),
            KeyCode::Char(']') => self.jump(true, |row| matches!(row, ReviewRowOf::File(_))),
            KeyCode::Char('[') => self.jump(false, |row| matches!(row, ReviewRowOf::File(_))),
            KeyCode::Enter => match self.row().map(|row| row.of.clone()) {
                Some(ReviewRowOf::File(head)) => {
                    self.state.toggle_file(&head.path);
                    self.compose();
                }
                Some(ReviewRowOf::Comment(comment))
                    if comment.folded || comment.at == ReviewCommentAt::Outdated =>
                {
                    self.state.toggle_comment(&comment.id);
                    self.compose();
                }
                _ => {}
            },
            _ => {}
        }
        ReviewAction::None
    }

    /// Tab: the other view, where there are two.
    fn switch_view(&mut self) -> ReviewAction {
        let Some(header) = self.header() else {
            return ReviewAction::None;
        };
        if header.views.len() < 2 {
            return ReviewAction::Notice("since your last review shows once a review is sent");
        }
        let next = match header.view {
            ReviewView::All => ReviewView::SinceLast,
            ReviewView::SinceLast => ReviewView::All,
        };
        self.state.view = Some(next);
        self.mark = None;
        self.cursor_key = None;
        self.scroll = 0;
        self.compose();
        ReviewAction::None
    }

    /// Enter does what the row under the cursor is for.
    fn enter(&mut self) -> ReviewAction {
        match self.row().map(|row| row.of.clone()) {
            Some(ReviewRowOf::File(head)) if self.mark.is_none() => {
                self.state.toggle_file(&head.path);
                self.compose();
                ReviewAction::None
            }
            Some(ReviewRowOf::Comment(comment)) => {
                if comment.folded || comment.at == ReviewCommentAt::Outdated {
                    self.state.toggle_comment(&comment.id);
                    self.compose();
                    return ReviewAction::None;
                }
                match comment.state {
                    ReviewCommentState::Draft => self.compose_edit(&comment),
                    ReviewCommentState::Sent { .. } => {
                        ReviewAction::Notice("a sent comment stays as it was sent; d deletes it")
                    }
                }
            }
            Some(ReviewRowOf::Gap(_)) => self.expand(true),
            _ => self.compose_new(),
        }
    }

    /// `f`: folds the file the cursor is in, or opens it, the cursor on its
    /// head.
    fn fold_here(&mut self) {
        let Some(head) = self.page.rows[..=self.cursor.min(self.page.rows.len().saturating_sub(1))]
            .iter()
            .rev()
            .find_map(|row| match &row.of {
                ReviewRowOf::File(head) => Some(head.path.clone()),
                _ => None,
            })
        else {
            return;
        };
        self.state.toggle_file(&head);
        self.mark = None;
        self.cursor_key = Some(format!("f/{head}"));
        self.compose();
    }

    /// Loads more context from the gap under the cursor: upward from the
    /// code below it, or downward from the code above.
    fn expand(&mut self, up: bool) -> ReviewAction {
        let Some(ReviewRowOf::Gap(gap)) = self.row().map(|row| &row.of) else {
            return ReviewAction::None;
        };
        if gap.loading {
            return ReviewAction::None;
        }
        let ask = match up {
            true => gap.up.clone().or_else(|| gap.down.clone()),
            false => gap.down.clone().or_else(|| gap.up.clone()),
        };
        let Some(ask) = ask else {
            return ReviewAction::None;
        };
        self.state.asked.push(ask.clone());
        self.compose();
        ReviewAction::Context(ask)
    }

    /// `c`: a comment on the selected lines, or on the whole file from its
    /// head alone.
    fn compose_new(&mut self) -> ReviewAction {
        let (from, to) = self.selected();
        let Some(anchor) = ui_view::anchor(&self.page, from, to) else {
            return ReviewAction::Notice("a comment goes on lines, or on a file from its heading");
        };
        let under = self.page.rows[to].key.clone();
        self.composing = Some(Composing {
            under,
            whole_file: anchor.start == 0,
            anchor: ReviewAnchor::to_wire(&anchor),
            id: ui_runtime::inputs::input_id(),
            editing: false,
            editor: Editor::default(),
            saving: false,
            failed: None,
        });
        self.mark = None;
        ReviewAction::None
    }

    /// Enter on a draft: its words, to change.
    fn compose_edit(&mut self, comment: &ReviewCommentRow) -> ReviewAction {
        let Some(anchor) = self
            .fetched
            .summary
            .as_ref()
            .and_then(|summary| summary.comments.iter().find(|held| held.id == comment.id))
            .and_then(|held| held.anchor.clone())
        else {
            return ReviewAction::None;
        };
        let mut editor = Editor::default();
        editor.set(&comment.text, Vec::new());
        self.composing = Some(Composing {
            under: format!("c/{}", hex(&comment.id)),
            whole_file: comment.at == ReviewCommentAt::File,
            anchor,
            id: comment.id.clone(),
            editing: true,
            editor,
            saving: false,
            failed: None,
        });
        ReviewAction::None
    }

    /// `d` on a comment deletes it: a draft goes; a sent one leaves the
    /// page, and the chat still holds it.
    fn delete(&mut self) -> ReviewAction {
        let Some(ReviewRowOf::Comment(comment)) = self.row().map(|row| &row.of) else {
            return ReviewAction::Notice("d deletes the comment the cursor is on");
        };
        ReviewAction::Edit {
            edits: vec![ReviewEdit {
                of: Some(review_edit::Of::Delete(comment.id.clone())),
            }],
            reopen: false,
        }
    }

    fn composing_key(&mut self, key: KeyEvent) -> ReviewAction {
        let Some(composing) = &mut self.composing else {
            return ReviewAction::None;
        };
        if key.code == KeyCode::Esc {
            self.composing = None;
            return ReviewAction::None;
        }
        if composing.saving {
            return ReviewAction::None;
        }
        if key.code == KeyCode::Enter && !key.modifiers.contains(KeyModifiers::SHIFT) {
            let words = composing.editor.text().trim().to_owned();
            let edit = match (composing.editing, words.is_empty()) {
                (false, true) => {
                    self.composing = None;
                    return ReviewAction::None;
                }
                (true, true) => review_edit::Of::Delete(composing.id.clone()),
                (_, false) => review_edit::Of::Put(ReviewPutComment {
                    id: composing.id.clone(),
                    anchor: Some(composing.anchor.clone()),
                    text: words,
                }),
            };
            composing.saving = true;
            composing.failed = None;
            return ReviewAction::Edit {
                edits: vec![ReviewEdit { of: Some(edit) }],
                reopen: false,
            };
        }
        composing.editor.key(key);
        ReviewAction::None
    }

    /// Paints the page over the whole of `area`. `footer` replaces the key
    /// hint when the app has something to say.
    pub fn draw(
        &mut self,
        paint: &mut Paint<'_>,
        area: Rect,
        footer: Option<Line<'static>>,
        theme: Theme,
    ) {
        let width = usize::from(area.width);
        let height = usize::from(area.height);
        let top = self.top_lines(width, theme);
        let body = self.body(width, theme);
        let hint = footer.unwrap_or_else(|| self.hint_line(width, theme));
        let room = height.saturating_sub(1);
        let body_height = room.saturating_sub(top.len());
        self.height = body_height;
        // The cursor's row in view, and the comment editor under it.
        let (first, last) = self.selected();
        let mut wanted = (
            body.spans.get(first).map_or(0, |span| span.0),
            body.spans.get(last).map_or(0, |span| span.1),
        );
        if let Some(end) = body.editor_end {
            wanted.1 = wanted.1.max(end);
        }
        // A gone round's text scrolls by itself, with no rows to follow.
        let follows = self.gone().is_none();
        if follows && wanted.0 < self.scroll {
            self.scroll = wanted.0;
        } else if follows && wanted.1 + 1 > self.scroll + body_height {
            self.scroll = (wanted.1 + 1).saturating_sub(body_height).min(wanted.0);
        }
        self.scroll = self
            .scroll
            .min(body.lines.len().saturating_sub(body_height));
        let shown = self.scroll..(self.scroll + body_height).min(body.lines.len());
        self.stream_gutters = shown
            .clone()
            .map(|line| {
                if body.code.contains(&line) {
                    body.gutter as u16
                } else {
                    0
                }
            })
            .collect();
        self.rows_on_screen = shown
            .clone()
            .map(|line| {
                body.spans
                    .iter()
                    .position(|(from, to)| (*from..=*to).contains(&line))
            })
            .collect();
        let body_top = top.len();
        let mut lines = top;
        lines.extend(body.lines[shown].iter().cloned());
        while lines.len() < room {
            lines.push(Line::default());
        }
        lines.push(hint);
        paint.render_widget(Paragraph::new(lines), area);
        self.stream_area = Rect {
            x: area.x,
            y: area.y + body_top as u16,
            width: area.width,
            height: body_height as u16,
        };
        if let Some((line, col)) = body.cursor
            && line >= self.scroll
            && line < self.scroll + body_height
        {
            let y = area.y as usize + body_top + line - self.scroll;
            let x = area.x as usize + col.min(width.saturating_sub(1));
            paint.set_cursor_position(Position::new(x as u16, y as u16));
        }
        if let Some(asking) = &self.asking {
            asking.draw(paint, area, theme);
        }
    }

    /// The lines above the body: a blank keeping the header off the
    /// terminal's edge, the header, the switch between the views when
    /// there are two, and the banners.
    fn top_lines(&self, width: usize, theme: Theme) -> Vec<Line<'static>> {
        let mut lines = vec![Line::default(), self.title(width, theme)];
        if let Some(header) = self.header()
            && header.views.len() > 1
        {
            lines.push(views_line(header, width, theme));
        }
        lines.push(Line::default());
        if !self.page.banners.is_empty() {
            for banner in &self.page.banners {
                lines.extend(banner_lines(banner, width, theme));
            }
            lines.push(Line::default());
        }
        lines
    }

    /// Like the chat's header: whose changes at the left (`name │ path`),
    /// what they are and their counts at the right ("review · since main
    /// at 4f2a1c9 │ 3 files · +12 −4 · 2 drafts").
    fn title(&self, width: usize, theme: Theme) -> Line<'static> {
        let bar = || Span::styled(" │ ", theme.faint());
        let mut right = Vec::new();
        if let Some(round) = &self.round {
            let mut about = format!("review · round {}", round.number);
            if let Some(header) = self.header().filter(|_| round.gone.is_none()) {
                let snapshot: String = header.snapshot.chars().take(7).collect();
                about.push_str(&format!(" at {snapshot}"));
                right = vec![
                    Span::styled(about, theme.faint()),
                    bar(),
                    Span::styled(counted(header.sent, "comment"), theme.muted()),
                ];
            } else {
                right = vec![Span::styled(about, theme.faint())];
            }
        } else if let Some(header) = self.header() {
            let about = match header.view {
                ReviewView::SinceLast => "since your last review".to_owned(),
                ReviewView::All if header.base_branch.is_empty() => "uncommitted".to_owned(),
                ReviewView::All => format!("since {}", header.base_branch),
            };
            let snapshot: String = header.snapshot.chars().take(7).collect();
            let mut counts = format!(
                "{} · +{} −{}",
                counted(header.files, "file"),
                header.added,
                header.removed
            );
            if header.drafts > 0 {
                counts.push_str(&format!(" · {}", counted(header.drafts, "draft")));
            }
            if header.sent > 0 {
                counts.push_str(&format!(" · {} sent", header.sent));
            }
            right = vec![
                Span::styled(format!("review · {about} at {snapshot}"), theme.faint()),
                bar(),
                Span::styled(counts, theme.muted()),
            ];
        }
        let right_width: usize = right.iter().map(|s| text::str_width(&s.content)).sum();
        let right_at = width.saturating_sub(2 + right_width);
        let mut line = Line::from(Span::raw("  "));
        let room = right_at.saturating_sub(2);
        let (name, place) = &self.owner;
        let name = if name.is_empty() { "Review" } else { name };
        push(&mut line, name, theme.bright(), room);
        if !place.is_empty() && text::line_width(&line) + 6 < room {
            line.spans.push(bar());
            push(&mut line, place.clone(), theme.faint(), room);
        }
        if !right.is_empty() && text::line_width(&line) + 2 <= right_at {
            text::pad_to(&mut line, right_at);
            line.spans.extend(right);
        }
        line
    }

    /// Every row as lines, with where each row's lines are.
    fn body(&self, width: usize, theme: Theme) -> Body {
        let digits = self
            .page
            .rows
            .iter()
            .filter_map(|row| match &row.of {
                ReviewRowOf::Line(line) => line.new_line.or(line.old_line),
                _ => None,
            })
            .max()
            .unwrap_or(0)
            .to_string()
            .len()
            .max(3);
        // "▌ " + number + " " + sign + " "
        let gutter = 2 + digits + 3;
        let mut body = Body {
            gutter,
            ..Body::default()
        };
        let (first, last) = self.selected();
        let draw = Draw {
            width,
            digits,
            gutter,
            theme,
            round: self.round.is_some(),
            since_rounds: self
                .header()
                .is_some_and(|header| header.view == ReviewView::All && header.rounds > 0),
        };
        if let Some(text) = self.gone() {
            let room = width.saturating_sub(4).max(1);
            let why = if self.round.as_ref().is_some_and(|round| round.unreachable) {
                "The agent's computer can't be reached and none of this round was loaded, so this \
                 is what the agent read."
            } else {
                "The agent's computer no longer holds this round, so this is what the agent read."
            };
            for part in text::wrap(why, room) {
                let mut line = Line::from(Span::raw("  "));
                push(&mut line, part, theme.muted(), width);
                body.lines.push(line);
            }
            body.lines.push(Line::default());
            for part in text.lines().flat_map(|line| text::wrap(line, room)) {
                let mut line = Line::from(Span::raw("  "));
                push(&mut line, part, theme.text(), width);
                body.lines.push(line);
            }
            return body;
        }
        if self.page.opening {
            let mut line = Line::from(Span::raw("  "));
            let words = match self.round {
                Some(_) => "Reading the round…",
                None => "Opening the review…",
            };
            push(&mut line, words, theme.muted(), width);
            body.lines.push(line);
        }
        if let Some(empty) = self.page.empty {
            let words = match empty {
                ReviewEmpty::Nothing => "Nothing to review: no changes and no comments.",
                ReviewEmpty::NothingSince => {
                    "Nothing changed since your last review. Tab shows all changes."
                }
            };
            let mut line = Line::from(Span::raw("  "));
            push(&mut line, words, theme.muted(), width);
            body.lines.push(line);
            if !self.page.rows.is_empty() {
                body.lines.push(Line::default());
            }
        }
        for (at, row) in self.page.rows.iter().enumerate() {
            let selected = (first..=last).contains(&at);
            let start = body.lines.len();
            match &row.of {
                ReviewRowOf::File(head) => {
                    if at > 0 {
                        body.lines.push(Line::default());
                        body.lines.push(Line::default());
                    }
                    let start = body.lines.len();
                    body.lines.extend(draw.file(head, selected));
                    body.spans.push((start, body.lines.len() - 1));
                    self.editor_under(row, &draw, &mut body);
                    continue;
                }
                ReviewRowOf::Hunk(hunk) => {
                    // Where a hunk sits: the definitions it is in, after a
                    // blank. A hunk that is the whole of a new file, in no
                    // definition, needs no saying where.
                    let after_gap =
                        at > 0 && matches!(self.page.rows[at - 1].of, ReviewRowOf::Gap(_));
                    if !after_gap {
                        body.lines.push(Line::default());
                    }
                    let start = body.lines.len();
                    if !hunk.scope.is_empty() || hunk.new_start > 1 || hunk.old_start > 1 {
                        body.lines
                            .push(draw.break_line(&hunk.scope.join(" › "), theme.muted()));
                    }
                    body.spans
                        .push((start, body.lines.len().saturating_sub(1).max(start)));
                    continue;
                }
                ReviewRowOf::Line(line) => {
                    body.code.insert(body.lines.len());
                    body.lines.push(draw.code(line, selected));
                }
                ReviewRowOf::Gap(gap) => body.lines.push(draw.gap(gap, selected)),
                ReviewRowOf::Comment(comment) => body.lines.extend(draw.comment(comment, selected)),
                ReviewRowOf::Loading { .. } => {
                    let mut line = Line::from(Span::raw(" ".repeat(gutter)));
                    push(&mut line, "loading…", theme.faint(), width);
                    body.lines.push(line);
                }
                ReviewRowOf::NoLines { why, .. } => {
                    let mut line = Line::from(Span::raw(" ".repeat(gutter)));
                    let words = match why {
                        ReviewNoLines::Binary => "a binary file: no lines to show",
                        ReviewNoLines::TooLarge => "too large a change to show here",
                    };
                    push(&mut line, words, theme.faint(), width);
                    body.lines.push(line);
                }
            }
            body.spans.push((start, body.lines.len().saturating_sub(1)));
            self.editor_under(row, &draw, &mut body);
        }
        body
    }

    /// The comment editor, when it opens under `row`.
    fn editor_under(&self, row: &ReviewRow, draw: &Draw, body: &mut Body) {
        let Some(composing) = self.composing.as_ref().filter(|c| c.under == row.key) else {
            return;
        };
        let theme = draw.theme;
        let indent = draw.gutter;
        let placeholder = match (composing.editing, composing.whole_file) {
            (true, _) => "change the draft; empty deletes it",
            (false, true) => "a comment for the agent on this file",
            (false, false) => "a comment for the agent on these lines",
        };
        let room = draw.width.saturating_sub(indent).max(4);
        let (editor, at) = editor_lines(&composing.editor, placeholder, room, theme);
        body.cursor = (!composing.saving).then_some((body.lines.len() + at.0, indent + at.1));
        for line in editor {
            let mut spans = vec![Span::raw(" ".repeat(indent))];
            spans.extend(line.spans);
            body.lines.push(Line::from(spans));
        }
        let said = match (&composing.failed, composing.saving) {
            (Some(why), _) => Some((format!("not saved: {why}"), theme.error())),
            (None, true) => Some(("saving…".to_owned(), theme.faint())),
            (None, false) => None,
        };
        if let Some((words, ink)) = said {
            let mut line = Line::from(Span::raw(" ".repeat(indent + 2)));
            push(&mut line, words, ink, draw.width);
            body.lines.push(line);
        }
        body.editor_end = Some(body.lines.len() - 1);
    }

    /// The keys for what the cursor is on: the key reads first, its action
    /// recedes, as on every hint line.
    fn hint_line(&self, width: usize, theme: Theme) -> Line<'static> {
        let mut keys: Vec<(&str, &str)> = Vec::new();
        if self.gone().is_some() {
            keys.extend([("j/k", "scroll"), ("q", "back")]);
        } else if self.round.is_some() {
            keys.push(("j/k", "move"));
            match self.row().map(|row| &row.of) {
                Some(ReviewRowOf::File(head)) => {
                    keys.push(("enter", if head.folded { "open" } else { "fold" }));
                }
                Some(ReviewRowOf::Comment(comment)) if comment.folded => {
                    keys.push(("enter", "open"));
                }
                _ => {}
            }
            keys.extend([("]/[", "file"), ("q", "back")]);
        } else if let Some(composing) = &self.composing {
            if composing.saving {
                keys.push(("esc", "close"));
            } else {
                keys.extend([("enter", "save"), ("ctrl+j", "newline"), ("esc", "cancel")]);
            }
        } else if self.mark.is_some() {
            keys.extend([
                ("j/k", "extend"),
                ("c", "comment on the lines"),
                ("esc", "clear"),
            ]);
        } else if self.header().is_none() {
            keys.extend([("r", "try again"), ("q", "back")]);
        } else {
            keys.push(("j/k", "move"));
            match self.row().map(|row| &row.of) {
                Some(ReviewRowOf::File(head)) => {
                    keys.push(("c", "comment on the file"));
                    keys.push(("enter", if head.folded { "open" } else { "fold" }));
                }
                Some(ReviewRowOf::Line(_)) => {
                    keys.push(("c", "comment"));
                    keys.push(("v", "range"));
                }
                Some(ReviewRowOf::Comment(comment)) => {
                    if comment.folded {
                        keys.push(("enter", "open"));
                    } else if comment.at == ReviewCommentAt::Outdated {
                        keys.push(("enter", "fold"));
                    } else if comment.state == ReviewCommentState::Draft {
                        keys.push(("enter", "edit"));
                    }
                    keys.push(("d", "delete"));
                }
                Some(ReviewRowOf::Gap(gap)) if !gap.loading => {
                    if gap.up.is_some() {
                        keys.push(("enter", "more ↑"));
                    }
                    if gap.down.is_some() {
                        keys.push(("e", "more ↓"));
                    }
                }
                _ => {}
            }
            let header = self.header().expect("checked above");
            if header.views.len() > 1 {
                keys.push(("tab", "view"));
            }
            if self.page.banners.iter().any(|banner| {
                matches!(
                    banner,
                    ReviewBanner::ChangedSince | ReviewBanner::ChangedSinceYouStarted
                )
            }) {
                keys.push(("r", "refresh"));
            }
            keys.push(("]/[", "file"));
            if header.discard_drafts {
                keys.push(("D", "discard drafts"));
            }
            if header.start_over {
                keys.push(("S", "start over"));
            }
            keys.push(("q", "back"));
        }
        let mut line = Line::from(Span::raw("  "));
        text::push_hints(&mut line, &keys, width.saturating_sub(4), theme);
        line
    }
}

/// The body's lines, and where each row of the page is among them.
#[derive(Default)]
struct Body {
    lines: Vec<Line<'static>>,
    /// Each row's first and last line, in row order.
    spans: Vec<(usize, usize)>,
    /// The lines that are code, whose gutter a copy leaves out.
    code: HashSet<usize>,
    /// The comment editor's cursor: line and column.
    cursor: Option<(usize, usize)>,
    /// The comment editor's last line.
    editor_end: Option<usize>,
    gutter: usize,
}

/// How rows are drawn at one width.
struct Draw {
    width: usize,
    digits: usize,
    gutter: usize,
    theme: Theme,
    /// All changes, after a round has been sent: a file says when it
    /// changed since then.
    since_rounds: bool,
    /// A sent round: its files are where its comments are, not the change
    /// as it stands, so a head says nothing of how the file changes now.
    round: bool,
}

impl Draw {
    fn bar(&self, selected: bool) -> Span<'static> {
        if selected {
            Span::styled("▌ ", self.theme.focus_bar())
        } else {
            Span::raw("  ")
        }
    }

    /// A file's landmark: folded or open, its path bright, how it changed
    /// and its counts, a hairline to the margin; under it, the definitions
    /// its change is in.
    fn file(&self, head: &ReviewFileHead, selected: bool) -> Vec<Line<'static>> {
        let (theme, width) = (self.theme, self.width);
        let mut line = Line::from(self.bar(selected));
        push(
            &mut line,
            if head.folded { "▸ " } else { "▾ " },
            theme.muted(),
            width,
        );
        let path = match &head.old_path {
            Some(old) => format!("{old} → {}", head.path),
            None => head.path.clone(),
        };
        push(&mut line, path, theme.bright(), width);
        // Groups of words after the path, set apart by dots: how it changed,
        // its counts, what it is, its comments.
        let mut said: Vec<Vec<(String, ratatui::style::Style)>> = Vec::new();
        match head.status {
            ui_view::FileStatus::Added => said.push(vec![("new".into(), theme.faint())]),
            ui_view::FileStatus::Deleted => said.push(vec![("deleted".into(), theme.faint())]),
            _ => {}
        }
        if head.binary {
            said.push(vec![("binary".into(), theme.faint())]);
        }
        if head.in_change && (head.added > 0 || head.removed > 0) {
            let mut counts = Vec::new();
            if head.added > 0 {
                counts.push((format!("+{}", head.added), theme.ok()));
            }
            if head.removed > 0 {
                let gap = if head.added > 0 { " " } else { "" };
                counts.push((format!("{gap}\u{2212}{}", head.removed), theme.error()));
            }
            said.push(counts);
        }
        for flag in &head.flags {
            said.push(vec![(
                match flag {
                    ReviewFileFlag::Generated => "generated",
                    ReviewFileFlag::Lockfile => "lock file",
                    ReviewFileFlag::Vendored => "vendored",
                    ReviewFileFlag::Large => "large",
                }
                .into(),
                theme.faint(),
            )]);
        }
        if head.comments > 0 {
            said.push(vec![(counted(head.comments, "comment"), theme.attention())]);
        }
        if self.round {
            // Nothing of how the file changes now.
        } else if !head.in_change {
            said.push(vec![("no longer changed".into(), theme.faint())]);
        } else if self.since_rounds && head.changed_since_last {
            said.push(vec![(
                "changed since your last review".into(),
                theme.faint(),
            )]);
        }
        for (at, group) in said.into_iter().enumerate() {
            let gap = if at == 0 { "  " } else { " · " };
            push(&mut line, gap, theme.faint(), width);
            for (words, ink) in group {
                push(&mut line, words, ink, width);
            }
        }
        line.spans.push(Span::raw(" "));
        let rule = width.saturating_sub(text::line_width(&line) + 2);
        push(&mut line, "─".repeat(rule), theme.hairline(), width);
        let mut lines = vec![line];
        let touched: Vec<&str> = head
            .outline
            .iter()
            .filter(|entry| entry.row.is_some())
            .map(|entry| entry.label.as_str())
            .collect();
        if !head.folded && !touched.is_empty() {
            let mut line = Line::from(Span::raw("    "));
            push(&mut line, "in ", theme.faint(), width);
            push(&mut line, touched.join(" · "), theme.muted(), width);
            lines.push(line);
        }
        lines
    }

    /// A break in the code: `⋯` in the number column, then `words` and a
    /// hairline to the margin, as where a hunk sits.
    fn break_line(&self, words: &str, ink: ratatui::style::Style) -> Line<'static> {
        let (theme, width, digits) = (self.theme, self.width, self.digits);
        let mut line = Line::from(Span::raw("  "));
        push(
            &mut line,
            format!("{:>digits$}   ", "⋯"),
            theme.muted(),
            width,
        );
        if !words.is_empty() {
            let room = width.saturating_sub(text::line_width(&line) + 4);
            let words = text::expand_tabs(words);
            push(&mut line, text::clip_to_width(&words, room), ink, width);
            push(&mut line, " ", theme.faint(), width);
        }
        let rule = width.saturating_sub(text::line_width(&line) + 2);
        push(&mut line, "─".repeat(rule), theme.hairline(), width);
        line
    }

    /// A gap: how many lines it holds and how far each ask reaches.
    fn gap(&self, gap: &ReviewGap, selected: bool) -> Line<'static> {
        let reach = |ask: &ReviewContextAsk, arrow: &str| match &ask.definition {
            // Upward an ask reaches a definition's start; downward, its end.
            Some(label) if arrow == "↑" => format!("{arrow} to {label}"),
            Some(label) => format!("{arrow} to the end of {label}"),
            None => format!(
                "{arrow} {}",
                counted(ask.end.saturating_sub(ask.start) + 1, "line")
            ),
        };
        let mut words = match gap.hidden {
            Some(hidden) => counted(hidden, "line"),
            None => "the rest of the file".to_owned(),
        };
        if gap.loading {
            words.push_str(" · loading…");
        } else {
            if let Some(up) = &gap.up {
                words.push_str(&format!(" · {}", reach(up, "↑")));
            }
            if let Some(down) = &gap.down {
                words.push_str(&format!(" · {}", reach(down, "↓")));
            }
        }
        let digits = self.digits;
        let mut line = Line::from(self.bar(selected));
        push(
            &mut line,
            format!("{:>digits$}   ", "⋮"),
            self.theme.faint(),
            self.width,
        );
        push(&mut line, words, self.theme.faint(), self.width);
        line
    }

    /// One line of code: its number, its sign, its words coloured as the
    /// host highlighted them, the words that changed from its partner
    /// heavier; a commented line's number in the attention ink.
    fn code(&self, code: &ReviewCodeLine, selected: bool) -> Line<'static> {
        let (theme, width, digits) = (self.theme, self.width, self.digits);
        let (sign, base) = match code.kind {
            LineKind::Added => ('+', theme.diff_added()),
            LineKind::Removed => ('-', theme.diff_removed()),
            LineKind::Context => (' ', theme.diff_context()),
        };
        let mut row = Line::from(self.bar(selected));
        let number = match code.new_line.or(code.old_line) {
            Some(number) => format!("{number:>digits$}"),
            None => " ".repeat(digits),
        };
        let ink = if code.commented {
            theme.attention()
        } else {
            theme.muted()
        };
        push(&mut row, number, ink, width);
        push(&mut row, " ", theme.muted(), width);
        push(&mut row, format!("{sign} "), base, width);
        row.spans.extend(self.runs(
            &code.runs,
            &code.text,
            base,
            width.saturating_sub(text::line_width(&row)),
        ));
        if code.kind != LineKind::Context {
            text::fill(&mut row, base, width);
        }
        row
    }

    /// A line's runs as spans within `room` columns, tabs widened.
    fn runs(
        &self,
        runs: &[ui_view::ReviewRun],
        text: &str,
        base: ratatui::style::Style,
        room: usize,
    ) -> Vec<Span<'static>> {
        let theme = self.theme;
        let mut pieces: Vec<(String, ratatui::style::Style)> = if runs.is_empty() {
            vec![(text.to_owned(), base)]
        } else {
            runs.iter()
                .map(|run| {
                    let mut style = theme.syntax_on_diff(base, run.kind);
                    if run.changed {
                        style = style.add_modifier(Modifier::BOLD);
                    }
                    (run.text.clone(), style)
                })
                .collect()
        };
        // Tabs to the next stop, counted across runs.
        let mut column = 0;
        for (words, _) in &mut pieces {
            if words.contains('\t') {
                let mut out = String::new();
                for c in words.chars() {
                    if c == '\t' {
                        let to = (column / 4 + 1) * 4;
                        out.extend(std::iter::repeat_n(' ', to - column));
                        column = to;
                    } else {
                        out.push(c);
                        column += text::str_width(c.encode_utf8(&mut [0; 4]));
                    }
                }
                *words = out;
            } else {
                column += text::str_width(words);
            }
        }
        let mut line = Line::default();
        text::push_runs(&mut line, &pieces, room);
        line.spans
    }

    /// A comment under its lines: a rail in the attention ink for a draft
    /// and the muted one for a sent comment, a line saying which it is and
    /// where it sits, the code it was written on when that is not drawn
    /// above it, then its words. An outdated comment folds to one line.
    fn comment(&self, comment: &ReviewCommentRow, selected: bool) -> Vec<Line<'static>> {
        let (theme, width) = (self.theme, self.width);
        let indent = self.gutter;
        let rail = match comment.state {
            ReviewCommentState::Draft => theme.attention(),
            ReviewCommentState::Sent { .. } => theme.muted(),
        };
        let lead = |selected: bool| {
            let mut line = Line::from(self.bar(selected));
            line.spans.push(Span::raw(" ".repeat(indent - 2)));
            push(&mut line, "│ ", rail, width);
            line
        };
        let mut about: Vec<(String, ratatui::style::Style)> = vec![match comment.state {
            ReviewCommentState::Draft => ("Draft".into(), theme.attention()),
            ReviewCommentState::Sent { round } => (format!("Sent in round {round}"), theme.muted()),
        }];
        match comment.at {
            ReviewCommentAt::Lines { start, end } if !comment.snippet.is_empty() => {
                about.push((lines_words(start, end), theme.faint()));
            }
            ReviewCommentAt::Lines { .. } => {}
            ReviewCommentAt::File => about.push(("on the file".into(), theme.faint())),
            ReviewCommentAt::Outdated => about.push(("outdated".into(), theme.warning())),
        }
        if comment.changed_near {
            about.push(("the code near it changed".into(), theme.warning()));
        }
        if !comment.scope.is_empty() && !comment.snippet.is_empty() {
            about.push((format!("in {}", comment.scope.join(" › ")), theme.faint()));
        }
        let mut head = lead(selected);
        if comment.folded {
            push(&mut head, "▸ ", theme.muted(), width);
        }
        for (at, (words, ink)) in about.into_iter().enumerate() {
            if at > 0 {
                push(&mut head, " · ", theme.faint(), width);
            }
            push(&mut head, words, ink, width);
        }
        if comment.folded {
            let first = text::first_line(&comment.text);
            push(&mut head, " · ", theme.faint(), width);
            push(&mut head, first, theme.text(), width);
            return vec![head];
        }
        let mut lines = vec![head];
        for code in &comment.snippet {
            let (sign, base) = match code.kind {
                LineKind::Added => ('+', theme.diff_added()),
                LineKind::Removed => ('-', theme.diff_removed()),
                LineKind::Context => (' ', theme.diff_context()),
            };
            let mut line = lead(selected);
            push(&mut line, format!("{sign} "), base, width);
            let room = width.saturating_sub(text::line_width(&line) + 1);
            line.spans
                .extend(self.runs(&code.runs, &code.text, base, room));
            lines.push(line);
        }
        let room = width.saturating_sub(indent + 3).max(1);
        for part in text::wrap(&comment.text, room) {
            let mut line = lead(selected);
            push(&mut line, part, theme.text(), width);
            lines.push(line);
        }
        lines
    }
}

/// The switch between the views: the one shown bright, each with the files
/// it lists.
fn views_line(header: &ReviewHeader, width: usize, theme: Theme) -> Line<'static> {
    let mut line = Line::from(Span::raw("  "));
    for (at, tab) in header.views.iter().enumerate() {
        if at > 0 {
            push(&mut line, "   ", theme.faint(), width);
        }
        let name = match tab.view {
            ReviewView::All => "All changes",
            ReviewView::SinceLast => "Since your last review",
        };
        let ink = if tab.view == header.view {
            theme.emphasis()
        } else {
            theme.muted()
        };
        push(&mut line, name, ink, width);
        push(&mut line, format!(" {}", tab.files), theme.faint(), width);
    }
    line
}

/// A banner: a mark, what it says, and the key that acts on it.
fn banner_lines(banner: &ReviewBanner, width: usize, theme: Theme) -> Vec<Line<'static>> {
    let (words, ink, key): (String, _, Option<(&str, &str)>) = match banner {
        ReviewBanner::ChangedSinceYouStarted => (
            "Changed since you started. Refreshing moves your drafts to the code as it is now."
                .into(),
            theme.warning(),
            Some(("r", "refresh")),
        ),
        ReviewBanner::ChangedSince => (
            "The agent changed code since this snapshot.".into(),
            theme.warning(),
            Some(("r", "refresh")),
        ),
        ReviewBanner::MovedDrafts { count } => (
            format!(
                "Refreshing moved {}; any whose lines were edited are now on their files.",
                counted(*count, "draft")
            ),
            theme.muted(),
            None,
        ),
        ReviewBanner::MergeUnderway => (
            "A merge or rebase was under way: conflicted files hold their markers.".into(),
            theme.warning(),
            None,
        ),
        ReviewBanner::NotOffered => (
            "Reviews aren't offered on this agent's computer: its reviews experiment is off, or \
             its amux predates them."
                .into(),
            theme.muted(),
            None,
        ),
        ReviewBanner::NotARepository => (
            "This agent's folder isn't a git repository, so it has no review.".into(),
            theme.muted(),
            None,
        ),
        ReviewBanner::RoundGone => (
            "The round's snapshot is gone from the agent's computer: its comments show with \
             the code saved with them."
                .into(),
            theme.muted(),
            None,
        ),
        ReviewBanner::SnapshotFailed { message } => (
            format!("Couldn't take a snapshot of the working tree: {message}"),
            theme.error(),
            Some(("r", "try again")),
        ),
        ReviewBanner::Unreachable => (
            "This agent's computer can't be reached: what was loaded still reads, and changes \
             wait for it."
                .into(),
            theme.warning(),
            None,
        ),
    };
    let room = width.saturating_sub(6).max(1);
    let mut lines = Vec::new();
    for (at, part) in text::wrap(&words, room).into_iter().enumerate() {
        let mut line = Line::from(Span::raw("  "));
        push(&mut line, if at == 0 { "● " } else { "  " }, ink, width);
        push(&mut line, part, theme.text(), width);
        lines.push(line);
    }
    if let Some((key, action)) = key
        && let Some(last) = lines.last_mut()
        && text::line_width(last) + 4 + key.len() + action.len() <= width
    {
        push(last, "  ", theme.faint(), width);
        push(last, key, theme.emphasis(), width);
        push(last, format!(" {action}"), theme.faint(), width);
    }
    lines
}

/// "1 file", "3 files".
fn counted(count: u32, noun: &str) -> String {
    match count {
        1 => format!("1 {noun}"),
        n => format!("{n} {noun}s"),
    }
}

/// "line 41", "lines 41–42".
fn lines_words(start: u32, end: u32) -> String {
    if start == end {
        format!("line {start}")
    } else {
        format!("lines {start}–{end}")
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
