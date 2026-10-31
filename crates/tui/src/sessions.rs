//! What the TUI's tests and the storybook share: the agent and the catch-up
//! a session built from an interpreter's fixtures starts from, and a drawn
//! buffer read back as text.

use ratatui::buffer::Buffer;
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

/// The buffer's text, one line per row, trailing blanks trimmed.
pub fn text(buffer: &Buffer) -> String {
    let area = buffer.area;
    let mut out = String::new();
    for y in 0..area.height {
        let mut line = String::new();
        for x in 0..area.width {
            line.push_str(buffer[(x, y)].symbol());
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}
