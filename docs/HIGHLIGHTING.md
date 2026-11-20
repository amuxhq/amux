# Syntax highlighting

*For developers changing how amux colours code: chat code blocks, edit cards and the review page's diffs, on every client.*

One engine colours every piece of code amux shows: the
[`highlight`](../crates/highlight/src/lib.rs) crate, over tree-sitter and a
fixed set of grammars. Code and its language go in; lines of runs come out,
each run tagged with a kind of token (`Plain`, `Keyword`, `String`,
`Number`, `Type`, `Function`, `Comment`, and for a diff `Inserted` and
`Deleted`). A client maps each kind to a colour of its own palette, so the
same block has the same runs on the terminal, the Mac and the iPhone and
reads in each one's colours.

The same grammars also give a file's **definitions**: its modules, types,
functions and item-level values, from which the review page says where a
hunk sits (`impl Store › fn open`) and which definitions a change touches.

Two callers use it in different ways:

- **Clients** colour the code they parse out of a chat themselves: fenced
  blocks in an agent's markdown, and the hunks an edit card shows. Each
  client parses markdown on its own, so the view layer exposes the
  colouring as functions (below), which the terminal calls directly and the
  Apple apps call through the bridge.
- **The agent's host** colours a review's diffs from whole files, because a
  hunk coloured alone is wrong whenever it starts inside a block comment or
  a string ([reviews](REVIEWS.md#file-bodies)).

## Languages

| Language | Grammar crate | Notes |
| --- | --- | --- |
| Rust, Swift, Go, Python, Java, C, C++ | `tree-sitter-<language>` | C++ runs C's query, then its own |
| TypeScript, TSX, JavaScript | `tree-sitter-typescript`, `tree-sitter-javascript` | TypeScript and TSX run JavaScript's query, then their own |
| Shell | `tree-sitter-bash` | Also every shell and console transcript a fence names |
| JSON, YAML, TOML | `tree-sitter-json`, `-yaml`, `-toml-ng` | Keys take `Keyword` |
| HTML, CSS | `tree-sitter-html`, `-css` | HTML's `script` and `style` bodies are coloured as JavaScript and CSS |
| Markdown | `tree-sitter-md` | Its block and inline grammars; headings `Keyword`, code spans and fences `String` |
| Diff | none | Coloured by each line's `+` or `-`; a `---` line followed by `+++` is a file header |

Seventeen languages have a grammar, Markdown's two counting as one. Anything
else is drawn plain.

**Picking a language** is a table lookup, never a guess
([`language.rs`](../crates/highlight/src/language.rs)):

1. A fence's info string: its first word, in any case, through an alias
   table of the words agents write (`zsh`, `sh` and `console` are shell,
   `jsonc` is JSON, `mjs` is JavaScript, `h` is C, `patch` is a diff).
2. A file: a few files known by name (`.zshrc`, `Cargo.lock`), else its
   extension, else for a file with neither the interpreter its `#!` line
   names, read through `env` and without a version suffix (`python3.12` is
   Python). The edit card reads the `#!` from the first line of the change.
3. Nothing else.

`Language::name` is the language's canonical lowercase word, which
`Language::from_fence` reads back. It is what `GetChanges` reports as a
changed file's `language` ([changes](WIRE.md#changes)), so the overview, the
review page and the clients name a file's language by one rule.

**Queries.** The highlight queries are the ones the grammar crates ship.
When two patterns capture one node the later one wins, so a grammar that
extends another runs the base query first. Where a shipped query gets a
language wrong, a short query of the crate's own under
[`queries/<language>/highlights.scm`](../crates/highlight/queries/) runs
after it and says why in a comment: Rust's all-caps constant pattern never
matches, Go's field pattern hides method names, Python's calls a one-letter
class a constant, and neither C query colours `true` and `false`. Capture
names map to kinds through one table in
[`grammar.rs`](../crates/highlight/src/grammar.rs); Swift's `@spell`
captures are dropped, since they would leave its comments plain.

**Injections** are kept only where a block's colours depend on them: Rust's
macro bodies parse as Rust, and HTML's script and style bodies as JavaScript
and CSS. Markdown has an injection query of the crate's own
([`queries/markdown/injections.scm`](../crates/highlight/queries/markdown/injections.scm)):
paragraph and heading text goes to the inline grammar with its children
included, which the shipped query leaves out, so code spans are found; and
fenced code, HTML blocks and front matter are not handed on, so a Markdown
file never waits for every grammar it might quote. A grammar counts as ready
only once every grammar it injects is built, so a block's colours never
depend on which grammars happened to be built first.

## Costs, and the interface they shape

Two costs were measured before the crate was written, and its interface
follows from them ([`highlighter.rs`](../crates/highlight/src/highlighter.rs)):

- **Building a grammar's configuration compiles its queries**: 17 ms for
  Rust, 30 ms for C++, 403 ms for Swift. So a configuration is built once
  per process, on first use, on a background thread. Until it is ready, a
  call returns the code plain and not `settled`, and `Highlighter::busy`
  holds; the caller draws what it got and draws again soon. The terminal
  draws again 50 ms later, and the Apple apps ask again 50 ms later.
- **tree-sitter's highlighter parses from scratch on every call**, so
  colouring a block still streaming in on every line that arrives is
  quadratic: 5.5 s in total for a 1,000-line block. `highlight_streaming`
  passes over a streaming block's completed lines in the background at most
  every `STREAM_INTERVAL` (100 ms) and fills in the rest plain. A block is
  known by its content (a later call whose code starts with what an earlier
  pass was asked for carries that block on), so a caller holds nothing per
  block. Only the open fence of a message still streaming is asked for this
  way; every finished block is coloured once.

Finished blocks are kept by content, 512 of them before the cache starts
again, so a block drawn again costs a lookup. A caller that keeps results by
something better than content uses `highlight_now`, which colours on the
calling thread and caches nothing: the host keeps a file's colouring by its
object id instead. `prepare(language)` and `preload()` build grammars on the
calling thread, for a caller that can wait, such as the storybook, which
wants its first frame settled.

`Highlighter::shared()` is the process's highlighter. `with_spawner` takes
the function that runs background work; a test builds one on `HeldJobs` and
runs the jobs when it chooses, so what a caller sees before and after each
build or pass is fixed whatever the machine's speed.

## Definitions

[`definitions.rs`](../crates/highlight/src/definitions.rs) finds a file's
definitions with a query of the crate's own for each of Rust, TypeScript,
TSX (sharing TypeScript's), JavaScript, Python, Go, Swift, C, C++ and Java,
under [`queries/<language>/definitions.scm`](../crates/highlight/queries/),
each tested against a sample file under
[`testdata/definitions/`](../crates/highlight/testdata/definitions/). Other
languages have none, so no chain and no outline.

- **A definition** has a kind, a name, a label, its lines (from 1, both
  ends included, as a diff counts them) and its parent. The kinds are fixed
  numbers from 1 (module, class, struct, enum, union, interface, impl, type
  alias, function, method, constructor, constant, static, macro), only ever
  added to, so the review service's `ReviewDefinitionKind` carries them as
  they are.
- **A label is a name, never a signature**, in the language's own word:
  `fn open`, `def put`, `func (*Store).Put`, `impl Display for Store`,
  `extension Store: Backend`, `put()` where a language has no word.
  Parameters and return types are long and change with the code, and a
  chain is read at a glance on a phone. The name carries what tells two
  definitions apart where the bare name would not: a Rust trait impl names
  its trait, a Go method its receiver.
- **A definition's lines reach over** the doc comments, attributes and
  decorators directly above it, and out to a node that only wraps it
  (`export`, a C++ template, a Go `type` declaring one type), so a change to
  a `#[derive]` sits in what it describes.
- **What counts:** named types and functions, nested named functions, and
  item-level constants and statics where the language marks them. Closures,
  lambdas, locals and the members of anonymous classes do not. A name that
  binds nothing (Go's and Rust's `_`) is no definition.

`Definitions::of(code, language)` parses a file; `enclosing(lines)` gives the
chain that holds a range, outermost first, and `outline(changed)` the
definitions any changed range touches, with what holds them. This runs where
a whole file is at hand, so it is synchronous: a language's query compiles
on first use in 0.4 ms (Go) to 55 ms (C++), and a 4,882-line Rust file takes
18.6 ms, most of it parsing. The host parses each file twice, once to colour
it and once for definitions, because tree-sitter's highlighter cannot take a
parsed tree, and keeps both by object id.

## Where it runs

**The view layer.** `ui_code::highlight_block(highlighter, code, info,
streaming)` colours a fenced block, its language from the fence's info
string, and `ui_code::highlight_patch(highlighter, path, hunks)` an edit
card's hunks as the file at `path` reads
([`crates/ui-code/src/lib.rs`](../crates/ui-code/src/lib.rs)). The ask card
and a landed edit both carry a file's change as hunks, so one function
serves both. Each hunk is coloured as a fragment of its own, since a block
comment or string one hunk leaves open does not reach the next; within a
hunk the old side (context and removed lines) and the new side (context and
added lines) are coloured apart, so a removed line that opens a string does
not swallow the added line after it. Both answer `HighlightedCode`,
`ui-view`'s own serializable value with `settled`
([`crates/ui-view/src/code.rs`](../crates/ui-view/src/code.rs)), which
crosses the bridge as JSON. The values are the view layer's rather than
`highlight`'s so that `highlight` stays free of serialization and its names
never collide with view values in the generated Swift. The functions are in
`ui-code` rather than `ui-view` so that a client that draws no code links no
tree-sitter: the browser extension builds `ui-view` to WebAssembly for its
agent picker with no C compiler
([browser](BROWSER.md#webassembly-is-optional)). `ui-code` depends on
`ui-view`, never the reverse, and the clients that draw code (the terminal,
the bridge and `shot`) depend on both.

**The terminal** colours every piece of code it draws through
[`crates/tui/src/code.rs`](../crates/tui/src/code.rs), with the shared
highlighter or, in a test, one the test hands the drawing thread. The theme
maps each kind to a style; on an edit card a token takes its syntax colour
over the line's diff tint and plain text keeps the line's diff ink, so added
and removed lines still read as such. A frame that drew unsettled code asks
to be drawn again. Its palettes hold every token kind readable on every
surface it is drawn on, diff tints included
([terminal](TERMINAL.md)).

**The Mac and iPhone apps** parse markdown in Swift (`AmuxWords`), so each
block asks the bridge, `amux_highlight_block` and `amux_highlight_patch`,
through `CodeHighlighting`, which the app puts in the environment, off the
main thread, and keeps settled answers by content. Each app draws the kinds
in its design's `syntax` colours, held to 4.5:1 on the code background and
under an edit's tints in light and dark by test. The Mac storybook links no
bridge and draws code plain ([iPhone](IOS.md), [Mac](MAC.md)).

**The agent's host** colours a review's files from whole files on its own
blocking pool and keeps the colouring by object id
([reviews](REVIEWS.md#file-bodies)), and names each changed file's language
for `GetChanges` and the review's file lists by its path, then its `#!`
line.

## Sizes

Measured on Apple Silicon with the release profile (fat LTO, one codegen
unit, `panic=abort`), stripped; compressed is `gzip -9`. A test binary built
a configuration for each grammar it linked, so every parser and query was
kept:

| Build | Installed | Compressed |
| --- | --- | --- |
| tree-sitter alone | 0.32 MB | 0.15 MB |
| Sixteen grammars | 17.07 MB | 2.30 MB |
| Sixteen and Markdown (what ships) | 17.85 MB | 2.43 MB |
| Sixteen and Ruby | 19.18 MB | 2.48 MB |

The phone's bridge, linked for the device as a dylib under the release
profile and stripped, went from 16.4 MB installed and 7.67 MB compressed to
33.9 MB and 9.85 MB when the highlighter first reached it: nearly all of it
the grammars. The lever for app size is the bridge's release profile, not
the grammar list.

## Decisions and rejected alternatives

- **Keeping syntect**, which coloured the terminal's code before. It was the
  terminal's alone, so the apps' code was plain and the host had nothing to
  colour diffs with; keeping it as a fallback for languages tree-sitter
  lacks would mean two engines whose colours disagree on the same page.
- **Guessing a language from the code** when no word names it. A wrong
  guess colours a block worse than drawing it plain.
- **SQL.** Its grammar alone is 13.5 MB installed, nearly as much as the
  other sixteen together.
- **Ruby** adds 2.11 MB installed and 0.18 MB compressed, more than Rust's
  or bash's grammar, and is rare in what amux's agents write; a `ruby` fence
  draws plain. Markdown adds 0.78 MB and 0.13 MB, less than Rust's grammar,
  and is the commonest fence in agent output after code, so it is in.
  Adding a language is one dependency and one table entry.
- **The grammars' own `tags.scm`** for definitions. Several grammars ship
  none, and Rust's does not count an `impl` as a definition.
- **Colouring a hunk alone** on the host. A hunk that starts inside a block
  comment or a string is coloured wrongly; whole files are coloured once and
  kept by object id, since a review's snapshots share most of their files.
- **Colouring a streaming block on every line that arrives**, which is
  quadratic; or waiting for its closing fence, which leaves a long block
  plain while it is read.

## Tests

| What | Where | Run |
| --- | --- | --- |
| A snippet per language with chosen tokens' kinds; fence words, extensions, names and `#!` lines; diff prefixes; a grammar plain until its held build runs; HTML waiting for JavaScript and CSS; the content cache; the streaming throttle with held jobs and stepped instants; every query compiles | [`crates/highlight/src/tests.rs`](../crates/highlight/src/tests.rs), `language.rs` | `just test-crate highlight` |
| Each language's definitions against its sample file: labels, kinds, lines, parents, chains and outlines | [`crates/highlight/src/definitions/tests.rs`](../crates/highlight/src/definitions/tests.rs) | `just test-crate highlight` |
| A block plain until its grammar is built, unknown and diff fences, streaming, both sides of a hunk coloured apart, hunks coloured apart, a script's later hunks | [`crates/ui-code/src/lib.rs`](../crates/ui-code/src/lib.rs) | `just test-crate ui-code` |
| The JSON shape of `HighlightedCode` | `crates/ui-view/src/code.rs` | `just test-crate ui-view` |
| Which fenced block is still open, a streaming message keeping only settled renders, tokens over diff tints; every token kind readable on every surface (`code_reads_on_every_surface_it_is_drawn_on`) | `crates/tui/src/` | `just test-tui` |
| The bridge's answers (`code_is_coloured_by_the_shared_view`) | [`crates/app-ffi/src/tests.rs`](../crates/app-ffi/src/tests.rs) | `just test-crate app-ffi` |
| The Swift side: `CodeHighlightingTests`, `MarkdownTests` and the phone palette's `SyntaxContrastTests`; the Mac palette's `SyntaxContrast` | `apps/apple/` | `just ios unit`, `just mac test` |
| The host's colouring of a review's files from whole files, cached by object id | [`crates/node/tests/review_files.rs`](../crates/node/tests/review_files.rs) | `just test-crate node` |
