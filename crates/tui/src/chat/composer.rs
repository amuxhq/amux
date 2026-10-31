//! The bottom of the chat: the queued and unconfirmed prompts, and the
//! composer.

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
/// are done, how many tool servers need signing in or failed, while
/// `servers` says some are news, how many `jobs` run in the background),
/// set apart by space rather than dots, and at the right the usage limit,
/// only near it and only while `typing`, since that is when it bears on a
/// choice. None when there is nothing to say.
pub fn edge_row(
    overview: &Overview,
    servers: bool,
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
    if servers {
        let sign_in = overview
            .failed_servers
            .iter()
            .filter(|server| server.needs_auth)
            .count();
        let failed = overview.failed_servers.len() - sign_in;
        if sign_in > 0 {
            let words = match sign_in {
                1 => "1 tool server needs sign-in".to_owned(),
                n => format!("{n} tool servers need sign-in"),
            };
            groups.push(vec![(words, theme.warning())]);
        }
        if failed > 0 {
            let s = if failed == 1 { "" } else { "s" };
            let words = format!("{failed} tool server{s} failed to start");
            groups.push(vec![(words, theme.error())]);
        }
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
    /// This client's prompt that never left, the agent's host out of
    /// reach: the client holds it and sends it when `host` is back.
    Held {
        input_id: Vec<u8>,
        text: Vec<Segment>,
        host: String,
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
            QueueEntry::Held { .. } => &[("enter", "cancel"), QUEUED, BACK],
            QueueEntry::Queued(_) | QueueEntry::Sending { .. } => &[QUEUED, BACK],
        }
    }

    pub fn text(&self) -> &[Segment] {
        match self {
            QueueEntry::Queued(row) => &row.text,
            QueueEntry::Sending { text, .. }
            | QueueEntry::Unconfirmed { text, .. }
            | QueueEntry::Held { text, .. } => text,
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
            QueueEntry::Held { host, .. } => format!("sends when {host} is back"),
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
            QueueEntry::Held { .. } => vec!["[Cancel]"],
        }
    }
}

/// The draft position under a click in the composer: `row` and `col`
/// count from the first wrapped line and the first column after the
/// prompt mark, wrapped at `width` as [`editor_lines`] wraps, a grapheme
/// at a time at its drawn width, so a click never lands inside a
/// character. The answer is a count of characters, an attachment's
/// placeholder one.
pub fn cursor_at(editor: &Editor, width: usize, target_row: usize, target_col: usize) -> usize {
    let room = width.saturating_sub(2).max(1);
    let mut row = 0usize;
    let mut used = 0usize;
    let mut at = 0usize;
    for segment in segments(editor.text(), editor.attachments()) {
        match segment {
            Segment::Text(text) => {
                for grapheme in text.graphemes(true) {
                    let chars = grapheme.chars().count();
                    if grapheme == "\n" || grapheme == "\r\n" {
                        if row >= target_row {
                            return at;
                        }
                        row += 1;
                        used = 0;
                        at += chars;
                        continue;
                    }
                    let shown = if editor.secret { "•" } else { grapheme };
                    let w = text::str_width(shown);
                    if used + w > room {
                        if row >= target_row {
                            return at;
                        }
                        row += 1;
                        used = 0;
                    }
                    if row > target_row || (row == target_row && used + w > target_col) {
                        return at;
                    }
                    used += w;
                    at += chars;
                }
            }
            Segment::Attachment(view) => {
                let w = text::str_width(&text::ellipsize(&chip(&view), room));
                if used > 0 && used + w > room {
                    if row >= target_row {
                        return at;
                    }
                    row += 1;
                    used = 0;
                }
                if row > target_row || (row == target_row && used + w > target_col) {
                    return at;
                }
                used += w;
                at += 1;
            }
        }
    }
    at
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
    let cursor = editor.cursor_chars();
    let mut rows: Vec<Vec<Span<'static>>> = vec![Vec::new()];
    let mut used = 0usize;
    let mut at = 0usize;
    let mut position = (0usize, 0usize);
    let place =
        |rows: &Vec<Vec<Span<'static>>>, used: usize, at: usize, position: &mut (usize, usize)| {
            if at == cursor {
                *position = (rows.len() - 1, used);
            }
        };
    for segment in segments(editor.text(), editor.attachments()) {
        match segment {
            // A grapheme at a time, so a combining accent stays on its
            // letter and the cursor never stops inside a character.
            Segment::Text(text) => {
                for grapheme in text.graphemes(true) {
                    place(&rows, used, at, &mut position);
                    at += grapheme.chars().count();
                    if grapheme == "\n" || grapheme == "\r\n" {
                        rows.push(Vec::new());
                        used = 0;
                        continue;
                    }
                    let shown = if editor.secret { "•" } else { grapheme };
                    let w = text::str_width(shown);
                    if used + w > room {
                        rows.push(Vec::new());
                        used = 0;
                    }
                    if let Some(row) = rows.last_mut() {
                        row.push(Span::styled(shown.to_owned(), theme.text()));
                    }
                    used += w;
                }
            }
            Segment::Attachment(view) => {
                let chip = text::ellipsize(&chip(&view), room);
                let w = text::str_width(&chip);
                if used > 0 && used + w > room {
                    rows.push(Vec::new());
                    used = 0;
                }
                place(&rows, used, at, &mut position);
                at += 1;
                if let Some(row) = rows.last_mut() {
                    row.push(Span::styled(chip, theme.chip()));
                }
                used += w;
            }
        }
    }
    place(&rows, used, at, &mut position);
    let lines = rows
        .into_iter()
        .enumerate()
        .map(|(i, spans)| {
            let mut line = Line::from(Span::styled(
                if i == 0 { lead } else { "  " },
                theme.attention(),
            ));
            line.spans.extend(spans);
            line
        })
        .collect();
    (lines, (position.0, position.1 + 2))
}
