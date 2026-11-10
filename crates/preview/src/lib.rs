//! The one line a fleet row quotes from what an agent said.
//!
//! The agent's host reads it from each reply as the reply is written, so a
//! row can say it with no stream open, and a client that holds the chat
//! reads it from the chat. Both read it here, so the line a stopped agent's
//! row keeps is the line its row showed while it was live.

use wire::Attachment;

/// The longest line kept, in characters: more than a row draws at the
/// largest text size, and short enough that the row's record stays small.
pub const MOST: usize = 300;

/// The first line of a reply as words, clipped to [`MOST`]: its markdown
/// read rather than shown, code fences skipped, and the attachment
/// placeholder dropped. A reply with no words names what it attached;
/// one with neither says nothing.
pub fn said_line(text: &str, attachments: &[Attachment]) -> Option<String> {
    text.lines()
        .filter(|line| !line.trim_start().starts_with("```"))
        .map(|line| {
            without_direction_controls(&markdown_words(&line.replace(attachments::PLACEHOLDER, "")))
        })
        .map(|line| line.trim().to_owned())
        .find(|line| !line.is_empty())
        .or_else(|| attachments.iter().find_map(attachment_name))
        .map(|line| clipped(&line))
}

fn clipped(line: &str) -> String {
    match line.char_indices().nth(MOST) {
        Some((at, _)) => format!("{}…", line[..at].trim_end()),
        None => line.to_owned(),
    }
}

fn attachment_name(attachment: &Attachment) -> Option<String> {
    use wire::attachment::Of;
    let name = match attachment.of.as_ref()? {
        Of::Image(blob) | Of::File(blob) => &blob.name,
        Of::Text(text) => &text.name,
        Of::Review(review) => &review.diff.as_ref()?.patch.as_ref()?.name,
    };
    (!name.is_empty()).then(|| without_direction_controls(name))
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
pub fn without_direction_controls(text: &str) -> String {
    if !text.chars().any(direction_control) {
        return text.to_owned();
    }
    text.chars().filter(|c| !direction_control(*c)).collect()
}

/// One line of markdown as the words it reads, through the parser the
/// chat draws replies with: a link is its text, an image its description,
/// and code, emphasis and a heading's, quote's or bullet's marks drop. The
/// line a row quotes from a reply reads as the chat draws that reply.
pub fn markdown_words(line: &str) -> String {
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
    fn a_reply_says_its_first_line_of_words() {
        let reply = "```rust\nfn main() {}\n```\n## Done, **all** green\nmore";
        assert_eq!(said_line(reply, &[]).as_deref(), Some("fn main() {}"));
        assert_eq!(
            said_line("## Done, **all** [green](x)\nmore", &[]).as_deref(),
            Some("Done, all green")
        );
        assert_eq!(said_line("  \n\n", &[]), None);
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

    #[test]
    fn a_long_line_is_clipped_on_a_character() {
        let line = "é".repeat(MOST + 10);
        let said = said_line(&line, &[]).unwrap();
        assert_eq!(said.chars().count(), MOST + 1);
        assert!(said.ends_with('…'));
        let short = "é".repeat(MOST);
        assert_eq!(said_line(&short, &[]).unwrap(), short);
    }
}
