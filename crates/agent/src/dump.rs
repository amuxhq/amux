//! The agent's part of a dump: its record, built when the dump asks for
//! it. The facts ring is run again through this process's interpreter,
//! which wrote it, keeping only what the interpreter concluded: never what
//! was said. Its own log the daemon reads from the agent directory.

use std::fs;
use std::path::Path;

use interpret::Interpreter;
use wire::{AgentSpec, DumpFile, DumpPart};

use crate::dir;

/// The name of the agent's record in its part.
pub const RECORD: &str = "record";

/// The facts ring as it stood when read: each segment's checkpoint and
/// bytes, oldest first, and what could not be read.
pub(crate) struct Facts {
    segments: Vec<(Vec<u8>, Vec<u8>)>,
    errors: Vec<String>,
}

/// Reads the facts ring under `dir`. Reading is cheap beside running the
/// ring through the interpreter, so the agent reads it at the step a
/// report asks at, before later steps add to the ring or rotate it.
pub(crate) fn facts(dir: &Path) -> Facts {
    let facts = dir.join(dir::PRIVATE).join(dir::FACTS);
    let mut errors = Vec::new();
    let mut segments = Vec::new();
    match journal::segments(&facts) {
        Ok(starts) => {
            for start in starts {
                let name = journal::segment_name(start);
                let read =
                    fs::read(core_ring::checkpoint_path(&facts, start)).and_then(|checkpoint| {
                        Ok((checkpoint, fs::read(journal::segment_path(&facts, start))?))
                    });
                match read {
                    Ok(segment) => segments.push(segment),
                    // An io error names its kind, never the bytes.
                    Err(error) => errors.push(format!("facts/{name}: {}", error.kind())),
                }
            }
        }
        Err(error) => errors.push(format!("{}: {}", dir::FACTS, error.kind())),
    }
    Facts { segments, errors }
}

/// The agent's record, encoded: `facts` run through `I` from each
/// segment's checkpoint, beside the lifecycle `core`, its oldest events
/// left out where the whole would not fit a report.
pub(crate) fn build<I: Interpreter>(
    spec: &AgentSpec,
    core: serde_json::Value,
    facts: Facts,
) -> Vec<u8> {
    let salt = uuid::Uuid::new_v4().into_bytes();
    let mut record = interpret::record::within::<I>(spec, salt, core, facts.segments, |record| {
        fits(&serde_json::to_vec(record).unwrap_or_default())
    });
    if !facts.errors.is_empty() {
        record["errors"] = facts.errors.into();
    }
    serde_json::to_vec(&record).unwrap_or_default()
}

/// Whether an encoded record fits a report once compressed, as the daemon
/// compresses it, with room for the errors added after.
fn fits(record: &[u8]) -> bool {
    const ROOM: usize = 1 << 10;
    zstd::bulk::compress(record, 3)
        .is_ok_and(|compressed| compressed.len() + ROOM <= interpret::record::LIMIT)
}

/// The record of an agent whose record could not be built: its kind and
/// lifecycle, and why there is nothing else, so the report says the build
/// failed rather than showing an agent that did nothing.
pub(crate) fn failed(spec: &AgentSpec, core: serde_json::Value, why: &str) -> Vec<u8> {
    let record = serde_json::json!({
        "settings": { "kind": spec.kind().as_str_name() },
        "core": core,
        "segments": [],
        "errors": [format!("the record could not be built: {why}")],
    });
    serde_json::to_vec(&record).unwrap_or_default()
}

/// The part the daemon asked for: the record alone.
pub(crate) fn part<I: Interpreter>(
    dir: &Path,
    dump_id: Vec<u8>,
    spec: &AgentSpec,
    core: serde_json::Value,
) -> DumpPart {
    DumpPart {
        dump_id,
        files: vec![DumpFile {
            name: RECORD.to_owned(),
            contents: build::<I>(spec, core, facts(dir)),
        }],
    }
}
