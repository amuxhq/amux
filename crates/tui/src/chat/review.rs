//! The review page: a full-screen reading of the agent's working-tree diff,
//! frozen when the page opened, with the person's comments on its lines.
//!
//! The changed files are listed on the left, the shared changes list the
//! Overview also draws; beside it, one continuous stream of every file's
//! hunks. The list follows the stream, and picking a file takes the stream
//! to it. The document is ui-view's [`review_doc`]; this module owns the
//! cursor, the scroll, the list's filter and the comment editor. Comments become one Review
//! attachment in the chat's draft, which the chat keeps in step with the
//! page as comments are saved and deleted.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame as Paint;
use ratatui::layout::{Position, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ui_view::{LineKind, ReviewDoc, review_doc};
use wire::{Attachment, Diff, Review, ReviewComment, attachment};

use super::changes::{changes_lines, ordered};
use super::composer::editor_lines;
use crate::editor::Editor;
use crate::text::{self, push};
use crate::theme::Theme;

/// A place a comment can go: a file as a whole, or one line of a hunk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    File(usize),
    Line {
        file: usize,
        hunk: usize,
        line: usize,
    },
}

/// Which side of the page has the keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Side {
    List,
    Stream,
}

/// The file list's width: a quarter of the page, within these bounds.
const LIST_MIN: usize = 24;
const LIST_MAX: usize = 36;
/// Narrower than this, the list shows only while it has the keys.
const LIST_ALWAYS: usize = 100;
/// The list's words sit this far in from its edges.
const LIST_INSET: usize = 2;

/// What a key on the page asks of the chat.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReviewAction {
    None,
    /// Back to the chat; the page stays behind its token.
    Close,
    /// The comments changed: bring the draft's token up to date.
    Comments,
}

/// A comment being written or edited under its target.
#[derive(Debug)]
struct Composing {
    target: Target,
    /// The comment being edited, by its index in the page's comments.
    editing: Option<usize>,
    editor: Editor,
}

#[derive(Debug)]
pub struct ReviewPage {
    diff: Diff,
    patch: String,
    comments: Vec<ReviewComment>,
    doc: ReviewDoc,
    targets: Vec<Target>,
    /// The selected target.
    at: usize,
    /// The first document line on screen.
    scroll: usize,
    composing: Option<Composing>,
    /// The body's height at the last draw, for paging.
    height: usize,
    /// Whose changes these are, for the header: the agent's name and where
    /// it works, as the chat's header says them.
    owner: (String, String),
    side: Side,
    /// The list's filter, while one is typed or kept.
    filter: Option<Editor>,
    filtering: bool,
    /// The document's files in the list's order, as the filter leaves them.
    order: Vec<usize>,
    /// The list's first line on screen.
    list_top: usize,
    /// The list's files on the last frame, for clicks: (row, columns, file).
    list_spots: Vec<(u16, (u16, u16), usize)>,
    /// The list's files on the last frame by the column's own lines.
    list_rows: Vec<(usize, usize)>,
}

impl ReviewPage {
    pub fn new(diff: Diff, patch: String) -> ReviewPage {
        let doc = review_doc(&diff, &patch, &[]);
        let mut page = ReviewPage {
            diff,
            patch,
            comments: Vec::new(),
            doc,
            targets: Vec::new(),
            at: 0,
            scroll: 0,
            composing: None,
            height: 0,
            owner: (String::new(), String::new()),
            side: Side::Stream,
            filter: None,
            filtering: false,
            order: Vec::new(),
            list_top: 0,
            list_spots: Vec::new(),
            list_rows: Vec::new(),
        };
        page.reorder();
        // Start on the first changed line: that is what the page is for.
        page.at = page
            .targets
            .iter()
            .position(|target| {
                matches!(target, Target::Line { .. })
                    && line_of(&page.doc, *target)
                        .is_some_and(|line| line.kind != LineKind::Context)
            })
            .unwrap_or(0);
        page
    }

    /// `files` of the document as the shared view groups them by folder.
    fn changes(&self, files: impl IntoIterator<Item = usize>) -> ui_view::Changes {
        ui_view::changes(&Diff {
            files: files
                .into_iter()
                .map(|f| {
                    let file = &self.doc.files[f];
                    wire::DiffFile {
                        path: file.path.clone(),
                        added: file.added,
                        removed: file.removed,
                        ..wire::DiffFile::default()
                    }
                })
                .collect(),
            ..Diff::default()
        })
    }

    /// The list's order, as the filter leaves it, and the targets in that
    /// order; the selection stays where it was when it still can.
    fn reorder(&mut self) {
        let was = (!self.targets.is_empty()).then(|| self.target());
        let needle = self
            .filter
            .as_ref()
            .map(|filter| filter.text().trim().to_lowercase())
            .filter(|needle| !needle.is_empty());
        self.order = ordered(&self.changes(0..self.doc.files.len()))
            .into_iter()
            .filter(|path| {
                needle
                    .as_ref()
                    .is_none_or(|needle| path.to_lowercase().contains(needle))
            })
            .filter_map(|path| self.doc.files.iter().position(|file| file.path == path))
            .collect();
        self.targets = targets(&self.doc, &self.order);
        self.at = was
            .and_then(|was| self.targets.iter().position(|target| *target == was))
            .unwrap_or(0);
    }

    pub fn comments(&self) -> &[ReviewComment] {
        &self.comments
    }

    /// The draft's token for this review, carrying the frozen diff and
    /// every comment.
    pub fn attachment(&self) -> Attachment {
        Attachment {
            of: Some(attachment::Of::Review(Review {
                diff: Some(self.diff.clone()),
                comments: self.comments.clone(),
            })),
        }
    }

    /// Whether `attachment` is this page's token: a review of the same
    /// patch.
    pub fn owns(&self, attachment: &Attachment) -> bool {
        let Some(attachment::Of::Review(review)) = &attachment.of else {
            return false;
        };
        let theirs = review.diff.as_ref().and_then(|diff| diff.patch.as_ref());
        theirs.map(|patch| &patch.hash) == self.diff.patch.as_ref().map(|patch| &patch.hash)
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
        if let Some(composing) = &mut self.composing {
            composing.editor.insert_str(text);
        }
    }

    pub fn key(&mut self, key: KeyEvent) -> ReviewAction {
        if self.composing.is_some() {
            return self.composing_key(key);
        }
        if self.filtering {
            self.filter_key(key);
            return ReviewAction::None;
        }
        if self.side == Side::List {
            return self.list_key(key);
        }
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return ReviewAction::Close,
            KeyCode::Tab => {
                self.side = Side::List;
                return ReviewAction::None;
            }
            KeyCode::Char('/') => {
                self.side = Side::List;
                self.filtering = true;
                self.filter.get_or_insert_with(Editor::default);
                return ReviewAction::None;
            }
            _ => {}
        }
        if self.targets.is_empty() {
            return ReviewAction::None;
        }
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => self.select(self.at + 1),
            KeyCode::Char('k') | KeyCode::Up => self.select(self.at.saturating_sub(1)),
            KeyCode::PageDown | KeyCode::Char(' ') => {
                self.select(self.at + self.height.max(1).saturating_sub(2).max(1))
            }
            KeyCode::PageUp => self.select(
                self.at
                    .saturating_sub(self.height.max(1).saturating_sub(2).max(1)),
            ),
            KeyCode::Char('g') | KeyCode::Home => self.select(0),
            KeyCode::Char('G') | KeyCode::End => self.select(usize::MAX),
            KeyCode::Char('J') => self.jump(true, starts_hunk),
            KeyCode::Char('K') => self.jump(false, starts_hunk),
            KeyCode::Char(']') => self.jump(true, |t| matches!(t, Target::File(_))),
            KeyCode::Char('[') => self.jump(false, |t| matches!(t, Target::File(_))),
            KeyCode::Char('c') => self.compose(None),
            KeyCode::Enter if !shift => {
                let newest = self.comments_at(self.target()).last().copied();
                self.compose(newest);
            }
            KeyCode::Char('d') => {
                if let Some(newest) = self.comments_at(self.target()).last().copied() {
                    self.comments.remove(newest);
                    self.refresh();
                    return ReviewAction::Comments;
                }
            }
            _ => {}
        }
        ReviewAction::None
    }

    /// The list has the keys: j/k move between files, taking the stream
    /// with them; Enter or Tab hands the keys to the stream; `/` filters;
    /// Esc backs out one level.
    fn list_key(&mut self, key: KeyEvent) -> ReviewAction {
        match key.code {
            KeyCode::Char('q') => return ReviewAction::Close,
            KeyCode::Esc if self.filter.is_some() => {
                self.filter = None;
                self.reorder();
            }
            KeyCode::Esc | KeyCode::Tab | KeyCode::Enter | KeyCode::Char('l') => {
                self.side = Side::Stream
            }
            KeyCode::Char('/') => {
                self.filtering = true;
                self.filter.get_or_insert_with(Editor::default);
            }
            KeyCode::Char('j') | KeyCode::Down => self.step_file(1),
            KeyCode::Char('k') | KeyCode::Up => self.step_file(-1),
            KeyCode::Char('g') | KeyCode::Home => self.step_file(isize::MIN / 2),
            KeyCode::Char('G') | KeyCode::End => self.step_file(isize::MAX / 2),
            _ => {}
        }
        ReviewAction::None
    }

    /// Typing the list's filter: the stream narrows to the files it
    /// matches. Enter keeps it; Esc clears it.
    fn filter_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.filtering = false;
                self.filter = None;
            }
            KeyCode::Enter => {
                self.filtering = false;
                if self.filter.as_ref().is_some_and(Editor::is_empty) {
                    self.filter = None;
                }
                return;
            }
            KeyCode::Down => return self.step_file(1),
            KeyCode::Up => return self.step_file(-1),
            _ => {
                if let Some(filter) = &mut self.filter {
                    filter.key(key);
                }
            }
        }
        self.reorder();
    }

    /// The stream to the file `by` places on in the list from the current
    /// one, at its header.
    fn step_file(&mut self, by: isize) {
        if self.order.is_empty() {
            return;
        }
        let here = (!self.targets.is_empty())
            .then(|| file_of(self.target()))
            .and_then(|file| self.order.iter().position(|f| *f == file))
            .unwrap_or(0);
        let next = here.saturating_add_signed(by).min(self.order.len() - 1);
        let file = self.order[next];
        if let Some(at) = self
            .targets
            .iter()
            .position(|target| *target == Target::File(file))
        {
            self.at = at;
        }
    }

    /// A click on the page: a file in the list takes the stream to it.
    pub fn click(&mut self, x: u16, y: u16) {
        let file = self
            .list_spots
            .iter()
            .find(|(row, (from, to), _)| *row == y && (*from..*to).contains(&x))
            .map(|(_, _, file)| *file);
        if let Some(file) = file
            && let Some(at) = self
                .targets
                .iter()
                .position(|target| *target == Target::File(file))
        {
            self.at = at;
            self.side = Side::List;
        }
    }

    /// Selects the header of the file at `path`, when the diff has it.
    pub fn show_file(&mut self, path: &str) {
        let found = self.targets.iter().position(|target| {
            matches!(target, Target::File(file) if self.doc.files.get(*file).is_some_and(|f| f.path == path))
        });
        if let Some(at) = found {
            self.select(at);
        }
    }

    pub fn scroll_by(&mut self, delta: isize) {
        self.select(self.at.saturating_add_signed(delta));
    }

    fn composing_key(&mut self, key: KeyEvent) -> ReviewAction {
        let Some(composing) = &mut self.composing else {
            return ReviewAction::None;
        };
        match key.code {
            KeyCode::Esc => {
                self.composing = None;
                ReviewAction::None
            }
            KeyCode::Enter if !key.modifiers.contains(KeyModifiers::SHIFT) => {
                let words = composing.editor.text().trim().to_owned();
                let target = composing.target;
                let editing = composing.editing;
                self.composing = None;
                match (editing, words.is_empty()) {
                    (None, true) => return ReviewAction::None,
                    (Some(index), true) => {
                        self.comments.remove(index);
                    }
                    (Some(index), false) => self.comments[index].text = words,
                    (None, false) => {
                        let comment = self.comment_for(target, words);
                        self.comments.push(comment);
                    }
                }
                self.refresh();
                ReviewAction::Comments
            }
            _ => {
                composing.editor.key(key);
                ReviewAction::None
            }
        }
    }

    fn target(&self) -> Target {
        self.targets[self.at.min(self.targets.len().saturating_sub(1))]
    }

    fn select(&mut self, at: usize) {
        self.at = at.min(self.targets.len().saturating_sub(1));
    }

    /// Moves to the next or previous target where a group starts: a file
    /// header, or a hunk's first line.
    fn jump(&mut self, forward: bool, starts: impl Fn(Target) -> bool) {
        let found = if forward {
            (self.at + 1..self.targets.len()).find(|&i| starts(self.targets[i]))
        } else {
            (0..self.at).rev().find(|&i| starts(self.targets[i]))
        };
        if let Some(at) = found {
            self.at = at;
        }
    }

    fn compose(&mut self, editing: Option<usize>) {
        let mut editor = Editor::default();
        if let Some(index) = editing {
            editor.set(&self.comments[index].text, Vec::new());
        }
        self.composing = Some(Composing {
            target: self.target(),
            editing,
            editor,
        });
    }

    fn comment_for(&self, target: Target, text: String) -> ReviewComment {
        let (file, line) = match target {
            Target::File(file) => (file, None),
            Target::Line { file, .. } => (file, line_of(&self.doc, target)),
        };
        let path = self.doc.files[file].path.clone();
        let (line, old_line) = match line {
            Some(line) => match (line.new_line, line.old_line) {
                (Some(new), _) => (new, 0),
                (None, Some(old)) => (0, old),
                (None, None) => (0, 0),
            },
            None => (0, 0),
        };
        ReviewComment {
            path,
            line,
            old_line,
            text,
        }
    }

    /// Indices of the comments on `target`, oldest first.
    fn comments_at(&self, target: Target) -> Vec<usize> {
        let probe = self.comment_for(target, String::new());
        self.comments
            .iter()
            .enumerate()
            .filter(|(_, c)| {
                c.path == probe.path && c.line == probe.line && c.old_line == probe.old_line
            })
            .map(|(i, _)| i)
            .collect()
    }

    /// Whose changes these are: the agent's name and its place, for the
    /// header.
    pub fn set_owner(&mut self, name: &str, place: &str) {
        self.owner = (name.to_owned(), place.to_owned());
    }

    fn refresh(&mut self) {
        self.doc = review_doc(&self.diff, &self.patch, &self.comments);
        self.reorder();
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
        // The list a quarter of the page on its left, past a hairline; on a
        // narrow page only while it has the keys, the stream taking it all
        // otherwise.
        let list_width = (width / 4).clamp(LIST_MIN, LIST_MAX).min(width / 2);
        let list_shown = width >= LIST_ALWAYS || self.side == Side::List;
        let stream_width = if list_shown {
            width.saturating_sub(list_width + 1)
        } else {
            width
        };
        let (body, selected, cursor) = self.body(stream_width, theme);
        // A blank above the header keeps it off the terminal's edge, as in
        // the chat; a blank under it.
        let top = vec![Line::default(), self.title(width, theme), Line::default()];
        let hint = footer.unwrap_or_else(|| self.hint_line(width, theme));
        let room = height.saturating_sub(1);
        let body_height = room.saturating_sub(top.len());
        self.height = body_height;
        let (first, last) = selected;
        if first < self.scroll {
            self.scroll = first;
        } else if last + 1 > self.scroll + body_height {
            self.scroll = (last + 1).saturating_sub(body_height);
        }
        self.scroll = self.scroll.min(body.len().saturating_sub(body_height));
        let stream: Vec<Line<'static>> = body
            .into_iter()
            .skip(self.scroll)
            .take(body_height)
            .collect();
        let body_top = top.len();
        let mut lines = top;
        self.list_spots.clear();
        let mut list_cursor = None;
        if list_shown {
            let (list, at) = self.list_column(list_width, body_height, theme);
            list_cursor = at;
            // The side with the keys has the bright frame: the rule
            // brightens beside the list's keys.
            let rule = if self.side == Side::List {
                theme.muted()
            } else {
                theme.hairline()
            };
            // As many rows as the taller side: a short list leaves its
            // column blank beside the rest of the stream.
            let rows = list.len().max(stream.len());
            let mut list = list.into_iter();
            for row in 0..rows {
                let mut line = list.next().unwrap_or_default();
                text::pad_to(&mut line, list_width);
                line.spans.push(Span::styled("│", rule));
                if let Some(right) = stream.get(row) {
                    line.spans.extend(right.spans.iter().cloned());
                }
                lines.push(line);
            }
            for (row, file) in self.list_rows.drain(..) {
                self.list_spots.push((
                    area.y + (body_top + row) as u16,
                    (area.x, area.x + list_width as u16),
                    file,
                ));
            }
        } else {
            lines.extend(stream);
        }
        while lines.len() < room {
            lines.push(Line::default());
        }
        lines.push(hint);
        paint.render_widget(Paragraph::new(lines), area);
        let stream_x = if list_shown { list_width + 1 } else { 0 };
        if let Some((col, row)) = list_cursor {
            paint.set_cursor_position(Position::new(
                area.x + col as u16,
                area.y + (body_top + row) as u16,
            ));
        } else if let Some((row, col)) = cursor
            && row >= self.scroll
            && row < self.scroll + body_height
        {
            let y = area.y as usize + body_top + row - self.scroll;
            let x = area.x as usize + stream_x + col.min(stream_width.saturating_sub(1));
            paint.set_cursor_position(Position::new(x as u16, y as u16));
        }
    }

    /// The list's column, `height` lines: its title (or the filter being
    /// typed), a blank, then the shared changes list scrolled to keep the
    /// stream's file in view on the flat highlight, its directory pinned
    /// when the group runs above the edge. Returns the lines and the
    /// filter's cursor as (column, line).
    fn list_column(
        &mut self,
        width: usize,
        height: usize,
        theme: Theme,
    ) -> (Vec<Line<'static>>, Option<(usize, usize)>) {
        let inner = width.saturating_sub(2 * LIST_INSET);
        let focused = self.side == Side::List;
        let mut title = Line::from(Span::raw(" ".repeat(LIST_INSET)));
        let mut cursor = None;
        match &self.filter {
            Some(filter) => {
                push(&mut title, "/ ", theme.attention(), width);
                let at = text::line_width(&title);
                push(&mut title, filter.text(), theme.text(), width);
                if self.filtering {
                    cursor = Some((at + filter.cursor_chars(), 0));
                }
            }
            None => {
                let ink = if focused {
                    theme.bright()
                } else {
                    theme.muted()
                };
                push(&mut title, "Files", ink, width);
                push(
                    &mut title,
                    format!(" {}", self.order.len()),
                    theme.faint(),
                    width,
                );
            }
        }
        let files = self.changes(self.order.iter().copied());
        let list = changes_lines(&files, inner, theme);
        let here = (!self.targets.is_empty()).then(|| file_of(self.target()));
        let here_line = list.files.iter().find_map(|(line, path)| {
            (here.map(|f| &self.doc.files[f].path) == Some(path)).then_some(*line)
        });
        let room = height.saturating_sub(2);
        if let Some(line) = here_line {
            if line < self.list_top + usize::from(self.list_top > 0) {
                self.list_top = line.saturating_sub(1);
            } else if line >= self.list_top + room {
                self.list_top = line + 1 - room;
            }
        }
        self.list_top = self.list_top.min(list.lines.len().saturating_sub(room));
        let mut shown: Vec<Line<'static>> = list
            .lines
            .iter()
            .skip(self.list_top)
            .take(room)
            .cloned()
            .collect();
        // A group scrolled above the edge keeps its directory in view.
        if let Some(dir) = list.pin(self.list_top)
            && let Some(first) = shown.first_mut()
        {
            *first = list.lines[dir].clone();
        }
        let mut rows = Vec::new();
        for (line, path) in &list.files {
            if (self.list_top..self.list_top + room).contains(line)
                && let Some(file) = self.doc.files.iter().position(|f| &f.path == path)
            {
                rows.push((2 + line - self.list_top, file));
            }
        }
        for line in &mut shown {
            line.spans.insert(0, Span::raw(" ".repeat(LIST_INSET)));
        }
        if let Some(line) = here_line
            && line >= self.list_top
            && line < self.list_top + room
        {
            let at = line - self.list_top;
            super::pane::card_highlight(&mut shown, at..at + 1, 0, width, false, theme);
        }
        self.list_rows = rows;
        let mut out = vec![title, Line::default()];
        out.extend(shown);
        (out, cursor)
    }

    /// The keys for the side that has them: the key reads first, its
    /// action recedes, as on every hint line.
    fn hint_line(&self, width: usize, theme: Theme) -> Line<'static> {
        let keys: &[(&str, &str)] = if self.composing.is_some() {
            &[("enter", "save"), ("ctrl+j", "newline"), ("esc", "cancel")]
        } else if self.filtering {
            &[("enter", "keep"), ("esc", "clear"), ("↑↓", "file")]
        } else if self.side == Side::List {
            &[
                ("j/k", "file"),
                ("enter", "read"),
                ("/", "filter"),
                ("tab", "stream"),
                ("q", "back"),
            ]
        } else {
            &[
                ("j/k", "move"),
                ("J/K", "hunk"),
                ("]/[", "file"),
                ("c", "comment"),
                ("enter", "edit"),
                ("d", "delete"),
                ("tab", "files"),
                ("q", "back"),
            ]
        };
        // The way out stays on the line, however narrow.
        let mut line = Line::from(Span::raw("  "));
        let fitted = text::fit_hints(keys, width.saturating_sub(4));
        for (i, &at) in fitted.shown.iter().enumerate() {
            let (key, action) = keys[at];
            if fitted.mark == Some(i) {
                push(&mut line, " …", theme.faint(), width);
            }
            if i > 0 {
                push(&mut line, " ".repeat(fitted.gap), theme.faint(), width);
            }
            push(&mut line, key, theme.emphasis(), width);
            push(&mut line, format!(" {action}"), theme.faint(), width);
        }
        if fitted.mark == Some(fitted.shown.len()) {
            push(&mut line, " …", theme.faint(), width);
        }
        line
    }

    /// Like the chat's header: whose changes at the left (`name │ path`),
    /// what they are against and their counts at the right ("review ·
    /// working tree at 3f2a1c9 │ 2 files · +12 −4 · 1 comment").
    fn title(&self, width: usize, theme: Theme) -> Line<'static> {
        let bar = || Span::styled(" │ ", theme.faint());
        let base = if self.doc.base.is_empty() {
            "working tree".to_owned()
        } else {
            format!("since {}", self.doc.base)
        };
        let head: String = self.doc.head.chars().take(7).collect();
        let mut about = format!("review · {base}");
        if !head.is_empty() {
            about.push_str(&format!(" at {head}"));
        }
        let files = self.doc.files.len();
        let mut counts = format!(
            "{files} file{} · +{} −{}",
            if files == 1 { "" } else { "s" },
            self.doc.added,
            self.doc.removed
        );
        match self.comments.len() {
            0 => {}
            1 => counts.push_str(" · 1 comment"),
            n => counts.push_str(&format!(" · {n} comments")),
        }
        let right = vec![
            Span::styled(about, theme.faint()),
            bar(),
            Span::styled(counts, theme.muted()),
        ];
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
        if text::line_width(&line) + 2 <= right_at {
            text::pad_to(&mut line, right_at);
            line.spans.extend(right);
        }
        line
    }

    /// Every file's section as lines, with the selected target's first and
    /// last line and the comment editor's cursor.
    #[allow(clippy::type_complexity)]
    fn body(
        &self,
        width: usize,
        theme: Theme,
    ) -> (Vec<Line<'static>>, (usize, usize), Option<(usize, usize)>) {
        let digits = self
            .doc
            .files
            .iter()
            .flat_map(|file| file.hunks.iter().flat_map(|hunk| &hunk.lines))
            .filter_map(|line| line.new_line.or(line.old_line))
            .max()
            .unwrap_or(0)
            .to_string()
            .len()
            .max(3);
        // "▌ " + number + " " + sign + " "
        let gutter = 2 + digits + 3;
        let selected_target = self.target();
        let mut lines: Vec<Line<'static>> = Vec::new();
        let mut selected = (0, 0);
        let mut cursor = None;
        for (t, target) in self.targets.iter().enumerate() {
            let is_selected = t == self.at.min(self.targets.len().saturating_sub(1));
            let bar = if is_selected {
                Span::styled("▌ ", theme.focus_bar())
            } else {
                Span::raw("  ")
            };
            let start = lines.len();
            let comments: &[String] = match *target {
                Target::File(f) => {
                    let file = &self.doc.files[f];
                    // Each file starts on a landmark like a plan's: its
                    // path bright, its counts, a hairline to the margin.
                    if !lines.is_empty() {
                        lines.push(Line::default());
                        lines.push(Line::default());
                    }
                    let mut line = Line::from(bar);
                    push(&mut line, file_name(file), theme.bright(), width);
                    if file.binary {
                        push(&mut line, "  binary", theme.faint(), width);
                    } else {
                        push(&mut line, "  ", theme.faint(), width);
                        if file.added > 0 {
                            push(&mut line, format!("+{}", file.added), theme.ok(), width);
                        }
                        if file.removed > 0 {
                            let gap = if file.added > 0 { " " } else { "" };
                            push(
                                &mut line,
                                format!("{gap}\u{2212}{}", file.removed),
                                theme.error(),
                                width,
                            );
                        }
                    }
                    push(&mut line, " ", theme.faint(), width);
                    let rule = width.saturating_sub(text::line_width(&line) + 2);
                    push(&mut line, "─".repeat(rule), theme.hairline(), width);
                    lines.push(line);
                    &file.comments
                }
                Target::Line { file, hunk, line } => {
                    let h = &self.doc.files[file].hunks[hunk];
                    // A hunk follows a blank. Where the patch leaves lines
                    // out before it, a break says so: "⋯" in the number
                    // column, git's name for where the hunk is, and a
                    // hairline to the margin, as the phone's band does.
                    if line == 0 {
                        lines.push(Line::default());
                        if h.skips {
                            let mut gap = Line::from(Span::raw("  "));
                            push(&mut gap, format!("{:>digits$}   ", "⋯"), theme.muted(), width);
                            if !h.scope.is_empty() {
                                let room = width.saturating_sub(text::line_width(&gap) + 4);
                                let scope = h.scope.replace('\t', "    ");
                                push(&mut gap, text::clip_to_width(&scope, room), theme.muted(), width);
                                push(&mut gap, " ", theme.faint(), width);
                            }
                            let rule = width.saturating_sub(text::line_width(&gap) + 2);
                            push(&mut gap, "─".repeat(rule), theme.hairline(), width);
                            lines.push(gap);
                        }
                    }
                    let diff_line = &h.lines[line];
                    let number = |n: Option<u32>| match n {
                        Some(n) => format!("{n:>digits$}"),
                        None => " ".repeat(digits),
                    };
                    let (sign, style) = match diff_line.kind {
                        LineKind::Added => ('+', theme.diff_added()),
                        LineKind::Removed => ('-', theme.diff_removed()),
                        LineKind::Context => (' ', theme.diff_context()),
                    };
                    // One column of numbers: a removed line by its old
                    // number, every other by its new one, as on the phone.
                    let mut row = Line::from(bar);
                    push(
                        &mut row,
                        format!("{} ", number(diff_line.new_line.or(diff_line.old_line))),
                        theme.muted(),
                        width,
                    );
                    let body = format!("{sign} {}", diff_line.text.replace('\t', "    "));
                    let room = width.saturating_sub(text::line_width(&row));
                    push(&mut row, text::clip_to_width(&body, room), style, width);
                    if diff_line.kind != LineKind::Context {
                        text::fill(&mut row, style, width);
                    }
                    lines.push(row);
                    &diff_line.comments
                }
            };
            let indent = match target {
                Target::File(_) => 2,
                Target::Line { .. } => gutter,
            };
            for comment in comments {
                for (i, part) in text::wrap(comment, width.saturating_sub(indent + 2).max(1))
                    .into_iter()
                    .enumerate()
                {
                    let mut line = Line::from(Span::raw(" ".repeat(indent)));
                    let lead = if i == 0 { "│ " } else { "  " };
                    push(&mut line, lead, theme.attention(), width);
                    push(&mut line, part, theme.text(), width);
                    lines.push(line);
                }
            }
            if let Some(composing) = self.composing.as_ref().filter(|c| c.target == *target) {
                let placeholder = match composing.editing {
                    Some(_) => "edit the comment; empty removes it",
                    None => "a comment for the agent on this line",
                };
                let room = width.saturating_sub(indent).max(4);
                let (editor, at) = editor_lines(&composing.editor, placeholder, room, theme);
                cursor = Some((lines.len() + at.0, indent + at.1));
                for line in editor {
                    let mut spans = vec![Span::raw(" ".repeat(indent))];
                    spans.extend(line.spans);
                    lines.push(Line::from(spans));
                }
            }
            if *target == selected_target && is_selected {
                selected = (start, lines.len().saturating_sub(1));
            }
        }
        if self.targets.is_empty() {
            let mut line = Line::from(Span::raw("  "));
            push(
                &mut line,
                "No changes in the working tree.",
                theme.muted(),
                width,
            );
            lines.push(line);
        }
        (lines, selected, cursor)
    }
}

/// Every place a comment can go, file by file in `order`.
fn targets(doc: &ReviewDoc, order: &[usize]) -> Vec<Target> {
    let mut out = Vec::new();
    for &f in order {
        let file = &doc.files[f];
        out.push(Target::File(f));
        for (h, hunk) in file.hunks.iter().enumerate() {
            for l in 0..hunk.lines.len() {
                out.push(Target::Line {
                    file: f,
                    hunk: h,
                    line: l,
                });
            }
        }
    }
    out
}

fn line_of(doc: &ReviewDoc, target: Target) -> Option<&ui_view::DiffLine> {
    match target {
        Target::Line { file, hunk, line } => doc.files.get(file)?.hunks.get(hunk)?.lines.get(line),
        Target::File(_) => None,
    }
}

fn file_of(target: Target) -> usize {
    match target {
        Target::File(file) | Target::Line { file, .. } => file,
    }
}

fn starts_hunk(target: Target) -> bool {
    matches!(target, Target::Line { line: 0, .. })
}

fn file_name(file: &ui_view::ReviewFile) -> String {
    match &file.old_path {
        Some(old) => format!("{old} → {}", file.path),
        None => file.path.clone(),
    }
}
