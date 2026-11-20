//! A snapshot store: a git object directory of its own, outside any
//! repository, holding the commits a review takes of a working tree.
//!
//! Git writes into it with `GIT_OBJECT_DIRECTORY` pointed at the store and
//! the repository's own object directory as an alternate, so nothing new
//! lands in the repository; it reads with the store as an alternate of the
//! repository, so a snapshot and the history beneath it read as one.

use std::path::{Path, PathBuf};

/// The snapshot store at one directory.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ObjectStore {
    dir: PathBuf,
}

impl ObjectStore {
    /// The store at `dir`, which need not exist until something is written.
    pub fn at(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// The store's directory: what `GIT_ALTERNATE_OBJECT_DIRECTORIES` names
    /// to read a snapshot in the repository.
    pub fn dir(&self) -> &Path {
        &self.dir
    }
}
