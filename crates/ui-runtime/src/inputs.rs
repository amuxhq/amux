//! Inputs in each kind's own arm. A person's acts on a chat are the same on
//! every kind; which message carries them is the kind's.

use wire::{
    Attachment, ClaudePtyInput, ClaudeSdkInput, CodexInput, Input, Interrupt, Kind, PromptInput,
    PromptSubmission, SendQueuedNow, WithdrawQueued, claude_pty_input, claude_sdk_input,
    codex_input,
};

/// A fresh client-generated input id.
pub fn input_id() -> Vec<u8> {
    uuid::Uuid::new_v4().as_bytes().to_vec()
}

/// One act, before it is put in a kind's arm.
enum Act {
    Prompt(PromptInput),
    Withdraw(WithdrawQueued),
    SendNow(SendQueuedNow),
    Interrupt,
}

fn wrap(kind: Kind, act: Act) -> Option<Input> {
    let of = match kind {
        Kind::ClaudePty => wire::KindInput::ClaudePty(ClaudePtyInput {
            of: Some(match act {
                Act::Prompt(prompt) => claude_pty_input::Of::Prompt(prompt),
                Act::Withdraw(withdraw) => claude_pty_input::Of::Withdraw(withdraw),
                Act::SendNow(now) => claude_pty_input::Of::SendNow(now),
                Act::Interrupt => claude_pty_input::Of::Interrupt(Interrupt {}),
            }),
        }),
        Kind::ClaudeSdk => wire::KindInput::ClaudeSdk(ClaudeSdkInput {
            of: Some(match act {
                Act::Prompt(prompt) => claude_sdk_input::Of::Prompt(prompt),
                Act::Withdraw(withdraw) => claude_sdk_input::Of::Withdraw(withdraw),
                Act::SendNow(now) => claude_sdk_input::Of::SendNow(now),
                Act::Interrupt => claude_sdk_input::Of::Interrupt(Interrupt {}),
            }),
        }),
        Kind::Codex => wire::KindInput::Codex(CodexInput {
            of: Some(match act {
                Act::Prompt(prompt) => codex_input::Of::Prompt(prompt),
                Act::Withdraw(withdraw) => codex_input::Of::Withdraw(withdraw),
                Act::SendNow(now) => codex_input::Of::SendNow(now),
                Act::Interrupt => codex_input::Of::Interrupt(Interrupt {}),
            }),
        }),
        Kind::Unspecified => return None,
    };
    Some(Input::new(input_id(), of))
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

/// Steers a queued prompt into the running turn.
pub fn send_now(kind: Kind, queued_input_id: &[u8]) -> Option<Input> {
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

/// Stops one background job by its key; None for a kind that cannot stop
/// one job (terminal Claude).
pub fn stop_job(kind: Kind, key: &str) -> Option<Input> {
    let stop = wire::StopJob {
        key: key.to_owned(),
    };
    match kind {
        Kind::ClaudeSdk => Some(Input::new(
            Vec::new(),
            wire::ClaudeSdkInput {
                of: Some(wire::claude_sdk_input::Of::StopJob(stop)),
            },
        )),
        Kind::Codex => Some(Input::new(
            Vec::new(),
            wire::CodexInput {
                of: Some(wire::codex_input::Of::StopJob(stop)),
            },
        )),
        _ => None,
    }
}

/// Takes terminal Claude's own view back to the agent's conversation; None
/// for a kind whose view cannot switch away.
pub fn switch_back(kind: Kind) -> Option<Input> {
    use wire::claude_pty_input::Of;
    (kind == Kind::ClaudePty).then(|| {
        Input::new(
            Vec::new(),
            wire::ClaudePtyInput {
                of: Some(Of::SwitchBack(wire::SwitchBack {})),
            },
        )
    })
}
