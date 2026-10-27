//! System journeys: whole stories told with the built binaries as a person
//! and the machine run them, each printing its transcript. `just journey
//! system <id>` runs one; the ids are declared in journeys/manifest.json.

#![cfg(unix)]

mod support;

use std::collections::BTreeMap;
use std::time::Duration;

use support::desk::{Desk, GRACE_SECS, kill, say, spoken};
use support::{
    Channel, amux_binary, chat, client, count, created_id, gated_turn, line_of, says, texts,
    turns_ended, until, until_within,
};

/// Three agents, one of each kind, are mid-turn when amux is killed. They
/// finish their turns with nobody reading, outlive their grace and exit.
/// amux comes back under its supervisor, a newer build is installed and
/// activated, and every finished turn is in the store; resuming the exited
/// agents loses nothing.
#[tokio::test(flavor = "multi_thread")]
async fn survive_daemon() {
    let running = node::version();
    let previous = version_stamp::previous(running).expect("a release version to go below");
    let channel = Channel::serve().await;
    let gate_dir = tempfile::tempdir().unwrap();
    let gate = gate_dir.path().join("gate");
    let desk = Desk::new(
        true,
        GRACE_SECS,
        &previous,
        channel.url(),
        gated_turn(&gate),
    );
    let work = desk.work.to_string_lossy().into_owned();
    say(format!(
        "== survive-daemon: amux {previous} installed; the build under test, {running}, is the newer release"
    ));

    desk.run(&["server", "start"]).await;
    let mut agents: BTreeMap<&str, Vec<u8>> = BTreeMap::new();
    for (name, agent, interface) in [
        ("terminal", "claude", "native"),
        ("headless", "claude", "amux"),
        ("codex", "codex", "amux"),
    ] {
        let created = desk
            .run(&[
                "new",
                agent,
                "--in",
                interface,
                "--name",
                name,
                "--cwd",
                &work,
                "--prompt",
                "first task",
            ])
            .await;
        agents.insert(name, created_id(&created));
    }
    let chats = client(&desk.socket).await;
    for (name, id) in &agents {
        until(&format!("{name} is mid-turn"), || {
            let mut chats = chats.clone();
            async move { says(&chat(&mut chats, id).await, "started", 1) }
        })
        .await
        .unwrap();
    }
    let listing = desk.run(&["ls"]).await;
    for name in agents.keys() {
        assert!(line_of(&listing, name).contains("working"), "{listing}");
    }

    // amux dies: the supervisor and its daemon, with no chance to tell
    // anyone. The agents see their control sockets close and carry on.
    let supervisor = desk.supervisor_pid().expect("a supervisor runs");
    let daemon = desk.daemon_pid();
    kill(supervisor);
    kill(daemon);
    say(format!(
        "-- killed amux supervise (pid {supervisor}) and amux daemon (pid {daemon}) with SIGKILL, every agent mid-turn"
    ));
    until("the daemon is gone", || async {
        (!desk.daemon_running())
            .then_some(())
            .ok_or("still running")
    })
    .await
    .unwrap();

    std::fs::write(&gate, b"").unwrap();
    for (name, id) in &agents {
        let dir = desk.agent_dir(id);
        until(
            &format!("{name} finishes its turn with no daemon"),
            || async { turns_ended(&dir, 1) },
        )
        .await
        .unwrap();
    }
    say("-- every agent finished its turn into its journal with no daemon running");
    for (name, id) in &agents {
        let dir = desk.agent_dir(id);
        until_within(
            &format!("{name} drains and exits once its grace runs out"),
            Duration::from_secs(GRACE_SECS) + support::PATIENCE,
            || async {
                (!agent_dir::locked(&dir))
                    .then_some(())
                    .ok_or("the directory is still locked")
            },
        )
        .await
        .unwrap();
    }
    say(format!(
        "-- after {GRACE_SECS} s of grace every agent drained and exited"
    ));

    // Any amux command finds amux stopped and starts its supervisor.
    let listing = desk.run(&["ls"]).await;
    for name in agents.keys() {
        assert!(
            line_of(&listing, name).contains("exited: while the daemon was away"),
            "{listing}"
        );
    }

    // The newer build is installed under the supervisor and activated.
    channel.publish(running, std::fs::read(amux_binary()).unwrap());
    say(format!("-- the stable channel now names {running}"));
    let updated = desk.run(&["update"]).await;
    assert!(
        updated.contains(&format!("amux {running} is running.")),
        "{updated}"
    );
    let installed = desk.run(&["--version"]).await;
    assert_eq!(installed.trim(), format!("amux {running}"));
    assert!(
        !desk.bin.join("amux.prev").exists(),
        "activated: prev is gone"
    );

    // Every finished turn is in the store.
    let mut chats = client(&desk.socket).await;
    let mut before = BTreeMap::new();
    for (name, id) in &agents {
        let found = chat(&mut chats, id).await;
        for said in ["first task", "started", "finished"] {
            assert_eq!(
                count(&found, said),
                1,
                "{name}'s first turn is whole in the store: {:?}",
                texts(&found)
            );
        }
        say(format!("{name} in the store: {:?}", spoken(&found)));
        before.insert(*name, found);
    }

    // Resuming the exited agents loses nothing.
    for name in agents.keys() {
        desk.run(&["resume", name, "second", "task"]).await;
    }
    for (name, id) in &agents {
        until(&format!("{name} finishes its second turn"), || {
            let mut chats = chats.clone();
            async move { says(&chat(&mut chats, id).await, "finished", 2) }
        })
        .await
        .unwrap();
        let after = chat(&mut chats, id).await;
        for item in &before[name] {
            assert!(after.contains(item), "{name}: {item:?} survives the resume");
        }
        assert!(texts(&after).contains(&"second task"));
        say(format!("{name} in the store: {:?}", spoken(&after)));
    }
    desk.run(&["ls"]).await;
    say("== survive-daemon: every finished turn is in the store and every resume kept its history");
    if keep() {
        let root = desk.keep();
        say(format!(
            "-- kept running for other clients: AMUX_CONFIG={} (install under {})",
            root.join("config.toml").display(),
            root.display()
        ));
        return;
    }
    for name in agents.keys() {
        desk.run(&["stop", name]).await;
    }
    desk.run(&["server", "stop"]).await;
}

/// Whether the journey leaves its install running afterwards, so a phone
/// or a terminal can open the same chats: `just journey system <id> --
/// --keep`.
fn keep() -> bool {
    std::env::var_os(KEEP_ENV).is_some()
}

const KEEP_ENV: &str = "AMUX_JOURNEY_KEEP";
