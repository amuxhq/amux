//! The ACP interpreter against the sessions the live lane recorded in
//! fixtures/acp/live, one fixture per recording, and the rules no recording
//! shows on its own.

use std::path::PathBuf;

use acp_protocol::agent::InitializeResponse;
use interpret::acp::{Acp, Opening, opening, quirks};
use interpret::{Effect, Event, Interpreter, fixture_script, run_golden};

fn fixtures() -> PathBuf {
    source_tree::crate_dir!().join("fixtures/acp")
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
    what_an_agent_presents_becomes_attachments => "authored-presented",
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

/// The capabilities the snapshots in `stepped` say, newest last.
fn said_capabilities(stepped: &interpret::Stepped) -> Option<wire::AgentCapabilities> {
    stepped
        .step
        .snapshot
        .as_ref()?
        .capabilities
        .clone()
        .map(|capabilities| *capabilities)
}

/// An ACP agent's snapshot says what its process can do: nothing it has
/// not said before `initialize` answers, and what that answer offered
/// after, with send-now for an agent known to steer. It is resumable until
/// it has a conversation to lose, as the daemon starts a later process for
/// one with none: then only where its process said it reopens one. It
/// forks where its process offered `session/fork`.
#[test]
fn an_acp_agents_snapshot_says_what_its_process_can_do() {
    for (provider, steers) in [("antigravity", true), ("my-own-agent", false)] {
        for reopens in [true, false] {
            let (mut state, first) = Acp::initial(&spec(provider));
            assert_eq!(
                first.snapshot.unwrap().capabilities,
                Some(Box::new(wire::AgentCapabilities::acp(true, false, steers))),
                "{provider} before it answered"
            );
            let answered = Acp::step(
                &mut state,
                rpc(
                    serde_json::json!({"jsonrpc": "2.0", "id": interpret::acp::INITIALIZE_REQUEST,
                    "result": {"protocolVersion": 1, "agentCapabilities": {
                        "loadSession": reopens, "promptCapabilities": {"image": true},
                        "sessionCapabilities": if reopens {
                            serde_json::json!({"fork": {}})
                        } else {
                            serde_json::json!({})
                        }}}}),
                ),
            );
            assert_eq!(
                said_capabilities(&answered),
                Some(wire::AgentCapabilities {
                    forks: reopens,
                    ..wire::AgentCapabilities::acp(true, true, steers)
                }),
                "{provider} once it answered, with no conversation yet"
            );
            let opened = Acp::step(
                &mut state,
                rpc(
                    serde_json::json!({"jsonrpc": "2.0", "id": interpret::acp::NEW_SESSION_REQUEST,
                    "result": {"sessionId": "s1"}}),
                ),
            );
            assert_eq!(
                said_capabilities(&opened),
                Some(wire::AgentCapabilities {
                    forks: reopens,
                    ..wire::AgentCapabilities::acp(reopens, true, steers)
                }),
                "{provider} with a conversation, reopening it: {reopens}"
            );
        }
    }
}

/// Claude's and Codex's snapshots say what every agent of their kind can
/// do, from the first.
#[test]
fn the_other_kinds_snapshots_say_what_their_kind_can_do() {
    let spec = |kind: wire::Kind| wire::AgentSpec {
        agent_id: b"agent".to_vec(),
        kind: kind as i32,
        incarnation: 1,
        ..Default::default()
    };
    let first = |step: wire::Step| step.snapshot.unwrap().capabilities;
    use interpret::claude_pty::ClaudePty;
    use interpret::claude_sdk::ClaudeSdk;
    use interpret::codex::Codex;
    assert_eq!(
        first(ClaudePty::initial(&spec(wire::Kind::ClaudePty)).1),
        Some(Box::new(wire::AgentCapabilities::claude_pty()))
    );
    assert_eq!(
        first(ClaudeSdk::initial(&spec(wire::Kind::ClaudeSdk)).1),
        Some(Box::new(wire::AgentCapabilities::claude_sdk()))
    );
    assert_eq!(
        first(Codex::initial(&spec(wire::Kind::Codex)).1),
        Some(Box::new(wire::AgentCapabilities::codex()))
    );
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

/// The sign-in an ACP snapshot in `stepped` says, if it says one.
fn said_sign_in(stepped: &interpret::Stepped) -> Option<wire::SignInState> {
    Some(stepped.step.snapshot.as_ref()?.sign_in.as_ref()?.state())
}

fn reopens(effects: &[Effect]) -> usize {
    effects
        .iter()
        .filter(|effect| matches!(effect, Effect::AcpReopen))
        .count()
}

fn auth_required(id: &str) -> Event {
    rpc(serde_json::json!({"jsonrpc": "2.0", "id": id,
        "error": {"code": acp_protocol::error_code::AUTH_REQUIRED,
            "message": "Authentication required"}}))
}

/// An agent that will not open its session until the person signs in does
/// not end: it waits idle, says it is signed out, and holds what is sent.
/// A prompt asks it again, once, by the same request; once the session
/// opens it is signed in and the held prompts go, in order.
#[test]
fn a_signed_out_agent_waits_and_a_prompt_asks_again() {
    let (mut state, _) = Acp::initial(&spec("opencode"));
    let refused = Acp::step(
        &mut state,
        auth_required(interpret::acp::NEW_SESSION_REQUEST),
    );
    assert_eq!(exit_cause(&refused.effects), None, "not an end");
    assert_eq!(said_sign_in(&refused), Some(wire::SignInState::SignedOut));
    assert_eq!(
        refused
            .step
            .snapshot
            .as_ref()
            .map(|snapshot| snapshot.phase()),
        Some(wire::Phase::Idle),
        "waiting on the person, not starting"
    );
    assert_eq!(
        reopens(&refused.effects),
        0,
        "nothing asks again on its own"
    );

    let first = Acp::step(&mut state, prompt("a", "first"));
    assert_eq!(reopens(&first.effects), 1, "the prompt asks again");
    assert!(writes(&first.effects).is_empty(), "the prompt waits");
    let second = Acp::step(&mut state, prompt("b", "second"));
    assert_eq!(reopens(&second.effects), 0, "one ask in flight at a time");

    // Still signed out. The person may have signed in after the ask went,
    // so the prompt sent meanwhile asks once more; a refusal of that waits
    // for the next prompt.
    let again = Acp::step(
        &mut state,
        auth_required(interpret::acp::NEW_SESSION_REQUEST),
    );
    assert_eq!(reopens(&again.effects), 1, "asked once more");
    let waits = Acp::step(
        &mut state,
        auth_required(interpret::acp::NEW_SESSION_REQUEST),
    );
    assert_eq!(reopens(&waits.effects), 0, "nothing asks again on its own");
    let third = Acp::step(&mut state, prompt("c", "third"));
    assert_eq!(reopens(&third.effects), 1);

    let opened = Acp::step(
        &mut state,
        rpc(
            serde_json::json!({"jsonrpc": "2.0", "id": interpret::acp::NEW_SESSION_REQUEST,
            "result": {"sessionId": "s1"}}),
        ),
    );
    assert_eq!(said_sign_in(&opened), Some(wire::SignInState::SignedIn));
    let sent = writes(&opened.effects);
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(sent[0]["method"], "session/prompt");
    assert_eq!(sent[0]["params"]["prompt"][0]["text"], "first");
}

/// A refused reopening of a saved conversation is asked again as the same
/// reopening, never as a new session: a fresh conversation is never passed
/// off as the old one resumed.
#[test]
fn a_signed_out_reopening_is_asked_again_as_itself() {
    let (mut state, _) = Acp::initial(&spec("antigravity"));
    Acp::step(
        &mut state,
        rpc(
            serde_json::json!({"jsonrpc": "2.0", "id": interpret::acp::NEW_SESSION_REQUEST,
            "result": {"sessionId": "s1"}}),
        ),
    );
    Acp::step(&mut state, Event::ProviderExit { code: Some(0) });
    let (mut state, _) = Acp::reincarnate(
        state,
        &wire::AgentSpec {
            incarnation: 2,
            ..spec("antigravity")
        },
    );
    let refused = Acp::step(&mut state, auth_required(interpret::acp::RESUME_REQUEST));
    assert_eq!(exit_cause(&refused.effects), None);
    let asked = Acp::step(&mut state, prompt("a", "again"));
    assert_eq!(
        reopens(&asked.effects),
        1,
        "the agent process sends its resume again"
    );
    assert!(
        writes(&asked.effects).is_empty(),
        "the interpreter writes no session of its own"
    );
    // A new session answered in its place is refused as ever.
    let fresh = Acp::step(
        &mut state,
        rpc(
            serde_json::json!({"jsonrpc": "2.0", "id": interpret::acp::NEW_SESSION_REQUEST,
            "result": {"sessionId": "s2"}}),
        ),
    );
    assert!(exit_cause(&fresh.effects).is_some());
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
        settings: Some(wire::AgentSettings {
            model: Some("big".into()),
            effort: Some("high".into()),
            ..Default::default()
        }),
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

fn set(id: &str, of: wire::acp_input::Of) -> Event {
    Event::Input(wire::Input::new(
        id.as_bytes().to_vec(),
        wire::AcpInput { of: Some(of) },
    ))
}

/// A person's model or effort pick is said on the step where the agent
/// took it, for its host to keep, and not where amux only asked: one the
/// agent refuses is never kept, a mode never is, and a value amux restores
/// itself as a session opens was nobody's pick.
#[test]
fn a_pick_is_said_once_the_agent_took_it() {
    let options = |model: &str, effort: &str, mode: &str| {
        serde_json::json!([
            select("model", "model", model, &["small", "big"]),
            select("effort", "thought_level", effort, &["low", "high"]),
            select("mode", "mode", mode, &["ask", "code"])
        ])
    };
    let (mut state, _) = Acp::initial(&spec("opencode"));
    let opened = Acp::step(
        &mut state,
        rpc(
            serde_json::json!({"jsonrpc": "2.0", "id": interpret::acp::NEW_SESSION_REQUEST,
            "result": {"sessionId": "s1", "configOptions": options("small", "low", "ask")}}),
        ),
    );
    assert_eq!(opened.step.picked, None);

    let asked = Acp::step(
        &mut state,
        set(
            "m",
            wire::acp_input::Of::Model(wire::SetModel {
                model: Some("big".into()),
            }),
        ),
    );
    assert_eq!(writes(&asked.effects)[0]["id"], "amux-set-1");
    assert_eq!(asked.step.picked, None, "asked, not yet taken");
    let taken = Acp::step(
        &mut state,
        rpc(serde_json::json!({"jsonrpc": "2.0", "id": "amux-set-1",
            "result": {"configOptions": options("big", "low", "ask")}})),
    );
    assert_eq!(
        taken.step.picked,
        Some(wire::AgentSettings {
            model: Some("big".into()),
            ..Default::default()
        })
    );

    Acp::step(
        &mut state,
        set(
            "e",
            wire::acp_input::Of::Effort(wire::SetEffort {
                effort: Some("high".into()),
            }),
        ),
    );
    let refused = Acp::step(
        &mut state,
        rpc(serde_json::json!({"jsonrpc": "2.0", "id": "amux-set-2",
            "error": {"code": -32602, "message": "not now"}})),
    );
    assert_eq!(refused.step.picked, None, "refused, so not kept");

    Acp::step(
        &mut state,
        set(
            "o",
            wire::acp_input::Of::Mode(wire::SetMode {
                value: "code".into(),
            }),
        ),
    );
    let mode = Acp::step(
        &mut state,
        rpc(serde_json::json!({"jsonrpc": "2.0", "id": "amux-set-3",
            "result": {"configOptions": options("big", "low", "code")}})),
    );
    assert_eq!(mode.step.picked, None, "a mode is never kept");
}

/// The catalogue the effects of a step published, if they published one.
fn published_catalogue(effects: &[Effect]) -> Option<wire::Catalogue> {
    use prost::Message as _;
    effects.iter().rev().find_map(|effect| match effect {
        Effect::WriteBlob { bytes, .. } => wire::Catalogue::decode(bytes.as_slice()).ok(),
        _ => None,
    })
}

/// A built-in agent's modes amux knows the levels of are its permissions:
/// listed as permissions, set by a permission input through the same ACP
/// call as a mode, and reported as the permission. Its other modes stay
/// modes. A created agent's permission wins over a mode created with it.
#[test]
fn a_built_in_agents_known_modes_are_its_permissions() {
    let options = |mode: &str| {
        serde_json::json!([select("mode", "mode", mode, &["plan", "default", "custom"])])
    };
    let mut created = spec("antigravity");
    created.config = Some(wire::EffectiveConfig {
        settings: Some(wire::AgentSettings {
            permission: Some("plan".into()),
            mode: Some("custom".into()),
            ..Default::default()
        }),
        ..Default::default()
    });
    let (mut state, _) = Acp::initial(&created);
    let opened = Acp::step(
        &mut state,
        rpc(
            serde_json::json!({"jsonrpc": "2.0", "id": interpret::acp::NEW_SESSION_REQUEST,
            "result": {"sessionId": "s1", "configOptions": options("default")}}),
        ),
    );
    let catalogue = published_catalogue(&opened.effects).expect("a catalogue");
    let permissions: Vec<(&str, wire::PermissionLevel)> = catalogue
        .permissions
        .iter()
        .map(|permission| (permission.value.as_str(), permission.level()))
        .collect();
    assert_eq!(
        permissions,
        [
            ("plan", wire::PermissionLevel::ReadOnly),
            ("default", wire::PermissionLevel::Ask),
        ]
    );
    let modes: Vec<&str> = catalogue
        .modes
        .iter()
        .map(|mode| mode.value.as_str())
        .collect();
    assert_eq!(modes, ["custom"]);
    let applied = writes(&opened.effects);
    assert_eq!(applied[0]["method"], "session/set_config_option");
    assert_eq!(applied[0]["params"]["value"], "plan");

    let applied = Acp::step(
        &mut state,
        rpc(
            serde_json::json!({"jsonrpc": "2.0", "id": "amux-apply-mode",
            "result": {"configOptions": options("plan")}}),
        ),
    );
    let settings = applied.step.snapshot.as_ref().unwrap().settings().clone();
    assert_eq!(settings.permission.as_deref(), Some("plan"));
    assert_eq!(settings.permission_level(), wire::PermissionLevel::ReadOnly);
    assert_eq!(settings.mode, None);

    let asked = Acp::step(
        &mut state,
        set(
            "o",
            wire::acp_input::Of::Mode(wire::SetMode {
                value: "custom".into(),
            }),
        ),
    );
    assert_eq!(writes(&asked.effects)[0]["params"]["value"], "custom");
    let moded = Acp::step(
        &mut state,
        rpc(serde_json::json!({"jsonrpc": "2.0", "id": "amux-set-1",
            "result": {"configOptions": options("custom")}})),
    );
    let settings = moded.step.snapshot.as_ref().unwrap().settings().clone();
    assert_eq!(settings.permission, None);
    assert_eq!(settings.mode.as_deref(), Some("custom"));

    let asked = Acp::step(
        &mut state,
        set(
            "p",
            wire::acp_input::Of::Permission(wire::SetPermission {
                value: "default".into(),
            }),
        ),
    );
    assert!(!refused(&asked.effects, "p"));
    assert_eq!(writes(&asked.effects)[0]["params"]["value"], "default");
    let permitted = Acp::step(
        &mut state,
        rpc(serde_json::json!({"jsonrpc": "2.0", "id": "amux-set-2",
            "result": {"configOptions": options("default")}})),
    );
    let settings = permitted.step.snapshot.as_ref().unwrap().settings().clone();
    assert_eq!(settings.permission.as_deref(), Some("default"));
    assert_eq!(settings.permission_level(), wire::PermissionLevel::Ask);
    assert_eq!(settings.mode, None);
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

/// A permission request with this tool call and amux's usual options.
fn asked_about(id: u64, tool_call: serde_json::Value) -> Event {
    rpc(
        serde_json::json!({"jsonrpc": "2.0", "id": id, "method": "session/request_permission",
        "params": {"sessionId": "s1", "toolCall": tool_call,
            "options": [{"optionId": "always", "name": "Always", "kind": "allow_always"},
                {"optionId": "once", "name": "Once", "kind": "allow_once"},
                {"optionId": "no", "name": "Deny", "kind": "reject_once"}]}}),
    )
}

fn update(update: serde_json::Value) -> Event {
    rpc(
        serde_json::json!({"jsonrpc": "2.0", "method": "session/update",
        "params": {"sessionId": "s1", "update": update}}),
    )
}

/// Whether the step answered permission request `id` allow once, opening
/// no ask.
fn allowed_unasked(state: &interpret::acp::State, stepped: &interpret::Stepped, id: u64) -> bool {
    writes(&stepped.effects)
        == vec![serde_json::json!({"jsonrpc": "2.0", "id": id,
            "result": {"outcome": {"outcome": "selected", "optionId": "once"}}})]
        && state.shared().asks().is_empty()
}

/// A call to one of amux's own tools is allowed at once, by each agent's
/// own way of naming a tool server's tool: Antigravity's `name` and `Run`
/// title (`agy-elicitation`), OpenCode's title on the call before its
/// permission request retitles it (`opencode-elicitation`,
/// `opencode-allow`), Grok's `use_tool` input (`grok-elicitation`); an
/// agent a person added in any of those ways.
#[test]
fn amuxs_own_tools_are_allowed_without_asking() {
    let cases: [(&str, Vec<Event>, serde_json::Value); 5] = [
        (
            "antigravity",
            Vec::new(),
            serde_json::json!({"toolCallId": "c1", "title": "Run mcp_amux_start_subagent",
                "name": "mcp_amux_start_subagent", "kind": "other", "status": "pending"}),
        ),
        (
            "opencode",
            vec![update(
                serde_json::json!({"sessionUpdate": "tool_call", "toolCallId": "c1",
                "title": "amux_message_agent", "kind": "other", "status": "pending", "rawInput": {}}),
            )],
            serde_json::json!({"toolCallId": "c1", "title": "send a message",
                "kind": "other", "status": "pending"}),
        ),
        (
            "grok",
            vec![update(
                serde_json::json!({"sessionUpdate": "tool_call", "toolCallId": "c1",
                "title": "use_tool",
                "rawInput": {"tool_name": "amux__list_agents", "tool_input": {}}}),
            )],
            serde_json::json!({"toolCallId": "c1"}),
        ),
        (
            "my-own-agent",
            Vec::new(),
            serde_json::json!({"toolCallId": "c1", "title": "amux__set_working_on"}),
        ),
        (
            "my-own-agent",
            Vec::new(),
            serde_json::json!({"toolCallId": "c1", "title": "Run mcp_amux_list_hosts"}),
        ),
    ];
    for (agent, before, call) in cases {
        let mut state = opened(agent, serde_json::json!({}));
        Acp::step(&mut state, prompt("a", "go"));
        for event in before {
            Acp::step(&mut state, event);
        }
        let stepped = Acp::step(&mut state, asked_about(7, call.clone()));
        assert!(allowed_unasked(&state, &stepped, 7), "{agent}: {call}");
    }
}

/// A call to amux's own tool from an agent that offers no one-time allow
/// asks as any other: allowing always would grant the session something
/// nobody chose.
#[test]
fn amuxs_own_tools_ask_where_only_always_is_offered() {
    let mut state = opened("antigravity", serde_json::json!({}));
    Acp::step(&mut state, prompt("a", "go"));
    let stepped = Acp::step(
        &mut state,
        rpc(
            serde_json::json!({"jsonrpc": "2.0", "id": 7, "method": "session/request_permission",
            "params": {"sessionId": "s1",
                "toolCall": {"toolCallId": "c1", "title": "Run mcp_amux_start_subagent",
                    "name": "mcp_amux_start_subagent", "kind": "other", "status": "pending"},
                "options": [{"optionId": "always", "name": "Always", "kind": "allow_always"},
                    {"optionId": "no", "name": "Deny", "kind": "reject_once"}]}}),
        ),
    );
    assert!(writes(&stepped.effects).is_empty());
    assert_eq!(state.shared().asks().open_asks().len(), 1);
}

/// Everything else asks: another server's tool, a name in another agent's
/// form, and amux's `attach`, which copies a file the agent names where the
/// agent reads unasked, so the agent's own settings see the read first.
#[test]
fn other_tools_and_attach_still_ask() {
    let cases = [
        (
            "antigravity",
            serde_json::json!({"toolCallId": "c1", "title": "Run mcp_spec_ask_the_operator",
            "name": "mcp_spec_ask_the_operator"}),
        ),
        (
            "antigravity",
            serde_json::json!({"toolCallId": "c1", "title": "amux_start_subagent"}),
        ),
        (
            "opencode",
            serde_json::json!({"toolCallId": "c1", "title": "amux__start_subagent"}),
        ),
        (
            "opencode",
            serde_json::json!({"toolCallId": "c1", "title": "amux_attach_file"}),
        ),
        (
            "grok",
            serde_json::json!({"toolCallId": "c1", "title": "use_tool",
            "rawInput": {"tool_name": "amux__attach_file"}}),
        ),
        (
            "my-own-agent",
            serde_json::json!({"toolCallId": "c1", "title": "./amux-spawn.sh"}),
        ),
    ];
    for (agent, call) in cases {
        let mut state = opened(agent, serde_json::json!({}));
        Acp::step(&mut state, prompt("a", "go"));
        let stepped = Acp::step(&mut state, asked_about(7, call.clone()));
        assert!(writes(&stepped.effects).is_empty(), "{agent}: {call}");
        assert_eq!(
            state.shared().asks().open_asks().len(),
            1,
            "{agent}: {call}"
        );
    }
}

fn image_prompt(id: &str) -> Event {
    Event::Input(wire::Input::new(
        id.as_bytes().to_vec(),
        wire::AcpInput {
            of: Some(wire::acp_input::Of::Prompt(wire::PromptInput {
                text: format!("what colour? {}", '\u{FFFC}'),
                attachments: vec![wire::Attachment {
                    of: Some(wire::attachment::Of::Image(wire::BlobRef {
                        hash: vec![0xab; 32],
                        name: "red.png".into(),
                        mime: "image/png".into(),
                        size: 8,
                    })),
                    ..Default::default()
                }],
            })),
        },
    ))
}

fn rejected_detail(effects: &[Effect], id: &str) -> Option<String> {
    effects.iter().find_map(|effect| match effect {
        Effect::Reply { input_id, verdict } if input_id == id.as_bytes() => match &verdict.of {
            Some(wire::send_input_response::Of::Rejected(rejected)) => rejected.detail.clone(),
            _ => None,
        },
        _ => None,
    })
}

/// An image goes to an agent whose prompts take images, with its bytes for
/// the agent process to put in; an agent that said its prompts take none
/// (Grok before 1.0.89) is refused it at once, in words naming the agent.
#[test]
fn an_image_is_refused_at_once_by_an_agent_that_takes_none() {
    let mut takes = opened(
        "grok",
        serde_json::json!({"promptCapabilities": {"image": true}}),
    );
    let sent = Acp::step(&mut takes, image_prompt("a"));
    assert!(!refused(&sent.effects, "a"));
    assert!(sent.effects.iter().any(|effect| matches!(effect,
        Effect::AcpPrompt { attachments, .. } if attachments.len() == 1)));

    let mut refuses = opened(
        "grok",
        serde_json::json!({"promptCapabilities": {"image": false}}),
    );
    let refusal = Acp::step(&mut refuses, image_prompt("a"));
    assert_eq!(
        rejected_detail(&refusal.effects, "a").as_deref(),
        Some("Grok does not take images")
    );
    assert!(writes(&refusal.effects).is_empty());
    assert!(
        !refusal
            .effects
            .iter()
            .any(|effect| matches!(effect, Effect::AcpPrompt { .. }))
    );
}

/// A conversation the agent cannot reopen ends at the next process's
/// `initialize`, with the cause that says so; no session is opened, so no
/// fresh conversation stands in for it.
#[test]
fn a_saved_conversation_no_process_can_reopen_ends_at_initialize() {
    let mut state = opened("my-own-agent", serde_json::json!({}));
    Acp::step(&mut state, Event::ProviderExit { code: Some(0) });
    let mut next = spec("my-own-agent");
    next.incarnation = 2;
    let (mut state, _) = Acp::reincarnate(state, &next);
    let stepped = Acp::step(
        &mut state,
        rpc(
            serde_json::json!({"jsonrpc": "2.0", "id": interpret::acp::INITIALIZE_REQUEST,
            "result": {"protocolVersion": 1, "agentCapabilities": {}}}),
        ),
    );
    assert_eq!(
        exit_cause(&stepped.effects).as_deref(),
        Some(interpret::acp::UNRESUMABLE)
    );
    assert!(writes(&stepped.effects).is_empty());
}

/// An `initialize` answer amux cannot read ends the agent rather than
/// leave it starting for good: no session request follows one.
#[test]
fn an_unreadable_initialize_answer_ends_the_agent() {
    let (mut state, _) = Acp::initial(&spec("my-own-agent"));
    let stepped = Acp::step(
        &mut state,
        rpc(
            serde_json::json!({"jsonrpc": "2.0", "id": interpret::acp::INITIALIZE_REQUEST,
            "result": {"protocolVersion": "one"}}),
        ),
    );
    assert!(exit_cause(&stepped.effects).is_some());
}

/// What an agent writes on stderr is logged, never drawn, and never ends
/// it.
#[test]
fn a_stderr_line_is_logged_and_drawn_nowhere() {
    let mut state = opened("grok", serde_json::json!({}));
    let stepped = Acp::step(
        &mut state,
        Event::Fact(interpret::Fact {
            channel: interpret::Channel::Agent,
            payload: interpret::acp::stderr_fact("ERROR worker quit with fatal: AuthRequired"),
        }),
    );
    assert!(stepped.step.items.is_empty());
    assert!(exit_cause(&stepped.effects).is_none());
    assert!(stepped.effects.iter().any(|effect| matches!(effect,
        Effect::Log { line } if line.contains("AuthRequired"))));
}

/// The `initialize` answer a recording holds.
fn recorded_initialize(recording: &str) -> InitializeResponse {
    serde_json::from_value(recorded_answer(recording, 1)).unwrap()
}

/// The result the agent answered the recording's request `id` with: 1 is
/// its `initialize`, 2 its `session/new`.
fn recorded_answer(recording: &str, id: u64) -> serde_json::Value {
    let io =
        std::fs::read_to_string(fixtures().join("live").join(recording).join("io.jsonl")).unwrap();
    io.lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|row| row["dir"] == "stdout")
        .filter_map(|row| serde_json::from_str::<serde_json::Value>(row["line"].as_str()?).ok())
        .find(|message| message["id"] == id && message.get("result").is_some())
        .map(|message| message["result"].clone())
        .unwrap_or_else(|| panic!("{recording} holds no answer to request {id}"))
}

/// `agy models` prints a line per model and effort (`agy-models.txt`, as
/// Antigravity 1.5.16 printed it): the host's copy has each model once, by
/// the value its sessions take, with the efforts it was printed with from
/// least to most, beside the modes amux knows Antigravity has. A model
/// printed with one effort keeps its whole id, which is what a session
/// takes (`gpt-oss-120b-medium`).
#[test]
fn agy_models_output_lists_antigravitys_models() {
    let printed = std::fs::read_to_string(fixtures().join("agy-models.txt")).unwrap();
    let copy = interpret::acp::listed_catalogue("antigravity", &printed);
    let models: Vec<(&str, &str, Vec<&str>)> = copy
        .models
        .iter()
        .map(|model| {
            (
                model.value.as_str(),
                model.display_name.as_str(),
                model.efforts.iter().map(String::as_str).collect(),
            )
        })
        .collect();
    let efforts = |efforts: &[&'static str]| efforts.to_vec();
    assert_eq!(
        models,
        [
            (
                "gemini-4-neon",
                "Gemini 4 Neon",
                efforts(&["low", "medium", "high", "xhigh"])
            ),
            (
                "gemini-4-carbon",
                "Gemini 4 Carbon",
                efforts(&["low", "medium", "high"])
            ),
            (
                "gemini-4-argon",
                "Gemini 4 Argon",
                efforts(&["low", "medium", "high"])
            ),
            (
                "gemini-4-hydrogen",
                "Gemini 4 Hydrogen",
                efforts(&["low", "medium", "high", "xhigh"])
            ),
            (
                "gemini-pro-orchestrator-neon",
                "Orchestrator (Neon)",
                efforts(&[])
            ),
            (
                "gemini-pro-orchestrator",
                "Orchestrator (Carbon)",
                efforts(&[])
            ),
            (
                "gemini-pro-orchestrator-argon",
                "Orchestrator (Argon)",
                efforts(&[])
            ),
            (
                "gemini-3.8-flash",
                "Gemini 3.8 Flash",
                efforts(&["low", "medium", "high"])
            ),
            (
                "claude-opus-5-5",
                "Claude Opus 5.5",
                efforts(&["low", "medium", "high"])
            ),
            (
                "claude-sonnet-5-5",
                "Claude Sonnet 5.5",
                efforts(&["low", "medium", "high"])
            ),
            ("gpt-oss-120b-medium", "GPT-OSS 120B (Medium)", efforts(&[])),
        ]
    );
    // The modes amux knows it has are its permissions.
    assert!(copy.modes.is_empty());
    let permissions: Vec<(&str, bool, wire::PermissionLevel)> = copy
        .permissions
        .iter()
        .map(|permission| {
            (
                permission.value.as_str(),
                permission.normal,
                permission.level(),
            )
        })
        .collect();
    assert_eq!(
        permissions,
        [
            ("plan", false, wire::PermissionLevel::ReadOnly),
            ("default", false, wire::PermissionLevel::Ask),
            ("accept-edits", false, wire::PermissionLevel::Auto),
            ("auto", false, wire::PermissionLevel::Auto),
        ]
    );
}

/// `opencode models` prints one model id per line: each is a model with no
/// efforts, beside the modes amux knows OpenCode has, none called normal.
/// A line that is not an id is passed over.
#[test]
fn opencode_models_output_lists_its_models() {
    let printed = "opencode/beam-free\nopencode/big-pickle\n\n  opencode/flint-40b-free  \nWARN cache is old\n";
    let copy = interpret::acp::listed_catalogue("opencode", printed);
    let models: Vec<&str> = copy.models.iter().map(|m| m.value.as_str()).collect();
    assert_eq!(
        models,
        [
            "opencode/beam-free",
            "opencode/big-pickle",
            "opencode/flint-40b-free"
        ]
    );
    assert!(copy.models.iter().all(|model| model.efforts.is_empty()));
    // The modes amux knows it has are its permissions.
    assert!(copy.modes.is_empty());
    let permissions: Vec<(&str, bool, wire::PermissionLevel)> = copy
        .permissions
        .iter()
        .map(|permission| {
            (
                permission.value.as_str(),
                permission.normal,
                permission.level(),
            )
        })
        .collect();
    assert_eq!(
        permissions,
        [
            ("plan", false, wire::PermissionLevel::ReadOnly),
            ("build", false, wire::PermissionLevel::Full),
        ]
    );
    assert!(copy.commands.is_empty());
    // An agent amux knows no modes of lists its models alone.
    let added = interpret::acp::listed_catalogue("my-agent", "m1\n");
    assert!(added.modes.is_empty() && added.permissions.is_empty());
}

/// Each built-in lists its models without a conversation its own way, and
/// an agent a person added only in one.
#[test]
fn each_built_in_lists_its_models_its_own_way() {
    use interpret::acp::{ModelListing, Printed};
    assert_eq!(quirks("grok").lists_models, ModelListing::Initialize);
    assert_eq!(
        quirks("antigravity").lists_models,
        ModelListing::Command {
            args: &["models"],
            printed: Printed::IdsByEffort
        }
    );
    assert_eq!(
        quirks("opencode").lists_models,
        ModelListing::Command {
            args: &["models"],
            printed: Printed::Ids
        }
    );
    assert_eq!(quirks("my-agent").lists_models, ModelListing::InSession);
}

/// The host's copy of each built-in, from its recorded `initialize` alone:
/// Grok lists its models, efforts and commands there and shows whether it
/// is signed in; Antigravity and OpenCode list nothing a session would
/// there, which the helper asks them for in their own ways, and
/// `initialize` cannot tell whether they are signed in.
#[test]
fn the_hosts_copy_comes_from_initialize_alone() {
    let grok = interpret::acp::host_offer("grok", &recorded_initialize("grok-catalogue"));
    assert_eq!(grok.version, "1.0.89");
    assert_eq!(grok.acp.reopen, wire::AcpReopen::Resume as i32);
    assert_eq!(grok.acp.sign_in, wire::SignInState::SignedIn as i32);
    assert!(grok.signed_in());
    let models: Vec<&str> = grok
        .catalogue
        .models
        .iter()
        .map(|model| model.value.as_str())
        .collect();
    assert!(models.contains(&"grok-4.7-build-fast"), "{models:?}");
    assert!(
        grok.catalogue
            .models
            .iter()
            .all(|model| model.efforts.contains(&"low".to_owned()))
    );
    assert!(
        grok.catalogue
            .commands
            .iter()
            .any(|command| command.name == "compact")
    );
    let new_agent = grok.catalogue.new_agent.clone().unwrap();
    assert!(models.contains(&new_agent.model.as_deref().unwrap()));
    assert!(new_agent.effort.is_some());

    // Grok 1.0.89 resumes and takes images; it queues a mid-turn prompt
    // itself rather than steering.
    assert_eq!(
        grok.capabilities,
        wire::AgentCapabilities::acp(true, true, false)
    );

    let signed_out = interpret::acp::host_offer("grok", &recorded_initialize("grok-signed_out"));
    assert_eq!(signed_out.acp.sign_in, wire::SignInState::SignedOut as i32);
    assert!(!signed_out.signed_in());

    for (agent, recording, name) in [
        ("antigravity", "agy-catalogue", "Antigravity ACP Server"),
        ("opencode", "opencode-catalogue", "OpenCode"),
    ] {
        let offer = interpret::acp::host_offer(agent, &recorded_initialize(recording));
        assert_eq!(offer.acp.name, name);
        assert!(!offer.version.is_empty());
        assert_eq!(offer.acp.reopen, wire::AcpReopen::Resume as i32);
        assert!(offer.acp.images && offer.acp.embedded_context);
        assert_eq!(
            offer.capabilities,
            wire::AgentCapabilities {
                forks: agent == "opencode",
                ..wire::AgentCapabilities::acp(true, true, agent == "antigravity")
            },
            "{agent}: it resumes and takes images; only Antigravity steers, and only OpenCode forks"
        );
        assert_eq!(offer.acp.sign_in, wire::SignInState::Unspecified as i32);
        assert!(offer.signed_in());
        assert!(offer.catalogue.models.is_empty());
        assert!(!offer.acp.sign_in_methods.is_empty());
    }
    let opencode =
        interpret::acp::host_offer("opencode", &recorded_initialize("opencode-catalogue"));
    assert_eq!(
        opencode.acp.sign_in_methods[0].description,
        "Run `opencode auth login` in the terminal"
    );
}

/// Antigravity's question tool writes its choices as titled constants
/// (`agy-question`): the ask offers them by title, and the title chosen
/// reaches the agent as the constant it asked for, not as typed text it
/// would take for no answer.
#[test]
fn a_forms_titled_choices_are_offered_by_title_and_sent_as_constants() {
    let mut state = opened("antigravity", serde_json::json!({}));
    Acp::step(&mut state, prompt("a", "ask me"));
    Acp::step(
        &mut state,
        rpc(
            serde_json::json!({"jsonrpc": "2.0", "id": "3", "method": "elicitation/create",
            "params": {"sessionId": "s1", "toolCallId": "c1", "mode": "form",
                "message": "The agent has a question.",
                "requestedSchema": {"type": "object", "properties": {
                    "q1": {"type": "string", "title": "Tea or coffee?",
                        "oneOf": [{"const": "1", "title": "Tea"}, {"const": "2", "title": "Coffee"}]},
                    "q1_other": {"type": "string", "title": "Other"}}}}}),
        ),
    );
    let asked = &state.shared().asks().open_asks()[0];
    let Some(wire::acp_ask::Body::Form(form)) = &asked.body else {
        panic!("not a form: {asked:?}");
    };
    let schema: serde_json::Value = serde_json::from_slice(&form.schema_json).unwrap();
    assert_eq!(
        schema["properties"]["q1"]["enum"],
        serde_json::json!(["Tea", "Coffee"])
    );
    let key = asked.key.clone();
    let answered = Acp::step(
        &mut state,
        Event::Input(wire::Input::new(
            b"f".to_vec(),
            wire::AcpInput {
                of: Some(wire::acp_input::Of::Answer(wire::AcpAnswer {
                    ask_key: key,
                    of: Some(wire::acp_answer::Of::Form(wire::FormAnswer {
                        action: wire::FormAction::Accept as i32,
                        content_json: br#"{"q1":"Coffee"}"#.to_vec(),
                    })),
                })),
            },
        )),
    );
    assert_eq!(
        writes(&answered.effects),
        vec![serde_json::json!({"jsonrpc": "2.0", "id": "3",
            "result": {"action": "accept", "content": {"q1": "2"}}})]
    );
}

/// Grok's question request, two questions, the first taking several.
fn grok_questions(id: u64) -> Event {
    rpc(
        serde_json::json!({"jsonrpc": "2.0", "id": id, "method": "_x.ai/ask_user_question",
        "params": {"sessionId": "s1", "toolCallId": "c1", "mode": "default", "questions": [
            {"question": "Which drinks?", "multiSelect": true,
                "options": [{"label": "Tea"}, {"label": "Coffee"}, {"label": "Juice"}]},
            {"question": "Hot or cold?", "multiSelect": null,
                "options": [{"label": "Hot"}, {"label": "Cold"}]}]}}),
    )
}

fn answer_questions(ask: &str, answers: Vec<wire::QuestionResponse>) -> Event {
    Event::Input(wire::Input::new(
        b"q".to_vec(),
        wire::AcpInput {
            of: Some(wire::acp_input::Of::Answer(wire::AcpAnswer {
                ask_key: ask.to_owned(),
                of: Some(wire::acp_answer::Of::Question(wire::QuestionAnswer {
                    answers,
                })),
            })),
        },
    ))
}

/// Grok's questions are a question ask; the answer names the labels picked,
/// a list for a question taking several, and the words typed for one
/// answered in the person's own words.
#[test]
fn groks_questions_are_answered_by_label_and_typed_words() {
    let mut state = opened("grok", serde_json::json!({}));
    Acp::step(&mut state, prompt("a", "ask me"));
    Acp::step(&mut state, grok_questions(4));
    let asked = &state.shared().asks().open_asks()[0];
    let Some(wire::acp_ask::Body::Question(question)) = &asked.body else {
        panic!("not a question: {asked:?}");
    };
    assert!(question.questions[0].multi_select);
    assert!(!question.questions[1].multi_select);
    assert!(
        question
            .questions
            .iter()
            .all(|question| question.allow_other)
    );
    let key = asked.key.clone();
    let answered = Acp::step(
        &mut state,
        answer_questions(
            &key,
            vec![
                wire::QuestionResponse {
                    selected: vec![0, 2],
                    ..Default::default()
                },
                wire::QuestionResponse {
                    other: Some("lukewarm".into()),
                    ..Default::default()
                },
            ],
        ),
    );
    assert_eq!(
        writes(&answered.effects),
        vec![serde_json::json!({"jsonrpc": "2.0", "id": 4, "result": {
            "outcome": "accepted",
            "answers": {"Which drinks?": ["Tea", "Juice"], "Hot or cold?": "lukewarm"}}})]
    );
    assert!(state.shared().asks().is_empty());
}

/// Every question skipped is a decline, which Grok goes on from with its
/// own judgment.
#[test]
fn groks_questions_all_skipped_are_declined() {
    let mut state = opened("grok", serde_json::json!({}));
    Acp::step(&mut state, prompt("a", "ask me"));
    Acp::step(&mut state, grok_questions(4));
    let key = state.shared().asks().open_asks()[0].key.clone();
    let answered = Acp::step(
        &mut state,
        answer_questions(&key, vec![wire::QuestionResponse::default(); 2]),
    );
    assert_eq!(
        writes(&answered.effects),
        vec![serde_json::json!({"jsonrpc": "2.0", "id": 4, "result": {"outcome": "cancelled"}})]
    );
}

/// Grok's request is Grok's: another agent sending it is told the method
/// is not found, and nothing opens.
#[test]
fn groks_question_request_is_refused_from_another_agent() {
    let mut state = opened("opencode", serde_json::json!({}));
    Acp::step(&mut state, prompt("a", "ask me"));
    let asked = Acp::step(&mut state, grok_questions(4));
    let written = writes(&asked.effects);
    assert_eq!(written.len(), 1, "{written:?}");
    assert_eq!(written[0]["error"]["code"], -32601);
    assert!(state.shared().asks().is_empty());
}

/// The ask each emission of call `id` in a step names as standing for it:
/// None where the call was not emitted, empty where it names none.
fn call_asks(stepped: &interpret::Stepped, id: &str) -> Option<String> {
    use prost::Message as _;
    stepped
        .step
        .items
        .iter()
        .filter(|item| item.key == format!("tool:{id}"))
        .filter_map(
            |item| match wire::AcpItem::decode(item.body.as_slice()).ok()?.kind {
                Some(wire::acp_item::Kind::Tool(tool)) => Some(tool.ask_item_key),
                _ => None,
            },
        )
        .next_back()
}

/// A question tool's call, which the agent's form names as its own, names
/// the form's ask in return, from the ask on and for the rest of the call,
/// so no client draws it as a step beside the question (`agy-question`).
#[test]
fn a_call_a_form_names_is_drawn_by_the_forms_ask() {
    let mut state = opened("antigravity", serde_json::json!({}));
    Acp::step(&mut state, prompt("a", "ask me"));
    let called = Acp::step(
        &mut state,
        update(
            serde_json::json!({"sessionUpdate": "tool_call", "toolCallId": "c1",
            "title": "Run ask_question", "kind": "other", "status": "pending"}),
        ),
    );
    assert_eq!(call_asks(&called, "c1").as_deref(), Some(""));
    let asked = Acp::step(
        &mut state,
        rpc(
            serde_json::json!({"jsonrpc": "2.0", "id": "3", "method": "elicitation/create",
            "params": {"sessionId": "s1", "toolCallId": "c1", "mode": "form",
                "message": "The agent has a question.",
                "requestedSchema": {"type": "object", "properties": {
                    "q1": {"type": "string", "title": "Tea or coffee?"}}}}}),
        ),
    );
    let ask = state.shared().asks().open_asks()[0].item_key.clone();
    assert_eq!(ask, "ask:ask-1");
    assert_eq!(call_asks(&asked, "c1"), Some(ask.clone()));
    let done = Acp::step(
        &mut state,
        update(
            serde_json::json!({"sessionUpdate": "tool_call_update", "toolCallId": "c1",
            "status": "completed"}),
        ),
    );
    assert_eq!(call_asks(&done, "c1"), Some(ask));
}

/// Grok names its question tool's call too, and the call's first word may
/// come after the question (`grok-question`); any other call stays a step.
#[test]
fn a_call_named_by_groks_question_is_drawn_by_it_even_when_it_comes_later() {
    let mut state = opened("grok", serde_json::json!({}));
    Acp::step(&mut state, prompt("a", "ask me"));
    let asked = Acp::step(&mut state, grok_questions(4));
    assert_eq!(call_asks(&asked, "c1"), None);
    let called = Acp::step(
        &mut state,
        update(
            serde_json::json!({"sessionUpdate": "tool_call", "toolCallId": "c1",
            "title": "ask_user_question", "kind": "other", "status": "pending"}),
        ),
    );
    assert_eq!(call_asks(&called, "c1").as_deref(), Some("ask:ask-1"));
    let other = Acp::step(
        &mut state,
        update(
            serde_json::json!({"sessionUpdate": "tool_call", "toolCallId": "c2",
            "title": "read_file", "kind": "read", "status": "pending"}),
        ),
    );
    assert_eq!(call_asks(&other, "c2").as_deref(), Some(""));
}

/// A piece of the agent's reply, as `agent_message_chunk` sends one.
fn said_block(block: serde_json::Value) -> Event {
    update(serde_json::json!({"sessionUpdate": "agent_message_chunk", "content": block}))
}

fn said_text(text: &str) -> Event {
    said_block(serde_json::json!({"type": "text", "text": text}))
}

/// A blob written: its hash and its bytes.
type Blob = (Vec<u8>, Vec<u8>);

/// Steps `events` in turn, then ends the reply (a thought ends a stream of
/// words): every item the steps emitted, newest last, and every blob
/// written.
fn reply_of(state: &mut interpret::acp::State, events: Vec<Event>) -> (Vec<wire::Item>, Vec<Blob>) {
    let mut items = Vec::new();
    let mut blobs = Vec::new();
    let end = update(serde_json::json!({"sessionUpdate": "agent_thought_chunk",
        "content": {"type": "text", "text": "next"}}));
    for event in events.into_iter().chain([end]) {
        let stepped = Acp::step(state, event);
        items.extend(stepped.step.items);
        blobs.extend(
            stepped
                .effects
                .into_iter()
                .filter_map(|effect| match effect {
                    Effect::WriteBlob { hash, bytes } => Some((hash, bytes)),
                    _ => None,
                }),
        );
    }
    (items, blobs)
}

/// The newest emission of the reply's message item.
fn final_message(items: &[wire::Item]) -> wire::Item {
    items
        .iter()
        .rev()
        .find(|item| item.key.starts_with("message"))
        .cloned()
        .expect("a reply")
}

fn blob_of(attachment: &wire::Attachment) -> (&'static str, wire::BlobRef) {
    match attachment.of.clone() {
        Some(wire::attachment::Of::Image(blob)) => ("image", blob),
        Some(wire::attachment::Of::File(blob)) => ("file", blob),
        other => panic!("not a blob: {other:?}"),
    }
}

fn base64(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn sha256(bytes: &[u8]) -> Vec<u8> {
    use sha2::Digest as _;
    sha2::Sha256::digest(bytes).to_vec()
}

/// What an agent presents in its reply (an image, a sound, an embedded
/// file) is an attachment in its place among the words: a placeholder in
/// the text, the attachment in order, and its blob written by its hash.
/// The agent's own words cannot fake a placeholder.
#[test]
fn what_an_agent_presents_in_its_reply_is_attached_in_its_place() {
    let mut state = opened("opencode", serde_json::json!({}));
    Acp::step(&mut state, prompt("a", "show me"));
    let png = b"\x89PNG\r\n\x1a\nfake";
    let wav = b"RIFFfake";
    let (items, blobs) = reply_of(
        &mut state,
        vec![
            said_text("Here \u{FFFC}is the chart: "),
            said_block(
                serde_json::json!({"type": "image", "mimeType": "image/png", "data": base64(png)}),
            ),
            said_text(" and the sound "),
            said_block(
                serde_json::json!({"type": "audio", "mimeType": "audio/wav", "data": base64(wav)}),
            ),
            said_text(" and the notes "),
            said_block(serde_json::json!({"type": "resource", "resource": {
                "uri": "file:///work/notes.md?v=2", "mimeType": "text/markdown", "text": "# Notes"}})),
            said_text("."),
        ],
    );
    let reply = final_message(&items);
    assert_eq!(
        reply.text,
        "Here \u{FFFD}is the chart: \u{FFFC} and the sound \u{FFFC} and the notes \u{FFFC}."
    );
    let attached: Vec<_> = reply.attachments.iter().map(blob_of).collect();
    let summary: Vec<_> = attached
        .iter()
        .map(|(kind, blob)| (*kind, blob.name.as_str(), blob.mime.as_str(), blob.size))
        .collect();
    assert_eq!(
        summary,
        vec![
            ("image", "image-1.png", "image/png", png.len() as u64),
            ("file", "audio-2.wav", "audio/wav", wav.len() as u64),
            ("file", "notes.md", "text/markdown", 7),
        ]
    );
    for (_, blob) in &attached {
        assert!(blobs.iter().any(|(hash, _)| *hash == blob.hash), "{blob:?}");
    }
    assert_eq!(attached[0].1.hash, sha256(png));
    assert!(blobs.contains(&(sha256(b"# Notes"), b"# Notes".to_vec())));
}

/// A link stays the link's words, a local file's too: storing a file the
/// agent only names would read it unasked into the agent's blobs, which
/// the agent reads unasked and every device is sent, around the permission
/// amux's attach tool asks for. Nothing is attached and nothing is read.
#[test]
fn a_link_in_a_reply_stays_its_words() {
    let mut state = opened("opencode", serde_json::json!({}));
    Acp::step(&mut state, prompt("a", "show me"));
    let link = |name: &str, uri: &str| {
        said_block(serde_json::json!({"type": "resource_link", "name": name, "uri": uri}))
    };
    let (items, blobs) = reply_of(
        &mut state,
        vec![
            said_text("See "),
            link("chart.png", "file:///work/my%20chart.png"),
            said_text(" and "),
            link("site", "https://example.com/"),
        ],
    );
    let reply = final_message(&items);
    assert_eq!(
        reply.text,
        "See [chart.png](file:///work/my%20chart.png) and [site](https://example.com/)"
    );
    assert!(reply.attachments.is_empty());
    assert!(blobs.is_empty());
}

/// A model that writes an element in its reply gets what it names, as on
/// every kind: the element becomes a placeholder and an attachment, beside
/// what the agent presented.
#[test]
fn an_element_in_an_acp_reply_becomes_its_attachment() {
    let mut state = opened("opencode", serde_json::json!({}));
    Acp::step(&mut state, prompt("a", "attach it"));
    let hash = "ab".repeat(32);
    let element = format!(
        r#"<amux-attachment kind="file" hash="sha256:{hash}" name="trace.json" mime="application/json" size="12" path="/x/blobs/{hash}"/>"#
    );
    let (items, _) = reply_of(
        &mut state,
        vec![
            said_block(
                serde_json::json!({"type": "image", "mimeType": "image/png", "data": base64(b"png")}),
            ),
            said_text(&format!("Here is the trace: {element} done.")),
        ],
    );
    let reply = final_message(&items);
    assert_eq!(reply.text, "\u{FFFC}Here is the trace: \u{FFFC} done.");
    let names: Vec<_> = reply
        .attachments
        .iter()
        .map(|attachment| blob_of(attachment).1.name)
        .collect();
    assert_eq!(names, ["image-1.png", "trace.json"]);
}

/// amux's attach tool's step carries the attachment from the element it
/// answered, by each agent's way of naming the tool; any other call's
/// output (an image a tool read, or a file it returned) is what the tool
/// saw, never attached, and writes no blob.
#[test]
fn only_the_attach_tools_step_carries_an_attachment() {
    let hash = "cd".repeat(32);
    let element = format!(
        r#"<amux-attachment kind="image" hash="sha256:{hash}" name="dot.png" mime="image/png" size="68" path="/x/blobs/{hash}"/>"#
    );
    for (provider, title, input) in [
        (
            "opencode",
            "amux_attach_file",
            serde_json::json!({"path": "dot.png"}),
        ),
        (
            "antigravity",
            "Run mcp_amux_attach_file",
            serde_json::json!({"path": "dot.png"}),
        ),
        (
            "grok",
            "use_tool",
            serde_json::json!({"tool_name": "amux__attach_file"}),
        ),
    ] {
        let mut state = opened(provider, serde_json::json!({}));
        Acp::step(&mut state, prompt("a", "attach it"));
        let mut items = Vec::new();
        let mut writes = 0;
        for event in [
            update(
                serde_json::json!({"sessionUpdate": "tool_call", "toolCallId": "c1",
                "title": title, "kind": "other", "status": "in_progress", "rawInput": input}),
            ),
            update(
                serde_json::json!({"sessionUpdate": "tool_call_update", "toolCallId": "c1",
                "status": "completed", "content": [{"type": "content",
                    "content": {"type": "text", "text": element}}]}),
            ),
            update(
                serde_json::json!({"sessionUpdate": "tool_call", "toolCallId": "c2",
                "title": "read_file", "kind": "read", "status": "completed", "content": [
                    {"type": "content", "content": {"type": "image", "mimeType": "image/png",
                        "data": base64(b"seen")}},
                    {"type": "content", "content": {"type": "resource", "resource": {
                        "uri": "file:///work/seen.txt", "text": "seen"}}},
                    {"type": "content", "content": {"type": "text", "text": element}}]}),
            ),
        ] {
            let stepped = Acp::step(&mut state, event);
            items.extend(stepped.step.items);
            writes += stepped
                .effects
                .iter()
                .filter(|effect| matches!(effect, Effect::WriteBlob { .. }))
                .count();
        }
        let newest = |key: &str| {
            items
                .iter()
                .rev()
                .find(|item| item.key == key)
                .cloned()
                .unwrap_or_else(|| panic!("{provider}: no {key}"))
        };
        let attached = newest("tool:c1");
        assert_eq!(attached.text, "\u{FFFC}", "{provider}");
        assert_eq!(
            blob_of(&attached.attachments[0]).1.name,
            "dot.png",
            "{provider}"
        );
        let read = newest("tool:c2");
        assert!(read.attachments.is_empty(), "{provider}: {read:?}");
        assert!(!read.text.contains('\u{FFFC}'), "{provider}");
        assert_eq!(writes, 0, "{provider}");
    }
}

/// The newest emission of call `id` in a step: the call and its words.
fn emitted_call(stepped: &interpret::Stepped, id: &str) -> Option<(wire::AcpTool, String)> {
    use prost::Message as _;
    stepped
        .step
        .items
        .iter()
        .rev()
        .filter(|item| item.key == format!("tool:{id}"))
        .find_map(
            |item| match wire::AcpItem::decode(item.body.as_slice()).ok()?.kind {
                Some(wire::acp_item::Kind::Tool(tool)) => Some((tool, item.text.clone())),
                _ => None,
            },
        )
}

/// Ends the running turn: the agent answers the prompt amux sent.
fn turn_ends(state: &mut interpret::acp::State, prompted: &interpret::Stepped) {
    let id = writes(&prompted.effects)
        .into_iter()
        .find(|written| written["method"] == "session/prompt")
        .expect("a prompt was sent")["id"]
        .clone();
    Acp::step(
        state,
        rpc(serde_json::json!({"jsonrpc": "2.0", "id": id,
            "result": {"stopReason": "end_turn"}})),
    );
}

/// A call still running when its turn ends is settled there, cancelled,
/// and its row is drawn for good. An update the agent sends for it later
/// is dropped, not taken for a new call that opens a blank running row.
#[test]
fn a_late_update_for_a_call_its_turn_settled_is_dropped() {
    let mut state = opened("opencode", serde_json::json!({}));
    let prompted = Acp::step(&mut state, prompt("a", "go"));
    Acp::step(
        &mut state,
        update(
            serde_json::json!({"sessionUpdate": "tool_call", "toolCallId": "c1",
            "title": "ls", "kind": "execute", "status": "in_progress"}),
        ),
    );
    turn_ends(&mut state, &prompted);
    let late = Acp::step(
        &mut state,
        update(
            serde_json::json!({"sessionUpdate": "tool_call_update", "toolCallId": "c1",
            "status": "completed", "content": [{"type": "content",
                "content": {"type": "text", "text": "late words"}}]}),
        ),
    );
    assert_eq!(emitted_call(&late, "c1"), None, "{:?}", late.step.items);
}

/// A command's exit code and a cut to its output are read from the call's
/// raw output under the names agents give them: OpenCode's
/// `metadata.exit` and `metadata.truncated`, Antigravity's `exit_code`.
#[test]
fn a_commands_exit_code_comes_from_its_raw_output() {
    let mut state = opened("opencode", serde_json::json!({}));
    Acp::step(&mut state, prompt("a", "go"));
    for (id, raw, code, truncated) in [
        (
            "c1",
            serde_json::json!({"output": "…", "metadata": {"exit": 2, "truncated": true}}),
            Some(2),
            true,
        ),
        (
            "c2",
            serde_json::json!({"exit_code": 0, "output": "done"}),
            Some(0),
            false,
        ),
        ("c3", serde_json::json!({"output": "no code"}), None, false),
    ] {
        Acp::step(
            &mut state,
            update(
                serde_json::json!({"sessionUpdate": "tool_call", "toolCallId": id,
                "title": "run", "kind": "execute", "status": "in_progress"}),
            ),
        );
        let ended = Acp::step(
            &mut state,
            update(
                serde_json::json!({"sessionUpdate": "tool_call_update", "toolCallId": id,
                "status": "completed", "rawOutput": raw}),
            ),
        );
        let (tool, _) = emitted_call(&ended, id).unwrap();
        assert_eq!(
            (tool.exit_code, tool.output_truncated),
            (code, truncated),
            "{id}"
        );
    }
}

/// A change keeps only the lines its texts do not share, so OpenCode's
/// whole file, sent on every update, is not sent on to every device; where
/// its raw output's patch says where the change starts, the change is
/// numbered from there. Grok's lines in the diff's `_meta` say where its
/// fragment starts, and move past the lines the fragment shares.
#[test]
fn a_change_keeps_its_own_lines_and_the_place_its_agent_says() {
    let file: String = (1..=60).map(|n| format!("line {n}\n")).collect();
    let edited = file.replace("line 41\n", "line forty-one\n");
    let mut state = opened("opencode", serde_json::json!({}));
    Acp::step(&mut state, prompt("a", "edit"));
    let patch = "Index: /work/f.txt\n===\n--- /work/f.txt\n+++ /work/f.txt\n\
        @@ -38,7 +38,7 @@\n line 38\n line 39\n line 40\n-line 41\n+line forty-one\n line 42\n";
    let ended = Acp::step(
        &mut state,
        update(
            serde_json::json!({"sessionUpdate": "tool_call", "toolCallId": "c1",
            "title": "edit", "kind": "edit", "status": "completed",
            "content": [{"type": "diff", "path": "/work/f.txt", "oldText": file, "newText": edited}],
            "rawOutput": {"metadata": {"filediff": {"file": "/work/f.txt", "patch": patch}}}}),
        ),
    );
    let (tool, _) = emitted_call(&ended, "c1").unwrap();
    let diff = &tool.diffs[0];
    assert_eq!(diff.old_text.as_deref(), Some("line 41\n"));
    assert_eq!(diff.new_text, "line forty-one\n");
    assert_eq!((diff.old_line, diff.new_line), (Some(41), Some(41)));

    let mut grok = opened("grok", serde_json::json!({}));
    Acp::step(&mut grok, prompt("a", "edit"));
    let edited = Acp::step(
        &mut grok,
        update(
            serde_json::json!({"sessionUpdate": "tool_call", "toolCallId": "c1",
            "title": "edit", "kind": "edit", "status": "completed",
            "content": [{"type": "diff", "path": "/work/f.txt",
                "oldText": "same\nhello\n", "newText": "same\ngoodbye\n",
                "_meta": {"old_line": 9, "new_line": 9}}]}),
        ),
    );
    let (tool, _) = emitted_call(&edited, "c1").unwrap();
    let diff = &tool.diffs[0];
    assert_eq!(diff.old_text.as_deref(), Some("hello\n"));
    assert_eq!((diff.old_line, diff.new_line), (Some(10), Some(10)));
}

/// A change's texts and a call's input are cut as output is, so a file an
/// agent writes whole does not go to every device whole. A change keeps the
/// same number of lines on both sides, as many as fit the cap on each, so
/// what is kept is still a true part of it, and says it was cut; a long
/// string in the input keeps its first cap's worth and ends in an
/// ellipsis, and the input is still JSON a client reads.
#[test]
fn a_calls_long_texts_are_cut_as_output_is() {
    let cap = interpret::OUTPUT_CAP;
    let long: String = "x".repeat(99) + "\n";
    let long = long.repeat(3 * cap / 100);
    let mut state = opened("opencode", serde_json::json!({}));
    Acp::step(&mut state, prompt("a", "write"));
    let written = Acp::step(
        &mut state,
        update(
            serde_json::json!({"sessionUpdate": "tool_call", "toolCallId": "c1",
            "title": "write", "kind": "edit", "status": "completed",
            "rawInput": {"filePath": "/work/big.txt", "content": long},
            "content": [{"type": "diff", "path": "/work/big.txt", "oldText": null, "newText": long}]}),
        ),
    );
    let (tool, _) = emitted_call(&written, "c1").unwrap();
    let diff = &tool.diffs[0];
    assert!(diff.truncated);
    assert!(diff.new_text.len() <= cap, "{}", diff.new_text.len());
    assert!(diff.new_text.ends_with('\n'));
    let input: serde_json::Value = serde_json::from_slice(&tool.input_json).unwrap();
    assert_eq!(input["filePath"], "/work/big.txt");
    let content = input["content"].as_str().unwrap();
    assert!(content.len() <= cap + '…'.len_utf8(), "{}", content.len());
    assert!(
        content.ends_with("\n…"),
        "{:?}",
        &content[content.len() - 8..]
    );

    // An edit whose sides differ in length: short lines replaced by long
    // ones. Both keep the same number of lines.
    let old: String = (0..5000).map(|n| format!("{n}\n")).collect();
    let new: String = (0..5000)
        .map(|n| format!("{n} {}\n", "y".repeat(60)))
        .collect();
    let edited = Acp::step(
        &mut state,
        update(
            serde_json::json!({"sessionUpdate": "tool_call", "toolCallId": "c2",
            "title": "edit", "kind": "edit", "status": "completed",
            "content": [{"type": "diff", "path": "/work/f.txt", "oldText": old, "newText": new}]}),
        ),
    );
    let (tool, _) = emitted_call(&edited, "c2").unwrap();
    let diff = &tool.diffs[0];
    assert!(diff.truncated);
    let kept_old = diff.old_text.as_deref().unwrap().lines().count();
    let kept_new = diff.new_text.lines().count();
    assert_eq!(kept_old, kept_new);
    assert!(kept_new > 0 && kept_new < 5000, "{kept_new}");
    assert!(diff.new_text.len() <= cap);
    // A change short enough is kept whole and says nothing was cut.
    let small = Acp::step(
        &mut state,
        update(
            serde_json::json!({"sessionUpdate": "tool_call", "toolCallId": "c3",
            "title": "edit", "kind": "edit", "status": "completed",
            "content": [{"type": "diff", "path": "/work/f.txt", "oldText": "a\n", "newText": "b\n"}]}),
        ),
    );
    assert!(!emitted_call(&small, "c3").unwrap().0.diffs[0].truncated);
}

/// While Grok's own reviewer decides on a call, the call waits, and its
/// content (the call's description) is not its output; a call the reviewer
/// blocks never ran, so it is denied, not failed (`grok-allow`).
#[test]
fn a_call_groks_reviewer_decides_on_waits_and_a_blocked_one_is_denied() {
    let mut state = opened("grok", serde_json::json!({}));
    Acp::step(&mut state, prompt("a", "run it"));
    let xai = |update: serde_json::Value| {
        rpc(
            serde_json::json!({"jsonrpc": "2.0", "method": "_x.ai/session_notification",
            "params": {"sessionId": "s1", "update": update}}),
        )
    };
    Acp::step(
        &mut state,
        update(
            serde_json::json!({"sessionUpdate": "tool_call", "toolCallId": "c1",
            "title": "run_terminal_command", "rawInput": {"command": "./script.sh"}}),
        ),
    );
    let reviewing = Acp::step(
        &mut state,
        xai(
            serde_json::json!({"sessionUpdate": "pending_interaction", "kind": "permission",
            "tool_call_id": "c1"}),
        ),
    );
    let (tool, _) = emitted_call(&reviewing, "c1").unwrap();
    assert_eq!(tool.state(), wire::ToolState::Pending);
    let described = Acp::step(
        &mut state,
        update(
            serde_json::json!({"sessionUpdate": "tool_call_update", "toolCallId": "c1",
            "kind": "execute", "content": [{"type": "content",
                "content": {"type": "text", "text": "Run the script"}}]}),
        ),
    );
    let (tool, words) = emitted_call(&described, "c1").unwrap();
    assert_eq!(
        (tool.state(), words.as_str()),
        (wire::ToolState::Pending, "")
    );
    Acp::step(
        &mut state,
        xai(serde_json::json!({"sessionUpdate": "interaction_resolved", "tool_call_id": "c1"})),
    );
    let blocked = Acp::step(
        &mut state,
        update(
            serde_json::json!({"sessionUpdate": "tool_call_update", "toolCallId": "c1",
            "status": "failed", "content": [{"type": "content", "content": {"type": "text",
                "text": "Tool `run_terminal_command` was not executed: Auto mode blocked this action (unknown script)."}}]}),
        ),
    );
    let (tool, words) = emitted_call(&blocked, "c1").unwrap();
    assert_eq!(tool.state(), wire::ToolState::Denied);
    assert!(words.contains("Auto mode blocked"), "{words}");
}
