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
        kind: wire::EnvelopeKind::Message as i32,
        text: "from a peer".into(),
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

/// The probe (notes/rearchitect/probes/codex-inject-drain.md, codex-cli
/// 0.157.0) found that a running turn drains injected items: the message
/// is consumed at the inject's acknowledgement and no empty turn is kicked
/// at turn end.
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
        written.as_ref().map(|catalogue| catalogue.hash.clone()),
        "the snapshot names the catalogue last written"
    );
    written
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
    assert_eq!(
        catalogue
            .commands
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
    assert!(
        catalogue.commands[0]
            .description
            .starts_with("Execute confirmed")
    );
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
            reason: interpret::reason::UNSUPPORTED.into(),
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

fn catalogue_hash(stepped: &interpret::Stepped) -> Option<Vec<u8>> {
    stepped.effects.iter().find_map(|effect| match effect {
        Effect::WriteCatalogue { hash, .. } => Some(hash.clone()),
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
        hash = catalogue_hash(&stepped).or(hash);
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
    let new_hash = catalogue_hash(&stepped).expect("the list changed");
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
        catalogue_hash(&stepped),
        None,
        "the same list writes nothing"
    );
    let [ask] = skills_asks(&stepped).try_into().expect("asked once more");
    same_ask(&ask, "amux-skills-4");
    answer["id"] = json!("amux-skills-4");
    let stepped = Codex::step(&mut state, rpc(answer));
    assert_eq!(
        catalogue_hash(&stepped),
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
