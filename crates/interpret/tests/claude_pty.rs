//! The Claude PTY interpreter against the recorded terminal sessions in
//! claude-specs and authored facts for what no recording shows.

use std::path::{Path, PathBuf};

use interpret::claude_pty::ClaudePty;
use interpret::{Event, Interpreter, claude_pty_input, run_golden};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/claude_pty")
}

fn golden(name: &str) {
    run_golden::<ClaudePty>(&fixtures().join(format!("{name}.json"))).assert_ok();
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
    lifecycle_events_write_boundaries_and_interrupt => "lifecycle",
    plans_are_items_with_their_verdict => "plans",
    a_question_takes_a_reply_instead => "question_reply",
    a_question_of_several_can_be_skipped => "question_skip",
    recorded_prompt => "recorded_prompt",
    recorded_prompt_multiline => "recorded_prompt_multiline",
    recorded_tools => "recorded_tools",
    recorded_interrupt => "recorded_interrupt",
    recorded_mode_cycle => "recorded_mode_cycle",
    recorded_permission_allow_once => "recorded_permission_allow_once",
    recorded_permission_allow_scoped => "recorded_permission_allow_scoped",
    recorded_permission_deny_feedback => "recorded_permission_deny_feedback",
    recorded_plan_approve => "recorded_plan_approve",
    recorded_plan_auto => "recorded_plan_auto",
    recorded_plan_request_changes => "recorded_plan_request_changes",
    recorded_question_single => "recorded_question_single",
    recorded_question_multi_other => "recorded_question_multi_other",
    recorded_question_mixed => "recorded_question_mixed",
    recorded_question_tabs => "recorded_question_tabs",
    recorded_question_other_single => "recorded_question_other_single",
    recorded_compact_relink => "recorded_compact_relink",
    recorded_clear_relink => "recorded_clear_relink",
    recorded_task_tools => "recorded_task_tools",
    recorded_socket_delivery => "recorded_socket_delivery",
    recorded_background_shell => "recorded_background_shell",
    recorded_file_changes_and_failure => "recorded_file_changes_and_failure",
    recorded_image => "recorded_image",
    recorded_question_cancelled => "recorded_question_cancelled",
    recorded_question_every_shape => "recorded_question_every_shape",
    recorded_subagent => "recorded_subagent",
    recorded_task_list => "recorded_task_list",
    recorded_thinking => "recorded_thinking",
    recorded_steer_queued => "recorded_steer_queued",
    recorded_api_error => "recorded_api_error",
    recorded_sign_in_problem => "recorded_sign_in_problem",
    calls_open_on_their_rows_and_hooks_make_none => "calls_open_on_their_rows",
    asks_open_on_the_hook_and_close_only_on_facts => "ask_rules",
    a_late_row_for_amuxs_own_prompt_leaves_the_ask_open => "late_prompt_row",
    a_prompt_row_behind_its_hooks_still_comes_first => "prompt_row_precedes_its_calls",
    the_trust_dialog_is_a_question_a_prompt_waits_behind => "trust_dialog",
    the_trust_question_exits_or_closes_when_trusted_elsewhere => "trust_dialog_exit_and_elsewhere",
    answers_through_amux_become_terminal_inputs => "answers",
    semantic_inputs_become_effects => "inputs",
    send_now_steers_a_queued_prompt_into_the_running_turn => "steering",
    send_now_is_refused_below_its_keymap_rows_version => "send_now_refused",
    background_subagent_answers_on_its_agent_row => "subagent_notification",
    boundaries_carry_the_provider_session_and_version => "boundaries",
    rows_no_recording_shows => "rows",
    recorded_steer_send_now => "recorded_steer_send_now",
    a_dialog_no_hook_can_answer_opens_an_unanswerable_ask => "unanswerable",
    asks_offer_and_type_only_the_menus_the_keymap_knows => "permission_menus",
    a_pasted_prompt_reads_as_what_was_sent => "pasted_prompt",
    recorded_question_skip_reply => "recorded_question_skip_reply",
    recorded_live_plan => "recorded_live_plan",
    recorded_live_questions => "recorded_live_questions",
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

/// The consumption point for agent messages on a terminal Claude: an
/// accepted message stays pending, gating a one-shot exit, until Claude
/// shows it to the model; then the pending set is empty.
#[test]
fn agent_messages_leave_the_pending_set_when_claude_shows_them_to_the_model() {
    let spec = wire::AgentSpec {
        agent_id: b"recipient".to_vec(),
        ..Default::default()
    };
    let (mut state, _) = ClaudePty::initial(&spec, "test");
    for (id, text) in [
        ("env-idle", "A2A_SOCKET_IDLE_21240"),
        ("env-busy", "A2A_SOCKET_BUSY_21240"),
    ] {
        let envelope = wire::Envelope {
            id: id.as_bytes().to_vec(),
            text: text.to_owned(),
            ..Default::default()
        };
        ClaudePty::step(&mut state, Event::AgentMessage(envelope));
    }
    assert_eq!(state.shared().pending_messages().len(), 2);
    assert!(!state.shared().quiescent());
    let recording = std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../claude-specs/fixtures/claude-pty/socket_delivery.rows.jsonl"),
    )
    .unwrap();
    let mut consumed_after = Vec::new();
    for (index, event) in ClaudePty::recording("transcript_rows", &recording)
        .unwrap()
        .into_iter()
        .enumerate()
    {
        let before = state.shared().pending_messages().len();
        ClaudePty::step(&mut state, event);
        if state.shared().pending_messages().len() < before {
            consumed_after.push(index);
        }
    }
    assert_eq!(consumed_after.len(), 2, "each message consumed once");
    // The recording ends as Claude starts answering the second message, so
    // the agent is busy, but nothing it accepted is still waiting.
    assert!(state.shared().pending_messages().is_empty());
}

/// Every fixture input the shared vocabulary names has a terminal arm.
#[test]
fn the_fixture_vocabulary_maps_to_terminal_inputs() {
    let cycle = interpret::FixtureInput::CyclePermissionMode {};
    assert!(claude_pty_input(b"k".to_vec(), &cycle).is_some());
}

/// The attach tool's element in a reply's transcript row becomes an
/// attachment at its place; a malformed element stays text.
#[test]
fn an_attachment_element_in_a_reply_becomes_an_item_attachment() {
    let attachment = wire::Attachment {
        of: Some(wire::attachment::Of::Image(wire::BlobRef {
            hash: vec![0xab; 32],
            name: "chart.png".into(),
            mime: "image/png".into(),
            size: 42,
        })),
    };
    let path = Path::new("/home/me/agents/a/blobs").join("ab".repeat(32));
    let element = attachments::element(&attachment, Some(&path));
    let row = serde_json::json!({
        "type": "assistant",
        "uuid": "u1",
        "timestamp": "2026-09-28T10:00:00Z",
        "message": {"role": "assistant", "content": [{
            "type": "text",
            "text": format!("Here is the chart: {element} and a broken <amux-attachment kind=\"image\"/>."),
        }]},
    });
    let spec = wire::AgentSpec {
        agent_id: b"agent".to_vec(),
        ..Default::default()
    };
    let (mut state, _) = ClaudePty::initial(&spec, "test");
    let mut item = None;
    for event in ClaudePty::recording("transcript_rows", format!("{row}\n").as_bytes()).unwrap() {
        let step = ClaudePty::step(&mut state, event).step;
        item = step
            .items
            .into_iter()
            .find(|item| item.key == "u1")
            .or(item);
    }
    let item = item.expect("the reply's item");
    assert_eq!(
        item.text,
        "Here is the chart: \u{FFFC} and a broken <amux-attachment kind=\"image\"/>."
    );
    assert_eq!(item.attachments, vec![attachment]);
}

/// A prompt queued with a pasted image is written as content blocks, text
/// then image. Steered into the running turn, its queued-command row is
/// still the steer: the steered entry leaves the queue as a steer item.
#[test]
fn a_queued_prompt_with_an_image_is_still_the_steer() {
    use interpret::{Channel, Fact, FixtureInput};
    use prost::Message as _;
    use serde_json::{Value, json};

    let fixture: Value = serde_json::from_slice(
        &std::fs::read(fixtures().join("steering.json")).expect("steering fixture"),
    )
    .unwrap();
    let events = fixture["events"].as_array().unwrap();
    let spec = wire::AgentSpec {
        agent_id: b"agent".to_vec(),
        ..Default::default()
    };
    let (mut state, _) = ClaudePty::initial(&spec, "test");
    // Up to Claude taking the steered prompt from its queue.
    let mut queued_row = None;
    for event in events {
        if let Some(input) = event.get("input") {
            let mut input = input.as_object().unwrap().clone();
            let id = input.remove("id").unwrap();
            let fixture_input: FixtureInput = serde_json::from_value(Value::Object(input)).unwrap();
            let input =
                ClaudePty::fixture_input(id.as_str().unwrap().as_bytes().to_vec(), &fixture_input)
                    .expect("a terminal input");
            ClaudePty::step(&mut state, Event::Input(input));
            continue;
        }
        let fact = &event["fact"];
        if fact["json"]["attachment"]["type"] == "queued_command" {
            queued_row = Some(fact["json"].clone());
            break;
        }
        let channel = match fact["channel"].as_str().unwrap() {
            "hook" => Channel::Hook,
            "transcript" => Channel::Transcript,
            other => panic!("channel {other}"),
        };
        ClaudePty::step(
            &mut state,
            Event::Fact(Fact {
                channel,
                payload: fact["json"].to_string().into_bytes(),
            }),
        );
    }
    let steered = |state: &interpret::claude_pty::State| {
        state
            .shared()
            .queue()
            .entries()
            .iter()
            .any(|entry| entry.input_id == b"p1" && entry.steer)
    };
    assert!(steered(&state), "p1 is steered before its row");

    let mut row = queued_row.expect("the queued-command row");
    row["attachment"]["prompt"] = json!([
        {"type": "text", "text": "[Image #1]\n\nUse the staging config."},
        {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "iVBORw0KGgo="}},
    ]);
    row["attachment"]["imagePasteIds"] = json!([1]);
    let stepped = ClaudePty::step(
        &mut state,
        Event::Fact(Fact {
            channel: Channel::Transcript,
            payload: row.to_string().into_bytes(),
        }),
    );
    let [item] = &stepped.step.items[..] else {
        panic!("one item: {:?}", stepped.step.items);
    };
    assert_eq!(item.input_id, b"p1");
    assert_eq!(item.text, "[Image #1]\n\nUse the staging config.");
    let body = wire::ClaudePtyItem::decode(item.body.as_slice()).unwrap();
    assert!(
        matches!(body.kind, Some(wire::claude_pty_item::Kind::Steer(_))),
        "{body:?}"
    );
    assert!(!steered(&state), "p1 left the queue");
    assert!(state.shared().queue().is_empty());
}

/// A job terminal Claude listed at a turn end and then it exits: the list empties on the exit, and
/// a new incarnation from a checkpoint that still listed it starts empty.
#[test]
fn the_job_list_empties_when_terminal_claude_exits() {
    use prost::Message as _;
    let jobs = |step: &wire::Step| {
        step.snapshot.as_ref().map(|snapshot| {
            let body = wire::ClaudePtySnapshot::decode(&snapshot.body[..]).unwrap();
            let background = body.background_jobs.unwrap_or_default();
            (background.known, background.jobs.len())
        })
    };
    let (mut state, spec) =
        interpret::run_until::<ClaudePty>(&fixtures().join("rows.json"), |step| {
            jobs(step).is_some_and(|(_, running)| running > 0)
        })
        .unwrap()
        .expect("the fixture lists a job");
    let checkpoint = interpret::encode_checkpoint(&state);

    let exited = ClaudePty::step(&mut state, Event::ProviderExit { code: Some(0) });
    let listed: <ClaudePty as Interpreter>::State =
        interpret::decode_checkpoint(&checkpoint).unwrap();
    let next = wire::AgentSpec {
        incarnation: spec.incarnation + 1,
        ..spec
    };
    let (_, step) = ClaudePty::reincarnate(listed, &next, "test");
    assert_eq!(
        (jobs(&exited.step), jobs(&step)),
        (Some((true, 0)), Some((true, 0))),
        "emptied on exit, and empty in a new incarnation"
    );
}

/// The last emission of each item in a fixture, in order of first
/// appearance.
fn replayed_items(fixture: &str) -> Vec<wire::Item> {
    let frames =
        interpret::replay::<ClaudePty>(&fixtures().join(format!("{fixture}.json"))).unwrap();
    let mut items: Vec<wire::Item> = Vec::new();
    for item in frames.into_iter().flat_map(|frame| frame.step.items) {
        match items.iter_mut().find(|held| held.key == item.key) {
            Some(held) => *held = item,
            None => items.push(item),
        }
    }
    items
}

fn body(item: &wire::Item) -> Option<wire::claude_pty_item::Kind> {
    use prost::Message as _;
    wire::ClaudePtyItem::decode(item.body.as_slice())
        .unwrap()
        .kind
}

#[test]
fn an_answered_ask_records_when_it_opened_and_closed() {
    use wire::claude_pty_item::Kind;
    // A permission answered through amux.
    let decisions = replayed_items("answers")
        .iter()
        .filter_map(|item| match body(item) {
            Some(Kind::Tool(call)) => call.decision,
            _ => None,
        })
        .filter(|decision| {
            !decision.elsewhere && decision.outcome == wire::DecisionOutcome::Allowed as i32
        })
        .collect::<Vec<_>>();
    assert!(!decisions.is_empty(), "the fixture answers a permission");
    for decision in decisions {
        let (asked, decided) = (
            decision.asked_at_ms.unwrap(),
            decision.decided_at_ms.unwrap(),
        );
        assert!(asked <= decided, "asked {asked} after decided {decided}");
    }
    // A question answered through amux, as Claude recorded it.
    let questions = replayed_items("recorded_question_single")
        .iter()
        .filter_map(|item| match body(item) {
            Some(Kind::Ask(ask)) => ask.closed,
            _ => None,
        })
        .filter(|closed| closed.outcome == wire::AskOutcome::Answered as i32)
        .collect::<Vec<_>>();
    assert!(!questions.is_empty(), "the fixture answers a question");
    for closed in questions {
        let (opened, at) = (closed.opened_at_ms.unwrap(), closed.closed_at_ms.unwrap());
        assert!(opened <= at, "opened {opened} after closed {at}");
    }
    // A plan approved through amux: decided no earlier than it was drawn.
    let plans = replayed_items("recorded_plan_approve")
        .into_iter()
        .filter_map(|item| match body(&item) {
            Some(Kind::Plan(plan)) => Some((item.at_ms, plan)),
            _ => None,
        })
        .filter(|(_, plan)| plan.verdict == wire::PlanVerdict::Approved as i32)
        .collect::<Vec<_>>();
    assert!(!plans.is_empty(), "the fixture approves a plan");
    for (drawn, plan) in plans {
        let decided = plan.decided_at_ms.unwrap();
        assert!(drawn <= decided, "drawn {drawn} after decided {decided}");
    }
}
