//! fake-claude-pty driven through a real terminal with the keys a person
//! types into Claude's own menus: what it reports lands in the transcript
//! and on the hook commands, and every row and payload has a recorded
//! shape.

// Unix only: Windows does not host terminal Claude. ConPTY re-renders the
// child's output in its own escape sequences, and Claude's messaging socket and
// the fake's raw console mode are Unix-only in this build; see
// docs/ARCHITECTURE.md, "Windows, as a stated cost".
#![cfg(unix)]

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use patience::{holds_for, until_within};
use provider_fakes::Kind;
use provider_fakes::shape::Classifier;
use serde_json::{Value, json};
use support::{corpus, script_file};

use crate::support;

const DEADLINE: Duration = Duration::from_secs(20);
const SESSION: &str = "5e55e55e-0000-4000-8000-0000000000aa";
/// The fake terminal's first screen.
const SCREEN: &[u8] = b"Claude Code (scripted)";

struct Terminal {
    handle: pty_host::PtyHandle,
    exit: pty_host::ExitMonitor,
    /// Holds the session's files until the test ends.
    _dir: tempfile::TempDir,
    transcript: PathBuf,
    hooks: PathBuf,
    socket: PathBuf,
    token: PathBuf,
}

impl Terminal {
    fn start(script: Value) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let cwd = root.join("work");
        std::fs::create_dir_all(&cwd).unwrap();
        let config = root.join("config");
        let hooks = root.join("hooks.jsonl");
        let recorder = format!("cat >> '{0}' && printf '\\n' >> '{0}'", hooks.display());
        let events = [
            "SessionStart",
            "PreToolUse",
            "PostToolUse",
            "PermissionRequest",
            "Stop",
            "SessionEnd",
            "Elicitation",
        ];
        let token = root.join("token");
        let mut hooks_settings: serde_json::Map<String, Value> = events
            .iter()
            .map(|event| {
                let command = if *event == "SessionStart" {
                    format!(
                        "{recorder}; printf %s \"$CLAUDE_CODE_MESSAGING_TOKEN\" > '{}'",
                        token.display()
                    )
                } else {
                    recorder.clone()
                };
                (
                    event.to_string(),
                    json!([{"hooks": [{"type": "command", "command": command}]}]),
                )
            })
            .collect();
        let settings = json!({ "hooks": std::mem::take(&mut hooks_settings) });
        let socket = messaging_socket();
        let script = script_file(&root, script);
        let spawned = pty_host::spawn(pty_host::PtySpawn {
            command: env!("CARGO_BIN_EXE_fake-claude-pty").into(),
            args: vec![
                "--session-id".into(),
                SESSION.into(),
                "--settings".into(),
                settings.to_string(),
                "--messaging-socket-path".into(),
                socket.display().to_string(),
            ],
            cwd: cwd.clone(),
            env: vec![
                (OsString::from("CLAUDE_CONFIG_DIR"), config.clone().into()),
                (OsString::from(provider_fakes::SCRIPT_ENV), script.into()),
            ],
            env_remove: Vec::new(),
            size: pty_host::PtySize::default(),
        })
        .unwrap();
        let transcript = provider_fakes::playback::transcript_path(&config, &cwd, SESSION);
        Self {
            handle: spawned.handle,
            exit: spawned.exit,
            _dir: dir,
            transcript,
            hooks,
            socket,
            token,
        }
    }

    /// Start and wait for the terminal to take input: from its first screen,
    /// after it resets its input. Its session starts only with the first
    /// prompt.
    async fn started(script: Value) -> Self {
        let terminal = Self::start(script);
        let mut output = terminal.handle.output();
        let mut seen = Vec::new();
        let live = tokio::time::timeout(DEADLINE, async {
            while let Some(bytes) = output.recv().await {
                seen.extend_from_slice(&bytes);
                if seen.windows(SCREEN.len()).any(|window| window == SCREEN) {
                    return true;
                }
            }
            false
        })
        .await;
        assert_eq!(live.ok(), Some(true), "the first screen is drawn");
        // Nothing reads the screen; drain it so the fake never blocks on it.
        tokio::spawn(async move { while output.recv().await.is_some() {} });
        terminal
    }

    async fn keys(&self, bytes: &[u8]) {
        self.handle.write(bytes).await.unwrap();
    }

    async fn prompt(&self, text: &str) {
        let mut bytes = b"\x1b[200~".to_vec();
        bytes.extend_from_slice(text.as_bytes());
        bytes.extend_from_slice(b"\x1b[201~");
        self.keys(&bytes).await;
        self.keys(b"\r").await;
    }

    fn rows(&self) -> Vec<Value> {
        lines(&self.transcript)
    }

    fn hooks(&self) -> Vec<Value> {
        lines(&self.hooks)
    }

    /// Wait until `count` payloads of `event` have reached the hook command.
    async fn hooks_of(&self, event: &str, count: usize) -> Vec<Value> {
        until_within(&format!("{count} {event} hooks"), DEADLINE, || {
            let hooks = self.hooks();
            let found: Vec<Value> = hooks
                .iter()
                .filter(|hook| hook["hook_event_name"] == event)
                .cloned()
                .collect();
            std::future::ready(if found.len() >= count {
                Ok(found)
            } else {
                Err(format!("hooks: {hooks:?}"))
            })
        })
        .await
        .unwrap()
    }

    /// Wait for a row the predicate matches.
    async fn row(&self, matches: impl Fn(&Value) -> bool) -> Value {
        until_within("a matching row", DEADLINE, || {
            let rows = self.rows();
            std::future::ready(
                rows.iter()
                    .find(|row| matches(row))
                    .cloned()
                    .ok_or_else(|| format!("rows: {rows:?}")),
            )
        })
        .await
        .unwrap()
    }

    /// How many payloads of `event` have reached the hook command so far.
    fn hooks_seen(&self, event: &str) -> usize {
        self.hooks()
            .into_iter()
            .filter(|hook| hook["hook_event_name"] == event)
            .count()
    }

    async fn hook(&self, event: &str) -> Value {
        self.hooks_of(event, 1).await.remove(0)
    }

    /// Every row and payload so far has a recorded shape.
    fn check_shapes(&self) {
        self.check_shapes_but(&[]);
    }

    /// Every row and payload but hooks of these events has a recorded shape.
    fn check_shapes_but(&self, unrecorded: &[&str]) {
        let corpus = corpus(Kind::ClaudePty);
        let mut classifier = Classifier::default();
        let hooks = self
            .hooks()
            .into_iter()
            .filter(|hook| {
                !unrecorded
                    .iter()
                    .any(|event| hook["hook_event_name"] == *event)
            })
            .collect::<Vec<_>>();
        for frame in self.rows().iter().chain(&hooks) {
            let group = classifier.provider(Kind::ClaudePty, frame);
            if let Err(drift) = corpus.check(Kind::ClaudePty, &group, frame) {
                panic!("{drift}");
            }
        }
    }

    async fn exit_code(mut self) -> u32 {
        self.handle
            .signal_process_group(pty_host::ProcessGroupSignal::Terminate)
            .ok();
        tokio::time::timeout(DEADLINE, self.exit.wait())
            .await
            .unwrap()
            .exit_code()
    }
}

impl Drop for Terminal {
    // A failed assertion must not leave the fake holding the terminal open.
    fn drop(&mut self) {
        let _ = self
            .handle
            .signal_process_group(pty_host::ProcessGroupSignal::Kill);
    }
}

/// Where the fake is told to serve its messaging socket: a Unix socket path,
/// which must be short.
fn messaging_socket() -> PathBuf {
    let dir = tempfile::Builder::new()
        .prefix("fp")
        .tempdir_in("/tmp")
        .unwrap();
    dir.keep().join("m.sock")
}

fn lines(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn contents(rows: &[Value]) -> Vec<String> {
    rows.iter()
        .filter(|row| row["type"] == "user" || row["type"] == "assistant")
        .map(|row| {
            let content = &row["message"]["content"];
            match content {
                Value::String(text) => format!("{} {text}", row["type"].as_str().unwrap()),
                _ => {
                    let block = &content[0];
                    let kind = block["type"].as_str().unwrap();
                    let detail = match kind {
                        "text" => block["text"].as_str().unwrap().to_owned(),
                        "tool_use" => block["name"].as_str().unwrap().to_owned(),
                        "tool_result" => format!("error={}", block["is_error"]),
                        _ => String::new(),
                    };
                    format!("{} {kind} {detail}", row["type"].as_str().unwrap())
                }
            }
        })
        .collect()
}

#[tokio::test]
async fn a_typed_prompt_runs_a_turn_reported_in_rows_and_hooks() {
    let terminal = Terminal::started(json!({"steps": [
        {"thinking": {"text": "Consider"}},
        {"tool": {"class": "exploration", "outcome": {"output": "read"}}},
        {"tool": {"class": "consequential", "input": {"command": "false"},
                  "outcome": {"output": "Exit code 1", "error": true}}},
        {"text": {"chunks": ["Do", "ne"]}},
        "turn_end",
    ]}))
    .await;
    terminal.prompt("Look around").await;
    let stop = terminal.hook("Stop").await;
    assert_eq!(stop["last_assistant_message"], "Done");
    assert_eq!(
        contents(&terminal.rows()),
        [
            "user Look around",
            "assistant thinking ",
            "assistant tool_use Read",
            "user tool_result error=false",
            "assistant tool_use Bash",
            "user tool_result error=true",
            "assistant text Done",
        ]
    );
    let pre = terminal.hooks_of("PreToolUse", 2).await;
    assert_eq!(pre[0]["tool_name"], "Read");
    // A failed call gets no PostToolUse.
    assert_eq!(terminal.hooks_of("PostToolUse", 1).await.len(), 1);
    let start = terminal.hook("SessionStart").await;
    assert_eq!(
        start["transcript_path"],
        terminal.transcript.display().to_string()
    );
    terminal.check_shapes();
    assert_eq!(terminal.exit_code().await, 0);
}

#[tokio::test]
async fn menus_answer_by_digit_and_a_deny_ends_the_turn() {
    let terminal = Terminal::started(json!({"steps": [
        {"ask": {"permission": {"class": "consequential", "input": {"command": "touch x"},
                                "outcome": {"output": "ok"}}}},
        {"ask": {"plan": {"markdown": "1. Do it"}}},
        {"ask": {"plan": {"markdown": "2. Again"}}},
        {"ask": {"permission": {"class": "consequential", "input": {"command": "rm x"}}}},
        {"text": {"chunks": ["never said"]}},
        "turn_end",
        {"text": {"chunks": ["next turn"]}},
        "turn_end",
    ]}))
    .await;
    terminal.prompt("Ask").await;
    terminal.hooks_of("PermissionRequest", 1).await;
    terminal.keys(b"1").await;
    terminal.hooks_of("PermissionRequest", 2).await;
    terminal.keys(b"3").await;
    terminal.keys(b"Not yet\r").await;
    terminal.hooks_of("PermissionRequest", 3).await;
    terminal.keys(b"2").await;
    terminal.hooks_of("PermissionRequest", 4).await;
    terminal.keys(b"3").await;
    // A deny ends the turn with its duration row and no Stop hook.
    terminal.row(|row| row["subtype"] == "turn_duration").await;
    let rows = contents(&terminal.rows());
    assert_eq!(
        rows,
        [
            "user Ask",
            "assistant tool_use Bash",
            "user tool_result error=false",
            "assistant tool_use ExitPlanMode",
            "user tool_result error=true",
            "assistant tool_use ExitPlanMode",
            "user tool_result error=false",
            "assistant tool_use Bash",
            "user tool_result error=true",
            "user text [Request interrupted by user for tool use]",
        ]
    );
    let feedback = terminal.rows().into_iter().find(|row| {
        row["message"]["content"][0]["content"]
            .as_str()
            .is_some_and(|text| text.contains("Not yet"))
    });
    assert!(feedback.is_some());
    terminal.prompt("Next").await;
    terminal.hook("Stop").await;
    assert_eq!(terminal.hooks_of("Stop", 1).await.len(), 1);
    assert!(contents(&terminal.rows()).contains(&"assistant text next turn".to_owned()));
    terminal.check_shapes();
    assert_eq!(terminal.exit_code().await, 0);
}

/// 2.1.283 in manual mode: Yes, Yes and don't ask again, Yes and switch to
/// auto mode, No. Escape is No; the third entry allows the call and
/// switches the session to auto mode.
#[tokio::test]
async fn escape_denies_on_the_menu_that_offers_auto_mode() {
    let terminal = Terminal::started(json!({"offers_auto_mode": true, "steps": [
        {"ask": {"permission": {"class": "consequential", "input": {"command": "curl a"}}}},
        {"text": {"chunks": ["never said"]}},
        "turn_end",
        {"ask": {"permission": {"class": "consequential", "input": {"command": "curl b"},
                                "outcome": {"output": "ok"}}}},
        "turn_end",
    ]}))
    .await;
    terminal.prompt("Fetch a").await;
    terminal.hooks_of("PermissionRequest", 1).await;
    terminal.keys(b"\x1b").await;
    terminal.row(|row| row["subtype"] == "turn_duration").await;
    terminal.prompt("Fetch b").await;
    terminal.hooks_of("PermissionRequest", 2).await;
    terminal.keys(b"3").await;
    terminal.hook("Stop").await;
    let rows = contents(&terminal.rows());
    assert_eq!(
        rows,
        [
            "user Fetch a",
            "assistant tool_use Bash",
            "user tool_result error=true",
            "user text [Request interrupted by user for tool use]",
            "user Fetch b",
            "assistant tool_use Bash",
            "user tool_result error=false",
        ]
    );
    let modes = terminal
        .rows()
        .into_iter()
        .filter(|row| row["type"] == "permission-mode")
        .map(|row| row["permissionMode"].clone())
        .collect::<Vec<_>>();
    assert_eq!(
        modes,
        [json!("default"), json!("auto")],
        "the session starts in manual mode and only the third entry switches it"
    );
    terminal.check_shapes();
    assert_eq!(terminal.exit_code().await, 0);
}

#[tokio::test]
async fn a_question_form_takes_digits_arrows_space_tab_and_enter() {
    let terminal = Terminal::started(json!({"steps": [
        {"ask": {"question": {"questions": [
            {"question": "Color?", "header": "Color", "options": ["Red", "Blue"]},
            {"question": "Sizes?", "header": "Size", "options": ["S", "M", "L"], "multi_select": true},
        ]}}},
        "turn_end",
    ]})).await;
    terminal.prompt("Choose").await;
    terminal.hook("PermissionRequest").await;
    // Question one: the digit picks Blue. Question two: down to M, toggle,
    // down to the other row, type an answer, toggle it, tab; then the
    // review page's two enters.
    terminal.keys(b"2").await;
    terminal.keys(b"\x1b[B").await;
    terminal.keys(b" ").await;
    terminal.keys(b"\x1b[B\x1b[B").await;
    terminal.keys(b"\r").await;
    terminal.keys(b"XL\r").await;
    terminal.keys(b" ").await;
    terminal.keys(b"\t").await;
    terminal.keys(b"\r").await;
    terminal.keys(b"\r").await;
    terminal.hook("Stop").await;
    let result = terminal
        .rows()
        .into_iter()
        .find(|row| row["toolUseResult"]["answers"].is_object())
        .unwrap();
    assert_eq!(
        result["toolUseResult"]["answers"],
        json!({"Color?": "Blue", "Sizes?": "M, XL"})
    );
    terminal.check_shapes();
    assert_eq!(terminal.exit_code().await, 0);
}

#[tokio::test]
async fn a_prompt_typed_mid_turn_folds_at_the_next_tool_and_escape_interrupts() {
    let probe = tempfile::tempdir().unwrap();
    let gate = probe.path().join("gate");
    let never = probe.path().join("never");
    let terminal = Terminal::started(json!({"steps": [
        {"wait_for": {"path": gate}},
        {"tool": {"class": "exploration"}},
        {"text": {"chunks": ["folded"]}},
        "turn_end",
        {"wait_for": {"path": never}},
        "turn_end",
    ]}))
    .await;
    terminal.prompt("First").await;
    terminal.prompt("Also this").await;
    terminal.row(|row| row["type"] == "queue-operation").await;
    std::fs::write(&gate, "").unwrap();
    terminal.hook("Stop").await;
    let rows = terminal.rows();
    let folded = rows
        .iter()
        .find(|row| row["attachment"]["type"] == "queued_command")
        .unwrap();
    assert_eq!(folded["attachment"]["prompt"], "Also this");
    terminal.prompt("Wait forever").await;
    terminal
        .row(|row| row.to_string().contains("Wait forever"))
        .await;
    terminal.keys(b"\x1b").await;
    terminal
        .row(|row| row["message"]["content"][0]["text"] == "[Request interrupted by user]")
        .await;
    // Claude runs no Stop hook for a turn the user interrupted. A window: a
    // hook that must not run leaves no mark to wait on.
    holds_for(
        "no Stop hook for the interrupted turn",
        Duration::from_millis(100),
        || {
            let stops = terminal.hooks_seen("Stop");
            async move { stops == 1 }
        },
    )
    .await
    .unwrap();
    terminal.check_shapes();
    assert_eq!(terminal.exit_code().await, 0);
}

#[tokio::test]
async fn a_tool_server_dialog_runs_the_elicitation_hook_and_is_held_until_escape_or_its_end() {
    let probe = tempfile::tempdir().unwrap();
    let answered = probe.path().join("answered");
    let terminal = Terminal::started(json!({"steps": [
        {"ask": {"tool_server_dialog": {"server": "github", "tool": "create_issue",
                                        "output": "created #42", "wait_for": answered}}},
        {"ask": {"tool_server_dialog": {"server": "docs", "tool": "sign_in", "link": true}}},
        {"text": {"chunks": ["Done"]}},
        "turn_end",
    ]}))
    .await;
    terminal.prompt("File it").await;
    let form = terminal.hook("Elicitation").await;
    // Claude 2.1.312's field names.
    assert_eq!(form["mcp_server_name"], "github");
    assert_eq!(form["mode"], "form");
    assert_eq!(form["session_id"], SESSION);
    // Held open: the call has no result until the dialog ends. A window: a
    // result that must not come leaves no mark to wait on.
    holds_for("the call held open", Duration::from_millis(200), || {
        let posts = terminal.hooks_seen("PostToolUse");
        async move { posts == 0 }
    })
    .await
    .unwrap();
    std::fs::write(&answered, "").unwrap();
    let post = terminal.hook("PostToolUse").await;
    assert_eq!(post["tool_name"], "mcp__github__create_issue");
    let link = terminal.hooks_of("Elicitation", 2).await.remove(1);
    assert_eq!(link["mode"], "url");
    assert_eq!(link["mcp_server_name"], "docs");
    // The interrupt key cancels the dialog; the turn goes on.
    terminal.keys(b"\x1b").await;
    let stop = terminal.hook("Stop").await;
    assert_eq!(stop["last_assistant_message"], "Done");
    assert_eq!(
        contents(&terminal.rows()),
        [
            "user File it",
            "assistant tool_use mcp__github__create_issue",
            "user tool_result error=false",
            "assistant tool_use mcp__docs__sign_in",
            "user tool_result error=true",
            "assistant text Done",
        ]
    );
    // No terminal recording carries an Elicitation hook or a tool server's
    // call: the sessions were recorded before amux registered the hook, and
    // without tool servers.
    terminal.check_shapes_but(&["Elicitation", "PostToolUse"]);
    assert_eq!(terminal.exit_code().await, 0);
}

#[tokio::test]
async fn a_messaging_socket_message_runs_a_turn_and_hooks_get_its_credentials() {
    use tokio::io::AsyncWriteExt;
    let terminal = Terminal::started(json!({"steps": [
        {"text": {"chunks": ["first"]}},
        "turn_end",
        {"text": {"chunks": ["from a peer"]}},
        "turn_end",
    ]}))
    .await;
    // The session, and with it the first hook, starts with the first prompt.
    terminal.prompt("hello").await;
    terminal.hook("Stop").await;
    // The token reaches hook commands through their environment only.
    let token = std::fs::read_to_string(&terminal.token).unwrap();
    assert_eq!(token.len(), 32);
    assert!(!terminal.hooks()[0].to_string().contains(&token));
    let mut stream = tokio::net::UnixStream::connect(&terminal.socket)
        .await
        .unwrap();
    // Without the right token the message is dropped.
    stream
        .write_all(b"{\"type\":\"auth\",\"token\":\"wrong\"}\n{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"dropped\"}}\n")
        .await
        .unwrap();
    drop(stream);
    // Had the message been taken, its turn would be on the transcript
    // before the tokened one's below.
    let mut stream = tokio::net::UnixStream::connect(&terminal.socket)
        .await
        .unwrap();
    let message = format!(
        "{{\"type\":\"auth\",\"token\":\"{token}\"}}\n{{\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":\"hello from a peer\"}}}}\n"
    );
    stream.write_all(message.as_bytes()).await.unwrap();
    drop(stream);
    terminal.hooks_of("Stop", 2).await;
    let rows = terminal.rows();
    assert!(
        !contents(&rows).iter().any(|row| row.contains("dropped")),
        "the wrong-token message was dropped: {:?}",
        contents(&rows)
    );
    let user = rows
        .iter()
        .find(|row| row["type"] == "user" && row["origin"]["kind"] == "peer")
        .unwrap();
    let content = user["message"]["content"].as_str().unwrap();
    assert!(
        content.starts_with("Another Claude session sent a message:\n<cross-session-message")
            && content.contains(">\nhello from a peer\n</cross-session-message>"),
        "the socket message is wrapped as terminal Claude records it: {content}"
    );
    assert_eq!(user["origin"]["kind"], "peer");
    assert!(contents(&rows).contains(&"assistant text from a peer".to_owned()));
    terminal.check_shapes();
    assert_eq!(terminal.exit_code().await, 0);
}

#[tokio::test]
async fn exit_ends_the_process_with_its_code() {
    let terminal = Terminal::started(json!({"steps": [
        {"text": {"chunks": ["bye"]}},
        {"exit": {"code": 5}},
    ]}))
    .await;
    terminal.prompt("Leave").await;
    let mut exit = terminal.exit.clone();
    let status = tokio::time::timeout(DEADLINE, exit.wait()).await.unwrap();
    assert_eq!(status.exit_code(), 5);
}
