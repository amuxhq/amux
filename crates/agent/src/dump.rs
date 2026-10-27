//! The agent's part of a dump: what only the agent process holds, redacted
//! by its own interpreter before it leaves the process.
//!
//! The part is the facts ring's segments (the last two, which is all the
//! ring keeps), the checkpoint at the start of each, every spec file in
//! the directory, and the tail of the agent's own log and its provider's,
//! each read across its `.1` and redacted as text. Each facts entry is redacted on its own and written back
//! as a line of the same shape, so a dump reader replays the ring exactly
//! as the agent would. A file that cannot be read is left out and named in
//! `dump-errors`, never included unredacted.

use std::path::Path;
use std::{fs, io};

use interpret::ring::Entry;
use interpret::{Interpreter, RedactTarget};
use wire::{DumpFile, DumpPart};

use crate::dir;

/// The name of the file listing what could not be read.
pub const DUMP_ERRORS: &str = "dump-errors";

pub(crate) fn part<I: Interpreter>(dir: &Path, dump_id: Vec<u8>) -> DumpPart {
    let mut files = Vec::new();
    let mut errors = Vec::new();
    let facts = dir.join(dir::PRIVATE).join(dir::FACTS);
    match journal::segments(&facts) {
        Ok(starts) => {
            for start in starts {
                let segment = journal::segment_path(&facts, start);
                let name = format!("{}/{}", dir::FACTS, journal::segment_name(start));
                let checkpoint_name = format!("{name}.checkpoint");
                match fs::read(&segment) {
                    Ok(bytes) => files.push(file(name, facts_segment::<I>(&bytes))),
                    Err(error) => errors.push(format!("{name}: {error}")),
                }
                let checkpoint = core_ring::checkpoint_path(&facts, start);
                let name = checkpoint_name;
                match fs::read(&checkpoint) {
                    Ok(bytes) => match I::redact(RedactTarget::Checkpoint(bytes)) {
                        RedactTarget::Checkpoint(bytes) => files.push(file(name, bytes)),
                        _ => errors.push(format!("{name}: redacted to another shape")),
                    },
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => errors.push(format!("{name}: {error}")),
                }
            }
        }
        Err(error) => errors.push(format!("{}: {error}", dir::FACTS)),
    }
    match spec_names(dir) {
        Ok(names) => {
            for name in names {
                match fs::read(dir.join(&name)) {
                    Ok(bytes) => match I::redact(RedactTarget::Spec(bytes)) {
                        RedactTarget::Spec(bytes) => files.push(file(name, bytes)),
                        _ => errors.push(format!("{name}: redacted to another shape")),
                    },
                    Err(error) => errors.push(format!("{name}: {error}")),
                }
            }
        }
        Err(error) => errors.push(format!("specs: {error}")),
    }
    for (name, path) in [
        (agent_dir::AGENT_LOG, dir.join(agent_dir::AGENT_LOG)),
        (
            dir::PROVIDER_LOG,
            dir.join(dir::PRIVATE).join(dir::PROVIDER_LOG),
        ),
    ] {
        match log_file::tail(&path, log_file::AGENT_CAP) {
            Ok(Some((tail, _))) => files.push(file(
                name.to_owned(),
                interpret::redact_text(&String::from_utf8_lossy(&tail)).into_bytes(),
            )),
            Ok(None) => {}
            Err(error) => errors.push(format!("{name}: {error}")),
        }
    }
    if !errors.is_empty() {
        errors.push(String::new());
        files.push(file(DUMP_ERRORS.to_owned(), errors.join("\n").into_bytes()));
    }
    DumpPart { dump_id, files }
}

fn file(name: String, contents: Vec<u8>) -> DumpFile {
    DumpFile { name, contents }
}

/// Every `spec.<n>` in the directory, oldest first.
fn spec_names(dir: &Path) -> io::Result<Vec<String>> {
    let mut specs = Vec::new();
    for entry in fs::read_dir(dir)? {
        let name = entry?.file_name();
        if let Some(n) = name
            .to_str()
            .and_then(|name| name.strip_prefix("spec."))
            .and_then(|n| n.parse::<u32>().ok())
        {
            specs.push(n);
        }
    }
    specs.sort_unstable();
    Ok(specs.into_iter().map(|n| format!("spec.{n}")).collect())
}

/// A facts segment with every entry redacted. A line this build cannot
/// read may hold anything, so it is replaced by a marker.
fn facts_segment<I: Interpreter>(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    for line in bytes.split(|byte| *byte == b'\n') {
        if line.is_empty() {
            continue;
        }
        let redacted = serde_json::from_slice::<Entry>(line)
            .ok()
            .and_then(redact_entry::<I>)
            .and_then(|entry| serde_json::to_vec(&entry).ok())
            .unwrap_or_else(|| br#"{"event":"unreadable"}"#.to_vec());
        out.extend_from_slice(&redacted);
        out.push(b'\n');
    }
    out
}

fn redact_entry<I: Interpreter>(entry: Entry) -> Option<Entry> {
    Some(match entry {
        Entry::Fact { .. } => {
            let interpret::Event::Fact(fact) = entry.event()? else {
                return None;
            };
            match I::redact(RedactTarget::Fact(fact)) {
                RedactTarget::Fact(fact) => Entry::of(&interpret::Event::Fact(fact)),
                _ => return None,
            }
        }
        Entry::Input { hex } => {
            match I::redact(RedactTarget::Input(interpret::from_hex(&hex).ok()?)) {
                RedactTarget::Input(bytes) => Entry::Input {
                    hex: interpret::to_hex(&bytes),
                },
                _ => return None,
            }
        }
        Entry::AgentMessage { hex } => {
            match I::redact(RedactTarget::AgentMessage(interpret::from_hex(&hex).ok()?)) {
                RedactTarget::AgentMessage(bytes) => Entry::AgentMessage {
                    hex: interpret::to_hex(&bytes),
                },
                _ => return None,
            }
        }
        other => other,
    })
}
