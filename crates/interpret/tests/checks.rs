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
    let mut shared =
        interpret::Shared::<wire::Ask>::new(&spec(wire::Kind::ClaudeSdk), wire::Kind::ClaudeSdk);
    let mut emit = interpret::Emit::default();
    shared.item(
        &mut emit,
        interpret::ItemDraft {
            key: "msg_1:0".into(),
            text: "half a".into(),
            ..Default::default()
        },
    );
    assert_eq!(shared.finish(emit, Vec::new()).broken, None);
    // The provider dies and the interpreter leaves the item streaming.
    shared.provider_exited();
    let broken = shared
        .finish(interpret::Emit::default(), Vec::new())
        .broken
        .map(|broken| broken.invariant);
    assert_eq!(broken, Some(checks::EXIT_LEFT_OPEN));
    quiet_everywhere(checks::EXIT_LEFT_OPEN);
}

/// Takes the first fixture of `I` that streams an item whose key starts
/// with `prefix` to that moment, and the provider exits there: the exit
/// closes what was streaming, and no check breaks.
fn an_exit_closes_what_streamed<I: Interpreter>(kind: &str, prefix: &str) {
    let streaming = |step: &wire::Step| {
        step.items
            .iter()
            .any(|item| item.key.starts_with(prefix) && !I::describe_item(&item.body).complete)
    };
    let (mut state, _) = fixtures(kind)
        .iter()
        .find_map(|fixture| interpret::run_until::<I>(fixture, streaming).unwrap())
        .unwrap_or_else(|| panic!("no {kind} fixture streams an item under {prefix:?}"));
    let open = I::structure(&state).open_items;
    assert!(!open.is_empty());
    let exit = I::step(&mut state, Event::ProviderExit { code: Some(1) });
    assert_eq!(exit.broken, None);
    for (key, _) in open {
        assert!(
            exit.step
                .items
                .iter()
                .any(|item| item.key == key && I::describe_item(&item.body).complete),
            "{kind}: {key} left open: {:?}",
            exit.step.items
        );
    }
}

#[test]
fn an_exit_closes_what_each_kind_was_streaming() {
    an_exit_closes_what_streamed::<ClaudeSdk>("claude_sdk", "msg_");
    an_exit_closes_what_streamed::<ClaudePty>("claude_pty", "stream:");
    an_exit_closes_what_streamed::<Codex>("codex", "");
    // Codex holds a turn's answer for a safety check and dies holding it.
    let (mut state, _) = Codex::initial(&spec(wire::Kind::Codex));
    let held = Codex::step(
        &mut state,
        fact(
            Channel::Rpc,
            json!({"method": "model/safetyBuffering/updated", "params": {
                "threadId": "t", "turnId": "u", "showBufferingUi": true}}),
        ),
    );
    assert_eq!(Codex::structure(&state).open_items.len(), 1, "{held:?}");
    let exit = Codex::step(&mut state, Event::ProviderExit { code: Some(1) });
    assert_eq!(exit.broken, None);
    assert!(Codex::structure(&state).open_items.is_empty());
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
