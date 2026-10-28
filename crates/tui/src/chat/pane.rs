//! The overview pane: the row above the composer, unfolded. The chat's
//! task list in full, each background job still running, the changed
//! files of the comparison the person chose, the tool servers that failed
//! and the usage limits when one is near, each section folding under its
//! heading. Drawn as a fixed title over a body that
//! scrolls; the chat places it beside itself, or over its feed on a narrow
//! terminal.

use std::collections::HashSet;

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ui_view::{JobRow, JobStop, Overview, TaskMark};
use wire::{BackgroundKind, UsageState};

use crate::text::{self, pad_to, push};
use crate::theme::Theme;

/// Columns between the pane's edge and its words: a highlighted job's tint
/// spans them, as home's cards span the margin.
const INSET: usize = 2;

/// The pane's sections, each under a heading that folds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Section {
    Tasks,
    Background,
    Changes,
    Servers,
    Usage,
}

impl Section {
    fn words(self) -> &'static str {
        match self {
            Section::Tasks => "Tasks",
            Section::Background => "Background",
            Section::Changes => "Changes",
            Section::Servers => "Tool servers",
            Section::Usage => "Usage",
        }
    }

    /// Its name in the client's saved layout.
    pub fn key(self) -> &'static str {
        match self {
            Section::Tasks => "tasks",
            Section::Background => "background",
            Section::Changes => "changes",
            Section::Servers => "servers",
            Section::Usage => "usage",
        }
    }

    pub fn from_key(key: &str) -> Option<Section> {
        [
            Section::Tasks,
            Section::Background,
            Section::Changes,
            Section::Servers,
            Section::Usage,
        ]
        .into_iter()
        .find(|section| section.key() == key)
    }
}

/// What a click on the pane reaches.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PaneHit {
    Close,
    Item(PaneItem),
    /// "↑ N more" or "↓ N more": a page up, or down.
    Page {
        up: bool,
    },
}

/// One of the pane's items the keys move across: the section headings,
/// the jobs and the changed files, as one list in the pane's order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PaneItem {
    /// A section's heading, which folds it.
    Heading(Section),
    /// A running background job, which opens the step that started it.
    Job(JobRow),
    /// A changed file, by its path.
    File(String),
}

impl PaneItem {
    /// Whether two items are the same one across frames: a job stays itself
    /// while its stop goes out and is answered.
    pub fn same(&self, other: &PaneItem) -> bool {
        match (self, other) {
            (PaneItem::Job(a), PaneItem::Job(b)) => {
                (&a.step, &a.command, a.started_at_ms) == (&b.step, &b.command, b.started_at_ms)
            }
            _ => self == other,
        }
    }
}

/// The key a stop names, for a job its provider can stop now.
pub fn stop_key(job: &JobRow) -> Option<&str> {
    match &job.stop {
        JobStop::Offered { key } | JobStop::Refused { key, .. } => Some(key),
        JobStop::Unavailable | JobStop::Stopping => None,
    }
}

/// What the pane shows, read once a frame.
pub struct Contents<'a> {
    pub overview: &'a Overview,
    pub folded: &'a HashSet<Section>,
}

impl Contents<'_> {
    /// The sections with something in them, in the pane's order.
    fn sections(&self) -> Vec<Section> {
        let mut sections = Vec::new();
        if self.overview.tasks.is_some() {
            sections.push(Section::Tasks);
        }
        if !self.overview.jobs.is_empty() {
            sections.push(Section::Background);
        }
        if self
            .overview
            .changes
            .as_ref()
            .is_some_and(|changes| changes.totals.files > 0)
        {
            sections.push(Section::Changes);
        }
        if !self.overview.failed_servers.is_empty() {
            sections.push(Section::Servers);
        }
        if self.overview.usage_near_limit.is_some() {
            sections.push(Section::Usage);
        }
        sections
    }

    /// The items the keys move across, in order: each heading, then the
    /// jobs or files under it unless it is folded.
    pub fn items(&self) -> Vec<PaneItem> {
        let mut items = Vec::new();
        for section in self.sections() {
            items.push(PaneItem::Heading(section));
            if self.folded.contains(&section) {
                continue;
            }
            match section {
                Section::Background => items.extend(
                    self.overview
                        .jobs
                        .iter()
                        .map(|job| PaneItem::Job(job.clone())),
                ),
                Section::Changes => items.extend(
                    self.overview
                        .changes
                        .iter()
                        .flat_map(super::changes::ordered)
                        .map(PaneItem::File),
                ),
                Section::Tasks | Section::Servers | Section::Usage => {}
            }
        }
        items
    }
}

/// The pane drawn: its fixed title line and its body, which scrolls.
#[derive(Default)]
pub struct PaneOut {
    pub title: Line<'static>,
    pub title_hits: Vec<((usize, usize), PaneHit)>,
    pub body: Vec<Line<'static>>,
    pub hits: Vec<(usize, (usize, usize), PaneHit)>,
    /// For each body line, the task, job or file it draws, numbered in
    /// order, to count what scrolling hides.
    pub entries: Vec<Option<usize>>,
    /// Each item's body line, for keeping the focused one in view.
    pub item_lines: Vec<(PaneItem, usize)>,
    /// The Changes groups' directory lines, and where the section ends: a
    /// scrolled group keeps its directory pinned.
    pub dir_lines: Vec<usize>,
    pub changes_end: usize,
}

impl PaneOut {
    /// The directory line to pin over body line `row`, the first under
    /// the top edge: the one naming the group `row` sits in, when it has
    /// scrolled out above. The next group's own line takes over as it
    /// reaches `row`.
    pub fn pin(&self, row: usize) -> Option<usize> {
        let dir = self.dir_lines.iter().copied().rfind(|line| *line <= row)?;
        (dir < row && row < self.changes_end).then_some(dir)
    }
}

/// The pane, `width` columns wide with its words `INSET` in from each side.
/// `lit` is the item drawn highlighted: a job or file as a card, a heading
/// by brightening. `focused` says the pane has the keys, which only its
/// title shows.
pub fn pane_lines(
    contents: &Contents<'_>,
    lit: Option<&PaneItem>,
    focused: bool,
    now_ms: i64,
    width: usize,
    theme: Theme,
) -> PaneOut {
    let inner = width.saturating_sub(2 * INSET);
    let mut out = PaneOut::default();
    let title_ink = if focused {
        theme.bright()
    } else {
        theme.text()
    };
    let mut title = Line::from(Span::raw(" ".repeat(INSET)));
    push(&mut title, "Overview", title_ink, width);
    const CLOSE: &str = "[x]";
    let close_at = INSET + inner.saturating_sub(text::str_width(CLOSE));
    pad_to(&mut title, close_at);
    push(&mut title, CLOSE, theme.muted(), width);
    out.title = title;
    out.title_hits.push((
        (close_at, close_at + text::str_width(CLOSE)),
        PaneHit::Close,
    ));

    let mut entry = 0;
    let push_line =
        |out: &mut PaneOut, line: Line<'static>, of: Option<usize>, hit: Option<PaneHit>| {
            if let Some(hit) = hit {
                out.hits.push((out.body.len(), (0, width), hit));
            }
            out.entries.push(of);
            out.body.push(line);
        };
    let blank = |out: &mut PaneOut| {
        out.entries.push(None);
        out.body.push(Line::default());
    };
    let mut card = None;
    let sections = contents.sections();
    for (n, section) in sections.iter().copied().enumerate() {
        if n > 0 {
            blank(&mut out);
            blank(&mut out);
        }
        let folded = contents.folded.contains(&section);
        let count = match section {
            Section::Tasks => contents
                .overview
                .tasks
                .as_ref()
                .map(|tasks| format!("{} of {}", tasks.done, tasks.total))
                .unwrap_or_default(),
            Section::Background => contents.overview.jobs.len().to_string(),
            Section::Changes => contents
                .overview
                .changes
                .as_ref()
                .map(|changes| changes.totals.files.to_string())
                .unwrap_or_default(),
            Section::Servers => contents.overview.failed_servers.len().to_string(),
            Section::Usage => match &contents.overview.usage_near_limit {
                Some(usage) if usage.blocked => "limit reached".to_owned(),
                _ => "near a limit".to_owned(),
            },
        };
        // Like home's headings: a fold marker, the words, the count and a
        // hairline to the edge. A control: lit, its marker and words
        // brighten, and only they answer the mouse.
        let item = PaneItem::Heading(section);
        let chosen = lit == Some(&item);
        let (marker_ink, words_ink) = if chosen {
            (theme.emphasis(), theme.emphasis())
        } else {
            (theme.faint(), theme.muted().add_modifier(Modifier::BOLD))
        };
        let mut line = Line::default();
        push(&mut line, if folded { "▸" } else { "▾" }, marker_ink, inner);
        pad_to(&mut line, 2);
        push(&mut line, section.words(), words_ink, inner);
        let words_end = INSET + text::line_width(&line);
        push(&mut line, format!(" {count} "), theme.faint(), inner);
        let rule = inner.saturating_sub(text::line_width(&line));
        push(&mut line, "─".repeat(rule), theme.hairline(), inner);
        out.item_lines.push((item.clone(), out.body.len()));
        out.hits
            .push((out.body.len(), (0, words_end), PaneHit::Item(item)));
        push_line(&mut out, line, None, None);
        if folded {
            continue;
        }
        blank(&mut out);
        match section {
            Section::Tasks => {
                let Some(tasks) = &contents.overview.tasks else {
                    continue;
                };
                for task in &tasks.entries {
                    let (mark, mark_style, words) = match task.mark {
                        TaskMark::Done => ("✓", theme.faint(), theme.faint()),
                        TaskMark::Current => ("●", theme.text(), theme.bright()),
                        TaskMark::Todo => ("○", theme.faint(), theme.text()),
                    };
                    for (i, part) in text::wrap(&task.subject, inner.saturating_sub(2).max(1))
                        .into_iter()
                        .enumerate()
                    {
                        let mut line = Line::default();
                        if i == 0 {
                            push(&mut line, mark, mark_style, inner);
                        }
                        pad_to(&mut line, 2);
                        push(&mut line, part, words, inner);
                        push_line(&mut out, line, Some(entry), None);
                    }
                    entry += 1;
                }
            }
            Section::Background => {
                for job in &contents.overview.jobs {
                    // Each job on one line: what it runs, and how long it
                    // has run. A command reads as code; the provider's own
                    // subagent or monitor is named by a description, so it
                    // reads as words after a tag saying which it is. A
                    // click (or Enter on the focused one) opens the step
                    // that started it.
                    // A job asked to stop says so in place of how long it
                    // has run.
                    let age = if job.stop == JobStop::Stopping {
                        "Stopping…".to_owned()
                    } else {
                        ran_for(now_ms - job.started_at_ms)
                    };
                    let mut line = Line::default();
                    let (tag, style) = match job.kind {
                        BackgroundKind::Subagent => (Some("subagent  "), theme.text()),
                        BackgroundKind::Monitor => (Some("monitor  "), theme.text()),
                        BackgroundKind::Shell
                        | BackgroundKind::Other
                        | BackgroundKind::Unspecified => (None, theme.code()),
                    };
                    if let Some(tag) = tag {
                        push(&mut line, tag, theme.faint(), inner);
                    }
                    let room = inner.saturating_sub(line.width() + text::str_width(&age) + 2);
                    let command = text::ellipsize(text::first_line(&job.command), room);
                    push(&mut line, command, style, inner);
                    pad_to(&mut line, inner.saturating_sub(text::str_width(&age)));
                    push(&mut line, age, theme.faint(), inner);
                    let item = PaneItem::Job(job.clone());
                    if lit == Some(&item) {
                        card = Some(out.body.len());
                    }
                    out.item_lines.push((item.clone(), out.body.len()));
                    push_line(
                        &mut out,
                        line,
                        Some(entry),
                        Some(PaneHit::Item(item.clone())),
                    );
                    // A stop the provider refused, in its words, under the
                    // job; the job can be stopped again.
                    if let JobStop::Refused { error, .. } = &job.stop {
                        let mut line = Line::default();
                        pad_to(&mut line, 2);
                        let words =
                            text::ellipsize(text::first_line(error), inner.saturating_sub(2));
                        push(&mut line, words, theme.error(), inner);
                        push_line(&mut out, line, Some(entry), Some(PaneHit::Item(item)));
                    }
                    entry += 1;
                }
            }
            Section::Changes => {
                // The shared changes list; only files take the keys, and a
                // click (or Enter) opens the review page at one.
                let Some(changes) = &contents.overview.changes else {
                    continue;
                };
                let list = super::changes::changes_lines(changes, inner, theme);
                let start = out.body.len();
                out.dir_lines
                    .extend(list.dir_lines.iter().map(|line| start + line));
                let mut files = list.files.into_iter().peekable();
                for (at, line) in list.lines.into_iter().enumerate() {
                    match files.next_if(|(file_at, _)| *file_at == at) {
                        Some((_, path)) => {
                            let item = PaneItem::File(path);
                            if lit == Some(&item) {
                                card = Some(out.body.len());
                            }
                            out.item_lines.push((item.clone(), out.body.len()));
                            push_line(&mut out, line, Some(entry), Some(PaneHit::Item(item)));
                            entry += 1;
                        }
                        None => push_line(&mut out, line, None, None),
                    }
                }
                out.changes_end = out.body.len();
            }
            Section::Servers => {
                for server in &contents.overview.failed_servers {
                    let what = if server.needs_auth {
                        "needs sign-in"
                    } else {
                        "failed to start"
                    };
                    let mut line = Line::default();
                    push(
                        &mut line,
                        format!("{} {what}", server.name),
                        theme.error(),
                        inner,
                    );
                    push_line(&mut out, line, Some(entry), None);
                    if !server.error.is_empty() {
                        let mut line = Line::default();
                        push(
                            &mut line,
                            text::ellipsize(text::first_line(&server.error), inner),
                            theme.faint(),
                            inner,
                        );
                        push_line(&mut out, line, Some(entry), None);
                    }
                    entry += 1;
                }
            }
            Section::Usage => {
                let Some(usage) = &contents.overview.usage_near_limit else {
                    continue;
                };
                for window in &usage.windows {
                    // Each window on two lines: its name and how full it
                    // is, then its state and when it resets, in the ink
                    // of its state.
                    let (state, ink) = match window.state {
                        UsageState::Blocked => (Some("reached"), theme.warning()),
                        UsageState::NearLimit => (Some("near"), theme.text()),
                        UsageState::Ok => (Some("fine"), theme.muted()),
                        UsageState::Unknown => (None, theme.muted()),
                    };
                    let used = window
                        .used_percent
                        .map_or("—".to_owned(), |used| format!("{used:.0}% used"));
                    let mut line = Line::default();
                    let room = inner.saturating_sub(text::str_width(&used) + 2);
                    push(
                        &mut line,
                        text::ellipsize(&super::composer::limit_name(&window.label), room),
                        ink,
                        inner,
                    );
                    pad_to(&mut line, inner.saturating_sub(text::str_width(&used)));
                    push(&mut line, used, ink, inner);
                    push_line(&mut out, line, Some(entry), None);
                    let resets = window
                        .resets_at_ms
                        .filter(|at| *at > now_ms)
                        .map(|at| format!("resets {}", super::composer::resets(at, now_ms)));
                    let words: Vec<String> =
                        state.map(str::to_owned).into_iter().chain(resets).collect();
                    if !words.is_empty() {
                        let mut line = Line::default();
                        pad_to(&mut line, 2);
                        push(&mut line, words.join(" · "), theme.faint(), inner);
                        push_line(&mut out, line, Some(entry), None);
                    }
                    entry += 1;
                }
                if let Some(credits) = &usage.credits {
                    let mut line = Line::default();
                    push(
                        &mut line,
                        format!("credits {credits}"),
                        theme.faint(),
                        inner,
                    );
                    push_line(&mut out, line, Some(entry), None);
                    entry += 1;
                }
            }
        }
    }
    if sections.is_empty() {
        let mut line = Line::default();
        push(&mut line, "Nothing running", theme.faint(), inner);
        push_line(&mut out, line, None, None);
    }

    for line in &mut out.body {
        line.spans.insert(0, Span::raw(" ".repeat(INSET)));
    }
    if let Some(at) = card {
        card_highlight(&mut out.body, at..at + 1, 0, width, false, theme);
    }
    out
}

/// What a window over the body, `height` lines from line `top`, hides past
/// each edge: how many tasks, jobs and files lie wholly above and below
/// what it shows, once an edge line gives way to its "↑ N more" or "↓ N
/// more". None where nothing is hidden that way, and none either where
/// only a heading or blank lines are: the edge line then shows as itself.
/// `cover` more lines under the top edge are hidden by a pinned line.
pub fn hidden(
    out: &PaneOut,
    top: usize,
    height: usize,
    cover: usize,
) -> (Option<usize>, Option<usize>) {
    let len = out.body.len();
    let count = |range: std::ops::Range<usize>, shown: std::ops::Range<usize>| {
        let mut seen: Vec<usize> = out.entries[range]
            .iter()
            .flatten()
            .copied()
            .filter(|entry| {
                !out.entries[shown.clone()]
                    .iter()
                    .flatten()
                    .any(|each| each == entry)
            })
            .collect();
        seen.dedup();
        seen.len()
    };
    let bottom = (top + height).min(len);
    let mut above_edge = top > 0;
    let mut below_edge = top + height < len;
    let window = |above_edge: bool, below_edge: bool| {
        (top + usize::from(above_edge) + cover).min(bottom)
            ..bottom.saturating_sub(usize::from(below_edge))
    };
    let above = above_edge.then(|| count(0..top + 1 + cover, window(true, below_edge)));
    if above == Some(0) {
        above_edge = false;
    }
    let shown = window(above_edge, below_edge);
    let below = below_edge.then(|| count(shown.end..len, shown.clone()));
    if below == Some(0) {
        below_edge = false;
    }
    (above.filter(|_| above_edge), below.filter(|_| below_edge))
}

/// The faint edge line that says how much scrolling hides that way.
pub fn more_line(up: bool, count: usize, theme: Theme) -> Line<'static> {
    let arrow = if up { "↑" } else { "↓" };
    Line::from(vec![
        Span::raw(" ".repeat(INSET)),
        Span::styled(format!("{arrow} {count} more"), theme.faint()),
    ])
}

/// Draws `range` of `lines` as home draws a highlighted card: tinted from
/// column `from` to `to`, and when `pad`, with half a line of the tint
/// above and below if the lines on both sides are blank. Where lines are
/// packed, the tint stays flat rather than lopsided.
pub fn card_highlight(
    lines: &mut [Line<'static>],
    range: std::ops::Range<usize>,
    from: usize,
    to: usize,
    pad: bool,
    theme: Theme,
) {
    let Some(surface) = theme.row_surface() else {
        return;
    };
    let Some(bg) = surface.bg else {
        return;
    };
    for line in lines.get_mut(range.clone()).into_iter().flatten() {
        *line = tint(std::mem::take(line), from, to, surface);
    }
    let edge = |glyph: &str| {
        Line::from(vec![
            Span::raw(" ".repeat(from)),
            Span::styled(
                glyph.repeat(to.saturating_sub(from)),
                Style::default().fg(bg),
            ),
        ])
    };
    let blank = |line: &Line<'_>| text::line_width(line) == 0 || is_spaces(line);
    let above = range.start.checked_sub(1);
    let padded = pad
        && above.and_then(|at| lines.get(at)).is_some_and(blank)
        && lines.get(range.end).is_some_and(blank);
    if let Some(above) = above.filter(|_| padded) {
        lines[above] = edge("▄");
        lines[range.end] = edge("▀");
    }
}

fn is_spaces(line: &Line<'_>) -> bool {
    line.spans
        .iter()
        .all(|span| span.content.chars().all(|c| c == ' '))
}

/// `line` with its cells from `from` to `to` on `surface`, each span
/// keeping its own ink.
fn tint(line: Line<'static>, from: usize, to: usize, surface: Style) -> Line<'static> {
    let mut out = Line::default();
    let mut col = 0;
    for span in line.spans {
        for c in span.content.chars() {
            let cell = text::str_width(c.encode_utf8(&mut [0; 4]));
            let style = if col >= from && col < to {
                span.style.patch(surface)
            } else {
                span.style
            };
            out.spans.push(Span::styled(c.to_string(), style));
            col += cell;
        }
    }
    if col < from {
        out.spans.push(Span::raw(" ".repeat(from - col)));
        col = from;
    }
    if col < to {
        out.spans.push(Span::styled(" ".repeat(to - col), surface));
    }
    out
}

/// How long a job has run, coarse enough not to tick every second once it
/// has run a minute: "12s", "4m", "1h 5m".
fn ran_for(ms: i64) -> String {
    let secs = ms.max(0) / 1_000;
    match secs {
        0..60 => format!("{secs}s"),
        60..3_600 => format!("{}m", secs / 60),
        _ => format!("{}h {}m", secs / 3_600, secs % 3_600 / 60),
    }
}
