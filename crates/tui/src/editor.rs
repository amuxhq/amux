//! A text field with readline editing and attachments as atomic tokens.
//!
//! The draft is the wire's own shape: text with one placeholder per
//! attachment and the attachments in order, so sending it is handing both
//! over. A placeholder is one cursor position and one backspace removes it
//! with its attachment. Every kill lands in a single-slot kill buffer, so no
//! clearing key loses what it cleared.

use attachments::PLACEHOLDER;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use unicode_segmentation::UnicodeSegmentation as _;
use wire::{Attachment, InlineText, attachment};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Editor {
    text: String,
    attachments: Vec<Attachment>,
    /// A byte offset at a grapheme boundary: the cursor steps, and
    /// Backspace removes, whole characters as the person sees them (an
    /// accented letter, an emoji with its skin tone).
    cursor: usize,
    kill: Option<(String, Vec<Attachment>)>,
    pasted: u32,
    /// Typed characters show as bullets: a secret answer.
    pub secret: bool,
}

/// Whether the editor took the key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edit {
    Changed,
    Moved,
    Ignored,
}

impl Editor {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn attachments(&self) -> &[Attachment] {
        &self.attachments
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty() && self.attachments.is_empty()
    }

    /// The cursor as a count of characters before it, a placeholder one.
    pub fn cursor_chars(&self) -> usize {
        self.text[..self.cursor].chars().count()
    }

    /// Puts the cursor after `chars` characters, or at the end; a count
    /// that falls inside a grapheme puts it before that grapheme.
    pub fn set_cursor_chars(&mut self, chars: usize) {
        let at = self
            .text
            .char_indices()
            .nth(chars)
            .map_or(self.text.len(), |(at, _)| at);
        self.cursor = if at == self.text.len() {
            at
        } else {
            self.text
                .grapheme_indices(true)
                .map(|(start, _)| start)
                .take_while(|start| *start <= at)
                .last()
                .unwrap_or(0)
        };
    }

    /// Replaces the whole draft and puts the cursor at its end.
    pub fn set(&mut self, text: &str, attachments: Vec<Attachment>) {
        self.text = text.to_owned();
        self.attachments = attachments;
        self.cursor = self.text.len();
    }

    /// Takes the draft out, leaving the field empty; the kill buffer stays.
    pub fn take(&mut self) -> (String, Vec<Attachment>) {
        self.cursor = 0;
        (
            std::mem::take(&mut self.text),
            std::mem::take(&mut self.attachments),
        )
    }

    /// Takes the draft out to send it: without trailing whitespace, such
    /// as the space that follows a chip attached last.
    pub fn take_prompt(&mut self) -> (String, Vec<Attachment>) {
        let (mut text, attachments) = self.take();
        text.truncate(text.trim_end().len());
        (text, attachments)
    }

    /// Puts a draft back after what the field holds, on its own line.
    pub fn restore(&mut self, text: &str, attachments: Vec<Attachment>) {
        if self.is_empty() {
            self.set(text, attachments);
            return;
        }
        self.text.push('\n');
        self.text.push_str(text);
        self.attachments.extend(attachments);
        self.cursor = self.text.len();
    }

    /// Clears the whole draft as one kill, so Ctrl+Y brings it back.
    pub fn kill_all(&mut self) -> bool {
        if self.is_empty() {
            return false;
        }
        let taken = self.take();
        self.kill = Some(taken);
        true
    }

    pub fn insert_str(&mut self, text: &str) {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        let clean: String = text.chars().filter(|c| *c != PLACEHOLDER).collect();
        self.text.insert_str(self.cursor, &clean);
        self.cursor += clean.len();
    }

    /// An attachment at the cursor.
    pub fn insert_attachment(&mut self, attachment: Attachment) {
        let index = self.index_at(self.cursor);
        self.attachments.insert(index, attachment);
        self.text.insert(self.cursor, PLACEHOLDER);
        self.cursor += PLACEHOLDER.len_utf8();
    }

    /// An attachment the person adds, at the cursor, followed by a real
    /// space unless whitespace already follows, so the next chip or word
    /// does not touch it.
    pub fn attach(&mut self, attachment: Attachment) {
        self.insert_attachment(attachment);
        if !self.text[self.cursor..]
            .chars()
            .next()
            .is_some_and(char::is_whitespace)
        {
            self.text.insert(self.cursor, ' ');
        }
        self.cursor += 1;
    }

    /// The index of the first attachment `matches` accepts.
    pub fn find_attachment(&self, matches: impl Fn(&Attachment) -> bool) -> Option<usize> {
        self.attachments.iter().position(matches)
    }

    /// Swaps the attachment at `index` in place; its token stays put.
    pub fn replace_attachment(&mut self, index: usize, attachment: Attachment) {
        if let Some(slot) = self.attachments.get_mut(index) {
            *slot = attachment;
        }
    }

    /// Removes the attachment at `index` with its token, keeping the cursor
    /// on the same text.
    pub fn remove_attachment(&mut self, index: usize) {
        let Some((at, _)) = self.text.match_indices(PLACEHOLDER).nth(index) else {
            return;
        };
        // The token and the space after it, which [`Editor::attach`] adds:
        // a draft left holding only that space would not read as empty.
        let mut len = PLACEHOLDER.len_utf8();
        if self.text[at + len..].starts_with(' ') {
            len += 1;
        }
        self.text.replace_range(at..at + len, "");
        self.attachments.remove(index);
        if self.cursor >= at + len {
            self.cursor -= len;
        } else if self.cursor > at {
            self.cursor = at;
        }
    }

    /// A bracketed or clipboard paste: long text becomes one token, as every
    /// client's paste does.
    /// Terminals deliver a paste's line breaks as carriage returns, so they
    /// are read as newlines before the lines are counted.
    pub fn paste(&mut self, text: &str) {
        let text = &text.replace("\r\n", "\n").replace('\r', "\n");
        if ui_view::pastes_as_attachment(text) {
            self.pasted += 1;
            self.attach(Attachment {
                of: Some(attachment::Of::Text(InlineText {
                    name: format!("pasted-{}", self.pasted),
                    text: text.to_owned(),
                })),
            });
        } else {
            self.insert_str(text);
        }
    }

    /// How many attachments sit before byte offset `at`.
    fn index_at(&self, at: usize) -> usize {
        self.text[..at].matches(PLACEHOLDER).count()
    }

    /// Removes `[from, to)` and returns it with its attachments.
    fn cut(&mut self, from: usize, to: usize) -> (String, Vec<Attachment>) {
        let first = self.index_at(from);
        let count = self.text[from..to].matches(PLACEHOLDER).count();
        let removed: Vec<Attachment> = self.attachments.drain(first..first + count).collect();
        let text: String = self.text.drain(from..to).collect();
        self.cursor = from;
        (text, removed)
    }

    fn kill_range(&mut self, from: usize, to: usize) -> Edit {
        if from == to {
            return Edit::Ignored;
        }
        let killed = self.cut(from, to);
        self.kill = Some(killed);
        Edit::Changed
    }

    fn yank(&mut self) -> Edit {
        let Some((text, attachments)) = self.kill.clone() else {
            return Edit::Ignored;
        };
        let mut attachments = attachments.into_iter();
        for c in text.chars() {
            if c == PLACEHOLDER {
                if let Some(attachment) = attachments.next() {
                    self.insert_attachment(attachment);
                }
            } else {
                self.text.insert(self.cursor, c);
                self.cursor += c.len_utf8();
            }
        }
        Edit::Changed
    }

    fn prev(&self, at: usize) -> usize {
        self.text[..at]
            .graphemes(true)
            .next_back()
            .map_or(0, |grapheme| at - grapheme.len())
    }

    fn next(&self, at: usize) -> usize {
        self.text[at..]
            .graphemes(true)
            .next()
            .map_or(at, |grapheme| at + grapheme.len())
    }

    fn line_start(&self, at: usize) -> usize {
        self.text[..at].rfind('\n').map_or(0, |i| i + 1)
    }

    fn line_end(&self, at: usize) -> usize {
        self.text[at..]
            .find('\n')
            .map_or(self.text.len(), |i| at + i)
    }

    fn word_left(&self, at: usize) -> usize {
        let before = &self.text[..at];
        let trimmed = before.trim_end_matches(|c: char| c.is_whitespace());
        trimmed.rfind(|c: char| c.is_whitespace()).map_or(0, |i| {
            i + trimmed[i..].chars().next().map_or(1, char::len_utf8)
        })
    }

    fn word_right(&self, at: usize) -> usize {
        let after = &self.text[at..];
        let skipped = after.len() - after.trim_start().len();
        let rest = &after[skipped..];
        at + skipped + rest.find(char::is_whitespace).unwrap_or(rest.len())
    }

    /// Moves a line up or down, keeping the column in characters.
    fn vertical(&mut self, down: bool) -> Edit {
        let start = self.line_start(self.cursor);
        let column = self.text[start..self.cursor].chars().count();
        let target = if down {
            let end = self.line_end(self.cursor);
            if end >= self.text.len() {
                return Edit::Ignored;
            }
            end + 1
        } else {
            if start == 0 {
                return Edit::Ignored;
            }
            self.line_start(start - 1)
        };
        let end = self.line_end(target);
        let mut at = target;
        for _ in 0..column {
            if at >= end {
                break;
            }
            at = self.next(at);
        }
        self.cursor = at;
        Edit::Moved
    }

    /// Whether the cursor is on the draft's first line: Up there leaves
    /// the field.
    pub fn on_first_line(&self) -> bool {
        self.line_start(self.cursor) == 0
    }

    /// Applies one key. Enter is never the editor's: the caller decides
    /// whether it sends, submits or advances.
    pub fn key(&mut self, key: KeyEvent) -> Edit {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('j') if ctrl => {
                self.insert_str("\n");
                Edit::Changed
            }
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.insert_str("\n");
                Edit::Changed
            }
            KeyCode::Char('b') if ctrl => self.move_to(self.prev(self.cursor)),
            KeyCode::Char('f') if ctrl => self.move_to(self.next(self.cursor)),
            KeyCode::Char('p') if ctrl => self.vertical(false),
            KeyCode::Char('n') if ctrl => self.vertical(true),
            KeyCode::Char('e') if ctrl => self.move_to(self.line_end(self.cursor)),
            KeyCode::Char('w') if ctrl => self.kill_range(self.word_left(self.cursor), self.cursor),
            KeyCode::Char('u') if ctrl => {
                self.kill_range(self.line_start(self.cursor), self.cursor)
            }
            KeyCode::Char('k') if ctrl => self.kill_range(self.cursor, self.line_end(self.cursor)),
            KeyCode::Char('d') if ctrl => {
                let next = self.next(self.cursor);
                if next == self.cursor {
                    return Edit::Ignored;
                }
                self.cut(self.cursor, next);
                Edit::Changed
            }
            KeyCode::Char('y') if ctrl => self.yank(),
            KeyCode::Char(_) if ctrl => Edit::Ignored,
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::ALT) => {
                let mut buf = [0u8; 4];
                self.insert_str(c.encode_utf8(&mut buf));
                Edit::Changed
            }
            KeyCode::Backspace => {
                if self.cursor == 0 {
                    return Edit::Ignored;
                }
                let prev = self.prev(self.cursor);
                self.cut(prev, self.cursor);
                Edit::Changed
            }
            KeyCode::Delete => {
                let next = self.next(self.cursor);
                if next == self.cursor {
                    return Edit::Ignored;
                }
                self.cut(self.cursor, next);
                Edit::Changed
            }
            KeyCode::Left if ctrl => self.move_to(self.word_left(self.cursor)),
            KeyCode::Right if ctrl => self.move_to(self.word_right(self.cursor)),
            KeyCode::Left => self.move_to(self.prev(self.cursor)),
            KeyCode::Right => self.move_to(self.next(self.cursor)),
            KeyCode::Up => self.vertical(false),
            KeyCode::Down => self.vertical(true),
            KeyCode::Home => self.move_to(self.line_start(self.cursor)),
            KeyCode::End => self.move_to(self.line_end(self.cursor)),
            _ => Edit::Ignored,
        }
    }

    fn move_to(&mut self, at: usize) -> Edit {
        if at == self.cursor {
            return Edit::Ignored;
        }
        self.cursor = at;
        Edit::Moved
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn typed(text: &str) -> Editor {
        let mut editor = Editor::default();
        editor.insert_str(text);
        editor
    }

    fn image(name: &str) -> Attachment {
        Attachment {
            of: Some(attachment::Of::Image(wire::BlobRef {
                hash: vec![1; 32],
                name: name.into(),
                mime: "image/png".into(),
                size: 10,
            })),
        }
    }

    #[test]
    fn an_attachment_is_one_position_and_one_backspace() {
        let mut editor = typed("see ");
        editor.insert_attachment(image("a.png"));
        editor.insert_str(" here");
        assert_eq!(editor.attachments().len(), 1);
        for _ in 0..5 {
            editor.key(key(KeyCode::Left));
        }
        assert_eq!(editor.key(key(KeyCode::Backspace)), Edit::Changed);
        assert!(editor.attachments().is_empty());
        assert_eq!(editor.text(), "see  here");
    }

    #[test]
    fn attachments_keep_their_order_when_inserted_between_others() {
        let mut editor = Editor::default();
        editor.insert_attachment(image("a.png"));
        editor.insert_attachment(image("c.png"));
        editor.key(key(KeyCode::Left));
        editor.insert_attachment(image("b.png"));
        let names: Vec<String> = editor
            .attachments()
            .iter()
            .map(|a| match &a.of {
                Some(attachment::Of::Image(blob)) => blob.name.clone(),
                _ => String::new(),
            })
            .collect();
        assert_eq!(names, ["a.png", "b.png", "c.png"]);
    }

    #[test]
    fn every_kill_can_be_yanked_back_with_its_attachments() {
        let mut editor = typed("keep ");
        editor.insert_attachment(image("a.png"));
        editor.insert_str(" words");
        assert!(editor.kill_all());
        assert!(editor.is_empty());
        assert_eq!(editor.key(ctrl('y')), Edit::Changed);
        assert_eq!(editor.text(), format!("keep {PLACEHOLDER} words"));
        assert_eq!(editor.attachments().len(), 1);

        editor.key(ctrl('w'));
        assert_eq!(editor.text(), format!("keep {PLACEHOLDER} "));
        editor.key(ctrl('u'));
        assert!(editor.is_empty());
        editor.key(ctrl('y'));
        assert_eq!(editor.attachments().len(), 1);
    }

    #[test]
    fn a_long_paste_becomes_one_text_token() {
        let mut editor = Editor::default();
        editor.paste(&"line\n".repeat(ui_view::PASTE_LINES));
        // The token, and the space a chip keeps after itself.
        assert_eq!(editor.text(), format!("{PLACEHOLDER} "));
        assert!(matches!(
            &editor.attachments()[0].of,
            Some(attachment::Of::Text(text)) if text.name == "pasted-1"
        ));
        editor.paste("short");
        assert_eq!(editor.text(), format!("{PLACEHOLDER} short"));
        // As a terminal delivers it: carriage returns between the lines.
        editor.paste(&"line\r".repeat(ui_view::PASTE_LINES));
        assert!(matches!(
            &editor.attachments()[1].of,
            Some(attachment::Of::Text(text)) if text.text == "line\n".repeat(ui_view::PASTE_LINES)
        ));
    }

    #[test]
    fn ctrl_j_is_the_newline_and_up_moves_between_lines() {
        let mut editor = typed("first");
        editor.key(ctrl('j'));
        editor.insert_str("second");
        assert!(!editor.on_first_line());
        assert_eq!(editor.key(key(KeyCode::Up)), Edit::Moved);
        assert!(editor.on_first_line());
        assert_eq!(editor.key(key(KeyCode::Up)), Edit::Ignored);
    }

    #[test]
    fn restore_puts_a_draft_back_without_losing_what_is_typed() {
        let mut editor = typed("new words");
        editor.restore("old words", vec![]);
        assert_eq!(editor.text(), "new words\nold words");
    }
}
