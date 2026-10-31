//! The window while the reader follows and while the reader is in history:
//! trimmed to its cap at the top as live rows arrive while following; held
//! still while reading, with arrivals kept apart and reported; released at
//! the reader's return when they continue the window, and reloaded from a
//! fresh tail when the head moved on past them.

use ui_state::{Msg, SessionState};
use wire::{Kind, Phase};

use crate::spec::harness::*;

/// A following session with rows `orders` held, caught up, and a window
/// cap of `cap`.
fn open(kind: Kind, cap: usize, orders: std::ops::RangeInclusive<u64>) -> SessionState {
    let mut state = SessionState::new(agent(kind), cap);
    apply_block(
        &mut state,
        ev_snapshot(snapshot(kind, 1, Phase::Working, &[], &[])),
    );
    for order in orders {
        apply_block(&mut state, ev_item(text(kind, order, order, "held")));
    }
    apply_block(&mut state, caught_up(100));
    state
}

fn orders(state: &SessionState) -> Vec<u64> {
    state
        .transcript()
        .iter()
        .map(|held| held.item.order)
        .collect()
}

#[test]
fn a_session_starts_following() {
    for kind in KINDS {
        let state = SessionState::new(agent(kind), 5);
        assert!(state.following());
        assert!(!state.arrivals_held());
        assert_eq!(state.page_room(), Some(5));
    }
}

#[test]
fn following_drops_the_oldest_rows_past_the_cap_as_live_rows_arrive() {
    for kind in KINDS {
        let mut state = open(kind, 5, 1..=5);
        assert!(
            !state.transcript().has_older(),
            "a window reaching order one holds everything"
        );
        let outcome = apply_block(&mut state, ev_item(text(kind, 6, 106, "new")));
        assert_eq!(outcome.changed, vec!["k6", "k1"]);
        assert_eq!(orders(&state), vec![2, 3, 4, 5, 6]);
        assert!(
            state.transcript().has_older(),
            "the dropped rows are older history again"
        );
        assert_eq!(state.page_room(), Some(0), "a full window pages nothing");

        // A later revision of a dropped key is ignored like any row below
        // the window, and an append to it is dropped without a Get.
        let outcome = apply_block(&mut state, ev_item(text(kind, 1, 200, "revised")));
        assert!(outcome.changed.is_empty());
        assert!(state.transcript().get("k1").is_none());
        let outcome = apply_block(&mut state, ev_append("k1", 200, 201, "x"));
        assert!(outcome.need_get.is_none() && outcome.changed.is_empty());
        assert!(state.transcript().get("k1").is_none());

        // Keys, input ids and referrers of dropped rows are forgotten.
        apply_block(&mut state, ev_item(prompt(kind, 7, 107, "hi", b"p7")));
        for order in 8..=12 {
            apply_block(&mut state, ev_item(text(kind, order, order + 100, "more")));
        }
        assert_eq!(orders(&state), vec![8, 9, 10, 11, 12]);
        assert!(state.transcript().key_for_input(b"p7").is_none());
        state.transcript().check().unwrap();
    }
}

#[test]
fn a_trim_clears_the_exhausted_mark_and_opens_the_low_run_below() {
    for kind in KINDS {
        let mut state = SessionState::new(agent(kind), 4);
        apply_block(
            &mut state,
            ev_snapshot(snapshot(kind, 1, Phase::Working, &[], &[])),
        );
        for order in 3..=6 {
            apply_block(&mut state, ev_item(read(kind, order, order)));
        }
        apply_block(&mut state, caught_up(100));
        apply_page(&mut state, vec![], true);
        assert!(!state.transcript().has_older());
        let run = state.transcript().run_at(3).unwrap();
        assert!(!run.open_below);

        let outcome = apply_block(&mut state, ev_item(read(kind, 7, 7)));
        assert!(state.transcript().has_older());
        let run = state.transcript().run_at(4).unwrap();
        assert_eq!((run.oldest, run.newest, run.steps), (4, 7, 4));
        assert!(
            run.open_below,
            "the run at the new low edge reads open below"
        );
        let mut changed = outcome.changed.clone();
        changed.sort();
        assert_eq!(changed, vec!["k3", "k4", "k5", "k6", "k7"]);
    }
}

#[test]
fn reading_holds_arrivals_apart_and_reports_them() {
    for kind in KINDS {
        let mut state = open(kind, 5, 1..=5);
        let outcome = apply_block(&mut state, Msg::Following(false));
        assert!(outcome.session);
        assert!(!state.arrivals_held());
        assert_eq!(state.page_room(), None, "in history a page is any size");

        let outcome = apply_block(&mut state, ev_item(streaming(kind, 6, 106, "a")));
        assert!(outcome.changed.is_empty(), "the window does not move");
        assert!(outcome.session, "the affordance and activity line may move");
        assert_eq!(state.transcript().head(), Some(5));
        assert!(state.arrivals_held(), "the affordance shows from this");
        assert_eq!(state.held_arrivals(), 1);

        // Appends to a held row apply to it; revisions inside the window
        // still apply to the window.
        let outcome = apply_block(&mut state, ev_append("k6", 106, 107, "b"));
        assert!(outcome.changed.is_empty());
        assert!(outcome.need_get.is_none());
        let outcome = apply_block(&mut state, ev_item(text(kind, 3, 150, "revised")));
        assert_eq!(outcome.changed, vec!["k3"]);

        // The snapshot stays live: an ask opens on the card at once.
        apply_block(
            &mut state,
            ev_snapshot(snapshot(kind, 160, Phase::NeedsYou, &["a1"], &[])),
        );
        assert_eq!(state.open_asks().len(), 1);

        // Rows paged in stay, however many the window then holds.
        let mut deep = open(kind, 3, 11..=13);
        apply_block(&mut deep, Msg::Following(false));
        let page = (5..=10).rev().map(|o| text(kind, o, o, "old")).collect();
        apply_page(&mut deep, page, false);
        apply_block(&mut deep, ev_item(text(kind, 14, 114, "live")));
        assert_eq!(orders(&deep), (5..=13).collect::<Vec<_>>());

        // Returning releases the held row with its appended text.
        let outcome = apply_block(&mut state, Msg::Following(true));
        assert!(!outcome.reload);
        assert!(outcome.changed.contains(&"k6".to_owned()));
        assert_eq!(state.transcript().get("k6").unwrap().item.text, "ab");
        assert!(!state.arrivals_held());
    }
}

#[test]
fn returning_releases_held_rows_that_fit_under_the_cap() {
    for kind in KINDS {
        let mut state = open(kind, 5, 1..=5);
        apply_block(&mut state, Msg::Following(false));
        for order in 6..=8 {
            apply_block(&mut state, ev_item(text(kind, order, order + 100, "live")));
        }
        assert_eq!(orders(&state), vec![1, 2, 3, 4, 5]);
        let outcome = apply_block(&mut state, Msg::Following(true));
        assert!(!outcome.reload, "a release fetches nothing");
        assert_eq!(orders(&state), vec![4, 5, 6, 7, 8]);
        assert!(state.following());
        assert!(!state.arrivals_held());
    }
}

#[test]
fn a_window_paged_past_the_cap_is_released_then_trimmed_from_the_top() {
    for kind in KINDS {
        let mut state = open(kind, 4, 10..=13);
        apply_block(&mut state, Msg::Following(false));
        let page = (4..=9).rev().map(|o| text(kind, o, o, "old")).collect();
        apply_page(&mut state, page, false);
        apply_block(&mut state, ev_item(text(kind, 14, 114, "live")));
        assert_eq!(state.transcript().len(), 10);
        let outcome = apply_block(&mut state, Msg::Following(true));
        assert!(!outcome.reload);
        assert_eq!(orders(&state), vec![11, 12, 13, 14]);
    }
}

#[test]
fn returning_after_the_head_moved_on_reloads_it_from_a_fresh_tail() {
    for kind in KINDS {
        let mut state = open(kind, 3, 1..=3);
        apply_block(&mut state, Msg::Following(false));
        for order in 4..=7 {
            apply_block(&mut state, ev_item(text(kind, order, order + 100, "live")));
        }
        assert!(state.head_moved_on());
        assert!(state.arrivals_held());
        assert_eq!(orders(&state), vec![1, 2, 3]);

        let outcome = apply_block(&mut state, Msg::Following(true));
        assert!(outcome.reload, "the driver reopens the stream");
        assert!(outcome.changed.is_empty(), "the old window stays on screen");
        // Until it does, live rows above the head wait for the reload.
        let outcome = apply_block(&mut state, ev_item(text(kind, 8, 108, "live")));
        assert!(outcome.changed.is_empty());
        let outcome = apply_block(&mut state, ev_append("k8", 108, 109, "x"));
        assert!(outcome.need_get.is_none());

        apply_block(&mut state, Msg::Reloading);
        assert!(state.reset_pending());
        apply_block(
            &mut state,
            ev_snapshot(snapshot(kind, 120, Phase::Working, &[], &[])),
        );
        for order in 7..=9 {
            apply_block(&mut state, ev_item(text(kind, order, order + 100, "tail")));
        }
        assert_eq!(orders(&state), vec![1, 2, 3], "built apart");
        let outcome = apply_block(&mut state, caught_up(130));
        assert!(outcome.reloaded, "the swap reports reloaded");
        assert_eq!(orders(&state), vec![7, 8, 9]);
        assert!(!state.head_moved_on());
        assert!(!state.arrivals_held());
        let outcome = apply_block(&mut state, ev_item(text(kind, 10, 140, "next")));
        assert_eq!(outcome.changed, vec!["k10", "k7"]);
    }
}

#[test]
fn the_held_set_is_bounded_by_the_cap() {
    for kind in KINDS {
        let mut state = open(kind, 3, 1..=3);
        apply_block(&mut state, Msg::Following(false));
        for order in 4..=6 {
            apply_block(
                &mut state,
                ev_item(streaming(kind, order, order + 100, "a")),
            );
        }
        assert_eq!(state.held_arrivals(), 3);
        assert!(!state.head_moved_on());
        apply_block(&mut state, ev_item(text(kind, 7, 107, "one too many")));
        assert_eq!(state.held_arrivals(), 0, "past the cap nothing is held");
        assert!(state.head_moved_on(), "only that the head moved on");
        assert!(state.arrivals_held());
        // Appends to rows it let go of wait for the reload, not a Get.
        let outcome = apply_block(&mut state, ev_append("k6", 106, 107, "b"));
        assert!(outcome.need_get.is_none());
        assert_eq!(orders(&state), vec![1, 2, 3]);
    }
}

#[test]
fn a_gap_above_the_held_rows_stops_the_holding() {
    for kind in KINDS {
        let mut state = open(kind, 5, 1..=3);
        apply_block(&mut state, Msg::Following(false));
        apply_block(&mut state, ev_item(text(kind, 4, 104, "next")));
        // A re-tail after a lag lands past rows this session never saw.
        apply_block(&mut state, lagged());
        apply_block(&mut state, ev_item(text(kind, 9, 109, "past a gap")));
        assert!(state.head_moved_on());
        assert_eq!(orders(&state), vec![1, 2, 3], "the reader's window stays");
        let outcome = apply_block(&mut state, Msg::Following(true));
        assert!(outcome.reload);
    }
}

#[test]
fn held_arrivals_keep_the_activity_line_and_inputs_current() {
    for kind in KINDS {
        let mut state = open(kind, 5, 1..=3);
        apply_block(&mut state, Msg::Following(false));
        apply_block(
            &mut state,
            ev_item(item(kind, 4, 104, "", Body::Thinking { complete: false })),
        );
        let activity = state.activity(10_000).unwrap();
        assert_eq!(activity.kind, ui_state::ActivityKind::Thinking);

        apply_block(&mut state, Msg::Send(prompt_input(b"p1", "hi")));
        apply_block(&mut state, ev_item(prompt(kind, 5, 105, "hi", b"p1")));
        assert_eq!(
            state.input_state(b"p1"),
            Some(ui_state::InputState::Settled),
            "a reflection held apart still settles the prompt"
        );
    }
}
