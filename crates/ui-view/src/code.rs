//! Code coloured by kind of token, as the values a client draws: a fenced
//! block in an agent's markdown and the fragment of a file an edit card
//! shows come back as [`HighlightedCode`], and each client maps a
//! [`TokenKind`] to its own palette. Colouring them is the `ui-code`
//! crate's, which holds the highlighter and its grammars; these values, and
//! the review page that carries runs a host coloured, need neither.

#[cfg(feature = "schema")]
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Which side of a change a line of a diff is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
pub enum LineKind {
    Context,
    Added,
    Removed,
}

/// What a run of code is, as far as colouring it goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
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
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
pub struct CodeRun {
    pub text: String,
    pub kind: TokenKind,
}

/// Code as lines of runs, without their line endings. Neighbouring runs on
/// a line differ in kind; an empty line has none.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
pub struct HighlightedCode {
    pub lines: Vec<Vec<CodeRun>>,
    /// False while work under way would colour more of it: a grammar being
    /// built, or a block still streaming that has lines not yet passed
    /// over. A client that draws an unsettled result draws it again soon
    /// (within a tenth of a second), asking afresh.
    pub settled: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

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
