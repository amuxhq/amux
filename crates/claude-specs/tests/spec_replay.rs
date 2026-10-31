use std::collections::BTreeSet;
use std::time::Duration;

use claude_specs::specs::MINIMUM_SUPPORTED;
use claude_specs::specs::pty::registry as pty_registry;
use replay_support::{below_minimum, load_recording, orphan_recordings, strict_replay};
use semver::Version;

const SPEC_TIMEOUT: Duration = Duration::from_secs(30);

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
