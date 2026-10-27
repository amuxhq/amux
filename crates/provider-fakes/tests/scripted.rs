//! Scripted mode held to the corpus. Each fake's script engine plays the
//! scenario of a recording: the host's recorded input, sent at the point
//! the recording sent it, and a script standing in for the model. What the
//! fake reports must be the ordered sequence of frame kinds the provider
//! recorded (message type, subtype, method, row type, lifecycle state,
//! result subtype and terminal reason). Ids, timestamps and model text are
//! not compared, nor are frames the fakes do not model at all: rate limits,
//! hook progress, background task tracking, streaming deltas and the
//! context attachments Claude writes around a prompt.
//!
//! Stdio recordings run until the host closes the provider's input, so the
//! whole sequence is compared. A terminal recording stops once the recorder
//! has seen what it waited for, so its transcript rows and hook payloads
//! must each be a prefix of what the fake writes.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use patience::until_within;
use provider_fakes::playback::{self, Channel, Process};
use provider_fakes::{Kind, SCRIPT_ENV};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// How long the fake may take to catch up with the recording before the
/// host's next input; past it the comparison shows where they parted.
const SYNC: Duration = Duration::from_secs(10);
/// How long a finished session may take to end.
const END: Duration = Duration::from_secs(20);
/// The fake terminal's first screen.
const SCREEN: &[u8] = b"Claude Code (scripted)";
/// Stands for a file the driver creates when the recording's gated frame is
/// due, releasing a held call.
const GATE: &str = "$GATE";
/// Stands for a file that never exists, holding the fake where a terminal
/// recording stops.
const NEVER: &str = "$NEVER";

struct Scenario {
    kind: Kind,
    recording: &'static str,
    /// The model's side of the recorded session.
    script: Value,
    /// The first recorded frame of this kind is where the held call returns:
    /// the driver creates the gate once the fake has caught up to it.
    gate_before: Option<&'static str>,
}

fn root(kind: Kind) -> PathBuf {
    let corpus = match kind {
        Kind::ClaudeSdk => "claude-specs/fixtures/sdk",
        Kind::ClaudePty => "claude-specs/fixtures/pty",
        Kind::Codex => "codex-specs/fixtures/runtime",
    };
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join(corpus)
}

fn binary(kind: Kind) -> &'static str {
    match kind {
        Kind::ClaudeSdk => env!("CARGO_BIN_EXE_fake-claude-sdk"),
        Kind::ClaudePty => env!("CARGO_BIN_EXE_fake-claude-pty"),
        Kind::Codex => env!("CARGO_BIN_EXE_fake-codex"),
    }
}

/// The kind of a frame a stdio provider wrote, or `None` for frames no
/// fake models.
fn frame_kind(kind: Kind, frame: &Value) -> Option<String> {
    match kind {
        Kind::ClaudeSdk => sdk_kind(frame),
        Kind::Codex => codex_kind(frame),
        Kind::ClaudePty => unreachable!("terminal sessions report rows and hooks"),
    }
}

fn blocks(content: &Value) -> String {
    content
        .as_array()
        .into_iter()
        .flatten()
        .map(|block| match block["type"].as_str().unwrap_or_default() {
            // Claude's interruption markers are protocol, not model text.
            "text"
                if block["text"]
                    .as_str()
                    .is_some_and(|t| t.starts_with("[Request")) =>
            {
                format!("text {}", block["text"].as_str().unwrap_or_default())
            }
            other => other.to_owned(),
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn sdk_kind(frame: &Value) -> Option<String> {
    let kind = frame["type"].as_str()?;
    let subtype = frame["subtype"].as_str().unwrap_or_default();
    Some(match kind {
        "stream_event" | "rate_limit_event" => return None,
        "system"
            if matches!(
                subtype,
                "hook_started"
                    | "hook_response"
                    | "thinking_tokens"
                    | "task_started"
                    | "task_progress"
                    | "task_notification"
                    | "status"
            ) =>
        {
            return None;
        }
        "command_lifecycle" => format!("command_lifecycle {}", frame["state"].as_str()?),
        "result" => format!(
            "result {subtype} {}",
            frame["terminal_reason"].as_str().unwrap_or_default()
        ),
        "control_request" => format!(
            "control_request {}",
            frame["request"]["subtype"].as_str().unwrap_or_default()
        ),
        "control_response" => format!(
            "control_response {}",
            frame["response"]["subtype"].as_str().unwrap_or_default()
        ),
        "user" if frame["isReplay"] == true => "user replay".into(),
        "user" | "assistant" => format!("{kind} {}", blocks(&frame["message"]["content"])),
        _ if subtype.is_empty() => kind.to_owned(),
        _ => format!("{kind} {subtype}"),
    })
}

fn codex_kind(frame: &Value) -> Option<String> {
    let Some(method) = frame["method"].as_str() else {
        return Some(
            if frame.get("error").is_some() {
                "error"
            } else {
                "response"
            }
            .into(),
        );
    };
    if frame.get("id").is_some() {
        return Some(format!("request {method}"));
    }
    Some(match method {
        "item/agentMessage/delta"
        | "item/reasoning/summaryPartAdded"
        | "item/reasoning/summaryTextDelta"
        | "item/reasoning/textDelta"
        | "item/commandExecution/outputDelta"
        | "item/plan/delta"
        | "thread/tokenUsage/updated"
        | "account/rateLimits/updated"
        | "account/updated"
        | "mcpServer/startupStatus/updated"
        | "remoteControl/status/changed"
        | "warning" => return None,
        "thread/status/changed" => {
            let status = &frame["params"]["status"];
            let flags: Vec<&str> = status["activeFlags"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            format!(
                "{method} {} {}",
                status["type"].as_str().unwrap_or_default(),
                flags.join(",")
            )
        }
        "item/started" | "item/completed" => format!(
            "{method} {}",
            frame["params"]["item"]["type"].as_str().unwrap_or_default()
        ),
        _ => method.to_owned(),
    })
}

/// The kind of a transcript row terminal Claude wrote, or `None` for rows
/// no fake models.
fn row_kind(row: &Value) -> Option<String> {
    let kind = row["type"].as_str()?;
    Some(match kind {
        "last-prompt"
        | "mode"
        | "permission-mode"
        | "atis-latch"
        | "ai-title"
        | "file-history-snapshot"
        | "bridge-session" => return None,
        "attachment" => {
            let attachment = row["attachment"]["type"].as_str()?;
            if attachment != "queued_command" {
                return None;
            }
            format!("attachment {attachment}")
        }
        "queue-operation" => format!("queue-operation {}", row["operation"].as_str()?),
        "system" => format!("system {}", row["subtype"].as_str()?),
        "user" if row["message"]["content"].is_string() => "user prompt".into(),
        "user" | "assistant" => format!("{kind} {}", blocks(&row["message"]["content"])),
        other => other.to_owned(),
    })
}

/// A script with its placeholders pointing at real files.
fn script(scenario: &Scenario, dir: &Path) -> (PathBuf, PathBuf) {
    let gate = dir.join("gate");
    let never = dir.join("never");
    fn place(value: &mut Value, gate: &Path, never: &Path) {
        match value {
            Value::String(text) if text == GATE => *text = gate.display().to_string(),
            Value::String(text) if text == NEVER => *text = never.display().to_string(),
            Value::Array(items) => items.iter_mut().for_each(|item| place(item, gate, never)),
            Value::Object(fields) => fields.values_mut().for_each(|v| place(v, gate, never)),
            _ => {}
        }
    }
    let mut script = scenario.script.clone();
    place(&mut script, &gate, &never);
    // The script must load as the fakes load it.
    let script: provider_fakes::Script = serde_json::from_value(script).unwrap();
    let path = dir.join("script.json");
    std::fs::write(&path, serde_json::to_vec(&script).unwrap()).unwrap();
    (path, gate)
}

fn first_process(scenario: &Scenario) -> Process {
    playback::load(&root(scenario.kind).join(scenario.recording))
        .unwrap()
        .into_iter()
        .next()
        .expect("a recorded provider process")
}

fn mismatch(what: &str, fake: &[String], recorded: &[String]) -> String {
    let at = fake
        .iter()
        .zip(recorded)
        .position(|(a, b)| a != b)
        .unwrap_or(fake.len().min(recorded.len()));
    let show = |items: &[String]| {
        items
            .iter()
            .enumerate()
            .map(|(index, item)| format!("{}{index:3} {item}", if index == at { ">" } else { " " }))
            .collect::<Vec<_>>()
            .join("\n")
    };
    format!(
        "{what} part at {at}\nfake\n{}\nrecording\n{}",
        show(fake),
        show(recorded)
    )
}

async fn stdio(scenario: &Scenario) -> Result<(), String> {
    let process = first_process(scenario);
    let dir = tempfile::tempdir().unwrap();
    let (path, gate) = script(scenario, dir.path());
    let mut child = tokio::process::Command::new(binary(scenario.kind))
        .args(&process.argv)
        .env(SCRIPT_ENV, &path)
        // Keeps a fake Claude's transcript out of the person's own
        // `~/.claude`, where it would otherwise write it as Claude does.
        .env("CLAUDE_CONFIG_DIR", dir.path().join("claude"))
        .current_dir(dir.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let mut fake: Vec<(String, Value)> = Vec::new();
    let mut recorded: Vec<(String, Value)> = Vec::new();
    // Read the fake's frames until it has written `count` compared ones.
    async fn catch_up(
        kind: Kind,
        lines: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
        fake: &mut Vec<(String, Value)>,
        count: usize,
        within: Duration,
    ) {
        let until = Instant::now() + within;
        while fake.len() < count {
            let left = until.saturating_duration_since(Instant::now());
            let line = match tokio::time::timeout(left, lines.next_line()).await {
                Ok(Ok(Some(line))) => line,
                Ok(Ok(None)) => panic!(
                    "the fake's stdout ended at {} of {count} compared frames",
                    fake.len()
                ),
                Ok(Err(error)) => panic!("reading the fake's stdout failed: {error}"),
                Err(_) => panic!(
                    "the fake wrote {} of {count} compared frames within {within:?}",
                    fake.len()
                ),
            };
            let frame: Value = serde_json::from_str(&line).unwrap();
            if let Some(kind) = frame_kind(kind, &frame) {
                fake.push((kind, frame));
            }
        }
    }
    // Reads the fake's frames to its end, once its stdin is closed.
    async fn drain(
        kind: Kind,
        lines: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
        fake: &mut Vec<(String, Value)>,
        within: Duration,
    ) {
        let read = tokio::time::timeout(within, async {
            while let Some(line) = lines.next_line().await.unwrap() {
                let frame: Value = serde_json::from_str(&line).unwrap();
                if let Some(kind) = frame_kind(kind, &frame) {
                    fake.push((kind, frame));
                }
            }
        })
        .await;
        assert!(
            read.is_ok(),
            "the fake did not end within {within:?} of its stdin closing"
        );
    }
    for event in &process.events {
        match event.channel {
            Channel::Input => {
                catch_up(scenario.kind, &mut lines, &mut fake, recorded.len(), SYNC).await;
                let mut frame: Value = serde_json::from_slice(&event.bytes).unwrap();
                answer_the_fake(scenario.kind, &mut frame, &fake, &recorded);
                let mut line = serde_json::to_vec(&frame).unwrap();
                line.push(b'\n');
                let _ = stdin.as_mut().unwrap().write_all(&line).await;
            }
            Channel::Output => {
                let frame: Value = serde_json::from_slice(&event.bytes).unwrap();
                let Some(kind) = frame_kind(scenario.kind, &frame) else {
                    continue;
                };
                if scenario.gate_before == Some(kind.as_str()) && !gate.exists() {
                    catch_up(scenario.kind, &mut lines, &mut fake, recorded.len(), SYNC).await;
                    std::fs::write(&gate, "").unwrap();
                }
                recorded.push((kind, frame));
            }
            other => panic!("a stdio recording has no {other:?}"),
        }
    }
    drop(stdin.take());
    drain(scenario.kind, &mut lines, &mut fake, END).await;
    let _ = tokio::time::timeout(END, child.wait()).await;
    let fake: Vec<String> = fake.into_iter().map(|(kind, _)| kind).collect();
    let recorded: Vec<String> = recorded.into_iter().map(|(kind, _)| kind).collect();
    if fake != recorded {
        return Err(mismatch("frames", &fake, &recorded));
    }
    Ok(())
}

/// Point a recorded answer at the fake's request: the provider's request
/// ids differ between the recording and the fake, so each recorded request
/// is matched to the fake's frame at the same place in the sequence.
fn answer_the_fake(
    kind: Kind,
    input: &mut Value,
    fake: &[(String, Value)],
    recorded: &[(String, Value)],
) {
    let (slot, id_of): (&mut Value, fn(&Value) -> &Value) = match kind {
        Kind::ClaudeSdk if input["type"] == "control_response" => (
            &mut input["response"]["request_id"],
            |frame| &frame["request_id"],
        ),
        Kind::Codex if input.get("method").is_none() && input.get("id").is_some() => {
            (&mut input["id"], |frame| &frame["id"])
        }
        _ => return,
    };
    let request = |kind: &str| kind.starts_with("control_request") || kind.starts_with("request ");
    for ((recorded_kind, recorded_frame), (fake_kind, fake_frame)) in recorded.iter().zip(fake) {
        if request(recorded_kind) && request(fake_kind) && id_of(recorded_frame) == slot {
            *slot = id_of(fake_frame).clone();
            return;
        }
    }
}

async fn terminal(scenario: &Scenario) -> Result<(), String> {
    const SESSION: &str = "5e55e55e-0000-4000-8000-0000000000cc";
    let process = first_process(scenario);
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().canonicalize().unwrap();
    let (path, gate) = script(scenario, &base);
    let config = base.join("config");
    let cwd = base.join("work");
    std::fs::create_dir_all(&cwd).unwrap();
    let hook_log = base.join("hooks.jsonl");
    // The recorded launch, with its hooks pointed at a local recorder.
    let mut args = Vec::new();
    let mut hooks = serde_json::Map::new();
    let mut argv = process.argv.iter();
    while let Some(arg) = argv.next() {
        if arg == "--settings" {
            let settings: Value = serde_json::from_str(argv.next().unwrap()).unwrap();
            let log = hook_log.display();
            let command = format!("cat >> '{log}' && printf '\\n' >> '{log}'");
            for event in settings["hooks"]
                .as_object()
                .into_iter()
                .flatten()
                .map(|(k, _)| k)
            {
                hooks.insert(
                    event.clone(),
                    json!([{"hooks": [{"type": "command", "command": command}]}]),
                );
            }
            continue;
        }
        args.push(arg.clone());
    }
    args.extend([
        "--settings".into(),
        json!({ "hooks": hooks }).to_string(),
        "--session-id".into(),
        SESSION.into(),
    ]);
    let spawned = pty_host::spawn(pty_host::PtySpawn {
        command: binary(Kind::ClaudePty).into(),
        args,
        cwd: cwd.clone(),
        env: vec![
            (OsString::from("CLAUDE_CONFIG_DIR"), config.clone().into()),
            (OsString::from(SCRIPT_ENV), path.into()),
        ],
        env_remove: Vec::new(),
        size: pty_host::PtySize::default(),
    })
    .unwrap();
    // The fake dies with the test, whichever way it ends: the PTY's
    // reader and waiter are blocking tasks the runtime waits for on its
    // way down, and they only return once the child is gone.
    let reaper = Reaper(spawned.handle);
    let handle = &reaper.0;
    let mut output = handle.output();
    // Keys count once the terminal takes input: from its first screen, after
    // it resets its input.
    let mut seen = Vec::new();
    let live = tokio::time::timeout(END, async {
        while let Some(bytes) = output.recv().await {
            seen.extend_from_slice(&bytes);
            if seen.windows(SCREEN.len()).any(|window| window == SCREEN) {
                return;
            }
        }
    })
    .await;
    assert!(live.is_ok(), "the terminal takes input");
    tokio::spawn(async move { while output.recv().await.is_some() {} });
    let transcript = playback::transcript_path(&config, &cwd, SESSION);
    let read = |path: &Path| -> Vec<Value> {
        std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    };
    let fake_rows = || -> Vec<String> { read(&transcript).iter().filter_map(row_kind).collect() };
    let fake_hooks = || -> Vec<String> {
        read(&hook_log)
            .iter()
            .map(|hook| {
                hook["hook_event_name"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned()
            })
            .collect()
    };
    // Waits for the fake to reach the recording's row and hook counts
    // before the next input.
    let catch_up = async |rows: usize, hooks: usize| {
        until_within("the fake to catch up with the recording", SYNC, || {
            let (have_rows, have_hooks) = (fake_rows().len(), fake_hooks().len());
            std::future::ready(if have_rows >= rows && have_hooks >= hooks {
                Ok(())
            } else {
                Err(format!(
                    "{have_rows} of {rows} rows, {have_hooks} of {hooks} hooks"
                ))
            })
        })
        .await
        .unwrap();
    };
    let mut rows = Vec::new();
    let mut hooks = Vec::new();
    for event in &process.events {
        match event.channel {
            Channel::Input => {
                catch_up(rows.len(), hooks.len()).await;
                handle.write(&event.bytes).await.unwrap();
            }
            Channel::Transcript => {
                let row: Value = serde_json::from_slice(&event.bytes).unwrap();
                let Some(kind) = row_kind(&row) else { continue };
                if scenario.gate_before == Some(kind.as_str()) && !gate.exists() {
                    // Claude may run a call's hooks before it writes the
                    // rows ahead of its result, so only rows gate the call.
                    catch_up(rows.len(), 0).await;
                    std::fs::write(&gate, "").unwrap();
                }
                rows.push(kind);
            }
            Channel::Hook => {
                let hook: Value = serde_json::from_slice(&event.bytes).unwrap();
                hooks.push(
                    hook["hook_event_name"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned(),
                );
            }
            Channel::Output | Channel::Exit => {}
        }
    }
    catch_up(rows.len(), hooks.len()).await;
    // Anything the fake would still write at this point is past the
    // recording, and the fake may write more than it (a queued prompt's
    // fold): the comparison below allows extras, so nothing is asserted on
    // them. A propagation sleep, so a mismatch report shows them.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let (fake_rows, fake_hooks) = (fake_rows(), fake_hooks());
    drop(reaper);
    if !fake_rows.starts_with(&rows) {
        return Err(mismatch("transcript rows", &fake_rows, &rows));
    }
    if !fake_hooks.starts_with(&hooks) {
        return Err(mismatch("hook payloads", &fake_hooks, &hooks));
    }
    Ok(())
}

/// Kills the fake's process group when dropped, so the test's end, by
/// return or by panic, is the fake's end too.
struct Reaper(pty_host::PtyHandle);

impl Drop for Reaper {
    fn drop(&mut self) {
        let _ = self
            .0
            .signal_process_group(pty_host::ProcessGroupSignal::Kill);
    }
}

async fn conforms(scenario: Scenario) {
    let played = match scenario.kind {
        Kind::ClaudePty => terminal(&scenario).await,
        Kind::ClaudeSdk | Kind::Codex => stdio(&scenario).await,
    };
    if let Err(drift) = played {
        panic!(
            "{} {}: {drift}",
            scenario.kind.binary_name(),
            scenario.recording
        );
    }
}

fn thinking() -> Value {
    json!({"thinking": {"text": "Considering"}})
}

fn text() -> Value {
    json!({"text": {"chunks": ["Done"]}})
}

fn bash(wait_for: Option<&str>) -> Value {
    let mut tool = json!({"name": "Bash", "class": "consequential", "input": {"command": "true"},
                          "outcome": {"output": "S1"}});
    if let Some(path) = wait_for {
        tool["wait_for"] = json!(path);
    }
    json!({ "tool": tool })
}

fn permission() -> Value {
    json!({"ask": {"permission": {"name": "Bash", "class": "consequential",
                                  "input": {"command": "touch x"}, "outcome": {"output": "ok"}}}})
}

fn question() -> Value {
    json!({"ask": {"question": {"questions": [
        {"question": "Which?", "header": "Pick", "options": ["A", "B"]},
    ]}}})
}

mod claude_sdk {
    use super::*;

    fn scenario(recording: &'static str, steps: Value, gate: Option<&'static str>) -> Scenario {
        Scenario {
            kind: Kind::ClaudeSdk,
            recording,
            script: json!({ "steps": steps }),
            gate_before: gate,
        }
    }

    #[tokio::test]
    async fn steer_preempted() {
        conforms(scenario(
            "steer_preempted",
            json!([
                thinking(),
                bash(Some(GATE)),
                thinking(),
                text(),
                "turn_end",
                thinking(),
                bash(None),
                thinking(),
                text(),
                "turn_end"
            ]),
            Some("user tool_result"),
        ))
        .await;
    }

    #[tokio::test]
    async fn steer_folded() {
        conforms(scenario(
            "steer_folded",
            json!([
                thinking(),
                bash(Some(GATE)),
                thinking(),
                bash(None),
                thinking(),
                text(),
                "turn_end",
                thinking(),
                text(),
                "turn_end"
            ]),
            Some("user tool_result"),
        ))
        .await;
    }

    #[tokio::test]
    async fn question_asked() {
        conforms(scenario(
            "question_asked",
            json!([thinking(), question(), thinking(), text(), "turn_end"]),
            None,
        ))
        .await;
    }

    #[tokio::test]
    async fn permission_callback() {
        conforms(scenario(
            "permission_callback",
            json!([thinking(), permission(), thinking(), text(), "turn_end"]),
            None,
        ))
        .await;
    }
}

// Unix only: Windows does not host terminal Claude, and the fake terminal
// Claude's raw console mode and hooks are Unix-only in this build; see
// docs/ARCHITECTURE.md, "Windows, as a stated cost".
#[cfg(unix)]
mod claude_pty {
    use super::*;

    fn scenario(recording: &'static str, steps: Value, gate: Option<&'static str>) -> Scenario {
        Scenario {
            kind: Kind::ClaudePty,
            recording,
            script: json!({ "steps": steps }),
            gate_before: gate,
        }
    }

    #[tokio::test]
    async fn steer_send_now() {
        conforms(scenario(
            "steer_send_now",
            json!([thinking(), bash(Some(NEVER)), {"wait_for": {"path": NEVER}}, "turn_end"]),
            None,
        ))
        .await;
    }

    #[tokio::test]
    async fn steer_queued() {
        conforms(scenario(
            "steer_queued",
            json!([
                thinking(),
                bash(Some(GATE)),
                thinking(),
                bash(None),
                thinking(),
                text(),
                "turn_end"
            ]),
            Some("user tool_result"),
        ))
        .await;
    }

    #[tokio::test]
    async fn permission_deny_feedback() {
        conforms(scenario(
            "permission_deny_feedback",
            json!([thinking(), permission(), "turn_end", {"wait_for": {"path": NEVER}},
                   "turn_end"]),
            None,
        ))
        .await;
    }
}

mod codex {
    use super::*;

    fn scenario(recording: &'static str, steps: Value) -> Scenario {
        Scenario {
            kind: Kind::Codex,
            recording,
            script: json!({ "steps": steps }),
            gate_before: None,
        }
    }

    #[tokio::test]
    async fn approval_allow() {
        conforms(scenario(
            "approval_allow",
            json!([
                thinking(),
                text(),
                permission(),
                thinking(),
                text(),
                "turn_end"
            ]),
        ))
        .await;
    }

    #[tokio::test]
    async fn access_grant() {
        conforms(scenario(
            "access_grant",
            json!([thinking(), text(),
                   {"ask": {"grant": {"reason": "Write outside", "paths": ["/tmp/x"]}}},
                   thinking(), bash(None), text(), "turn_end"]),
        ))
        .await;
    }

    #[tokio::test]
    async fn tool_server_form() {
        conforms(scenario(
            "tool_server_form",
            json!([thinking(), text(), thinking(),
                   {"ask": {"form": {"server": "spec", "message": "Confirm the word BLUE.",
                                     "schema": {"type": "object", "properties": {}}}}},
                   {"ask": {"link": {"server": "spec", "message": "Open the page to confirm.",
                                     "url": "https://example.com/confirm"}}},
                   text(), "turn_end"]),
        ))
        .await;
    }

    #[tokio::test]
    async fn plan_mode() {
        conforms(scenario(
            "plan_mode",
            json!([thinking(), question(), thinking(),
                   {"ask": {"plan": {"markdown": "## Write the color"}}}, "turn_end"]),
        ))
        .await;
    }
}
