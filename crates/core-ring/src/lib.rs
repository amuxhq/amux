//! The bounded ring of a core's inputs since its last checkpoint, on disk,
//! so the core's state can be rebuilt and a dump can replay what it was fed.
//!
//! The ring is segments named by the global offset of their first byte, as
//! the journal is. Each segment starts with a checkpoint beside it,
//! `<offset>.checkpoint`: the core's whole state before the segment's first
//! input. A segment rotates once it reaches its size; the newest two are
//! kept, so a dump is the last two segments plus the checkpoint at each
//! one's start. The state at any moment is the newest checkpoint with its
//! segment's inputs fed through the core again. An input is one JSON line,
//! so recording one costs one bounded write.

mod limit;
mod step;

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write as _};
use std::marker::PhantomData;
use std::path::{Path, PathBuf};

pub use limit::{Admitted, DumpKind, DumpLimit, with_suppressed};
use serde::Serialize;
use serde::de::DeserializeOwned;
pub use step::{Check, DumpReason, StepBudget, Violation, finish_step};

/// Segments kept.
const KEEP: usize = 2;
const CHECKPOINT: &str = "checkpoint";

/// The writer of one ring of `T`s.
pub struct Ring<T> {
    dir: PathBuf,
    segment_bytes: u64,
    /// The global offset of the next byte.
    offset: u64,
    /// The open segment's start and file.
    segment: (u64, File),
    inputs: PhantomData<fn(&T)>,
}

/// What a ring holds for resuming: the newest checkpoint and the inputs
/// written after it.
pub struct Saved<T> {
    pub checkpoint: Vec<u8>,
    pub inputs: Vec<T>,
}

impl<T: Serialize + DeserializeOwned> Ring<T> {
    /// Opens the ring in `dir`, `ring_bytes` across the segments it keeps,
    /// starting a new segment whose checkpoint is `checkpoint`: every
    /// process starts its own segment from the state it resumed.
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
            inputs: PhantomData,
        };
        ring.prune()?;
        Ok(ring)
    }

    /// Whether the open segment is full, so the next input starts a new one
    /// after [`Ring::rotate`].
    pub fn full(&self) -> bool {
        self.offset - self.segment.0 >= self.segment_bytes
    }

    /// Starts a new segment at `checkpoint`, the state now. Segments older
    /// than the two newest are dropped.
    pub fn rotate(&mut self, checkpoint: &[u8]) -> io::Result<()> {
        let file = start_segment(&self.dir, self.offset, checkpoint)?;
        self.segment = (self.offset, file);
        self.prune()
    }

    /// The global offset of the next input.
    pub fn offset(&self) -> u64 {
        self.offset
    }

    /// Appends one input. A failed write leaves the ring as it was, since a
    /// torn line would hide every input written after it from a resume.
    pub fn append(&mut self, input: &T) -> io::Result<()> {
        self.append_as(input)
    }

    /// Appends one line that reads back as a `T`, for a writer that holds
    /// what it records borrowed. Fails as [`Ring::append`] does.
    pub fn append_as<U: Serialize + ?Sized>(&mut self, input: &U) -> io::Result<()> {
        let mut line = serde_json::to_vec(input).map_err(io::Error::other)?;
        line.push(b'\n');
        if let Err(error) = self.segment.1.write_all(&line) {
            self.undo(self.offset);
            return Err(error);
        }
        self.offset += line.len() as u64;
        Ok(())
    }

    /// Takes the open segment back to `offset`, forgetting the inputs after
    /// it: whatever they led to never took effect, so a resume must not
    /// count them. Best effort; a rotation since `offset` keeps what it
    /// wrote.
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

/// The newest checkpoint in `dir` and the inputs after it; None when the
/// ring holds none. A torn last line, from a process that died mid-write, is
/// cut.
pub fn saved<T: DeserializeOwned>(dir: &Path) -> io::Result<Option<Saved<T>>> {
    Ok(newest(dir)?.map(|(checkpoint, segment)| Saved {
        checkpoint,
        inputs: inputs(&segment),
    }))
}

/// The newest checkpoint in `dir` and its segment's bytes as written,
/// unread; None when the ring holds none.
pub fn newest(dir: &Path) -> io::Result<Option<(Vec<u8>, Vec<u8>)>> {
    for start in journal::segments(dir)?.into_iter().rev() {
        let checkpoint = match fs::read(checkpoint_path(dir, start)) {
            Ok(checkpoint) => checkpoint,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        let bytes = fs::read(journal::segment_path(dir, start))?;
        return Ok(Some((checkpoint, bytes)));
    }
    Ok(None)
}

/// The inputs of one segment's bytes, in order. A torn last line, from a
/// process that died mid-write, is cut, and so is everything after a line
/// this build cannot read.
pub fn inputs<T: DeserializeOwned>(segment: &[u8]) -> Vec<T> {
    segment
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map_while(|line| serde_json::from_slice(line).ok())
        .collect()
}

/// Where the checkpoint of the segment starting at `start` lives.
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
    use serde::Deserialize;

    use super::*;

    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    struct Tick {
        at_ms: i64,
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
            ring.append(&Tick { at_ms: at }).unwrap();
        }
        let segments = journal::segments(&facts).unwrap();
        assert_eq!(segments.len(), 2);
        for start in &segments {
            assert!(checkpoint_path(&facts, *start).exists());
        }
        let saved = super::saved::<Tick>(&facts).unwrap().unwrap();
        let first = saved.inputs.first().expect("a tick").at_ms;
        assert_eq!(saved.checkpoint, format!("state {first}").into_bytes());
        assert_eq!(saved.inputs.last(), Some(&Tick { at_ms: 9 }));

        // A new process starts its own segment after the last.
        drop(ring);
        let ring = Ring::<Tick>::open(facts.clone(), 60, b"resumed").unwrap();
        assert!(!ring.full());
        let saved = super::saved::<Tick>(&facts).unwrap().unwrap();
        assert_eq!(saved.checkpoint, b"resumed");
        assert!(saved.inputs.is_empty());
    }

    #[test]
    fn a_torn_tail_is_cut_and_never_returned() {
        let dir = tempfile::tempdir().unwrap();
        let facts = dir.path().join("facts");
        let mut ring = Ring::open(facts.clone(), 1 << 20, b"state").unwrap();
        ring.append(&Tick { at_ms: 1 }).unwrap();
        ring.append(&Tick { at_ms: 2 }).unwrap();
        drop(ring);
        let start = *journal::segments(&facts).unwrap().last().unwrap();
        let mut segment = OpenOptions::new()
            .append(true)
            .open(journal::segment_path(&facts, start))
            .unwrap();
        segment.write_all(br#"{"at_ms":"#).unwrap();

        let saved = super::saved::<Tick>(&facts).unwrap().unwrap();
        assert_eq!(saved.inputs, vec![Tick { at_ms: 1 }, Tick { at_ms: 2 }]);
    }

    #[test]
    fn an_undone_append_is_gone_from_the_saved_inputs() {
        let dir = tempfile::tempdir().unwrap();
        let facts = dir.path().join("facts");
        let mut ring = Ring::open(facts.clone(), 1 << 20, b"state").unwrap();
        ring.append(&Tick { at_ms: 1 }).unwrap();
        let mark = ring.offset();
        ring.append(&Tick { at_ms: 2 }).unwrap();
        ring.undo(mark);
        ring.append(&Tick { at_ms: 3 }).unwrap();

        let saved = super::saved::<Tick>(&facts).unwrap().unwrap();
        assert_eq!(saved.inputs, vec![Tick { at_ms: 1 }, Tick { at_ms: 3 }]);
    }
}
