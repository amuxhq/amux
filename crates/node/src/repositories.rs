//! Where a host offers to start an agent: the project list its person
//! chose, then the directories its agents ran in lately.
//!
//! The recent list is kept in the store rather than read from the agents'
//! rows, so a directory stays on it after the agents that ran there are
//! deleted.

use std::collections::BTreeSet;
use std::path::PathBuf;

use wire::{ListRepositoriesResponse, ProjectEntry};

/// The most directories one answer carries, whatever limit it asks for.
const MAX_DIRECTORIES: usize = store::RECENT_DIRECTORIES;

/// What a host answers: its projects that still exist, in the order they
/// were chosen, then its recent directories that still exist and are not
/// projects, newest first; both matching `query` (case-insensitively, by
/// name or path) and at most `limit` in all, projects first. Reads the file
/// system: call it off the async workers.
pub(crate) fn list(
    projects: &[PathBuf],
    recent: &[(String, i64)],
    query: Option<&str>,
    limit: u32,
) -> ListRepositoriesResponse {
    let limit = (limit as usize).min(MAX_DIRECTORIES);
    let query = query.unwrap_or_default().to_lowercase();
    let matches = |entry: &ProjectEntry| {
        entry.name.to_lowercase().contains(&query) || entry.path.to_lowercase().contains(&query)
    };
    let mut seen = BTreeSet::new();
    let projects: Vec<ProjectEntry> = projects
        .iter()
        .filter_map(|path| path.canonicalize().ok())
        .filter(|path| path.is_dir())
        .filter_map(|path| entry(path, None))
        .filter(|entry| matches(entry) && seen.insert(entry.path.clone()))
        .take(limit)
        .collect();
    let recent: Vec<ProjectEntry> = recent
        .iter()
        .map(|(path, at)| (PathBuf::from(path), *at))
        // A directory since replaced by a symlink is not the one that was
        // used, and gone is gone.
        .filter(|(path, _)| path.canonicalize().ok().as_ref() == Some(path) && path.is_dir())
        .filter_map(|(path, at)| entry(path, Some(at)))
        .filter(|entry| matches(entry) && seen.insert(entry.path.clone()))
        .take(limit - projects.len())
        .collect();
    ListRepositoriesResponse { recent, projects }
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
    fn projects_come_first_and_the_limit_caps_both_lists_together() {
        let temp = tempfile::tempdir().unwrap();
        for name in ["app", "site", "notes"] {
            std::fs::create_dir(temp.path().join(name)).unwrap();
        }
        let projects = [
            temp.path().join("site"),
            temp.path().join("missing"),
            temp.path().join("app"),
            // Listed once, however it is spelled.
            temp.path().join("site/../site"),
        ];
        let recent = recorded(&[
            (&temp.path().join("notes"), 2),
            (&temp.path().join("app"), 1),
        ]);
        let listed = list(&projects, &recent, None, 10);
        let names = |entries: &[ProjectEntry]| -> Vec<String> {
            entries.iter().map(|e| e.name.clone()).collect()
        };
        assert_eq!(names(&listed.projects), ["site", "app"]);
        assert!(listed.projects.iter().all(|e| e.last_used_ms.is_none()));
        assert_eq!(
            names(&listed.recent),
            ["notes"],
            "a project is not recent too"
        );

        let capped = list(&projects, &recent, None, 2);
        assert_eq!(names(&capped.projects), ["site", "app"]);
        assert!(capped.recent.is_empty(), "the limit counts both lists");
        let found = list(&projects, &recent, Some("NOT"), 10);
        assert!(found.projects.is_empty());
        assert_eq!(names(&found.recent), ["notes"]);
    }
}
