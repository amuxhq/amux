//! Whether a session's record is enough: replayed beside the whole history
//! it was made from, it must leave the session model in the same structure
//! after every message, with the same outcome, and break the same check at
//! the same step. A difference names a body field the model reads and the
//! record left out.

use std::fmt::Write as _;

use ui_state::{
    ActivityKind, ExitCause, InputState, InputWhat, Msg, Outcome, PhaseView, SessionState,
};
use wire::dump::{ClientSessionHistory, client_traced};

use crate::bundle::ClientHistory;
use crate::replay::{Failing, replay_client};

/// The verdict both replays reach: how many messages held, or the step
/// that broke the window, the same for the record as for the history.
pub fn replays_alike(whole: &ClientSessionHistory) -> Result<Result<usize, Failing>, String> {
    let record = ui_state::record(whole.clone());
    let whole_steps = steps(whole)?;
    let record_steps = steps(&record)?;
    if whole_steps.len() != record_steps.len() {
        return Err(format!(
            "{} messages in the history, {} in its record",
            whole_steps.len(),
            record_steps.len()
        ));
    }
    for (step, (whole, record)) in whole_steps.iter().zip(&record_steps).enumerate() {
        if whole != record {
            return Err(format!(
                "after message {step} the record differs:\n  history: {whole}\n  record:  {record}"
            ));
        }
    }
    let verdict = replay_client("whole", &history(whole))?;
    let recorded = replay_client("whole", &history(&record))?;
    if verdict != recorded {
        return Err(format!(
            "the history replays to {verdict:?}, its record to {recorded:?}"
        ));
    }
    Ok(verdict)
}

fn history(history: &ClientSessionHistory) -> ClientHistory {
    ClientHistory {
        start: history.start.clone().unwrap_or_default(),
        events: history.events.clone().unwrap_or_default().traced,
    }
}

/// The model's structure at its checkpoint and after each message, beside
/// that message's outcome.
fn steps(history: &ClientSessionHistory) -> Result<Vec<String>, String> {
    let mut state = SessionState::from_wire(history.start.clone().unwrap_or_default())?;
    let mut now = 0;
    let mut steps = vec![structure(&state, now)];
    for traced in history.events.iter().flat_map(|events| &events.traced) {
        now = now.max(traced.at_ms);
        let Some(client_traced::Event::Msg(msg)) = &traced.event else {
            continue;
        };
        let outcome = state.update(Msg::from_wire(msg.clone())?);
        let mut step = structure(&state, now);
        let _ = write!(step, " | {}", outcome_structure(&outcome));
        steps.push(step);
    }
    Ok(steps)
}

fn outcome_structure(outcome: &Outcome) -> String {
    format!(
        "changed {:?} need_get {:?} reloaded {} session {} reload {} broken {:?}",
        outcome.changed,
        outcome.need_get,
        outcome.reloaded,
        outcome.session,
        outcome.reload,
        outcome.broken
    )
}

/// Everything the session model concludes, without what a view draws
/// from a body: the window's rows by key, order, revision and class, the
/// runs, the inputs, the asks, the queue, the phase and the activity line.
fn structure(state: &SessionState, now_ms: i64) -> String {
    let mut out = String::new();
    let transcript = state.transcript();
    for held in transcript.iter() {
        let item = &held.item;
        let _ = write!(
            out,
            "[{} o{} r{} {:?} in={} at={} att={}]",
            item.key,
            item.order,
            item.revision,
            held.class,
            hex(&item.input_id),
            item.at_ms,
            item.attachments.len()
        );
    }
    let runs: Vec<_> = transcript.runs().spans().collect();
    let turns: Vec<bool> = transcript
        .iter()
        .map(|held| transcript.turn_ended_after(held.item.order))
        .collect();
    let _ = write!(
        out,
        " runs {runs:?} turns {turns:?} older {} pending {} arrivals {} moved_on {} held {}",
        transcript.has_older(),
        state.reset_pending(),
        state.held_arrivals(),
        state.head_moved_on(),
        state.arrivals_held()
    );
    let _ = write!(
        out,
        " caught_up {} {:?} generation {:?} epoch {} cap {} following {} snapshot {} can_send {}",
        state.caught_up(),
        state.connection(),
        state.generation(),
        state.epoch(),
        state.cap(),
        state.following(),
        state.has_snapshot(),
        state.can_send()
    );
    let phase = match state.phase() {
        // The host's own words for an exit are out of the record.
        PhaseView::Exited {
            cause: ExitCause::Failed(_),
        } => "Exited(failed)".to_owned(),
        phase => format!("{phase:?}"),
    };
    let agent = state.agent_state();
    let asks: Vec<(&str, &str)> = state
        .open_asks()
        .iter()
        .map(|ask| (ask.key(), ask.item_key()))
        .collect();
    let _ = write!(
        out,
        " phase {phase} composer {:?} asks {asks:?} dismissed {} revision {} at {} catalogue {:?} held {:?}",
        state.composer(),
        state.asks_dismissed(),
        agent.revision,
        agent.at_ms,
        agent.catalogue,
        state.held_catalogue()
    );
    let queue: Vec<(String, bool)> = state
        .queue()
        .iter()
        .map(|row| (hex(&row.entry.input_id), row.steered))
        .collect();
    let _ = write!(out, " queue {queue:?}");
    for sent in state.inputs().iter() {
        let what = match &sent.what {
            InputWhat::Prompt { .. } => "prompt".to_owned(),
            InputWhat::Answer { ask_key } => format!("answer {ask_key}"),
            InputWhat::Withdraw { target } => format!("withdraw {}", hex(target)),
            InputWhat::SendNow { target } => format!("send-now {}", hex(target)),
            InputWhat::Interrupt => "interrupt".to_owned(),
            InputWhat::Other => "other".to_owned(),
        };
        let state = match &sent.state {
            // The agent's own words for a refusal are out of the record.
            InputState::Rejected(refusal) => format!("Rejected({:?})", refusal.reason),
            state => format!("{state:?}"),
        };
        let _ = write!(
            out,
            " input {} {what} {state} feed {}",
            hex(&sent.id),
            sent.in_feed
        );
    }
    if let Some(activity) = state.activity(now_ms) {
        let kind = match &activity.kind {
            ActivityKind::Running { key } => format!("running {key}"),
            kind => format!("{kind:?}"),
        };
        let _ = write!(
            out,
            " activity {kind} since {} turn {}",
            activity.since_ms, activity.turn_elapsed_ms
        );
    }
    out
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
