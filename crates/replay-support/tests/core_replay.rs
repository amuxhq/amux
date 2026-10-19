//! Dump bundles replayed through all four stages with the models'
//! properties judged at every step. Both fixtures are bundles a real daemon
//! wrote on a net served through `testnet ctl`, one host and one agent on
//! the fake Claude SDK provider. `fixtures/healthy`: two turns, then `amux
//! dump`. `fixtures/violation`: the dump the core asked for itself when a
//! stream opened under a lower generation than one before it, on a build
//! that recorded the highest generation opened as one more than it was.
//! The first stream raised the record to 2 and a checkpoint kept it, so
//! the second stream, after that checkpoint, breaks the invariant again
//! when replayed by this build, which records it correctly.

use std::path::{Path, PathBuf};
use std::process::Command;

use replay_support::bundle::Bundle;
use replay_support::replay::{Stage, Verdict};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

#[test]
fn every_stage_of_a_healthy_dump_holds() {
    let bundle = Bundle::open(&fixture("healthy")).expect("the fixture opens");
    let core = bundle.core.as_ref().expect("the bundle carries the core");
    assert!(
        core.inputs
            .iter()
            .any(|line| matches!(line, daemon_core::Recorded::Read { .. })),
        "the core read a journal since its checkpoint, so the replay lays one out"
    );
    assert_eq!(bundle.replay_core().expect("the core replays"), None);

    let verdicts = bundle.replay_all().expect("the bundle replays");
    let stages: Vec<Stage> = verdicts.iter().map(|verdict| verdict.stage).collect();
    assert_eq!(
        stages,
        [
            Stage::Interpreter,
            Stage::Core,
            Stage::Session,
            Stage::Views
        ]
    );
    for verdict in &verdicts {
        assert!(matches!(verdict.verdict, Verdict::Holds(_)), "{verdict}");
    }
}

#[test]
fn dump_replay_prints_one_holding_line_per_stage() {
    let output = Command::new(env!("CARGO_BIN_EXE_dump-replay"))
        .arg(fixture("healthy"))
        .output()
        .expect("dump-replay runs");
    let stdout = String::from_utf8_lossy(&output.stdout);
    println!("$ dump-replay tests/fixtures/healthy\n{stdout}");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 4, "{stdout}");
    for (line, stage) in lines
        .iter()
        .zip(["interpreter", "daemon core", "session", "views"])
    {
        assert!(line.starts_with(stage), "{line}");
        assert!(line.contains(" holds "), "{line}");
    }
}

#[test]
fn a_dump_the_core_asked_for_replays_to_its_failing_step_and_names_the_invariant() {
    let dir = fixture("violation");
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join("manifest.json")).unwrap()).unwrap();
    let reason = manifest["reason"].as_str().unwrap();
    assert!(
        reason.starts_with("the daemon core: ") && reason.contains(daemon_core::GENERATION_FELL),
        "{reason}"
    );
    assert_eq!(manifest["automatic"], true);

    let bundle = Bundle::open(&dir).expect("the fixture opens");
    let failing = bundle
        .replay_core()
        .expect("the core replays")
        .expect("the replay fails where the live core did");
    // The fourth input since the checkpoint is the second stream's
    // Subscribe, the step the live core asked for the dump at.
    let core = bundle.core.as_ref().unwrap();
    let Some(daemon_core::Recorded::Input(input)) = core.inputs.get(3) else {
        panic!("the ring's fourth line is an input");
    };
    assert!(
        matches!(
            input.as_ref(),
            daemon_core::Input::ClientCall {
                request: daemon_core::payload::ClientRequest::Subscribe { .. },
                ..
            }
        ),
        "{input:?}"
    );
    assert_eq!(failing.step, 4, "{failing}");
    assert_eq!(failing.property, daemon_core::GENERATION_FELL);
    assert!(
        failing.detail.contains("generation 1 after one under 2"),
        "{failing}"
    );

    let verdicts = bundle.replay_all().expect("the bundle replays");
    let failing_stages: Vec<Stage> = verdicts
        .iter()
        .filter(|verdict| matches!(verdict.verdict, Verdict::Fails(_)))
        .map(|verdict| verdict.stage)
        .collect();
    assert_eq!(failing_stages, [Stage::Core], "the core is the culprit");
}

#[test]
fn dump_replay_exits_non_zero_naming_the_step_and_the_property() {
    let output = Command::new(env!("CARGO_BIN_EXE_dump-replay"))
        .arg(fixture("violation"))
        .output()
        .expect("dump-replay runs");
    let stdout = String::from_utf8_lossy(&output.stdout);
    println!(
        "$ dump-replay tests/fixtures/violation\n{stdout}exit status {:?}\n",
        output.status.code()
    );
    assert_eq!(output.status.code(), Some(1));
    let core = stdout
        .lines()
        .find(|line| line.starts_with("daemon core"))
        .expect("a line for the core");
    assert!(core.contains("FAILS") && core.contains("step 4"), "{core}");
    assert!(core.contains(daemon_core::GENERATION_FELL), "{core}");
}

#[test]
fn a_ring_missing_a_read_fails_the_reproduction_property() {
    // The ring loses the line saying how far the core's first ingest read
    // the journal: replayed, that step reads the journal all the same, and
    // the replay no longer reproduces the run it was recorded from.
    let scratch = tempfile::tempdir().unwrap();
    copy_dir(&fixture("healthy"), scratch.path());
    let inputs = scratch.path().join("core/inputs");
    let ring = std::fs::read_to_string(&inputs).unwrap();
    let mut lines: Vec<&str> = ring.lines().collect();
    let at = lines
        .iter()
        .position(|line| line.starts_with("{\"Read\""))
        .expect("the ring records a read");
    lines.remove(at);
    std::fs::write(&inputs, lines.join("\n") + "\n").unwrap();
    // Steps count inputs: the read's is the input before it.
    let step = lines[..at]
        .iter()
        .filter(|line| line.starts_with("{\"Input\""))
        .count();

    let bundle = Bundle::open(scratch.path()).unwrap();
    let failing = bundle
        .replay_core()
        .unwrap()
        .expect("the replay reads what the ring does not say");
    println!("{failing}");
    assert_eq!(failing.property, models::single_daemon::REPRODUCES);
    assert_eq!(failing.step, step, "{failing}");
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}
