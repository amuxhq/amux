//! A person's acts on a chat as inputs, under fresh ids. The acts are the
//! same on every kind; which message carries them is the kind's, which
//! [`ui_view::act_input`] knows.

use ui_view::Act;
use wire::{
    AgentCapabilities, Attachment, Input, Kind, PromptInput, PromptSubmission, SendQueuedNow,
    StopJob, WithdrawQueued,
};

/// A fresh client-generated input id.
pub fn input_id() -> Vec<u8> {
    uuid::Uuid::new_v4().as_bytes().to_vec()
}

fn wrap(kind: Kind, act: Act) -> Option<Input> {
    Some(Input::new(input_id(), ui_view::act_input(kind, act)?))
}

/// A first prompt for a create or a resume, under a fresh id.
pub fn submission(text: &str, attachments: Vec<Attachment>) -> PromptSubmission {
    PromptSubmission {
        input_id: input_id(),
        prompt: Some(PromptInput {
            text: text.to_owned(),
            attachments,
        }),
    }
}

/// A first prompt as the kind's prompt input under the same id, as the
/// chat shows it while it is on its way; `None` for an unknown kind.
pub fn submitted(kind: Kind, submission: &PromptSubmission) -> Option<Input> {
    let mut input = wrap(
        kind,
        Act::Prompt(submission.prompt.clone().unwrap_or_default()),
    )?;
    input.input_id = submission.input_id.clone();
    Some(input)
}

/// A prompt in the kind's arm, under a fresh id; `None` for an unknown kind.
pub fn prompt(kind: Kind, text: &str, attachments: Vec<Attachment>) -> Option<Input> {
    wrap(
        kind,
        Act::Prompt(PromptInput {
            text: text.to_owned(),
            attachments,
        }),
    )
}

pub fn withdraw(kind: Kind, queued_input_id: &[u8]) -> Option<Input> {
    wrap(
        kind,
        Act::Withdraw(WithdrawQueued {
            queued_input_id: queued_input_id.to_vec(),
        }),
    )
}

/// Steers a queued prompt into the running turn; None for an agent not
/// known to take one into its running turn, which sends it when the turn
/// ends.
pub fn send_now(kind: Kind, can: &AgentCapabilities, queued_input_id: &[u8]) -> Option<Input> {
    if !can.sends_now {
        return None;
    }
    wrap(
        kind,
        Act::SendNow(SendQueuedNow {
            queued_input_id: queued_input_id.to_vec(),
        }),
    )
}

pub fn interrupt(kind: Kind) -> Option<Input> {
    wrap(kind, Act::Interrupt)
}

/// Stops one background job by its key; None for an agent that cannot
/// stop one job (terminal Claude, an ACP agent).
pub fn stop_job(kind: Kind, can: &AgentCapabilities, key: &str) -> Option<Input> {
    let stop = StopJob {
        key: key.to_owned(),
    };
    let mut input = wrap(kind, Act::StopJob(stop)).filter(|_| can.stops_job)?;
    // The session gives it its id as it sends it.
    input.input_id = Vec::new();
    Some(input)
}

/// Takes the agent's own view back to its conversation; None for an agent
/// whose view cannot switch away (anything but terminal Claude).
pub fn switch_back(kind: Kind, can: &AgentCapabilities) -> Option<Input> {
    let mut input = wrap(kind, Act::SwitchBack).filter(|_| can.switches_back)?;
    // The session gives it its id as it sends it.
    input.input_id = Vec::new();
    Some(input)
}
