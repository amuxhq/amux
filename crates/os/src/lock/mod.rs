//! A lock held for a process's life, over a file whose contents stay
//! readable while it is held.

use std::fs::{File, OpenOptions};
use std::io::{self, Seek as _, SeekFrom, Write as _};
use std::path::Path;

#[cfg(unix)]
mod unix;
#[cfg(unix)]
use unix as imp;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as imp;

#[cfg(test)]
mod tests;

/// An exclusive lock on a file, held until this is dropped or the process
/// dies, when the kernel releases it.
///
/// Unix: `flock` over the whole file, through std's [`File::try_lock`].
///
/// Windows: `LockFileEx` on the single byte at offset `u64::MAX - 1`. A
/// Windows lock is mandatory over the bytes it covers, so one past the
/// data leaves the file's contents readable, and writable through
/// [`FileLock::write_data`], while it is held: the supervisor's lock and
/// the agent's carry their pids. Every amux lock file is one of these, and
/// is probed with [`FileLock::held`]; a std lock ([`File::try_lock`], the
/// whole file) would make the data unreadable.
#[derive(Debug)]
pub struct FileLock {
    file: File,
}

impl FileLock {
    /// Takes the lock on `path`, creating the file if needed and keeping
    /// what it holds; `None` when another process (or another handle in
    /// this one) holds it.
    pub fn try_acquire(path: &Path) -> io::Result<Option<FileLock>> {
        let mut options = crate::fs::private_open_options();
        options.read(true).create(true).truncate(false);
        let file = options.open(path)?;
        Ok(imp::try_lock(&file)?.then_some(FileLock { file }))
    }

    /// Takes the lock through `file`, already open on a lock file: one a
    /// predecessor handed this process ([`crate::process::file_from_inherited`]).
    /// On Unix a lock goes with its descriptor across an exec, so a file the
    /// image this one replaced held is held already, and taking it again
    /// through the same descriptor succeeds; `None` when another process
    /// holds it.
    pub fn try_from_file(file: File) -> io::Result<Option<FileLock>> {
        Ok(imp::try_lock(&file)?.then_some(FileLock { file }))
    }

    /// Whether some process holds the lock on `path`: no file, no lock.
    /// Finding out takes the lock and releases it at once, so a process
    /// taking it at that instant can fail to; callers probe only files
    /// whose holder they are not starting.
    ///
    /// The probe opens the file for writing, as a holder does: on an NFS
    /// mount that implements `flock` with byte-range locks, an exclusive
    /// lock needs a descriptor open for writing, and one open only for
    /// reading fails to probe. A file this user may only read is probed
    /// read-only, which every local file system honours. A probe that
    /// fails is an error, never "free".
    pub fn held(path: &Path) -> io::Result<bool> {
        let opened = match OpenOptions::new().read(true).write(true).open(path) {
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
                OpenOptions::new().read(true).open(path)
            }
            opened => opened,
        };
        let file = match opened {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error),
        };
        if imp::try_lock(&file)? {
            imp::unlock(&file);
            Ok(false)
        } else {
            Ok(true)
        }
    }

    /// Replaces the file's contents with `bytes`, on disk before this
    /// returns, keeping the lock.
    pub fn write_data(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.file.set_len(0)?;
        self.file.seek(SeekFrom::Start(0))?;
        self.file.write_all(bytes)?;
        self.file.sync_all()
    }

    /// The locked file, for a holder that hands it to a successor.
    pub fn file(&self) -> &File {
        &self.file
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        // Unlock explicitly: on Unix a descriptor a concurrently forked
        // child inherited would otherwise keep the lock until it execs.
        imp::unlock(&self.file);
    }
}
