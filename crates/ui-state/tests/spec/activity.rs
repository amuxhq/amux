//! The activity line: derived from the newest items and the phase, timed
//! against item timestamps with the caller's clock.

use prost::Message as _;
use ui_state::{ActivityKind, SessionState};
use wire::{Kind, Phase, ToolState};

use crate::spec::harness::*;

fn working(kind: Kind) -> SessionState {
    let mut state = SessionState::new(agent(kind), CAP);
    apply_checked(
        &mut state,
        ev_snapshot(snapshot(kind, 1, Phase::Working, &[], &[])),
    );
    apply_checked(
        &mut state,
        ev_item(prompt(kind, 1, 1, "run the tests", b"p1")),
    );
    apply_checked(&mut state, caught_up(1));
    state
}

fn kind_at(state: &SessionState, now_ms: i64) -> (ActivityKind, i64) {
    let activity = state.activity(now_ms).expect("an activity while working");
    (activity.kind, activity.elapsed_ms)
}

#[test]
fn working_is_the_default_timed_from_the_turns_start() {
    for kind in KINDS {
        let state = working(kind);
        assert_eq!(kind_at(&state, 25_000), (ActivityKind::Working, 24_000));
        assert_eq!(
            state.activity(0).unwrap().elapsed_ms,
            0,
            "a clock behind the item never goes negative"
        );
    }
}

#[test]
fn an_in_flight_tool_is_running_named_by_its_call() {
    for kind in KINDS {
        let mut state = working(kind);
        apply_checked(&mut state, ev_item(command(kind, 2, 2, ToolState::Running)));
        assert_eq!(
            kind_at(&state, 14_000),
            (ActivityKind::Running { key: "k2".into() }, 12_000)
        );
        // The turn's own clock runs on under the call, from the prompt.
        assert_eq!(state.activity(14_000).unwrap().turn_elapsed_ms, 13_000);
        apply_checked(
            &mut state,
            ev_item(command(kind, 2, 3, ToolState::Succeeded)),
        );
        assert_eq!(kind_at(&state, 14_000).0, ActivityKind::Working);
    }
}

/// Terminal Claude writes a call's row up to seconds after its PreToolUse
/// hook; the snapshot names the call meanwhile.
#[test]
fn terminal_claudes_announced_call_is_running_until_a_newer_row() {
    let mut state = working(Kind::ClaudePty);
    let mut announced = snapshot(Kind::ClaudePty, 2, Phase::Working, &[], &[]);
    announced.body = wire::ClaudePtySnapshot {
        running_calls: vec![wire::RunningCall {
            tool_use_id: "t1".into(),
            tool_name: "Bash".into(),
            since_ms: 2_500,
        }],
        ..Default::default()
    }
    .encode_to_vec();
    apply_checked(&mut state, ev_snapshot(announced));
    assert_eq!(
        kind_at(&state, 4_000),
        (ActivityKind::Running { key: "t1".into() }, 1_500)
    );
    // An in-flight row older than the announcement does not hide it.
    let mut state_with_older_row = state.clone();
    apply_checked(
        &mut state_with_older_row,
        ev_item(command(Kind::ClaudePty, 2, 2, ToolState::Running)),
    );
    assert_eq!(
        kind_at(&state_with_older_row, 4_000).0,
        ActivityKind::Running { key: "t1".into() }
    );
    // A newer in-flight row is the running call.
    apply_checked(
        &mut state,
        ev_item(command(Kind::ClaudePty, 3, 3, ToolState::Running)),
    );
    assert_eq!(
        kind_at(&state, 4_000),
        (ActivityKind::Running { key: "k3".into() }, 1_000)
    );
}

#[test]
fn an_open_thinking_block_is_thinking() {
    for kind in KINDS {
        let mut state = working(kind);
        apply_checked(
            &mut state,
            ev_item(item(kind, 2, 2, "", Body::Thinking { complete: false })),
        );
        assert_eq!(kind_at(&state, 3_000).0, ActivityKind::Thinking);
        // Terminal Claude's thinking row lands complete: it stays on Working.
        apply_checked(
            &mut state,
            ev_item(item(kind, 2, 3, "", Body::Thinking { complete: true })),
        );
        assert_eq!(kind_at(&state, 3_000).0, ActivityKind::Working);
    }
}

#[test]
fn a_parent_waiting_on_subagents_counts_them() {
    for kind in KINDS {
        let mut state = working(kind);
        for order in 2..=4 {
            let call = Body::Tool {
                name: "Task",
                state: ToolState::Running,
                explore: false,
                subagent: true,
            };
            apply_checked(&mut state, ev_item(item(kind, order, order, "", call)));
        }
        assert_eq!(
            kind_at(&state, 9_000).0,
            ActivityKind::Subagents { count: 3 }
        );
    }
}

#[test]
fn a_provider_retry_shows_its_attempt() {
    for kind in KINDS {
        let mut state = working(kind);
        apply_checked(
            &mut state,
            ev_item(item(
                kind,
                2,
                2,
                "",
                Body::Retry {
                    attempt: 2,
                    max: 10,
                },
            )),
        );
        assert_eq!(
            kind_at(&state, 2_500).0,
            ActivityKind::Retrying {
                attempt: 2,
                max_attempts: 10,
                retry_at_ms: None
            }
        );
    }
}

#[test]
fn compacting_shows_until_the_compaction_lands() {
    let kind = Kind::ClaudeSdk;
    let mut state = working(kind);
    apply_checked(&mut state, ev_item(item(kind, 2, 2, "", Body::Compacting)));
    assert_eq!(kind_at(&state, 2_000).0, ActivityKind::Compacting);
}

#[test]
fn nothing_shows_unless_the_agent_works() {
    for kind in KINDS {
        for phase in [Phase::Idle, Phase::NeedsYou, Phase::Starting] {
            let mut state = SessionState::new(agent(kind), CAP);
            apply_checked(&mut state, ev_snapshot(snapshot(kind, 1, phase, &[], &[])));
            assert_eq!(state.activity(5_000), None);
        }
        let mut state = working(kind);
        apply_checked(
            &mut state,
            ui_state::Msg::Entry(exited(agent(kind), wire::ExitCause::Killed)),
        );
        assert_eq!(state.activity(5_000), None);
    }
}
