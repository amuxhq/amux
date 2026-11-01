use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use chrono::{DateTime, Utc};
use redaction::RedactionSummary;
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::parse_script;

/// Provenance and the inventory of a recording's immutable content.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub spec: String,
    pub recorded: Recorded,
    pub content: BTreeMap<String, String>,
    pub redaction: RedactionSummary,
    pub session_ids: Vec<String>,
    pub provider_extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Recorded {
    pub provider: String,
    pub version: Version,
    pub model: String,
    pub at: DateTime<Utc>,
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum RecordingError {
    #[error("recording file is missing: {0}")]
    Missing(PathBuf),
    #[error("recording file is malformed: {path}: {reason}")]
    Malformed { path: PathBuf, reason: String },
    #[error("recording content does not match its manifest: {path}")]
    ContentMismatch { path: PathBuf },
    #[error("recording contains an uninventoried file: {path}")]
    Uninventoried { path: PathBuf },
}

/// A recording's manifest, once every replay-relevant byte is accounted
/// for and its I/O parses.
pub fn load_recording(dir: &Path) -> Result<Manifest, RecordingError> {
    let manifest_path = dir.join("manifest.json");
    let manifest_bytes = read_required(&manifest_path)?;
    let manifest: Manifest =
        serde_json::from_slice(&manifest_bytes).map_err(|error| RecordingError::Malformed {
            path: manifest_path.clone(),
            reason: error.to_string(),
        })?;

    validate_inventory(dir, &manifest.content)?;
    parse_script(&dir.join("io.jsonl"))?;
    Ok(manifest)
}

impl Manifest {
    /// The manifest of a capture whose files are already in `dir`, every
    /// replay-relevant one inventoried by its digest.
    pub fn inventory(dir: &Path, spec: &str, recorded: Recorded) -> Result<Self, RecordingError> {
        Ok(Self {
            spec: spec.to_owned(),
            recorded,
            content: build_inventory(dir)?,
            redaction: RedactionSummary::default(),
            session_ids: Vec::new(),
            provider_extra: serde_json::Map::new(),
        })
    }
}

fn validate_inventory(
    dir: &Path,
    content: &BTreeMap<String, String>,
) -> Result<(), RecordingError> {
    for required in ["io.jsonl", "spawn.jsonl"] {
        if !content.contains_key(required) {
            return Err(RecordingError::Uninventoried {
                path: dir.join(required),
            });
        }
    }

    for (relative, expected) in content {
        validate_inventory_path(relative, &dir.join("manifest.json"))?;
        let path = dir.join(relative);
        let actual = digest_file(&path)?;
        if &actual != expected {
            return Err(RecordingError::ContentMismatch { path });
        }
    }

    let mut actual = Vec::new();
    collect_files(dir, dir, &mut actual)?;
    for relative in actual {
        if relative == "manifest.json" {
            continue;
        }
        if !content.contains_key(&relative) {
            return Err(RecordingError::Uninventoried {
                path: dir.join(relative),
            });
        }
    }
    Ok(())
}

fn build_inventory(dir: &Path) -> Result<BTreeMap<String, String>, RecordingError> {
    let mut content = BTreeMap::new();
    for relative in ["io.jsonl", "spawn.jsonl"] {
        content.insert(relative.to_string(), digest_file(&dir.join(relative))?);
    }
    let sessions = dir.join("sessions");
    if sessions.exists() {
        if !sessions.is_dir() {
            return Err(malformed(&sessions, "sessions must be a directory"));
        }
        let mut files = Vec::new();
        collect_files(dir, &sessions, &mut files)?;
        for relative in files {
            content.insert(relative.clone(), digest_file(&dir.join(relative))?);
        }
    }
    Ok(content)
}

fn collect_files(
    root: &Path,
    current: &Path,
    files: &mut Vec<String>,
) -> Result<(), RecordingError> {
    let entries = std::fs::read_dir(current).map_err(|error| io_error(current, error))?;
    for entry in entries {
        let entry = entry.map_err(|error| io_error(current, error))?;
        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path).map_err(|error| io_error(&path, error))?;
        if metadata.file_type().is_symlink() {
            return Err(malformed(&path, "symlinks are not recording content"));
        }
        if metadata.is_dir() {
            collect_files(root, &path, files)?;
        } else if metadata.is_file() {
            let relative = path
                .strip_prefix(root)
                .map_err(|error| malformed(&path, error))?;
            files.push(relative_path(relative)?);
        }
    }
    files.sort();
    Ok(())
}

fn validate_inventory_path(relative: &str, manifest: &Path) -> Result<(), RecordingError> {
    let path = Path::new(relative);
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
        || !(relative == "io.jsonl"
            || relative == "spawn.jsonl"
            || relative.starts_with("sessions/"))
    {
        return Err(malformed(
            manifest,
            format!("invalid inventory path {relative:?}"),
        ));
    }
    Ok(())
}

fn relative_path(path: &Path) -> Result<String, RecordingError> {
    let mut parts = Vec::new();
    for component in path.components() {
        let Component::Normal(part) = component else {
            return Err(malformed(path, "recording path is not relative"));
        };
        let part = part
            .to_str()
            .ok_or_else(|| malformed(path, "recording path is not UTF-8"))?;
        parts.push(part);
    }
    Ok(parts.join("/"))
}

fn digest_file(path: &Path) -> Result<String, RecordingError> {
    let bytes = read_required(path)?;
    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity(71);
    encoded.push_str("sha256:");
    for byte in digest {
        use std::fmt::Write as _;
        write!(&mut encoded, "{byte:02x}").expect("writing to String cannot fail");
    }
    Ok(encoded)
}

fn read_required(path: &Path) -> Result<Vec<u8>, RecordingError> {
    std::fs::read(path).map_err(|error| io_error(path, error))
}

fn io_error(path: &Path, error: std::io::Error) -> RecordingError {
    if error.kind() == std::io::ErrorKind::NotFound {
        RecordingError::Missing(path.to_path_buf())
    } else {
        malformed(path, error)
    }
}

fn malformed(path: &Path, reason: impl ToString) -> RecordingError {
    RecordingError::Malformed {
        path: path.to_path_buf(),
        reason: reason.to_string(),
    }
}
