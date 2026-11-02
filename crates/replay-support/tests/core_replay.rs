//! Dump bundles replayed through their stages with the models' properties
//! judged at every step: a healthy one, one whose core record breaks an
//! invariant, one whose chat's record breaks the window's one block, and
//! one of every trigger after a run marked in everything anybody wrote.
//! `support` says how each is written.

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
    assert_eq!(stages, [Stage::Agent, Stage::Core, Stage::Session]);
    for verdict in &verdicts[..2] {
        assert!(matches!(verdict.verdict, Verdict::Holds(_)), "{verdict}");
    }
    // `amux dump` from the command line shows no chat: the session stage
    // has nothing to replay, and says so.
    assert!(bundle.clients.is_empty());
    assert!(
        matches!(verdicts[2].verdict, Verdict::Absent(_)),
        "{}",
        verdicts[2]
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
fn repro_names_the_clients_failing_message_and_the_property() {
    let dumped = support::client_violation();
    let output = support::repro(dumped.path());
    let stdout = String::from_utf8_lossy(&output.stdout);
    println!(
        "$ repro <a client's violation>\n{stdout}exit status {:?}\n",
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
    let core = stdout
        .lines()
        .find(|line| line.starts_with("daemon core"))
        .expect("a line for the core");
    assert!(core.contains(" holds "), "{core}");
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

        let output = support::repro(scratch.path());
        assert_eq!(output.status.code(), Some(1), "{what}");
    }
}

#[test]
fn repro_prints_one_line_per_stage() {
    let dumped = support::healthy();
    let output = support::repro(dumped.path());
    let stdout = String::from_utf8_lossy(&output.stdout);
    println!("$ repro <a healthy dump>\n{stdout}");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 6, "{stdout}");
    assert!(lines[0].starts_with("== "), "{stdout}");
    assert!(
        lines[1].starts_with("agent") && lines[1].contains(" holds "),
        "{stdout}"
    );
    assert!(
        lines[2].starts_with("daemon core") && lines[2].contains(" holds "),
        "{stdout}"
    );
    assert!(
        lines[3].starts_with("session") && lines[3].contains(" absent "),
        "{stdout}"
    );
    assert!(lines[4].starts_with("kept "), "{stdout}");
    assert!(lines[5].starts_with("to make it a test: "), "{stdout}");
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
fn repro_exits_non_zero_naming_the_step_and_the_property() {
    let dumped = support::core_violation();
    let output = support::repro(dumped.path());
    let stdout = String::from_utf8_lossy(&output.stdout);
    println!(
        "$ repro <a core's violation>\n{stdout}exit status {:?}\n",
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

/// Whatever asked for it, a report carries none of what anybody wrote,
/// in any form, and its core's record still replays.
#[test]
fn no_report_of_any_trigger_carries_a_word_and_each_core_replays() {
    let marker = support::MARKER;
    // A marker in any of the forms a bundle could carry it.
    let forms = [
        marker.as_bytes().to_vec(),
        marker.to_uppercase().into_bytes(),
        marker
            .bytes()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
            .into_bytes(),
    ];
    let mut triggers = Vec::new();
    for dumped in support::marked() {
        let manifest = dumped.manifest();
        triggers.push(manifest["trigger"].as_str().unwrap().to_owned());
        assert!(manifest["core"]["over_limit"].is_null(), "{manifest}");
        let mut files = Vec::new();
        list(dumped.path(), dumped.path(), &mut files);
        for (name, bytes) in &files {
            for form in &forms {
                assert!(
                    !bytes.windows(form.len()).any(|window| window == form),
                    "{name} holds the marker"
                );
            }
        }
        let bundle = Bundle::open(dumped.path()).expect("the bundle opens");
        let core = bundle.core.as_ref().expect("the bundle carries the core");
        let inputs = core
            .inputs
            .iter()
            .filter(|line| matches!(line, daemon_core::Recorded::Input(_)))
            .count();
        assert!(inputs > 0, "the window since the checkpoint has inputs");
        assert_eq!(bundle.replay_core().expect("the core replays"), None);
        let verdicts = bundle.replay_all().expect("the bundle replays");
        println!(
            "{}: {} files, {} bytes; {}",
            manifest["trigger"],
            files.len(),
            files.iter().map(|(_, bytes)| bytes.len()).sum::<usize>(),
            verdicts[1]
        );
        assert!(
            matches!(verdicts[1].verdict, Verdict::Holds(_)),
            "{}",
            verdicts[1]
        );
        for verdict in &verdicts {
            assert!(!verdict.verdict.failed(), "{verdict}");
        }
    }
    let mut expected: Vec<String> = support::TRIGGERS
        .iter()
        .map(|(_, check)| {
            if check.is_empty() {
                "person".to_owned()
            } else {
                (*check).to_owned()
            }
        })
        .collect();
    expected.extend(expected.clone());
    assert_eq!(triggers, expected, "each trigger on each host");
}

fn list(root: &std::path::Path, dir: &std::path::Path, out: &mut Vec<(String, Vec<u8>)>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            list(root, &path, out);
        } else {
            let name = path.strip_prefix(root).unwrap().display().to_string();
            out.push((name, std::fs::read(&path).unwrap()));
        }
    }
}
