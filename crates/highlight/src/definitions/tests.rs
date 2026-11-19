//! Each language's definitions query against a sample file of its own,
//! under `testdata/definitions/`: the whole list, with kinds, labels,
//! lines and nesting, and the chain at chosen lines, including one inside
//! a closure or lambda (which is no definition) and one outside any
//! definition.

use super::*;

/// The definitions as an indented outline, one per line: kind, label and
/// lines, indented by depth.
fn listed(definitions: &[Definition]) -> String {
    let mut out = String::new();
    for definition in definitions {
        let mut depth = 0;
        let mut parent = definition.parent;
        while let Some(at) = parent {
            depth += 1;
            parent = definitions[at].parent;
        }
        out.push_str(&format!(
            "{}{:?} {} {}-{}\n",
            "  ".repeat(depth),
            definition.kind,
            definition.label,
            definition.lines.start,
            definition.lines.end,
        ));
    }
    out
}

fn chain(definitions: &Definitions, line: u32) -> String {
    definitions
        .enclosing(LineRange::line(line))
        .iter()
        .map(|definition| definition.label.as_str())
        .collect::<Vec<_>>()
        .join(" › ")
}

#[track_caller]
fn check(language: Language, code: &str, expected: &str, chains: &[(u32, &str)]) {
    let definitions = Definitions::of(code, Some(language));
    assert_eq!(
        listed(definitions.all()),
        expected,
        "{language:?}'s definitions"
    );
    for (line, expected) in chains {
        assert_eq!(
            &chain(&definitions, *line),
            expected,
            "{language:?}: the chain at line {line}"
        );
    }
}

#[test]
fn rust() {
    check(
        Language::Rust,
        include_str!("../../testdata/definitions/sample.rs"),
        "\
Constant const MAX_BLOBS 5-6
Static static GREETING 8-8
Struct struct Store 10-14
Enum enum Error 16-19
Interface trait Backend 21-27
  Method fn read 22-22
  Method fn exists 24-26
Impl impl Store 29-45
  Constant const LIMIT 30-30
  Method fn open 32-36
  Method fn put 38-44
Impl impl Backend for &'a Store 47-51
  Method fn read 48-50
TypeAlias type Blobs 53-53
Macro macro_rules! blob 55-59
Module mod tests 61-69
  Function fn puts_a_blob 64-68
",
        &[
            (34, "impl Store › fn open"),
            (49, "impl Backend for &'a Store › fn read"),
            (66, "mod tests › fn puts_a_blob"),
            (11, "struct Store"),
            (30, "impl Store › const LIMIT"),
            (2, ""),
        ],
    );
}

#[test]
fn python() {
    check(
        Language::Python,
        include_str!("../../testdata/definitions/sample.py"),
        "\
Constant MAX_BLOBS 5-5
Class class Error 9-10
Class class Store 13-36
  Constant LIMIT 16-16
  Method def __init__ 18-20
  Method def size 22-24
  Method def put 26-32
    Function def check 27-28
  Class class Entry 34-36
    Method def __repr__ 35-36
Function def open_store 39-42
Function def fetch 45-46
",
        &[
            (28, "class Store › def put › def check"),
            (22, "class Store › def size"),
            (36, "class Store › class Entry › def __repr__"),
            (41, "def open_store"),
            (7, ""),
        ],
    );
}

#[test]
fn go() {
    check(
        Language::Go,
        include_str!("../../testdata/definitions/sample.go"),
        "\
Constant const MaxBlobs 5-6
Constant const defaultName 9-9
Constant const limit 10-10
Static var ErrFull 13-13
Struct type Store 15-18
Interface type Backend 20-22
TypeAlias type Name 24-24
Function func Open 26-31
Method func (*Store).Put 33-39
Method func (Store).Read 41-44
",
        &[
            (36, "func (*Store).Put"),
            (17, "type Store"),
            (28, "func Open"),
            (9, "const defaultName"),
            (3, ""),
        ],
    );
    // A declaration of several names gives a definition for each, side by
    // side.
    check(
        Language::Go,
        "package p\n\nvar one, two int\n\nconst (\n\ta, b = 1, 2\n)\n",
        "\
Static var one 3-3
Static var two 3-3
Constant const a 5-7
Constant const b 5-7
",
        &[(3, "var one"), (6, "const a")],
    );
}

#[test]
fn swift() {
    check(
        Language::Swift,
        include_str!("../../testdata/definitions/sample.swift"),
        "\
Constant let maxBlobs 3-3
Class class Store 5-27
  Constructor init 10-12
  Method func put 14-18
  Struct struct Entry 20-26
    Method func describe 23-25
Enum enum StoreError 29-32
Interface protocol Backend 34-36
  Method func read 35-35
Impl extension Store: Backend 38-42
  Method func read 39-41
Class actor Cache 44-46
  Method func warm 45-45
TypeAlias typealias Blobs 48-48
Function func openStore 50-52
",
        &[
            (16, "class Store › func put"),
            (24, "class Store › struct Entry › func describe"),
            (40, "extension Store: Backend › func read"),
            (45, "actor Cache › func warm"),
            (1, ""),
        ],
    );
    // Every conformance, in the order the code lists them; and each name a
    // declaration of several gives.
    check(
        Language::Swift,
        "extension Store: First, Second, Third {\n    func a() {}\n}\nlet one = 1, two = 2\n",
        "\
Impl extension Store: First, Second, Third 1-3
  Method func a 2-2
Constant let one 4-4
Constant let two 4-4
",
        &[(2, "extension Store: First, Second, Third › func a")],
    );
}

#[test]
fn c() {
    check(
        Language::C,
        include_str!("../../testdata/definitions/sample.c"),
        "\
Constant max_name 6-6
Struct struct blob 8-12
Struct typedef struct store 14-17
Enum enum error 19-22
Union union value 24-27
Function full() 29-31
Function store_open() 33-38
Function store_put() 40-46
",
        &[
            (35, "store_open()"),
            (10, "struct blob"),
            (16, "typedef struct store"),
            (7, ""),
        ],
    );
    // Each constant of a declaration of several.
    check(
        Language::C,
        "const int low = 1, high = 2;\n",
        "\
Constant low 1-1
Constant high 1-1
",
        &[],
    );
}

#[test]
fn cpp() {
    check(
        Language::Cpp,
        include_str!("../../testdata/definitions/sample.cpp"),
        "\
Module namespace blobs 5-52
  Constant kMaxBlobs 7-7
  Enum enum Error 9-9
  Class class Store 11-28
    Constructor Store() 13-13
    Method ~Store() 14-14
    Method put() 16-16
    Method size() 18-20
    Struct struct Entry 22-24
  Function Store::put() 30-37
  Function largest() 39-46
  Module namespace detail 48-50
    Function reset() 49-49
",
        &[
            (33, "namespace blobs › Store::put()"),
            (19, "namespace blobs › class Store › size()"),
            (23, "namespace blobs › class Store › struct Entry"),
            (39, "namespace blobs › largest()"),
            (49, "namespace blobs › namespace detail › reset()"),
            (2, ""),
        ],
    );
    // A constructor defined outside its class is a constructor, under a
    // namespace or a template too; a destructor defined there is a
    // function, as any other method defined there is.
    check(
        Language::Cpp,
        "Store::Store(int size) {}\nStore::~Store() {}\nblobs::Store::Store() {}\ntemplate <class T>\nBox<T>::Box() {}\nStore::Other() {}\n",
        "\
Constructor Store::Store() 1-1
Function Store::~Store() 2-2
Constructor blobs::Store::Store() 3-3
Constructor Box<T>::Box() 4-5
Function Store::Other() 6-6
",
        &[(5, "Box<T>::Box()")],
    );
}

#[test]
fn java() {
    check(
        Language::Java,
        include_str!("../../testdata/definitions/sample.java"),
        "\
Class class Store 6-46
  Constant MAX_BLOBS 8-8
  Constructor Store() 12-13
  Method toString() 15-18
  Method put() 20-28
  Class class Entry 30-36
    Method describe() 33-35
  Enum enum Kind 38-45
    Method isLarge() 42-44
Interface interface Backend 48-50
  Method read() 49-49
Class record Blob 52-56
  Constructor Blob 53-55
",
        &[
            (22, "class Store › put()"),
            (43, "class Store › enum Kind › isLarge()"),
            (54, "record Blob › Blob"),
            (15, "class Store › toString()"),
            (3, ""),
        ],
    );
    // Each constant of a declaration of several.
    check(
        Language::Java,
        "class Limits {\n    static final int ONE=1, TWO=2;\n}\n",
        "\
Class class Limits 1-3
  Constant ONE 2-2
  Constant TWO 2-2
",
        &[(2, "class Limits › ONE")],
    );
}

#[test]
fn javascript() {
    check(
        Language::JavaScript,
        include_str!("../../testdata/definitions/sample.js"),
        "\
Constant const MAX_BLOBS 3-3
Class class Store 7-31
  Constructor constructor 11-14
  Method put() 16-22
  Method onChange() 24-26
  Method get size 28-30
Function function openStore 33-38
  Function function helper 34-36
Function const load 40-43
Function function* names 45-47
",
        &[
            (17, "class Store › put()"),
            (35, "function openStore › function helper"),
            (41, "const load"),
            (25, "class Store › onChange()"),
            (5, ""),
        ],
    );
    // A class expression's methods are not a declared class's, so nothing
    // sits under the constant that holds it.
    check(
        Language::JavaScript,
        "const factory = () => class {\n    run() {}\n};\n",
        "\
Function const factory 1-3
",
        &[(2, "const factory")],
    );
}

#[test]
fn typescript() {
    check(
        Language::TypeScript,
        include_str!("../../testdata/definitions/sample.ts"),
        "\
Constant const MAX_BLOBS 3-3
Interface interface Backend 5-7
  Method read() 6-6
TypeAlias type Blobs 9-9
Enum enum Kind 11-14
Class class Store 16-35
  Constructor constructor 20-20
  Method read() 22-24
  Method put() 26-32
  Method onChange() 34-34
Class abstract class Base 37-39
  Method open() 38-38
Module namespace Stores 41-45
  Function function open 42-44
Function const load 47-49
",
        &[
            (27, "class Store › put()"),
            (43, "namespace Stores › function open"),
            (6, "interface Backend › read()"),
            (48, "const load"),
            (1, ""),
        ],
    );
    // A class expression's methods are not a declared class's, so nothing
    // sits under the constant or method that holds it.
    check(
        Language::TypeScript,
        "const factory = () => class {\n    run(): void {}\n};\nclass Outer {\n    make() {\n        return class { inner() {} };\n    }\n}\n",
        "\
Function const factory 1-3
Class class Outer 4-8
  Method make() 5-7
",
        &[(2, "const factory"), (6, "class Outer › make()")],
    );
}

#[test]
fn tsx() {
    check(
        Language::Tsx,
        include_str!("../../testdata/definitions/sample.tsx"),
        "\
Interface interface Props 3-5
Function function Greeting 7-15
Function const Badge 17-17
Class class Panel 19-23
  Method render() 20-22
",
        &[
            (9, "function Greeting"),
            (21, "class Panel › render()"),
            (17, "const Badge"),
            (2, ""),
        ],
    );
    check(
        Language::Tsx,
        "const factory = () => class {\n    run() { return <div />; }\n};\n",
        "\
Function const factory 1-3
",
        &[(2, "const factory")],
    );
}

#[test]
fn names_carry_what_tells_a_definition_apart() {
    let names = |language, code| -> Vec<String> {
        Definitions::of(code, Some(language))
            .all()
            .iter()
            .map(|definition| definition.name.clone())
            .collect()
    };
    let rust = names(
        Language::Rust,
        "impl<T: Clone> From<T>\n    for Store\n{\n}\n",
    );
    assert_eq!(rust, ["From<T> for Store"]);
    let go = names(
        Language::Go,
        include_str!("../../testdata/definitions/sample.go"),
    );
    assert!(go.contains(&"(*Store).Put".to_owned()), "{go:?}");
}

#[test]
fn a_range_sits_in_what_holds_all_of_it() {
    let definitions = Definitions::of(
        include_str!("../../testdata/definitions/sample.rs"),
        Some(Language::Rust),
    );
    let labels = |lines| -> Vec<&str> {
        definitions
            .enclosing(lines)
            .iter()
            .map(|definition| definition.label.as_str())
            .collect()
    };
    // From `fn open` into `fn put`: only the impl holds both.
    assert_eq!(labels(LineRange::new(34, 40)), ["impl Store"]);
    assert_eq!(labels(LineRange::new(40, 34)), ["impl Store"]);
    assert_eq!(labels(LineRange::new(39, 41)), ["impl Store", "fn put"]);
    assert!(labels(LineRange::new(44, 48)).is_empty());
}

#[test]
fn an_outline_holds_what_the_changes_touch_and_what_holds_that() {
    let definitions = Definitions::of(
        include_str!("../../testdata/definitions/sample.rs"),
        Some(Language::Rust),
    );
    let outline = definitions.outline(&[
        LineRange::new(33, 34),
        LineRange::line(49),
        LineRange::line(2),
    ]);
    assert_eq!(
        listed(&outline),
        "\
Impl impl Store 29-45
  Method fn open 32-36
Impl impl Backend for &'a Store 47-51
  Method fn read 48-50
"
    );
    assert_eq!(
        outline
            .iter()
            .map(|definition| definition.parent)
            .collect::<Vec<_>>(),
        [None, Some(0), None, Some(2)],
        "parents index into the outline itself"
    );
    assert!(definitions.outline(&[]).is_empty());
    assert!(definitions.outline(&[LineRange::line(2)]).is_empty());
}

#[test]
fn two_definitions_on_one_line_give_the_first() {
    let definitions = Definitions::of("impl A { fn one() {} fn two() {} }\n", Some(Language::Rust));
    assert_eq!(chain(&definitions, 1), "impl A › fn one");
    assert_eq!(definitions.all().len(), 3);
}

#[test]
fn a_comment_ending_a_line_of_code_is_not_the_next_definitions() {
    let definitions = Definitions::of(
        "const A: u8 = 1; // one\nfn two() {}\n",
        Some(Language::Rust),
    );
    assert_eq!(
        listed(definitions.all()),
        "Constant const A 1-1\nFunction fn two 2-2\n"
    );
    let spaced = Definitions::of("// About two.\n\nfn two() {}\n", Some(Language::Rust));
    assert_eq!(listed(spaced.all()), "Function fn two 3-3\n");
}

#[test]
fn a_language_without_a_query_has_no_definitions() {
    let code = "fn main() {}\nclass Foo:\n    pass\n";
    for language in [
        Language::Shell,
        Language::Json,
        Language::Yaml,
        Language::Toml,
        Language::Html,
        Language::Css,
        Language::Markdown,
        Language::Diff,
    ] {
        assert!(!Definitions::supported(language), "{language:?}");
        let definitions = Definitions::of(code, Some(language));
        assert!(definitions.is_empty(), "{language:?}");
        assert!(definitions.enclosing(LineRange::line(1)).is_empty());
        assert!(definitions.outline(&[LineRange::new(1, 3)]).is_empty());
    }
    assert!(Definitions::of(code, None).is_empty());
}

#[test]
fn every_query_compiles_and_names_known_kinds() {
    for grammar in &GRAMMARS {
        match compile(grammar.id) {
            Ok(compiled) => assert_eq!(
                compiled.is_some(),
                grammar.definitions.is_some(),
                "{}",
                grammar.name
            ),
            Err(error) => panic!("{}: {error}", grammar.name),
        }
    }
    for language in Language::ALL {
        let expected = matches!(
            language,
            Language::Rust
                | Language::TypeScript
                | Language::Tsx
                | Language::JavaScript
                | Language::Python
                | Language::Go
                | Language::Swift
                | Language::C
                | Language::Cpp
                | Language::Java
        );
        assert_eq!(Definitions::supported(language), expected, "{language:?}");
    }
}

#[test]
fn kinds_keep_their_numbers() {
    use DefinitionKind::*;
    let numbered = [
        (Module, 1),
        (Class, 2),
        (Struct, 3),
        (Enum, 4),
        (Union, 5),
        (Interface, 6),
        (Impl, 7),
        (TypeAlias, 8),
        (Function, 9),
        (Method, 10),
        (Constructor, 11),
        (Constant, 12),
        (Static, 13),
        (Macro, 14),
    ];
    for (kind, number) in numbered {
        assert_eq!(kind as u32, number, "{kind:?}");
    }
}
