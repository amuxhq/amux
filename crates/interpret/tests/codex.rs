//! The Codex interpreter against the app-server sessions the live lane
//! recorded in fixtures/codex/live and authored facts for what no recording
//! shows.

use std::path::{Path, PathBuf};

use interpret::codex::{Codex, State};
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
    a_subagent_is_stopped_from_its_chat_in_three_steps => "child_stop_three_steps",
    a_message_injected_mid_turn_is_consumed_at_the_inject_acknowledgement => "inject_drained",
    a_retried_401_says_nothing_after_good_turns => "retried_401_then_good_turns",
    a_limit_after_a_warning_stays_said => "limit_after_a_warning",
    a_message_whose_turn_ended_before_its_acknowledgement_is_kicked => "inject_drained_turn_end",
    amuxs_own_tools_draw_a_message_or_nothing => "amux_tools",
    another_client_drives_the_same_thread => "co_drive",
    plans_open_a_decision_amux_answers => "plans",
    a_plan_is_decided_from_codexs_own_app => "co_drive_plan",
    questions_are_skipped_noted_or_replied_to => "question_answers",
    the_thread_carries_the_agents_name => "thread_name",
    an_unnamed_thread_is_titled_on_a_hidden_thread => "thread_title",
    a_named_thread_is_not_titled => "thread_title_name_given",
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
    recorded_child_approval => "recorded_child_approval",
    recorded_child_command => "recorded_child_command",
    recorded_child_continued => "recorded_child_continued",
    recorded_child_resume => "recorded_child_resume",
    recorded_child_stop => "recorded_child_stop",
    recorded_child_tools => "recorded_child_tools",
    recorded_compaction => "recorded_compaction",
    recorded_exploring => "recorded_exploring",
    recorded_failing_command => "recorded_failing_command",
    recorded_file_changes => "recorded_file_changes",
    recorded_file_moved => "recorded_file_moved",
    recorded_image => "recorded_image",
    recorded_inject_drain => "recorded_inject_drain",
    recorded_inject_idle => "recorded_inject_idle",
    recorded_interrupt => "recorded_interrupt",
    recorded_reasoning_summary => "recorded_reasoning_summary",
    recorded_signed_out => "recorded_signed_out",
    recorded_subagent => "recorded_subagent",
    recorded_tool_server_form => "recorded_tool_server_form",
    recorded_turn_error => "recorded_turn_error",
    recorded_turn_retries => "recorded_turn_retries",
    recorded_steer_folded => "recorded_steer_folded",
    recorded_turn_round_trip => "recorded_turn_round_trip",
    recorded_two_assistant_messages => "recorded_two_assistant_messages",
    recorded_web_search => "recorded_web_search",
    recorded_live_plan => "recorded_live_plan",
    recorded_live_questions => "recorded_live_questions",
    recorded_live_usage => "recorded_live_usage",
    recorded_live_permission => "recorded_live_permission",
    recorded_live_mode => "recorded_live_mode",
    recorded_live_attach => "recorded_live_attach",
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
fn inject_mid_turn() -> (State, Vec<Value>, bool) {
    let spec = wire::AgentSpec {
        agent_id: b"agent".to_vec(),
        ..Default::default()
    };
    let (mut state, _) = Codex::initial(&spec);
    Codex::step(
        &mut state,
        rpc(
            json!({"id": 2, "result": {"thread": {"id": "t1", "cliVersion": "0.157.0", "turns": []}, "model": "m"}}),
        ),
    );
    Codex::step(&mut state, prompt(b"p1", "count"));
    Codex::step(
        &mut state,
        rpc(
            json!({"method": "turn/started", "params": {"threadId": "t1", "turn": {"id": "turn-1"}}}),
        ),
    );
    let inject = writes(&Codex::step(&mut state, agent_message(b"e1")).effects);
    assert_eq!(inject.len(), 1, "one inject, no kick while a turn runs");
    assert_eq!(inject[0]["method"], "thread/inject_items");
    Codex::step(
        &mut state,
        rpc(json!({"id": inject[0]["id"], "result": {}})),
    );
    let after_ack = state.shared().pending_messages().contains(&b"e1".to_vec());
    let end = writes(
        &Codex::step(
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

/// A running turn drains injected items (codex-cli 0.157.0 onwards): the
/// message is consumed at the inject's acknowledgement and no empty turn is
/// kicked at turn end.
#[test]
fn a_message_injected_mid_turn_is_consumed_at_its_acknowledgement() {
    let (state, end, pending_after_ack) = inject_mid_turn();
    assert!(!pending_after_ack, "consumed at the inject acknowledgement");
    assert!(
        !end.iter().any(is_empty_turn),
        "no empty turn is kicked at turn end: {end:?}"
    );
    assert!(state.shared().quiescent());
}

#[test]
fn an_idle_inject_is_kicked() {
    let (mut state, _) = Codex::initial(&wire::AgentSpec::default());
    Codex::step(
        &mut state,
        rpc(json!({"method": "thread/started", "params": {"thread": {"id": "t1"}}})),
    );
    let sent = writes(&Codex::step(&mut state, agent_message(b"e1")).effects);
    assert_eq!(sent[0]["method"], "thread/inject_items");
    assert!(is_empty_turn(&sent[1]), "{sent:?}");
    Codex::step(&mut state, rpc(json!({"id": sent[0]["id"], "result": {}})));
    assert!(
        !state.shared().pending_messages().is_empty(),
        "an idle inject waits for its turn"
    );
    Codex::step(
        &mut state,
        rpc(json!({"id": sent[1]["id"], "result": {"turn": {"id": "turn-1"}}})),
    );
    assert!(state.shared().pending_messages().is_empty());
}

#[test]
fn a_prompt_with_attachments_leaves_them_to_the_agent_process() {
    let (mut state, _) = Codex::initial(&wire::AgentSpec::default());
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
                    attachments: vec![wire::Attachment {
                        of: Some(wire::attachment::Of::Image(wire::BlobRef::default())),
                    }],
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
    // The commands are the ones amux runs, typed as "/name"; the skills are
    // apart, mentioned as "$name".
    assert_eq!(
        catalogue
            .commands
            .iter()
            .map(|command| command.name.as_str())
            .collect::<Vec<_>>(),
        ["compact", "clear", "review", "init", "plan"]
    );
    let skills = &catalogue.skills;
    assert_eq!(
        skills
            .iter()
            .map(|skill| (skill.name.as_str(), skill.source.as_str()))
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
        let (mut state, _) = Codex::initial(&spec);
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
    let (mut state, _) = Codex::initial(&spec);
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
    use wire::Decision::{Abort, Approve, ApproveAlways, ApproveSession, Deny};
    assert_eq!(
        decisions(json!("session")),
        [Approve, ApproveSession, Deny, Abort]
    );
    assert_eq!(
        decisions(json!("always")),
        [Approve, ApproveAlways, Deny, Abort],
        "no remember-for-session choice when Codex offers only always"
    );
    assert_eq!(
        decisions(json!(["session", "always"])),
        [Approve, ApproveSession, ApproveAlways, Deny, Abort]
    );
}

/// "Always" on a tool server's call is Codex's own choice: answered with
/// `persist: always`, which Codex writes into the person's config, and
/// recorded as that grant.
#[test]
fn an_mcp_approval_answered_always_persists_it() {
    let mut agent = Driven::started();
    agent.rpc(json!({
        "id": 0,
        "method": "mcpServer/elicitation/request",
        "params": {
            "_meta": {"codex_approval_kind": "mcp_tool_call", "persist": ["session", "always"]},
            "message": "Allow the server to run tool \"ask\"?",
            "mode": "form",
            "requestedSchema": {"properties": {}, "type": "object"},
            "serverName": "spec",
            "threadId": "t1",
            "turnId": "turn-1",
        },
    }));
    let key = agent.snapshot.asks[0].key.clone();
    let answered = agent.step(Event::Input(wire::Input::new(
        b"a1".to_vec(),
        wire::CodexInput {
            of: Some(wire::codex_input::Of::Answer(wire::CodexAnswer {
                ask_key: key,
                of: Some(wire::codex_answer::Of::Approve(
                    wire::Decision::ApproveAlways as i32,
                )),
            })),
        },
    )));
    assert_eq!(
        writes(&answered.effects),
        [
            json!({"id": 0, "result": {"action": "accept", "content": {}, "_meta": {"persist": "always"}}})
        ]
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
    let (mut state, _) = Codex::initial(&spec);
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
    let (_, step) = Codex::reincarnate(listed, &next);
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
    let (mut state, _) = Codex::initial(&spec);
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
    let (mut state, _) = Codex::initial(&script.spec);
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
    let (mut state, _) = Codex::initial(&spec);
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
        "permission={:?} mode={:?} approval={:?}",
        snapshot.permission, snapshot.mode, snapshot.approval_policy,
    );
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
    let (mut state, _) = Codex::initial(&spec);
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
    let (mut state, _) = Codex::initial(&wire::AgentSpec::default());
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
    let (mut state, _) = Codex::initial(&spec);
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
    let (mut state, _) = Codex::initial(&spec);
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
    let (mut state, _) = Codex::initial(&spec);
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
    let (mut state, _) = Codex::initial(&spec);
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
    let (mut state, _) = Codex::initial(&spec);
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
    let (mut state, _) = Codex::initial(&spec);
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
    let (mut state, _) = Codex::initial(&spec);
    Codex::step(
        &mut state,
        rpc(
            json!({"id": 2, "result": {"thread": {"id": "t1", "cliVersion": "0.162.1", "turns": []}, "model": "m"}}),
        ),
    );
    state
}

fn refused(will_retry: bool) -> Event {
    rpc(json!({"method": "error", "params": {
        "threadId": "t1", "turnId": "turn-1", "willRetry": will_retry,
        "error": {"message": "refused", "codexErrorInfo": "unauthorized", "additionalDetails": null},
    }}))
}

/// The sign-in state the step published, if it published one.
fn sign_in_of(stepped: &interpret::Stepped) -> Option<wire::SignInState> {
    use prost::Message as _;
    let snapshot = stepped.step.snapshot.as_ref()?;
    let body = wire::CodexSnapshot::decode(snapshot.body.as_slice()).unwrap();
    body.sign_in.map(|sign_in| sign_in.state())
}

/// A 401 Codex is retrying is no sign-in problem. Giving up on one raises
/// the card, and the model's next answer takes it down.
#[test]
fn only_giving_up_on_a_401_fails_the_sign_in_and_an_answer_ends_it() {
    let mut state = idle_codex();
    let retried = Codex::step(&mut state, refused(true));
    assert_ne!(sign_in_of(&retried), Some(wire::SignInState::Failed));
    let stopped = Codex::step(&mut state, refused(false));
    assert_eq!(sign_in_of(&stopped), Some(wire::SignInState::Failed));
    let answered = Codex::step(
        &mut state,
        rpc(json!({"method": "item/started", "params": {
            "threadId": "t1", "turnId": "turn-2",
            "item": {"type": "agentMessage", "id": "m1", "text": ""},
        }})),
    );
    assert_eq!(sign_in_of(&answered), Some(wire::SignInState::SignedIn));
}

/// A turn can end on a 401 with no error notification of its own, after
/// retries that said nothing about the sign-in.
#[test]
fn a_turn_that_ends_on_a_401_fails_the_sign_in() {
    let mut state = idle_codex();
    Codex::step(&mut state, refused(true));
    let ended = Codex::step(
        &mut state,
        rpc(
            json!({"method": "turn/completed", "params": {"threadId": "t1", "turn": {
                "id": "turn-1", "status": "failed",
                "error": {"message": "refused", "codexErrorInfo": "unauthorized", "additionalDetails": null},
            }}}),
        ),
    );
    assert_eq!(sign_in_of(&ended), Some(wire::SignInState::Failed));
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

/// A prompt holding an attachment this build cannot read is refused with
/// its reason, and Codex is sent nothing: it would lose the attachment
/// without a word.
#[test]
fn a_prompt_with_an_attachment_this_build_cannot_read_is_refused() {
    let mut state = idle_codex();
    let input = wire::Input::new(
        b"x1".to_vec(),
        wire::CodexInput {
            of: Some(wire::codex_input::Of::Prompt(wire::PromptInput {
                text: "see this".into(),
                attachments: vec![wire::Attachment { of: None }],
            })),
        },
    );
    let stepped = Codex::step(&mut state, Event::Input(input));
    assert!(writes(&stepped.effects).is_empty());
    assert!(
        stepped.effects.iter().any(|effect| matches!(
            effect,
            Effect::Reply { verdict, .. } if format!("{verdict:?}").contains(attachments::UNREADABLE)
        )),
        "{:?}",
        stepped.effects
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
    let (mut state, _) = Codex::initial(&spec);
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
    let (mut state, _) = Codex::initial(&spec);
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
    /// Each provider subagent's parts, by its agent id.
    subs: std::collections::BTreeMap<Vec<u8>, Sub>,
}

/// What a provider subagent's parts said, as a client would hold it.
#[derive(Default)]
struct Sub {
    starts: Vec<wire::NativeStart>,
    snapshot: Option<wire::Snapshot>,
    body: Option<wire::CodexSnapshot>,
    items: Vec<wire::Item>,
    end: Option<wire::IncarnationEnd>,
}

impl Driven {
    /// An agent whose thread `t1` has started.
    fn started() -> Self {
        use prost::Message as _;
        let spec = wire::AgentSpec {
            agent_id: b"agent".to_vec(),
            ..Default::default()
        };
        let (state, step) = Codex::initial(&spec);
        let snapshot = wire::CodexSnapshot::decode(
            step.snapshot.expect("an initial snapshot").body.as_slice(),
        )
        .unwrap();
        let mut driven = Driven {
            state,
            snapshot,
            items: Vec::new(),
            subs: Default::default(),
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
        for part in &stepped.step.agents {
            let sub = self.subs.entry(part.agent_id.clone()).or_default();
            sub.starts.extend(part.start.clone());
            if let Some(snapshot) = &part.snapshot {
                sub.body = Some(wire::CodexSnapshot::decode(snapshot.body.as_slice()).unwrap());
                sub.snapshot = Some(snapshot.clone());
            }
            sub.items.extend(part.items.iter().cloned());
            if part.end.is_some() {
                sub.end.clone_from(&part.end);
            }
        }
        stepped
    }

    /// The subagent of child thread `thread`.
    fn sub(&self, thread: &str) -> &Sub {
        self.subs
            .get(&interpret::subagent_id(b"agent", thread))
            .unwrap_or_else(|| panic!("no subagent for {thread}"))
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
        agent
            .items
            .iter()
            .any(|item| item.text.contains("routed to fallback")),
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
            serde_json::to_value(&agent.state).unwrap()["active_turn"],
            serde_json::Value::Null,
            "{method}'s late answer reopened the ended turn"
        );
    }
}

/// A later incarnation's agent, whose thread `t1` the agent process is
/// resuming with `agent-resume`.
fn resuming() -> Driven {
    use prost::Message as _;
    let mut first = Driven::started();
    let spec = wire::AgentSpec {
        agent_id: b"agent".to_vec(),
        incarnation: 2,
        ..Default::default()
    };
    let state = std::mem::replace(&mut first.state, Codex::initial(&spec).0);
    let (state, step) = Codex::reincarnate(state, &spec);
    let snapshot = wire::CodexSnapshot::decode(step.snapshot.unwrap().body.as_slice()).unwrap();
    Driven {
        state,
        snapshot,
        items: Vec::new(),
        subs: Default::default(),
    }
}

fn exits(stepped: &interpret::Stepped) -> bool {
    stepped
        .effects
        .iter()
        .any(|effect| matches!(effect, Effect::Exit { .. }))
}

/// A resume leaves the turns out, so its answer has none however long the
/// thread; that is no sign of a new thread, whose history Codex has yet to
/// write, so nothing reads the whole history back.
#[test]
fn a_resumed_thread_is_not_read_back_in_full() {
    let mut agent = resuming();
    let answered = agent.rpc(json!({"id": "agent-resume", "result": {
        "thread": {"id": "t1", "cliVersion": "0.163.0", "turns": []}, "model": "m"}}));
    assert!(
        !writes(&answered.effects)
            .iter()
            .any(|write| write["method"] == "thread/read"),
        "{:?}",
        writes(&answered.effects)
    );
}

/// An archived thread refuses a resume until it is unarchived, which
/// Codex's own words ask for: the agent unarchives it and resumes again
/// rather than ending the incarnation, which retries the same refusal.
#[test]
fn an_archived_thread_is_unarchived_and_resumed() {
    let mut agent = resuming();
    let refused = agent.rpc(json!({"id": "agent-resume", "error": {"code": -32600,
        "message": "session t1 is archived. Run `codex unarchive t1` to unarchive it first."}}));
    assert!(
        !exits(&refused),
        "the archived thread ended the incarnation"
    );
    let sent = writes(&refused.effects);
    let unarchive = sent
        .iter()
        .find(|write| write["method"] == "thread/unarchive")
        .unwrap_or_else(|| panic!("no unarchive: {sent:?}"));
    assert_eq!(unarchive["params"]["threadId"], "t1");
    let resumed = agent.rpc(json!({"id": unarchive["id"], "result": {"thread": {"id": "t1"}}}));
    let sent = writes(&resumed.effects);
    let resume = sent
        .iter()
        .find(|write| write["method"] == "thread/resume")
        .unwrap_or_else(|| panic!("no second resume: {sent:?}"));
    assert_eq!(resume["params"]["threadId"], "t1");
    assert_eq!(resume["params"]["excludeTurns"], true);
    agent.rpc(json!({"id": resume["id"], "result": {
        "thread": {"id": "t1", "cliVersion": "0.163.0", "turns": []}, "model": "m"}}));
    assert_eq!(agent.snapshot.thread_id.as_deref(), Some("t1"));
}

/// A thread whose history is gone cannot be resumed, archived or not: the
/// agent says so and starts a new thread, which it keeps from then on.
#[test]
fn a_thread_with_no_history_starts_afresh_with_a_notice() {
    let mut agent = resuming();
    let refused = agent.rpc(json!({"id": "agent-resume", "error": {"code": -32600,
        "message": "no rollout found for thread id t1"}}));
    assert!(
        !exits(&refused),
        "the missing history ended the incarnation"
    );
    assert!(
        agent.items.iter().any(|item| item.text.contains("history")),
        "the person is not told the conversation started over"
    );
    let sent = writes(&refused.effects);
    let start = sent
        .iter()
        .find(|write| write["method"] == "thread/start")
        .unwrap_or_else(|| panic!("no new thread: {sent:?}"));
    assert!(
        interpret::codex::starts_a_thread(start["id"].as_str().unwrap()),
        "the agent process would not keep the new thread"
    );
    agent.rpc(json!({"id": start["id"], "result": {
        "thread": {"id": "t2", "cliVersion": "0.163.0", "turns": []}, "model": "m"}}));
    assert_eq!(agent.snapshot.thread_id.as_deref(), Some("t2"));
}

/// `/clear` moves the agent to a new thread; Codex keeps a thread loaded,
/// with its tool servers, while a client is subscribed, so the agent
/// unsubscribes from the old thread and from the child threads it spawned,
/// as Codex's own app does when it switches threads.
#[test]
fn a_clear_unsubscribes_the_old_thread_and_its_children() {
    let mut agent = Driven::started();
    agent.rpc(
        json!({"method": "item/completed", "params": {"threadId": "t1", "turnId": "turn-1",
        "item": {"type": "collabAgentToolCall", "id": "spawn-1", "tool": "spawnAgent",
            "status": "completed", "senderThreadId": "t1", "receiverThreadIds": ["child-1"],
            "prompt": "look around", "agentsStates": {}}}}),
    );
    let cleared = agent.prompt(b"p1", "/clear");
    let start = cleared
        .iter()
        .find(|write| write["method"] == "thread/start")
        .expect("a new thread")
        .clone();
    let answered = agent.rpc(json!({"id": start["id"], "result": {
        "thread": {"id": "t2", "cliVersion": "0.163.0", "turns": []}, "model": "m"}}));
    let mut unsubscribed: Vec<String> = writes(&answered.effects)
        .iter()
        .filter(|write| write["method"] == "thread/unsubscribe")
        .map(|write| write["params"]["threadId"].as_str().unwrap().to_owned())
        .collect();
    unsubscribed.sort();
    assert_eq!(unsubscribed, ["child-1", "t1"]);
}

/// Codex closes a thread only once no client is subscribed, so its own
/// thread closing means it is gone: the incarnation ends, and the next one
/// resumes it, rather than drawing a row nobody can read.
#[test]
fn the_agents_thread_closing_ends_the_incarnation() {
    let mut agent = Driven::started();
    let closed = agent.rpc(json!({"method": "thread/closed", "params": {"threadId": "t1"}}));
    assert!(
        agent.unrecognized().is_empty(),
        "{:?}",
        agent.unrecognized()
    );
    assert!(exits(&closed), "the closed thread left the agent running");
}

/// Every message the audit of Codex 0.163 found amux drawing as an
/// "unrecognized" row is either drawn, folded into the turn's status or
/// dropped on purpose: none shows as a row nobody can read.
#[test]
fn known_codex_messages_draw_no_unrecognized_rows() {
    let mut agent = Driven::started();
    agent.rpc(
        json!({"method": "turn/started", "params": {"threadId": "t1", "turn": {"id": "turn-1"}}}),
    );
    let item = |kind: &str, extra: Value| {
        let mut item = json!({"type": kind, "id": format!("{kind}-1")});
        item.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        json!({"method": "item/completed", "params": {"threadId": "t1", "turnId": "turn-1", "item": item}})
    };
    let hook_run = json!({"id": "h1", "eventName": "preToolUse", "handlerType": "command",
        "executionMode": "sync", "scope": "turn", "sourcePath": "/h.sh", "source": "user",
        "displayOrder": 0, "status": "completed", "statusMessage": null, "startedAt": 1,
        "completedAt": 2, "durationMs": 1, "entries": []});
    for message in [
        json!({"method": "hook/started", "params": {"threadId": "t1", "turnId": "turn-1", "run": hook_run}}),
        json!({"method": "hook/completed", "params": {"threadId": "t1", "turnId": "turn-1", "run": hook_run}}),
        json!({"method": "model/verification", "params": {"threadId": "t1", "turnId": "turn-1",
            "verifications": ["trustedAccessForCyber"]}}),
        json!({"method": "autoApprovalReview/strictReviewRequired", "params": {"threadId": "t1",
            "turnId": "turn-1", "startedAtMs": 1}}),
        json!({"method": "model/safetyBuffering/updated", "params": {"threadId": "t1", "turnId": "turn-1",
            "model": "m", "useCases": [], "reasons": [], "showBufferingUi": true, "fasterModel": null}}),
        json!({"method": "model/safetyBuffering/updated", "params": {"threadId": "t1", "turnId": "turn-1",
            "model": "m", "useCases": [], "reasons": [], "showBufferingUi": false, "fasterModel": null}}),
        json!({"method": "guardianWarning", "params": {"threadId": "t1",
            "message": "Automatic approval review approved (risk: low): fine."}}),
        json!({"method": "item/mcpToolCall/progress", "params": {"threadId": "t1", "turnId": "turn-1",
            "itemId": "x", "message": "half way"}}),
        json!({"method": "item/fileChange/patchUpdated", "params": {"threadId": "t1", "turnId": "turn-1", "itemId": "x"}}),
        json!({"method": "app/list/updated", "params": {"data": []}}),
        json!({"method": "thread/queue/changed", "params": {"threadId": "t1"}}),
        json!({"method": "thread/environment/connected", "params": {"threadId": "t1"}}),
        json!({"method": "thread/environment/disconnected", "params": {"threadId": "t1"}}),
        json!({"method": "thread/attachment/updated", "params": {"threadId": "t1"}}),
        json!({"method": "thread/prediction/updated", "params": {"threadId": "t1"}}),
        json!({"method": "account/gatewayOAuth/changed", "params": {}}),
        json!({"method": "mcpServer/oauthLogin/completed", "params": {"name": "docs", "success": true}}),
        json!({"method": "mcpServer/event/stream/notification", "params": {}}),
        json!({"method": "modelProvider/authRecoveryStarted", "params": {}}),
        json!({"method": "modelProvider/authRecoveryCompleted", "params": {}}),
        json!({"method": "windows/worldWritableWarning", "params": {}}),
        json!({"method": "windowsSandbox/setupCompleted", "params": {}}),
        item("enteredReviewMode", json!({"review": "current changes"})),
        item("exitedReviewMode", json!({"review": "current changes"})),
        item("hookPrompt", json!({"fragments": []})),
        item(
            "subAgentActivity",
            json!({"kind": "started", "agentThreadId": "c1", "agentPath": "/a"}),
        ),
        item("sleep", json!({})),
        item("functionCallOutput", json!({})),
    ] {
        agent.rpc(message);
    }
    assert!(
        agent.unrecognized().is_empty(),
        "{:?}",
        agent.unrecognized()
    );
    let texts: Vec<&str> = agent.items.iter().map(|item| item.text.as_str()).collect();
    let shows = |words: &str| texts.iter().any(|text| text.contains(words));
    assert!(shows("Trusted Access for Cyber"), "{texts:?}");
    assert!(shows("additional safety checks"), "{texts:?}");
    assert!(shows("Code review started"), "{texts:?}");
    assert!(shows("Code review finished"), "{texts:?}");
    assert!(
        !shows("Automatic approval review approved"),
        "an approved review's warning repeats the verdict row: {texts:?}"
    );
}

/// Codex's slow safety check holds the answer for a while; it is the turn's
/// status, as Codex's own app shows it, open while the check runs.
#[test]
fn a_safety_check_is_the_turns_status_while_it_runs() {
    let mut agent = Driven::started();
    agent.rpc(
        json!({"method": "turn/started", "params": {"threadId": "t1", "turn": {"id": "turn-1"}}}),
    );
    let buffering = |on: bool| {
        json!({"method": "model/safetyBuffering/updated", "params": {"threadId": "t1", "turnId": "turn-1",
            "model": "m", "useCases": [], "reasons": [], "showBufferingUi": on, "fasterModel": null}})
    };
    let on = agent.rpc(buffering(true));
    let [status] = &on.step.items[..] else {
        panic!("one status item: {:?}", on.step.items);
    };
    assert!(
        !status.text.is_empty() && !matches!(status.body.as_slice(), []),
        "{status:?}"
    );
    let off = agent.rpc(buffering(false));
    assert!(
        off.step.items.iter().any(|item| item.key == status.key),
        "the check's end does not close its status"
    );
}

/// A Codex agent whose thread `t1` runs a turn while its child thread
/// `child` works on the same server, started by the parent's spawn.
fn with_a_child() -> Driven {
    let mut agent = Driven::started();
    agent.rpc(
        json!({"method": "turn/started", "params": {"threadId": "t1", "turn": {"id": "turn-1"}}}),
    );
    agent.rpc(spawned("spawn-1", "child", "started"));
    agent.rpc(
        json!({"id": "amux-read-1", "result": {"thread": {"id": "child", "agentNickname": "Ada"}}}),
    );
    agent.rpc(json!({"method": "turn/started", "params": {"threadId": "child", "turn": {"id": "child-turn"}}}));
    agent
}

/// Codex 0.166.0's word on the parent's thread that it started, or sent a
/// follow-up to, the subagent in `thread`.
fn spawned(id: &str, thread: &str, kind: &str) -> Value {
    json!({"method": "item/started", "params": {"threadId": "t1", "turnId": "turn-1",
        "item": {"type": "subAgentActivity", "id": id, "kind": kind, "agentThreadId": thread,
            "agentPath": "/a", "model": "gpt-6.1-sol", "reasoningEffort": "low"}}})
}

fn turn_ends(agent: &mut Driven) -> interpret::Stepped {
    agent.rpc(
        json!({"method": "turn/completed", "params": {"threadId": "t1",
        "turn": {"id": "turn-1", "status": "completed"}}}),
    )
}

fn child_turn_ends(agent: &mut Driven, status: &str) -> interpret::Stepped {
    agent.rpc(
        json!({"method": "turn/completed", "params": {"threadId": "child",
        "turn": {"id": "child-turn", "status": status}}}),
    )
}

/// A person's input to the subagent in `thread`.
fn to_child(thread: &str, id: &[u8], of: wire::codex_input::Of) -> Event {
    Event::ChildInput {
        agent_id: interpret::subagent_id(b"agent", thread),
        input: wire::Input::new(id.to_vec(), wire::CodexInput { of: Some(of) }),
    }
}

trait TurnEndWithStarts {
    fn turn_end_with_starts(&self) -> bool;
}

impl TurnEndWithStarts for interpret::Stepped {
    /// The step ends the agent's turn and starts a subagent.
    fn turn_end_with_starts(&self) -> bool {
        self.step.turn_end.is_some() && self.step.agents.iter().any(|part| part.start.is_some())
    }
}

fn replied(stepped: &interpret::Stepped, id: &[u8]) -> Option<bool> {
    stepped.effects.iter().find_map(|effect| match effect {
        Effect::Reply { input_id, verdict } if input_id == id => Some(matches!(
            verdict.of,
            Some(wire::send_input_response::Of::Accepted(_))
        )),
        _ => None,
    })
}

/// The spawn is one collab row in the parent's chat naming the subagent's
/// agent. The subagent's row takes its title from its start alone, so the
/// start waits for `thread/read` to answer with Codex's nickname for it.
#[test]
fn a_spawn_starts_the_subagent_from_its_call_row_once_its_nickname_is_read() {
    let mut agent = Driven::started();
    agent.rpc(
        json!({"method": "turn/started", "params": {"threadId": "t1", "turn": {"id": "turn-1"}}}),
    );
    let spawn = agent.rpc(spawned("spawn-1", "child", "started"));
    assert!(spawn.broken.is_none(), "{:?}", spawn.broken);
    let child_id = interpret::subagent_id(b"agent", "child");
    let row = spawn
        .step
        .items
        .iter()
        .find(|item| item.key == "spawn-1")
        .expect("the spawn's row in the parent's chat");
    let Ok(wire::CodexItem {
        kind: Some(wire::codex_item::Kind::Work(work)),
    }) = <wire::CodexItem as prost::Message>::decode(row.body.as_slice())
    else {
        panic!("a work row: {row:?}");
    };
    let Some(wire::work::Of::Collab(collab)) = work.of else {
        panic!("a collab row: {work:?}");
    };
    assert_eq!(collab.thread_ids, ["child"]);
    assert_eq!(collab.agent_ids, std::slice::from_ref(&child_id));
    assert!(
        spawn.step.agents.is_empty(),
        "started before its nickname was read"
    );
    let read = writes(&spawn.effects)
        .into_iter()
        .find(|write| write["method"] == "thread/read")
        .expect("the child's thread is read for its nickname");
    assert_eq!(read["params"]["threadId"], "child");
    agent.rpc(json!({"method": "turn/started", "params": {"threadId": "child", "turn": {"id": "child-turn"}}}));
    let answered = agent.rpc(
        json!({"id": read["id"], "result": {"thread": {"id": "child",
        "agentNickname": "Lovelace", "canAcceptDirectInput": false, "parentThreadId": "t1"}}}),
    );
    assert!(answered.broken.is_none(), "{:?}", answered.broken);
    let [start] = &agent.sub("child").starts[..] else {
        panic!("one start");
    };
    assert_eq!(start.provider_child_id, "child");
    assert_eq!(start.call_agent_id, b"agent");
    assert_eq!(start.call_key, "spawn-1");
    assert_eq!(start.title, "Lovelace");
    let snapshot = agent.sub("child").snapshot.clone().unwrap();
    assert_eq!(snapshot.title, None, "the title is the start's alone");
    let body = agent.sub("child").body.clone().unwrap();
    let jobs = body.background_jobs.unwrap().jobs;
    assert_eq!(jobs.len(), 1, "its own job, which its Stop names");
    assert_eq!(jobs[0].key, "child");
    assert_eq!(jobs[0].kind, wire::BackgroundKind::Subagent as i32);
    assert_eq!(body.model.as_deref(), Some("gpt-6.1-sol"));
}

/// A start held for the nickname holds what the child says meanwhile, and
/// both go out, in order, when the read answers.
#[test]
fn a_held_start_holds_the_childs_first_items_until_its_nickname_is_read() {
    let mut agent = Driven::started();
    agent.rpc(
        json!({"method": "turn/started", "params": {"threadId": "t1", "turn": {"id": "turn-1"}}}),
    );
    agent.rpc(spawned("spawn-1", "child", "started"));
    agent.rpc(json!({"method": "turn/started", "params": {"threadId": "child", "turn": {"id": "child-turn"}}}));
    let item = agent.rpc(
        json!({"method": "item/started", "params": {"threadId": "child",
        "turnId": "child-turn", "item": {"type": "agentMessage", "id": "m", "text": ""}}}),
    );
    assert!(item.broken.is_none(), "{:?}", item.broken);
    assert!(
        agent.subs.is_empty(),
        "the child's first item did not wait for its start"
    );

    let read = agent.rpc(
        json!({"id": "amux-read-1", "result": {"thread": {"id": "child", "agentNickname": "Ada"}}}),
    );
    assert!(read.broken.is_none(), "{:?}", read.broken);
    let part = &read.step.agents[0];
    assert_eq!(
        part.start.as_ref().map(|start| start.title.as_str()),
        Some("Ada")
    );
    assert_eq!(
        part.items
            .iter()
            .map(|item| item.key.as_str())
            .collect::<Vec<_>>(),
        ["m"],
        "the held item went out after the start"
    );
}

/// A nickname that does not come within the hold, or a parent's turn that
/// ends first, lets the start go out without it: titled by the task name
/// the parent gave the child where Codex says one, so a host that leaves at
/// its turn end has journaled the row.
#[test]
fn a_held_start_goes_out_after_its_hold_or_with_its_parents_turn_end() {
    let mut agent = Driven::started();
    agent.rpc(
        json!({"method": "turn/started", "params": {"threadId": "t1", "turn": {"id": "turn-1"}}}),
    );
    agent.rpc(
        json!({"method": "item/started", "params": {"threadId": "t1", "turnId": "turn-1",
        "item": {"type": "subAgentActivity", "id": "spawn-1", "kind": "started",
            "agentThreadId": "child", "agentPath": "/root/read_a"}}}),
    );
    agent.rpc(
        json!({"method": "item/started", "params": {"threadId": "child",
        "turnId": "child-turn", "item": {"type": "agentMessage", "id": "m", "text": ""}}}),
    );
    assert!(
        agent.subs.is_empty(),
        "the child's first item did not wait for its start"
    );
    let early = agent.step(Event::Tick { at_ms: 2_999 });
    assert!(early.step.agents.is_empty(), "the hold ended early");
    let late = agent.step(Event::Tick { at_ms: 3_000 });
    assert!(late.broken.is_none(), "{:?}", late.broken);
    let part = &late.step.agents[0];
    assert_eq!(
        part.start.as_ref().map(|start| start.title.as_str()),
        Some("read_a")
    );
    assert_eq!(part.items.len(), 1, "the held item went out with the start");

    let mut quiet = Driven::started();
    quiet.rpc(
        json!({"method": "turn/started", "params": {"threadId": "t1", "turn": {"id": "turn-1"}}}),
    );
    quiet.rpc(spawned("spawn-1", "child", "started"));
    let ended = turn_ends(&mut quiet);
    assert!(
        ended.turn_end_with_starts(),
        "the parent's turn end left the start held"
    );
}

/// Where Codex says what a parent asked its child, as an older Codex's
/// collab call does, the child's chat opens with it as a prompt; a
/// follow-up's opens the continued chat the same way.
#[test]
fn a_collab_calls_prompt_is_the_childs_first_item() {
    let mut agent = Driven::started();
    agent.rpc(
        json!({"method": "turn/started", "params": {"threadId": "t1", "turn": {"id": "turn-1"}}}),
    );
    let call = |id: &str, tool: &str, status: &str, prompt: &str| {
        json!({"method": if status == "completed" { "item/completed" } else { "item/started" },
            "params": {"threadId": "t1", "turnId": "turn-1",
            "item": {"type": "collabAgentToolCall", "id": id, "tool": tool, "status": status,
                "senderThreadId": "t1", "receiverThreadIds": ["child"], "prompt": prompt,
                "agentsStates": {}}}})
    };
    agent.rpc(call(
        "spawn-1",
        "spawnAgent",
        "completed",
        "Count the files.",
    ));
    let read = agent.rpc(
        json!({"id": "amux-read-1", "result": {"thread": {"id": "child", "agentNickname": "Ada"}}}),
    );
    assert!(read.broken.is_none(), "{:?}", read.broken);
    let prompt = |sub: &Sub, n: usize| {
        let item = &sub.items[n];
        let Ok(wire::CodexItem {
            kind: Some(wire::codex_item::Kind::Prompt(_)),
        }) = <wire::CodexItem as prost::Message>::decode(item.body.as_slice())
        else {
            panic!("not a prompt: {item:?}");
        };
        (item.key.clone(), item.text.clone())
    };
    assert_eq!(
        prompt(agent.sub("child"), 0),
        ("prompt:spawn-1".to_owned(), "Count the files.".to_owned())
    );
    agent.rpc(
        json!({"method": "turn/completed", "params": {"threadId": "child",
        "turn": {"id": "child-turn", "status": "completed"}}}),
    );
    assert!(agent.sub("child").end.is_some());

    let follow_up = agent.rpc(call(
        "send-1",
        "sendInput",
        "inProgress",
        "Now count lines.",
    ));
    assert!(follow_up.broken.is_none(), "{:?}", follow_up.broken);
    let sub = agent.sub("child");
    assert_eq!(sub.starts.len(), 2, "the follow-up continued it");
    assert_eq!(
        prompt(sub, sub.items.len() - 1),
        ("prompt:send-1".to_owned(), "Now count lines.".to_owned())
    );
}

/// Codex 0.166 says the prompt on the spawn call and then echoes it as the
/// child thread's own first message (seen live with `gpt-5.6-luna`): the
/// child's chat shows it once.
#[test]
fn a_childs_echo_of_its_spawn_prompt_draws_it_once() {
    let mut agent = Driven::started();
    agent.rpc(
        json!({"method": "turn/started", "params": {"threadId": "t1", "turn": {"id": "turn-1"}}}),
    );
    agent.rpc(
        json!({"method": "item/completed", "params": {"threadId": "t1", "turnId": "turn-1",
        "item": {"type": "collabAgentToolCall", "id": "spawn-1", "tool": "spawnAgent",
            "status": "completed", "senderThreadId": "t1", "receiverThreadIds": ["child"],
            "prompt": "Count the files.", "agentsStates": {}}}}),
    );
    agent.rpc(
        json!({"id": "amux-read-1", "result": {"thread": {"id": "child", "agentNickname": "Ada"}}}),
    );
    agent.rpc(json!({"method": "turn/started", "params": {"threadId": "child", "turn": {"id": "child-turn"}}}));
    for method in ["item/started", "item/completed"] {
        let echoed = agent.rpc(json!({"method": method, "params": {"threadId": "child",
            "turnId": "child-turn", "item": {"type": "userMessage", "id": "u1",
                "content": [{"type": "text", "text": "Count the files.", "text_elements": []}]}}}));
        assert!(echoed.broken.is_none(), "{:?}", echoed.broken);
    }
    let keys: Vec<&str> = agent
        .sub("child")
        .items
        .iter()
        .map(|item| item.key.as_str())
        .collect();
    assert_eq!(keys, ["prompt:spawn-1"], "the prompt is drawn twice");
}

/// A child's items are its own chat's: none reach the parent's, and the
/// subagent ends at its own turn end, finished.
#[test]
fn a_childs_items_are_its_own_and_its_turn_end_finishes_it() {
    let mut agent = with_a_child();
    let parents = agent.items.len();
    for message in [
        json!({"method": "item/started", "params": {"threadId": "child", "turnId": "child-turn",
            "item": {"type": "commandExecution", "id": "child-cmd", "command": "echo hi", "cwd": "/w",
                "status": "inProgress", "commandActions": []}}}),
        json!({"method": "item/commandExecution/outputDelta", "params": {"threadId": "child",
            "turnId": "child-turn", "itemId": "child-cmd", "delta": "hi\n"}}),
        json!({"method": "item/completed", "params": {"threadId": "child", "turnId": "child-turn",
            "item": {"type": "commandExecution", "id": "child-cmd", "command": "echo hi", "cwd": "/w",
                "status": "completed", "exitCode": 0, "aggregatedOutput": "hi\n", "commandActions": []}}}),
        json!({"method": "item/started", "params": {"threadId": "child", "turnId": "child-turn",
            "item": {"type": "agentMessage", "id": "child-msg", "text": ""}}}),
        json!({"method": "item/agentMessage/delta", "params": {"threadId": "child",
            "turnId": "child-turn", "itemId": "child-msg", "delta": "hi"}}),
        json!({"method": "item/completed", "params": {"threadId": "child", "turnId": "child-turn",
            "item": {"type": "agentMessage", "id": "child-msg", "text": "hi"}}}),
    ] {
        let stepped = agent.rpc(message);
        assert!(stepped.broken.is_none(), "{:?}", stepped.broken);
    }
    assert_eq!(
        agent.items.len(),
        parents,
        "a child's item reached the parent's chat"
    );
    let keys: Vec<&str> = agent
        .sub("child")
        .items
        .iter()
        .map(|item| item.key.as_str())
        .collect();
    assert!(
        keys.contains(&"child-cmd") && keys.contains(&"child-msg"),
        "{keys:?}"
    );
    assert_eq!(
        agent
            .sub("child")
            .snapshot
            .as_ref()
            .unwrap()
            .last_said
            .as_deref(),
        Some("hi")
    );
    let ended = child_turn_ends(&mut agent, "completed");
    assert!(ended.broken.is_none(), "{:?}", ended.broken);
    let end = agent.sub("child").end.clone().expect("the subagent ended");
    assert_eq!(end.cause, wire::ExitCause::Finished as i32);
}

/// Codex's interrupt ends a child's turn but not the command it runs: the
/// subagent stays, listing the command as its own job, until the command
/// ends, then reads stopped.
#[test]
fn an_interrupted_child_stays_until_the_command_it_left_running_ends() {
    let mut agent = with_a_child();
    agent.rpc(
        json!({"method": "item/started", "params": {"threadId": "child", "turnId": "child-turn",
        "item": {"type": "commandExecution", "id": "child-cmd", "command": "sleep 97", "cwd": "/w",
            "status": "inProgress", "commandActions": []}}}),
    );
    let interrupted = child_turn_ends(&mut agent, "interrupted");
    assert!(
        agent.sub("child").end.is_none(),
        "it ended with its command running"
    );
    let list = writes(&interrupted.effects)
        .into_iter()
        .find(|write| write["method"] == "thread/backgroundTerminals/list")
        .expect("the child's commands are listed");
    assert_eq!(list["params"]["threadId"], "child");
    agent.rpc(
        json!({"id": list["id"], "result": {"data": [{"itemId": "child-cmd",
        "processId": "4242", "command": "sleep 97", "cwd": "/w"}], "nextCursor": null}}),
    );
    let jobs = agent
        .sub("child")
        .body
        .clone()
        .unwrap()
        .background_jobs
        .unwrap()
        .jobs;
    let keys: Vec<&str> = jobs.iter().map(|job| job.key.as_str()).collect();
    assert_eq!(keys, ["child", "4242"]);
    assert!(
        agent
            .snapshot
            .background_jobs
            .as_ref()
            .is_none_or(|jobs| jobs.jobs.is_empty()),
        "the child's command was listed on the parent"
    );
    let ended = agent.rpc(
        json!({"method": "item/completed", "params": {"threadId": "child",
        "turnId": "child-turn", "item": {"type": "commandExecution", "id": "child-cmd",
            "command": "sleep 97", "cwd": "/w", "status": "failed", "exitCode": -1,
            "commandActions": []}}}),
    );
    assert!(ended.broken.is_none(), "{:?}", ended.broken);
    let end = agent.sub("child").end.clone().expect("the subagent ended");
    assert_eq!(end.cause, wire::ExitCause::Stopped as i32);
}

/// A child's command ask is the subagent's: it opens in its snapshot,
/// pointing at its own command row, outlives the parent's turn end, and
/// closes when Codex says it was answered elsewhere.
#[test]
fn a_childs_command_approval_is_the_subagents_own() {
    let mut agent = with_a_child();
    let asked = agent.rpc(
        json!({"id": 7, "method": "item/commandExecution/requestApproval",
        "params": {"threadId": "child", "turnId": "child-turn", "itemId": "child-cmd",
            "command": "rm -rf build", "cwd": "/work", "reason": "clean"}}),
    );
    assert!(
        !asked.step.items.iter().any(|item| item.key == "child-cmd"),
        "the child's command became a row in the parent's turn"
    );
    assert!(
        agent.snapshot.asks.is_empty(),
        "the ask opened on the parent"
    );
    let sub = agent.sub("child");
    assert!(sub.items.iter().any(|item| item.key == "child-cmd"));
    let asks = &sub.body.as_ref().unwrap().asks;
    assert_eq!(asks.len(), 1);
    assert_eq!(asks[0].item_key, "child-cmd");
    assert_eq!(
        sub.snapshot.as_ref().unwrap().phase,
        wire::Phase::NeedsYou as i32
    );
    let ended = turn_ends(&mut agent);
    assert!(
        writes(&ended.effects).is_empty(),
        "the parent's turn end answered the child's ask: {:?}",
        writes(&ended.effects)
    );
    assert_eq!(agent.sub("child").body.as_ref().unwrap().asks.len(), 1);
    agent.rpc(json!({"method": "serverRequest/resolved", "params": {"threadId": "child", "requestId": 7}}));
    assert!(
        agent.sub("child").body.as_ref().unwrap().asks.is_empty(),
        "Codex said it was answered"
    );
}

/// A person answers a subagent's approval from its chat: the answer is
/// Codex's JSON-RPC response to that request, and the decision lands on
/// the subagent's own command row.
#[test]
fn a_subagents_approval_is_answered_with_codexs_response() {
    let mut agent = with_a_child();
    agent.rpc(
        json!({"id": 7, "method": "item/commandExecution/requestApproval",
        "params": {"threadId": "child", "turnId": "child-turn", "itemId": "child-cmd",
            "command": "touch x", "cwd": "/work", "availableDecisions": ["accept", "cancel"]}}),
    );
    let ask = agent.sub("child").body.as_ref().unwrap().asks[0]
        .key
        .clone();
    let answered = agent.step(to_child(
        "child",
        b"a1",
        wire::codex_input::Of::Answer(wire::CodexAnswer {
            ask_key: ask,
            of: Some(wire::codex_answer::Of::Approve(
                wire::Decision::Approve as i32,
            )),
        }),
    ));
    assert!(answered.broken.is_none(), "{:?}", answered.broken);
    assert_eq!(replied(&answered, b"a1"), Some(true));
    let [response] = &writes(&answered.effects)[..] else {
        panic!("one response: {:?}", writes(&answered.effects));
    };
    assert_eq!(response["id"], 7);
    assert_eq!(response["result"]["decision"], "accept");
    assert!(agent.sub("child").body.as_ref().unwrap().asks.is_empty());
    let again = agent.step(to_child(
        "child",
        b"a2",
        wire::codex_input::Of::Answer(wire::CodexAnswer {
            ask_key: "7".into(),
            of: Some(wire::codex_answer::Of::Approve(
                wire::Decision::Approve as i32,
            )),
        }),
    ));
    assert_eq!(
        replied(&again, b"a2"),
        Some(false),
        "a stale answer was taken"
    );
}

/// A subagent takes no prompt, and a person's input to a subagent that is
/// not running is refused.
#[test]
fn a_subagent_takes_only_answers_and_stops() {
    let mut agent = with_a_child();
    let prompted = agent.step(to_child(
        "child",
        b"p1",
        wire::codex_input::Of::Prompt(wire::PromptInput {
            text: "hello".into(),
            ..Default::default()
        }),
    ));
    assert_eq!(replied(&prompted, b"p1"), Some(false));
    assert!(writes(&prompted.effects).is_empty());
    let unknown = agent.step(to_child(
        "nobody",
        b"s1",
        wire::codex_input::Of::StopJob(wire::StopJob {
            key: "nobody".into(),
        }),
    ));
    assert_eq!(replied(&unknown, b"s1"), Some(false));
}

/// A child's file-change ask shows the child's own changes.
#[test]
fn a_childs_file_change_approval_shows_its_diff() {
    let mut agent = with_a_child();
    agent.rpc(
        json!({"method": "item/started", "params": {"threadId": "child", "turnId": "child-turn",
        "item": {"type": "fileChange", "id": "child-patch", "status": "inProgress",
            "changes": [{"path": "/work/a.txt", "kind": {"type": "update", "move_path": null},
                "diff": "@@ -1 +1 @@\n-a\n+b\n"}]}}}),
    );
    agent.rpc(json!({"id": 8, "method": "item/fileChange/requestApproval",
        "params": {"threadId": "child", "turnId": "child-turn", "itemId": "child-patch"}}));
    let asks = &agent.sub("child").body.as_ref().unwrap().asks;
    let Some(wire::codex_ask::Body::FileChange(change)) = &asks[0].body else {
        panic!("a file-change ask: {asks:?}");
    };
    assert_eq!(change.changes.len(), 1, "the child's diff is missing");
    assert_eq!(asks[0].item_key, "child-patch");
}

/// A tool server's approval belongs to that server's call, not to whichever
/// call started last; a child's names its own server's call, in its own
/// chat.
#[test]
fn an_mcp_approval_names_its_own_servers_call() {
    let mut agent = with_a_child();
    agent.rpc(
        json!({"method": "item/started", "params": {"threadId": "t1", "turnId": "turn-1",
        "item": {"type": "mcpToolCall", "id": "call-spec", "server": "spec", "tool": "ask",
            "status": "inProgress", "arguments": {}}}}),
    );
    agent.rpc(
        json!({"method": "item/started", "params": {"threadId": "child", "turnId": "child-turn",
        "item": {"type": "mcpToolCall", "id": "call-docs", "server": "docs", "tool": "search",
            "status": "inProgress", "arguments": {}}}}),
    );
    agent.rpc(
        json!({"id": 9, "method": "mcpServer/elicitation/request", "params": {
        "_meta": {"codex_approval_kind": "mcp_tool_call", "tool_name": "search"},
        "message": "Allow docs to run search?", "mode": "form",
        "requestedSchema": {"properties": {}, "type": "object"},
        "serverName": "docs", "threadId": "child", "turnId": "child-turn"}}),
    );
    let ask = &agent.sub("child").body.as_ref().unwrap().asks[0];
    let Some(wire::codex_ask::Body::McpTool(tool)) = &ask.body else {
        panic!("a tool approval: {ask:?}");
    };
    assert_eq!(
        (tool.server.as_str(), tool.tool.as_str()),
        ("docs", "search")
    );
    assert_eq!(
        ask.item_key, "call-docs",
        "not pinned to the child's own call"
    );
}

/// amux's `status` called on a child's thread is the subagent saying what
/// it works on, not the agent.
#[test]
fn status_from_a_child_lands_on_the_subagent() {
    let mut agent = with_a_child();
    agent.rpc(
        json!({"method": "item/started", "params": {"threadId": "child", "turnId": "child-turn",
        "item": {"type": "mcpToolCall", "id": "st", "server": "amux", "tool": "status",
            "status": "inProgress", "arguments": {"working_on": "checking the child"}}}}),
    );
    assert_eq!(
        agent
            .sub("child")
            .snapshot
            .as_ref()
            .unwrap()
            .working_on
            .as_deref(),
        Some("checking the child")
    );
    let host = agent.state.shared().working_on().map(str::to_owned);
    assert_ne!(host.as_deref(), Some("checking the child"));
}

/// The person stops a subagent from its chat in Codex 0.166.0's three
/// steps: its turn is interrupted; the command that left running is listed
/// on its thread and ended; and the parent's running turn is steered with
/// a note, which ends the parent's `wait`. The subagent reads stopping
/// until its command ends, then stopped.
#[test]
fn a_subagent_stops_in_three_steps() {
    let mut agent = with_a_child();
    agent.rpc(
        json!({"method": "item/started", "params": {"threadId": "child", "turnId": "child-turn",
        "item": {"type": "commandExecution", "id": "child-cmd", "command": "sleep 97", "cwd": "/w",
            "status": "inProgress", "commandActions": []}}}),
    );
    let stop = agent.step(to_child(
        "child",
        b"s1",
        wire::codex_input::Of::StopJob(wire::StopJob {
            key: "child".into(),
        }),
    ));
    assert_eq!(replied(&stop, b"s1"), Some(true));
    let [interrupt] = &writes(&stop.effects)[..] else {
        panic!("one interrupt: {:?}", writes(&stop.effects));
    };
    assert_eq!(interrupt["method"], "turn/interrupt");
    assert_eq!(interrupt["params"]["threadId"], "child");
    assert_eq!(interrupt["params"]["turnId"], "child-turn");
    let own = &agent
        .sub("child")
        .body
        .as_ref()
        .unwrap()
        .background_jobs
        .as_ref()
        .unwrap()
        .jobs[0];
    assert!(own.stopping, "its own job reads stopping");
    let answered = agent.rpc(json!({"id": interrupt["id"], "result": {}}));
    let list = writes(&answered.effects)
        .into_iter()
        .find(|write| write["method"] == "thread/backgroundTerminals/list")
        .expect("its commands are listed next");
    assert_eq!(list["params"]["threadId"], "child");
    child_turn_ends(&mut agent, "interrupted");
    let listed = agent.rpc(
        json!({"id": list["id"], "result": {"data": [{"itemId": "child-cmd",
        "processId": "4242", "command": "sleep 97", "cwd": "/w"}], "nextCursor": null}}),
    );
    let written = writes(&listed.effects);
    let terminate = written
        .iter()
        .find(|write| write["method"] == "thread/backgroundTerminals/terminate")
        .expect("its command is ended");
    assert_eq!(terminate["params"]["threadId"], "child");
    assert_eq!(terminate["params"]["processId"], "4242");
    let steer = written
        .iter()
        .find(|write| write["method"] == "turn/steer")
        .expect("the parent is told");
    assert_eq!(steer["params"]["threadId"], "t1");
    assert_eq!(steer["params"]["expectedTurnId"], "turn-1");
    let note = steer["params"]["input"][0]["text"].as_str().unwrap();
    assert!(note.contains("stopped"), "{note}");
    assert!(
        agent.sub("child").end.is_none(),
        "it ended with its command running"
    );
    agent.rpc(json!({"id": terminate["id"], "result": {"terminated": true}}));
    let ended = agent.rpc(
        json!({"method": "item/completed", "params": {"threadId": "child",
        "turnId": "child-turn", "item": {"type": "commandExecution", "id": "child-cmd",
            "command": "sleep 97", "cwd": "/w", "status": "failed", "exitCode": -1,
            "commandActions": []}}}),
    );
    assert!(ended.broken.is_none(), "{:?}", ended.broken);
    let end = agent.sub("child").end.clone().expect("the subagent ended");
    assert_eq!(end.cause, wire::ExitCause::Stopped as i32);
}

/// With no turn of the parent's running, the note is recorded in its
/// thread for its next turn instead: nobody waits, and nothing starts a
/// turn on the person's behalf.
#[test]
fn a_stopped_subagents_idle_parent_has_the_note_injected() {
    let mut agent = with_a_child();
    turn_ends(&mut agent);
    let stop = agent.step(to_child(
        "child",
        b"s1",
        wire::codex_input::Of::StopJob(wire::StopJob {
            key: "child".into(),
        }),
    ));
    let interrupt = writes(&stop.effects).remove(0);
    let answered = agent.rpc(json!({"id": interrupt["id"], "result": {}}));
    let list = writes(&answered.effects).remove(0);
    let listed = agent.rpc(json!({"id": list["id"], "result": {"data": [], "nextCursor": null}}));
    let written = writes(&listed.effects);
    let methods: Vec<&str> = written
        .iter()
        .filter_map(|w| w["method"].as_str())
        .collect();
    assert_eq!(methods, ["thread/inject_items"]);
    assert_eq!(written[0]["params"]["threadId"], "t1");
}

/// A follow-up from the parent continues the same subagent, under the
/// follow-up's call in the parent's chat, and at once: nothing of the child
/// was kept past its end, and its row already has its title.
#[test]
fn a_follow_up_continues_the_subagent_under_its_own_call() {
    let mut agent = with_a_child();
    child_turn_ends(&mut agent, "completed");
    assert!(agent.sub("child").end.is_some());
    // Codex 0.166.0 names no model on a follow-up.
    let followed = agent.rpc(
        json!({"method": "item/started", "params": {"threadId": "t1", "turnId": "turn-1",
        "item": {"type": "subAgentActivity", "id": "follow-1", "kind": "interacted",
            "agentThreadId": "child", "agentPath": "/a", "model": null, "reasoningEffort": null}}}),
    );
    assert_eq!(agent.sub("child").starts.len(), 2, "started at once");
    let read = writes(&followed.effects)
        .into_iter()
        .find(|write| write["method"] == "thread/read")
        .expect("the child's thread is read again");
    agent.rpc(
        json!({"id": read["id"], "result": {"thread": {"id": "child",
        "agentNickname": "Ada", "model": "gpt-6.1-sol", "reasoningEffort": "low"}}}),
    );
    agent.rpc(
        json!({"method": "turn/started", "params": {"threadId": "child",
        "turn": {"id": "child-turn-2"}}}),
    );
    let continued = agent.rpc(
        json!({"method": "item/started", "params": {"threadId": "child",
        "turnId": "child-turn-2", "item": {"type": "agentMessage", "id": "m2", "text": ""}}}),
    );
    assert!(continued.broken.is_none(), "{:?}", continued.broken);
    let starts = &agent.sub("child").starts;
    assert_eq!(starts.len(), 2, "continued");
    assert_eq!(starts[1].call_key, "follow-1");
    assert_eq!(starts[1].call_agent_id, b"agent");
    let body = agent.sub("child").body.clone().unwrap();
    assert_eq!(
        (body.model.as_deref(), body.effort.as_deref()),
        (Some("gpt-6.1-sol"), Some("low")),
        "its model comes back from the read"
    );
}

/// Nothing of a child thread stays in the checkpoint once its subagent
/// ended, so a long-lived agent's checkpoint does not grow with every child
/// it ran.
#[test]
fn a_finished_child_leaves_nothing_in_the_checkpoint() {
    let mut agent = with_a_child();
    let running = serde_json::to_value(&agent.state).unwrap();
    assert!(running["children"].get("child").is_some(), "{running:#}");
    child_turn_ends(&mut agent, "completed");
    assert!(agent.sub("child").end.is_some());
    let ended = serde_json::to_value(&agent.state).unwrap();
    assert!(ended.get("children").is_none(), "{:#}", ended["children"]);
}

/// The note telling the parent a person stopped its child is sent again as
/// an injected item when the steer is refused, though the child ended in
/// the step that sent the steer and nothing of it is kept.
#[test]
fn a_refused_stop_note_is_sent_again_once_the_child_is_gone() {
    let mut agent = with_a_child();
    let stop = agent.step(to_child(
        "child",
        b"s1",
        wire::codex_input::Of::StopJob(wire::StopJob {
            key: "child".into(),
        }),
    ));
    let interrupt = writes(&stop.effects).remove(0);
    agent.rpc(json!({"id": interrupt["id"], "result": {}}));
    let ended = child_turn_ends(&mut agent, "interrupted");
    assert!(agent.sub("child").end.is_some());
    let steer = writes(&ended.effects)
        .into_iter()
        .find(|write| write["method"] == "turn/steer")
        .expect("the parent is told");
    let refused = agent
        .rpc(json!({"id": steer["id"], "error": {"code": -32600, "message": "no active turn"}}));
    let inject = writes(&refused.effects)
        .into_iter()
        .find(|write| write["method"] == "thread/inject_items")
        .expect("the parent is told again");
    assert_eq!(inject["params"]["threadId"], "t1");
    assert!(
        inject.to_string().contains("agent Ada you started"),
        "{inject}"
    );
}

/// The agent's process ending ends every subagent it ran, with what they
/// held open finished.
#[test]
fn a_subagent_ends_with_its_hosts_process() {
    let mut agent = with_a_child();
    agent.rpc(
        json!({"method": "item/started", "params": {"threadId": "child", "turnId": "child-turn",
        "item": {"type": "agentMessage", "id": "child-msg", "text": "half"}}}),
    );
    let exited = agent.step(Event::ProviderExit { code: Some(1) });
    assert!(exited.broken.is_none(), "{:?}", exited.broken);
    let end = agent.sub("child").end.clone().expect("the subagent ended");
    assert_eq!(end.cause, wire::ExitCause::ProviderExited as i32);
}

/// A child's request amux cannot answer is refused, and draws no row in the
/// parent.
#[test]
fn a_childs_unknown_request_draws_nothing_in_the_parent() {
    let mut agent = with_a_child();
    let refused = agent.rpc(json!({"id": 10, "method": "item/somethingNew/request",
        "params": {"threadId": "child", "turnId": "child-turn"}}));
    assert_eq!(
        writes(&refused.effects).len(),
        1,
        "the request was not refused"
    );
    assert!(
        agent.unrecognized().is_empty(),
        "{:?}",
        agent.unrecognized()
    );
}

/// A Codex agent whose server lists `skills`, each `(name, path)`.
fn with_skills(skills: &[(&str, &str)]) -> Driven {
    let mut agent = Driven::started();
    let listed: Vec<Value> = skills
        .iter()
        .map(|(name, path)| {
            json!({"name": name, "description": "", "path": path, "scope": "user", "enabled": true})
        })
        .collect();
    agent.rpc(json!({"id": "amux-skills", "result": {"data": [{"cwd": "/work", "skills": listed, "errors": []}]}}));
    agent
}

fn turn_input(writes: &[Value]) -> Vec<Value> {
    writes
        .iter()
        .find(|write| write["method"] == "turn/start")
        .unwrap_or_else(|| panic!("a turn: {writes:?}"))["params"]["input"]
        .as_array()
        .unwrap()
        .clone()
}

/// Codex selects a skill from a "$name" mention anywhere in the message, or
/// from a skill item naming its file: the text goes as typed, with an item
/// for each skill it mentions, as Codex's own app sends them.
#[test]
fn a_dollar_mention_sends_the_skill_with_its_path() {
    let mut agent = with_skills(&[("lint", "/skills/lint/SKILL.md")]);
    let input = turn_input(&agent.prompt(b"p1", "tidy this up with $lint please"));
    assert_eq!(input[0]["type"], "text");
    assert_eq!(input[0]["text"], "tidy this up with $lint please");
    assert_eq!(
        input[1..],
        [json!({"type": "skill", "name": "lint", "path": "/skills/lint/SKILL.md"})]
    );
}

/// Two skills of one name select nothing from a bare mention; the item
/// names one of them, the first Codex lists.
#[test]
fn a_mention_of_a_shared_name_names_one_skill() {
    let mut agent = with_skills(&[
        ("lint", "/repo/lint/SKILL.md"),
        ("lint", "/home/lint/SKILL.md"),
    ]);
    let input = turn_input(&agent.prompt(b"p1", "$lint"));
    let skills: Vec<&Value> = input
        .iter()
        .filter(|part| part["type"] == "skill")
        .collect();
    assert_eq!(
        skills,
        [&json!({"type": "skill", "name": "lint", "path": "/repo/lint/SKILL.md"})]
    );
}

/// "/name" is a command; a skill typed that way would reach the model as
/// words, so it is refused with the way Codex takes it.
#[test]
fn a_skill_after_a_slash_is_refused_with_its_mention() {
    let mut agent = with_skills(&[("lint", "/skills/lint/SKILL.md")]);
    let stepped = agent.step(prompt(b"p1", "/lint"));
    let refused = stepped
        .effects
        .iter()
        .find_map(|effect| match effect {
            Effect::Reply { verdict, .. } => match &verdict.of {
                Some(wire::send_input_response::Of::Rejected(rejected)) => Some(rejected.clone()),
                _ => None,
            },
            _ => None,
        })
        .expect("refused");
    assert!(
        refused
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("$lint")),
        "{refused:?}"
    );
}

/// A person stops one command Codex runs in the background: amux asks
/// Codex to terminate it by the process id Codex listed, the job reads
/// stopping until the command ends, and Codex's error shows on the job.
#[test]
fn a_stopped_background_command_reads_stopping_until_it_ends() {
    use prost::Message as _;
    let verdicts = |effects: &[Effect]| {
        effects
            .iter()
            .filter_map(|effect| match effect {
                Effect::Reply { verdict, .. } => verdict.of.clone(),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let mut state = idle_codex();
    let mut jobs: Vec<wire::BackgroundJob> = Vec::new();
    let mut step = |state: &mut State, event: Event| {
        let stepped = Codex::step(state, event);
        if let Some(snapshot) = &stepped.step.snapshot {
            jobs = wire::CodexSnapshot::decode(&snapshot.body[..])
                .unwrap()
                .background_jobs
                .unwrap_or_default()
                .jobs;
        }
        (writes(&stepped.effects), jobs.clone(), stepped)
    };
    let command = |id: &str, pid: &str| {
        json!({"type": "commandExecution", "id": id, "command": format!("cargo watch {id}"),
            "cwd": "/w", "processId": pid, "source": "unifiedExecStartup", "status": "inProgress",
            "commandActions": [], "aggregatedOutput": null, "exitCode": null, "durationMs": null})
    };
    let mut listed = Vec::new();
    for event in [
        prompt(b"p1", "watch both"),
        rpc(
            json!({"method": "turn/started", "params": {"threadId": "t1", "turn": {"id": "turn-1", "status": "inProgress"}}}),
        ),
        rpc(
            json!({"method": "item/started", "params": {"threadId": "t1", "turnId": "turn-1", "item": command("exec-1", "8911")}}),
        ),
        rpc(
            json!({"method": "item/started", "params": {"threadId": "t1", "turnId": "turn-1", "item": command("exec-2", "8912")}}),
        ),
        rpc(
            json!({"method": "turn/completed", "params": {"threadId": "t1", "turn": {"id": "turn-1", "items": [], "status": "completed", "error": null}}}),
        ),
    ] {
        listed.extend(step(&mut state, event).0);
    }
    let list = listed
        .iter()
        .find(|write| write["method"] == "thread/backgroundTerminals/list")
        .expect("Codex is asked what runs on");
    let terminal = |id: &str, pid: &str| json!({"itemId": id, "processId": pid, "command": format!("cargo watch {id}"), "cwd": "/w"});
    let (_, jobs, _) = step(
        &mut state,
        rpc(
            json!({"id": list["id"], "result": {"data": [terminal("exec-1", "8911"), terminal("exec-2", "8912")], "nextCursor": null}}),
        ),
    );
    assert_eq!(
        jobs.iter().map(|job| job.key.as_str()).collect::<Vec<_>>(),
        ["8911", "8912"]
    );
    let stop = |id: &[u8], key: &str| {
        Event::Input(wire::Input::new(
            id.to_vec(),
            wire::CodexInput {
                of: Some(wire::codex_input::Of::StopJob(wire::StopJob {
                    key: key.into(),
                })),
            },
        ))
    };
    let terminate = |writes: &[Value]| {
        writes
            .iter()
            .find(|write| write["method"] == "thread/backgroundTerminals/terminate")
            .cloned()
    };

    let (written, jobs, stopped) = step(&mut state, stop(b"s1", "8911"));
    assert!(
        matches!(
            verdicts(&stopped.effects)[..],
            [wire::send_input_response::Of::Accepted(_)]
        ),
        "{:?}",
        stopped.effects
    );
    let sent = terminate(&written).unwrap_or_else(|| panic!("no terminate in {written:?}"));
    assert_eq!(
        sent["params"],
        json!({"threadId": "t1", "processId": "8911"})
    );
    let first = jobs.iter().find(|job| job.key == "8911").unwrap();
    assert!(first.stopping, "{jobs:?}");

    let (written, _, _) = step(&mut state, stop(b"s2", "8912"));
    let refused = terminate(&written).expect("the second stop is sent");
    let (_, jobs, _) = step(
        &mut state,
        rpc(json!({"id": refused["id"], "error": {"code": -32600, "message": "no such process"}})),
    );
    let second = jobs.iter().find(|job| job.key == "8912").unwrap();
    assert!(
        !second.stopping && second.stop_error == "no such process",
        "{second:?}"
    );

    let ended = json!({"type": "commandExecution", "id": "exec-1", "command": "cargo watch exec-1",
        "cwd": "/w", "processId": "8911", "source": "unifiedExecStartup", "status": "completed",
        "commandActions": [], "aggregatedOutput": "", "exitCode": 143, "durationMs": 10});
    let (_, jobs, _) = step(
        &mut state,
        rpc(
            json!({"method": "item/completed", "params": {"threadId": "t1", "turnId": "turn-1", "item": ended}}),
        ),
    );
    assert!(jobs.iter().all(|job| job.key != "8911"), "{jobs:?}");

    let (written, _, unknown) = step(&mut state, stop(b"s3", "1"));
    assert!(matches!(
        verdicts(&unknown.effects)[..],
        [wire::send_input_response::Of::Rejected(_)]
    ));
    assert!(terminate(&written).is_none());
}

fn first_prompt_spec() -> wire::AgentSpec {
    wire::AgentSpec {
        agent_id: b"agent".to_vec(),
        incarnation: 1,
        bootstrap: Some(wire::Bootstrap {
            of: Some(wire::bootstrap::Of::Prompt(wire::PromptSubmission {
                input_id: b"first".to_vec(),
                prompt: Some(wire::PromptInput {
                    text: "Review the parser.".into(),
                    ..Default::default()
                }),
            })),
        }),
        ..Default::default()
    }
}

/// The first frame stays a bare snapshot with nothing queued; the agent's
/// first event draws its started rule, then the prompt.
fn assert_drawn_at_once(first_frame: &wire::Step, drawn: &wire::Step) {
    assert!(first_frame.items.is_empty());
    let snapshot = first_frame.snapshot.as_ref().expect("a snapshot");
    assert_eq!(snapshot.phase(), wire::Phase::Starting);
    assert!(snapshot.queue.is_empty());
    let rows: Vec<&str> = drawn.items.iter().map(|item| item.key.as_str()).collect();
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert!(rows[0].starts_with("boundary:"), "{rows:?}");
    assert_eq!(drawn.items[1].input_id, b"first");
    assert_eq!(drawn.items[1].text, "Review the parser.");
}

/// A first prompt is the first turn from the moment it is given: drawn while
/// Codex starts, sent once its thread exists, and given no second row or
/// second started rule when it is.
#[test]
fn a_first_prompt_is_drawn_while_codex_starts_and_sent_once() {
    let (mut state, first_frame) = Codex::initial(&first_prompt_spec());
    let drawn = Codex::step(&mut state, Event::Tick { at_ms: 1 });
    assert_drawn_at_once(&first_frame, &drawn.step);

    let ran = Codex::step(
        &mut state,
        rpc(
            json!({"id": 2, "result": {"thread": {"id": "t1", "cliVersion": "0.160.0", "turns": []}, "model": "m"}}),
        ),
    );
    let turns = writes(&ran.effects)
        .into_iter()
        .filter(|write| write["method"] == "turn/start")
        .count();
    assert_eq!(turns, 1, "{:?}", ran.effects);
    assert!(
        ran.step
            .items
            .iter()
            .all(|item| item.input_id != b"first" && !item.key.starts_with("boundary:")),
        "{:?}",
        ran.step.items
    );
}

const MINUTE: i64 = 60 * 1000;

/// An idle thread of 150k tokens whose last turn ended at `at`.
fn big_idle_codex(at: i64) -> State {
    let mut state = idle_codex();
    Codex::step(&mut state, Event::Tick { at_ms: at });
    Codex::step(&mut state, prompt(b"p1", "count"));
    Codex::step(
        &mut state,
        rpc(
            json!({"method": "turn/started", "params": {"threadId": "t1", "turn": {"id": "turn-1"}}}),
        ),
    );
    // Codex records the prompt, by the client id amux gave it.
    Codex::step(
        &mut state,
        rpc(
            json!({"method": "item/completed", "params": {"threadId": "t1", "turnId": "turn-1",
            "item": {"id": "u1", "type": "userMessage", "clientId": "7031",
                "content": [{"type": "text", "text": "count"}]}}}),
        ),
    );
    Codex::step(
        &mut state,
        rpc(
            json!({"method": "thread/tokenUsage/updated", "params": {"threadId": "t1", "turnId": "turn-1",
            "tokenUsage": {"last": {"totalTokens": 150_000}, "modelContextWindow": 400_000}}}),
        ),
    );
    let ended = Codex::step(
        &mut state,
        rpc(
            json!({"method": "turn/completed", "params": {"threadId": "t1", "turn": {"id": "turn-1", "status": "completed"}}}),
        ),
    );
    assert!(ended.step.turn_end.is_some());
    state
}

/// amux compacts an idle Codex thread inside the half hour OpenAI's cache
/// keeps it. Codex runs the compaction as a turn of its own, which is
/// nobody's: no turn starts or ends for the agent, a message waits for it,
/// and the compacted boundary says it ran while idle.
#[test]
fn an_idle_thread_is_compacted_inside_the_cache_lifetime() {
    let mut state = big_idle_codex(1_000);
    let due = 1_000 + 25 * MINUTE;
    assert_eq!(Codex::wake_at(&state), Some(due));
    let sent = writes(&Codex::step(&mut state, Event::Tick { at_ms: due }).effects);
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(sent[0]["method"], "thread/compact/start");
    // Until its turn starts, only the deadline for starting it is awaited.
    assert_eq!(Codex::wake_at(&state), Some(due + 2 * MINUTE));

    let held = writes(&Codex::step(&mut state, agent_message(b"e1")).effects);
    assert!(
        held.is_empty(),
        "a message waits for the compaction: {held:?}"
    );

    Codex::step(&mut state, rpc(json!({"id": sent[0]["id"], "result": {}})));
    let started = Codex::step(
        &mut state,
        rpc(
            json!({"method": "turn/started", "params": {"threadId": "t1", "turn": {"id": "turn-2"}}}),
        ),
    );
    assert_eq!(started.step.turn_end, None);
    assert_eq!(Codex::wake_at(&state), None);
    let compacted = Codex::step(
        &mut state,
        rpc(
            json!({"method": "item/completed", "params": {"threadId": "t1", "turnId": "turn-2",
            "item": {"id": "c1", "type": "contextCompaction"}}}),
        ),
    );
    let boundaries: Vec<wire::Boundary> = compacted
        .step
        .items
        .iter()
        .filter_map(|item| {
            use prost::Message as _;
            match wire::CodexItem::decode(item.body.as_slice()).ok()?.kind? {
                wire::codex_item::Kind::Boundary(boundary) => Some(boundary),
                _ => None,
            }
        })
        .collect();
    assert_eq!(
        boundaries,
        [wire::Boundary {
            kind: wire::BoundaryKind::Compacted as i32,
            idle: true,
            ..Default::default()
        }]
    );
    let over = Codex::step(
        &mut state,
        rpc(
            json!({"method": "turn/completed", "params": {"threadId": "t1", "turn": {"id": "turn-2", "status": "completed"}}}),
        ),
    );
    assert_eq!(over.step.turn_end, None, "no turn ends for it");
    let released = writes(&over.effects);
    assert_eq!(released[0]["method"], "thread/inject_items", "{released:?}");
    assert!(is_empty_turn(&released[1]), "{released:?}");
}

/// A machine that slept past the half hour finds the cache gone:
/// compacting then saves nothing.
#[test]
fn no_idle_compaction_once_the_cache_has_gone() {
    let mut state = big_idle_codex(1_000);
    let late = Codex::step(
        &mut state,
        Event::Tick {
            at_ms: 1_000 + 31 * MINUTE,
        },
    );
    assert!(writes(&late.effects).is_empty());
    assert_eq!(Codex::wake_at(&state), None);
}

/// Codex accepting the compaction and never starting a turn for it would
/// hold the queue forever: amux stops waiting after two minutes.
#[test]
fn an_idle_compaction_codex_never_starts_stops_holding_the_queue() {
    let mut state = big_idle_codex(1_000);
    let due = 1_000 + 25 * MINUTE;
    let sent = writes(&Codex::step(&mut state, Event::Tick { at_ms: due }).effects);
    Codex::step(&mut state, rpc(json!({"id": sent[0]["id"], "result": {}})));
    assert_eq!(Codex::wake_at(&state), Some(due + 2 * MINUTE));
    Codex::step(
        &mut state,
        Event::Tick {
            at_ms: due + 2 * MINUTE,
        },
    );
    assert!(!state.shared().is_housekeeping());
    let typed = writes(&Codex::step(&mut state, prompt(b"p2", "carry on")).effects);
    assert_eq!(typed[0]["method"], "turn/start", "{typed:?}");
}

/// A stop during an idle compaction interrupts the turn Codex runs it in.
#[test]
fn a_stop_interrupts_an_idle_compactions_turn() {
    let mut state = big_idle_codex(1_000);
    let due = 1_000 + 25 * MINUTE;
    let sent = writes(&Codex::step(&mut state, Event::Tick { at_ms: due }).effects);
    Codex::step(&mut state, rpc(json!({"id": sent[0]["id"], "result": {}})));
    Codex::step(
        &mut state,
        rpc(
            json!({"method": "turn/started", "params": {"threadId": "t1", "turn": {"id": "turn-2"}}}),
        ),
    );
    let stop = Event::Input(wire::Input::new(
        b"s1".to_vec(),
        wire::CodexInput {
            of: Some(wire::codex_input::Of::Interrupt(Default::default())),
        },
    ));
    let written = writes(&Codex::step(&mut state, stop).effects);
    assert_eq!(written[0]["method"], "turn/interrupt", "{written:?}");
    assert_eq!(written[0]["params"]["turnId"], "turn-2");
    let over = Codex::step(
        &mut state,
        rpc(
            json!({"method": "turn/completed", "params": {"threadId": "t1", "turn": {"id": "turn-2", "status": "interrupted"}}}),
        ),
    );
    assert_eq!(over.step.turn_end, None);
    assert!(!state.shared().is_housekeeping());
}

fn stop_child(agent: &mut Driven, thread: &str, id: &[u8]) -> interpret::Stepped {
    agent.step(to_child(
        thread,
        id,
        wire::codex_input::Of::StopJob(wire::StopJob { key: thread.into() }),
    ))
}

fn told_the_parent(writes: &[Value]) -> bool {
    writes
        .iter()
        .any(|write| write["method"] == "turn/steer" || write["method"] == "thread/inject_items")
}

/// A stopped subagent with nothing left running ends at its interrupted
/// turn's end, which can come before its command list answers: its parent
/// is still told, so its `wait` does not wait on until its timeout.
#[test]
fn a_stopped_subagent_that_ends_before_its_list_still_tells_its_parent() {
    let mut agent = with_a_child();
    let stop = stop_child(&mut agent, "child", b"s1");
    let interrupt = writes(&stop.effects).remove(0);
    let mut written = writes(&child_turn_ends(&mut agent, "interrupted").effects);
    assert!(agent.sub("child").end.is_some(), "the subagent ended");
    let answered = agent.rpc(json!({"id": interrupt["id"], "result": {}}));
    written.extend(writes(&answered.effects));
    assert!(
        told_the_parent(&written),
        "the parent was never told: {written:?}"
    );
}

/// A stop that comes once the subagent's row is out but before its turn
/// has an id interrupts the turn as soon as Codex names it.
#[test]
fn a_subagent_stopped_before_its_turn_starts_is_interrupted_when_it_does() {
    let mut agent = Driven::started();
    agent.rpc(
        json!({"method": "turn/started", "params": {"threadId": "t1", "turn": {"id": "turn-1"}}}),
    );
    agent.rpc(spawned("spawn-1", "child", "started"));
    agent.rpc(
        json!({"id": "amux-read-1", "result": {"thread": {"id": "child", "agentNickname": "Ada"}}}),
    );
    let stop = stop_child(&mut agent, "child", b"s1");
    assert_eq!(replied(&stop, b"s1"), Some(true));
    assert!(
        writes(&stop.effects).is_empty(),
        "nothing to interrupt yet: {:?}",
        writes(&stop.effects)
    );
    let started = agent.rpc(
        json!({"method": "turn/started", "params": {"threadId": "child", "turn": {"id": "child-turn"}}}),
    );
    let interrupt = writes(&started.effects)
        .into_iter()
        .find(|write| write["method"] == "turn/interrupt")
        .expect("the turn is interrupted once named");
    assert_eq!(interrupt["params"]["threadId"], "child");
    assert_eq!(interrupt["params"]["turnId"], "child-turn");
}

/// Codex refusing the interrupt leaves the subagent running, its job saying
/// why, and a second stop tries again.
#[test]
fn a_refused_interrupt_says_why_and_the_stop_can_be_tried_again() {
    let mut agent = with_a_child();
    let stop = stop_child(&mut agent, "child", b"s1");
    let interrupt = writes(&stop.effects).remove(0);
    let refused = agent.rpc(
        json!({"id": interrupt["id"], "error": {"code": -32600, "message": "no active turn"}}),
    );
    assert!(
        writes(&refused.effects).is_empty(),
        "a refused interrupt went on to list: {:?}",
        writes(&refused.effects)
    );
    let own = agent
        .sub("child")
        .body
        .as_ref()
        .unwrap()
        .background_jobs
        .as_ref()
        .unwrap()
        .jobs[0]
        .clone();
    assert!(!own.stopping, "its job still reads stopping");
    assert_eq!(own.stop_error, "no active turn");
    let again = stop_child(&mut agent, "child", b"s2");
    assert!(
        writes(&again.effects)
            .iter()
            .any(|write| write["method"] == "turn/interrupt"),
        "the second stop did nothing"
    );
}

/// A child thread takes no input, so stopping a child's own child tells
/// no one, rather than the agent's thread, which never waited on it.
#[test]
fn stopping_a_childs_child_steers_no_one() {
    let mut agent = with_a_child();
    let spawn = agent.rpc(
        json!({"method": "item/started", "params": {"threadId": "child",
        "turnId": "child-turn", "item": {"type": "subAgentActivity", "id": "spawn-2",
        "kind": "started", "agentThreadId": "grandchild", "agentPath": "/root/a/b"}}}),
    );
    let read = writes(&spawn.effects).remove(0);
    agent.rpc(json!({"id": read["id"], "result": {"thread": {"id": "grandchild", "agentNickname": "Bo"}}}));
    agent.rpc(
        json!({"method": "turn/started", "params": {"threadId": "grandchild",
        "turn": {"id": "grandchild-turn"}}}),
    );
    let stop = stop_child(&mut agent, "grandchild", b"s1");
    let interrupt = writes(&stop.effects).remove(0);
    let answered = agent.rpc(json!({"id": interrupt["id"], "result": {}}));
    let list = writes(&answered.effects).remove(0);
    let mut written = writes(
        &agent
            .rpc(json!({"id": list["id"], "result": {"data": [], "nextCursor": null}}))
            .effects,
    );
    written.extend(writes(
        &agent
            .rpc(
                json!({"method": "turn/completed", "params": {"threadId": "grandchild",
                "turn": {"id": "grandchild-turn", "status": "interrupted"}}}),
            )
            .effects,
    ));
    assert!(!told_the_parent(&written), "a thread was told: {written:?}");
}

/// What a start held when the process went belonged to that incarnation:
/// a later follow-up does not read it back.
#[test]
fn a_held_start_lets_go_of_what_it_held_when_the_process_goes() {
    use prost::Message as _;
    let mut agent = Driven::started();
    agent.rpc(
        json!({"method": "turn/started", "params": {"threadId": "t1", "turn": {"id": "turn-1"}}}),
    );
    agent.rpc(spawned("spawn-1", "child", "started"));
    agent.rpc(
        json!({"method": "item/started", "params": {"threadId": "child",
        "turnId": "child-turn", "item": {"type": "agentMessage", "id": "stale", "text": ""}}}),
    );
    let spec = wire::AgentSpec {
        agent_id: b"agent".to_vec(),
        incarnation: 2,
        ..Default::default()
    };
    let state = std::mem::replace(&mut agent.state, Codex::initial(&spec).0);
    let (state, step) = Codex::reincarnate(state, &spec);
    agent.state = state;
    agent.snapshot = wire::CodexSnapshot::decode(step.snapshot.unwrap().body.as_slice()).unwrap();
    agent.rpc(
        json!({"method": "turn/started", "params": {"threadId": "t1", "turn": {"id": "turn-2"}}}),
    );
    let follow_up = agent.rpc(
        json!({"method": "item/started", "params": {"threadId": "t1",
        "turnId": "turn-2", "item": {"type": "subAgentActivity", "id": "send-1",
        "kind": "interacted", "agentThreadId": "child"}}}),
    );
    assert!(follow_up.broken.is_none(), "{:?}", follow_up.broken);
    let keys: Vec<&str> = agent
        .sub("child")
        .items
        .iter()
        .map(|item| item.key.as_str())
        .collect();
    assert!(
        !keys.contains(&"stale"),
        "the last incarnation's item came back: {keys:?}"
    );
}

/// A held start whose held words already end the subagent ends it once
/// when the process goes.
#[test]
fn a_held_subagent_that_its_held_words_end_ends_once() {
    let mut agent = Driven::started();
    agent.rpc(
        json!({"method": "turn/started", "params": {"threadId": "t1", "turn": {"id": "turn-1"}}}),
    );
    agent.rpc(spawned("spawn-1", "child", "started"));
    agent.rpc(json!({"method": "turn/started", "params": {"threadId": "child", "turn": {"id": "child-turn"}}}));
    child_turn_ends(&mut agent, "completed");
    assert!(agent.subs.is_empty(), "the start went out before the read");
    let exited = agent.step(Event::ProviderExit { code: Some(0) });
    assert!(exited.broken.is_none(), "{:?}", exited.broken);
    let ends = exited
        .step
        .agents
        .iter()
        .filter(|part| part.end.is_some())
        .count();
    assert_eq!(ends, 1, "{:?}", exited.step.agents);
}

/// The spawn's `item/completed` can come after the child's turn is over;
/// it starts nothing again.
#[test]
fn a_spawns_late_completion_does_not_start_the_child_again() {
    let mut agent = with_a_child();
    child_turn_ends(&mut agent, "completed");
    assert!(agent.sub("child").end.is_some(), "the subagent ended");
    let mut late = spawned("spawn-1", "child", "started");
    late["method"] = json!("item/completed");
    let stepped = agent.rpc(late);
    assert!(stepped.broken.is_none(), "{:?}", stepped.broken);
    assert!(
        stepped.step.agents.iter().all(|part| part.start.is_none()),
        "it started again: {:?}",
        stepped.step.agents
    );
    assert_eq!(agent.sub("child").starts.len(), 1);
}
