//! Run membership: every tool step between two pieces of what the agent or
//! the person said, kept current on every message and equal to a rebuild
//! from the transcript.

use proptest::collection::vec;
use proptest::prelude::*;
use proptest::sample::Index;
use ui_state::{Run, SessionState};
use wire::{Kind, Phase, ToolState};

use crate::spec::harness::*;

fn open(kind: Kind) -> SessionState {
    let mut state = SessionState::new(agent(kind), CAP);
    apply_checked(
        &mut state,
        ev_snapshot(snapshot(kind, 1, Phase::Working, &[], &[])),
    );
    state
}

fn run_of(state: &SessionState, order: u64) -> Option<Run> {
    state.transcript().run_at(order)
}

#[test]
fn a_run_is_every_tool_step_between_two_pieces_of_text() {
    for kind in KINDS {
        let mut state = open(kind);
        apply_checked(&mut state, ev_item(text(kind, 10, 1, "looking")));
        assert_eq!(run_of(&state, 10), None, "text is in no run");
        apply_checked(&mut state, ev_item(read(kind, 11, 2)));
        let run = run_of(&state, 11).unwrap();
        assert_eq!((run.oldest, run.newest, run.steps), (11, 11, 1));
        assert!(!run.closed, "nothing follows it yet");
        let outcome = apply_checked(
            &mut state,
            ev_item(command(kind, 12, 3, ToolState::Succeeded)),
        );
        assert_eq!(outcome.changed, vec!["k12", "k11"]);
        apply_checked(&mut state, ev_item(grep(kind, 13, 4)));
        let run = run_of(&state, 12).unwrap();
        assert_eq!((run.oldest, run.newest, run.steps), (11, 13, 3));
        assert_eq!(
            (run.oldest_key.as_str(), run.newest_key.as_str()),
            ("k11", "k13")
        );
        // The agent speaks: the run ends, and the next step starts another.
        let outcome = apply_checked(&mut state, ev_item(text(kind, 14, 5, "found it")));
        assert_eq!(outcome.changed, vec!["k14", "k11", "k12", "k13"]);
        assert!(run_of(&state, 13).unwrap().closed);
        apply_checked(&mut state, ev_item(read(kind, 15, 6)));
        assert_eq!(run_of(&state, 15).unwrap().oldest, 15);
        assert_eq!(run_of(&state, 13).unwrap().steps, 3);
    }
}

#[test]
fn thinking_and_retries_pass_through_a_run_and_a_turn_end_closes_it() {
    for kind in KINDS {
        let mut state = open(kind);
        apply_checked(&mut state, ev_item(read(kind, 1, 1)));
        let outcome = apply_checked(&mut state, ev_item(thinking(kind, 2, 2)));
        assert_eq!(outcome.changed, vec!["k2"], "thinking moves no run");
        apply_checked(
            &mut state,
            ev_item(item(kind, 3, 3, "", Body::Retry { attempt: 1, max: 3 })),
        );
        apply_checked(&mut state, ev_item(command(kind, 4, 4, ToolState::Failed)));
        let run = run_of(&state, 2).unwrap();
        assert_eq!((run.oldest, run.newest, run.steps), (1, 4, 2));
        let outcome = apply_checked(&mut state, ev_item(turn(kind, 5, 5)));
        assert!(run_of(&state, 4).unwrap().closed);
        assert_eq!(run_of(&state, 5), None);
        let mut changed = outcome.changed.clone();
        changed.sort();
        assert_eq!(changed, vec!["k1", "k2", "k3", "k4", "k5"]);
        assert!(state.transcript().turn_ended_after(4));
    }
}

#[test]
fn a_turn_end_settles_the_steps_of_runs_the_agent_spoke_after() {
    for kind in KINDS {
        let mut state = open(kind);
        apply_checked(
            &mut state,
            ev_item(prompt(kind, 1, 1, "fix the lint", b"p")),
        );
        apply_checked(&mut state, ev_item(command(kind, 2, 2, ToolState::Failed)));
        apply_checked(&mut state, ev_item(text(kind, 3, 3, "one more try")));
        apply_checked(
            &mut state,
            ev_item(command(kind, 4, 4, ToolState::Succeeded)),
        );
        apply_checked(&mut state, ev_item(text(kind, 5, 5, "done")));
        assert!(!state.transcript().turn_ended_after(2));
        let outcome = apply_checked(&mut state, ev_item(turn(kind, 6, 6)));
        let mut changed = outcome.changed.clone();
        changed.sort();
        assert_eq!(changed, vec!["k2", "k4", "k6"]);
        assert!(state.transcript().turn_ended_after(2));
    }
}

#[test]
fn the_newest_step_moves_while_every_row_id_stays() {
    for kind in KINDS {
        let mut state = open(kind);
        for order in 1..=3 {
            apply_checked(&mut state, ev_item(read(kind, order, order)));
        }
        let keys: Vec<String> = state.transcript().keys().cloned().collect();
        let outcome = apply_checked(&mut state, ev_item(read(kind, 4, 4)));
        assert_eq!(
            outcome.changed,
            vec!["k4", "k1", "k2", "k3"],
            "every member's run moved"
        );
        assert_eq!(run_of(&state, 1).unwrap().newest_key, "k4");
        let after: Vec<String> = state.transcript().keys().cloned().collect();
        assert_eq!(&after[..3], &keys[..], "row ids never move");
    }
}

#[test]
fn a_page_merging_into_a_run_at_the_edge_keeps_ids_and_grows_the_count() {
    for kind in KINDS {
        let mut state = open(kind);
        for order in 20..=21 {
            apply_checked(&mut state, ev_item(read(kind, order, order)));
        }
        apply_checked(&mut state, ev_item(text(kind, 22, 22, "found it")));
        apply_checked(&mut state, caught_up(22));
        let run = run_of(&state, 20).unwrap();
        assert!(
            run.open_below,
            "the run starts at the oldest held row with older history"
        );
        let items = (12..=19)
            .rev()
            .map(|order| {
                if order % 2 == 0 {
                    read(kind, order, order)
                } else {
                    command(kind, order, order, ToolState::Succeeded)
                }
            })
            .collect();
        apply_page(&mut state, items, false);
        let run = run_of(&state, 21).unwrap();
        assert_eq!((run.oldest, run.newest, run.steps), (12, 21, 10));
        assert_eq!(
            run.newest_key, "k21",
            "the step the reader looks at keeps its id"
        );
        assert!(run.open_below);
        apply_page(&mut state, vec![thinking(kind, 11, 11)], false);
        assert!(
            run_of(&state, 21).unwrap().open_below,
            "thinking below does not end it"
        );
        apply_page(&mut state, vec![text(kind, 10, 10, "earlier")], false);
        assert!(
            !run_of(&state, 21).unwrap().open_below,
            "text below closes it"
        );
    }
}

#[test]
fn open_below_is_set_only_for_the_run_nothing_below_ends() {
    for kind in KINDS {
        let mut state = open(kind);
        for order in 5..=6 {
            apply_checked(&mut state, ev_item(read(kind, order, order)));
        }
        apply_checked(&mut state, ev_item(text(kind, 7, 7, "a")));
        for order in 8..=9 {
            apply_checked(&mut state, ev_item(read(kind, order, order)));
        }
        assert!(run_of(&state, 5).unwrap().open_below);
        assert!(!run_of(&state, 8).unwrap().open_below);
        let outcome = apply_page(&mut state, vec![], true);
        assert_eq!(
            outcome.changed,
            vec!["k5", "k6"],
            "exhausted history closes the edge run"
        );
        assert!(!run_of(&state, 5).unwrap().open_below);
    }
}

#[test]
fn a_revision_that_changes_an_items_class_splits_or_merges_runs() {
    for kind in KINDS {
        let mut state = open(kind);
        apply_checked(&mut state, ev_item(read(kind, 1, 1)));
        apply_checked(&mut state, ev_item(streaming(kind, 2, 2, "hm")));
        apply_checked(&mut state, ev_item(read(kind, 3, 3)));
        assert_eq!(run_of(&state, 1).unwrap().steps, 1);
        // The text between is revised into a call: one run of three.
        let outcome = apply_checked(&mut state, ev_item(command(kind, 2, 4, ToolState::Running)));
        assert_eq!(outcome.changed.len(), 3);
        assert_eq!(run_of(&state, 3).unwrap().steps, 3);
        // And back: split.
        apply_checked(&mut state, ev_item(text(kind, 2, 5, "hm")));
        assert_eq!(run_of(&state, 1).unwrap().steps, 1);
        assert_eq!(run_of(&state, 3).unwrap().oldest, 3);
    }
}

/// One message in a run-index case.
#[derive(Clone, Debug)]
enum Arrival {
    /// A new item at the head.
    Head(u8),
    /// A revision of an item between the first and the head.
    Revise(Index, u8),
    /// A page of up to `back` older items, maybe the last page.
    Page {
        back: u64,
        shapes: Vec<u8>,
        exhausted: bool,
    },
}

fn shape() -> impl Strategy<Value = u8> {
    0..6u8
}

fn arrival() -> impl Strategy<Value = Arrival> {
    prop_oneof![
        2 => shape().prop_map(Arrival::Head),
        1 => (any::<Index>(), shape()).prop_map(|(at, shape)| Arrival::Revise(at, shape)),
        1 => (1..=3u64, vec(shape(), 3), prop::bool::weighted(0.1)).prop_map(
            |(back, shapes, exhausted)| Arrival::Page { back, shapes, exhausted }
        ),
    ]
}

proptest! {
    #![proptest_config(cases(128))]

    // apply_checked asserts the rebuild and changed-key invariants on every
    // message; this drives it through random windows, pages and revisions.
    #[test]
    fn the_run_index_equals_a_rebuild_after_every_message(
        kind in kinds(),
        start in 30..50u64,
        arrivals in vec(arrival(), 0..=80),
    ) {
        let mut state = open(kind);
        let mut head = start;
        apply_checked(&mut state, ev_item(read(kind, head, 1)));
        let mut revision = 2;
        for arrival in arrivals {
            revision += 1;
            let (order, shape) = match arrival {
                Arrival::Head(shape) => {
                    head += 1;
                    (head, shape)
                }
                Arrival::Revise(at, shape) => {
                    (start + at.index((head - start + 1) as usize) as u64, shape)
                }
                Arrival::Page { back, shapes, exhausted } => {
                    let oldest = state.oldest_order().unwrap();
                    if oldest > 1 {
                        let items = (oldest.saturating_sub(back).max(1)..oldest)
                            .zip(&shapes)
                            .map(|(o, shape)| shaped_item(kind, *shape, o, revision))
                            .collect();
                        apply_page(&mut state, items, exhausted);
                    }
                    continue;
                }
            };
            apply_checked(&mut state, ev_item(shaped_item(kind, shape, order, revision)));
        }
    }
}

fn shaped_item(kind: Kind, shape: u8, order: u64, revision: u64) -> wire::Item {
    match shape {
        0 => read(kind, order, revision),
        1 => thinking(kind, order, revision),
        2 => command(kind, order, revision, ToolState::Succeeded),
        3 => turn(kind, order, revision),
        _ => text(kind, order, revision, "prose"),
    }
}
