//! The installation: one data directory, one daemon.
//!
//! ```text
//! <data_dir>/
//!   installation.lock     exclusive; one daemon per data dir
//!   generation            boot id, clean-shutdown flag, generation counter
//!   installation_id       this installation's random id, written once
//!   reports/              debug bundles
//!   profiles/<profile_id>/  one per profile; the id is its host id
//!     profile             what it is called and when it was made
//!     store.sqlite        the profile store
//!     core-ring/          the core's checkpoints and every input it
//!                         stepped since each
//!     agents/<agent_id>/  one directory per agent, the agent's contract
//!     replicas/<host_id>/agents/<agent_id>/blobs/
//!                         bytes fetched for a peer's agent
//! ```

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const INSTALLATION_LOCK: &str = "installation.lock";
pub const GENERATION: &str = "generation";
pub const REPORTS: &str = "reports";
pub const PROFILES: &str = "profiles";
pub const STORE: &str = "store.sqlite";
pub const CORE_RING: &str = "core-ring";
pub const AGENTS: &str = "agents";
pub const REPLICAS: &str = "replicas";
pub const INSTALLATION_ID: &str = "installation_id";

/// The installation's id, minted at its first start: what counts an install,
/// apart from the host ids of its profiles.
pub(crate) fn installation_id(data_dir: &Path) -> io::Result<uuid::Uuid> {
    let path = data_dir.join(INSTALLATION_ID);
    match fs::read_to_string(&path) {
        Ok(text) => match text.trim().parse() {
            Ok(id) => return Ok(id),
            // Unreadable is as good as missing: the id only counts installs.
            Err(_) => tracing::warn!(path = %path.display(), "minting a new installation id"),
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let id = uuid::Uuid::new_v4();
    write_durably(&path, id.to_string().as_bytes())?;
    Ok(id)
}

/// The installation's exclusive lock, held for the daemon's lifetime. The
/// file is never removed: unlinking it would let a second opener lock a
/// different inode while this one is still held. It holds no data; what it
/// would hold stays readable while it is held ([`os::lock::FileLock`]).
#[derive(Debug)]
pub struct InstallationLock {
    _lock: os::lock::FileLock,
}

impl InstallationLock {
    /// Takes `<data_dir>/installation.lock`, creating the directory as
    /// needed, or reports that another daemon holds it.
    pub fn acquire(data_dir: &Path) -> Result<Self, LockError> {
        private_dir(data_dir).map_err(LockError::Io)?;
        let path = data_dir.join(INSTALLATION_LOCK);
        match os::lock::FileLock::try_acquire(&path).map_err(LockError::Io)? {
            Some(lock) => Ok(Self { _lock: lock }),
            None => Err(LockError::Busy(path)),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LockError {
    #[error("another amux daemon holds {}", .0.display())]
    Busy(PathBuf),
    #[error("taking the installation lock: {0}")]
    Io(io::Error),
}

/// How many reports `reports/` keeps: dump bundles, and the text files the
/// relay and the supervisor write when their cores break.
pub const REPORTS_KEPT: usize = 20;

/// Removes all but the newest [`REPORTS_KEPT`] reports in `dir`, by when
/// each was last written. A dotted name, a bundle still being assembled,
/// is left alone.
pub fn prune_reports(dir: &Path) -> io::Result<()> {
    let mut reports = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        let meta = entry.metadata()?;
        reports.push((meta.modified()?, entry.file_name()));
    }
    reports.sort();
    let excess = reports.len().saturating_sub(REPORTS_KEPT);
    for (_, name) in reports.into_iter().take(excess) {
        // One a person has open is pruned at a later report.
        remove_or_leave(&dir.join(name), Duration::ZERO, "an old report");
    }
    Ok(())
}

/// Removes what dumps a crash interrupted left: bundles being assembled
/// under `reports/`, and the store copies beside each profile's store,
/// which are unredacted. Run at start, under the installation lock, when no
/// dump can be under way.
pub(crate) fn sweep_dump_leftovers(data_dir: &Path, profile_dirs: &[PathBuf]) {
    let dirs = std::iter::once(data_dir.join(REPORTS)).chain(profile_dirs.iter().cloned());
    for dir in dirs {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if !entry.file_name().to_string_lossy().starts_with(".dump-") {
                continue;
            }
            remove_or_leave(&entry.path(), Duration::ZERO, "a dump's leftover");
        }
    }
}

/// Creates a directory, and its parents, only this user can enter. One
/// that is there already is left as it is: it may be a folder a person
/// chose to hold a socket. What an earlier build left of the root is made
/// private at start, by [`prepare_root`].
pub fn private_dir(path: &Path) -> io::Result<()> {
    if path.is_dir() {
        return Ok(());
    }
    os::fs::create_private_dir(path)
}

/// Replaces `path` with `bytes` so that a crash or power cut leaves either
/// the old file or the new one, and the new one is on disk before this
/// returns, readable by this user only ([`os::fs::write_durably`]).
pub use os::fs::write_durably;

/// Makes a rename or a creation in `dir` durable ([`os::fs::sync_dir`]).
pub use os::fs::sync_dir;

/// Replaces `path` with `bytes` so that a reader sees the old file or the
/// new one, never half of one, without waiting for the drive: for a file
/// whose loss to a power cut costs only a recomputation or a stale hint.
pub fn write_cache(path: &Path, bytes: &[u8]) -> io::Result<()> {
    os::fs::replace_atomically(path, bytes)
}

/// How long a removal in a sweep or a discard waits for something else to
/// let go of what it removes before leaving it for the next pass.
pub(crate) const REMOVAL_PATIENCE: Duration = Duration::from_secs(3);

/// Removes the file or tree at `path`, waiting up to `patience` for
/// something else to let go of it. One still held (a shell working inside,
/// a log open without delete sharing) or refused is left, with a warning
/// naming `what`, for the next pass, never an error: true once it is gone.
pub(crate) fn remove_or_leave(path: &Path, patience: Duration, what: &str) -> bool {
    match os::fs::remove_patiently(path, patience) {
        Ok(os::fs::Removed::Gone) => true,
        Ok(os::fs::Removed::Later) => {
            tracing::warn!(path = %path.display(), "{what} is still in use; the next pass removes it");
            false
        }
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "{what} could not be removed; the next pass tries again");
            false
        }
    }
}

/// Readies the installation's root at the daemon's start. It says when the
/// root's volume is a poor home for one, and makes the root, its reports
/// and each profile and its agents only this user's, which upgrades in
/// place a root an earlier build left readable by others, as every one on
/// Windows was. Nothing here stops a start: what fails is said and left.
pub(crate) fn prepare_root(data_dir: &Path) {
    match os::fs::volume_supports_root(data_dir) {
        Ok(os::fs::VolumeCheck::Suitable) => {}
        Ok(check) => tracing::warn!(
            root = %data_dir.display(),
            "the root's volume is a poor home for amux's data ({check}): a file something \
             holds may not be replaced or removed, and the files may not be private"
        ),
        Err(error) => {
            tracing::warn!(root = %data_dir.display(), %error, "the root's volume could not be checked");
        }
    }
    let private = |path: &Path| {
        if let Err(error) = os::fs::make_private(path) {
            tracing::warn!(path = %path.display(), %error, "could not be made private");
        }
    };
    private(data_dir);
    if let Err(error) = os::fs::create_private_dir(&data_dir.join(REPORTS)) {
        tracing::warn!(%error, "the reports folder could not be made private");
    }
    let Ok(profiles) = fs::read_dir(data_dir.join(PROFILES)) else {
        return;
    };
    for profile in profiles.flatten() {
        let dir = profile.path();
        if !dir.is_dir() {
            continue;
        }
        private(&dir);
        let agents = dir.join(AGENTS);
        if agents.is_dir() {
            private(&agents);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_keep_the_newest_and_leave_a_bundle_being_assembled() {
        let dir = tempfile::tempdir().unwrap();
        let old = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000);
        for n in 0..REPORTS_KEPT + 2 {
            let path = dir.path().join(format!("dump-{n:02}"));
            fs::create_dir(&path).unwrap();
            fs::write(path.join("manifest.json"), b"{}").unwrap();
            let at = old + std::time::Duration::from_secs(n as u64);
            let mut options = fs::OpenOptions::new();
            options.read(true);
            // Windows opens a directory only with backup semantics, and
            // sets its times only through a handle that may write them.
            #[cfg(windows)]
            {
                use std::os::windows::fs::OpenOptionsExt;
                options.write(true).custom_flags(0x0200_0000);
            }
            options.open(&path).unwrap().set_modified(at).unwrap();
        }
        let text = dir.path().join("relay-1-ab.json");
        fs::write(&text, b"window").unwrap();
        fs::create_dir(dir.path().join(".dump-99")).unwrap();

        prune_reports(dir.path()).unwrap();
        let mut left: Vec<String> = fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left.len(), REPORTS_KEPT + 1, "{left:?}");
        assert!(left.contains(&".dump-99".to_owned()));
        assert!(left.contains(&"relay-1-ab.json".to_owned()));
        // The three oldest bundles went.
        for gone in ["dump-00", "dump-01", "dump-02"] {
            assert!(!left.contains(&gone.to_owned()), "{left:?}");
        }
    }

    /// A write whose replace fails takes its temporary file with it: what
    /// a failed write leaves is the old file, and only the old file.
    #[test]
    fn a_durable_write_that_fails_leaves_no_temporary_file() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join(GENERATION);
        fs::create_dir(&target).unwrap();
        fs::write(target.join("inside"), b"x").unwrap();
        assert!(write_durably(&target, b"x").is_err());
        assert!(write_cache(&target, b"x").is_err());
        let left: Vec<String> = fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(left, [GENERATION], "nothing but the target is left");
    }

    #[test]
    fn a_crashed_dumps_leftovers_are_swept_and_nothing_else() {
        let data = tempfile::tempdir().unwrap();
        let reports = data.path().join(REPORTS);
        let profile = data.path().join(PROFILES).join("p");
        fs::create_dir_all(reports.join(".dump-1-ab")).unwrap();
        fs::create_dir_all(reports.join("dump-2-cd")).unwrap();
        fs::create_dir_all(&profile).unwrap();
        for name in [".dump-ef.sqlite", ".dump-ef.sqlite-wal", STORE] {
            fs::write(profile.join(name), b"x").unwrap();
        }
        sweep_dump_leftovers(data.path(), std::slice::from_ref(&profile));
        assert!(!reports.join(".dump-1-ab").exists());
        assert!(reports.join("dump-2-cd").exists());
        assert!(!profile.join(".dump-ef.sqlite").exists());
        assert!(!profile.join(".dump-ef.sqlite-wal").exists());
        assert!(profile.join(STORE).exists());
    }
}
