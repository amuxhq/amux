//! An agent's record, built from its facts ring: the interpreter's
//! conclusions and the checks its steps broke, with nothing that was said.

use interpret::claude_sdk::ClaudeSdk;
use interpret::codex::Codex;
use interpret::ring::Entry;
use interpret::{Channel, Event, Fact, Interpreter};
use serde_json::{Value, json};
use wire::AgentSpec;

const MARKER: &str = "PLANTEDrecordmarker0007";

fn line(event: &Event) -> Vec<u8> {
    let mut line = serde_json::to_vec(&Entry::of(event)).unwrap();
    line.push(b'\n');
    line
}

fn fact(value: Value) -> Event {
    Event::Fact(Fact {
        channel: Channel::Stream,
        payload: serde_json::to_vec(&value).unwrap(),
    })
}

#[test]
fn a_record_keeps_the_sessions_shape_and_its_checks_and_no_words() {
    let spec = AgentSpec {
        agent_id: b"agent".to_vec(),
        kind: wire::Kind::ClaudeSdk as i32,
        incarnation: 1,
        provider_command: format!("/opt/{MARKER}/claude"),
        provider_args: vec![
            "--model".into(),
            format!("{MARKER}-model"),
            format!("--settings={MARKER}"),
        ],
        ..AgentSpec::default()
    };
    let (state, _) = ClaudeSdk::initial(&spec);
    let checkpoint = interpret::encode_checkpoint(&state);
    let stream = |event: Value| fact(json!({"type": "stream_event", "event": event}));
    let events = [
        fact(
            json!({"type": "system", "subtype": "init", "session_id": "s",
            "claude_code_version": "2.1.312", "cwd": format!("/home/{MARKER}")}),
        ),
        fact(json!({"type": "assistant", "session_id": "s", "message": {
            "id": "msg_1", "role": "assistant", "content": [
                {"type": "tool_use", "id": "toolu_1", "name": "Read",
                 "input": {"file_path": format!("/home/{MARKER}.txt"),
                    // A key that is the data's, shaped like a field's.
                    MARKER: {MARKER: true}}}]}})),
        fact(
            json!({"type": "user", "session_id": "s", "message": {"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": "toolu_0", "content": MARKER}]}}),
        ),
        stream(
            json!({"type": "message_start", "message": {"id": "msg_2", "role": "assistant", "content": []}}),
        ),
        stream(
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
        ),
        stream(
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": MARKER}}),
        ),
        Event::ProviderExit { code: Some(1) },
    ];
    let segment: Vec<u8> = events.iter().flat_map(line).collect();
    let record = interpret::record::record::<ClaudeSdk>(
        &spec,
        [3; 16],
        json!({"mode": "running"}),
        [(checkpoint, segment)],
    );
    let bytes = serde_json::to_vec(&record).unwrap();
    let text = String::from_utf8_lossy(&bytes);
    assert!(!text.contains(MARKER), "{text}");
    let record: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(record["settings"]["provider"], "claude");
    assert_eq!(record["settings"]["version"], "2.1.312");
    assert_eq!(
        record["settings"]["flags"],
        json!(["--model", "--settings"])
    );
    let events = record["segments"][0]["events"].as_array().unwrap();
    assert_eq!(events.len(), 7, "{text}");
    let tools = &events[1]["event"]["tools"][0];
    assert_eq!(tools["name"], "Read", "{text}");
    let keys = tools["keys"].as_array().unwrap();
    assert_eq!(keys.len(), 2, "{text}");
    assert!(keys.contains(&json!("file_path")), "{text}");
    // A result for a call never made breaks its check, and the record
    // keeps which step broke it.
    let broken: Vec<&Value> = events.iter().map(|fed| &fed["broken"]).collect();
    assert_eq!(
        broken,
        [
            &Value::Null,
            &Value::Null,
            &json!(interpret::checks::TOOL_RESULT_WITHOUT_CALL.id),
            &Value::Null,
            &Value::Null,
            &Value::Null,
            &Value::Null,
        ],
        "{text}"
    );
    // The provider died halfway through a message, which ends as far as
    // it streamed.
    let closed = &events[6]["step"]["items"];
    assert!(
        closed
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["arm"] == "message" && item["op"] == "close"),
        "{text}"
    );
}

#[test]
fn a_codex_record_keeps_no_home_folder_and_no_key_that_is_the_datas() {
    let spec = AgentSpec {
        agent_id: b"agent".to_vec(),
        kind: wire::Kind::Codex as i32,
        incarnation: 1,
        ..AgentSpec::default()
    };
    let (state, _) = Codex::initial(&spec);
    let checkpoint = interpret::encode_checkpoint(&state);
    let rpc = |value: Value| {
        Event::Fact(Fact {
            channel: Channel::Rpc,
            payload: serde_json::to_vec(&value).unwrap(),
        })
    };
    let item = |item: Value| {
        rpc(json!({"method": "item/started",
            "params": {"threadId": "t", "turnId": "u", "item": item}}))
    };
    let events = [
        // The person's shell, found in their home.
        item(json!({"type": "commandExecution", "id": "c1",
            "command": format!("/Users/{MARKER}/.nix-profile/bin/zsh -lc 'ls -la'"),
            "cwd": "/w", "status": "inProgress",
            "commandActions": [{"type": "unknown", "command": "ls -la", MARKER: 1}]})),
        // Changes keyed by the files they touch, and a change carrying a
        // key of its own.
        item(
            json!({"type": "fileChange", "id": "f1", "status": "inProgress",
            "changes": {MARKER: {"type": "add", "content": "x"}}}),
        ),
        item(
            json!({"type": "fileChange", "id": "f2", "status": "inProgress",
            "changes": [{"path": "/w/a", "kind": {"type": "add"}, "diff": "", MARKER: "x"}]}),
        ),
    ];
    let segment: Vec<u8> = events.iter().flat_map(line).collect();
    let record = interpret::record::record::<Codex>(
        &spec,
        [3; 16],
        json!({"mode": "running"}),
        [(checkpoint, segment)],
    );
    let text = record.to_string();
    assert!(!text.contains(MARKER), "{text}");
    assert!(!text.contains("nix-profile"), "{text}");
    assert!(!text.contains("Users"), "{text}");
    let shapes = &record["segments"][0]["events"];
    assert_eq!(
        shapes[0]["event"]["shape"]["params"]["item"]["command"]["=text"][0], "=zsh -lc '",
        "{text}"
    );
    // The change's own fields stay named; the key that is not one does not.
    let change = shapes[2]["event"]["shape"]["params"]["item"]["changes"][0]
        .as_object()
        .unwrap();
    let mut names: Vec<&str> = change.keys().map(String::as_str).collect();
    names.retain(|name| !name.starts_with('@'));
    assert_eq!(names, ["diff", "kind", "path"], "{text}");
    assert_eq!(change.len(), 4, "{text}");
}

#[test]
fn a_checkpoint_this_build_cannot_read_runs_no_event() {
    let spec = AgentSpec::default();
    let record = interpret::record::record::<ClaudeSdk>(
        &spec,
        [3; 16],
        Value::Null,
        [(b"not a checkpoint".to_vec(), line(&Event::DaemonLost))],
    );
    assert_eq!(
        record["segments"],
        json!([{"checkpoint": null, "events": []}])
    );
}
