//! Sessions built from what an interpreter wrote, committed the way the
//! owning daemon commits them: for the TUI's tests, and for the storybook,
//! which replays recorded fixtures through an interpreter of its own.

use std::collections::HashMap;

use ui_state::Msg;
use wire::{Kind, SessionEvent, session_event};

/// Commits interpreter steps the way the owning daemon does: an order once
/// per new key, a revision per record.
#[derive(Clone, Default)]
pub struct Committer {
    pub revision: u64,
    next_order: u64,
    orders: HashMap<String, u64>,
    revisions: HashMap<String, u64>,
}

impl Committer {
    pub fn commit(&mut self, step: &wire::Step) -> Vec<SessionEvent> {
        let event = |of| SessionEvent { of: Some(of) };
        let mut out = Vec::new();
        for item in &step.items {
            self.revision += 1;
            let order = *self.orders.entry(item.key.clone()).or_insert_with(|| {
                self.next_order += 1;
                self.next_order
            });
            self.revisions.insert(item.key.clone(), self.revision);
            let mut item = item.clone();
            item.order = order;
            item.revision = self.revision;
            out.push(event(session_event::Of::Item(item)));
        }
        for append in &step.appends {
            self.revision += 1;
            let base = self
                .revisions
                .insert(append.key.clone(), self.revision)
                .unwrap_or(0);
            let mut append = append.clone();
            append.base_revision = base;
            append.revision = self.revision;
            out.push(event(session_event::Of::Append(append)));
        }
        if let Some(snapshot) = &step.snapshot {
            self.revision += 1;
            let mut snapshot = snapshot.clone();
            snapshot.revision = self.revision;
            out.push(event(session_event::Of::Snapshot(snapshot)));
        }
        out
    }
}

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
