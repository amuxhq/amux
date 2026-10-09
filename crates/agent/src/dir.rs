//! The agent directory: its lock, its spec files, its raw terminal log and
//! its blobs. The directory is the whole contract with the daemon.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

pub use agent_dir::{BLOBS, CTL_SOCK, JOURNAL, LOCK, PRIVATE, PTY, PTY_SOCK, lock};
use prost::Message as _;
use wire::AgentSpec;
pub const HOOKS_SOCK: &str = "hooks.sock";
/// The socket the agent's Codex app server listens on, under private/.
/// Windows hosts Codex over stdio.
#[cfg_attr(not(unix), allow(dead_code))]
pub const CODEX_SOCK: &str = "codex.sock";
/// Terminal Claude's messaging socket, under private/, which Claude binds.
pub const MESSAGING_SOCK: &str = "messaging.sock";
/// How far the agent has read Claude's transcript, and which one.
pub const TRANSCRIPT_CURSOR: &str = "transcript-cursor";
/// The facts ring and its checkpoints, under private/.
pub const FACTS: &str = "facts";
/// The provider's own session id, kept so a resume continues it.
pub const PROVIDER_SESSION: &str = "provider-session";
/// What the provider wrote to stderr.
pub const PROVIDER_LOG: &str = "provider.log";

/// The newest `spec.<n>` and its number.
pub fn newest_spec(dir: &Path) -> io::Result<Option<(u32, AgentSpec)>> {
    let mut newest = None;
    for entry in fs::read_dir(dir)? {
        let name = entry?.file_name();
        let Some(n) = name
            .to_str()
            .and_then(|name| name.strip_prefix("spec."))
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        if newest.is_none_or(|newest| n > newest) {
            newest = Some(n);
        }
    }
    let Some(n) = newest else { return Ok(None) };
    let bytes = fs::read(dir.join(format!("spec.{n}")))?;
    let spec = AgentSpec::decode(bytes.as_slice())
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    Ok(Some((n, spec)))
}

/// Creates a directory only this user can enter.
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

/// Writes `blobs/<hex>` by temp-and-rename; the name is the content's hash,
/// so a second writer of the same bytes is harmless.
pub fn write_blob(dir: &Path, hash: &[u8], bytes: &[u8]) -> io::Result<()> {
    let folder = dir.join(BLOBS);
    fs::create_dir_all(&folder)?;
    let name = interpret::to_hex(hash);
    let path = folder.join(&name);
    if path.exists() {
        return Ok(());
    }
    let temp = folder.join(format!(".{name}.{}", std::process::id()));
    fs::write(&temp, bytes)?;
    fs::rename(&temp, &path)
}

/// Raw terminal bytes in segments named by the global offset of their first
/// byte, like the journal; the oldest beyond `keep` are removed. Terminal
/// clients read them by position.
pub struct PtyLog {
    dir: PathBuf,
    segment_size: u64,
    keep: usize,
    offset: u64,
    segment: Option<(u64, File)>,
}

impl PtyLog {
    pub fn open(dir: PathBuf, segment_size: u64, keep: usize) -> io::Result<Self> {
        fs::create_dir_all(&dir)?;
        let segment = match journal::segments(&dir)?.last() {
            Some(&start) => {
                let file = OpenOptions::new()
                    .append(true)
                    .open(journal::segment_path(&dir, start))?;
                let len = file.metadata()?.len();
                Some((start, file, len))
            }
            None => None,
        };
        let offset = segment.as_ref().map_or(0, |(start, _, len)| start + len);
        Ok(Self {
            dir,
            segment_size: segment_size.max(1),
            keep: keep.max(1),
            offset,
            segment: segment.map(|(start, file, _)| (start, file)),
        })
    }

    /// Where the log ends: the global offset of the next byte.
    pub fn offset(&self) -> u64 {
        self.offset
    }

    pub fn append(&mut self, mut bytes: &[u8]) -> io::Result<()> {
        while !bytes.is_empty() {
            let full = self
                .segment
                .as_ref()
                .is_none_or(|(start, _)| self.offset - start >= self.segment_size);
            if full {
                self.segment = None;
                let file = OpenOptions::new()
                    .append(true)
                    .create_new(true)
                    .open(journal::segment_path(&self.dir, self.offset))?;
                self.segment = Some((self.offset, file));
                self.prune()?;
            }
            let (start, file) = self.segment.as_mut().expect("a segment is open");
            let room = (*start + self.segment_size - self.offset) as usize;
            let take = room.min(bytes.len());
            file.write_all(&bytes[..take])?;
            self.offset += take as u64;
            bytes = &bytes[take..];
        }
        Ok(())
    }

    fn prune(&self) -> io::Result<()> {
        let starts = journal::segments(&self.dir)?;
        let excess = starts.len().saturating_sub(self.keep);
        for start in &starts[..excess] {
            fs::remove_file(journal::segment_path(&self.dir, *start))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_terminal_log_rotates_by_size_and_keeps_the_newest_segments() {
        let dir = tempfile::tempdir().unwrap();
        let mut log = PtyLog::open(dir.path().join("pty"), 4, 2).unwrap();
        log.append(b"0123456789").unwrap();
        assert_eq!(
            journal::segments(&dir.path().join("pty")).unwrap(),
            vec![4, 8]
        );
        drop(log);
        let mut log = PtyLog::open(dir.path().join("pty"), 4, 2).unwrap();
        log.append(b"ab").unwrap();
        let pty = dir.path().join("pty");
        assert_eq!(journal::segments(&pty).unwrap(), vec![4, 8]);
        assert_eq!(fs::read(journal::segment_path(&pty, 8)).unwrap(), b"89ab");
    }

    #[test]
    fn a_second_locker_is_refused_while_the_first_holds_the_lock() {
        let dir = tempfile::tempdir().unwrap();
        let held = lock(dir.path()).unwrap().expect("the first lock");
        assert!(lock(dir.path()).unwrap().is_none());
        drop(held);
        assert!(lock(dir.path()).unwrap().is_some());
    }
}
