//! Asks: opened and closed only by snapshots, drawn before CaughtUp only
//! when the entry says needs_you, dismissed when the agent has exited.

use ui_state::{InputState, Msg, Refusal, SessionState};
use wire::{Kind, Phase};

use crate::spec::harness::*;

#[test]
fn an_ask_is_drawn_before_caught_up_only_when_the_entry_says_needs_you() {
    for kind in KINDS {
        // Answered from another device: the entry already says working, the
        // cached snapshot still lists the ask.
        let mut state = SessionState::new(with_phase(agent(kind), Phase::Working), CAP);
        apply_checked(
            &mut state,
            ev_snapshot(snapshot(kind, 5, Phase::NeedsYou, &["ask-1"], &[])),
        );
        assert!(
            state.open_asks().is_empty(),
            "no flash from a cached snapshot"
        );
        apply_checked(
            &mut state,
            ev_snapshot(snapshot(kind, 6, Phase::Working, &[], &[])),
        );
        apply_checked(&mut state, caught_up(6));
        assert!(state.open_asks().is_empty());

        // Still open: the entry says needs_you, so the card draws at once.
        let mut state = SessionState::new(with_phase(agent(kind), Phase::NeedsYou), CAP);
        apply_checked(
            &mut state,
            ev_snapshot(snapshot(kind, 5, Phase::NeedsYou, &["ask-1"], &[])),
        );
        let keys: Vec<&str> = state.open_asks().iter().map(|ask| ask.key()).collect();
        assert_eq!(keys, ["ask-1"]);

        // After CaughtUp the snapshot alone decides.
        let mut state = SessionState::new(with_phase(agent(kind), Phase::Idle), CAP);
        apply_checked(
            &mut state,
            ev_snapshot(snapshot(kind, 5, Phase::NeedsYou, &["ask-1"], &[])),
        );
        assert!(state.open_asks().is_empty());
        apply_checked(&mut state, caught_up(5));
        assert_eq!(
            state.open_asks().len(),
            1,
            "the accepted flip when CaughtUp lands"
        );
    }
}

#[test]
fn asks_keep_the_interpreters_order_and_the_head_comes_first() {
    for kind in KINDS {
        let mut state = SessionState::new(with_phase(agent(kind), Phase::NeedsYou), CAP);
        apply_checked(
            &mut state,
            ev_snapshot(snapshot(
                kind,
                5,
                Phase::NeedsYou,
                &["ask-1", "ask-2", "ask-3"],
                &[],
            )),
        );
        apply_checked(&mut state, caught_up(5));
        let keys: Vec<&str> = state.open_asks().iter().map(|ask| ask.key()).collect();
        assert_eq!(keys, ["ask-1", "ask-2", "ask-3"]);
    }
}

/// The person answers; the reply is only the interpreter's verdict. The card
/// shows "sending" and stays until a snapshot without the ask arrives: an
/// ask closes on a provider fact, never on a reply or a timer.
#[test]
fn ask_closes_on_fact() {
    let kind = Kind::ClaudePty;
    let mut tape = Tape::new(with_phase(agent(kind), Phase::NeedsYou));
    tape.apply(
        "snapshot: needs you",
        ev_snapshot(snapshot(kind, 10, Phase::NeedsYou, &["toolu_1"], &[])),
    );
    tape.apply(
        "tool call awaiting approval",
        ev_item(command(kind, 1, 10, wire::ToolState::Pending)),
    );
    tape.apply("CaughtUp", caught_up(10));
    assert_eq!(tape.state.open_asks().len(), 1);

    tape.apply(
        "answer: allow",
        Msg::Send(answer_input(kind, b"a1", "toolu_1")),
    );
    assert_eq!(
        tape.state.answering("toolu_1").map(|sent| &sent.state),
        Some(&InputState::Sent)
    );
    assert_eq!(tape.state.open_asks().len(), 1, "sending: the card stays");

    tape.apply(
        "reply: accepted",
        Msg::Sent(b"a1".to_vec(), accepted(false)),
    );
    assert_eq!(tape.state.input_state(b"a1"), Some(InputState::Settled));
    assert_eq!(
        tape.state.open_asks().len(),
        1,
        "a reply is not the closing fact"
    );

    tape.apply(
        "tool runs",
        ev_item(command(kind, 1, 11, wire::ToolState::Running)),
    );
    assert_eq!(tape.state.open_asks().len(), 1);

    tape.apply(
        "snapshot: ask closed",
        ev_snapshot(snapshot(kind, 12, Phase::Working, &[], &[])),
    );
    assert!(
        tape.state.open_asks().is_empty(),
        "the snapshot without the ask closes it"
    );
    tape.print("ask_closes_on_fact");
}

#[test]
fn a_stale_answer_comes_back_rejected_and_the_card_returns_with_the_reason() {
    for kind in KINDS {
        let mut state = SessionState::new(with_phase(agent(kind), Phase::NeedsYou), CAP);
        apply_checked(
            &mut state,
            ev_snapshot(snapshot(kind, 5, Phase::NeedsYou, &["ask-1"], &[])),
        );
        apply_checked(&mut state, caught_up(5));
        apply_checked(&mut state, Msg::Send(answer_input(kind, b"a1", "ask-1")));
        apply_checked(
            &mut state,
            Msg::Sent(b"a1".to_vec(), rejected(wire::Refusal::ClosedAsk)),
        );
        let answer = state.answering("ask-1").unwrap();
        assert_eq!(
            answer.state,
            InputState::Rejected(Refusal::of(wire::Refusal::ClosedAsk))
        );
        assert_eq!(state.open_asks().len(), 1, "only a fact closes it");
    }
}

#[test]
fn an_exited_agents_open_asks_are_drawn_dismissed() {
    for kind in KINDS {
        let mut state = SessionState::new(with_phase(agent(kind), Phase::NeedsYou), CAP);
        apply_checked(
            &mut state,
            ev_snapshot(snapshot(kind, 5, Phase::NeedsYou, &["ask-1"], &[])),
        );
        apply_checked(&mut state, caught_up(5));
        assert!(!state.asks_dismissed());
        apply_checked(
            &mut state,
            Msg::Entry(exited(agent(kind), wire::ExitCause::Killed)),
        );
        assert_eq!(state.open_asks().len(), 1);
        assert!(state.asks_dismissed());
        assert_eq!(
            state.phase(),
            ui_state::PhaseView::Exited {
                cause: ui_state::ExitCause::Ended
            }
        );
    }
}
