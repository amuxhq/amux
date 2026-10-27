//! The phone's component manifest: every picture the component snapshot
//! suite pins, with a sentence saying what it shows, checked against the
//! committed baselines.
//!
//! `apps/apple/Goldens/manifest.json` lists the components the snapshot
//! suite photographs in-process from authored view data. A reviewer reads it
//! as the index of those pictures, so each must say what it shows, no name
//! may be used twice, and the list and the baselines must name the same
//! pictures.

use std::collections::BTreeSet;
use std::path::Path;

use serde::Deserialize;

/// A picture with a sentence saying what it shows, for whoever reviews it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoldenComponent {
    pub id: String,
    pub shows: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoldenManifest {
    pub components: Vec<GoldenComponent>,
}

impl GoldenManifest {
    pub fn read(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        let manifest: Self =
            serde_json::from_str(&text).map_err(|error| format!("{}: {error}", path.display()))?;
        let mut seen = BTreeSet::new();
        for component in &manifest.components {
            if component.shows.trim().is_empty() {
                return Err(format!(
                    "{}: {} does not say what it shows",
                    path.display(),
                    component.id
                ));
            }
            if !seen.insert(&component.id) {
                return Err(format!(
                    "{}: {} is named twice",
                    path.display(),
                    component.id
                ));
            }
        }
        Ok(manifest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_picture_that_does_not_say_what_it_shows_is_refused() {
        let room = tempfile::tempdir().expect("a temporary directory");
        let path = room.path().join("manifest.json");
        std::fs::write(
            &path,
            r#"{"components": [{"id": "row.prompt", "shows": " "}]}"#,
        )
        .expect("a manifest");
        let error = GoldenManifest::read(&path).expect_err("a silent picture");
        assert!(error.contains("does not say"), "{error}");
    }

    #[test]
    fn a_name_used_twice_is_refused() {
        let room = tempfile::tempdir().expect("a temporary directory");
        let path = room.path().join("manifest.json");
        std::fs::write(
            &path,
            r#"{"components": [{"id": "fleet.busy", "shows": "a"}, {"id": "fleet.busy", "shows": "b"}]}"#,
        )
        .expect("a manifest");
        let error = GoldenManifest::read(&path).expect_err("a repeated name");
        assert!(error.contains("named twice"), "{error}");
    }

    const GOLDENS: &str = "../../apps/apple/Goldens";
    const SNAPSHOTS: &str =
        "../../apps/apple/AmuxComponentSnapshotTests/__Snapshots__/ComponentSnapshotTests";

    fn committed() -> GoldenManifest {
        GoldenManifest::read(&Path::new(GOLDENS).join("manifest.json")).expect("the manifest")
    }

    fn files(directory: &str, extension: &str) -> BTreeSet<String> {
        std::fs::read_dir(directory)
            .expect("the baselines")
            .map(|entry| {
                entry
                    .expect("a file")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .filter(|name| name.ends_with(extension))
            .collect()
    }

    /// The component snapshot suite names a baseline after its example id,
    /// with every character that is not a letter or digit made a dash.
    fn snapshot(id: &str, appearance: &str) -> String {
        format!("components.{}-{appearance}.png", id.replace('.', "-"))
    }

    /// Every component the catalogue pins has a sentence, and the catalogue
    /// vocabulary is all there: each row kind, the activity line, each ask
    /// body, the escape, not-confirmed with resend and discard, the exited
    /// composer's resume and the strip once per provider kind.
    #[test]
    fn the_components_are_the_catalogue_with_a_sentence_each() {
        let manifest = committed();
        let named: BTreeSet<String> = manifest
            .components
            .iter()
            .flat_map(|component| {
                ["light", "dark"].map(|appearance| snapshot(&component.id, appearance))
            })
            .collect();
        assert_eq!(
            named,
            files(SNAPSHOTS, ".png"),
            "components and their baselines disagree"
        );
        let ids: BTreeSet<&str> = manifest.components.iter().map(|c| c.id.as_str()).collect();
        let required = [
            "row.prompt",
            "row.prose",
            "row.prose-working-note",
            "row.thinking",
            "row.tool-call",
            "row.file-change",
            "row.command",
            "row.explore",
            "row.run",
            "row.subagent-running",
            "row.subagent-done",
            "row.background",
            "row.image",
            "row.slash-output",
            "row.turn-end",
            "row.stopped",
            "row.compaction",
            "row.error",
            "row.model-switch",
            "row.boundary",
            "row.agent-message",
            "row.auto-review",
            "row.unrecognized",
            "row.ask-question",
            "row.ask-plan-approved",
            "row.ask-form",
            "row.ask-link",
            "row.ask-grant",
            "row.ask-unanswerable",
            "composer.working",
            "composer.thinking",
            "composer.retrying",
            "ask.permission-command",
            "ask.permission-edit",
            "ask.permission-tool",
            "ask.codex-command",
            "ask.question-single",
            "ask.questions-step",
            "ask.question-secret",
            "ask.plan",
            "ask.form",
            "ask.link",
            "ask.access",
            "ask.unanswerable",
            "composer.not-confirmed",
            "composer.resume",
            "composer.strip-claude-pty",
            "composer.strip-claude-sdk",
            "composer.strip-codex",
            "composer.dictation-denied",
        ];
        let missing: Vec<&str> = required
            .into_iter()
            .filter(|id| !ids.contains(id))
            .collect();
        assert!(missing.is_empty(), "the catalogue lost {missing:?}");
    }
}
