//! fake-claude-sdk as a host sees it: every frame it composes has a recorded
//! shape, and it echoes, queues, folds and cancels the way Claude does.

use std::process::Stdio;
use std::time::Duration;

use provider_fakes::Kind;
use serde_json::{Value, json};
use support::Host as Line;

use crate::support;

/// Claude 2.1.283 sends control_cancel_request when an interrupt lands on an
/// open permission request, as seen live; no recording has one yet.
const EXEMPT: &[&str] = &["control_cancel_request"];

struct Host(Line);

impl std::ops::Deref for Host {
    type Target = Line;
    fn deref(&self) -> &Line {
        &self.0
    }
}

impl std::ops::DerefMut for Host {
    fn deref_mut(&mut self) -> &mut Line {
        &mut self.0
    }
}

impl Host {
    async fn start(script: Value) -> Self {
        let mut host = Host(
            Line::spawn(
                Kind::ClaudeSdk,
                env!("CARGO_BIN_EXE_fake-claude-sdk"),
                &[
                    "--print",
                    "--input-format",
                    "stream-json",
                    "--output-format",
                    "stream-json",
                    "--verbose",
                    "--session-id",
                    "5e55e55e-0000-4000-8000-000000000001",
                    "--replay-user-messages",
                    "--include-partial-messages",
                    "--messaging-socket-path",
                    "/tmp/fake-claude-sdk.sock",
                ],
                script,
                EXEMPT,
            )
            .await,
        );
        host.send(json!({"type":"control_request","request_id":"req_0","request":{"subtype":"initialize"}}))
            .await;
        host.until(|frame| frame["type"] == "control_response")
            .await;
        host
    }

    async fn prompt(&mut self, uuid: &str, text: &str) {
        let frame = json!({
            "type": "user",
            "uuid": uuid,
            "session_id": "",
            "parent_tool_use_id": null,
            "message": { "role": "user", "content": text },
        });
        self.send(frame).await;
    }

    fn trace(&self) -> Vec<String> {
        self.0.trace(describe)
    }

    async fn close(self) -> i32 {
        self.0.close().await
    }
}

fn describe(frame: &Value) -> String {
    let text = |value: &Value| value.as_str().unwrap_or_default().to_owned();
    match text(&frame["type"]).as_str() {
        "command_lifecycle" => format!(
            "lifecycle {} {}",
            &text(&frame["command_uuid"])[..8],
            text(&frame["state"])
        ),
        "user" if frame.get("isReplay").is_some() => {
            format!("replay {}", &text(&frame["uuid"])[..8])
        }
        "user" => format!("user {}", text(&frame["message"]["content"][0]["type"])),
        "assistant" => format!(
            "assistant {}",
            text(&frame["message"]["content"][0]["type"])
        ),
        "result" => format!("result {}", text(&frame["subtype"])),
        "system" => format!("system {}", text(&frame["subtype"])),
        "stream_event" => format!("stream {}", text(&frame["event"]["type"])),
        "control_request" => format!("request {}", text(&frame["request"]["subtype"])),
        "control_response" => "response".into(),
        other => other.to_owned(),
    }
}

const A: &str = "aaaaaaaa-0000-4000-8000-000000000001";
const B: &str = "bbbbbbbb-0000-4000-8000-000000000002";

#[tokio::test]
async fn an_idle_prompt_is_echoed_by_its_uuid_and_closed_by_its_lifecycle() {
    let mut host = Host::start(json!({"steps": [
        {"text": {"chunks": ["Hel", "lo"]}},
        "turn_end",
    ]}))
    .await;
    host.prompt(A, "Say hello").await;
    let result = host.until(|frame| frame["type"] == "result").await;
    assert_eq!(result["result"], "Hello");
    host.until(|frame| frame["type"] == "command_lifecycle" && frame["state"] == "completed")
        .await;
    let replay = host
        .frames
        .iter()
        .find(|frame| frame["isReplay"] == true)
        .unwrap();
    assert_eq!(replay["uuid"], A);
    assert_eq!(replay["message"]["content"], "Say hello");
    assert_eq!(
        host.trace(),
        [
            "response",
            "lifecycle aaaaaaaa queued",
            "lifecycle aaaaaaaa started",
            "system init",
            "replay aaaaaaaa",
            "stream message_start",
            "stream content_block_start",
            "stream content_block_delta",
            "stream content_block_delta",
            "assistant text",
            "stream content_block_stop",
            "result success",
            "lifecycle aaaaaaaa completed",
        ]
    );
    assert_eq!(host.close().await, 0);
}

#[tokio::test]
async fn a_message_mid_turn_folds_at_the_next_tool_result() {
    let dir = tempfile::tempdir().unwrap();
    let gate = dir.path().join("gate");
    let mut host = Host::start(json!({"steps": [
        {"wait_for": {"path": gate}},
        {"tool": {"class": "consequential", "outcome": {"output": "ran"}}},
        {"text": {"chunks": ["done"]}},
        "turn_end",
    ]}))
    .await;
    host.prompt(A, "Run it").await;
    host.until(|frame| frame["isReplay"] == true).await;
    host.prompt(B, "Also this").await;
    host.until(|frame| frame["state"] == "queued" && frame["command_uuid"] == B)
        .await;
    std::fs::write(&gate, "").unwrap();
    host.until(|frame| frame["state"] == "completed" && frame["command_uuid"] == A)
        .await;
    let trace = host.trace();
    let after_init = trace
        .iter()
        .position(|line| line == "replay aaaaaaaa")
        .unwrap();
    assert_eq!(
        trace[after_init..],
        [
            "replay aaaaaaaa",
            "lifecycle bbbbbbbb queued",
            "assistant tool_use",
            "user tool_result",
            "replay bbbbbbbb",
            "lifecycle bbbbbbbb started",
            "stream message_start",
            "stream content_block_start",
            "stream content_block_delta",
            "assistant text",
            "stream content_block_stop",
            // A folded command completes before the turn's result, the one
            // that started the turn after it, as steer_folded records.
            "lifecycle bbbbbbbb completed",
            "result success",
            "lifecycle aaaaaaaa completed",
        ]
    );
    assert_eq!(host.close().await, 0);
}

#[tokio::test]
async fn interrupt_cuts_the_running_turn_short() {
    let dir = tempfile::tempdir().unwrap();
    let gate = dir.path().join("never");
    let mut host = Host::start(json!({"steps": [
        {"wait_for": {"path": gate}},
        {"text": {"chunks": ["never said"]}},
        "turn_end",
    ]}))
    .await;
    host.prompt(A, "Wait").await;
    host.until(|frame| frame["isReplay"] == true).await;
    host.send(
        json!({"type":"control_request","request_id":"req_1","request":{"subtype":"interrupt"}}),
    )
    .await;
    let result = host.until(|frame| frame["type"] == "result").await;
    assert_eq!(result["subtype"], "error_during_execution");
    let trace = host.trace();
    assert!(!trace.iter().any(|line| line.contains("never")));
    let markers = host
        .frames
        .iter()
        .filter(|frame| {
            frame["message"]["content"][0]["text"]
                .as_str()
                .is_some_and(|t| t.starts_with("[Request"))
        })
        .count();
    assert_eq!(markers, 1, "the interrupt writes one marker");
    assert_eq!(host.close().await, 0);
}

#[tokio::test]
async fn every_ask_blocks_until_answered_and_resolves_the_call() {
    let mut host = Host::start(json!({"steps": [
        {"thinking": {"text": "Consider"}},
        {"tool": {"class": "exploration", "outcome": {"output": "read"}}},
        {"tool": {"class": "consequential", "input": {"command": "false"},
                  "outcome": {"output": "Exit code 1", "error": true}}},
        {"ask": {"permission": {"class": "consequential", "input": {"command": "rm x"},
                                "outcome": {"output": "removed"}}}},
        {"ask": {"permission": {"class": "consequential", "outcome": {"output": "denied?"}}}},
        {"ask": {"question": {"questions": [{"question": "Which?", "header": "Pick",
                                             "options": ["Red", "Blue"]}]}}},
        {"ask": {"plan": {"markdown": "1. Do it"}}},
        {"ask": {"form": {"server": "amux", "message": "Confirm",
                          "schema": {"type": "object", "properties": {"ok": {"type": "string"}}}}}},
        {"text": {"chunks": ["all done"]}},
        "turn_end",
    ]}))
    .await;
    host.prompt(A, "Ask everything").await;
    let answers = [
        json!({"behavior": "allow", "updatedInput": {"command": "rm x"}}),
        json!({"behavior": "deny", "message": "No thanks"}),
        json!({"behavior": "allow", "updatedInput": {"questions": [], "answers": {"Which?": "Blue"}}}),
        json!({"behavior": "allow", "updatedInput": {"plan": "1. Do it"}}),
        json!({"action": "accept", "content": {"ok": "yes"}}),
    ];
    for answer in answers {
        let request = host.until(|frame| frame["type"] == "control_request").await;
        host.send(json!({"type":"control_response","response":{
            "subtype":"success","request_id":request["request_id"],"response":answer}}))
            .await;
    }
    let result = host.until(|frame| frame["type"] == "result").await;
    assert_eq!(result["result"], "all done");
    let results: Vec<(bool, String)> = host
        .frames
        .iter()
        .filter(|frame| frame["message"]["content"][0]["type"] == "tool_result")
        .map(|frame| {
            let block = &frame["message"]["content"][0];
            (block["is_error"] == true, block["content"].to_string())
        })
        .collect();
    assert_eq!(results.len(), 7, "{results:?}");
    assert!(!results[0].0 && results[0].1.contains("read"));
    assert!(results[1].0);
    assert!(!results[2].0 && results[2].1.contains("removed"));
    assert!(results[3].0 && results[3].1.contains("No thanks"));
    assert!(results[4].1.contains("\\\"Which?\\\"=\\\"Blue\\\""));
    assert!(results[5].1.contains("approved your plan"));
    assert!(results[6].1.contains("elicitation accept"));
    assert_eq!(host.close().await, 0);
}

/// An approved plan that sets the permission mode takes Claude out of plan
/// mode, and Claude says so in a status message.
#[tokio::test]
async fn an_approved_plan_setting_the_mode_leaves_plan_mode() {
    let mut host = Host::start(json!({"steps": [
        {"ask": {"plan": {"markdown": "1. Do it"}}},
        {"text": {"chunks": ["doing it"]}},
        "turn_end",
    ]}))
    .await;
    host.prompt(A, "Plan it").await;
    let request = host.until(|frame| frame["type"] == "control_request").await;
    host.send(json!({"type":"control_response","response":{
        "subtype":"success","request_id":request["request_id"],"response":{
            "behavior": "allow", "updatedInput": {"plan": "1. Do it"},
            "updatedPermissions": [{"type": "setMode", "mode": "default", "destination": "session"}]}}}))
        .await;
    let status = host
        .until(|frame| frame["type"] == "system" && frame["subtype"] == "status")
        .await;
    assert_eq!(status["permissionMode"], "default");
    assert_eq!(status["status"], Value::Null);
    host.until(|frame| frame["type"] == "result").await;
    assert_eq!(host.close().await, 0);
}

/// An interrupt with a question open, as Claude 2.1.283 answers it:
/// the open request cancelled, the call refused, the tool-use marker, an
/// aborted result.
#[tokio::test]
async fn interrupt_cancels_an_open_question_and_refuses_its_call() {
    let mut host = Host::start(json!({"steps": [
        {"ask": {"question": {"questions": [{"question": "Ship it today?", "header": "Ship",
                                             "options": ["Yes", "No"]}]}}},
        {"text": {"chunks": ["never said"]}},
        "turn_end",
    ]}))
    .await;
    host.prompt(A, "Ask").await;
    let request = host.until(|frame| frame["type"] == "control_request").await;
    host.send(
        json!({"type":"control_request","request_id":"req_1","request":{"subtype":"interrupt"}}),
    )
    .await;
    let cancel = host
        .until(|frame| frame["type"] == "control_cancel_request")
        .await;
    assert_eq!(cancel["request_id"], request["request_id"]);
    let result = host.until(|frame| frame["type"] == "result").await;
    assert_eq!(result["subtype"], "error_during_execution");
    assert_eq!(result["terminal_reason"], "aborted_tools");
    let after: Vec<&Value> = host
        .frames
        .iter()
        .skip_while(|frame| frame["type"] != "control_cancel_request")
        .filter(|frame| frame["type"] == "user")
        .collect();
    let refused = &after[0]["message"]["content"][0];
    assert_eq!(refused["type"], "tool_result");
    assert_eq!(refused["is_error"], true);
    assert_eq!(refused["tool_use_id"], request["request"]["tool_use_id"]);
    assert_eq!(
        after[1]["message"]["content"][0]["text"],
        "[Request interrupted by user for tool use]"
    );
    assert_eq!(host.close().await, 0);
}

#[tokio::test]
async fn a_pause_keeps_the_turn_busy_and_interrupt_cuts_it() {
    let mut host = Host::start(json!({"steps": [
        {"pause": {"ms": 60000}},
        {"text": {"chunks": ["never said"]}},
        "turn_end",
    ]}))
    .await;
    host.prompt(A, "Work").await;
    host.until(|frame| frame["isReplay"] == true).await;
    host.send(
        json!({"type":"control_request","request_id":"req_1","request":{"subtype":"interrupt"}}),
    )
    .await;
    let result = host.until(|frame| frame["type"] == "result").await;
    assert_eq!(result["subtype"], "error_during_execution");
    assert_eq!(host.close().await, 0);
}

#[tokio::test]
async fn exit_ends_the_process_with_its_code_mid_turn() {
    let mut host = Host::start(json!({"steps": [
        {"text": {"chunks": ["bye"]}},
        {"exit": {"code": 3}},
    ]}))
    .await;
    host.prompt(A, "Leave").await;
    host.until(|frame| frame["type"] == "assistant").await;
    assert_eq!(host.0.exited_within(Duration::from_secs(1)).await, 3);
}

#[tokio::test]
async fn a_script_asking_for_an_ask_headless_claude_cannot_raise_fails_to_start() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("script.json");
    std::fs::write(
        &path,
        json!({"steps": [{"ask": {"link": {"server": "s", "message": "m", "url": "https://x"}}}]})
            .to_string(),
    )
    .unwrap();
    let status = tokio::process::Command::new(env!("CARGO_BIN_EXE_fake-claude-sdk"))
        .env(provider_fakes::SCRIPT_ENV, &path)
        .stdin(Stdio::null())
        .status()
        .await
        .unwrap();
    assert_eq!(status.code(), Some(provider_fakes::DRIFT_EXIT));
}

#[tokio::test]
async fn the_initialize_answer_offers_the_scripted_models_and_commands() {
    let offered = |host: &Host| {
        host.frames
            .iter()
            .find(|frame| frame["type"] == "control_response")
            .unwrap()["response"]["response"]
            .clone()
    };
    let host = Host::start(json!({"steps": []})).await;
    let answer = offered(&host);
    assert_eq!(
        answer["models"],
        json!([{"value": "claude-fake-1", "displayName": "claude-fake-1",
                "description": "The scripted model",
                "supportedEffortLevels": ["low", "medium", "high"]}])
    );
    assert_eq!(answer["commands"], json!([]));
    assert_eq!(host.close().await, 0);

    let host = Host::start(json!({
        "steps": [],
        "models": [{"value": "haiku"}],
        "commands": [{"name": "stripe:test-cards", "description": "Test cards",
                      "argument_hint": "[brand]"}],
    }))
    .await;
    let answer = offered(&host);
    assert_eq!(
        answer["models"],
        json!([{"value": "haiku", "displayName": "haiku", "description": ""}])
    );
    assert_eq!(
        answer["commands"],
        json!([{"name": "stripe:test-cards", "description": "Test cards",
                "argumentHint": "[brand]"}])
    );
    assert_eq!(host.close().await, 0);
}

#[tokio::test]
async fn account_and_session_facts_play_in_recorded_shapes() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("notes.md");
    std::fs::write(&file, "old line\n").unwrap();
    let mut host = Host::start(json!({
        "servers": [
            {"name": "linear", "status": "connected"},
            {"name": "grafana", "status": "needs-auth"},
            {"name": "broken", "status": "failed"},
        ],
        "edit_files": true,
        "chunk_ms": 30,
        "steps": [
            {"usage": {"status": "allowed_warning", "windows": [
                {"name": "five_hour", "used_percent": 81.0, "resets_in_s": 3600},
                {"name": "seven_day", "used_percent": 40.0, "resets_in_s": 86400},
            ]}},
            {"text": {"chunks": ["Wri", "ting"]}},
            {"tool": {"name": "Edit", "class": "consequential", "input": {
                "file_path": file, "old_string": "old line", "new_string": "new line",
            }}},
            {"tool": {"name": "Write", "class": "consequential", "input": {
                "file_path": dir.path().join("fresh/new.md"), "content": "fresh\n",
            }}},
            {"tool": {"name": "TaskCreate", "class": "consequential", "input": {"subject": "Draft", "description": "Draft it"}}},
            {"tool": {"name": "TaskUpdate", "class": "consequential", "input": {"taskId": "1", "status": "in_progress"}}},
            {"ask": {"question": {"questions": [{
                "question": "Which layout?",
                "header": "Layout",
                "options": [
                    {"label": "Sidebar", "description": "Navigation on the left", "preview": "│S│ content"},
                    "Topbar",
                ],
            }]}}},
            {"usage": {"status": "rejected", "windows": [
                {"name": "five_hour", "used_percent": 100.0, "resets_in_s": 600},
            ]}},
            "turn_end",
            {"auth_failed": {"message": "Failed to authenticate. API Error: 401 API key is invalid."}},
        ],
    }))
    .await;
    host.prompt(A, "Go").await;
    let init = host.until(|frame| frame["subtype"] == "init").await;
    assert_eq!(init["mcp_servers"][1]["status"], "needs-auth");
    let warning = host
        .until(|frame| frame["type"] == "rate_limit_event")
        .await;
    assert_eq!(warning["rate_limit_info"]["status"], "allowed_warning");
    let ask = host
        .until(|frame| frame["request"]["subtype"] == "can_use_tool")
        .await;
    let options = &ask["request"]["input"]["questions"][0]["options"];
    assert_eq!(options[0]["preview"], "│S│ content");
    assert_eq!(options[1]["description"], "Topbar");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "new line\n");
    let results: Vec<&Value> = host
        .frames
        .iter()
        .filter_map(|frame| frame.get("tool_use_result"))
        .collect();
    assert!(results.iter().any(|result| result["task"]["id"] == "1"));
    assert!(
        results
            .iter()
            .any(|result| result["statusChange"]["to"] == "in_progress")
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("fresh/new.md")).unwrap(),
        "fresh\n"
    );
    host.send(json!({
        "type": "control_response",
        "response": {
            "subtype": "success",
            "request_id": ask["request_id"],
            "response": {"behavior": "allow", "updatedInput": {
                "questions": ask["request"]["input"]["questions"],
                "answers": {"Which layout?": "Sidebar"},
            }},
        },
    }))
    .await;
    let reached = host
        .until(|frame| frame["type"] == "rate_limit_event")
        .await;
    assert_eq!(reached["rate_limit_info"]["status"], "rejected");
    host.until(|frame| frame["type"] == "result").await;
    host.prompt(B, "Again").await;
    let error = host
        .until(|frame| frame["is_api_error_message"] == true)
        .await;
    assert_eq!(error["error"], "authentication_failed");
    let failed = host.until(|frame| frame["type"] == "result").await;
    assert_eq!(failed["is_error"], true);
    assert_eq!(failed["api_error_status"], 401);
    assert_eq!(host.close().await, 0);
}

#[tokio::test]
async fn a_background_command_returns_at_once_and_its_job_ends_when_its_gate_opens() {
    let dir = tempfile::tempdir().unwrap();
    let gate = dir.path().join("gate");
    let mut host = Host::start(json!({"steps": [
        {"tool": {"name": "Bash", "class": "consequential", "wait_for": gate, "input": {
            "command": "scripts/watch", "description": "Watch memory", "run_in_background": true,
        }}},
        {"text": {"chunks": ["watching"]}},
        "turn_end",
    ]}))
    .await;
    host.prompt(A, "Watch it").await;
    host.until(|frame| frame["state"] == "completed" && frame["command_uuid"] == A)
        .await;
    let running = host.trace();
    assert!(
        !running.contains(&"system task_notification".to_owned()),
        "the job outlives the turn: {running:?}"
    );

    std::fs::write(&gate, "").unwrap();
    let notified = host
        .until(|frame| frame["subtype"] == "task_notification")
        .await;
    assert_eq!(notified["status"], "completed");
    let trace = host.trace();
    let launch = trace
        .iter()
        .position(|line| line == "assistant tool_use")
        .unwrap();
    assert_eq!(
        trace[launch..launch + 4],
        [
            "assistant tool_use",
            "system background_tasks_changed",
            "system task_started",
            "user tool_result",
        ]
    );
    assert_eq!(
        trace[trace.len() - 3..],
        [
            "system background_tasks_changed",
            "system task_updated",
            "system task_notification",
        ]
    );
    assert_eq!(host.close().await, 0);
}

/// A subagent's script: a word, a command, and its answer.
fn subagent(task: &str, word: &str) -> Value {
    json!({"task": task, "steps": [
        {"thinking": {"text": ""}},
        {"tool": {"name": "Bash", "class": "consequential",
                  "input": {"command": format!("echo {word}"), "description": format!("Echo {word}")},
                  "outcome": {"output": word}}},
        {"text": {"chunks": [format!("It printed {word}.")]}},
    ]})
}

/// Four subagents started in one message run as 2.1.325 runs subagents
/// their parent waits on: each call starts a task whose prompt, thinking,
/// calls and answer stream tagged with its id and its call, the four take
/// turns, and each hands its answer back as its call's result before the
/// parent goes on.
#[tokio::test]
async fn parallel_subagents_stream_their_own_conversations_and_hand_back() {
    let words = ["RED", "GREEN", "BLUE", "GOLD"];
    let mut host = Host::start(json!({"steps": [
        {"subagents": {"agents": words.iter().map(|word| subagent(&format!("Echo {word} and report it"), word)).collect::<Vec<_>>()}},
        {"text": {"chunks": ["RED GREEN BLUE GOLD"]}},
        "turn_end",
    ]}))
    .await;
    host.prompt(A, "Four subagents").await;
    let result = host.until(|frame| frame["type"] == "result").await;
    assert_eq!(result["result"], "RED GREEN BLUE GOLD");

    let frames = host.frames.clone();
    let started: Vec<&Value> = frames
        .iter()
        .filter(|frame| frame["subtype"] == "task_started")
        .collect();
    assert_eq!(started.len(), 4);
    for (task, word) in started.iter().zip(words) {
        assert_eq!(task["task_type"], "local_agent");
        assert_eq!(task["is_backgrounded"], false);
        let (id, call) = (&task["task_id"], &task["tool_use_id"]);
        let own: Vec<&Value> = frames
            .iter()
            .filter(|frame| frame["agent_id"] == *id)
            .collect();
        assert!(own.iter().all(|frame| frame["parent_tool_use_id"] == *call));
        let kinds: Vec<String> = own
            .iter()
            .map(|frame| {
                format!(
                    "{} {}",
                    frame["type"].as_str().unwrap(),
                    frame["message"]["content"][0]["type"].as_str().unwrap()
                )
            })
            .collect();
        assert_eq!(
            kinds,
            [
                "user text",
                "assistant thinking",
                "assistant tool_use",
                "user tool_result",
                "assistant text"
            ],
            "subagent {word}"
        );
        assert_eq!(own[0]["message"]["content"][0]["text"], task["prompt"]);
        let handed: Vec<&Value> = frames
            .iter()
            .filter(|frame| {
                frame["parent_tool_use_id"].is_null()
                    && frame["message"]["content"][0]["tool_use_id"] == *call
            })
            .collect();
        assert_eq!(handed.len(), 1, "one result for {word}'s call");
        assert!(
            handed[0]["message"]["content"][0]["content"]
                .to_string()
                .contains(&format!("It printed {word}."))
        );
    }
    // They take turns: every subagent has run a command before the first
    // one answers.
    let first_answer = frames
        .iter()
        .position(|frame| {
            frame["agent_id"].is_string()
                && frame["message"]["content"][0]["type"] == "text"
                && frame["type"] == "assistant"
        })
        .unwrap();
    let commands = frames[..first_answer]
        .iter()
        .filter(|frame| {
            frame["agent_id"].is_string() && frame["message"]["content"][0]["type"] == "tool_use"
        })
        .count();
    assert_eq!(commands, 4);
    let notified: Vec<&Value> = frames
        .iter()
        .filter(|frame| frame["subtype"] == "task_notification")
        .collect();
    assert_eq!(notified.len(), 4);
    assert!(notified.iter().all(|frame| frame["status"] == "completed"));
    assert!(
        !frames
            .iter()
            .any(|frame| frame["type"] == "stream_event" && frame["agent_id"].is_string()),
        "a subagent's messages are not streamed"
    );
    assert_eq!(host.close().await, 0);
}

/// A subagent's ask names the subagent.
#[tokio::test]
async fn a_subagents_ask_names_it() {
    let mut host = Host::start(json!({"steps": [
        {"subagents": {"agents": [{"task": "Touch a file", "steps": [
            {"ask": {"permission": {"class": "consequential", "input": {"command": "touch asked.txt"},
                                    "outcome": {"output": ""}}}},
            {"text": {"chunks": ["done"]}},
        ]}]}},
        {"text": {"chunks": ["DONE"]}},
        "turn_end",
    ]}))
    .await;
    host.prompt(A, "Ask from a subagent").await;
    let request = host.until(|frame| frame["type"] == "control_request").await;
    let started = host
        .frames
        .iter()
        .find(|frame| frame["subtype"] == "task_started")
        .unwrap()
        .clone();
    assert_eq!(request["request"]["subtype"], "can_use_tool");
    assert_eq!(request["request"]["agent_id"], started["task_id"]);
    host.send(json!({"type":"control_response","response":{
        "subtype":"success","request_id":request["request_id"],
        "response":{"behavior": "allow", "updatedInput": {"command": "touch asked.txt"}}}}))
        .await;
    let result = host.until(|frame| frame["type"] == "result").await;
    assert_eq!(result["result"], "DONE");
    assert_eq!(host.close().await, 0);
}

/// A subagent stopped by its task id ends as Claude ends one: the task is
/// killed and says it stopped, and the call that started it returns
/// interrupted, so the parent goes on.
#[tokio::test]
async fn a_stopped_subagent_says_so_and_its_call_returns_interrupted() {
    let dir = tempfile::tempdir().unwrap();
    let never = dir.path().join("never");
    let mut host = Host::start(json!({"steps": [
        {"subagents": {"agents": [{"task": "Sleep long", "steps": [
            {"tool": {"name": "Bash", "class": "consequential", "wait_for": never,
                      "input": {"command": "sleep 90", "description": "Sleep"}}},
            {"text": {"chunks": ["never said"]}},
        ]}]}},
        {"text": {"chunks": ["It was stopped."]}},
        "turn_end",
    ]}))
    .await;
    host.prompt(A, "Start and stop").await;
    let started = host.until(|frame| frame["subtype"] == "task_started").await;
    host.until(|frame| frame["subtype"] == "task_progress")
        .await;
    host.send(json!({"type":"control_request","request_id":"stop-1",
        "request":{"subtype":"stop_task","task_id":started["task_id"]}}))
        .await;
    let response = host
        .until(|frame| {
            frame["type"] == "control_response" && frame["response"]["request_id"] == "stop-1"
        })
        .await;
    assert_eq!(response["response"]["subtype"], "success");
    let result = host.until(|frame| frame["type"] == "result").await;
    assert_eq!(result["result"], "It was stopped.");
    let frames = &host.frames;
    let updated = frames
        .iter()
        .find(|frame| frame["subtype"] == "task_updated")
        .unwrap();
    assert_eq!(updated["patch"]["status"], "killed");
    let notified = frames
        .iter()
        .find(|frame| frame["subtype"] == "task_notification")
        .unwrap();
    assert_eq!(notified["status"], "stopped");
    let returned = frames
        .iter()
        .find(|frame| {
            frame["parent_tool_use_id"].is_null()
                && frame["message"]["content"][0]["tool_use_id"] == started["tool_use_id"]
        })
        .unwrap();
    assert_eq!(returned["message"]["content"][0]["is_error"], true);
    assert!(
        !frames
            .iter()
            .any(|frame| frame["message"]["content"][0]["text"] == "never said")
    );
    assert_eq!(host.close().await, 0);
}

/// A subagent started in the background runs as 2.1.325 runs one: its
/// call returns "launched" at once and the turn that started it ends while
/// it works; its steps play once no turn runs, its end notifies, and that
/// notification starts a turn of Claude's own, with no prompt replayed,
/// which plays the script's next steps.
#[tokio::test]
async fn a_background_subagent_outlives_its_turn_and_its_end_starts_one() {
    let dir = tempfile::tempdir().unwrap();
    let gate = dir.path().join("go");
    let mut host = Host::start(json!({"steps": [
        {"subagents": {"agents": [{"task": "Count the files", "background": true, "steps": [
            {"wait_for": {"path": gate}},
            {"text": {"chunks": ["There are 3 files."]}},
        ]}]}},
        {"text": {"chunks": ["It is counting."]}},
        "turn_end",
        {"text": {"chunks": ["Done: 3 files."]}},
        "turn_end",
    ]}))
    .await;
    host.prompt(A, "Count with a subagent").await;
    let result = host.until(|frame| frame["type"] == "result").await;
    assert_eq!(result["result"], "It is counting.");
    let frames = host.frames.clone();
    let started = frames
        .iter()
        .find(|frame| frame["subtype"] == "task_started")
        .unwrap();
    assert_eq!(started["is_backgrounded"], true);
    let launched = frames
        .iter()
        .find(|frame| {
            frame["parent_tool_use_id"].is_null()
                && frame["message"]["content"][0]["tool_use_id"] == started["tool_use_id"]
        })
        .unwrap();
    assert_eq!(launched["tool_use_result"]["status"], "async_launched");
    assert!(
        !frames
            .iter()
            .any(|frame| frame["subtype"] == "task_notification"),
        "the subagent outlives the turn"
    );

    std::fs::write(&gate, "").unwrap();
    let notified = host
        .until(|frame| frame["subtype"] == "task_notification")
        .await;
    assert_eq!(notified["status"], "completed");
    assert_eq!(notified["summary"], "There are 3 files.");
    let result = host.until(|frame| frame["type"] == "result").await;
    assert_eq!(result["result"], "Done: 3 files.");
    let at = host
        .frames
        .iter()
        .position(|frame| frame["subtype"] == "task_notification")
        .unwrap();
    let follow_up: Vec<String> = host.frames[at + 1..]
        .iter()
        .filter(|frame| frame["type"] != "stream_event")
        .map(|frame| {
            format!(
                "{} {}",
                frame["type"].as_str().unwrap(),
                frame["subtype"].as_str().unwrap_or_default()
            )
        })
        .collect();
    assert_eq!(
        follow_up,
        ["system init", "assistant ", "result success"],
        "no prompt is replayed into the follow-up turn"
    );
    assert_eq!(host.close().await, 0);
}

/// A subagent stopped while it waits on the host, on an open ask or in a
/// pause, stops waiting: an open ask is withdrawn, and its call returns
/// interrupted, so the parent goes on.
#[tokio::test]
async fn a_subagent_stopped_while_it_waits_stops_waiting() {
    let asking = json!({"ask": {"permission": {"class": "consequential",
        "input": {"command": "touch asked.txt"}, "outcome": {"output": ""}}}});
    let pausing = json!({"pause": {"ms": 60_000}});
    for (waits, waiting) in [("ask", asking), ("pause", pausing)] {
        let mut host = Host::start(json!({"steps": [
            {"subagents": {"agents": [{"task": "Wait", "steps": [
                waiting,
                {"text": {"chunks": ["never said"]}},
            ]}]}},
            {"text": {"chunks": ["It was stopped."]}},
            "turn_end",
        ]}))
        .await;
        host.prompt(A, "Start and stop").await;
        let started = host.until(|frame| frame["subtype"] == "task_started").await;
        let asked = if waits == "ask" {
            let request = host.until(|frame| frame["type"] == "control_request").await;
            Some(request["request_id"].clone())
        } else {
            // The subagent's prompt is written before its first step.
            host.until(|frame| frame["agent_id"] == started["task_id"])
                .await;
            None
        };
        host.send(json!({"type":"control_request","request_id":"stop-1",
            "request":{"subtype":"stop_task","task_id":started["task_id"]}}))
            .await;
        let result = host.until(|frame| frame["type"] == "result").await;
        assert_eq!(
            result["result"], "It was stopped.",
            "stopped in its {waits}"
        );
        let frames = &host.frames;
        if let Some(asked) = asked {
            assert!(
                frames
                    .iter()
                    .any(|frame| frame["type"] == "control_cancel_request"
                        && frame["request_id"] == asked),
                "the stopped subagent's ask is withdrawn"
            );
        }
        let returned = frames
            .iter()
            .find(|frame| {
                frame["parent_tool_use_id"].is_null()
                    && frame["message"]["content"][0]["tool_use_id"] == started["tool_use_id"]
            })
            .unwrap();
        assert_eq!(
            returned["message"]["content"][0]["is_error"], true,
            "{waits}"
        );
        assert!(
            !frames
                .iter()
                .any(|frame| frame["message"]["content"][0]["text"] == "never said"),
            "{waits}"
        );
        assert_eq!(host.close().await, 0);
    }
}

/// An interrupt ends the parent's turn and spares a subagent it started in
/// the background, as Claude spares one for a host that declares it stops
/// each task itself (amux's `perTaskStopAffordance`): the subagent's steps
/// play on once the turn is over, each call it makes returns its result,
/// and its end notifies as completed.
#[tokio::test]
async fn a_background_subagent_plays_on_whole_after_its_parents_turn_is_interrupted() {
    let dir = tempfile::tempdir().unwrap();
    let never = dir.path().join("never");
    let mut host = Host::start(json!({"steps": [
        {"subagents": {"agents": [{"task": "Read the file", "background": true, "steps": [
            {"tool": {"class": "exploration", "outcome": {"output": "read"}}},
            {"text": {"chunks": ["It says hello."]}},
        ]}]}},
        {"wait_for": {"path": never}},
        "turn_end",
    ]}))
    .await;
    host.prompt(A, "Read with a subagent").await;
    let started = host.until(|frame| frame["subtype"] == "task_started").await;
    host.until(|frame| {
        frame["parent_tool_use_id"].is_null()
            && frame["message"]["content"][0]["tool_use_id"] == started["tool_use_id"]
    })
    .await;
    host.send(
        json!({"type":"control_request","request_id":"req_1","request":{"subtype":"interrupt"}}),
    )
    .await;
    let result = host.until(|frame| frame["type"] == "result").await;
    assert_eq!(result["subtype"], "error_during_execution");
    let notified = host
        .until(|frame| frame["subtype"] == "task_notification")
        .await;
    assert_eq!(notified["status"], "completed");
    assert_eq!(notified["summary"], "It says hello.");
    let call = host
        .frames
        .iter()
        .find_map(|frame| {
            let block = &frame["message"]["content"][0];
            (frame["type"] == "assistant"
                && block["type"] == "tool_use"
                && block["name"] != "Agent")
                .then(|| block["id"].clone())
        })
        .expect("the subagent's call");
    assert!(
        host.frames
            .iter()
            .any(|frame| frame["message"]["content"][0]["tool_use_id"] == call),
        "the subagent's call returns no result: {:#?}",
        host.trace()
    );
    assert_eq!(host.close().await, 0);
}
