use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use chrono::{DateTime, Utc};
use redaction::RedactionSummary;
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{IoEvent, parse_script};

pub const MANIFEST_SCHEMA_VERSION: u32 = 2;

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
    pub schema_version: u32,
    pub spec: String,
    pub recorded: Recorded,
    pub content: BTreeMap<String, String>,
    #[serde(default)]
    pub redaction: RedactionSummary,
    #[serde(default)]
    pub session_ids: Vec<String>,
    #[serde(default)]
    pub provider_extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Recorded {
    pub provider: String,
    pub version: Version,
    pub model: String,
    pub at: DateTime<Utc>,
    pub source_kind: SourceKind,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    LiveCapture,
    Migrated {
        from: String,
    },
    /// Wire traffic written by hand to state a protocol shape the live corpus
    /// does not contain. `note` says what it stands in for and why, so a reader
    /// never mistakes it for something a provider actually said.
    Scripted {
        note: String,
    },
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
    if manifest.schema_version != MANIFEST_SCHEMA_VERSION {
        return Err(RecordingError::Malformed {
            path: manifest_path,
            reason: format!(
                "unsupported schema version {}; expected {MANIFEST_SCHEMA_VERSION}",
                manifest.schema_version
            ),
        });
    }

    validate_inventory(dir, &manifest.content)?;
    let io = parse_script(&dir.join("io.jsonl"))?;
    Ok(Recording {
        dir: dir.to_path_buf(),
        manifest,
        io,
    })
}

/// Convert one copied claude-sdk donor manifest to the ledgered shape.
pub fn migrate_legacy_manifest(
    legacy: &serde_json::Value,
    provider: &str,
    dir: &Path,
) -> Result<Manifest, RecordingError> {
    let path = dir.join("manifest.json");
    let object = legacy
        .as_object()
        .ok_or_else(|| malformed(&path, "legacy manifest must be a JSON object"))?;
    let spec = required_string(object, "spec", &path)?;
    let version_key = format!("{provider}_version");
    let version = object
        .get(&version_key)
        .or_else(|| object.get("claude_code_version"))
        .or_else(|| object.get("version"))
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| malformed(&path, format!("missing provider version for {provider}")))?;
    let version = Version::parse(version)
        .map_err(|error| malformed(&path, format!("invalid provider version: {error}")))?;
    let model = required_string(object, "model", &path)?;
    let at = legacy_recorded_at(object, &path)?;
    let from = object
        .get("agent_sdk_version")
        .and_then(serde_json::Value::as_str)
        .map(|version| format!("claude-sdk {version}"))
        .unwrap_or_else(|| {
            let schema = object
                .get("schema_version")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            format!("legacy manifest schema {schema}")
        });
    let session_ids = object
        .get("session_ids")
        .cloned()
        .map(|value| serde_json::from_value(value).map_err(|error| malformed(&path, error)))
        .transpose()?
        .unwrap_or_default();
    let redaction = object
        .get("redaction_summary")
        .cloned()
        .map(|value| serde_json::from_value(value).map_err(|error| malformed(&path, error)))
        .transpose()?
        .unwrap_or_default();

    let mapped = [
        "schema_version",
        "spec",
        "claude_code_version",
        "version",
        version_key.as_str(),
        "model",
        "recorded_at",
        "at",
        "session_ids",
        "source_kind",
        "content_sha256",
        "redaction_summary",
        "persisted_sessions",
        "observed_frames",
    ];
    let provider_extra = object
        .iter()
        .filter(|(key, _)| !mapped.contains(&key.as_str()))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();

    Ok(Manifest {
        schema_version: MANIFEST_SCHEMA_VERSION,
        spec,
        recorded: Recorded {
            provider: provider.to_string(),
            version,
            model,
            at,
            source_kind: SourceKind::Migrated { from },
        },
        content: build_inventory(dir)?,
        redaction,
        session_ids,
        provider_extra,
    })
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

fn required_string(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
    path: &Path,
) -> Result<String, RecordingError> {
    object
        .get(field)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| malformed(path, format!("missing string field {field}")))
}

fn legacy_recorded_at(
    object: &serde_json::Map<String, serde_json::Value>,
    path: &Path,
) -> Result<DateTime<Utc>, RecordingError> {
    let Some(value) = object.get("recorded_at").or_else(|| object.get("at")) else {
        return Ok(DateTime::UNIX_EPOCH);
    };
    let value = value
        .as_str()
        .ok_or_else(|| malformed(path, "legacy recorded_at must be a string"))?;
    value
        .parse()
        .map_err(|error| malformed(path, format!("invalid recorded_at: {error}")))
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

    const LEGACY_FIXTURE: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/legacy_manifest"
    );

    fn copy_fixture() -> tempfile::TempDir {
        let temp = tempfile::tempdir().unwrap();
        copy_tree(Path::new(LEGACY_FIXTURE), temp.path());
        temp
    }

    fn copy_tree(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).unwrap();
        for entry in std::fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            let target = to.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy_tree(&entry.path(), &target);
            } else {
                std::fs::copy(entry.path(), target).unwrap();
            }
        }
    }

    fn migrate_fixture(dir: &Path) -> Manifest {
        let legacy: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.join("manifest.json")).unwrap()).unwrap();
        migrate_legacy_manifest(&legacy, "claude", dir).unwrap()
    }

    fn install_migrated_manifest(dir: &Path) -> Manifest {
        let manifest = migrate_fixture(dir);
        let mut bytes = serde_json::to_vec_pretty(&manifest).unwrap();
        bytes.push(b'\n');
        std::fs::write(dir.join("manifest.json"), bytes).unwrap();
        manifest
    }

    #[test]
    fn manifest_types_serialize_with_semver_versions() {
        let manifest = Manifest {
            schema_version: MANIFEST_SCHEMA_VERSION,
            spec: "session/example".to_string(),
            recorded: Recorded {
                provider: "codex".to_string(),
                version: Version::parse("0.150.1").unwrap(),
                model: "gpt-5.6-luna".to_string(),
                at: "2026-08-29T12:00:00Z".parse().unwrap(),
                source_kind: SourceKind::LiveCapture,
            },
            content: BTreeMap::new(),
            redaction: RedactionSummary::default(),
            session_ids: Vec::new(),
            provider_extra: serde_json::Map::new(),
        };

        let json = serde_json::to_value(&manifest).unwrap();
        assert_eq!(json["recorded"]["version"], "0.150.1");
        assert_eq!(json["recorded"]["source_kind"], "live_capture");
        assert_eq!(serde_json::from_value::<Manifest>(json).unwrap(), manifest);
    }

    #[test]
    fn migrated_manifest_inventories_donor_sessions_recursively() {
        let temp = copy_fixture();
        let manifest = migrate_fixture(temp.path());

        assert_eq!(manifest.schema_version, MANIFEST_SCHEMA_VERSION);
        assert_eq!(
            manifest.recorded.version,
            Version::parse("2.1.247").unwrap()
        );
        assert_eq!(manifest.recorded.at, DateTime::UNIX_EPOCH);
        assert!(matches!(
            manifest.recorded.source_kind,
            SourceKind::Migrated { ref from } if from == "claude-sdk 0.3.247"
        ));
        assert_eq!(
            serde_json::to_value(&manifest).unwrap()["recorded"]["source_kind"]["migrated"]["from"],
            "claude-sdk 0.3.247"
        );
        assert_eq!(
            manifest.content.keys().cloned().collect::<Vec<_>>(),
            vec![
                "io.jsonl",
                "sessions/4c2067b7-f57e-4999-b24d-afdd6b61c8a7.jsonl",
                "sessions/4c2067b7-f57e-4999-b24d-afdd6b61c8a7/subagents/agent.jsonl",
                "spawn.jsonl",
            ]
        );
        assert!(
            manifest
                .content
                .values()
                .all(|hash| hash.starts_with("sha256:") && hash.len() == 71)
        );
        assert_eq!(manifest.provider_extra["agent_sdk_version"], "0.3.247");
    }

    #[test]
    fn load_manifest_rejects_hash_mismatch() {
        let temp = copy_fixture();
        install_migrated_manifest(temp.path());
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
        let temp = copy_fixture();
        install_migrated_manifest(temp.path());
        let path = temp.path().join("sessions/uninventoried.jsonl");
        std::fs::write(&path, b"{}\n").unwrap();

        assert_eq!(
            load_recording(temp.path()).unwrap_err(),
            RecordingError::Uninventoried { path }
        );
    }
}
