//! Dump bundles replayed through all four stages with the models'
//! properties judged at every step: a healthy one, one whose core part
//! breaks an invariant, and one whose client session breaks the window's
//! one block. `support` says how each is written.

use std::process::Command;

use replay_support::bundle::Bundle;
use replay_support::replay::{Failing, Stage, Verdict};

use crate::support::{self, copy_dir};

#[test]
fn every_stage_of_a_healthy_dump_holds() {
    let dumped = support::healthy();
    let bundle = Bundle::open(dumped.path()).expect("the bundle opens");
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
    // `amux dump` from the command line has no client attached: the
    // session stage opens the store as a Subscribe does, and says so.
    assert!(bundle.clients.is_empty());
    let session = verdicts[2].to_string();
    assert!(
        session.contains("with no client attached from the store's opening"),
        "{session}"
    );
}

#[test]
fn a_dump_a_client_asked_for_replays_its_session_to_the_failing_message() {
    let dumped = support::client_violation();
    let agent = dumped.agent();
    let bundle = Bundle::open(dumped.path()).expect("the bundle opens");
    let [client] = &bundle.clients[..] else {
        panic!("{} client sessions", bundle.clients.len());
    };
    assert_eq!(client.agent, agent);
    let history = client.history.as_ref().expect("the session reads");
    // The trace was cut at the message that failed the check live: the
    // CaughtUp the client checks its window at.
    let last = history
        .events
        .iter()
        .rev()
        .find_map(|traced| match &traced.event {
            Some(wire::dump::client_traced::Event::Msg(msg)) => Some(msg),
            _ => None,
        })
        .expect("a message");
    assert!(
        matches!(
            &last.of,
            Some(wire::dump::client_session_msg::Of::Event(
                wire::SessionEvent {
                    of: Some(wire::session_event::Of::CaughtUp(_))
                }
            ))
        ),
        "{last:?}"
    );

    let verdicts = bundle.replay_all().expect("the bundle replays");
    for verdict in &verdicts {
        println!("{verdict}");
    }
    let failing: Vec<&Failing> = verdicts
        .iter()
        .filter_map(|verdict| match &verdict.verdict {
            Verdict::Fails(failing) => Some(failing),
            _ => None,
        })
        .collect();
    let [failing] = failing[..] else {
        panic!("one stage fails: {verdicts:?}");
    };
    assert_eq!(verdicts[2].stage, Stage::Session);
    assert!(matches!(verdicts[2].verdict, Verdict::Fails(_)));
    assert_eq!(failing.property, models::client::ONE_BLOCK);
    // Messages count from the checkpoint: Connection, Opening, Snapshot,
    // the row at order 1, then the row at order 3 with order 2 never
    // applied.
    assert_eq!(failing.step, 5, "{failing}");
    assert!(
        failing.what.contains(&agent) && failing.what.contains("order 3"),
        "{failing}"
    );
    assert!(failing.detail.contains("between 1 and 3"), "{failing}");
}

#[test]
fn dump_replay_names_the_clients_failing_message_and_the_property() {
    let dumped = support::client_violation();
    let output = Command::new(env!("CARGO_BIN_EXE_dump-replay"))
        .arg(dumped.path())
        .output()
        .expect("dump-replay runs");
    let stdout = String::from_utf8_lossy(&output.stdout);
    println!(
        "$ dump-replay <a client's violation>\n{stdout}exit status {:?}\n",
        output.status.code()
    );
    assert_eq!(output.status.code(), Some(1));
    let session = stdout
        .lines()
        .find(|line| line.starts_with("session"))
        .expect("a line for the session");
    assert!(
        session.contains("FAILS") && session.contains("step 5"),
        "{session}"
    );
    assert!(session.contains(models::client::ONE_BLOCK), "{session}");
    for stage in ["interpreter", "daemon core", "views"] {
        let line = stdout
            .lines()
            .find(|line| line.starts_with(stage))
            .expect("a line per stage");
        assert!(line.contains(" holds "), "{line}");
    }
}

#[test]
fn a_client_session_the_manifest_names_that_cannot_be_read_fails_the_stage() {
    let dumped = support::client_violation();
    let agent = dumped.agent();
    let events = format!("client/sessions/{agent}/events");
    let checkpoint = format!("client/sessions/{agent}/checkpoint");
    for (what, file, bytes) in [
        ("unreadable events", &events, Some(&b"\xff\xff\xff"[..])),
        ("a missing checkpoint", &checkpoint, None),
    ] {
        let scratch = tempfile::tempdir().unwrap();
        copy_dir(dumped.path(), scratch.path());
        let path = scratch.path().join(file);
        match bytes {
            Some(bytes) => std::fs::write(&path, bytes).unwrap(),
            None => std::fs::remove_file(&path).unwrap(),
        }
        let bundle = Bundle::open(scratch.path()).expect("the bundle opens");
        let verdicts = bundle.replay_all().expect("the bundle replays");
        let session = &verdicts[2];
        println!("{what}: {session}");
        assert_eq!(session.stage, Stage::Session);
        let Verdict::Unreadable(why) = &session.verdict else {
            panic!("{what}: {session}");
        };
        assert!(why.contains(&agent), "{why}");
        assert!(session.verdict.failed());

        let output = Command::new(env!("CARGO_BIN_EXE_dump-replay"))
            .arg(scratch.path())
            .output()
            .expect("dump-replay runs");
        assert_eq!(output.status.code(), Some(1), "{what}");
    }
}

#[test]
fn dump_replay_prints_one_holding_line_per_stage() {
    let dumped = support::healthy();
    let output = Command::new(env!("CARGO_BIN_EXE_dump-replay"))
        .arg(dumped.path())
        .output()
        .expect("dump-replay runs");
    let stdout = String::from_utf8_lossy(&output.stdout);
    println!("$ dump-replay <a healthy dump>\n{stdout}");
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
    let dumped = support::core_violation();
    let bundle = Bundle::open(dumped.path()).expect("the bundle opens");
    let failing = bundle
        .replay_core()
        .expect("the core replays")
        .expect("the replay fails where the live core did");
    // The second input since the checkpoint is the second stream's
    // Subscribe, the step the live core would have asked for the dump at.
    let core = bundle.core.as_ref().unwrap();
    let Some(daemon_core::Recorded::Input(input)) = core.inputs.get(1) else {
        panic!("the ring's second line is an input");
    };
    assert!(
        matches!(
            input.as_ref(),
            daemon_core::Input::ClientCall {
                request: daemon_core::payload::ClientRequest::SubscribeSession { .. },
                ..
            }
        ),
        "{input:?}"
    );
    assert_eq!(failing.step, 2, "{failing}");
    assert_eq!(failing.property, daemon_core::GENERATION_FELL.sentence);
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
    let dumped = support::core_violation();
    let output = Command::new(env!("CARGO_BIN_EXE_dump-replay"))
        .arg(dumped.path())
        .output()
        .expect("dump-replay runs");
    let stdout = String::from_utf8_lossy(&output.stdout);
    println!(
        "$ dump-replay <a core's violation>\n{stdout}exit status {:?}\n",
        output.status.code()
    );
    assert_eq!(output.status.code(), Some(1));
    let core = stdout
        .lines()
        .find(|line| line.starts_with("daemon core"))
        .expect("a line for the core");
    assert!(core.contains("FAILS") && core.contains("step 2"), "{core}");
    assert!(
        core.contains(daemon_core::GENERATION_FELL.sentence),
        "{core}"
    );
}

#[test]
fn a_ring_missing_a_read_fails_the_reproduction_property() {
    // The ring loses the line saying how far the core's first ingest read
    // the journal: replayed, that step reads the journal all the same, and
    // the replay no longer reproduces the run it was recorded from.
    let dumped = support::healthy();
    let inputs = dumped.path().join("core/inputs");
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

    let bundle = Bundle::open(dumped.path()).unwrap();
    let failing = bundle
        .replay_core()
        .unwrap()
        .expect("the replay reads what the ring does not say");
    println!("{failing}");
    assert_eq!(failing.property, models::single_daemon::REPRODUCES);
    assert_eq!(failing.step, step, "{failing}");
}
