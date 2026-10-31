//! Report a Problem: the screen frozen as it was at the key, rectangles
//! dragged over it with the mouse, a note on each and one overall, handed
//! to the daemon, which writes the bundle into its reports directory and
//! sends it to the profile's account when it is signed in to one.
//!
//! The bundle is the phone's layout with a terminal's frame: `report.json`
//! carries the note and the marks, measured in cells, and declares the
//! parts present or absent with a reason; `frame.txt` is the frozen screen
//! as text to read, and `frame.json` the same cells with their colours and
//! styles, from which a picture can be drawn later (`amux-shot frame`)
//! without a font in this binary; `dump/` is the profile's dump with this
//! client's parts, started at the key so its round trip costs nothing while
//! the note is written. The terminal keeps no log of its own (the daemon's
//! is in the dump), draws no picture itself and records no view trace, and
//! says so.

use std::path::PathBuf;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use client::Client;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::Frame as Paint;
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use serde_json::json;
use tokio::task::JoinHandle;

use crate::chat::composer::editor_lines;
use crate::editor::Editor;
use crate::text::{self, push};
use crate::theme::Theme;

/// A rectangle over the frozen frame, in cells, with what is wrong there.
#[derive(Clone, Debug, PartialEq)]
pub struct Mark {
    pub area: Rect,
    pub note: String,
}

/// What a key or click on the report asks of the app.
#[derive(Debug, PartialEq)]
pub enum ReportAction {
    None,
    /// Leave without writing anything.
    Cancel,
    /// Write the bundle.
    Save,
}

/// The profile's dump, on its way: its directory, or why there is none.
pub type Dump = JoinHandle<Result<PathBuf, String>>;

pub struct Report {
    frozen: Buffer,
    /// The palette the frozen cells were drawn in, which names what a
    /// cell left at the terminal's default looked like.
    theme: Theme,
    at: DateTime<Utc>,
    marks: Vec<Mark>,
    /// Where a drag started, and where it is now.
    drag: Option<(Position, Position)>,
    note: Editor,
    /// The mark whose note is being written, by its place in `marks`.
    marking: Option<usize>,
    mark_note: Editor,
    dump: Option<Dump>,
}

impl Report {
    /// A report over the frozen screen, with the dump started for it; with
    /// none on its way (no profile runs), saving it records that no dump
    /// was started.
    pub fn new(frozen: Buffer, theme: Theme, dump: Option<Dump>) -> Report {
        Report {
            frozen,
            theme,
            at: Utc::now(),
            marks: Vec::new(),
            drag: None,
            note: Editor::default(),
            marking: None,
            mark_note: Editor::default(),
            dump,
        }
    }

    fn field(&mut self) -> &mut Editor {
        if self.marking.is_some() {
            &mut self.mark_note
        } else {
            &mut self.note
        }
    }

    /// Enter saves (a mark's note, or the report); Esc backs out one level:
    /// a mark's note is skipped, the report is left unwritten. Ctrl+C
    /// clears the field.
    pub fn key(&mut self, key: KeyEvent) -> ReportAction {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => {
                if self.marking.take().is_some() {
                    self.mark_note = Editor::default();
                    return ReportAction::None;
                }
                ReportAction::Cancel
            }
            KeyCode::Enter if !key.modifiers.contains(KeyModifiers::SHIFT) => {
                if let Some(at) = self.marking.take() {
                    let note = std::mem::take(&mut self.mark_note);
                    self.marks[at].note = note.text().trim().to_owned();
                    return ReportAction::None;
                }
                ReportAction::Save
            }
            KeyCode::Char('c') if ctrl => {
                self.field().kill_all();
                ReportAction::None
            }
            _ => {
                self.field().key(key);
                ReportAction::None
            }
        }
    }

    pub fn paste(&mut self, text: &str) {
        self.field().paste(text);
    }

    /// A drag with the left button marks a rectangle; letting go asks for
    /// its note.
    pub fn mouse(&mut self, event: MouseEvent) {
        let at = Position::new(event.column, event.row);
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => self.drag = Some((at, at)),
            MouseEventKind::Drag(MouseButton::Left) => {
                if let Some((_, now)) = &mut self.drag {
                    *now = at;
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                if let Some((from, _)) = self.drag.take() {
                    let area = spanned(from, at);
                    // A click with no drag marks the cell under it.
                    self.marks.push(Mark {
                        area,
                        note: String::new(),
                    });
                    self.marking = Some(self.marks.len() - 1);
                    self.mark_note = Editor::default();
                }
            }
            _ => {}
        }
    }

    /// The frozen frame, the marks over it, and the panel at the foot of
    /// `area`.
    pub fn draw(&self, paint: &mut Paint<'_>, area: Rect, theme: Theme) {
        let buffer = paint.buffer_mut();
        let shared = area.intersection(self.frozen.area);
        for y in shared.top()..shared.bottom() {
            for x in shared.left()..shared.right() {
                buffer[(x, y)] = self.frozen[(x, y)].clone();
            }
        }
        let drawn = self
            .marks
            .iter()
            .map(|mark| mark.area)
            .chain(self.drag.map(|(from, to)| spanned(from, to)));
        for mark in drawn {
            highlight(buffer, mark.intersection(area));
        }

        let width = usize::from(area.width);
        let inner = width.saturating_sub(4).max(1);
        let (prompt, field) = match self.marking {
            Some(at) => (
                format!("Mark {}: what is wrong here?", at + 1),
                &self.mark_note,
            ),
            None => ("What went wrong?".to_owned(), &self.note),
        };
        let (editor, (row, col)) = editor_lines(field, &prompt, inner, theme);
        let mut lines = Vec::new();
        let mut rule = Line::from(Span::raw("  "));
        push(&mut rule, "Report a problem ", theme.emphasis(), width);
        let marks = match self.marks.len() {
            0 => "drag over anything wrong to mark it".to_owned(),
            1 => "1 mark".to_owned(),
            n => format!("{n} marks"),
        };
        push(&mut rule, format!("· {marks} "), theme.faint(), width);
        let fill = width.saturating_sub(text::line_width(&rule) + 2);
        push(&mut rule, "─".repeat(fill), theme.hairline(), width);
        lines.push(rule);
        lines.push(Line::default());
        let editor_top = lines.len();
        for line in editor {
            let mut padded = Line::from(Span::raw("  "));
            padded.spans.extend(line.spans);
            lines.push(padded);
        }
        lines.push(Line::default());
        let keys = if self.marking.is_some() {
            "enter keep the note   esc no note"
        } else {
            "enter send   esc cancel   drag to mark"
        };
        let mut hint = Line::from(Span::raw("  "));
        push(&mut hint, keys, theme.muted(), width);
        lines.push(hint);
        let height = u16::try_from(lines.len())
            .unwrap_or(u16::MAX)
            .min(area.height);
        let panel = Rect::new(area.x, area.bottom() - height, area.width, height);
        paint.render_widget(Clear, panel);
        paint.render_widget(Paragraph::new(lines), panel);
        let y = panel.y as usize + editor_top + row;
        let x = area.x as usize + 2 + col;
        if y < usize::from(area.bottom()) {
            paint.set_cursor_position(Position::new(x as u16, y as u16));
        }
    }

    /// Hands the report to the daemon once the dump has finished: it
    /// writes the bundle into its reports directory and sends it on when
    /// the profile is signed in, and says where it went.
    pub async fn submit(
        mut self,
        client: Arc<dyn Client>,
        build: String,
    ) -> Result<wire::SubmitReportResponse, String> {
        let dump = match self.dump.take() {
            Some(task) => task
                .await
                .unwrap_or_else(|error| Err(format!("the dump stopped: {error}"))),
            None => Err("no dump was started".to_owned()),
        };
        let mut header = json!({
            "build": build,
            "created_at": self.at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            "note": self.note.text().trim(),
            // Marks and the viewport are in cells: a terminal frame has no
            // pixels.
            "marks": self.marks.iter().map(|mark| json!({
                "x": mark.area.x,
                "y": mark.area.y,
                "width": mark.area.width,
                "height": mark.area.height,
                "note": mark.note,
            })).collect::<Vec<_>>(),
            "viewport": [self.frozen.area.width, self.frozen.area.height],
            "parts": {
                "frame": "present",
                "trace": absent("the terminal records no view trace; its runtime's order of events is in the dump"),
            },
        });
        // The daemon declares a dump it holds; why there is none is this
        // client's to say.
        if let Err(why) = &dump {
            header["parts"]["dump"] = absent(why.clone());
        }
        let file = |name: &str, contents: Vec<u8>| wire::DumpFile {
            name: name.to_owned(),
            contents,
        };
        let cells = serde_json::to_vec(&frame_cells(&self.frozen, self.theme))
            .map_err(|error| error.to_string())?;
        let request = wire::SubmitReportRequest {
            dump_path: dump
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_default(),
            files: vec![
                file(
                    "report.json",
                    serde_json::to_vec_pretty(&header).map_err(|error| error.to_string())?,
                ),
                file("frame.txt", frame_text(&self.frozen).into_bytes()),
                file("frame.json", cells),
            ],
        };
        client
            .submit_report(request)
            .await
            .map_err(|error| format!("the daemon could not take the report: {error}"))
    }
}

/// The rectangle two corners span, both included.
fn spanned(a: Position, b: Position) -> Rect {
    let (left, right) = (a.x.min(b.x), a.x.max(b.x));
    let (top, bottom) = (a.y.min(b.y), a.y.max(b.y));
    Rect::new(left, top, right - left + 1, bottom - top + 1)
}

/// A mark shows as its cells reversed, so what it marks stays readable.
fn highlight(buffer: &mut Buffer, area: Rect) {
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            let cell = &mut buffer[(x, y)];
            let style = cell.style().add_modifier(Modifier::REVERSED);
            cell.set_style(style);
        }
    }
}

fn absent(reason: impl Into<String>) -> serde_json::Value {
    json!({ "absent": { "reason": reason.into() } })
}

/// The frozen cells as lines of text, trailing blanks dropped.
fn frame_text(frame: &Buffer) -> String {
    let mut out = String::new();
    for y in frame.area.top()..frame.area.bottom() {
        let mut line = String::new();
        let mut skip = 0;
        for x in frame.area.left()..frame.area.right() {
            if skip > 0 {
                skip -= 1;
                continue;
            }
            let symbol = frame[(x, y)].symbol();
            skip = text::str_width(symbol).saturating_sub(1);
            line.push_str(symbol);
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

/// The frozen cells with what they were drawn in: each row as runs of
/// cells sharing one style, from their first column. A blank cell on the
/// same ground joins the run before it, its ink being invisible, so words
/// stay in one run with the spaces between them. A colour is `#rrggbb`,
/// `ansi:N` for one of the terminal's own numbered colours, or `default`
/// for the terminal's own ink or ground, whose values `default` gives as
/// this client's palette had them.
fn frame_cells(frame: &Buffer, theme: Theme) -> serde_json::Value {
    let hex = |(red, green, blue): (u8, u8, u8)| format!("#{red:02x}{green:02x}{blue:02x}");
    let rows: Vec<serde_json::Value> = (frame.area.top()..frame.area.bottom())
        .map(|y| {
            let mut runs: Vec<serde_json::Value> = Vec::new();
            let mut current: Option<(u16, String, Color, Color, Modifier)> = None;
            let mut skip = 0;
            for x in frame.area.left()..frame.area.right() {
                if skip > 0 {
                    skip -= 1;
                    continue;
                }
                let cell = &frame[(x, y)];
                let symbol = cell.symbol();
                skip = text::str_width(symbol).saturating_sub(1);
                let style = (cell.fg, cell.bg, cell.modifier);
                let drawn_on_blank =
                    Modifier::UNDERLINED | Modifier::REVERSED | Modifier::CROSSED_OUT;
                let blank =
                    |modifier: Modifier| symbol == " " && !modifier.intersects(drawn_on_blank);
                match &mut current {
                    Some((_, text, fg, bg, modifier))
                        if (*fg, *bg, *modifier) == style
                            || (*bg == cell.bg && blank(cell.modifier) && blank(*modifier)) =>
                    {
                        text.push_str(symbol);
                    }
                    _ => {
                        runs.extend(current.take().map(run));
                        current = Some((x, symbol.to_owned(), style.0, style.1, style.2));
                    }
                }
            }
            runs.extend(current.map(run));
            serde_json::Value::Array(runs)
        })
        .collect();
    json!({
        "width": frame.area.width,
        "height": frame.area.height,
        "default": {
            "fg": hex(theme.tokens.text.rgb),
            "bg": hex(theme.tokens.background.rgb),
        },
        "rows": rows,
    })
}

fn run((x, text, fg, bg, modifier): (u16, String, Color, Color, Modifier)) -> serde_json::Value {
    let mut out = json!({ "x": x, "text": text, "fg": color_name(fg), "bg": color_name(bg) });
    let styles: Vec<&str> = [
        (Modifier::BOLD, "bold"),
        (Modifier::DIM, "dim"),
        (Modifier::ITALIC, "italic"),
        (Modifier::UNDERLINED, "underlined"),
        (Modifier::REVERSED, "reversed"),
        (Modifier::CROSSED_OUT, "crossed_out"),
        (Modifier::HIDDEN, "hidden"),
    ]
    .into_iter()
    .filter(|(flag, _)| modifier.contains(*flag))
    .map(|(_, name)| name)
    .collect();
    if !styles.is_empty() {
        out["styles"] = json!(styles);
    }
    out
}

fn color_name(color: Color) -> String {
    let ansi = |index: u8| format!("ansi:{index}");
    match color {
        Color::Reset => "default".to_owned(),
        Color::Rgb(red, green, blue) => format!("#{red:02x}{green:02x}{blue:02x}"),
        Color::Indexed(index) => ansi(index),
        Color::Black => ansi(0),
        Color::Red => ansi(1),
        Color::Green => ansi(2),
        Color::Yellow => ansi(3),
        Color::Blue => ansi(4),
        Color::Magenta => ansi(5),
        Color::Cyan => ansi(6),
        Color::Gray => ansi(7),
        Color::DarkGray => ansi(8),
        Color::LightRed => ansi(9),
        Color::LightGreen => ansi(10),
        Color::LightYellow => ansi(11),
        Color::LightBlue => ansi(12),
        Color::LightMagenta => ansi(13),
        Color::LightCyan => ansi(14),
        Color::White => ansi(15),
    }
}

#[cfg(test)]
mod tests {
    use ratatui::buffer::Buffer;
    use ratatui::layout::{Position, Rect};

    use super::{frame_text, spanned};

    #[test]
    fn a_drag_spans_both_corners_whichever_way_it_went() {
        let up_left = spanned(Position::new(9, 7), Position::new(2, 3));
        assert_eq!(up_left, Rect::new(2, 3, 8, 5));
        assert_eq!(
            spanned(Position::new(4, 4), Position::new(4, 4)),
            Rect::new(4, 4, 1, 1)
        );
    }

    #[test]
    fn the_frame_text_is_the_cells_without_trailing_blanks() {
        let mut frame = Buffer::empty(Rect::new(0, 0, 6, 2));
        frame.set_string(0, 0, "amux", ratatui::style::Style::default());
        frame.set_string(1, 1, "日本", ratatui::style::Style::default());
        assert_eq!(frame_text(&frame), "amux\n 日本\n");
    }
}
