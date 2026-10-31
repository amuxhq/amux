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

/// One line of markdown as the words it reads, through the parser the
/// chat draws replies with: a link is its text, an image its description,
/// and code, emphasis and a heading's, quote's or bullet's marks drop. The
/// line a row quotes from a reply reads as the chat draws that reply.
pub(crate) fn markdown_words(line: &str) -> String {
    use pulldown_cmark::{Event, Options, Parser};
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_TASKLISTS | Options::ENABLE_STRIKETHROUGH;
    let mut out = String::new();
    for event in Parser::new_ext(line, options) {
        if let Event::Text(words) | Event::Code(words) = event {
            out.push_str(&words);
        }
    }
    out
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
