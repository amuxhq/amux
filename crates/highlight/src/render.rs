//! Turning code into lines of runs: through a grammar's highlighter, as a
//! diff's prefixes, or plain.

use std::cell::RefCell;

use tree_sitter_highlight::{HighlightEvent, Highlighter};

use crate::grammar::{Configured, GrammarId, injected};
use crate::{Line, Run, TokenKind};

/// Lines being assembled from pieces of text, each piece one kind. A line
/// ends at each `\n` (a `\r` before it is dropped), and code ending in a
/// newline has no empty line after it, so the lines are those
/// [`str::lines`] gives.
struct Builder {
    lines: Vec<Line>,
    current: Line,
}

impl Builder {
    fn new() -> Self {
        Builder {
            lines: Vec::new(),
            current: Vec::new(),
        }
    }

    fn push(&mut self, text: &str, kind: TokenKind) {
        let mut pieces = text.split('\n');
        if let Some(first) = pieces.next() {
            self.piece(first, kind);
        }
        for piece in pieces {
            self.end_line();
            self.piece(piece, kind);
        }
    }

    fn piece(&mut self, text: &str, kind: TokenKind) {
        if text.is_empty() {
            return;
        }
        match self.current.last_mut() {
            Some(last) if last.kind == kind => last.text.push_str(text),
            _ => self.current.push(Run {
                text: text.to_owned(),
                kind,
            }),
        }
    }

    fn end_line(&mut self) {
        if let Some(last) = self.current.last_mut()
            && last.text.ends_with('\r')
        {
            last.text.pop();
            if last.text.is_empty() {
                self.current.pop();
            }
        }
        self.lines.push(std::mem::take(&mut self.current));
    }

    fn finish(mut self, code: &str) -> Vec<Line> {
        if !code.is_empty() && !code.ends_with('\n') {
            self.end_line();
        }
        self.lines
    }
}

/// The code in the reading ink, one run a line.
pub(crate) fn plain(code: &str) -> Vec<Line> {
    let mut lines = Builder::new();
    lines.push(code, TokenKind::Plain);
    lines.finish(code)
}

/// A unified diff coloured by its lines' prefixes. A `---` line followed
/// by a `+++` line names a file's two sides, so both are headers rather
/// than a removed and an added line.
pub(crate) fn diff(code: &str) -> Vec<Line> {
    let mut lines = Builder::new();
    let source: Vec<&str> = code.split_inclusive('\n').collect();
    let mut header_follows = false;
    for (at, line) in source.iter().enumerate() {
        let header = std::mem::take(&mut header_follows)
            || (line.starts_with("--- ")
                && source
                    .get(at + 1)
                    .is_some_and(|next| next.starts_with("+++ ")));
        header_follows = header && line.starts_with("--- ");
        let kind = match line.as_bytes().first() {
            _ if header => TokenKind::Plain,
            Some(b'+') => TokenKind::Inserted,
            Some(b'-') => TokenKind::Deleted,
            _ => TokenKind::Plain,
        };
        lines.push(line, kind);
    }
    lines.finish(code)
}

thread_local! {
    /// The highlighter keeps a parser and query cursors between calls; one
    /// per thread, since it is not shared.
    static HIGHLIGHTER: RefCell<Highlighter> = RefCell::new(Highlighter::new());
}

/// The code through `host`'s grammar. `config` finds a built grammar for
/// the host and for each grammar it injects; the caller has made sure they
/// are all built. Code the highlighter fails on is drawn plain.
pub(crate) fn highlighted<'a>(
    code: &str,
    host: GrammarId,
    config: impl Fn(GrammarId) -> Option<&'a Configured>,
) -> Vec<Line> {
    let Some(configured) = config(host) else {
        return plain(code);
    };
    let kinds = &configured.kinds;
    HIGHLIGHTER.with(|highlighter| {
        let Ok(mut highlighter) = highlighter.try_borrow_mut() else {
            return plain(code);
        };
        let events =
            highlighter.highlight(&configured.config, code.as_bytes(), None, None, |name| {
                injected(host, name)
                    .and_then(&config)
                    .map(|injected| &injected.config)
            });
        let Ok(events) = events else {
            return plain(code);
        };
        let mut lines = Builder::new();
        let mut stack: Vec<TokenKind> = Vec::new();
        for event in events {
            match event {
                Ok(HighlightEvent::HighlightStart(highlight)) => {
                    stack.push(kinds.get(highlight.0).copied().unwrap_or(TokenKind::Plain));
                }
                Ok(HighlightEvent::HighlightEnd) => {
                    stack.pop();
                }
                Ok(HighlightEvent::Source { start, end }) => {
                    let kind = stack.last().copied().unwrap_or(TokenKind::Plain);
                    match code.get(start..end) {
                        Some(text) => lines.push(text, kind),
                        None => return plain(code),
                    }
                }
                Err(_) => return plain(code),
            }
        }
        lines.finish(code)
    })
}

#[cfg(test)]
mod tests {
    use TokenKind::*;

    use super::*;

    fn runs(lines: &[Line]) -> Vec<Vec<(&str, TokenKind)>> {
        lines
            .iter()
            .map(|line| {
                line.iter()
                    .map(|run| (run.text.as_str(), run.kind))
                    .collect()
            })
            .collect()
    }

    #[test]
    fn plain_lines_are_the_lines_of_the_code() {
        assert!(plain("").is_empty());
        assert_eq!(runs(&plain("a\n")), vec![vec![("a", Plain)]]);
        assert_eq!(
            runs(&plain("a\nb")),
            vec![vec![("a", Plain)], vec![("b", Plain)]]
        );
        assert_eq!(
            runs(&plain("a\r\n\nb\n")),
            vec![vec![("a", Plain)], vec![], vec![("b", Plain)]]
        );
        for code in ["", "\n", "a", "a\n", "a\n\n", "\n\nb", "x\r\ny"] {
            assert_eq!(plain(code).len(), code.lines().count(), "{code:?}");
        }
    }

    #[test]
    fn a_diff_is_coloured_by_its_prefixes() {
        let code = "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1,2 +1,2 @@\n keep\n-old\n+new\n--- removed dashes\n\
                    diff --git a/y b/y\n--- a/y\n+++ b/y\n@@ -1 +1 @@\n-y\n";
        assert_eq!(
            runs(&diff(code)),
            vec![
                vec![("diff --git a/x b/x", Plain)],
                vec![("--- a/x", Plain)],
                vec![("+++ b/x", Plain)],
                vec![("@@ -1,2 +1,2 @@", Plain)],
                vec![(" keep", Plain)],
                vec![("-old", Deleted)],
                vec![("+new", Inserted)],
                vec![("--- removed dashes", Deleted)],
                vec![("diff --git a/y b/y", Plain)],
                vec![("--- a/y", Plain)],
                vec![("+++ b/y", Plain)],
                vec![("@@ -1 +1 @@", Plain)],
                vec![("-y", Deleted)],
            ]
        );
        // Lines an agent writes without headers or hunks are still added
        // and removed.
        assert_eq!(
            runs(&diff("-a\n+b")),
            vec![vec![("-a", Deleted)], vec![("+b", Inserted)]]
        );
    }
}
