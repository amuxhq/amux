//! Chat rows as terminal lines. Each row is one line of glyph, verb, subject
//! and ` · ` meta; consecutive tool rows hang off one rail in the gutter,
//! and rows that open show their detail below when the reader expands
//! them. Wording lives here; the facts come from `ui_view::Row`.

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ui_view::{AttachmentView, CallPhase, ExploreVerb, LineKind, PatchHead, Row, RowKind, Segment};
use wire::{BoundaryKind, EnvelopeKind, SendState};

use crate::text::{self, first_line, push};
use crate::theme::Theme;

/// Where body text starts.
const INDENT: usize = 4;
/// Lines of an opened body before it is cut with a count.
pub const OPEN_LINES: usize = 40;

/// How the reader has this row, and where it sits among its neighbours.
#[derive(Clone, Copy, Debug, Default)]
pub struct RowState {
    pub focused: bool,
    /// The row is open, or its run is expanded.
    pub expanded: bool,
    /// A tool row beside another: it hangs off the rail in the gutter.
    pub rail: bool,
    /// The next row continues the rail, so no blank line separates them.
    pub joined: bool,
}

/// What a row shows beyond its own view value, looked up by the layout.
#[derive(Clone, Debug)]
pub struct RowFacts {
    /// A landed file change's patch head.
    pub patch: Option<PatchHead>,
    /// The leader key, for the keys a row names.
    pub leader: char,
}

impl Default for RowFacts {
    fn default() -> Self {
        RowFacts {
            patch: None,
            leader: 'a',
        }
    }
}

/// Whether a row is a tool row: one that joins its neighbours' rail.
pub fn on_rail(row: &Row) -> bool {
    matches!(
        row.kind,
        RowKind::Explore { .. }
            | RowKind::Command { .. }
            | RowKind::ToolCall { .. }
            | RowKind::FileChange { .. }
            | RowKind::Background { .. }
            | RowKind::Subagent { .. }
    )
}

/// The lines one row draws at `width`, its trailing blank separator
/// included unless the next row joins its rail. A row the view marks
/// collapsed draws nothing.
pub fn row_lines(row: &Row, state: RowState, width: usize, theme: Theme) -> Vec<Line<'static>> {
    let mut lines = body(row, state, width, theme);
    if lines.is_empty() {
        return lines;
    }
    if state.rail {
        for line in &mut lines {
            if let Some(first) = line.spans.first_mut()
                && first.content.starts_with(' ')
            {
                let rest = first.content[1..].to_owned();
                *first = Span::styled(rest, first.style);
                line.spans.insert(0, Span::styled("│", theme.muted()));
            }
        }
    }
    let bar = if state.focused {
        Some(Span::styled("▌", theme.focus_bar()))
    } else if row.attention {
        Some(Span::styled("▌", theme.attention()))
    } else {
        None
    };
    if let Some(bar) = bar {
        // The bar takes the row's first cell, so the row keeps its width.
        for line in &mut lines {
            if let Some(first) = line.spans.first_mut() {
                let mut chars = first.content.chars();
                if matches!(chars.next(), Some(' ' | '▎' | '│')) {
                    *first = Span::styled(chars.as_str().to_owned(), first.style);
                }
            }
            line.spans.insert(0, bar.clone());
        }
    }
    if !state.joined {
        lines.push(Line::default());
    }
    lines
}

/// A call's phase as a word after it, for a row whose verb does not say
/// it. With a decision on the row, the decision says what came of asking,
/// so the phase's word for the same thing is left to it.
pub(crate) fn state_meta(phase: CallPhase, decided: bool) -> Option<&'static str> {
    match phase {
        CallPhase::Asking | CallPhase::Pending if decided => None,
        CallPhase::Asking | CallPhase::Pending => Some("waiting"),
        CallPhase::Running => Some("running"),
        CallPhase::Succeeded => None,
        CallPhase::Failed => Some("failed"),
        CallPhase::Denied | CallPhase::Cancelled if decided => None,
        CallPhase::Denied => Some("denied"),
        CallPhase::Cancelled => Some("cancelled"),
    }
}

/// A call's verb says what happened to it: asking the person, under way,
/// refused, cancelled or done. The meta then never repeats it.
pub(crate) fn call_verb(phase: CallPhase, [wants, doing, done]: [&'static str; 3]) -> &'static str {
    match phase {
        CallPhase::Denied => "Denied",
        CallPhase::Asking => wants,
        CallPhase::Pending | CallPhase::Running => doing,
        CallPhase::Cancelled => "Cancelled",
        CallPhase::Succeeded | CallPhase::Failed => done,
    }
}

/// "  ✔ Ran cargo test · exit 101 · 4.2s": the subject gives way first, so
/// the meta always shows.
fn head(
    glyph: (&str, Style),
    verb: &str,
    subject: &str,
    meta: &str,
    width: usize,
    theme: Theme,
) -> Line<'static> {
    let mut line = Line::from(Span::raw("  "));
    push(&mut line, glyph.0, glyph.1, width);
    push(&mut line, " ", theme.text(), width);
    let meta = if meta.is_empty() {
        String::new()
    } else if subject.is_empty() && verb.is_empty() {
        meta.to_owned()
    } else {
        format!(" · {meta}")
    };
    let room = width.saturating_sub(text::str_width(&meta));
    if !verb.is_empty() {
        push(&mut line, verb, theme.emphasis(), room);
        if !subject.is_empty() {
            push(&mut line, " ", theme.text(), room);
        }
    }
    push(&mut line, subject, theme.code(), room);
    push(&mut line, meta, theme.muted(), width);
    line
}

/// Indented body lines, cut with a count past `limit`.
pub(crate) fn detail(
    text: &str,
    width: usize,
    style: Style,
    limit: usize,
    theme: Theme,
) -> Vec<Line<'static>> {
    let wrapped = text::wrap(text, width.saturating_sub(INDENT));
    let total = wrapped.len();
    let mut lines: Vec<Line<'static>> = wrapped
        .into_iter()
        .take(limit)
        .map(|line| {
            Line::from(vec![
                Span::raw(" ".repeat(INDENT)),
                Span::styled(line, style),
            ])
        })
        .collect();
    if total > limit {
        lines.push(Line::from(vec![
            Span::raw(" ".repeat(INDENT)),
            Span::styled(format!("··· {} more lines", total - limit), theme.muted()),
        ]));
    }
    lines
}

/// A link on a drawn line: its columns and where it goes.
pub(crate) type Link = (usize, usize, String);

/// Markdown drawn at the text's indent, each line with the links on it by
/// column, for a feed that opens them on a click.
pub(crate) fn markdown_linked(
    source: &str,
    width: usize,
    wide: usize,
    theme: Theme,
) -> Vec<(Line<'static>, Vec<Link>)> {
    crate::markdown::markdown_lines_wide(
        source,
        width.saturating_sub(INDENT).max(1),
        wide.saturating_sub(INDENT).max(1),
        theme,
    )
    .iter()
    .map(|line| {
        let mut spans = vec![Span::raw(" ".repeat(INDENT))];
        spans.extend(line.spans.iter().cloned());
        let links = line
            .links
            .iter()
            .map(|(from, to, url)| (INDENT + from, INDENT + to, url.clone()))
            .collect();
        (Line::from(spans), links)
    })
    .collect()
}

/// An attachment's chip, from its reference alone.
pub fn chip(view: &AttachmentView) -> String {
    match view {
        // The name says what it is.
        AttachmentView::Image(blob) | AttachmentView::File(blob) => {
            format!("[{} · {}]", blob.name, text::bytes(blob.size))
        }
        AttachmentView::Text { name, lines, .. } => {
            format!("[{name} · {}]", super::feed::lines_words(*lines))
        }
        AttachmentView::Review { comments, .. } => match comments {
            1 => "[review · 1 comment]".into(),
            n => format!("[review · {n} comments]"),
        },
        AttachmentView::Empty => "[attachment]".into(),
    }
}

/// Text with chips at their places, wrapped at `width` in `style`.
pub fn segment_lines(
    segments: &[Segment],
    width: usize,
    style: Style,
    theme: Theme,
) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = vec![Line::default()];
    let width = width.max(1);
    for segment in segments {
        match segment {
            Segment::Text(text) => {
                for (i, part) in text.split('\n').enumerate() {
                    if i > 0 {
                        lines.push(Line::default());
                    }
                    // Words after a chip fill what is left of its line, then
                    // wrap at the message's whole width.
                    let used = lines.last().map_or(0, text::line_width);
                    let wrapped = text::wrap_after(part, width.saturating_sub(used), width);
                    for (j, piece) in wrapped.into_iter().enumerate() {
                        if j > 0 {
                            lines.push(Line::default());
                        }
                        if let Some(last) = lines.last_mut() {
                            last.spans.push(Span::styled(piece, style));
                        }
                    }
                }
            }
            Segment::Attachment(view) => {
                let chip = chip(view);
                let used = lines.last().map_or(0, text::line_width);
                if used > 0 && used + text::str_width(&chip) + 1 > width {
                    lines.push(Line::default());
                } else if used > 0
                    && let Some(last) = lines.last_mut()
                {
                    last.spans.push(Span::styled(" ", style));
                }
                if let Some(last) = lines.last_mut() {
                    last.spans.push(Span::styled(
                        text::ellipsize(&chip, width),
                        theme.chip_raised(),
                    ));
                }
            }
        }
    }
    lines
}

pub(crate) fn explore_verb(verb: ExploreVerb) -> &'static str {
    match verb {
        ExploreVerb::Read => "Read",
        ExploreVerb::Search => "Searched",
        ExploreVerb::List => "Listed",
        ExploreVerb::Fetch => "Fetched",
        ExploreVerb::WebSearch => "Searched the web",
    }
}

fn body(row: &Row, state: RowState, width: usize, theme: Theme) -> Vec<Line<'static>> {
    if row.collapsed {
        return Vec::new();
    }
    let open = state.expanded;
    match &row.kind {
        // The feed draws the person's and the agent's words, the steps,
        // the asks and the turns' ends.
        RowKind::Prompt { .. }
        | RowKind::Prose { .. }
        | RowKind::Thinking { .. }
        | RowKind::ToolCall { .. }
        | RowKind::FileChange { .. }
        | RowKind::Command { .. }
        | RowKind::Explore { .. }
        | RowKind::Subagent { .. }
        | RowKind::Background { .. }
        | RowKind::Image { .. }
        | RowKind::Ask(_)
        | RowKind::TurnEnd { .. }
        | RowKind::Stopped => Vec::new(),
        RowKind::SlashOutput {
            command,
            args,
            output,
        } => {
            let subject = if args.is_empty() {
                command.clone()
            } else {
                format!("{command} {args}")
            };
            let mut lines = vec![head(("/", theme.muted()), "", &subject, "", width, theme)];
            let limit = if open { OPEN_LINES } else { 6 };
            lines.extend(detail(output, width, theme.muted(), limit, theme));
            lines
        }
        RowKind::Compaction {
            tokens_before,
            tokens_after,
            automatic,
        } => {
            let mut words = String::from("Compacted");
            if let (Some(before), Some(after)) = (tokens_before, tokens_after) {
                words.push_str(&format!(
                    " · {} → {}",
                    text::tokens(*before),
                    text::tokens(*after)
                ));
            }
            if *automatic {
                words.push_str(" · automatic");
            }
            vec![rule(&words, width, theme)]
        }
        RowKind::Error {
            error_kind,
            message,
            attempts,
            gave_up,
        } => {
            let meta = if *gave_up && *attempts > 1 {
                format!("gave up after {attempts} tries")
            } else {
                String::new()
            };
            let subject = if error_kind.is_empty() {
                first_line(message).to_owned()
            } else {
                error_kind.clone()
            };
            let mut lines = vec![head(
                ("✗", theme.error()),
                "",
                &subject,
                &meta,
                width,
                theme,
            )];
            if !error_kind.is_empty() && !message.is_empty() {
                lines.extend(detail(
                    message,
                    width,
                    theme.muted(),
                    if open { OPEN_LINES } else { 3 },
                    theme,
                ));
            }
            lines
        }
        RowKind::ModelSwitch { from, to, reason } => {
            let _ = from;
            vec![head(
                ("⇄", theme.muted()),
                "Switched to",
                to,
                reason,
                width,
                theme,
            )]
        }
        RowKind::Boundary { kind, cause } => {
            let word = match kind {
                BoundaryKind::Started => "started",
                BoundaryKind::Cleared => "cleared",
                BoundaryKind::Compacted => "compacted",
                BoundaryKind::Resumed => "resumed",
                BoundaryKind::Forked => "forked",
                BoundaryKind::Restarted => "restarted",
                BoundaryKind::Exited => "exited",
                BoundaryKind::DaemonLost => "lost its daemon",
                BoundaryKind::Unspecified => "session",
            };
            let words = if cause.is_empty() {
                word.to_owned()
            } else {
                format!("{word} · {cause}")
            };
            vec![rule(&words, width, theme)]
        }
        RowKind::AgentMessage {
            from,
            kind,
            text: body,
            to,
            sent,
            rejection,
        } => {
            let (verb, who) = if to.is_empty() {
                ("From", from.as_str())
            } else {
                ("To", to.as_str())
            };
            let meta = match (kind, sent) {
                (EnvelopeKind::Finished, _) => "finished".to_owned(),
                (EnvelopeKind::Failed, _) => "failed".to_owned(),
                (_, SendState::Sending) => "sending".to_owned(),
                (_, SendState::Rejected) => format!("not delivered · {rejection}"),
                _ => String::new(),
            };
            let mut lines = vec![head(("✉", theme.muted()), verb, who, &meta, width, theme)];
            let limit = if open { OPEN_LINES } else { 1 };
            lines.extend(detail(body.trim(), width, theme.text(), limit, theme));
            lines
        }
        RowKind::AutoReview {
            status,
            status_word,
            risk,
            rationale,
            ..
        } => {
            let meta = if risk.is_empty() {
                String::new()
            } else {
                format!("{risk} risk")
            };
            // Codex's own word for a status this build does not know.
            let (verb, word) = match status {
                wire::ReviewStatus::Approved => ("Auto-approved", ""),
                wire::ReviewStatus::Denied => ("Auto-denied", ""),
                wire::ReviewStatus::InProgress => ("Auto-reviewing", ""),
                wire::ReviewStatus::Timeout => ("Auto-review timed out", ""),
                wire::ReviewStatus::Abort => ("Auto-review stopped", ""),
                wire::ReviewStatus::Unspecified => ("Auto-reviewed", status_word.as_str()),
            };
            let mut lines = vec![head(("⚖", theme.muted()), verb, word, &meta, width, theme)];
            if open {
                lines.extend(detail(rationale, width, theme.muted(), OPEN_LINES, theme));
            }
            lines
        }
        RowKind::Unrecognized { what, summary } => {
            vec![head(
                ("?", theme.muted()),
                "Unreadable",
                what,
                first_line(summary),
                width,
                theme,
            )]
        }
        RowKind::Hidden => Vec::new(),
    }
}

/// A landed edit's patch under its row: numbered lines in the diff
/// colours, then how much more there is and the key that opens it.
pub(crate) fn patch_lines(
    patch: &PatchHead,
    open: bool,
    leader: char,
    width: usize,
    theme: Theme,
) -> Vec<Line<'static>> {
    let digits = patch
        .lines
        .iter()
        .filter_map(|line| line.number)
        .max()
        .map_or(0, |n| n.to_string().len());
    let mut lines = Vec::new();
    for line in &patch.lines {
        let (sign, style) = match line.kind {
            LineKind::Added => ("+", theme.diff_added()),
            LineKind::Removed => ("-", theme.diff_removed()),
            LineKind::Context => (" ", theme.diff_context()),
        };
        let number = line
            .number
            .map(|n| format!("{n:>digits$}"))
            .unwrap_or_else(|| " ".repeat(digits));
        let mut out = Line::from(Span::raw(" ".repeat(INDENT)));
        if digits > 0 {
            push(&mut out, format!("{number} │ "), theme.muted(), width);
        }
        push(&mut out, format!("{sign}{}", line.text), style, width);
        lines.push(out);
    }
    if patch.more > 0 {
        let mut out = Line::from(Span::raw(" ".repeat(INDENT)));
        let words = if open {
            format!("··· {} more lines", patch.more)
        } else {
            format!("··· {} more lines · C-{leader} o open", patch.more)
        };
        push(&mut out, words, theme.muted(), width);
        lines.push(out);
    }
    lines
}

/// The end of `text` in at most `max` characters, "…" marking a cut.
pub(crate) fn tail(text: &str, max: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max {
        return text.to_owned();
    }
    let kept: String = chars[chars.len() - max.saturating_sub(1)..]
        .iter()
        .collect();
    format!("…{kept}")
}

/// A session boundary, drawn like home's section headings: the words in
/// the faint ink, then a hairline to the right margin.
pub(crate) fn rule(words: &str, width: usize, theme: Theme) -> Line<'static> {
    let mut line = Line::from(Span::raw("  "));
    push(&mut line, format!("{words} "), theme.faint(), width);
    let end = width.saturating_sub(2);
    let used = text::line_width(&line);
    if used < end {
        line.spans
            .push(Span::styled("─".repeat(end - used), theme.hairline()));
    }
    line
}
