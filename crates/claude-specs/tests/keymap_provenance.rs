use claude_specs::specs::{pty, pty_registry};
use claude_specs::terminal::keymap::{BAKED_KEYMAPS, KeymapSource, load_str};

#[test]
fn baked_verified_entries_have_matching_recording_evidence() {
    for (origin, contents) in BAKED_KEYMAPS {
        let keymap = load_str(contents, origin, KeymapSource::Baked).unwrap();
        for verified in keymap.verified {
            let entry = pty_registry()
                .iter()
                .find(|entry| entry.name == verified.spec)
                .unwrap_or_else(|| {
                    panic!(
                        "baked keymap entry {} names unknown PTY spec {}",
                        verified.version, verified.spec
                    )
                });
            let recording =
                replay_support::load_recording(&pty::fixtures_root().join(entry.recording))
                    .unwrap_or_else(|error| {
                        panic!(
                            "baked keymap entry {}/{}/{} has no recording: {error}",
                            verified.version, verified.run_id, verified.spec
                        )
                    });
            let recorded_matches = recording.manifest.recorded.version == verified.version
                && recording
                    .manifest
                    .provider_extra
                    .get("run_id")
                    .and_then(serde_json::Value::as_str)
                    == Some(verified.run_id.as_str());
            let verification_matches = recording.manifest.verified.iter().any(|evidence| {
                evidence.version == verified.version && evidence.run_id == verified.run_id
            });
            assert!(
                recorded_matches || verification_matches,
                "baked keymap entry {}/{}/{} has no matching Recorded or Verification evidence",
                verified.version,
                verified.run_id,
                verified.spec
            );
        }
    }
}
