//! The journal: an append-only queue of interpreter steps between an agent
//! process and its daemon, as files on disk.
//!
//! A journal is a directory of segment files, each named by the global byte
//! offset of its first byte. The writer closes a segment before it creates
//! the next, so a segment is final — never written again — exactly when a
//! later one exists. A reader's whole state is one integer, a global offset:
//! it opens the segment whose name is the largest at or below it.
//!
//! A frame is a varint length prefix and a protobuf [`Step`]. Frames never
//! span segments. The writer uses plain writes and no fsync; a process crash
//! loses nothing because the page cache survives it. A reader never returns
//! a partial frame: bytes at the end of a segment that do not make a whole
//! frame are reported as [`Torn`] and left for a later read, since a live
//! writer may still be completing them.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read as _, Seek, SeekFrom, Write as _};
use std::path::{Path, PathBuf};

use prost::Message as _;
pub use wire::Step;

#[cfg(feature = "synthetic")]
pub mod synthetic;

/// Frames larger than this are treated as corruption rather than awaited.
pub const MAX_FRAME_BYTES: u64 = 64 * 1024 * 1024;

/// Width of a segment name; wider offsets still parse.
const NAME_DIGITS: usize = 10;

/// The file name of the segment starting at `offset`.
pub fn segment_name(offset: u64) -> String {
    format!("{offset:0NAME_DIGITS$}")
}

/// The path of the segment starting at `offset`.
pub fn segment_path(dir: &Path, offset: u64) -> PathBuf {
    dir.join(segment_name(offset))
}

/// Every segment's start offset, ascending. Files whose names are not all
/// digits are not segments and are ignored.
pub fn segments(dir: &Path) -> io::Result<Vec<u64>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut starts = Vec::new();
    for entry in entries {
        let name = entry?.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.is_empty()
            && name.bytes().all(|byte| byte.is_ascii_digit())
            && let Ok(start) = name.parse()
        {
            starts.push(start);
        }
    }
    starts.sort_unstable();
    Ok(starts)
}

/// True when a segment after `segment_start` exists, so that segment never
/// changes again. What deletion below a durable cursor keys on.
pub fn is_final(dir: &Path, segment_start: u64) -> io::Result<bool> {
    Ok(segments(dir)?
        .into_iter()
        .any(|start| start > segment_start))
}

/// Segments lying entirely below `durable_cursor`, oldest first, except the
/// newest `keep` of them: a segment is entirely below the cursor when the
/// next segment starts at or before it.
pub fn reclaimable(dir: &Path, durable_cursor: u64, keep: usize) -> io::Result<Vec<u64>> {
    let starts = segments(dir)?;
    let below = starts
        .windows(2)
        .filter(|pair| pair[1] <= durable_cursor)
        .map(|pair| pair[0])
        .collect::<Vec<_>>();
    let reclaim = below.len().saturating_sub(keep);
    Ok(below[..reclaim].to_vec())
}

/// The global offset just past the journal's last whole frame: where a
/// writer opening the journal now would append. Zero for an empty journal.
/// A consumer whose cursor lies beyond it lost the journal's tail to a
/// power cut after storing that cursor.
pub fn end(dir: &Path) -> io::Result<u64> {
    let Some(&start) = segments(dir)?.last() else {
        return Ok(0);
    };
    match fs::read(segment_path(dir, start)) {
        Ok(bytes) => Ok(start + whole_prefix(&bytes) as u64),
        // Reclaimed or cut between listing and reading: look again.
        Err(error) if error.kind() == io::ErrorKind::NotFound => end(dir),
        Err(error) => Err(error),
    }
}

/// Encodes one frame: varint length, then the step.
pub fn encode_frame(step: &Step) -> Vec<u8> {
    step.encode_length_delimited_to_vec()
}

/// Encodes one frame of exactly `len` bytes, so a step written over
/// another keeps every later frame at its offset: the length prefix is
/// written wider than it needs to be, and an unknown field no reader keeps
/// pads the step. None when the step does not fit.
pub fn encode_frame_exact(step: &Step, len: usize) -> Option<Vec<u8>> {
    /// A field number the step does not use, so a reader skips it.
    const PADDING_FIELD: u64 = 2047;
    let body = step.encode_to_vec();
    let room = len.checked_sub(body.len())?;
    // The prefix may take from one to ten bytes; whatever it leaves is the
    // padding field, which needs its key, its length and its bytes.
    for prefix in 1..=10usize.min(room) {
        let mut padding = Vec::new();
        let pad = room - prefix;
        if pad > 0 {
            let key = (PADDING_FIELD << 3) | 2;
            let key_len = prost::encoding::encoded_len_varint(key);
            let Some(rest) = pad.checked_sub(key_len) else {
                continue;
            };
            // The padding's length prefix and its bytes share what is left.
            let Some(filler) = (0..=rest).rev().find(|filler| {
                prost::encoding::encoded_len_varint(*filler as u64) + filler == rest
            }) else {
                continue;
            };
            prost::encoding::encode_varint(key, &mut padding);
            prost::encoding::encode_varint(filler as u64, &mut padding);
            padding.resize(padding.len() + filler, 0);
        }
        let length = (body.len() + padding.len()) as u64;
        if prost::encoding::encoded_len_varint(length) > prefix {
            continue;
        }
        let mut frame = Vec::with_capacity(len);
        // A varint written wider than it needs: continuation bits on the
        // leading bytes, zero groups after the value runs out.
        for at in 0..prefix {
            let group = ((length >> (7 * at)) & 0x7f) as u8;
            frame.push(if at + 1 < prefix { group | 0x80 } else { group });
        }
        frame.extend_from_slice(&body);
        frame.extend_from_slice(&padding);
        return Some(frame);
    }
    None
}

/// What decoding the front of a buffer found.
#[derive(Debug, PartialEq)]
pub enum Frame {
    /// A whole frame of this many bytes.
    Whole(Box<Step>, usize),
    /// The bytes so far are the start of a frame; more may complete it.
    Incomplete,
    /// The bytes can never be a frame: a zero length, a length beyond
    /// [`MAX_FRAME_BYTES`], or a body that is not a Step.
    Corrupt,
}

/// Decodes the frame at the front of `bytes`.
pub fn decode_frame(bytes: &[u8]) -> Frame {
    let mut length: u64 = 0;
    let mut prefix = 0;
    loop {
        let Some(&byte) = bytes.get(prefix) else {
            return Frame::Incomplete;
        };
        if prefix == 10 {
            return Frame::Corrupt;
        }
        length |= u64::from(byte & 0x7f) << (7 * prefix);
        prefix += 1;
        if byte & 0x80 == 0 {
            break;
        }
    }
    // The writer never writes an empty step, so a zero length is garbage,
    // such as the zero-filled tail a file system can leave after power loss.
    if length == 0 || length > MAX_FRAME_BYTES {
        return Frame::Corrupt;
    }
    let end = prefix + length as usize;
    if bytes.len() < end {
        return Frame::Incomplete;
    }
    match Step::decode(&bytes[prefix..end]) {
        Ok(step) => Frame::Whole(Box::new(step), end),
        Err(_) => Frame::Corrupt,
    }
}

/// Appends frames, rotating to a new segment at the configured size.
#[derive(Debug)]
pub struct Writer {
    dir: PathBuf,
    segment_size: u64,
    /// Global offset of the next byte to write.
    offset: u64,
    /// The open segment and its start, once one exists.
    segment: Option<(u64, File)>,
}

impl Writer {
    /// Opens the journal in `dir` for appending, creating the directory.
    /// A torn frame left at the end of the newest segment by an earlier
    /// writer is cut off, so appends continue from the last whole frame.
    pub fn open(dir: impl Into<PathBuf>, segment_size: u64) -> io::Result<Self> {
        let dir = dir.into();
        fs::create_dir_all(&dir)?;
        let Some(&start) = segments(&dir)?.last() else {
            return Ok(Self {
                dir,
                segment_size,
                offset: 0,
                segment: None,
            });
        };
        let path = segment_path(&dir, start);
        let bytes = fs::read(&path)?;
        let whole = whole_prefix(&bytes);
        if whole < bytes.len() {
            truncate(&path, whole as u64)?;
        }
        let file = OpenOptions::new().append(true).open(&path)?;
        Ok(Self {
            dir,
            segment_size,
            offset: start + whole as u64,
            segment: Some((start, file)),
        })
    }

    /// The global offset of the next byte to be written.
    pub fn offset(&self) -> u64 {
        self.offset
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Appends one framed step with a plain write and no fsync, and returns
    /// the global offset after it. An empty step writes nothing. Disk full is
    /// the error the agent drains on. A failed write leaves the journal
    /// ending at the last whole frame, so a later append that succeeds is
    /// not stuck behind a frame that can never complete.
    pub fn append(&mut self, step: &Step) -> io::Result<u64> {
        if step.encoded_len() == 0 {
            return Ok(self.offset);
        }
        let frame = encode_frame(step);
        if let Err(error) = self.write_frame_bytes(&frame) {
            if let Some((start, _)) = &self.segment {
                // Best effort: a disk too full to write may still truncate,
                // and a failure here leaves a torn tail a reopen cuts off.
                let _ = truncate(&segment_path(&self.dir, *start), self.offset - start);
            }
            return Err(error);
        }
        Ok(self.offset)
    }

    /// Writes bytes that are all or part of one frame, rotating first when
    /// the open segment is non-empty and the frame would take it past the
    /// segment size. Callers other than tests write whole frames.
    fn write_frame_bytes(&mut self, bytes: &[u8]) -> io::Result<()> {
        let rotate = match &self.segment {
            None => true,
            Some((start, _)) => {
                let used = self.offset - start;
                used > 0 && used + bytes.len() as u64 > self.segment_size
            }
        };
        if rotate {
            // Close before create: once the next segment exists, this one is
            // final and readers may rely on it never changing.
            self.segment = None;
            let file = OpenOptions::new()
                .append(true)
                .create_new(true)
                .open(segment_path(&self.dir, self.offset))?;
            self.segment = Some((self.offset, file));
        }
        let (_, file) = self.segment.as_mut().expect("segment opened above");
        file.write_all(bytes)?;
        self.offset += bytes.len() as u64;
        Ok(())
    }

    /// Appends the rest of a frame whose start was written without
    /// rotation checks; used only by the synthetic writer.
    #[cfg(feature = "synthetic")]
    fn continue_frame(&mut self, bytes: &[u8]) -> io::Result<()> {
        let (_, file) = self
            .segment
            .as_mut()
            .ok_or_else(|| io::Error::other("no frame in progress"))?;
        file.write_all(bytes)?;
        self.offset += bytes.len() as u64;
        Ok(())
    }
}

/// Cuts the segment at `path` to `len` bytes, through a handle of its own.
/// The writer's append handle cannot do it on Windows: append access there
/// omits FILE_WRITE_DATA, which moving a file's end requires, so the call
/// fails with access denied. The append handle, still open, writes at the
/// new end.
pub fn truncate(path: &Path, len: u64) -> io::Result<()> {
    OpenOptions::new().write(true).open(path)?.set_len(len)
}

/// The length of the longest run of whole frames at the front of `bytes`.
fn whole_prefix(bytes: &[u8]) -> usize {
    let mut at = 0;
    while let Frame::Whole(_, len) = decode_frame(&bytes[at..]) {
        at += len;
    }
    at
}

/// Bytes at the end of a segment that are not a whole frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Torn {
    /// The global offset just past the last whole frame; where the reader
    /// stays until the frame completes.
    pub last_whole: u64,
    /// The torn bytes lie in a final segment and can never complete: the
    /// reader skipped them and continued at the next segment.
    pub skipped: bool,
}

/// What one read found.
#[derive(Debug, Default, PartialEq)]
pub struct Batch {
    /// Each whole frame with the global offset just past it, which is the
    /// cursor a consumer stores once the frame is committed.
    pub frames: Vec<(u64, Step)>,
    pub torn: Option<Torn>,
    /// The read stopped at its frame limit with more whole frames after.
    pub more: bool,
    /// The global offset just past the last byte the read found, whole
    /// frame or not: the journal as this read saw it ends here, so a
    /// journal cut at this offset reads the same again.
    pub seen: u64,
}

/// Reads whole frames from a cursor onwards.
#[derive(Debug)]
pub struct Reader {
    dir: PathBuf,
    cursor: u64,
}

impl Reader {
    pub fn new(dir: impl Into<PathBuf>, cursor: u64) -> Self {
        Self {
            dir: dir.into(),
            cursor,
        }
    }

    /// The global offset just past the last whole frame read.
    pub fn cursor(&self) -> u64 {
        self.cursor
    }

    /// Moves the cursor, for a consumer that rewinds to its durable cursor.
    pub fn seek(&mut self, cursor: u64) {
        self.cursor = cursor;
    }

    /// Reads every whole frame from the cursor to the end of the journal
    /// and advances the cursor past them. A torn tail is reported, never
    /// returned as a frame; in the newest segment the cursor stays at the
    /// last whole frame so a later read picks the frame up once written.
    pub fn read_to_end(&mut self) -> io::Result<Batch> {
        self.read_up_to(usize::MAX)
    }

    /// Reads as [`Reader::read_to_end`] does, but stops after `limit`
    /// frames and says so in [`Batch::more`], so a consumer can commit a
    /// long backlog in pieces.
    pub fn read_up_to(&mut self, limit: usize) -> io::Result<Batch> {
        let mut batch = Batch::default();
        loop {
            let starts = segments(&self.dir)?;
            let Some(position) = starts.iter().rposition(|&start| start <= self.cursor) else {
                // Nothing at or below the cursor: nothing written yet, or the
                // cursor predates the oldest segment and the next one is
                // where the journal resumes.
                match starts.first() {
                    Some(&first) if first > self.cursor => {
                        self.cursor = first;
                        continue;
                    }
                    _ => return Ok(batch),
                }
            };
            let start = starts[position];
            let next = starts.get(position + 1).copied();
            let mut file = match File::open(segment_path(&self.dir, start)) {
                Ok(file) => file,
                // Reclaimed between listing and opening: list again.
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            file.seek(SeekFrom::Start(self.cursor - start))?;
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)?;
            batch.seen = batch.seen.max(self.cursor + bytes.len() as u64);
            let mut at = 0;
            while let Frame::Whole(step, len) = decode_frame(&bytes[at..]) {
                if batch.frames.len() == limit {
                    batch.more = true;
                    return Ok(batch);
                }
                at += len;
                self.cursor += len as u64;
                batch.frames.push((self.cursor, *step));
            }
            let torn = at < bytes.len();
            match next {
                // The newest segment: whatever follows may still be written.
                None => {
                    if torn {
                        batch.torn = Some(Torn {
                            last_whole: self.cursor,
                            skipped: false,
                        });
                    }
                    return Ok(batch);
                }
                // A final segment ends here for good; continue in the next.
                Some(next) => {
                    if torn || self.cursor != next {
                        batch.torn = Some(Torn {
                            last_whole: self.cursor,
                            skipped: true,
                        });
                    }
                    self.cursor = next;
                }
            }
        }
    }
}
