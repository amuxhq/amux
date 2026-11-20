//! What the reviews experiment leaves outside the store, removed when the
//! daemon opens a profile without it: each own agent's snapshot store. The
//! store's tables go with the experiment by the store's own rules.

use std::path::Path;

use crate::runtime::Launch;

/// The profile at `dir` opens under `launch`, its store open.
pub(crate) fn on_open(_dir: &Path, _store: &store::Sqlite, _launch: &Launch) {
    // With the experiment off, removes each own agent's review directory,
    // following no link out of the agent's directory.
}
