//! The five input states, resolution at CaughtUp, the exited composer and
//! steering a queued prompt.

use ui_state::{Composer, InputOutcome, InputState, Msg, Refusal, RefusalReason, SessionState};
use wire::{Kind, Phase};

use crate::harness::*;

fn live(kind: Kind, phase: Phase, queue: &[(&[u8], bool)]) -> SessionState {
    let mut state = SessionState::new(with_phase(agent(kind), phase), CAP);
    apply_checked(
        &mut state,
        ev_snapshot(snapshot(kind, 5, phase, &[], queue)),
    );
    apply_checked(&mut state, ev_item(text(kind, 1, 5, "hello")));
    apply_checked(&mut state, caught_up(5));
    state
}

#[test]
fn a_prompt_is_sent_then_settled_by_its_reflection() {
    for kind in KINDS {
        let mut state = live(kind, Phase::Idle, &[]);
        apply_checked(&mut state, Msg::Send(prompt_input(kind, b"p1", "go")));
        assert_eq!(state.input_state(b"p1"), Some(InputState::Sent));
        assert_eq!(state.sending().count(), 1, "the optimistic row exists");
        apply_checked(&mut state, Msg::Sent(b"p1".to_vec(), accepted(false)));
        assert_eq!(
            state.input_state(b"p1"),
            Some(InputState::Sent),
            "accepted, waiting for the reflection"
        );
        apply_checked(&mut state, ev_item(prompt(kind, 2, 6, "go", b"p1")));
        assert_eq!(state.input_state(b"p1"), Some(InputState::Settled));
        assert_eq!(
            state.sending().count(),
            0,
            "the reflection replaces the optimistic row"
        );
    }
}

#[test]
fn a_reflection_that_beats_the_reply_settles_it() {
    for kind in KINDS {
        let mut state = live(kind, Phase::Idle, &[]);
        apply_checked(&mut state, Msg::Send(prompt_input(kind, b"p1", "go")));
        apply_checked(&mut state, ev_item(prompt(kind, 2, 6, "go", b"p1")));
        apply_checked(&mut state, Msg::Sent(b"p1".to_vec(), accepted(false)));
        assert_eq!(state.input_state(b"p1"), Some(InputState::Settled));
    }
}

#[test]
fn a_queued_prompt_is_queued_until_it_leaves_the_queue() {
    for kind in KINDS {
        let mut state = live(kind, Phase::Working, &[]);
        apply_checked(&mut state, Msg::Send(prompt_input(kind, b"p1", "next")));
        apply_checked(&mut state, Msg::Sent(b"p1".to_vec(), accepted(true)));
        assert_eq!(state.input_state(b"p1"), Some(InputState::Queued));
        // A snapshot from before the queueing does not settle it.
        apply_checked(
            &mut state,
            ev_snapshot(snapshot(kind, 6, Phase::Working, &[], &[])),
        );
        assert_eq!(state.input_state(b"p1"), Some(InputState::Queued));
        apply_checked(
            &mut state,
            ev_snapshot(snapshot(kind, 7, Phase::Working, &[], &[(b"p1", false)])),
        );
        let queue = state.queue();
        assert_eq!(queue.len(), 1);
        assert!(!queue[0].steered);
        apply_checked(
            &mut state,
            ev_snapshot(snapshot(kind, 8, Phase::Working, &[], &[])),
        );
        assert_eq!(state.input_state(b"p1"), Some(InputState::Settled));
    }
}

/// The snapshot that lists a queued prompt can beat the reply accepting it.
/// Once the prompt has been seen listed, a snapshot without it means it
/// left the queue (withdrawn or submitted), whichever arrived first.
#[test]
fn a_queued_prompt_listed_before_its_reply_settles_when_it_leaves() {
    for kind in KINDS {
        // Withdrawn after the reply.
        let mut state = live(kind, Phase::Working, &[]);
        apply_checked(&mut state, Msg::Send(prompt_input(kind, b"p1", "next")));
        apply_checked(
            &mut state,
            ev_snapshot(snapshot(kind, 6, Phase::Working, &[], &[(b"p1", false)])),
        );
        apply_checked(&mut state, Msg::Sent(b"p1".to_vec(), accepted(true)));
        assert_eq!(state.input_state(b"p1"), Some(InputState::Queued));
        apply_checked(
            &mut state,
            ev_snapshot(snapshot(kind, 7, Phase::Working, &[], &[])),
        );
        assert_eq!(state.input_state(b"p1"), Some(InputState::Settled));

        // Withdrawn before the reply arrived.
        let mut state = live(kind, Phase::Working, &[]);
        apply_checked(&mut state, Msg::Send(prompt_input(kind, b"p2", "next")));
        apply_checked(
            &mut state,
            ev_snapshot(snapshot(kind, 6, Phase::Working, &[], &[(b"p2", false)])),
        );
        apply_checked(
            &mut state,
            ev_snapshot(snapshot(kind, 7, Phase::Working, &[], &[])),
        );
        apply_checked(&mut state, Msg::Sent(b"p2".to_vec(), accepted(true)));
        assert_eq!(state.input_state(b"p2"), Some(InputState::Settled));
    }
}

#[test]
fn a_rejection_is_final_and_touches_no_row() {
    for kind in KINDS {
        let mut state = live(kind, Phase::Idle, &[]);
        let before = projection(state.transcript());
        apply_checked(&mut state, Msg::Send(prompt_input(kind, b"p1", "go")));
        let outcome = apply_checked(
            &mut state,
            Msg::Sent(b"p1".to_vec(), rejected(wire::Refusal::Draining)),
        );
        assert!(outcome.changed.is_empty());
        assert_eq!(
            state.input_state(b"p1"),
            Some(InputState::Rejected(Refusal::of(RefusalReason::Draining)))
        );
        assert_eq!(projection(state.transcript()), before);
    }
}

#[test]
fn an_uncertain_input_found_in_the_queue_or_items_at_caught_up_resolves() {
    for kind in KINDS {
        let mut state = live(kind, Phase::Working, &[]);
        apply_checked(
            &mut state,
            Msg::Send(prompt_input(kind, b"q", "queued one")),
        );
        apply_checked(
            &mut state,
            Msg::Send(prompt_input(kind, b"r", "reflected one")),
        );
        apply_checked(
            &mut state,
            Msg::Connection(ui_state::Connection::Reconnecting),
        );
        apply_checked(&mut state, Msg::Sent(b"q".to_vec(), InputOutcome::Lost));
        apply_checked(&mut state, Msg::Sent(b"r".to_vec(), InputOutcome::Lost));
        assert_eq!(state.input_state(b"q"), Some(InputState::Uncertain));
        apply_checked(
            &mut state,
            ev_snapshot(snapshot(kind, 9, Phase::Working, &[], &[(b"q", false)])),
        );
        apply_checked(
            &mut state,
            ev_item(prompt(kind, 2, 8, "reflected one", b"r")),
        );
        assert_eq!(
            state.input_state(b"q"),
            Some(InputState::Uncertain),
            "judged only at CaughtUp"
        );
        assert_eq!(
            state.input_state(b"r"),
            Some(InputState::Uncertain),
            "judged only at CaughtUp"
        );
        assert!(
            state.queue().is_empty(),
            "an uncertain input is not shown as queued"
        );
        apply_checked(&mut state, caught_up(9));
        assert_eq!(state.input_state(b"q"), Some(InputState::Queued));
        assert_eq!(state.input_state(b"r"), Some(InputState::Settled));
        assert_eq!(state.queue().len(), 1);
    }
}

/// Input reply lost, then a withdrawal from another client: the phone finds
/// the prompt nowhere at CaughtUp and leaves it to the person.
#[test]
fn an_uncertain_input_absent_at_caught_up_is_left_with_resend_and_discard() {
    for kind in KINDS {
        let mut state = live(kind, Phase::Working, &[]);
        let input = prompt_input(kind, b"P", "do the thing");
        apply_checked(&mut state, Msg::Send(input.clone()));
        apply_checked(
            &mut state,
            ev_snapshot(snapshot(kind, 6, Phase::Working, &[], &[(b"P", false)])),
        );
        apply_checked(&mut state, Msg::Sent(b"P".to_vec(), InputOutcome::Lost));
        // The stale cached snapshot on reconnect still lists P.
        apply_checked(
            &mut state,
            Msg::Connection(ui_state::Connection::Reconnecting),
        );
        apply_checked(
            &mut state,
            ev_snapshot(snapshot(kind, 6, Phase::Working, &[], &[(b"P", false)])),
        );
        assert!(
            state.queue().is_empty(),
            "never shown as queued before CaughtUp"
        );
        // The terminal withdrew it; the origin's snapshot says so.
        apply_checked(
            &mut state,
            ev_snapshot(snapshot(kind, 7, Phase::Idle, &[], &[])),
        );
        apply_checked(&mut state, caught_up(7));
        assert_eq!(state.input_state(b"P"), Some(InputState::Uncertain));
        let not_confirmed: Vec<_> = state
            .not_confirmed()
            .map(|sent| sent.input.clone())
            .collect();
        assert_eq!(
            not_confirmed,
            vec![input.clone()],
            "resend carries the input as sent"
        );
        assert_eq!(state.inputs().iter().count(), 1, "nothing was resent");
        // Resend is a new input with a new id, judged afresh; discard drops it.
        let mut resend = input;
        resend.input_id = b"P2".to_vec();
        apply_checked(&mut state, Msg::Send(resend));
        apply_checked(&mut state, Msg::Discard(b"P".to_vec()));
        assert_eq!(state.input_state(b"P"), None);
        assert_eq!(state.input_state(b"P2"), Some(InputState::Sent));
    }
}

/// The origin-rewind case: the prompt withdrawn just before the reconnect is
/// never shown as queued, through the Reset and after the swap.
#[test]
fn a_prompt_withdrawn_before_a_reset_is_never_shown_as_queued() {
    for kind in KINDS {
        let mut state = live(kind, Phase::Working, &[]);
        apply_checked(
            &mut state,
            Msg::Send(prompt_input(kind, b"W", "withdraw me")),
        );
        apply_checked(&mut state, Msg::Sent(b"W".to_vec(), InputOutcome::Lost));
        let mut ever_queued = false;
        for msg in [
            ev_snapshot(snapshot(kind, 6, Phase::Working, &[], &[(b"W", false)])),
            reset(),
            ev_snapshot(snapshot(kind, 3, Phase::Idle, &[], &[])),
            ev_item(text(kind, 1, 1, "rebuilt")),
            caught_up(3),
        ] {
            apply_checked(&mut state, msg);
            ever_queued |=
                !state.queue().is_empty() || state.input_state(b"W") == Some(InputState::Queued);
        }
        assert!(!ever_queued);
        assert_eq!(state.input_state(b"W"), Some(InputState::Uncertain));
    }
}

#[test]
fn an_exited_chat_offers_resume_for_the_draft() {
    for kind in KINDS {
        let mut state = live(kind, Phase::Idle, &[]);
        apply_checked(
            &mut state,
            Msg::Entry(exited(agent(kind), wire::ExitCause::Finished)),
        );
        assert_eq!(state.composer(), Composer::Resume);
        assert!(!state.can_send());
        // Resume starts a new incarnation; the entry flips live.
        let mut resumed = agent(kind);
        resumed.incarnation = 2;
        apply_checked(&mut state, Msg::Entry(resumed));
        assert_eq!(state.composer(), Composer::Send);
    }
}

#[test]
fn a_send_that_raced_the_exit_flips_the_composer_to_resume() {
    for kind in KINDS {
        let mut state = live(kind, Phase::Idle, &[]);
        apply_checked(&mut state, Msg::Send(prompt_input(kind, b"p1", "draft")));
        apply_checked(
            &mut state,
            Msg::Sent(b"p1".to_vec(), rejected(wire::Refusal::Exited)),
        );
        assert_eq!(
            state.composer(),
            Composer::Resume,
            "before the entry says so"
        );
        assert_eq!(
            state.input_state(b"p1"),
            Some(InputState::Rejected(Refusal::of(RefusalReason::Exited)))
        );
        let draft = &state.inputs().get(b"p1").unwrap().input;
        assert_eq!(
            draft,
            &prompt_input(kind, b"p1", "draft"),
            "the draft is intact"
        );
        // A stale live entry of the same incarnation does not undo it.
        apply_checked(&mut state, Msg::Entry(agent(kind)));
        assert_eq!(state.composer(), Composer::Resume);
        let mut resumed = agent(kind);
        resumed.incarnation = 2;
        apply_checked(&mut state, Msg::Entry(resumed));
        assert_eq!(state.composer(), Composer::Send);
    }
}

#[test]
fn late_results_after_a_reopen_are_ignored() {
    for kind in KINDS {
        let mut state = live(kind, Phase::Idle, &[]);
        let before = state.clone();
        let outcome = apply_checked(
            &mut state,
            Msg::Sent(b"from-the-last-open".to_vec(), accepted(true)),
        );
        assert!(!outcome.session);
        assert_eq!(state, before);
    }
}

#[test]
fn send_now_marks_a_queued_prompt_steered_until_its_reflection() {
    for kind in KINDS {
        let mut state = live(kind, Phase::Working, &[]);
        apply_checked(
            &mut state,
            Msg::Send(prompt_input(kind, b"p1", "also check x")),
        );
        apply_checked(&mut state, Msg::Sent(b"p1".to_vec(), accepted(true)));
        apply_checked(
            &mut state,
            ev_snapshot(snapshot(kind, 6, Phase::Working, &[], &[(b"p1", false)])),
        );
        assert!(
            !state.queue()[0].steered,
            "prompts never steer on their own"
        );
        apply_checked(&mut state, Msg::Send(send_now_input(kind, b"s1", b"p1")));
        assert!(
            state.queue()[0].steered,
            "steered while Send now is in flight"
        );
        apply_checked(&mut state, Msg::Sent(b"s1".to_vec(), accepted(false)));
        assert_eq!(state.input_state(b"s1"), Some(InputState::Settled));
        apply_checked(
            &mut state,
            ev_snapshot(snapshot(kind, 7, Phase::Working, &[], &[(b"p1", true)])),
        );
        assert!(state.queue()[0].steered, "the queue's steer flag holds it");
        let mut steer = item(kind, 2, 8, "also check x", Body::Prompt);
        steer.input_id = b"p1".to_vec();
        apply_checked(&mut state, ev_item(steer));
        apply_checked(
            &mut state,
            ev_snapshot(snapshot(kind, 8, Phase::Working, &[], &[])),
        );
        assert!(state.queue().is_empty());
        assert_eq!(state.input_state(b"p1"), Some(InputState::Settled));
    }
}

#[test]
fn a_host_unreachable_rejection_is_never_uncertain() {
    for kind in KINDS {
        let mut state = live(kind, Phase::Idle, &[]);
        apply_checked(&mut state, Msg::Send(prompt_input(kind, b"p1", "go")));
        apply_checked(
            &mut state,
            Msg::Sent(b"p1".to_vec(), rejected(wire::Refusal::HostUnreachable)),
        );
        assert_eq!(
            state.input_state(b"p1"),
            Some(InputState::Rejected(Refusal::of(
                RefusalReason::HostUnreachable
            )))
        );
        assert_eq!(state.not_confirmed().count(), 0);
    }
}
