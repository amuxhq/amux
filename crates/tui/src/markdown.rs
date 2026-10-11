//! Terminal markdown for agent text. The source is parsed (CommonMark with
//! GFM tables, task lists and strikethrough) and drawn as typography in the
//! chat's inks, never as its syntax: headings bright and bold, bullets,
//! quotes on a faint rail, links as underlined text that opens on a click,
//! rules as hairlines, code blocks indented and coloured by the terminal's
//! own palette, tables in aligned columns.
//!
//! Paragraphs reflow to the width given. A word is never split unless it is
//! longer than a whole line, URLs included; code lines keep their spacing
//! and hard-wrap. A message's lines are kept by content and width, so only
//! a message still streaming is parsed again.

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::rc::Rc;

use pulldown_cmark::{Alignment, CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

use crate::highlight;
use crate::text::{clip_to_width, str_width};
use crate::theme::Theme;

/// One drawn line: its spans, and the links on it by column.
#[derive(Clone, Debug, Default)]
pub(crate) struct MdLine {
    pub spans: Vec<Span<'static>>,
    pub links: Vec<(usize, usize, String)>,
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

/// A reply still streaming, less a table header whose delimiter row has
/// not arrived: until then the parser reads the header as a paragraph of
/// pipes, so it waits a moment and appears as a table.
pub(crate) fn streaming_source(source: &str) -> &str {
    let trimmed = source.trim_end_matches([' ', '\t']);
    let body = trimmed.trim_end_matches('\n');
    let start = body.rfind('\n').map_or(0, |at| at + 1);
    let last = &body[start..];
    let pipe_row = last.trim_start().starts_with('|');
    // A row under a delimiter row belongs to a table already drawn.
    let before = &body[..start.saturating_sub(1).min(body.len())];
    let previous = before.rsplit('\n').next().unwrap_or("").trim();
    let in_table = previous.starts_with('|');
    if pipe_row && !in_table {
        &source[..start]
    } else {
        source
    }
}

/// The source drawn as rows of spans, wrapped at `width`.
pub(crate) fn markdown_rows(source: &str, width: usize, theme: Theme) -> Vec<Vec<Span<'static>>> {
    markdown_lines(source, width, theme)
        .iter()
        .map(|line| line.spans.clone())
        .collect()
}

/// The source drawn as lines with their links, wrapped at `width`.
pub(crate) fn markdown_lines(source: &str, width: usize, theme: Theme) -> Rc<Vec<MdLine>> {
    markdown_lines_wide(source, width, width, theme)
}

/// [`markdown_lines`] with tables free to use `wide` columns, wider than
/// the reading measure prose keeps to. Kept for the next frame by content,
/// widths and theme, so a message is parsed again only while it changes.
pub(crate) fn markdown_lines_wide(
    source: &str,
    width: usize,
    wide: usize,
    theme: Theme,
) -> Rc<Vec<MdLine>> {
    type Kept = HashMap<(u64, usize, usize, bool), (Theme, Rc<Vec<MdLine>>)>;
    thread_local! {
        static KEPT: RefCell<Kept> = RefCell::new(HashMap::new());
    }
    let width = width.max(1);
    let wide = wide.max(width);
    let hash = {
        let mut hasher = DefaultHasher::new();
        source.hash(&mut hasher);
        hasher.finish()
    };
    // Drawn before the grammars arrived, code is plain: draw it again once
    // they have.
    let key = (hash, width, wide, highlight::ready());
    if let Some(lines) = KEPT.with(|kept| {
        kept.borrow()
            .get(&key)
            .filter(|(drawn_in, _)| *drawn_in == theme)
            .map(|(_, lines)| lines.clone())
    }) {
        return lines;
    }
    let lines = Rc::new(render(source, width, wide, theme));
    KEPT.with(|kept| {
        let mut kept = kept.borrow_mut();
        if kept.len() > 512 {
            kept.clear();
        }
        kept.insert(key, (theme, lines.clone()));
    });
    lines
}

fn render(source: &str, width: usize, wide: usize, theme: Theme) -> Vec<MdLine> {
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_TASKLISTS | Options::ENABLE_STRIKETHROUGH;
    let mut drawer = Drawer::new(width, wide, theme);
    for event in Parser::new_ext(source, options) {
        drawer.event(event);
    }
    drawer.flush();
    let mut out = drawer.out;
    while out
        .last()
        .is_some_and(|line| line.spans.iter().all(|s| s.content.trim().is_empty()))
    {
        out.pop();
    }
    out
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
    alignments: Vec<Alignment>,
    rows: Vec<Vec<Vec<Run>>>,
    row: Vec<Vec<Run>>,
}

struct Drawer {
    /// The reading measure prose wraps at.
    width: usize,
    /// How wide a table may run.
    wide: usize,
    theme: Theme,
    out: Vec<MdLine>,
    runs: Vec<Run>,
    containers: Vec<Container>,
    /// The next number of each open list; None for a bulleted one.
    lists: Vec<Option<u64>>,
    bold: usize,
    italic: usize,
    strike: usize,
    heading: bool,
    /// The item's task is done: its words recede.
    done: bool,
    link: Option<String>,
    code: Option<(String, String)>,
    table: Option<Table>,
    /// Nothing has been drawn since the current list item began.
    fresh_item: bool,
    last_blank: bool,
}

impl Drawer {
    fn new(width: usize, wide: usize, theme: Theme) -> Drawer {
        Drawer {
            width,
            wide,
            theme,
            out: Vec::new(),
            runs: Vec::new(),
            containers: Vec::new(),
            lists: Vec::new(),
            bold: 0,
            italic: 0,
            strike: 0,
            heading: false,
            done: false,
            link: None,
            code: None,
            table: None,
            fresh_item: false,
            last_blank: true,
        }
    }

    fn quoted(&self) -> bool {
        self.containers
            .iter()
            .any(|container| matches!(container, Container::Quote))
    }

    /// The style inline text takes here.
    fn style(&self) -> Style {
        let theme = self.theme;
        let mut style = if self.heading {
            theme.bright().add_modifier(Modifier::BOLD)
        } else if self.done {
            theme.faint()
        } else if self.quoted() {
            theme.muted()
        } else {
            theme.text()
        };
        if self.bold > 0 {
            style = style.add_modifier(Modifier::BOLD);
        }
        if self.italic > 0 {
            style = style.add_modifier(Modifier::ITALIC);
        }
        if self.strike > 0 {
            style = style.add_modifier(Modifier::CROSSED_OUT);
        }
        if self.link.is_some() {
            style = style.add_modifier(Modifier::UNDERLINED);
        }
        style
    }

    fn event(&mut self, event: Event<'_>) {
        if let Some((_, code)) = &mut self.code {
            match event {
                Event::Text(text) => {
                    code.push_str(&text);
                    return;
                }
                Event::End(TagEnd::CodeBlock) => {}
                _ => return,
            }
        }
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => self.text(&text),
            Event::Code(code) => {
                let mut style = self.theme.code();
                if self.link.is_some() {
                    style = style.add_modifier(Modifier::UNDERLINED);
                }
                self.runs.push(Run {
                    text: code.into_string(),
                    style,
                    link: self.link.clone(),
                });
            }
            // A single newline stays a line break, unlike CommonMark: agents
            // write lists, logs and "one per line" answers as lines, and
            // Claude's and Codex's own interfaces draw them so. A blank line
            // still parts paragraphs.
            Event::SoftBreak | Event::HardBreak => self.runs.push(Run {
                text: "\n".into(),
                style: Style::default(),
                link: None,
            }),
            Event::Html(html) | Event::InlineHtml(html) => self.text(&html),
            Event::Rule => {
                self.flush();
                self.gap();
                let (first, _) = self.prefixes();
                let room = self.width.saturating_sub(spans_width(&first)).max(1);
                let mut spans = first;
                spans.push(Span::styled("─".repeat(room), self.theme.hairline()));
                self.push_line(spans, Vec::new());
            }
            Event::TaskListMarker(done) => {
                self.done = done;
                if let Some(Container::Item { marker, width, .. }) = self.containers.last_mut() {
                    let mark = if done { "✓ " } else { "○ " };
                    *marker = vec![Span::styled(mark, self.theme.faint())];
                    *width = 2;
                }
            }
            Event::FootnoteReference(name) => self.text(&format!("[{name}]")),
            _ => {}
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => {
                if !self.fresh_item {
                    self.flush();
                    self.gap();
                }
            }
            Tag::Heading { .. } => {
                self.flush();
                self.gap();
                self.heading = true;
            }
            Tag::BlockQuote(_) => {
                self.flush();
                self.gap();
                self.containers.push(Container::Quote);
            }
            Tag::CodeBlock(kind) => {
                self.flush();
                if !self.fresh_item {
                    self.gap();
                }
                let language = match kind {
                    CodeBlockKind::Fenced(language) => language.into_string(),
                    CodeBlockKind::Indented => String::new(),
                };
                self.code = Some((language, String::new()));
            }
            Tag::List(first) => {
                self.flush();
                // A list starts a block; one nested in an item hangs under
                // its text without a gap.
                if !self
                    .containers
                    .iter()
                    .any(|container| matches!(container, Container::Item { .. }))
                {
                    self.gap();
                }
                self.lists.push(first);
            }
            Tag::Item => {
                self.flush();
                let words = match self.lists.last_mut() {
                    Some(Some(next)) => {
                        let words = format!("{next}. ");
                        *next += 1;
                        words
                    }
                    _ => "• ".to_owned(),
                };
                let ink = if self.quoted() {
                    self.theme.muted()
                } else {
                    self.theme.text()
                };
                self.containers.push(Container::Item {
                    width: str_width(&words),
                    marker: vec![Span::styled(words, ink)],
                    used: false,
                });
                self.fresh_item = true;
                self.done = false;
            }
            Tag::Table(alignments) => {
                self.flush();
                self.gap();
                self.table = Some(Table {
                    alignments,
                    rows: Vec::new(),
                    row: Vec::new(),
                });
            }
            Tag::TableHead | Tag::TableRow => {
                if let Some(table) = &mut self.table {
                    table.row.clear();
                }
            }
            Tag::TableCell => self.runs.clear(),
            Tag::Emphasis => self.italic += 1,
            Tag::Strong => self.bold += 1,
            Tag::Strikethrough => self.strike += 1,
            Tag::Link { dest_url, .. } | Tag::Image { dest_url, .. } => {
                self.link = Some(dest_url.into_string());
            }
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => self.flush(),
            TagEnd::Heading(_) => {
                self.flush();
                self.heading = false;
            }
            TagEnd::BlockQuote(_) => {
                self.flush();
                if matches!(self.containers.last(), Some(Container::Quote)) {
                    self.containers.pop();
                }
            }
            TagEnd::CodeBlock => {
                if let Some((language, code)) = self.code.take() {
                    self.code_block(&language, &code);
                }
            }
            TagEnd::List(_) => {
                self.flush();
                self.lists.pop();
            }
            TagEnd::Item => {
                self.flush();
                if matches!(self.containers.last(), Some(Container::Item { .. })) {
                    self.containers.pop();
                }
                self.done = false;
                self.fresh_item = false;
            }
            TagEnd::TableCell => {
                let cell = std::mem::take(&mut self.runs);
                if let Some(table) = &mut self.table {
                    table.row.push(cell);
                }
            }
            TagEnd::TableHead | TagEnd::TableRow => {
                if let Some(table) = &mut self.table {
                    let row = std::mem::take(&mut table.row);
                    table.rows.push(row);
                }
            }
            TagEnd::Table => {
                if let Some(table) = self.table.take() {
                    self.draw_table(table);
                }
            }
            TagEnd::Emphasis => self.italic = self.italic.saturating_sub(1),
            TagEnd::Strong => self.bold = self.bold.saturating_sub(1),
            TagEnd::Strikethrough => self.strike = self.strike.saturating_sub(1),
            TagEnd::Link | TagEnd::Image => self.link = None,
            _ => {}
        }
    }

    /// Inline text, with bare URLs underlined and linked to themselves.
    fn text(&mut self, text: &str) {
        let style = self.style();
        if self.link.is_some() {
            self.runs.push(Run {
                text: text.to_owned(),
                style,
                link: self.link.clone(),
            });
            return;
        }
        let mut rest = text;
        while let Some(at) = find_url(rest) {
            let (before, from) = rest.split_at(at);
            if !before.is_empty() {
                self.runs.push(Run {
                    text: before.to_owned(),
                    style,
                    link: None,
                });
            }
            let end = url_end(from);
            let url = &from[..end];
            self.runs.push(Run {
                text: url.to_owned(),
                style: style.add_modifier(Modifier::UNDERLINED),
                link: Some(url.to_owned()),
            });
            rest = &from[end..];
        }
        if !rest.is_empty() {
            self.runs.push(Run {
                text: rest.to_owned(),
                style,
                link: None,
            });
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
        self.last_blank = false;
        self.out.push(MdLine { spans, links });
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
            spans: rail,
            links: Vec::new(),
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
        let (first, rest) = self.prefixes();
        let room = self
            .width
            .saturating_sub(spans_width(&first).max(spans_width(&rest)))
            .max(1);
        for (i, pieces) in wrap(&runs, room).into_iter().enumerate() {
            let prefix = if i == 0 { &first } else { &rest };
            let (spans, links) = placed(prefix, &pieces);
            self.push_line(spans, links);
        }
        self.fresh_item = false;
    }

    /// A code block: no fences and no box, two columns in from the text,
    /// coloured by kind of token; long lines keep their spacing and wrap.
    fn code_block(&mut self, language: &str, code: &str) {
        let theme = self.theme;
        let lines: Vec<Vec<(String, Style)>> = match highlight::highlight(code, language) {
            Some(tokens) => tokens
                .iter()
                .map(|line| {
                    line.iter()
                        .map(|(text, kind)| (text.clone(), theme.syntax(*kind)))
                        .collect()
                })
                .collect(),
            None => code
                .lines()
                .map(|line| vec![(line.to_owned(), theme.text())])
                .collect(),
        };
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
                spans.push(indent.clone());
                spans.extend(row);
                self.push_line(spans, Vec::new());
            }
        }
        self.fresh_item = false;
    }

    /// A table: aligned columns under a bold header, with inner rules only:
    /// a hairline between columns, one under the header, and one under
    /// every row once any cell wraps. It may use the chat's full width;
    /// columns shrink by their content, the widest first and not below
    /// their longest word where that can be helped. When a column would
    /// drop below [`MIN_COLUMN`], each row is drawn as a record instead.
    fn draw_table(&mut self, table: Table) {
        let theme = self.theme;
        let (first, rest) = self.prefixes();
        let room = self.wide.saturating_sub(spans_width(&rest)).max(1);
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
        let aligns: Vec<Alignment> = (0..columns)
            .map(|column| {
                table
                    .alignments
                    .get(column)
                    .copied()
                    .unwrap_or(Alignment::None)
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
                links: Vec::new(),
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
                        Alignment::Right => (slack, 0),
                        Alignment::Center => (slack / 2, slack - slack / 2),
                        Alignment::Left | Alignment::None => (0, slack),
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
                lines.push(MdLine { spans, links });
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
    /// themselves, a hairline across the reading width between records.
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

/// Where the next bare URL in `text` starts.
fn find_url(text: &str) -> Option<usize> {
    ["https://", "http://"]
        .iter()
        .filter_map(|scheme| text.find(scheme))
        .min()
}

/// How far a bare URL at the start of `text` runs: to whitespace, less the
/// sentence's punctuation after it.
fn url_end(text: &str) -> usize {
    let end = text.find(char::is_whitespace).unwrap_or(text.len());
    let trimmed = text[..end].trim_end_matches(['.', ',', ';', ':', '!', '?', ')', '\'', '"']);
    trimmed.len()
}

/// Greedy word wrap over styled runs into lines of at most `width`
/// columns. A word is never split unless it is longer than a whole line
/// (a URL that fits any line is never broken); a hard break starts a line.
/// The space between two words of one link carries its underline.
fn wrap(runs: &[Run], width: usize) -> Vec<Vec<Run>> {
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
    let mut used = 0;
    let mut spaced = false;
    for token in tokens {
        match token {
            Token::Break => {
                lines.push(Vec::new());
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
                    }
                    lines.last_mut().expect("a line").extend(pieces);
                    used = needed;
                } else {
                    // Longer than a whole line: split it, the one case a
                    // word is broken.
                    if used > 0 {
                        lines.push(Vec::new());
                        used = 0;
                    }
                    for piece in pieces {
                        let mut rest = piece.text.as_str();
                        while !rest.is_empty() {
                            if used == width {
                                lines.push(Vec::new());
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
    }
    lines
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
