//! Evidence updates owned by the Claude specification probe.

use std::path::Path;

use replay_support::{SpecEntry, Verification, append_verification, load_recording};

use crate::terminal::keymap::{self, VerifiedVersion};

/// Record a passing live verification in both the recording ledger and the
/// baked keymap whose semantic programs the PTY specification exercised.
pub fn append_pty_verification(
    fixtures: &Path,
    keymap: &Path,
    entry: SpecEntry,
    verification: Verification,
) -> Result<(), std::io::Error> {
    append_verification(&fixtures.join(entry.recording), verification.clone())
        .map_err(std::io::Error::other)?;
    append_keymap(keymap, entry, verification.version, verification.run_id)
}

/// Mint keymap evidence for a newly recorded specification only after its
/// manifest has been written and strict replay has passed.
pub fn append_recorded_pty(
    fixtures: &Path,
    keymap: &Path,
    entry: SpecEntry,
    version: semver::Version,
    run_id: String,
) -> Result<(), std::io::Error> {
    let recording =
        load_recording(&fixtures.join(entry.recording)).map_err(std::io::Error::other)?;
    let recorded_run = recording
        .manifest
        .provider_extra
        .get("run_id")
        .and_then(serde_json::Value::as_str);
    if recording.manifest.spec != entry.name
        || recording.manifest.recorded.version != version
        || recorded_run != Some(run_id.as_str())
    {
        return Err(std::io::Error::other(format!(
            "recording {} does not carry the passing version/run {version}/{run_id}",
            entry.name
        )));
    }
    append_keymap(keymap, entry, version, run_id)
}

fn append_keymap(
    path: &Path,
    entry: SpecEntry,
    version: semver::Version,
    run_id: String,
) -> Result<(), std::io::Error> {
    keymap::append_verified(
        path,
        VerifiedVersion {
            version,
            run_id,
            spec: entry.name.to_owned(),
        },
    )
    .map_err(std::io::Error::other)
}
