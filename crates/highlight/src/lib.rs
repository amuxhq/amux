//! Syntax highlighting for the code amux shows: code and its language in,
//! lines of runs out, each run tagged with what kind of token it is. A
//! client maps each kind to a colour of its own palette, so code reads in
//! the person's colours on every client.
//!
//! The engine is tree-sitter, with a fixed set of grammars and the
//! highlight queries their crates ship ([`Language`] lists them). A
//! language is picked from a fence's word or a file's path and first line;
//! anything else is drawn plain, and a diff is coloured by its lines'
//! prefixes with no grammar at all.
//!
//! Two costs shape the interface. Building a grammar's configuration
//! compiles its queries, up to half a second for the largest, so it
//! happens once per grammar, on first use, in the background: until then a
//! call returns the code plain and not [settled](Highlighted::settled), and
//! the caller draws it plain and draws again soon. And tree-sitter's
//! highlighter parses from scratch on every call, so a block still
//! streaming in is not highlighted on every line that arrives:
//! [`Highlighter::highlight_streaming`] passes over its completed lines in
//! the background at a bounded rate and fills in the rest plain.
//!
//! ```
//! use highlight::{Highlighter, Language, TokenKind};
//!
//! let highlighter = Highlighter::new();
//! highlighter.prepare(Language::Rust);
//! let block = highlighter.highlight("fn main() {}\n", Language::from_fence("rust"));
//! assert!(block.settled);
//! assert_eq!(block.lines[0][0].text, "fn");
//! assert_eq!(block.lines[0][0].kind, TokenKind::Keyword);
//! ```

mod grammar;
mod highlighter;
mod language;
mod render;

use std::sync::Arc;

pub use highlighter::{Highlighter, Job, STREAM_INTERVAL};
pub use language::Language;

/// What a run of code is, as far as colouring it goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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
    /// A diff's added and removed lines.
    Inserted,
    Deleted,
}

/// A stretch of one line's text that is all one kind.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Run {
    pub text: String,
    pub kind: TokenKind,
}

/// One line of code, without its line ending, as runs. Neighbouring runs
/// differ in kind; an empty line has none.
pub type Line = Vec<Run>;

/// A block of code as lines of runs: one line for each of the code's
/// lines, as [`str::lines`] splits them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Highlighted {
    pub lines: Arc<[Line]>,
    /// False while work under way would colour more of the block: its
    /// grammar is being built, or a streaming pass is running or due. A
    /// caller drawing an unsettled block should draw it again soon, as it
    /// does while [`Highlighter::busy`] holds.
    pub settled: bool,
}

impl Highlighted {
    /// The code in the reading ink, with nothing more to come.
    pub fn plain(code: &str) -> Highlighted {
        Highlighted::settled(render::plain(code))
    }

    fn settled(lines: Vec<Line>) -> Highlighted {
        Highlighted::settled_shared(lines.into())
    }

    fn settled_shared(lines: Arc<[Line]>) -> Highlighted {
        Highlighted {
            lines,
            settled: true,
        }
    }

    fn unsettled(lines: Vec<Line>) -> Highlighted {
        Highlighted {
            lines: lines.into(),
            settled: false,
        }
    }
}

#[cfg(test)]
mod tests;
