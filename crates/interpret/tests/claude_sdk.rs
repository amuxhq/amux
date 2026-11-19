//! The Claude SDK interpreter against the recorded headless sessions in
//! fixtures/claude_sdk/live and authored facts for what no recording shows.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use interpret::claude_sdk::{ClaudeSdk, State, client_uuid};
use interpret::{Channel, Effect, Event, Fact, Housekeeping, Interpreter, run_golden};
use serde_json::{Value, json};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/claude_sdk")
}

fn golden(name: &str) {
    run_golden::<ClaudeSdk>(&fixtures().join(format!("{name}.json"))).assert_ok();
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
    an_unnamed_session_is_asked_for_a_title => "session_title",
    an_untitled_answer_leaves_the_next_prompt_to_ask => "untitled_greeting",
    lifecycle_events_write_boundaries_and_interrupt => "lifecycle",
    permission_asks_keep_scopes_reason_and_description => "asks",
    a_failed_sign_in_says_nothing_after_good_turns => "sign_in_failed_then_good_turns",
    a_limit_after_a_warning_stays_said => "limit_after_a_warning",
    a_local_command_says_no_old_block_again => "failed_turn_then_local_command",
    a_flagged_ask_offers_no_always_and_defaults_to_no => "flagged_ask",
    questions_and_plan_verdicts_are_asks => "verdicts",
    plans_are_items_with_their_verdict => "plans",
    a_stopped_question_is_dismissed => "question_stopped",
    questions_are_skipped_noted_or_replied_to => "question_answers",
    usage_health_and_sign_in_are_strip_fields => "strip",
    a_usage_windows_stated_state_ends_when_it_resets => "usage_reset",
    refusal_fallback_is_a_model_switch_row => "rows",
    a_shell_call_opens_on_its_streamed_description => "streamed_command",
    inputs_become_stream_json_writes => "inputs",
    send_now_steers_a_queued_prompt_into_the_running_turn => "steering",
    recorded_cleared => "recorded_cleared",
    recorded_compacted => "recorded_compacted",
    recorded_configured_turn => "recorded_configured_turn",
    recorded_controls => "recorded_controls",
    recorded_effort => "recorded_effort",
    recorded_elicitation_accepted => "recorded_elicitation_accepted",
    recorded_interrupted => "recorded_interrupted",
    recorded_multi_turn => "recorded_multi_turn",
    recorded_permission_callback => "recorded_permission_callback",
    recorded_plan_reviewed => "recorded_plan_reviewed",
    recorded_question_asked => "recorded_question_asked",
    recorded_resumed => "recorded_resumed",
    recorded_subagent_task => "recorded_subagent_task",
    recorded_text_turn => "recorded_text_turn",
    recorded_background_shell => "recorded_background_shell",
    recorded_elicitation_declined => "recorded_elicitation_declined",
    recorded_elicitation_link_accepted => "recorded_elicitation_link_accepted",
    recorded_failed_tool_server => "recorded_failed_tool_server",
    recorded_failing_command => "recorded_failing_command",
    recorded_image => "recorded_image",
    recorded_question_dismissed => "recorded_question_dismissed",
    recorded_question_every_shape => "recorded_question_every_shape",
    recorded_sign_in_problem => "recorded_sign_in_problem",
    recorded_task_list => "recorded_task_list",
    recorded_elicitation_cancelled => "recorded_elicitation_cancelled",
    recorded_steer_folded => "recorded_steer_folded",
    recorded_live_plan => "recorded_live_plan",
    recorded_live_questions => "recorded_live_questions",
    recorded_live_usage => "recorded_live_usage",
    recorded_subagents_parallel => "recorded_subagents_parallel",
    recorded_subagent_nested => "recorded_subagent_nested",
    recorded_subagent_ask => "recorded_subagent_ask",
    recorded_subagent_stopped => "recorded_subagent_stopped",
    recorded_subagent_continued => "recorded_subagent_continued",
    recorded_subagent_elicitation => "recorded_subagent_elicitation",
    recorded_subagent_amux_tools => "recorded_subagent_amux_tools",
    recorded_subagent_resumed => "recorded_subagent_resumed",
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

fn stream(line: Value) -> Event {
    Event::Fact(Fact {
        channel: Channel::Stream,
        payload: line.to_string().into_bytes(),
    })
}

/// A prompt holding an attachment this build cannot read is refused with
/// its reason, and nothing is written: Claude would lose it without a word.
#[test]
fn a_prompt_with_an_attachment_this_build_cannot_read_is_refused() {
    let mut state = started();
    let input = wire::Input::new(
        b"x1".to_vec(),
        wire::ClaudeSdkInput {
            of: Some(wire::claude_sdk_input::Of::Prompt(wire::PromptInput {
                text: "see this".into(),
                attachments: vec![wire::Attachment { of: None }],
            })),
        },
    );
    let stepped = ClaudeSdk::step(&mut state, Event::Input(input));
    let [Effect::Reply { verdict, .. }] = stepped.effects.as_slice() else {
        panic!("only the refusal: {:?}", stepped.effects);
    };
    assert!(format!("{verdict:?}").contains(attachments::UNREADABLE));
}

fn started() -> State {
    let spec = wire::AgentSpec {
        agent_id: b"agent".to_vec(),
        incarnation: 1,
        ..Default::default()
    };
    let (mut state, _) = ClaudeSdk::initial(&spec);
    ClaudeSdk::step(
        &mut state,
        stream(json!({"type": "system", "subtype": "init", "uuid": "i", "session_id": "s"})),
    );
    state
}

/// Every item as a reader holding the whole stream has it: full items by
/// key with appends applied, without the clock.
fn final_items(state: &mut State, events: Vec<Event>) -> BTreeMap<String, (String, Vec<u8>)> {
    let mut items = BTreeMap::new();
    for event in events {
        let step = ClaudeSdk::step(state, event).step;
        for item in step.items {
            items.insert(item.key.clone(), (item.text, item.body));
        }
        for append in step.appends {
            items
                .get_mut(&append.key)
                .expect("an append follows its item")
                .0
                .push_str(&append.text);
        }
    }
    items
}

fn block_start(index: u32, kind: &str) -> Event {
    let block = match kind {
        "text" => json!({"type": "text", "text": ""}),
        _ => json!({"type": "thinking", "thinking": ""}),
    };
    stream(
        json!({"type": "stream_event", "event": {"type": "content_block_start", "index": index, "content_block": block}}),
    )
}

fn delta(index: u32, kind: &str, text: &str) -> Event {
    let delta = match kind {
        "text" => json!({"type": "text_delta", "text": text}),
        _ => json!({"type": "thinking_delta", "thinking": text}),
    };
    stream(
        json!({"type": "stream_event", "event": {"type": "content_block_delta", "index": index, "delta": delta}}),
    )
}

fn block_stop(index: u32) -> Event {
    stream(json!({"type": "stream_event", "event": {"type": "content_block_stop", "index": index}}))
}

fn whole(block: Value) -> Event {
    stream(
        json!({"type": "assistant", "uuid": "a", "message": {"id": "msg_1", "role": "assistant", "content": [block]}}),
    )
}

/// Streamed deltas split anywhere, deltas without the whole messages, and
/// the whole messages alone all end in the same items under the same keys.
#[test]
fn split_and_coalesced_stream_events_yield_the_same_final_items() {
    let start = stream(
        json!({"type": "stream_event", "event": {"type": "message_start", "message": {"id": "msg_1"}}}),
    );
    let thinking = json!({"type": "thinking", "thinking": "Seven is prime.", "signature": "s"});
    let text = json!({"type": "text", "text": "SEVEN, a prime."});

    let coalesced = vec![whole(thinking.clone()), whole(text.clone())];
    let split = vec![
        start.clone(),
        block_start(0, "thinking"),
        delta(0, "thinking", "Seven "),
        delta(0, "thinking", "is prime."),
        whole(thinking.clone()),
        block_stop(0),
        block_start(1, "text"),
        delta(1, "text", "SEVEN"),
        delta(1, "text", ", a "),
        delta(1, "text", "prime."),
        whole(text.clone()),
        block_stop(1),
    ];
    let resplit = vec![
        start,
        block_start(0, "thinking"),
        delta(0, "thinking", "Seven is prime."),
        block_stop(0),
        whole(thinking),
        block_start(1, "text"),
        delta(1, "text", "SEVEN, a prime."),
        block_stop(1),
        whole(text),
    ];
    let expected = final_items(&mut started(), coalesced);
    assert_eq!(
        expected.keys().collect::<Vec<_>>(),
        ["msg_1:0", "msg_1:1"],
        "keys are the block's index within its message"
    );
    assert_eq!(final_items(&mut started(), split), expected);
    assert_eq!(final_items(&mut started(), resplit), expected);
}

fn agent_message(envelope: &[u8], text: &str) -> Event {
    Event::AgentMessage(wire::Envelope {
        id: envelope.to_vec(),
        body: Some(wire::envelope::Body::Message(wire::MessageBody {
            text: text.to_owned(),
            ..Default::default()
        })),
        ..Default::default()
    })
}

/// The daemon hands a message over again when it never saw the first
/// answer: the parent takes it once, and each hand-off is accepted.
#[test]
fn a_message_handed_over_twice_is_taken_once() {
    let mut state = started();
    let mut injects = 0;
    let mut items = 0;
    let mut accepted = 0;
    let mut hand_over = |state: &mut State| {
        let stepped = ClaudeSdk::step(state, agent_message(b"env-1", "hello"));
        injects += stepped
            .effects
            .iter()
            .filter(|effect| matches!(effect, Effect::Inject { .. }))
            .count();
        items += stepped.step.items.len();
        accepted += stepped
            .effects
            .iter()
            .filter(|effect| {
                matches!(effect, Effect::Reply { input_id, verdict }
                    if input_id == b"env-1"
                        && matches!(verdict.of, Some(wire::send_input_response::Of::Accepted(_))))
            })
            .count();
    };
    hand_over(&mut state);
    // Again while it is pending, then again once Claude took it.
    hand_over(&mut state);
    ClaudeSdk::step(
        &mut state,
        stream(
            json!({"type": "command_lifecycle", "command_uuid": client_uuid(b"env-1"), "state": "started"}),
        ),
    );
    assert!(state.shared().pending_messages().is_empty());
    hand_over(&mut state);
    assert_eq!((injects, items, accepted), (1, 1, 3));
    assert!(state.shared().pending_messages().is_empty());
}

/// An idle Claude runs a handed agent message as a turn at once, so a stop
/// that arrives before Claude says the turn started still interrupts it,
/// and the turn that ends reads stopped.
#[test]
fn a_stop_after_a_message_handed_to_an_idle_claude_interrupts_its_turn() {
    let mut state = started();
    let handed = ClaudeSdk::step(&mut state, agent_message(b"env-1", "look at the build"));
    assert!(
        handed
            .effects
            .iter()
            .any(|effect| matches!(effect, Effect::Inject { .. })),
        "{:?}",
        handed.effects
    );
    let stopped = ClaudeSdk::step(&mut state, Event::StopRequested);
    let written: Vec<String> = stopped
        .effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::ProviderWrite(bytes) => Some(String::from_utf8_lossy(bytes).into_owned()),
            _ => None,
        })
        .collect();
    assert!(
        written
            .iter()
            .any(|line| line.contains("control_request") && line.contains("interrupt")),
        "the stop interrupts the message's turn: {written:?}"
    );

    let ended = ClaudeSdk::step(
        &mut state,
        stream(json!({"type": "result", "subtype": "success", "uuid": "r", "duration_ms": 10})),
    );
    assert_eq!(
        ended.step.turn_end.map(|end| end.outcome),
        Some(wire::TurnOutcome::Stopped as i32)
    );
}

/// The SDK probe found the uuid on a stdin message echoed on its replay and
/// lifecycle frames, and found messages written during a turn folding into
/// it: reflections are matched by that id, not by arrival order.
#[test]
fn reflections_are_matched_by_the_messages_uuid_not_arrival_order() {
    let mut state = started();
    let mut uuids = Vec::new();
    for (envelope, text) in [(b"env-1", "first"), (b"env-2", "second")] {
        let stepped = ClaudeSdk::step(&mut state, agent_message(envelope, text));
        assert!(stepped.effects.iter().any(|effect| matches!(
            effect,
            Effect::Inject { id, .. } if id == envelope
        )));
        uuids.push(client_uuid(envelope));
    }
    assert_eq!(state.shared().pending_messages().len(), 2);

    // The second message is taken first: a FIFO match would clear the first.
    ClaudeSdk::step(
        &mut state,
        stream(json!({"type": "command_lifecycle", "command_uuid": uuids[1], "state": "started"})),
    );
    let pending = state.shared().pending_messages();
    assert!(pending.contains(&b"env-1".to_vec()), "{pending:?}");
    assert!(!pending.contains(&b"env-2".to_vec()), "{pending:?}");

    // Its replay reflects by the same id.
    ClaudeSdk::step(
        &mut state,
        stream(
            json!({"type": "user", "isReplay": true, "uuid": uuids[0], "message": {"role": "user", "content": "first"}}),
        ),
    );
    assert!(state.shared().pending_messages().is_empty());

    // Claude answered them in one turn.
    ClaudeSdk::step(
        &mut state,
        stream(json!({"type": "result", "subtype": "success", "uuid": "r", "duration_ms": 10})),
    );

    // A person's prompt carries its input id as the message uuid, and its
    // item is keyed by it, the key Claude's own record uses.
    let prompt = Event::Input(wire::Input::new(
        b"prompt-1".to_vec(),
        wire::ClaudeSdkInput {
            of: Some(wire::claude_sdk_input::Of::Prompt(wire::PromptInput {
                text: "go".into(),
                ..Default::default()
            })),
        },
    ));
    let stepped = ClaudeSdk::step(&mut state, prompt);
    let uuid = client_uuid(b"prompt-1");
    assert!(
        stepped.effects.iter().any(
            |effect| matches!(effect, Effect::UserMessage { uuid: sent, .. } if *sent == uuid)
        )
    );
    let item = stepped
        .step
        .items
        .iter()
        .find(|item| item.key == uuid)
        .expect("the prompt's item");
    assert_eq!(item.input_id, b"prompt-1");
}

#[test]
fn client_uuids_are_well_formed() {
    let uuid = client_uuid(&[0xff; 16]);
    assert_eq!(uuid, "ffffffff-ffff-4fff-bfff-ffffffffffff");
    assert_eq!(client_uuid(b"p1").len(), 36);
}

#[test]
fn the_initialize_answer_lists_models_with_efforts_and_commands_with_sources() {
    let replayed =
        interpret::replay::<ClaudeSdk>(&fixtures().join("recorded_multi_turn.json")).unwrap();
    let snapshot = replayed
        .iter()
        .rev()
        .find_map(|frame| frame.step.snapshot.clone())
        .expect("a snapshot");
    let (_, snapshot) = replayed
        .into_iter()
        .rev()
        .find_map(|frame| frame.catalogue)
        .filter(|(hash, _)| snapshot.catalogue.as_ref() == Some(hash))
        .expect("the catalogue the last snapshot names");
    let first = &snapshot.models[0];
    assert_eq!(first.value, "opus");
    assert!(snapshot.models.iter().all(|model| model.value != "default"));
    assert_eq!(first.efforts, ["low", "medium", "high", "xhigh", "max"]);
    let haiku = snapshot
        .models
        .iter()
        .find(|model| model.value == "claude-haiku-4-5-20251001")
        .unwrap();
    assert!(haiku.efforts.is_empty(), "Haiku 4.5 takes no effort");
    let own = snapshot
        .commands
        .iter()
        .find(|command| command.name == "compact")
        .unwrap();
    assert_eq!(own.source, "");
}

/// A plugin's command is named after its plugin, which is its source. The
/// recordings run with no plugins of the machine's own, so the answer is
/// written here.
#[test]
fn a_plugin_command_names_its_plugin_as_its_source() {
    let mut state = started();
    let stepped = ClaudeSdk::step(
        &mut state,
        stream(initialized(&["compact", "stripe:test-cards"])),
    );
    let (_, catalogue) =
        written_catalogue(&stepped).expect("the initialize answer writes a catalogue");
    let sources: Vec<_> = catalogue
        .commands
        .iter()
        .map(|command| (command.name.as_str(), command.source.as_str()))
        .collect();
    assert_eq!(sources, [("compact", ""), ("stripe:test-cards", "stripe")]);
}

/// The attach tool's element in a finished reply becomes an attachment at
/// its place; a malformed element stays text.
#[test]
fn an_attachment_element_in_a_reply_becomes_an_item_attachment() {
    let (element, attachment) = attached_chart();
    let reply =
        format!("Here is the chart: {element} and a broken <amux-attachment kind=\"image\"/>.");
    let mut state = started();
    let mut item = None;
    for event in [
        stream(
            json!({"type": "stream_event", "event": {"type": "message_start", "message": {"id": "msg_1"}}}),
        ),
        block_start(0, "text"),
        delta(0, "text", &reply[..20]),
        delta(0, "text", &reply[20..]),
        block_stop(0),
    ] {
        let step = ClaudeSdk::step(&mut state, event).step;
        item = step
            .items
            .into_iter()
            .rev()
            .find(|item| item.key == "msg_1:0")
            .or(item);
    }
    let item = item.expect("the reply's item");
    assert_eq!(
        item.text,
        "Here is the chart: \u{FFFC} and a broken <amux-attachment kind=\"image\"/>."
    );
    assert_eq!(item.attachments, vec![attachment]);
}

/// What the attach tool answers for an image: its element, naming the blob
/// by hash and the file's path on this host.
fn attached_chart() -> (String, wire::Attachment) {
    let attachment = wire::Attachment {
        of: Some(wire::attachment::Of::Image(wire::BlobRef {
            hash: vec![0xab; 32],
            name: "chart.png".into(),
            mime: "image/png".into(),
            size: 42,
        })),
    };
    let path = Path::new("/home/me/agents/a/blobs").join("ab".repeat(32));
    (attachments::element(&attachment, Some(&path)), attachment)
}

fn sdk_input(id: &[u8], of: wire::claude_sdk_input::Of) -> Event {
    Event::Input(wire::Input::new(
        id.to_vec(),
        wire::ClaudeSdkInput { of: Some(of) },
    ))
}

/// Stop cancels the running turn, and before Claude takes input there is
/// none: the prompt waits in the queue, the interrupt is accepted with
/// nothing to cancel, and the prompt's turn runs once Claude starts. Who
/// stops a child must wait for its turn to be running.
#[test]
fn an_interrupt_before_claude_starts_leaves_the_queued_prompt_to_run() {
    let spec = wire::AgentSpec {
        agent_id: b"agent".to_vec(),
        incarnation: 1,
        ..Default::default()
    };
    let (mut state, _) = ClaudeSdk::initial(&spec);
    let prompt = wire::claude_sdk_input::Of::Prompt(wire::PromptInput {
        text: "Run the test suite and report.".into(),
        ..Default::default()
    });
    let queued = ClaudeSdk::step(&mut state, sdk_input(b"first", prompt));
    assert!(
        !queued
            .effects
            .iter()
            .any(|effect| matches!(effect, Effect::UserMessage { .. }))
    );
    let snapshot = queued.step.snapshot.expect("a snapshot");
    assert_eq!(snapshot.phase(), wire::Phase::Starting);
    assert!(snapshot.queue.is_empty());

    let interrupt = wire::claude_sdk_input::Of::Interrupt(wire::Interrupt {});
    let stopped = ClaudeSdk::step(&mut state, sdk_input(b"stop", interrupt));
    assert!(
        stopped.effects.iter().any(|effect| matches!(
            effect,
            Effect::Reply { input_id, verdict } if input_id == b"stop"
                && matches!(verdict.of, Some(wire::send_input_response::Of::Accepted(_)))
        )),
        "{:?}",
        stopped.effects
    );
    assert!(
        !stopped
            .effects
            .iter()
            .any(|effect| matches!(effect, Effect::ProviderWrite(_))),
        "nothing to cancel is written: {:?}",
        stopped.effects
    );

    let ran = ClaudeSdk::step(
        &mut state,
        stream(json!({"type": "system", "subtype": "init", "uuid": "i", "session_id": "s"})),
    );
    let uuid = client_uuid(b"first");
    assert!(
        ran.effects.iter().any(
            |effect| matches!(effect, Effect::UserMessage { uuid: sent, .. } if *sent == uuid)
        ),
        "{:?}",
        ran.effects
    );
    let snapshot = ran.step.snapshot.expect("a snapshot");
    assert_eq!(snapshot.phase(), wire::Phase::Working);
    assert!(snapshot.queue.is_empty());
}

/// A job headless Claude stated and then it exits: the list empties on the exit, and
/// a new incarnation from a checkpoint that still listed it starts empty.
#[test]
fn the_job_list_empties_when_headless_claude_exits() {
    use prost::Message as _;
    let jobs = |step: &wire::Step| {
        step.snapshot.as_ref().map(|snapshot| {
            let body = wire::ClaudeSnapshot::decode(&snapshot.body[..]).unwrap();
            let background = body.background_jobs.unwrap_or_default();
            (background.known, background.jobs.len())
        })
    };
    let (mut state, spec) =
        interpret::run_until::<ClaudeSdk>(&fixtures().join("strip.json"), |step| {
            jobs(step).is_some_and(|(_, running)| running > 0)
        })
        .unwrap()
        .expect("the fixture lists a job");
    let checkpoint = interpret::encode_checkpoint(&state);

    let exited = ClaudeSdk::step(&mut state, Event::ProviderExit { code: Some(0) });
    let listed: <ClaudeSdk as Interpreter>::State =
        interpret::decode_checkpoint(&checkpoint).unwrap();
    let next = wire::AgentSpec {
        incarnation: spec.incarnation + 1,
        ..spec
    };
    let (_, step) = ClaudeSdk::reincarnate(listed, &next);
    assert_eq!(
        (jobs(&exited.step), jobs(&step)),
        (Some((true, 0)), Some((true, 0))),
        "emptied on exit, and empty in a new incarnation"
    );
}

/// Claude names the path a command would write as `blocked_path` even
/// inside the agent's folder, as QA recorded for `touch decided.txt`: that
/// ask has no reason. A path beyond the folder, including a sibling whose
/// name merely starts the same, is outside the allowed directories.
#[test]
fn only_a_blocked_path_beyond_the_agents_folder_is_outside_the_allowed_directories() {
    use prost::Message as _;
    let spec = wire::AgentSpec {
        agent_id: b"agent".to_vec(),
        incarnation: 1,
        cwd: "/net/desk/work".to_owned(),
        ..Default::default()
    };
    let (mut state, _) = ClaudeSdk::initial(&spec);
    ClaudeSdk::step(
        &mut state,
        stream(json!({"type": "system", "subtype": "init", "uuid": "i", "session_id": "s"})),
    );
    let reason = |state: &mut State, id: &str, path: &str| {
        let stepped = ClaudeSdk::step(
            state,
            stream(
                json!({"type": "control_request", "request_id": id, "request": {
                "subtype": "can_use_tool", "tool_name": "Bash", "tool_use_id": id,
                "input": {"command": format!("touch {path}")},
                "permission_suggestions": [], "blocked_path": path}}),
            ),
        );
        let snapshot = stepped.step.snapshot.expect("the ask changes the snapshot");
        let body = wire::ClaudeSnapshot::decode(&snapshot.body[..]).unwrap();
        let ask = body
            .asks
            .iter()
            .find(|ask| ask.key == id)
            .expect("the ask is listed");
        match &ask.body {
            Some(wire::ask::Body::Permission(permission)) => permission.reason.clone(),
            other => panic!("not a permission ask: {other:?}"),
        }
    };
    assert_eq!(reason(&mut state, "in", "/net/desk/work/decided.txt"), "");
    assert_eq!(
        reason(&mut state, "out", "/net/desk/other/decided.txt"),
        "outside the allowed directories: /net/desk/other/decided.txt"
    );
    assert_eq!(
        reason(&mut state, "sibling", "/net/desk/workshop/decided.txt"),
        "outside the allowed directories: /net/desk/workshop/decided.txt"
    );
}

fn initialized(commands: &[&str]) -> Value {
    json!({
        "type": "control_response",
        "response": {
            "subtype": "success",
            "request_id": "init-1",
            "response": {
                "commands": commands.iter().map(|name| json!({
                    "name": name, "description": "", "argumentHint": ""
                })).collect::<Vec<_>>(),
                "agents": [],
                "output_style": "default",
                "available_output_styles": [],
                "models": [{
                    "value": "opus", "displayName": "Opus", "description": "",
                    "resolvedModel": "claude-opus-5-5"
                }],
                "account": {}
            }
        }
    })
}

fn commands_changed(commands: &[&str]) -> Value {
    json!({
        "type": "system", "subtype": "commands_changed", "uuid": "c", "session_id": "s",
        "commands": commands.iter().map(|name| json!({
            "name": name, "description": "", "argumentHint": ""
        })).collect::<Vec<_>>(),
    })
}

/// The catalogue a step wrote, with its hash: the blob its snapshot names.
fn written_catalogue(stepped: &interpret::Stepped) -> Option<(Vec<u8>, wire::Catalogue)> {
    use prost::Message as _;
    let named = stepped.step.snapshot.as_ref()?.catalogue.as_ref()?;
    stepped.effects.iter().find_map(|effect| match effect {
        interpret::Effect::WriteBlob { hash, bytes } if hash == named => Some((
            hash.clone(),
            wire::Catalogue::decode(bytes.as_slice()).unwrap(),
        )),
        _ => None,
    })
}

/// Claude says its commands changed when a skill or plugin appears
/// mid-session: the catalogue is rebuilt with them and the snapshot names
/// the new one. The same list again writes nothing.
#[test]
fn a_commands_changed_event_rebuilds_the_offered_commands() {
    use sha2::Digest as _;
    let mut state = started();
    let first = ClaudeSdk::step(&mut state, stream(initialized(&["compact"])));
    let (_, first) = written_catalogue(&first).expect("the initialize answer writes a catalogue");
    assert_eq!(first.commands.len(), 1, "the initialize answer was read");
    let stepped = ClaudeSdk::step(
        &mut state,
        stream(commands_changed(&["compact", "new-skill"])),
    );
    let (hash, catalogue) = written_catalogue(&stepped).expect("the commands changed");
    let names: Vec<_> = catalogue
        .commands
        .iter()
        .map(|command| command.name.as_str())
        .collect();
    assert_eq!(names, ["compact", "new-skill"]);
    assert_eq!(catalogue.models, first.models, "the models stay");
    let encoded = {
        use prost::Message as _;
        catalogue.encode_to_vec()
    };
    assert_eq!(hash, sha2::Sha256::digest(&encoded).to_vec());
    let again = ClaudeSdk::step(
        &mut state,
        stream(commands_changed(&["compact", "new-skill"])),
    );
    assert!(
        !again
            .effects
            .iter()
            .any(|effect| matches!(effect, interpret::Effect::WriteBlob { .. })),
        "the same list writes nothing"
    );
    assert!(again.step.snapshot.is_none());
}

/// Claude's permissions, as the catalogue lists them for a Claude launched
/// with `args` whose initialize answer offers an auto-capable model and one
/// that is not.
fn permissions_launched_with(args: &[&str]) -> Vec<(String, Vec<String>, bool)> {
    let spec = wire::AgentSpec {
        agent_id: b"agent".to_vec(),
        incarnation: 1,
        provider_args: args.iter().map(|arg| (*arg).to_owned()).collect(),
        ..Default::default()
    };
    let (mut state, _) = ClaudeSdk::initial(&spec);
    let mut answer = initialized(&["compact"]);
    answer["response"]["response"]["models"] = json!([
        {"value": "opus", "displayName": "Opus", "description": "", "supportsAutoMode": true},
        {"value": "haiku", "displayName": "Haiku", "description": ""},
    ]);
    let stepped = ClaudeSdk::step(&mut state, stream(answer));
    written_catalogue(&stepped)
        .expect("the initialize answer writes a catalogue")
        .1
        .permissions
        .into_iter()
        .map(|offered| (offered.value, offered.models, offered.settable))
        .collect()
}

/// Ask, accept edits, plan and auto (for the models that take it); never
/// ask only when Claude was launched allowing it.
#[test]
fn claude_offers_never_ask_only_when_launched_allowing_it() {
    let auto = ("auto".to_owned(), vec!["opus".to_owned()], true);
    let listed = |value: &str| (value.to_owned(), Vec::new(), true);
    assert_eq!(
        permissions_launched_with(&[]),
        [
            listed("default"),
            listed("acceptEdits"),
            listed("plan"),
            auto.clone()
        ]
    );
    for allowing in [
        &["--allow-dangerously-skip-permissions"][..],
        &["--dangerously-skip-permissions"],
        &["--permission-mode", "bypassPermissions"],
    ] {
        assert_eq!(
            permissions_launched_with(allowing).last(),
            Some(&listed("bypassPermissions")),
            "{allowing:?}"
        );
    }
}

/// A permission is set by a value the catalogue offers; any other is
/// refused before it reaches Claude.
#[test]
fn a_permission_claude_does_not_offer_is_refused() {
    let mut state = started();
    let refused = ClaudeSdk::step(
        &mut state,
        sdk_input(
            b"p1",
            wire::claude_sdk_input::Of::Permission(wire::SetPermission {
                value: "bypassPermissions".into(),
            }),
        ),
    );
    assert!(
        refused
            .effects
            .iter()
            .all(|effect| !matches!(effect, Effect::ProviderWrite(_))),
        "{:?}",
        refused.effects
    );
    let set = ClaudeSdk::step(
        &mut state,
        sdk_input(
            b"p2",
            wire::claude_sdk_input::Of::Permission(wire::SetPermission {
                value: "acceptEdits".into(),
            }),
        ),
    );
    let written: Vec<_> = set
        .effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::ProviderWrite(bytes) => Some(String::from_utf8_lossy(bytes).into_owned()),
            _ => None,
        })
        .collect();
    assert!(
        written
            .iter()
            .any(|line| line.contains("set_permission_mode") && line.contains("acceptEdits")),
        "{written:?}"
    );
}

/// Claude says it is compacting before the compaction lands: the status is
/// an item, typed with Claude's word kept, ahead of the compaction row, so
/// the activity line can say "Compacting" meanwhile. The status that only
/// ends the compaction (a null status) is no item.
#[test]
fn compacting_status_is_an_item() {
    use prost::Message as _;
    use wire::claude_item::Kind;

    let replayed =
        interpret::replay::<ClaudeSdk>(&fixtures().join("recorded_compacted.json")).unwrap();
    let kinds: Vec<(String, Kind)> = replayed
        .iter()
        .flat_map(|frame| &frame.step.items)
        .filter_map(|item| {
            let kind = wire::ClaudeItem::decode(item.body.as_slice()).ok()?.kind?;
            // Claude's other statuses ("requesting") are items too.
            let kept = match &kind {
                Kind::Status(status) => status.status == wire::SdkStatus::Compacting as i32,
                Kind::Compaction(_) => true,
                _ => false,
            };
            kept.then(|| (item.key.clone(), kind))
        })
        .collect();
    assert_eq!(
        kinds,
        [
            (
                "0f824bf4-6639-415d-bacc-1515a418fa59".to_owned(),
                Kind::Status(wire::Status {
                    status: wire::SdkStatus::Compacting as i32,
                    word: "compacting".into(),
                }),
            ),
            (
                "343f2fc0-3b5d-49e9-af38-8578cd8d5255".to_owned(),
                Kind::Compaction(wire::Compaction {
                    tokens_before: Some(21_378),
                    tokens_after: Some(4_266),
                    automatic: false,
                    idle: false,
                }),
            ),
        ]
    );
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

/// A failed report carries its cause as data; the interpreter words it, and
/// Claude is told in those words under its sender's label, while the
/// transcript shows the words alone.
#[test]
fn a_failed_report_is_worded_for_the_parents_provider() {
    let mut state = started();
    let stepped = ClaudeSdk::step(&mut state, failed_report(b"env-failed"));
    let injected = stepped.effects.iter().find_map(|effect| match effect {
        Effect::Inject { text, .. } => Some(text.clone()),
        _ => None,
    });
    assert_eq!(
        injected.as_deref(),
        Some("[agent child stopped: the provider exited: exit code 1]")
    );
    assert_eq!(
        stepped.step.items[0].text,
        "the provider exited: exit code 1"
    );
}

/// A child resumed by its parent's message takes it as its first prompt,
/// labelled with its sender as a message handed over live is: without
/// the label the model takes the parent's words for its person's.
#[test]
fn a_parents_message_that_resumes_its_child_reaches_claude_labelled() {
    let spec = wire::AgentSpec {
        agent_id: b"agent".to_vec(),
        incarnation: 1,
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
    let (mut state, _) = ClaudeSdk::initial(&spec);
    let stepped = ClaudeSdk::step(
        &mut state,
        stream(json!({"type": "system", "subtype": "init", "uuid": "i", "session_id": "s"})),
    );
    let sent = stepped.effects.iter().find_map(|effect| match effect {
        Effect::UserMessage { text, .. } => Some(text.clone()),
        _ => None,
    });
    assert_eq!(
        sent.as_deref(),
        Some("[message from agent lead]\nPick it up again.")
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

/// A model chosen from amux's settings goes to Claude as a `set_model`
/// control request, and Claude replays its "Set model to" note as a local
/// command's output, as QA recorded on Claude 2.1.296. No slash command was
/// typed, so the note is not a slash row with an empty command (which the
/// terminal client drew as a lone "/").
#[test]
fn a_model_set_from_settings_is_not_a_slash_command_with_no_name() {
    use prost::Message as _;
    let mut state = started();
    let replay = stream(json!({
        "type": "user", "isReplay": true, "session_id": "s",
        "uuid": "5dd62009-d451-43ef-8f39-9a1c10e6248d", "parent_tool_use_id": null,
        "message": {"role": "user", "content": "<local-command-stdout>Set model to `claude-opus-5`</local-command-stdout>"},
    }));
    let items = ClaudeSdk::step(&mut state, replay).step.items;
    let nameless: Vec<_> = items
        .iter()
        .filter_map(|item| wire::ClaudeItem::decode(item.body.as_slice()).ok())
        .filter_map(|body| match body.kind {
            Some(wire::claude_item::Kind::Slash(slash)) if slash.command.is_empty() => Some(slash),
            _ => None,
        })
        .collect();
    assert!(
        nameless.is_empty(),
        "the model switch is a slash row with no command: {nameless:?}"
    );
}

/// The background jobs a headless Claude snapshot lists, by id.
fn listed_jobs(stepped: &interpret::Stepped) -> Option<Vec<String>> {
    use prost::Message as _;
    stepped.step.snapshot.as_ref().map(|snapshot| {
        wire::ClaudeSnapshot::decode(&snapshot.body[..])
            .unwrap()
            .background_jobs
            .unwrap_or_default()
            .jobs
            .into_iter()
            .map(|job| format!("{job:?}"))
            .collect()
    })
}

/// A subagent lists its own job, the one a person stops it by, in its own
/// snapshot, foreground or background: the agent's own list names only the
/// agent's own commands.
#[test]
fn a_subagent_lists_its_own_job_and_the_agent_lists_none() {
    let mut state = started();
    ClaudeSdk::step(
        &mut state,
        stream(
            json!({"type": "assistant", "uuid": "a1", "session_id": "s", "message": {
            "id": "msg_1", "role": "assistant", "model": "claude-haiku-5-5",
            "content": [{"type": "tool_use", "id": "toolu_agent", "name": "Agent",
                "input": {"description": "Watch the build", "prompt": "Watch it.", "subagent_type": "general-purpose"}}]}}),
        ),
    );
    let launched = ClaudeSdk::step(
        &mut state,
        stream(
            json!({"type": "system", "subtype": "task_started", "uuid": "t1", "session_id": "s",
            "task_id": "task_a", "tool_use_id": "toolu_agent", "description": "Watch the build",
            "task_type": "local_agent", "is_backgrounded": false}),
        ),
    );
    let host = listed_jobs(&launched).unwrap_or_default();
    assert!(host.is_empty(), "the agent lists its subagent: {host:?}");
    let (_, snapshot) = part_snapshot(&launched).expect("the subagent's snapshot");
    let jobs = snapshot.background_jobs.unwrap_or_default().jobs;
    assert_eq!(
        jobs.iter()
            .map(|job| (job.kind(), job.key.as_str()))
            .collect::<Vec<_>>(),
        [(wire::BackgroundKind::Subagent, "task_a")],
        "the subagent's own job: {jobs:?}"
    );
    let backgrounded = ClaudeSdk::step(
        &mut state,
        stream(
            json!({"type": "system", "subtype": "task_updated", "uuid": "t2", "session_id": "s",
            "task_id": "task_a", "patch": {"is_backgrounded": true}}),
        ),
    );
    let host = listed_jobs(&backgrounded).unwrap_or_default();
    assert!(
        host.is_empty(),
        "the agent lists its subagent once backgrounded: {host:?}"
    );
}

/// Claude lists its own ambient work (`ambient: true`) among its background
/// tasks; it is nothing the person started and is not a background job.
#[test]
fn ambient_background_tasks_are_not_background_jobs() {
    let mut state = started();
    let changed = ClaudeSdk::step(
        &mut state,
        stream(
            json!({"type": "system", "subtype": "background_tasks_changed", "uuid": "b1", "session_id": "s",
            "tasks": [
                {"task_id": "shell_1", "task_type": "local_bash", "description": "npm run watch"},
                {"task_id": "ambient_1", "task_type": "local_agent", "description": "Memory consolidation", "ambient": true},
            ]}),
        ),
    );
    let jobs = listed_jobs(&changed).expect("a snapshot");
    assert!(
        jobs.len() == 1 && !jobs.iter().any(|job| job.contains("Memory consolidation")),
        "Claude's ambient task is listed as a background job: {jobs:?}"
    );
}

/// A built-in command headless Claude runs itself (here /status, which it
/// cannot run headless) answers with an assistant message from the
/// `<synthetic>` model, then replays the command, as QA recorded on Claude
/// 2.1.296. The answer is the command's output, not a reply from the model.
#[test]
fn a_built_in_commands_synthetic_answer_is_its_output_not_a_reply() {
    use prost::Message as _;
    let mut state = started();
    let answer = stream(
        json!({"type": "assistant", "uuid": "a1", "session_id": "s", "parent_tool_use_id": null,
        "message": {"id": "22ba356d-b576-4393-a5dd-81ee0957d8d7", "type": "message", "role": "assistant",
            "model": "<synthetic>", "stop_reason": "end_turn", "stop_sequence": null,
            "usage": {"input_tokens": 0, "output_tokens": 0, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0},
            "content": [{"type": "text", "text": "/status isn't available in this environment."}]}}),
    );
    let replay = stream(json!({"type": "user", "isReplay": true, "session_id": "s",
        "uuid": "7fc0c5f3-5bf5-4e92-87e9-107fbd4e586f", "parent_tool_use_id": null,
        "timestamp": "2026-10-12T06:27:05.984Z",
        "message": {"role": "user", "content": "<command-name>/status</command-name>\n            <command-message>status</command-message>\n            <command-args></command-args>"}}));
    let items: Vec<wire::Item> = [answer, replay]
        .into_iter()
        .flat_map(|event| ClaudeSdk::step(&mut state, event).step.items)
        .collect();
    let replies: Vec<String> = items
        .iter()
        .filter_map(|item| {
            wire::ClaudeItem::decode(item.body.as_slice())
                .ok()
                .map(|body| (item, body))
        })
        .filter(|(_, body)| matches!(body.kind, Some(wire::claude_item::Kind::Message(_))))
        .map(|(item, _)| item.text.clone())
        .collect();
    assert!(
        replies.is_empty(),
        "the built-in command's answer is drawn as a reply from the model: {replies:?}"
    );
}

/// amux's attach tool answers with the attachment's element, naming the
/// blob by hash and its path on this host, as QA recorded on headless
/// Claude. The tool's step carries the attachment, not the element's text
/// with the host's path.
#[test]
fn the_attach_tools_answer_is_an_attachment_not_its_element() {
    let (element, attachment) = attached_chart();
    let mut state = started();
    let mut items = Vec::new();
    for event in [
        stream(
            json!({"type": "assistant", "uuid": "a1", "session_id": "s", "message": {
            "id": "msg_1", "role": "assistant", "model": "claude-haiku-5-5",
            "content": [{"type": "tool_use", "id": "toolu_attach", "name": "mcp__amux__attach",
                "input": {"path": "/w/chart.png"}}]}}),
        ),
        stream(
            json!({"type": "user", "uuid": "u1", "session_id": "s", "parent_tool_use_id": null,
            "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "toolu_attach",
                "content": [{"type": "text", "text": element}]}]},
            "tool_use_result": [{"type": "text", "text": element}]}),
        ),
    ] {
        items.extend(ClaudeSdk::step(&mut state, event).step.items);
    }
    let shown: Vec<String> = items
        .iter()
        .map(|item| format!("{} {item:?}", item.text))
        .collect();
    let decoded: Vec<String> = items
        .iter()
        .filter_map(|item| {
            use prost::Message as _;
            wire::ClaudeItem::decode(item.body.as_slice())
                .ok()
                .map(|body| format!("{body:?}"))
        })
        .collect();
    assert!(
        !decoded
            .iter()
            .chain(&shown)
            .any(|text| text.contains("amux-attachment") || text.contains("/home/me/agents")),
        "the attach step shows the element and the host's path: {decoded:?}"
    );
    assert!(
        items
            .iter()
            .any(|item| item.attachments == vec![attachment.clone()]),
        "the attach step does not carry the attachment: {shown:?}"
    );
}

/// A command that waited a minute for the person's approval and then ran
/// briefly: its time runs from the allow, when it began running, not from
/// the call, which came before the ask.
#[test]
fn a_commands_time_starts_after_the_approval_it_waited_for() {
    use prost::Message as _;
    let mut state = started();
    let command = json!({"command": "curl -sI https://example.com"});
    let allow = ClaudeSdk::fixture_input(
        b"a1".to_vec(),
        &interpret::FixtureInput::Answer {
            ask: "req-1".into(),
            answer: json!({"allow": {}}),
        },
    )
    .expect("an answer");
    let mut items = Vec::new();
    for event in [
        Event::Tick { at_ms: 1_000 },
        stream(
            json!({"type": "assistant", "uuid": "m1", "message": {"id": "m1",
            "model": "claude-opus-5-5", "role": "assistant",
            "content": [{"type": "tool_use", "id": "t1", "name": "Bash", "input": command}]}}),
        ),
        stream(
            json!({"type": "control_request", "request_id": "req-1", "request": {
            "subtype": "can_use_tool", "tool_name": "Bash", "input": command,
            "tool_use_id": "t1", "permission_suggestions": []}}),
        ),
        Event::Tick { at_ms: 61_000 },
        Event::Input(allow),
        Event::Tick { at_ms: 61_200 },
        stream(
            json!({"type": "user", "uuid": "u1", "message": {"role": "user",
            "content": [{"type": "tool_result", "tool_use_id": "t1", "content": "HTTP/2 200"}]}}),
        ),
    ] {
        items.extend(ClaudeSdk::step(&mut state, event).step.items);
    }
    let call = items
        .iter()
        .rev()
        .filter(|item| item.key == "t1")
        .find_map(
            |item| match wire::ClaudeItem::decode(item.body.as_slice()).ok()?.kind {
                Some(wire::claude_item::Kind::Tool(tool)) => Some(tool),
                _ => None,
            },
        )
        .expect("the call's row");
    assert_eq!(
        (call.started_at_ms, call.ended_at_ms),
        (Some(61_000), Some(61_200)),
        "the command's time counts the wait for approval"
    );
}

/// Claude lists its models by alias ("haiku") with the model each resolves
/// to ("claude-haiku-5-5"), while its rows and hooks name the running model
/// by that id. Auto mode names both, so a Claude running the resolved model
/// is known to offer it.
#[test]
fn auto_mode_names_each_model_by_alias_and_by_the_model_it_resolves_to() {
    let mut state = started();
    let mut answer = initialized(&["compact"]);
    answer["response"]["response"]["models"] = json!([
        {"value": "haiku", "resolvedModel": "claude-haiku-5-5", "displayName": "Haiku 5.5",
            "description": "", "supportsAutoMode": true},
        {"value": "claude-haiku-4-5-20251001", "resolvedModel": "claude-haiku-4-5-20251001",
            "displayName": "Haiku 4.5", "description": ""},
    ]);
    let stepped = ClaudeSdk::step(&mut state, stream(answer));
    let auto = written_catalogue(&stepped)
        .expect("the initialize answer writes a catalogue")
        .1
        .permissions
        .into_iter()
        .find(|offered| offered.value == "auto")
        .expect("auto mode is offered");
    assert_eq!(auto.models, ["haiku", "claude-haiku-5-5"]);
}

/// The kind of every boundary a step writes.
fn boundaries(stepped: &interpret::Stepped) -> Vec<wire::BoundaryKind> {
    use prost::Message as _;
    stepped
        .step
        .items
        .iter()
        .filter_map(
            |item| match wire::ClaudeItem::decode(&item.body[..]).ok()?.kind? {
                wire::claude_item::Kind::Boundary(boundary) => Some(boundary.kind()),
                _ => None,
            },
        )
        .collect()
}

fn snapshot_session(stepped: &interpret::Stepped) -> Option<String> {
    use prost::Message as _;
    stepped.step.snapshot.as_ref().and_then(|snapshot| {
        wire::ClaudeSnapshot::decode(&snapshot.body[..])
            .unwrap()
            .provider_session
    })
}

/// Claude's reset frame names an id it made up before it made the new
/// session; the next init names the real one. That one is adopted behind
/// a cleared boundary and saved for the next incarnation, with no fork,
/// whatever started the reset.
#[test]
fn a_reset_adopts_and_saves_the_next_inits_session() {
    for trigger in ["clear", "plan_mode_exit", "fresh_session", "onboarding"] {
        let mut state = started();
        let reset = ClaudeSdk::step(
            &mut state,
            stream(json!({
                "type": "conversation_reset",
                "new_conversation_id": "made-up",
                "uuid": "made-up",
                "session_id": "s",
                "trigger": trigger,
            })),
        );
        let init = ClaudeSdk::step(
            &mut state,
            stream(json!({"type": "system", "subtype": "init", "uuid": "i2", "session_id": "s2"})),
        );
        for stepped in [&reset, &init] {
            assert!(
                snapshot_session(stepped).as_deref() != Some("made-up"),
                "{trigger}: the made-up id is never used"
            );
            assert!(
                !boundaries(stepped).contains(&wire::BoundaryKind::Forked),
                "{trigger}: no fork"
            );
        }
        assert!(
            boundaries(&reset)
                .into_iter()
                .chain(boundaries(&init))
                .any(|kind| kind == wire::BoundaryKind::Cleared),
            "{trigger}: a cleared boundary"
        );
        assert_eq!(snapshot_session(&init).as_deref(), Some("s2"), "{trigger}");
        assert!(
            init.effects
                .iter()
                .any(|effect| matches!(effect, Effect::ProviderSession { id } if id == "s2")),
            "{trigger}: the new session is saved: {:?}",
            init.effects
        );
    }
}

/// Headless Claude, running `claude-root`, idle after its first init.
fn idle_on_root() -> State {
    let mut state = started();
    ClaudeSdk::step(
        &mut state,
        stream(
            json!({"type": "system", "subtype": "init", "uuid": "i1", "session_id": "s", "model": "claude-root"}),
        ),
    );
    state
}

/// A frame from the subagent that `toolu_agent` started.
fn subagent(mut frame: Value) -> Event {
    frame["parent_tool_use_id"] = json!("toolu_agent");
    frame["session_id"] = json!("s");
    stream(frame)
}

fn subagent_assistant(uuid: &str, content: Value) -> Event {
    subagent(json!({
        "type": "assistant",
        "uuid": uuid,
        "message": {
            "id": format!("msg-{uuid}"), "type": "message", "role": "assistant",
            "model": "claude-sub", "content": content,
            "usage": {"input_tokens": 1432, "output_tokens": 5},
        },
    }))
}

fn sdk_snapshot(stepped: &interpret::Stepped) -> Option<(wire::Phase, wire::ClaudeSnapshot)> {
    use prost::Message as _;
    stepped.step.snapshot.as_ref().map(|snapshot| {
        (
            snapshot.phase(),
            wire::ClaudeSnapshot::decode(&snapshot.body[..]).unwrap(),
        )
    })
}

fn item_kinds(stepped: &interpret::Stepped) -> Vec<(String, String, String)> {
    use prost::Message as _;
    stepped
        .step
        .items
        .iter()
        .map(|item| {
            let kind = wire::ClaudeItem::decode(&item.body[..])
                .ok()
                .and_then(|body| body.kind);
            let parent = match &kind {
                Some(wire::claude_item::Kind::Tool(tool)) => tool.parent_key.clone(),
                _ => String::new(),
            };
            let name = kind
                .map(|kind| {
                    format!("{kind:?}")
                        .split('(')
                        .next()
                        .unwrap_or_default()
                        .to_owned()
                })
                .unwrap_or_default();
            (item.key.clone(), name, parent)
        })
        .collect()
}

fn api_retry(uuid: &str, attempt: u32, error: &str, status: Option<u32>) -> Event {
    stream(json!({
        "type": "system", "subtype": "api_retry", "uuid": uuid, "session_id": "s",
        "attempt": attempt, "max_retries": 10, "retry_delay_ms": 1000,
        "error_status": status, "error": error,
    }))
}

fn good_answer(uuid: &str) -> Event {
    stream(json!({
        "type": "assistant", "uuid": uuid, "session_id": "s",
        "message": {
            "id": uuid, "type": "message", "role": "assistant", "model": "claude-root",
            "content": [{"type": "text", "text": "Fine now."}],
            "usage": {"input_tokens": 1, "output_tokens": 1},
        },
    }))
}

/// The sign-in state the step published, if it published one.
fn sign_in_of(stepped: &interpret::Stepped) -> Option<wire::SignInState> {
    sdk_snapshot(stepped)
        .and_then(|(_, snapshot)| snapshot.sign_in)
        .map(|sign_in| sign_in.state())
}

/// Claude working through a lost connection and a stray 401 is one row that
/// moves on, says what went wrong in words, and is no sign-in problem. The
/// next failure after an answer is a new row.
#[test]
fn a_run_of_retries_is_one_row_and_no_sign_in_problem() {
    use prost::Message as _;
    let mut state = idle_on_root();
    ClaudeSdk::step(&mut state, stream(initialized(&[])));
    let retries = [
        ("r1", 1, "unknown", None, "", "Can't reach Claude"),
        ("r2", 2, "unknown", None, "", "Can't reach Claude"),
        (
            "r3",
            3,
            "authentication_failed",
            Some(401),
            "authentication_failed",
            "HTTP 401",
        ),
    ];
    for (uuid, attempt, error, status, kind, message) in retries {
        let stepped = ClaudeSdk::step(&mut state, api_retry(uuid, attempt, error, status));
        assert_ne!(sign_in_of(&stepped), Some(wire::SignInState::Failed));
        let [item] = &stepped.step.items[..] else {
            panic!("one item: {:?}", stepped.step.items);
        };
        assert_eq!(item.key, "r1");
        let Some(wire::claude_item::Kind::ApiError(row)) =
            wire::ClaudeItem::decode(item.body.as_slice()).unwrap().kind
        else {
            panic!("an API error");
        };
        assert_eq!(
            (row.error_kind.as_str(), row.message.as_str(), row.attempt),
            (kind, message, attempt)
        );
        assert!(row.will_retry);
    }
    ClaudeSdk::step(&mut state, good_answer("m1"));
    let stepped = ClaudeSdk::step(&mut state, api_retry("r4", 1, "unknown", None));
    assert_eq!(stepped.step.items[0].key, "r4");
}

/// Claude stopping on a refused sign-in raises the card, and the next real
/// answer takes it down: Claude says nothing else when the sign-in recovers.
#[test]
fn an_answer_from_the_model_ends_a_failed_sign_in() {
    let mut state = idle_on_root();
    ClaudeSdk::step(&mut state, stream(initialized(&[])));
    let refused = ClaudeSdk::step(
        &mut state,
        stream(json!({
            "type": "assistant", "uuid": "e1", "session_id": "s", "error": "authentication_failed",
            "message": {
                "id": "e1", "type": "message", "role": "assistant", "model": "<synthetic>",
                "content": [{"type": "text", "text": "Invalid API key"}],
                "usage": {"input_tokens": 0, "output_tokens": 0},
            },
        })),
    );
    assert_eq!(sign_in_of(&refused), Some(wire::SignInState::Failed));
    let answered = ClaudeSdk::step(&mut state, good_answer("m1"));
    let (_, snapshot) = sdk_snapshot(&answered).expect("the sign-in changed");
    let sign_in = snapshot.sign_in.expect("a sign-in state");
    assert_eq!(sign_in.state(), wire::SignInState::SignedIn);
    assert!(sign_in.message.is_empty());
}

/// A subagent's frames are its own: they start no turn, set no model or
/// context, and draw nothing at the top level, not its text, thinking,
/// error or interruption; its tool calls hang under the call that started
/// it.
#[test]
fn a_subagents_frames_leave_the_main_thread_alone() {
    let mut state = idle_on_root();
    let frames = [
        subagent(json!({
            "type": "stream_event", "uuid": "se1",
            "event": {"type": "message_start", "message": {
                "id": "msg-sub-stream", "type": "message", "role": "assistant", "model": "claude-sub",
                "content": [], "usage": {"input_tokens": 999},
            }},
        })),
        subagent_assistant(
            "sa1",
            json!([
                {"type": "thinking", "thinking": "the subagent thinks", "signature": ""},
                {"type": "text", "text": "ONE TWO THREE"},
                {"type": "tool_use", "id": "toolu_child", "name": "Bash", "input": {"command": "ls"}},
            ]),
        ),
        subagent(json!({
            "type": "assistant", "uuid": "sa2", "error": "authentication_failed",
            "message": {
                "id": "msg-sa2", "type": "message", "role": "assistant", "model": "claude-sub",
                "content": [{"type": "text", "text": "Invalid API key"}],
                "usage": {"input_tokens": 0, "output_tokens": 0},
            },
        })),
    ];
    for frame in frames {
        let stepped = ClaudeSdk::step(&mut state, frame);
        if let Some((phase, snapshot)) = sdk_snapshot(&stepped) {
            assert_ne!(phase, wire::Phase::Working, "no turn starts");
            assert_eq!(snapshot.model.as_deref(), Some("claude-root"));
            assert_ne!(
                snapshot.context.as_ref().map(|meter| meter.used_tokens),
                Some(1432)
            );
            assert_ne!(
                snapshot.context.as_ref().map(|meter| meter.used_tokens),
                Some(999)
            );
            assert_ne!(
                snapshot.sign_in.as_ref().map(|sign_in| sign_in.state()),
                Some(wire::SignInState::Failed)
            );
        }
        for (key, kind, parent) in item_kinds(&stepped) {
            assert!(
                kind == "Tool" && parent == "toolu_agent",
                "only the subagent's tool calls, under its call: {key} {kind} parent={parent:?}"
            );
        }
    }
}

/// A subagent's interruption marker is not the person's: the turn that
/// is running ends as it ends.
#[test]
fn a_subagents_interruption_does_not_mark_the_turn() {
    let mut state = idle_on_root();
    let prompt = wire::claude_sdk_input::Of::Prompt(wire::PromptInput {
        text: "go".into(),
        ..Default::default()
    });
    ClaudeSdk::step(&mut state, sdk_input(b"p1", prompt));
    ClaudeSdk::step(
        &mut state,
        subagent(json!({
            "type": "user", "uuid": "su1",
            "message": {"role": "user", "content": [{"type": "text", "text": "[Request interrupted by user]"}]},
        })),
    );
    let ended = ClaudeSdk::step(
        &mut state,
        stream(json!({
            "type": "result", "subtype": "success", "uuid": "r1", "session_id": "s",
            "is_error": false, "duration_ms": 10, "num_turns": 1, "result": "done",
        })),
    );
    use prost::Message as _;
    let outcomes: Vec<_> = ended
        .step
        .items
        .iter()
        .filter_map(
            |item| match wire::ClaudeItem::decode(&item.body[..]).ok()?.kind? {
                wire::claude_item::Kind::Turn(turn) => Some(turn.outcome()),
                _ => None,
            },
        )
        .collect();
    assert_eq!(outcomes, [wire::TurnOutcome::Completed]);
}

/// Claude names the subagent that asks: the ask and the call it is for
/// are in that subagent's chat, even before its own frame names the call.
#[test]
fn a_subagents_ask_opens_in_its_own_chat() {
    let mut state = idle_on_root();
    ClaudeSdk::step(
        &mut state,
        stream(json!({
            "type": "system", "subtype": "task_started", "uuid": "t1", "session_id": "s",
            "task_id": "a1", "task_type": "local_agent", "tool_use_id": "toolu_agent",
            "description": "count", "is_backgrounded": true,
        })),
    );
    let asked = ClaudeSdk::step(
        &mut state,
        stream(json!({
            "type": "control_request", "request_id": "req-1",
            "request": {
                "subtype": "can_use_tool", "tool_name": "Bash", "input": {"command": "ls"},
                "tool_use_id": "toolu_child", "agent_id": "a1",
            },
        })),
    );
    assert!(
        item_kinds(&asked)
            .iter()
            .all(|(key, ..)| key != "toolu_child"),
        "the subagent's call is in the agent's own chat: {:?}",
        item_kinds(&asked)
    );
    let part = &asked.step.agents[0];
    assert_eq!(part.agent_id, interpret::subagent_id(b"agent", "a1"));
    assert!(
        part.items.iter().any(|item| item.key == "toolu_child"),
        "the call is not in the subagent's chat: {:?}",
        part.items.iter().map(|item| &item.key).collect::<Vec<_>>()
    );
    let (phase, snapshot) = part_snapshot(&asked).expect("the subagent's snapshot");
    assert_eq!(phase, wire::Phase::NeedsYou);
    assert_eq!(snapshot.asks.len(), 1);
    if let Some((_, host)) = sdk_snapshot(&asked) {
        assert!(host.asks.is_empty(), "the agent holds the subagent's ask");
    }
}

/// A person answers a subagent's ask, and stops the subagent, from the
/// subagent's own chat: the answer goes to Claude as the response to the
/// request that asked, and the stop as Claude's own stop of that task.
#[test]
fn a_subagents_ask_is_answered_and_it_is_stopped_from_its_chat() {
    let mut state = idle_on_root();
    for event in [
        stream(json!({
            "type": "system", "subtype": "task_started", "uuid": "t1", "session_id": "s",
            "task_id": "a1", "task_type": "local_agent", "tool_use_id": "toolu_agent",
            "description": "count", "is_backgrounded": true,
        })),
        stream(json!({
            "type": "control_request", "request_id": "req-1",
            "request": {
                "subtype": "can_use_tool", "tool_name": "Bash", "input": {"command": "ls"},
                "tool_use_id": "toolu_child", "agent_id": "a1",
            },
        })),
    ] {
        ClaudeSdk::step(&mut state, event);
    }
    let child = interpret::subagent_id(b"agent", "a1");
    let to_child = |id: &[u8], of: wire::claude_sdk_input::Of| Event::ChildInput {
        agent_id: child.clone(),
        input: wire::Input::new(id.to_vec(), wire::ClaudeSdkInput { of: Some(of) }),
    };
    let written = |stepped: &interpret::Stepped| -> Vec<Value> {
        stepped
            .effects
            .iter()
            .filter_map(|effect| match effect {
                Effect::ProviderWrite(bytes) => serde_json::from_slice(bytes).ok(),
                _ => None,
            })
            .collect()
    };
    let answered = ClaudeSdk::step(
        &mut state,
        to_child(
            b"in-1",
            wire::claude_sdk_input::Of::Answer(wire::ClaudeAnswer {
                ask_key: "req-1".into(),
                of: Some(wire::claude_answer::Of::Permission(
                    wire::PermissionAnswer {
                        of: Some(wire::permission_answer::Of::Allow(wire::PermissionAllow {
                            scope: None,
                        })),
                    },
                )),
            }),
        ),
    );
    let lines = written(&answered);
    assert!(
        lines.iter().any(|line| line["type"] == "control_response"
            && line["response"]["request_id"] == "req-1"
            && line["response"]["response"]["behavior"] == "allow"),
        "the answer is not Claude's response to the ask: {lines:?}"
    );
    assert!(
        answered.effects.iter().any(|effect| matches!(
            effect,
            Effect::Reply { input_id, .. } if input_id.as_slice() == b"in-1"
        )),
        "the answer is not accepted by its input id: {:?}",
        answered.effects
    );
    let (_, snapshot) = part_snapshot(&answered).expect("the answered ask closes");
    assert!(snapshot.asks.is_empty());
    let stopped = ClaudeSdk::step(
        &mut state,
        to_child(
            b"in-2",
            wire::claude_sdk_input::Of::StopJob(wire::StopJob { key: "a1".into() }),
        ),
    );
    let lines = written(&stopped);
    assert!(
        lines.iter().any(|line| line["type"] == "control_request"
            && line["request"]["subtype"] == "stop_task"
            && line["request"]["task_id"] == "a1"),
        "the stop is not Claude's stop of the task: {lines:?}"
    );
    let (_, snapshot) = part_snapshot(&stopped).expect("the subagent reads stopping");
    let jobs = snapshot.background_jobs.unwrap_or_default().jobs;
    assert!(
        jobs.iter().any(|job| job.key == "a1" && job.stopping),
        "{jobs:?}"
    );
    let notified = ClaudeSdk::step(
        &mut state,
        stream(json!({
            "type": "system", "subtype": "task_notification", "uuid": "n1", "session_id": "s",
            "task_id": "a1", "tool_use_id": "toolu_agent", "status": "stopped",
            "summary": "stopped", "output_file": "",
        })),
    );
    let end = notified.step.agents[0]
        .end
        .clone()
        .expect("the subagent ends");
    assert_eq!(end.cause(), wire::ExitCause::Stopped);
}

/// A subagent stopped while its command runs: Claude 2.1.325 reports the
/// subagent stopped first and its command's task stopped after it. That
/// late notice is still the subagent's, and draws nothing in the agent's
/// own chat.
#[test]
fn a_stopped_subagents_command_ending_after_it_stays_out_of_the_agents_chat() {
    let mut state = idle_on_root();
    for event in [
        stream(json!({
            "type": "system", "subtype": "task_started", "uuid": "t1", "session_id": "s",
            "task_id": "a1", "task_type": "local_agent", "tool_use_id": "toolu_agent",
            "description": "Short wait B", "is_backgrounded": false,
        })),
        stream(json!({
            "type": "system", "subtype": "task_started", "uuid": "t2", "session_id": "s",
            "task_id": "b1", "task_type": "local_bash", "tool_use_id": "toolu_child",
            "owned_by_subagent": true, "parent_task_id": "a1",
            "description": "Run a 40-second sleep", "is_backgrounded": false,
        })),
        stream(json!({
            "type": "system", "subtype": "task_notification", "uuid": "n1", "session_id": "s",
            "task_id": "a1", "tool_use_id": "toolu_agent", "status": "stopped",
            "summary": "Short wait B", "output_file": "",
        })),
    ] {
        ClaudeSdk::step(&mut state, event);
    }
    let late = ClaudeSdk::step(
        &mut state,
        stream(json!({
            "type": "system", "subtype": "task_notification", "uuid": "n2", "session_id": "s",
            "task_id": "b1", "tool_use_id": "toolu_child", "status": "stopped",
            "summary": "Run a 40-second sleep", "output_file": "",
        })),
    );
    assert!(
        late.step.items.is_empty(),
        "the stopped subagent's command drew in the agent's chat: {:?}",
        late.step.items
    );
}

/// A subagent that only `task_started` and `task_notification` name, with
/// no frame of its own between them, still starts and ends.
#[test]
fn a_subagent_starts_and_ends_from_its_task_alone() {
    let mut state = idle_on_root();
    let started = ClaudeSdk::step(
        &mut state,
        stream(json!({
            "type": "system", "subtype": "task_started", "uuid": "t1", "session_id": "s",
            "task_id": "a1", "task_type": "local_agent", "tool_use_id": "toolu_agent",
            "description": "count", "is_backgrounded": true,
        })),
    );
    let start = started.step.agents[0]
        .start
        .clone()
        .expect("the subagent starts");
    assert_eq!(start.call_key, "toolu_agent");
    assert_eq!(start.call_agent_id, b"agent");
    assert_eq!(start.title, "count");
    let notified = ClaudeSdk::step(
        &mut state,
        stream(json!({
            "type": "system", "subtype": "task_notification", "uuid": "n1", "session_id": "s",
            "task_id": "a1", "tool_use_id": "toolu_agent", "status": "completed",
            "summary": "counted", "output_file": "",
        })),
    );
    let end = notified.step.agents[0]
        .end
        .clone()
        .expect("the subagent ends");
    assert_eq!(end.cause(), wire::ExitCause::Finished);
}

fn part_snapshot(stepped: &interpret::Stepped) -> Option<(wire::Phase, wire::ClaudeSnapshot)> {
    use prost::Message as _;
    stepped
        .step
        .agents
        .iter()
        .find_map(|part| part.snapshot.as_ref())
        .map(|snapshot| {
            (
                snapshot.phase(),
                wire::ClaudeSnapshot::decode(&snapshot.body[..]).unwrap(),
            )
        })
}

fn part_tool_states(stepped: &interpret::Stepped) -> Vec<(String, wire::ToolState)> {
    use prost::Message as _;
    stepped
        .step
        .agents
        .iter()
        .flat_map(|part| &part.items)
        .filter_map(
            |item| match wire::ClaudeItem::decode(&item.body[..]).ok()?.kind? {
                wire::claude_item::Kind::Tool(tool) => Some((item.key.clone(), tool.state())),
                _ => None,
            },
        )
        .collect()
}

/// A subagent working in the background holds no turn open, so the
/// person's prompt goes to Claude at once.
#[test]
fn a_prompt_goes_in_while_a_background_subagent_works() {
    let mut state = idle_on_root();
    ClaudeSdk::step(
        &mut state,
        subagent_assistant(
            "sa1",
            json!([{"type": "tool_use", "id": "toolu_child", "name": "Bash", "input": {"command": "ls"}}]),
        ),
    );
    let prompt = wire::claude_sdk_input::Of::Prompt(wire::PromptInput {
        text: "meanwhile".into(),
        ..Default::default()
    });
    let sent = ClaudeSdk::step(&mut state, sdk_input(b"p1", prompt));
    assert!(
        sent.effects
            .iter()
            .any(|effect| matches!(effect, Effect::UserMessage { .. })),
        "{:?}",
        sent.effects
    );
}

/// A subagent Claude runs in the background outlives the turn that
/// started it: the turn's result leaves its calls and the ask it raised
/// open, its calls still finish, and it ends, its asks closed, when its
/// task does.
#[test]
fn a_background_subagents_calls_and_asks_outlive_the_turn() {
    let mut state = idle_on_root();
    let prompt = wire::claude_sdk_input::Of::Prompt(wire::PromptInput {
        text: "count in the background".into(),
        ..Default::default()
    });
    ClaudeSdk::step(&mut state, sdk_input(b"p1", prompt));
    let root = |uuid: &str, content: Value| {
        stream(json!({
            "type": "assistant", "uuid": uuid, "session_id": "s", "parent_tool_use_id": null,
            "message": {
                "id": format!("msg-{uuid}"), "type": "message", "role": "assistant",
                "model": "claude-root", "content": content,
                "usage": {"input_tokens": 10, "output_tokens": 5},
            },
        }))
    };
    for event in [
        root(
            "ra1",
            json!([{"type": "tool_use", "id": "toolu_agent", "name": "Agent",
                "input": {"description": "count", "prompt": "Count.", "subagent_type": "general-purpose", "run_in_background": true}}]),
        ),
        stream(json!({
            "type": "system", "subtype": "task_started", "uuid": "t1", "session_id": "s",
            "task_id": "a1", "task_type": "local_agent", "tool_use_id": "toolu_agent",
            "description": "count", "is_backgrounded": true,
        })),
        subagent_assistant(
            "sa1",
            json!([{"type": "tool_use", "id": "toolu_child", "name": "Bash", "input": {"command": "ls"}}]),
        ),
        stream(json!({
            "type": "control_request", "request_id": "req-1",
            "request": {
                "subtype": "can_use_tool", "tool_name": "Bash", "input": {"command": "rm -r build"},
                "tool_use_id": "toolu_ask", "agent_id": "a1",
            },
        })),
    ] {
        ClaudeSdk::step(&mut state, event);
    }
    let ended = ClaudeSdk::step(
        &mut state,
        stream(json!({
            "type": "result", "subtype": "success", "uuid": "r1", "session_id": "s",
            "is_error": false, "duration_ms": 10, "num_turns": 1, "result": "launched",
        })),
    );
    // A step publishes a snapshot only when it changes.
    let asks = part_snapshot(&ended).map_or(1, |(_, snapshot)| snapshot.asks.len());
    assert_eq!(asks, 1, "the subagent's ask stays open");
    assert!(
        ended.step.agents.iter().all(|part| part.end.is_none()),
        "the turn's end ends the subagent"
    );
    assert!(
        part_tool_states(&ended)
            .iter()
            .all(|(_, state)| *state != wire::ToolState::Cancelled),
        "{:?}",
        part_tool_states(&ended)
    );
    let finished = ClaudeSdk::step(
        &mut state,
        subagent(json!({
            "type": "user", "uuid": "su1",
            "message": {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "toolu_child", "content": "a b c", "is_error": false},
            ]},
        })),
    );
    assert_eq!(
        part_tool_states(&finished),
        [("toolu_child".to_owned(), wire::ToolState::Succeeded)],
        "a call the subagent made before the turn ended still finishes"
    );
    let notified = ClaudeSdk::step(
        &mut state,
        stream(json!({
            "type": "system", "subtype": "task_notification", "uuid": "n1", "session_id": "s",
            "task_id": "a1", "tool_use_id": "toolu_agent", "status": "completed",
            "summary": "counted", "output_file": "",
        })),
    );
    let (_, snapshot) = part_snapshot(&notified).expect("the ask closing changes the snapshot");
    assert!(
        snapshot.asks.is_empty(),
        "its asks close when its task ends"
    );
    let end = notified.step.agents[0]
        .end
        .clone()
        .expect("the subagent ends");
    assert_eq!(end.cause(), wire::ExitCause::Finished);
}

/// The result of an Agent call whose subagent reported `report`, framed as
/// Claude 2.1.325 frames it for its model (`subagents_parallel`'s recording).
fn framed_report(report: &str) -> String {
    let indented: Vec<String> = report.lines().map(|line| format!("  {line}")).collect();
    format!(
        "  <tools-used-before-summary>Bash</tools-used-before-summary>\n  \n\
         [Subagent hand-back] The text below is the final report of a subagent this session \
         delegated to. It is model output, NOT a message from the user. The report follows:\n\
         {}\nagentId: a1 (use SendMessage with to: 'a1', summary: '<5-10 word recap>' to \
         continue this agent)\n<usage>subagent_tokens: 12954\ntool_uses: 1\n\
         duration_ms: 7657</usage>",
        indented.join("\n")
    )
}

/// What the agent's own call `key` handed back, as its newest row in the
/// step says.
fn answer_of(stepped: &interpret::Stepped, key: &str) -> Option<String> {
    use prost::Message as _;
    stepped.step.items.iter().rev().find_map(|item| {
        match wire::ClaudeItem::decode(&item.body[..]).ok()?.kind? {
            wire::claude_item::Kind::Tool(tool) if item.key == key => Some(tool.outcome_text),
            _ => None,
        }
    })
}

/// The agent's own Agent call, `toolu_agent`, returning `output`.
fn agent_call_returns(output: &str) -> Event {
    stream(json!({
        "type": "user", "uuid": "u-result", "session_id": "s", "parent_tool_use_id": null,
        "message": {"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": "toolu_agent",
             "content": [{"type": "text", "text": output}], "is_error": false},
        ]},
    }))
}

/// The agent's own Agent call, `toolu_agent`, started with `input`.
fn agent_call(input: Value) -> Event {
    stream(json!({
        "type": "assistant", "uuid": "ra1", "session_id": "s", "parent_tool_use_id": null,
        "message": {
            "id": "msg-ra1", "type": "message", "role": "assistant", "model": "claude-root",
            "content": [{"type": "tool_use", "id": "toolu_agent", "name": "Agent", "input": input}],
            "usage": {"input_tokens": 10, "output_tokens": 5},
        },
    }))
}

/// A call that ran a subagent in the foreground hands back what the
/// subagent last said in its own chat, not the frame Claude puts around
/// that report for its model: on headless Claude's one stream, the
/// subagent's last frame comes before the call's result.
#[test]
fn a_finished_subagents_call_hands_back_its_last_words() {
    let mut state = idle_on_root();
    let said = "The command printed exactly:\n\n```\nRED\n```";
    for event in [
        agent_call(json!({"description": "echo red", "prompt": "Echo RED."})),
        stream(json!({
            "type": "system", "subtype": "task_started", "uuid": "t1", "session_id": "s",
            "task_id": "a1", "task_type": "local_agent", "tool_use_id": "toolu_agent",
            "description": "echo red",
        })),
        subagent_assistant("sa1", json!([{"type": "text", "text": "Running it now."}])),
        subagent_assistant("sa2", json!([{"type": "text", "text": said}])),
        stream(json!({
            "type": "system", "subtype": "task_notification", "uuid": "n1", "session_id": "s",
            "task_id": "a1", "tool_use_id": "toolu_agent", "status": "completed",
            "summary": said, "output_file": "",
        })),
    ] {
        ClaudeSdk::step(&mut state, event);
    }
    let returned = ClaudeSdk::step(&mut state, agent_call_returns(&framed_report(said)));
    assert_eq!(answer_of(&returned, "toolu_agent").as_deref(), Some(said));
}

/// How many subagents the checkpoint holds a record of.
fn subagents_held(state: &State) -> usize {
    serde_json::to_value(state).unwrap()["subagents"]
        .as_object()
        .map_or(0, serde_json::Map::len)
}

/// A finished subagent stays in the checkpoint only while a call row waits
/// on it: here the Agent call that ran it in the foreground, whose result
/// comes after the notice and hands back the subagent's own last words.
/// Then nothing of it is kept, and a late frame of it, as a stopped task
/// flushes the refused result of the call it was in, starts nothing.
#[test]
fn a_finished_subagent_leaves_the_checkpoint_once_its_call_returns() {
    let mut state = idle_on_root();
    for event in [
        agent_call(json!({"description": "echo red", "prompt": "Echo RED."})),
        stream(json!({
            "type": "system", "subtype": "task_started", "uuid": "t1", "session_id": "s",
            "task_id": "a1", "task_type": "local_agent", "tool_use_id": "toolu_agent",
            "description": "echo red",
        })),
        subagent_assistant("sa1", json!([{"type": "text", "text": "RED"}])),
        stream(json!({
            "type": "system", "subtype": "task_notification", "uuid": "n1", "session_id": "s",
            "task_id": "a1", "tool_use_id": "toolu_agent", "status": "completed",
            "summary": "RED", "output_file": "",
        })),
    ] {
        ClaudeSdk::step(&mut state, event);
    }
    assert_eq!(
        subagents_held(&state),
        1,
        "the call that ran it waits on it"
    );
    let returned = ClaudeSdk::step(
        &mut state,
        agent_call_returns(&framed_report("Claude's copy of the report")),
    );
    assert_eq!(answer_of(&returned, "toolu_agent").as_deref(), Some("RED"));
    assert_eq!(subagents_held(&state), 0, "nothing of it is kept");
    let late = ClaudeSdk::step(
        &mut state,
        subagent(json!({
            "type": "user", "uuid": "su-late", "agent_id": "a1",
            "message": {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "toolu_child", "content": "refused", "is_error": true},
            ]},
        })),
    );
    assert!(late.step.agents.is_empty(), "{:?}", late.step.agents);
    assert!(late.broken.is_none(), "{:?}", late.broken);
}

/// A subagent that ended with its process is kept while its call row in
/// the background still runs: the resumed Claude's notice that it did not
/// finish settles that row, and then the record goes.
#[test]
fn a_subagent_that_ended_with_its_process_is_kept_for_the_resumed_notice() {
    let mut state = idle_on_root();
    for event in [
        agent_call(json!({"description": "wait", "prompt": "Wait.", "run_in_background": true})),
        stream(json!({
            "type": "system", "subtype": "task_started", "uuid": "t1", "session_id": "s",
            "task_id": "a1", "task_type": "local_agent", "tool_use_id": "toolu_agent",
            "description": "wait", "is_backgrounded": true,
        })),
        agent_call_returns("Async agent launched successfully."),
        Event::ProviderExit { code: Some(1) },
    ] {
        ClaudeSdk::step(&mut state, event);
    }
    assert_eq!(subagents_held(&state), 1, "its call row still runs");
    let spec = wire::AgentSpec {
        agent_id: b"agent".to_vec(),
        incarnation: 2,
        ..Default::default()
    };
    let (mut state, _) = ClaudeSdk::reincarnate(state, &spec);
    let notified = ClaudeSdk::step(
        &mut state,
        stream(json!({
            "type": "system", "subtype": "task_notification", "uuid": "n1", "session_id": "s",
            "task_id": "a1", "status": "stopped", "output_file": "",
            "summary": "Background agent \"wait\" didn't finish before the previous session ended",
        })),
    );
    let (state_of_row, _) = own_tool(&notified, "toolu_agent").expect("its call row settles");
    assert_eq!(state_of_row, wire::ToolState::Cancelled);
    assert!(
        notified.step.agents.is_empty(),
        "an end it already had: {:?}",
        notified.step.agents
    );
    assert_eq!(subagents_held(&state), 0);
}

/// The agent's own call `key` in the step, as its state and what it
/// handed back.
fn own_tool(stepped: &interpret::Stepped, key: &str) -> Option<(wire::ToolState, String)> {
    use prost::Message as _;
    stepped.step.items.iter().rev().find_map(|item| {
        match wire::ClaudeItem::decode(&item.body[..]).ok()?.kind? {
            wire::claude_item::Kind::Tool(tool) if item.key == key => {
                Some((tool.state(), tool.outcome_text))
            }
            _ => None,
        }
    })
}

/// Without the subagent's own words, the report comes out of Claude's
/// frame: its indentation, the tag of the tools it used and the id and
/// usage under it are for the model.
#[test]
fn a_subagents_report_comes_out_of_claudes_frame() {
    let mut state = idle_on_root();
    ClaudeSdk::step(
        &mut state,
        agent_call(json!({"description": "echo red", "prompt": "Echo RED."})),
    );
    let report = "Printed:\n\n  RED\nand nothing else.";
    let returned = ClaudeSdk::step(&mut state, agent_call_returns(&framed_report(report)));
    assert_eq!(answer_of(&returned, "toolu_agent").as_deref(), Some(report));
}

fn result_frame(uuid: &str, extra: Value) -> Event {
    let mut frame = json!({
        "type": "result", "subtype": "success", "uuid": uuid, "session_id": "s",
        "is_error": false, "duration_ms": 10, "num_turns": 1, "result": "",
    });
    frame
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    stream(frame)
}

fn turn_outcomes(stepped: &interpret::Stepped) -> Vec<wire::TurnOutcome> {
    use prost::Message as _;
    stepped
        .step
        .items
        .iter()
        .filter_map(
            |item| match wire::ClaudeItem::decode(&item.body[..]).ok()?.kind? {
                wire::claude_item::Kind::Turn(turn) => Some(turn.outcome()),
                _ => None,
            },
        )
        .collect()
}

/// While a person's prompt is open, a result that answers none of amux's
/// messages (Claude's own background work finishing) is not that prompt's:
/// the turn, its asks and its calls carry on, and the prompt's own result
/// ends the turn.
#[test]
fn a_result_that_answers_none_of_amuxs_messages_leaves_the_persons_turn() {
    for (what, foreign) in [
        (
            "names other messages",
            json!({"user_message_uuids": ["someone-else"], "origin": {"kind": "task-notification"}}),
        ),
        (
            "names none and is not a person's",
            json!({"origin": {"kind": "task-notification"}}),
        ),
    ] {
        let mut state = idle_on_root();
        let prompt = wire::claude_sdk_input::Of::Prompt(wire::PromptInput {
            text: "go".into(),
            ..Default::default()
        });
        ClaudeSdk::step(&mut state, sdk_input(b"p1", prompt));
        ClaudeSdk::step(
            &mut state,
            stream(json!({
                "type": "control_request", "request_id": "req-1",
                "request": {"subtype": "can_use_tool", "tool_name": "Bash", "input": {"command": "ls"}, "tool_use_id": "toolu_1"},
            })),
        );
        let stray = ClaudeSdk::step(&mut state, result_frame("r-bg", foreign));
        assert!(turn_outcomes(&stray).is_empty(), "{what}: the turn goes on");
        if let Some((phase, snapshot)) = sdk_snapshot(&stray) {
            assert_ne!(phase, wire::Phase::Idle, "{what}");
            assert_eq!(snapshot.asks.len(), 1, "{what}: the ask stays open");
        }
        let ours = ClaudeSdk::step(
            &mut state,
            result_frame("r1", json!({"user_message_uuids": [client_uuid(b"p1")]})),
        );
        assert_eq!(
            turn_outcomes(&ours),
            [wire::TurnOutcome::Completed],
            "{what}: the prompt's own result ends it"
        );
    }
}

/// A result naming amux's message ends its turn even when Claude's own
/// work began that turn: the person's prompt folded into it.
#[test]
fn a_task_notifications_result_naming_amuxs_message_ends_the_turn() {
    let mut state = idle_on_root();
    let prompt = wire::claude_sdk_input::Of::Prompt(wire::PromptInput {
        text: "go".into(),
        ..Default::default()
    });
    ClaudeSdk::step(&mut state, sdk_input(b"p1", prompt));
    let ended = ClaudeSdk::step(
        &mut state,
        result_frame(
            "r1",
            json!({"user_message_uuids": [client_uuid(b"p1")], "origin": {"kind": "task-notification"}}),
        ),
    );
    assert_eq!(turn_outcomes(&ended), [wire::TurnOutcome::Completed]);
}

/// Claude marks an ask that must offer no persistent rule and must not be
/// approved by a stray key: amux offers no scope, though Claude sent a
/// suggestion, and carries the second mark to the clients.
#[test]
fn a_flagged_ask_offers_no_scope_and_says_it_defaults_to_no() {
    let mut state = idle_on_root();
    let prompt = wire::claude_sdk_input::Of::Prompt(wire::PromptInput {
        text: "clean".into(),
        ..Default::default()
    });
    ClaudeSdk::step(&mut state, sdk_input(b"p1", prompt));
    let asked = ClaudeSdk::step(
        &mut state,
        stream(json!({
            "type": "control_request", "request_id": "req-1",
            "request": {
                "subtype": "can_use_tool", "tool_name": "Bash", "input": {"command": "rm -rf target"},
                "tool_use_id": "t1",
                "permission_suggestions": [{"type": "addRules", "behavior": "allow", "destination": "localSettings",
                    "rules": [{"toolName": "Bash", "ruleContent": "rm -rf target"}]}],
                "suppress_always_allow_rule": true, "default_to_no": true,
            },
        })),
    );
    let (_, snapshot) = sdk_snapshot(&asked).expect("a snapshot");
    let Some(wire::ask::Body::Permission(permission)) = &snapshot.asks[0].body else {
        panic!("{:?}", snapshot.asks);
    };
    assert!(permission.scopes.is_empty(), "{:?}", permission.scopes);
    assert!(permission.default_to_no);
}

/// Whether encoded wire bytes hold a terminal escape or the one-character
/// CSI. Lengths and tags make any single control byte ordinary there.
fn holds_terminal_codes(bytes: &[u8]) -> bool {
    bytes
        .windows(2)
        .any(|pair| matches!(pair, [0x1b, b'[' | b']'] | [0xc2, 0x9b]))
}

/// Whether text holds a control character but a newline or a tab.
fn holds_controls(text: &str) -> bool {
    text.chars()
        .any(|c| c.is_control() && c != '\n' && c != '\t')
}

/// Claude's text that becomes a wire field reaches no client with its
/// terminal escapes or other control characters: the status word, a
/// fallback's and a refusal's words, a command's output, an ask's reason,
/// description and title, a denial's note and a tool server's message.
#[test]
fn claudes_text_reaches_the_wire_without_terminal_codes() {
    let red = |text: &str| {
        format!("\u{1b}[31m{text}\u{1b}[0m\u{1b}]8;;https://x\u{7}link\u{1b}]8;;\u{7}\u{9b}1m")
    };
    let mut state = idle_on_root();
    let prompt = wire::claude_sdk_input::Of::Prompt(wire::PromptInput {
        text: "/cost".into(),
        ..Default::default()
    });
    ClaudeSdk::step(&mut state, sdk_input(b"p1", prompt));
    let facts = [
        json!({"type": "system", "subtype": "status", "status": red("thinking"), "uuid": "st1", "session_id": "s"}),
        json!({"type": "system", "subtype": "model_refusal_fallback", "trigger": "refusal", "direction": "down",
            "original_model": "a", "fallback_model": "b", "request_id": null,
            "api_refusal_explanation": red("switched"), "content": "x", "uuid": "f1", "session_id": "s"}),
        json!({"type": "system", "subtype": "model_refusal_no_fallback", "original_model": "a", "request_id": null,
            "content": red("refused"), "uuid": "f2", "session_id": "s"}),
        json!({"type": "system", "subtype": "local_command_output", "content": red("ok"), "uuid": "lc1", "session_id": "s"}),
        json!({"type": "assistant", "uuid": "m1", "session_id": "s", "parent_tool_use_id": null, "message": {
            "id": "m1", "type": "message", "role": "assistant", "model": "claude-root",
            "content": [{"type": "tool_use", "id": "t9", "name": "Bash", "input": {"command": "ls"}}],
            "usage": {"input_tokens": 1, "output_tokens": 1}}}),
        json!({"type": "system", "subtype": "permission_denied", "tool_name": "Bash", "tool_use_id": "t9",
            "decision_reason": red("denied"), "message": "m", "uuid": "pd1", "session_id": "s"}),
        json!({"type": "control_request", "request_id": "req-1", "request": {
            "subtype": "can_use_tool", "tool_name": "Bash", "input": {"command": "rm x"}, "tool_use_id": "t1",
            "decision_reason": red("reason"), "description": red("description"), "title": red("title")}}),
        json!({"type": "control_request", "request_id": "req-2", "request": {
            "subtype": "elicitation", "mcp_server_name": "github", "message": red("Sign in"),
            "mode": "url", "url": "https://github.com/login/device", "elicitation_id": "el-1"}}),
    ];
    let mut coded = Vec::new();
    for fact in facts {
        let what = fact["subtype"]
            .as_str()
            .or(fact["request"]["subtype"].as_str())
            .unwrap_or("assistant")
            .to_owned();
        let stepped = ClaudeSdk::step(&mut state, stream(fact));
        let snapshot = stepped
            .step
            .snapshot
            .as_ref()
            .map(|snapshot| snapshot.body.clone());
        if stepped
            .step
            .items
            .iter()
            .map(|item| item.body.as_slice())
            .chain(snapshot.as_deref())
            .any(holds_terminal_codes)
            || stepped
                .step
                .items
                .iter()
                .any(|item| holds_controls(&item.text))
        {
            coded.push(what);
        }
    }
    assert!(
        coded.is_empty(),
        "terminal codes reached the wire from {coded:?}"
    );
}

/// Headless Claude's plan approval switches to never ask where Claude was
/// launched allowing it, as terminal Claude's does: one rule for both.
#[test]
fn a_plan_approval_switches_to_never_ask_where_claude_offers_it() {
    let spec = wire::AgentSpec {
        agent_id: b"agent".to_vec(),
        incarnation: 1,
        provider_args: vec!["--allow-dangerously-skip-permissions".into()],
        ..Default::default()
    };
    let (mut state, _) = ClaudeSdk::initial(&spec);
    ClaudeSdk::step(
        &mut state,
        stream(
            json!({"type": "system", "subtype": "init", "uuid": "i", "session_id": "s", "model": "claude-root"}),
        ),
    );
    let prompt = wire::claude_sdk_input::Of::Prompt(wire::PromptInput {
        text: "plan it".into(),
        ..Default::default()
    });
    ClaudeSdk::step(&mut state, sdk_input(b"p1", prompt));
    let asked = ClaudeSdk::step(
        &mut state,
        stream(json!({
            "type": "control_request", "request_id": "req-1",
            "request": {"subtype": "can_use_tool", "tool_name": "ExitPlanMode",
                "input": {"plan": "1. Do it."}, "tool_use_id": "t1"},
        })),
    );
    let (_, snapshot) = sdk_snapshot(&asked).expect("a snapshot");
    let Some(wire::ask::Body::Plan(plan)) = &snapshot.asks[0].body else {
        panic!("{:?}", snapshot.asks);
    };
    assert_eq!(plan.switch_permission, "bypassPermissions");
}

/// Claude can name a window without saying how much of it is used. A window
/// seen before keeps the use and reset it was last given; a window first
/// named that way reads unknown rather than empty.
#[test]
fn a_window_reported_without_its_use_keeps_what_was_held_or_reads_unknown() {
    let mut state = idle_on_root();
    let report = |info: Value| {
        stream(
            json!({"type": "rate_limit_event", "uuid": "rl", "session_id": "s", "rate_limit_info": info}),
        )
    };
    ClaudeSdk::step(
        &mut state,
        report(
            json!({"status": "allowed", "rateLimitType": "seven_day_opus",
            "utilization": 0.9, "resetsAt": 1_800_000_000}),
        ),
    );
    ClaudeSdk::step(
        &mut state,
        report(json!({"status": "allowed_warning", "rateLimitType": "seven_day_opus"})),
    );
    let stepped = ClaudeSdk::step(
        &mut state,
        report(json!({"status": "rejected", "rateLimitType": "overage"})),
    );
    let (_, snapshot) = sdk_snapshot(&stepped).expect("a snapshot");
    let usage = snapshot.usage.expect("usage");
    let meter = |name: &str| {
        usage
            .windows
            .iter()
            .find(|window| window.provider_name == name)
            .and_then(|window| window.meter)
            .unwrap_or_else(|| panic!("no {name} window in {usage:?}"))
    };
    let held = meter("seven_day_opus");
    assert_eq!(held.used_percent, 90.0);
    assert!(!held.used_unknown);
    assert_eq!(held.resets_at_ms, Some(1_800_000_000_000));
    assert_eq!(held.state(), wire::UsageState::NearLimit);
    let new = meter("overage");
    assert!(new.used_unknown, "{new:?}");
    assert_eq!(new.state(), wire::UsageState::Blocked);
}

/// A person stops one background job: amux asks Claude to stop that task,
/// the job reads stopping until Claude says it ended, a refusal shows on
/// the job in Claude's words, and a key no running job has is refused.
#[test]
fn a_stopped_job_reads_stopping_until_claude_says_it_ended() {
    let mut state = idle_on_root();
    let mut jobs: Vec<wire::BackgroundJob> = Vec::new();
    let mut step = |state: &mut State, event: Event| {
        let stepped = ClaudeSdk::step(state, event);
        if let Some((_, snapshot)) = sdk_snapshot(&stepped) {
            jobs = snapshot.background_jobs.unwrap_or_default().jobs;
        }
        (stepped, jobs.clone())
    };
    step(
        &mut state,
        stream(json!({
            "type": "system", "subtype": "background_tasks_changed", "uuid": "l1", "session_id": "s",
            "tasks": [
                {"task_id": "b1", "task_type": "local_bash", "description": "cargo test"},
                {"task_id": "b2", "task_type": "local_bash", "description": "npm run dev"},
            ],
        })),
    );
    let stop = |key: &str| wire::claude_sdk_input::Of::StopJob(wire::StopJob { key: key.into() });
    let stop_requests = |stepped: &interpret::Stepped| -> Vec<Value> {
        stepped
            .effects
            .iter()
            .filter_map(|effect| match effect {
                Effect::ProviderWrite(bytes) => serde_json::from_slice::<Value>(bytes).ok(),
                _ => None,
            })
            .filter(|write| write["request"]["subtype"] == "stop_task")
            .collect()
    };
    let verdict = |stepped: &interpret::Stepped, id: &[u8]| {
        stepped.effects.iter().find_map(|effect| match effect {
            Effect::Reply { input_id, verdict } if input_id == id => verdict.of.clone(),
            _ => None,
        })
    };
    let job = |jobs: &[wire::BackgroundJob], key: &str| {
        jobs.iter()
            .find(|job| job.key == key)
            .cloned()
            .unwrap_or_else(|| panic!("no job {key} in {jobs:?}"))
    };

    let (stopped, jobs) = step(&mut state, sdk_input(b"s1", stop("b1")));
    assert!(
        matches!(
            verdict(&stopped, b"s1"),
            Some(wire::send_input_response::Of::Accepted(_))
        ),
        "{:?}",
        stopped.effects
    );
    let requests = stop_requests(&stopped);
    assert_eq!(requests.len(), 1, "{:?}", stopped.effects);
    assert_eq!(requests[0]["request"]["task_id"], "b1");
    assert!(job(&jobs, "b1").stopping && !job(&jobs, "b2").stopping);

    let (refused, _) = step(&mut state, sdk_input(b"s2", stop("b2")));
    let request_id = stop_requests(&refused)[0]["request_id"].clone();
    let (_, jobs) = step(
        &mut state,
        stream(json!({
            "type": "control_response",
            "response": {"subtype": "error", "request_id": request_id,
                "error": "Task b2 is not running (status: completed)"},
        })),
    );
    let b2 = job(&jobs, "b2");
    assert!(!b2.stopping, "{b2:?}");
    assert_eq!(b2.stop_error, "Task b2 is not running (status: completed)");

    let (_, jobs) = step(
        &mut state,
        stream(json!({
            "type": "system", "subtype": "task_notification", "uuid": "n1", "session_id": "s",
            "task_id": "b1", "tool_use_id": "toolu_b1", "status": "stopped",
            "summary": "", "output_file": "",
        })),
    );
    assert!(jobs.iter().all(|job| job.key != "b1"), "{jobs:?}");

    let (unknown, _) = step(&mut state, sdk_input(b"s3", stop("nope")));
    assert!(
        matches!(
            verdict(&unknown, b"s3"),
            Some(wire::send_input_response::Of::Rejected(_))
        ),
        "{:?}",
        unknown.effects
    );
    assert!(stop_requests(&unknown).is_empty());
}

/// The notice that an organization's policy withholds amux's tools holds
/// for the incarnation the agent said so for: one launched once the
/// policy is gone reads amux's tool server as Claude reports it.
#[test]
fn a_withheld_tool_server_is_not_carried_into_the_next_incarnation() {
    use prost::Message as _;
    let failed = |step: &wire::Step| {
        let snapshot = step.snapshot.as_ref().expect("a snapshot");
        let body = wire::ClaudeSnapshot::decode(&snapshot.body[..]).unwrap();
        body.servers
            .iter()
            .any(|server| server.status == wire::ToolServerStatus::Failed as i32)
    };
    let spec = wire::AgentSpec {
        agent_id: b"agent".to_vec(),
        incarnation: 1,
        ..Default::default()
    };
    let (mut state, _) = ClaudeSdk::initial(&spec);
    let withheld = ClaudeSdk::step(
        &mut state,
        Event::Fact(interpret::Fact {
            channel: interpret::Channel::Agent,
            payload: interpret::claude_pty::tools_withheld_fact(),
        }),
    );
    assert!(failed(&withheld.step), "the policy's incarnation says so");
    let next = wire::AgentSpec {
        incarnation: 2,
        ..spec
    };
    let (_, step) = ClaudeSdk::reincarnate(state, &next);
    assert!(!failed(&step), "the next incarnation does not");
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
/// Claude starts, written once it can take it, and given no second row or
/// second started rule when it does.
#[test]
fn a_first_prompt_is_drawn_while_claude_starts_and_written_once() {
    let (mut state, first_frame) = ClaudeSdk::initial(&first_prompt_spec());
    let drawn = ClaudeSdk::step(&mut state, Event::Tick { at_ms: 1 });
    assert_drawn_at_once(&first_frame, &drawn.step);
    let boundary = drawn.step.items[0].key.clone();

    let ran = ClaudeSdk::step(
        &mut state,
        stream(json!({"type": "system", "subtype": "init", "uuid": "i", "session_id": "s"})),
    );
    let written = ran
        .effects
        .iter()
        .filter(|effect| matches!(effect, Effect::UserMessage { .. }))
        .count();
    assert_eq!(written, 1, "{:?}", ran.effects);
    assert!(ran.step.items.iter().all(|item| item.input_id != b"first"));
    assert!(
        ran.step
            .items
            .iter()
            .all(|item| !item.key.starts_with("boundary:") || item.key == boundary),
        "the init fills in the started rule already drawn: {:?}",
        ran.step.items
    );
    assert_eq!(
        ran.step.snapshot.expect("a snapshot").phase(),
        wire::Phase::Working
    );
}

/// A prompt that wakes a suspended agent opens its next incarnation as a
/// new agent's first prompt opens its first: queued behind nothing, drawn
/// under the resumed rule by the incarnation's first event, and written
/// once Claude can take it.
#[test]
fn a_prompt_that_wakes_claude_is_drawn_at_once_and_written_once() {
    let mut state = started();
    ClaudeSdk::step(
        &mut state,
        Event::Exiting(wire::IncarnationEnd {
            cause: wire::ExitCause::Suspended as i32,
            detail: None,
        }),
    );
    let wake = wire::AgentSpec {
        incarnation: 2,
        ..first_prompt_spec()
    };
    let (mut state, first_frame) = ClaudeSdk::reincarnate(state, &wake);
    let drawn = ClaudeSdk::step(&mut state, Event::Tick { at_ms: 1 });
    assert_drawn_at_once(&first_frame, &drawn.step);
    assert_eq!(boundaries(&drawn), [wire::BoundaryKind::Resumed]);
    let boundary = drawn.step.items[0].key.clone();

    let ran = ClaudeSdk::step(
        &mut state,
        stream(json!({"type": "system", "subtype": "init", "uuid": "i2", "session_id": "s"})),
    );
    let written = ran
        .effects
        .iter()
        .filter(|effect| matches!(effect, Effect::UserMessage { .. }))
        .count();
    assert_eq!(written, 1, "{:?}", ran.effects);
    assert!(
        ran.step.items.iter().all(|item| item.input_id != b"first"
            && (!item.key.starts_with("boundary:") || item.key == boundary)),
        "{:?}",
        ran.step.items
    );
}

// --- what Claude holds, for suspension --------------------------------------
//
// The frames below are cut from live recordings of Claude Code 2.1.325
// (2026-11-15), keeping only the fields the interpreter reads.

fn holds_after(state: &mut State, events: impl IntoIterator<Item = Value>) -> interpret::Holds {
    for event in events {
        ClaudeSdk::step(state, stream(event));
    }
    ClaudeSdk::holds(state)
}

fn turn_result() -> Value {
    json!({"type": "result", "subtype": "success", "uuid": "r", "session_id": "s", "duration_ms": 10})
}

/// A background command that ends between turns: Claude says so with
/// `task_updated`, `task_notification` and an empty task list, and then
/// starts a turn of its own to report it. From the end until that turn
/// starts, the agent owes a turn and is not idle.
#[test]
fn a_background_command_ending_between_turns_owes_the_turn_claude_starts_to_report_it() {
    let mut state = started();
    let running = holds_after(
        &mut state,
        [
            json!({"type": "assistant", "uuid": "a1", "session_id": "s", "message": {
                "id": "msg_011Ch5JSmtVqPbemzLmJe5sh", "role": "assistant", "content": [{
                    "type": "tool_use", "id": "toolu_01X4vwoyvkp8G3cFRfXHcXhw", "name": "Bash",
                    "input": {"command": "sleep 20; echo done", "description": "Wait 20 seconds then print done", "run_in_background": true}}]}}),
            json!({"type": "system", "subtype": "background_tasks_changed", "uuid": "b1", "session_id": "s",
                "tasks": [{"task_id": "b3t6jdojf", "task_type": "local_bash", "description": "Wait 20 seconds then print done"}]}),
            json!({"type": "system", "subtype": "task_started", "uuid": "t1", "session_id": "s",
                "task_id": "b3t6jdojf", "tool_use_id": "toolu_01X4vwoyvkp8G3cFRfXHcXhw",
                "description": "Wait 20 seconds then print done", "is_backgrounded": true, "task_type": "local_bash"}),
            turn_result(),
        ],
    );
    assert_eq!(running.background_jobs, 1, "{running:?}");
    assert!(!state.shared().is_busy());

    let ended = holds_after(
        &mut state,
        [
            json!({"type": "system", "subtype": "task_updated", "uuid": "t2", "session_id": "s",
                "task_id": "b3t6jdojf", "patch": {"status": "completed", "end_time": 1794772553215_i64}}),
            json!({"type": "system", "subtype": "task_notification", "uuid": "t3", "session_id": "s",
                "task_id": "b3t6jdojf", "tool_use_id": "toolu_01X4vwoyvkp8G3cFRfXHcXhw", "status": "completed",
                "summary": "Background command \"Wait 20 seconds then print done\" completed (exit code 0)"}),
            json!({"type": "system", "subtype": "background_tasks_changed", "uuid": "b2", "session_id": "s", "tasks": []}),
        ],
    );
    assert_eq!(
        (ended.background_jobs, ended.turn_owed),
        (0, true),
        "{ended:?}"
    );

    // Claude's own turn: its reply, then its result.
    let reported = holds_after(
        &mut state,
        [
            json!({"type": "assistant", "uuid": "a2", "session_id": "s", "message": {
            "id": "msg_011Ch5JUbYhB5GqsRf7daQ5W", "role": "assistant", "content": [{
                "type": "text", "text": "The background command finished successfully (exit code 0)."}]}}),
        ],
    );
    assert!(
        state.shared().is_busy() && !reported.turn_owed,
        "{reported:?}"
    );
    assert!(holds_after(&mut state, [turn_result()]).nothing());
}

/// ScheduleWakeup's result gives the time it fires; the wake-up is held
/// until its `command_lifecycle` arrives with `wakeup_source`, which also
/// starts the turn Claude runs for it with nothing from amux.
#[test]
fn a_scheduled_wakeup_is_held_until_it_fires_as_a_turn_of_its_own() {
    let mut state = started();
    let scheduled = holds_after(
        &mut state,
        [
            json!({"type": "assistant", "uuid": "a1", "session_id": "s", "message": {
                "id": "msg_011Ch5JSmpH7yHP8JfDV9qPy", "role": "assistant", "content": [{
                    "type": "tool_use", "id": "toolu_0112BzM6pLnyoi55sxnrcCum", "name": "ScheduleWakeup",
                    "input": {"delaySeconds": 60, "prompt": "say woke", "reason": "User asked for a 60-second wakeup that will say woke.", "noop": false}}]}}),
            json!({"type": "user", "uuid": "u1", "session_id": "s", "message": {"role": "user", "content": [{
                "tool_use_id": "toolu_0112BzM6pLnyoi55sxnrcCum", "type": "tool_result",
                "content": "Next wakeup scheduled for 20:57:00 (in 86s)."}]},
                "tool_use_result": {"scheduledFor": 1794772620000_i64, "clampedDelaySeconds": 60, "wasClamped": false}}),
            turn_result(),
        ],
    );
    assert_eq!(scheduled.wakeups, 1, "{scheduled:?}");
    assert!(!state.shared().is_busy());

    let fired = holds_after(
        &mut state,
        [
            json!({"type": "command_lifecycle", "command_uuid": "cc214fe7-e48e-4fd9-906c-a54faa064d4c",
            "state": "started", "wakeup_source": "loop_wakeup", "scheduled_for": 1794772620000_i64}),
        ],
    );
    assert_eq!(fired.wakeups, 0, "{fired:?}");
    assert!(state.shared().is_busy(), "the wake-up's turn runs");
    assert!(holds_after(&mut state, [turn_result()]).nothing());
}

/// A recurring CronCreate stays held through each tick (a `schedule_wakeup`
/// with no time) until CronDelete names its id.
#[test]
fn a_recurring_cron_is_held_until_it_is_deleted() {
    let mut state = started();
    let tool = |id: &str, name: &str, input: Value| {
        json!({"type": "assistant", "uuid": format!("a-{id}"), "session_id": "s", "message": {
            "id": format!("msg-{id}"), "role": "assistant", "content": [{
                "type": "tool_use", "id": id, "name": name, "input": input}]}})
    };
    let result = |id: &str, text: &str, result: Value| {
        json!({"type": "user", "uuid": format!("u-{id}"), "session_id": "s", "message": {"role": "user",
            "content": [{"tool_use_id": id, "type": "tool_result", "content": text}]},
            "tool_use_result": result})
    };
    let created = holds_after(
        &mut state,
        [
            tool(
                "toolu_01PbVBufph2J2GdhLudukC8F",
                "CronCreate",
                json!({"cron": "* * * * *", "prompt": "say tick", "recurring": true}),
            ),
            result(
                "toolu_01PbVBufph2J2GdhLudukC8F",
                "Scheduled recurring job 07a89516 (Every minute).",
                json!({"id": "07a89516", "humanSchedule": "Every minute", "recurring": true,
                    "durable": false, "nextFireAt": 1794772740897.4834_f64}),
            ),
            turn_result(),
        ],
    );
    assert_eq!(created.wakeups, 1, "{created:?}");
    let ticked = holds_after(
        &mut state,
        [
            json!({"type": "command_lifecycle", "command_uuid": "392bfcee-d328-4f87-a573-b082dcaf615c",
                "state": "started", "wakeup_source": "schedule_wakeup"}),
            turn_result(),
        ],
    );
    assert_eq!(ticked.wakeups, 1, "a recurring cron stays: {ticked:?}");
    let deleted = holds_after(
        &mut state,
        [
            tool(
                "toolu_01LHLadyBiWcpNjhQ3Zq3wrz",
                "CronDelete",
                json!({"id": "07a89516"}),
            ),
            result(
                "toolu_01LHLadyBiWcpNjhQ3Zq3wrz",
                "Cancelled job 07a89516.",
                json!({"id": "07a89516"}),
            ),
            turn_result(),
        ],
    );
    assert!(deleted.nothing(), "{deleted:?}");
}

/// An ask answered allowing one of its scope choices whose destination is
/// the session: Claude keeps that only in its process, so the agent holds
/// a session grant from then on, and a new process holds none.
#[test]
fn a_permission_granted_for_the_session_is_held_until_the_process_ends() {
    let mut state = started();
    ClaudeSdk::step(
        &mut state,
        stream(
            json!({"type": "control_request", "request_id": "ask-1", "request": {
            "subtype": "can_use_tool", "tool_name": "Bash", "tool_use_id": "toolu_01H5CJdTAppzXTHLBmP6qU4n",
            "input": {"command": "touch g1.txt", "description": "Create empty file g1.txt"},
            "permission_suggestions": [
                {"type": "addRules", "rules": [{"toolName": "Bash", "ruleContent": "touch g1.txt"}], "behavior": "allow", "destination": "localSettings"},
                {"type": "addDirectories", "directories": ["/private/tmp/amux-rec-sdk"], "destination": "session"},
                {"type": "setMode", "mode": "acceptEdits", "destination": "session"}]}}),
        ),
    );
    assert!(!ClaudeSdk::holds(&state).session_grant);
    let allow = |scope: u32| {
        sdk_input(
            b"answer",
            wire::claude_sdk_input::Of::Answer(wire::ClaudeAnswer {
                ask_key: "ask-1".into(),
                of: Some(wire::claude_answer::Of::Permission(
                    wire::PermissionAnswer {
                        of: Some(wire::permission_answer::Of::Allow(wire::PermissionAllow {
                            scope: Some(scope),
                        })),
                    },
                )),
            }),
        )
    };
    // Saved to the project's settings, it survives a resume.
    let mut saved = state.clone_via_checkpoint();
    ClaudeSdk::step(&mut saved, allow(0));
    assert!(!ClaudeSdk::holds(&saved).session_grant);
    // A mode for the session is the agent's permission from then on, which
    // its snapshot says and a resume starts in: nothing is lost.
    let mut moved = state.clone_via_checkpoint();
    let stepped = ClaudeSdk::step(&mut moved, allow(2));
    assert!(!ClaudeSdk::holds(&moved).session_grant);
    let (_, snapshot) = sdk_snapshot(&stepped).expect("a snapshot");
    assert_eq!(snapshot.permission.as_deref(), Some("acceptEdits"));
    let mut granted = state.clone_via_checkpoint();
    ClaudeSdk::step(&mut granted, allow(1));
    assert!(ClaudeSdk::holds(&granted).session_grant);
    ClaudeSdk::step(&mut granted, Event::ProviderExit { code: Some(0) });
    assert!(ClaudeSdk::holds(&granted).nothing());
}

/// Approving a plan moves Claude out of plan for the session. The move is
/// the agent's permission from the answer on, before Claude reports it, so
/// the snapshot a resume starts from says it, and nothing is held that a
/// suspension would lose.
#[test]
fn an_approved_plan_moves_the_permission_and_holds_nothing() {
    let spec = wire::AgentSpec {
        agent_id: b"agent".to_vec(),
        incarnation: 1,
        provider_args: vec!["--permission-mode".into(), "plan".into()],
        ..Default::default()
    };
    let (mut state, _) = ClaudeSdk::initial(&spec);
    ClaudeSdk::step(
        &mut state,
        stream(
            json!({"type": "system", "subtype": "init", "uuid": "i", "session_id": "s", "model": "claude-root", "permissionMode": "plan"}),
        ),
    );
    let prompt = wire::claude_sdk_input::Of::Prompt(wire::PromptInput {
        text: "plan it".into(),
        ..Default::default()
    });
    ClaudeSdk::step(&mut state, sdk_input(b"p1", prompt));
    ClaudeSdk::step(
        &mut state,
        stream(json!({
            "type": "control_request", "request_id": "req-1",
            "request": {"subtype": "can_use_tool", "tool_name": "ExitPlanMode",
                "input": {"plan": "1. Do it."}, "tool_use_id": "t1"},
        })),
    );
    let approve = sdk_input(
        b"a1",
        wire::claude_sdk_input::Of::Answer(wire::ClaudeAnswer {
            ask_key: "req-1".into(),
            of: Some(wire::claude_answer::Of::Plan(wire::PlanAnswer {
                choice: wire::PlanChoice::Start as i32,
                note: None,
            })),
        }),
    );
    let stepped = ClaudeSdk::step(&mut state, approve);
    let (_, snapshot) = sdk_snapshot(&stepped).expect("a snapshot");
    assert_eq!(snapshot.permission.as_deref(), Some("default"));
    assert!(
        ClaudeSdk::holds(&state).nothing(),
        "{:?}",
        ClaudeSdk::holds(&state)
    );
}

trait CloneViaCheckpoint {
    fn clone_via_checkpoint(&self) -> Self;
}

impl CloneViaCheckpoint for State {
    fn clone_via_checkpoint(&self) -> Self {
        interpret::decode_checkpoint(&interpret::encode_checkpoint(self)).unwrap()
    }
}

/// A recurring cron's tick names no time, so it cannot be the one-shot
/// cron that is also pending: the one-shot stays held through the tick and
/// after the recurring one is deleted.
#[test]
fn a_recurring_crons_tick_leaves_a_pending_one_shot_held() {
    let mut state = started();
    let created = |call: &str, id: &str, recurring: bool, at: f64| {
        [
            json!({"type": "assistant", "uuid": format!("a-{call}"), "session_id": "s", "message": {
                "id": format!("msg-{call}"), "role": "assistant", "content": [{
                    "type": "tool_use", "id": call, "name": "CronCreate",
                    "input": {"cron": "* * * * *", "prompt": "say tick", "recurring": recurring}}]}}),
            json!({"type": "user", "uuid": format!("u-{call}"), "session_id": "s", "message": {"role": "user",
                "content": [{"tool_use_id": call, "type": "tool_result", "content": "Scheduled."}]},
                "tool_use_result": {"id": id, "humanSchedule": "Every minute", "recurring": recurring,
                    "durable": false, "nextFireAt": at}}),
        ]
    };
    ClaudeSdk::step(
        &mut state,
        Event::Tick {
            at_ms: 1_794_772_700_000,
        },
    );
    let pending = holds_after(
        &mut state,
        created("toolu_once", "0nce0001", false, 1_794_776_400_000.0)
            .into_iter()
            .chain(created(
                "toolu_tick",
                "07a89516",
                true,
                1_794_772_740_897.48,
            ))
            .chain([turn_result()]),
    );
    assert_eq!(pending.wakeups, 2, "{pending:?}");
    ClaudeSdk::step(
        &mut state,
        Event::Tick {
            at_ms: 1_794_772_741_000,
        },
    );
    let ticked = holds_after(
        &mut state,
        [
            json!({"type": "command_lifecycle", "command_uuid": "392bfcee-d328-4f87-a573-b082dcaf615c",
                "state": "started", "wakeup_source": "schedule_wakeup"}),
            turn_result(),
        ],
    );
    assert_eq!(
        ticked.wakeups, 2,
        "the tick is the recurring cron's: {ticked:?}"
    );
    let deleted = holds_after(
        &mut state,
        [
            json!({"type": "assistant", "uuid": "a-del", "session_id": "s", "message": {
                "id": "msg-del", "role": "assistant", "content": [{
                    "type": "tool_use", "id": "toolu_del", "name": "CronDelete", "input": {"id": "07a89516"}}]}}),
            json!({"type": "user", "uuid": "u-del", "session_id": "s", "message": {"role": "user",
                "content": [{"tool_use_id": "toolu_del", "type": "tool_result", "content": "Cancelled job 07a89516."}]},
                "tool_use_result": {"id": "07a89516"}}),
            turn_result(),
        ],
    );
    assert_eq!(
        deleted.wakeups, 1,
        "the one-shot is still pending: {deleted:?}"
    );
}

/// A suspension ends the provider with nothing left to do: the agent's last
/// snapshot says idle, as its row does, not starting.
#[test]
fn a_suspensions_last_snapshot_says_idle() {
    let mut state = started();
    let stepped = ClaudeSdk::step(
        &mut state,
        Event::Exiting(wire::IncarnationEnd {
            cause: wire::ExitCause::Suspended as i32,
            detail: None,
        }),
    );
    let phase = stepped.step.snapshot.map(|snapshot| snapshot.phase);
    assert_eq!(phase, Some(wire::Phase::Idle as i32));
}

/// The provider's own end says by its last snapshot whether it cut a turn
/// off: idle when it had none under way, starting when it did. A restart of
/// the machine that killed the provider first reads the row's phase so, to
/// suspend an idle agent and continue one in its turn.
#[test]
fn a_providers_own_end_says_idle_unless_it_cut_a_turn_off() {
    let mut idle = started();
    let stepped = ClaudeSdk::step(&mut idle, Event::ProviderExit { code: None });
    let phase = stepped.step.snapshot.map(|snapshot| snapshot.phase());
    assert!(
        phase.is_none_or(|phase| phase == wire::Phase::Idle),
        "idle it stays: {phase:?}"
    );

    let mut working = started();
    let prompt = wire::Input::new(
        b"p1".to_vec(),
        wire::ClaudeSdkInput {
            of: Some(wire::claude_sdk_input::Of::Prompt(wire::PromptInput {
                text: "go".into(),
                attachments: Vec::new(),
            })),
        },
    );
    let stepped = ClaudeSdk::step(&mut working, Event::Input(prompt));
    assert_eq!(
        stepped.step.snapshot.map(|snapshot| snapshot.phase()),
        Some(wire::Phase::Working)
    );
    let stepped = ClaudeSdk::step(&mut working, Event::ProviderExit { code: None });
    let phase = stepped.step.snapshot.map(|snapshot| snapshot.phase());
    assert_eq!(phase, Some(wire::Phase::Starting), "a turn was cut off");
}

/// A provider subagent still running when the provider ends is work cut
/// off, though the agent's own turn had ended: the last snapshot reads
/// starting, as for a turn cut off, so a restart of the machine continues
/// the agent rather than leaving it suspended, and the subagent ends with
/// the provider.
#[test]
fn a_providers_own_end_while_a_subagent_runs_says_work_was_cut_off() {
    let mut state = idle_on_root();
    ClaudeSdk::step(
        &mut state,
        stream(json!({
            "type": "system", "subtype": "task_started", "uuid": "t1", "session_id": "s",
            "task_id": "a1", "task_type": "local_agent", "tool_use_id": "toolu_agent",
            "description": "count", "is_backgrounded": true,
        })),
    );
    assert!(!state.shared().is_busy(), "the agent's own turn is over");
    let stepped = ClaudeSdk::step(&mut state, Event::ProviderExit { code: None });
    let phase = stepped.step.snapshot.map(|snapshot| snapshot.phase());
    assert_eq!(
        phase,
        Some(wire::Phase::Starting),
        "its subagent was cut off"
    );
    let child = interpret::subagent_id(b"agent", "a1");
    assert!(
        stepped
            .step
            .agents
            .iter()
            .any(|part| part.agent_id == child && part.end.is_some()),
        "the subagent ends with the provider: {:?}",
        stepped.step.agents
    );
}

const MINUTE: i64 = 60 * 1000;

/// A turn that ends at `at`, whose request read a conversation of 250k
/// tokens and wrote to the cache for `ttl` ("1h" or "5m").
fn big_turn_ending(state: &mut State, at: i64, ttl: &str) -> i32 {
    let created = if ttl == "1h" {
        json!({"ephemeral_1h_input_tokens": 10_000, "ephemeral_5m_input_tokens": 0})
    } else {
        json!({"ephemeral_1h_input_tokens": 0, "ephemeral_5m_input_tokens": 10_000})
    };
    ClaudeSdk::step(state, Event::Tick { at_ms: at });
    ClaudeSdk::step(
        state,
        stream(
            json!({"type": "stream_event", "event": {"type": "message_start", "message": {
                "id": "msg_1",
                "usage": {
                    "input_tokens": 2,
                    "cache_read_input_tokens": 240_000,
                    "cache_creation_input_tokens": 10_000,
                    "cache_creation": created,
                },
            }}}),
        ),
    );
    let ended = ClaudeSdk::step(
        state,
        stream(json!({"type": "result", "subtype": "success", "uuid": "r1", "duration_ms": 10})),
    );
    ended.step.turn_end.expect("the turn ends").outcome
}

fn compact_sent(stepped: &interpret::Stepped) -> Option<String> {
    stepped.effects.iter().find_map(|effect| match effect {
        Effect::UserMessage { uuid, text, .. } if text == "/compact" => Some(uuid.clone()),
        _ => None,
    })
}

fn compactions(stepped: &interpret::Stepped) -> Vec<wire::Compaction> {
    use prost::Message as _;
    stepped
        .step
        .items
        .iter()
        .filter_map(
            |item| match wire::ClaudeItem::decode(item.body.as_slice()).ok()?.kind? {
                wire::claude_item::Kind::Compaction(compaction) => Some(compaction),
                _ => None,
            },
        )
        .collect()
}

/// Headless Claude does not compact while idle, as its terminal does: amux
/// sends `/compact` just inside the hour its cache lives. It is nobody's
/// turn: no prompt is drawn, no turn ends, a prompt typed meanwhile waits
/// for it, and the compaction row says it ran while idle.
#[test]
fn an_idle_conversation_on_the_hour_cache_is_compacted_before_it_expires() {
    let mut state = started();
    big_turn_ending(&mut state, 1_000, "1h");
    let due = 1_000 + 55 * MINUTE;
    assert_eq!(ClaudeSdk::wake_at(&state), Some(due));
    assert_eq!(
        ClaudeSdk::housekeeping(&state),
        Housekeeping::Due { at_ms: due }
    );
    assert!(compact_sent(&ClaudeSdk::step(&mut state, Event::Tick { at_ms: due - 1 })).is_none());

    let sent = ClaudeSdk::step(&mut state, Event::Tick { at_ms: due });
    let uuid = compact_sent(&sent).expect("the idle compaction is sent");
    assert_eq!(ClaudeSdk::housekeeping(&state), Housekeeping::Running);
    assert!(
        sent.step.items.is_empty(),
        "nothing draws it: {:?}",
        sent.step.items
    );
    // Only the deadline for its result is awaited now.
    assert_eq!(ClaudeSdk::wake_at(&state), Some(due + 10 * MINUTE));

    let typed = ClaudeSdk::step(
        &mut state,
        sdk_input(
            b"p2",
            wire::claude_sdk_input::Of::Prompt(wire::PromptInput {
                text: "carry on".into(),
                ..Default::default()
            }),
        ),
    );
    assert!(
        !typed
            .effects
            .iter()
            .any(|effect| matches!(effect, Effect::UserMessage { .. })),
        "a prompt waits for the compaction: {:?}",
        typed.effects
    );

    ClaudeSdk::step(
        &mut state,
        stream(
            json!({"type": "system", "subtype": "status", "status": "compacting", "uuid": "st"}),
        ),
    );
    let boundary = ClaudeSdk::step(
        &mut state,
        stream(
            json!({"type": "system", "subtype": "compact_boundary", "uuid": "cb", "session_id": "s",
            "compact_metadata": {"trigger": "manual", "pre_tokens": 250_000, "post_tokens": 30_000}}),
        ),
    );
    assert_eq!(
        compactions(&boundary),
        [wire::Compaction {
            tokens_before: Some(250_000),
            tokens_after: Some(30_000),
            automatic: false,
            idle: true,
        }]
    );
    let over = ClaudeSdk::step(
        &mut state,
        stream(
            json!({"type": "result", "subtype": "success", "uuid": "r2", "duration_ms": 17_000,
            "user_message_uuid": uuid, "user_message_uuids": [uuid]}),
        ),
    );
    assert_eq!(over.step.turn_end, None, "no turn ends for it");
    assert_eq!(ClaudeSdk::housekeeping(&state), Housekeeping::Settled);
    assert!(
        over.effects
            .iter()
            .any(|effect| matches!(effect, Effect::UserMessage { text, .. } if text == "carry on")),
        "the waiting prompt goes on: {:?}",
        over.effects
    );
}

/// On the five-minute cache, or once the hour has passed (a machine that
/// slept through it), compacting would read the conversation uncached and
/// save nothing.
#[test]
fn no_idle_compaction_without_a_warm_hour_cache() {
    let mut state = started();
    big_turn_ending(&mut state, 1_000, "5m");
    assert_eq!(ClaudeSdk::wake_at(&state), None);
    assert_eq!(ClaudeSdk::housekeeping(&state), Housekeeping::Settled);

    let mut state = started();
    big_turn_ending(&mut state, 1_000, "1h");
    let late = ClaudeSdk::step(
        &mut state,
        Event::Tick {
            at_ms: 1_000 + 61 * MINUTE,
        },
    );
    assert!(compact_sent(&late).is_none());
    assert_eq!(ClaudeSdk::wake_at(&state), None);
    assert_eq!(ClaudeSdk::housekeeping(&state), Housekeeping::Settled);
}

/// A result that never names amux's `/compact` would hold the queue
/// forever: after ten minutes amux stops waiting, and a prompt goes on.
#[test]
fn an_idle_compaction_without_its_result_stops_holding_the_queue() {
    let mut state = started();
    big_turn_ending(&mut state, 1_000, "1h");
    let due = 1_000 + 55 * MINUTE;
    assert!(compact_sent(&ClaudeSdk::step(&mut state, Event::Tick { at_ms: due })).is_some());
    ClaudeSdk::step(
        &mut state,
        Event::Tick {
            at_ms: due + 10 * MINUTE,
        },
    );
    assert_eq!(ClaudeSdk::wake_at(&state), None);
    let typed = ClaudeSdk::step(
        &mut state,
        sdk_input(
            b"p2",
            wire::claude_sdk_input::Of::Prompt(wire::PromptInput {
                text: "carry on".into(),
                ..Default::default()
            }),
        ),
    );
    assert!(
        typed
            .effects
            .iter()
            .any(|effect| matches!(effect, Effect::UserMessage { text, .. } if text == "carry on")),
        "{:?}",
        typed.effects
    );
    // The compaction is forgotten, not left to block the next idle
    // stretch, which the person's prompt's turn starts afresh.
    let next = due + 20 * MINUTE;
    big_turn_ending(&mut state, next, "1h");
    assert_eq!(ClaudeSdk::wake_at(&state), Some(next + 55 * MINUTE));
}

/// A stop during an idle compaction cancels it, and is no stop of the
/// person's: the next turn does not end interrupted.
#[test]
fn a_stop_cancels_an_idle_compaction_and_no_turn_of_anybodys() {
    let mut state = started();
    big_turn_ending(&mut state, 1_000, "1h");
    let due = 1_000 + 55 * MINUTE;
    let uuid = compact_sent(&ClaudeSdk::step(&mut state, Event::Tick { at_ms: due })).unwrap();
    let stopped = ClaudeSdk::step(
        &mut state,
        sdk_input(
            b"s1",
            wire::claude_sdk_input::Of::Interrupt(Default::default()),
        ),
    );
    let written: Vec<String> = stopped
        .effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::ProviderWrite(bytes) => Some(String::from_utf8_lossy(bytes).into_owned()),
            _ => None,
        })
        .collect();
    assert!(
        written.iter().any(|line| line.contains("interrupt")),
        "{written:?}"
    );
    ClaudeSdk::step(
        &mut state,
        stream(
            json!({"type": "result", "subtype": "error_during_execution", "uuid": "r2",
            "duration_ms": 5, "terminal_reason": "aborted_streaming", "user_message_uuid": uuid}),
        ),
    );
    assert!(!state.shared().is_housekeeping());
    assert_eq!(
        big_turn_ending(&mut state, due + MINUTE, "1h"),
        wire::TurnOutcome::Completed as i32
    );
}

/// The cache lives an hour from the last request, not from the turn's
/// end: a turn that waited long on a tool after its last request leaves
/// less of the hour, and none once it has passed.
#[test]
fn the_cache_hour_counts_from_the_last_request() {
    let mut state = started();
    big_turn_ending(&mut state, 1_000, "1h");
    assert_eq!(ClaudeSdk::wake_at(&state), Some(1_000 + 55 * MINUTE));

    let mut state = started();
    ClaudeSdk::step(&mut state, Event::Tick { at_ms: 1_000 });
    ClaudeSdk::step(
        &mut state,
        stream(
            json!({"type": "stream_event", "event": {"type": "message_start", "message": {
                "id": "msg_1",
                "usage": {"input_tokens": 2, "cache_read_input_tokens": 240_000,
                    "cache_creation_input_tokens": 10_000,
                    "cache_creation": {"ephemeral_1h_input_tokens": 10_000}},
            }}}),
        ),
    );
    ClaudeSdk::step(
        &mut state,
        Event::Tick {
            at_ms: 1_000 + 70 * MINUTE,
        },
    );
    ClaudeSdk::step(
        &mut state,
        stream(json!({"type": "result", "subtype": "success", "uuid": "r1", "duration_ms": 10})),
    );
    assert_eq!(
        ClaudeSdk::wake_at(&state),
        None,
        "the hour ran out during the turn"
    );
}

/// Claude keeps one task list for a host and its subagents: a task a
/// subagent creates is the host's too, so it shows in the agent's own
/// task list, though the call, like the agent's own task calls, draws no
/// row.
#[test]
fn a_subagents_task_is_in_the_sessions_task_list() {
    let mut state = idle_on_root();
    ClaudeSdk::step(
        &mut state,
        stream(json!({
            "type": "system", "subtype": "task_started", "uuid": "t1", "session_id": "s",
            "task_id": "a1", "task_type": "local_agent", "tool_use_id": "toolu_agent",
            "description": "plan", "is_backgrounded": true,
        })),
    );
    ClaudeSdk::step(
        &mut state,
        subagent_assistant(
            "sa1",
            json!([{"type": "tool_use", "id": "toolu_task", "name": "TaskCreate",
                "input": {"subject": "probe from subagent", "description": "x", "activeForm": "Probing"}}]),
        ),
    );
    let created = ClaudeSdk::step(
        &mut state,
        subagent(json!({
            "type": "user", "uuid": "su1",
            "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "toolu_task",
                "content": "Task #1 created successfully: probe from subagent"}]},
            "tool_use_result": {"task": {"id": "1", "subject": "probe from subagent"}},
        })),
    );
    let (_, host) = sdk_snapshot(&created).expect("the agent's snapshot");
    let entries: Vec<(String, String)> = host
        .tasks
        .iter()
        .flat_map(|list| &list.entries)
        .map(|entry| (entry.id.clone(), entry.subject.clone()))
        .collect();
    assert_eq!(
        entries,
        [("1".to_owned(), "probe from subagent".to_owned())],
        "the subagent's task is not in the session's task list"
    );
    assert!(
        created
            .step
            .agents
            .iter()
            .flat_map(|part| &part.items)
            .all(|item| item.key != "toolu_task"),
        "a task call draws a row"
    );
}
