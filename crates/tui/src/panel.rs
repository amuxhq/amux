//! Panels drawn over the screen: a hairline border with a title on its top
//! edge, placed either rising from a place it belongs to (a setting on the
//! composer's edge, the leader's hint) or centred over home as a modal.
//! Everything around a panel stays visible; nothing is dimmed.

use ratatui::Frame as Paint;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};

use crate::text::{self, push};
use crate::theme::Theme;

/// `rows` inside a hairline border `inner` columns wide, one column of
/// padding either side, `title` on the top edge. A row wider than `inner`
/// is cut; `lit` rows take `surface` edge to edge inside the border, under
/// any span without its own background.
pub(crate) fn bordered(
    title: &str,
    rows: Vec<Line<'static>>,
    inner: usize,
    lit: &[usize],
    surface: Option<Style>,
    theme: Theme,
) -> Vec<Line<'static>> {
    let edge = theme.hairline();
    let outer = inner + 4;
    let mut top = Line::from(Span::styled("╭─ ", edge));
    push(&mut top, title, theme.muted(), outer);
    push(&mut top, " ", edge, outer);
    let used = text::line_width(&top);
    push(
        &mut top,
        "─".repeat((inner + 3).saturating_sub(used)),
        edge,
        outer,
    );
    push(&mut top, "╮", edge, outer);
    let mut lines = vec![top];
    for (at, row) in rows.into_iter().enumerate() {
        let mut body = Line::default();
        for span in row.spans {
            push(&mut body, &span.content, span.style, inner);
        }
        text::pad_to(&mut body, inner);
        if lit.contains(&at)
            && let Some(surface) = surface
        {
            for span in &mut body.spans {
                if span.style.bg.is_none() {
                    span.style = span.style.patch(surface);
                }
            }
        }
        let mut line = Line::from(Span::styled("│ ", edge));
        line.spans.extend(body.spans);
        push(&mut line, " │", edge, outer);
        lines.push(line);
    }
    let mut bottom = Line::from(Span::styled("╰", edge));
    push(&mut bottom, "─".repeat(inner + 2), edge, outer);
    push(&mut bottom, "╯", edge, outer);
    lines.push(bottom);
    lines
}

/// The widest of `rows`, for a panel that fits its content.
pub(crate) fn content_width(rows: &[Line<'static>]) -> usize {
    rows.iter().map(text::line_width).max().unwrap_or(0)
}

/// Draws `lines` (a bordered panel) with its foot on the row above `above`
/// and its left edge two columns left of `anchor` where it fits inside
/// `area`. Returns where it was drawn and how many of its first lines were
/// cut for want of room.
pub(crate) fn rise(
    paint: &mut Paint<'_>,
    lines: Vec<Line<'static>>,
    anchor: u16,
    above: u16,
    area: Rect,
) -> Option<(Rect, usize)> {
    let width = content_width(&lines) as u16;
    let height = (lines.len() as u16).min(above.saturating_sub(area.y));
    if height == 0 || width == 0 {
        return None;
    }
    let right = area.x + area.width.saturating_sub(2);
    // The border sits two columns left of the words it rises from, so its
    // contents line up under them.
    let x = anchor
        .saturating_sub(2)
        .min(right.saturating_sub(width))
        .max(area.x + 2);
    let skip = lines.len() - usize::from(height);
    let rect = Rect {
        x,
        y: above - height,
        width: width.min(right.saturating_sub(x)),
        height,
    };
    paint.render_widget(Clear, rect);
    paint.render_widget(
        Paragraph::new(lines.into_iter().skip(skip).collect::<Vec<_>>()),
        rect,
    );
    Some((rect, skip))
}

/// Draws `lines` (a bordered panel) centred across `area`, a little above
/// the middle where the eye already is. Returns where it was drawn.
pub(crate) fn centre(paint: &mut Paint<'_>, lines: Vec<Line<'static>>, area: Rect) -> Rect {
    let width = (content_width(&lines) as u16).min(area.width);
    let height = (lines.len() as u16).min(area.height);
    let rect = Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 3,
        width,
        height,
    };
    paint.render_widget(Clear, rect);
    paint.render_widget(Paragraph::new(lines), rect);
    rect
}

/// A row of buttons (`[Stop]  [Cancel]`), each with its columns in the row;
/// the `focus`th, which Enter presses, stands out on a chip.
pub(crate) fn buttons<H: Clone>(
    labels: &[(&str, H)],
    focus: usize,
    theme: Theme,
) -> (Line<'static>, Vec<(usize, usize, H)>) {
    let mut line = Line::default();
    let mut spots = Vec::new();
    for (i, (label, hit)) in labels.iter().enumerate() {
        if i > 0 {
            line.spans.push(Span::raw("  "));
        }
        let from = text::line_width(&line);
        let ink = if i == focus {
            theme.chip().add_modifier(Modifier::BOLD)
        } else {
            theme.text()
        };
        line.spans.push(Span::styled(format!("[{label}]"), ink));
        spots.push((from, text::line_width(&line), hit.clone()));
    }
    (line, spots)
}

/// A key legend inside a panel: each key, then its action, faint.
pub(crate) fn legend(keys: &[(&str, &str)], theme: Theme) -> Line<'static> {
    let mut line = Line::default();
    for (i, (key, action)) in keys.iter().enumerate() {
        if i > 0 {
            line.spans.push(Span::raw("   "));
        }
        line.spans
            .push(Span::styled(key.to_string(), theme.muted()));
        line.spans
            .push(Span::styled(format!(" {action}"), theme.faint()));
    }
    line
}

/// `label` with the first `letter` in it (any case) brightened and
/// underlined: the key's mnemonic, where the word holds it.
pub(crate) fn mnemonic(label: &str, letter: char, theme: Theme) -> Vec<Span<'static>> {
    let lower = letter.to_ascii_lowercase();
    match label
        .char_indices()
        .find(|(_, c)| c.to_ascii_lowercase() == lower && letter.is_ascii_alphabetic())
    {
        Some((at, c)) => {
            let end = at + c.len_utf8();
            vec![
                Span::styled(label[..at].to_owned(), theme.faint()),
                Span::styled(
                    label[at..end].to_owned(),
                    theme.bright().add_modifier(Modifier::UNDERLINED),
                ),
                Span::styled(label[end..].to_owned(), theme.faint()),
            ]
        }
        None => vec![Span::styled(label.to_owned(), theme.faint())],
    }
}
