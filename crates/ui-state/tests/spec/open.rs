//! Opening a chat and the stream markers: CaughtUp, Detached, Lagged,
//! Reset, and a re-tail after the runtime connection drops.

use ui_state::{Composer, Connection, Msg, SessionState, Waiting};
use wire::Phase;

use crate::harness::*;

#[test]
fn open_from_the_snapshot_and_held_rows_with_caught_up_later() {
    for kind in KINDS {
        let mut tape = Tape::new(agent(kind));
        tape.apply(
            "snapshot",
            ev_snapshot(snapshot(kind, 40, Phase::Working, &[], &[])),
        );
        for order in 31..=40 {
            tape.apply("held row", ev_item(text(kind, order, order, "held")));
        }
        assert_eq!(
            tape.state.transcript().len(),
            10,
            "the first render has the held rows"
        );
        assert!(!tape.state.can_send());
        assert_eq!(tape.state.composer(), Composer::Holds(Waiting::CatchingUp));
        tape.apply(
            "live row before CaughtUp",
            ev_item(text(kind, 41, 41, "delta")),
        );
        tape.apply("CaughtUp", caught_up(41));
        assert!(tape.state.can_send());
        assert_eq!(tape.state.composer(), Composer::Send);
        assert_eq!(tape.state.transcript().head(), Some(41));
    }
}

#[test]
fn open_from_a_snapshot_alone_then_a_live_tail() {
    for kind in KINDS {
        let mut tape = Tape::new(agent(kind));
        tape.apply(
            "snapshot, no block",
            ev_snapshot(snapshot(kind, 3, Phase::Starting, &[], &[])),
        );
        assert!(tape.state.transcript().is_empty());
        tape.apply("CaughtUp", caught_up(3));
        for order in 1..=3 {
            tape.apply("live", ev_item(text(kind, order, 3 + order, "tail")));
        }
        assert_eq!(tape.state.oldest_order(), Some(1));
        assert!(!tape.state.transcript().has_older());
    }
}

#[test]
fn an_empty_snapshot_body_is_starting_with_nothing_known() {
    for kind in KINDS {
        let mut state = SessionState::new(agent(kind), CAP);
        let mut empty = snapshot(kind, 0, Phase::Starting, &[], &[]);
        empty.body.clear();
        apply_checked(&mut state, ev_snapshot(empty));
        let decoded = state.agent_state();
        assert_eq!(
            decoded,
            &ui_state::AgentState {
                kind,
                phase: Phase::Starting,
                ..Default::default()
            }
        );
        assert_eq!(decoded.model, None);
        assert!(decoded.asks.is_empty());
        assert_eq!(state.phase(), ui_state::PhaseView::Starting);
    }
}

#[test]
fn a_subscribe_to_an_away_host_paints_held_rows_then_detached() {
    for kind in KINDS {
        let mut tape = Tape::new(agent(kind));
        tape.apply(
            "snapshot",
            ev_snapshot(snapshot(kind, 40, Phase::Idle, &[], &[])),
        );
        for order in 300..=340 {
            tape.apply("held", ev_item(text(kind, order, order, "cached")));
        }
        tape.apply("Detached", detached());
        assert_eq!(tape.state.transcript().len(), 41, "the rows stay");
        assert!(!tape.state.can_send());
        assert_eq!(tape.state.composer(), Composer::Holds(Waiting::Detached));
        // The draft is the client's; nothing here touches it, and nothing
        // else happens until the source resumes after its cursor.
        tape.apply("delta after wake", ev_item(text(kind, 341, 341, "delta")));
        tape.apply("CaughtUp", caught_up(341));
        assert!(tape.state.can_send());
        assert_eq!(tape.state.transcript().head(), Some(341));
    }
}

#[test]
fn detached_clears_caught_up_and_keeps_the_rows() {
    for kind in KINDS {
        let mut state = SessionState::new(agent(kind), CAP);
        apply_checked(
            &mut state,
            ev_snapshot(snapshot(kind, 2, Phase::Idle, &[], &[])),
        );
        apply_checked(&mut state, ev_item(text(kind, 1, 2, "a")));
        apply_checked(&mut state, caught_up(2));
        assert!(state.can_send());
        let outcome = apply_checked(&mut state, detached());
        assert!(outcome.changed.is_empty());
        assert!(!state.caught_up());
        assert!(!state.can_send());
        assert_eq!(state.transcript().len(), 1);
    }
}

#[test]
fn a_retail_that_meets_the_window_merges_without_a_reload() {
    for kind in KINDS {
        let mut state = SessionState::new(agent(kind), CAP);
        apply_checked(
            &mut state,
            ev_snapshot(snapshot(kind, 5, Phase::Idle, &[], &[])),
        );
        for order in 1..=5 {
            apply_checked(&mut state, ev_item(text(kind, order, order, "a")));
        }
        apply_checked(&mut state, caught_up(5));
        apply_checked(&mut state, Msg::Connection(Connection::Reconnecting));
        assert_eq!(state.composer(), Composer::Holds(Waiting::Reconnecting));
        apply_checked(&mut state, Msg::Connection(Connection::Live));
        apply_checked(
            &mut state,
            ev_snapshot(snapshot(kind, 7, Phase::Idle, &[], &[])),
        );
        // The re-tail overlaps the held window: deduped by key.
        for order in 4..=7 {
            apply_checked(&mut state, ev_item(text(kind, order, order.max(6), "b")));
        }
        let outcome = apply_checked(&mut state, caught_up(7));
        assert!(!outcome.reloaded);
        assert_eq!(state.oldest_order(), Some(1));
        assert_eq!(state.transcript().head(), Some(7));
    }
}

#[test]
fn a_reopen_under_the_generation_a_live_reset_named_continues_the_window() {
    for kind in KINDS {
        for opened_under in [0, 1] {
            // Opened under generation 1, or on a replica no source had
            // caught up yet, which names none (0).
            let mut state = SessionState::new(agent(kind), CAP);
            apply_checked(&mut state, opening(opened_under));
            apply_checked(
                &mut state,
                ev_snapshot(snapshot(kind, 3, Phase::Idle, &[], &[])),
            );
            for order in 1..=3 {
                apply_checked(&mut state, ev_item(text(kind, order, order, "old")));
            }
            apply_checked(&mut state, caught_up(3));

            // The replica takes the block of generation 2, live.
            apply_checked(&mut state, reset(2));
            apply_checked(
                &mut state,
                ev_snapshot(snapshot(kind, 9, Phase::Idle, &[], &[])),
            );
            for order in 4..=5 {
                apply_checked(&mut state, ev_item(text(kind, order, order + 4, "new")));
            }
            let outcome = apply_checked(&mut state, caught_up(9));
            assert!(outcome.reloaded, "the Reset swaps its block in");

            // A reader pages below the fresh block.
            apply_page(&mut state, vec![text(kind, 3, 7, "paged")], false);
            assert_eq!(state.oldest_order(), Some(3));

            // The stream lags and reopens under generation 2 with a tail
            // that meets the window: the window continues.
            apply_checked(&mut state, lagged());
            apply_checked(&mut state, Msg::Connection(Connection::Live));
            apply_checked(&mut state, opening(2));
            apply_checked(
                &mut state,
                ev_snapshot(snapshot(kind, 10, Phase::Idle, &[], &[])),
            );
            for order in 5..=6 {
                apply_checked(&mut state, ev_item(text(kind, order, order + 4, "new")));
            }
            let outcome = apply_checked(&mut state, caught_up(10));
            assert!(
                !outcome.reloaded,
                "opened under {opened_under}: a reopen under the Reset's generation does not \
                 rebuild"
            );
            assert_eq!(state.oldest_order(), Some(3), "the paged row stays");
            assert_eq!(state.transcript().head(), Some(6));
            assert_eq!(state.generation(), Some(2));
        }
    }
}

#[test]
fn nothing_is_kept_between_opens() {
    for kind in KINDS {
        let mut first = SessionState::new(agent(kind), CAP);
        apply_checked(
            &mut first,
            ev_snapshot(snapshot(kind, 2, Phase::Idle, &[], &[])),
        );
        apply_checked(&mut first, ev_item(text(kind, 1, 2, "a")));
        apply_checked(&mut first, Msg::Send(prompt_input(kind, b"p1", "hi")));
        let reopened = SessionState::new(agent(kind), CAP);
        assert_eq!(reopened, SessionState::new(agent(kind), CAP));
        assert!(reopened.transcript().is_empty());
        assert_eq!(reopened.inputs().iter().count(), 0);
        assert!(!reopened.has_snapshot());
    }
}

#[test]
fn a_transcript_that_fails_its_check_says_so_at_caught_up_and_at_reset() {
    for kind in KINDS {
        let mut state = SessionState::new(agent(kind), 10);
        state.update(ev_snapshot(snapshot(kind, 1, Phase::Working, &[], &[])));
        // An origin skipping an order breaks the block the client holds.
        state.update(ev_item(text(kind, 1, 1, "one")));
        state.update(ev_item(text(kind, 3, 3, "three")));
        let outcome = state.update(caught_up(3));
        let broken = outcome.broken.expect("a gap is a broken transcript");
        assert!(broken.contains("between 1 and 3"), "{broken}");
        let outcome = state.update(reset(2));
        assert!(outcome.broken.is_some(), "{outcome:?}");

        let mut state = SessionState::new(agent(kind), 10);
        apply_block(
            &mut state,
            ev_snapshot(snapshot(kind, 1, Phase::Working, &[], &[])),
        );
        for order in 1..=3 {
            apply_block(&mut state, ev_item(text(kind, order, order, "held")));
        }
        assert_eq!(apply_block(&mut state, caught_up(3)).broken, None);
        assert_eq!(apply_block(&mut state, reset(2)).broken, None);
    }
}
