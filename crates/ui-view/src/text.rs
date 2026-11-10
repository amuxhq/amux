//! Text as a person reads it, whichever client draws it: paths named from
//! the agent's folder, command output as a terminal shows it, and names
//! without direction controls.

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

pub(crate) use preview::without_direction_controls;

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
}
