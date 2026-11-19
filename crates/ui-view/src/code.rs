//! Code coloured by kind of token, for the code a client draws itself: a
//! fenced block in an agent's markdown, which each client parses on its
//! own, and the fragment of a file an edit card shows. Every client calls
//! the same functions, so a block reads with the same runs on each; each
//! client maps a [`TokenKind`] to its own palette.
//!
//! The highlighting is the `highlight` crate's, through the highlighter the
//! process shares. Its grammars are built in the background on first use,
//! so a result can come back plain and not [settled](HighlightedCode::settled):
//! the client draws what it got and asks again soon.

use std::time::{Duration, Instant};

use highlight::{Highlighted, Highlighter, Language};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::review::LineKind;
use crate::rows::{EditLine, PatchHead};

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
pub fn highlight_block(code: &str, info: &str, streaming: bool) -> HighlightedCode {
    block(
        Highlighter::shared(),
        code,
        info,
        streaming.then(Instant::now),
    )
}

/// An edit card's patch lines, one result line for each, coloured as the
/// file at `path` is. Removed lines are highlighted with the context around
/// them as the old file read, added lines with it as the new file reads, so
/// neither side's tokens run into the other's. A `File` line switches to
/// that file's path and a `Hunk` line starts a new fragment, since the
/// lines either side of it are not neighbours in the file; both come back
/// as empty lines, the client drawing its own words for them.
pub fn highlight_edit(path: &str, lines: &[EditLine]) -> HighlightedCode {
    edit(
        Highlighter::shared(),
        path,
        lines.iter().map(|line| match line {
            EditLine::File(file) => Piece::File(&file.path),
            EditLine::Hunk { .. } => Piece::Break,
            EditLine::Line(line) => Piece::Line(line.kind, &line.text),
        }),
    )
}

/// A landed edit's patch head, all of one file, one result line for each
/// of its lines, hunk after hunk; each hunk is coloured as a fragment of
/// its own, as [`highlight_edit`] colours a card's.
pub fn highlight_patch(path: &str, patch: &PatchHead) -> HighlightedCode {
    patch_code(Highlighter::shared(), path, patch)
}

/// Builds every grammar on the calling thread, for a client that wants
/// its first frame's code coloured and settled and can wait for it.
/// Returns how long it took.
pub fn preload_highlighting() -> Duration {
    Highlighter::shared().preload()
}

pub(crate) fn block(
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

pub(crate) fn patch_code(
    highlighter: &Highlighter,
    path: &str,
    patch: &PatchHead,
) -> HighlightedCode {
    let mut out = HighlightedCode {
        lines: Vec::new(),
        settled: true,
    };
    for hunk in &patch.hunks {
        let code = edit(
            highlighter,
            path,
            hunk.iter().map(|line| Piece::Line(line.kind, &line.text)),
        );
        out.lines.extend(code.lines);
        out.settled &= code.settled;
    }
    out
}

/// One line of an edit as the highlighting reads it.
pub(crate) enum Piece<'a> {
    /// Lines of another file start here.
    File(&'a str),
    /// The lines after this one do not follow on from those before.
    Break,
    Line(LineKind, &'a str),
}

pub(crate) fn edit<'a>(
    highlighter: &Highlighter,
    path: &'a str,
    pieces: impl Iterator<Item = Piece<'a>>,
) -> HighlightedCode {
    let mut out = HighlightedCode {
        lines: Vec::new(),
        settled: true,
    };
    let mut path = path;
    // The fragment being gathered: each line's place in `out`, its kind
    // and its text.
    let mut fragment: Vec<(usize, LineKind, &str)> = Vec::new();
    for piece in pieces {
        match piece {
            Piece::File(next) => {
                fragment_into(highlighter, path, &mut fragment, &mut out);
                path = next;
                out.lines.push(Vec::new());
            }
            Piece::Break => {
                fragment_into(highlighter, path, &mut fragment, &mut out);
                out.lines.push(Vec::new());
            }
            Piece::Line(kind, text) => {
                fragment.push((out.lines.len(), kind, text));
                out.lines.push(plain(text));
            }
        }
    }
    fragment_into(highlighter, path, &mut fragment, &mut out);
    out
}

/// Colours a fragment's lines in place in `out`: the old side (context
/// and removed lines) and the new side (context and added lines) each as
/// code of their own, the context taking the new side's colours.
fn fragment_into(
    highlighter: &Highlighter,
    path: &str,
    fragment: &mut Vec<(usize, LineKind, &str)>,
    out: &mut HighlightedCode,
) {
    let lines = std::mem::take(fragment);
    let Some((_, _, first)) = lines.first() else {
        return;
    };
    // A file without an extension is read by a `#!` on the fragment's first
    // line, where an edit that writes or starts a script carries it.
    let Some(language) = Language::from_file(path, first) else {
        return;
    };
    // The old side colours only its removed lines: the context it shares
    // with the new side takes the new side's colours.
    let old: Vec<&(usize, LineKind, &str)> = lines
        .iter()
        .filter(|(_, kind, _)| *kind != LineKind::Added)
        .collect();
    let new: Vec<&(usize, LineKind, &str)> = lines
        .iter()
        .filter(|(_, kind, _)| *kind != LineKind::Removed)
        .collect();
    for (side, only) in [(old, Some(LineKind::Removed)), (new, None)] {
        if !side
            .iter()
            .any(|(_, kind, _)| only.is_none_or(|only| *kind == only))
        {
            continue;
        }
        let mut code = String::new();
        for (_, _, text) in &side {
            code.push_str(text);
            code.push('\n');
        }
        let Highlighted {
            lines: coloured,
            settled,
        } = highlighter.highlight(&code, Some(language));
        out.settled &= settled;
        for ((at, kind, _), line) in side.iter().zip(coloured.iter()) {
            if only.is_none_or(|only| *kind == only) {
                out.lines[*at] = runs(line);
            }
        }
    }
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
    use std::sync::{Arc, Mutex};

    use highlight::Job;

    use super::*;
    use crate::rows::{FileChangeView, FileRow, PatchLine};

    /// Background work held until the test runs it, so what a caller sees
    /// before and after each job is fixed.
    #[derive(Clone, Default)]
    struct Queue(Arc<Mutex<Vec<Job>>>);

    impl Queue {
        fn highlighter(&self) -> Highlighter {
            let queue = self.clone();
            Highlighter::with_spawner(move |job| queue.0.lock().unwrap().push(job))
        }

        fn run(&self) {
            loop {
                let jobs = std::mem::take(&mut *self.0.lock().unwrap());
                if jobs.is_empty() {
                    return;
                }
                for job in jobs {
                    job();
                }
            }
        }
    }

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
        let queue = Queue::default();
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
        let queue = Queue::default();
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
        let queue = Queue::default();
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

    #[test]
    fn an_edit_colours_each_side_as_its_file_reads() {
        let queue = Queue::default();
        let highlighter = queue.highlighter();
        highlighter.prepare(Language::Python);
        // Read as one text, the added line would sit inside the string the
        // removed lines open and close.
        let lines = [
            Piece::Line(LineKind::Context, "def f():"),
            Piece::Line(LineKind::Removed, "    s = \"\"\""),
            Piece::Line(LineKind::Added, "    n = 1"),
            Piece::Line(LineKind::Removed, "    \"\"\""),
        ];
        let code = edit(&highlighter, "src/f.py", lines.into_iter());
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
    fn an_edit_of_several_files_takes_each_files_language() {
        let queue = Queue::default();
        let highlighter = queue.highlighter();
        highlighter.prepare(Language::Rust);
        highlighter.prepare(Language::Python);
        let file = |path: &str| FileRow {
            path: path.to_owned(),
            change: FileChangeView::Edited,
            added: 1,
            removed: 0,
            line: None,
        };
        let line = |text: &str| {
            EditLine::Line(PatchLine {
                number: None,
                kind: LineKind::Added,
                text: text.to_owned(),
            })
        };
        let lines = [
            EditLine::File(file("a.rs")),
            line("fn a() {}"),
            EditLine::Hunk {
                old_start: 9,
                new_start: 9,
            },
            line("struct B;"),
            EditLine::File(file("b.py")),
            line("def b(): pass"),
            EditLine::File(file("notes.txt")),
            line("def plain"),
        ];
        let pieces = lines.iter().map(|line| match line {
            EditLine::File(file) => Piece::File(&file.path),
            EditLine::Hunk { .. } => Piece::Break,
            EditLine::Line(line) => Piece::Line(line.kind, &line.text),
        });
        let code = edit(&highlighter, "a.rs", pieces);
        assert!(code.settled);
        assert_eq!(code.lines.len(), lines.len());
        assert!(code.lines[0].is_empty() && code.lines[2].is_empty());
        assert!(has(&code, 1, "fn", TokenKind::Keyword));
        assert!(has(&code, 3, "struct", TokenKind::Keyword));
        assert!(has(&code, 5, "def", TokenKind::Keyword));
        assert_eq!(kinds(&code)[7], vec![("def plain", TokenKind::Plain)]);
    }

    /// A landed edit's hunks are fragments of their own: the block comment
    /// the first leaves open does not run into the second, which is
    /// elsewhere in the file.
    #[test]
    fn a_patchs_hunks_are_coloured_apart() {
        let queue = Queue::default();
        let highlighter = queue.highlighter();
        highlighter.prepare(Language::Rust);
        let line = |number, kind, text: &str| PatchLine {
            number: Some(number),
            kind,
            text: text.to_owned(),
        };
        let patch = PatchHead {
            hunks: vec![
                vec![
                    line(3, LineKind::Context, "fn a() {}"),
                    line(4, LineKind::Added, "/* opened here"),
                ],
                vec![line(40, LineKind::Added, "fn far() {}")],
            ],
            more: 0,
        };
        let code = patch_code(&highlighter, "src/lib.rs", &patch);
        assert!(code.settled);
        assert_eq!(code.lines.len(), 3, "one result line for each patch line");
        assert!(has(&code, 1, "/* opened here", TokenKind::Comment));
        assert!(
            has(&code, 2, "fn", TokenKind::Keyword),
            "{:?}",
            kinds(&code)
        );
        // Read as one fragment, the second hunk would be inside the comment.
        let joined = PatchHead {
            hunks: vec![patch.lines().cloned().collect()],
            more: 0,
        };
        let joined = patch_code(&highlighter, "src/lib.rs", &joined);
        assert!(!has(&joined, 2, "fn", TokenKind::Keyword));
    }

    #[test]
    fn an_edit_whose_grammar_is_building_is_unsettled() {
        let queue = Queue::default();
        let highlighter = queue.highlighter();
        let code = edit(
            &highlighter,
            "main.go",
            [Piece::Line(LineKind::Added, "func main() {}")].into_iter(),
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
