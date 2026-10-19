//! What the fences read of a host's store, shared by the real net and the
//! simulated one so both hold a host to the same conditions. Each answers
//! what it found, or what it saw instead for the failure report.

use store::{AgentKey, Marker, Sqlite};

/// The host holds the agent's row at `order`: its newest row is that one
/// or a later one.
pub(crate) fn holds_order(store: &Sqlite, key: &AgentKey, order: u64) -> Result<(), String> {
    let newest = store
        .last_n(key, 1)
        .map_err(|error| error.to_string())?
        .pop()
        .map(|item| item.order);
    match newest {
        Some(held) if held >= order => Ok(()),
        Some(held) => Err(format!("newest order {held}")),
        None => Err("no rows".to_owned()),
    }
}

/// The order of the row that ended the agent's `k`th turn, counting from
/// one, among the rows the host holds.
pub(crate) fn turn_end(store: &Sqlite, key: &AgentKey, k: usize) -> Result<u64, String> {
    let mut ends: Vec<u64> = store
        .cut(key, u32::MAX)
        .map_err(|error| error.to_string())?
        .held
        .iter()
        .filter(|item| item.key.starts_with("turn:"))
        .map(|item| item.order)
        .collect();
    ends.sort_unstable();
    ends.get(k - 1)
        .copied()
        .ok_or_else(|| format!("{} turns ended", ends.len()))
}

/// The order of the row that reflects the input accepted under
/// `input_id`.
pub(crate) fn settled(store: &Sqlite, key: &AgentKey, input_id: &[u8]) -> Result<u64, String> {
    store
        .item_by_input(key, input_id)
        .map_err(|error| error.to_string())?
        .map(|item| item.order)
        .ok_or_else(|| "no row carries it".to_owned())
}

/// The newest revision the agent's own host has committed.
pub(crate) fn origin_newest(store: &Sqlite, key: &AgentKey) -> Result<u64, String> {
    store
        .agent(key)
        .map_err(|error| error.to_string())?
        .map(|row| row.next_revision - 1)
        .ok_or_else(|| "no origin row".to_owned())
}

/// Whether a replica is complete through the origin's `newest` revision
/// and has caught up with its stream.
pub(crate) fn replica_current(store: &Sqlite, key: &AgentKey, newest: u64) -> Result<(), String> {
    let row = store
        .agent(key)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("no replica row; origin newest {newest}"))?;
    let marker = store.cut(key, 0).map_err(|error| error.to_string())?.marker;
    if row.source_cursor != newest || marker != Some(Marker::CaughtUp) {
        return Err(format!(
            "replica cursor {}, marker {marker:?}; origin newest {newest}",
            row.source_cursor
        ));
    }
    Ok(())
}
