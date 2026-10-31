pub mod bundle;
pub mod live;
mod manifest;
mod registry;
pub mod replay;
mod sanitize;
pub mod show;
pub mod transport;

use std::path::Path;

pub use manifest::{
    MANIFEST_SCHEMA_VERSION, Manifest, Recorded, Recording, RecordingError, load_recording,
};
pub use registry::{RegistryRow, SpecEntry, below_minimum, orphan_recordings, registry_rows};
pub use sanitize::sanitize;
pub use transport::{
    ReplayAdvance, ReplayClock, ReplayController, ReplayError, ReplayReport, ReplayTransport,
    ReplayWriteMismatch, StrictReplay, strict_replay,
};

/// A single IO event from a recorded session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IoEvent {
    pub us: u64,
    pub direction: IoDirection,
    pub line: String,
    pub transport_id: Option<String>,
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IoDirection {
    Write,
    Read,
}

/// Load an `io.jsonl` file into a timestamped event list.
pub fn load_script(path: impl AsRef<Path>) -> Vec<IoEvent> {
    let path = path.as_ref();
    let display = path.display().to_string();
    parse_script(path).unwrap_or_else(|e| panic!("failed to load {display}: {e}"))
}

fn parse_script(path: &Path) -> Result<Vec<IoEvent>, RecordingError> {
    let content = std::fs::read_to_string(path).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => RecordingError::Missing(path.to_path_buf()),
        _ => RecordingError::Malformed {
            path: path.to_path_buf(),
            reason: error.to_string(),
        },
    })?;
    let mut events = Vec::new();
    for (index, line) in content.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let value: serde_json::Value =
            serde_json::from_str(line).map_err(|error| RecordingError::Malformed {
                path: path.to_path_buf(),
                reason: format!("line {}: {error}", index + 1),
            })?;
        let us = value.get("us").and_then(|v| v.as_u64()).unwrap_or(0);
        let dir = value.get("dir").and_then(|v| v.as_str()).unwrap_or("");
        let payload = value
            .get("line")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let direction = match dir {
            "stdin" => IoDirection::Write,
            "stdout" => IoDirection::Read,
            _ => continue,
        };
        let transport_id = value
            .get("transport_id")
            .or_else(|| value.get("spawn_id"))
            .or_else(|| value.get("process_id"))
            .and_then(|v| v.as_str())
            .map(ToOwned::to_owned);
        let session_id = value
            .get("session_id")
            .and_then(|v| v.as_str())
            .map(ToOwned::to_owned);
        events.push(IoEvent {
            us,
            direction,
            line: payload,
            transport_id,
            session_id,
        });
    }
    Ok(events)
}

/// The PATH a probe standing in for `command` hands the real provider:
/// `path` less every folder whose `command` is the probe itself. A wrapper
/// earlier on the PATH that runs the next `command` it finds (a usage
/// meter, a version manager) then reaches the real provider instead of
/// the probe again, which would start another wrapper, without end.
pub fn path_past_probe(
    probe: &Path,
    command: &std::ffi::OsStr,
    path: &std::ffi::OsStr,
) -> std::ffi::OsString {
    let probe = std::fs::canonicalize(probe).unwrap_or_else(|_| probe.to_owned());
    let kept = std::env::split_paths(path).filter(|folder| {
        std::fs::canonicalize(folder.join(command)).map_or(true, |found| found != probe)
    });
    std::env::join_paths(kept).unwrap_or_else(|_| path.to_owned())
}
