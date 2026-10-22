//! The dump printer against the committed bundle: the row, the journal in
//! the emission goldens' format, the store slice, the facts as written, and
//! every other file a bundle can hold.

use std::path::{Path, PathBuf};
use std::process::Command;

use daemon_core::{Daemon, Input, Recorded, StartOptions};
use replay_support::bundle::Bundle;
use replay_support::show::{self, ShowError};
use store::{AgentKey, AgentRow, Sqlite};

const AGENT: &str = "d6832ca2-2f0a-4307-9cf6-5a0e13e937cc";

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bundle")
}

#[test]
fn the_journal_prints_as_the_goldens_do_and_reads_beside_the_facts_replay() {
    let bundle = Bundle::open(&fixture()).expect("the fixture bundle opens");
    let agent = &bundle.agents[0];
    let kind = agent.row.kind();
    let replayed = agent.replay_facts().expect("the facts replay");
    // The ring starts after the interpreter's first frame, so the replay is
    // the journal from frame 1 on.
    let journal = show::render_steps(kind, 1, &agent.journal[1..]).unwrap();
    let replay = show::render_steps(kind, 1, &replayed).unwrap();
    assert!(journal.starts_with("## 1 step\nitem "), "{journal}");
    // The fixture's journal predates the catalogue's current encoding, so
    // the hash its snapshots name is not the one a replay computes today;
    // every other line matches.
    let comparable = |text: &str| {
        text.lines()
            .filter(|line| !line.starts_with("snapshot catalogue "))
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert_eq!(comparable(&replay), comparable(&journal));

    let text = show::show(&fixture(), None).expect("the bundle prints");
    let whole = show::render_steps(kind, 0, &agent.journal).unwrap();
    assert!(
        text.contains(&format!("-- journal (16 frames) --\n{whole}")),
        "{text}"
    );
    assert!(text.contains("-- replay (15 steps) --\n"), "{text}");
    assert!(
        text.contains(
            "first difference in frame 1: journal \"snapshot catalogue 4030c5e1\", \
             replay \"snapshot catalogue c5a5aa38\"\n"
        ),
        "{text}"
    );
    assert!(
        text.contains(&replay),
        "the replay prints whole when it differs"
    );
}

#[test]
fn every_section_of_the_fixture_prints() {
    let text = show::show(&fixture(), None).expect("the bundle prints");
    for expected in [
        "== dump 30dd8cad-608a-433a-89ff-dbbd17ef8038 ==\nreason \"fixture\"\n",
        "errors none\n",
        &format!("== agent {AGENT} (worker, CLAUDE_SDK) ==\n"),
        &format!("-- row --\nagent_id {AGENT}\n"),
        "lifecycle LIVE\n",
        "-- store (8 rows) --\norder=1 revision=6 item boundary:1 boundary final ",
        "order=7 revision=21 item msg_fake331e0c860000000000000004:0 message final \
         at=1791271653099 text=\"second answer more\"\n",
        "snapshot revision=23\nsnapshot catalogue 4030c5e1\nsnapshot IDLE ",
        "-- facts 0000000000 (45 lines) --\ncheckpoint {\"active_tasks\":{}",
        "-- part spec.1 (wire.AgentSpec) --\n",
        "== other files (0) ==\n",
    ] {
        assert!(text.contains(expected), "missing {expected:?} in\n{text}");
    }
    let facts =
        std::fs::read_to_string(fixture().join(format!("agents/{AGENT}/part/facts/0000000000")))
            .unwrap();
    for line in facts.lines() {
        assert!(text.contains(&format!("\n{line}\n")), "{line}");
    }
}

/// What the committed bundle lacks: a daemon log, a client's part, an
/// agent part's errors, another host's row and slice, and a facts line the
/// redactor could not keep.
#[test]
fn the_rest_of_a_bundle_prints_and_an_unreadable_fact_hides_nothing() {
    let dir = tempfile::tempdir().unwrap();
    copy(&fixture(), dir.path());
    let agent = dir.path().join("agents").join(AGENT);
    std::fs::write(dir.path().join("daemon.log"), "a line\n".repeat(4000)).unwrap();
    std::fs::create_dir_all(dir.path().join("client/fleet")).unwrap();
    std::fs::write(dir.path().join("client/fleet/state.txt"), "fleet open\n").unwrap();
    std::fs::write(agent.join("part/dump-errors"), "checkpoint 3: gone\n").unwrap();
    std::fs::create_dir_all(agent.join("host")).unwrap();
    std::fs::copy(agent.join("row.pb"), agent.join("host/row.pb")).unwrap();
    std::fs::copy(agent.join("store.pb"), agent.join("host/store.pb")).unwrap();
    let facts = agent.join("part/facts/0000000000");
    let mut lines = std::fs::read_to_string(&facts).unwrap();
    lines = lines.replacen("{\"event\":\"git\"}", "{\"event\":\"unreadable\"}", 1);
    std::fs::write(&facts, lines).unwrap();

    let text = show::show(dir.path(), Some("worker")).expect("the bundle prints");
    for expected in [
        "-- part dump-errors (19 bytes) --\ncheckpoint 3: gone\n",
        &format!("-- host row --\nagent_id {AGENT}\n"),
        "-- host store (8 rows) --\norder=1 revision=6 ",
        "== other files (2) ==\n",
        "-- client/fleet/state.txt (11 bytes) --\nfleet open\n",
        "-- daemon.log (28000 bytes) --\ntext, not printed: ",
        "{\"event\":\"tick\",\"at_ms\":1791271650055}\n{\"event\":\"unreadable\"}\n\
         {\"event\":\"tick\",\"at_ms\":1791271650067}\n",
        "{\"event\":\"tick\",\"at_ms\":1791271653120}\n{\"event\":\"git\"}\n",
    ] {
        assert!(text.contains(expected), "missing {expected:?} in\n{text}");
    }
}

/// The daemon core's part: a checkpoint, the lines its driver recorded
/// since and the store as of the checkpoint, each printed, and each counted
/// as shown.
#[test]
fn the_daemon_cores_part_prints_its_checkpoint_inputs_and_store() {
    let dir = tempfile::tempdir().unwrap();
    copy(&fixture(), dir.path());
    let core = dir.path().join("core");
    std::fs::create_dir_all(&core).unwrap();

    let own = [0x11; 16].to_vec();
    let agent = AgentKey::new(own.clone(), [0x22; 16].to_vec());
    let path = core.join("store.sqlite");
    let mut store = Sqlite::open(&path, own).unwrap();
    let mut row = AgentRow::new(agent.clone(), wire::Kind::ClaudeSdk, "/src/amux");
    row.name = "worker".into();
    store.put_agent(&row).unwrap();
    let step = wire::Step {
        items: vec![wire::Item {
            key: "k".into(),
            kind: wire::Kind::ClaudeSdk as i32,
            ..Default::default()
        }],
        ..Default::default()
    };
    let clock = store::CommitClock {
        now_ms: 0,
        notify_delay_ms: 0,
    };
    store.commit(&agent, &[(57, step)], clock).unwrap();
    let daemon = Daemon::new(store, StartOptions::default());
    std::fs::write(core.join("checkpoint"), daemon.checkpoint().to_bytes()).unwrap();
    drop(daemon);

    let id = daemon_core::AgentId([0x22; 16]);
    let lines = [
        Recorded::Input(Box::new(Input::Nudge { agent: id })),
        Recorded::Read {
            agent: id,
            seen: 57,
            failed: None,
        },
    ];
    let mut written = String::new();
    for line in &lines {
        written.push_str(&serde_json::to_string(line).unwrap());
        written.push('\n');
    }
    std::fs::write(core.join("inputs"), &written).unwrap();

    let bundle = Bundle::open(dir.path()).expect("the bundle opens");
    let read = bundle
        .core
        .as_ref()
        .expect("the bundle has the core's part");
    assert_eq!(read.inputs, lines);

    let text = show::show(dir.path(), None).expect("the bundle prints");
    for expected in [
        "\n== daemon core ==\n-- checkpoint (schema 1, ",
        "-- inputs (1 inputs, 1 journal reads) --\n",
        &written,
        "-- store as of the checkpoint --\nagent 22222222-2222-2222-2222-222222222222 \
         \"worker\" host 11111111-1111-1111-1111-111111111111 lifecycle 1 ingest_cursor=57 ",
        "== other files (0) ==\n",
    ] {
        assert!(text.contains(expected), "missing {expected:?} in\n{text}");
    }
}

#[test]
fn the_binary_prints_one_agent_and_refuses_what_is_not_a_bundle() {
    let run = |args: &[&std::ffi::OsStr]| {
        Command::new(env!("CARGO_BIN_EXE_dump-show"))
            .args(args)
            .output()
            .unwrap()
    };
    let printed = run(&[fixture().as_os_str(), "--agent".as_ref(), "d683".as_ref()]);
    assert!(printed.status.success());
    let text = String::from_utf8(printed.stdout).unwrap();
    assert_eq!(text, show::show(&fixture(), Some("d683")).unwrap());
    assert!(text.contains(&format!("== agent {AGENT} (worker, CLAUDE_SDK) ==")));

    let unknown = run(&[fixture().as_os_str(), "--agent".as_ref(), "nobody".as_ref()]);
    assert!(!unknown.status.success());
    assert!(String::from_utf8_lossy(&unknown.stderr).contains("no agent in the bundle matches"));

    let elsewhere = fixture().join("agents");
    let refused = run(&[elsewhere.as_os_str()]);
    assert!(!refused.status.success());
    assert_eq!(
        String::from_utf8_lossy(&refused.stderr),
        format!(
            "dump-show: {} is not a dump bundle: it has no manifest.json\n",
            elsewhere.display()
        )
    );
    assert!(matches!(
        show::show(&elsewhere, None),
        Err(ShowError::NotABundle(_))
    ));
}

fn copy(from: &Path, to: &Path) {
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            std::fs::create_dir_all(&target).unwrap();
            copy(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// A client's session prints as the state its trace began from and every
/// event its driver saw, so nothing in the bundle is left as bytes.
#[test]
fn a_clients_session_prints_its_checkpoint_and_every_event() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/client_violation");
    let printed = show::show(&dir, None).expect("the bundle prints");
    let section = printed
        .split("\n== client session 01aaa7bf-2866-4f4a-af4d-08abe99db157 ==\n")
        .nth(1)
        .expect("a section for the session")
        .split("\n== ")
        .next()
        .unwrap();
    assert!(section.starts_with("-- checkpoint --\n"), "{section}");
    assert!(section.contains("-- events (8) --"), "{section}");
    assert!(
        section.contains("driver Subscribed { tail: 16 }"),
        "{section}"
    );
    assert!(section.contains("order 1\n"), "{section}");
    assert!(section.contains(" order 3\n"), "{section}");
    assert!(section.contains("CaughtUp rev 13"), "{section}");
    assert!(printed.contains("== other files (0) =="), "{printed}");
}
