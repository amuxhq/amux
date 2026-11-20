//! The ACP interpreter against the sessions the live lane recorded in
//! fixtures/acp/live, one fixture per recording, and the rules no recording
//! shows on its own.

use std::path::{Path, PathBuf};

use acp_protocol::agent::InitializeResponse;
use interpret::acp::{Acp, Opening, opening, quirks};
use interpret::{Effect, Event, Interpreter, fixture_script, run_golden};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/acp")
}

fn golden(name: &str) {
    run_golden::<Acp>(&fixtures().join(format!("{name}.json"))).assert_ok();
}

macro_rules! goldens {
    ($($test:ident => $fixture:literal,)*) => {
        $(
            #[test]
            fn $test() {
                golden($fixture);
            }
        )*

        const FIXTURES: &[&str] = &[$($fixture),*];
    };
}

goldens! {
    an_added_agent_with_no_quirks => "authored-added_agent",
    recorded_agy_allow => "agy-allow",
    recorded_agy_allow_always => "agy-allow_always",
    recorded_agy_cancel => "agy-cancel",
    recorded_agy_catalogue => "agy-catalogue",
    recorded_agy_compact => "agy-compact",
    recorded_agy_config => "agy-config",
    recorded_agy_deny => "agy-deny",
    recorded_agy_edit => "agy-edit",
    recorded_agy_elicitation => "agy-elicitation",
    recorded_agy_image => "agy-image",
    recorded_agy_load => "agy-load",
    recorded_agy_mid_turn => "agy-mid_turn",
    recorded_agy_prompt => "agy-prompt",
    recorded_agy_question => "agy-question",
    recorded_agy_queued => "agy-queued",
    recorded_agy_read => "agy-read",
    recorded_agy_resume => "agy-resume",
    recorded_grok_allow => "grok-allow",
    recorded_grok_allow_always => "grok-allow_always",
    recorded_grok_cancel => "grok-cancel",
    recorded_grok_catalogue => "grok-catalogue",
    recorded_grok_compact => "grok-compact",
    recorded_grok_config => "grok-config",
    recorded_grok_deny => "grok-deny",
    recorded_grok_edit => "grok-edit",
    recorded_grok_elicitation => "grok-elicitation",
    recorded_grok_image => "grok-image",
    recorded_grok_load => "grok-load",
    recorded_grok_mid_turn => "grok-mid_turn",
    recorded_grok_prompt => "grok-prompt",
    recorded_grok_question => "grok-question",
    recorded_grok_queued => "grok-queued",
    recorded_grok_read => "grok-read",
    recorded_grok_resume => "grok-resume",
    recorded_grok_signed_out => "grok-signed_out",
    recorded_opencode_allow => "opencode-allow",
    recorded_opencode_allow_always => "opencode-allow_always",
    recorded_opencode_cancel => "opencode-cancel",
    recorded_opencode_catalogue => "opencode-catalogue",
    recorded_opencode_compact => "opencode-compact",
    recorded_opencode_config => "opencode-config",
    recorded_opencode_deny => "opencode-deny",
    recorded_opencode_edit => "opencode-edit",
    recorded_opencode_elicitation => "opencode-elicitation",
    recorded_opencode_image => "opencode-image",
    recorded_opencode_load => "opencode-load",
    recorded_opencode_mid_turn => "opencode-mid_turn",
    recorded_opencode_prompt => "opencode-prompt",
    recorded_opencode_question => "opencode-question",
    recorded_opencode_queued => "opencode-queued",
    recorded_opencode_read => "opencode-read",
    recorded_opencode_resume => "opencode-resume",
}

#[test]
fn every_fixture_has_a_test() {
    let mut on_disk = std::fs::read_dir(fixtures())
        .unwrap()
        .filter_map(|entry| {
            let path = entry.unwrap().path();
            (path.extension()? == "json").then(|| path.file_stem()?.to_str().map(str::to_owned))?
        })
        .collect::<Vec<_>>();
    on_disk.sort();
    let mut named = FIXTURES
        .iter()
        .map(|name| (*name).to_owned())
        .collect::<Vec<_>>();
    named.sort();
    assert_eq!(on_disk, named);
}

/// Every recording reads with nothing amux does not know: no unrecognized
/// row, and no vendor method off the known list. Grok's methods on that
/// list are dropped by name, deliberately; one not on it is logged, and
/// fails this test until someone decides what it is.
#[test]
fn every_recording_replays_with_nothing_unknown() {
    let mut problems = Vec::new();
    for name in FIXTURES
        .iter()
        .filter(|name| !name.starts_with("authored-"))
    {
        let script = fixture_script::<Acp>(&fixtures().join(format!("{name}.json"))).unwrap();
        let mut spec = script.spec.clone();
        let (mut state, _) = Acp::initial(&spec);
        for (label, event) in script.events {
            let Some(event) = event else {
                if label.starts_with("reincarnate") {
                    spec.incarnation += 1;
                    state = Acp::reincarnate(state, &spec).0;
                }
                continue;
            };
            let stepped = Acp::step(&mut state, event);
            for item in &stepped.step.items {
                if Acp::describe_item(&item.body).arm == "unrecognized" {
                    problems.push(format!(
                        "{name} {label}: {}",
                        Acp::describe_item(&item.body).text
                    ));
                }
            }
            for effect in &stepped.effects {
                if let Effect::Log { line } = effect
                    && line.contains("which amux does not read")
                {
                    problems.push(format!("{name} {label}: {line}"));
                }
            }
        }
    }
    assert!(problems.is_empty(), "{problems:#?}");
}

fn initialize(capabilities: serde_json::Value) -> InitializeResponse {
    serde_json::from_value(serde_json::json!({
        "protocolVersion": 1,
        "agentCapabilities": capabilities,
    }))
    .unwrap()
}

#[test]
fn a_saved_session_is_resumed_before_it_is_loaded() {
    let both = initialize(serde_json::json!({
        "loadSession": true,
        "sessionCapabilities": {"resume": {}},
    }));
    let load = initialize(serde_json::json!({"loadSession": true}));
    let neither = initialize(serde_json::json!({}));
    assert_eq!(opening(&both, Some("s")), Opening::Resume("s".into()));
    assert_eq!(opening(&load, Some("s")), Opening::Load("s".into()));
    // A conversation no new process can reopen is over: never a new one
    // in its place.
    assert_eq!(opening(&neither, Some("s")), Opening::Unresumable);
    assert_eq!(opening(&neither, None), Opening::New);
    assert_eq!(opening(&both, None), Opening::New);
}

#[test]
fn an_agent_a_person_added_has_no_quirks() {
    assert_eq!(quirks("my-own-agent"), interpret::acp::Quirks::default());
    assert!(quirks("antigravity").steers);
    assert!(!quirks("opencode").steers && !quirks("grok").steers);
}

fn rpc(message: serde_json::Value) -> Event {
    Event::Fact(interpret::Fact {
        channel: interpret::Channel::Rpc,
        payload: message.to_string().into_bytes(),
    })
}

fn spec(provider: &str) -> wire::AgentSpec {
    wire::AgentSpec {
        agent_id: b"agent".to_vec(),
        kind: wire::Kind::Acp as i32,
        incarnation: 1,
        provider: provider.to_owned(),
        ..Default::default()
    }
}

fn opened(provider: &str, capabilities: serde_json::Value) -> interpret::acp::State {
    let (mut state, _) = Acp::initial(&spec(provider));
    Acp::step(
        &mut state,
        rpc(
            serde_json::json!({"jsonrpc": "2.0", "id": interpret::acp::INITIALIZE_REQUEST,
            "result": {"protocolVersion": 1, "agentCapabilities": capabilities}}),
        ),
    );
    Acp::step(
        &mut state,
        rpc(
            serde_json::json!({"jsonrpc": "2.0", "id": interpret::acp::NEW_SESSION_REQUEST,
            "result": {"sessionId": "s1"}}),
        ),
    );
    state
}

#[test]
fn a_session_no_new_process_can_reopen_is_held() {
    let held = opened("my-own-agent", serde_json::json!({}));
    assert!(Acp::holds(&held).unresumable);
    assert!(!Acp::holds(&held).nothing());
    let reopens = opened("my-own-agent", serde_json::json!({"loadSession": true}));
    assert!(!Acp::holds(&reopens).unresumable);
}

fn prompt(id: &str, text: &str) -> Event {
    Event::Input(wire::Input::new(
        id.as_bytes().to_vec(),
        wire::AcpInput {
            of: Some(wire::acp_input::Of::Prompt(wire::PromptInput {
                text: text.into(),
                attachments: Vec::new(),
            })),
        },
    ))
}

fn send_now(id: &str, target: &str) -> Event {
    Event::Input(wire::Input::new(
        id.as_bytes().to_vec(),
        wire::AcpInput {
            of: Some(wire::acp_input::Of::SendNow(wire::SendQueuedNow {
                queued_input_id: target.as_bytes().to_vec(),
            })),
        },
    ))
}

fn refused(effects: &[Effect], id: &str) -> bool {
    effects.iter().any(|effect| {
        matches!(effect,
        Effect::Reply { input_id, verdict } if input_id == id.as_bytes()
            && matches!(verdict.of, Some(wire::send_input_response::Of::Rejected(_))))
    })
}

/// A prompt sent mid-turn waits for the running one's answer, and only an
/// agent known to fold one into its turn is sent one early.
#[test]
fn send_now_goes_only_to_an_agent_that_steers() {
    for (provider, steers) in [
        ("antigravity", true),
        ("opencode", false),
        ("my-own-agent", false),
    ] {
        let mut state = opened(provider, serde_json::json!({}));
        Acp::step(&mut state, prompt("a", "first"));
        let queued = Acp::step(&mut state, prompt("b", "second"));
        assert!(
            !queued
                .effects
                .iter()
                .any(|effect| matches!(effect, Effect::ProviderWrite(_)))
        );
        let sent = Acp::step(&mut state, send_now("c", "b"));
        assert_eq!(refused(&sent.effects, "c"), !steers, "{provider}");
    }
}

fn writes(effects: &[Effect]) -> Vec<serde_json::Value> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::ProviderWrite(bytes) => serde_json::from_slice(bytes).ok(),
            _ => None,
        })
        .collect()
}

fn exit_cause(effects: &[Effect]) -> Option<String> {
    effects.iter().find_map(|effect| match effect {
        Effect::Exit { cause } => Some(cause.clone()),
        _ => None,
    })
}

#[test]
fn a_refused_reopening_ends_the_agent_and_starts_nothing() {
    let (mut state, _) = Acp::initial(&spec("antigravity"));
    let stepped = Acp::step(
        &mut state,
        rpc(
            serde_json::json!({"jsonrpc": "2.0", "id": interpret::acp::RESUME_REQUEST,
            "error": {"code": -32002, "message": "no such session"}}),
        ),
    );
    let cause = exit_cause(&stepped.effects).unwrap();
    assert!(
        cause.contains("refused to reopen its conversation"),
        "{cause}"
    );
    assert!(writes(&stepped.effects).is_empty());
}

#[test]
fn a_new_session_must_name_itself() {
    let (mut state, _) = Acp::initial(&spec("antigravity"));
    let stepped = Acp::step(
        &mut state,
        rpc(
            serde_json::json!({"jsonrpc": "2.0", "id": interpret::acp::NEW_SESSION_REQUEST,
            "result": {}}),
        ),
    );
    assert!(exit_cause(&stepped.effects).is_some());
}

fn permission(id: u64) -> Event {
    rpc(
        serde_json::json!({"jsonrpc": "2.0", "id": id, "method": "session/request_permission",
        "params": {"sessionId": "s1", "toolCall": {"toolCallId": "c1"},
            "options": [{"optionId": "yes", "name": "Allow", "kind": "allow_once"}]}}),
    )
}

fn answer(id: &str, ask: &str) -> Event {
    Event::Input(wire::Input::new(
        id.as_bytes().to_vec(),
        wire::AcpInput {
            of: Some(wire::acp_input::Of::Answer(wire::AcpAnswer {
                ask_key: ask.to_owned(),
                of: Some(wire::acp_answer::Of::Option("yes".to_owned())),
            })),
        },
    ))
}

/// An agent may reuse a request id in a later process; an answer to the
/// earlier ask never reaches the later one.
#[test]
fn a_stale_answer_never_meets_an_ask_that_reused_its_request_id() {
    let mut state = opened("opencode", serde_json::json!({"loadSession": true}));
    Acp::step(&mut state, prompt("a", "go"));
    Acp::step(&mut state, permission(0));
    Acp::step(&mut state, Event::ProviderExit { code: Some(0) });
    let mut next = spec("opencode");
    next.incarnation = 2;
    let (mut state, _) = Acp::reincarnate(state, &next);
    Acp::step(
        &mut state,
        rpc(
            serde_json::json!({"jsonrpc": "2.0", "id": interpret::acp::LOAD_REQUEST,
            "result": {}}),
        ),
    );
    Acp::step(&mut state, prompt("b", "again"));
    Acp::step(&mut state, permission(0));
    let stale = Acp::step(&mut state, answer("x", "ask-1"));
    assert!(refused(&stale.effects, "x"));
    assert!(writes(&stale.effects).is_empty());
    let fresh = Acp::step(&mut state, answer("y", "ask-2"));
    assert!(!refused(&fresh.effects, "y"));
    assert_eq!(writes(&fresh.effects)[0]["id"], 0);
}

#[test]
fn an_ask_while_cancelling_is_answered_cancelled_at_once() {
    let mut state = opened("opencode", serde_json::json!({}));
    Acp::step(&mut state, prompt("a", "go"));
    Acp::step(
        &mut state,
        Event::Input(wire::Input::new(
            b"i".to_vec(),
            wire::AcpInput {
                of: Some(wire::acp_input::Of::Interrupt(wire::Interrupt {})),
            },
        )),
    );
    let asked = Acp::step(&mut state, permission(3));
    assert_eq!(
        writes(&asked.effects),
        vec![serde_json::json!({"jsonrpc": "2.0", "id": 3,
            "result": {"outcome": {"outcome": "cancelled"}}})]
    );
    assert!(state.shared().asks().is_empty());
}

#[test]
fn an_agent_message_is_not_withdrawn() {
    let mut state = opened("opencode", serde_json::json!({}));
    Acp::step(&mut state, prompt("a", "go"));
    Acp::step(
        &mut state,
        Event::AgentMessage(wire::Envelope {
            id: b"m".to_vec(),
            body: Some(wire::envelope::Body::Message(wire::MessageBody {
                text: "hello".into(),
                attachments: Vec::new(),
            })),
            ..Default::default()
        }),
    );
    let withdrawn = Acp::step(
        &mut state,
        Event::Input(wire::Input::new(
            b"w".to_vec(),
            wire::AcpInput {
                of: Some(wire::acp_input::Of::Withdraw(wire::WithdrawQueued {
                    queued_input_id: b"m".to_vec(),
                })),
            },
        )),
    );
    assert!(refused(&withdrawn.effects, "w"));
}

fn select(id: &str, category: &str, current: &str, values: &[&str]) -> serde_json::Value {
    serde_json::json!({"type": "select", "id": id, "name": id, "category": category,
        "currentValue": current,
        "options": values.iter().map(|value| serde_json::json!({"value": value, "name": value}))
            .collect::<Vec<_>>()})
}

/// The model is restored first and the thought level checked against what
/// the model offers; the first prompt waits for both.
#[test]
fn settings_are_restored_model_first_before_the_first_prompt() {
    let mut created = spec("opencode");
    created.config = Some(wire::EffectiveConfig {
        model: Some("big".into()),
        effort: Some("high".into()),
        ..Default::default()
    });
    let (mut state, _) = Acp::initial(&created);
    let opened = Acp::step(
        &mut state,
        rpc(
            serde_json::json!({"jsonrpc": "2.0", "id": interpret::acp::NEW_SESSION_REQUEST,
            "result": {"sessionId": "s1",
                "configOptions": [select("model", "model", "small", &["small", "big"])]}}),
        ),
    );
    let sent = writes(&opened.effects);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["id"], "amux-apply-model");
    let held = Acp::step(&mut state, prompt("a", "go"));
    assert!(writes(&held.effects).is_empty());
    let model = Acp::step(
        &mut state,
        rpc(
            serde_json::json!({"jsonrpc": "2.0", "id": "amux-apply-model",
            "result": {"configOptions": [select("model", "model", "big", &["small", "big"]),
                select("effort", "thought_level", "low", &["low", "high"])]}}),
        ),
    );
    let sent = writes(&model.effects);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["id"], "amux-apply-effort");
    let effort = Acp::step(
        &mut state,
        rpc(
            serde_json::json!({"jsonrpc": "2.0", "id": "amux-apply-effort",
            "result": {"configOptions": [select("model", "model", "big", &["small", "big"]),
                select("effort", "thought_level", "high", &["low", "high"])]}}),
        ),
    );
    let sent = writes(&effort.effects);
    assert_eq!(sent[0]["method"], "session/prompt");
}

/// A send-now the agent turns down waits its turn again, with the refusal
/// shown, rather than standing as a steer that never landed.
#[test]
fn a_refused_send_now_waits_again() {
    let mut state = opened("antigravity", serde_json::json!({}));
    Acp::step(&mut state, prompt("a", "first"));
    Acp::step(&mut state, prompt("b", "second"));
    let sent = Acp::step(&mut state, send_now("c", "b"));
    assert!(!refused(&sent.effects, "c"));
    let steer = writes(&sent.effects)
        .into_iter()
        .find(|write| write["method"] == "session/prompt")
        .unwrap();
    let turned_down = Acp::step(
        &mut state,
        rpc(serde_json::json!({"jsonrpc": "2.0", "id": steer["id"],
            "error": {"code": -32603, "message": "busy"}})),
    );
    assert!(state.shared().queue().contains(b"b"));
    assert!(!state.shared().queue().has_steered());
    assert!(
        turned_down
            .step
            .items
            .iter()
            .any(|item| item.key.contains("error"))
    );
}
