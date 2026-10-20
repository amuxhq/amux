//! Any bytes in a journal's segments, as power loss or a disk can leave
//! them: the reader returns whole frames and stops its cursor inside what
//! is there, never past it, and never panics.

#![no_main]

use std::fs;
use std::path::PathBuf;
use std::sync::OnceLock;

use journal::{Reader, segment_path};
use libfuzzer_sys::fuzz_target;

fn dir() -> &'static PathBuf {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("amux-fuzz-journal-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    })
}

fuzz_target!(|bytes: &[u8]| {
    let dir = dir();
    for entry in fs::read_dir(dir).unwrap() {
        fs::remove_file(entry.unwrap().path()).unwrap();
    }
    // The first byte, when there is one, cuts the rest into two segments,
    // so a read crosses from a sealed segment into the newest.
    let (cut, rest) = match bytes.split_first() {
        Some((&cut, rest)) => (usize::from(cut).min(rest.len()), rest),
        None => (0, bytes),
    };
    let (first, second) = rest.split_at(cut);
    fs::write(segment_path(dir, 0), first).unwrap();
    if !second.is_empty() {
        fs::write(segment_path(dir, first.len() as u64), second).unwrap();
    }
    let mut reader = Reader::new(dir.clone(), 0);
    if let Ok(batch) = reader.read_to_end() {
        assert!(reader.cursor() <= rest.len() as u64);
        drop(batch);
    }
});
