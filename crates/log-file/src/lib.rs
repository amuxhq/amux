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
//! A dump reads [`window`]: the lines of `.1` and the live file together
//! written in the minute before it, at the level every process logs at by
//! default, never more than it asks for. [`tail`] is the newest bytes
//! whatever their time.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// A daemon's, a supervisor's or a phone's log.
pub const PROCESS_CAP: u64 = 8 << 20;
/// Each of an agent's logs: its own, and its provider's.
pub const AGENT_CAP: u64 = 1 << 20;

/// How far back a dump's logs reach: the minute before what it reports,
/// which is what explains a failure, not the hours a capped log holds.
pub const DUMP_WINDOW_MS: i64 = 60_000;

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
            rotate(&self.path)?;
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
    let len = match std::fs::metadata(path) {
        Ok(meta) => meta.len(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if len > cap {
        rotate(path)?;
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

/// The lines of the log at `path` and its previous file stamped at or after
/// `since_ms` (Unix milliseconds) at INFO or above, out of its newest `max`
/// bytes; `None` when there is no log. A line without a stamp, such as a
/// panic's, goes with the stamped line before it, and one before any
/// stamped line in the tail is left out: its time is not known.
pub fn window(path: &Path, since_ms: i64, max: u64) -> io::Result<Option<Vec<u8>>> {
    let Some((bytes, _)) = tail(path, max)? else {
        return Ok(None);
    };
    let mut kept = Vec::new();
    let mut keeping = false;
    for line in bytes.split_inclusive(|byte| *byte == b'\n') {
        if let Some((at_ms, level)) = stamp(line) {
            keeping = at_ms >= since_ms && matches!(level, "INFO" | "WARN" | "ERROR");
        }
        if keeping {
            kept.extend_from_slice(line);
        }
    }
    Ok(Some(kept))
}

/// Now on the wall clock the logs are stamped with, in Unix milliseconds.
pub fn unix_ms_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_millis()).unwrap_or(i64::MAX)
        })
}

/// A line's time in Unix milliseconds and its level, as the processes'
/// formatter writes them: `2026-11-01T10:00:00.123456Z  INFO target: …`.
fn stamp(line: &[u8]) -> Option<(i64, &str)> {
    let line = std::str::from_utf8(line.get(..line.len().min(64))?).ok()?;
    let (time, rest) = line.split_once(' ')?;
    let level = rest.trim_start().split(' ').next()?;
    let time = time.strip_suffix('Z')?;
    let (date, clock) = time.split_once('T')?;
    let mut date = date.splitn(3, '-').map(str::parse::<i64>);
    let (year, month, day) = (date.next()?.ok()?, date.next()?.ok()?, date.next()?.ok()?);
    let (clock, fraction) = clock.split_once('.').unwrap_or((clock, ""));
    let mut clock = clock.splitn(3, ':').map(str::parse::<i64>);
    let (hour, minute, second) = (
        clock.next()?.ok()?,
        clock.next()?.ok()?,
        clock.next()?.ok()?,
    );
    let millis = match fraction.get(..3) {
        Some(digits) => digits.parse::<i64>().ok()?,
        None if fraction.is_empty() => 0,
        None => return None,
    };
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    // Days since 1970-01-01 of a proleptic Gregorian date.
    let (y, m) = if month <= 2 {
        (year - 1, month + 9)
    } else {
        (year, month - 3)
    };
    let era = y.div_euclid(400);
    let of_era = y - era * 400;
    let of_year = (153 * m + 2) / 5 + day - 1;
    let of_cycle = of_era * 365 + of_era / 4 - of_era / 100 + of_year;
    let days = era * 146_097 + of_cycle - 719_468;
    let seconds = days * 86_400 + hour * 3_600 + minute * 60 + second;
    Some((seconds * 1_000 + millis, level))
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

/// Copies the log to its previous file and empties it in place. The
/// truncation goes through a handle of its own, because Windows lets an
/// append-only handle add to a file but not shorten it.
fn rotate(path: &Path) -> io::Result<()> {
    std::fs::copy(path, previous(path))?;
    OpenOptions::new().write(true).open(path)?.set_len(0)
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
    fn a_stamp_reads_as_unix_milliseconds_and_its_level() {
        assert_eq!(
            stamp(b"2026-11-01T10:00:00.123456Z  INFO node: started\n"),
            Some((1_793_527_200_123, "INFO"))
        );
        assert_eq!(
            stamp(b"1970-01-01T00:00:01Z DEBUG x\n"),
            Some((1_000, "DEBUG"))
        );
        assert_eq!(stamp(b"thread 'main' panicked at src/lib.rs\n"), None);
    }

    #[test]
    fn the_window_keeps_the_minute_before_at_info_and_above() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.log");
        assert_eq!(window(&path, 0, 1 << 20).unwrap(), None);
        std::fs::write(
            previous(&path),
            "loose line of unknown time\n\
             1970-01-01T00:00:01.000Z  INFO x: too old\n",
        )
        .unwrap();
        std::fs::write(
            &path,
            "1970-01-01T00:01:00.000Z  INFO x: in time\n\
             a panic's second line\n\
             1970-01-01T00:01:01.000Z DEBUG x: below the default level\n\
             its continuation\n\
             1970-01-01T00:01:02.000Z  WARN x: kept\n",
        )
        .unwrap();
        let kept = window(&path, 60_000, 1 << 20).unwrap().unwrap();
        assert_eq!(
            String::from_utf8(kept).unwrap(),
            "1970-01-01T00:01:00.000Z  INFO x: in time\n\
             a panic's second line\n\
             1970-01-01T00:01:02.000Z  WARN x: kept\n"
        );
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
