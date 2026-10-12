//! Opening a chat and the stream markers: CaughtUp, Detached, Lagged,
//! Reset, and a re-tail after the runtime connection drops.

use ui_state::{Composer, Connection, Msg, SessionState, Waiting};
use wire::{Kind, Phase};

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

/// The origin rewound: its stream sends Reset, a fresh tail and CaughtUp.
/// The rows on screen stay until that CaughtUp swaps the rebuilt transcript
/// in, and the composer is live only after it.
#[test]
fn reset_swaps_at_caught_up() {
    let kind = Kind::ClaudeSdk;
    let mut tape = Tape::new(agent(kind));
    tape.apply(
        "snapshot",
        ev_snapshot(snapshot(kind, 220, Phase::Idle, &[], &[])),
    );
    for order in 300..=303 {
        tape.apply("held", ev_item(text(kind, order, 200 + order, "stale")));
    }
    tape.apply("CaughtUp", caught_up(503));
    assert!(tape.state.can_send());
    let on_screen = projection(tape.state.transcript());

    let outcome = tape.apply("Reset", reset());
    assert!(
        outcome.changed.is_empty(),
        "a Reset changes nothing on screen"
    );
    assert!(
        !tape.state.can_send(),
        "the composer waits for the rebuilt transcript"
    );
    tape.apply(
        "snapshot after rewind",
        ev_snapshot(snapshot(kind, 214, Phase::Idle, &[], &[])),
    );
    for order in 701..=703 {
        let outcome = tape.apply("fresh tail", ev_item(text(kind, order, order, "rebuilt")));
        assert!(
            outcome.changed.is_empty(),
            "nothing is drawn before the swap"
        );
    }
    let outcome = tape.apply("fresh append", ev_append("k703", 703, 704, "!"));
    assert!(outcome.changed.is_empty() && outcome.need_get.is_none());
    assert_eq!(
        projection(tape.state.transcript()),
        on_screen,
        "the stale rows stay on screen"
    );
    assert!(tape.state.reset_pending());

    let epoch = tape.state.epoch();
    let outcome = tape.apply("CaughtUp", caught_up(704));
    assert!(outcome.reloaded);
    assert_eq!(
        outcome.changed,
        vec!["k300", "k301", "k302", "k303", "k701", "k702", "k703"],
        "every row that left or arrived"
    );
    assert!(tape.state.can_send());
    assert_eq!(tape.state.oldest_order(), Some(701));
    assert_eq!(
        tape.state.transcript().get("k703").unwrap().item.text,
        "rebuilt!"
    );
    assert_eq!(tape.state.epoch(), epoch + 1);

    // A page requested before the swap describes the transcript that is gone.
    let late = Msg::Page {
        items: vec![text(kind, 299, 1, "late")],
        exhausted: false,
        epoch,
    };
    let outcome = tape.apply("late page from before the swap", late);
    assert!(outcome.changed.is_empty());
    assert_eq!(tape.state.oldest_order(), Some(701));
    tape.print("reset_swaps_at_caught_up");
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
fn a_retail_past_a_gap_builds_apart_and_swaps_at_caught_up() {
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
        apply_checked(&mut state, lagged());
        assert!(
            !state.can_send(),
            "Lagged closes the stream; the driver re-tails"
        );
        apply_checked(
            &mut state,
            ev_snapshot(snapshot(kind, 90, Phase::Idle, &[], &[])),
        );
        for order in 80..=90 {
            let outcome = apply_checked(&mut state, ev_item(text(kind, order, order, "b")));
            assert!(
                outcome.changed.is_empty(),
                "a tail that leaves a gap is not appended"
            );
        }
        assert_eq!(state.transcript().head(), Some(5));
        let outcome = apply_checked(&mut state, caught_up(90));
        assert!(outcome.reloaded);
        assert_eq!(state.oldest_order(), Some(80));
        assert!(state.can_send());
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
