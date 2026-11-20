//! Snapshots of a working tree, untracked files included, committed into an
//! [`ObjectStore`](crate::objects::ObjectStore) without writing anything to
//! the repository: taking one, telling whether the tree has moved since one
//! without writing objects, and dropping those no longer kept.

/// A snapshot as it was taken.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    /// The snapshot's commit, by its full object id.
    pub commit: String,
    /// The paths left out of it for being over
    /// [`LARGE_FILE_BYTES`](crate::LARGE_FILE_BYTES), from the repository's
    /// top folder.
    pub omitted: Vec<String>,
    /// A merge or rebase was under way, so conflicted files hold their
    /// markers as content.
    pub merging: bool,
}
