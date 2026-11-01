//! A terminal pane as a journey's golden holds it: the text, with what
//! moves from run to run masked, and a map of each cell's style.

use std::collections::BTreeMap;

use unicode_width::UnicodeWidthChar as _;

use crate::ctl::tui::{Style, styled_cells};

/// The columns a character takes: none for a combining mark, two for a
/// wide one.
pub(crate) fn cell_width(character: char) -> usize {
    match character.width() {
        Some(0) => 0,
        Some(2) => 2,
        _ => 1,
    }
}

fn cells(text: &str) -> usize {
    text.chars().map(cell_width).sum()
}

/// A style as the legend names it: the JSON object of its SGR attributes,
/// keys sorted, colours in SGR's own spelling (`5;n`, `2;r;g;b`), as the
/// goldens were first written.
fn style_key(style: &Style) -> String {
    let colour = |colour: &str| {
        if let Some(index) = colour.strip_prefix("ansi:") {
            Some(format!("\"5;{index}\""))
        } else {
            let hex = colour.strip_prefix('#')?;
            let channel = |at: usize| u8::from_str_radix(&hex[at..at + 2], 16).unwrap_or(0);
            Some(format!(
                "\"2;{};{};{}\"",
                channel(0),
                channel(2),
                channel(4)
            ))
        }
    };
    let mut fields: BTreeMap<&str, String> = BTreeMap::new();
    for (name, value) in [("fg", &style.fg), ("bg", &style.bg)] {
        if let Some(value) = colour(value) {
            fields.insert(name, value);
        }
    }
    for (on, name) in [
        (style.bold, "bold"),
        (style.dim, "dim"),
        (style.italic, "italic"),
        (style.underlined, "underline"),
        (style.reversed, "reverse"),
        (style.crossed_out, "strike"),
    ] {
        if on {
            fields.insert(name, "true".to_owned());
        }
    }
    let fields: Vec<String> = fields
        .iter()
        .map(|(name, value)| format!("\"{name}\": {value}"))
        .collect();
    format!("{{{}}}", fields.join(", "))
}

/// tmux `capture-pane -e` output as text rows and one style key per cell.
pub(crate) fn parse_styled(dump: &str, cols: usize) -> (Vec<String>, Vec<Vec<String>>) {
    styled_cells(dump, cols)
        .into_iter()
        .map(|row| {
            let text: String = row.iter().map(|(text, _)| text.as_str()).collect();
            let keys = row.iter().map(|(_, style)| style_key(style)).collect();
            (text.trim_end().to_owned(), keys)
        })
        .unzip()
}

/// One character per cell: `.` for the default style, then letters in
/// order of first appearance, with a legend naming each.
pub(crate) fn style_map(styles: &[Vec<String>]) -> String {
    let blank = style_key(&Style::plain());
    let mut legend: Vec<(String, char)> = vec![(blank, '.')];
    let mut letters = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789".chars();
    let mut rows = Vec::new();
    for row in styles {
        let mut out = String::new();
        for key in row {
            let letter = match legend.iter().find(|(known, _)| known == key) {
                Some((_, letter)) => *letter,
                None => {
                    let letter = letters.next().unwrap_or('?');
                    legend.push((key.clone(), letter));
                    letter
                }
            };
            out.push(letter);
        }
        rows.push(out);
    }
    let names: Vec<String> = legend
        .iter()
        .filter(|(_, letter)| *letter != '.')
        .map(|(key, letter)| format!("{letter} = {key}"))
        .collect();
    format!("{}\n--- legend\n{}\n", rows.join("\n"), names.join("\n"))
}

fn is_word(character: char) -> bool {
    character.is_ascii_alphanumeric() || character == '_'
}

/// Relative ages (`just now`, `3m ago`), a prompt's time of day at its
/// row's right edge, when a limit resets, the fleet's compact ages and the
/// served net's random scratch suffixes: everything in a frame that moves
/// with the clock or the run.
pub(crate) fn normalize(text: &str) -> String {
    let lines: Vec<String> = text
        .split('\n')
        .map(|line| {
            let line = clock_at_end(line);
            let line = resets(&line);
            let line = ages(&line);
            let line = fleet_ages(&line);
            let line = scratch(&line);
            scratch_tail(&line)
        })
        .collect();
    lines.join("\n")
}

/// `hh:mm` for a time of day a space before the line's end.
fn clock_at_end(line: &str) -> String {
    match line.as_bytes() {
        [.., b' ', h, hh, b':', m, mm]
            if [h, hh, m, mm].iter().all(|digit| digit.is_ascii_digit()) =>
        {
            format!("{}hh:mm", &line[..line.len() - 5])
        }
        _ => line.to_owned(),
    }
}

/// `hh:mm` for the time after `resets `.
fn resets(line: &str) -> String {
    let mut out = String::new();
    let mut rest = line;
    while let Some(at) = rest.find("resets ") {
        let after = at + "resets ".len();
        out.push_str(&rest[..after]);
        rest = &rest[after..];
        let t = rest.as_bytes();
        if t.len() >= 5
            && t[0].is_ascii_digit()
            && t[1].is_ascii_digit()
            && t[2] == b':'
            && t[3].is_ascii_digit()
            && t[4].is_ascii_digit()
        {
            out.push_str("hh:mm");
            rest = &rest[5..];
        }
    }
    out.push_str(rest);
    out
}

/// `<age>` padded to the width of `just now` or `<n><unit> ago`, at word
/// boundaries.
fn ages(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let boundary = i == 0 || !is_word(chars[i - 1]);
        if boundary && let Some(len) = age_at(&chars[i..]) {
            let end = i + len;
            if end == chars.len() || !is_word(chars[end]) {
                out.push_str(&format!("{:<width$}", "<age>", width = len));
                i = end;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

fn age_at(chars: &[char]) -> Option<usize> {
    let just: Vec<char> = "just now".chars().collect();
    if chars.starts_with(&just) {
        return Some(just.len());
    }
    let digits = chars.iter().take_while(|c| c.is_ascii_digit()).count();
    if digits == 0 {
        return None;
    }
    let unit = *chars.get(digits)?;
    if !matches!(unit, 's' | 'm' | 'h' | 'd') {
        return None;
    }
    let ago: Vec<char> = " ago".chars().collect();
    chars[digits + 1..]
        .starts_with(&ago)
        .then_some(digits + 1 + ago.len())
}

/// The fleet's compact age (`now`, `2m`, `3h`) between a space and two:
/// `<a>` for three characters, `<>` for two.
fn fleet_ages(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if i > 0
            && chars[i - 1] == ' '
            && let Some(len) = fleet_age_at(&chars[i..])
            && chars.get(i + len) == Some(&' ')
            && chars.get(i + len + 1) == Some(&' ')
        {
            out.push_str(if len == 3 { "<a>" } else { "<>" });
            i += len;
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

fn fleet_age_at(chars: &[char]) -> Option<usize> {
    if chars.starts_with(&['n', 'o', 'w']) {
        return Some(3);
    }
    let digits = chars.iter().take_while(|c| c.is_ascii_digit()).count();
    if !(1..=2).contains(&digits) {
        return None;
    }
    matches!(chars.get(digits), Some('m' | 'h' | 'd')).then_some(digits + 1)
}

/// A served net's scratch suffix after `aj-` or `testnet`: one to eight
/// word characters not followed by another, each masked as `x`.
fn scratch(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    'scan: while i < chars.len() {
        for prefix in ["aj-", "testnet"] {
            let p: Vec<char> = prefix.chars().collect();
            if chars[i..].starts_with(&p) {
                let start = i + p.len();
                let run = chars[start..].iter().take_while(|c| is_word(**c)).count();
                if run >= 1 {
                    // The greedy one-to-eight, then not followed by a word
                    // character: a longer run backs off to none at all.
                    let take = run.min(8);
                    if take == run {
                        out.extend(&p);
                        out.extend(std::iter::repeat_n('x', take));
                        i = start + take;
                        continue 'scan;
                    }
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// What is left of a scratch suffix after a path's middle was cut: the
/// word characters between `…` and the next `/`, masked as `x`.
fn scratch_tail(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    'scan: while i < chars.len() {
        for prefix in ["aj-", "testnet"] {
            let p: Vec<char> = prefix.chars().collect();
            if chars[i..].starts_with(&p) {
                let word = chars[i + p.len()..]
                    .iter()
                    .take_while(|c| is_word(**c))
                    .count();
                let ellipsis = i + p.len() + word;
                if chars.get(ellipsis) == Some(&'…') {
                    let tail = chars[ellipsis + 1..]
                        .iter()
                        .take_while(|c| is_word(**c))
                        .count();
                    if tail >= 1 && chars.get(ellipsis + 1 + tail) == Some(&'/') {
                        out.extend(&chars[i..=ellipsis]);
                        out.extend(std::iter::repeat_n('x', tail));
                        i = ellipsis + 1 + tail;
                        continue 'scan;
                    }
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// `text` with the physical `prefix` spelled `spelled` (the same width),
/// whole or the piece of it a dialog left showing on either side.
pub(crate) fn spell_root(text: &str, prefix: &str, spelled: &str, tail: &str) -> String {
    if prefix == spelled {
        return text.to_owned();
    }
    let mut text = text.replace(prefix, spelled);
    let p: Vec<char> = prefix.chars().collect();
    let s: Vec<char> = spelled.chars().collect();
    for size in (4..p.len()).rev() {
        // Cut on the right: the prefix's start, then something it does not
        // continue with.
        let head: String = p[..size].iter().collect();
        let next = p[size];
        let spelled_head: String = s[..size].iter().collect();
        let mut out = String::new();
        let mut rest = text.as_str();
        while let Some(at) = rest.find(&head) {
            let after = &rest[at + head.len()..];
            out.push_str(&rest[..at]);
            if after.starts_with(next) {
                out.push_str(&head);
            } else {
                out.push_str(&spelled_head);
            }
            rest = after;
        }
        out.push_str(rest);
        text = out;
        // Cut on the left: the prefix's end, then the rest of the root.
        let end: String = p[p.len() - size..].iter().collect();
        let spelled_end: String = s[s.len() - size..].iter().collect();
        text = text.replace(&format!("{end}{tail}"), &format!("{spelled_end}{tail}"));
    }
    text
}

/// `physical` spelled as the shorter `logical` everywhere, its dropped
/// cells coming back as blanks in the first gap of two or more blanks
/// after it, else at the row's end.
pub(crate) fn respell(
    texts: Vec<String>,
    styles: Vec<Vec<String>>,
    physical: &str,
    logical: &str,
) -> (Vec<String>, Vec<Vec<String>>) {
    let drop = physical.chars().count() - logical.chars().count();
    let mut out_texts = Vec::new();
    let mut out_styles = Vec::new();
    for (mut text, mut row) in texts.into_iter().zip(styles) {
        while let Some(start) = text.find(physical) {
            let cell = cells(&text[..start]);
            text = format!(
                "{}{}{}",
                &text[..start],
                logical,
                &text[start + physical.len()..]
            );
            row.drain(cell..cell + drop);
            match text[start + logical.len()..].find("  ") {
                None => {
                    let last = row.last().cloned().unwrap_or_default();
                    row.extend(std::iter::repeat_n(last, drop));
                }
                Some(gap) => {
                    let gap = start + logical.len() + gap;
                    let at = cells(&text[..gap]);
                    text = format!("{}{}{}", &text[..gap], " ".repeat(drop), &text[gap..]);
                    let fill = row[at].clone();
                    row.splice(at..at, std::iter::repeat_n(fill, drop));
                }
            }
        }
        out_texts.push(text);
        out_styles.push(row);
    }
    (out_texts, out_styles)
}

/// A duration measured on the run's own clock (`380ms`, `1.2s`, `1m 4s`)
/// after a space and before ` · ` or the line's end, as character indices.
fn durations(chars: &[char]) -> Vec<(usize, usize)> {
    let mut found = Vec::new();
    let mut i = 1;
    while i < chars.len() {
        if chars[i - 1] == ' '
            && let Some(len) = duration_at(&chars[i..])
        {
            let end = i + len;
            let closes = end == chars.len() || chars[end..].starts_with(&[' ', '·', ' ']);
            if closes {
                found.push((i, end));
                i = end;
                continue;
            }
        }
        i += 1;
    }
    found
}

fn duration_at(chars: &[char]) -> Option<usize> {
    let digits = |from: usize| {
        chars[from..]
            .iter()
            .take_while(|c| c.is_ascii_digit())
            .count()
    };
    let whole = digits(0);
    if whole == 0 {
        return None;
    }
    // `1m 4s`
    if chars.get(whole) == Some(&'m') && chars.get(whole + 1) == Some(&' ') {
        let seconds = digits(whole + 2);
        if seconds > 0 && chars.get(whole + 2 + seconds) == Some(&'s') {
            return Some(whole + 2 + seconds + 1);
        }
    }
    let mut at = whole;
    if chars.get(at) == Some(&'.') {
        let fraction = digits(at + 1);
        if fraction > 0 {
            at += 1 + fraction;
        }
    }
    if chars[at..].starts_with(&['m', 's']) {
        return Some(at + 2);
    }
    (chars.get(at) == Some(&'s')).then_some(at + 1)
}

/// Each duration becomes `<t>`. Right-aligned meta keeps its place, the
/// blanks before it absorbing the difference; a duration after one space
/// takes the mask's own width, and the row keeps the pane's width.
pub(crate) fn mask_durations(
    texts: Vec<String>,
    styles: Vec<Vec<String>>,
) -> (Vec<String>, Vec<Vec<String>>) {
    let mut out_texts = Vec::new();
    let mut out_styles = Vec::new();
    for (text, mut row) in texts.into_iter().zip(styles) {
        let width = row.len();
        let mut chars: Vec<char> = text.chars().collect();
        for (start, end) in durations(&chars).into_iter().rev() {
            let mut blank = start;
            while blank > 0 && chars[blank - 1] == ' ' {
                blank -= 1;
            }
            let extra = if start - blank > 1 {
                (end - start) as isize - 3
            } else {
                0
            };
            if (start - blank) as isize + extra < 1 {
                continue;
            }
            let cell = |index: usize| chars[..index].iter().map(|c| cell_width(*c)).sum::<usize>();
            let (c_blank, c_start, c_end) = (cell(blank), cell(start), cell(end));
            let fill = if c_blank < c_start {
                row[c_blank].clone()
            } else {
                row[c_start].clone()
            };
            let pad = ((c_start - c_blank) as isize + extra).max(0) as usize;
            let masked_style = row[c_start].clone();
            let replacement: Vec<String> = std::iter::repeat_n(fill, pad)
                .chain(std::iter::repeat_n(masked_style, 3))
                .collect();
            row.splice(c_blank..c_end, replacement);
            let spaces = ((start - blank) as isize + extra).max(0) as usize;
            let mut next: Vec<char> = chars[..blank].to_vec();
            next.extend(std::iter::repeat_n(' ', spaces));
            next.extend("<t>".chars());
            next.extend(&chars[end..]);
            chars = next;
        }
        if let Some(last) = row.last().cloned() {
            row.resize(width, last);
        }
        out_texts.push(chars.into_iter().collect());
        out_styles.push(row);
    }
    (out_texts, out_styles)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_styled_capture_reads_as_text_and_one_style_per_cell() {
        let (texts, styles) = parse_styled("\x1b[1;38;5;3mhi\x1b[0m there", 10);
        assert_eq!(texts, ["hi there"]);
        assert_eq!(styles[0][0], "{\"bold\": true, \"fg\": \"5;3\"}");
        assert_eq!(styles[0][2], "{}");
        assert_eq!(styles[0].len(), 10);
        assert_eq!(
            style_map(&styles),
            "aa........\n--- legend\na = {\"bold\": true, \"fg\": \"5;3\"}\n"
        );
    }

    #[test]
    fn what_moves_with_the_clock_or_the_run_is_masked() {
        assert_eq!(normalize("  sent 3m ago  "), "  sent <age>   ");
        assert_eq!(normalize("  keeper  now  "), "  keeper  <a>  ");
        assert_eq!(normalize("  keeper  2m  "), "  keeper  <>  ");
        assert_eq!(normalize("› hello       12:04"), "› hello       hh:mm");
        assert_eq!(
            normalize("limit · resets 09:30 ok"),
            "limit · resets hh:mm ok"
        );
        assert_eq!(normalize("/tmp/aj-ab12_xY/x"), "/tmp/aj-xxxxxxx/x");
        assert_eq!(
            normalize("/tmp/testnetAbc…de/work"),
            "/tmp/testnetxxx…xx/work"
        );
    }

    #[test]
    fn a_duration_is_masked_keeping_the_row_width() {
        let (texts, styles) = parse_styled("ran  1.2s", 9);
        let (texts, styles) = mask_durations(texts, styles);
        assert_eq!(texts, ["ran   <t>"]);
        assert_eq!(styles[0].len(), 9);
        let (texts, _) = mask_durations(
            vec!["took 380ms · done".into()],
            vec![vec!["{}".into(); 17]],
        );
        assert_eq!(texts, ["took <t> · done"]);
    }

    /// A row styled `styled` where it has text and `.` after it.
    fn cells(text: &str, width: usize, styled: &str) -> Vec<String> {
        let used = text.trim_end().chars().count();
        (0..width)
            .map(|at| if at < used { styled } else { "." }.to_owned())
            .collect()
    }

    #[test]
    fn a_duration_in_a_sentence_keeps_the_rows_width_whatever_its_own() {
        let mut rows = std::collections::BTreeSet::new();
        for duration in ["5ms", "12ms", "380ms", "1s", "1m 4s"] {
            let text = format!("  Ran deploy --check · {duration} · allowed");
            let (texts, styles) = mask_durations(vec![text.clone()], vec![cells(&text, 40, "a")]);
            assert_eq!(
                texts,
                ["  Ran deploy --check · <t> · allowed"],
                "{duration}"
            );
            assert_eq!(styles[0].len(), 40, "{duration}");
            rows.insert(styles[0].concat());
        }
        assert_eq!(rows.len(), 1, "{rows:?}");
    }

    #[test]
    fn a_duration_ending_a_row_keeps_the_rows_width() {
        for duration in ["380ms", "1.2s", "9s"] {
            let text = format!("    Worked {duration}");
            let (texts, styles) = mask_durations(vec![text.clone()], vec![cells(&text, 30, "a")]);
            assert_eq!(texts, ["    Worked <t>"], "{duration}");
            assert_eq!(
                styles[0].concat(),
                "a".repeat(14) + &".".repeat(16),
                "{duration}"
            );
        }
    }

    #[test]
    fn right_aligned_meta_keeps_its_place() {
        let text = format!("  ✔ Ran make{}380ms", " ".repeat(10));
        let row: Vec<String> = [("a", 12), (".", 10), ("m", 5), (".", 3)]
            .into_iter()
            .flat_map(|(style, count)| std::iter::repeat_n(style.to_owned(), count))
            .collect();
        let (texts, styles) = mask_durations(vec![text], vec![row]);
        assert_eq!(texts, [format!("  ✔ Ran make{}<t>", " ".repeat(12))]);
        let expected: Vec<String> = [("a", 12), (".", 12), ("m", 3), (".", 3)]
            .into_iter()
            .flat_map(|(style, count)| std::iter::repeat_n(style.to_owned(), count))
            .collect();
        assert_eq!(styles[0], expected);
    }

    #[test]
    fn macos_paths_read_as_linux_ones_whole_or_cut_by_a_dialog() {
        let spell = |text: &str| spell_root(text, "/private/tmp", "/tmp/journey", "/aj-terminal");
        assert_eq!(
            spell("Claude · /private/tmp/aj-terminal/net/desk/work"),
            "Claude · /tmp/journey/aj-terminal/net/desk/work"
        );
        // Cut on the right by a dialog, and on the left.
        assert_eq!(
            spell("Claude · /private╭─ Hosts"),
            "Claude · /tmp/jou╭─ Hosts"
        );
        assert_eq!(
            spell("│ Stop? │ate/tmp/aj-terminal/net"),
            "│ Stop? │journey/aj-terminal/net"
        );
        // Too short a piece to be sure of is left alone.
        assert_eq!(spell("a /pr b"), "a /pr b");
    }
}
