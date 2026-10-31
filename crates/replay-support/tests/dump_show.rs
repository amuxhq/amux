//! The dump printer against a healthy bundle: the row, the journal in the
//! emission goldens' format, the store slice, the facts as written, and
//! every other file a bundle can hold.

use std::process::Command;

use daemon_core::{Daemon, Input, Recorded, StartOptions};
use replay_support::bundle::Bundle;
use replay_support::show::{self, ShowError};
use store::{AgentKey, AgentRow, Sqlite};

use crate::support;

#[test]
fn the_journal_prints_as_the_goldens_do_and_reads_beside_the_facts_replay() {
    let dumped = support::healthy();
    let bundle = Bundle::open(dumped.path()).expect("the bundle opens");
    let agent = &bundle.agents[0];
    let kind = agent.row.kind();
    let replayed = agent.replay_facts().expect("the facts replay");
    // The ring starts after the interpreter's first frame, so the replay is
    // the journal from frame 1 on.
    let journal = show::render_steps(kind, 1, &agent.journal[1..]).unwrap();
    let replay = show::render_steps(kind, 1, &replayed).unwrap();
    assert!(journal.starts_with("## 1 step\nitem "), "{journal}");
    assert_eq!(
        support::placeholders_aside(&replay),
        support::placeholders_aside(&journal)
    );

    // The printer compares them as written, so it shows where the
    // placeholders first differ.
    let text = show::show(dumped.path(), None).expect("the bundle prints");
    let whole = show::render_steps(kind, 0, &agent.journal).unwrap();
    assert!(
        text.contains(&format!(
            "-- journal ({} frames) --\n{whole}",
            agent.journal.len()
        )),
        "{text}"
    );
    assert!(
        text.contains(&format!("-- replay ({} steps) --\n", replayed.len())),
        "{text}"
    );
    let difference = text
        .lines()
        .find(|line| line.starts_with("first difference in frame 1: journal "))
        .expect("the first difference is named");
    assert!(
        difference.contains("<REDACTED_EMAIL>") && difference.contains("<REDACTED_IDENTIFIER>"),
        "{difference}"
    );
    assert!(
        text.contains(&replay),
        "the replay prints whole when it differs"
    );
}

#[test]
fn every_section_of_a_bundle_prints() {
    let dumped = support::healthy();
    let agent = dumped.agent();
    let manifest = dumped.manifest();
    let bundle = Bundle::open(dumped.path()).unwrap();
    let rows = bundle.agents[0].store.as_ref().unwrap().items.len();
    let facts = std::fs::read_to_string(
        dumped
            .path()
            .join(format!("agents/{agent}/part/facts/0000000000")),
    )
    .unwrap();
    let text = show::show(dumped.path(), None).expect("the bundle prints");
    for expected in [
        &format!(
            "== dump {} ==\nreason \"a healthy net\"\n",
            manifest["dump_id"].as_str().unwrap()
        ),
        "errors none\n",
        &format!("== agent {agent} (worker, CLAUDE_SDK) ==\n"),
        &format!("-- row --\nagent_id {agent}\n"),
        "lifecycle LIVE\n",
        &format!("-- store ({rows} rows) --\norder=1 revision="),
        " message final ",
        " text=\"second answer more\"\n",
        "\nsnapshot catalogue ",
        "\nsnapshot IDLE ",
        &format!(
            "-- facts 0000000000 ({} lines) --\ncheckpoint {{\"active_tasks\":{{}}",
            facts.lines().count()
        ),
        "-- part spec.1 (wire.AgentSpec) --\n",
        "== other files (0) ==\n",
    ] {
        assert!(text.contains(expected), "missing {expected:?} in\n{text}");
    }
    for line in facts.lines() {
        assert!(text.contains(&format!("\n{line}\n")), "{line}");
    }
}

/// What a dump of an in-process daemon lacks: a daemon log, a client's
/// part, an agent part's errors, and a facts line the redactor could not
/// keep.
#[test]
fn the_rest_of_a_bundle_prints_and_an_unreadable_fact_hides_nothing() {
    let dumped = support::healthy();
    let dir = dumped.path();
    let agent = dir.join("agents").join(dumped.agent());
    std::fs::write(dir.join("daemon.log"), "a line\n".repeat(4000)).unwrap();
    std::fs::create_dir_all(dir.join("client/fleet")).unwrap();
    std::fs::write(dir.join("client/fleet/state.txt"), "fleet open\n").unwrap();
    std::fs::write(agent.join("part/dump-errors"), "checkpoint 3: gone\n").unwrap();
    // The first git event becomes one no build reads; the lines around it
    // and the later git events print as they were.
    let facts = agent.join("part/facts/0000000000");
    let written = std::fs::read_to_string(&facts).unwrap();
    let mut lines: Vec<&str> = written.lines().collect();
    let git = "{\"event\":\"git\"}";
    let first = lines.iter().position(|line| *line == git).unwrap();
    let later = lines.iter().rposition(|line| *line == git).unwrap();
    assert!(0 < first && first < later, "two git events, after a line");
    lines[first] = "{\"event\":\"unreadable\"}";
    std::fs::write(&facts, lines.join("\n") + "\n").unwrap();

    let text = show::show(dir, Some("worker")).expect("the bundle prints");
    for expected in [
        "-- part dump-errors (19 bytes) --\ncheckpoint 3: gone\n",
        "== other files (2) ==\n",
        "-- client/fleet/state.txt (11 bytes) --\nfleet open\n",
        "-- daemon.log (28000 bytes) --\ntext, not printed: ",
        &(lines[first - 1..=first + 1].join("\n") + "\n"),
        &(lines[later - 1..=later].join("\n") + "\n"),
    ] {
        assert!(text.contains(expected), "missing {expected:?} in\n{text}");
    }
}

/// The daemon core's part: a checkpoint, the lines its driver recorded
/// since and the store as of the checkpoint, each printed, and each counted
/// as shown.
#[test]
fn the_daemon_cores_part_prints_its_checkpoint_inputs_and_store() {
    let dumped = support::healthy();
    let dir = dumped.path();
    // The daemon's own part gives way to one written here.
    let core = dir.join("core");
    std::fs::remove_dir_all(&core).unwrap();
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
    std::fs::write(core.join("checkpoint"), daemon.checkpoint()).unwrap();
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

    let bundle = Bundle::open(dir).expect("the bundle opens");
    let read = bundle
        .core
        .as_ref()
        .expect("the bundle has the core's part");
    assert_eq!(read.inputs, lines);

    let text = show::show(dir, None).expect("the bundle prints");
    for expected in [
        "\n== daemon core ==\n",
        "\n-- checkpoint (",
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
    let dumped = support::healthy();
    let agent = dumped.agent();
    let prefix = &agent[..4];
    let run = |args: &[&std::ffi::OsStr]| {
        Command::new(env!("CARGO_BIN_EXE_dump-show"))
            .args(args)
            .output()
            .unwrap()
    };
    let printed = run(&[
        dumped.path().as_os_str(),
        "--agent".as_ref(),
        prefix.as_ref(),
    ]);
    assert!(printed.status.success());
    let text = String::from_utf8(printed.stdout).unwrap();
    assert_eq!(text, show::show(dumped.path(), Some(prefix)).unwrap());
    assert!(text.contains(&format!("== agent {agent} (worker, CLAUDE_SDK) ==")));

    let unknown = run(&[
        dumped.path().as_os_str(),
        "--agent".as_ref(),
        "nobody".as_ref(),
    ]);
    assert!(!unknown.status.success());
    assert!(String::from_utf8_lossy(&unknown.stderr).contains("no agent in the bundle matches"));

    let elsewhere = dumped.path().join("agents");
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

/// A client's session prints as the state its trace began from and every
/// event its driver saw, so nothing in the bundle is left as bytes.
#[test]
fn a_clients_session_prints_its_checkpoint_and_every_event() {
    let dumped = support::client_violation();
    let printed = show::show(dumped.path(), None).expect("the bundle prints");
    let section = printed
        .split(&format!("\n== client session {} ==\n", dumped.agent()))
        .nth(1)
        .expect("a section for the session")
        .split("\n== ")
        .next()
        .unwrap();
    assert!(section.starts_with("-- checkpoint --\n"), "{section}");
    let traced = &dumped.manifest()["client"]["sessions"][0]["traced"];
    assert!(
        section.contains(&format!("-- events ({traced}) --")),
        "{section}"
    );
    assert!(
        section.contains("driver Subscribed { tail: 16 }"),
        "{section}"
    );
    assert!(section.contains("order 1\n"), "{section}");
    assert!(section.contains(" order 3\n"), "{section}");
    assert!(section.contains(" CaughtUp rev "), "{section}");
    assert!(printed.contains("== other files (0) =="), "{printed}");
}
