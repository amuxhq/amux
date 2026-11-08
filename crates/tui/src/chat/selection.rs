//! Text dragged over with the mouse: what a drawn line reads as when copied,
//! and the two kinds of selection. The feed's is held by its rows, so it
//! stays on the words while the feed scrolls and streams; the side pane's and
//! the review page's are held by screen cells, as those scroll only under the
//! reader's own hand, which clears them.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Line;
use unicode_segmentation::UnicodeSegmentation as _;

use crate::text::str_width;

/// How a drawn line continues the one before it when copied.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Join {
    /// A line of its own.
    #[default]
    New,
    /// The same paragraph, wrapped where a space was.
    Space,
    /// The same word or code line, cut at the edge.
    Tight,
}

/// The columns of a drawn line that hold its words, and how it continues
/// the line before. Margins, rails and a message's time fall outside them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LineText {
    pub from: usize,
    pub to: usize,
    pub join: Join,
    /// Code: its spaces and blank lines are its own, and copy as drawn.
    pub exact: bool,
}

impl LineText {
    /// From the ink alone: everything between a line's first and last
    /// visible character.
    pub fn of(line: &Line<'_>) -> LineText {
        let mut at = 0;
        let mut first = None;
        let mut last = 0;
        for span in &line.spans {
            for grapheme in span.content.graphemes(true) {
                let width = str_width(grapheme);
                if !grapheme.trim().is_empty() {
                    first.get_or_insert(at);
                    last = at + width;
                }
                at += width;
            }
        }
        LineText {
            from: first.unwrap_or(0),
            to: last,
            join: Join::New,
            exact: false,
        }
    }
}

/// A place in the feed: a row, one of its drawn lines, a column.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Point {
    pub order: u64,
    pub line: usize,
    pub col: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FeedSelection {
    pub anchor: Point,
    pub head: Point,
}

impl FeedSelection {
    pub fn bounds(&self) -> (Point, Point) {
        if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }

    /// Whether this drawn line lies between the selection's ends.
    pub fn covers(&self, order: u64, line: usize) -> bool {
        let (start, end) = self.bounds();
        (start.order, start.line) <= (order, line) && (order, line) <= (end.order, end.line)
    }

    /// The columns selected on a line, inside its words.
    pub fn columns(&self, order: u64, line: usize, text: LineText) -> Option<(usize, usize)> {
        if !self.covers(order, line) {
            return None;
        }
        let (start, end) = self.bounds();
        let from = if (start.order, start.line) == (order, line) {
            start.col
        } else {
            0
        };
        let to = if (end.order, end.line) == (order, line) {
            end.col.saturating_add(1)
        } else {
            usize::MAX
        };
        let (from, to) = (from.max(text.from), to.min(text.to));
        (from < to).then_some((from, to))
    }
}

/// What a surface last drew, cell by cell. Kept every frame, so a press,
/// drag and release that arrive together before the next redraw still read
/// what was on screen.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScreenCells {
    pub rect: Rect,
    rows: Vec<Vec<String>>,
    /// Columns at the start of each row that are not words, a diff's gutter.
    left: Vec<u16>,
}

impl ScreenCells {
    pub fn capture(buffer: &Buffer, rect: Rect, left: Vec<u16>) -> ScreenCells {
        let rect = rect.intersection(buffer.area);
        let rows = (rect.top()..rect.bottom())
            .map(|y| {
                (rect.left()..rect.right())
                    .map(|x| buffer[(x, y)].symbol().to_owned())
                    .collect()
            })
            .collect();
        ScreenCells { rect, rows, left }
    }

    fn gutter(&self, y: u16) -> u16 {
        self.left
            .get(usize::from(y - self.rect.y))
            .copied()
            .unwrap_or(0)
    }
}

/// A selection over screen cells inside `rect`, in reading order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScreenSelection {
    pub rect: Rect,
    pub anchor: (u16, u16),
    pub head: (u16, u16),
}

impl ScreenSelection {
    pub fn new(rect: Rect, anchor: (u16, u16)) -> ScreenSelection {
        ScreenSelection {
            rect,
            anchor,
            head: anchor,
        }
    }

    pub fn drag_to(&mut self, x: u16, y: u16) {
        let right = self.rect.right().saturating_sub(1).max(self.rect.x);
        let bottom = self.rect.bottom().saturating_sub(1).max(self.rect.y);
        self.head = (x.clamp(self.rect.x, right), y.clamp(self.rect.y, bottom));
    }

    fn bounds(&self) -> ((u16, u16), (u16, u16)) {
        let (a, b) = (self.anchor, self.head);
        if (a.1, a.0) <= (b.1, b.0) {
            (a, b)
        } else {
            (b, a)
        }
    }

    /// The characters selected on one row, each with its column and width:
    /// a wide character counts when any of its cells is covered.
    fn on_row<'a>(&self, cells: &'a ScreenCells, y: u16) -> Vec<(u16, u16, &'a str)> {
        let (start, end) = self.bounds();
        let words_from = cells.rect.x + cells.gutter(y);
        let from = if y == start.1 { start.0 } else { cells.rect.x }.max(words_from);
        let to = if y == end.1 {
            end.0 + 1
        } else {
            cells.rect.right()
        };
        let row = &cells.rows[usize::from(y - cells.rect.y)];
        let mut out = Vec::new();
        let mut at = 0usize;
        while let Some(symbol) = row.get(at) {
            let x = cells.rect.x + at as u16;
            let width = str_width(symbol).max(1) as u16;
            if x >= words_from && x < to && x + width > from {
                out.push((x, width, symbol.as_str()));
            }
            at += usize::from(width);
        }
        out
    }

    fn rows(&self, cells: &ScreenCells) -> std::ops::RangeInclusive<u16> {
        let (start, end) = self.bounds();
        start.1..=end.1.min(cells.rect.bottom().saturating_sub(1))
    }

    /// Lights the selected words in the frame just drawn.
    pub fn paint(&self, buffer: &mut Buffer, cells: &ScreenCells, style: Style) {
        for y in self.rows(cells) {
            let on_row = self.on_row(cells, y);
            let ink = || {
                on_row
                    .iter()
                    .filter(|(_, _, symbol)| !symbol.trim().is_empty())
            };
            let (Some(first), Some(last)) = (ink().next(), ink().next_back()) else {
                continue;
            };
            for x in first.0..last.0 + last.1 {
                buffer[(x, y)].set_style(style);
            }
        }
    }

    pub fn text(&self, cells: &ScreenCells) -> String {
        let pieces: Vec<Piece> = self
            .rows(cells)
            .map(|y| {
                let row: String = self
                    .on_row(cells, y)
                    .into_iter()
                    .map(|(_, _, symbol)| symbol)
                    .collect();
                let row = row.trim_end();
                let row = if cells.gutter(y) > 0 {
                    row
                } else {
                    row.trim_start()
                };
                Piece::new(row, Join::New)
            })
            .collect();
        joined(&pieces)
    }
}

/// One drawn line's selected words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Piece {
    pub text: String,
    pub join: Join,
    pub exact: bool,
}

impl Piece {
    pub fn new(text: &str, join: Join) -> Piece {
        Piece {
            text: text.to_owned(),
            join,
            exact: false,
        }
    }
}

/// Selected lines as one text: wrapped lines rejoined, one blank line at
/// most between blocks outside code, nothing blank at either end.
pub fn joined(pieces: &[Piece]) -> String {
    let mut out = String::new();
    for (i, piece) in pieces.iter().enumerate() {
        let text = if piece.exact {
            piece.text.as_str()
        } else {
            piece.text.trim_end()
        };
        if i == 0 {
            out.push_str(text);
            continue;
        }
        match piece.join {
            Join::New => {
                if !piece.exact && text.is_empty() && (out.is_empty() || out.ends_with('\n')) {
                    continue;
                }
                out.push('\n');
                out.push_str(text);
            }
            Join::Space => {
                if !text.is_empty() && !out.is_empty() && !out.ends_with([' ', '\n']) {
                    out.push(' ');
                }
                out.push_str(text.trim_start());
            }
            Join::Tight => out.push_str(text),
        }
    }
    out.trim_matches('\n').trim_end().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrapped_lines_rejoin_and_blank_lines_collapse() {
        let pieces = [
            Piece::new("the first", Join::New),
            Piece::new("line goes on", Join::Space),
            Piece::new("", Join::New),
            Piece::new("", Join::New),
            Piece::new("  let a = long", Join::New),
            Piece::new("_name;", Join::Tight),
            Piece::new("", Join::New),
        ];
        assert_eq!(
            joined(&pieces),
            "the first line goes on\n\n  let a = long_name;"
        );
    }

    #[test]
    fn code_keeps_a_space_at_its_cut_and_every_blank_line() {
        let code = |text: &str, join| Piece {
            text: text.to_owned(),
            join,
            exact: true,
        };
        let pieces = [
            code("let a = \"two ", Join::New),
            code("words\";", Join::Tight),
            code("", Join::New),
            code("", Join::New),
            code("next", Join::New),
        ];
        assert_eq!(joined(&pieces), "let a = \"two words\";\n\n\nnext");
    }

    #[test]
    fn a_drag_from_the_second_cell_of_a_wide_character_includes_it() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 10, 1));
        buffer.set_string(0, 0, "你好", Style::default());
        let cells = ScreenCells::capture(&buffer, buffer.area, Vec::new());
        let mut selection = ScreenSelection::new(cells.rect, (1, 0));
        selection.drag_to(3, 0);
        assert_eq!(selection.text(&cells), "你好");
    }

    #[test]
    fn a_selection_reads_the_same_dragged_either_way() {
        let at = |order, line, col| Point { order, line, col };
        let down = FeedSelection {
            anchor: at(1, 2, 3),
            head: at(2, 0, 4),
        };
        let up = FeedSelection {
            anchor: down.head,
            head: down.anchor,
        };
        let text = LineText {
            from: 2,
            to: 20,
            ..LineText::default()
        };
        for selection in [down, up] {
            assert_eq!(selection.columns(1, 1, text), None);
            assert_eq!(selection.columns(1, 2, text), Some((3, 20)));
            assert_eq!(selection.columns(1, 3, text), Some((2, 20)));
            assert_eq!(selection.columns(2, 0, text), Some((2, 5)));
            assert_eq!(selection.columns(2, 1, text), None);
        }
    }
}
