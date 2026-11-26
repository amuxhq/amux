//! The files beside the binary that record an update: `<binary>.prev`, the
//! binary that was running before the swap, whose presence means an update
//! is not yet activated; `<binary>.rejected`, the version of the last build
//! that was rolled back; and `<binary>.staged`, a download not yet
//! installed.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

use semver::Version;

use super::sync_parent;
use crate::install::write_durably;

/// The supervisor's lock file in the data directory; it holds the pid.
pub const SUPERVISOR_LOCK: &str = "supervisor.lock";

#[derive(Clone, Debug)]
pub struct Files {
    pub prev: PathBuf,
    pub rejected: PathBuf,
    pub staged: PathBuf,
    /// Where a Windows supervisor moves prev when it is its own running
    /// executable, which cannot be deleted; removed at the next start.
    pub aside: PathBuf,
}

fn beside(binary: &Path, suffix: &str) -> PathBuf {
    let mut name = binary
        .file_name()
        .map(OsString::from)
        .unwrap_or_else(|| OsString::from("amux"));
    name.push(suffix);
    binary.with_file_name(name)
}

impl Files {
    pub fn beside(binary: &Path) -> Self {
        Self {
            prev: beside(binary, ".prev"),
            rejected: beside(binary, ".rejected"),
            staged: beside(binary, ".staged"),
            aside: beside(binary, ".old"),
        }
    }

    pub fn prev_exists(&self) -> bool {
        self.prev.symlink_metadata().is_ok()
    }

    /// Whether prev is the same file as `binary`: a hard link made by a swap
    /// that never renamed over the path.
    pub fn prev_is(&self, binary: &Path) -> bool {
        matches!((FileId::of(&self.prev), FileId::of(binary)), (Ok(prev), Ok(binary)) if prev == binary)
    }

    /// The rolled-back build's version, if one was recorded.
    pub fn rejected(&self) -> Option<Version> {
        let text = std::fs::read_to_string(&self.rejected).ok()?;
        Version::parse(text.trim()).ok()
    }

    /// Records the rolled-back build, durably, before prev moves.
    pub fn write_rejected(&self, version: &Version) -> io::Result<()> {
        write_durably(&self.rejected, version.to_string().as_bytes())
    }

    pub fn remove_prev(&self) -> io::Result<()> {
        std::fs::remove_file(&self.prev)?;
        sync_parent(&self.prev)
    }

    /// Activation's cleanup: prev is no longer needed.
    pub fn retire_prev(&self, mine: &FileId) -> io::Result<()> {
        if cfg!(windows) && FileId::of(&self.prev).is_ok_and(|prev| prev == *mine) {
            std::fs::rename(&self.prev, &self.aside)?;
            return sync_parent(&self.prev);
        }
        self.remove_prev()
    }

    /// Renames prev back over the binary. On Windows the binary may be
    /// this supervisor's own running image, when it started after a swap
    /// and before the activation that would have retired prev; that cannot
    /// be replaced, so it moves aside first, where the next start removes
    /// it. Otherwise the binary is the build that just failed to start, and
    /// on Windows an image that has only just exited can stay held for a
    /// moment, so the rename over it is patient.
    pub fn restore_prev(&self, binary: &Path, mine: &FileId) -> io::Result<()> {
        if cfg!(windows) && FileId::of(binary).is_ok_and(|current| current == *mine) {
            let _ = std::fs::remove_file(&self.aside);
            std::fs::rename(binary, &self.aside)?;
        }
        os::fs::rename_patiently(&self.prev, binary)?;
        sync_parent(binary)
    }

    /// Puts the verified staged binary at the path, keeping the current one
    /// as prev. On Unix the path is never missing: prev is a hard link and
    /// the staged file is renamed over the path. On Windows, where the
    /// running file cannot be replaced, the current one is renamed aside to
    /// prev first.
    pub fn install_staged(&self, binary: &Path) -> io::Result<()> {
        if self.prev_exists() {
            std::fs::remove_file(&self.prev)?;
        }
        #[cfg(unix)]
        std::fs::hard_link(binary, &self.prev)?;
        #[cfg(windows)]
        std::fs::rename(binary, &self.prev)?;
        std::fs::rename(&self.staged, binary).inspect_err(|_| self.put_back(binary))?;
        sync_parent(binary)
    }

    /// Undoes the move aside after the staged binary failed to take the
    /// path. The path is what the service manager starts the supervisor
    /// by, and on Windows it is empty now: the current binary goes back
    /// before the failure is reported.
    #[cfg(windows)]
    fn put_back(&self, binary: &Path) {
        let _ = std::fs::rename(&self.prev, binary);
    }

    /// On Unix the path never moved, and prev, a link to it, is
    /// recognised at the next start; there is nothing to undo.
    #[cfg(not(windows))]
    fn put_back(&self, _binary: &Path) {}

    /// What an interrupted download or a Windows update leaves behind.
    pub fn clear_leftovers(&self) {
        let _ = std::fs::remove_file(&self.staged);
        let _ = std::fs::remove_file(&self.aside);
    }
}

pub fn make_executable(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// A file's identity: device and inode on Unix, volume and file index on
/// Windows. Two paths with one identity are hard links to one file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileId {
    device: u64,
    file: u64,
}

impl FileId {
    #[cfg(unix)]
    pub fn of(path: &Path) -> io::Result<Self> {
        use std::os::unix::fs::MetadataExt as _;
        let metadata = std::fs::metadata(path)?;
        Ok(Self {
            device: metadata.dev(),
            file: metadata.ino(),
        })
    }

    #[cfg(windows)]
    pub fn of(path: &Path) -> io::Result<Self> {
        use std::os::windows::io::AsRawHandle as _;

        use windows_sys::Win32::Foundation::HANDLE;
        use windows_sys::Win32::Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
        };
        let file = std::fs::File::open(path)?;
        // SAFETY: an all-zero struct is a valid out parameter.
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: querying a handle we own into a struct we own.
        if unsafe { GetFileInformationByHandle(file.as_raw_handle() as HANDLE, &mut info) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            device: u64::from(info.dwVolumeSerialNumber),
            file: (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn install() -> (tempfile::TempDir, PathBuf, Files) {
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("amux");
        std::fs::write(&binary, b"running").unwrap();
        let files = Files::beside(&binary);
        (dir, binary, files)
    }

    #[test]
    fn a_failed_install_leaves_the_binary_at_its_path() {
        let (_dir, binary, files) = install();
        // Nothing staged: the rename into place fails.
        let error = files.install_staged(&binary).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert_eq!(std::fs::read(&binary).unwrap(), b"running");
        if cfg!(windows) {
            assert!(!files.prev_exists(), "prev went back over the empty path");
        }
    }

    #[test]
    fn prev_goes_back_over_a_binary_that_is_the_running_image() {
        let (_dir, binary, files) = install();
        std::fs::write(&files.prev, b"previous").unwrap();
        let mine = FileId::of(&binary).unwrap();
        files.restore_prev(&binary, &mine).unwrap();
        assert_eq!(std::fs::read(&binary).unwrap(), b"previous");
        assert!(!files.prev_exists());
        if cfg!(windows) {
            assert_eq!(std::fs::read(&files.aside).unwrap(), b"running");
        }
    }

    /// A rollback renames prev over the build that just failed to start.
    /// On Windows an image that has only just exited can stay held for a
    /// moment; the rename waits for it rather than failing the supervisor.
    /// A handle sharing nothing stands in for that hold here.
    #[test]
    fn prev_goes_back_over_a_binary_held_for_a_moment() {
        let (_dir, binary, files) = install();
        std::fs::write(&files.prev, b"previous").unwrap();
        let held = match provider_fakes::faults::hold_exclusively(&binary) {
            Ok(held) => held,
            // Unix has no share modes, so nothing holds a file this way.
            Err(error) if error.kind() == io::ErrorKind::Unsupported => return,
            Err(error) => panic!("holding the binary: {error}"),
        };
        let release = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(200));
            drop(held);
        });
        let mine = FileId::of(&files.prev).unwrap();
        files.restore_prev(&binary, &mine).unwrap();
        release.join().unwrap();
        assert_eq!(std::fs::read(&binary).unwrap(), b"previous");
        assert!(!files.prev_exists());
    }
}
