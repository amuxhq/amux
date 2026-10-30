//! Cell arithmetic: widths, clipping, wrapping and line assembly. Every
//! measurement in the crate goes through here, with the unicode-width
//! version ratatui renders with, so layout and paint never disagree.

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation;

/// Display width in terminal cells: CJK and emoji two, combining marks zero.
pub(crate) fn str_width(text: &str) -> usize {
    unicode_width::UnicodeWidthStr::width(text)
}

/// The longest prefix of `text` fitting `max` cells, cut at a grapheme
/// boundary so a wide grapheme never straddles the cut.
pub(crate) fn clip_to_width(text: &str, max: usize) -> &str {
    let mut used = 0usize;
    let mut end = 0usize;
    for (offset, grapheme) in text.grapheme_indices(true) {
        let width = str_width(grapheme);
        if used + width > max {
            break;
        }
        used += width;
        end = offset + grapheme.len();
    }
    &text[..end]
}

/// `text` clipped to `max` cells with an ellipsis when anything was cut.
pub(crate) fn ellipsize(text: &str, max: usize) -> String {
    if str_width(text) <= max {
        return text.to_owned();
    }
    if max == 0 {
        return String::new();
    }
    format!("{}…", clip_to_width(text, max - 1))
}

/// `text` in at most `max` cells, cut after its last whole word that fits
/// with "…" in place of the rest, so no word is left half-shown.
pub(crate) fn ellipsize_words(text: &str, max: usize) -> String {
    if str_width(text) <= max {
        return text.to_owned();
    }
    let room = clip_to_width(text, max.saturating_sub(1));
    match room.rfind(' ') {
        Some(at) => format!("{}…", room[..at].trim_end_matches([' ', ','])),
        None => ellipsize(text, max),
    }
}

/// How a key line's `key action` pairs fit `room` columns: the gap
/// between them, which of them show, in order, and where a ` …` marking a
/// cut goes: before the `mark`th shown pair (`shown.len()` for the end).
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Fitted {
    pub gap: usize,
    pub shown: Vec<usize>,
    pub mark: Option<usize>,
}

/// Lays out a key line: three blanks between pairs, or two when that makes
/// them all fit. Short of room, whole pairs go from the end (each line
/// lists its keys most needed first), but the way out (`esc`, `q`) always
/// stays, last, and the cut is marked with `…` where it fits (see
/// [`Fitted`]); a key is never cut mid-word.
pub(crate) fn fit_hints(pairs: &[(&str, &str)], room: usize) -> Fitted {
    let size = |(key, action): &(&str, &str)| str_width(key) + 1 + str_width(action);
    let total =
        |gap: usize| pairs.iter().map(size).sum::<usize>() + gap * pairs.len().saturating_sub(1);
    let all = (0..pairs.len()).collect();
    if total(3) <= room {
        return Fitted {
            gap: 3,
            shown: all,
            mark: None,
        };
    }
    if total(2) <= room {
        return Fitted {
            gap: 2,
            shown: all,
            mark: None,
        };
    }
    let way_out = |at: &usize| matches!(pairs[*at].0, "esc" | "q");
    let (out, rest): (Vec<usize>, Vec<usize>) = (0..pairs.len()).partition(way_out);
    let kept: usize = out.iter().map(|at| 2 + size(&pairs[*at])).sum();
    let mut used = 0;
    let mut shown = Vec::new();
    for at in rest {
        let need = if shown.is_empty() { 0 } else { 2 } + size(&pairs[at]);
        if used + need + kept > room {
            break;
        }
        used += need;
        shown.push(at);
    }
    // The mark only where it fits: a key is worth more than the mark.
    let mark = (used + kept + 2 <= room).then_some(shown.len());
    shown.extend(out);
    Fitted {
        gap: 2,
        shown,
        mark,
    }
}

/// Columns between tab stops in an agent's code.
const TAB_STOP: usize = 4;

/// `text` with each tab widened to spaces up to the next tab stop: a
/// terminal cell cannot hold a tab, and drawn as nothing it glues the words
/// either side together and loses a Makefile's or Go's indentation.
pub(crate) fn expand_tabs(text: &str) -> std::borrow::Cow<'_, str> {
    if !text.contains('\t') {
        return text.into();
    }
    let mut out = String::with_capacity(text.len() + TAB_STOP);
    let mut column = 0;
    for grapheme in text.graphemes(true) {
        match grapheme {
            "\t" => {
                let to = (column / TAB_STOP + 1) * TAB_STOP;
                out.extend(std::iter::repeat_n(' ', to - column));
                column = to;
            }
            "\n" | "\r\n" => {
                out.push_str(grapheme);
                column = 0;
            }
            _ => {
                out.push_str(grapheme);
                column += str_width(grapheme);
            }
        }
    }
    out.into()
}

pub(crate) fn line_width(line: &Line<'_>) -> usize {
    line.spans.iter().map(|span| str_width(&span.content)).sum()
}

/// Appends `text` to `line`, clipped so the line stays within `width`.
pub(crate) fn push(line: &mut Line<'static>, text: impl AsRef<str>, style: Style, width: usize) {
    let room = width.saturating_sub(line_width(line));
    let text = ellipsize(text.as_ref(), room);
    if !text.is_empty() {
        line.spans.push(Span::styled(text, style));
    }
}

/// A path on this machine as the person would write it: their home
/// directory as `~`.
pub(crate) fn tilde(path: &str) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    let home = home.trim_end_matches('/');
    match path.strip_prefix(home) {
        Some(rest) if !home.is_empty() && (rest.is_empty() || rest.starts_with('/')) => {
            format!("~{rest}")
        }
        _ => path.to_owned(),
    }
}

/// Whether a line is only half-block padding drawn with `glyph`: the edge a
/// tinted block draws above (`▄`) or below (`▀`) its words.
pub(crate) fn is_padding(line: &Line<'_>, glyph: char) -> bool {
    let mut any = false;
    for c in line.spans.iter().flat_map(|span| span.content.chars()) {
        if c == glyph {
            any = true;
        } else if c != ' ' {
            return false;
        }
    }
    any
}

/// Whether a line is a tinted block's blank padding line: nothing but
/// blanks, painted on a surface.
fn is_tinted_blank(line: &Line<'_>) -> bool {
    line.spans
        .iter()
        .all(|span| span.content.chars().all(|c| c == ' '))
        && line
            .spans
            .iter()
            .any(|span| span.style.bg.is_some() && !span.content.is_empty())
}

/// Whether a line is painted on a surface at all.
fn is_tinted(line: &Line<'_>) -> bool {
    line.spans
        .iter()
        .any(|span| span.style.bg.is_some() && !span.content.is_empty())
}

/// Padding is drawn only while its block's words are on screen: a lower
/// edge at the top of a window, whose block scrolled off above, and an
/// upper edge at the bottom, whose block is still below, would otherwise
/// read as stray bands. Half-block edges say which they are by their
/// glyph; a whole tinted padding line by whether its neighbour inside the
/// window is part of the same block.
pub(crate) fn drop_cut_padding(lines: &mut [Line<'static>]) {
    let len = lines.len();
    if len == 0 {
        return;
    }
    let cut_top = is_padding(&lines[0], '▀')
        || (is_tinted_blank(&lines[0]) && lines.get(1).is_none_or(|next| !is_tinted(next)));
    if cut_top {
        lines[0] = Line::default();
    }
    let last = len - 1;
    let cut_bottom = is_padding(&lines[last], '▄')
        || (last > 0 && is_tinted_blank(&lines[last]) && !is_tinted(&lines[last - 1]));
    if cut_bottom {
        lines[last] = Line::default();
    }
}

/// `over` laid on top of `under` from column `at`: what `under` holds
/// left and right of it stays, cut at cell boundaries.
pub(crate) fn overlay(
    under: &Line<'static>,
    at: usize,
    over: Line<'static>,
    width: usize,
) -> Line<'static> {
    let over_width = line_width(&over);
    let mut out = Line::default();
    let mut out_col = 0usize;
    let mut under_col = 0usize;
    let mut placed = false;
    let place = |out: &mut Line<'static>, out_col: &mut usize| {
        if *out_col < at {
            out.spans.push(Span::raw(" ".repeat(at - *out_col)));
        }
        out.spans.extend(over.spans.iter().cloned());
        *out_col = at + over_width;
    };
    for span in &under.spans {
        for c in span.content.chars() {
            let w = str_width(c.encode_utf8(&mut [0; 4]));
            let start = under_col;
            let end = start + w;
            under_col = end;
            if end <= at {
                out.spans.push(Span::styled(c.to_string(), span.style));
                out_col = end;
            } else if start >= at + over_width {
                if !placed {
                    place(&mut out, &mut out_col);
                    placed = true;
                }
                if out_col < start {
                    out.spans.push(Span::raw(" ".repeat(start - out_col)));
                }
                out.spans.push(Span::styled(c.to_string(), span.style));
                out_col = end;
            } else if !placed {
                place(&mut out, &mut out_col);
                placed = true;
            }
        }
    }
    if !placed && at < width {
        place(&mut out, &mut out_col);
    }
    out
}

/// Puts `text` at the right edge when it fits after what the line holds,
/// with at least two cells between them; otherwise leaves the line alone.
pub(crate) fn push_right(line: &mut Line<'static>, text: &str, style: Style, width: usize) {
    let used = line_width(line);
    let need = str_width(text);
    if text.is_empty() || used + 2 + need > width {
        return;
    }
    line.spans.push(Span::raw(" ".repeat(width - used - need)));
    line.spans.push(Span::styled(text.to_owned(), style));
}

/// Pads the line with blanks up to column `col`.
pub(crate) fn pad_to(line: &mut Line<'static>, col: usize) {
    let used = line_width(line);
    if used < col {
        line.spans.push(Span::raw(" ".repeat(col - used)));
    }
}

/// Pads the line with `style` to `width`, so a surface colour fills it.
pub(crate) fn fill(line: &mut Line<'static>, style: Style, width: usize) {
    let used = line_width(line);
    if used < width {
        line.spans
            .push(Span::styled(" ".repeat(width - used), style));
    }
}

/// Word-wraps one paragraph at `width` cells; a word longer than the line
/// is split. Newlines in `text` start new lines; an empty line stays one.
pub(crate) fn wrap(text: &str, width: usize) -> Vec<String> {
    wrap_after(text, width, width)
}

/// Word-wraps as [`wrap`] does, with only `first` cells left on the first
/// line (it continues one already holding something) and `width` on every
/// line after. A first word that fits a whole line but not the room left
/// starts on the next line, leaving the first piece empty, as does all of
/// it when no room is left at all.
pub(crate) fn wrap_after(text: &str, first: usize, width: usize) -> Vec<String> {
    let width = width.max(1);
    let first = first.min(width);
    let mut out: Vec<String> = Vec::new();
    for paragraph in text.split('\n') {
        let mut line = String::new();
        let mut used = 0usize;
        for word in words(paragraph) {
            let limit = if out.is_empty() { first } else { width };
            let w = str_width(word);
            if used + w <= limit {
                line.push_str(word);
                used += w;
                continue;
            }
            if word.trim().is_empty() {
                out.push(std::mem::take(&mut line));
                used = 0;
                continue;
            }
            if !line.is_empty() || (out.is_empty() && w <= width) {
                out.push(std::mem::take(&mut line).trim_end().to_owned());
            }
            let mut rest = word;
            loop {
                let limit = if out.is_empty() { first } else { width };
                if str_width(rest) <= limit {
                    break;
                }
                if limit == 0 {
                    // A full first line takes nothing; the word starts on
                    // the next.
                    out.push(std::mem::take(&mut line).trim_end().to_owned());
                    continue;
                }
                let head = clip_to_width(rest, limit);
                let head = if head.is_empty() {
                    rest.graphemes(true).next().unwrap_or(rest)
                } else {
                    head
                };
                out.push(head.to_owned());
                rest = &rest[head.len()..];
            }
            line.push_str(rest);
            used = str_width(rest);
        }
        out.push(line.trim_end().to_owned());
    }
    out
}

/// `paragraph` as the runs a line may break between: each run of spaces,
/// and each run of everything else, so punctuation stays with its word
/// ("order." never leaves its full stop for the next line).
fn words(paragraph: &str) -> impl Iterator<Item = &str> {
    let mut bounds = paragraph.split_word_bound_indices().peekable();
    std::iter::from_fn(move || {
        let (start, first) = bounds.next()?;
        let space = first.trim().is_empty();
        let mut end = start + first.len();
        while let Some((at, next)) = bounds.peek() {
            if next.trim().is_empty() != space || space {
                break;
            }
            end = at + next.len();
            bounds.next();
        }
        Some(&paragraph[start..end])
    })
}

/// The first line of `text`, for a row that opens to the rest.
pub(crate) fn first_line(text: &str) -> &str {
    text.trim_start().lines().next().unwrap_or_default()
}

/// "1m 42s", "8s", "340ms".
pub(crate) fn duration(ms: i64) -> String {
    let ms = ms.max(0);
    if ms < 1_000 {
        return format!("{ms}ms");
    }
    let secs = ms / 1_000;
    if secs < 60 {
        if ms < 10_000 && ms % 1_000 != 0 {
            return format!("{:.1}s", ms as f64 / 1_000.0);
        }
        return format!("{secs}s");
    }
    let mins = secs / 60;
    if mins < 60 {
        return format!("{mins}m {}s", secs % 60);
    }
    format!("{}h {}m", mins / 60, mins % 60)
}

/// "148k", "1.2M", "812".
pub(crate) fn tokens(count: u64) -> String {
    match count {
        0..1_000 => count.to_string(),
        1_000..1_000_000 => format!("{}k", count / 1_000),
        _ => format!("{:.1}M", count as f64 / 1_000_000.0),
    }
}

/// "12 KB", "3.4 MB", "900 B".
pub(crate) fn bytes(size: u64) -> String {
    match size {
        0..1_024 => format!("{size} B"),
        1_024..1_048_576 => format!("{} KB", size / 1_024),
        _ => format!("{:.1} MB", size as f64 / 1_048_576.0),
    }
}

/// "3m ago", "2h ago", "just now": the fleet's last-activity column.
/// A fleet cell's age: "now" under a minute, then minutes, hours, days.
pub(crate) fn age(now_ms: i64, then_ms: i64) -> String {
    if then_ms <= 0 {
        return String::new();
    }
    let secs = (now_ms - then_ms).max(0) / 1_000;
    match secs {
        0..60 => "now".into(),
        60..3_600 => format!("{}m", secs / 60),
        3_600..86_400 => format!("{}h", secs / 3_600),
        _ => format!("{}d", secs / 86_400),
    }
}

/// A count with its thousands set apart: 1,240.
pub fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// How long something ran or has run, coarsely: "1s" (never "0s"), "4m",
/// "2h 5m".
pub(crate) fn ran_for(ms: i64) -> String {
    let secs = ms.max(0) / 1_000;
    match secs {
        0..60 => format!("{}s", secs.max(1)),
        60..3_600 => format!("{}m", secs / 60),
        _ => format!("{}h {}m", secs / 3_600, secs % 3_600 / 60),
    }
}

/// The end of `text` in at most `max` cells, "…" where it is cut.
pub(crate) fn tail(text: &str, max: usize) -> String {
    if str_width(text) <= max {
        return text.to_owned();
    }
    let keep = max.saturating_sub(1);
    let mut out: Vec<char> = Vec::new();
    let mut used = 0;
    for c in text.chars().rev() {
        let w = str_width(c.encode_utf8(&mut [0; 4]));
        if used + w > keep {
            break;
        }
        used += w;
        out.push(c);
    }
    out.reverse();
    format!("…{}", out.into_iter().collect::<String>())
}

/// `text` in at most `max` columns, cut in the middle so both ends show.
pub(crate) fn ellipsize_middle(text: &str, max: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max || max < 8 {
        return text.to_owned();
    }
    let head = (max - 1) / 2;
    let tail = max - 1 - head;
    let mut out: String = chars[..head].iter().collect();
    out.push('…');
    out.extend(&chars[chars.len() - tail..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_breaks_at_words_and_splits_only_overlong_ones() {
        assert_eq!(wrap("alpha beta gamma", 10), vec!["alpha beta", "gamma"]);
        assert_eq!(wrap("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
        assert_eq!(wrap("one\n\ntwo", 10), vec!["one", "", "two"]);
        // Punctuation stays with its word.
        assert_eq!(
            wrap("in this order. 1: go", 13),
            vec!["in this", "order. 1: go"]
        );
        assert_eq!(wrap("see e.g. here", 7), vec!["see", "e.g.", "here"]);
    }

    #[test]
    fn wide_graphemes_never_straddle_a_clip() {
        assert_eq!(clip_to_width("日本語", 5), "日本");
        assert_eq!(ellipsize("日本語", 5), "日本…");
    }

    #[test]
    fn durations_read_the_way_the_vocabulary_writes_them() {
        assert_eq!(duration(102_000), "1m 42s");
        assert_eq!(duration(8_000), "8s");
        assert_eq!(duration(4_200), "4.2s");
        assert_eq!(tokens(148_000), "148k");
    }

    #[test]
    fn right_meta_is_dropped_rather_than_colliding() {
        let mut line = Line::from("left");
        push_right(&mut line, "meta", Style::default(), 9);
        assert_eq!(line_width(&line), 4);
        push_right(&mut line, "meta", Style::default(), 12);
        assert_eq!(line_width(&line), 12);
    }
}
