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

/// The agent's record, encoded: the facts ring under `dir` run through
/// `I` from each segment's checkpoint, beside the lifecycle `core`.
pub(crate) fn record<I: Interpreter>(
    dir: &Path,
    spec: &AgentSpec,
    core: serde_json::Value,
) -> Vec<u8> {
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
    let salt = uuid::Uuid::new_v4().into_bytes();
    let mut record = interpret::record::record::<I>(spec, salt, core, segments);
    if !errors.is_empty() {
        record["errors"] = errors.into();
    }
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
            contents: record::<I>(dir, spec, core),
        }],
    }
}
