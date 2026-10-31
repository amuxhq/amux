use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use chrono::{DateTime, Utc};
use redaction::RedactionSummary;
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{IoEvent, parse_script};

/// A verified recording directory and its parsed I/O events.
#[derive(Clone, Debug)]
pub struct Recording {
    pub dir: PathBuf,
    pub manifest: Manifest,
    pub io: Vec<IoEvent>,
}

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
    #[error("recording {spec} was made at {recorded}, below minimum {minimum}")]
    BelowMinimum {
        spec: String,
        recorded: Version,
        minimum: Version,
    },
}

/// Load a recording only after every replay-relevant byte is accounted for.
pub fn load_recording(dir: &Path) -> Result<Recording, RecordingError> {
    let manifest_path = dir.join("manifest.json");
    let manifest_bytes = read_required(&manifest_path)?;
    let manifest: Manifest =
        serde_json::from_slice(&manifest_bytes).map_err(|error| RecordingError::Malformed {
            path: manifest_path.clone(),
            reason: error.to_string(),
        })?;

    validate_inventory(dir, &manifest.content)?;
    let io = parse_script(&dir.join("io.jsonl"))?;
    Ok(Recording {
        dir: dir.to_path_buf(),
        manifest,
        io,
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    const SESSION: &str = "sessions/4c2067b7-f57e-4999-b24d-afdd6b61c8a7.jsonl";
    const SUBAGENT: &str = "sessions/4c2067b7-f57e-4999-b24d-afdd6b61c8a7/subagents/agent.jsonl";

    fn recorded() -> Recorded {
        Recorded {
            provider: "claude".to_string(),
            version: Version::parse("2.1.312").unwrap(),
            model: "claude-haiku-4-5".to_string(),
            at: DateTime::UNIX_EPOCH,
        }
    }

    /// A capture's files, with a session transcript and a subagent's below it.
    fn capture() -> tempfile::TempDir {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("io.jsonl"), b"").unwrap();
        std::fs::write(temp.path().join("spawn.jsonl"), b"{}\n").unwrap();
        for relative in [SESSION, SUBAGENT] {
            let path = temp.path().join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"{}\n").unwrap();
        }
        temp
    }

    fn install_manifest(dir: &Path) -> Manifest {
        let manifest = Manifest::inventory(dir, "example", recorded()).unwrap();
        let mut bytes = serde_json::to_vec_pretty(&manifest).unwrap();
        bytes.push(b'\n');
        std::fs::write(dir.join("manifest.json"), bytes).unwrap();
        manifest
    }

    #[test]
    fn manifest_types_serialize_with_semver_versions() {
        let manifest = Manifest {
            spec: "session/example".to_string(),
            recorded: Recorded {
                provider: "codex".to_string(),
                version: Version::parse("0.150.1").unwrap(),
                model: "gpt-5.6-luna".to_string(),
                at: "2026-08-29T12:00:00Z".parse().unwrap(),
            },
            content: BTreeMap::new(),
            redaction: RedactionSummary::default(),
            session_ids: Vec::new(),
            provider_extra: serde_json::Map::new(),
        };

        let json = serde_json::to_value(&manifest).unwrap();
        assert_eq!(json["recorded"]["version"], "0.150.1");
        assert_eq!(serde_json::from_value::<Manifest>(json).unwrap(), manifest);
    }

    #[test]
    fn the_inventory_covers_session_transcripts_recursively() {
        let temp = capture();
        let manifest = install_manifest(temp.path());

        assert_eq!(
            manifest.content.keys().cloned().collect::<Vec<_>>(),
            vec!["io.jsonl", SESSION, SUBAGENT, "spawn.jsonl"]
        );
        assert!(
            manifest
                .content
                .values()
                .all(|hash| hash.starts_with("sha256:") && hash.len() == 71)
        );
        assert_eq!(load_recording(temp.path()).unwrap().manifest, manifest);
    }

    #[test]
    fn load_manifest_rejects_hash_mismatch() {
        let temp = capture();
        install_manifest(temp.path());
        std::fs::write(temp.path().join("io.jsonl"), b"changed\n").unwrap();

        assert_eq!(
            load_recording(temp.path()).unwrap_err(),
            RecordingError::ContentMismatch {
                path: temp.path().join("io.jsonl")
            }
        );
    }

    #[test]
    fn load_manifest_rejects_uninventoried_file() {
        let temp = capture();
        install_manifest(temp.path());
        let path = temp.path().join("sessions/uninventoried.jsonl");
        std::fs::write(&path, b"{}\n").unwrap();

        assert_eq!(
            load_recording(temp.path()).unwrap_err(),
            RecordingError::Uninventoried { path }
        );
    }
}
