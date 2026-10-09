//! Replica blob files have a byte budget of their own, evicted least
//! recently read first, independent of rows: they are disposable and
//! refetchable, and rows cannot say which files they still share. The one
//! exception is a file the caller pins: the catalogue a replica's current
//! snapshot names, which its picker needs while the origin is away. Own
//! blobs have no budget; they live and die with their agent's directory.

use std::collections::{BTreeMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};

/// An index of replica blob files by last read, on the runtime's clock.
#[derive(Debug, Default)]
pub struct BlobLru {
    files: BTreeMap<PathBuf, Entry>,
}

#[derive(Clone, Copy, Debug)]
struct Entry {
    size: u64,
    last_read: i64,
}

impl BlobLru {
    /// Indexes every file under `root` (the runtime's replicas directory),
    /// treating each as read at `now`, since reads before a restart are not
    /// known.
    pub fn scan(root: &Path, now: i64) -> io::Result<Self> {
        let mut lru = Self::default();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let entries = match std::fs::read_dir(&dir) {
                Ok(entries) => entries,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            for entry in entries {
                let entry = entry?;
                let kind = entry.file_type()?;
                if kind.is_dir() {
                    stack.push(entry.path());
                } else if kind.is_file()
                    && entry.path().parent().and_then(Path::file_name) == Some("blobs".as_ref())
                {
                    lru.insert(entry.path(), entry.metadata()?.len(), now);
                }
            }
        }
        Ok(lru)
    }

    /// Records a file fetched and written at `now`.
    pub fn insert(&mut self, path: PathBuf, size: u64, now: i64) {
        self.files.insert(
            path,
            Entry {
                size,
                last_read: now,
            },
        );
    }

    /// Records a read at `now`.
    pub fn touch(&mut self, path: &Path, now: i64) {
        if let Some(entry) = self.files.get_mut(path) {
            entry.last_read = entry.last_read.max(now);
        }
    }

    /// Forgets files removed with their agent.
    pub fn forget_under(&mut self, dir: &Path) {
        self.files.retain(|path, _| !path.starts_with(dir));
    }

    pub fn bytes(&self) -> u64 {
        self.files.values().map(|entry| entry.size).sum()
    }

    /// Deletes the least recently read files until the rest fit `budget`,
    /// and returns them in deletion order. A file in `pinned` is never
    /// deleted, even if the rest cannot fit without it. A file already gone
    /// is simply forgotten.
    pub fn sweep(&mut self, budget: u64, pinned: &HashSet<PathBuf>) -> io::Result<Vec<PathBuf>> {
        let mut total = self.bytes();
        let mut order = self
            .files
            .iter()
            .filter(|(path, _)| !pinned.contains(*path))
            .map(|(path, entry)| (entry.last_read, path.clone()))
            .collect::<Vec<_>>();
        order.sort();
        let mut removed = Vec::new();
        for (_, path) in order {
            if total <= budget {
                break;
            }
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            if let Some(entry) = self.files.remove(&path) {
                total -= entry.size;
            }
            removed.push(path);
        }
        Ok(removed)
    }
}
