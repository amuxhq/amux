//! A person's acts on a chat as inputs: the same message for every kind,
//! each under a fresh client-generated id.

use wire::agent_input::Of;
use wire::{
    AgentInput, Attachment, Input, Interrupt, PromptInput, PromptSubmission, SendQueuedNow,
    WithdrawQueued,
};

/// A fresh client-generated input id.
pub fn input_id() -> Vec<u8> {
    uuid::Uuid::new_v4().as_bytes().to_vec()
}

fn input(of: Of) -> Input {
    Input::new(input_id(), AgentInput { of: Some(of) })
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

/// A first prompt as a prompt input under the same id, as the chat shows
/// it while it is on its way.
pub fn submitted(submission: &PromptSubmission) -> Input {
    let mut input = input(Of::Prompt(submission.prompt.clone().unwrap_or_default()));
    input.input_id = submission.input_id.clone();
    input
}

pub fn prompt(text: &str, attachments: Vec<Attachment>) -> Input {
    input(Of::Prompt(PromptInput {
        text: text.to_owned(),
        attachments,
    }))
}

pub fn withdraw(queued_input_id: &[u8]) -> Input {
    input(Of::Withdraw(WithdrawQueued {
        queued_input_id: queued_input_id.to_vec(),
    }))
}

/// Steers a queued prompt into the running turn.
pub fn send_now(queued_input_id: &[u8]) -> Input {
    input(Of::SendNow(SendQueuedNow {
        queued_input_id: queued_input_id.to_vec(),
    }))
}

pub fn interrupt() -> Input {
    input(Of::Interrupt(Interrupt {}))
}

/// Stops one background job by its key. Terminal Claude cannot stop one
/// job, and refuses it.
pub fn stop_job(key: &str) -> Input {
    input(Of::StopJob(wire::StopJob {
        key: key.to_owned(),
    }))
}

/// Takes terminal Claude's own view back to the agent's conversation; the
/// other kinds' views cannot switch away, and refuse it.
pub fn switch_back() -> Input {
    input(Of::SwitchBack(wire::SwitchBack {}))
}
