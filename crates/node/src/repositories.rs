//! Where a host offers to start an agent: the directories its agents ran
//! in lately, and the Git repositories under its configured roots.
//!
//! The recent list is kept in the store rather than read from the agents'
//! rows, so a directory stays on it after the agents that ran there are
//! deleted.

use std::collections::BTreeSet;
use std::path::PathBuf;

use wire::{ListRepositoriesResponse, ProjectEntry};

/// The most directories one answer carries, whatever limit it asks for.
const MAX_DIRECTORIES: usize = store::RECENT_DIRECTORIES;

/// What a host answers: its recent directories that still exist, then the
/// repositories under `roots`, both matching `query` (case-insensitively,
/// by name or path) and at most `limit` in all, recent ones first. Reads
/// the file system: call it off the async workers.
pub(crate) fn list(
    roots: &[PathBuf],
    recent: &[(String, i64)],
    query: Option<&str>,
    limit: u32,
) -> ListRepositoriesResponse {
    let limit = (limit as usize).min(MAX_DIRECTORIES);
    let query = query.unwrap_or_default().to_lowercase();
    let matches = |entry: &ProjectEntry| {
        entry.name.to_lowercase().contains(&query) || entry.path.to_lowercase().contains(&query)
    };
    let roots: Vec<PathBuf> = roots
        .iter()
        .filter_map(|root| root.canonicalize().ok())
        .filter(|root| root.is_dir() && root.to_str().is_some())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut seen = BTreeSet::new();
    let recent: Vec<ProjectEntry> = recent
        .iter()
        .map(|(path, at)| (PathBuf::from(path), *at))
        // A directory since replaced by a symlink is not the one that was
        // used, and gone is gone.
        .filter(|(path, _)| path.canonicalize().ok().as_ref() == Some(path) && path.is_dir())
        .filter_map(|(path, at)| entry(path, Some(at)))
        .filter(|entry| matches(entry) && seen.insert(entry.path.clone()))
        .take(limit)
        .collect();
    let mut repositories = Vec::new();
    let mut visited = BTreeSet::new();
    let mut pending: BTreeSet<PathBuf> = roots.iter().cloned().collect();
    while recent.len() + repositories.len() < limit {
        let Some(path) = pending.pop_first() else {
            break;
        };
        // Symlinks resolve, but never out of the roots.
        let Ok(path) = path.canonicalize() else {
            continue;
        };
        if !roots.iter().any(|root| path.starts_with(root)) || !visited.insert(path.clone()) {
            continue;
        }
        // A checkout has a .git directory, a worktree a .git file. Nothing
        // inside a repository is searched: not its Git internals, not its
        // dependencies.
        if path.join(".git").exists() {
            if let Some(entry) = entry(path, None)
                && matches(&entry)
                && seen.insert(entry.path.clone())
            {
                repositories.push(entry);
            }
            continue;
        }
        let Ok(children) = std::fs::read_dir(&path) else {
            continue;
        };
        for child in children.flatten() {
            if child.file_type().is_ok_and(|kind| kind.is_dir()) {
                pending.insert(child.path());
            }
        }
    }
    ListRepositoriesResponse {
        recent,
        repositories,
        roots: roots
            .iter()
            .filter_map(|root| root.to_str().map(str::to_owned))
            .collect(),
    }
}

/// A directory as a listing names it; `None` for a path that is not UTF-8,
/// which no client could send back.
fn entry(path: PathBuf, last_used_ms: Option<i64>) -> Option<ProjectEntry> {
    let text = path.to_str()?.to_owned();
    let name = path
        .file_name()
        .map_or_else(|| text.clone(), |name| name.to_string_lossy().into_owned());
    Some(ProjectEntry {
        path: text,
        name,
        last_used_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recorded(paths: &[(&std::path::Path, i64)]) -> Vec<(String, i64)> {
        let mut store = store::Sqlite::open_in_memory([0; 16]).unwrap();
        for (path, at) in paths {
            let path = path.canonicalize().unwrap();
            store
                .record_recent_directory(path.to_str().unwrap(), *at)
                .unwrap();
        }
        store.recent_directories().unwrap()
    }

    #[test]
    fn recent_lists_the_latest_use_of_directories_that_are_still_there() {
        let temp = tempfile::tempdir().unwrap();
        let (first, second) = (temp.path().join("first"), temp.path().join("second"));
        std::fs::create_dir(&first).unwrap();
        std::fs::create_dir(&second).unwrap();
        // An older use of the same directory cannot move it back.
        let recent = recorded(&[(&first, 2_000), (&second, 1_000), (&first, 500)]);
        let listed = list(&[], &recent, None, 10);
        let paths: Vec<_> = listed.recent.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(paths, ["first", "second"]);
        assert_eq!(listed.recent[0].last_used_ms, Some(2_000));

        std::fs::remove_dir(&first).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&second, &first).unwrap();
        let listed = list(&[], &recent, None, 10);
        let paths: Vec<_> = listed.recent.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(paths, ["second"]);
    }

    #[test]
    fn the_limit_caps_both_lists_together_and_overlapping_roots_list_once() {
        let temp = tempfile::tempdir().unwrap();
        for index in 0..205 {
            std::fs::create_dir_all(temp.path().join(format!("repo-{index:03}/.git"))).unwrap();
        }
        let roots = [
            temp.path().to_path_buf(),
            temp.path().join("repo-000"),
            temp.path().join("missing"),
        ];
        let listed = list(&roots, &[], None, u32::MAX);
        assert_eq!(listed.repositories.len(), MAX_DIRECTORIES);
        assert_eq!(listed.repositories[0].name, "repo-000");
        assert_eq!(listed.repositories[199].name, "repo-199");
        assert_eq!(listed.roots.len(), 2);

        let recent = recorded(&[(&temp.path().join("repo-150"), 1)]);
        let listed = list(&roots, &recent, None, 3);
        let names: Vec<_> = listed
            .repositories
            .iter()
            .map(|e| e.name.as_str())
            .collect();
        assert_eq!(listed.recent[0].name, "repo-150");
        assert_eq!(names, ["repo-000", "repo-001"]);
    }
}
