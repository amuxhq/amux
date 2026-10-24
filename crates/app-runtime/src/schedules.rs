//! The client model's schedules as the host hears them: walks each and, at
//! every cut, hands over what moved as a chat delta would, applies it to a
//! copy of the host's keys, and holds that copy equal to the session's
//! window. Cuts take one action at a time on even seeds and three on odd
//! ones, so a delta also covers several moves coalesced, as a busy phone's
//! does.

use std::collections::BTreeSet;

use models::client::Action;
use models::{Bounds, ClientModel, Machine};
use ui_state::{InputOutcome, Key, Msg};
use wire::session_event::Of;

use crate::delta::Sent;
use crate::values::KeysMoved;

/// The schedules walked, one per seed.
const SEEDS: u64 = 12;
/// The most actions a schedule takes.
const DEPTH: usize = 40;

fn every(seed: u64) -> usize {
    if seed.is_multiple_of(2) { 1 } else { 3 }
}

/// The host's keys after a delta moved them.
fn apply(held: &mut Vec<Key>, sequence: KeysMoved) {
    match sequence {
        KeysMoved::Reload(keys) => *held = keys,
        KeysMoved::Edges {
            above,
            below,
            dropped,
        } => {
            held.drain(..dropped as usize);
            held.splice(0..0, below);
            held.extend(above);
        }
    }
}

fn walk(seed: u64) {
    let model = ClientModel::ci();
    let walk = models::walk(&model, Bounds::depth(DEPTH), seed);
    assert!(walk.failure.is_none(), "{walk}");
    let mut state = model.init();
    // The runtime's session, fed what the model fed its client: the outcomes
    // of its updates are the changes a delta is computed from.
    let mut session = state.client().clone();
    let mut sent = Sent::default();
    let mut held = Vec::new();
    let opening = sent.keys_moved(session.transcript(), true, &[]).0;
    apply(&mut held, opening.expect("an opening is a reload"));
    for actions in walk.actions.chunks(every(seed)) {
        let mut changed = Vec::new();
        let mut seen = BTreeSet::new();
        let mut reloaded = false;
        let mut messages = Vec::new();
        for action in actions {
            state = model.next(&state, action).expect("a walked action applies");
            for msg in state.fed() {
                messages.push(describe(msg));
                let outcome = session.update(msg.clone());
                for key in outcome.changed {
                    if seen.insert(key.clone()) {
                        changed.push(key);
                    }
                }
                reloaded |= outcome.reloaded;
            }
        }
        let (sequence, wanted) = sent.keys_moved(session.transcript(), reloaded, &changed);
        let paged = match &sequence {
            Some(KeysMoved::Edges { below, .. }) => below.clone(),
            _ => Vec::new(),
        };
        let above = match &sequence {
            Some(KeysMoved::Edges { above, .. }) => above.clone(),
            _ => Vec::new(),
        };
        let whole = matches!(sequence, Some(KeysMoved::Reload(_)));
        if let Some(sequence) = sequence {
            apply(&mut held, sequence);
        }
        let window: Vec<Key> = session.transcript().keys().cloned().collect();
        let actions: Vec<String> = actions.iter().map(name).collect();
        assert_eq!(
            held, window,
            "seed {seed}: the host's keys differ from the window after {actions:?} fed \
             {messages:?}"
        );
        // Rows go with every changed key the host already held, but not
        // with a key new at either edge or a reload, whose rows are read
        // when drawn.
        if whole {
            assert!(wanted.is_empty(), "seed {seed}: rows with a reload");
            continue;
        }
        for key in &changed {
            let owed = window.contains(key) && !paged.contains(key) && !above.contains(key);
            assert_eq!(
                wanted.contains(key),
                owed,
                "seed {seed}: the row for {key} after {actions:?}"
            );
        }
    }
}

fn name(action: &Action) -> String {
    format!("{action:?}")
}

/// A message as a person reads a trace: the event and the keys, revisions
/// and markers it carries.
fn describe(msg: &Msg) -> String {
    match msg {
        Msg::Event(event) => match &event.of {
            Some(Of::Opening(opening)) => format!("Opening generation {}", opening.generation),
            Some(Of::Reset(reset)) => format!("Reset generation {}", reset.generation),
            Some(Of::Snapshot(snapshot)) => format!("Snapshot r{}", snapshot.revision),
            Some(Of::Item(item)) => format!("Item {}@r{} {:?}", item.key, item.revision, item.text),
            Some(Of::Append(append)) => format!(
                "Append {} r{}->r{} {:?}",
                append.key, append.base_revision, append.revision, append.text
            ),
            Some(Of::CaughtUp(caught_up)) => format!("CaughtUp r{}", caught_up.revision),
            Some(Of::Lagged(_)) => "Lagged".into(),
            other => format!("{other:?}"),
        },
        Msg::Page {
            items, exhausted, ..
        } => {
            let keys: Vec<&str> = items.iter().map(|item| item.key.as_str()).collect();
            format!("Page {keys:?} exhausted {exhausted}")
        }
        Msg::Connection(connection) => format!("Connection {connection:?}"),
        Msg::Send(_) => "Send the prompt".into(),
        Msg::Sent(_, InputOutcome::Lost) => "Sent: lost".into(),
        Msg::Sent(_, outcome) => format!("Sent: {outcome:?}"),
        Msg::Discard(_) => "Discard the prompt".into(),
        other => format!("{other:?}"),
    }
}

#[test]
fn a_hosts_keys_follow_the_window_through_every_schedule() {
    for seed in 0..SEEDS {
        walk(seed);
    }
}
