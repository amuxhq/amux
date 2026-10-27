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

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

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
/// different inode while this one is still held.
#[derive(Debug)]
pub struct InstallationLock {
    file: Option<File>,
    path: PathBuf,
}

impl InstallationLock {
    /// Takes `<data_dir>/installation.lock`, creating the directory as
    /// needed, or reports that another daemon holds it.
    pub fn acquire(data_dir: &Path) -> Result<Self, LockError> {
        private_dir(data_dir).map_err(LockError::Io)?;
        let path = data_dir.join(INSTALLATION_LOCK);
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(LockError::Io)?;
        match file.try_lock() {
            Ok(()) => Ok(Self {
                file: Some(file),
                path,
            }),
            Err(fs::TryLockError::WouldBlock) => Err(LockError::Busy(path)),
            Err(fs::TryLockError::Error(error)) => Err(LockError::Io(error)),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for InstallationLock {
    fn drop(&mut self) {
        // Unlock explicitly: a descriptor a concurrently forked child
        // inherited would otherwise keep the lock until that child execs.
        if let Some(file) = self.file.take() {
            let _ = file.unlock();
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
        reports.push((meta.modified()?, entry.file_name(), meta.is_dir()));
    }
    reports.sort();
    let excess = reports.len().saturating_sub(REPORTS_KEPT);
    for (_, name, is_dir) in reports.into_iter().take(excess) {
        let path = dir.join(name);
        let removed = if is_dir {
            fs::remove_dir_all(&path)
        } else {
            fs::remove_file(&path)
        };
        match removed {
            Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
            _ => {}
        }
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
            let path = entry.path();
            let removed = if path.is_dir() {
                fs::remove_dir_all(&path)
            } else {
                fs::remove_file(&path)
            };
            if let Err(error) = removed {
                tracing::warn!(%error, path = %path.display(), "cannot remove a dump's leftover");
            }
        }
    }
}

/// Creates a directory, and its parents, only this user can enter.
pub fn private_dir(path: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)
}

/// Replaces `path` with `bytes` so that a crash or power cut leaves either
/// the old file or the new one, and the new one is on disk before this
/// returns: a temporary file, fsync, rename, fsync of the directory.
pub fn write_durably(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let (dir, temp) = temp_beside(path)?;
    {
        let mut file = File::create(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    fs::rename(&temp, path)?;
    sync_dir(dir)
}

/// Replaces `path` with `bytes` so that a reader sees the old file or the
/// new one, never half of one, without waiting for the drive: for a file
/// whose loss to a power cut costs only a recomputation or a stale hint.
pub fn write_cache(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let (_, temp) = temp_beside(path)?;
    fs::write(&temp, bytes)?;
    fs::rename(&temp, path).inspect_err(|_| {
        let _ = fs::remove_file(&temp);
    })
}

/// The directory `path` is in, and a temporary name beside it that no
/// other process writing the same file uses.
fn temp_beside(path: &Path) -> io::Result<(&Path, std::path::PathBuf)> {
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::other("a file written whole needs a parent directory"))?;
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::other("a file written whole needs a name"))?;
    let temp = dir.join(format!(
        ".{}.{}.tmp",
        name.to_string_lossy(),
        std::process::id()
    ));
    Ok((dir, temp))
}

/// Makes a rename in `dir` durable.
#[cfg(unix)]
pub fn sync_dir(dir: &Path) -> io::Result<()> {
    File::open(dir)?.sync_all()
}

/// Windows makes a rename durable with the file's own flush; a directory
/// cannot be opened for one.
#[cfg(not(unix))]
pub fn sync_dir(_dir: &Path) -> io::Result<()> {
    Ok(())
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
        let text = dir.path().join("relay-1-ab.txt");
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
        assert!(left.contains(&"relay-1-ab.txt".to_owned()));
        // The three oldest bundles went.
        for gone in ["dump-00", "dump-01", "dump-02"] {
            assert!(!left.contains(&gone.to_owned()), "{left:?}");
        }
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
