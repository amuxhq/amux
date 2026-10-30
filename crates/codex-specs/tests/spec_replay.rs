use codex_specs::specs::{MINIMUM_SUPPORTED, SpecSource, fixtures_root, registry, run};
use replay_support::{
    ReplayOptions, below_minimum, load_recording, orphan_recordings, strict_replay,
};
use semver::Version;

#[tokio::test]
async fn every_registered_specification_replays_strictly() {
    for entry in registry() {
        eprintln!("replaying {}", entry.name);
        let recording = load_recording(&fixtures_root().join(entry.recording))
            .unwrap_or_else(|error| panic!("load {}: {error}", entry.name));
        assert!(
            entry
                .allowed_models
                .contains(&recording.manifest.recorded.model.as_str()),
            "{} was recorded with disallowed model {}",
            entry.name,
            recording.manifest.recorded.model
        );
        let replay = strict_replay(&recording, ReplayOptions::default());
        let controller = replay.controller.clone();
        run(entry, SpecSource::Recorded(replay))
            .await
            .unwrap_or_else(|error| panic!("{error}"));
        let report = controller
            .finish()
            .unwrap_or_else(|error| panic!("{}: {error}", entry.name));
        assert!(report.is_complete(), "{}: {report:?}", entry.name);
    }
}

#[test]
fn corpus_has_no_orphans_and_meets_the_minimum() {
    let root = fixtures_root();
    assert_eq!(orphan_recordings(&root, registry()), Vec::<String>::new());
    let minimum = Version::parse(MINIMUM_SUPPORTED).expect("minimum is semantic");
    assert_eq!(below_minimum(&root, &minimum), Vec::new());
}
