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
        name: "worker".into(),
        lifecycle: wire::Lifecycle::Live as i32,
        phase: phase as i32,
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
