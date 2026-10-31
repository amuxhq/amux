//! Immediate-mode layout: each frame lays rows out from the anchor at the
//! known width until the feed is full, so heights are exact before
//! anything is painted and the cost is the rows on screen, not the window.
//!
//! Rows come from ui-view one item at a time, each carrying its place in
//! its run of tool steps and, from the chat's tool-step choice, whether it
//! shows at all. Of the steps that show, layout picks the drawing: open,
//! every step under a header; under way, its newest few; folded, one line
//! on its newest step and any failure its turn left unresolved.

use std::collections::HashSet;

use ratatui::text::Line;
use ui_state::{Fold, ItemClass, Key, SessionState};
use ui_view::{LIVE_STEPS, Row, RunCounts, ToolRows, chat_rows_for};

use super::feed::{self, Header, LineHits, Placement};
use super::rows::{RowFacts, RowState, row_lines};
use crate::theme::Theme;

/// How a chat draws its runs of tool steps: the person's choice, kept for
/// each chat.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolSteps {
    /// Each run folds to one line on its newest step; under way, its
    /// newest few steps show.
    #[default]
    Collapse,
    /// Every step on its own line.
    ShowAll,
    /// No steps but a failure its turn left unresolved.
    Hide,
}

impl ToolSteps {
    /// The choice as the shared view takes it, with the runs the reader
    /// opened.
    pub fn rows(self, open: &HashSet<Key>) -> ToolRows<'_> {
        match self {
            ToolSteps::Collapse => ToolRows::Collapse { open },
            ToolSteps::ShowAll => ToolRows::ShowAll,
            ToolSteps::Hide => ToolRows::Hide,
        }
    }

    /// The next choice, round from collapse through show all and hide.
    pub fn next(self) -> ToolSteps {
        match self {
            ToolSteps::Collapse => ToolSteps::ShowAll,
            ToolSteps::ShowAll => ToolSteps::Hide,
            ToolSteps::Hide => ToolSteps::Collapse,
        }
    }

    pub fn words(self) -> &'static str {
        match self {
            ToolSteps::Collapse => "collapse",
            ToolSteps::ShowAll => "show all",
            ToolSteps::Hide => "hide",
        }
    }
}

/// What a block's toggle key opens or closes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Toggle {
    /// The row itself.
    Row,
    /// A run, by one of its steps.
    Run(Key),
    /// A step's detail.
    Step(Key),
}

/// Rows fetched per page, and the tail a chat opens with.
pub const PAGE: u32 = 40;
/// The most rows a chat's window keeps while the reader follows the newest.
/// While following, the layout pages only to keep a page of rows above the
/// screen, and the window must still hold a full screen and that page after
/// trimming, or each live row would trim what the next page fetches again:
/// rows draw at least one line, and 160 lines is taller than a terminal in
/// use, so a screen and the 40-row lookahead fit in 200.
pub const CAP: u32 = 200;
/// Fewer held rows than this above the screen asks for an older page.
pub const LOOKAHEAD: u64 = PAGE as u64;
/// The largest page asked for at an open collapsed run.
pub const RUN_PAGE_CAP: u32 = 1_000;

/// Where the reader is.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Anchor {
    /// Following the newest row.
    #[default]
    Bottom,
    /// This row at the top of the feed, with `offset` of its lines above
    /// the screen.
    Top { key: Key, offset: usize },
}

/// One drawn row: its key and its lines.
#[derive(Clone, Debug)]
pub struct Block {
    pub key: Key,
    pub order: u64,
    pub row: Row,
    pub lines: Vec<Line<'static>>,
    /// What clicking along each line does, one entry per line.
    pub hits: Vec<LineHits>,
    pub toggle: Toggle,
}

/// What a frame's layout produced.
#[derive(Clone, Debug, Default)]
pub struct Laid {
    /// Exactly the feed's lines, top first; blank lines pad a short chat.
    pub lines: Vec<Line<'static>>,
    /// What clicking along each of `lines` does.
    pub hits: Vec<LineHits>,
    /// The drawn rows, top first, with how many of the first one's lines
    /// sit above the screen.
    pub blocks: Vec<Block>,
    pub top_offset: usize,
    /// The feed shows the newest row at its bottom.
    pub at_bottom: bool,
    /// An older page to ask for, and its size, when the reader is within a
    /// page of the oldest held row and older history exists.
    pub page: Option<u32>,
}

/// The view state layout reads.
pub struct Frame<'a> {
    pub anchor: &'a Anchor,
    pub expanded: &'a HashSet<Key>,
    pub focus: Option<&'a Key>,
    pub width: usize,
    pub height: usize,
    pub theme: Theme,
    pub leader: char,
    /// Runs the reader opened, each held by one of its steps (see
    /// `ui_view::run_is_open`): folding, they show every step.
    pub open_runs: &'a HashSet<Key>,
    pub tools: ToolSteps,
    /// The step an ask in the composer's box points at: the box shows it,
    /// so the feed does not draw it twice.
    pub asking: Option<&'a Key>,
    /// Lines after the newest row, drawn by the caller and scrolled with
    /// the rest: the running turn's live end (your prompt on its way, the
    /// activity line).
    pub tail: &'a [Line<'static>],
}

impl Frame<'_> {
    fn options(&self) -> ToolRows<'_> {
        self.tools.rows(self.open_runs)
    }

    /// The row drawn at one held order, as shown, with whether its subagent
    /// parent is open; None when it draws nothing here.
    fn shown(&self, state: &SessionState, order: u64) -> Option<(Row, bool)> {
        let held = state.transcript().at(order)?;
        let row =
            chat_rows_for(state, std::slice::from_ref(&held.item.key), self.options()).pop()?;
        let child_open = row
            .parent
            .as_ref()
            .is_some_and(|parent| self.expanded.contains(parent));
        let mut shown = row;
        if child_open {
            shown.collapsed = false;
        }
        if shown.collapsed || matches!(shown.kind, ui_view::RowKind::Hidden) {
            return None;
        }
        Some((shown, child_open))
    }

    /// A row drawn on its own, outside the feed's turns: asks closed long
    /// ago, errors, boundaries and the other kinds the feed leaves as rows.
    fn row_block(&self, state: &SessionState, order: u64) -> Option<Block> {
        let (shown, child_open) = self.shown(state, order)?;
        let expanded = self.expanded.contains(&shown.id);
        let row_state = RowState {
            focused: self.focus == Some(&shown.id),
            expanded,
        };
        let mut lines = row_lines(&shown, row_state, self.width, self.theme);
        if child_open {
            for line in &mut lines {
                line.spans.insert(0, "  ".into());
            }
        }
        if lines.is_empty() {
            return None;
        }
        let hits = vec![Vec::new(); lines.len()];
        Some(Block {
            key: shown.id.clone(),
            order,
            row: shown,
            lines,
            hits,
            toggle: Toggle::Row,
        })
    }

    /// The row for one held order, or None when it draws nothing here.
    /// Inside a run, what draws depends on whether the run is open, under
    /// way or folded; outside one, text and turn ends draw from [`feed`],
    /// and the rest keeps its own drawing.
    fn block(&self, state: &SessionState, order: u64) -> Option<Block> {
        let transcript = state.transcript();
        let held = transcript.at(order)?;
        if self.asking == Some(&held.item.key) {
            return None;
        }
        let in_run = chat_rows_for(state, std::slice::from_ref(&held.item.key), self.options())
            .pop()
            .filter(|row| row.run.is_some());
        let (placement, row, toggle) = match in_run {
            Some(row) => {
                // Inside a run only its steps draw, and of them only those
                // the view shows.
                if held.class.fold() != Fold::Step
                    || row.collapsed
                    || matches!(row.kind, ui_view::RowKind::Hidden)
                {
                    return None;
                }
                let run = row.run.clone()?;
                // The run's last step drawn: its newest, unless that is the
                // one the composer's box is asking about.
                let last =
                    run.is_last() || (run.recent == Some(1) && self.asking == Some(&run.last));
                let running =
                    matches!(&held.class, ItemClass::Tool(tool) if tool.in_flight) && run.is_last();
                let opened = self.tools == ToolSteps::Collapse
                    && ui_view::run_is_open(state, order, self.open_runs);
                if opened || self.tools == ToolSteps::ShowAll {
                    // Opened, a run lists every step on its own line, under
                    // the line that folds it again.
                    let header = (opened && row.id == run.first).then(|| Header {
                        counts: Some(self.counts(state, &run.last)),
                        run: run.clone(),
                        earlier: 0,
                    });
                    let placement = Placement::Step {
                        header,
                        joined: !last,
                        // Opened while under way, the step running now is
                        // still the bright one, as it is folded.
                        current: run.live && running,
                    };
                    let toggle = Toggle::Step(row.id.clone());
                    (placement, row, toggle)
                } else if self.tools == ToolSteps::Hide {
                    let toggle = Toggle::Step(row.id.clone());
                    (Placement::Failed { blank: true }, row, toggle)
                } else if run.live {
                    // Under way: the newest few steps, the newest bright
                    // while it runs.
                    let earlier = run.steps.saturating_sub(LIVE_STEPS);
                    let first = run.recent == Some(run.steps.min(LIVE_STEPS) - 1);
                    let placement = Placement::Step {
                        header: (first && earlier > 0).then(|| Header {
                            run: run.clone(),
                            counts: None,
                            earlier,
                        }),
                        joined: !last,
                        current: running,
                    };
                    let toggle = Toggle::Step(row.id.clone());
                    (placement, row, toggle)
                } else if run.is_last() {
                    let toggle = Toggle::Run(run.last.clone());
                    (Placement::Folded { run }, row, toggle)
                } else {
                    // Folded, the other steps that show are failures their
                    // turn left unresolved.
                    let toggle = Toggle::Step(row.id.clone());
                    (Placement::Failed { blank: false }, row, toggle)
                }
            }
            None => {
                let (row, _) = self.shown(state, order)?;
                if !matches!(
                    row.kind,
                    ui_view::RowKind::Prompt { .. }
                        | ui_view::RowKind::Prose { .. }
                        | ui_view::RowKind::Thinking { .. }
                        | ui_view::RowKind::TurnEnd { .. }
                        | ui_view::RowKind::Stopped
                        | ui_view::RowKind::Ask(_)
                ) && !feed::is_step(&row)
                {
                    // Errors, boundaries and the rest draw as rows.
                    return self.row_block(state, order);
                }
                let toggle = Toggle::Step(row.id.clone());
                (Placement::Plain, row, toggle)
            }
        };
        let expanded = self.expanded.contains(&row.id);
        let mut facts = RowFacts {
            leader: self.leader,
            ..RowFacts::default()
        };
        if expanded && matches!(row.kind, ui_view::RowKind::FileChange { .. }) {
            // Opened, an edit shows its whole patch.
            facts.patch = ui_view::patch_head(state, &row.id, usize::MAX);
        }
        let drawn = feed::row_lines(&row, &placement, expanded, &facts, self.width, self.theme)?;
        let mut lines = drawn.lines;
        if self.focus == Some(&row.id) {
            for line in &mut lines {
                line.spans
                    .insert(0, ratatui::text::Span::styled("▌", self.theme.focus_bar()));
                if let Some(second) = line.spans.get_mut(1)
                    && second.content.starts_with(' ')
                {
                    second.content = second.content[1..].to_owned().into();
                }
            }
        }
        Some(Block {
            key: row.id.clone(),
            order,
            row,
            lines,
            hits: drawn.hits,
            toggle,
        })
    }

    /// Held rows that would draw below `top`: every held order above the
    /// oldest, less the members of folded runs that do not draw.
    fn held_above(&self, state: &SessionState, top: u64) -> u64 {
        let transcript = state.transcript();
        let Some(oldest) = transcript.oldest_held() else {
            return 0;
        };
        let mut count = top.saturating_sub(oldest);
        for span in transcript.runs().spans() {
            if *span.start() >= top {
                break;
            }
            if self.tools == ToolSteps::ShowAll
                || (self.tools == ToolSteps::Collapse
                    && ui_view::run_is_open(state, *span.start(), self.open_runs))
            {
                continue;
            }
            let end = (*span.end()).min(top.saturating_sub(1));
            // A run's members above its newest step do not draw; hidden,
            // its newest does not either.
            let hidden = if *span.end() < top && self.tools == ToolSteps::Collapse {
                span.end() - span.start()
            } else {
                end + 1 - span.start()
            };
            count = count.saturating_sub(hidden);
        }
        count
    }

    /// What the run whose newest step is `last` did.
    fn counts(&self, state: &SessionState, last: &Key) -> RunCounts {
        chat_rows_for(state, std::slice::from_ref(last), ToolRows::ShowAll)
            .pop()
            .and_then(|row| row.run)
            .and_then(|run| run.counts)
            .unwrap_or_default()
    }

    pub fn layout(&self, state: &SessionState) -> Laid {
        let transcript = state.transcript();
        let (Some(oldest), Some(head)) = (transcript.oldest_held(), transcript.head()) else {
            return Laid {
                lines: vec![Line::default(); self.height],
                hits: vec![Vec::new(); self.height],
                at_bottom: true,
                page: transcript.has_older().then_some(PAGE),
                ..Laid::default()
            };
        };
        let anchored = match self.anchor {
            Anchor::Bottom => None,
            Anchor::Top { key, offset } => {
                transcript.get(key).map(|held| (held.item.order, *offset))
            }
        };
        let mut laid = match anchored {
            Some((order, offset)) => self
                .downward(state, order, offset)
                .unwrap_or_else(|| self.upward(state, head)),
            None => self.upward(state, head),
        };
        let top = laid.blocks.first().map_or(oldest, |block| block.order);
        if self.held_above(state, top) < LOOKAHEAD && transcript.has_older() {
            let run_page = laid
                .blocks
                .first()
                .and_then(|block| block.row.run.as_ref())
                .filter(|run| run.open_below)
                .map(|run| run.steps.clamp(PAGE, RUN_PAGE_CAP));
            // Following, a page brings only what fits under the cap, so the
            // next live row never trims what it fetched.
            let room = state
                .page_room()
                .map_or(u32::MAX, |room| u32::try_from(room).unwrap_or(u32::MAX));
            let size = run_page.unwrap_or(PAGE).min(room);
            laid.page = (size > 0).then_some(size);
        }
        laid
    }

    /// Fills the feed upward from `from`, newest at the bottom.
    fn upward(&self, state: &SessionState, from: u64) -> Laid {
        let mut blocks = Vec::new();
        let mut total = self.tail.len();
        let mut order = Some(from);
        while let Some(at) = order {
            if total >= self.height {
                break;
            }
            if let Some(block) = self.block(state, at) {
                total += block.lines.len();
                blocks.push(block);
            }
            order = before(state, at).next_back().map(|held| held.item.order);
        }
        blocks.reverse();
        let top_offset = total.saturating_sub(self.height);
        let mut lines: Vec<Line<'static>> = blocks
            .iter()
            .flat_map(|block| block.lines.iter().cloned())
            .chain(self.tail.iter().cloned())
            .skip(top_offset)
            .collect();
        let mut hits: Vec<LineHits> = blocks
            .iter()
            .flat_map(|block| block.hits.iter().cloned())
            .chain(self.tail.iter().map(|_| Vec::new()))
            .skip(top_offset)
            .collect();
        if lines.len() < self.height {
            let pad = self.height - lines.len();
            let mut padded = vec![Line::default(); pad];
            padded.append(&mut lines);
            lines = padded;
            let mut padded = vec![Vec::new(); pad];
            padded.append(&mut hits);
            hits = padded;
        }
        Laid {
            lines,
            hits,
            blocks,
            top_offset,
            at_bottom: true,
            ..Laid::default()
        }
    }

    /// Fills the feed downward from the anchor row; None when what lies
    /// below it does not fill the feed, so the reader is at the bottom.
    fn downward(&self, state: &SessionState, from: u64, offset: usize) -> Option<Laid> {
        let transcript = state.transcript();
        let head = transcript.head()?;
        let mut blocks = Vec::new();
        let mut total = 0usize;
        let mut reached_head = true;
        for held in transcript.range(from..=head) {
            if let Some(block) = self.block(state, held.item.order) {
                total += block.lines.len();
                blocks.push(block);
            }
            if total >= offset + self.height {
                reached_head = held.item.order == head;
                break;
            }
        }
        let offset = offset.min(
            blocks
                .first()
                .map_or(0, |b| b.lines.len().saturating_sub(1)),
        );
        // The live end follows the newest row.
        let tail = if reached_head { self.tail } else { &[] };
        let total = total + tail.len();
        // What lies below fits exactly: that is the bottom too.
        if total < offset + self.height || (reached_head && total == offset + self.height) {
            return None;
        }
        let lines = blocks
            .iter()
            .flat_map(|block| block.lines.iter().cloned())
            .chain(tail.iter().cloned())
            .skip(offset)
            .take(self.height)
            .collect();
        let hits = blocks
            .iter()
            .flat_map(|block| block.hits.iter().cloned())
            .chain(tail.iter().map(|_| Vec::new()))
            .skip(offset)
            .take(self.height)
            .collect();
        Some(Laid {
            lines,
            hits,
            blocks,
            top_offset: offset,
            at_bottom: false,
            ..Laid::default()
        })
    }

    /// The anchor `lines` above (negative) or below the top of `laid`;
    /// Bottom once it would pass the newest row.
    pub fn scrolled(&self, state: &SessionState, laid: &Laid, delta: isize) -> Anchor {
        let transcript = state.transcript();
        let Some(first) = laid.blocks.first() else {
            return Anchor::Bottom;
        };
        let mut order = first.order;
        let mut offset = laid.top_offset as isize + delta;
        let mut height = first.lines.len() as isize;
        while offset < 0 {
            let above = before(state, order)
                .rev()
                .find_map(|held| self.block(state, held.item.order));
            let Some(block) = above else {
                offset = 0;
                break;
            };
            order = block.order;
            height = block.lines.len() as isize;
            offset += height;
        }
        while offset >= height {
            let below = after(state, order).find_map(|held| self.block(state, held.item.order));
            let Some(block) = below else {
                return Anchor::Bottom;
            };
            offset -= height;
            order = block.order;
            height = block.lines.len() as isize;
        }
        let key = transcript
            .at(order)
            .map(|held| held.item.key.clone())
            .unwrap_or_default();
        let anchor = Anchor::Top {
            key,
            offset: offset as usize,
        };
        // Scrolling down to where the rest no longer fills the feed is
        // following again.
        let probe = Frame {
            anchor: &anchor,
            ..*self
        };
        if probe.downward(state, order, offset as usize).is_none() {
            return Anchor::Bottom;
        }
        anchor
    }
}

/// Held items strictly older than `order`, oldest first.
fn before(state: &SessionState, order: u64) -> impl DoubleEndedIterator<Item = &ui_state::Held> {
    let transcript = state.transcript();
    let oldest = transcript.oldest_held().unwrap_or(order);
    let end = order.saturating_sub(1);
    let empty = order <= oldest;
    transcript
        .range(if empty { order..=order } else { oldest..=end })
        .filter(move |_| !empty)
}

/// Held items strictly newer than `order`, oldest first.
fn after(state: &SessionState, order: u64) -> impl DoubleEndedIterator<Item = &ui_state::Held> {
    let transcript = state.transcript();
    let head = transcript.head().unwrap_or(order);
    let empty = order >= head;
    transcript
        .range(if empty {
            order..=order
        } else {
            order + 1..=head
        })
        .filter(move |_| !empty)
}
