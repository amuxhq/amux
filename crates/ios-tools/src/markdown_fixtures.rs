//! Markdown parsed by the shared parser for clients that draw without the
//! bridge: the Mac storybook, the iPhone catalogue, the Android storybook
//! and the client tests that draw prose. Each builds its rows from values
//! alone, and a row's words come parsed, so the parse they would have had
//! is checked in here: `cargo run -p ios-tools -- markdown-fixtures` writes
//! it and `--check` fails when the committed file differs.
//!
//! A client looks a text up by its source, so a story keeps its words
//! where it uses them. A text a story uses must be listed here, word for
//! word; the lookup fails loudly for one that is not.

use serde::Serialize;
use ui_view::{InlineRun, Markdown, inline_runs};

use crate::swift_types::write_or_check;

/// Where the fixtures live, from the repository root. The Swift package
/// bundles the file as a resource and the Android storybook reads it from
/// the same place, so every client draws the same parse.
pub const OUTPUT: &str =
    "apps/apple/Packages/AmuxCore/Sources/AmuxFixtures/Fixtures/markdown-fixtures.json";

/// Every text the stories, the catalogue and the client tests draw as prose.
const SOURCES: &[&str] = &[
    // The Mac storybook.
    "The relay test fails about one run in twenty on CI. Can you find out why?",
    "I'll start with the test and the last few CI logs.",
    "Found it. The test cuts the link **before the relay has acknowledged the hello**, so on a slow runner the reconnect races the first handshake.\n\n- `reconnect_after_cut` sleeps 50 ms instead of waiting for the link to settle\n- on CI the hello takes up to 80 ms\n\nThe fix is to wait on the settled event:\n\n```rust\nlink.settled().await;\nrelay.cut(&link);\n```",
    "Draft the release notes for 0.9.0 from the changelog.",
    "Audit the pairing code four ways at once and report back.",
    "Starting four subagents, one per area.",
    "Survey the relay's retry path: where a dropped link is retried, and what bounds it.",
    "The dialler retries a dropped link with backoff capped at 30 s and gives up at the link's deadline. The cap is `MAX_BACKOFF` in dial.rs.",
    "Two things on the test.",
    "Both done: the dial names its host in the error, and the wait for the link to settle is bounded by `SETTLE`, so a link that never settles fails the test instead of hanging it.",
    // The iPhone catalogue and the Android storybook.
    "Collapse the pairing errors onto one string.",
    "Here is the screen and the log.",
    "These two, then run the suite.",
    "Use the new error table instead.",
    "> The retry only fires when the socket closes cleanly.\n\nOh **really**?",
    "3 specs updated, all green.",
    "Please rebase onto main first.",
    "Done. The three arms are one now.",
    "Plan how to collapse the pairing errors.",
    "Done. The three arms are one now, and the new test asserts on the single string. Clippy still flags an unused import I left for you to decide on.",
    "Drop the unused import and run the whole suite.",
    "Split the pairing audit four ways and report back.",
    "Survey the relay\u{2019}s retry path: where a dropped link is retried, and what bounds it.",
    "The dialler retries a dropped link with backoff capped at 30 s, and gives up after the link\u{2019}s deadline. The cap is `MAX_BACKOFF` in dial.rs.",
    "Done. The three arms are **one** now:\n\n- `Code::NotFound` and `Code::InvalidPin` share one string\n- the new test asserts on it\n\n```rust\n_ => \"could not pair, check the code\",\n```",
    "The guard now names the host:\n\n```swift\n// Refuse a pairing the host did not offer.\nfunc refuse(_ code: Int) throws -> String {\n    guard code > 0 else { throw PairingError.unknown }\n    return \"no such host: \\(code)\"\n}\n```\n\n```ts\nexport const retry = (tries: number): Promise<void> => wait(250 * tries)\n```\n\n```diff\n-      Code::NotFound => \"no such host\",\n+      _ => \"could not pair, check the code\",\n```",
    "The client maps gRPC statuses onto distinct strings in three places. The protocol refuses to distinguish them, so the client must not either.\n\n1. One arm, one string\n2. Update the three specs",
    // The client tests that draw prose.
    "1. Split the read path.\n2. Run the spec suite.",
    "1. Split read and write paths.\n2. Add unit tests.",
    "1. Split read and write paths.",
    "Updated backoff logic in `retry.rs`.",
];

/// Texts drawn while they are still being written.
const STREAMING: &[&str] = &[
    "Checking which crates read the table first.",
    "Here is the loop:\n\n```python\nfor host in hosts:\n    if host.reachable:\n        pair(host, retries=3",
];

/// Labels an agent wrote, such as questions and options, whose inline
/// markdown the stories draw. A label with no markup in it needs no entry:
/// it is its words.
const LABELS: &[&str] = &[
    "`q1/README.md` is absent; `../plan1/README.md` has two lines (`UPDATED`, `done`). Which should be used?",
    "`q1/README.md` is absent; `../plan1/README.md` has two lines. Which should be used?",
    "Create `README.md`",
    "Create the missing file with exactly one line: `UPDATED`.",
    "Change `../plan1/README.md` instead.",
];

#[derive(Serialize)]
struct Fixtures {
    /// Finished texts, parsed.
    finished: Vec<Markdown>,
    /// Texts still being written, parsed as such.
    streaming: Vec<Markdown>,
    /// Labels, read for their inline markdown.
    labels: Vec<Label>,
}

#[derive(Serialize)]
struct Label {
    source: String,
    runs: Vec<InlineRun>,
}

/// The fixtures file's contents.
pub fn generate() -> String {
    let fixtures = Fixtures {
        finished: SOURCES
            .iter()
            .map(|source| Markdown::parse(source, false))
            .collect(),
        streaming: STREAMING
            .iter()
            .map(|source| Markdown::parse(source, true))
            .collect(),
        labels: LABELS
            .iter()
            .map(|source| Label {
                source: (*source).to_owned(),
                runs: inline_runs(source),
            })
            .collect(),
    };
    let mut json = serde_json::to_string_pretty(&fixtures).expect("fixtures serialize");
    json.push('\n');
    json
}

/// `markdown-fixtures [--check]`, run from anywhere in the repository.
pub fn main(args: &[String]) -> std::result::Result<(), Box<dyn std::error::Error>> {
    let check = match args {
        [] => false,
        [flag] if flag == "--check" => true,
        _ => return Err("usage: ios-tools markdown-fixtures [--check]".into()),
    };
    write_or_check("markdown-fixtures", &[(OUTPUT, generate())], check)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::swift_types::repository;

    #[test]
    fn the_committed_fixtures_match_the_parser() {
        let committed = std::fs::read_to_string(repository().join(OUTPUT)).unwrap_or_default();
        assert!(
            committed == generate(),
            "run `cargo run -p ios-tools -- markdown-fixtures` and commit the result"
        );
    }

    #[test]
    fn each_text_is_listed_once() {
        let mut seen = HashSet::new();
        for source in SOURCES.iter().chain(STREAMING).chain(LABELS) {
            assert!(seen.insert(source), "listed twice: {source:?}");
        }
    }
}
