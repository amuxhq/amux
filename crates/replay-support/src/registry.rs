use std::collections::BTreeSet;
use std::path::Path;

use semver::Version;

use crate::{RecordingError, load_recording};

/// One executable specification and the recording that verifies it offline.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SpecEntry {
    pub name: &'static str,
    pub recording: &'static str,
    pub allowed_models: &'static [&'static str],
}

/// The stable, printable provenance row for one registered specification.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegistryRow {
    pub spec: String,
    pub recording: String,
    pub recorded: Version,
}

/// Load every registered recording and return its recorded version in
/// registry order.
pub fn registry_rows(
    fixtures_root: &Path,
    registry: &[SpecEntry],
) -> Result<Vec<RegistryRow>, RecordingError> {
    registry
        .iter()
        .map(|entry| {
            let recording = load_recording(&fixtures_root.join(entry.recording))?;
            Ok(RegistryRow {
                spec: entry.name.to_string(),
                recording: entry.recording.to_string(),
                recorded: recording.manifest.recorded.version,
            })
        })
        .collect()
}

/// Return recording directories beneath `fixtures_root` that no registry entry
/// claims. Only directories containing a manifest are recordings.
pub fn orphan_recordings(fixtures_root: &Path, registry: &[SpecEntry]) -> Vec<String> {
    let claimed = registry
        .iter()
        .map(|entry| normalized_relative(Path::new(entry.recording)))
        .collect::<BTreeSet<_>>();

    recording_directories(fixtures_root)
        .into_iter()
        .filter(|recording| !claimed.contains(recording))
        .collect()
}

/// Return every loadable recording beneath `fixtures_root` whose capture
/// version predates the provider crate's supported minimum.
pub fn below_minimum(fixtures_root: &Path, minimum: &Version) -> Vec<(String, Version)> {
    recording_directories(fixtures_root)
        .into_iter()
        .filter_map(|relative| {
            let recording = load_recording(&fixtures_root.join(&relative)).ok()?;
            (recording.manifest.recorded.version < *minimum)
                .then_some((relative, recording.manifest.recorded.version))
        })
        .collect()
}

fn recording_directories(fixtures_root: &Path) -> Vec<String> {
    let mut recordings = Vec::new();
    collect_recordings(fixtures_root, fixtures_root, &mut recordings);
    recordings.sort();
    recordings
}

fn collect_recordings(root: &Path, current: &Path, recordings: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(current) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        if path.join("manifest.json").is_file() {
            if let Ok(relative) = path.strip_prefix(root) {
                recordings.push(normalized_relative(relative));
            }
        } else {
            collect_recordings(root, &path, recordings);
        }
    }
}

fn normalized_relative(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Manifest, Recorded};

    const MODELS: &[&str] = &["test-model"];
    const REGISTRY: &[SpecEntry] = &[
        SpecEntry {
            name: "current_spec",
            recording: "current",
            allowed_models: MODELS,
        },
        SpecEntry {
            name: "old_spec",
            recording: "nested/old",
            allowed_models: MODELS,
        },
    ];

    fn write_recording(root: &Path, relative: &str, spec: &str, version: &str) {
        let dir = root.join(relative);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("io.jsonl"), b"").unwrap();
        std::fs::write(dir.join("spawn.jsonl"), b"").unwrap();
        let recorded = Recorded {
            provider: "codex".to_string(),
            version: Version::parse(version).unwrap(),
            model: "test-model".to_string(),
            at: chrono::DateTime::UNIX_EPOCH,
        };
        let manifest = Manifest::inventory(&dir, spec, recorded).unwrap();
        let mut bytes = serde_json::to_vec_pretty(&manifest).unwrap();
        bytes.push(b'\n');
        std::fs::write(dir.join("manifest.json"), bytes).unwrap();
    }

    fn fixture() -> tempfile::TempDir {
        let temp = tempfile::tempdir().unwrap();
        write_recording(temp.path(), "current", "current_spec", "1.2.0");
        write_recording(temp.path(), "nested/old", "old_spec", "0.9.0");
        write_recording(temp.path(), "orphan", "orphan_spec", "1.1.0");
        temp
    }

    #[test]
    fn registry_rows_list_recording_versions() {
        let fixture = fixture();

        assert_eq!(
            registry_rows(fixture.path(), REGISTRY).unwrap(),
            vec![
                RegistryRow {
                    spec: "current_spec".to_string(),
                    recording: "current".to_string(),
                    recorded: Version::parse("1.2.0").unwrap(),
                },
                RegistryRow {
                    spec: "old_spec".to_string(),
                    recording: "nested/old".to_string(),
                    recorded: Version::parse("0.9.0").unwrap(),
                },
            ]
        );
    }

    #[test]
    fn orphan_recordings_find_unclaimed_recording_directories() {
        let fixture = fixture();

        assert_eq!(orphan_recordings(fixture.path(), REGISTRY), vec!["orphan"]);
    }

    #[test]
    fn minimum_check_finds_recordings_below_the_supported_version() {
        let fixture = fixture();

        assert_eq!(
            below_minimum(fixture.path(), &Version::parse("1.0.0").unwrap()),
            vec![("nested/old".to_string(), Version::parse("0.9.0").unwrap())]
        );
    }
}
