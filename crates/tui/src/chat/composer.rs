//! The bottom of the chat: the queued and unconfirmed prompts, and the
//! composer.

use pulldown_cmark::{Event, Options, Parser, Tag};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ui_view::{Overview, QueuedRow, Segment, segments};
use unicode_segmentation::UnicodeSegmentation as _;

use super::rows::chip;
use crate::editor::Editor;
use crate::text::{self, push};
use crate::theme::Theme;

/// Most lines the composer grows to before it scrolls.
pub const COMPOSER_LINES: usize = 6;

/// Columns between the groups of the row above the composer.
const GROUP_GAP: usize = 4;

/// The row that sits on the composer's box, starting on its border's
/// column so it reads as the composer's and not the feed's: what is still
/// in flight in this chat at the left (the task in progress and how many
/// are done, how many `jobs` run in the background), set apart by space
/// rather than dots, and at the right the usage limit,
/// only near it and only while `typing`, since that is when it bears on a
/// choice. None when there is nothing to say.
pub fn edge_row(
    overview: &Overview,
    jobs: usize,
    typing: bool,
    now_ms: i64,
    width: usize,
    theme: Theme,
) -> Option<Line<'static>> {
    const MARGIN: usize = 2;
    // The task's name is the one part that gives way: it shortens, then
    // drops, before anything else does.
    let mut name: Option<String> = None;
    let mut groups: Vec<Vec<(String, ratatui::style::Style)>> = Vec::new();
    if let Some(tasks) = overview.tasks.as_ref().filter(|t| t.done < t.total) {
        let count = format!("{} of {} tasks", tasks.done, tasks.total);
        if !tasks.current.is_empty() {
            name = Some(tasks.current.clone());
        }
        groups.push(vec![(count, theme.faint())]);
    }
    if jobs > 0 {
        let s = if jobs == 1 { "" } else { "s" };
        groups.push(vec![(format!("{jobs} background job{s}"), theme.faint())]);
    }
    // The key that unfolds the row into the pane, named last.
    if !groups.is_empty() {
        groups.push(vec![("ctrl+o".to_owned(), theme.faint())]);
    }
    let usage = overview
        .usage_near_limit
        .as_ref()
        .filter(|usage| typing && !usage.blocked)
        .and_then(|usage| fullest(&usage.windows))
        .map(|(window, used)| usage_words(window, used, now_ms, theme));
    if groups.is_empty() && usage.is_none() {
        return None;
    }
    let end = width.saturating_sub(MARGIN);
    let right_width = usage
        .as_ref()
        .map(|spans| spans.iter().map(|(w, _)| text::str_width(w)).sum::<usize>())
        .unwrap_or(0);
    let room = if right_width > 0 {
        end.saturating_sub(right_width + GROUP_GAP)
    } else {
        end
    };
    let fixed: usize = MARGIN
        + groups
            .iter()
            .flatten()
            .map(|(words, _)| text::str_width(words))
            .sum::<usize>()
        + GROUP_GAP * groups.len().saturating_sub(1);
    const NAME_MIN: usize = 12;
    if let Some(name) = name {
        let name_room = room.saturating_sub(fixed + 2);
        if name_room >= NAME_MIN {
            let first = &mut groups[0];
            first[0].0 = format!("  {}", first[0].0);
            first.insert(0, (text::ellipsize(&name, name_room), theme.muted()));
        }
    }
    let mut line = Line::from(Span::raw(" ".repeat(MARGIN)));
    for (i, group) in groups.into_iter().enumerate() {
        if i > 0 {
            push(&mut line, " ".repeat(GROUP_GAP), theme.faint(), room);
        }
        for (words, style) in group {
            push(&mut line, words, style, room);
        }
    }
    if let Some(spans) = usage
        && text::line_width(&line) + GROUP_GAP + right_width <= end
    {
        text::pad_to(&mut line, end - right_width);
        for (words, style) in spans {
            line.spans.push(Span::styled(words, style));
        }
    }
    Some(line)
}

/// The window nearest its limit, and its use, of those whose use is known.
fn fullest(windows: &[ui_view::UsageWindowView]) -> Option<(&ui_view::UsageWindowView, f64)> {
    windows
        .iter()
        .filter_map(|window| Some((window, window.used_percent?)))
        .max_by(|(_, a), (_, b)| a.total_cmp(b))
}

/// "5-hour limit 81% used · resets 22:56": one usage window in words.
fn usage_words(
    window: &ui_view::UsageWindowView,
    used: f64,
    now_ms: i64,
    theme: Theme,
) -> Vec<(String, ratatui::style::Style)> {
    let name = limit_name(&window.label);
    let mut spans = vec![
        (name, theme.muted()),
        (format!(" {used:.0}% used"), theme.text()),
    ];
    if let Some(at) = window.resets_at_ms.filter(|at| *at > now_ms) {
        spans.push((format!(" · resets {}", resets(at, now_ms)), theme.faint()));
    }
    spans
}

/// A usage window in words: the 5-hour limit, the weekly one or a model's
/// weekly one; a window known only by its length or the provider's own
/// name says that.
pub(crate) fn limit_name(label: &ui_view::UsageLabel) -> String {
    match label {
        ui_view::UsageLabel::FiveHour => "5-hour limit".to_owned(),
        ui_view::UsageLabel::Weekly { model: None } => "Weekly limit".to_owned(),
        ui_view::UsageLabel::Weekly { model: Some(model) } => format!("{model} weekly limit"),
        ui_view::UsageLabel::Minutes(minutes) => format!("{minutes}-minute limit"),
        ui_view::UsageLabel::Named(name) => format!("{name} limit"),
    }
}

/// "5-hour limit reached · resets 23:24": the window of a usage limit that
/// has been reached, for the composer's edge; the fullest one when the
/// provider does not say which. None when no limit is reached.
pub fn limit_reached(overview: &Overview, now_ms: i64) -> Option<String> {
    let usage = overview
        .usage_near_limit
        .as_ref()
        .filter(|usage| usage.blocked)?;
    let Some(window) = usage
        .windows
        .iter()
        .find(|window| window.state == wire::UsageState::Blocked)
        .or_else(|| fullest(&usage.windows).map(|(window, _)| window))
    else {
        return Some("Usage limit reached".to_owned());
    };
    let mut words = format!("{} reached", limit_name(&window.label));
    if let Some(at) = window.resets_at_ms.filter(|at| *at > now_ms) {
        words.push_str(&format!(" · resets {}", resets(at, now_ms)));
    }
    Some(words)
}

/// When a limit resets: the time today, else the weekday.
pub(crate) fn resets(at_ms: i64, now_ms: i64) -> String {
    let Some(at) = super::feed::local_time(at_ms) else {
        return String::new();
    };
    if at_ms - now_ms < 20 * 3_600_000 {
        at.format("%H:%M").to_string()
    } else {
        at.format("%a").to_string()
    }
}

/// One entry of the queue block above the composer.
#[derive(Clone, Debug, PartialEq)]
pub enum QueueEntry {
    /// A prompt in the agent's queue.
    Queued(QueuedRow),
    /// This client's prompt on its way into the queue, drawn at once.
    /// `waiting` names the host while the link to it is down.
    Sending {
        input_id: Vec<u8>,
        text: Vec<Segment>,
        waiting: Option<String>,
    },
    /// This client's prompt whose connection dropped before a reply and
    /// that catching up found neither queued nor in the transcript: it may
    /// not have arrived, and only the person can say whether to send it
    /// again.
    Unconfirmed {
        input_id: Vec<u8>,
        text: Vec<Segment>,
    },
}

impl QueueEntry {
    pub fn hint(&self) -> &'static [(&'static str, &'static str)] {
        const QUEUED: (&str, &str) = ("↑/↓", "queued");
        const BACK: (&str, &str) = ("esc", "back");
        match self {
            QueueEntry::Queued(row) if row.can_send_now && row.can_withdraw => &[
                ("enter", "send now"),
                ("backspace", "withdraw"),
                QUEUED,
                BACK,
            ],
            QueueEntry::Queued(row) if row.can_withdraw => {
                &[("backspace", "withdraw"), QUEUED, BACK]
            }
            QueueEntry::Unconfirmed { .. } => {
                &[("enter", "resend"), ("backspace", "discard"), QUEUED, BACK]
            }
            QueueEntry::Queued(_) | QueueEntry::Sending { .. } => &[QUEUED, BACK],
        }
    }

    pub fn text(&self) -> &[Segment] {
        match self {
            QueueEntry::Queued(row) => &row.text,
            QueueEntry::Sending { text, .. } | QueueEntry::Unconfirmed { text, .. } => text,
        }
    }

    /// What it says at its right: how it waits.
    pub fn state_words(&self) -> String {
        match self {
            QueueEntry::Queued(row) if row.steered => "sending into this turn".to_owned(),
            QueueEntry::Queued(row) => match &row.from_agent {
                Some(agent) => format!("queued from {agent}"),
                None => "queued".to_owned(),
            },
            QueueEntry::Sending {
                waiting: Some(host),
                ..
            } => format!("waiting for {host}…"),
            QueueEntry::Sending { .. } => "queued".to_owned(),
            QueueEntry::Unconfirmed { .. } => "may not have arrived".to_owned(),
        }
    }

    /// Its controls, in order: what Enter does first, what Backspace does
    /// second.
    pub fn controls(&self) -> Vec<&'static str> {
        match self {
            QueueEntry::Queued(row) => {
                let mut out = Vec::new();
                if row.can_send_now {
                    out.push("[Send Now]");
                }
                if row.can_withdraw {
                    out.push("[Withdraw]");
                }
                out
            }
            QueueEntry::Sending { .. } => Vec::new(),
            QueueEntry::Unconfirmed { .. } => vec!["[Resend]", "[Discard]"],
        }
    }
}

/// A quote drawn on more rows than this folds once the cursor leaves it.
const QUOTE_FOLDS_OVER: usize = 4;
/// The rows of a folded quote still drawn.
const QUOTE_FOLDED_ROWS: usize = 2;
/// The rail a quoted row starts with, in the columns of its `> `.
const RAIL: &str = "│";

/// How one drawn piece of the draft reads.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Ink {
    bold: bool,
    italic: bool,
    code: bool,
    /// The `*` or `` ` `` that asks for the style: kept, and receding.
    mark: bool,
    quote: bool,
    rail: bool,
    chip: bool,
    fold: bool,
}

impl Ink {
    fn style(self, theme: Theme) -> Style {
        if self.chip {
            return theme.chip();
        }
        if self.rail || self.fold || self.mark {
            return theme.faint();
        }
        let mut style = if self.code {
            theme.code()
        } else if self.quote {
            theme.muted()
        } else {
            theme.text()
        };
        if self.bold {
            style = style.add_modifier(Modifier::BOLD);
        }
        if self.italic {
            style = style.add_modifier(Modifier::ITALIC);
        }
        style
    }
}

/// A piece of a drawn row. `at` counts as the cursor does; `chars` is what
/// it covers of the draft, none for what only the drawing adds.
#[derive(Clone, Debug)]
struct Cell {
    shown: String,
    width: usize,
    at: usize,
    chars: usize,
    ink: Ink,
}

impl Cell {
    fn rail(at: usize) -> Cell {
        Cell {
            shown: format!("{RAIL} "),
            width: 2,
            at,
            chars: 0,
            ink: Ink {
                rail: true,
                ..Ink::default()
            },
        }
    }
}

/// The ink of every character of the draft. Character by character, so no
/// mark is hidden and the cursor and wrapping never depend on the styling;
/// the parser is the chat's, so `2 * 3 * 4` stays plain, and an unclosed
/// `**` until it closes.
fn inks(text: &str) -> Vec<Ink> {
    let mut chars_at = vec![0usize; text.len() + 1];
    let mut count = 0;
    for (byte, c) in text.char_indices() {
        chars_at[byte..byte + c.len_utf8()].fill(count);
        count += 1;
    }
    chars_at[text.len()] = count;
    let mut out = vec![Ink::default(); count];

    let mut start = 0;
    for line in text.split('\n') {
        let chars = line.chars().count();
        if line.starts_with('>') {
            for ink in &mut out[start..start + chars] {
                ink.quote = true;
            }
            out[start].rail = true;
        }
        start += chars + 1;
    }

    let mut styled = |range: std::ops::Range<usize>, marks: usize, set: fn(&mut Ink)| {
        let (from, to) = (chars_at[range.start], chars_at[range.end]);
        if to < from + 2 * marks {
            return;
        }
        for ink in &mut out[from..from + marks] {
            ink.mark = true;
        }
        for ink in &mut out[to - marks..to] {
            ink.mark = true;
        }
        for ink in &mut out[from + marks..to - marks] {
            set(ink);
        }
    };
    for (event, range) in Parser::new_ext(text, Options::empty()).into_offset_iter() {
        match event {
            Event::Start(Tag::Strong) => styled(range, 2, |ink| ink.bold = true),
            Event::Start(Tag::Emphasis) => styled(range, 1, |ink| ink.italic = true),
            Event::Code(_) => {
                let ticks = text[range.clone()]
                    .chars()
                    .take_while(|c| *c == '`')
                    .count();
                styled(range, ticks, |ink| ink.code = true);
            }
            _ => {}
        }
    }
    out
}

/// The draft's quotes, each a run of lines starting `>`, as the characters
/// from the first's start to the last's end.
fn quote_blocks(text: &str) -> Vec<(usize, usize)> {
    let mut blocks: Vec<(usize, usize)> = Vec::new();
    let mut start = 0;
    let mut open = false;
    for line in text.split('\n') {
        let chars = line.chars().count();
        if line.starts_with('>') {
            match blocks.last_mut() {
                Some(block) if open => block.1 = start + chars,
                _ => blocks.push((start, start + chars)),
            }
            open = true;
        } else {
            open = false;
        }
        start += chars + 1;
    }
    blocks
}

/// The draft as rows at `room`. A line too long for its row breaks after
/// its last space, so a word moves down whole and only a word wider than
/// the row is cut. A secret is cut at the edge and styled not at all: its
/// spaces and marks would show. A long quote the cursor is outside folds.
fn draft_rows(editor: &Editor, room: usize) -> Vec<Vec<Cell>> {
    let inks = if editor.secret {
        Vec::new()
    } else {
        inks(editor.text())
    };
    let ink_at = |at: usize| inks.get(at).copied().unwrap_or_default();
    let mut rows: Vec<Vec<Cell>> = vec![Vec::new()];
    let mut used = 0usize;
    let mut at = 0usize;
    // Just after this row's last space: its cells and width to there.
    let mut after_space: Option<(usize, usize)> = None;
    for segment in segments(editor.text(), editor.attachments()) {
        match segment {
            Segment::Text(text) => {
                for grapheme in text.graphemes(true) {
                    let chars = grapheme.chars().count();
                    if grapheme == "\n" || grapheme == "\r\n" {
                        rows.last_mut().expect("a row").push(Cell {
                            shown: String::new(),
                            width: 0,
                            at,
                            chars,
                            ink: Ink::default(),
                        });
                        rows.push(Vec::new());
                        used = 0;
                        after_space = None;
                        at += chars;
                        continue;
                    }
                    let ink = ink_at(at);
                    let shown = if editor.secret {
                        "•"
                    } else if ink.rail {
                        RAIL
                    } else {
                        grapheme
                    };
                    let width = text::str_width(shown);
                    if used + width > room {
                        // A quoted line's later rows carry its rail too.
                        let rail = ink.quote && room > 4;
                        let indent = if rail { 2 } else { 0 };
                        let mut row = if rail {
                            vec![Cell::rail(at)]
                        } else {
                            Vec::new()
                        };
                        match after_space.take() {
                            Some((cells, before)) if indent + used - before + width <= room => {
                                let current = rows.last_mut().expect("a row");
                                row.extend(current.split_off(cells));
                                used = indent + used - before;
                            }
                            _ => used = indent,
                        }
                        if let (true, Some(next)) = (rail, row.get(1).map(|cell| cell.at)) {
                            row[0].at = next;
                        }
                        rows.push(row);
                    }
                    let row = rows.last_mut().expect("a row");
                    row.push(Cell {
                        shown: shown.to_owned(),
                        width,
                        at,
                        chars,
                        ink,
                    });
                    used += width;
                    at += chars;
                    if !editor.secret && grapheme.trim().is_empty() {
                        after_space = Some((row.len(), used));
                    }
                }
            }
            Segment::Attachment(view) => {
                let shown = text::ellipsize(&chip(&view), room);
                let width = text::str_width(&shown);
                if used > 0 && used + width > room {
                    rows.push(Vec::new());
                    used = 0;
                    after_space = None;
                }
                rows.last_mut().expect("a row").push(Cell {
                    shown,
                    width,
                    at,
                    chars: 1,
                    ink: Ink {
                        chip: true,
                        ..Ink::default()
                    },
                });
                used += width;
                at += 1;
            }
        }
    }
    if !editor.secret {
        fold_quotes(&mut rows, editor);
    }
    rows
}

/// Folds each long quote the cursor is outside to its first rows and a
/// count of the rest. The count's position is the first row it hides, so a
/// click on it puts the cursor there and opens the quote.
fn fold_quotes(rows: &mut Vec<Vec<Cell>>, editor: &Editor) {
    let cursor = editor.cursor_chars();
    for (start, end) in quote_blocks(editor.text()).into_iter().rev() {
        if (start..=end).contains(&cursor) {
            continue;
        }
        let inside = |row: &Vec<Cell>| {
            row.iter()
                .find(|cell| cell.chars > 0)
                .is_some_and(|cell| (start..end).contains(&cell.at))
        };
        let Some(first) = rows.iter().position(inside) else {
            continue;
        };
        let count = rows[first..].iter().take_while(|row| inside(row)).count();
        if count <= QUOTE_FOLDS_OVER {
            continue;
        }
        let hidden = count - QUOTE_FOLDED_ROWS;
        let at = rows[first + QUOTE_FOLDED_ROWS]
            .iter()
            .find(|cell| cell.chars > 0)
            .map_or(start, |cell| cell.at);
        let words = format!("… {hidden} more lines");
        let fold = vec![
            Cell::rail(at),
            Cell {
                width: text::str_width(&words),
                shown: words,
                at,
                chars: 0,
                ink: Ink {
                    fold: true,
                    ..Ink::default()
                },
            },
        ];
        rows.splice(first + QUOTE_FOLDED_ROWS..first + count, [fold]);
    }
}

/// The characters one Backspace removes when the cursor is just below a
/// folded quote: the quote and the blank between. Like a chip, what is
/// drawn as one thing goes at once.
pub fn folded_quote_above(editor: &Editor, width: usize) -> Option<(usize, usize)> {
    let cursor = editor.cursor_chars();
    let chars: Vec<char> = editor.text().chars().collect();
    let (start, end) = quote_blocks(editor.text())
        .into_iter()
        .take_while(|(_, end)| *end < cursor)
        .last()?;
    let gap = &chars[end..cursor];
    if gap.is_empty() || gap.len() > 2 || gap.iter().any(|c| *c != '\n') {
        return None;
    }
    let room = width.saturating_sub(2).max(1);
    draft_rows(editor, room)
        .iter()
        .flatten()
        .any(|cell| cell.ink.fold && (start..end).contains(&cell.at))
        .then_some((start, cursor))
}

/// The draft position under a click in the composer: `row` and `col` count
/// from the first wrapped line and the first column after the prompt mark,
/// wrapped at `width` as [`editor_lines`] wraps, so a click never lands
/// inside a character. The answer is a count of characters, an attachment's
/// placeholder one.
pub fn cursor_at(editor: &Editor, width: usize, target_row: usize, target_col: usize) -> usize {
    let room = width.saturating_sub(2).max(1);
    let rows = draft_rows(editor, room);
    let end = editor.text().chars().count();
    let Some(row) = rows.get(target_row) else {
        return end;
    };
    let mut used = 0usize;
    for cell in row {
        if cell.ink.fold || cell.width == 0 {
            return cell.at;
        }
        if cell.chars > 0 && used + cell.width > target_col {
            return cell.at;
        }
        used += cell.width;
    }
    // Past the end of the words: the end of the draft, or of a wrapped row.
    if target_row + 1 == rows.len() {
        end
    } else {
        row.last().map_or(end, |cell| cell.at + cell.chars)
    }
}

/// The composer's lines at `width` and where its cursor is: text runs and
/// attachment chips at their places, a chip one cursor position.
pub fn editor_lines(
    editor: &Editor,
    placeholder: &str,
    width: usize,
    theme: Theme,
) -> (Vec<Line<'static>>, (usize, usize)) {
    let lead = "▎ ";
    let room = width.saturating_sub(2).max(1);
    if editor.is_empty() {
        let mut line = Line::from(Span::styled(lead, theme.attention()));
        push(&mut line, placeholder, theme.muted(), width);
        return (vec![line], (0, 2));
    }
    let rows = draft_rows(editor, room);
    let cursor = editor.cursor_chars();
    let mut position = None;
    let lines = rows
        .iter()
        .enumerate()
        .map(|(i, row)| {
            let mut line = Line::from(Span::styled(
                if i == 0 { lead } else { "  " },
                theme.attention(),
            ));
            let mut used = 0;
            for cell in row {
                if position.is_none() && cell.chars > 0 && cell.at == cursor {
                    position = Some((i, used));
                }
                if !cell.shown.is_empty() {
                    line.spans
                        .push(Span::styled(cell.shown.clone(), cell.ink.style(theme)));
                }
                used += cell.width;
            }
            line
        })
        .collect();
    let (row, col) = position.unwrap_or_else(|| {
        let last = rows.len() - 1;
        (last, rows[last].iter().map(|cell| cell.width).sum())
    });
    (lines, (row, col + 2))
}
