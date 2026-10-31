//! What the interpreter-driven view tests share.

use std::collections::HashMap;

use ui_state::SessionState;
use ui_view::{Row, ToolRows, chat_rows_for};
use wire::{SessionEvent, session_event};

/// The window's cap: more rows than any case here delivers.
pub const CAP: usize = 200;

/// Every row the state holds, oldest first.
pub fn all_rows(state: &SessionState, tools: ToolRows) -> Vec<Row> {
    let keys: Vec<_> = state
        .transcript()
        .iter()
        .map(|held| held.item.key.clone())
        .collect();
    chat_rows_for(state, &keys, tools)
}

/// Commits interpreter steps the way the owning daemon does.
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
