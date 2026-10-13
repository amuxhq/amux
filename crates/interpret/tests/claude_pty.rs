//! The Claude PTY interpreter against the recorded terminal sessions in
//! claude-specs and authored facts for what no recording shows.

use std::path::{Path, PathBuf};

use interpret::claude_pty::ClaudePty;
use interpret::{Effect, Event, Interpreter, claude_pty_input, run_golden};

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
    let (mut state, _) = ClaudePty::initial(&wire::AgentSpec::default(), "test");
    let stepped = ClaudePty::step(&mut state, failed_report(b"env-failed"));
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

// --- plan approval choices: Yes, Yes and switch to a named permission, Keep
// planning. The switch's permission is the one Claude's own plan menu moves
// to: auto mode where that menu offers it, accept edits where it does not.

/// The plan ask the interpreter opens on a recorded plan approval, with the
/// launch's Claude version and what the host's Claude offers changed.
fn plan_ask_on(version: &str, offered: Option<wire::Catalogue>) -> wire::PlanAsk {
    plan_ask_launched(version, |spec| spec.offered = offered)
}

/// The plan ask the interpreter opens on a recorded plan approval, with the
/// launch's Claude version and its spec changed by `launched`.
fn plan_ask_launched(version: &str, launched: impl FnOnce(&mut wire::AgentSpec)) -> wire::PlanAsk {
    use prost::Message as _;
    let mut script =
        interpret::fixture_script::<ClaudePty>(&fixtures().join("recorded_plan_auto.json"))
            .unwrap();
    launched(&mut script.spec);
    let (mut state, _) = ClaudePty::initial(&script.spec, &script.producer);
    for (_, event) in script.events {
        let Some(mut event) = event else { continue };
        if let Event::Fact(fact) = &mut event
            && fact.channel == interpret::Channel::Agent
        {
            let text = String::from_utf8(fact.payload.clone()).unwrap();
            fact.payload = text.replace("2.1.251", version).into_bytes();
        }
        let stepped = ClaudePty::step(&mut state, event);
        let plan = stepped.step.snapshot.and_then(|snapshot| {
            wire::ClaudePtySnapshot::decode(snapshot.body.as_slice())
                .unwrap()
                .asks
                .into_iter()
                .find_map(|ask| match ask.body {
                    Some(wire::ask::Body::Plan(plan)) => Some(plan),
                    _ => None,
                })
        });
        if let Some(plan) = plan {
            return plan;
        }
    }
    panic!("the recording opens no plan ask");
}

fn choices(plan: &wire::PlanAsk) -> Vec<wire::PlanChoice> {
    plan.choices
        .iter()
        .map(|choice| wire::PlanChoice::try_from(*choice).unwrap())
        .collect()
}

/// The host's Claude offers auto mode, as Claude 2.1.296 on Haiku 5.5 does.
fn offers_auto() -> wire::Catalogue {
    wire::Catalogue {
        permissions: vec![
            wire::OfferedPermission {
                value: "default".into(),
                normal: true,
                settable: true,
                ..Default::default()
            },
            wire::OfferedPermission {
                value: "acceptEdits".into(),
                settable: true,
                ..Default::default()
            },
            wire::OfferedPermission {
                value: "auto".into(),
                settable: true,
                ..Default::default()
            },
        ],
        ..Default::default()
    }
}

/// Claude 2.1.296, offering auto mode, draws its plan menu as "Yes, and
/// use auto mode / Yes, manually approve edits / Tell Claude what to
/// change": no entry accepts edits. The ask offers Yes, Yes and switch to
/// auto mode, and Keep planning, and names auto as the switch's permission.
#[test]
fn a_plan_ask_where_claude_offers_auto_mode_switches_to_auto() {
    let plan = plan_ask_on("2.1.296", Some(offers_auto()));
    println!("plan ask: {plan:?} {:?}", choices(&plan));
    assert!(
        !(choices(&plan).contains(&wire::PlanChoice::StartSwitching)
            && plan.switch_permission == "acceptEdits"),
        "the ask offers to start accepting edits, which this Claude's plan menu does not have; \
         its switch moves to auto mode: {:?}",
        choices(&plan)
    );
    assert_eq!(
        choices(&plan).len(),
        3,
        "Yes, Yes and switch, Keep planning"
    );
    assert!(
        format!("{plan:?}").contains("\"auto\""),
        "the ask names auto as the permission its switch moves to: {plan:?}"
    );
}

/// Claude 2.1.251, with no auto mode, draws "Yes, auto-accept edits / Yes,
/// manually approve edits / No, keep planning". The ask offers Yes, Yes and
/// switch to accept edits, and Keep planning, and names acceptEdits as the
/// switch's permission, so a client words it from the permission rather
/// than from a fixed choice.
#[test]
fn a_plan_ask_where_claude_offers_no_auto_mode_switches_to_accept_edits() {
    let plan = plan_ask_on("2.1.251", None);
    println!("plan ask: {plan:?} {:?}", choices(&plan));
    assert_eq!(
        choices(&plan).len(),
        3,
        "Yes, Yes and switch, Keep planning"
    );
    assert_eq!(choices(&plan)[0], wire::PlanChoice::Start);
    assert_eq!(choices(&plan)[2], wire::PlanChoice::KeepPlanning);
    assert!(
        format!("{plan:?}").contains("\"acceptEdits\""),
        "the ask names acceptEdits as the permission its switch moves to: {plan:?}"
    );
}

/// Claude 2.1.296 started with bypass permissions draws its plan menu with
/// "Yes, and switch to BYPASS PERMISSIONS" first, as QA recorded on a real
/// agent. Nothing reads that menu off the screen: the switch's target
/// follows from the launch flags, so the ask names bypassPermissions even
/// where the host's Claude offers auto mode.
#[test]
fn a_plan_ask_on_claude_launched_with_bypass_permissions_switches_to_bypass() {
    let plan = plan_ask_launched("2.1.296", |spec| {
        spec.offered = Some(offers_auto());
        spec.provider_args = vec!["--permission-mode".into(), "bypassPermissions".into()];
    });
    println!("plan ask: {plan:?} {:?}", choices(&plan));
    assert_eq!(
        plan.switch_permission, "bypassPermissions",
        "the ask's switch is not the one Claude launched with bypass permissions draws"
    );
}

/// A child resumed by its parent's message types it as its first prompt,
/// labelled with its sender as a message handed over live is, and its
/// transcript's reflection of the labelled text is a row of the words.
#[test]
fn a_parents_message_that_resumes_its_child_reaches_terminal_claude_labelled() {
    use interpret::claude_pty::TerminalInput;
    use interpret::{Channel, Fact};
    use serde_json::json;
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
    let labelled = "[message from agent lead]\nPick it up again.";
    let (mut state, _) = ClaudePty::initial(&spec, "test");
    let stepped = ClaudePty::step(
        &mut state,
        Event::Fact(Fact {
            channel: Channel::Agent,
            payload: interpret::claude_pty::ready_fact(),
        }),
    );
    let typed = stepped.effects.iter().find_map(|effect| match effect {
        Effect::Terminal(TerminalInput::Prompt { text, .. }) => Some(text.clone()),
        _ => None,
    });
    assert_eq!(typed.as_deref(), Some(labelled));
    let reflected = ClaudePty::step(
        &mut state,
        Event::Fact(Fact {
            channel: Channel::Transcript,
            payload: json!({
                "type": "user", "uuid": "u1", "timestamp": "2027-01-15T08:00:00.010Z",
                "sessionId": "s1", "origin": {"kind": "human"}, "promptSource": "typed",
                "message": {"role": "user", "content": labelled},
            })
            .to_string()
            .into_bytes(),
        }),
    );
    let item = reflected
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

/// Every item terminal Claude's transcript rows give, in order.
fn items_from_rows(rows: &[serde_json::Value]) -> Vec<wire::Item> {
    let spec = wire::AgentSpec {
        agent_id: b"agent".to_vec(),
        ..Default::default()
    };
    let (mut state, _) = ClaudePty::initial(&spec, "test");
    let text: String = rows.iter().map(|row| format!("{row}\n")).collect();
    let mut items = Vec::new();
    for event in ClaudePty::recording("transcript_rows", text.as_bytes()).unwrap() {
        items.extend(ClaudePty::step(&mut state, event).step.items);
    }
    items
}

/// A `!command` typed in terminal Claude's own view: Claude writes the
/// command and its output as user rows wrapped in its own markup, as QA
/// recorded on Claude 2.1.296. They are a shell step, not two prompts from
/// the person showing the markup.
#[test]
fn a_bang_command_is_a_shell_step_not_prompts_carrying_claudes_markup() {
    let row = |uuid: &str, content: &str| {
        serde_json::json!({
            "type": "user", "uuid": uuid, "timestamp": "2026-10-12T07:40:59.399Z",
            "sessionId": "s1", "isSidechain": false, "userType": "external",
            "message": {"role": "user", "content": content},
        })
    };
    let items = items_from_rows(&[
        row("u1", "<bash-input>echo bang-ran-123</bash-input>"),
        row(
            "u2",
            "<bash-stdout>bang-ran-123</bash-stdout><bash-stderr></bash-stderr>",
        ),
    ]);
    let markup: Vec<&str> = items
        .iter()
        .map(|item| item.text.as_str())
        .filter(|text| text.contains("<bash-"))
        .collect();
    assert!(
        markup.is_empty(),
        "Claude's local-command markup is shown as the person's words: {markup:?}"
    );
}

/// A slash command's output is written with terminal colour codes
/// (`/compact`, `/context`); the record keeps the words, not the codes.
#[test]
fn slash_command_output_keeps_no_terminal_colour_codes() {
    let items = items_from_rows(&[
        serde_json::json!({
            "type": "user", "uuid": "u0", "timestamp": "2026-10-12T02:32:39.946Z",
            "sessionId": "s1", "isSidechain": false, "userType": "external",
            "message": {"role": "user", "content": "<command-name>/compact</command-name>\n            <command-message>compact</command-message>\n            <command-args></command-args>"},
        }),
        serde_json::json!({
            "type": "user", "uuid": "u1", "parentUuid": "u0", "timestamp": "2026-10-12T02:32:41.386Z",
            "sessionId": "s1", "isSidechain": false, "userType": "external",
            "message": {"role": "user", "content": "<local-command-stdout>\u{1b}[2mCompacted (ctrl+o to see full summary)\u{1b}[22m</local-command-stdout>"},
        }),
    ]);
    let coded: Vec<&str> = items
        .iter()
        .map(|item| item.text.as_str())
        .filter(|text| text.contains('\u{1b}') || text.contains("[22m"))
        .collect();
    assert!(
        !items.is_empty() && coded.is_empty(),
        "the slash output carries colour codes: {coded:?} (items: {:?})",
        items.iter().map(|item| &item.text).collect::<Vec<_>>()
    );
}

/// A prompt sent with attachments is typed into terminal Claude with each
/// attachment as its element, and Claude's transcript row carries the
/// elements back. The prompt's item holds the attachments at their places,
/// as a reply's does, not the elements' raw text with the host's blob paths.
#[test]
fn a_prompt_typed_with_attachments_holds_them_not_their_elements() {
    let attachment = wire::Attachment {
        of: Some(wire::attachment::Of::File(wire::BlobRef {
            hash: vec![0xb8; 32],
            name: "a.txt".into(),
            mime: "text/plain".into(),
            size: 100,
        })),
    };
    let path = Path::new("/home/me/agents/a/blobs").join("b8".repeat(32));
    let element = attachments::element(&attachment, Some(&path));
    let items = items_from_rows(&[serde_json::json!({
        "type": "user", "uuid": "u1", "timestamp": "2026-10-12T01:05:16.000Z",
        "sessionId": "s1", "origin": {"kind": "human"}, "promptSource": "typed",
        "message": {"role": "user", "content": format!("{element} Reply with only the word ok.")},
    })]);
    let prompt = items
        .iter()
        .find(|item| item.key == "u1")
        .expect("the prompt's item");
    assert!(
        !prompt.text.contains("<amux-attachment") && prompt.attachments == vec![attachment],
        "the prompt shows the raw element: {:?} with attachments {:?}",
        prompt.text,
        prompt.attachments
    );
}

/// A model chosen with `/model` in terminal Claude's own view: Claude writes
/// the command and "Set model to `Haiku 5.5` …" to its transcript at once,
/// as QA recorded on 2.1.296. The snapshot names the new model then, not
/// only once the next turn's rows carry it.
#[test]
fn a_model_set_in_claudes_own_view_shows_before_the_next_turn() {
    use prost::Message as _;
    let spec = wire::AgentSpec {
        agent_id: b"agent".to_vec(),
        ..Default::default()
    };
    let (mut state, _) = ClaudePty::initial(&spec, "test");
    let rows = [
        serde_json::json!({
            "type": "user", "uuid": "u0", "timestamp": "2026-10-12T07:43:20.000Z",
            "sessionId": "s1", "isSidechain": false, "userType": "external",
            "message": {"role": "user", "content": "<command-name>/model</command-name>\n            <command-message>model</command-message>\n            <command-args></command-args>"},
        }),
        serde_json::json!({
            "type": "user", "uuid": "u1", "parentUuid": "u0", "timestamp": "2026-10-12T07:43:23.238Z",
            "sessionId": "s1", "isSidechain": false, "userType": "external",
            "message": {"role": "user", "content": "<local-command-stdout>Set model to `Haiku 5.5` and saved as your default for new sessions</local-command-stdout>"},
        }),
    ];
    let text: String = rows.iter().map(|row| format!("{row}\n")).collect();
    let mut named = None;
    for event in ClaudePty::recording("transcript_rows", text.as_bytes()).unwrap() {
        if let Some(snapshot) = ClaudePty::step(&mut state, event).step.snapshot {
            let body = wire::ClaudePtySnapshot::decode(snapshot.body.as_slice()).unwrap();
            named = body.model_name.or(body.model).or(named);
        }
    }
    assert!(
        named.as_deref().is_some_and(|name| name.contains("Haiku")),
        "the snapshot keeps the old model after Claude said it set Haiku 5.5: {named:?}"
    );
}

/// A subagent Claude ran in the background asks permission for a command,
/// then the main agent's turn stops with that subagent still running, as QA
/// recorded on 2.1.296 (hook order: the subagent's PermissionRequest, then
/// the main Stop listing it under background_tasks). Claude's own view
/// keeps the subagent's dialog open; amux keeps the ask open too, so a
/// person can answer it, and does not call the agent idle.
#[test]
fn a_background_subagents_ask_stays_open_past_the_main_turns_stop() {
    use prost::Message as _;
    let hook = |json: serde_json::Value| {
        Event::Fact(interpret::Fact {
            channel: interpret::Channel::Hook,
            payload: json.to_string().into_bytes(),
        })
    };
    let common = serde_json::json!({
        "session_id": "s1", "transcript_path": "/claude/projects/w/s1.jsonl", "cwd": "/w",
        "prompt_id": "p1", "permission_mode": "acceptEdits",
    });
    let with = |extra: serde_json::Value| {
        let mut value = common.clone();
        value
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        hook(value)
    };
    let spec = wire::AgentSpec {
        agent_id: b"agent".to_vec(),
        ..Default::default()
    };
    let (mut state, _) = ClaudePty::initial(&spec, "test");
    let command = serde_json::json!({"command": "python3 -c 'import time; time.sleep(60)'", "description": "Sleep"});
    let mut last = None;
    let mut seen = Vec::new();
    for event in [
        with(
            serde_json::json!({"hook_event_name": "SessionStart", "source": "startup", "model": "claude-haiku-5-5"}),
        ),
        with(
            serde_json::json!({"hook_event_name": "UserPromptSubmit", "prompt": "Use a subagent to sleep."}),
        ),
        with(
            serde_json::json!({"hook_event_name": "PreToolUse", "tool_name": "Agent", "tool_use_id": "toolu_agent",
            "tool_input": {"description": "Run a 60-second sleep", "prompt": "Sleep.", "subagent_type": "general-purpose"}}),
        ),
        with(
            serde_json::json!({"hook_event_name": "PostToolUse", "tool_name": "Agent", "tool_use_id": "toolu_agent",
            "tool_input": {"description": "Run a 60-second sleep", "prompt": "Sleep.", "subagent_type": "general-purpose"},
            "tool_response": {"isAsync": true, "status": "async_launched", "agentId": "a7aebc"}}),
        ),
        with(
            serde_json::json!({"hook_event_name": "PreToolUse", "agent_id": "a7aebc", "agent_type": "general-purpose",
            "tool_name": "Bash", "tool_input": command, "tool_use_id": "toolu_bash"}),
        ),
        with(
            serde_json::json!({"hook_event_name": "PermissionRequest", "agent_id": "a7aebc", "agent_type": "general-purpose",
            "tool_name": "Bash", "tool_input": command,
            "permission_suggestions": [{"type": "addRules", "rules": [{"toolName": "Bash", "ruleContent": "python3 -c 'import time; time.sleep(60)'"}], "behavior": "allow", "destination": "localSettings"}]}),
        ),
        with(
            serde_json::json!({"hook_event_name": "Stop", "stop_hook_active": false,
            "last_assistant_message": "I've started a subagent. It's still running.",
            "background_tasks": [{"id": "a7aebc", "type": "subagent", "status": "running", "description": "Run a 60-second sleep", "agent_type": "general-purpose"}]}),
        ),
    ] {
        if let Some(snapshot) = ClaudePty::step(&mut state, event).step.snapshot {
            let body = wire::ClaudePtySnapshot::decode(snapshot.body.as_slice()).unwrap();
            last = Some((snapshot.phase(), body.asks.len()));
            seen.push(last.unwrap());
        }
    }
    let (phase, asks) = last.expect("a snapshot");
    assert!(
        asks > 0 && phase != wire::Phase::Idle,
        "the subagent's ask is not open once the main turn stops: phase {phase:?}, {asks} asks; every snapshot (phase, asks): {seen:?}"
    );
}

// --- terminal Claude's turn, permission, effort and conversation follow its
// own records: the transcript rows and the hooks, never its screen.

fn hook(json: serde_json::Value) -> Event {
    Event::Fact(interpret::Fact {
        channel: interpret::Channel::Hook,
        payload: json.to_string().into_bytes(),
    })
}

fn transcript_row(json: serde_json::Value) -> Event {
    Event::Fact(interpret::Fact {
        channel: interpret::Channel::Transcript,
        payload: json.to_string().into_bytes(),
    })
}

fn prompt_input(id: &str, text: &str) -> Event {
    let input = ClaudePty::fixture_input(
        id.as_bytes().to_vec(),
        &interpret::FixtureInput::Prompt { text: text.into() },
    )
    .expect("a terminal prompt");
    Event::Input(input)
}

fn decoded(stepped: &interpret::Stepped) -> Option<(wire::Phase, wire::ClaudePtySnapshot)> {
    use prost::Message as _;
    stepped.step.snapshot.as_ref().map(|snapshot| {
        (
            snapshot.phase(),
            wire::ClaudePtySnapshot::decode(snapshot.body.as_slice()).unwrap(),
        )
    })
}

/// Terminal Claude on 2.1.296, launched by amux, its terminal live and its
/// session s1 started: the state and the last snapshot it gave.
fn started_claude() -> (
    interpret::claude_pty::State,
    (wire::Phase, wire::ClaudePtySnapshot),
) {
    let spec = wire::AgentSpec {
        agent_id: b"agent".to_vec(),
        incarnation: 1,
        ..Default::default()
    };
    let (mut state, step) = ClaudePty::initial(&spec, "test");
    let mut last = step.snapshot.as_ref().map(|snapshot| {
        use prost::Message as _;
        (
            snapshot.phase(),
            wire::ClaudePtySnapshot::decode(snapshot.body.as_slice()).unwrap(),
        )
    });
    let launch = serde_json::json!({
        "type": "launch", "version": "2.1.296", "keymap": "claude-2.1",
        "permission_menus": {"per_suggestion": [1], "folded": [2]},
    });
    for event in [
        Event::Fact(interpret::Fact {
            channel: interpret::Channel::Agent,
            payload: launch.to_string().into_bytes(),
        }),
        Event::Fact(interpret::Fact {
            channel: interpret::Channel::Agent,
            payload: interpret::claude_pty::ready_fact(),
        }),
        hook(serde_json::json!({
            "hook_event_name": "SessionStart", "source": "startup", "session_id": "s1",
            "transcript_path": "/claude/projects/w/s1.jsonl", "cwd": "/w",
            "model": "claude-opus-5-5",
        })),
    ] {
        last = decoded(&ClaudePty::step(&mut state, event)).or(last);
    }
    (state, last.expect("a snapshot"))
}

/// The user-prompt row Claude writes when it takes a typed prompt.
fn typed_row(uuid: &str, text: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "user", "uuid": uuid, "timestamp": "2026-10-12T08:00:00.000Z",
        "sessionId": "s1", "isSidechain": false, "userType": "external",
        "origin": {"kind": "human"}, "promptSource": "typed",
        "message": {"role": "user", "content": text},
    })
}

/// amux types a prompt into terminal Claude; the turn starts when Claude's
/// transcript records the prompt, not when the keys are written. A prompt
/// typed into one of Claude's own screens is never recorded, and the agent
/// must not read as working on it.
#[test]
fn a_typed_prompt_starts_the_turn_on_its_transcript_row_not_on_the_keys() {
    let (mut state, (idle, _)) = started_claude();
    let typed = ClaudePty::step(&mut state, prompt_input("p1", "hello"));
    assert!(
        typed.effects.iter().any(|effect| matches!(
            effect,
            Effect::Terminal(interpret::claude_pty::TerminalInput::Prompt { .. })
        )),
        "the prompt is typed: {:?}",
        typed.effects
    );
    let after_keys = decoded(&typed).map_or(idle, |(phase, _)| phase);
    let recorded = ClaudePty::step(&mut state, transcript_row(typed_row("u1", "hello")));
    let after_row = decoded(&recorded).map_or(after_keys, |(phase, _)| phase);
    assert_eq!(
        (after_keys, after_row),
        (wire::Phase::Idle, wire::Phase::Working),
        "the turn starts when the keys are written, before Claude records the prompt"
    );
}

/// amux cycles terminal Claude's permission from default: it knows the
/// cycle order, so the snapshot names accept edits at once, without reading
/// Claude's footer. Claude's own record of the mode comes only with the next
/// prompt.
#[test]
fn a_permission_amux_cycles_is_in_the_snapshot_at_once() {
    let (mut state, (_, before)) = started_claude();
    let mut last = before;
    for event in [
        transcript_row(serde_json::json!({
            "type": "permission-mode", "permissionMode": "default", "sessionId": "s1",
        })),
        Event::Input(wire::Input::new(
            b"k1".to_vec(),
            wire::ClaudePtyInput {
                of: Some(wire::claude_pty_input::Of::CyclePermissionMode(
                    wire::CyclePermissionMode {},
                )),
            },
        )),
    ] {
        if let Some((_, snapshot)) = decoded(&ClaudePty::step(&mut state, event)) {
            last = snapshot;
        }
    }
    assert_eq!(
        last.permission.as_deref(),
        Some("acceptEdits"),
        "the snapshot does not name the permission amux moved Claude to"
    );
}

/// Claude's assistant rows carry the turn's effort (`"effort": "high"`) and
/// the model asked for: the snapshot names the effort from them.
#[test]
fn the_effort_comes_from_claudes_assistant_rows() {
    let (mut state, (_, before)) = started_claude();
    let mut last = before;
    for event in [
        transcript_row(typed_row("u1", "hello")),
        transcript_row(serde_json::json!({
            "type": "assistant", "uuid": "a1", "parentUuid": "u1",
            "timestamp": "2026-10-12T08:00:01.000Z", "sessionId": "s1",
            "isSidechain": false, "userType": "external",
            "effort": "high", "requestedModel": "claude-opus-5-5",
            "message": {
                "id": "msg_1", "type": "message", "role": "assistant",
                "model": "claude-opus-5-5",
                "content": [{"type": "text", "text": "hi"}],
                "stop_reason": null,
                "usage": {"input_tokens": 1, "output_tokens": 1},
            },
        })),
    ] {
        if let Some((_, snapshot)) = decoded(&ClaudePty::step(&mut state, event)) {
            last = snapshot;
        }
    }
    assert!(
        format!("{last:?}").contains("\"high\""),
        "the snapshot does not name the effort Claude recorded: {last:?}"
    );
}

/// `/resume` typed in Claude's own view moves it to another conversation:
/// SessionStart says `resume` with that session's id. That conversation is
/// not this agent's, so a prompt through amux is refused rather than typed
/// into it.
#[test]
fn a_prompt_is_refused_while_claude_shows_a_conversation_that_is_not_this_agents() {
    let (mut state, _) = started_claude();
    ClaudePty::step(
        &mut state,
        hook(serde_json::json!({
            "hook_event_name": "SessionStart", "source": "resume", "session_id": "other",
            "prompt_id": "p9", "transcript_path": "/claude/projects/w/other.jsonl",
            "cwd": "/w", "model": "claude-opus-5-5",
        })),
    );
    let stepped = ClaudePty::step(&mut state, prompt_input("p1", "hello"));
    let typed = stepped.effects.iter().any(|effect| {
        matches!(
            effect,
            Effect::Terminal(interpret::claude_pty::TerminalInput::Prompt { .. })
        )
    });
    let replies: Vec<String> = stepped
        .effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Reply { verdict, .. } => Some(format!("{verdict:?}")),
            _ => None,
        })
        .collect();
    assert!(
        !typed && replies.iter().any(|reply| reply.contains("Rejected")),
        "the prompt went into a conversation that is not this agent's: typed {typed}, replies {replies:?}"
    );
}

/// A subagent Claude ran in the background is listed at the main turn's
/// Stop; when it finishes, Claude's SubagentStop hook names it, and the
/// background list drops it then rather than at some later turn's end.
#[test]
fn a_finished_background_subagent_leaves_the_background_list() {
    let (mut state, _) = started_claude();
    let common = |extra: serde_json::Value| {
        let mut value = serde_json::json!({
            "session_id": "s1", "transcript_path": "/claude/projects/w/s1.jsonl",
            "cwd": "/w", "prompt_id": "p1", "permission_mode": "default",
        });
        value
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        hook(value)
    };
    let mut listed = Vec::new();
    for event in [
        common(
            serde_json::json!({"hook_event_name": "Stop", "stop_hook_active": false,
            "last_assistant_message": "Started a subagent.",
            "background_tasks": [{"id": "a7aebc", "type": "subagent", "status": "running",
                "description": "Run a 60-second sleep", "agent_type": "general-purpose"}]}),
        ),
        common(
            serde_json::json!({"hook_event_name": "SubagentStop", "stop_hook_active": false,
            "agent_id": "a7aebc", "agent_type": "general-purpose",
            "agent_transcript_path": "/claude/projects/w/s1/subagents/agent-a7aebc.jsonl",
            "last_assistant_message": "Slept."}),
        ),
    ] {
        if let Some((_, snapshot)) = decoded(&ClaudePty::step(&mut state, event)) {
            listed.push(snapshot.background_jobs.unwrap_or_default().jobs.len());
        }
    }
    assert_eq!(
        listed.first().copied(),
        Some(1),
        "the Stop hook lists the subagent: {listed:?}"
    );
    assert_eq!(
        listed.last().copied(),
        Some(0),
        "the finished subagent stays in the background list: {listed:?}"
    );
}

/// A command that waited a minute for the person's approval and then ran
/// for under a second: its time runs from when it began running, which
/// Claude's PostToolUse hook gives as its duration, not from the call's
/// row, written before the ask.
#[test]
fn a_commands_time_starts_after_the_approval_it_waited_for() {
    use prost::Message as _;
    let (mut state, _) = started_claude();
    let command =
        serde_json::json!({"command": "curl -sI https://example.com", "description": "Fetch"});
    let common = |extra: serde_json::Value| {
        let mut value = serde_json::json!({
            "session_id": "s1", "transcript_path": "/claude/projects/w/s1.jsonl",
            "cwd": "/w", "prompt_id": "p1", "permission_mode": "default",
        });
        value
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        hook(value)
    };
    // 2026-10-12T08:00:01Z, when Claude wrote the call's row.
    let called_ms = 1_791_792_001_000;
    let mut items = Vec::new();
    for event in [
        transcript_row(typed_row("u1", "fetch it")),
        common(
            serde_json::json!({"hook_event_name": "PreToolUse", "tool_name": "Bash",
            "tool_input": command, "tool_use_id": "toolu_curl"}),
        ),
        common(
            serde_json::json!({"hook_event_name": "PermissionRequest", "tool_name": "Bash",
            "tool_input": command}),
        ),
        transcript_row(serde_json::json!({
            "type": "assistant", "uuid": "a1", "parentUuid": "u1",
            "timestamp": "2026-10-12T08:00:01.000Z", "sessionId": "s1",
            "isSidechain": false, "userType": "external",
            "message": {
                "id": "msg_1", "type": "message", "role": "assistant",
                "model": "claude-haiku-5-5",
                "content": [{"type": "tool_use", "id": "toolu_curl", "name": "Bash", "input": command}],
                "stop_reason": "tool_use",
                "usage": {"input_tokens": 1, "output_tokens": 1},
            },
        })),
        Event::Tick {
            at_ms: called_ms + 60_200,
        },
        common(
            serde_json::json!({"hook_event_name": "PostToolUse", "tool_name": "Bash",
            "tool_input": command, "tool_use_id": "toolu_curl",
            "tool_response": {"stdout": "HTTP/2 200", "stderr": "", "interrupted": false},
            "duration_ms": 194}),
        ),
        transcript_row(serde_json::json!({
            "type": "user", "uuid": "u2", "parentUuid": "a1",
            "timestamp": "2026-10-12T08:01:01.200Z", "sessionId": "s1",
            "isSidechain": false, "userType": "external",
            "message": {"role": "user", "content": [
                {"tool_use_id": "toolu_curl", "type": "tool_result", "content": "HTTP/2 200", "is_error": false}]},
            "toolUseResult": {"stdout": "HTTP/2 200", "stderr": "", "interrupted": false},
        })),
    ] {
        items.extend(ClaudePty::step(&mut state, event).step.items);
    }
    let call = items
        .iter()
        .rev()
        .filter(|item| item.key == "toolu_curl")
        .find_map(
            |item| match wire::ClaudePtyItem::decode(item.body.as_slice()).ok()?.kind {
                Some(wire::claude_pty_item::Kind::Tool(tool)) => Some(tool),
                _ => None,
            },
        )
        .expect("the call's row");
    assert_eq!(
        (call.ended_at_ms, call.started_at_ms),
        (Some(called_ms + 60_200), Some(called_ms + 60_200 - 194)),
        "the command's time counts the wait for approval"
    );
}

/// Terminal Claude on a model that takes auto mode, in manual mode: the
/// catalogue names the model by alias and by the id Claude's hooks name it
/// by, as Claude 2.1.296 on Haiku 5.5 lists it.
fn claude_offering_auto() -> interpret::claude_pty::State {
    let mut catalogue = offers_auto();
    for permission in &mut catalogue.permissions {
        if permission.value == "auto" {
            permission.models = vec!["haiku".into(), "claude-haiku-5-5".into()];
        }
    }
    let spec = wire::AgentSpec {
        agent_id: b"agent".to_vec(),
        incarnation: 1,
        offered: Some(catalogue),
        ..Default::default()
    };
    let (mut state, _) = ClaudePty::initial(&spec, "test");
    let launch = serde_json::json!({
        "type": "launch", "version": "2.1.296", "keymap": "claude-2.1",
        "permission_menus": {"per_suggestion": [1], "folded": [2]},
    });
    for event in [
        Event::Fact(interpret::Fact {
            channel: interpret::Channel::Agent,
            payload: launch.to_string().into_bytes(),
        }),
        Event::Fact(interpret::Fact {
            channel: interpret::Channel::Agent,
            payload: interpret::claude_pty::ready_fact(),
        }),
        hook(serde_json::json!({
            "hook_event_name": "SessionStart", "source": "startup", "session_id": "s1",
            "transcript_path": "/claude/projects/w/s1.jsonl", "cwd": "/w",
            "model": "claude-haiku-5-5", "permission_mode": "default",
        })),
    ] {
        ClaudePty::step(&mut state, event);
    }
    state
}

/// Claude's permission menu in manual mode, where its model takes auto
/// mode, reads Yes / Yes, and always allow … / Yes, and switch to auto mode
/// / No. The ask offers the switch as a scope moving to auto; choosing it
/// types that entry, and the permission is auto at once.
#[test]
fn a_permission_ask_offers_claudes_switch_to_auto_mode() {
    let mut state = claude_offering_auto();
    let opened = ClaudePty::step(
        &mut state,
        hook(serde_json::json!({
            "hook_event_name": "PermissionRequest", "session_id": "s1",
            "transcript_path": "/claude/projects/w/s1.jsonl", "cwd": "/w",
            "permission_mode": "default", "tool_name": "Bash",
            "tool_input": {"command": "touch decided.txt"},
            "permission_suggestions": [{"type": "addRules", "rules": [{"toolName": "Bash",
                "ruleContent": "touch decided.txt"}], "behavior": "allow",
                "destination": "localSettings"}],
        })),
    );
    let (_, snapshot) = decoded(&opened).expect("a snapshot");
    let ask = snapshot.asks.first().expect("the ask").clone();
    let Some(wire::ask::Body::Permission(permission)) = &ask.body else {
        panic!("a permission ask: {ask:?}");
    };
    let modes: Vec<Option<String>> = permission
        .scopes
        .iter()
        .map(|scope| scope.grant.as_ref().and_then(|grant| grant.mode.clone()))
        .collect();
    assert_eq!(
        modes,
        [None, Some("auto".to_owned())],
        "the ask leaves out Claude's switch to auto mode: {permission:?}"
    );
    let answer = ClaudePty::fixture_input(
        b"a1".to_vec(),
        &interpret::FixtureInput::Answer {
            ask: ask.key.clone(),
            answer: serde_json::json!({"allow": {"scope": 1}}),
        },
    )
    .expect("an answer");
    let answered = ClaudePty::step(&mut state, Event::Input(answer));
    assert!(
        answered.effects.iter().any(|effect| matches!(
            effect,
            Effect::Terminal(interpret::claude_pty::TerminalInput::Permission {
                choice: interpret::claude_pty::PermissionChoice::AllowSwitchingToAuto,
                ..
            })
        )),
        "the switch to auto mode is not what is typed: {:?}",
        answered.effects
    );
    let (_, after) = decoded(&answered).expect("a snapshot");
    assert_eq!(after.permission.as_deref(), Some("auto"));
}

/// A plan approved with its switch, where Claude offers auto mode, types
/// the menu's first entry (which Claude takes to auto mode) and shows the
/// permission as auto at once: a QA
/// run saw the chat say "switched to Accept edits" while Claude ran in auto
/// mode.
#[test]
fn a_plan_approved_with_its_switch_lands_in_the_switch_it_names() {
    let mut state = claude_offering_auto();
    let opened = ClaudePty::step(
        &mut state,
        hook(serde_json::json!({
            "hook_event_name": "PermissionRequest", "session_id": "s1",
            "transcript_path": "/claude/projects/w/s1.jsonl", "cwd": "/w",
            "permission_mode": "plan", "tool_name": "ExitPlanMode",
            "tool_input": {"plan": "# Plan"}, "permission_suggestions": [],
        })),
    );
    let (_, snapshot) = decoded(&opened).expect("a snapshot");
    let ask = snapshot.asks.first().expect("the plan ask").clone();
    let Some(wire::ask::Body::Plan(plan)) = &ask.body else {
        panic!("a plan ask: {ask:?}");
    };
    assert_eq!(plan.switch_permission, "auto");
    let answer = ClaudePty::fixture_input(
        b"a1".to_vec(),
        &interpret::FixtureInput::Answer {
            ask: ask.key.clone(),
            answer: serde_json::json!({"approve_plan": {"switch_to": "auto"}}),
        },
    )
    .expect("an answer");
    let answered = ClaudePty::step(&mut state, Event::Input(answer));
    assert!(
        answered.effects.iter().any(|effect| matches!(
            effect,
            Effect::Terminal(interpret::claude_pty::TerminalInput::Plan(
                interpret::claude_pty::PlanChoice::ApproveSwitching
            ))
        )),
        "the plan's switch is not what is typed: {:?}",
        answered.effects
    );
    let (_, after) = decoded(&answered).expect("a snapshot");
    assert_eq!(
        after.permission.as_deref(),
        Some("auto"),
        "the permission shown is not the one the switch moved to"
    );
}

/// The plan's switch and Shift+Tab's cycle know auto mode the same way:
/// the plan ask switches to auto, and the cycle goes accept edits, plan,
/// auto, then back to manual, as Claude's own does.
#[test]
fn auto_mode_is_in_the_plan_switch_and_the_cycle_where_claude_offers_it() {
    let mut state = claude_offering_auto();
    let mut seen = Vec::new();
    for at in 0..4 {
        let cycled = ClaudePty::step(
            &mut state,
            Event::Input(wire::Input::new(
                format!("k{at}").into_bytes(),
                wire::ClaudePtyInput {
                    of: Some(wire::claude_pty_input::Of::CyclePermissionMode(
                        wire::CyclePermissionMode {},
                    )),
                },
            )),
        );
        let (_, snapshot) = decoded(&cycled).expect("a snapshot");
        seen.push(snapshot.permission.unwrap_or_default());
    }
    assert_eq!(seen, ["acceptEdits", "plan", "auto", "default"]);
    let opened = ClaudePty::step(
        &mut state,
        hook(serde_json::json!({
            "hook_event_name": "PermissionRequest", "session_id": "s1",
            "transcript_path": "/claude/projects/w/s1.jsonl", "cwd": "/w",
            "permission_mode": "plan", "tool_name": "ExitPlanMode",
            "tool_input": {"plan": "# Plan"}, "permission_suggestions": [],
        })),
    );
    let (_, snapshot) = decoded(&opened).expect("a snapshot");
    let plan = snapshot
        .asks
        .iter()
        .find_map(|ask| match &ask.body {
            Some(wire::ask::Body::Plan(plan)) => Some(plan.clone()),
            _ => None,
        })
        .expect("the plan ask");
    assert_eq!(plan.switch_permission, "auto");
}

/// Claude starts its session only once a prompt arrives, so before then no
/// hook names its model: the one it was launched with runs, and Shift+Tab
/// already cycles through auto mode where that model takes it.
#[test]
fn before_claude_names_its_model_the_launched_one_decides_auto_mode() {
    let mut catalogue = offers_auto();
    for permission in &mut catalogue.permissions {
        if permission.value == "auto" {
            permission.models = vec!["haiku".into(), "claude-haiku-5-5".into()];
        }
    }
    let spec = wire::AgentSpec {
        agent_id: b"agent".to_vec(),
        incarnation: 1,
        offered: Some(catalogue),
        provider_args: vec!["--model".into(), "haiku".into()],
        ..Default::default()
    };
    let (mut state, _) = ClaudePty::initial(&spec, "test");
    let launch = serde_json::json!({
        "type": "launch", "version": "2.1.296", "keymap": "claude-2.1",
        "permission_menus": {"per_suggestion": [1], "folded": [2]},
    });
    ClaudePty::step(
        &mut state,
        Event::Fact(interpret::Fact {
            channel: interpret::Channel::Agent,
            payload: launch.to_string().into_bytes(),
        }),
    );
    let mut seen = Vec::new();
    for at in 0..3 {
        let cycled = ClaudePty::step(
            &mut state,
            Event::Input(wire::Input::new(
                format!("k{at}").into_bytes(),
                wire::ClaudePtyInput {
                    of: Some(wire::claude_pty_input::Of::CyclePermissionMode(
                        wire::CyclePermissionMode {},
                    )),
                },
            )),
        );
        if let Some((_, snapshot)) = decoded(&cycled) {
            seen.push(snapshot.permission.unwrap_or_default());
        }
    }
    assert_eq!(seen, ["acceptEdits", "plan", "auto"]);
}

fn typed_prompts(stepped: &interpret::Stepped) -> usize {
    stepped
        .effects
        .iter()
        .filter(|effect| {
            matches!(
                effect,
                Effect::Terminal(interpret::claude_pty::TerminalInput::Prompt { .. })
            )
        })
        .count()
}

fn turn_duration_row(uuid: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "system", "subtype": "turn_duration", "uuid": uuid,
        "timestamp": "2026-10-12T08:00:05.000Z", "sessionId": "s1",
        "isSidechain": false, "userType": "external", "durationMs": 5000,
        "messageCount": 2, "isMeta": false,
    })
}

/// A prompt amux typed is in flight until Claude records it, even though
/// the agent is not yet working on it: the next prompt waits in the queue
/// behind it rather than being typed after it, where Claude would take both
/// as one prompt.
#[test]
fn a_prompt_waits_behind_a_typed_prompt_claude_has_not_recorded() {
    let (mut state, _) = started_claude();
    let first = ClaudePty::step(&mut state, prompt_input("p1", "first"));
    assert_eq!(
        typed_prompts(&first),
        1,
        "the first prompt is typed at once"
    );

    let second = ClaudePty::step(&mut state, prompt_input("p2", "second"));
    let tick = ClaudePty::step(&mut state, Event::Tick { at_ms: 1_000 });
    assert_eq!(
        typed_prompts(&second) + typed_prompts(&tick),
        0,
        "the second prompt is typed while the first is unrecorded"
    );
    assert_eq!(
        state.shared().queue().entries().len(),
        1,
        "the second prompt waits"
    );

    let recorded = ClaudePty::step(&mut state, transcript_row(typed_row("u1", "first")));
    assert_eq!(typed_prompts(&recorded), 0, "the first prompt's turn runs");

    let ended = ClaudePty::step(&mut state, transcript_row(turn_duration_row("t1")));
    assert_eq!(
        typed_prompts(&ended),
        1,
        "the second goes once the turn ends"
    );
}
