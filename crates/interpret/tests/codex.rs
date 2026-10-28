//! The Codex interpreter against the recorded app-server sessions in
//! codex-specs and authored facts for what no recording shows.

use std::path::{Path, PathBuf};

use interpret::codex::{
    CODEX_INJECT_CONSUMPTION, Codex, CodexWith, Drained, InjectConsumption, Parked, State,
};
use interpret::{Channel, Effect, Event, Fact, Interpreter, run_golden};
use serde_json::{Value, json};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/codex")
}

fn golden(name: &str) {
    run_golden::<Codex>(&fixtures().join(format!("{name}.json"))).assert_ok();
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
    amuxs_own_tools_draw_a_message_or_nothing => "amux_tools",
    another_client_drives_the_same_thread => "co_drive",
    plans_open_a_decision_amux_answers => "plans",
    a_plan_is_decided_from_codexs_own_app => "co_drive_plan",
    questions_are_skipped_noted_or_replied_to => "question_answers",
    the_thread_carries_the_agents_name => "thread_name",
    amuxs_own_tools_are_approved_without_asking => "amux_tool_approval",
    a_failed_turn_start_drops_its_pending_interrupt => "interrupt_turn_start_failed",
    an_exit_drops_a_pending_interrupt => "interrupt_exited",
    a_held_message_rides_the_first_prompts_turn => "held_message_rides_the_prompt",
    lifecycle_events_write_boundaries_and_interrupt => "lifecycle",
    asks_cover_every_request_codex_makes => "asks",
    inputs_become_app_server_requests => "inputs",
    send_now_steers_a_queued_prompt_into_the_running_turn => "steering",
    rows_for_reroutes_errors_and_the_strip => "rows",
    offered_models_and_skills_come_from_the_server => "offered",
    refused_lists_stay_empty_without_a_row => "offered_refused",
    recorded_access_grant => "recorded_access_grant",
    recorded_approval_allow => "recorded_approval_allow",
    recorded_approval_deny => "recorded_approval_deny",
    recorded_approval_scopes => "recorded_approval_scopes",
    recorded_automatic_review => "recorded_automatic_review",
    recorded_background_terminal => "recorded_background_terminal",
    recorded_compaction => "recorded_compaction",
    recorded_dynamic_tools => "recorded_dynamic_tools",
    recorded_exploring => "recorded_exploring",
    recorded_failing_command => "recorded_failing_command",
    recorded_file_changes => "recorded_file_changes",
    recorded_file_moved => "recorded_file_moved",
    recorded_image => "recorded_image",
    recorded_initialize_and_start => "recorded_initialize_and_start",
    recorded_inject_busy => "recorded_inject_busy",
    recorded_inject_drain => "recorded_inject_drain",
    recorded_inject_idle => "recorded_inject_idle",
    recorded_interrupt => "recorded_interrupt",
    recorded_plan_mode => "recorded_plan_mode",
    recorded_reasoning_summary => "recorded_reasoning_summary",
    recorded_signed_out => "recorded_signed_out",
    recorded_subagent => "recorded_subagent",
    recorded_thread_list_and_resume => "recorded_thread_list_and_resume",
    recorded_tool_server_form => "recorded_tool_server_form",
    recorded_turn_error => "recorded_turn_error",
    recorded_turn_retries => "recorded_turn_retries",
    recorded_turn_round_trip => "recorded_turn_round_trip",
    recorded_two_assistant_messages => "recorded_two_assistant_messages",
    recorded_two_clients_prompt => "recorded_two_clients_prompt",
    recorded_two_clients_approval => "recorded_two_clients_approval",
    recorded_two_clients_steer => "recorded_two_clients_steer",
    recorded_web_search => "recorded_web_search",
    recorded_live_plan => "recorded_live_plan",
    recorded_live_questions => "recorded_live_questions",
    recorded_live_usage => "recorded_live_usage",
    recorded_live_permission => "recorded_live_permission",
    recorded_live_mode => "recorded_live_mode",
}

/// Each consumption arm has its own golden over the same facts, so the
/// probe's constant can flip without losing either.
const ARM_FIXTURES: &[&str] = &["inject_parked", "inject_drained", "inject_drained_turn_end"];

#[test]
fn the_parked_arm_kicks_an_empty_turn_and_consumes_at_its_acknowledgement() {
    run_golden::<CodexWith<Parked>>(&fixtures().join("inject_parked.json")).assert_ok();
}

#[test]
fn the_drained_arm_consumes_at_the_inject_acknowledgement() {
    run_golden::<CodexWith<Drained>>(&fixtures().join("inject_drained.json")).assert_ok();
}

#[test]
fn the_drained_arm_kicks_a_message_whose_turn_ended_before_its_acknowledgement() {
    run_golden::<CodexWith<Drained>>(&fixtures().join("inject_drained_turn_end.json")).assert_ok();
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
        .chain(ARM_FIXTURES)
        .map(|name| (*name).to_owned())
        .collect::<Vec<_>>();
    named.sort();
    assert_eq!(on_disk, named);
}

fn rpc(message: Value) -> Event {
    Event::Fact(Fact {
        channel: Channel::Rpc,
        payload: message.to_string().into_bytes(),
    })
}

fn writes(effects: &[Effect]) -> Vec<Value> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::ProviderWrite(bytes) => serde_json::from_slice(bytes).ok(),
            _ => None,
        })
        .collect()
}

fn agent_message(id: &[u8]) -> Event {
    Event::AgentMessage(wire::Envelope {
        id: id.to_vec(),
        body: Some(wire::envelope::Body::Message(wire::MessageBody {
            text: "from a peer".into(),
            ..Default::default()
        })),
        ..Default::default()
    })
}

fn prompt(id: &[u8], text: &str) -> Event {
    Event::Input(wire::Input::new(
        id.to_vec(),
        wire::CodexInput {
            of: Some(wire::codex_input::Of::Prompt(wire::PromptInput {
                text: text.into(),
                ..Default::default()
            })),
        },
    ))
}

/// A thread with a turn running and an agent message injected into it,
/// acknowledged; then the turn ends. Returns what the turn end wrote and
/// the state after it.
fn inject_mid_turn<I: Interpreter<State = State>>() -> (State, Vec<Value>, bool) {
    let spec = wire::AgentSpec {
        agent_id: b"agent".to_vec(),
        ..Default::default()
    };
    let (mut state, _) = I::initial(&spec, "test");
    I::step(
        &mut state,
        rpc(
            json!({"id": 2, "result": {"thread": {"id": "t1", "cliVersion": "0.157.0", "turns": []}, "model": "m"}}),
        ),
    );
    I::step(&mut state, prompt(b"p1", "count"));
    I::step(
        &mut state,
        rpc(
            json!({"method": "turn/started", "params": {"threadId": "t1", "turn": {"id": "turn-1"}}}),
        ),
    );
    let inject = writes(&I::step(&mut state, agent_message(b"e1")).effects);
    assert_eq!(inject.len(), 1, "one inject, no kick while a turn runs");
    assert_eq!(inject[0]["method"], "thread/inject_items");
    I::step(
        &mut state,
        rpc(json!({"id": inject[0]["id"], "result": {}})),
    );
    let after_ack = state.shared().pending_messages().contains(&b"e1".to_vec());
    let end = writes(
        &I::step(
            &mut state,
            rpc(json!({"method": "turn/completed", "params": {"threadId": "t1", "turn": {"id": "turn-1", "status": "completed"}}})),
        )
        .effects,
    );
    (state, end, after_ack)
}

fn is_empty_turn(write: &Value) -> bool {
    write["method"] == "turn/start" && write["params"]["input"] == json!([])
}

/// A probe of codex-cli 0.157.0 found that a running turn drains injected
/// items: the message is consumed at the inject's acknowledgement and no
/// empty turn is kicked at turn end.
#[test]
fn probe_decided_codex_consumption() {
    assert_eq!(CODEX_INJECT_CONSUMPTION, InjectConsumption::DrainedMidTurn);
    let (state, end, pending_after_ack) = inject_mid_turn::<Codex>();
    assert_eq!(state.consumption(), InjectConsumption::DrainedMidTurn);
    assert!(!pending_after_ack, "consumed at the inject acknowledgement");
    assert!(
        !end.iter().any(is_empty_turn),
        "no empty turn is kicked at turn end: {end:?}"
    );
    assert!(state.shared().quiescent());
}

#[test]
fn the_parked_arm_keeps_the_message_pending_until_the_kicked_turn_is_acknowledged() {
    let (mut state, end, pending_after_ack) = inject_mid_turn::<CodexWith<Parked>>();
    assert!(pending_after_ack, "an acknowledged inject is still parked");
    let kick = end
        .iter()
        .find(|write| is_empty_turn(write))
        .expect("an empty turn is kicked at turn end");
    assert!(!state.shared().quiescent());
    CodexWith::<Parked>::step(
        &mut state,
        rpc(json!({"id": kick["id"], "result": {"turn": {"id": "turn-2"}}})),
    );
    assert!(state.shared().pending_messages().is_empty());
}

#[test]
fn an_idle_inject_is_kicked_under_either_arm() {
    fn idle<I: Interpreter<State = State>>() {
        let (mut state, _) = I::initial(&wire::AgentSpec::default(), "test");
        I::step(
            &mut state,
            rpc(json!({"method": "thread/started", "params": {"thread": {"id": "t1"}}})),
        );
        let sent = writes(&I::step(&mut state, agent_message(b"e1")).effects);
        assert_eq!(sent[0]["method"], "thread/inject_items");
        assert!(is_empty_turn(&sent[1]), "{sent:?}");
        I::step(&mut state, rpc(json!({"id": sent[0]["id"], "result": {}})));
        assert!(
            !state.shared().pending_messages().is_empty(),
            "an idle inject waits for its turn"
        );
        I::step(
            &mut state,
            rpc(json!({"id": sent[1]["id"], "result": {"turn": {"id": "turn-1"}}})),
        );
        assert!(state.shared().pending_messages().is_empty());
    }
    idle::<CodexWith<Parked>>();
    idle::<CodexWith<Drained>>();
}

#[test]
fn a_prompt_with_attachments_leaves_them_to_the_agent_process() {
    let (mut state, _) = Codex::initial(&wire::AgentSpec::default(), "test");
    Codex::step(
        &mut state,
        rpc(json!({"method": "thread/started", "params": {"thread": {"id": "t1"}}})),
    );
    let effects = Codex::step(
        &mut state,
        Event::Input(wire::Input::new(
            b"p1".to_vec(),
            wire::CodexInput {
                of: Some(wire::codex_input::Of::Prompt(wire::PromptInput {
                    text: "What is this? \u{FFFC}".into(),
                    attachments: vec![wire::Attachment::default()],
                })),
            },
        )),
    )
    .effects;
    let Some(Effect::CodexTurnInput {
        request,
        attachments,
    }) = effects
        .iter()
        .find(|effect| matches!(effect, Effect::CodexTurnInput { .. }))
    else {
        panic!("a turn input with attachments: {effects:?}");
    };
    let request: Value = serde_json::from_slice(request).unwrap();
    assert_eq!(request["method"], "turn/start");
    assert_eq!(attachments.len(), 1);
}

/// The catalogue a fixture's replay last wrote, which its last snapshot
/// names; None when it wrote none.
fn last_catalogue(fixture: &str) -> Option<wire::Catalogue> {
    let replayed = interpret::replay::<Codex>(&fixtures().join(format!("{fixture}.json"))).unwrap();
    let named = replayed
        .iter()
        .rev()
        .find_map(|frame| frame.step.snapshot.clone())
        .expect("a snapshot")
        .catalogue;
    let written = replayed.into_iter().rev().find_map(|frame| frame.catalogue);
    assert_eq!(
        named,
        written.as_ref().map(|(hash, _)| hash.clone()),
        "the snapshot names the catalogue last written"
    );
    written.map(|(_, catalogue)| catalogue)
}

#[test]
fn offered_lists_keep_what_the_server_named() {
    let catalogue = last_catalogue("offered").expect("a catalogue");
    let astra = &catalogue.models[0];
    assert_eq!(astra.value, "gpt-6-astra");
    assert_eq!(astra.display_name, "GPT-6-Astra");
    assert_eq!(
        astra.description,
        "Frontier intelligence for the most demanding work."
    );
    assert_eq!(
        astra.efforts,
        ["low", "medium", "high", "xhigh", "max", "ultra"]
    );
    assert_eq!(astra.default_effort.as_deref(), Some("medium"));
    assert_eq!(
        catalogue
            .models
            .iter()
            .map(|model| model.value.as_str())
            .collect::<Vec<_>>(),
        ["gpt-6-astra", "gpt-6-sol"],
        "both pages, the hidden model left out"
    );
    // The commands amux runs come first; the skills follow.
    let skills: Vec<_> = catalogue
        .commands
        .iter()
        .filter(|command| command.source != "codex")
        .collect();
    assert_eq!(
        skills
            .iter()
            .map(|command| (command.name.as_str(), command.source.as_str()))
            .collect::<Vec<_>>(),
        [
            ("autopilot", "user"),
            ("documents:documents", "user"),
            ("imagegen", "system")
        ],
        "the turned-off skill left out"
    );
    assert!(skills[0].description.starts_with("Execute confirmed"));
    assert_eq!(last_catalogue("offered_refused"), None);
}

/// The attach tool's element in a finished agent message becomes an
/// attachment at its place, whether the message completes on its own or is
/// closed by the turn's end; a malformed element stays text.
#[test]
fn an_attachment_element_in_a_reply_becomes_an_item_attachment() {
    let attachment = wire::Attachment {
        of: Some(wire::attachment::Of::File(wire::BlobRef {
            hash: vec![0xcd; 32],
            name: "report.pdf".into(),
            mime: "application/pdf".into(),
            size: 7,
        })),
    };
    let path = Path::new("/home/me/agents/a/blobs").join("cd".repeat(32));
    let element = attachments::element(&attachment, Some(&path));
    let reply = format!("Report: {element} and <amux-attachment kind=\"file\"/>.");
    let expected = "Report: \u{FFFC} and <amux-attachment kind=\"file\"/>.";

    for completes in [true, false] {
        let spec = wire::AgentSpec {
            agent_id: b"agent".to_vec(),
            ..Default::default()
        };
        let (mut state, _) = Codex::initial(&spec, "test");
        Codex::step(
            &mut state,
            rpc(
                json!({"id": 2, "result": {"thread": {"id": "t1", "cliVersion": "0.157.0", "turns": []}, "model": "m"}}),
            ),
        );
        Codex::step(&mut state, prompt(b"p1", "the report"));
        Codex::step(
            &mut state,
            rpc(
                json!({"method": "turn/started", "params": {"threadId": "t1", "turn": {"id": "turn-1"}}}),
            ),
        );
        let item = json!({"type": "agentMessage", "id": "m1", "text": ""});
        Codex::step(
            &mut state,
            rpc(
                json!({"method": "item/started", "params": {"threadId": "t1", "turnId": "turn-1", "item": item}}),
            ),
        );
        Codex::step(
            &mut state,
            rpc(
                json!({"method": "item/agentMessage/delta", "params": {"threadId": "t1", "turnId": "turn-1", "itemId": "m1", "delta": reply}}),
            ),
        );
        let last = if completes {
            let item = json!({"type": "agentMessage", "id": "m1", "text": reply});
            rpc(
                json!({"method": "item/completed", "params": {"threadId": "t1", "turnId": "turn-1", "item": item}}),
            )
        } else {
            rpc(
                json!({"method": "turn/completed", "params": {"threadId": "t1", "turn": {"id": "turn-1", "status": "completed"}}}),
            )
        };
        let items = Codex::step(&mut state, last).step.items;
        let item = items
            .iter()
            .find(|item| item.key == "m1")
            .unwrap_or_else(|| panic!("the finished message (completes: {completes})"));
        assert_eq!(item.text, expected, "completes: {completes}");
        assert_eq!(
            item.attachments,
            vec![attachment.clone()],
            "completes: {completes}"
        );
    }
}

/// A tool-call approval Codex elicits from `server`, its `_meta.persist`
/// as given, on a started thread: what the interpreter wrote and the asks
/// left open.
fn approval_elicited(server: &str, persist: Value) -> (Vec<Value>, Vec<wire::CodexAsk>) {
    use prost::Message as _;
    let spec = wire::AgentSpec {
        agent_id: b"agent".to_vec(),
        ..Default::default()
    };
    let (mut state, _) = Codex::initial(&spec, "test");
    Codex::step(
        &mut state,
        rpc(
            json!({"id": 2, "result": {"thread": {"id": "t1", "cliVersion": "0.160.0", "turns": []}, "model": "m"}}),
        ),
    );
    let stepped = Codex::step(
        &mut state,
        rpc(json!({
            "id": 0,
            "method": "mcpServer/elicitation/request",
            "params": {
                "_meta": {
                    "codex_approval_kind": "mcp_tool_call",
                    "persist": persist,
                    "tool_params": {"word": "BLUE"},
                },
                "message": "Allow the server to run tool \"ask\"?",
                "mode": "form",
                "requestedSchema": {"properties": {}, "type": "object"},
                "serverName": server,
                "threadId": "t1",
                "turnId": "turn-1",
            },
        })),
    );
    let asks = stepped
        .step
        .snapshot
        .map(|snapshot| {
            wire::CodexSnapshot::decode(snapshot.body.as_slice())
                .unwrap()
                .asks
        })
        .unwrap_or_default();
    (writes(&stepped.effects), asks)
}

/// Codex writes `persist` as a single string when it offers one lifetime.
#[test]
fn an_approval_offering_one_lifetime_as_a_string_is_read() {
    let (written, asks) = approval_elicited(interpret::AMUX_TOOL_SERVER, json!("session"));
    assert_eq!(
        written,
        [
            json!({"id": 0, "result": {"action": "accept", "content": {}, "_meta": {"persist": "session"}}})
        ],
        "amux's own tool is approved at once, for the session"
    );
    assert!(asks.is_empty());

    let decisions = |persist| {
        let (written, asks) = approval_elicited("spec", persist);
        assert!(written.is_empty(), "{written:?}");
        let [ask] = &asks[..] else {
            panic!("one ask: {asks:?}");
        };
        ask.decisions
            .iter()
            .map(|decision| wire::Decision::try_from(*decision).unwrap())
            .collect::<Vec<_>>()
    };
    use wire::Decision::{Abort, Approve, ApproveSession, Deny};
    assert_eq!(
        decisions(json!("session")),
        [Approve, ApproveSession, Deny, Abort]
    );
    assert_eq!(
        decisions(json!("always")),
        [Approve, Deny, Abort],
        "no remember-for-session choice when Codex offers only always"
    );
}

/// An approval is answered only with a decision the request offered:
/// another is refused as unsupported, writes nothing to Codex and leaves
/// the ask open; an offered one is sent and closes it.
#[test]
fn approval_outside_offered_decisions_is_rejected() {
    use prost::Message as _;
    let spec = wire::AgentSpec {
        agent_id: b"agent".to_vec(),
        ..Default::default()
    };
    let (mut state, _) = Codex::initial(&spec, "test");
    Codex::step(
        &mut state,
        rpc(
            json!({"id": 2, "result": {"thread": {"id": "t1", "cliVersion": "0.160.0", "turns": []}, "model": "m"}}),
        ),
    );
    // Codex offers no remember-for-session choice when it persists only
    // always.
    Codex::step(
        &mut state,
        rpc(json!({
            "id": 0,
            "method": "mcpServer/elicitation/request",
            "params": {
                "_meta": {
                    "codex_approval_kind": "mcp_tool_call",
                    "persist": "always",
                    "tool_params": {"word": "BLUE"},
                },
                "message": "Allow the server to run tool \"ask\"?",
                "mode": "form",
                "requestedSchema": {"properties": {}, "type": "object"},
                "serverName": "spec",
                "threadId": "t1",
                "turnId": "turn-1",
            },
        })),
    );
    let key = state.shared().asks().open_asks()[0].key.clone();
    let approve = |id: &[u8], decision: wire::Decision| {
        Event::Input(wire::Input::new(
            id.to_vec(),
            wire::CodexInput {
                of: Some(wire::codex_input::Of::Answer(wire::CodexAnswer {
                    ask_key: key.clone(),
                    of: Some(wire::codex_answer::Of::Approve(decision as i32)),
                })),
            },
        ))
    };
    let verdicts = |effects: &[Effect]| {
        effects
            .iter()
            .filter_map(|effect| match effect {
                Effect::Reply { verdict, .. } => verdict.of.clone(),
                _ => None,
            })
            .collect::<Vec<_>>()
    };

    let refused = Codex::step(&mut state, approve(b"a1", wire::Decision::ApproveSession));
    assert_eq!(
        verdicts(&refused.effects),
        [wire::send_input_response::Of::Rejected(wire::Rejected {
            refusal: wire::Refusal::Unsupported as i32,
            detail: None,
        })]
    );
    assert!(writes(&refused.effects).is_empty(), "nothing went to Codex");
    assert!(refused.step.encoded_len() == 0, "the ask stays open");
    assert_eq!(state.shared().asks().open_asks().len(), 1);

    let sent = Codex::step(&mut state, approve(b"a2", wire::Decision::Approve));
    assert!(matches!(
        verdicts(&sent.effects)[..],
        [wire::send_input_response::Of::Accepted(_)]
    ));
    assert_eq!(
        writes(&sent.effects).len(),
        1,
        "the offered decision is sent"
    );
    assert!(state.shared().asks().is_empty());
}

/// A job Codex reported and then it exits: the list empties on the exit, and
/// a new incarnation from a checkpoint that still listed it starts empty.
#[test]
fn the_job_list_empties_when_codex_exits() {
    use prost::Message as _;
    let jobs = |step: &wire::Step| {
        step.snapshot.as_ref().map(|snapshot| {
            let body = wire::CodexSnapshot::decode(&snapshot.body[..]).unwrap();
            let background = body.background_jobs.unwrap_or_default();
            (background.known, background.jobs.len())
        })
    };
    let (mut state, spec) = interpret::run_until::<Codex>(&fixtures().join("rows.json"), |step| {
        jobs(step).is_some_and(|(_, running)| running > 0)
    })
    .unwrap()
    .expect("the fixture lists a job");
    let checkpoint = interpret::encode_checkpoint(&state);

    let exited = Codex::step(&mut state, Event::ProviderExit { code: Some(0) });
    let listed: <Codex as Interpreter>::State = interpret::decode_checkpoint(&checkpoint).unwrap();
    let next = wire::AgentSpec {
        incarnation: spec.incarnation + 1,
        ..spec
    };
    let (_, step) = Codex::reincarnate(listed, &next, "test");
    assert_eq!(
        (jobs(&exited.step), jobs(&step)),
        (Some((true, 0)), Some((true, 0))),
        "emptied on exit, and empty in a new incarnation"
    );
}

/// A thread whose turn runs `cargo watch` as command cmd-1.
fn command_running() -> State {
    let spec = wire::AgentSpec {
        agent_id: b"agent".to_vec(),
        ..Default::default()
    };
    let (mut state, _) = Codex::initial(&spec, "test");
    for event in [
        rpc(
            json!({"id": 2, "result": {"thread": {"id": "t1", "cliVersion": "0.160.0", "turns": []}, "model": "m"}}),
        ),
        prompt(b"p1", "watch the build"),
        rpc(
            json!({"method": "turn/started", "params": {"threadId": "t1", "turn": {"id": "turn-1"}}}),
        ),
        rpc(
            json!({"method": "item/started", "params": {"threadId": "t1", "turnId": "turn-1", "item": {
            "type": "commandExecution", "id": "cmd-1", "command": "cargo watch", "cwd": "/work",
            "commandActions": [], "status": "inProgress", "exitCode": null}}}),
        ),
    ] {
        Codex::step(&mut state, event);
    }
    state
}

fn output(delta: &str) -> Event {
    rpc(
        json!({"method": "item/commandExecution/outputDelta", "params": {"threadId": "t1", "turnId": "turn-1", "itemId": "cmd-1", "delta": delta}}),
    )
}

/// Output of a command that is no longer the newest item travels as an
/// append of the new text alone, not as the whole command again.
#[test]
fn a_command_below_the_newest_item_streams_its_output_as_appends() {
    let mut state = command_running();
    Codex::step(&mut state, output("[watching]\n"));
    Codex::step(
        &mut state,
        rpc(
            json!({"method": "item/started", "params": {"threadId": "t1", "turnId": "turn-1", "item": {
            "type": "agentMessage", "id": "msg-1", "text": ""}}}),
        ),
    );
    let stepped = Codex::step(&mut state, output("[rebuilt]\n"));
    let resent: Vec<_> = stepped
        .step
        .items
        .iter()
        .map(|item| (item.key.as_str(), item.text.len()))
        .collect();
    let appended: Vec<_> = stepped
        .step
        .appends
        .iter()
        .map(|append| (append.key.as_str(), append.text.as_str()))
        .collect();
    assert_eq!(
        (resent, appended),
        (vec![], vec![("cmd-1", "[rebuilt]\n")]),
        "only the new text is sent"
    );
}

/// A command that prints without end keeps a bounded amount of output in
/// the interpreter's state.
#[test]
fn a_command_that_prints_without_end_keeps_bounded_state() {
    use prost::Message as _;
    let mut state = command_running();
    let line = format!("{}\n", "x".repeat(1023));
    let mut resent = None;
    for printed in 1..=4096 {
        let stepped = Codex::step(&mut state, output(&line));
        if let Some(item) = stepped
            .step
            .items
            .into_iter()
            .find(|item| item.key == "cmd-1")
        {
            resent = Some((item, printed * line.len()));
        }
    }
    let checkpoint = interpret::encode_checkpoint(&state).len();
    assert!(
        checkpoint < 512 * 1024,
        "4 MiB of output left a checkpoint of {checkpoint} bytes"
    );
    let (resent, printed) = resent.expect("the command was sent again whole when cut");
    assert!(resent.text.len() <= 2 * interpret::OUTPUT_CAP);
    assert!(resent.text.starts_with('x'), "cut at a line boundary");
    let Some(wire::codex_item::Kind::Work(wire::Work {
        of: Some(wire::work::Of::Command(command)),
        ..
    })) = wire::CodexItem::decode(resent.body.as_slice())
        .unwrap()
        .kind
    else {
        panic!("a command item");
    };
    assert_eq!(
        command.output_dropped_bytes as usize + resent.text.len(),
        printed,
        "the body counts every byte dropped"
    );
    let open = state.shared().open_item("cmd-1").unwrap().text.len();
    assert!(open <= 2 * interpret::OUTPUT_CAP);
}

/// The `skills/list` requests a step wrote, as JSON.
fn skills_asks(stepped: &interpret::Stepped) -> Vec<Value> {
    stepped
        .effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::ProviderWrite(bytes) => serde_json::from_slice::<Value>(bytes).ok(),
            _ => None,
        })
        .filter(|request| request["method"] == "skills/list")
        .collect()
}

/// The hash of the catalogue a step wrote: the blob its snapshot names.
fn catalogue_written(stepped: &interpret::Stepped) -> Option<Vec<u8>> {
    let named = stepped.step.snapshot.as_ref()?.catalogue.as_ref()?;
    stepped.effects.iter().find_map(|effect| match effect {
        Effect::WriteBlob { hash, .. } if hash == named => Some(hash.clone()),
        _ => None,
    })
}

/// Codex says its skills changed with an empty notification: the
/// interpreter asks for the list again exactly as it first did and
/// rebuilds the catalogue from the answer, which changes its hash only when
/// the list changed. Notices arriving while an ask is out cost one more ask
/// after its answer, not one each.
#[test]
fn a_skills_changed_notice_asks_again_and_rebuilds_the_catalogue() {
    let script = interpret::fixture_script::<Codex>(&fixtures().join("offered.json")).unwrap();
    let (mut state, _) = Codex::initial(&script.spec, &script.producer);
    let mut first_ask = None;
    let mut answer = None;
    let mut hash = None;
    for (_, event) in script.events {
        let Some(event) = event else { continue };
        if let Event::Fact(fact) = &event {
            let message: Value = serde_json::from_slice(&fact.payload).unwrap();
            if message["id"] == "amux-skills" {
                answer = Some(message);
            }
        }
        let stepped = Codex::step(&mut state, event);
        first_ask = first_ask.or(skills_asks(&stepped).into_iter().next());
        hash = catalogue_written(&stepped).or(hash);
    }
    let first_ask = first_ask.expect("the server is asked for its skills");
    let mut answer = answer.expect("the fixture answers it");
    let hash = hash.expect("a catalogue");
    let same_ask = |ask: &Value, id: &str| {
        assert_eq!(ask["id"], id);
        assert_eq!(
            ask["params"], first_ask["params"],
            "asked as the first time"
        );
    };
    let changed = || rpc(json!({"method": "skills/changed", "params": {}}));

    // A skill appears: asked again, and the new list is a new catalogue.
    let stepped = Codex::step(&mut state, changed());
    let [ask] = skills_asks(&stepped).try_into().expect("one ask");
    same_ask(&ask, "amux-skills-2");
    let skills = answer["result"]["data"][0]["skills"]
        .as_array_mut()
        .unwrap();
    let mut added = skills[0].clone();
    added["name"] = json!("new-skill");
    skills.push(added);
    answer["id"] = json!("amux-skills-2");
    let stepped = Codex::step(&mut state, rpc(answer.clone()));
    let new_hash = catalogue_written(&stepped).expect("the list changed");
    assert_ne!(new_hash, hash);
    assert_eq!(
        stepped
            .step
            .snapshot
            .and_then(|snapshot| snapshot.catalogue),
        Some(new_hash),
        "the snapshot names the new catalogue"
    );

    // Two notices while the ask is out: one ask now, one after its answer.
    let stepped = Codex::step(&mut state, changed());
    let [ask] = skills_asks(&stepped).try_into().expect("one ask");
    same_ask(&ask, "amux-skills-3");
    let stepped = Codex::step(&mut state, changed());
    assert!(skills_asks(&stepped).is_empty(), "one ask is already out");
    answer["id"] = json!("amux-skills-3");
    let stepped = Codex::step(&mut state, rpc(answer.clone()));
    assert_eq!(
        catalogue_written(&stepped),
        None,
        "the same list writes nothing"
    );
    let [ask] = skills_asks(&stepped).try_into().expect("asked once more");
    same_ask(&ask, "amux-skills-4");
    answer["id"] = json!("amux-skills-4");
    let stepped = Codex::step(&mut state, rpc(answer));
    assert_eq!(
        catalogue_written(&stepped),
        None,
        "the same list writes nothing"
    );
    assert!(skills_asks(&stepped).is_empty());
    assert!(
        stepped.step.snapshot.is_none(),
        "nothing a client draws changed"
    );
}

/// Codex's settings notice says who answers approvals and which
/// collaboration mode the thread is in: a reviewer model on the default
/// sandbox is the auto permission, and plan is the mode.
#[test]
fn the_settings_notice_names_the_mode_and_who_answers_approvals() {
    use prost::Message as _;
    let spec = wire::AgentSpec {
        agent_id: b"agent".to_vec(),
        ..Default::default()
    };
    let (mut state, _) = Codex::initial(&spec, "test");
    Codex::step(
        &mut state,
        rpc(
            json!({"id": 2, "result": {"thread": {"id": "t1", "cliVersion": "0.160.0", "turns": []}, "model": "m"}}),
        ),
    );
    let stepped = Codex::step(
        &mut state,
        rpc(json!({
            "method": "thread/settings/updated",
            "params": {
                "threadId": "t1",
                "threadSettings": {
                    "approvalPolicy": "on-request",
                    "approvalsReviewer": "auto_review",
                    "collaborationMode": {
                        "mode": "plan",
                        "settings": {"model": "m", "reasoning_effort": "high"},
                    },
                    "cwd": "/work",
                    "effort": "high",
                    "model": "m",
                    "sandboxPolicy": {"type": "workspaceWrite"},
                },
            },
        })),
    );
    let snapshot = stepped
        .step
        .snapshot
        .expect("the notice moves the snapshot");
    let snapshot = wire::CodexSnapshot::decode(snapshot.body.as_slice()).unwrap();
    println!(
        "permission={:?} mode={:?} approval={:?} sandbox={:?} reviewer={:?} collaboration={:?}",
        snapshot.permission,
        snapshot.mode,
        snapshot.approval_policy,
        snapshot.sandbox,
        snapshot.approvals_reviewer,
        snapshot.collaboration_mode
    );
    assert_eq!(snapshot.approvals_reviewer.as_deref(), Some("auto_review"));
    assert_eq!(snapshot.collaboration_mode.as_deref(), Some("plan"));
    assert_eq!(snapshot.permission.as_deref(), Some("auto"));
    assert_eq!(snapshot.mode.as_deref(), Some("plan"));
}

/// An agent created in plan mode reads plan from the start, and its first
/// turn sets it.
#[test]
fn a_mode_chosen_at_creation_is_set_by_the_first_turn() {
    use prost::Message as _;
    let spec = wire::AgentSpec {
        agent_id: b"agent".to_vec(),
        config: Some(wire::EffectiveConfig {
            mode: Some("plan".into()),
            ..Default::default()
        }),
        bootstrap: Some(wire::Bootstrap {
            of: Some(wire::bootstrap::Of::Prompt(wire::PromptSubmission {
                input_id: b"p1".to_vec(),
                prompt: Some(wire::PromptInput {
                    text: "Plan it.".into(),
                    ..Default::default()
                }),
            })),
        }),
        ..Default::default()
    };
    let (mut state, _) = Codex::initial(&spec, "test");
    let stepped = Codex::step(
        &mut state,
        rpc(
            json!({"id": 2, "result": {"thread": {"id": "t1", "cliVersion": "0.160.0", "turns": []}, "model": "m"}}),
        ),
    );
    let turn = writes(&stepped.effects)
        .into_iter()
        .find(|write| write["method"] == "turn/start")
        .expect("the first prompt starts a turn");
    assert_eq!(
        turn["params"]["collaborationMode"]["mode"], "plan",
        "{turn}"
    );
    assert_eq!(
        turn["params"]["collaborationMode"]["settings"]["model"],
        "m"
    );
    let snapshot = stepped
        .step
        .snapshot
        .expect("the thread moves the snapshot");
    let snapshot = wire::CodexSnapshot::decode(snapshot.body.as_slice()).unwrap();
    assert_eq!(snapshot.mode.as_deref(), Some("plan"));
}

/// A child's failed report as its daemon hands it over: no text, the cause
/// as data.
fn failed_report(id: &[u8]) -> Event {
    Event::AgentMessage(wire::Envelope {
        id: id.to_vec(),
        from: Some(wire::Sender {
            value: Some(wire::sender::Value::Agent(wire::AgentSender {
                name: "child".into(),
                ..Default::default()
            })),
        }),
        body: Some(wire::envelope::Body::Report(wire::ChildReport {
            end: Some(wire::child_report::End::Exited(wire::Exited {
                ended: Some(wire::IncarnationEnd {
                    cause: wire::ExitCause::ProviderExited as i32,
                    detail: Some("exit code 1".into()),
                }),
            })),
            ..Default::default()
        })),
        ..Default::default()
    })
}

/// A failed report carries its cause as data; the interpreter words it into
/// the item it injects into the thread.
#[test]
fn a_failed_report_is_worded_for_the_parents_provider() {
    let (mut state, _) = Codex::initial(&wire::AgentSpec::default(), "test");
    Codex::step(
        &mut state,
        rpc(json!({"method": "thread/started", "params": {"thread": {"id": "t1"}}})),
    );
    let stepped = Codex::step(&mut state, failed_report(b"env-failed"));
    let sent = writes(&stepped.effects);
    assert_eq!(sent[0]["method"], "thread/inject_items");
    assert!(
        sent[0]
            .to_string()
            .contains("[agent child stopped: the provider exited: exit code 1]"),
        "{sent:?}"
    );
    assert_eq!(
        stepped.step.items[0].text,
        "the provider exited: exit code 1"
    );
}

/// The agent process resumes a thread with `thread/resume` under its own
/// request id. When Codex refuses it (here, as recorded, because the
/// previous incarnation's server still held the thread), the incarnation
/// cannot go on: the interpreter must end it with Codex's words rather
/// than wait at "starting" for a thread that will never come, swallowing
/// every prompt sent to it.
#[test]
fn a_refused_thread_resume_ends_the_incarnation_with_codexs_words() {
    let spec = wire::AgentSpec {
        agent_id: b"agent".to_vec(),
        incarnation: 2,
        ..Default::default()
    };
    let (mut state, _) = Codex::initial(&spec, "test");
    Codex::step(
        &mut state,
        rpc(
            json!({"id": "agent-initialize", "result": {"userAgent": "amux/0.161.0", "codexHome": "/codex-home"}}),
        ),
    );
    let refused = Codex::step(
        &mut state,
        rpc(json!({"id": "agent-thread", "error": {"code": -32600,
            "message": "thread 01a12b90-5d40-72a1-8510-d20d631e1c4d already has an active writer"}})),
    );
    let exits: Vec<&String> = refused
        .effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Exit { cause } => Some(cause),
            _ => None,
        })
        .collect();
    assert!(
        exits
            .iter()
            .any(|cause| cause.contains("already has an active writer")),
        "a refused thread/resume ends the incarnation with Codex's words; effects: {:?}, step: {:?}",
        refused.effects,
        refused.step,
    );
}

/// A child resumed by its parent's message takes it as its first prompt,
/// labelled with its sender as a message handed over live is: without
/// the label the model takes the parent's words for its person's.
#[test]
fn a_parents_message_that_resumes_its_child_reaches_codex_labelled() {
    let spec = wire::AgentSpec {
        agent_id: b"agent".to_vec(),
        bootstrap: Some(wire::Bootstrap {
            of: Some(wire::bootstrap::Of::AgentMessage(wire::Envelope {
                id: b"m1".to_vec(),
                from: Some(wire::Sender {
                    value: Some(wire::sender::Value::Agent(wire::AgentSender {
                        name: "lead".into(),
                        ..Default::default()
                    })),
                }),
                body: Some(wire::envelope::Body::Message(wire::MessageBody {
                    text: "Pick it up again.".into(),
                    ..Default::default()
                })),
                ..Default::default()
            })),
        }),
        ..Default::default()
    };
    let (mut state, _) = Codex::initial(&spec, "test");
    let stepped = Codex::step(
        &mut state,
        rpc(
            json!({"id": 2, "result": {"thread": {"id": "t1", "cliVersion": "0.160.0", "turns": []}, "model": "m"}}),
        ),
    );
    let turn = writes(&stepped.effects)
        .into_iter()
        .find(|write| write["method"] == "turn/start")
        .expect("the message starts a turn");
    assert_eq!(
        turn["params"]["input"][0]["text"], "[message from agent lead]\nPick it up again.",
        "{turn}"
    );
    let item = stepped
        .step
        .items
        .iter()
        .find(|item| item.input_id == b"m1")
        .expect("the message's row");
    assert_eq!(
        item.text, "Pick it up again.",
        "the row keeps the words alone"
    );
}

/// Codex 0.162 sends a goal's progress (`thread/goal/updated`) several times
/// a turn and a startup `configWarning` naming project config it ignored, as
/// QA recorded. Neither is a row saying amux could not read it: the goal is
/// drawn or dropped like `thread/goal/cleared`, and the warning keeps its words.
#[test]
fn codexs_goal_updates_and_config_warnings_are_not_unreadable_rows() {
    let spec = wire::AgentSpec {
        agent_id: b"agent".to_vec(),
        ..Default::default()
    };
    let (mut state, _) = Codex::initial(&spec, "test");
    Codex::step(
        &mut state,
        rpc(
            json!({"id": 2, "result": {"thread": {"id": "t1", "cliVersion": "0.162.1", "turns": []}, "model": "m"}}),
        ),
    );
    let warning = "Project-local config, hooks, and exec policies are disabled in the following folders until the project is trusted, but skills still load.";
    let mut unreadable = Vec::new();
    let mut texts = String::new();
    for notification in [
        json!({"method": "configWarning", "params": {"summary": warning, "details": null}}),
        json!({"method": "thread/goal/updated", "params": {"threadId": "t1", "turnId": "turn-1",
            "goal": {"threadId": "t1", "objective": "Create a.txt, then b.txt.", "status": "active",
                "tokenBudget": null, "tokensUsed": 0, "timeUsedSeconds": 0,
                "createdAt": 1791782769, "updatedAt": 1791782769}}}),
    ] {
        for item in Codex::step(&mut state, rpc(notification)).step.items {
            texts.push_str(&item.text);
            if let Ok(body) = <wire::CodexItem as prost::Message>::decode(item.body.as_slice())
                && let Some(wire::codex_item::Kind::Unrecognized(what)) = body.kind
            {
                unreadable.push(what.fact_type);
            }
        }
    }
    assert!(
        unreadable.is_empty(),
        "Codex notifications drawn as unreadable rows: {unreadable:?}"
    );
    assert!(
        texts.contains("Project-local config"),
        "the config warning's words reach the record: {texts:?}"
    );
}

/// Codex 0.164 sends each turn's moderation verdict
/// (`turn/moderationMetadata`) and which turn its own app last showed
/// (`thread/readState/changed`), as QA recorded on Linux. Neither is
/// something a chat draws, nor a row saying amux could not read it.
#[test]
fn codexs_moderation_and_read_state_notices_are_not_unreadable_rows() {
    let spec = wire::AgentSpec {
        agent_id: b"agent".to_vec(),
        ..Default::default()
    };
    let (mut state, _) = Codex::initial(&spec, "test");
    Codex::step(
        &mut state,
        rpc(
            json!({"id": 2, "result": {"thread": {"id": "t1", "cliVersion": "0.164.0", "turns": []}, "model": "m"}}),
        ),
    );
    let mut rows = Vec::new();
    for notification in [
        json!({"method": "turn/moderationMetadata", "params": {"threadId": "t1", "turnId": "turn-1",
            "metadata": {"prompt": {"omnimod": {"protection_type": "omnimod",
                "outputs": [{"results": [{"category_flags": {"A1": false, "REFUSED": false}}]}]}}}}}),
        json!({"method": "thread/readState/changed", "params": {"threadId": "t1",
            "readState": {"firstUnread": {"type": "turn", "turnId": "turn-1"}, "revision": "r1"}},
            "emittedAtMs": 1792972625564_u64}),
    ] {
        for item in Codex::step(&mut state, rpc(notification)).step.items {
            rows.push(item.text);
        }
    }
    assert!(rows.is_empty(), "rows drawn for Codex's notices: {rows:?}");
}

/// Codex runs a model's command through the user's shell and asks about the
/// wrapped form (`/bin/zsh -lc "printf '%s\\\\n' …"`), with the command the
/// model wrote in `commandActions`, as QA recorded on Codex 0.162. Codex's
/// own approval shows `printf '%s\n' …`; amux's ask shows the same command,
/// not the shell wrapper with its quoting doubled.
#[test]
fn a_command_approval_shows_the_command_codex_shows_not_its_shell_wrapper() {
    use prost::Message as _;
    let spec = wire::AgentSpec {
        agent_id: b"agent".to_vec(),
        ..Default::default()
    };
    let (mut state, _) = Codex::initial(&spec, "test");
    Codex::step(
        &mut state,
        rpc(
            json!({"id": 2, "result": {"thread": {"id": "t1", "cliVersion": "0.162.1", "turns": []}, "model": "m"}}),
        ),
    );
    let script = "printf '%s\\n' ééé && echo aaa > out.txt";
    let stepped = Codex::step(
        &mut state,
        rpc(json!({
            "id": 0,
            "method": "item/commandExecution/requestApproval",
            "params": {
                "kind": "command", "threadId": "t1", "turnId": "turn-1", "itemId": "exec-1",
                "reason": "May I run the exact requested command with permission to create out.txt?",
                "command": "/bin/zsh -lc \"printf '%s\\\\n' ééé && echo aaa > out.txt\"",
                "cwd": "/work",
                "commandActions": [{"type": "unknown", "command": script}],
                "proposedExecpolicyAmendment": ["/bin/zsh", "-lc", script],
                "availableDecisions": ["accept", {"acceptWithExecpolicyAmendment": {"execpolicy_amendment": ["/bin/zsh", "-lc", script]}}, "cancel"],
            },
        })),
    );
    let asks = stepped
        .step
        .snapshot
        .map(|snapshot| {
            wire::CodexSnapshot::decode(snapshot.body.as_slice())
                .unwrap()
                .asks
        })
        .unwrap_or_default();
    let command = asks
        .iter()
        .find_map(|ask| match &ask.body {
            Some(wire::codex_ask::Body::Command(command)) => Some(command.command.clone()),
            _ => None,
        })
        .expect("a command ask");
    assert_eq!(command, script, "the ask shows the shell wrapper");
}

/// Codex's interrupt ends the turn but not a command it runs as a unified
/// exec process: the command's item stays in progress and completes on its
/// own later, as QA recorded on Codex 0.162 (Codex's own view says "1
/// background terminal running"). amux keeps that step running until Codex
/// says it ended, rather than drawing it cancelled under "You stopped it".
#[test]
fn a_command_codex_keeps_running_past_an_interrupt_is_not_drawn_cancelled() {
    use prost::Message as _;
    let spec = wire::AgentSpec {
        agent_id: b"agent".to_vec(),
        ..Default::default()
    };
    let (mut state, _) = Codex::initial(&spec, "test");
    Codex::step(
        &mut state,
        rpc(
            json!({"id": 2, "result": {"thread": {"id": "t1", "cliVersion": "0.162.1", "turns": []}, "model": "m"}}),
        ),
    );
    let command = json!({"type": "commandExecution", "id": "exec-1", "command": "/bin/zsh -lc 'sleep 47'",
        "cwd": "/w", "processId": "8911", "source": "unifiedExecStartup", "status": "inProgress",
        "commandActions": [{"type": "unknown", "command": "sleep 47"}],
        "aggregatedOutput": null, "exitCode": null, "durationMs": null});
    let mut states = Vec::new();
    for message in [
        json!({"method": "turn/started", "params": {"threadId": "t1", "turn": {"id": "turn-1", "status": "inProgress"}}}),
        json!({"method": "item/started", "params": {"threadId": "t1", "turnId": "turn-1", "item": command}}),
        json!({"method": "turn/completed", "params": {"threadId": "t1", "turn": {"id": "turn-1", "items": [], "status": "interrupted", "error": null}}}),
    ] {
        for item in Codex::step(&mut state, rpc(message)).step.items {
            if item.key.contains("exec-1")
                && let Ok(body) = wire::CodexItem::decode(item.body.as_slice())
                && let Some(wire::codex_item::Kind::Work(work)) = body.kind
            {
                states.push(work.state());
            }
        }
    }
    assert_eq!(
        states.last(),
        Some(&wire::ToolState::Running),
        "the command Codex still runs is drawn ended at the interrupt: {states:?}"
    );
}

/// A command still waiting for its approval when the turn is interrupted
/// (a Stop) never ran: it is cut short, not a background terminal. It was
/// recorded as running on in the background, which the phone drew as
/// "In background · touch decided.txt · finished".
#[test]
fn a_command_awaiting_approval_at_an_interrupt_never_ran() {
    use prost::Message as _;
    let mut state = idle_codex();
    let command = json!({"type": "commandExecution", "id": "exec-1", "command": "/bin/zsh -lc 'touch decided.txt'",
        "cwd": "/w", "processId": null, "source": "agent", "status": "inProgress",
        "commandActions": [{"type": "unknown", "command": "touch decided.txt"}],
        "aggregatedOutput": null, "exitCode": null, "durationMs": null});
    let mut works = Vec::new();
    for message in [
        json!({"method": "turn/started", "params": {"threadId": "t1", "turn": {"id": "turn-1", "status": "inProgress"}}}),
        json!({"method": "item/started", "params": {"threadId": "t1", "turnId": "turn-1", "item": command}}),
        json!({"id": 0, "method": "item/commandExecution/requestApproval", "params": {
            "threadId": "t1", "turnId": "turn-1", "itemId": "exec-1",
            "command": "touch decided.txt", "cwd": "/w",
            "availableDecisions": ["accept", "cancel"]}}),
        json!({"method": "turn/completed", "params": {"threadId": "t1", "turn": {"id": "turn-1", "items": [], "status": "interrupted", "error": null}}}),
    ] {
        for item in Codex::step(&mut state, rpc(message)).step.items {
            if item.key.contains("exec-1")
                && let Ok(body) = wire::CodexItem::decode(item.body.as_slice())
                && let Some(wire::codex_item::Kind::Work(work)) = body.kind
            {
                works.push(work);
            }
        }
    }
    let last = works.last().expect("the command's item");
    let Some(wire::work::Of::Command(command)) = &last.of else {
        panic!("{last:?}");
    };
    assert!(
        !command.background && last.state() == wire::ToolState::Cancelled,
        "a command that never ran reads as running on in the background: {last:?}"
    );
}

// --- Codex's slash commands: amux runs the ones it has a request for and
// refuses the rest, rather than sending them to the model as words.

/// Codex with thread t1 open and no turn running.
fn idle_codex() -> State {
    let spec = wire::AgentSpec {
        agent_id: b"agent".to_vec(),
        ..Default::default()
    };
    let (mut state, _) = Codex::initial(&spec, "test");
    Codex::step(
        &mut state,
        rpc(
            json!({"id": 2, "result": {"thread": {"id": "t1", "cliVersion": "0.162.1", "turns": []}, "model": "m"}}),
        ),
    );
    state
}

/// What sending `text` as a prompt wrote to Codex, and the replies to it.
fn slash(state: &mut State, id: &[u8], text: &str) -> (Vec<Value>, Vec<String>) {
    let stepped = Codex::step(state, prompt(id, text));
    let replies = stepped
        .effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Reply { verdict, .. } => Some(format!("{verdict:?}")),
            _ => None,
        })
        .collect();
    (writes(&stepped.effects), replies)
}

/// The words a turn/start write sends Codex.
fn turn_text(write: &Value) -> String {
    write["params"]["input"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|input| input["text"].as_str())
        .collect()
}

/// `/clear` starts a new Codex thread, and that thread becomes this agent's
/// conversation: the next prompt's turn runs on it.
#[test]
fn clear_starts_a_codex_thread_the_agent_then_works_in() {
    let mut state = idle_codex();
    let (written, replies) = slash(&mut state, b"c1", "/clear");
    let start = written
        .iter()
        .find(|write| write["method"] == "thread/start")
        .unwrap_or_else(|| {
            panic!("/clear starts no thread: wrote {written:?}, replies {replies:?}")
        });
    Codex::step(
        &mut state,
        rpc(
            json!({"id": start["id"], "result": {"thread": {"id": "t2", "cliVersion": "0.162.1", "turns": []}, "model": "m"}}),
        ),
    );
    let (written, _) = slash(&mut state, b"p1", "hello");
    let turn = written
        .iter()
        .find(|write| write["method"] == "turn/start")
        .expect("the prompt starts a turn");
    assert_eq!(
        turn["params"]["threadId"], "t2",
        "the turn runs on the old thread"
    );
}

/// `/review` asks Codex for its own review, not a turn with those words.
#[test]
fn review_runs_codexs_review() {
    let mut state = idle_codex();
    let (written, replies) = slash(&mut state, b"r1", "/review");
    assert!(
        written
            .iter()
            .any(|write| write["method"] == "review/start"),
        "/review is not Codex's review: wrote {written:?}, replies {replies:?}"
    );
}

/// `/init` sends Codex the fixed prompt that writes the repository's agent
/// instructions, not the bare word.
#[test]
fn init_sends_codex_its_fixed_prompt() {
    let mut state = idle_codex();
    let (written, replies) = slash(&mut state, b"i1", "/init");
    let texts: Vec<String> = written
        .iter()
        .filter(|write| write["method"] == "turn/start")
        .map(turn_text)
        .collect();
    assert!(
        texts.len() == 1 && texts[0].trim() != "/init" && texts[0].len() > 20,
        "/init does not send the fixed prompt: turns {texts:?}, replies {replies:?}"
    );
}

/// `/plan` puts the thread in plan mode, as choosing the mode does: no
/// turn carries the word, and the next turn runs in plan.
#[test]
fn plan_sets_the_collaboration_mode() {
    let mut state = idle_codex();
    let (written, _) = slash(&mut state, b"m1", "/plan");
    let sent_as_words = written
        .iter()
        .any(|write| write["method"] == "turn/start" && turn_text(write).contains("/plan"));
    let (written, _) = slash(&mut state, b"p1", "make a plan");
    let mode = written
        .iter()
        .find(|write| write["method"] == "turn/start")
        .map(|turn| turn["params"]["collaborationMode"]["mode"].clone());
    assert!(
        !sent_as_words && mode == Some(json!("plan")),
        "/plan sent as words: {sent_as_words}; the next turn's mode: {mode:?}"
    );
}

/// A slash command amux has no request for (`/new`, or a word Codex does
/// not know) is refused, not sent to the model as a prompt.
#[test]
fn a_slash_command_amux_cannot_run_on_codex_is_refused() {
    for text in ["/new", "/frobnicate now"] {
        let mut state = idle_codex();
        let (written, replies) = slash(&mut state, b"x1", text);
        assert!(
            written.is_empty() && replies.iter().any(|reply| reply.contains("Rejected")),
            "{text} is not refused: wrote {written:?}, replies {replies:?}"
        );
    }
}

/// Codex's catalogue lists the commands amux runs for it, then its skills.
#[test]
fn codexs_catalogue_lists_the_commands_amux_runs() {
    let catalogue = last_catalogue("offered").expect("a catalogue");
    let names: Vec<&str> = catalogue
        .commands
        .iter()
        .map(|command| command.name.trim_start_matches('/'))
        .collect();
    for command in ["compact", "clear", "review", "init", "plan"] {
        assert!(
            names.contains(&command),
            "the catalogue does not list /{command}: {names:?}"
        );
    }
    assert!(!names.contains(&"new"), "{names:?}");
}

/// Codex's interrupt ends the turn but its unified-exec commands run on as
/// background terminals. amux asks Codex for its background terminals when
/// the interrupted turn ends, so the list names the command.
#[test]
fn an_interrupt_lists_the_commands_codex_keeps_running() {
    let mut state = idle_codex();
    let command = json!({"type": "commandExecution", "id": "exec-1", "command": "/bin/zsh -lc 'sleep 47'",
        "cwd": "/w", "processId": "8911", "source": "unifiedExecStartup", "status": "inProgress",
        "commandActions": [{"type": "unknown", "command": "sleep 47"}],
        "aggregatedOutput": null, "exitCode": null, "durationMs": null});
    let mut written = Vec::new();
    for event in [
        prompt(b"p1", "sleep for a while"),
        rpc(
            json!({"method": "turn/started", "params": {"threadId": "t1", "turn": {"id": "turn-1", "status": "inProgress"}}}),
        ),
        rpc(
            json!({"method": "item/started", "params": {"threadId": "t1", "turnId": "turn-1", "item": command}}),
        ),
        Event::Input(wire::Input::new(
            b"i1".to_vec(),
            wire::CodexInput {
                of: Some(wire::codex_input::Of::Interrupt(wire::Interrupt {})),
            },
        )),
        rpc(
            json!({"method": "turn/completed", "params": {"threadId": "t1", "turn": {"id": "turn-1", "items": [], "status": "interrupted", "error": null}}}),
        ),
    ] {
        written.extend(writes(&Codex::step(&mut state, event).effects));
    }
    let methods: Vec<&Value> = written.iter().map(|write| &write["method"]).collect();
    assert!(
        methods
            .iter()
            .any(|method| *method == "thread/backgroundTerminals/list"),
        "nothing asks Codex what it still runs after the interrupt: {methods:?}"
    );
}

/// A web search that opens a page names the page's address, which is all
/// Codex says about it.
#[test]
fn a_web_search_that_opens_a_page_shows_its_address() {
    let mut state = idle_codex();
    let mut texts = Vec::new();
    for message in [
        json!({"method": "turn/started", "params": {"threadId": "t1", "turn": {"id": "turn-1", "status": "inProgress"}}}),
        json!({"method": "item/completed", "params": {"threadId": "t1", "turnId": "turn-1", "item": {
            "type": "webSearch", "id": "ws-1", "query": "",
            "action": {"type": "openPage", "url": "https://docs.rs/tokio/latest/tokio/"}}}}),
    ] {
        for item in Codex::step(&mut state, rpc(message)).step.items {
            texts.push(format!("{} {item:?}", item.text));
        }
    }
    assert!(
        texts
            .iter()
            .any(|text| text.contains("https://docs.rs/tokio/latest/tokio/")),
        "the step does not name the page it opened: {texts:?}"
    );
}

/// A command that waited a minute for the person's approval and then ran
/// briefly: Codex's duration is its run alone, so its time starts that
/// long before it completed, not when Codex announced it, before the ask.
#[test]
fn a_commands_time_starts_after_the_approval_it_waited_for() {
    use prost::Message as _;
    let spec = wire::AgentSpec {
        agent_id: b"agent".to_vec(),
        ..Default::default()
    };
    let (mut state, _) = Codex::initial(&spec, "test");
    let item = |status: &str, duration: Value| {
        json!({"type": "commandExecution", "id": "cmd-1", "command": "curl -sI https://example.com",
            "cwd": "/work", "commandActions": [], "status": status, "exitCode": 0,
            "durationMs": duration})
    };
    let mut items = Vec::new();
    for event in [
        rpc(
            json!({"id": 2, "result": {"thread": {"id": "t1", "cliVersion": "0.160.0", "turns": []}, "model": "m"}}),
        ),
        prompt(b"p1", "fetch it"),
        rpc(
            json!({"method": "turn/started", "params": {"threadId": "t1", "turn": {"id": "turn-1"}}}),
        ),
        rpc(
            json!({"method": "item/started", "params": {"threadId": "t1", "turnId": "turn-1",
            "startedAtMs": 1_000, "item": item("inProgress", Value::Null)}}),
        ),
        rpc(
            json!({"method": "item/completed", "params": {"threadId": "t1", "turnId": "turn-1",
            "startedAtMs": 1_000, "completedAtMs": 61_200, "item": item("completed", json!(194))}}),
        ),
    ] {
        items.extend(Codex::step(&mut state, event).step.items);
    }
    let work = items
        .iter()
        .rev()
        .filter(|item| item.key == "cmd-1")
        .find_map(
            |item| match wire::CodexItem::decode(item.body.as_slice()).ok()?.kind {
                Some(wire::codex_item::Kind::Work(work)) => Some(work),
                _ => None,
            },
        )
        .expect("the command's row");
    assert_eq!(
        (work.started_at_ms, work.ended_at_ms),
        (Some(61_200 - 194), Some(61_200)),
        "the command's time counts the wait for approval"
    );
}

/// A prompt longer than Codex takes (1,048,576 characters) is refused
/// before it is sent, in words naming the limit, so it goes back to the
/// composer; Codex would refuse it only after amux had taken it, and the
/// draft would be gone. A prompt at the limit goes.
#[test]
fn a_prompt_over_codexs_limit_is_refused_before_it_is_sent() {
    let spec = wire::AgentSpec {
        agent_id: b"agent".to_vec(),
        ..Default::default()
    };
    let (mut state, _) = Codex::initial(&spec, "test");
    Codex::step(
        &mut state,
        rpc(
            json!({"id": 2, "result": {"thread": {"id": "t1", "cliVersion": "0.160.0", "turns": []}, "model": "m"}}),
        ),
    );
    let verdict = |stepped: &interpret::Stepped| {
        stepped
            .effects
            .iter()
            .find_map(|effect| match effect {
                Effect::Reply { verdict, .. } => Some(verdict.clone()),
                _ => None,
            })
            .expect("a verdict")
    };
    let over = Codex::step(&mut state, prompt(b"p1", &"é".repeat(1_048_577)));
    let refused = match verdict(&over).of {
        Some(wire::send_input_response::Of::Rejected(rejected)) => rejected,
        other => panic!("the long prompt was taken: {other:?}"),
    };
    assert_eq!(refused.refusal(), wire::Refusal::TooLong);
    assert!(
        refused
            .detail
            .as_deref()
            .is_some_and(|words| words.contains("1,048,576")),
        "the refusal does not name the limit: {refused:?}"
    );
    assert!(
        writes(&over.effects).is_empty(),
        "the long prompt was written"
    );
    let at = Codex::step(&mut state, prompt(b"p2", &"é".repeat(1_048_576)));
    assert!(
        !matches!(
            verdict(&at).of,
            Some(wire::send_input_response::Of::Rejected(_))
        ),
        "a prompt at the limit was refused"
    );
}

/// A Codex agent driven by authored facts, keeping the last snapshot and
/// every item its steps wrote.
struct Driven {
    state: State,
    snapshot: wire::CodexSnapshot,
    items: Vec<wire::Item>,
}

impl Driven {
    /// An agent whose thread `t1` has started.
    fn started() -> Self {
        use prost::Message as _;
        let spec = wire::AgentSpec {
            agent_id: b"agent".to_vec(),
            ..Default::default()
        };
        let (state, step) = Codex::initial(&spec, "test");
        let snapshot = wire::CodexSnapshot::decode(
            step.snapshot.expect("an initial snapshot").body.as_slice(),
        )
        .unwrap();
        let mut driven = Driven {
            state,
            snapshot,
            items: Vec::new(),
        };
        driven.rpc(
            json!({"id": 2, "result": {"thread": {"id": "t1", "cliVersion": "0.163.0", "turns": []}, "model": "m"}}),
        );
        driven
    }

    fn step(&mut self, event: Event) -> interpret::Stepped {
        use prost::Message as _;
        let stepped = Codex::step(&mut self.state, event);
        if let Some(snapshot) = &stepped.step.snapshot {
            self.snapshot = wire::CodexSnapshot::decode(snapshot.body.as_slice()).unwrap();
        }
        self.items.extend(stepped.step.items.iter().cloned());
        stepped
    }

    fn rpc(&mut self, message: Value) -> interpret::Stepped {
        self.step(rpc(message))
    }

    /// The writes a prompt makes.
    fn prompt(&mut self, id: &[u8], text: &str) -> Vec<Value> {
        writes(&self.step(prompt(id, text)).effects)
    }
}

fn rate_limits(id: Option<&str>, used: i64) -> Value {
    let mut limits = json!({
        "primary": {"usedPercent": used, "windowDurationMins": 300, "resetsAt": 1_800_000_000},
    });
    if let Some(id) = id {
        limits["limitId"] = json!(id);
    }
    json!({"method": "account/rateLimits/updated", "params": {"rateLimits": limits}})
}

/// Codex reports several limits, each under its own id; the meter is the
/// `codex` limit's (an update without an id is that one), as in Codex's own
/// app, so another limit's update leaves it alone.
#[test]
fn the_usage_meter_reads_the_codex_limit_only() {
    let mut agent = Driven::started();
    agent.rpc(rate_limits(None, 12));
    agent.rpc(rate_limits(Some("other_model"), 100));
    let usage = agent.snapshot.usage.clone().expect("usage");
    let used = usage.windows[0].meter.as_ref().unwrap().used_percent;
    assert_eq!(used, 12.0, "another limit's update moved the meter");
    assert_ne!(usage.state(), wire::UsageState::Blocked);
    agent.rpc(rate_limits(Some("codex"), 40));
    let usage = agent.snapshot.usage.clone().unwrap();
    assert_eq!(usage.windows[0].meter.as_ref().unwrap().used_percent, 40.0);
}

impl Driven {
    /// The bodies of every item written so far.
    fn bodies(&self) -> Vec<wire::codex_item::Kind> {
        self.items
            .iter()
            .filter_map(|item| {
                <wire::CodexItem as prost::Message>::decode(item.body.as_slice())
                    .ok()?
                    .kind
            })
            .collect()
    }

    /// The fact types drawn as unrecognized rows so far.
    fn unrecognized(&self) -> Vec<String> {
        self.bodies()
            .into_iter()
            .filter_map(|kind| match kind {
                wire::codex_item::Kind::Unrecognized(what) => Some(what.fact_type),
                _ => None,
            })
            .collect()
    }
}

/// `model/rerouted` covers one request: Codex keeps the chosen model for the
/// next, so amux does too, including for the thread `/clear` starts. Codex
/// says so with a warning carrying its link, which is what the person sees.
#[test]
fn a_reroute_leaves_the_chosen_model_and_shows_codexs_warning() {
    let mut agent = Driven::started();
    let rerouted = agent.rpc(json!({"method": "model/rerouted", "params": {
        "threadId": "t1", "turnId": "turn-1", "fromModel": "m", "toModel": "fallback",
        "reason": "highRiskCyberActivity"}}));
    assert!(
        rerouted.step.items.is_empty(),
        "the reroute drew a row of its own beside Codex's warning"
    );
    agent.rpc(json!({"method": "warning", "params": {
        "threadId": "t1", "message": "Your request was routed to fallback. Apply for access at https://example.com"}}));
    assert_eq!(agent.snapshot.model.as_deref(), Some("m"));
    assert!(
        agent.items.iter().any(|item| item.text.contains("routed to fallback")),
        "Codex's warning was not drawn"
    );
    let cleared = agent.prompt(b"p1", "/clear");
    for write in &cleared {
        if let Some(model) = write["params"]["model"].as_str() {
            assert_eq!(model, "m", "{write}");
        }
    }
}

/// Codex writes a turn's completion and the answer to the `turn/start` (or
/// `review/start`) that began it from separate tasks, so the answer can come
/// last; a late answer must not reopen the turn that already ended.
#[test]
fn a_late_turn_start_answer_does_not_reopen_an_ended_turn() {
    for (text, method) in [("hello", "turn/start"), ("/review", "review/start")] {
        let mut agent = Driven::started();
        let sent = agent.prompt(b"p1", text);
        let start = sent
            .iter()
            .find(|write| write["method"] == method)
            .unwrap_or_else(|| panic!("{method} sent: {sent:?}"))
            .clone();
        agent.rpc(json!({"method": "turn/started", "params": {"threadId": "t1", "turn": {"id": "turn-1"}}}));
        agent.rpc(json!({"method": "turn/completed", "params": {"threadId": "t1", "turn": {"id": "turn-1", "status": "completed"}}}));
        agent.rpc(json!({"id": start["id"], "result": {"turn": {"id": "turn-1"}}}));
        assert_eq!(
            agent.snapshot.active_turn, None,
            "{method}'s late answer reopened the ended turn"
        );
    }
}
