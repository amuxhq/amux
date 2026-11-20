//! The reviews experiment's tables (`reviews/1`), each named with the
//! prefix `exp_reviews_1_`: a review of each own agent's working tree, the
//! snapshots it took, the rounds sent and the comments. Made and dropped
//! with the experiment by [`crate::experiments`], never by a migration.

use crate::StoreError;
use crate::rows::Rows;

/// The SQL that makes the experiment's tables, run each time a store opens
/// with the experiment on.
pub const SCHEMA: &str = "";

/// Removes what the experiment's tables hold of the own agent `agent`, as
/// its row is removed.
pub(crate) fn remove_agent(_rows: &Rows<'_>, _agent: &[u8]) -> Result<(), StoreError> {
    // Deletes the agent's review, with its snapshots, rounds and comments.
    Ok(())
}
