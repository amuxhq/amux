//! Text as a person reads it, whichever client draws it: paths named from
//! the agent's folder, command output as a terminal shows it, names without
//! direction controls, and a markdown line read as words.

/// `path` relative to the agent's folder `cwd` when the file is inside it,
/// else whole. A file the agent works on is nearly always in its own
/// folder, and the absolute path repeats that folder at the front, where a
/// narrow screen has to cut it to show the file's name. Paths are the
/// host's, so either separator ends the folder.
pub(crate) fn within(cwd: &str, path: &str) -> String {
    let folder = cwd.trim_end_matches(['/', '\\']);
    if folder.is_empty() {
        return path.to_owned();
    }
    match path.strip_prefix(folder) {
        Some(rest) if rest.starts_with(['/', '\\']) && rest.len() > 1 => rest[1..].to_owned(),
        _ => path.to_owned(),
    }
}

/// Whether `c` changes the direction text is laid out in: the embeddings,
/// overrides and isolates, and the marks that steer neutral characters.
fn direction_control(c: char) -> bool {
    matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{200E}' | '\u{200F}' | '\u{061C}')
}

/// `text` without direction controls. A right-to-left override in a file
/// name makes `invoice\u{202E}fdp.exe` read `invoiceexe.pdf`; left in, each
/// renderer lays it out its own way, so the name is drawn as its letters,
/// in the order they were written.
pub(crate) fn without_direction_controls(text: &str) -> String {
    if !text.chars().any(direction_control) {
        return text.to_owned();
    }
    text.chars().filter(|c| !direction_control(*c)).collect()
}

/// A line of command output as a terminal leaves it on screen: colour and
/// other escape sequences are not text, and a carriage return moves back
/// to the line's start, so what follows writes over what was there (a
/// progress bar's last state, `aaa\rbbb` as `bbb`).
pub(crate) fn terminal_line(line: &str) -> String {
    if !line.contains(['\u{1b}', '\r', '\u{9b}']) {
        return line.to_owned();
    }
    let mut screen: Vec<char> = Vec::new();
    let mut at = 0;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => at = 0,
            '\u{1b}' => match chars.next() {
                // A control sequence: parameters and intermediates, then
                // one final byte.
                Some('[') => {
                    for c in chars.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&c) {
                            break;
                        }
                    }
                }
                // An operating-system command, ended by BEL or ESC \.
                Some(']' | 'P' | '_' | '^') => {
                    while let Some(c) = chars.next() {
                        if c == '\u{7}' {
                            break;
                        }
                        if c == '\u{1b}' && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                // A two-character escape: the next character is its own.
                _ => {}
            },
            '\u{9b}' => {
                for c in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&c) {
                        break;
                    }
                }
            }
            c => {
                if at < screen.len() {
                    screen[at] = c;
                } else {
                    screen.push(c);
                }
                at += 1;
            }
        }
    }
    screen.into_iter().collect()
}

/// Command output as a terminal shows it, line by line.
pub(crate) fn terminal_text(text: &str) -> String {
    if !text.contains(['\u{1b}', '\r', '\u{9b}']) {
        return text.to_owned();
    }
    text.split('\n')
        .map(terminal_line)
        .collect::<Vec<_>>()
        .join("\n")
}

/// One line of markdown as the words it reads: a link is its text, an
/// image its description, code and emphasis lose their marks, and a
/// heading, quote or bullet loses its lead. The line a row quotes from a
/// reply reads as the chat draws that reply.
pub(crate) fn markdown_words(line: &str) -> String {
    let mut line = line.trim();
    let hashes = line.len() - line.trim_start_matches('#').len();
    if (1..=6).contains(&hashes) && line[hashes..].starts_with(' ') {
        line = line[hashes..].trim_start();
    }
    while let Some(rest) = line.strip_prefix('>') {
        line = rest.trim_start();
    }
    for bullet in ["- ", "* ", "+ "] {
        if let Some(rest) = line.strip_prefix(bullet) {
            line = rest.trim_start();
            break;
        }
    }
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '\\' if chars
                .get(i + 1)
                .is_some_and(|next| next.is_ascii_punctuation()) =>
            {
                out.push(chars[i + 1]);
                i += 2;
            }
            '`' => {
                // A code span: its text as written, between runs of the
                // same number of backticks.
                let ticks = chars[i..].iter().take_while(|c| **c == '`').count();
                let open = i + ticks;
                let close = (open..chars.len()).find(|&at| {
                    chars[at..].iter().take_while(|c| **c == '`').count() == ticks
                        && (at == 0 || chars[at - 1] != '`')
                });
                match close {
                    Some(close) => {
                        let code: String = chars[open..close].iter().collect();
                        out.push_str(code.trim());
                        i = close + ticks;
                    }
                    None => {
                        out.extend(&chars[i..open]);
                        i = open;
                    }
                }
            }
            '!' if chars.get(i + 1) == Some(&'[') => match link(&chars, i + 1) {
                Some((text, end)) => {
                    out.push_str(&markdown_words(&text));
                    i = end;
                }
                None => {
                    out.push(c);
                    i += 1;
                }
            },
            '[' => match link(&chars, i) {
                Some((text, end)) => {
                    out.push_str(&markdown_words(&text));
                    i = end;
                }
                None => {
                    out.push(c);
                    i += 1;
                }
            },
            '<' => {
                // An autolink: the address itself.
                let end = chars[i + 1..].iter().position(|c| *c == '>' || *c == ' ');
                match end.map(|end| i + 1 + end) {
                    Some(end) if chars[end] == '>' => {
                        let inner: String = chars[i + 1..end].iter().collect();
                        if inner.contains("://") || inner.contains('@') {
                            out.push_str(&inner);
                            i = end + 1;
                        } else {
                            out.push(c);
                            i += 1;
                        }
                    }
                    _ => {
                        out.push(c);
                        i += 1;
                    }
                }
            }
            '*' | '_' | '~' => {
                let run = chars[i..].iter().take_while(|next| **next == c).count();
                let before = i.checked_sub(1).map(|at| chars[at]);
                let after = chars.get(i + run).copied();
                let opens = after.is_some_and(|after| !after.is_whitespace());
                let closes = before.is_some_and(|before| !before.is_whitespace());
                // Underscores inside a word, as in snake_case, are letters.
                let inside_word = c == '_'
                    && before.is_some_and(char::is_alphanumeric)
                    && after.is_some_and(char::is_alphanumeric);
                // A lone tilde is a letter; strikethrough takes two.
                let lone_tilde = c == '~' && run < 2;
                if (opens || closes) && !inside_word && !lone_tilde {
                    i += run;
                } else {
                    out.extend(&chars[i..i + run]);
                    i += run;
                }
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

/// A link at `open` (its `[`): its text and the index past its target,
/// when it is one.
fn link(chars: &[char], open: usize) -> Option<(String, usize)> {
    let mut depth = 0;
    let mut close = None;
    for (at, c) in chars.iter().enumerate().skip(open) {
        match c {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(at);
                    break;
                }
            }
            _ => {}
        }
    }
    let close = close?;
    if chars.get(close + 1) != Some(&'(') {
        return None;
    }
    let mut depth = 0;
    let end = chars
        .iter()
        .enumerate()
        .skip(close + 1)
        .find(|(_, c)| {
            match c {
                '(' => depth += 1,
                ')' => depth -= 1,
                _ => {}
            }
            depth == 0
        })?
        .0;
    Some((chars[open + 1..close].iter().collect(), end + 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_of_output_keeps_what_a_terminal_leaves_on_screen() {
        assert_eq!(terminal_line("\u{1b}[31mRED\u{1b}[0m TAB"), "RED TAB");
        assert_eq!(terminal_line("aaa\rbbb"), "bbb");
        assert_eq!(
            terminal_line("progress 10%\rprogress 100%"),
            "progress 100%"
        );
        assert_eq!(terminal_line("abcdef\rXY"), "XYcdef");
        assert_eq!(
            terminal_line("\u{1b}]8;;https://x.test\u{7}link\u{1b}]8;;\u{7}"),
            "link"
        );
        assert_eq!(terminal_line("plain"), "plain");
    }

    #[test]
    fn a_markdown_line_reads_as_its_words() {
        assert_eq!(
            markdown_words("Updated [README.md](/w/README.md) so its only line is `UPDATED`."),
            "Updated README.md so its only line is UPDATED."
        );
        assert_eq!(
            markdown_words("## **Done**: all *three* fixed"),
            "Done: all three fixed"
        );
        assert_eq!(
            markdown_words("- see <https://x.test>"),
            "see https://x.test"
        );
        assert_eq!(
            markdown_words("renamed snake_case_name"),
            "renamed snake_case_name"
        );
        assert_eq!(markdown_words("2 * 3 = 6 and ~5"), "2 * 3 = 6 and ~5");
        assert_eq!(markdown_words("![chart](a.png) attached"), "chart attached");
        assert_eq!(markdown_words("a [bracket] stays"), "a [bracket] stays");
        assert_eq!(markdown_words("``a ` b`` code"), "a ` b code");
        assert_eq!(markdown_words(r"\*not emphasis\*"), "*not emphasis*");
    }
}
