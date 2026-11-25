//! What the interpreter-driven view tests share.

use ui_state::SessionState;
use ui_view::{Row, ToolRows, chat_rows_for};
use wire::{Kind, Phase};

/// The window's cap: more rows than any case here delivers.
pub const CAP: usize = 200;

/// The agent a session under test is with.
pub fn agent(kind: Kind, phase: Phase) -> wire::Agent {
    wire::Agent {
        agent_id: b"agent".to_vec(),
        host_id: b"host".to_vec(),
        kind: kind as i32,
        provider: match kind {
            Kind::ClaudePty | Kind::ClaudeSdk => wire::agents::CLAUDE,
            Kind::Codex => wire::agents::CODEX,
            _ => "opencode",
        }
        .into(),
        name: "worker".into(),
        lifecycle: wire::Lifecycle::Live as i32,
        status: wire::AgentStatus::in_phase(phase),
        incarnation: 1,
        ..wire::Agent::default()
    }
}

/// Every row the state holds, oldest first.
pub fn all_rows(state: &SessionState, tools: ToolRows) -> Vec<Row> {
    let keys: Vec<_> = state
        .transcript()
        .iter()
        .map(|held| held.item.key.clone())
        .collect();
    chat_rows_for(state, &keys, tools)
}

/// What an agent of `kind` can do, as its interpreter says in every
/// snapshot (an ACP agent here is one that said nothing at `initialize`).
pub fn capabilities(kind: Kind) -> wire::AgentCapabilities {
    match kind {
        Kind::ClaudePty => wire::AgentCapabilities::claude_pty(),
        Kind::ClaudeSdk => wire::AgentCapabilities::claude_sdk(),
        Kind::Codex => wire::AgentCapabilities::codex(wire::Platform::here()),
        _ => wire::AgentCapabilities::acp(false, false, false),
    }
}
