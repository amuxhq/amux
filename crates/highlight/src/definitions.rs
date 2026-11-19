//! The definitions in a file: its modules, types, functions and item-level
//! constants, each with its kind, name, lines and the definition it sits
//! in. From them come where a line sits in the code, as a chain of short
//! labels (`impl Store › fn open`), and the outline of the definitions a
//! set of changed lines touches.
//!
//! Each language has a definitions query of the crate's own, under
//! `queries/<language>/definitions.scm`, because the grammars' `tags.scm`
//! do not serve: several grammars ship none, and Rust's does not count an
//! `impl` as a definition. A language without one has no definitions, so
//! no chain and no outline.
//!
//! This runs where a whole file is at hand (the host, serving a diff), so
//! it is synchronous: a query is compiled on its language's first use, in
//! a few milliseconds, and a file is parsed from scratch on every call. A
//! caller that sees the same file again keeps the [`Definitions`].
//!
//! ```
//! use highlight::{Definitions, Language, LineRange};
//!
//! let code = "impl Store {\n    fn open() {\n        todo!()\n    }\n}\n";
//! let definitions = Definitions::of(code, Some(Language::Rust));
//! let chain: Vec<&str> = definitions
//!     .enclosing(LineRange::line(3))
//!     .iter()
//!     .map(|definition| definition.label.as_str())
//!     .collect();
//! assert_eq!(chain, ["impl Store", "fn open"]);
//! ```

use std::collections::HashMap;
use std::sync::OnceLock;

use tree_sitter::{Node, Parser, Query, QueryCursor, StreamingIterator};

use crate::Language;
use crate::grammar::{DefinitionRules, GRAMMARS, GrammarId};

/// What a definition is, across languages. The language's own word for it
/// is in the definition's [label](Definition::label); this is for what
/// a client does with any language alike, such as picking an icon.
///
/// Each kind's number is fixed for good, as a protocol enum's values are:
/// a kind is only ever added, with a new number. None is 0, which a
/// protocol enum keeps for a value left unset.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u32)]
pub enum DefinitionKind {
    /// A module or namespace: Rust's `mod`, C++'s and TypeScript's
    /// `namespace`.
    Module = 1,
    /// A class, and its relatives: Swift's `actor`, Java's `record`.
    Class = 2,
    Struct = 3,
    Enum = 4,
    Union = 5,
    /// A set of requirements a type meets: an interface, Rust's `trait`,
    /// Swift's `protocol`.
    Interface = 6,
    /// Methods given to a type declared elsewhere: Rust's `impl`, Swift's
    /// `extension`.
    Impl = 7,
    /// A new name for a type: Rust's and TypeScript's `type`, C++'s
    /// `using`, Swift's `typealias`, and Go's `type` for anything but a
    /// struct or interface.
    TypeAlias = 8,
    /// A function outside any type.
    Function = 9,
    /// A function inside a type, or Go's function with a receiver.
    Method = 10,
    Constructor = 11,
    /// A value fixed at item level: `const`, a module-level all-caps name
    /// in Python, Java's `static final`.
    Constant = 12,
    /// A variable at item level: Rust's `static`, Go's `var`.
    Static = 13,
    /// A macro: Rust's `macro_rules!`.
    Macro = 14,
}

impl DefinitionKind {
    /// The kind a query's `@definition.<kind>` capture names.
    fn from_capture(kind: &str) -> Option<DefinitionKind> {
        Some(match kind {
            "module" => DefinitionKind::Module,
            "class" => DefinitionKind::Class,
            "struct" => DefinitionKind::Struct,
            "enum" => DefinitionKind::Enum,
            "union" => DefinitionKind::Union,
            "interface" => DefinitionKind::Interface,
            "impl" => DefinitionKind::Impl,
            "type" => DefinitionKind::TypeAlias,
            "function" => DefinitionKind::Function,
            "method" => DefinitionKind::Method,
            "constructor" => DefinitionKind::Constructor,
            "constant" => DefinitionKind::Constant,
            "static" => DefinitionKind::Static,
            "macro" => DefinitionKind::Macro,
            _ => return None,
        })
    }
}

/// A span of a file's lines, counted from 1, both ends included, as a
/// diff counts them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LineRange {
    pub start: u32,
    pub end: u32,
}

impl LineRange {
    /// The lines from `start` to `end`. A range given backwards is read
    /// forwards.
    pub fn new(start: u32, end: u32) -> LineRange {
        LineRange {
            start: start.min(end),
            end: start.max(end),
        }
    }

    /// One line.
    pub fn line(line: u32) -> LineRange {
        LineRange::new(line, line)
    }

    pub fn contains(self, other: LineRange) -> bool {
        self.start <= other.start && other.end <= self.end
    }

    pub fn overlaps(self, other: LineRange) -> bool {
        self.start <= other.end && other.start <= self.end
    }
}

/// One definition in a file.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Definition {
    pub kind: DefinitionKind,
    /// What the definition is called, as the code spells it, with any run
    /// of whitespace as one space. Where the name alone would not say which
    /// definition it is, it carries what the code puts beside it: a Rust
    /// trait impl's trait (`Display for Store`), a Go method's receiver
    /// (`(*Store).Open`), a Swift extension's conformances (`Store:
    /// Backend`), a C++ function's qualifier (`Store::open`).
    pub name: String,
    /// The name as a reader sees it in a chain: with the language's word
    /// for the definition before it (`impl Store`, `fn open`, `func
    /// (*Store).Open`, `class Foo`) or, where the language has none, a
    /// function's parentheses after (`open()` in Java, C or a JavaScript
    /// class). Never a signature: parameters and return types are long
    /// and change with the code, and the chain is read at a glance.
    pub label: String,
    /// The definition's lines: from its doc comment, attributes or
    /// decorators, if any, to its last line.
    pub lines: LineRange,
    /// The definition this one is inside, as an index into the list it
    /// came in. Parents come before their children.
    pub parent: Option<usize>,
}

/// Every definition in one file, in the order they start, parents before
/// their children.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Definitions {
    definitions: Vec<Definition>,
}

impl Definitions {
    /// The definitions in a whole file. A language without a definitions
    /// query, or no language, gives none.
    pub fn of(code: &str, language: Option<Language>) -> Definitions {
        let Some(grammar) = language.and_then(GrammarId::of) else {
            return Definitions::default();
        };
        let Some(compiled) = compiled(grammar) else {
            return Definitions::default();
        };
        let mut parser = Parser::new();
        if parser
            .set_language(&grammar.grammar().language.into())
            .is_err()
        {
            return Definitions::default();
        }
        let Some(tree) = parser.parse(code, None) else {
            return Definitions::default();
        };
        Definitions {
            definitions: compiled.find(tree.root_node(), code),
        }
    }

    /// Whether the language has a definitions query, so that
    /// [`Definitions::of`] can find any.
    pub fn supported(language: Language) -> bool {
        GrammarId::of(language).is_some_and(|grammar| grammar.grammar().definitions.is_some())
    }

    pub fn all(&self) -> &[Definition] {
        &self.definitions
    }

    pub fn is_empty(&self) -> bool {
        self.definitions.is_empty()
    }

    /// Where the lines sit in the code: the definitions that hold all of
    /// them, outermost first. For a hunk, pass its changed lines, not its
    /// context, which may reach into the definition before.
    ///
    /// Where two definitions share the lines, as two one-line functions on
    /// one line can, the first is taken.
    pub fn enclosing(&self, lines: LineRange) -> Vec<&Definition> {
        let mut innermost: Option<(usize, usize)> = None;
        for (index, definition) in self.definitions.iter().enumerate() {
            if !definition.lines.contains(lines) {
                continue;
            }
            let depth = self.depth(index);
            if innermost.is_none_or(|(_, deepest)| depth > deepest) {
                innermost = Some((index, depth));
            }
        }
        let mut chain = Vec::new();
        let mut at = innermost.map(|(index, _)| index);
        while let Some(index) = at {
            chain.push(&self.definitions[index]);
            at = self.definitions[index].parent;
        }
        chain.reverse();
        chain
    }

    /// The definitions that any of the changed lines falls in, with what
    /// holds them: a change inside `fn open` gives `impl Store` and `fn
    /// open` under it. Each definition's [parent](Definition::parent) is
    /// an index into the outline itself, so the outline stands alone.
    pub fn outline(&self, changed: &[LineRange]) -> Vec<Definition> {
        // A definition's parent holds all its lines, so whatever a change
        // touches, the change touches its parents too, and every parent
        // kept is in the outline before its children.
        let mut at = vec![None; self.definitions.len()];
        let mut outline = Vec::new();
        for (index, definition) in self.definitions.iter().enumerate() {
            if changed
                .iter()
                .any(|lines| definition.lines.overlaps(*lines))
            {
                at[index] = Some(outline.len());
                outline.push(Definition {
                    parent: definition.parent.and_then(|parent| at[parent]),
                    ..definition.clone()
                });
            }
        }
        outline
    }

    fn depth(&self, mut index: usize) -> usize {
        let mut depth = 0;
        while let Some(parent) = self.definitions[index].parent {
            depth += 1;
            index = parent;
        }
        depth
    }
}

/// A language's definitions query, compiled, and where in it to look.
struct Compiled {
    query: Query,
    rules: &'static DefinitionRules,
    name: Option<u32>,
    trait_name: Option<u32>,
    receiver: Option<u32>,
    conformance: Option<u32>,
    /// Each capture's kind, for the `@definition.<kind>` captures.
    kinds: Vec<Option<DefinitionKind>>,
    /// How each pattern labels what it finds.
    labels: Vec<Label>,
}

/// What a pattern puts around a definition's name in its label.
struct Label {
    /// The language's word for the definition, before the name.
    keyword: Option<Box<str>>,
    /// What follows the name: a function's parentheses where the language
    /// has no word for it.
    suffix: Option<Box<str>>,
}

/// The grammar's definitions query, compiled on first use. The compiled
/// queries are the process's, since a query holds no state of a parse.
fn compiled(grammar: GrammarId) -> Option<&'static Compiled> {
    static COMPILED: [OnceLock<Option<Compiled>>; GRAMMARS.len()] =
        [const { OnceLock::new() }; GRAMMARS.len()];
    COMPILED[grammar.index()]
        .get_or_init(|| compile(grammar).ok().flatten())
        .as_ref()
}

/// Compiles the grammar's definitions query: `None` for a grammar without
/// one, an error for a query that does not compile or names a kind the
/// crate does not know.
fn compile(grammar: GrammarId) -> Result<Option<Compiled>, String> {
    let grammar = grammar.grammar();
    let Some(rules) = grammar.definitions else {
        return Ok(None);
    };
    let query =
        Query::new(&grammar.language.into(), rules.query).map_err(|error| error.to_string())?;
    let mut kinds = Vec::new();
    for name in query.capture_names() {
        kinds.push(match name.strip_prefix("definition.") {
            Some(kind) => Some(
                DefinitionKind::from_capture(kind)
                    .ok_or_else(|| format!("{}: unknown kind @{name}", grammar.name))?,
            ),
            None => None,
        });
    }
    let labels = (0..query.pattern_count())
        .map(|pattern| {
            let setting = |key: &str| {
                query
                    .property_settings(pattern)
                    .iter()
                    .find(|property| &*property.key == key)
                    .and_then(|property| property.value.clone())
            };
            Label {
                keyword: setting("keyword"),
                suffix: setting("suffix"),
            }
        })
        .collect();
    Ok(Some(Compiled {
        name: query.capture_index_for_name("name"),
        trait_name: query.capture_index_for_name("trait"),
        receiver: query.capture_index_for_name("receiver"),
        conformance: query.capture_index_for_name("conformance"),
        query,
        rules,
        kinds,
        labels,
    }))
}

/// A definition found, before nesting is worked out.
struct Found {
    /// The pattern that found it, which decides between two definitions
    /// that reach over the same bytes.
    pattern: usize,
    /// The bytes the definition spans, with its leading comments and
    /// attributes and any node it alone wraps.
    bytes: std::ops::Range<usize>,
    definition: Definition,
}

impl Compiled {
    fn find(&self, root: Node, code: &str) -> Vec<Definition> {
        let mut found: Vec<Found> = Vec::new();
        // Each node captured, and where in `found` its definition is: the
        // patterns are listed most specific first, and of two that capture
        // a node, the first decides what it is.
        let mut seen: HashMap<usize, usize> = HashMap::new();
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&self.query, root, code.as_bytes());
        while let Some(found_match) = matches.next() {
            let mut node = None;
            let mut kind = None;
            let mut name = None;
            let mut trait_name = None;
            let mut receiver = None;
            let mut conformances = Vec::new();
            for capture in found_match.captures() {
                let index = Some(capture.index);
                if let Some(Some(captured)) = self.kinds.get(capture.index as usize) {
                    node = Some(capture.node);
                    kind = Some(*captured);
                } else if index == self.name {
                    name = name.or(Some(capture.node));
                } else if index == self.trait_name {
                    trait_name = Some(capture.node);
                } else if index == self.receiver {
                    receiver = Some(capture.node);
                } else if index == self.conformance {
                    conformances.push(text(capture.node, code));
                }
            }
            let Some((node, kind)) = node.zip(kind) else {
                continue;
            };
            let Some(name) = name.or_else(|| node.child_by_field_name("name")) else {
                continue;
            };
            let mut name = text(name, code);
            if let Some(trait_name) = trait_name {
                name = format!("{} for {name}", text(trait_name, code));
            }
            if let Some(receiver) = receiver {
                name = format!("({}).{name}", text(receiver, code));
            }
            if !conformances.is_empty() {
                name = format!("{name}: {}", conformances.join(", "));
            }
            let Label { keyword, suffix } = &self.labels[found_match.pattern_index];
            let label = match keyword {
                Some(keyword) => format!("{keyword} {name}"),
                None => name.clone(),
            } + suffix.as_deref().unwrap_or("");
            let id = node.id();
            let (first, node) = self.extent(node, code);
            let lines = LineRange::new(line(first.start_position().row), line(last_row(node)));
            let definition = Found {
                pattern: found_match.pattern_index,
                bytes: first.start_byte()..node.end_byte(),
                definition: Definition {
                    kind,
                    name,
                    label,
                    lines,
                    parent: None,
                },
            };
            match seen.get(&id) {
                Some(&at) if found[at].pattern <= definition.pattern => {}
                Some(&at) => found[at] = definition,
                None => {
                    seen.insert(id, found.len());
                    found.push(definition);
                }
            }
        }
        // Outer before inner where two start together, so a stack of what
        // is open gives each definition its parent.
        found.sort_by(|a, b| {
            a.bytes
                .start
                .cmp(&b.bytes.start)
                .then(b.bytes.end.cmp(&a.bytes.end))
                .then(a.pattern.cmp(&b.pattern))
        });
        found.dedup_by(|later, earlier| later.bytes == earlier.bytes);
        let mut open: Vec<(usize, usize)> = Vec::new();
        let mut definitions = Vec::with_capacity(found.len());
        for (index, found) in found.into_iter().enumerate() {
            while open.last().is_some_and(|(_, end)| *end < found.bytes.end) {
                open.pop();
            }
            definitions.push(Definition {
                parent: open.last().map(|(parent, _)| *parent),
                ..found.definition
            });
            open.push((index, found.bytes.end));
        }
        definitions
    }

    /// Where a definition starts and ends: the node, grown to a node that
    /// does nothing but wrap it (an `export`, a decorated definition, a
    /// template, a declaration of one type), and the first of the comments,
    /// attributes and decorators directly above that.
    fn extent<'tree>(&self, mut node: Node<'tree>, code: &str) -> (Node<'tree>, Node<'tree>) {
        while let Some(parent) = node.parent()
            && self.rules.wrappers.contains(&parent.kind())
            && wraps_alone(parent, node)
        {
            node = parent;
        }
        let mut start = node;
        while let Some(previous) = start.prev_named_sibling()
            && self.rules.leading.contains(&previous.kind())
            && first_on_its_line(previous, code)
            && last_row(previous) + 1 >= start.start_position().row
        {
            start = previous;
        }
        (start, node)
    }
}

/// Whether `child` is the last of `parent`'s named children and the only
/// one of its kind: what an `export` or a decorated definition is to the
/// definition in it, and a grouped `const (...)` is not to each constant.
fn wraps_alone(parent: Node, child: Node) -> bool {
    let mut cursor = parent.walk();
    let mut last = None;
    for sibling in parent.named_children(&mut cursor) {
        if sibling.id() != child.id() && sibling.kind() == child.kind() {
            return false;
        }
        last = Some(sibling.id());
    }
    last == Some(child.id())
}

/// Whether only whitespace comes before the node on its first line, so a
/// comment at the end of a line of code above is not taken for the next
/// definition's.
fn first_on_its_line(node: Node, code: &str) -> bool {
    let before = &code[..node.start_byte()];
    let line_start = before.rfind('\n').map_or(0, |at| at + 1);
    before[line_start..].trim().is_empty()
}

/// The last row a node's text is on. A node that ends with its line's
/// newline, as a Rust line comment does, ends at the start of the next
/// row, which holds none of it.
fn last_row(node: Node) -> usize {
    let end = node.end_position();
    if end.column == 0 && end.row > node.start_position().row {
        end.row - 1
    } else {
        end.row
    }
}

/// A tree's row as a line counted from 1.
fn line(row: usize) -> u32 {
    u32::try_from(row + 1).unwrap_or(u32::MAX)
}

/// A node's text with each run of whitespace as one space.
fn text(node: Node, code: &str) -> String {
    code[node.byte_range()]
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests;
