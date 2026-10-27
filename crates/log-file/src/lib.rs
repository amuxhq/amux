//! The log file every amux process writes: appended to, and kept under a
//! cap by moving its contents to `<name>.1` (replacing the previous one)
//! and starting again from empty.
//!
//! The move is a copy and a truncation in place, never a rename, so every
//! other handle on the file keeps writing to the live one: the process's
//! own standard error, which a launcher opened onto the same file, and a
//! child's, such as a provider's, which outlives no rotation's reopening
//! because there is none. Every such handle must append, which each
//! launcher in amux does; [`LogFile::claim_stderr`] makes the process's
//! own one so. A line another handle writes between the copy and the
//! truncation is lost; nothing else is.
//!
//! A dump reads [`tail`]: the newest bytes of `.1` and the live file
//! together, from a line start, never more than it asks for.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// A daemon's, a supervisor's or a phone's log.
pub const PROCESS_CAP: u64 = 8 << 20;
/// Each of an agent's logs: its own, and its provider's.
pub const AGENT_CAP: u64 = 1 << 20;

/// An open log file, rotated as its writes pass its cap.
#[derive(Debug)]
pub struct LogFile {
    path: PathBuf,
    cap: u64,
    file: File,
}

impl LogFile {
    /// Opens `path` for appending, creating it and its directory, owner-only.
    pub fn open(path: &Path, cap: u64) -> io::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Ok(Self {
            path: path.to_owned(),
            cap,
            file: append(path)?,
        })
    }

    /// Points the process's standard error at this file, appending, so a
    /// panic or a stray message lands in the live file and follows its
    /// rotations. Windows keeps the standard error it was given.
    pub fn claim_stderr(&self) -> io::Result<()> {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            // SAFETY: both descriptors are open for the call's length, and
            // dup2 only replaces descriptor 2 with a copy of ours.
            if unsafe { libc::dup2(self.file.as_raw_fd(), libc::STDERR_FILENO) } < 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    }
}

impl Write for LogFile {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        // The length is the file's, so what other handles wrote counts.
        let len = self.file.metadata()?.len();
        if len > 0 && len + bytes.len() as u64 > self.cap {
            rotate(&self.path, &self.file)?;
        }
        self.file.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

/// Rotates `path` if it has passed `cap`: for a log other processes
/// write, which its owner trims now and then rather than on every write.
pub fn trim(path: &Path, cap: u64) -> io::Result<()> {
    let file = match OpenOptions::new().write(true).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if file.metadata()?.len() > cap {
        rotate(path, &file)?;
    }
    Ok(())
}

/// The previous file of the log at `path`.
pub fn previous(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".1");
    PathBuf::from(name)
}

/// The newest `max` bytes of the log at `path`, its previous file's first,
/// starting at a line, and how many bytes before them were left out; `None`
/// when there is no log.
pub fn tail(path: &Path, max: u64) -> io::Result<Option<(Vec<u8>, u64)>> {
    let older = previous(path);
    let (mut bytes, mut skipped) = match read_tail(path, max)? {
        Some((bytes, skipped)) if skipped > 0 => {
            // The live file alone fills the tail: all of the previous one
            // is left out too.
            let older = std::fs::metadata(&older).map_or(0, |meta| meta.len());
            (bytes, skipped + older)
        }
        current => {
            let (mut bytes, _) = current.clone().unwrap_or_default();
            let room = max - bytes.len() as u64;
            match read_tail(&older, room)? {
                Some((mut older, skipped)) => {
                    older.append(&mut bytes);
                    (older, skipped)
                }
                None if current.is_none() => return Ok(None),
                None => (bytes, 0),
            }
        }
    };
    if skipped > 0
        && let Some(newline) = bytes.iter().position(|byte| *byte == b'\n')
    {
        skipped += newline as u64 + 1;
        bytes.drain(..=newline);
    }
    Ok(Some((bytes, skipped)))
}

/// The newest `max` bytes of one file, and how many came before them.
fn read_tail(path: &Path, max: u64) -> io::Result<Option<(Vec<u8>, u64)>> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let len = file.metadata()?.len();
    let mut skipped = len.saturating_sub(max);
    file.seek(SeekFrom::Start(skipped))?;
    let mut bytes = Vec::with_capacity((len - skipped) as usize);
    file.read_to_end(&mut bytes)?;
    // The file may have grown since its length was read: the newest `max`
    // bytes are still all there is room for.
    let grown = (bytes.len() as u64).saturating_sub(max);
    bytes.drain(..grown as usize);
    skipped += grown;
    Ok(Some((bytes, skipped)))
}

fn append(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

/// Copies the log to its previous file and empties it in place.
fn rotate(path: &Path, file: &File) -> io::Result<()> {
    std::fs::copy(path, previous(path))?;
    file.set_len(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap_or_default()
    }

    #[test]
    fn a_write_past_the_cap_moves_the_log_to_its_previous_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.log");
        let mut log = LogFile::open(&path, 10).unwrap();
        log.write_all(b"one\n").unwrap();
        log.write_all(b"two\n").unwrap();
        log.write_all(b"three\n").unwrap();
        assert_eq!(read(&previous(&path)), "one\ntwo\n");
        assert_eq!(read(&path), "three\n");
        log.write_all(b"four\n").unwrap();
        log.write_all(b"five\n").unwrap();
        assert_eq!(read(&previous(&path)), "three\n");
        assert_eq!(read(&path), "four\nfive\n");
    }

    #[test]
    fn another_appending_handle_keeps_writing_to_the_live_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("provider.log");
        let mut other = append(&path).unwrap();
        other.write_all(b"0123456789ab\n").unwrap();
        trim(&path, 10).unwrap();
        other.write_all(b"after\n").unwrap();
        assert_eq!(read(&previous(&path)), "0123456789ab\n");
        assert_eq!(read(&path), "after\n");
        // Under the cap, nothing moves.
        trim(&path, 10).unwrap();
        assert_eq!(read(&path), "after\n");
        trim(&dir.path().join("missing.log"), 10).unwrap();
    }

    #[test]
    fn the_tail_reads_across_the_previous_file_from_a_line_start() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.log");
        assert_eq!(tail(&path, 100).unwrap(), None);
        std::fs::write(previous(&path), "old one\nold two\n").unwrap();
        std::fs::write(&path, "new\n").unwrap();
        assert_eq!(
            tail(&path, 100).unwrap(),
            Some((b"old one\nold two\nnew\n".to_vec(), 0))
        );
        // Cut inside "old one": that line goes whole.
        assert_eq!(
            tail(&path, 14).unwrap(),
            Some((b"old two\nnew\n".to_vec(), 8))
        );
        // Cut inside the live file: the previous file is not read.
        std::fs::write(&path, "a long line\nshort\n").unwrap();
        assert_eq!(tail(&path, 8).unwrap(), Some((b"short\n".to_vec(), 28)));
    }
}
