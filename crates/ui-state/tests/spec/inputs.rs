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
            reset(1),
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

/// A prompt refused because its host could not be reached certainly never
/// left: it is held, never uncertain, and goes again under its own id once
/// the chat has caught up again.
#[test]
fn a_host_unreachable_rejection_holds_the_prompt_until_the_chat_catches_up() {
    for kind in KINDS {
        let mut state = live(kind, Phase::Idle, &[]);
        apply_checked(&mut state, Msg::Send(prompt_input(kind, b"p1", "go")));
        apply_checked(
            &mut state,
            Msg::Sent(b"p1".to_vec(), rejected(wire::Refusal::HostUnreachable)),
        );
        assert_eq!(state.input_state(b"p1"), Some(InputState::Held));
        assert_eq!(state.not_confirmed().count(), 0);
        assert_eq!(state.held().count(), 1);
        let outcome = apply_checked(
            &mut state,
            Msg::Event(wire::SessionEvent {
                of: Some(wire::session_event::Of::CaughtUp(wire::CaughtUp::default())),
            }),
        );
        assert_eq!(
            outcome
                .resend
                .iter()
                .map(|input| input.input_id.clone())
                .collect::<Vec<_>>(),
            vec![b"p1".to_vec()],
            "caught up, the held prompt goes again under its id"
        );
        assert_eq!(state.input_state(b"p1"), Some(InputState::Sent));
    }
}

fn host(presence: wire::Presence) -> Msg {
    Msg::Host(wire::HostEntry {
        host_id: b"host".to_vec(),
        name: "desk".into(),
        presence: presence as i32,
        ..Default::default()
    })
}

fn resent(outcome: &ui_state::Outcome) -> Vec<Vec<u8>> {
    outcome
        .resend
        .iter()
        .map(|input| input.input_id.clone())
        .collect()
}

/// The host's word that it is back can land while a send to it is still in
/// flight, and the send then fails as unreachable: the host is online and
/// the chat current, so the prompt goes again at once instead of waiting
/// for a return that already happened.
#[test]
fn a_prompt_held_after_its_host_came_back_goes_at_once() {
    for kind in KINDS {
        let mut state = live(kind, Phase::Idle, &[]);
        apply_checked(&mut state, host(wire::Presence::Away));
        apply_checked(&mut state, Msg::Send(prompt_input(kind, b"p1", "go")));
        apply_checked(&mut state, host(wire::Presence::Online));
        let outcome = apply_checked(
            &mut state,
            Msg::Sent(b"p1".to_vec(), rejected(wire::Refusal::HostUnreachable)),
        );
        assert_eq!(resent(&outcome), vec![b"p1".to_vec()]);
        assert_eq!(state.input_state(b"p1"), Some(InputState::Sent));
    }
}

/// Held while its host read online, a prompt goes at the next word that
/// the host is online, with no away in between; one that failed under the
/// newest word waits for it instead of going again at once.
#[test]
fn a_prompt_held_while_its_host_read_online_goes_at_its_next_online_word() {
    for kind in KINDS {
        let mut state = live(kind, Phase::Idle, &[]);
        apply_checked(&mut state, host(wire::Presence::Online));
        apply_checked(&mut state, Msg::Send(prompt_input(kind, b"p1", "go")));
        let outcome = apply_checked(
            &mut state,
            Msg::Sent(b"p1".to_vec(), rejected(wire::Refusal::HostUnreachable)),
        );
        assert!(outcome.resend.is_empty());
        assert_eq!(state.input_state(b"p1"), Some(InputState::Held));
        let outcome = apply_checked(&mut state, host(wire::Presence::Online));
        assert_eq!(resent(&outcome), vec![b"p1".to_vec()]);
        assert_eq!(state.input_state(b"p1"), Some(InputState::Sent));
        let outcome = apply_checked(&mut state, host(wire::Presence::Online));
        assert!(outcome.resend.is_empty(), "each held prompt goes once");
    }
}
