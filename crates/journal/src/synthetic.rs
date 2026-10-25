//! A synthetic journal writer for tests: it appends authored steps through
//! the real writer, and can leave a frame half-written as a live writer
//! does mid-write, or cut the journal at any byte as a writer that died or
//! lost its page cache would.

use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

use crate::{Step, Writer, encode_frame, segment_path, segments};

#[derive(Debug)]
pub struct SyntheticWriter {
    writer: Writer,
    segment_size: u64,
    /// The unwritten remainder of a frame started with `write_partial`.
    pending: Option<Vec<u8>>,
}

impl SyntheticWriter {
    pub fn open(dir: impl Into<PathBuf>, segment_size: u64) -> io::Result<Self> {
        Ok(Self {
            writer: Writer::open(dir, segment_size)?,
            segment_size,
            pending: None,
        })
    }

    pub fn dir(&self) -> &Path {
        self.writer.dir()
    }

    pub fn offset(&self) -> u64 {
        self.writer.offset()
    }

    /// Appends a whole step and returns the offset after it.
    pub fn append(&mut self, step: &Step) -> io::Result<u64> {
        assert!(self.pending.is_none(), "finish the partial frame first");
        self.writer.append(step)
    }

    /// Writes the first `keep` bytes of `step`'s frame, as a live writer
    /// that has not finished its write. Returns the frame's full length.
    pub fn write_partial(&mut self, step: &Step, keep: usize) -> io::Result<usize> {
        assert!(self.pending.is_none(), "one partial frame at a time");
        let frame = encode_frame(step);
        assert!(
            keep < frame.len(),
            "a partial frame keeps fewer bytes than the frame"
        );
        self.writer.write_frame_bytes(&frame[..keep])?;
        self.pending = Some(frame[keep..].to_vec());
        Ok(frame.len())
    }

    /// Writes the rest of the frame `write_partial` started.
    pub fn finish_partial(&mut self) -> io::Result<u64> {
        let rest = self
            .pending
            .take()
            .ok_or_else(|| io::Error::other("no partial frame"))?;
        self.writer.continue_frame(&rest)?;
        Ok(self.writer.offset())
    }

    /// Cuts the journal at global offset `byte`: the segment holding it is
    /// truncated there and every later segment is removed. This is a writer
    /// that died mid-frame, or a page cache lost with the machine. The
    /// writer is reopened, which is what a restarted agent process does.
    pub fn cut_at(&mut self, byte: u64) -> io::Result<()> {
        cut(self.writer.dir(), byte)?;
        self.pending = None;
        self.writer = Writer::open(self.writer.dir().to_path_buf(), self.segment_size)?;
        Ok(())
    }
}

/// Overwrites the journal in `dir` with zeros from global offset `byte` to
/// its end, its length unchanged: what a power cut can leave of writes the
/// file system had sized but not yet stored.
pub fn zero_from(dir: &Path, byte: u64) -> io::Result<()> {
    use std::io::{Seek, SeekFrom, Write};
    for start in segments(dir)? {
        let path = segment_path(dir, start);
        let len = fs::metadata(&path)?.len();
        if start + len <= byte {
            continue;
        }
        let from = byte.saturating_sub(start);
        let mut file = OpenOptions::new().write(true).open(path)?;
        file.seek(SeekFrom::Start(from))?;
        file.write_all(&vec![0; (len - from) as usize])?;
    }
    Ok(())
}

/// Truncates the journal in `dir` at global offset `byte` without
/// reopening any writer: the state a dead writer leaves behind.
pub fn cut(dir: &Path, byte: u64) -> io::Result<()> {
    for start in segments(dir)? {
        let path = segment_path(dir, start);
        if start > byte || (start == byte && start != 0) {
            fs::remove_file(path)?;
        } else {
            let len = fs::metadata(&path)?.len();
            if start + len > byte {
                OpenOptions::new()
                    .write(true)
                    .open(path)?
                    .set_len(byte - start)?;
            }
        }
    }
    Ok(())
}
