//! `acp-probe` in front of a stand-in agent carries its stdio through
//! unchanged and records it, and a capture spanning two agent processes
//! joins the corpus sanitized, each line naming its process.

#![cfg(unix)]

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

use provider_fakes::dispatch::{self, Plan};
use qualification::recording::{acp, load_recording};
use serde_json::{Value, json};

/// A stand-in agent in `dir` that answers one request with the id it was
/// sent, says something on stderr and exits: the `fake-provider` binary
/// playing an answer.
fn agent(dir: &Path) -> std::path::PathBuf {
    let agent = dispatch::program(dir, "agent");
    dispatch::install(
        &dispatch::program(provider_fakes::binaries::dir(), dispatch::BINARY),
        &agent,
        &Plan::Answer {
            lines: 1,
            stdout: "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"agentInfo\":{\"name\":\"Stand-in\",\"version\":\"2.3.4\"}}}\n"
                .to_owned(),
            stderr: "agent noise\n".to_owned(),
            code: 0,
        },
    )
    .unwrap();
    agent
}

fn rows(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn the_probe_carries_an_agents_stdio_through_and_records_it() {
    let root = tempfile::tempdir().unwrap();
    let capture = root.path().join("capture");
    let agent = agent(root.path());
    let request =
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1}}"#;

    let mut child = Command::new(source_tree::bin!("acp-probe"))
        .arg("--")
        .arg(&agent)
        .current_dir(root.path())
        .env("ACP_CAPTURE_DIR", &capture)
        .env_remove("ACP_REAL_PATH")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    writeln!(child.stdin.take().unwrap(), "{request}").unwrap();
    let output = child.wait_with_output().unwrap();

    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.starts_with(r#"{"jsonrpc":"2.0","id":1,"result""#),
        "{stdout}"
    );
    assert_eq!(String::from_utf8_lossy(&output.stderr), "agent noise\n");
    let io = rows(&capture.join("io.jsonl"));
    let directions: Vec<&str> = io.iter().map(|row| row["dir"].as_str().unwrap()).collect();
    assert_eq!(directions, ["stdin", "stdout"]);
    assert_eq!(io[0]["line"], request, "lines are recorded as they crossed");
    let spawn = rows(&capture.join("spawn.jsonl"));
    assert_eq!(spawn.len(), 1);
    assert_eq!(spawn[0]["command"], agent.to_string_lossy().as_ref());
    assert!(io.iter().all(|row| row["process"] == spawn[0]["process"]));
    let errors = rows(&capture.join("stderr.jsonl"));
    assert_eq!(errors[0]["line"], "agent noise");
}

#[test]
fn the_probe_refuses_to_run_an_agent_unrecorded() {
    let output = Command::new(source_tree::bin!("acp-probe"))
        .args(["--", "true"])
        .env_remove("ACP_CAPTURE_DIR")
        .env_remove("ACP_REAL_PATH")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn a_two_process_capture_joins_sanitized_with_its_processes_told_apart() {
    let root = tempfile::tempdir().unwrap();
    let capture = root.path().join("capture");
    std::fs::create_dir_all(&capture).unwrap();
    let home = std::env::var("HOME").unwrap();
    let host = gethostname();
    let spawn = |process: &str| {
        json!({"us": 0, "transport_id": process, "process": process, "command": "agent",
               "argv": ["acp"], "cwd": format!("{home}/project")})
    };
    let row = |process: &str, dir: &str, us: u64, line: Value| {
        json!({"us": us, "dir": dir, "line": line.to_string(), "transport_id": "amux",
               "process": process})
    };
    let first = [
        row(
            "p1",
            "stdin",
            2,
            json!({"jsonrpc": "2.0", "id": 2, "method": "session/new", "params": {
            "cwd": format!("{home}/project"),
            "mcpServers": [{"name": "tools", "command": "tools", "args": [],
                "env": [{"name": "API_TOKEN", "value": "env-secret-1"}],
                "headers": [{"name": "Authorization", "value": "header-secret-2"}]}],
            "_meta": {"choices": [{"name": "API key", "value": "kept-choice"}]}}}),
        ),
        row(
            "p1",
            "stdout",
            5,
            json!({"jsonrpc": "2.0", "id": 1, "result": {
            "agentInfo": {"name": "Stand-in", "version": "2.3.4"},
            "_meta": {"hostname": host, "agentId": "install-1"}}}),
        ),
        row(
            "p1",
            "stdout",
            9,
            json!({"jsonrpc": "2.0", "id": 2, "result": {
            "sessionId": "s-1",
            "_meta": {"antigravity": {"tier": {"name": "Some Plan", "id": "plan-1"}}},
            "subscription_tier_display": "Top Tier",
            "configOptions": [{"id": "model", "category": "model", "currentValue": "big"}]}}),
        ),
        row(
            "p1",
            "stdout",
            12,
            json!({"jsonrpc": "2.0", "method": "session/update", "params": {
            "sessionId": "s-1", "update": {"sessionUpdate": "config_option_update",
            "configOptions": [{"id": "model", "category": "model", "currentValue": "small"}]}}}),
        ),
    ];
    let second = [
        row(
            "p2",
            "stdin",
            1,
            json!({"jsonrpc": "2.0", "id": 2, "method": "session/load",
            "params": {"sessionId": "s-1", "cwd": format!("{home}/project")}}),
        ),
        row(
            "p2",
            "stdout",
            3,
            json!({"jsonrpc": "2.0", "method": "session/update", "params": {
            "sessionId": "s-1", "update": {"sessionUpdate": "agent_message_chunk",
            "content": {"type": "text", "text": format!("I read {home}/project/notes.txt")}}}}),
        ),
    ];
    let lines = |rows: &[Value]| {
        rows.iter()
            .map(|row| format!("{row}\n"))
            .collect::<String>()
    };
    std::fs::write(
        capture.join("spawn.jsonl"),
        lines(&[spawn("p1"), spawn("p2")]),
    )
    .unwrap();
    std::fs::write(
        capture.join("io.jsonl"),
        lines(&first)
            + &lines(&second)
            + "{\"us\":4,\"dir\":\"stderr\",\"line\":\"x\",\"process\":\"p1\"}\n",
    )
    .unwrap();

    let corpus = root.path().join("corpus");
    let joined = acp::join(
        &capture,
        &corpus,
        "stand-in-load",
        "stand-in",
        "a test",
        serde_json::Map::from_iter([("note".to_owned(), json!(format!("{home}/project failed")))]),
    )
    .unwrap();

    let manifest = load_recording(&joined).unwrap();
    assert_eq!(manifest.recorded.provider, "stand-in");
    assert_eq!(manifest.recorded.version.to_string(), "2.3.4");
    assert_eq!(
        manifest.recorded.model, "small",
        "the newest model option wins"
    );
    assert_eq!(manifest.session_ids, ["s-1"]);
    assert_eq!(manifest.provider_extra["driven_by"], "a test");
    let io = std::fs::read_to_string(joined.join("io.jsonl")).unwrap();
    let processes: Vec<String> = io
        .lines()
        .map(|line| {
            serde_json::from_str::<Value>(line).unwrap()["process"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect();
    assert_eq!(
        processes,
        ["1", "1", "1", "1", "2", "2"],
        "stderr rows do not join"
    );
    for leaked in [
        home.as_str(),
        host.as_str(),
        "Some Plan",
        "plan-1",
        "install-1",
        "env-secret-1",
        "header-secret-2",
        "Top Tier",
    ] {
        assert!(!io.contains(leaked), "{leaked} left in {io}");
    }
    assert!(
        io.contains("\\\"tier\\\":{"),
        "the tier keeps its shape: {io}"
    );
    assert!(
        io.contains("kept-choice"),
        "only env and headers pairs are judged by name"
    );
    let manifest = std::fs::read_to_string(joined.join("manifest.json")).unwrap();
    assert!(!manifest.contains(&home), "{manifest}");
    let spawns = std::fs::read_to_string(joined.join("spawn.jsonl")).unwrap();
    assert!(!spawns.contains(&home), "{spawns}");
}

fn gethostname() -> String {
    let output = Command::new("hostname").output().unwrap();
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

#[test]
fn a_recording_name_must_be_one_plain_folder_inside_the_corpus() {
    let root = tempfile::tempdir().unwrap();
    let corpus = root.path().join("corpus");
    let outside = root.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    let capture = root.path().join("capture");
    std::fs::create_dir_all(&capture).unwrap();
    for name in [
        "../outside",
        outside.to_str().unwrap(),
        "a/b",
        ".hidden",
        "",
        "..",
    ] {
        let joined = acp::join(
            &capture,
            &corpus,
            name,
            "p",
            "a test",
            serde_json::Map::new(),
        );
        assert!(joined.is_err(), "{name:?} was accepted");
    }
    assert!(outside.is_dir(), "nothing outside the corpus was removed");
}

/// Runs the probe in front of `agent` (a shell command) with `ACP_CAPTURE_DIR`
/// at `capture`.
fn probe(agent: &str, capture: &Path) -> std::process::Child {
    Command::new(source_tree::bin!("acp-probe"))
        .args(["--", "sh", "-c", agent])
        .current_dir(capture.parent().unwrap())
        .env("ACP_CAPTURE_DIR", capture)
        .env_remove("ACP_REAL_PATH")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

#[test]
fn the_probe_exits_with_the_agents_status_while_the_client_holds_stdin_open() {
    let root = tempfile::tempdir().unwrap();
    let mut child = probe("exit 7", &root.path().join("capture"));
    let _held = child.stdin.take();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            panic!("the probe outlived its agent by 10 s");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    assert_eq!(status.code(), Some(7));
}

#[test]
fn the_probe_relays_bytes_untouched_and_frames_only_the_record() {
    let root = tempfile::tempdir().unwrap();
    let capture = root.path().join("capture");
    let sent: &[u8] = b"one\r\ntwo\xff\nlast";
    let mut child = probe("cat", &capture);
    child.stdin.take().unwrap().write_all(sent).unwrap();
    let output = child.wait_with_output().unwrap();

    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        output.stdout, sent,
        "CRLF, invalid UTF-8 and no final newline survive"
    );
    let io = rows(&capture.join("io.jsonl"));
    let lines = |direction: &str| {
        io.iter()
            .filter(|row| row["dir"] == direction)
            .map(|row| row["line"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>()
    };
    let expected = ["one", "two\u{fffd}", "last"];
    assert_eq!(lines("stdin"), expected);
    assert_eq!(lines("stdout"), expected);
}

#[test]
fn a_capture_that_cannot_be_written_never_starts_the_agent() {
    let root = tempfile::tempdir().unwrap();
    let capture = root.path().join("capture");
    // A folder where the I/O log should be: the log cannot be opened.
    std::fs::create_dir_all(capture.join("io.jsonl")).unwrap();
    let ran = root.path().join("ran");
    let child = probe(&format!("touch '{}'", ran.display()), &capture);
    let output = child.wait_with_output().unwrap();

    assert!(!output.status.success(), "{output:?}");
    assert!(!ran.exists(), "the agent was started");
    assert!(String::from_utf8_lossy(&output.stderr).contains("io.jsonl"));
}
