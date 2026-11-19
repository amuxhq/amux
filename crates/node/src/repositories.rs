//! Where a host offers to start an agent: the repository list its person
//! chose, then the directories its agents ran in lately, then those Claude
//! Code and Codex ran in outside amux.
//!
//! The recent list is kept in the store rather than read from the agents'
//! rows, so a directory stays on it after the agents that ran there are
//! deleted.

use std::collections::BTreeSet;
use std::path::PathBuf;

use wire::{ListRepositoriesResponse, RepositoryEntry};

/// The most directories one answer carries, whatever limit it asks for.
const MAX_DIRECTORIES: usize = store::RECENT_DIRECTORIES;

/// What a profile's runtime asks of the installation about its folders:
/// the person's home, where Claude Code and Codex keep their sessions, and
/// pinning a folder among the repositories in the settings, which the
/// installation applies to every profile.
#[derive(Clone)]
pub struct Pins {
    home: Option<PathBuf>,
    pin: std::sync::Arc<Pin>,
}

/// Pins (or unpins) a folder in the installation's settings.
type Pin = dyn Fn(&str, bool) -> Result<(), crate::host_settings::SettingsError> + Send + Sync;

impl Pins {
    pub fn new(
        home: Option<PathBuf>,
        pin: impl Fn(&str, bool) -> Result<(), crate::host_settings::SettingsError>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        Self {
            home,
            pin: std::sync::Arc::new(pin),
        }
    }

    pub(crate) fn home(&self) -> Option<&std::path::Path> {
        self.home.as_deref()
    }

    pub(crate) fn pin(
        &self,
        path: &str,
        pinned: bool,
    ) -> Result<(), crate::host_settings::SettingsError> {
        (self.pin)(path, pinned)
    }
}

impl Default for Pins {
    /// A runtime with no installation behind it keeps no settings.
    fn default() -> Self {
        Self::new(None, |_, _| {
            Err(crate::host_settings::SettingsError::Invalid(
                "this host keeps no repository list".into(),
            ))
        })
    }
}

/// The most folders a host's own entry offers.
pub(crate) const MOST_FOLDERS: usize = 50;

/// The folders a host's own entry offers: its pinned repositories that
/// still exist, in their order, then the recent directories that still
/// exist, newest first, then where Claude Code and Codex ran lately
/// (`history`, newest first). Each path once, the first reason it is
/// offered winning, and at most [`MOST_FOLDERS`]. Reads the file system and
/// asks git about each folder: call it off the async workers.
pub(crate) fn folders(
    pinned: &[PathBuf],
    recent: &[(String, i64)],
    history: &[RepositoryEntry],
) -> Vec<wire::Folder> {
    let listed = list(pinned, recent, None, MOST_FOLDERS as u32);
    let mut seen = BTreeSet::new();
    let mut folders = Vec::new();
    let offered = listed
        .repositories
        .into_iter()
        .map(|entry| (entry, wire::FolderKind::Pinned))
        .chain(
            listed
                .recent
                .into_iter()
                .map(|entry| (entry, wire::FolderKind::Recent)),
        )
        .chain(
            history
                .iter()
                .cloned()
                .map(|entry| (entry, wire::FolderKind::History)),
        );
    for (entry, kind) in offered {
        if folders.len() == MOST_FOLDERS {
            break;
        }
        if !seen.insert(entry.path.clone()) {
            continue;
        }
        folders.push(wire::Folder {
            checkout: git_facts::checkout(std::path::Path::new(&entry.path)).map(to_wire),
            path: entry.path,
            name: entry.name,
            kind: kind as i32,
            last_used_ms: entry.last_used_ms,
        });
    }
    folders
}

/// A checkout as the wire says it.
pub(crate) fn to_wire(checkout: git_facts::Checkout) -> wire::Checkout {
    let text = |path: std::path::PathBuf| path.to_string_lossy().into_owned();
    wire::Checkout {
        root: text(checkout.root),
        main: checkout.main.map(text),
        remote: checkout.remote,
    }
}

/// What a host answers: its repositories that still exist, in the order they
/// were chosen, then its recent directories that still exist and are not
/// repositories, newest first; both matching `query` (case-insensitively, by
/// name or path) and at most `limit` in all, repositories first. Reads the file
/// system: call it off the async workers.
pub(crate) fn list(
    repositories: &[PathBuf],
    recent: &[(String, i64)],
    query: Option<&str>,
    limit: u32,
) -> ListRepositoriesResponse {
    let limit = (limit as usize).min(MAX_DIRECTORIES);
    let query = query.unwrap_or_default().to_lowercase();
    let matches = |entry: &RepositoryEntry| {
        entry.name.to_lowercase().contains(&query) || entry.path.to_lowercase().contains(&query)
    };
    let mut seen = BTreeSet::new();
    let repositories: Vec<RepositoryEntry> = repositories
        .iter()
        .filter_map(|path| path.canonicalize().ok())
        .filter(|path| path.is_dir())
        .filter_map(|path| entry(path, None))
        .filter(|entry| matches(entry) && seen.insert(entry.path.clone()))
        .take(limit)
        .collect();
    let recent: Vec<RepositoryEntry> = recent
        .iter()
        .map(|(path, at)| (PathBuf::from(path), *at))
        // A directory since replaced by a symlink is not the one that was
        // used, and gone is gone.
        .filter(|(path, _)| path.canonicalize().ok().as_ref() == Some(path) && path.is_dir())
        .filter_map(|(path, at)| entry(path, Some(at)))
        .filter(|entry| matches(entry) && seen.insert(entry.path.clone()))
        .take(limit - repositories.len())
        .collect();
    ListRepositoriesResponse {
        recent,
        repositories,
    }
}

/// A directory as a listing names it; `None` for a path that is not UTF-8,
/// which no client could send back.
fn entry(path: PathBuf, last_used_ms: Option<i64>) -> Option<RepositoryEntry> {
    let text = path.to_str()?.to_owned();
    let name = path
        .file_name()
        .map_or_else(|| text.clone(), |name| name.to_string_lossy().into_owned());
    Some(RepositoryEntry {
        remote: git_facts::origin(&path),
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
    fn folders_list_pinned_then_recent_then_history_each_path_once() {
        let temp = tempfile::tempdir().unwrap();
        let make = |name: &str| {
            let path = temp.path().join(name);
            std::fs::create_dir(&path).unwrap();
            path.canonicalize().unwrap()
        };
        let (app, notes, scratch, dotfiles) = (
            make("app"),
            make("notes"),
            make("scratch"),
            make("dotfiles"),
        );
        std::fs::create_dir(app.join(".git")).unwrap();
        let recent = recorded(&[(&scratch, 3_000), (&app, 2_000)]);
        let history = [
            RepositoryEntry {
                path: dotfiles.to_str().unwrap().into(),
                name: "dotfiles".into(),
                last_used_ms: Some(1_000),
                remote: None,
            },
            // Already pinned: offered once, as pinned.
            RepositoryEntry {
                path: notes.to_str().unwrap().into(),
                name: "notes".into(),
                last_used_ms: Some(900),
                remote: None,
            },
        ];
        let folders = folders(&[notes.clone(), app.clone()], &recent, &history);
        let listed: Vec<(&str, wire::FolderKind)> = folders
            .iter()
            .map(|folder| (folder.name.as_str(), folder.kind()))
            .collect();
        assert_eq!(
            listed,
            [
                ("notes", wire::FolderKind::Pinned),
                ("app", wire::FolderKind::Pinned),
                ("scratch", wire::FolderKind::Recent),
                ("dotfiles", wire::FolderKind::History),
            ]
        );
        assert_eq!(
            folders[1]
                .checkout
                .as_ref()
                .map(|checkout| checkout.root.as_str()),
            app.to_str(),
            "a folder in Git says its working tree"
        );
        assert_eq!(folders[0].checkout, None, "a plain folder has none");
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
    fn repositories_come_first_and_the_limit_caps_both_lists_together() {
        let temp = tempfile::tempdir().unwrap();
        for name in ["app", "site", "notes"] {
            std::fs::create_dir(temp.path().join(name)).unwrap();
        }
        let repositories = [
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
        let listed = list(&repositories, &recent, None, 10);
        let names = |entries: &[RepositoryEntry]| -> Vec<String> {
            entries.iter().map(|e| e.name.clone()).collect()
        };
        assert_eq!(names(&listed.repositories), ["site", "app"]);
        assert!(listed.repositories.iter().all(|e| e.last_used_ms.is_none()));
        assert_eq!(
            names(&listed.recent),
            ["notes"],
            "a repository is not recent too"
        );

        let capped = list(&repositories, &recent, None, 2);
        assert_eq!(names(&capped.repositories), ["site", "app"]);
        assert!(capped.recent.is_empty(), "the limit counts both lists");
        let found = list(&repositories, &recent, Some("NOT"), 10);
        assert!(found.repositories.is_empty());
        assert_eq!(names(&found.recent), ["notes"]);
    }
}
