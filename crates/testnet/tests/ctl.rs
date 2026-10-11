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
    let status = said(&ctl(folder, &["status"]));
    assert!(
        status.contains("agents outlive a stopped daemon by 20s"),
        "the status names the net's agent grace: {status}"
    );
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
    // has carried it over. Its tmux reads the net's own configuration,
    // whatever the operator's says.
    ctl(folder, &["tui", "open", "laptop"]);
    let conf = std::fs::read_to_string(folder.join(".testnet/tmux.conf"))
        .expect("the net's tmux configuration");
    assert!(conf.contains("extended-keys on"), "{conf}");
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

#[test]
fn a_finding_may_name_the_earlier_one_it_duplicates() {
    let run = tempfile::tempdir().expect("a run folder");
    std::fs::write(run.path().join("ledger.jsonl"), "").expect("a ledger");
    let ledger = |tester: &str, args: &[&str]| {
        let folder = run.path().join(tester);
        std::fs::create_dir_all(folder.join("findings/lost-row")).expect("a finding folder");
        std::fs::write(
            folder.join("findings/lost-row/finding.md"),
            "# A row goes missing\n\n- **Severity:** wrong state\n",
        )
        .expect("a finding");
        Command::new(env!("CARGO_BIN_EXE_testnet"))
            .args(["ctl", "ledger"])
            .args(args)
            .current_dir(&folder)
            .env("AMUX_QA_RUN", run.path())
            .env("AMUX_QA_TESTER", tester)
            .env_remove("AMUX_QA_SCOPE")
            .output()
            .expect("testnet runs")
    };

    assert!(ledger("t1", &["finding", "lost-row"]).status.success());
    let unknown = ledger(
        "t2",
        &["finding", "lost-row", "--duplicate-of", "t9/lost-row"],
    );
    assert!(
        !unknown.status.success(),
        "a duplicate of nothing is refused"
    );
    assert!(
        String::from_utf8_lossy(&unknown.stderr).contains("t1/lost-row"),
        "the refusal names the findings there are"
    );
    assert!(
        ledger(
            "t2",
            &["finding", "lost-row", "--duplicate-of", "t1/lost-row"]
        )
        .status
        .success()
    );
    // A finding whose severity was never filled in has none, not the
    // template comment that lists every severity.
    let unfilled = run.path().join("t3/findings/vague");
    std::fs::create_dir_all(&unfilled).expect("a finding folder");
    std::fs::write(
        unfilled.join("finding.md"),
        "# Vague\n\n- **Severity:** <!-- one of: data loss, broken flow, wrong state, visual, wording -->\n",
    )
    .expect("a finding");
    let recorded = Command::new(env!("CARGO_BIN_EXE_testnet"))
        .args(["ctl", "--json", "ledger", "finding", "vague"])
        .current_dir(run.path().join("t3"))
        .env("AMUX_QA_RUN", run.path())
        .env("AMUX_QA_TESTER", "t3")
        .output()
        .expect("testnet runs");
    let entry: serde_json::Value =
        serde_json::from_slice(&recorded.stdout).expect("the entry as JSON");
    assert_eq!(entry["severity"], "", "{entry}");
    // One written past the template, with no title or severity line, is
    // refused rather than listed blank.
    let shapeless = run.path().join("t4/findings/notes");
    std::fs::create_dir_all(&shapeless).expect("a finding folder");
    std::fs::write(
        shapeless.join("finding.md"),
        "The row went missing.
",
    )
    .expect("a finding");
    let refused = Command::new(env!("CARGO_BIN_EXE_testnet"))
        .args(["ctl", "ledger", "finding", "notes"])
        .current_dir(run.path().join("t4"))
        .env("AMUX_QA_RUN", run.path())
        .env("AMUX_QA_TESTER", "t4")
        .output()
        .expect("testnet runs");
    assert!(!refused.status.success(), "a shapeless finding is refused");
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("`# title` line"),
        "{}",
        String::from_utf8_lossy(&refused.stderr)
    );
    let read = said(&ledger("t2", &["read"]));
    assert!(
        read.contains("t1/lost-row — A row goes missing\n"),
        "{read}"
    );
    assert!(
        read.contains("t2/lost-row — A row goes missing (duplicates t1/lost-row)"),
        "{read}"
    );
}

#[test]
fn down_never_kills_a_process_that_took_a_crashed_nets_pid() {
    let folder = tempfile::tempdir().expect("a folder");
    let folder = folder.path();
    ctl(folder, &["up"]);
    let down = Down(folder);
    let record_path = folder.join(".testnet/net.json");
    let mut record: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&record_path).expect("net.json"))
            .expect("its record");
    let serve = record["pid"].as_u64().expect("serve's pid").to_string();
    // Serve crashes, and another process now has the pid net.json names.
    assert!(
        Command::new("kill")
            .args(["-KILL", &serve])
            .status()
            .expect("kill runs")
            .success()
    );
    let mut stranger = Command::new("sleep").arg("120").spawn().expect("sleep");
    record["pid"] = stranger.id().into();
    std::fs::write(&record_path, record.to_string()).expect("net.json rewritten");

    let started = std::time::Instant::now();
    ctl(folder, &["down"]);
    assert!(
        stranger.try_wait().expect("its status").is_none(),
        "the process that took the pid is untouched"
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(30),
        "down does not wait on a process that is not the net's"
    );
    let _ = stranger.kill();
    drop(down);
}

#[test]
fn a_qa_tester_is_refused_fake_agents() {
    let run = tempfile::tempdir().expect("a run folder");
    for args in [
        &["host", "add", "desk", "--provider", "fake"][..],
        &["host", "add", "desk", "--script", "hello.json"],
        &["spawn", "--host", "desk", "--provider", "fake", "worker"],
        &[
            "spawn",
            "--host",
            "desk",
            "--script",
            "hello.json",
            "worker",
        ],
        &["up", "topology.json"],
    ] {
        let refused = Command::new(env!("CARGO_BIN_EXE_testnet"))
            .arg("ctl")
            .args(args)
            .current_dir(run.path())
            .env("AMUX_QA_RUN", run.path())
            .output()
            .expect("testnet runs");
        assert!(!refused.status.success(), "{args:?} is refused");
        assert!(
            String::from_utf8_lossy(&refused.stderr).contains("real agents only"),
            "{args:?}: {}",
            String::from_utf8_lossy(&refused.stderr)
        );
    }
}
