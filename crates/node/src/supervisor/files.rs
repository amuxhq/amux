//! The files beside the binary that record an update: `<binary>.prev`, the
//! binary that was running before the swap, whose presence means an update
//! is not yet activated; `<binary>.rejected`, the version of the last build
//! that was rolled back; and `<binary>.staged`, a download not yet
//! installed. Windows' login launcher, `amux-launch`, lives beside the
//! binary too, and an update replaces it from the same release, staged as
//! `<launcher>.staged`. A download is written to `<staged>.part` and takes
//! the staged name only once it is verified, so a staged file is always
//! the release's.
//!
//! On Windows every process started from a binary keeps it mapped until it
//! exits: the supervisor, its daemon, and the agents and tool servers of
//! every earlier build still running. A mapped file can be renamed away but
//! never deleted or renamed over
//! ([`os::fs::RUNNING_IMAGE_REPLACEABLE`]). So where an update would delete
//! a binary or rename over one, a Windows supervisor first moves it aside
//! to a name of its own, `<binary>.old.<n>`, never over an existing one,
//! and deletes it there if nothing runs it. Every supervisor start and
//! every update check sweeps those names, and leaves what is still mapped
//! for the next sweep: no sweep ever fails the supervisor. `.prev`,
//! `.rejected` and `.staged` keep their names and meaning, which the next
//! release's supervisor reads.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use os::fs::{RUNNING_IMAGE_REPLACEABLE, Removed};
use semver::Version;

use super::sync_parent;
use crate::install::write_durably;

/// The supervisor's lock file in the data directory; it holds the pid.
pub const SUPERVISOR_LOCK: &str = "supervisor.lock";

/// What a binary moved aside is named after: `<binary>.old.<n>`.
const ASIDE: &str = ".old.";

/// What a download is written to until it is verified: `<staged>.part`.
const PART: &str = ".part";

#[derive(Clone, Debug)]
pub struct Files {
    binary: PathBuf,
    pub prev: PathBuf,
    pub rejected: PathBuf,
    pub staged: PathBuf,
    /// Windows' login launcher (`amux-launch`) beside the binary, which an
    /// update replaces from the same release.
    pub launcher: PathBuf,
    /// The launcher's download, not yet installed.
    pub launcher_staged: PathBuf,
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
        let launcher = binary.with_file_name(os::paths::exe_name(crate::release::LAUNCHER));
        Self {
            binary: binary.to_owned(),
            prev: beside(binary, ".prev"),
            rejected: beside(binary, ".rejected"),
            staged: beside(binary, ".staged"),
            launcher_staged: beside(&launcher, ".staged"),
            launcher,
        }
    }

    /// Where the download staged at `staged` is written until it is
    /// verified.
    pub fn part(staged: &Path) -> PathBuf {
        beside(staged, PART)
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

    /// prev is no longer needed: activation's cleanup, or a prev a start
    /// found to be the binary itself. On Windows it may be this
    /// supervisor's own image, or one agents of its build still run, so it
    /// moves aside rather than going at once.
    pub fn retire_prev(&self) -> io::Result<()> {
        discard(&self.prev, &self.binary)?;
        sync_parent(&self.prev)
    }

    /// Renames prev back over the binary. On Windows the binary at the path
    /// may be this supervisor's own running image (it started after a swap
    /// and before the activation that would have retired prev), and is at
    /// least the build that just failed to start, whose image can stay held
    /// for a moment after it exits: it moves aside first, and prev takes
    /// the free path.
    pub fn restore_prev(&self, binary: &Path) -> io::Result<()> {
        let aside = match RUNNING_IMAGE_REPLACEABLE || binary.symlink_metadata().is_err() {
            true => None,
            false => Some(move_aside(binary, &self.binary)?),
        };
        if let Err(error) = os::fs::rename_patiently(&self.prev, binary) {
            // The path is what a login item starts the supervisor by: what
            // was there goes back before the failure is reported.
            if let Some(aside) = &aside {
                let _ = os::fs::rename_patiently(aside, binary);
            }
            return Err(error);
        }
        if let Some(aside) = &aside {
            let _ = os::fs::remove_patiently(aside, Duration::ZERO);
        }
        sync_parent(binary)
    }

    /// Puts the verified staged binary at the path, keeping the current one
    /// as prev. On Unix the path is never missing: prev is a hard link and
    /// the staged file is renamed over the path. On Windows, where the
    /// running file cannot be replaced, the current one is renamed aside to
    /// prev first.
    pub fn install_staged(&self, binary: &Path) -> io::Result<()> {
        if self.prev_exists() {
            discard(&self.prev, &self.binary)?;
        }
        if RUNNING_IMAGE_REPLACEABLE {
            std::fs::hard_link(binary, &self.prev)?;
        } else {
            os::fs::rename_patiently(binary, &self.prev)?;
        }
        os::fs::rename_patiently(&self.staged, binary).inspect_err(|_| self.put_back(binary))?;
        sync_parent(binary)
    }

    /// Undoes the move aside after the staged binary failed to take the
    /// path. The path is what the service manager starts the supervisor
    /// by, and on Windows it is empty now: the current binary goes back
    /// before the failure is reported. On Unix the path never moved, and
    /// prev, a link to it, is recognised at the next start; there is
    /// nothing to undo.
    fn put_back(&self, binary: &Path) {
        if !RUNNING_IMAGE_REPLACEABLE {
            let _ = os::fs::rename_patiently(&self.prev, binary);
        }
    }

    /// Puts the staged launcher, downloaded with the binary from the same
    /// release, in place of the one beside the binary, which may be
    /// running (it is the login item's restart policy, and on Windows
    /// keeps its image mapped): that one moves aside. False when nothing
    /// was staged. A failure leaves the launcher that was there; the
    /// launcher's contract with amux is promised across releases, so an
    /// older one goes on working until the next release replaces it.
    pub fn install_launcher(&self) -> io::Result<bool> {
        if self.launcher_staged.symlink_metadata().is_err() {
            return Ok(false);
        }
        let installed = self.replace_launcher();
        let _ = os::fs::remove_patiently(&self.launcher_staged, Duration::ZERO);
        installed.map(|()| true)
    }

    fn replace_launcher(&self) -> io::Result<()> {
        let aside = match RUNNING_IMAGE_REPLACEABLE || self.launcher.symlink_metadata().is_err() {
            true => None,
            false => Some(move_aside(&self.launcher, &self.launcher)?),
        };
        if let Err(error) = os::fs::rename_patiently(&self.launcher_staged, &self.launcher) {
            if let Some(aside) = &aside {
                let _ = os::fs::rename_patiently(aside, &self.launcher);
            }
            return Err(error);
        }
        if let Some(aside) = &aside {
            let _ = os::fs::remove_patiently(aside, Duration::ZERO);
        }
        sync_parent(&self.launcher)
    }

    /// What a start finds left over: downloads an interrupted update never
    /// installed, and the binaries moved aside ([`Files::sweep_asides`]).
    /// First the launcher an update was interrupted in replacing goes back
    /// ([`Files::recover_launcher`]), since what it comes from is among
    /// what is cleared. While the launcher's path is still empty (Windows
    /// held every file that could fill it), the staged launcher is kept, as
    /// the sweep keeps the launcher's asides: the next start tries again.
    pub fn clear_leftovers(&self) {
        self.recover_launcher();
        let launcher_missing = self.launcher_missing();
        for staged in [&self.staged, &self.launcher_staged] {
            if !(launcher_missing && staged == &self.launcher_staged) {
                let _ = os::fs::remove_patiently(staged, Duration::ZERO);
            }
            let _ = os::fs::remove_patiently(&Self::part(staged), Duration::ZERO);
        }
        self.sweep_asides();
    }

    /// Puts a launcher at its path when an update stopped between moving
    /// the one there aside and renaming the staged one into its place (the
    /// supervisor killed, the machine switched off): the login item names
    /// that path, and nothing would be there to start at the next login.
    /// The staged launcher goes in if there is one, as the update would
    /// have put it (a staged file is a verified download, [`Files::part`]);
    /// else, or when it can't be moved (Windows holding it), the launcher
    /// moved aside last goes back. A launcher at the path, or nothing to
    /// put there, leaves everything as it is.
    fn recover_launcher(&self) {
        if !self.launcher_missing() {
            return;
        }
        let staged =
            Some(self.launcher_staged.clone()).filter(|staged| staged.symlink_metadata().is_ok());
        for from in staged.into_iter().chain(self.newest_launcher_aside()) {
            match os::fs::rename_patiently(&from, &self.launcher)
                .and_then(|()| sync_parent(&self.launcher))
            {
                Ok(()) => {
                    tracing::info!(
                        from = %from.display(),
                        "put back the launcher an interrupted update left missing"
                    );
                    return;
                }
                Err(error) => tracing::warn!(
                    from = %from.display(),
                    %error,
                    "could not put back the launcher an interrupted update left missing"
                ),
            }
        }
    }

    /// Whether nothing is at the launcher's path, which the login item
    /// names: what could fill it (the staged launcher, the asides) is then
    /// kept rather than cleared.
    fn launcher_missing(&self) -> bool {
        self.launcher.symlink_metadata().is_err()
    }

    /// What the launcher's asides are named after: `<launcher>.old.`.
    fn launcher_aside_stem(&self) -> Option<String> {
        Some(format!(
            "{}{ASIDE}",
            self.launcher.file_name()?.to_string_lossy()
        ))
    }

    /// The launcher moved aside last: the one written last (a rename keeps
    /// a file's time), and of two written at once the higher `n`.
    fn newest_launcher_aside(&self) -> Option<PathBuf> {
        let stem = self.launcher_aside_stem()?;
        self.asides()
            .into_iter()
            .filter_map(|aside| {
                let n: u32 = aside
                    .file_name()?
                    .to_string_lossy()
                    .strip_prefix(stem.as_str())?
                    .parse()
                    .ok()?;
                let written = aside
                    .symlink_metadata()
                    .and_then(|meta| meta.modified())
                    .ok()?;
                Some(((written, n), aside))
            })
            .max_by(|(a, _), (b, _)| a.cmp(b))
            .map(|(_, aside)| aside)
    }

    /// Removes each binary moved aside that nothing runs any more, and
    /// leaves the rest for a later sweep. Never an error: a sweep is
    /// cleanup, and nothing the supervisor does waits on it. While the
    /// launcher's path is empty, the launcher's asides are kept: one of
    /// them is what [`Files::recover_launcher`] puts back.
    pub fn sweep_asides(&self) {
        let keep = match self.launcher_missing() {
            true => self.launcher_aside_stem(),
            false => None,
        };
        for aside in self.asides() {
            let launchers = keep.as_deref().is_some_and(|stem| {
                aside
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with(stem))
            });
            if launchers {
                tracing::info!(path = %aside.display(), "kept a launcher moved aside: none is at its path");
                continue;
            }
            match os::fs::remove_patiently(&aside, Duration::ZERO) {
                Ok(Removed::Gone) => {
                    tracing::info!(path = %aside.display(), "removed a binary moved aside");
                }
                Ok(Removed::Later) => {
                    tracing::debug!(path = %aside.display(), "a binary moved aside still runs");
                }
                Err(error) => {
                    tracing::warn!(path = %aside.display(), %error, "could not remove a binary moved aside");
                }
            }
        }
    }

    /// Moves `path`, one of these files that a running process still
    /// holds, aside to a fresh `<program>.old.<n>` as an update does (the
    /// launcher's name for the launcher's files, the binary's for the
    /// rest): how uninstalling leaves what Windows won't let it delete.
    pub fn set_aside(&self, path: &Path) -> io::Result<PathBuf> {
        let program = if path == self.launcher || path == self.launcher_staged {
            &self.launcher
        } else {
            &self.binary
        };
        move_aside(path, program)
    }

    /// Every binary moved aside beside the binary, the launcher's among
    /// them: `<binary>.old.<n>` and `<launcher>.old.<n>`.
    pub fn asides(&self) -> Vec<PathBuf> {
        let Some(dir) = self.binary.parent() else {
            return Vec::new();
        };
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        let stems: Vec<String> = [&self.binary, &self.launcher]
            .iter()
            .filter_map(|path| path.file_name())
            .map(|name| format!("{}{ASIDE}", name.to_string_lossy()))
            .collect();
        let mut asides: Vec<PathBuf> = entries
            .flatten()
            .filter(|entry| {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                stems.iter().any(|stem| {
                    name.strip_prefix(stem.as_str()).is_some_and(|n| {
                        !n.is_empty() && n.bytes().all(|byte| byte.is_ascii_digit())
                    })
                })
            })
            .map(|entry| entry.path())
            .collect();
        asides.sort();
        asides
    }
}

/// Gets a binary that is no longer needed out of the way: deleted where a
/// running image can be (Unix); on Windows renamed aside, then deleted if
/// nothing runs it, and otherwise left for a sweep.
fn discard(path: &Path, binary: &Path) -> io::Result<()> {
    if RUNNING_IMAGE_REPLACEABLE {
        return std::fs::remove_file(path);
    }
    let aside = move_aside(path, binary)?;
    let _ = os::fs::remove_patiently(&aside, Duration::ZERO);
    Ok(())
}

/// Renames `path`, a build of `binary` (the binary itself, its prev, or the
/// launcher), to `<binary>.old.<n>`, the first `n` no file has, so it never
/// goes over one still mapped. Only the supervisor, which holds its lock,
/// names files so.
fn move_aside(path: &Path, binary: &Path) -> io::Result<PathBuf> {
    let aside = (1..=u32::MAX)
        .map(|n| beside(binary, &format!("{ASIDE}{n}")))
        .find(|aside| aside.symlink_metadata().is_err())
        .ok_or_else(|| io::Error::other("no free name to move a binary aside to"))?;
    os::fs::rename_patiently(path, &aside)?;
    Ok(aside)
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
    use provider_fakes::faults::hold_exclusively;

    use super::*;

    fn install() -> (tempfile::TempDir, PathBuf, Files) {
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("amux");
        std::fs::write(&binary, b"running").unwrap();
        let files = Files::beside(&binary);
        (dir, binary, files)
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn a_failed_install_leaves_the_binary_at_its_path() {
        let (_dir, binary, files) = install();
        // Nothing staged: the rename into place fails.
        let error = files.install_staged(&binary).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert_eq!(std::fs::read(&binary).unwrap(), b"running");
        if !RUNNING_IMAGE_REPLACEABLE {
            assert!(!files.prev_exists(), "prev went back over the empty path");
        }
    }

    #[test]
    fn prev_goes_back_over_the_binary_and_what_was_there_goes() {
        let (dir, binary, files) = install();
        std::fs::write(&files.prev, b"previous").unwrap();
        files.restore_prev(&binary).unwrap();
        assert_eq!(std::fs::read(&binary).unwrap(), b"previous");
        assert!(!files.prev_exists());
        assert_eq!(names(dir.path()), ["amux"], "nothing runs the failed build");
    }

    /// A rollback renames prev over the build that just failed to start.
    /// On Windows an image that has only just exited can stay held for a
    /// moment; the move aside waits for it rather than failing the
    /// supervisor. A handle sharing nothing stands in for that hold here.
    #[test]
    fn prev_goes_back_over_a_binary_held_for_a_moment() {
        let (_dir, binary, files) = install();
        std::fs::write(&files.prev, b"previous").unwrap();
        let held = match hold_exclusively(&binary) {
            Ok(held) => held,
            // Unix has no share modes, so nothing holds a file this way.
            Err(error) if error.kind() == io::ErrorKind::Unsupported => return,
            Err(error) => panic!("holding the binary: {error}"),
        };
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            drop(held);
        });
        files.restore_prev(&binary).unwrap();
        release.join().unwrap();
        assert_eq!(std::fs::read(&binary).unwrap(), b"previous");
        assert!(!files.prev_exists());
    }

    /// Two updates in a row while the builds before each still run. On
    /// Windows a retired build moves aside to a name of its own, never over
    /// one an earlier build left there, which something still holds: an
    /// agent of that build, mapping its image. A handle sharing nothing
    /// stands in for it here, refusing the deletion as a mapping does. What
    /// nothing holds goes at once; a sweep removes the rest once nothing
    /// holds it. On Unix prev is deleted at once.
    #[test]
    fn each_retired_build_moves_aside_to_a_name_of_its_own() {
        let (dir, binary, files) = install();
        let first = dir.path().join("amux.old.1");
        std::fs::write(&first, b"first").unwrap();
        let held = match hold_exclusively(&first) {
            Ok(held) => Some(held),
            // Unix has no share modes; there, nothing moves aside.
            Err(error) if error.kind() == io::ErrorKind::Unsupported => {
                std::fs::remove_file(&first).unwrap();
                None
            }
            Err(error) => panic!("holding the first build: {error}"),
        };
        for version in [&b"second"[..], b"third"] {
            std::fs::write(&files.staged, version).unwrap();
            files.install_staged(&binary).unwrap();
            assert_eq!(std::fs::read(&binary).unwrap(), version);
            files.retire_prev().unwrap();
            assert!(!files.prev_exists());
            let expected: &[&str] = match held {
                Some(_) => &["amux", "amux.old.1"],
                None => &["amux"],
            };
            assert_eq!(
                names(dir.path()),
                expected,
                "nothing holds the retired build"
            );
        }
        if held.is_some() {
            files.sweep_asides();
            assert_eq!(names(dir.path()), ["amux", "amux.old.1"], "still held");
        }
        drop(held);
        files.sweep_asides();
        assert_eq!(names(dir.path()), ["amux"]);
    }

    /// The launcher staged with the binary replaces the one beside it,
    /// which may be running.
    #[test]
    fn the_staged_launcher_replaces_the_one_beside_the_binary() {
        let (dir, _binary, files) = install();
        assert!(!files.install_launcher().unwrap(), "nothing staged");
        std::fs::write(&files.launcher, b"old launcher").unwrap();
        std::fs::write(&files.launcher_staged, b"new launcher").unwrap();
        assert!(files.install_launcher().unwrap());
        assert_eq!(std::fs::read(&files.launcher).unwrap(), b"new launcher");
        assert!(files.launcher_staged.symlink_metadata().is_err());
        let launcher = files.launcher.file_name().unwrap().to_string_lossy();
        let mut expected = vec!["amux".to_owned(), launcher.into_owned()];
        expected.sort();
        assert_eq!(names(dir.path()), expected, "the old one ran nothing");
    }

    fn launcher_name(files: &Files) -> String {
        files
            .launcher
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned()
    }

    /// A start removes what an interrupted update left: the downloads,
    /// finished or not, and every binary moved aside, the launcher's too,
    /// and nothing else.
    #[test]
    fn a_start_clears_downloads_and_binaries_moved_aside() {
        let (dir, _binary, files) = install();
        let launcher = launcher_name(&files);
        for name in [
            launcher.clone(),
            "amux.staged".to_owned(),
            "amux.staged.part".to_owned(),
            "amux.old.3".to_owned(),
            format!("{launcher}.old.1"),
            format!("{launcher}.staged"),
            format!("{launcher}.staged.part"),
            "amux.old.notes".to_owned(),
            "amux.prev".to_owned(),
        ] {
            std::fs::write(dir.path().join(name), b"").unwrap();
        }
        files.clear_leftovers();
        let mut expected = vec![
            "amux".to_owned(),
            "amux.old.notes".to_owned(),
            "amux.prev".to_owned(),
            launcher,
        ];
        expected.sort();
        assert_eq!(names(dir.path()), expected);
    }

    /// An update stopped (the supervisor killed) after moving the launcher
    /// aside and before the staged one took its path leaves the login
    /// item naming nothing. The next start puts the staged launcher there,
    /// as the update would have, before it clears downloads and sweeps.
    #[test]
    fn a_start_puts_the_staged_launcher_where_an_interrupted_update_left_none() {
        let (dir, _binary, files) = install();
        let launcher = launcher_name(&files);
        std::fs::write(
            dir.path().join(format!("{launcher}.old.1")),
            b"old launcher",
        )
        .unwrap();
        std::fs::write(&files.launcher_staged, b"new launcher").unwrap();
        files.clear_leftovers();
        assert_eq!(std::fs::read(&files.launcher).unwrap(), b"new launcher");
        let mut expected = vec!["amux".to_owned(), launcher];
        expected.sort();
        assert_eq!(names(dir.path()), expected, "the old one ran nothing");
    }

    /// With nothing staged, the launcher moved aside last goes back: the
    /// one written last, whatever its `n` (the first free one is taken).
    #[test]
    fn a_start_puts_back_the_launcher_moved_aside_last_where_none_is_left() {
        let (dir, _binary, files) = install();
        let launcher = launcher_name(&files);
        let now = std::time::SystemTime::now();
        for (n, text, age) in [(1, "newest", 0), (2, "older", 60), (3, "oldest", 120)] {
            let aside = dir.path().join(format!("{launcher}.old.{n}"));
            std::fs::write(&aside, text).unwrap();
            std::fs::File::options()
                .write(true)
                .open(&aside)
                .unwrap()
                .set_modified(now - Duration::from_secs(age))
                .unwrap();
        }
        files.clear_leftovers();
        assert_eq!(std::fs::read(&files.launcher).unwrap(), b"newest");
        let mut expected = vec!["amux".to_owned(), launcher];
        expected.sort();
        assert_eq!(names(dir.path()), expected, "nothing runs the others");

        // A launcher at its path is left as it is, and nothing comes back.
        std::fs::write(
            dir.path().join(format!("{}.old.1", launcher_name(&files))),
            b"aside",
        )
        .unwrap();
        files.clear_leftovers();
        assert_eq!(std::fs::read(&files.launcher).unwrap(), b"newest");
    }

    /// While nothing is at the launcher's path, no sweep (a start's, or
    /// the one before each update check) removes a launcher moved aside:
    /// it is what a start puts back. The binary's asides go as ever.
    #[test]
    fn a_sweep_keeps_the_launchers_asides_while_its_path_is_empty() {
        let (dir, _binary, files) = install();
        let launcher = launcher_name(&files);
        let aside = format!("{launcher}.old.1");
        std::fs::write(dir.path().join(&aside), b"old launcher").unwrap();
        std::fs::write(dir.path().join("amux.old.2"), b"old build").unwrap();
        files.sweep_asides();
        let mut expected = vec!["amux".to_owned(), aside];
        expected.sort();
        assert_eq!(names(dir.path()), expected);
    }

    /// Windows holding the staged launcher (a virus scanner reading the
    /// new download) fails its rename into place: the launcher moved aside
    /// last goes back instead. With that one held too, nothing fills the
    /// path, and the next start finds the staged launcher and the aside
    /// both still there, and puts the staged one in.
    #[test]
    fn a_start_falls_back_to_the_aside_when_the_staged_launcher_is_held() {
        let (dir, _binary, files) = install();
        let launcher = launcher_name(&files);
        let aside = dir.path().join(format!("{launcher}.old.1"));
        std::fs::write(&aside, b"old launcher").unwrap();
        std::fs::write(&files.launcher_staged, b"new launcher").unwrap();
        let staged = match hold_exclusively(&files.launcher_staged) {
            Ok(held) => held,
            // Unix has no share modes, so nothing holds a file this way.
            Err(error) if error.kind() == io::ErrorKind::Unsupported => return,
            Err(error) => panic!("holding the staged launcher: {error}"),
        };
        files.clear_leftovers();
        assert_eq!(std::fs::read(&files.launcher).unwrap(), b"old launcher");
        drop(staged);

        // Both held: the path stays empty, and what could fill it stays.
        std::fs::remove_file(&files.launcher).unwrap();
        std::fs::write(&aside, b"old launcher").unwrap();
        std::fs::write(&files.launcher_staged, b"new launcher").unwrap();
        let held = [
            hold_exclusively(&files.launcher_staged).unwrap(),
            hold_exclusively(&aside).unwrap(),
        ];
        files.clear_leftovers();
        drop(held);
        assert!(files.launcher.symlink_metadata().is_err());
        files.sweep_asides();
        let mut expected = vec![
            "amux".to_owned(),
            format!("{launcher}.old.1"),
            format!("{launcher}.staged"),
        ];
        expected.sort();
        assert_eq!(names(dir.path()), expected, "nothing usable swept");

        files.clear_leftovers();
        assert_eq!(std::fs::read(&files.launcher).unwrap(), b"new launcher");
        let mut expected = vec!["amux".to_owned(), launcher];
        expected.sort();
        assert_eq!(names(dir.path()), expected);
    }
}
