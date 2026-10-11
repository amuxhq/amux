//! `testnet ctl` from a shell's point of view: an empty net started in a
//! folder grows a host and a link, runs a fake agent, shows the terminal
//! client as a picture, keeps a capture of it all, and shuts down leaving
//! the evidence behind.

#![cfg(unix)]

use std::path::Path;
use std::process::{Command, Output};

fn ctl(folder: &Path, args: &[&str]) -> Output {
    let output = Command::new(env!("CARGO_BIN_EXE_testnet"))
        .arg("ctl")
        .args(args)
        .current_dir(folder)
        // A run's scope or ledger in the caller's environment is not this
        // test's.
        .env_remove("AMUX_QA_SCOPE")
        .env_remove("AMUX_QA_RUN")
        .env_remove("TMUX")
        .output()
        .expect("testnet runs");
    assert!(
        output.status.success(),
        "ctl {args:?} failed:\n{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn said(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Shuts the net down however the test ends, so a failed assertion leaves
/// no daemons or tmux server behind.
struct Down<'a>(&'a Path);

impl Drop for Down<'_> {
    fn drop(&mut self) {
        let _ = Command::new(env!("CARGO_BIN_EXE_testnet"))
            .args(["ctl", "down"])
            .current_dir(self.0)
            .output();
    }
}

#[test]
fn a_session_grows_a_net_drives_it_and_keeps_the_evidence() {
    let folder = tempfile::tempdir().expect("a folder");
    let folder = folder.path();
    std::fs::write(
        folder.join("hello.json"),
        r#"{"steps": [{"text": {"chunks": ["hello from the fake"]}}, "turn_end"]}"#,
    )
    .expect("a script");

    ctl(folder, &["up"]);
    let down = Down(folder);
    ctl(folder, &["host", "add", "desk"]);
    ctl(folder, &["host", "add", "laptop"]);
    assert!(said(&ctl(folder, &["link", "desk", "laptop"])).contains("up"));
    let spawned = said(&ctl(
        folder,
        &[
            "spawn",
            "--host",
            "desk",
            "--script",
            "hello.json",
            "--prompt",
            "say hello",
            "--wait",
            "worker",
        ],
    ));
    assert!(spawned.contains("at rest"), "{spawned}");
    let chat = said(&ctl(folder, &["chat", "desk", "worker"]));
    assert!(chat.contains("hello from the fake"), "{chat}");

    // The laptop's terminal client shows the desk's agent once the link
    // has carried it over.
    ctl(folder, &["tui", "open", "laptop"]);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        let screen = said(&ctl(folder, &["tui", "screen", "laptop"]));
        if screen.contains("worker") {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the laptop's client never listed worker:\n{screen}"
        );
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    let shot = said(&ctl(folder, &["tui", "shot", "laptop"]));
    let png = shot.lines().next().expect("the shot's path");
    assert!(png.ends_with(".png") && Path::new(png).is_file(), "{shot}");
    let frame = std::fs::read_to_string(png.replace(".png", ".frame.json")).expect("its frame");
    assert!(
        frame.contains("worker"),
        "the frame holds the screen's text"
    );

    // A flapping link is whole again once the flap stops: what the desk
    // does next reaches the laptop.
    ctl(
        folder,
        &["flap", "desk", "laptop", "--down", "300ms", "--up", "300ms"],
    );
    std::thread::sleep(std::time::Duration::from_secs(1));
    assert!(said(&ctl(folder, &["flap", "stop"])).contains("restored"));
    ctl(
        folder,
        &[
            "spawn",
            "--host",
            "desk",
            "--script",
            "hello.json",
            "--prompt",
            "again",
            "--wait",
            "second",
        ],
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while !said(&ctl(folder, &["fleet", "laptop"])).contains("second") {
        assert!(
            std::time::Instant::now() < deadline,
            "the laptop never heard of the agent made after the flap"
        );
        std::thread::sleep(std::time::Duration::from_millis(250));
    }

    let captured = said(&ctl(
        folder,
        &["capture", "lost-row", "--note", "a smoke test's capture"],
    ));
    assert!(captured.contains("captured into"), "{captured}");
    let finding = folder.join("findings/lost-row");
    for file in [
        "finding.md",
        "topology.json",
        "session.jsonl",
        "logs/testnet.log",
    ] {
        assert!(finding.join(file).is_file(), "the capture has {file}");
    }
    let topology = std::fs::read_to_string(finding.join("topology.json")).expect("topology");
    assert!(
        topology.contains("\"laptop\"") && topology.contains("\"desk\""),
        "the capture's topology has the hosts added at run time: {topology}"
    );
    for host in ["desk", "laptop"] {
        let dumps: Vec<_> = std::fs::read_dir(finding.join("dumps").join(host))
            .expect("a dump folder per host")
            .flatten()
            .collect();
        assert_eq!(dumps.len(), 1, "one dump of {host}");
        assert!(dumps[0].path().join("manifest.json").is_file());
    }
    let screens: Vec<String> = std::fs::read_dir(finding.join("screens"))
        .expect("screens")
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        screens.iter().filter(|name| name.ends_with(".png")).count() >= 2,
        "the earlier shot and a fresh one: {screens:?}"
    );

    drop(down);
    assert!(
        !folder.join(".testnet/net").exists(),
        "the net's own files go with it"
    );
    let log = std::fs::read_to_string(folder.join("session.jsonl")).expect("the session log");
    assert!(
        log.lines().count() >= 9,
        "every command is in the session log"
    );
    assert!(finding.join("finding.md").is_file(), "the evidence stays");
}
