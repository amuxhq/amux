//! An agent's record, built from its facts ring: the interpreter's
//! conclusions and the checks its steps broke, with nothing that was said.

use interpret::claude_pty::ClaudePty;
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
            json!({"type": "system", "subtype": "init", "session_id": "s", "uuid": "u",
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

/// An item closed just before the facts ring rotates and opened again just
/// after breaks its check live, and the record's replay from the second
/// segment's checkpoint breaks it at the same event, as the checkpoint
/// carries the keys the checks remember closing.
#[test]
fn an_item_reopened_across_a_checkpoint_breaks_its_check_in_the_record() {
    let spec = AgentSpec {
        agent_id: b"agent".to_vec(),
        kind: wire::Kind::Codex as i32,
        incarnation: 1,
        ..AgentSpec::default()
    };
    let item = |method: &str, text: &str| {
        Event::Fact(Fact {
            channel: Channel::Rpc,
            payload: serde_json::to_vec(&json!({"method": method, "params": {
                "threadId": "t", "turnId": "u",
                "item": {"type": "agentMessage", "id": "msg_1", "text": text}}}))
            .unwrap(),
        })
    };
    let (mut state, _) = Codex::initial(&spec);
    let first_checkpoint = interpret::encode_checkpoint(&state);
    let before = [item("item/started", ""), item("item/completed", "done")];
    for event in &before {
        assert!(Codex::step(&mut state, event.clone()).broken.is_none());
    }
    let rotated = interpret::encode_checkpoint(&state);
    let after = item("item/started", "");
    let live = Codex::step(&mut state, after.clone()).broken;
    assert_eq!(
        live.map(|broken| broken.invariant.id),
        Some(interpret::checks::ITEM_BEFORE_OPEN.id)
    );
    let record = interpret::record::record::<Codex>(
        &spec,
        [3; 16],
        Value::Null,
        [
            (first_checkpoint, before.iter().flat_map(line).collect()),
            (rotated, line(&after)),
        ],
    );
    assert_eq!(
        record["segments"][1]["events"][0]["broken"],
        interpret::checks::ITEM_BEFORE_OPEN.id,
        "{record}"
    );
}

fn codex_item(method: &str, id: &str, text: &str) -> Event {
    Event::Fact(Fact {
        channel: Channel::Rpc,
        payload: serde_json::to_vec(&json!({"method": method, "params": {
            "threadId": "t", "turnId": "u",
            "item": {"type": "agentMessage", "id": id, "text": text}}}))
        .unwrap(),
    })
}

/// A record too long for a report keeps its newest events, the one that
/// broke a check among them, and says how many older ones it left out.
/// The segment it starts in starts from the interpreter's state at its
/// first kept event.
#[test]
fn a_long_record_keeps_its_newest_events_and_counts_the_rest() {
    let spec = AgentSpec {
        agent_id: b"agent".to_vec(),
        kind: wire::Kind::Codex as i32,
        incarnation: 1,
        ..AgentSpec::default()
    };
    let filler = |from: usize| -> Vec<Event> {
        (from..from + 10)
            .flat_map(|n| {
                let id = format!("msg_{n}");
                [
                    codex_item("item/started", &id, ""),
                    codex_item("item/completed", &id, "done"),
                ]
            })
            .collect()
    };
    let (mut state, _) = Codex::initial(&spec);
    let mut segments = Vec::new();
    let mut total = 0;
    for events in [filler(0), {
        let mut events = filler(10);
        events.extend([
            codex_item("item/started", "open", ""),
            codex_item("item/started", "again", ""),
            codex_item("item/completed", "again", "done"),
            codex_item("item/started", "again", ""),
        ]);
        events
    }] {
        let checkpoint = interpret::encode_checkpoint(&state);
        for event in &events {
            Codex::step(&mut state, event.clone());
        }
        total += events.len();
        segments.push((
            checkpoint,
            events.iter().flat_map(line).collect::<Vec<u8>>(),
        ));
    }
    let kept = |record: &Value| -> usize {
        record["segments"]
            .as_array()
            .unwrap()
            .iter()
            .map(|segment| segment["events"].as_array().unwrap().len())
            .sum()
    };
    // Room for the three newest events alone.
    let record = interpret::record::within::<Codex>(
        &spec,
        [3; 16],
        Value::Null,
        segments.clone(),
        |record| kept(record) <= 3,
    );
    assert_eq!(kept(&record), 3, "{record}");
    assert_eq!(record["events_left_out"], total - 3, "{record}");
    let segments_kept = record["segments"].as_array().unwrap();
    assert_eq!(segments_kept.len(), 1, "the older segment goes whole");
    // It starts after the item left open was opened, as the state there
    // stood.
    assert_eq!(
        segments_kept[0]["checkpoint"]["open_items"]
            .as_array()
            .map(Vec::len),
        Some(1),
        "{record}"
    );
    let events = segments_kept[0]["events"].as_array().unwrap();
    assert_eq!(
        events[2]["broken"],
        interpret::checks::ITEM_BEFORE_OPEN.id,
        "{record}"
    );
    // A record that fits is whole.
    let whole = interpret::record::within::<Codex>(&spec, [3; 16], Value::Null, segments, |_| true);
    assert_eq!(kept(&whole), total);
    assert!(whole.get("events_left_out").is_none());
}

/// Digits after a provider's fixed phrase are as much anyone's as words:
/// a card number after `user_note: ` keeps only the phrase, in a fact and
/// in a prompt's text.
#[test]
fn only_the_phrase_of_a_marked_text_is_kept_whatever_follows_it() {
    const CARD: &str = "4111 1111 1111 1111";
    let spec = AgentSpec {
        agent_id: b"agent".to_vec(),
        kind: wire::Kind::ClaudeSdk as i32,
        incarnation: 1,
        ..AgentSpec::default()
    };
    let (state, _) = ClaudeSdk::initial(&spec);
    let prompt = wire::ClaudeSdkInput {
        of: Some(wire::claude_sdk_input::Of::Prompt(wire::PromptInput {
            text: format!("user_note: {CARD}"),
            attachments: Vec::new(),
        })),
    };
    let events = [
        fact(
            json!({"type": "user", "session_id": "s", "message": {"role": "user",
            "content": [{"type": "text", "text": format!("user_note: {CARD}")}]}}),
        ),
        Event::Input(wire::Input {
            input_id: b"p1".to_vec(),
            kind: wire::Kind::ClaudeSdk as i32,
            body: prost::Message::encode_to_vec(&prompt),
        }),
    ];
    let record = interpret::record::record::<ClaudeSdk>(
        &spec,
        [3; 16],
        Value::Null,
        [(
            interpret::encode_checkpoint(&state),
            events.iter().flat_map(line).collect(),
        )],
    );
    let text = record.to_string();
    for digits in [CARD, "1111 1111"] {
        assert!(!text.contains(digits), "{text}");
    }
    let shapes = &record["segments"][0]["events"];
    let kept = shapes[0]["event"]["shape"]["message"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(kept.starts_with("@user_note: @"), "{kept}");
    let typed = shapes[1]["event"]["shape"]["prompt"]["text"]
        .as_str()
        .unwrap();
    assert!(typed.starts_with("@user_note: @"), "{typed}");
    // The count Codex's interpreter reads off a reconnect stays.
    let skeleton = interpret::skeleton::Skeleton { salt: [3; 16] };
    assert!(
        skeleton
            .text("Reconnecting... 2/5")
            .ends_with("=Reconnecting... 2/5")
    );
    assert!(
        skeleton
            .text(&format!("Reconnecting... {CARD}"))
            .starts_with("@Reconnecting... @")
    );
}

/// A model named the way a cloud or a gateway names it, a permission
/// outside the providers' own and a wrapper the person launches the
/// provider through are the person's, and aliased wherever the record
/// would keep a setting; a model named as a provider names its own is
/// kept.
#[test]
fn a_record_keeps_a_setting_only_in_the_providers_own_form() {
    let arn = format!("arn:aws:bedrock:us-east-1:123456789012:inference-profile/{MARKER}");
    let spec = AgentSpec {
        agent_id: b"agent".to_vec(),
        kind: wire::Kind::ClaudeSdk as i32,
        incarnation: 1,
        provider_command: format!("/opt/bin/{MARKER}-claude-wrapper"),
        config: Some(wire::EffectiveConfig {
            model: Some(arn.clone()),
            permission: Some(format!("{MARKER}Permission")),
            mode: Some("plan".into()),
            effort: Some("high".into()),
            ..wire::EffectiveConfig::default()
        }),
        ..AgentSpec::default()
    };
    let (state, _) = ClaudeSdk::initial(&spec);
    let set_model = |model: &str| {
        Event::Input(wire::Input {
            input_id: model.as_bytes().to_vec(),
            kind: wire::Kind::ClaudeSdk as i32,
            body: prost::Message::encode_to_vec(&wire::ClaudeSdkInput {
                of: Some(wire::claude_sdk_input::Of::Model(wire::SetModel {
                    model: Some(model.to_owned()),
                })),
            }),
        })
    };
    let events = [
        fact(
            json!({"type": "system", "subtype": "init", "session_id": "s", "uuid": "u",
            "claude_code_version": "2.1.312", "model": arn}),
        ),
        fact(
            json!({"type": "system", "subtype": "init", "session_id": "s", "uuid": "u",
            "claude_code_version": "2.1.312", "model": "claude-opus-5-5[1m]"}),
        ),
        set_model(&format!("us.anthropic.{MARKER}-v1:0")),
        set_model("claude-sonnet-5"),
    ];
    let record = interpret::record::record::<ClaudeSdk>(
        &spec,
        [3; 16],
        Value::Null,
        [(
            interpret::encode_checkpoint(&state),
            events.iter().flat_map(line).collect(),
        )],
    );
    let text = record.to_string();
    assert!(!text.contains(MARKER), "{text}");
    assert!(!text.contains("123456789012"), "{text}");
    let settings = &record["settings"];
    for aliased in ["provider", "model", "permission"] {
        assert!(
            settings[aliased].as_str().unwrap().starts_with('@'),
            "{aliased}: {settings}"
        );
    }
    assert_eq!(settings["mode"], "plan");
    assert_eq!(settings["effort"], "high");
    let shapes = &record["segments"][0]["events"];
    let model = |index: usize, at: &[&str]| -> String {
        let mut value = &shapes[index]["event"]["shape"];
        for field in at {
            value = &value[*field];
        }
        value.as_str().unwrap().to_owned()
    };
    assert!(model(0, &["model"]).starts_with('@'));
    assert!(model(1, &["model"]).ends_with("=claude-opus-5-5[1m]"));
    assert!(model(2, &["model", "model"]).starts_with('@'));
    assert!(model(3, &["model", "model"]).ends_with("=claude-sonnet-5"));
}

#[test]
fn a_tool_servers_form_keeps_its_own_shape_and_none_of_its_field_names() {
    let spec = AgentSpec {
        agent_id: b"agent".to_vec(),
        kind: wire::Kind::ClaudePty as i32,
        incarnation: 1,
        ..AgentSpec::default()
    };
    let (state, _) = ClaudePty::initial(&spec);
    let hook = Event::Fact(Fact {
        channel: Channel::Hook,
        payload: serde_json::to_vec(&json!({
            "hook_event_name": "Elicitation", "session_id": "s",
            "transcript_path": "/t", "cwd": "/w",
            "mcp_server_name": "forms", "message": "Fill this in", "mode": "form",
            "requested_schema": {"type": "object", "required": [MARKER],
                "properties": {MARKER: {"type": "string", "title": MARKER}}},
        }))
        .unwrap(),
    });
    let record = interpret::record::record::<ClaudePty>(
        &spec,
        [3; 16],
        Value::Null,
        [(interpret::encode_checkpoint(&state), line(&hook))],
    );
    let text = record.to_string();
    assert!(!text.contains(MARKER), "{text}");
    let schema = &record["segments"][0]["events"][0]["event"]["shape"]["requested_schema"];
    assert_eq!(schema["type"], "=object", "{text}");
    let fields = schema["properties"].as_object().unwrap();
    assert_eq!(fields.len(), 1, "{text}");
    let field = fields.values().next().unwrap();
    assert_eq!(field["type"], "=string", "{text}");
    assert_eq!(field.as_object().unwrap().len(), 2, "{text}");
    assert!(field.get("title").is_none(), "{text}");
}

#[test]
fn a_field_the_providers_schema_does_not_know_is_named_by_where_it_sits() {
    let spec = AgentSpec {
        agent_id: b"agent".to_vec(),
        kind: wire::Kind::Codex as i32,
        incarnation: 1,
        ..AgentSpec::default()
    };
    let (state, _) = Codex::initial(&spec);
    let started = Event::Fact(Fact {
        channel: Channel::Rpc,
        payload: serde_json::to_vec(&json!({"method": "item/started", "params": {
            "threadId": "t", "turnId": "u", "item": {
                "type": "commandExecution", "id": "c1", "command": "ls", "cwd": "/w",
                "status": "inProgress", "commandActions": [], MARKER: "x"}}}))
        .unwrap(),
    });
    let record = interpret::record::record::<Codex>(
        &spec,
        [3; 16],
        Value::Null,
        [(interpret::encode_checkpoint(&state), line(&started))],
    );
    let text = record.to_string();
    assert!(!text.contains(MARKER), "{text}");
    let event = &record["segments"][0]["events"][0]["event"];
    assert_eq!(event["untyped"], json!(["params.item"]), "{text}");
    let item = event["shape"]["params"]["item"].as_object().unwrap();
    assert_eq!(
        item["cwd"].as_str().map(|cwd| cwd.starts_with('@')),
        Some(true)
    );
    assert_eq!(
        item.keys().filter(|name| name.starts_with('@')).count(),
        1,
        "{text}"
    );
}
