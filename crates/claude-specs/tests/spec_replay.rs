use std::collections::BTreeSet;
use std::time::Duration;

use claude_specs::specs::{
    MINIMUM_SUPPORTED, SpecSource, fixtures_root, pty_registry, run, sdk_registry,
};
use replay_support::{below_minimum, load_recording, orphan_recordings, strict_replay};
use semver::Version;

const SPEC_TIMEOUT: Duration = Duration::from_secs(30);

mod sdk_replays {
    use super::*;

    macro_rules! scenarios {
        ($($name:ident => $entry:literal),+ $(,)?) => {
            const SCENARIOS: &[&str] = &[$($entry),+];
            $(
                #[tokio::test]
                async fn $name() {
                    replay($entry).await;
                }
            )+
        };
    }

    scenarios!(
        session_text_turn => "session/text_turn",
        session_streamed_turn => "session/streamed_turn",
        session_multi_turn => "session/multi_turn",
        session_sign_in_problem => "session/sign_in_problem",
        commands_compacted => "commands/compacted",
        commands_cleared => "commands/cleared",
        control_permission_mode_and_model => "control/permission_mode_and_model",
        control_session_introspection => "control/session_introspection",
        control_effort => "control/effort",
        tools_permission_callback => "tools/permission_callback",
        tools_question_asked => "tools/question_asked",
        tools_plan_reviewed => "tools/plan_reviewed",
        tools_question_every_shape => "tools/question_every_shape",
        tools_question_dismissed => "tools/question_dismissed",
        tools_elicitation_accepted => "tools/elicitation_accepted",
        tools_elicitation_declined => "tools/elicitation_declined",
        tools_elicitation_cancelled => "tools/elicitation_cancelled",
        tools_elicitation_link_accepted => "tools/elicitation_link_accepted",
        tools_hook_lifecycle => "tools/hook_lifecycle",
        options_configured_turn => "options/configured_turn",
        configured_effortful_turn => "configured/effortful_turn",
        agents_subagent_task => "agents/subagent_task",
        history_resumed => "history/resumed",
        results_interrupted => "results/interrupted",
        work_image => "work/image",
        work_failing_command => "work/failing_command",
        work_task_list => "work/task_list",
        work_background_shell => "work/background_shell",
        control_failed_tool_server => "control/failed_tool_server",
        probes_client_id => "probes/client_id",
        probes_side_channel => "probes/side_channel",
        probes_steer_folded => "probes/steer_folded",
        probes_steer_preempted => "probes/steer_preempted",
    );

    #[test]
    fn every_registered_scenario_has_a_test() {
        assert!(!SCENARIOS.is_empty());
        assert_eq!(
            SCENARIOS.iter().copied().collect::<BTreeSet<_>>(),
            sdk_registry()
                .iter()
                .map(|entry| entry.name)
                .collect::<BTreeSet<_>>(),
        );
    }

    #[tokio::test(start_paused = true)]
    async fn recorded_drains_observe_eof_without_waiting_for_live_quiet() {
        let started = tokio::time::Instant::now();
        for entry in ["agents/subagent_task", "tools/hook_lifecycle"] {
            replay(entry).await;
        }
        assert_eq!(started.elapsed(), Duration::ZERO);
    }

    #[tokio::test]
    async fn another_hook_setup_cannot_replace_an_in_flight_sessions_observations() {
        let mut first = Box::pin(replay("tools/hook_lifecycle"));
        std::future::poll_fn(|cx| {
            assert!(std::future::Future::poll(first.as_mut(), cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;

        // Open another setup before the first session's spawned callbacks
        // run. Reject its transport declaration so its empty log cannot
        // coincidentally match the first session's completed hook sequence.
        let entry = sdk_registry()
            .iter()
            .find(|entry| entry.name == "tools/hook_lifecycle")
            .unwrap();
        let recording = load_recording(&fixtures_root().join(entry.recording)).unwrap();
        let error = run(
            entry,
            SpecSource::Recorded {
                replay: strict_replay(&recording),
                transport_order: Vec::new(),
                session_ids: recording.manifest.session_ids,
            },
        )
        .await
        .expect_err("the second setup has no declared transport");
        assert!(error.claim.contains("recording has 0 transports"));
        first.await;
    }
}

mod pty_replays {
    use super::*;

    macro_rules! scenarios {
        ($($name:ident),+ $(,)?) => {
            const SCENARIOS: &[&str] = &[$(stringify!($name)),+];

            $(
                #[tokio::test]
                async fn $name() {
                    let entry = pty_registry().iter()
                        .find(|entry| entry.name == stringify!($name))
                        .expect("replay scenario remains registered");
                    replay_pty(entry).await;
                }
            )+
        };
    }

    scenarios!(
        prompt,
        prompt_multiline,
        prompt_long,
        tools,
        permission_allow_once,
        permission_allow_scoped,
        permission_deny_feedback,
        plan_approve,
        plan_auto,
        plan_request_changes,
        question_single,
        question_multi_other,
        question_mixed,
        question_tabs,
        question_other_single,
        interrupt,
        mode_cycle,
        compact_relink,
        clear_relink,
        question_every_shape,
        question_cancelled,
        question_skip_reply,
        subagent,
        background_shell,
        task_list,
        file_changes_and_failure,
        image,
        thinking,
        api_error,
        sign_in_problem,
        steer_queued,
        steer_send_now,
    );

    #[tokio::test(start_paused = true)]
    async fn recorded_preparation_and_cleanup_do_not_wait_for_live_timers() {
        let entry = pty_registry()
            .iter()
            .find(|entry| entry.name == "plan_approve")
            .unwrap();
        let started = tokio::time::Instant::now();
        replay_pty(entry).await;
        assert_eq!(started.elapsed(), Duration::ZERO);
    }

    #[tokio::test(start_paused = true)]
    async fn cleanup_drains_a_recorded_tail_larger_than_the_transport_buffers() {
        let entry = pty_registry()
            .iter()
            .find(|entry| entry.name == "prompt")
            .unwrap();
        let mut recording =
            load_recording(&claude_specs::specs::pty::fixtures_root().join(entry.recording))
                .unwrap();
        let us = recording.io.last().unwrap().us;
        let line = serde_json::json!({
            "row": {"type": "system", "content": "x".repeat(8192)}
        })
        .to_string();
        for index in 1..=1024 {
            recording.io.push(replay_support::IoEvent {
                us: us + index,
                direction: replay_support::IoDirection::Read,
                line: line.clone(),
                transport_id: Some("transcript".to_owned()),
                session_id: None,
            });
        }
        replay_pty_recording(entry, recording).await;
    }

    #[test]
    fn every_registered_scenario_has_a_test() {
        assert_eq!(
            SCENARIOS.iter().copied().collect::<BTreeSet<_>>(),
            pty_registry()
                .iter()
                .map(|entry| entry.name)
                .collect::<BTreeSet<_>>(),
        );
    }
}

async fn replay_pty(entry: &replay_support::SpecEntry) {
    let root = claude_specs::specs::pty::fixtures_root();
    eprintln!("replaying PTY {}", entry.name);
    let recording = load_recording(&root.join(entry.recording))
        .unwrap_or_else(|error| panic!("load {}: {error}", entry.name));
    replay_pty_recording(entry, recording).await;
}

async fn replay_pty_recording(
    entry: &replay_support::SpecEntry,
    recording: replay_support::Recording,
) {
    assert!(
        entry
            .allowed_models
            .contains(&recording.manifest.recorded.model.as_str()),
        "{} was recorded with disallowed model {}",
        entry.name,
        recording.manifest.recorded.model
    );
    let replay = strict_replay(&recording);
    let report = tokio::time::timeout(
        SPEC_TIMEOUT,
        claude_specs::specs::pty::run(
            entry,
            claude_specs::specs::pty::Source::Recorded {
                replay,
                manifest: Box::new(recording.manifest),
            },
        ),
    )
    .await
    .unwrap_or_else(|_| panic!("{} stalled after {SPEC_TIMEOUT:?}", entry.name))
    .unwrap_or_else(|error| panic!("{error}"));
    assert!(
        report.replay.is_some_and(|report| report.is_complete()),
        "{} did not return a complete replay report",
        entry.name
    );
}

#[tokio::test]
async fn elicited() {
    replay("tools/elicitation_accepted").await;
}

#[tokio::test]
async fn hook_lifecycle() {
    replay("tools/hook_lifecycle").await;
}

async fn replay(name: &str) {
    let entry = sdk_registry()
        .iter()
        .find(|entry| entry.name == name)
        .unwrap_or_else(|| panic!("missing registered specification {name}"));
    eprintln!("replaying {}", entry.name);
    let recording = load_recording(&fixtures_root().join(entry.recording))
        .unwrap_or_else(|error| panic!("load {}: {error}", entry.name));
    assert!(
        entry
            .allowed_models
            .contains(&recording.manifest.recorded.model.as_str()),
        "{} was recorded with disallowed model {}",
        entry.name,
        recording.manifest.recorded.model
    );

    let mut seen = BTreeSet::new();
    let transport_order = recording
        .io
        .iter()
        .filter_map(|event| event.transport_id.clone())
        .filter(|transport| seen.insert(transport.clone()))
        .collect::<Vec<_>>();
    let replay = strict_replay(&recording);
    let controller = replay.controller.clone();
    let outcome = match tokio::time::timeout(
        SPEC_TIMEOUT,
        run(
            entry,
            SpecSource::Recorded {
                replay,
                transport_order,
                session_ids: recording.manifest.session_ids.clone(),
            },
        ),
    )
    .await
    {
        Ok(outcome) => outcome,
        Err(_) => {
            let report = controller.finish().err().map(|error| error.report);
            panic!(
                "{} stalled after {SPEC_TIMEOUT:?}; replay={report:?}",
                entry.name
            );
        }
    };
    if let Err(error) = outcome {
        let report = controller.finish().err().map(|error| error.report);
        panic!("{error}; replay={report:?}");
    }
    let report = controller
        .finish()
        .unwrap_or_else(|error| panic!("{}: {error}: {:?}", entry.name, error.report));
    assert!(report.is_complete(), "{}: {report:?}", entry.name);
}

#[test]
fn sdk_corpus_is_inventoried_current_and_unorphaned() {
    let root = fixtures_root();
    assert_eq!(
        orphan_recordings(&root, sdk_registry()),
        Vec::<String>::new()
    );
    let minimum = Version::parse(MINIMUM_SUPPORTED).expect("minimum is semantic");
    assert_eq!(below_minimum(&root, &minimum), Vec::new());

    for entry in sdk_registry() {
        let recording = load_recording(&root.join(entry.recording))
            .unwrap_or_else(|error| panic!("load {}: {error}", entry.name));
        assert_eq!(recording.manifest.spec, entry.name);
        assert_eq!(recording.manifest.recorded.provider, "claude");
        assert!(recording.manifest.recorded.version >= minimum);
        assert!(
            [Version::new(2, 1, 312)].contains(&recording.manifest.recorded.version),
            "{} was captured against an unreviewed Claude version {}",
            entry.name,
            recording.manifest.recorded.version
        );
        assert!(recording.manifest.content.contains_key("io.jsonl"));
        assert!(recording.manifest.content.contains_key("spawn.jsonl"));
    }
}

#[test]
fn recorded_pty_corpus_is_inventoried_current_and_unorphaned() {
    let root = claude_specs::specs::pty::fixtures_root();
    if !root.exists() {
        return;
    }
    assert_eq!(
        orphan_recordings(&root, pty_registry()),
        Vec::<String>::new()
    );
    let minimum = Version::parse(MINIMUM_SUPPORTED).expect("minimum is semantic");
    assert_eq!(below_minimum(&root, &minimum), Vec::new());
    for entry in pty_registry() {
        let recording = load_recording(&root.join(entry.recording))
            .unwrap_or_else(|error| panic!("load {}: {error}", entry.name));
        assert_eq!(recording.manifest.spec, entry.name);
        assert_eq!(recording.manifest.recorded.provider, "claude");
        assert!(recording.manifest.recorded.version >= minimum);
        assert_eq!(
            recording
                .io
                .iter()
                .filter_map(|event| event.transport_id.as_deref())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["hook", "pty", "transcript"]),
            "{} does not carry all three PTY transports",
            entry.name
        );
    }
}
