//! Filesystem faults a test injects, one call on every OS, so a test that
//! needs a write to fail or a file to be unreadable needs no `cfg`.
//!
//! Each fault lasts as long as the guard it returns and is undone when the
//! guard drops, so the test's temporary folder can be removed after it.
//!
//! | Fault | Unix | Windows |
//! |---|---|---|
//! | [`deny`] | the mode's bits off (`chmod`) | a deny entry for Everyone at the head of the object's DACL |
//! | [`read_only`] | every write bit off | the read-only attribute |
//! | [`hold_exclusively`] | `Unsupported`: Unix has no share modes | a handle opened sharing nothing, so every other open fails with a sharing violation |
//! | [`link_dir`] | a symbolic link | a junction, which takes no privilege |
//! | [`symlink`] | a symbolic link | a symbolic link, or [`Linked::Skipped`] with its reason where the user may not make one |
//!
//! A test whose fault is a permission checks [`unenforced`] first: a
//! process running as root on Unix reads and writes whatever the mode
//! says, so the fault would not happen.
//!
//! Tests are its only readers, so it is built only with the `testing`
//! feature. `provider_fakes::faults` re-exports it for the tests that
//! already depend on the fakes.

use std::fs::{File, Permissions};
use std::io;
use std::path::{Path, PathBuf};

#[cfg(test)]
mod tests;
#[cfg(unix)]
mod unix;
#[cfg(unix)]
use unix as imp;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as imp;

/// What [`deny`] takes away.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Deny {
    /// Reading a file's contents, or listing a directory.
    Read,
    /// Changing a file's contents, or adding to or removing from a
    /// directory: what a full disk refuses.
    Write,
    /// Everything but reading the metadata: `chmod 000`.
    All,
}

/// A [`deny`] in force; dropping it gives back what it took.
#[derive(Debug)]
pub struct Denied {
    path: PathBuf,
    restore: Option<imp::Restore>,
}

impl Denied {
    /// Gives back what was taken now, reporting a failure the drop would
    /// swallow.
    pub fn undo(mut self) -> io::Result<()> {
        match self.restore.take() {
            Some(restore) => restore.apply(&self.path),
            None => Ok(()),
        }
    }
}

impl Drop for Denied {
    fn drop(&mut self) {
        if let Some(restore) = self.restore.take() {
            let _ = restore.apply(&self.path);
        }
    }
}

/// Makes `path` (a file or a directory) refuse `what` to every process of
/// this user until the guard drops.
pub fn deny(path: &Path, what: Deny) -> io::Result<Denied> {
    let restore = imp::deny(path, what)?;
    Ok(Denied {
        path: path.to_owned(),
        restore: Some(restore),
    })
}

/// A [`read_only`] in force; dropping it puts the permissions back.
#[derive(Debug)]
pub struct ReadOnly {
    path: PathBuf,
    was: Option<Permissions>,
}

impl Drop for ReadOnly {
    fn drop(&mut self) {
        if let Some(was) = self.was.take() {
            let _ = std::fs::set_permissions(&self.path, was);
        }
    }
}

/// Marks the file at `path` read-only until the guard drops: opening it to
/// write fails, and on Windows so does removing it.
pub fn read_only(path: &Path) -> io::Result<ReadOnly> {
    let was = std::fs::metadata(path)?.permissions();
    let mut read_only = was.clone();
    read_only.set_readonly(true);
    std::fs::set_permissions(path, read_only)?;
    Ok(ReadOnly {
        path: path.to_owned(),
        was: Some(was),
    })
}

/// The file at `path`, held open so that no other open of it succeeds
/// until this drops.
#[derive(Debug)]
pub struct Held(#[allow(dead_code)] File);

/// Opens the file at `path` sharing nothing, as an editor, a virus scanner
/// or another amux process holding it does on Windows: every other open,
/// for any access, fails with a sharing violation
/// ([`is_sharing_violation`]) until the guard drops. `Unsupported` on
/// Unix, where opens never exclude each other.
pub fn hold_exclusively(path: &Path) -> io::Result<Held> {
    imp::hold_exclusively(path).map(Held)
}

/// Whether `error` is the one an open of a file [`hold_exclusively`]
/// holds fails with.
pub fn is_sharing_violation(error: &io::Error) -> bool {
    imp::is_sharing_violation(error)
}

/// Makes `link` a link to the directory `target`, which must exist: a
/// symbolic link on Unix, a junction on Windows, where a junction needs no
/// privilege and a directory symbolic link does. Reading through either
/// reaches the target; [`remove_link`] removes the link and leaves the
/// target as it was.
pub fn link_dir(target: &Path, link: &Path) -> io::Result<()> {
    imp::link_dir(&std::path::absolute(target)?, link)
}

/// Removes a link [`link_dir`] or [`symlink`] made, never what it names.
pub fn remove_link(link: &Path) -> io::Result<()> {
    imp::remove_link(link)
}

/// What [`symlink`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Linked {
    Made,
    /// The OS would not let this user make a symbolic link: the test that
    /// needs one skips, saying this.
    Skipped(String),
}

/// Makes `link` a symbolic link to `target` (a file, or a directory when
/// `target` is one now). Where the user lacks the privilege to make one
/// (Windows without Developer Mode or `SeCreateSymbolicLinkPrivilege`),
/// that is [`Linked::Skipped`] with its reason, not an error, so the test
/// skips at run time saying why.
pub fn symlink(target: &Path, link: &Path) -> io::Result<Linked> {
    imp::symlink(target, link)
}

/// Disables the privileges that carry this process past an ACL, where its
/// token has them enabled, and names those it disabled: on Windows
/// SeBackup, SeRestore and SeTakeOwnership, which open a file whatever its
/// DACL says (`DeleteFileW` and `RemoveDirectoryW` ask with backup intent),
/// and SeDebug, which opens any process. An administrator's token holds
/// them disabled, but a runner may hand its tests one with them enabled:
/// on GitHub's Windows runner the refusals tests set up in DACLs never
/// happened (CI run 45718964949). Processes started afterwards inherit the token as
/// it is then. On Unix there is nothing to disable: see [`unenforced`].
///
/// Buck runs each test in a process of its own, so this lasts the test.
pub fn hold_to_acls() -> io::Result<Vec<&'static str>> {
    imp::hold_to_acls()
}

/// Why a permission fault would not happen to this process, when it would
/// not: run as root on Unix, a process reads and writes whatever the mode
/// says. A test injecting [`deny`] or [`read_only`] returns early, saying
/// this, when it is `Some`.
pub fn unenforced() -> Option<&'static str> {
    static UNENFORCED: std::sync::OnceLock<Option<&'static str>> = std::sync::OnceLock::new();
    *UNENFORCED.get_or_init(|| {
        // This crate takes no tempfile (docs/ARCHITECTURE.md's crate map),
        // so the probe is named for this process and removed after.
        let probe = std::env::temp_dir().join(format!("os-faults-probe-{}", std::process::id()));
        std::fs::write(&probe, "probe").ok()?;
        let read = deny(&probe, Deny::All).ok().map(|denied| {
            let read = std::fs::read(&probe).is_ok();
            drop(denied);
            read
        });
        let _ = std::fs::remove_file(&probe);
        read?.then_some("this process is not held to file permissions (it runs as root)")
    })
}
