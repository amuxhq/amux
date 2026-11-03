//! The dump printer against a healthy bundle: the agent's record, the
//! daemon core's record, a chat's record and every other file a bundle can
//! hold.

use std::process::Command;

use daemon_core::{Daemon, Input, Recorded, StartOptions};
use replay_support::bundle::Bundle;
use replay_support::show::{self, ShowError};
use store::{AgentKey, AgentRow, Sqlite};

use crate::support;

#[test]
fn every_section_of_a_bundle_prints() {
    let dumped = support::healthy();
    let agent = dumped.agent();
    let manifest = dumped.manifest();
    let path = dumped.path().join(format!("agents/{agent}/record"));
    let record = std::fs::read_to_string(&path).unwrap();
    // Every event's skeleton makes a record longer than the printer shows
    // whole, so it names the file instead.
    let record = format!(
        "-- record ({} bytes) --\ntext, not printed: {}\n",
        record.len(),
        path.display()
    );
    let text = show::show(dumped.path(), None).expect("the bundle prints");
    for expected in [
        &format!(
            "== dump {} ==\nreason \"a healthy net\"\ntrigger person\n",
            manifest["dump_id"].as_str().unwrap()
        ),
        "errors none\n",
        &format!("== agent {agent} (CLAUDE_SDK) ==\n"),
        &record,
        "\n== daemon core ==\n",
        &format!("-- journal of {agent} ("),
        &format!("-- agents/{agent}/agent.log ("),
    ] {
        assert!(text.contains(expected), "missing {expected:?} in\n{text}");
    }
}

/// What a dump of an in-process daemon lacks: a daemon log and a client's
/// part, each printed as the other files are.
#[test]
fn the_rest_of_a_bundle_prints() {
    let dumped = support::healthy();
    let dir = dumped.path();
    std::fs::write(dir.join("daemon.log"), "a line\n".repeat(4000)).unwrap();
    std::fs::create_dir_all(dir.join("client/fleet")).unwrap();
    std::fs::write(dir.join("client/fleet/state.txt"), "fleet open\n").unwrap();

    let text = show::show(dir, None).expect("the bundle prints");
    for expected in [
        "== other files (3) ==\n",
        "-- client/fleet/state.txt (11 bytes) --\nfleet open\n",
        "-- daemon.log (28000 bytes) --\ntext, not printed: ",
    ] {
        assert!(text.contains(expected), "missing {expected:?} in\n{text}");
    }
}

/// The daemon core's record: a checkpoint, the lines its driver recorded
/// since and the store as of the checkpoint, each printed, and each counted
/// as shown.
#[test]
fn the_daemon_cores_part_prints_its_checkpoint_inputs_and_store() {
    let dumped = support::healthy();
    let dir = dumped.path();
    // The daemon's own record gives way to one written here.
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
        .expect("the bundle has the core's record");
    assert_eq!(read.inputs, lines);

    let text = show::show(dir, None).expect("the bundle prints");
    for expected in [
        "\n== daemon core ==\n",
        "\n-- checkpoint (",
        "-- inputs (1 inputs, 1 journal reads) --\n",
        &written,
        "-- store as of the checkpoint --\nagent 22222222-2222-2222-2222-222222222222 \
         \"worker\" host 11111111-1111-1111-1111-111111111111 lifecycle 1 ingest_cursor=57 ",
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
    assert!(text.contains(&format!("== agent {agent} (CLAUDE_SDK) ==")));

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
}
