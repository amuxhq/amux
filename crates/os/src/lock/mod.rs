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
/// Unix: `flock` over the whole file, which is what std's
/// [`File::try_lock`] takes, so a build that locks with std and one that
/// locks with this exclude each other.
///
/// Windows: `LockFileEx` on the single byte at offset `u64::MAX - 1`
/// ([`LOCKED_BYTE`]). std's [`File::try_lock`] (Rust 1.98,
/// `library/std/src/sys/fs/windows.rs`) locks offset 0 for `u64::MAX`
/// bytes, the range `0 ..= u64::MAX - 1`, which holds that byte: so an
/// older build holding the file with std and a newer one probing with this
/// exclude each other, and the other way round. The byte lies past any
/// data, and a Windows lock is mandatory only over the bytes it covers, so
/// the file's contents stay readable, and writable through
/// [`FileLock::write_data`], while it is held: `amux status` reads the
/// supervisor's pid out of its lock.
#[derive(Debug)]
pub struct FileLock {
    file: File,
}

/// The byte a [`FileLock`] locks on Windows.
pub const LOCKED_BYTE: u64 = u64::MAX - 1;

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

    /// Whether some process holds the lock on `path`: no file, no lock.
    /// Finding out takes the lock and releases it at once, so a process
    /// taking it at that instant can fail to; callers probe only files
    /// whose holder they are not starting.
    pub fn held(path: &Path) -> io::Result<bool> {
        let file = match OpenOptions::new().read(true).open(path) {
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
