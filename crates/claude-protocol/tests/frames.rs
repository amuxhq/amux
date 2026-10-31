//! The initialize exchange and the tolerance rules, on recorded and made-up
//! frames.

use std::path::Path;

use claude_protocol::stream::control::{ControlOutcome, InitializeRequestBody};
use claude_protocol::stream::{
    self, ContentBlock, ControlRequest, ControlRequestBody, ControlResponse, Extensions,
    InitializationResult, Input, Message, MessageContent, MessageParam, Output, PermissionMode,
    PermissionResult, PermissionUpdate, PermissionUpdateDestination, ResultMessage, Role,
    UserInput,
};
use serde_json::Value;

/// The lines of one recording that contain every needle, in order.
fn recorded(spec: &str, stdout: bool, needles: &[&str]) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../claude-specs/fixtures/live/sdk")
        .join(spec)
        .join("io.jsonl");
    std::fs::read_to_string(&path)
        .expect("recording")
        .lines()
        .map(|record| serde_json::from_str::<Value>(record).expect("record"))
        .filter(|record| (record["dir"] == "stdout") == stdout)
        .map(|record| record["line"].as_str().expect("line").to_owned())
        .find(|line| needles.iter().all(|needle| line.contains(needle)))
        .unwrap_or_else(|| panic!("{spec}: no line with {needles:?}"))
}

#[test]
fn the_initialize_exchange_reads_both_ways() {
    let sent = recorded("controls", false, &[r#""subtype":"initialize""#]);
    let Input::ControlRequest(request) = stream::strict_input(sent.as_bytes()).expect("known")
    else {
        panic!("not a control request");
    };
    assert!(matches!(request.request, ControlRequestBody::Initialize(_)));
    let answered = recorded(
        "controls",
        true,
        &[
            &format!(r#""request_id":"{}""#, request.request_id),
            "commands",
        ],
    );
    let Output::ControlResponse(response) = stream::strict(answered.as_bytes()).expect("known")
    else {
        panic!("not a control response");
    };
    assert_eq!(response.response.subtype, ControlOutcome::Success);
    let result: InitializationResult = response.result().expect("a payload").expect("decodes");
    assert!(!result.commands.is_empty());
}

#[test]
fn what_amux_writes_encodes_with_sorted_keys() {
    let initialize = Input::ControlRequest(ControlRequest::new(
        "req_0",
        ControlRequestBody::Initialize(InitializeRequestBody::default()),
    ));
    assert_eq!(
        stream::encode(&initialize),
        br#"{"request":{"subtype":"initialize"},"request_id":"req_0","type":"control_request"}"#
    );

    let prompt = Input::User(UserInput {
        message: MessageParam {
            role: Role::User,
            content: MessageContent::Text("hi".into()),
            extensions: Extensions::new(),
        },
        parent_tool_use_id: None,
        session_id: "s".into(),
        uuid: None,
        extensions: Extensions::new(),
    });
    assert_eq!(
        stream::encode(&prompt),
        br#"{"message":{"content":"hi","role":"user"},"parent_tool_use_id":null,"session_id":"s","type":"user"}"#
    );

    let mode = Input::ControlRequest(ControlRequest::new(
        "req_1",
        ControlRequestBody::SetPermissionMode(stream::control::SetPermissionModeRequest {
            mode: PermissionMode::AcceptEdits,
            extensions: Extensions::new(),
        }),
    ));
    assert_eq!(
        stream::encode(&mode),
        br#"{"request":{"mode":"acceptEdits","subtype":"set_permission_mode"},"request_id":"req_1","type":"control_request"}"#
    );

    let refusal = Input::ControlResponse(ControlResponse::error("req_9", "no"));
    assert_eq!(
        stream::encode(&refusal),
        br#"{"response":{"error":"no","request_id":"req_9","subtype":"error"},"type":"control_response"}"#
    );
}

#[test]
fn amux_answers_write_only_the_keys_amux_sets() {
    fn answer(payload: &impl serde::Serialize) -> String {
        let response = ControlResponse::success("req_1", payload);
        String::from_utf8(stream::encode(&Input::ControlResponse(response))).expect("UTF-8")
    }
    let allow = PermissionResult::Allow {
        updated_input: Some(serde_json::json!({"command": "ls"})),
        updated_permissions: None,
        tool_use_id: None,
    };
    assert_eq!(
        answer(&allow),
        r#"{"response":{"request_id":"req_1","response":{"behavior":"allow","updatedInput":{"command":"ls"}},"subtype":"success"},"type":"control_response"}"#
    );
    let plan = PermissionResult::Allow {
        updated_input: Some(serde_json::json!({})),
        updated_permissions: Some(vec![PermissionUpdate::SetMode {
            mode: PermissionMode::AcceptEdits,
            destination: PermissionUpdateDestination::Session,
        }]),
        tool_use_id: None,
    };
    assert_eq!(
        answer(&plan),
        r#"{"response":{"request_id":"req_1","response":{"behavior":"allow","updatedInput":{},"updatedPermissions":[{"destination":"session","mode":"acceptEdits","type":"setMode"}]},"subtype":"success"},"type":"control_response"}"#
    );
    let deny = PermissionResult::Deny {
        message: "no".into(),
        interrupt: Some(false),
        tool_use_id: None,
    };
    assert_eq!(
        answer(&deny),
        r#"{"response":{"request_id":"req_1","response":{"behavior":"deny","interrupt":false,"message":"no"},"subtype":"success"},"type":"control_response"}"#
    );
}

#[test]
fn unknown_fields_and_blocks_are_kept_and_refused_strictly() {
    let line = br#"{"type":"assistant","uuid":"u","session_id":"s","parent_tool_use_id":null,"mood":"calm","message":{"id":"m","type":"message","role":"assistant","model":"x","usage":{"input_tokens":1,"output_tokens":1},"content":[{"type":"hologram","shape":"cube"},{"type":"text","text":"hi"}]}}"#;
    let Output::Message(Message::Assistant(assistant)) = stream::decode(line).expect("decodes")
    else {
        panic!("not an assistant message");
    };
    assert_eq!(assistant.extensions["mood"], "calm");
    assert!(matches!(
        assistant.message.content[0],
        ContentBlock::Unknown(_)
    ));
    let drift = stream::strict(line).expect_err("refused");
    assert!(drift.to_string().contains("hologram"), "{drift}");
}

#[test]
fn a_line_that_is_not_an_object_is_the_only_decode_error() {
    assert!(stream::decode(b"[1]").is_err());
    assert!(stream::decode(b"nope").is_err());
    assert!(matches!(
        stream::decode(br#"{"no":"type"}"#),
        Ok(Output::Unknown(_))
    ));
}

#[test]
fn a_field_claude_always_writes_is_filled_in_and_refused_strictly() {
    // A result without `session_id` or `modelUsage`, as amux's own test
    // inputs write it.
    let line = br#"{"type":"result","subtype":"success","uuid":"r","duration_ms":10,"duration_api_ms":8,"is_error":false,"num_turns":1,"stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":1},"permission_denials":[],"result":"done"}"#;
    let Output::Message(Message::Result(ResultMessage::Success(success))) =
        stream::decode(line).expect("decodes")
    else {
        panic!("not a successful result");
    };
    assert!(success.common.model_usage.is_none());
    assert_eq!(success.common.session_id, "");
    assert_eq!(success.result, "done");

    let drift = stream::strict(line).expect_err("refused");
    let reasons = drift.to_string();
    assert!(reasons.contains("missing field `modelUsage`"), "{reasons}");
    assert!(reasons.contains("missing field `session_id`"), "{reasons}");
}

#[test]
fn a_transcript_row_or_hook_missing_what_claude_always_writes_is_filled_in_and_refused_strictly() {
    use claude_protocol::{hooks, transcript};

    // A user row and a hook payload as amux's own test inputs write them:
    // no promptId, version, cwd and the rest of the envelope.
    let row = br#"{"type":"user","uuid":"u1","timestamp":"2027-01-15T08:00:00.000Z","message":{"role":"user","content":"go"}}"#;
    let transcript::Row::User(user) = transcript::decode(row).expect("decodes") else {
        panic!("not a user row");
    };
    assert_eq!(user.envelope.uuid, "u1");
    assert_eq!(user.envelope.version, "");
    let reasons = transcript::strict(row).expect_err("refused").to_string();
    assert!(reasons.contains("missing field `promptId`"), "{reasons}");
    assert!(reasons.contains("missing field `version`"), "{reasons}");

    let hook = br#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{},"tool_use_id":"t1"}"#;
    let hooks::Payload::PreToolUse(call) = hooks::decode(hook).expect("decodes") else {
        panic!("not a PreToolUse payload");
    };
    assert_eq!(call.tool_use_id, "t1");
    let reasons = hooks::strict(hook).expect_err("refused").to_string();
    assert!(
        reasons.contains("missing field `transcript_path`"),
        "{reasons}"
    );
}
