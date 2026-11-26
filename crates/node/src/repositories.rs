//! Where a host offers to start an agent: the repository list its person
//! chose, then the directories its agents ran in lately, then those Claude
//! Code and Codex ran in outside amux.
//!
//! The recent list is kept in the store rather than read from the agents'
//! rows, so a directory stays on it after the agents that ran there are
//! deleted.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::plain_paths::{canonical, plain};

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

/// The folders a host's own entry offers, as the latest refresh found them.
/// Refreshes run as tasks of their own and a slow scan can finish after a
/// later one: each refresh takes a number when it is asked for, and what
/// it found is kept only when no refresh asked for after it has been kept
/// already, so a pin made a moment ago is not put back by an older scan.
#[derive(Default)]
pub(crate) struct Offered {
    asked: std::sync::atomic::AtomicU64,
    /// The number of the refresh whose folders these are, and the folders.
    held: std::sync::Mutex<(u64, Vec<wire::Folder>)>,
}

impl Offered {
    /// Numbers a refresh being asked for, later ones higher.
    pub(crate) fn ask(&self) -> u64 {
        self.asked
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            .wrapping_add(1)
    }

    pub(crate) fn get(&self) -> Vec<wire::Folder> {
        self.held.lock().unwrap().1.clone()
    }

    /// Keeps what refresh `number` found unless a later refresh's folders
    /// are held; answers whether the folders held moved.
    pub(crate) fn found(&self, number: u64, folders: Vec<wire::Folder>) -> bool {
        let mut held = self.held.lock().unwrap();
        if number <= held.0 {
            return false;
        }
        let moved = held.1 != folders;
        *held = (number, folders);
        moved
    }
}

/// The folders a host's own entry offers: its pinned repositories that
/// still exist, in their order, then the recent directories that still
/// exist, newest first, then where Claude Code and Codex ran lately
/// (`history`, newest first). Each path once, the first reason it is
/// offered winning, and at most [`MOST_FOLDERS`]. Reads the file system and
/// asks git about each folder: call it off the async workers.
pub(crate) fn folders(
    pinned: &[PathBuf],
    recent: &[(String, i64)],
    history: &[wire::Folder],
) -> Vec<wire::Folder> {
    let pinned = pinned
        .iter()
        .filter_map(|path| canonical(path).ok())
        .filter(|path| path.is_dir())
        .filter_map(|path| folder(&path, wire::FolderKind::Pinned, None));
    let recent = recent
        .iter()
        // Read plain: an earlier build stored Windows' verbatim form.
        .map(|(path, at)| (plain(PathBuf::from(path)), *at))
        // A directory since replaced by a symlink is not the one that was
        // used, and gone is gone.
        .filter(|(path, _)| canonical(path).ok().as_ref() == Some(path) && path.is_dir())
        .filter_map(|(path, at)| folder(&path, wire::FolderKind::Recent, Some(at)));
    let mut seen = BTreeSet::new();
    pinned
        .chain(recent)
        .chain(history.iter().cloned())
        .filter(|folder| seen.insert(folder.path.clone()))
        .take(MOST_FOLDERS)
        .collect()
}

/// A directory as a host's entry offers it; `None` for a path that is not
/// UTF-8, which no client could send back.
pub(crate) fn folder(
    path: &Path,
    kind: wire::FolderKind,
    last_used_ms: Option<i64>,
) -> Option<wire::Folder> {
    let text = path.to_str()?.to_owned();
    let name = path
        .file_name()
        .map_or_else(|| text.clone(), |name| name.to_string_lossy().into_owned());
    Some(wire::Folder {
        checkout: git_facts::checkout(path).map(to_wire),
        path: text,
        name,
        kind: kind as i32,
        last_used_ms,
    })
}

/// A checkout as the wire says it.
pub(crate) fn to_wire(checkout: git_facts::Checkout) -> wire::Checkout {
    let text = |path: PathBuf| path.to_string_lossy().into_owned();
    wire::Checkout {
        root: text(checkout.root),
        main: checkout.main.map(text),
        remote: checkout.remote,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recorded(paths: &[(&std::path::Path, i64)]) -> Vec<(String, i64)> {
        let mut store = store::Sqlite::open_in_memory([0; 16]).unwrap();
        for (path, at) in paths {
            // As an earlier build stored it: Windows' verbatim form there.
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
            canonical(&path).unwrap()
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
            folder(&dotfiles, wire::FolderKind::History, Some(1_000)).unwrap(),
            // Already pinned: offered once, as pinned.
            folder(&notes, wire::FolderKind::History, Some(900)).unwrap(),
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

    fn named(folders: &[wire::Folder]) -> Vec<(&str, wire::FolderKind)> {
        folders
            .iter()
            .map(|folder| (folder.name.as_str(), folder.kind()))
            .collect()
    }

    /// Two refreshes asked for one after the other can finish in either
    /// order; the later one's folders are what stay offered.
    #[test]
    fn an_older_refresh_finishing_last_does_not_replace_a_newer_one() {
        let offered = Offered::default();
        let pinned = |name: &str| wire::Folder {
            path: format!("/{name}"),
            name: name.to_owned(),
            kind: wire::FolderKind::Pinned as i32,
            ..wire::Folder::default()
        };
        let (older, newer) = (offered.ask(), offered.ask());
        assert!(
            offered.found(newer, vec![pinned("app")]),
            "the newer one moves them"
        );
        assert!(
            !offered.found(older, Vec::new()),
            "the older one, finishing last, is dropped"
        );
        assert_eq!(offered.get(), [pinned("app")]);
        let latest = offered.ask();
        assert!(
            !offered.found(latest, vec![pinned("app")]),
            "the same folders do not move"
        );
        assert!(
            offered.found(offered.ask(), Vec::new()),
            "a later unpin is kept"
        );
        assert!(offered.get().is_empty());
    }

    #[test]
    fn recent_lists_the_latest_use_of_directories_that_are_still_there() {
        let temp = tempfile::tempdir().unwrap();
        let (first, second) = (temp.path().join("first"), temp.path().join("second"));
        std::fs::create_dir(&first).unwrap();
        std::fs::create_dir(&second).unwrap();
        // An older use of the same directory cannot move it back.
        let recent = recorded(&[(&first, 2_000), (&second, 1_000), (&first, 500)]);
        let listed = folders(&[], &recent, &[]);
        assert_eq!(
            named(&listed),
            [
                ("first", wire::FolderKind::Recent),
                ("second", wire::FolderKind::Recent)
            ]
        );
        assert_eq!(listed[0].last_used_ms, Some(2_000));

        std::fs::remove_dir(&first).unwrap();
        // A junction on Windows, which takes no privilege.
        provider_fakes::faults::link_dir(&second, &first).unwrap();
        let listed = folders(&[], &recent, &[]);
        assert_eq!(named(&listed), [("second", wire::FolderKind::Recent)]);
    }

    #[test]
    fn pinned_folders_that_are_gone_or_spelled_twice_are_offered_once_or_not_at_all() {
        let temp = tempfile::tempdir().unwrap();
        for name in ["app", "site", "notes"] {
            std::fs::create_dir(temp.path().join(name)).unwrap();
        }
        let pinned = [
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
        let listed = folders(&pinned, &recent, &[]);
        assert_eq!(
            named(&listed),
            [
                ("site", wire::FolderKind::Pinned),
                ("app", wire::FolderKind::Pinned),
                ("notes", wire::FolderKind::Recent),
            ],
            "a pinned folder is not recent too"
        );
        assert!(listed[..2].iter().all(|f| f.last_used_ms.is_none()));
    }
}
