//! The journal fixtures the simulated net's in-process agents play (an
//! agent declared with `AgentDecl::journal` writes one as its own): every
//! file listed, every frame whole, every journal obeying the interpreter's
//! invariants and reaching what its listing says it reaches.
//!
//! The fixtures compare and never regenerate. `cut_missing_journals` writes
//! a listed journal whose file does not exist yet and refuses to touch one
//! that does; it is ignored so no ordinary run writes anything.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use interpret::claude_pty::ClaudePty;
use interpret::claude_sdk::ClaudeSdk;
use interpret::codex::Codex;
use interpret::testkind::TestKind;
use interpret::{EndRules, Interpreter, check_invariants};
use prost::Message as _;
use serde::Deserialize;
use wire::Step;

fn dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/journals")
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Listing {
    journal: Vec<Journal>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    name: String,
    kind: String,
    source: String,
    reaches: Vec<Reach>,
    #[serde(default)]
    queue_may_remain: bool,
    #[serde(default)]
    streams_may_remain: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum Reach {
    /// A step ends a turn.
    TurnEnd,
    /// A snapshot holds an open ask.
    Ask,
    /// Work handed in while a turn ran is taken up after that turn ends: a
    /// later turn ends after a queued prompt or an agent's message arrived
    /// during the first.
    FollowUp,
    /// The last step, and no other, ends the incarnation.
    Exit,
}

impl Journal {
    fn frames(&self) -> PathBuf {
        dir().join(format!("{}.frames", self.name))
    }

    fn end(&self) -> EndRules {
        EndRules {
            queue_may_remain: self.queue_may_remain,
            streams_may_remain: self.streams_may_remain,
        }
    }
}

fn listing() -> Vec<Journal> {
    let text = std::fs::read_to_string(dir().join("journals.toml")).expect("read journals.toml");
    toml::from_str::<Listing>(&text)
        .expect("parse journals.toml")
        .journal
}

/// Every frame of a frames file, in order; a torn or corrupt tail fails.
fn read_frames(path: &Path) -> Vec<Step> {
    let bytes =
        std::fs::read(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    let mut steps = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        match journal::decode_frame(&bytes[at..]) {
            journal::Frame::Whole(step, length) => {
                steps.push(*step);
                at += length;
            }
            other => panic!(
                "{}: byte {at} is not a whole frame: {other:?}",
                path.display()
            ),
        }
    }
    steps
}

/// What the journal fails to reach of what its listing claims.
fn unreached<I: Interpreter>(steps: &[Step], reaches: &[Reach]) -> Vec<String> {
    let turn_ends = steps
        .iter()
        .enumerate()
        .filter(|(_, step)| step.turn_end.is_some())
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let handed_in_before = |end: usize| {
        steps[..end].iter().any(|step| {
            step.snapshot
                .as_ref()
                .is_some_and(|snapshot| !snapshot.queue.is_empty())
                || step
                    .items
                    .iter()
                    .any(|item| I::describe_item(&item.body).arm == "agent_message")
        })
    };
    let ends = steps
        .iter()
        .enumerate()
        .filter(|(_, step)| step.incarnation_end.is_some())
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    reaches
        .iter()
        .filter(|reach| match reach {
            Reach::TurnEnd => turn_ends.is_empty(),
            Reach::Ask => !steps.iter().any(|step| {
                step.snapshot
                    .as_ref()
                    .is_some_and(|snapshot| !I::describe_snapshot(&snapshot.body).asks.is_empty())
            }),
            Reach::FollowUp => !(turn_ends.len() >= 2 && handed_in_before(turn_ends[0])),
            Reach::Exit => ends != [steps.len() - 1],
        })
        .map(|reach| format!("{reach:?}"))
        .collect()
}

fn judge<I: Interpreter>(journal: &Journal) -> Vec<String> {
    let steps = read_frames(&journal.frames());
    let mut problems = check_invariants::<I>(&steps, &journal.end());
    for reach in unreached::<I>(&steps, &journal.reaches) {
        problems.push(format!("never reaches {reach}"));
    }
    problems
}

/// The journal the agent would write for a script: its interpreter's steps,
/// leaving out the empty ones as the journal's writer does.
fn cut<I: Interpreter>(source: &Path) -> Vec<u8> {
    interpret::replay::<I>(source)
        .unwrap_or_else(|error| panic!("replay {}: {error}", source.display()))
        .iter()
        .filter(|frame| frame.step.encoded_len() > 0)
        .flat_map(|frame| journal::encode_frame(&frame.step))
        .collect()
}

macro_rules! by_kind {
    ($kind:expr, $function:ident, $($argument:expr),*) => {
        match $kind {
            "claude_pty" => $function::<ClaudePty>($($argument),*),
            "claude_sdk" => $function::<ClaudeSdk>($($argument),*),
            "codex" => $function::<Codex>($($argument),*),
            "test" => $function::<TestKind>($($argument),*),
            other => panic!("no interpreter is named {other}"),
        }
    };
}

#[test]
fn every_journal_is_listed_whole_and_obeys_the_interpreters_invariants() {
    let journals = listing();
    let mut failures = Vec::new();
    for journal in &journals {
        let source = dir().join(&journal.source);
        if !source.exists() {
            failures.push(format!(
                "{}: no source at {}",
                journal.name,
                source.display()
            ));
        }
        if !journal.frames().exists() {
            failures.push(format!(
                "{}: no frames file; cut it with `cargo test -p testnet --test journal_fixtures -- --ignored cut_missing_journals`",
                journal.name
            ));
            continue;
        }
        for problem in by_kind!(journal.kind.as_str(), judge, journal) {
            failures.push(format!("{}: {problem}", journal.name));
        }
    }

    let listed = journals
        .iter()
        .map(|journal| format!("{}.frames", journal.name))
        .collect::<BTreeSet<_>>();
    assert_eq!(listed.len(), journals.len(), "a journal is listed twice");
    for entry in std::fs::read_dir(dir()).expect("read the fixtures") {
        let name = entry.expect("entry").file_name();
        let name = name.to_string_lossy();
        if name.ends_with(".frames") && !listed.contains(name.as_ref()) {
            failures.push(format!("{name} is not in journals.toml"));
        }
    }

    let reached = journals
        .iter()
        .flat_map(|journal| journal.reaches.iter().copied())
        .collect::<Vec<_>>();
    for reach in [Reach::TurnEnd, Reach::Ask, Reach::FollowUp, Reach::Exit] {
        if !reached.contains(&reach) {
            failures.push(format!("no journal reaches {reach:?}"));
        }
    }
    for kind in ["claude_pty", "claude_sdk", "codex"] {
        if !journals.iter().any(|journal| journal.kind == kind) {
            failures.push(format!("no journal is cut from a {kind} recording"));
        }
    }

    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
#[ignore = "writes fixtures; run by hand to add a journal"]
fn cut_missing_journals() {
    for journal in listing() {
        let path = journal.frames();
        if path.exists() {
            continue;
        }
        let bytes = by_kind!(journal.kind.as_str(), cut, &dir().join(&journal.source));
        std::fs::write(&path, bytes)
            .unwrap_or_else(|error| panic!("write {}: {error}", path.display()));
        println!("cut {}", path.display());
    }
}
