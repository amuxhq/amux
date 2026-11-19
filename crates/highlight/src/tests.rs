//! Each language's grammar and queries colour a representative snippet:
//! the tokens a reader looks for (keywords, names being defined, types,
//! literals, comments) come out as their kinds.

use TokenKind::*;

use super::*;

/// The block's runs that carry a colour, line by line.
fn coloured(language: Language, code: &str) -> Vec<(std::string::String, TokenKind)> {
    let highlighter = Highlighter::new();
    highlighter.prepare(language);
    let block = highlighter.highlight(code, Some(language));
    assert!(
        block.settled,
        "{language:?} is prepared, so its block is settled"
    );
    assert_eq!(
        block.lines.len(),
        code.lines().count(),
        "{language:?} keeps the code's lines"
    );
    for (line, source) in block.lines.iter().zip(code.lines()) {
        let text: std::string::String = line.iter().map(|run| run.text.as_str()).collect();
        assert_eq!(
            text, source,
            "{language:?} keeps every character of the line"
        );
    }
    block
        .lines
        .iter()
        .flatten()
        .filter(|run| run.kind != Plain && !run.text.trim().is_empty())
        .map(|run| (run.text.trim().to_owned(), run.kind))
        .collect()
}

#[track_caller]
fn assert_kinds(language: Language, code: &str, expected: &[(&str, TokenKind)]) {
    let runs = coloured(language, code);
    for (text, kind) in expected {
        assert!(
            runs.iter()
                .any(|(run, run_kind)| run == text && run_kind == kind),
            "{language:?}: expected {text:?} as {kind:?}; coloured runs: {runs:?}"
        );
    }
}

#[test]
fn rust() {
    let code = "/// Docs.\n#[derive(Debug)]\npub struct Store { path: PathBuf }\nimpl Store {\n    fn open(&self, n: u32) -> Option<String> {\n        let s = format!(\"{}\", n * 2);\n        // note\n        const MAX: usize = 3;\n        Some(s.to_string())\n    }\n}\n";
    assert_kinds(
        Language::Rust,
        code,
        &[
            ("/// Docs.", Comment),
            ("#[derive(Debug)]", Type),
            ("pub", Keyword),
            ("struct", Keyword),
            ("Store", Type),
            ("fn", Keyword),
            ("open", Function),
            ("self", Keyword),
            ("u32", Type),
            // The macro's arguments are parsed as Rust.
            ("format!", Function),
            ("\"{}\"", String),
            ("2", Number),
            ("// note", Comment),
            ("MAX", Number),
            ("to_string", Function),
        ],
    );
}

#[test]
fn swift() {
    let code = "import SwiftUI\n// note\nstruct ContentView: View {\n    @State var count = 0\n    func tap(_ n: Int) -> String {\n        let s = \"n \\(n)\"\n        return s.uppercased()\n    }\n}\n";
    assert_kinds(
        Language::Swift,
        code,
        &[
            ("import", Keyword),
            ("// note", Comment),
            ("struct", Keyword),
            ("ContentView", Type),
            ("@State", Type),
            ("var", Keyword),
            ("0", Number),
            ("func", Keyword),
            ("tap", Function),
            ("Int", Type),
            ("\"n", String),
            ("uppercased", Function),
        ],
    );
}

#[test]
fn typescript() {
    let code = "import { x } from \"./x\";\ninterface Point { x: number; y: string }\nexport function add(a: number, b: number): number {\n  // sum\n  return a + b + 1.5;\n}\nconst p = new Map<string, Point>();\n";
    assert_kinds(
        Language::TypeScript,
        code,
        &[
            ("import", Keyword),
            ("\"./x\"", String),
            ("interface", Keyword),
            ("Point", Type),
            ("number", Type),
            ("add", Function),
            ("// sum", Comment),
            ("1.5", Number),
            ("Map", Type),
        ],
    );
}

#[test]
fn tsx() {
    let code = "export const App = ({ name }: Props) => {\n  return <div className=\"app\">{name}</div>;\n};\n";
    assert_kinds(
        Language::Tsx,
        code,
        &[
            ("export", Keyword),
            ("Props", Type),
            ("div", Keyword),
            ("className", Type),
            ("\"app\"", String),
        ],
    );
}

#[test]
fn javascript() {
    let code = "const fs = require('fs');\nclass A extends B {\n  run(x) { return `v ${x}`; }\n}\nconsole.log(true, null, 42); // done\n";
    assert_kinds(
        Language::JavaScript,
        code,
        &[
            ("const", Keyword),
            ("require", Function),
            ("'fs'", String),
            ("class", Keyword),
            ("A", Type),
            ("run", Function),
            ("`v", String),
            ("true", Number),
            ("42", Number),
            ("// done", Comment),
        ],
    );
    // An interpolation is code, not part of its string.
    let runs = coloured(Language::JavaScript, "`v ${x}`;\n");
    assert!(!runs.iter().any(|(text, _)| text.contains('x')), "{runs:?}");
}

#[test]
fn python() {
    let code = "import os\n@cache\ndef f(x: int) -> str:\n    \"\"\"Doc.\"\"\"\n    if x is None:\n        return f\"{x}\"\n    return str(len([1, 2.0, True]))  # c\nclass C(Base):\n    pass\n";
    assert_kinds(
        Language::Python,
        code,
        &[
            ("import", Keyword),
            ("@cache", Function),
            ("def", Keyword),
            ("f", Function),
            ("int", Type),
            ("\"\"\"Doc.\"\"\"", String),
            ("None", Number),
            ("len", Function),
            ("2.0", Number),
            ("True", Number),
            ("# c", Comment),
            ("class", Keyword),
            ("C", Type),
        ],
    );
}

#[test]
fn go() {
    let code = "package main\nimport \"fmt\"\ntype T struct { N int }\nfunc (t *T) Run(n int) error {\n\t// c\n\tfmt.Println(n, true, nil)\n\treturn nil\n}\n";
    assert_kinds(
        Language::Go,
        code,
        &[
            ("package", Keyword),
            ("\"fmt\"", String),
            ("struct", Keyword),
            ("T", Type),
            ("func", Keyword),
            ("Run", Function),
            ("error", Type),
            ("// c", Comment),
            ("Println", Function),
            ("true", Number),
        ],
    );
}

#[test]
fn shell() {
    let code = "#!/bin/bash\nset -e\nfor f in *.rs; do\n  echo \"$f\" 42 # c\ndone\nexport X=$(pwd)\nfunction go() { ls -la; }\n";
    assert_kinds(
        Language::Shell,
        code,
        &[
            ("#!/bin/bash", Comment),
            ("for", Keyword),
            ("do", Keyword),
            ("echo", Function),
            ("# c", Comment),
            ("export", Keyword),
            ("function", Keyword),
            ("go", Function),
        ],
    );
}

#[test]
fn json() {
    let code = "{\n  \"name\": \"amux\",\n  \"n\": 1.5,\n  \"ok\": true,\n  \"x\": null\n}\n";
    assert_kinds(
        Language::Json,
        code,
        &[
            ("\"name\"", String),
            ("\"amux\"", String),
            ("1.5", Number),
            ("true", Number),
            ("null", Number),
        ],
    );
}

#[test]
fn yaml() {
    let code = "# c\nname: ci\non: [push]\njobs:\n  test:\n    runs-on: macos-15\n    steps:\n      - run: just test\n        timeout: 30\n";
    assert_kinds(
        Language::Yaml,
        code,
        &[
            ("# c", Comment),
            ("name", Keyword),
            ("ci", String),
            ("runs-on", Keyword),
            ("just test", String),
            ("30", Number),
        ],
    );
}

#[test]
fn toml() {
    let code = "# c\n[package]\nname = \"amux\"\nlto = true\nn = 3\n";
    assert_kinds(
        Language::Toml,
        code,
        &[
            ("# c", Comment),
            ("package", Type),
            ("name", Keyword),
            ("\"amux\"", String),
            ("true", Number),
            ("3", Number),
        ],
    );
    // The `=` is not part of the key.
    let block = {
        let highlighter = Highlighter::new();
        highlighter.prepare(Language::Toml);
        highlighter.highlight("a = 1\n", Some(Language::Toml))
    };
    let equals = block.lines[0]
        .iter()
        .find(|run| run.text.contains('='))
        .expect("the line has its =");
    assert_eq!(equals.kind, Plain);
}

#[test]
fn c() {
    let code = "#include <stdio.h>\n#define N 3\nstatic int add(int a, int b) { return a + b; }\nint main(void) {\n  // c\n  printf(\"%d\\n\", add(1, 2));\n  return NULL;\n}\n";
    assert_kinds(
        Language::C,
        code,
        &[
            ("#include", Keyword),
            ("<stdio.h>", String),
            ("static", Keyword),
            ("int", Type),
            ("add", Function),
            ("// c", Comment),
            ("printf", Function),
            ("\"%d\\n\"", String),
            ("1", Number),
            ("NULL", Number),
        ],
    );
}

#[test]
fn cpp() {
    let code = "#include <vector>\nnamespace amux {\ntemplate <typename T>\nclass Box : public Base {\n public:\n  auto get() const -> T { return std::move(value_); }\n  // c\n  bool ok = true;\n};\n}\n";
    assert_kinds(
        Language::Cpp,
        code,
        &[
            ("#include", Keyword),
            ("namespace", Keyword),
            ("template", Keyword),
            ("class", Keyword),
            ("Box", Type),
            ("get", Function),
            ("move", Function),
            ("// c", Comment),
            ("bool", Type),
            ("true", Number),
        ],
    );
}

#[test]
fn java() {
    let code = "package a;\nimport java.util.List;\n@Override\npublic class Main extends Base {\n  public static void main(String[] args) {\n    // c\n    System.out.println(\"hi\" + 42 + true);\n  }\n}\n";
    assert_kinds(
        Language::Java,
        code,
        &[
            ("package", Keyword),
            ("Override", Type),
            ("public", Keyword),
            ("Main", Type),
            ("void", Type),
            ("main", Function),
            ("// c", Comment),
            ("println", Function),
            ("\"hi\"", String),
            ("42", Number),
        ],
    );
}

#[test]
fn html() {
    let code = "<!DOCTYPE html>\n<html lang=\"en\">\n<!-- c -->\n<script>const x = 1;</script>\n<style>body { color: red; }</style>\n</html>\n";
    assert_kinds(
        Language::Html,
        code,
        &[
            ("html", Keyword),
            ("lang", Type),
            ("en", String),
            ("<!-- c -->", Comment),
            // Script and style bodies are JavaScript and CSS.
            ("const", Keyword),
            ("1", Number),
            ("body", Keyword),
        ],
    );
}

#[test]
fn css() {
    let code = "@media (max-width: 600px) {\n  .a > #b:hover { color: #fff; margin: 0 4px; }\n}\n/* c */\n";
    assert_kinds(
        Language::Css,
        code,
        &[
            ("@media", Keyword),
            ("color", Type),
            ("#fff", String),
            ("600", Number),
            ("/* c */", Comment),
        ],
    );
}
