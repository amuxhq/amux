//! The interpreter's state an incarnation starts from, and the journal
//! steps it starts with.

use interpret::ring::Entry;
use interpret::{Event, Interpreter};
use wire::{AgentSpec, Step};

/// What the previous incarnation left in the facts ring: the interpreter's
/// state at the newest checkpoint and every event recorded after it.
pub struct Resumed<S> {
    pub state: S,
    pub inputs: Vec<Entry>,
}

/// The interpreter's state for this incarnation and the steps it starts
/// with. A first incarnation starts from nothing. A later one continues the
/// state the last one ended in, rebuilt from the ring's newest checkpoint
/// and the events after it, so its item keys carry on from there. An
/// incarnation that ended without its final boundary (killed, or crashed)
/// gets it now, then the resume step re-emits whatever is still open in
/// full.
pub(crate) fn starting<I: Interpreter>(
    spec: &AgentSpec,
    resumed: Option<Resumed<I::State>>,
) -> (I::State, Vec<Step>) {
    let Some(Resumed { mut state, inputs }) = resumed else {
        let (state, step) = I::initial(spec);
        return (state, vec![step]);
    };
    let ended = inputs
        .last()
        .is_none_or(|entry| matches!(entry, Entry::ProviderExit { .. } | Entry::Exiting { .. }));
    // The steps were journaled when first made; an entry this build cannot
    // read is skipped, as it always was.
    interpret::ring::rerun::<I>(&mut state, inputs);
    let mut steps = Vec::new();
    if !ended {
        let mut stepped = I::step(
            &mut state,
            Event::Exiting(wire::IncarnationEnd {
                cause: wire::ExitCause::EndedUnexpectedly as i32,
                detail: None,
            }),
        );
        // The incarnation this boundary closes was marked exited, with the
        // daemon's own cause, when its process was found gone; a cause here
        // would be read as this incarnation's.
        stepped.step.incarnation_end = None;
        steps.push(stepped.step);
    }
    let (state, resume) = I::reincarnate(state, spec);
    steps.push(resume);
    (state, steps)
}
