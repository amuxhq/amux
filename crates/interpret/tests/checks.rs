//! The checks every interpreter's steps are judged by: each fires on a
//! sequence crafted to break it, and none fires on any fixture, recorded or
//! authored, of any kind.

use std::path::{Path, PathBuf};

use interpret::checks::{self, Check};
use interpret::claude_pty::ClaudePty;
use interpret::claude_sdk::ClaudeSdk;
use interpret::codex::Codex;
use interpret::{Channel, Event, Fact, Interpreter, replay};
use serde_json::{Value, json};
use wire::AgentSpec;

/// Every fixture of one kind, by path.
fn fixtures(kind: &str) -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(kind);
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    paths.sort();
    paths
}

/// Every step of every fixture of `I` that broke `check`.
fn fired<I: Interpreter>(kind: &str, check: Check) -> Vec<String> {
    let mut fired = Vec::new();
    for fixture in fixtures(kind) {
        let frames = replay::<I>(&fixture).unwrap_or_else(|error| {
            panic!("{}: {error}", fixture.display());
        });
        for frame in frames {
            if let Some(broken) = frame.broken
                && broken.invariant == check
            {
                fired.push(format!(
                    "{} at {}: {}",
                    fixture.display(),
                    frame.label,
                    broken.detail
                ));
            }
        }
    }
    fired
}

/// The check stays quiet on every fixture of every kind.
fn quiet_everywhere(check: Check) {
    let mut all = fired::<ClaudeSdk>("claude_sdk", check);
    all.extend(fired::<ClaudePty>("claude_pty", check));
    all.extend(fired::<Codex>("codex", check));
    assert!(all.is_empty(), "{}: {all:#?}", check.id);
}

fn spec(kind: wire::Kind) -> AgentSpec {
    AgentSpec {
        agent_id: b"agent".to_vec(),
        kind: kind as i32,
        incarnation: 1,
        created_at_ms: 1_000,
        ..AgentSpec::default()
    }
}

fn fact(channel: Channel, value: Value) -> Event {
    Event::Fact(Fact {
        channel,
        payload: serde_json::to_vec(&value).unwrap(),
    })
}

/// Steps `events` through a fresh interpreter of `I`, returning each
/// step's broken check id.
fn run<I: Interpreter>(kind: wire::Kind, events: Vec<Event>) -> Vec<Option<&'static str>> {
    let (mut state, _) = I::initial(&spec(kind));
    events
        .into_iter()
        .map(|event| {
            I::step(&mut state, event)
                .broken
                .map(|broken| broken.invariant.id)
        })
        .collect()
}

#[test]
fn a_tool_result_for_a_call_never_seen_breaks_its_check() {
    let steps = run::<ClaudeSdk>(
        wire::Kind::ClaudeSdk,
        vec![fact(
            Channel::Stream,
            json!({
                "type": "user",
                "message": {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "toolu_never_called", "content": "done"}
                ]},
                "session_id": "s",
            }),
        )],
    );
    assert_eq!(steps, [Some(checks::TOOL_RESULT_WITHOUT_CALL.id)]);
    let codex = run::<Codex>(
        wire::Kind::Codex,
        vec![fact(
            Channel::Rpc,
            json!({"method": "item/completed", "params": {
                "threadId": "t", "turnId": "u",
                "item": {"type": "commandExecution", "id": "never_started", "command": "ls",
                    "cwd": "/", "status": "completed", "commandActions": []},
            }}),
        )],
    );
    assert_eq!(codex, [Some(checks::TOOL_RESULT_WITHOUT_CALL.id)]);
    quiet_everywhere(checks::TOOL_RESULT_WITHOUT_CALL);
}

#[test]
fn an_exit_that_leaves_an_item_streaming_breaks_its_check() {
    // Claude dies halfway through streaming a message.
    let start = |event: Value| {
        fact(
            Channel::Stream,
            json!({"type": "stream_event", "event": event}),
        )
    };
    let steps = run::<ClaudeSdk>(
        wire::Kind::ClaudeSdk,
        vec![
            start(
                json!({"type": "message_start", "message": {"id": "msg_1", "role": "assistant", "content": []}}),
            ),
            start(
                json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
            ),
            start(
                json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "half a"}}),
            ),
            Event::ProviderExit { code: Some(1) },
        ],
    );
    assert_eq!(
        steps.last(),
        Some(&Some(checks::EXIT_LEFT_OPEN.id)),
        "{steps:?}"
    );
    quiet_everywhere(checks::EXIT_LEFT_OPEN);
}

#[test]
fn an_answer_accepted_for_a_closed_ask_breaks_its_check() {
    let mut shared =
        interpret::Shared::<wire::Ask>::new(&spec(wire::Kind::ClaudeSdk), wire::Kind::ClaudeSdk);
    shared.open_ask(wire::Ask {
        key: "ask:1".into(),
        ..wire::Ask::default()
    });
    let mut emit = interpret::Emit::default();
    shared.close_ask("ask:1");
    let ok = shared.finish(emit, Vec::new());
    assert_eq!(ok.broken, None);
    // A later answer to the closed ask is refused, as it must be; then the
    // interpreter takes it anyway.
    emit = interpret::Emit::default();
    assert!(shared.answer(&mut emit, b"late", "ask:1").is_none());
    shared.accept(&mut emit, b"late", false);
    let broken = shared
        .finish(emit, Vec::new())
        .broken
        .map(|broken| broken.invariant);
    assert_eq!(broken, Some(checks::CLOSED_ASK_ANSWERED));
    quiet_everywhere(checks::CLOSED_ASK_ANSWERED);
}

#[test]
fn an_item_opened_again_after_its_final_state_breaks_its_check() {
    // Codex finishes a message, then starts an item under the same id.
    let item = |method: &str, text: &str| {
        fact(
            Channel::Rpc,
            json!({"method": method, "params": {"threadId": "t", "turnId": "u", "item":
                {"type": "agentMessage", "id": "msg_1", "text": text}}}),
        )
    };
    let steps = run::<Codex>(
        wire::Kind::Codex,
        vec![
            item("item/started", ""),
            item("item/completed", "done"),
            item("item/started", ""),
        ],
    );
    assert_eq!(steps, [None, None, Some(checks::ITEM_BEFORE_OPEN.id)]);
    quiet_everywhere(checks::ITEM_BEFORE_OPEN);
}
