//! The Claude SDK interpreter against the recorded headless sessions in
//! claude-specs and authored facts for what no recording shows.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use interpret::claude_sdk::{CORRELATION, ClaudeSdk, Correlation, State, client_uuid};
use interpret::{Channel, Effect, Event, Fact, Interpreter, run_golden};
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
    lifecycle_events_write_boundaries_and_interrupt => "lifecycle",
    permission_asks_keep_scopes_reason_and_description => "asks",
    questions_and_plan_verdicts_are_asks => "verdicts",
    plans_are_items_with_their_verdict => "plans",
    a_stopped_question_is_dismissed => "question_stopped",
    questions_are_skipped_noted_or_replied_to => "question_answers",
    usage_health_and_sign_in_are_strip_fields => "strip",
    a_usage_windows_stated_state_ends_when_it_resets => "usage_reset",
    refusal_fallback_is_a_model_switch_row => "rows",
    inputs_become_stream_json_writes => "inputs",
    send_now_steers_a_queued_prompt_into_the_running_turn => "steering",
    recorded_cleared => "recorded_cleared",
    recorded_compacted => "recorded_compacted",
    recorded_configured_turn => "recorded_configured_turn",
    recorded_connected_mcp_servers => "recorded_connected_mcp_servers",
    recorded_controls => "recorded_controls",
    recorded_effortful_turn => "recorded_effortful_turn",
    recorded_elicitation_accepted => "recorded_elicitation_accepted",
    recorded_every_hook_event => "recorded_every_hook_event",
    recorded_forked => "recorded_forked",
    recorded_hook_lifecycle => "recorded_hook_lifecycle",
    recorded_in_process_mcp => "recorded_in_process_mcp",
    recorded_interrupted => "recorded_interrupted",
    recorded_introspection => "recorded_introspection",
    recorded_max_budget => "recorded_max_budget",
    recorded_max_turns => "recorded_max_turns",
    recorded_multi_turn => "recorded_multi_turn",
    recorded_permission_callback => "recorded_permission_callback",
    recorded_plan_reviewed => "recorded_plan_reviewed",
    recorded_question_asked => "recorded_question_asked",
    recorded_resumed => "recorded_resumed",
    recorded_resumed_at => "recorded_resumed_at",
    recorded_session_maintenance => "recorded_session_maintenance",
    recorded_streamed_turn => "recorded_streamed_turn",
    recorded_subagent_task => "recorded_subagent_task",
    recorded_text_turn => "recorded_text_turn",
    recorded_background_shell => "recorded_background_shell",
    recorded_client_id => "recorded_client_id",
    recorded_elicitation_declined => "recorded_elicitation_declined",
    recorded_elicitation_link_refused => "recorded_elicitation_link_refused",
    recorded_failed_tool_server => "recorded_failed_tool_server",
    recorded_failing_command => "recorded_failing_command",
    recorded_image => "recorded_image",
    recorded_question_dismissed => "recorded_question_dismissed",
    recorded_question_every_shape => "recorded_question_every_shape",
    recorded_side_channel => "recorded_side_channel",
    recorded_sign_in_problem => "recorded_sign_in_problem",
    recorded_task_list => "recorded_task_list",
    recorded_elicitation_cancelled => "recorded_elicitation_cancelled",
    recorded_steer_folded => "recorded_steer_folded",
    recorded_steer_preempted => "recorded_steer_preempted",
    recorded_live_plan => "recorded_live_plan",
    recorded_live_questions => "recorded_live_questions",
    recorded_live_usage => "recorded_live_usage",
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

fn started() -> State {
    let spec = wire::AgentSpec {
        agent_id: b"agent".to_vec(),
        incarnation: 1,
        ..Default::default()
    };
    let (mut state, _) = ClaudeSdk::initial(&spec, "test");
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
        text: text.to_owned(),
        ..Default::default()
    })
}

/// The SDK probe found the uuid on a stdin message echoed on its replay and
/// lifecycle frames, and found messages written during a turn folding into
/// it: reflections are matched by that id, not by arrival order.
#[test]
fn probe_decided_sdk_correlation() {
    assert_eq!(CORRELATION, Correlation::ClientId);
    let mut state = started();
    let mut uuids = Vec::new();
    for (envelope, text) in [(b"env-1", "first"), (b"env-2", "second")] {
        let stepped = ClaudeSdk::step(&mut state, agent_message(envelope, text));
        assert!(stepped.effects.iter().any(|effect| matches!(
            effect,
            Effect::Inject { envelope: sent, .. } if sent.id == envelope
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
    assert_eq!(first.value, "default");
    assert_eq!(first.display_name, "Default (recommended)");
    assert!(first.description.starts_with("Opus 5 with 1M context"));
    assert_eq!(first.efforts, ["low", "medium", "high", "xhigh", "max"]);
    let haiku = snapshot
        .models
        .iter()
        .find(|model| model.value == "haiku")
        .unwrap();
    assert!(haiku.efforts.is_empty(), "Haiku takes no effort");
    let plugin = snapshot
        .commands
        .iter()
        .find(|command| command.name == "stripe:test-cards")
        .unwrap();
    assert_eq!(plugin.source, "stripe");
    let own = snapshot
        .commands
        .iter()
        .find(|command| command.name == "compact")
        .unwrap();
    assert_eq!(own.source, "");
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
    let (mut state, _) = ClaudeSdk::initial(&spec, "test");
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
    assert_eq!(snapshot.queue.len(), 1);

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
            let body = wire::ClaudeSdkSnapshot::decode(&snapshot.body[..]).unwrap();
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
    let (_, step) = ClaudeSdk::reincarnate(listed, &next, "test");
    assert_eq!(
        (jobs(&exited.step), jobs(&step)),
        (Some((true, 0)), Some((true, 0))),
        "emptied on exit, and empty in a new incarnation"
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
    let (mut state, _) = ClaudeSdk::initial(&spec, "test");
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
    use wire::claude_sdk_item::Kind;

    let replayed =
        interpret::replay::<ClaudeSdk>(&fixtures().join("recorded_compacted.json")).unwrap();
    let kinds: Vec<(String, Kind)> = replayed
        .iter()
        .flat_map(|frame| &frame.step.items)
        .filter_map(|item| {
            let kind = wire::ClaudeSdkItem::decode(item.body.as_slice())
                .ok()?
                .kind?;
            matches!(kind, Kind::Status(_) | Kind::Compaction(_)).then(|| (item.key.clone(), kind))
        })
        .collect();
    assert_eq!(
        kinds,
        [
            (
                "00d461b3-c569-43f4-80ff-33ac9a87bfaf".to_owned(),
                Kind::Status(wire::Status {
                    status: wire::SdkStatus::Compacting as i32,
                    word: "compacting".into(),
                }),
            ),
            (
                "115f463b-1a4e-4a07-bf00-88e995d814ac".to_owned(),
                Kind::Compaction(wire::Compaction {
                    tokens_before: Some(29_543),
                    tokens_after: Some(2_963),
                    automatic: false,
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
        kind: wire::EnvelopeKind::Failed as i32,
        ended: Some(wire::IncarnationEnd {
            cause: wire::ExitCause::ProviderExited as i32,
            detail: Some("exit code 1".into()),
        }),
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
        Effect::Inject { envelope, .. } => Some(envelope.text.clone()),
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
                kind: wire::EnvelopeKind::Message as i32,
                text: "Pick it up again.".into(),
                ..Default::default()
            })),
        }),
        ..Default::default()
    };
    let (mut state, _) = ClaudeSdk::initial(&spec, "test");
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
        .filter_map(|item| wire::ClaudeSdkItem::decode(item.body.as_slice()).ok())
        .filter_map(|body| match body.kind {
            Some(wire::claude_sdk_item::Kind::Slash(slash)) if slash.command.is_empty() => {
                Some(slash)
            }
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
        wire::ClaudeSdkSnapshot::decode(&snapshot.body[..])
            .unwrap()
            .background_jobs
            .unwrap_or_default()
            .jobs
            .into_iter()
            .map(|job| format!("{job:?}"))
            .collect()
    })
}

/// A subagent that starts in the foreground and is then sent to the
/// background: Claude says so only on `task_updated`, as a patch with
/// `is_backgrounded`. It is a background job from then on, as one started
/// in the background is.
#[test]
fn a_task_backgrounded_after_it_started_is_a_background_job() {
    let mut state = started();
    let mut last = None;
    let shown = ClaudeSdk::step(
        &mut state,
        stream(
            json!({"type": "assistant", "uuid": "a1", "session_id": "s", "message": {
            "id": "msg_1", "role": "assistant", "model": "claude-haiku-5-5",
            "content": [{"type": "tool_use", "id": "toolu_agent", "name": "Agent",
                "input": {"description": "Watch the build", "prompt": "Watch it.", "subagent_type": "general-purpose"}}]}}),
        ),
    );
    last = listed_jobs(&shown).or(last);
    let launched = ClaudeSdk::step(
        &mut state,
        stream(
            json!({"type": "system", "subtype": "task_started", "uuid": "t1", "session_id": "s",
            "task_id": "task_a", "tool_use_id": "toolu_agent", "description": "Watch the build",
            "task_type": "local_agent", "is_backgrounded": false}),
        ),
    );
    last = listed_jobs(&launched).or(last);
    let backgrounded = ClaudeSdk::step(
        &mut state,
        stream(
            json!({"type": "system", "subtype": "task_updated", "uuid": "t2", "session_id": "s",
            "task_id": "task_a", "patch": {"is_backgrounded": true}}),
        ),
    );
    let jobs = listed_jobs(&backgrounded).or(last);
    assert!(
        jobs.as_ref().is_some_and(|jobs| jobs.len() == 1),
        "the subagent sent to the background is not a background job: {jobs:?}"
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
            wire::ClaudeSdkItem::decode(item.body.as_slice())
                .ok()
                .map(|body| (item, body))
        })
        .filter(|(_, body)| matches!(body.kind, Some(wire::claude_sdk_item::Kind::Message(_))))
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
            wire::ClaudeSdkItem::decode(item.body.as_slice())
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
            |item| match wire::ClaudeSdkItem::decode(item.body.as_slice()).ok()?.kind {
                Some(wire::claude_sdk_item::Kind::Tool(tool)) => Some(tool),
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
