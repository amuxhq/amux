//! The file system as amux uses it: owner-only folders and files, durable
//! whole-file writes, patient renames and removals, one canonical path
//! form, and what a root's volume must support.
//!
//! Each function is one API with an implementation per OS (`unix.rs`,
//! `windows.rs`); callers never write `cfg`. A primitive an OS cannot
//! provide returns [`io::ErrorKind::Unsupported`], unless its doc comment
//! names the exception.

use std::fs::{File, Metadata, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use std::{fmt, io};

#[cfg(unix)]
mod unix;
#[cfg(unix)]
use unix as imp;
mod verbatim;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as imp;

#[cfg(test)]
mod tests;

/// How long a durable write and [`rename_patiently`] keep retrying while
/// something else holds the file: long enough for a virus scanner or an
/// indexer that opened it without delete sharing, short enough that a
/// handle held for good is reported, not waited on.
pub const SHARING_PATIENCE: Duration = Duration::from_secs(1);

// Owner-only ----------------------------------------------------------------

/// Creates `dir`, and any missing parent, with `dir` itself owner-only; an
/// existing `dir` is made owner-only ([`make_private`]).
///
/// Unix: mode 0700 (on the parents it creates too). Windows: a protected
/// DACL giving full control to the current user, SYSTEM and Administrators
/// only, inherited (OI|CI) by what is created inside, set as the folder is
/// created, so no other user can open it in between; parents it creates
/// inherit theirs as usual.
pub fn create_private_dir(dir: &Path) -> io::Result<()> {
    imp::create_private_dir(dir)
}

/// Makes the existing file or folder at `path` owner-only.
///
/// Unix: 0700 for a folder, 0600 for a file (0700 for one with an execute
/// bit, which it keeps). Windows: the protected DACL [`create_private_dir`]
/// describes, applied to a folder's contents by inheritance (anything
/// inside that inherits its ACL takes the new one), so a folder that was
/// not private becomes so in place, with what it holds.
pub fn make_private(path: &Path) -> io::Result<()> {
    imp::make_private(path)
}

/// Options that create a file only its owner can open: write access, to
/// which the caller adds what else it needs.
///
/// Unix: mode 0600. Windows: std cannot carry a security descriptor into
/// the file's creation, so the file takes its folder's inherited DACL:
/// owner-only inside a [`create_private_dir`] folder or one
/// [`make_private`] was applied to, and only there. A secret that may land
/// anywhere else is made with [`create_private`], which is owner-only
/// wherever it creates; so are [`write_durably`] and [`create_new_durably`].
pub fn private_open_options() -> OpenOptions {
    imp::private_open_options()
}

/// How [`create_private`] treats a file already at the path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Create {
    /// Only a new file: an existing one is [`io::ErrorKind::AlreadyExists`].
    New,
    /// A new file, or the existing one emptied.
    Truncate,
    /// A new file, or the existing one, every write going to its end.
    Append,
}

/// Opens `path` for reading and writing (appending, for
/// [`Create::Append`]), creating it owner-only.
///
/// A file it creates is owner-only from its creation, wherever it is:
/// Unix, mode 0600 given to `open`; Windows, the protected DACL
/// [`create_private_dir`] describes, given to `CreateFileW`, so no other
/// user can open it in between. An existing file it opens is made
/// owner-only through the handle before this returns (0600 by `fchmod`;
/// the same DACL by `SetSecurityInfo`).
pub fn create_private(path: &Path, how: Create) -> io::Result<File> {
    imp::create_private(path, how)
}

/// Whether `path` is owner-only in its own right, as [`create_private_dir`],
/// [`make_private`] and [`create_private`] leave it, for a test to check;
/// [`crate::testing::is_private`] is its public form.
#[cfg(any(test, feature = "testing"))]
pub(crate) fn is_private(path: &Path) -> io::Result<bool> {
    imp::is_private(path)
}

/// The current user's SID as a string (`S-1-5-21-…`), for an SDDL.
/// Windows only: elsewhere it returns [`io::ErrorKind::Unsupported`].
pub fn current_user_sid() -> io::Result<String> {
    imp::current_user_sid()
}

// Durable whole-file writes --------------------------------------------------

/// Replaces `path` with `bytes` so that a crash or power cut leaves either
/// the old file or the new one, and the new one is on disk before this
/// returns.
///
/// A temporary file beside `path`, written, flushed, then atomically put in
/// place, then the folder synced ([`sync_dir`]). Windows replaces through
/// the open temporary file with POSIX semantics (`FileRenameInfoEx`,
/// `REPLACE_IF_EXISTS | POSIX_SEMANTICS`), so a reader holding the old file
/// with delete sharing doesn't stop it, falling back to a plain replace on
/// a volume that refuses POSIX semantics. A sharing violation is retried
/// for [`SHARING_PATIENCE`], then reported. The temporary file is never
/// left behind, whatever fails.
///
/// The result is owner-only on every OS: 0600 on Unix, the DACL
/// [`create_private_dir`] describes on Windows.
pub fn write_durably(path: &Path, bytes: &[u8]) -> io::Result<()> {
    durably(path, bytes, Publish::Replace, Flush::Yes)
}

/// Replaces `path` with `bytes` as [`write_durably`] does, readers seeing
/// the old file or the new one and never part of either, but with nothing
/// flushed: for a file that means nothing after a crash, such as the name
/// of a pipe that died with its process.
pub fn replace_atomically(path: &Path, bytes: &[u8]) -> io::Result<()> {
    durably(path, bytes, Publish::Replace, Flush::No)
}

/// Writes `bytes` to `path` as [`write_durably`] does, but only if `path`
/// does not exist: otherwise [`io::ErrorKind::AlreadyExists`], with the
/// existing file untouched. The check and the creation are one step, so of
/// two writers exactly one succeeds.
pub fn create_new_durably(path: &Path, bytes: &[u8]) -> io::Result<()> {
    durably(path, bytes, Publish::New, Flush::Yes)
}

/// How a durable write puts its temporary file in place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Publish {
    Replace,
    New,
}

/// Whether a whole-file write reaches the disk before it returns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Flush {
    Yes,
    No,
}

fn durably(path: &Path, bytes: &[u8], publish: Publish, flush: Flush) -> io::Result<()> {
    let name = path.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} names no file to write", path.display()),
        )
    })?;
    let dir = match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    };
    sweep_left_stages();
    let (temp, file) = temp_beside(dir, &name.to_string_lossy())?;
    // Windows deletes the temporary file through its own handle when this
    // fails, before anyone else can open it; what follows is for whatever
    // that could not do.
    match imp::finish_durably(file, &temp, path, bytes, publish, flush) {
        Ok(()) if flush == Flush::Yes => sync_dir(dir),
        Ok(()) => Ok(()),
        Err(error) => Err(abandon_stage(&temp, error)),
    }
}

/// Temporary files a failed write could not remove yet (something else
/// held them), removed by a later write.
static LEFT_STAGES: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());

/// Removes the temporary file of a write that failed with `error`, waiting
/// out a brief hold; one still held is kept for a later write to remove,
/// and the error says so.
fn abandon_stage(temp: &Path, error: io::Error) -> io::Error {
    match remove_patiently(temp, SHARING_PATIENCE) {
        Ok(Removed::Gone) => error,
        Ok(Removed::Later) | Err(_) => {
            LEFT_STAGES
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(temp.to_owned());
            io::Error::new(
                error.kind(),
                format!(
                    "{error} (its temporary file {} is still held; a later write removes it)",
                    temp.display()
                ),
            )
        }
    }
}

/// Removes what [`abandon_stage`] could not, keeping what is still held.
fn sweep_left_stages() {
    let left = std::mem::take(
        &mut *LEFT_STAGES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    );
    if left.is_empty() {
        return;
    }
    let still: Vec<PathBuf> = left
        .into_iter()
        .filter(|temp| !matches!(remove_patiently(temp, Duration::ZERO), Ok(Removed::Gone)))
        .collect();
    LEFT_STAGES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .extend(still);
}

/// The temporary files failed writes left that are still to be removed.
#[cfg(test)]
fn left_stages() -> Vec<PathBuf> {
    LEFT_STAGES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

/// A new owner-only file beside the target, under a name no other writer
/// uses: `.<name>.<pid>.<n>.tmp`.
fn temp_beside(dir: &Path, name: &str) -> io::Result<(PathBuf, File)> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let pid = std::process::id();
    loop {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let temp = dir.join(format!(".{name}.{pid}.{n}.tmp"));
        match imp::create_temp(&temp) {
            Ok(file) => return Ok((temp, file)),
            // A crashed writer's leftover under a reused pid.
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists && n < u64::MAX => continue,
            Err(error) => return Err(error),
        }
    }
}

/// Makes a rename or creation in `dir` durable.
///
/// Unix: fsync of the folder. Windows: the folder opened with
/// `FILE_FLAG_BACKUP_SEMANTICS | GENERIC_WRITE`, then `FlushFileBuffers`,
/// which NTFS and ReFS honour for a folder: it commits the folder's index,
/// where a rename lands.
pub fn sync_dir(dir: &Path) -> io::Result<()> {
    imp::sync_dir(dir)
}

// Patient reads, renames and removals -----------------------------------------

/// Reads the whole of `path`, retrying for [`SHARING_PATIENCE`] while
/// something else holds it. [`write_durably`] and [`create_new_durably`]
/// put a file in place through a handle shared with nobody, closed a
/// moment later, so on Windows a reader that finds the file in that moment
/// meets a sharing violation; Unix has none, so it is one read there.
pub fn read_patiently(path: &Path) -> io::Result<Vec<u8>> {
    patiently(SHARING_PATIENCE, || std::fs::read(path))
}

/// Renames `from` to `to`, replacing a file at `to`, retrying for
/// [`SHARING_PATIENCE`] while something else holds either one (Windows'
/// sharing violations; Unix has none, so it is one rename there).
pub fn rename_patiently(from: &Path, to: &Path) -> io::Result<()> {
    patiently(SHARING_PATIENCE, || std::fs::rename(from, to))
}

/// Whether the file a running process was started from can be deleted or
/// renamed over while it runs: on Unix, where the process keeps the inode;
/// not on Windows, which keeps the image mapped and refuses both until
/// every process running it has exited. There it can only be renamed
/// away, to a name nothing else uses.
pub const RUNNING_IMAGE_REPLACEABLE: bool = cfg!(unix);

/// What [`remove_patiently`] did.
#[must_use]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Removed {
    /// Nothing is at the path any more (or nothing was).
    Gone,
    /// Something else still holds it or something inside it: a process
    /// with its working directory there, or a file open without delete
    /// sharing. What could go has gone; try again later.
    Later,
}

/// Removes the file or folder tree at `path`, retrying for `patience`
/// while something else holds it, then reporting [`Removed::Later`]
/// instead of an error: a sweep leaves what's held for its next pass, and
/// nothing a start does dies of it. A read-only file is removed too. An
/// error is only what waiting can't fix, such as a permission refused.
pub fn remove_patiently(path: &Path, patience: Duration) -> io::Result<Removed> {
    let started = Instant::now();
    let mut pause = Duration::from_millis(5);
    loop {
        let removed = match std::fs::symlink_metadata(path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Removed::Gone),
            Err(error) => Err(error),
            // A link goes itself, never what it points at: a link to a
            // folder (a junction too, on Windows) goes as the file
            // removal's own case.
            Ok(meta) if meta.is_dir() => imp::remove_dir_all(path),
            Ok(_) => imp::remove_file(path),
        };
        match removed {
            Ok(()) => return Ok(Removed::Gone),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Removed::Gone),
            Err(error) if imp::is_held(&error) => {
                if started.elapsed() >= patience {
                    return Ok(Removed::Later);
                }
            }
            Err(error) => return Err(error),
        }
        std::thread::sleep(pause.min(patience.saturating_sub(started.elapsed())));
        pause = (pause * 2).min(Duration::from_millis(100));
    }
}

/// Runs `attempt` until it succeeds, fails for a reason other than a
/// sharing violation, or `patience` runs out.
fn patiently<T>(patience: Duration, mut attempt: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    let started = Instant::now();
    let mut pause = Duration::from_millis(5);
    loop {
        match attempt() {
            Err(error) if imp::is_held(&error) && started.elapsed() < patience => {
                std::thread::sleep(pause.min(patience.saturating_sub(started.elapsed())));
                pause = (pause * 2).min(Duration::from_millis(100));
            }
            done => return done,
        }
    }
}

// One canonical path form -------------------------------------------------------

/// `path` made canonical, in the form people and programs take:
/// [`std::fs::canonicalize`], then [`plain`].
pub fn canonical(path: &Path) -> io::Result<PathBuf> {
    path.canonicalize().map(plain)
}

/// A path in the form programs take, without Windows' verbatim prefix
/// where the plain form means the same: `\\?\C:\…` becomes `C:\…`, and
/// `\\?\UNC\server\share\…` becomes `\\server\share\…`. Windows writes a
/// canonical path verbatim, a form `cmd.exe`, git and many tools an agent
/// runs refuse or print back oddly.
///
/// A path stays verbatim when its plain form would name something else
/// or nothing: one of `MAX_PATH` or more, one with a component Windows
/// would rewrite (a trailing dot or space, `.` or `..`) or read as a
/// device (`con`, `nul`, `com1`…), and a verbatim form with no plain one
/// (`\\?\Volume{…}`). Elsewhere, and for a path without the prefix,
/// `path` is returned as it is.
pub fn plain(path: PathBuf) -> PathBuf {
    if !imp::VERBATIM_PATHS {
        return path;
    }
    match path.to_str().and_then(verbatim::plain) {
        Some(plain) => PathBuf::from(plain),
        None => path,
    }
}

/// `path` as a library that calls the OS itself, not through std, must be
/// handed it to reach a deep path, SQLite among them. Windows: absolute,
/// and verbatim (`\\?\…`) when it is too long for a plain path, the rule
/// std applies to every path it hands Windows; without it a process that
/// is not long-path aware opens nothing past `MAX_PATH`. Elsewhere `path`
/// as it is. Only for handing over: what is stored or shown stays in the
/// plain form ([`canonical`]).
pub fn long_form(path: &Path) -> PathBuf {
    imp::long_form(path)
}

/// The existing `path` in its shortest form, for a command line that
/// must fit a limit. Windows: absolute, each folder and file by its 8.3
/// short name (`GetShortPathNameW`, `C:\Users\SOMEON~1\…`), which names
/// the same file; a name the volume keeps no short name for (short names
/// turned off on it, or ReFS) stays long. Missing is an error there.
/// Elsewhere `path` as it is, which has no short form.
pub fn short_path(path: &Path) -> io::Result<PathBuf> {
    imp::short_path(path)
}

/// Whether `name`, as one file or folder name, is one Windows cannot give
/// a file: a device's (`CON`, `PRN`, `AUX`, `NUL`, `COM0`–`COM9`,
/// `LPT0`–`LPT9`, in any case, alone or before an extension), or one
/// ending in a dot or a space, which Windows drops. Pure text, the same
/// answer on every OS, so a name one host takes works on every other.
pub fn reserved_name(name: &str) -> bool {
    verbatim::reserved(name) || name.ends_with('.') || name.ends_with(' ')
}

// Identity and times -------------------------------------------------------------

/// A file's identity: its device and inode on Unix, its volume's serial
/// number and file index on Windows. Two paths with one identity are hard
/// links to one file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileId {
    device: u64,
    file: u64,
}

impl FileId {
    /// The identity of the file at `path`, following links.
    pub fn of(path: &Path) -> io::Result<FileId> {
        let (device, file) = imp::file_id(path)?;
        Ok(FileId { device, file })
    }
}

/// Sets when the file or folder at `path` was last modified. Windows opens
/// a folder only with backup semantics, and sets its times only through a
/// handle that may write them; this opens either as it must.
pub fn set_modified(path: &Path, at: std::time::SystemTime) -> io::Result<()> {
    imp::open_for_times(path)?.set_modified(at)
}

// Executables ------------------------------------------------------------------

/// Lets `path` be run. Unix: an execute bit wherever there is a read bit.
/// Windows: nothing, and `Ok`: Windows runs a file by its extension, and
/// has no bit to set.
pub fn make_executable(path: &Path) -> io::Result<()> {
    imp::make_executable(path)
}

/// Signs the program at `path` again, ad hoc, as a binary whose bytes were
/// changed after linking needs before it runs. macOS: `codesign --force
/// --sign -`, which only a Mach-O passes. Elsewhere nothing, and `Ok`: no
/// other OS asks a program for a signature to run it (documented
/// exception).
pub fn sign_ad_hoc(path: &Path) -> io::Result<()> {
    imp::sign_ad_hoc(path)
}

/// Takes Gatekeeper's quarantine off the file at `path` (not following a
/// link), should the process that wrote it be one that marks what it
/// downloads: a program run with nobody to answer Gatekeeper's question
/// must not carry it. `Ok` when the file has none. Elsewhere nothing, and
/// `Ok`: Windows keeps no quarantine of what a process writes, and Linux
/// has no Gatekeeper (documented exception).
pub fn clear_quarantine(path: &Path) -> io::Result<()> {
    imp::clear_quarantine(path)
}

/// Whether the file at `path`, whose metadata is `meta`, can be run as a
/// program. Unix: a regular file with an execute bit. Windows: a regular
/// file whose extension `PATHEXT` lists (`.exe`, `.cmd`, `.bat`…).
pub fn is_executable(path: &Path, meta: &Metadata) -> bool {
    imp::is_executable(path, meta)
}

/// Whether the file whose metadata is `meta` has an execute bit, as git
/// records a file's mode: any of the three on Unix. Windows: `false`,
/// as its files have no such bit (documented exception).
pub fn has_execute_bit(meta: &Metadata) -> bool {
    imp::has_execute_bit(meta)
}

// The root's volume ----------------------------------------------------------------

/// Whether the volume holding `path` (or, for a path not yet created, its
/// nearest existing ancestor) can hold an amux root.
///
/// Windows: a local fixed drive, formatted NTFS or ReFS, that supports
/// POSIX unlink and rename (`FILE_SUPPORTS_POSIX_UNLINK_RENAME`), which
/// durable writes and removal of open files rely on. Unix: always
/// [`VolumeCheck::Suitable`] (documented exception), as every volume amux
/// meets there has POSIX semantics.
pub fn volume_supports_root(path: &Path) -> io::Result<VolumeCheck> {
    imp::volume_supports_root(path)
}

/// What [`volume_supports_root`] found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VolumeCheck {
    Suitable,
    /// Each thing the volume lacks; never empty.
    Unsuitable(Vec<VolumeProblem>),
}

/// One thing a volume lacks for a root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VolumeProblem {
    /// A network share, removable or optical drive, or RAM disk: `kind`
    /// names which.
    NotLocal { kind: &'static str },
    /// Formatted with a file system other than NTFS or ReFS.
    FileSystem { name: String },
    /// No POSIX unlink and rename.
    NoPosixRename,
}

impl fmt::Display for VolumeProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotLocal { kind } => write!(f, "it is on a {kind}, not a local fixed drive"),
            Self::FileSystem { name } => write!(f, "its file system is {name}, not NTFS or ReFS"),
            Self::NoPosixRename => f.write_str("its file system has no POSIX unlink and rename"),
        }
    }
}

impl fmt::Display for VolumeCheck {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Suitable => f.write_str("suitable for a root"),
            Self::Unsuitable(problems) => {
                for (n, problem) in problems.iter().enumerate() {
                    if n > 0 {
                        f.write_str("; ")?;
                    }
                    write!(f, "{problem}")?;
                }
                Ok(())
            }
        }
    }
}
