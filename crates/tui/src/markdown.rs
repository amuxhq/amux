//! Terminal markdown for agent text. The text comes parsed, as the blocks
//! every client draws ([`ui_view::Markdown`]), and is drawn here as
//! typography in the chat's inks, never as its syntax: headings bright and
//! bold, bullets, quotes on a faint rail, links as underlined text that
//! opens on a click, rules as hairlines, code blocks indented and coloured
//! by the terminal's own palette, tables in aligned columns.
//!
//! Paragraphs reflow to the width given. A word is never split unless it is
//! longer than a whole line, URLs included; code lines keep their spacing
//! and hard-wrap. A message's lines are kept by content and width, so only
//! a message still streaming is drawn again.

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::rc::Rc;

use ratatui::style::{Modifier, Style};
use ratatui::text::Span;
use ui_view::{InlineRun, Markdown, ProseBlock, ProseItem, TableAlign};

use crate::chat::selection::Join;
use crate::text::{clip_to_width, str_width};
use crate::theme::Theme;

/// One drawn line: its spans, and the links on it by column.
#[derive(Clone, Debug, Default)]
pub(crate) struct MdLine {
    pub spans: Vec<Span<'static>>,
    pub links: Vec<(usize, usize, String)>,
    /// The column copied text starts at: past a quote's rail and, on a
    /// wrapped line, past the hanging indent.
    pub copy_from: usize,
    /// How the line continues the one before it when copied.
    pub join: Join,
    /// A code line: copied with its spacing as drawn.
    pub exact: bool,
}

/// A styled run of inline text, with the link it belongs to.
#[derive(Clone, Debug)]
struct Run {
    text: String,
    style: Style,
    link: Option<String>,
}

/// A table whose columns would be narrower than this is drawn as records.
const MIN_COLUMN: usize = 12;
/// The rule between table columns, with a column of room either side.
const RULE: &str = " │ ";

/// Words an agent wrote that come as text rather than parsed with a row,
/// such as a question or an option's preview, drawn as rows of spans
/// wrapped at `width`.
pub(crate) fn markdown_rows(source: &str, width: usize, theme: Theme) -> Vec<Vec<Span<'static>>> {
    markdown_lines(&Markdown::parse(source, false), false, width, theme)
        .iter()
        .map(|line| line.spans.clone())
        .collect()
}

/// The text drawn as lines with their links, wrapped at `width`.
/// `streaming` is for a message still being written, whose last code block
/// may still be open. Kept for the next frame by source, width and theme
/// once every code block in it is coloured as far as it will be, so a
/// message is drawn again only while it changes or its code is still
/// being coloured.
pub(crate) fn markdown_lines(
    markdown: &Markdown,
    streaming: bool,
    width: usize,
    theme: Theme,
) -> Rc<Vec<MdLine>> {
    type Kept = HashMap<(u64, usize, bool), (Theme, Rc<Vec<MdLine>>)>;
    thread_local! {
        static KEPT: RefCell<Kept> = RefCell::new(HashMap::new());
    }
    let width = width.max(1);
    let hash = {
        let mut hasher = DefaultHasher::new();
        markdown.source.hash(&mut hasher);
        hasher.finish()
    };
    let key = (hash, width, streaming);
    if let Some(lines) = KEPT.with(|kept| {
        kept.borrow()
            .get(&key)
            .filter(|(drawn_in, _)| *drawn_in == theme)
            .map(|(_, lines)| lines.clone())
    }) {
        return lines;
    }
    let (lines, settled) = render(&markdown.blocks, streaming, width, theme);
    let lines = Rc::new(lines);
    if !settled {
        return lines;
    }
    KEPT.with(|kept| {
        let mut kept = kept.borrow_mut();
        if kept.len() > 512 {
            kept.clear();
        }
        kept.insert(key, (theme, lines.clone()));
    });
    lines
}

/// The blocks' lines, and whether every code block in them is settled.
fn render(
    blocks: &[ProseBlock],
    streaming: bool,
    width: usize,
    theme: Theme,
) -> (Vec<MdLine>, bool) {
    let mut drawer = Drawer::new(width, theme, streaming);
    drawer.blocks(blocks);
    drawer.flush();
    let mut out = drawer.out;
    while out
        .last()
        .is_some_and(|line| line.spans.iter().all(|s| s.content.trim().is_empty()))
    {
        out.pop();
    }
    (out, drawer.settled)
}

/// What encloses the text being drawn, outermost first.
#[derive(Clone, Debug)]
enum Container {
    Quote,
    /// A list item: its marker, drawn on its first line, and the column its
    /// text starts at, which wrapped lines and nested blocks hang from.
    Item {
        marker: Vec<Span<'static>>,
        width: usize,
        used: bool,
    },
}

struct Table {
    alignments: Vec<TableAlign>,
    /// The header's cells, then each row's.
    rows: Vec<Vec<Vec<Run>>>,
}

struct Drawer {
    width: usize,
    theme: Theme,
    /// The message is still being written, so its unclosed code block is
    /// the one still arriving.
    streaming: bool,
    out: Vec<MdLine>,
    runs: Vec<Run>,
    containers: Vec<Container>,
    heading: bool,
    /// The item's task is done: its words recede.
    done: bool,
    /// Every code block drawn so far is coloured as far as it will be.
    settled: bool,
    /// Nothing has been drawn since the current list item began.
    fresh_item: bool,
    last_blank: bool,
}

impl Drawer {
    fn new(width: usize, theme: Theme, streaming: bool) -> Drawer {
        Drawer {
            width,
            theme,
            streaming,
            out: Vec::new(),
            runs: Vec::new(),
            containers: Vec::new(),
            heading: false,
            done: false,
            settled: true,
            fresh_item: false,
            last_blank: true,
        }
    }

    fn quoted(&self) -> bool {
        self.containers
            .iter()
            .any(|container| matches!(container, Container::Quote))
    }

    fn in_item(&self) -> bool {
        self.containers
            .iter()
            .any(|container| matches!(container, Container::Item { .. }))
    }

    /// The style plain inline text takes here.
    fn ink(&self) -> Style {
        let theme = self.theme;
        if self.heading {
            theme.emphasis()
        } else if self.done {
            theme.faint()
        } else if self.quoted() {
            theme.muted()
        } else {
            theme.text()
        }
    }

    /// A parsed run as drawn here; a line break is a run of its own.
    fn runs_of(&self, runs: &[InlineRun]) -> Vec<Run> {
        let ink = self.ink();
        let mut out = Vec::new();
        for run in runs {
            let mut style = if run.code { self.theme.code() } else { ink };
            if !run.code {
                if run.strong {
                    style = style.add_modifier(Modifier::BOLD);
                }
                if run.emphasis {
                    style = style.add_modifier(Modifier::ITALIC);
                }
                if run.strikethrough {
                    style = style.add_modifier(Modifier::CROSSED_OUT);
                }
            }
            if run.link.is_some() {
                style = style.add_modifier(Modifier::UNDERLINED);
            }
            for (i, piece) in run.text.split('\n').enumerate() {
                if i > 0 {
                    out.push(Run {
                        text: "\n".into(),
                        style: Style::default(),
                        link: None,
                    });
                }
                if !piece.is_empty() {
                    out.push(Run {
                        text: piece.to_owned(),
                        style,
                        link: run.link.clone(),
                    });
                }
            }
        }
        out
    }

    fn blocks(&mut self, blocks: &[ProseBlock]) {
        for block in blocks {
            self.block(block);
        }
    }

    fn block(&mut self, block: &ProseBlock) {
        match block {
            ProseBlock::Paragraph { text } => {
                if !self.fresh_item {
                    self.flush();
                    self.gap();
                }
                self.runs = self.runs_of(text);
                self.flush();
            }
            ProseBlock::Heading { text, .. } => {
                self.flush();
                self.gap();
                self.heading = true;
                self.runs = self.runs_of(text);
                self.flush();
                self.heading = false;
            }
            ProseBlock::Quote { blocks } => {
                self.flush();
                self.gap();
                self.containers.push(Container::Quote);
                self.blocks(blocks);
                self.flush();
                self.containers.pop();
            }
            ProseBlock::Code {
                language,
                code,
                closed,
            } => {
                self.flush();
                if !self.fresh_item {
                    self.gap();
                }
                let open = self.streaming && !closed;
                self.code_block(language, code, open);
            }
            ProseBlock::List {
                ordered,
                start,
                items,
                ..
            } => {
                self.flush();
                // A list starts a block; one nested in an item hangs under
                // its text without a gap.
                if !self.in_item() {
                    self.gap();
                }
                self.list(*ordered, *start, items);
                self.flush();
            }
            ProseBlock::Table {
                alignments,
                header,
                rows,
            } => {
                self.flush();
                self.gap();
                let rows = std::iter::once(header)
                    .chain(rows)
                    .map(|row| row.iter().map(|cell| self.runs_of(cell)).collect())
                    .collect();
                self.draw_table(Table {
                    alignments: alignments.clone(),
                    rows,
                });
            }
            ProseBlock::Rule => {
                self.flush();
                self.gap();
                let (first, _) = self.prefixes();
                let room = self.width.saturating_sub(spans_width(&first)).max(1);
                let mut spans = first;
                spans.push(Span::styled("─".repeat(room), self.theme.hairline()));
                self.push_line(spans, Vec::new());
            }
        }
    }

    /// A list's items, each marker in a column as wide as the list's widest,
    /// so every item's text starts at one edge: numbers stand right-aligned
    /// in it, `9.` over `10.`.
    fn list(&mut self, ordered: bool, start: u64, items: &[ProseItem]) {
        let markers: Vec<String> = items
            .iter()
            .enumerate()
            .map(|(at, item)| match item.task {
                Some(true) => "✓".to_owned(),
                Some(false) => "○".to_owned(),
                None if ordered => format!("{}.", start.saturating_add(at as u64)),
                None => "•".to_owned(),
            })
            .collect();
        let column = markers
            .iter()
            .map(|marker| str_width(marker))
            .max()
            .unwrap_or(0)
            + 1;
        for (item, marker) in items.iter().zip(markers) {
            self.flush();
            let ink = match item.task {
                Some(_) => self.theme.faint(),
                None if self.quoted() => self.theme.muted(),
                None => self.theme.text(),
            };
            let pad = column - 1 - str_width(&marker);
            let words = format!("{}{marker} ", " ".repeat(pad));
            self.containers.push(Container::Item {
                width: column,
                marker: vec![Span::styled(words, ink)],
                used: false,
            });
            self.fresh_item = true;
            self.done = item.task == Some(true);
            self.blocks(&item.blocks);
            self.flush();
            if matches!(self.containers.last(), Some(Container::Item { .. })) {
                self.containers.pop();
            }
            self.done = false;
            self.fresh_item = false;
        }
    }

    /// The prefixes a block's first and later lines take from what encloses
    /// it: a quote's rail, a list item's marker then its hanging indent.
    fn prefixes(&mut self) -> (Vec<Span<'static>>, Vec<Span<'static>>) {
        let rail = Span::styled("│ ", self.theme.faint());
        let mut first = Vec::new();
        let mut rest = Vec::new();
        for container in &mut self.containers {
            match container {
                Container::Quote => {
                    first.push(rail.clone());
                    rest.push(rail.clone());
                }
                Container::Item {
                    marker,
                    width,
                    used,
                } => {
                    if *used {
                        first.push(Span::raw(" ".repeat(*width)));
                    } else {
                        first.extend(marker.iter().cloned());
                        *used = true;
                    }
                    rest.push(Span::raw(" ".repeat(*width)));
                }
            }
        }
        (first, rest)
    }

    fn push_line(&mut self, spans: Vec<Span<'static>>, links: Vec<(usize, usize, String)>) {
        self.push_copied(spans, links, 0, Join::New);
    }

    fn push_copied(
        &mut self,
        spans: Vec<Span<'static>>,
        links: Vec<(usize, usize, String)>,
        copy_from: usize,
        join: Join,
    ) {
        self.last_blank = false;
        self.out.push(MdLine {
            spans,
            links,
            copy_from,
            join,
            exact: false,
        });
    }

    /// One blank line between blocks, carrying a quote's rail inside it.
    fn gap(&mut self) {
        if self.last_blank {
            return;
        }
        let rail: Vec<Span<'static>> = self
            .containers
            .iter()
            .filter(|container| matches!(container, Container::Quote))
            .map(|_| Span::styled("│", self.theme.faint()))
            .collect();
        self.out.push(MdLine {
            copy_from: spans_width(&rail),
            spans: rail,
            ..MdLine::default()
        });
        self.last_blank = true;
    }

    /// Draws the inline text gathered so far as wrapped lines.
    fn flush(&mut self) {
        if self.runs.is_empty() {
            return;
        }
        let runs = std::mem::take(&mut self.runs);
        if runs.iter().all(|run| run.text.trim().is_empty()) {
            return;
        }
        // A list's marker is copied with its first line; rails and hanging
        // indents are not.
        let rails = 2 * self
            .containers
            .iter()
            .take_while(|container| matches!(container, Container::Quote))
            .count();
        let marked = self
            .containers
            .iter()
            .any(|container| matches!(container, Container::Item { used: false, .. }));
        let (first, rest) = self.prefixes();
        let room = self
            .width
            .saturating_sub(spans_width(&first).max(spans_width(&rest)))
            .max(1);
        for (i, (pieces, join)) in wrap_joined(&runs, room).into_iter().enumerate() {
            let prefix = if i == 0 { &first } else { &rest };
            let copy_from = match (i, marked) {
                (0, true) => rails,
                _ => spans_width(prefix),
            };
            let (spans, links) = placed(prefix, &pieces);
            self.push_copied(spans, links, copy_from, join);
        }
        self.fresh_item = false;
    }

    /// A code block: no fences and no box, two columns in from the text,
    /// coloured by kind of token; long lines keep their spacing and wrap.
    /// `open` is for the block still being written.
    fn code_block(&mut self, language: &str, code: &str, open: bool) {
        let code = &crate::text::expand_tabs(code);
        let (lines, settled) = crate::code::block(code, language, open, self.theme);
        self.settled &= settled;
        let (first, rest) = self.prefixes();
        let indent = Span::raw("  ");
        let room = self.width.saturating_sub(spans_width(&rest) + 2).max(1);
        for (n, line) in lines.iter().enumerate() {
            for (i, row) in hard_wrap(line, room).into_iter().enumerate() {
                let mut spans = if n == 0 && i == 0 {
                    first.clone()
                } else {
                    rest.clone()
                };
                let copy_from = spans_width(&spans) + 2;
                spans.push(indent.clone());
                spans.extend(row);
                let join = if i == 0 { Join::New } else { Join::Tight };
                self.push_copied(spans, Vec::new(), copy_from, join);
                if let Some(drawn) = self.out.last_mut() {
                    drawn.exact = true;
                }
            }
        }
        self.fresh_item = false;
    }

    /// A table: aligned columns under a bold header, with inner rules only:
    /// a hairline between columns, one under the header, and one under
    /// every row once any cell wraps. Columns shrink by their content, the
    /// widest first and not below their longest word where that can be
    /// helped. When a column would drop below [`MIN_COLUMN`], each row is
    /// drawn as a record instead.
    fn draw_table(&mut self, table: Table) {
        let theme = self.theme;
        let (first, rest) = self.prefixes();
        let room = self.width.saturating_sub(spans_width(&rest)).max(1);
        let columns = table.rows.iter().map(Vec::len).max().unwrap_or(0);
        if columns == 0 {
            return;
        }
        let cell = |row: usize, column: usize| -> Vec<Run> {
            let runs = table.rows[row].get(column).cloned().unwrap_or_default();
            if row == 0 {
                runs.into_iter()
                    .map(|mut run| {
                        run.style = run.style.add_modifier(Modifier::BOLD);
                        run
                    })
                    .collect()
            } else {
                runs
            }
        };
        let natural: Vec<usize> = (0..columns)
            .map(|column| {
                (0..table.rows.len())
                    .map(|row| runs_width(&cell(row, column)))
                    .max()
                    .unwrap_or(0)
                    .max(1)
            })
            .collect();
        let longest_word: Vec<usize> = (0..columns)
            .map(|column| {
                (0..table.rows.len())
                    .flat_map(|row| {
                        let text: String = cell(row, column)
                            .iter()
                            .map(|run| run.text.as_str())
                            .collect();
                        text.split_whitespace().map(str_width).collect::<Vec<_>>()
                    })
                    .max()
                    .unwrap_or(1)
            })
            .collect();
        let rules = RULE.len() * (columns - 1);
        let available = room.saturating_sub(rules);
        // Each column starts at what it cannot do without: its longest word
        // (or MIN_COLUMN, whichever is more), never more than its content;
        // failing that, MIN_COLUMN with words split; failing that, records.
        let whole: Vec<usize> = (0..columns)
            .map(|c| natural[c].min(longest_word[c].max(MIN_COLUMN)))
            .collect();
        let split: Vec<usize> = natural.iter().map(|w| (*w).min(MIN_COLUMN)).collect();
        let base = if whole.iter().sum::<usize>() <= available {
            whole
        } else if split.iter().sum::<usize>() <= available {
            split
        } else {
            self.records(&table, first, rest);
            return;
        };
        // The room left is shared out like water: a column that needs less
        // than an even share gets all it needs, so short text never wraps
        // while long text has room, and what remains is split evenly among
        // the columns that still want more.
        let mut widths = base.clone();
        let mut spare = available.saturating_sub(base.iter().sum::<usize>());
        let mut wanting: Vec<usize> = (0..columns).filter(|c| natural[*c] > base[*c]).collect();
        wanting.sort_by_key(|c| natural[*c] - base[*c]);
        while !wanting.is_empty() && spare > 0 {
            let share = spare / wanting.len();
            let want = natural[wanting[0]] - widths[wanting[0]];
            if want <= share {
                widths[wanting[0]] += want;
                spare -= want;
                wanting.remove(0);
                continue;
            }
            // Everyone left wants more than an even share: split it.
            let mut left = spare;
            for (n, c) in wanting.iter().enumerate() {
                let give = if n + 1 == wanting.len() {
                    left
                } else {
                    share.max(1).min(left)
                };
                widths[*c] += give;
                left -= give;
            }
            break;
        }

        let wrapped: Vec<Vec<Vec<Vec<Run>>>> = (0..table.rows.len())
            .map(|row| {
                (0..columns)
                    .map(|column| wrap(&cell(row, column), widths[column]))
                    .collect()
            })
            .collect();
        let wraps = wrapped
            .iter()
            .any(|row| row.iter().any(|lines| lines.len() > 1));
        // Alignment as the markdown's markers say, header included.
        let aligns: Vec<TableAlign> = (0..columns)
            .map(|column| {
                table
                    .alignments
                    .get(column)
                    .copied()
                    .unwrap_or(TableAlign::None)
            })
            .collect();
        let bar = || Span::styled(RULE, theme.hairline());
        let rule_line = |ink: Style| {
            let mut spans = Vec::new();
            for (column, width) in widths.iter().enumerate() {
                if column > 0 {
                    spans.push(Span::styled("─┼─", ink));
                }
                spans.push(Span::styled("─".repeat(*width), ink));
            }
            MdLine {
                spans,
                ..MdLine::default()
            }
        };
        let mut lines: Vec<MdLine> = Vec::new();
        let rows = wrapped.len();
        for (r, cells) in wrapped.iter().enumerate() {
            let height = cells.iter().map(Vec::len).max().unwrap_or(1).max(1);
            for line in 0..height {
                let mut spans: Vec<Span<'static>> = Vec::new();
                let mut links = Vec::new();
                let mut col = 0;
                for (column, cell) in cells.iter().enumerate() {
                    if column > 0 {
                        spans.push(bar());
                        col += RULE.len();
                    }
                    let pieces = cell.get(line).cloned().unwrap_or_default();
                    let used = runs_width(&pieces);
                    let slack = widths[column].saturating_sub(used);
                    let (left, right) = match aligns[column] {
                        TableAlign::Right => (slack, 0),
                        TableAlign::Center => (slack / 2, slack - slack / 2),
                        TableAlign::Left | TableAlign::None => (0, slack),
                    };
                    spans.push(Span::raw(" ".repeat(left)));
                    let (cell_spans, cell_links) = placed(&[], &pieces);
                    links.extend(
                        cell_links
                            .into_iter()
                            .map(|(from, to, url)| (col + left + from, col + left + to, url)),
                    );
                    spans.extend(cell_spans);
                    // The last column needs no padding after it.
                    if column + 1 < columns {
                        spans.push(Span::raw(" ".repeat(right)));
                    }
                    col += left + used + right;
                }
                lines.push(MdLine {
                    spans,
                    links,
                    ..MdLine::default()
                });
            }
            // With rules between rows, the header's is one ink stronger so
            // the header stays apart from them.
            if r == 0 {
                lines.push(rule_line(if wraps {
                    theme.faint()
                } else {
                    theme.hairline()
                }));
            } else if wraps && r + 1 < rows {
                lines.push(rule_line(theme.hairline()));
            }
        }
        for (i, line) in lines.into_iter().enumerate() {
            let prefix = if i == 0 { &first } else { &rest };
            let offset = spans_width(prefix);
            let mut spans = prefix.clone();
            spans.extend(line.spans);
            let links = line
                .links
                .into_iter()
                .map(|(from, to, url)| (offset + from, offset + to, url))
                .collect();
            self.push_line(spans, links);
        }
        self.fresh_item = false;
    }

    /// A table too wide for its columns, drawn as records: each row's
    /// "Header: value" lines, headers bold, values wrapping under
    /// themselves, a hairline across the width between records.
    fn records(&mut self, table: &Table, first: Vec<Span<'static>>, rest: Vec<Span<'static>>) {
        let theme = self.theme;
        let room = self.width.saturating_sub(spans_width(&rest)).max(1);
        let header = table.rows.first().cloned().unwrap_or_default();
        let mut first = Some(first);
        let prefix = |first: &mut Option<Vec<Span<'static>>>| match first.take() {
            Some(first) => first,
            None => rest.clone(),
        };
        for (r, row) in table.rows.iter().enumerate().skip(1) {
            if r > 1 {
                let mut spans = prefix(&mut first);
                spans.push(Span::styled("─".repeat(room), theme.hairline()));
                self.push_line(spans, Vec::new());
            }
            for (column, value) in row.iter().enumerate() {
                let key: Vec<Run> = header
                    .get(column)
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|mut run| {
                        run.style = theme.text().add_modifier(Modifier::BOLD);
                        run
                    })
                    .collect();
                let key_width = runs_width(&key) + 2;
                let mut label = key;
                label.push(Run {
                    text: ": ".into(),
                    style: theme.text(),
                    link: None,
                });
                // The value hangs under itself; a key too long for that puts
                // its value on the next line.
                let (hang, value_room, own_line) = if room.saturating_sub(key_width) >= 16 {
                    (key_width, room - key_width, false)
                } else {
                    (2, room.saturating_sub(2).max(1), true)
                };
                let value_lines = wrap(value, value_room);
                if own_line {
                    let (spans, links) = placed(&prefix(&mut first), &label);
                    self.push_line(spans, links);
                }
                for (i, pieces) in value_lines.into_iter().enumerate() {
                    let mut lead = prefix(&mut first);
                    let mut runs = Vec::new();
                    if i == 0 && !own_line {
                        runs.extend(label.iter().cloned());
                    } else {
                        lead.push(Span::raw(" ".repeat(hang)));
                    }
                    runs.extend(pieces);
                    let (spans, links) = placed(&lead, &runs);
                    self.push_line(spans, links);
                }
            }
        }
        self.fresh_item = false;
    }
}

fn spans_width(spans: &[Span<'_>]) -> usize {
    spans.iter().map(|span| str_width(&span.content)).sum()
}

fn runs_width(runs: &[Run]) -> usize {
    runs.iter().map(|run| str_width(&run.text)).sum()
}

/// A line's spans after `prefix`, with its links' columns.
fn placed(
    prefix: &[Span<'static>],
    pieces: &[Run],
) -> (Vec<Span<'static>>, Vec<(usize, usize, String)>) {
    let mut spans = prefix.to_vec();
    let mut col = spans_width(prefix);
    let mut links: Vec<(usize, usize, String)> = Vec::new();
    for piece in pieces {
        let width = str_width(&piece.text);
        if let Some(url) = &piece.link {
            match links.last_mut() {
                Some((_, to, last)) if *to == col && last == url => *to += width,
                _ => links.push((col, col + width, url.clone())),
            }
        }
        spans.push(Span::styled(piece.text.clone(), piece.style));
        col += width;
    }
    (spans, links)
}

/// Greedy word wrap over styled runs into lines of at most `width`
/// columns. A word is never split unless it is longer than a whole line
/// (a URL that fits any line is never broken); a hard break starts a line.
/// The space between two words of one link carries its underline.
fn wrap(runs: &[Run], width: usize) -> Vec<Vec<Run>> {
    wrap_joined(runs, width)
        .into_iter()
        .map(|(line, _)| line)
        .collect()
}

/// [`wrap`], with how each line continues the one before it.
fn wrap_joined(runs: &[Run], width: usize) -> Vec<(Vec<Run>, Join)> {
    let width = width.max(1);
    enum Token {
        Word(Vec<Run>),
        Space,
        Break,
    }
    let mut tokens: Vec<Token> = Vec::new();
    let mut word: Vec<Run> = Vec::new();
    for run in runs {
        if run.text == "\n" {
            if !word.is_empty() {
                tokens.push(Token::Word(std::mem::take(&mut word)));
            }
            tokens.push(Token::Break);
            continue;
        }
        let mut piece = String::new();
        for c in run.text.chars() {
            if c.is_whitespace() {
                if !piece.is_empty() {
                    word.push(Run {
                        text: std::mem::take(&mut piece),
                        style: run.style,
                        link: run.link.clone(),
                    });
                }
                if !word.is_empty() {
                    tokens.push(Token::Word(std::mem::take(&mut word)));
                }
                tokens.push(Token::Space);
            } else {
                piece.push(c);
            }
        }
        if !piece.is_empty() {
            word.push(Run {
                text: piece,
                style: run.style,
                link: run.link.clone(),
            });
        }
    }
    if !word.is_empty() {
        tokens.push(Token::Word(word));
    }

    let mut lines: Vec<Vec<Run>> = vec![Vec::new()];
    let mut joins = vec![Join::New];
    let mut used = 0;
    let mut spaced = false;
    for token in tokens {
        match token {
            Token::Break => {
                lines.push(Vec::new());
                joins.push(Join::New);
                used = 0;
                spaced = false;
            }
            Token::Space => spaced = used > 0,
            Token::Word(pieces) => {
                let needed = runs_width(&pieces);
                if used > 0 && used + usize::from(spaced) + needed <= width {
                    if spaced {
                        let line = lines.last_mut().expect("a line");
                        let before = line.last().map(|run| run.link.clone()).unwrap_or(None);
                        let after = pieces.first().and_then(|run| run.link.clone());
                        let (style, link) = match (before, after) {
                            (Some(a), Some(b)) if a == b => (pieces[0].style, Some(a)),
                            _ => (Style::default(), None),
                        };
                        line.push(Run {
                            text: " ".into(),
                            style,
                            link,
                        });
                        used += 1;
                    }
                    lines.last_mut().expect("a line").extend(pieces);
                    used += needed;
                } else if needed <= width {
                    if used > 0 {
                        lines.push(Vec::new());
                        joins.push(Join::Space);
                    }
                    lines.last_mut().expect("a line").extend(pieces);
                    used = needed;
                } else {
                    // Longer than a whole line: split it, the one case a
                    // word is broken.
                    if used > 0 {
                        lines.push(Vec::new());
                        joins.push(Join::Space);
                        used = 0;
                    }
                    for piece in pieces {
                        let mut rest = piece.text.as_str();
                        while !rest.is_empty() {
                            if used == width {
                                lines.push(Vec::new());
                                joins.push(Join::Tight);
                                used = 0;
                            }
                            let take = clip_to_width(rest, width - used);
                            let take = if take.is_empty() {
                                &rest[..rest.chars().next().map_or(0, char::len_utf8)]
                            } else {
                                take
                            };
                            lines.last_mut().expect("a line").push(Run {
                                text: take.to_owned(),
                                style: piece.style,
                                link: piece.link.clone(),
                            });
                            used += str_width(take);
                            rest = &rest[take.len()..];
                        }
                    }
                }
                spaced = false;
            }
        }
    }
    if lines.last().is_some_and(Vec::is_empty) && lines.len() > 1 {
        lines.pop();
        joins.pop();
    }
    lines.into_iter().zip(joins).collect()
}

/// A code line split into rows of at most `width` columns, its spacing
/// kept.
fn hard_wrap(line: &[(String, Style)], width: usize) -> Vec<Vec<Span<'static>>> {
    let width = width.max(1);
    let mut rows: Vec<Vec<Span<'static>>> = vec![Vec::new()];
    let mut used = 0;
    for (text, style) in line {
        let mut rest = text.as_str();
        while !rest.is_empty() {
            if used == width {
                rows.push(Vec::new());
                used = 0;
            }
            let take = clip_to_width(rest, width - used);
            let take = if take.is_empty() {
                &rest[..rest.chars().next().map_or(0, char::len_utf8)]
            } else {
                take
            };
            rows.last_mut()
                .expect("a row")
                .push(Span::styled(take.to_owned(), *style));
            used += str_width(take);
            rest = &rest[take.len()..];
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(source: &str, width: usize) -> Vec<String> {
        let (lines, _) = render(
            &Markdown::parse(source, false).blocks,
            false,
            width,
            Theme::default(),
        );
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect()
            })
            .collect()
    }

    /// The marker column is as wide as the list's widest marker, so item 9's
    /// words and item 10's start at one edge, and so do their wrapped lines.
    #[test]
    fn a_list_s_items_start_their_words_at_one_edge() {
        let source: Vec<String> = (1..=10).map(|n| format!("{n}. item {n}")).collect();
        let lines = texts(&source.join("\n"), 80);
        assert_eq!(lines[8], " 9. item 9");
        assert_eq!(lines[9], "10. item 10");
        let wrapped = texts("9. one two three four\n10. five", 12);
        assert_eq!(
            wrapped,
            [" 9. one two", "    three", "    four", "10. five"]
        );
        assert_eq!(
            texts("- [x] built\n- [ ] tested", 80),
            ["✓ built", "○ tested"]
        );
    }

    fn styles_of(lines: &[MdLine], text: &str) -> Vec<Style> {
        lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .filter(|span| span.content == text)
            .map(|span| span.style)
            .collect()
    }

    #[test]
    fn a_streaming_message_streams_only_its_open_block() {
        // The background work is held and run here, so what each frame
        // shows does not hang on a thread's speed.
        let held = ui_code::HeldJobs::default();
        crate::code::draw_with(held.highlighter());
        let theme = Theme::default();
        let keyword = theme.syntax(ui_view::TokenKind::Keyword);
        let plain = theme.syntax(ui_view::TokenKind::Plain);
        let source = "Closed:\n\n```rust\nstruct Closed;\n```\n\nOpen:\n\n```rust\nstruct Open;\nenum Partial";

        // Until the grammar is built, every block is plain.
        let parsed = Markdown::parse(source, true);
        let (building, settled) = render(&parsed.blocks, true, 80, theme);
        assert!(!settled);
        assert_eq!(styles_of(&building, "struct Closed;"), vec![plain]);
        held.run();

        // The closed block is highlighted whole at once; the open one is
        // plain until its background pass comes back.
        let (first, settled) = render(&parsed.blocks, true, 80, theme);
        assert!(!settled);
        assert_eq!(styles_of(&first, "struct"), vec![keyword]);
        assert_eq!(styles_of(&first, "struct Open;"), vec![plain]);
        assert_eq!(styles_of(&first, "enum Partial"), vec![plain]);
        assert_eq!(held.len(), 1, "the open block's pass is under way");
        crate::code::begin_frame();
        let again = markdown_lines(&parsed, true, 80, theme);
        assert!(
            crate::code::drew_unsettled(),
            "the frame asks to be drawn again"
        );
        assert!(
            !Rc::ptr_eq(&again, &markdown_lines(&parsed, true, 80, theme)),
            "an unsettled message is not kept"
        );

        held.run();
        let (passed, settled) = render(&parsed.blocks, true, 80, theme);
        assert!(settled);
        assert_eq!(styles_of(&passed, "struct"), vec![keyword, keyword]);
        // The last line, not complete yet, waits for the block to grow.
        assert_eq!(styles_of(&passed, "enum Partial"), vec![plain]);
        let kept = markdown_lines(&parsed, true, 80, theme);
        assert!(Rc::ptr_eq(&kept, &markdown_lines(&parsed, true, 80, theme)));

        // Finished, the same words are highlighted whole.
        let done = Markdown::parse(source, false);
        let (done, settled) = render(&done.blocks, false, 80, theme);
        assert!(settled);
        assert_eq!(styles_of(&done, "struct"), vec![keyword, keyword]);
        assert!(styles_of(&done, "enum Partial").is_empty());
    }
}
