//! The agent and the catch-up a session built from an interpreter's
//! fixtures starts from: for the TUI's tests, and for the storybook, which
//! replays recorded fixtures through an interpreter of its own.

use ui_state::Msg;
use wire::{Kind, SessionEvent, session_event};

pub fn agent(kind: Kind) -> wire::Agent {
    wire::Agent {
        agent_id: b"agent".to_vec(),
        host_id: b"host".to_vec(),
        kind: kind as i32,
        name: "worker".into(),
        lifecycle: wire::Lifecycle::Live as i32,
        phase: wire::Phase::Idle as i32,
        incarnation: 1,
        ..wire::Agent::default()
    }
}

pub fn caught_up(revision: u64) -> Msg {
    Msg::Event(SessionEvent {
        of: Some(session_event::Of::CaughtUp(wire::CaughtUp { revision })),
    })
}
