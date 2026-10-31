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

    async fn prompt(&mut self, uuid: &str, text: &str, priority: Option<&str>) {
        let mut frame = json!({
            "type": "user",
            "uuid": uuid,
            "session_id": "",
            "parent_tool_use_id": null,
            "message": { "role": "user", "content": text },
        });
        if let Some(priority) = priority {
            frame["priority"] = json!(priority);
        }
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
const C: &str = "cccccccc-0000-4000-8000-000000000003";

#[tokio::test]
async fn an_idle_prompt_is_echoed_by_its_uuid_and_closed_by_its_lifecycle() {
    let mut host = Host::start(json!({"steps": [
        {"text": {"chunks": ["Hel", "lo"]}},
        "turn_end",
    ]}))
    .await;
    host.prompt(A, "Say hello", None).await;
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
async fn a_default_message_mid_turn_folds_at_the_next_tool_result_and_later_waits() {
    let dir = tempfile::tempdir().unwrap();
    let gate = dir.path().join("gate");
    let mut host = Host::start(json!({"steps": [
        {"wait_for": {"path": gate}},
        {"tool": {"class": "consequential", "outcome": {"output": "ran"}}},
        {"text": {"chunks": ["done"]}},
        "turn_end",
        {"text": {"chunks": ["later"]}},
        "turn_end",
    ]}))
    .await;
    host.prompt(A, "Run it", None).await;
    host.until(|frame| frame["isReplay"] == true).await;
    host.prompt(B, "Also this", None).await;
    host.prompt(C, "Afterwards", Some("later")).await;
    host.until(|frame| frame["state"] == "queued" && frame["command_uuid"] == C)
        .await;
    std::fs::write(&gate, "").unwrap();
    host.until(|frame| frame["state"] == "completed" && frame["command_uuid"] == A)
        .await;
    host.until(|frame| frame["state"] == "completed" && frame["command_uuid"] == C)
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
            "lifecycle cccccccc queued",
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
            "lifecycle cccccccc started",
            "system init",
            "replay cccccccc",
            "stream message_start",
            "stream content_block_start",
            "stream content_block_delta",
            "assistant text",
            "stream content_block_stop",
            "result success",
            "lifecycle cccccccc completed",
        ]
    );
    assert_eq!(host.close().await, 0);
}

#[tokio::test]
async fn interrupt_and_now_cut_the_running_turn_short() {
    let dir = tempfile::tempdir().unwrap();
    let gate = dir.path().join("never");
    let mut host = Host::start(json!({"steps": [
        {"wait_for": {"path": gate}},
        {"text": {"chunks": ["never said"]}},
        "turn_end",
        {"wait_for": {"path": gate}},
        "turn_end",
        {"text": {"chunks": ["preempted"]}},
        "turn_end",
    ]}))
    .await;
    host.prompt(A, "Wait", None).await;
    host.until(|frame| frame["isReplay"] == true).await;
    host.send(
        json!({"type":"control_request","request_id":"req_1","request":{"subtype":"interrupt"}}),
    )
    .await;
    let result = host.until(|frame| frame["type"] == "result").await;
    assert_eq!(result["subtype"], "error_during_execution");
    host.prompt(B, "Wait again", None).await;
    host.until(|frame| frame["isReplay"] == true && frame["uuid"] == B)
        .await;
    host.prompt(C, "Now instead", Some("now")).await;
    // A preempted turn ends successfully with a terminal reason saying it
    // was cut, without an interruption marker, and its command is
    // cancelled, as Claude 2.1.292 was recorded doing.
    let cut = host.until(|frame| frame["type"] == "result").await;
    assert_eq!(cut["subtype"], "success");
    assert_eq!(cut["is_error"], false);
    assert_eq!(cut["terminal_reason"], "aborted_streaming");
    let next = host.until(|frame| frame["type"] == "result").await;
    assert_eq!(next["result"], "preempted");
    let trace = host.trace();
    assert!(!trace.iter().any(|line| line.contains("never")));
    assert!(trace.contains(&"lifecycle bbbbbbbb cancelled".to_owned()));
    let markers = host
        .frames
        .iter()
        .filter(|frame| {
            frame["message"]["content"][0]["text"]
                .as_str()
                .is_some_and(|t| t.starts_with("[Request"))
        })
        .count();
    assert_eq!(markers, 1, "only the interrupt writes a marker");
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
    host.prompt(A, "Ask everything", None).await;
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
    host.prompt(A, "Plan it", None).await;
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
    host.prompt(A, "Ask", None).await;
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
    host.prompt(A, "Work", None).await;
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
    host.prompt(A, "Leave", None).await;
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
    host.prompt(A, "Go", None).await;
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
    host.prompt(B, "Again", None).await;
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
    host.prompt(A, "Watch it", None).await;
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
