//! Agent text as markdown, parsed once for every client. A reply, a plan
//! and a prompt reach each client as [`Markdown`]: the source and the
//! blocks it parses to, so every client draws the same structure from the
//! same reading and none of them parses on its own thread at draw time.
//!
//! The reading is CommonMark with GitHub's tables, task lists,
//! strikethrough and bare web addresses as links, with one change: a
//! single newline stays a line break. Agents write lists, logs and "one
//! per line" answers as lines, and Claude's and Codex's own interfaces
//! draw them so; a blank line still parts paragraphs.
//!
//! Rows are built again whenever their item changes, and the terminal
//! builds the rows it draws on every frame, so a parse is kept by its
//! source: a reply is parsed once for each revision of its text.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::{Arc, LazyLock, Mutex};

use pulldown_cmark::{Alignment, CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
#[cfg(feature = "schema")]
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Text as written, and the blocks it reads as.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
pub struct Markdown {
    /// The text as the agent or person wrote it, for copying and for
    /// reading aloud.
    pub source: String,
    pub blocks: Vec<ProseBlock>,
}

/// One block of prose.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
pub enum ProseBlock {
    /// Level 1 to 6.
    Heading {
        level: u32,
        text: Vec<InlineRun>,
    },
    Paragraph {
        text: Vec<InlineRun>,
    },
    /// Numbered from `start` when `ordered`, else bulleted. `depth` is how
    /// many lists hold this one: 0 for a list of its own. A client sizes
    /// the marker column for the list's widest marker, so every item's
    /// text starts at one edge.
    List {
        ordered: bool,
        start: u64,
        depth: u32,
        items: Vec<ProseItem>,
    },
    Quote {
        blocks: Vec<ProseBlock>,
    },
    /// Each row holds a cell per column, as the header does; `alignments`
    /// has one entry per column.
    Table {
        alignments: Vec<TableAlign>,
        header: Vec<Vec<InlineRun>>,
        rows: Vec<Vec<Vec<InlineRun>>>,
    },
    Rule,
    /// Code, with the first word of its fence's info string as its
    /// language (empty when it names none, and for indented code). Its
    /// lines are parted by `\n`. `closed` is false for a fence the text
    /// ends inside: in a reply still streaming, the block still being
    /// written. That one keeps a last empty line after a final newline,
    /// which says the line before it is complete.
    Code {
        language: String,
        code: String,
        closed: bool,
    },
}

/// One list item: its blocks, the first drawn beside the marker; and for
/// a task list's item, whether its task is done.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
pub struct ProseItem {
    pub task: Option<bool>,
    pub blocks: Vec<ProseBlock>,
}

/// How a table column's cells sit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
pub enum TableAlign {
    /// As the column's text runs: the markdown named no alignment.
    None,
    Left,
    Center,
    Right,
}

/// A stretch of inline text that is all one style. A line break is a
/// `\n` in its text. Neighbouring runs differ in style.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
pub struct InlineRun {
    pub text: String,
    pub strong: bool,
    pub emphasis: bool,
    pub strikethrough: bool,
    /// Inline code.
    pub code: bool,
    /// The address the run links to, as written. Whether a client opens
    /// it is the client's to decide.
    pub link: Option<String>,
}

/// Its text, after its styles when it has any (`strong code "x"`), so a
/// view's debug form, which the goldens record, stays readable.
impl std::fmt::Debug for InlineRun {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (on, name) in [
            (self.strong, "strong"),
            (self.emphasis, "emphasis"),
            (self.strikethrough, "strikethrough"),
            (self.code, "code"),
        ] {
            if on {
                write!(f, "{name} ")?;
            }
        }
        if let Some(link) = &self.link {
            write!(f, "link({link:?}) ")?;
        }
        self.text.fmt(f)
    }
}

impl InlineRun {
    /// `text` in no style at all.
    pub fn plain(text: impl Into<String>) -> InlineRun {
        InlineRun {
            text: text.into(),
            ..InlineRun::default()
        }
    }

    fn same_style(&self, other: &InlineRun) -> bool {
        self.strong == other.strong
            && self.emphasis == other.emphasis
            && self.strikethrough == other.strikethrough
            && self.code == other.code
            && self.link == other.link
    }
}

/// The source alone: a row's debug form, which the views' goldens record,
/// reads as its words, and the blocks are the parser's, tested here.
impl std::fmt::Debug for Markdown {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.source.fmt(f)
    }
}

impl Markdown {
    /// `source` parsed. `streaming` is for text still being written: a
    /// last line that could still become a table's header waits until the
    /// line under it says whether it does, so a table does not first
    /// appear as a paragraph of pipes.
    pub fn parse(source: &str, streaming: bool) -> Markdown {
        Markdown {
            source: source.to_owned(),
            blocks: kept(source, streaming).as_ref().clone(),
        }
    }

    /// The text of every block, without its markup, one block to a line.
    pub fn plain_text(&self) -> String {
        let mut out = Vec::new();
        for block in &self.blocks {
            block_text(block, &mut out);
        }
        out.join("\n")
    }
}

/// A one-line label an agent wrote, such as a question or an option, read
/// for its inline markdown alone: `1. Rust` and `# 42` stay as written
/// rather than becoming a list or a heading.
pub fn inline_runs(source: &str) -> Vec<InlineRun> {
    if source.is_empty() {
        return Vec::new();
    }
    let escaped: Vec<String> = normalized(source)
        .split('\n')
        .map(escape_block_start)
        .collect();
    let mut runs = Vec::new();
    for block in parse(&escaped.join("\n")) {
        if let ProseBlock::Paragraph { text } = block {
            if !runs.is_empty() {
                runs.push(InlineRun::plain("\n"));
            }
            runs.extend(text);
        }
    }
    coalesce(runs)
}

/// The text of inline runs, without their markup.
pub fn runs_text(runs: &[InlineRun]) -> String {
    runs.iter().map(|run| run.text.as_str()).collect()
}

fn block_text(block: &ProseBlock, out: &mut Vec<String>) {
    match block {
        ProseBlock::Heading { text, .. } | ProseBlock::Paragraph { text } => {
            out.push(runs_text(text));
        }
        ProseBlock::List { items, .. } => {
            for item in items {
                for block in &item.blocks {
                    block_text(block, out);
                }
            }
        }
        ProseBlock::Quote { blocks } => {
            for block in blocks {
                block_text(block, out);
            }
        }
        ProseBlock::Table { header, rows, .. } => {
            for row in std::iter::once(header).chain(rows) {
                out.push(
                    row.iter()
                        .map(|cell| runs_text(cell))
                        .collect::<Vec<_>>()
                        .join("\t"),
                );
            }
        }
        ProseBlock::Rule => {}
        ProseBlock::Code { code, .. } => out.push(code.clone()),
    }
}

/// A line's first character escaped when it would open a block, so the
/// line can only be a paragraph's.
fn escape_block_start(line: &str) -> String {
    let line = line.trim_start_matches([' ', '\t']);
    let Some(first) = line.chars().next() else {
        return String::new();
    };
    let after = line[first.len_utf8()..].chars().next();
    let opens = match first {
        // A heading, a quote, a setext underline or a table: escaping
        // these changes no inline reading.
        '#' | '>' | '=' | '|' => true,
        // A bullet, or a rule; `**strong**` and `_em_` are neither.
        '-' | '+' | '*' | '_' => {
            matches!(after, None | Some(' ' | '\t'))
                || line.chars().all(|c| c == first || c == ' ' || c == '\t')
        }
        // A fence; a code span opens with fewer.
        '`' | '~' => line.starts_with(&first.to_string().repeat(3)),
        _ => false,
    };
    if opens {
        return format!("\\{line}");
    }
    let digits = line.chars().take_while(char::is_ascii_digit).count();
    if digits > 0 && matches!(line[digits..].chars().next(), Some('.' | ')')) {
        return format!("{}\\{}", &line[..digits], &line[digits..]);
    }
    line.to_owned()
}

/// Parses kept, newest first, by source: two generations, so a source read
/// within the last few hundred parses is found again, and the oldest go a
/// generation at a time.
struct Kept {
    now: HashMap<(String, bool), Arc<Vec<ProseBlock>>>,
    before: HashMap<(String, bool), Arc<Vec<ProseBlock>>>,
}

const KEPT_PER_GENERATION: usize = 512;

static KEPT: LazyLock<Mutex<Kept>> = LazyLock::new(|| {
    Mutex::new(Kept {
        now: HashMap::new(),
        before: HashMap::new(),
    })
});

fn kept(source: &str, streaming: bool) -> Arc<Vec<ProseBlock>> {
    let key = (source.to_owned(), streaming);
    {
        let mut kept = KEPT.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(blocks) = kept.now.get(&key) {
            return blocks.clone();
        }
        if let Some(blocks) = kept.before.remove(&key) {
            kept.now.insert(key, blocks.clone());
            return blocks;
        }
    }
    let text = normalized(source);
    let text = if streaming {
        without_unsettled_header(&text)
    } else {
        &text
    };
    let blocks = Arc::new(parse(text));
    let mut kept = KEPT.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if kept.now.len() >= KEPT_PER_GENERATION {
        kept.before = std::mem::take(&mut kept.now);
    }
    kept.now.insert(key, blocks.clone());
    blocks
}

/// The source with `\r\n` and a lone `\r` read as the `\n` they end a line
/// with, so no `\r` reaches a heading, a cell or a code block's lines.
fn normalized(source: &str) -> String {
    if source.contains('\r') {
        source.replace("\r\n", "\n").replace('\r', "\n")
    } else {
        source.to_owned()
    }
}

/// Text still streaming, less a last line of pipes that no table holds
/// yet: until the delimiter row under it arrives, it would read as a
/// paragraph.
fn without_unsettled_header(source: &str) -> &str {
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

fn options() -> Options {
    Options::ENABLE_TABLES | Options::ENABLE_TASKLISTS | Options::ENABLE_STRIKETHROUGH
}

fn parse(source: &str) -> Vec<ProseBlock> {
    let mut builder = Builder::default();
    let mut fences = Fences::new(source);
    for (event, range) in Parser::new_ext(source, options()).into_offset_iter() {
        if let Some(unclosed) = fences.see(&event, range) {
            builder.unclosed = unclosed;
        }
        builder.event(event);
    }
    builder.finish()
}

/// Follows the parser's events for the fenced code block the source leaves
/// unclosed: in text still streaming, the block still being written.
///
/// The parser has already decided which lines are code (how far a closing
/// fence may be indented, that it must be as long as the opening, that a
/// quote's marker is not part of it), so the only thing read here is what
/// follows the block's last code: a block is closed when that holds a run
/// of its opening's fence character at least as long as the opening. Only
/// a closing fence can put one there. Comparing where the code ends with
/// where the block ends is not enough: the parser counts a trailing line
/// of indentation as the block's but emits it as no code.
///
/// An unclosed block is the one being written only when it runs to the
/// end of the text. A block a quote or list item ends before the text does
/// is finished; nothing more will join it.
struct Fences<'a> {
    source: &'a str,
    /// Where the text ends, less trailing whitespace.
    end: usize,
    /// The fenced block open now: its opening fence, where the block ends,
    /// and where its code ends.
    block: Option<(&'a str, usize, usize)>,
}

impl<'a> Fences<'a> {
    fn new(source: &'a str) -> Fences<'a> {
        Fences {
            source,
            end: source.trim_end().len(),
            block: None,
        }
    }

    /// Takes the next event; at a fenced block's end, says whether the
    /// block is unclosed.
    fn see(&mut self, event: &Event, range: Range<usize>) -> Option<bool> {
        match event {
            // Before any code, the block's code ends with its opening line.
            Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(_))) => {
                let text = &self.source[range.clone()];
                let opening = text.find('\n').map_or(range.end, |at| range.start + at + 1);
                let text = text.trim_start();
                let mark = text.chars().next().unwrap_or('`');
                let fence = &text[..text.len() - text.trim_start_matches(mark).len()];
                self.block = Some((fence, range.end, opening));
                None
            }
            Event::Text(_) => {
                if let Some((_, _, code)) = &mut self.block {
                    *code = range.end;
                }
                None
            }
            Event::End(TagEnd::CodeBlock) => {
                let (fence, block, code) = self.block.take()?;
                let closed = self.source[code..block].contains(fence);
                Some(!closed && block >= self.end)
            }
            _ => None,
        }
    }
}

/// A block that holds blocks, open while its events arrive.
enum Open {
    Quote(Vec<ProseBlock>),
    List {
        ordered: bool,
        start: u64,
        depth: u32,
        items: Vec<ProseItem>,
    },
    Item {
        task: Option<bool>,
        blocks: Vec<ProseBlock>,
    },
}

#[derive(Default)]
struct Table {
    alignments: Vec<TableAlign>,
    header: Vec<Vec<InlineRun>>,
    rows: Vec<Vec<Vec<InlineRun>>>,
    row: Vec<Vec<InlineRun>>,
}

#[derive(Default)]
struct Builder {
    out: Vec<ProseBlock>,
    open: Vec<Open>,
    /// Inline text gathered for the paragraph, heading or cell under way.
    runs: Vec<InlineRun>,
    strong: usize,
    emphasis: usize,
    strikethrough: usize,
    link: Option<String>,
    heading: Option<u32>,
    /// The code block being gathered: its language and its code.
    code: Option<(String, String, bool)>,
    /// The code block ending next is unclosed.
    unclosed: bool,
    table: Option<Table>,
}

impl Builder {
    fn push_block(&mut self, block: ProseBlock) {
        match self.open.last_mut() {
            Some(Open::Quote(blocks) | Open::Item { blocks, .. }) => blocks.push(block),
            // A list holds only items; anything else ends up in an item.
            Some(Open::List { .. }) | None => self.out.push(block),
        }
    }

    /// The inline text gathered so far as a paragraph: a tight list item's
    /// words, which come without a paragraph of their own, or a stray run
    /// such as an HTML block's.
    fn flush(&mut self) {
        let runs = coalesce(std::mem::take(&mut self.runs));
        if runs.iter().all(|run| run.text.trim().is_empty()) {
            return;
        }
        self.push_block(ProseBlock::Paragraph { text: runs });
    }

    fn run(&mut self, text: &str, code: bool, link: Option<String>) {
        if text.is_empty() {
            return;
        }
        self.runs.push(InlineRun {
            text: text.to_owned(),
            strong: self.strong > 0,
            emphasis: self.emphasis > 0,
            strikethrough: self.strikethrough > 0,
            code,
            link,
        });
    }

    /// Inline text, with bare web addresses linked to themselves.
    fn text(&mut self, text: &str) {
        if self.link.is_some() {
            self.run(text, false, self.link.clone());
            return;
        }
        let mut rest = text;
        while let Some(at) = find_url(rest) {
            let (before, from) = rest.split_at(at);
            self.run(before, false, None);
            let end = url_end(from);
            let url = &from[..end];
            self.run(url, false, Some(url.to_owned()));
            rest = &from[end..];
        }
        self.run(rest, false, None);
    }

    fn event(&mut self, event: Event<'_>) {
        if let Some((_, code, _)) = &mut self.code {
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
            Event::Code(code) => self.run(&code, true, self.link.clone()),
            Event::SoftBreak | Event::HardBreak => self.runs.push(InlineRun::plain("\n")),
            // Markup amux does not draw is kept as its words.
            Event::Html(html) | Event::InlineHtml(html) => self.text(&html),
            Event::Rule => {
                self.flush();
                self.push_block(ProseBlock::Rule);
            }
            Event::TaskListMarker(done) => {
                if let Some(Open::Item { task, .. }) = self.open.last_mut() {
                    *task = Some(done);
                }
            }
            Event::FootnoteReference(name) => self.text(&format!("[{name}]")),
            Event::InlineMath(text) | Event::DisplayMath(text) => self.run(&text, true, None),
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph | Tag::HtmlBlock => self.flush(),
            Tag::Heading { level, .. } => {
                self.flush();
                self.heading = Some(level as u32);
            }
            Tag::BlockQuote(_) => {
                self.flush();
                self.open.push(Open::Quote(Vec::new()));
            }
            Tag::CodeBlock(kind) => {
                self.flush();
                let language = match kind {
                    CodeBlockKind::Fenced(info) => {
                        info.split_whitespace().next().unwrap_or("").to_owned()
                    }
                    CodeBlockKind::Indented => String::new(),
                };
                self.code = Some((language, String::new(), false));
            }
            Tag::List(first) => {
                self.flush();
                let depth = self
                    .open
                    .iter()
                    .filter(|open| matches!(open, Open::List { .. }))
                    .count() as u32;
                self.open.push(Open::List {
                    ordered: first.is_some(),
                    start: first.unwrap_or(1),
                    depth,
                    items: Vec::new(),
                });
            }
            Tag::Item => {
                self.flush();
                self.open.push(Open::Item {
                    task: None,
                    blocks: Vec::new(),
                });
            }
            Tag::Table(alignments) => {
                self.flush();
                self.table = Some(Table {
                    alignments: alignments
                        .into_iter()
                        .map(|alignment| match alignment {
                            Alignment::None => TableAlign::None,
                            Alignment::Left => TableAlign::Left,
                            Alignment::Center => TableAlign::Center,
                            Alignment::Right => TableAlign::Right,
                        })
                        .collect(),
                    ..Table::default()
                });
            }
            Tag::TableHead | Tag::TableRow => {
                if let Some(table) = &mut self.table {
                    table.row.clear();
                }
            }
            Tag::TableCell => self.runs.clear(),
            Tag::Emphasis => self.emphasis += 1,
            Tag::Strong => self.strong += 1,
            Tag::Strikethrough => self.strikethrough += 1,
            // An image is drawn as its words, linking to it.
            Tag::Link { dest_url, .. } | Tag::Image { dest_url, .. } => {
                self.link = Some(dest_url.into_string());
            }
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph | TagEnd::HtmlBlock => self.flush(),
            TagEnd::Heading(_) => {
                let runs = coalesce(std::mem::take(&mut self.runs));
                let level = self.heading.take().unwrap_or(1);
                self.push_block(ProseBlock::Heading { level, text: runs });
            }
            TagEnd::BlockQuote(_) => {
                self.flush();
                if let Some(Open::Quote(blocks)) = self.open.pop() {
                    self.push_block(ProseBlock::Quote { blocks });
                }
            }
            TagEnd::CodeBlock => {
                let unclosed = std::mem::take(&mut self.unclosed);
                if let Some((language, mut code, _)) = self.code.take() {
                    // A finished block's last line ends with the newline
                    // before its closing fence, which is no line of its own.
                    if !unclosed && code.ends_with('\n') {
                        code.pop();
                    }
                    self.push_block(ProseBlock::Code {
                        language,
                        code,
                        closed: !unclosed,
                    });
                }
            }
            TagEnd::List(_) => {
                self.flush();
                if let Some(Open::List {
                    ordered,
                    start,
                    depth,
                    items,
                }) = self.open.pop()
                {
                    self.push_block(ProseBlock::List {
                        ordered,
                        start,
                        depth,
                        items,
                    });
                }
            }
            TagEnd::Item => {
                self.flush();
                if let Some(Open::Item { task, blocks }) = self.open.pop()
                    && let Some(Open::List { items, .. }) = self.open.last_mut()
                {
                    items.push(ProseItem { task, blocks });
                }
            }
            TagEnd::TableCell => {
                let cell = coalesce(std::mem::take(&mut self.runs));
                if let Some(table) = &mut self.table {
                    table.row.push(cell);
                }
            }
            TagEnd::TableHead => {
                if let Some(table) = &mut self.table {
                    table.header = std::mem::take(&mut table.row);
                }
            }
            TagEnd::TableRow => {
                if let Some(table) = &mut self.table {
                    let row = std::mem::take(&mut table.row);
                    table.rows.push(row);
                }
            }
            TagEnd::Table => {
                if let Some(table) = self.table.take() {
                    self.push_block(ProseBlock::Table {
                        alignments: table.alignments,
                        header: table.header,
                        rows: table.rows,
                    });
                }
            }
            TagEnd::Emphasis => self.emphasis = self.emphasis.saturating_sub(1),
            TagEnd::Strong => self.strong = self.strong.saturating_sub(1),
            TagEnd::Strikethrough => self.strikethrough = self.strikethrough.saturating_sub(1),
            TagEnd::Link | TagEnd::Image => self.link = None,
            _ => {}
        }
    }

    fn finish(mut self) -> Vec<ProseBlock> {
        self.flush();
        self.out
    }
}

/// Neighbouring runs of one style as one run, and no empty runs.
fn coalesce(runs: Vec<InlineRun>) -> Vec<InlineRun> {
    let mut out: Vec<InlineRun> = Vec::with_capacity(runs.len());
    for run in runs {
        if run.text.is_empty() {
            continue;
        }
        match out.last_mut() {
            Some(last) if last.same_style(&run) => last.text.push_str(&run.text),
            _ => out.push(run),
        }
    }
    out
}

/// Where the next bare web address in `text` starts.
fn find_url(text: &str) -> Option<usize> {
    ["https://", "http://"]
        .iter()
        .filter_map(|scheme| text.find(scheme))
        .min()
}

/// How far a bare web address at the start of `text` runs: to whitespace,
/// less the sentence's punctuation after it.
fn url_end(text: &str) -> usize {
    let end = text.find(char::is_whitespace).unwrap_or(text.len());
    let trimmed = text[..end].trim_end_matches(['.', ',', ';', ':', '!', '?', ')', '\'', '"']);
    trimmed.len()
}

#[cfg(test)]
mod tests;
