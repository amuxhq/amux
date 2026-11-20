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
    assert_eq!(opening(&neither, Some("s")), Opening::New);
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
