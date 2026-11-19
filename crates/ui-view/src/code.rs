//! Code coloured by kind of token, for the code a client draws itself: a
//! fenced block in an agent's markdown, which each client parses on its
//! own, and the fragment of a file an edit card shows. Every client calls
//! the same functions, so a block reads with the same runs on each; each
//! client maps a [`TokenKind`] to its own palette.
//!
//! The highlighting is the `highlight` crate's, through the [`Highlighter`]
//! the caller gives: in a client, the one the process shares
//! ([`Highlighter::shared`]); in a test, one whose background work the test
//! runs when it chooses ([`HeldJobs`](crate::HeldJobs)). Its grammars are built in
//! the background on first use, so a result can come back plain and not
//! [settled](HighlightedCode::settled): the client draws what it got and
//! asks again soon.

use std::time::Instant;

use highlight::{Highlighted, Highlighter, Language};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::review::LineKind;
use crate::rows::{PatchHunk, PatchLine};

/// What a run of code is, as far as colouring it goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum TokenKind {
    /// Names, operators and punctuation: the reading ink.
    Plain,
    Keyword,
    String,
    /// Numbers and other literal constants.
    Number,
    Type,
    Function,
    Comment,
    /// A diff's added and removed lines, in a block written as a diff.
    Inserted,
    Deleted,
}

/// A stretch of one line's text that is all one kind.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct CodeRun {
    pub text: String,
    pub kind: TokenKind,
}

/// Code as lines of runs, without their line endings. Neighbouring runs on
/// a line differ in kind; an empty line has none.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct HighlightedCode {
    pub lines: Vec<Vec<CodeRun>>,
    /// False while work under way would colour more of it: a grammar being
    /// built, or a block still streaming that has lines not yet passed
    /// over. A client that draws an unsettled result draws it again soon
    /// (within a tenth of a second), asking afresh.
    pub settled: bool,
}

/// A fenced code block, its language from the fence's info string (its
/// first word, through the alias table agents' words need: `zsh` is
/// shell, `jsonc` JSON). An unknown or missing language is drawn plain; a
/// `diff` block is coloured by its lines' `+` and `-`. One line comes back
/// for each of the code's lines, as `str::lines` splits them.
///
/// `streaming` is for the one block still being written: the last block
/// of a message still streaming in, whose closing fence has not arrived.
/// Its completed lines are highlighted in the background at a bounded rate
/// and the rest comes back plain, because highlighting it whole on every
/// line that arrives costs time quadratic in its length. Every other block,
/// a finished one, is highlighted once and kept by content.
pub fn highlight_block(
    highlighter: &Highlighter,
    code: &str,
    info: &str,
    streaming: bool,
) -> HighlightedCode {
    block(highlighter, code, info, streaming.then(Instant::now))
}

/// A file's change as an edit card shows it, coloured as the file at
/// `path` is: one result line for each of its lines, hunk after hunk. The
/// ask card and a landed edit's head both carry a file's change as its
/// hunks, and both are coloured here.
///
/// Each hunk is coloured as a fragment of its own, since its lines are not
/// neighbours of the next hunk's: a block comment or string one leaves open
/// does not colour the next. Within a hunk, removed lines are highlighted
/// with the context around them as the old file read, added lines with it
/// as the new file reads, so neither side's tokens run into the other's.
pub fn highlight_patch(
    highlighter: &Highlighter,
    path: &str,
    hunks: &[PatchHunk],
) -> HighlightedCode {
    let mut out = HighlightedCode {
        lines: Vec::new(),
        settled: true,
    };
    // A file without an extension is read by a `#!` on the patch's first
    // line, where an edit that writes or starts a script carries it; the
    // later hunks are of the same file.
    let first = hunks.iter().flat_map(|hunk| &hunk.lines).next();
    let language = first.and_then(|line| Language::from_file(path, &line.text));
    for hunk in hunks {
        let code = match language {
            Some(language) => fragment(highlighter, language, &hunk.lines),
            None => HighlightedCode {
                lines: hunk.lines.iter().map(|line| plain(&line.text)).collect(),
                settled: true,
            },
        };
        out.lines.extend(code.lines);
        out.settled &= code.settled;
    }
    out
}

/// `streaming` carries the time a streaming block is asked at, which a
/// test fixes.
fn block(
    highlighter: &Highlighter,
    code: &str,
    info: &str,
    streaming: Option<Instant>,
) -> HighlightedCode {
    let language = Language::from_fence(info);
    let highlighted = match streaming {
        Some(now) => highlighter.highlight_streaming(code, language, now),
        None => highlighter.highlight(code, language),
    };
    HighlightedCode {
        lines: highlighted.lines.iter().map(|line| runs(line)).collect(),
        settled: highlighted.settled,
    }
}

/// One hunk's lines, coloured in place: the old side (context and removed
/// lines) and the new side (context and added lines) each as code of its
/// own, the context taking the new side's colours.
fn fragment(highlighter: &Highlighter, language: Language, lines: &[PatchLine]) -> HighlightedCode {
    let mut out = HighlightedCode {
        lines: lines.iter().map(|line| plain(&line.text)).collect(),
        settled: true,
    };
    // The old side colours only its removed lines: the context it shares
    // with the new side takes the new side's colours.
    for (left_out, only) in [
        (LineKind::Added, Some(LineKind::Removed)),
        (LineKind::Removed, None),
    ] {
        let side: Vec<(usize, &PatchLine)> = lines
            .iter()
            .enumerate()
            .filter(|(_, line)| line.kind != left_out)
            .collect();
        if !side
            .iter()
            .any(|(_, line)| only.is_none_or(|only| line.kind == only))
        {
            continue;
        }
        let mut code = String::new();
        for (_, line) in &side {
            code.push_str(&line.text);
            code.push('\n');
        }
        let Highlighted {
            lines: coloured,
            settled,
        } = highlighter.highlight(&code, Some(language));
        out.settled &= settled;
        for ((at, line), runs_of) in side.iter().zip(coloured.iter()) {
            if only.is_none_or(|only| line.kind == only) {
                out.lines[*at] = runs(runs_of);
            }
        }
    }
    out
}

fn runs(line: &[highlight::Run]) -> Vec<CodeRun> {
    line.iter()
        .map(|run| CodeRun {
            text: run.text.clone(),
            kind: run.kind.into(),
        })
        .collect()
}

fn plain(text: &str) -> Vec<CodeRun> {
    if text.is_empty() {
        return Vec::new();
    }
    vec![CodeRun {
        text: text.to_owned(),
        kind: TokenKind::Plain,
    }]
}

impl From<highlight::TokenKind> for TokenKind {
    fn from(kind: highlight::TokenKind) -> TokenKind {
        match kind {
            highlight::TokenKind::Plain => TokenKind::Plain,
            highlight::TokenKind::Keyword => TokenKind::Keyword,
            highlight::TokenKind::String => TokenKind::String,
            highlight::TokenKind::Number => TokenKind::Number,
            highlight::TokenKind::Type => TokenKind::Type,
            highlight::TokenKind::Function => TokenKind::Function,
            highlight::TokenKind::Comment => TokenKind::Comment,
            highlight::TokenKind::Inserted => TokenKind::Inserted,
            highlight::TokenKind::Deleted => TokenKind::Deleted,
        }
    }
}

#[cfg(test)]
mod tests {
    use highlight::HeldJobs;

    use super::*;

    fn kinds(code: &HighlightedCode) -> Vec<Vec<(&str, TokenKind)>> {
        code.lines
            .iter()
            .map(|line| {
                line.iter()
                    .map(|run| (run.text.as_str(), run.kind))
                    .collect()
            })
            .collect()
    }

    fn has(code: &HighlightedCode, line: usize, text: &str, kind: TokenKind) -> bool {
        code.lines[line]
            .iter()
            .any(|run| run.text == text && run.kind == kind)
    }

    #[test]
    fn a_block_draws_plain_and_unsettled_until_its_grammar_is_built() {
        let queue = HeldJobs::default();
        let highlighter = queue.highlighter();
        let first = block(&highlighter, "fn main() {}\n", "rust", None);
        assert!(!first.settled);
        assert_eq!(
            kinds(&first),
            vec![vec![("fn main() {}", TokenKind::Plain)]]
        );
        queue.run();
        let ready = block(&highlighter, "fn main() {}\n", "rust", None);
        assert!(ready.settled);
        assert!(has(&ready, 0, "fn", TokenKind::Keyword));
    }

    #[test]
    fn an_unknown_fence_is_plain_and_a_diff_fence_keeps_its_signs() {
        let queue = HeldJobs::default();
        let highlighter = queue.highlighter();
        let plain = block(&highlighter, "puts 1\n", "ruby", None);
        assert!(plain.settled);
        assert_eq!(kinds(&plain), vec![vec![("puts 1", TokenKind::Plain)]]);
        let diff = block(&highlighter, "-a\n+b\n c\n", "diff", Some(Instant::now()));
        assert!(diff.settled);
        assert_eq!(
            kinds(&diff),
            vec![
                vec![("-a", TokenKind::Deleted)],
                vec![("+b", TokenKind::Inserted)],
                vec![(" c", TokenKind::Plain)],
            ]
        );
    }

    #[test]
    fn a_streaming_block_colours_its_completed_lines_in_the_background() {
        let queue = HeldJobs::default();
        let highlighter = queue.highlighter();
        highlighter.prepare(Language::Rust);
        let now = Instant::now();
        let first = block(&highlighter, "let a = 1;\nlet", "rust", Some(now));
        assert!(!first.settled, "a pass over the first line is under way");
        assert!(!has(&first, 0, "let", TokenKind::Keyword));
        queue.run();
        let passed = block(&highlighter, "let a = 1;\nlet", "rust", Some(now));
        assert!(passed.settled);
        assert!(has(&passed, 0, "let", TokenKind::Keyword));
        assert_eq!(kinds(&passed)[1], vec![("let", TokenKind::Plain)]);
    }

    fn line(kind: LineKind, text: &str) -> PatchLine {
        PatchLine {
            number: None,
            kind,
            text: text.to_owned(),
        }
    }

    fn hunk(lines: Vec<PatchLine>) -> PatchHunk {
        PatchHunk { start: None, lines }
    }

    #[test]
    fn an_edit_colours_each_side_as_its_file_reads() {
        let queue = HeldJobs::default();
        let highlighter = queue.highlighter();
        highlighter.prepare(Language::Python);
        // Read as one text, the added line would sit inside the string the
        // removed lines open and close.
        let lines = vec![
            line(LineKind::Context, "def f():"),
            line(LineKind::Removed, "    s = \"\"\""),
            line(LineKind::Added, "    n = 1"),
            line(LineKind::Removed, "    \"\"\""),
        ];
        let code = highlight_patch(&highlighter, "src/f.py", &[hunk(lines)]);
        assert!(code.settled);
        assert_eq!(code.lines.len(), 4);
        assert!(has(&code, 0, "def", TokenKind::Keyword));
        assert!(has(&code, 1, "\"\"\"", TokenKind::String));
        assert!(has(&code, 2, "1", TokenKind::Number));
        assert!(has(&code, 3, "    \"\"\"", TokenKind::String));
        let interleaved = block(
            &highlighter,
            "def f():\n    s = \"\"\"\n    n = 1\n    \"\"\"\n",
            "python",
            None,
        );
        assert!(!has(&interleaved, 2, "1", TokenKind::Number));
    }

    #[test]
    fn a_patch_takes_its_files_language_and_none_is_plain() {
        let queue = HeldJobs::default();
        let highlighter = queue.highlighter();
        highlighter.prepare(Language::Python);
        let hunks = [hunk(vec![line(LineKind::Added, "def b(): pass")])];
        let code = highlight_patch(&highlighter, "b.py", &hunks);
        assert!(code.settled);
        assert!(has(&code, 0, "def", TokenKind::Keyword));
        let plain = highlight_patch(&highlighter, "notes.txt", &hunks);
        assert!(plain.settled);
        assert_eq!(
            kinds(&plain),
            vec![vec![("def b(): pass", TokenKind::Plain)]]
        );
    }

    /// A patch's hunks are fragments of their own: the block comment the
    /// first leaves open does not run into the second, which is elsewhere
    /// in the file.
    #[test]
    fn a_patchs_hunks_are_coloured_apart() {
        let queue = HeldJobs::default();
        let highlighter = queue.highlighter();
        highlighter.prepare(Language::Rust);
        let hunks = [
            hunk(vec![
                line(LineKind::Context, "fn a() {}"),
                line(LineKind::Added, "/* opened here"),
            ]),
            hunk(vec![line(LineKind::Added, "fn far() {}")]),
        ];
        let code = highlight_patch(&highlighter, "src/lib.rs", &hunks);
        assert!(code.settled);
        assert_eq!(code.lines.len(), 3, "one result line for each patch line");
        assert!(has(&code, 1, "/* opened here", TokenKind::Comment));
        assert!(
            has(&code, 2, "fn", TokenKind::Keyword),
            "{:?}",
            kinds(&code)
        );
        // Read as one fragment, the second hunk would be inside the comment.
        let joined = [hunk(hunks.iter().flat_map(|h| h.lines.clone()).collect())];
        let joined = highlight_patch(&highlighter, "src/lib.rs", &joined);
        assert!(!has(&joined, 2, "fn", TokenKind::Keyword));
    }

    /// A script without an extension is read by the `#!` its patch starts
    /// with, in every hunk: the later ones are of the same file.
    #[test]
    fn a_scripts_later_hunks_take_the_language_its_first_line_names() {
        let queue = HeldJobs::default();
        let highlighter = queue.highlighter();
        highlighter.prepare(Language::Python);
        let hunks = [
            hunk(vec![line(LineKind::Context, "#!/usr/bin/env python3")]),
            hunk(vec![line(LineKind::Added, "def far(): pass")]),
        ];
        let code = highlight_patch(&highlighter, "bin/tool", &hunks);
        assert!(code.settled);
        assert!(
            has(&code, 1, "def", TokenKind::Keyword),
            "{:?}",
            kinds(&code)
        );
    }

    #[test]
    fn an_edit_whose_grammar_is_building_is_unsettled() {
        let queue = HeldJobs::default();
        let highlighter = queue.highlighter();
        let code = highlight_patch(
            &highlighter,
            "main.go",
            &[hunk(vec![line(LineKind::Added, "func main() {}")])],
        );
        assert!(!code.settled);
        assert_eq!(
            kinds(&code),
            vec![vec![("func main() {}", TokenKind::Plain)]]
        );
    }

    #[test]
    fn the_result_crosses_as_json() {
        let code = HighlightedCode {
            lines: vec![vec![CodeRun {
                text: "fn".into(),
                kind: TokenKind::Keyword,
            }]],
            settled: true,
        };
        let json = serde_json::to_string(&code).unwrap();
        assert_eq!(
            json,
            r#"{"lines":[[{"text":"fn","kind":"Keyword"}]],"settled":true}"#
        );
        assert_eq!(
            serde_json::from_str::<HighlightedCode>(&json).unwrap(),
            code
        );
    }
}
