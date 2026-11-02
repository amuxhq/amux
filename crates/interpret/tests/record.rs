//! An agent's record, built from its facts ring: the interpreter's
//! conclusions and the checks its steps broke, with nothing that was said.

use interpret::claude_sdk::ClaudeSdk;
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
                 "input": {"file_path": format!("/home/{MARKER}.txt")}}]}})),
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
    assert_eq!(events.len(), 6, "{text}");
    assert_eq!(
        events[1]["event"]["tools"],
        json!([{"name": "Read", "keys": ["file_path"]}]),
        "{text}"
    );
    // The provider died halfway through a message: the replay's last step
    // breaks the check the live one did.
    let broken: Vec<&Value> = events.iter().map(|fed| &fed["broken"]).collect();
    assert_eq!(
        broken.last(),
        Some(&&json!(interpret::checks::EXIT_LEFT_OPEN.id)),
        "{text}"
    );
    assert!(broken[..5].iter().all(|broken| broken.is_null()), "{text}");
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
