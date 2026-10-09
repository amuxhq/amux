//! The facts ring: every event the interpreter was fed, in order, in
//! private/facts/, so the interpreter's state can be rebuilt and a dump can
//! show what the provider actually said.
//!
//! The ring is segments named by the global offset of their first byte, as
//! the journal is. Each segment starts with a checkpoint beside it,
//! `<offset>.checkpoint`: the interpreter's whole state before the
//! segment's first entry. A segment rotates once it reaches its size; the
//! newest two are kept, so a dump is the last two segments plus the
//! checkpoint at each one's start. The state at any moment is the newest
//! checkpoint with its segment's entries fed through the interpreter again.
//!
//! An entry is one JSON line. Provider payloads are text when they are
//! UTF-8, which every provider's are, and hex otherwise; inputs are their
//! encoded protobuf in hex.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

pub use interpret::ring::Entry;

/// Segments kept.
const KEEP: usize = 2;
const CHECKPOINT: &str = "checkpoint";

/// The writer of one agent's ring.
pub struct Ring {
    dir: PathBuf,
    segment_bytes: u64,
    /// The global offset of the next byte.
    offset: u64,
    /// The open segment's start and file.
    segment: (u64, File),
}

/// What a ring holds for resuming: the newest checkpoint and the entries
/// written after it.
pub struct Saved {
    pub checkpoint: Vec<u8>,
    pub entries: Vec<Entry>,
}

impl Ring {
    /// Opens the ring in `dir`, starting a new segment whose checkpoint is
    /// `checkpoint`: every incarnation starts its own segment from the state
    /// it resumed.
    pub fn open(dir: PathBuf, ring_bytes: u64, checkpoint: &[u8]) -> io::Result<Self> {
        fs::create_dir_all(&dir)?;
        let offset = match journal::segments(&dir)?.last() {
            Some(&start) => start + fs::metadata(journal::segment_path(&dir, start))?.len(),
            None => 0,
        };
        let segment = start_segment(&dir, offset, checkpoint)?;
        let ring = Self {
            dir,
            segment_bytes: (ring_bytes / KEEP as u64).max(1),
            offset,
            segment: (offset, segment),
        };
        ring.prune()?;
        Ok(ring)
    }

    /// Whether the open segment is full, so the next entry starts a new one
    /// after [`Ring::rotate`].
    pub fn full(&self) -> bool {
        self.offset - self.segment.0 >= self.segment_bytes
    }

    /// Starts a new segment from `checkpoint`, the state now.
    pub fn rotate(&mut self, checkpoint: &[u8]) -> io::Result<()> {
        let file = start_segment(&self.dir, self.offset, checkpoint)?;
        self.segment = (self.offset, file);
        self.prune()
    }

    /// The global offset of the next entry.
    pub fn offset(&self) -> u64 {
        self.offset
    }

    /// Appends one entry. A failed write leaves the ring as it was, since a
    /// torn line would hide every entry written after it from a resume.
    pub fn append(&mut self, entry: &Entry) -> io::Result<()> {
        let mut line = serde_json::to_vec(entry).map_err(io::Error::other)?;
        line.push(b'\n');
        if let Err(error) = self.segment.1.write_all(&line) {
            self.undo(self.offset);
            return Err(error);
        }
        self.offset += line.len() as u64;
        Ok(())
    }

    /// Takes the open segment back to `offset`, forgetting the entries
    /// after it: their steps never reached the journal, so a resume must
    /// not count them as written. Best effort; a rotation since `offset`
    /// keeps what it wrote.
    pub fn undo(&mut self, offset: u64) {
        let start = self.segment.0;
        if offset >= start
            && journal::truncate(&journal::segment_path(&self.dir, start), offset - start).is_ok()
        {
            self.offset = offset;
        }
    }

    fn prune(&self) -> io::Result<()> {
        let starts = journal::segments(&self.dir)?;
        let excess = starts.len().saturating_sub(KEEP);
        for start in &starts[..excess] {
            fs::remove_file(journal::segment_path(&self.dir, *start))?;
            match fs::remove_file(checkpoint_path(&self.dir, *start)) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
}

/// The newest checkpoint in `dir` and the entries after it; None when the
/// ring holds none. A torn last line, from a process that died mid-write, is
/// left out.
pub fn saved(dir: &Path) -> io::Result<Option<Saved>> {
    for start in journal::segments(dir)?.into_iter().rev() {
        let checkpoint = match fs::read(checkpoint_path(dir, start)) {
            Ok(checkpoint) => checkpoint,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        let bytes = fs::read(journal::segment_path(dir, start))?;
        let entries = interpret::ring::entries(&bytes);
        return Ok(Some(Saved {
            checkpoint,
            entries,
        }));
    }
    Ok(None)
}

pub fn checkpoint_path(dir: &Path, start: u64) -> PathBuf {
    dir.join(format!("{}.{CHECKPOINT}", journal::segment_name(start)))
}

/// Writes the checkpoint by temp-and-rename, then creates the segment: a
/// segment never exists without the state it starts from.
fn start_segment(dir: &Path, start: u64, checkpoint: &[u8]) -> io::Result<File> {
    let path = checkpoint_path(dir, start);
    let temp = path.with_extension("tmp");
    fs::write(&temp, checkpoint)?;
    fs::rename(&temp, &path)?;
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(journal::segment_path(dir, start))
}

#[cfg(test)]
mod tests {
    use interpret::{Channel, Event, Fact};
    use wire::StopMode;

    use super::*;

    fn tick(at_ms: i64) -> Entry {
        Entry::Tick { at_ms }
    }

    #[test]
    fn the_ring_rotates_by_size_keeps_two_segments_and_resumes_from_the_newest() {
        let dir = tempfile::tempdir().unwrap();
        let facts = dir.path().join("facts");
        let mut ring = Ring::open(facts.clone(), 60, b"state 0").unwrap();
        for at in 0..10 {
            if ring.full() {
                ring.rotate(format!("state {at}").as_bytes()).unwrap();
            }
            ring.append(&tick(at)).unwrap();
        }
        let segments = journal::segments(&facts).unwrap();
        assert_eq!(segments.len(), 2);
        for start in &segments {
            assert!(checkpoint_path(&facts, *start).exists());
        }
        let saved = super::saved(&facts).unwrap().unwrap();
        let first = match saved.entries.first() {
            Some(Entry::Tick { at_ms }) => *at_ms,
            other => panic!("a tick, not {other:?}"),
        };
        assert_eq!(saved.checkpoint, format!("state {first}").into_bytes());
        assert_eq!(saved.entries.last(), Some(&tick(9)));

        // A new incarnation starts its own segment after the last.
        drop(ring);
        let ring = Ring::open(facts.clone(), 60, b"resumed").unwrap();
        assert!(!ring.full());
        let saved = super::saved(&facts).unwrap().unwrap();
        assert_eq!(saved.checkpoint, b"resumed");
        assert!(saved.entries.is_empty());
    }

    #[test]
    fn entries_round_trip_every_event() {
        let events = [
            Event::Fact(Fact {
                channel: Channel::Hook,
                payload: br#"{"a":1}"#.to_vec(),
            }),
            Event::Fact(Fact {
                channel: Channel::Stream,
                payload: vec![0xff, 0x00],
            }),
            Event::Input(wire::Input {
                input_id: b"i1".to_vec(),
                ..Default::default()
            }),
            Event::Tick { at_ms: 5 },
            Event::ProviderExit { code: Some(3) },
            Event::DaemonLost,
            Event::StopRequested(StopMode::Abort),
            Event::Exiting(wire::Exiting {
                cause: wire::ExitCause::Stopped as i32,
                detail: None,
            }),
            Event::Exiting(wire::Exiting {
                cause: wire::ExitCause::ProviderExited as i32,
                detail: Some("exit code 3".into()),
            }),
            // A cause a newer build wrote is kept as its value.
            Event::Exiting(wire::Exiting {
                cause: 99,
                detail: None,
            }),
        ];
        for event in events {
            let line = serde_json::to_vec(&Entry::of(&event)).unwrap();
            let entry: Entry = serde_json::from_slice(&line).unwrap();
            assert_eq!(entry.event(), Some(event));
        }
    }
}
