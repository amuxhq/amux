//! A person's acts on a chat in each kind's own arm. The acts are the same
//! on every kind; which message carries them is the kind's. Pure, so every
//! client builds them the same way, the browser extension's too; the
//! caller gives each its id.

use wire::{
    ClaudePtyInput, ClaudeSdkInput, CodexInput, CreateAgentRequest, Interrupt, Kind, KindInput,
    PromptInput, SendQueuedNow, StopJob, WithdrawQueued, claude_pty_input, claude_sdk_input,
    codex_input,
};

/// One act, before it is put in a kind's arm.
#[derive(Clone, Debug, PartialEq)]
pub enum Act {
    Prompt(PromptInput),
    Withdraw(WithdrawQueued),
    SendNow(SendQueuedNow),
    Interrupt,
    StopJob(StopJob),
    SwitchBack,
}

/// The act in the kind's arm; None for a kind that has no input for it.
/// Whether an act is offered at all is the agent's capabilities' to say,
/// never the kind's.
pub fn act_input(kind: Kind, act: Act) -> Option<KindInput> {
    let of = match kind {
        Kind::ClaudePty => KindInput::ClaudePty(ClaudePtyInput {
            of: Some(match act {
                Act::Prompt(prompt) => claude_pty_input::Of::Prompt(prompt),
                Act::Withdraw(withdraw) => claude_pty_input::Of::Withdraw(withdraw),
                Act::SendNow(now) => claude_pty_input::Of::SendNow(now),
                Act::Interrupt => claude_pty_input::Of::Interrupt(Interrupt {}),
                Act::SwitchBack => claude_pty_input::Of::SwitchBack(wire::SwitchBack {}),
                Act::StopJob(_) => return None,
            }),
        }),
        Kind::ClaudeSdk => KindInput::ClaudeSdk(ClaudeSdkInput {
            of: Some(match act {
                Act::Prompt(prompt) => claude_sdk_input::Of::Prompt(prompt),
                Act::Withdraw(withdraw) => claude_sdk_input::Of::Withdraw(withdraw),
                Act::SendNow(now) => claude_sdk_input::Of::SendNow(now),
                Act::Interrupt => claude_sdk_input::Of::Interrupt(Interrupt {}),
                Act::StopJob(stop) => claude_sdk_input::Of::StopJob(stop),
                Act::SwitchBack => return None,
            }),
        }),
        Kind::Codex => KindInput::Codex(CodexInput {
            of: Some(match act {
                Act::Prompt(prompt) => codex_input::Of::Prompt(prompt),
                Act::Withdraw(withdraw) => codex_input::Of::Withdraw(withdraw),
                Act::SendNow(now) => codex_input::Of::SendNow(now),
                Act::Interrupt => codex_input::Of::Interrupt(Interrupt {}),
                Act::StopJob(stop) => codex_input::Of::StopJob(stop),
                Act::SwitchBack => return None,
            }),
        }),
        Kind::Acp => KindInput::Acp(wire::AcpInput {
            of: Some(match act {
                Act::Prompt(prompt) => wire::acp_input::Of::Prompt(prompt),
                Act::Withdraw(withdraw) => wire::acp_input::Of::Withdraw(withdraw),
                Act::SendNow(now) => wire::acp_input::Of::SendNow(now),
                Act::Interrupt => wire::acp_input::Of::Interrupt(Interrupt {}),
                Act::StopJob(_) | Act::SwitchBack => return None,
            }),
        }),
        Kind::Unspecified => return None,
    };
    Some(of)
}

/// A new agent's create request: its provider, the kind its host starts
/// it as, and what was chosen for it. The caller fills in where and what
/// else.
pub fn create_request(
    provider: &str,
    kind: Kind,
    chosen: &crate::NewAgentChoices,
) -> CreateAgentRequest {
    CreateAgentRequest {
        provider: provider.to_owned(),
        kind: kind as i32,
        settings: Some(wire::AgentSettings {
            model: chosen.model.clone(),
            effort: chosen.effort.clone(),
            permission: chosen.permission.clone(),
            permission_level: 0,
            mode: chosen.mode.clone(),
        }),
        ..CreateAgentRequest::default()
    }
}
