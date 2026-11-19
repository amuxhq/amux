//! The grammars: each language's tree-sitter parser and the queries that
//! run over its trees, and how a query's capture names become token kinds.
//!
//! The highlight queries are the ones the grammars' crates ship. A
//! language whose grammar extends another's (TypeScript over JavaScript,
//! C++ over C) runs the base query first and its own after: when two
//! patterns capture the same node the later one wins, so the more specific
//! query goes last. Where a shipped query gets a language wrong, a short
//! query of the crate's own, under `queries/<language>/`, runs after it and
//! says why.
//!
//! The table holds every query the crate runs over a language's trees, so
//! a query of another kind (definitions, say) is one more field on
//! [`Grammar`] and one more file under `queries/<language>/`.

use tree_sitter_highlight::HighlightConfiguration;
use tree_sitter_language::LanguageFn;

use crate::{Language, TokenKind};

/// A grammar's slot in [`GRAMMARS`]. The public languages, and the
/// grammars only another one injects (Markdown's inline text).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GrammarId {
    Rust,
    Swift,
    TypeScript,
    Tsx,
    JavaScript,
    Python,
    Go,
    Shell,
    Json,
    Yaml,
    Toml,
    C,
    Cpp,
    Java,
    Html,
    Css,
    Markdown,
    MarkdownInline,
}

impl GrammarId {
    pub(crate) fn of(language: Language) -> Option<GrammarId> {
        Some(match language {
            Language::Rust => GrammarId::Rust,
            Language::Swift => GrammarId::Swift,
            Language::TypeScript => GrammarId::TypeScript,
            Language::Tsx => GrammarId::Tsx,
            Language::JavaScript => GrammarId::JavaScript,
            Language::Python => GrammarId::Python,
            Language::Go => GrammarId::Go,
            Language::Shell => GrammarId::Shell,
            Language::Json => GrammarId::Json,
            Language::Yaml => GrammarId::Yaml,
            Language::Toml => GrammarId::Toml,
            Language::C => GrammarId::C,
            Language::Cpp => GrammarId::Cpp,
            Language::Java => GrammarId::Java,
            Language::Html => GrammarId::Html,
            Language::Css => GrammarId::Css,
            Language::Markdown => GrammarId::Markdown,
            Language::Diff => return None,
        })
    }

    pub(crate) fn index(self) -> usize {
        self as usize
    }

    pub(crate) fn grammar(self) -> &'static Grammar {
        let grammar = &GRAMMARS[self.index()];
        debug_assert_eq!(grammar.id, self, "the grammar table is out of slot order");
        grammar
    }
}

/// One grammar and the queries the crate runs over its trees.
pub(crate) struct Grammar {
    pub(crate) id: GrammarId,
    /// The name an injection asks for this grammar by.
    pub(crate) name: &'static str,
    pub(crate) language: LanguageFn,
    /// Highlight queries, joined in this order.
    pub(crate) highlights: &'static [&'static str],
    pub(crate) injections: &'static str,
    pub(crate) locals: &'static [&'static str],
    /// The grammars this one's injections may hand text to. Each must be
    /// ready before this one counts as ready, so a block's colours never
    /// depend on which grammars happened to have been built first. Text an
    /// injection hands to any other language keeps its host's colours.
    pub(crate) injects: &'static [GrammarId],
    /// Capture names this grammar colours otherwise than the rest do.
    pub(crate) kinds: &'static [(&'static str, TokenKind)],
}

pub(crate) static GRAMMARS: [Grammar; 18] = [
    Grammar {
        id: GrammarId::Rust,
        name: "rust",
        language: tree_sitter_rust::LANGUAGE,
        highlights: &[
            tree_sitter_rust::HIGHLIGHTS_QUERY,
            include_str!("../queries/rust/highlights.scm"),
        ],
        // Macro bodies parse as Rust, so `vec![..]` and `format!(..)` are
        // coloured like the code around them.
        injections: tree_sitter_rust::INJECTIONS_QUERY,
        locals: &[],
        injects: &[GrammarId::Rust],
        kinds: &[],
    },
    Grammar {
        id: GrammarId::Swift,
        name: "swift",
        language: tree_sitter_swift::LANGUAGE,
        highlights: &[tree_sitter_swift::HIGHLIGHTS_QUERY],
        // Its injections are for regex and comment grammars the crate does
        // not have.
        injections: "",
        locals: &[tree_sitter_swift::LOCALS_QUERY],
        injects: &[],
        kinds: &[],
    },
    Grammar {
        id: GrammarId::TypeScript,
        name: "typescript",
        language: tree_sitter_typescript::LANGUAGE_TYPESCRIPT,
        highlights: &[
            tree_sitter_javascript::HIGHLIGHT_QUERY,
            tree_sitter_typescript::HIGHLIGHTS_QUERY,
        ],
        injections: "",
        locals: &[
            tree_sitter_javascript::LOCALS_QUERY,
            tree_sitter_typescript::LOCALS_QUERY,
        ],
        injects: &[],
        kinds: &[],
    },
    Grammar {
        id: GrammarId::Tsx,
        name: "tsx",
        language: tree_sitter_typescript::LANGUAGE_TSX,
        highlights: &[
            tree_sitter_javascript::HIGHLIGHT_QUERY,
            tree_sitter_typescript::HIGHLIGHTS_QUERY,
            tree_sitter_javascript::JSX_HIGHLIGHT_QUERY,
        ],
        injections: "",
        locals: &[
            tree_sitter_javascript::LOCALS_QUERY,
            tree_sitter_typescript::LOCALS_QUERY,
        ],
        injects: &[],
        kinds: &[],
    },
    Grammar {
        id: GrammarId::JavaScript,
        name: "javascript",
        // The JavaScript grammar parses JSX too, so its tag query runs
        // over every JavaScript block.
        language: tree_sitter_javascript::LANGUAGE,
        highlights: &[
            tree_sitter_javascript::HIGHLIGHT_QUERY,
            tree_sitter_javascript::JSX_HIGHLIGHT_QUERY,
        ],
        // Its injections hand tagged templates to whatever language the tag
        // names, and regexes and doc comments to grammars the crate lacks.
        injections: "",
        locals: &[tree_sitter_javascript::LOCALS_QUERY],
        injects: &[],
        kinds: &[],
    },
    Grammar {
        id: GrammarId::Python,
        name: "python",
        language: tree_sitter_python::LANGUAGE,
        highlights: &[
            tree_sitter_python::HIGHLIGHTS_QUERY,
            include_str!("../queries/python/highlights.scm"),
        ],
        injections: "",
        locals: &[],
        injects: &[],
        kinds: &[],
    },
    Grammar {
        id: GrammarId::Go,
        name: "go",
        language: tree_sitter_go::LANGUAGE,
        highlights: &[
            tree_sitter_go::HIGHLIGHTS_QUERY,
            include_str!("../queries/go/highlights.scm"),
        ],
        injections: "",
        locals: &[],
        injects: &[],
        kinds: &[],
    },
    Grammar {
        id: GrammarId::Shell,
        name: "bash",
        language: tree_sitter_bash::LANGUAGE,
        highlights: &[tree_sitter_bash::HIGHLIGHT_QUERY],
        injections: "",
        locals: &[],
        injects: &[],
        kinds: &[],
    },
    Grammar {
        id: GrammarId::Json,
        name: "json",
        language: tree_sitter_json::LANGUAGE,
        highlights: &[tree_sitter_json::HIGHLIGHTS_QUERY],
        injections: "",
        locals: &[],
        injects: &[],
        kinds: &[],
    },
    Grammar {
        id: GrammarId::Yaml,
        name: "yaml",
        language: tree_sitter_yaml::LANGUAGE,
        highlights: &[tree_sitter_yaml::HIGHLIGHTS_QUERY],
        injections: "",
        locals: &[],
        injects: &[],
        // A YAML file is mostly keys: in the reading ink it would be one
        // colour down the left, so they take the keyword's, as tags do.
        kinds: &[("property", TokenKind::Keyword)],
    },
    Grammar {
        id: GrammarId::Toml,
        name: "toml",
        language: tree_sitter_toml_ng::LANGUAGE,
        highlights: &[tree_sitter_toml_ng::HIGHLIGHTS_QUERY],
        injections: "",
        locals: &[],
        injects: &[],
        kinds: &[("property", TokenKind::Keyword)],
    },
    Grammar {
        id: GrammarId::C,
        name: "c",
        language: tree_sitter_c::LANGUAGE,
        highlights: &[
            tree_sitter_c::HIGHLIGHT_QUERY,
            include_str!("../queries/c/highlights.scm"),
        ],
        injections: "",
        locals: &[],
        injects: &[],
        kinds: &[],
    },
    Grammar {
        id: GrammarId::Cpp,
        name: "cpp",
        language: tree_sitter_cpp::LANGUAGE,
        highlights: &[
            tree_sitter_c::HIGHLIGHT_QUERY,
            tree_sitter_cpp::HIGHLIGHT_QUERY,
            include_str!("../queries/c/highlights.scm"),
        ],
        // Its one injection hands a raw string to the language its
        // delimiter names, which is rarely one the crate has.
        injections: "",
        locals: &[],
        injects: &[],
        kinds: &[],
    },
    Grammar {
        id: GrammarId::Java,
        name: "java",
        language: tree_sitter_java::LANGUAGE,
        highlights: &[tree_sitter_java::HIGHLIGHTS_QUERY],
        injections: "",
        locals: &[],
        injects: &[],
        kinds: &[],
    },
    Grammar {
        id: GrammarId::Html,
        name: "html",
        language: tree_sitter_html::LANGUAGE,
        highlights: &[tree_sitter_html::HIGHLIGHTS_QUERY],
        // `<script>` and `<style>` bodies are JavaScript and CSS.
        injections: tree_sitter_html::INJECTIONS_QUERY,
        locals: &[],
        injects: &[GrammarId::JavaScript, GrammarId::Css],
        kinds: &[],
    },
    Grammar {
        id: GrammarId::Css,
        name: "css",
        language: tree_sitter_css::LANGUAGE,
        highlights: &[tree_sitter_css::HIGHLIGHTS_QUERY],
        injections: "",
        locals: &[],
        injects: &[],
        // Property names, as against the values they are given.
        kinds: &[("property", TokenKind::Type)],
    },
    Grammar {
        id: GrammarId::Markdown,
        name: "markdown",
        language: tree_sitter_md::LANGUAGE,
        highlights: &[tree_sitter_md::HIGHLIGHT_QUERY_BLOCK],
        // The block grammar leaves a paragraph's text to the inline one,
        // for code spans and links. A fence's code is drawn plain between
        // its literal-coloured fences.
        injections: include_str!("../queries/markdown/injections.scm"),
        locals: &[],
        injects: &[GrammarId::MarkdownInline],
        kinds: &[],
    },
    Grammar {
        id: GrammarId::MarkdownInline,
        name: "markdown_inline",
        language: tree_sitter_md::INLINE_LANGUAGE,
        highlights: &[tree_sitter_md::HIGHLIGHT_QUERY_INLINE],
        injections: "",
        locals: &[],
        injects: &[],
        kinds: &[],
    },
];

/// The capture names the crate colours, and the kind each becomes. A
/// capture takes the entry that shares the most of its dotted parts
/// (`function.method.builtin` is a function); a capture matching none,
/// like `variable`, `operator` or `punctuation`, adds no colour of its own
/// and reads in the colour of what encloses it, so the quotes of a string
/// are the string's. Every configuration recognises the same names in the
/// same order, because a highlight event carries only the name's index and
/// injected text is reported in the host's events.
const NAMES: [(&str, TokenKind); 25] = [
    ("keyword", TokenKind::Keyword),
    ("tag", TokenKind::Keyword),
    // The words of CSS's at-rules.
    ("charset", TokenKind::Keyword),
    ("import", TokenKind::Keyword),
    ("keyframes", TokenKind::Keyword),
    ("media", TokenKind::Keyword),
    ("namespace", TokenKind::Keyword),
    ("supports", TokenKind::Keyword),
    // `self` and `this`.
    ("variable.builtin", TokenKind::Keyword),
    ("string", TokenKind::String),
    ("escape", TokenKind::String),
    ("number", TokenKind::Number),
    ("boolean", TokenKind::Number),
    ("constant", TokenKind::Number),
    ("type", TokenKind::Type),
    ("constructor", TokenKind::Type),
    ("attribute", TokenKind::Type),
    ("function", TokenKind::Function),
    ("comment", TokenKind::Comment),
    // Markdown's headings and literals.
    ("text.title", TokenKind::Keyword),
    ("text.literal", TokenKind::String),
    // Recognised so the grammars above can colour them; plain otherwise.
    ("property", TokenKind::Plain),
    // An interpolation inside a string is code, not the string: its plain
    // capture stops the string's colour reaching in.
    ("embedded", TokenKind::Plain),
    ("none", TokenKind::Plain),
    // An `=` inside a TOML pair, which the grammar captures whole as its
    // key, is not part of the key.
    ("operator", TokenKind::Plain),
];

/// A grammar's highlight configuration, and the kind each of its
/// recognised names becomes.
pub(crate) struct Configured {
    pub(crate) config: HighlightConfiguration,
    pub(crate) kinds: Vec<TokenKind>,
}

impl Grammar {
    /// Compiles the grammar's queries. This is the expensive step (tens of
    /// milliseconds for most grammars, nearer half a second for Swift's),
    /// which is why it runs once per grammar and off the draw thread.
    pub(crate) fn configure(&self) -> Result<Configured, tree_sitter::QueryError> {
        let highlights = without_editor_captures(&self.highlights.concat());
        let locals = self.locals.concat();
        let mut config = HighlightConfiguration::new(
            self.language.into(),
            self.name,
            &highlights,
            self.injections,
            &locals,
        )?;
        config.configure(&NAMES.map(|(name, _)| name));
        let kinds = NAMES
            .iter()
            .map(|(name, kind)| {
                self.kinds
                    .iter()
                    .find(|(own, _)| own == name)
                    .map_or(*kind, |(_, own)| *own)
            })
            .collect();
        Ok(Configured { config, kinds })
    }
}

/// The query without the captures that mean something to an editor and
/// nothing to a highlighter (`@spell` marks text to spellcheck). Such a
/// capture shares a pattern with the one that colours the node, and the
/// last capture on a node decides its colour, so an unrecognised one would
/// leave Swift's comments plain.
fn without_editor_captures(query: &str) -> String {
    let mut out = String::with_capacity(query.len());
    let mut rest = query;
    while let Some(at) = rest.find('@') {
        out.push_str(&rest[..at]);
        let name_len = rest[at + 1..]
            .find(|c: char| !(c.is_alphanumeric() || matches!(c, '.' | '_' | '-')))
            .unwrap_or(rest.len() - at - 1);
        let name = &rest[at + 1..at + 1 + name_len];
        if !matches!(name, "spell" | "nospell") {
            out.push_str(&rest[at..at + 1 + name_len]);
        }
        rest = &rest[at + 1 + name_len..];
    }
    out.push_str(rest);
    out
}

/// The grammar an injection names, among those `host` may hand text to.
pub(crate) fn injected(host: GrammarId, name: &str) -> Option<GrammarId> {
    let host = host.grammar();
    host.injects.iter().copied().find(|id| {
        let grammar = id.grammar();
        grammar.name == name || Language::from_fence(name).and_then(GrammarId::of) == Some(*id)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_is_in_slot_order() {
        for (index, grammar) in GRAMMARS.iter().enumerate() {
            assert_eq!(
                grammar.id.index(),
                index,
                "{} is out of place",
                grammar.name
            );
        }
    }

    #[test]
    fn every_grammar_compiles_its_queries() {
        for grammar in &GRAMMARS {
            if let Err(error) = grammar.configure() {
                panic!("{}: {error}", grammar.name);
            }
        }
    }

    #[test]
    fn editor_captures_are_dropped_and_others_kept() {
        assert_eq!(
            without_editor_captures("(comment) @comment @spell\n(x) @nospell @string.special"),
            "(comment) @comment \n(x)  @string.special"
        );
    }

    #[test]
    fn injections_name_the_grammars_they_reach() {
        assert_eq!(
            injected(GrammarId::Html, "javascript"),
            Some(GrammarId::JavaScript)
        );
        assert_eq!(injected(GrammarId::Html, "css"), Some(GrammarId::Css));
        assert_eq!(injected(GrammarId::Rust, "rust"), Some(GrammarId::Rust));
        assert_eq!(
            injected(GrammarId::Markdown, "markdown_inline"),
            Some(GrammarId::MarkdownInline)
        );
        assert_eq!(injected(GrammarId::Markdown, "yaml"), None);
        assert_eq!(injected(GrammarId::Python, "python"), None);
    }
}
