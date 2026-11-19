//! Which language a piece of code is in: from a fence's language word, or
//! from a file's path and first line. Only these tables decide it; nothing
//! guesses from the code itself, because a guess that is wrong colours a
//! block worse than drawing it plain.

/// A language the crate colours. Every one but [`Language::Diff`] has a
/// tree-sitter grammar; a diff is coloured by its lines' prefixes alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Language {
    Rust,
    Swift,
    TypeScript,
    Tsx,
    JavaScript,
    Python,
    Go,
    /// Bash, and the shells and transcripts close enough to read as it.
    Shell,
    Json,
    Yaml,
    Toml,
    C,
    Cpp,
    Java,
    Html,
    Css,
    /// A unified diff: added and removed lines, no grammar.
    Diff,
}

impl Language {
    /// Every language, in a fixed order.
    pub const ALL: [Language; 17] = [
        Language::Rust,
        Language::Swift,
        Language::TypeScript,
        Language::Tsx,
        Language::JavaScript,
        Language::Python,
        Language::Go,
        Language::Shell,
        Language::Json,
        Language::Yaml,
        Language::Toml,
        Language::C,
        Language::Cpp,
        Language::Java,
        Language::Html,
        Language::Css,
        Language::Diff,
    ];

    /// The language's canonical lowercase name, one [`Language::from_fence`]
    /// reads back, so it can stand for the language wherever a word must.
    pub fn name(self) -> &'static str {
        match self {
            Language::Rust => "rust",
            Language::Swift => "swift",
            Language::TypeScript => "typescript",
            Language::Tsx => "tsx",
            Language::JavaScript => "javascript",
            Language::Python => "python",
            Language::Go => "go",
            Language::Shell => "bash",
            Language::Json => "json",
            Language::Yaml => "yaml",
            Language::Toml => "toml",
            Language::C => "c",
            Language::Cpp => "cpp",
            Language::Java => "java",
            Language::Html => "html",
            Language::Css => "css",
            Language::Diff => "diff",
        }
    }

    /// The language a fence's info string names. Only its first word
    /// counts (`rust,ignore`, `python {.numberLines}`), in any case. The
    /// words are the languages' names, their usual file extensions and the
    /// other names agents write; anything else is drawn plain.
    pub fn from_fence(info: &str) -> Option<Language> {
        let word = info
            .trim_start()
            .split(|c: char| c.is_whitespace() || c == ',' || c == '{')
            .next()?
            .trim_start_matches('.')
            .to_ascii_lowercase();
        Some(match word.as_str() {
            "rust" | "rs" => Language::Rust,
            "swift" => Language::Swift,
            "typescript" | "ts" | "mts" | "cts" => Language::TypeScript,
            "tsx" => Language::Tsx,
            "javascript" | "js" | "mjs" | "cjs" | "jsx" | "node" => Language::JavaScript,
            "python" | "py" | "python3" | "py3" | "pyi" => Language::Python,
            "go" | "golang" => Language::Go,
            // A console transcript is mostly commands; its output lines
            // read as words, which is how bash colours them anyway.
            "bash" | "sh" | "shell" | "zsh" | "ksh" | "dash" | "console" | "shellscript"
            | "shell-session" => Language::Shell,
            "json" | "jsonc" | "json5" | "jsonl" => Language::Json,
            "yaml" | "yml" => Language::Yaml,
            "toml" => Language::Toml,
            "c" | "h" => Language::C,
            "cpp" | "c++" | "cc" | "cxx" | "hpp" | "hh" | "hxx" => Language::Cpp,
            "java" => Language::Java,
            "html" | "htm" | "xhtml" => Language::Html,
            "css" => Language::Css,
            "diff" | "patch" | "udiff" => Language::Diff,
            _ => return None,
        })
    }

    /// The language of a file from its path alone: its name for the few
    /// files known by name, else its extension.
    pub fn from_path(path: &str) -> Option<Language> {
        let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
        let by_name = match name {
            ".bashrc" | ".bash_profile" | ".bash_aliases" | ".bash_logout" | ".profile"
            | ".zshrc" | ".zshenv" | ".zprofile" | ".zlogin" | "PKGBUILD" => Some(Language::Shell),
            "Cargo.lock" => Some(Language::Toml),
            _ => None,
        };
        if by_name.is_some() {
            return by_name;
        }
        // A leading dot names a hidden file, not an extension.
        let (stem, extension) = name.rsplit_once('.')?;
        if stem.is_empty() {
            return None;
        }
        Some(match extension.to_ascii_lowercase().as_str() {
            "rs" => Language::Rust,
            "swift" => Language::Swift,
            "ts" | "mts" | "cts" => Language::TypeScript,
            "tsx" => Language::Tsx,
            "js" | "mjs" | "cjs" | "jsx" => Language::JavaScript,
            "py" | "pyi" | "pyw" => Language::Python,
            "go" => Language::Go,
            "sh" | "bash" | "zsh" | "ksh" => Language::Shell,
            "json" | "jsonc" | "json5" | "jsonl" => Language::Json,
            "yaml" | "yml" => Language::Yaml,
            "toml" => Language::Toml,
            "c" | "h" => Language::C,
            "cpp" | "cc" | "cxx" | "c++" | "hpp" | "hh" | "hxx" | "h++" => Language::Cpp,
            "java" => Language::Java,
            "html" | "htm" | "xhtml" => Language::Html,
            "css" => Language::Css,
            "diff" | "patch" => Language::Diff,
            _ => return None,
        })
    }

    /// The language a script's `#!` line runs it in: the interpreter's
    /// name, read through `env` and its options, without a version suffix
    /// (`python3.12` is Python).
    pub fn from_shebang(line: &str) -> Option<Language> {
        let rest = line.strip_prefix("#!")?;
        let mut words = rest.split_whitespace();
        let mut program = basename(words.next()?);
        if program == "env" {
            program = basename(words.find(|word| !word.starts_with('-') && !word.contains('='))?);
        }
        let program = program.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.');
        Some(match program {
            "sh" | "bash" | "zsh" | "ksh" | "dash" | "ash" => Language::Shell,
            // `uv run --script` runs a Python file with its inline metadata.
            "python" | "pypy" | "uv" => Language::Python,
            "node" | "nodejs" => Language::JavaScript,
            "deno" | "bun" | "ts-node" | "tsx" => Language::TypeScript,
            "swift" => Language::Swift,
            _ => return None,
        })
    }

    /// The language of a file: its path first, then the `#!` line its
    /// content starts with, for the scripts that carry no extension.
    pub fn from_file(path: &str, content: &str) -> Option<Language> {
        Language::from_path(path).or_else(|| Language::from_shebang(content.lines().next()?))
    }
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::Language::{self, *};

    #[test]
    fn fence_words_pick_through_the_alias_table() {
        for (word, language) in [
            ("rust", Rust),
            ("rs", Rust),
            ("rust,ignore", Rust),
            ("Rust", Rust),
            (" rust", Rust),
            ("swift", Swift),
            ("ts", TypeScript),
            ("typescript", TypeScript),
            ("tsx", Tsx),
            ("js", JavaScript),
            ("mjs", JavaScript),
            ("cjs", JavaScript),
            ("jsx", JavaScript),
            ("py", Python),
            ("python3", Python),
            ("python {.numberLines}", Python),
            ("go", Go),
            ("golang", Go),
            ("bash", Shell),
            ("sh", Shell),
            ("zsh", Shell),
            ("console", Shell),
            ("shell", Shell),
            ("json", Json),
            ("jsonc", Json),
            ("yml", Yaml),
            ("yaml", Yaml),
            ("toml", Toml),
            ("c", C),
            ("h", C),
            ("cpp", Cpp),
            ("c++", Cpp),
            ("hpp", Cpp),
            ("java", Java),
            ("html", Html),
            ("css", Css),
            ("diff", Diff),
            ("patch", Diff),
        ] {
            assert_eq!(
                Language::from_fence(word),
                Some(language),
                "fence word {word:?}"
            );
        }
        for word in ["", "text", "plaintext", "sql", "ruby", "elixir", "mermaid"] {
            assert_eq!(Language::from_fence(word), None, "fence word {word:?}");
        }
    }

    #[test]
    fn every_name_reads_back_as_its_language() {
        for language in Language::ALL {
            assert_eq!(Language::from_fence(language.name()), Some(language));
        }
    }

    #[test]
    fn paths_pick_by_extension_or_known_name() {
        for (path, language) in [
            ("crates/highlight/src/lib.rs", Rust),
            ("App/Sources/View.swift", Swift),
            ("web/index.ts", TypeScript),
            ("web/App.tsx", Tsx),
            ("web/main.mjs", JavaScript),
            ("tools/run.py", Python),
            ("cmd/main.go", Go),
            ("scripts/install.sh", Shell),
            ("package.json", Json),
            (".github/workflows/ci.yml", Yaml),
            ("Cargo.toml", Toml),
            ("Cargo.lock", Toml),
            ("src/parser.c", C),
            ("include/tree_sitter/api.h", C),
            ("src/scanner.cc", Cpp),
            ("Main.java", Java),
            ("docs/index.html", Html),
            ("site/style.css", Css),
            ("fix.patch", Diff),
            ("SHOUTING.RS", Rust),
            ("windows\\path\\main.go", Go),
            ("home/.zshrc", Shell),
        ] {
            assert_eq!(Language::from_path(path), Some(language), "path {path:?}");
        }
        for path in [
            "Makefile",
            "LICENSE",
            ".gitignore",
            "notes.txt",
            "query.sql",
            "archive.tar.gz",
        ] {
            assert_eq!(Language::from_path(path), None, "path {path:?}");
        }
    }

    #[test]
    fn shebangs_pick_the_interpreter() {
        for (line, language) in [
            ("#!/bin/sh", Shell),
            ("#!/bin/bash -e", Shell),
            ("#!/usr/bin/env bash", Shell),
            ("#!/usr/bin/env zsh", Shell),
            ("#!/usr/bin/env python3", Python),
            ("#!/usr/bin/python3.12", Python),
            ("#!/usr/bin/env -S uv run --script", Python),
            ("#!/usr/bin/env node", JavaScript),
            ("#!/usr/bin/env -S deno run --allow-read", TypeScript),
            ("#!/usr/bin/env -i PATH=/bin bash", Shell),
            ("#! /bin/sh", Shell),
        ] {
            assert_eq!(
                Language::from_shebang(line),
                Some(language),
                "shebang {line:?}"
            );
        }
        for line in [
            "#!/usr/bin/env ruby",
            "#!/usr/bin/env",
            "# a comment",
            "bash",
            "",
        ] {
            assert_eq!(Language::from_shebang(line), None, "shebang {line:?}");
        }
    }

    #[test]
    fn a_file_reads_its_extension_before_its_shebang() {
        assert_eq!(
            Language::from_file("bin/tool", "#!/usr/bin/env python3\nprint(1)\n"),
            Some(Python)
        );
        assert_eq!(
            Language::from_file("bin/tool.sh", "#!/usr/bin/env python3\n"),
            Some(Shell)
        );
        assert_eq!(Language::from_file("bin/tool", "print(1)\n"), None);
        assert_eq!(Language::from_file("bin/tool", ""), None);
    }
}
