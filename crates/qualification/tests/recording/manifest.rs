//! A joined capture's manifest inventories every replay-relevant file by
//! its digest, and a recording that disagrees with it is refused.

use std::collections::BTreeMap;
use std::path::Path;

use chrono::DateTime;
use qualification::recording::{Manifest, Recorded, RecordingError, load_recording};
use redaction::RedactionSummary;
use semver::Version;

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
    assert_eq!(load_recording(temp.path()).unwrap(), manifest);
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
