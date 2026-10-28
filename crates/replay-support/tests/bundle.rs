//! A dump bundle a real daemon wrote for a real agent on the fake Claude
//! SDK provider (two turns, thinking and streamed text), replayed through
//! its three pure stages.

use std::path::Path;

use replay_support::bundle::{AgentDump, Bundle, final_texts};
use replay_support::replay::{Stage, Verdict};
use ui_view::RowKind;
use wire::Kind;

fn bundle() -> Bundle {
    Bundle::open(&Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bundle"))
        .expect("the fixture bundle opens")
}

#[test]
fn the_facts_replay_through_the_interpreter_to_what_the_journal_holds() {
    let bundle = bundle();
    let [agent] = bundle.agents.as_slice() else {
        panic!("one agent in the fixture");
    };
    assert_eq!(agent.row.kind(), Kind::ClaudeSdk);
    let replayed = agent.replay_facts().expect("the facts replay");
    assert!(!replayed.is_empty());
    // The ring starts after the interpreter's first frame, which it wrote
    // before any fact: everything after it comes back from the facts alone.
    let journaled = final_texts(&agent.journal[1..]);
    let from_facts = final_texts(&replayed);
    assert_eq!(from_facts, journaled);
    assert!(
        from_facts.values().any(|text| text == "second answer more"),
        "{from_facts:#?}"
    );
    let snapshot = |steps: &[wire::Step]| {
        steps
            .iter()
            .rev()
            .find_map(|step| step.snapshot.clone())
            .map(|snapshot| (snapshot.phase, snapshot.body))
    };
    assert_eq!(
        snapshot(&replayed),
        snapshot(&agent.journal),
        "the replay ends in the journal's snapshot"
    );
}

#[test]
fn the_store_slice_replays_through_the_session_model_and_the_views() {
    let bundle = bundle();
    let agent = &bundle.agents[0];
    let state = agent.session();
    assert!(state.caught_up());
    assert!(state.has_snapshot());
    let held: Vec<&str> = state
        .transcript()
        .iter()
        .map(|held| held.item.key.as_str())
        .collect();
    let stored: Vec<&str> = agent
        .store
        .as_ref()
        .expect("the fixture carries a slice")
        .items
        .iter()
        .map(|item| item.key.as_str())
        .collect();
    assert_eq!(held, stored, "every stored row is held, in order");

    let rows = AgentDump::rows(&state);
    let ids: Vec<&str> = rows.iter().map(|row| row.id.as_str()).collect();
    assert_eq!(ids, held, "one row per item, keyed by it");
    let prose: Vec<&str> = rows
        .iter()
        .filter(|row| matches!(row.kind, RowKind::Prose { .. }))
        .map(|row| row.id.as_str())
        .collect();
    assert!(!prose.is_empty());
    for key in prose {
        assert!(
            state
                .transcript()
                .get(key)
                .unwrap()
                .item
                .text
                .ends_with("more"),
            "{key}"
        );
    }
    for row in &rows {
        println!("{:>3} {:<44} {:?}", row.order, row.id, row.kind);
    }
}

/// A report is uploaded with its dump packed into one compressed
/// `wire.DumpPart`; the downloaded file opens as the directory it was packed
/// from.
#[test]
fn a_packed_dump_opens_as_the_bundle_it_was_packed_from() {
    use prost::Message as _;
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bundle");
    let mut files = Vec::new();
    let mut stack = vec![dir.clone()];
    while let Some(at) = stack.pop() {
        for entry in std::fs::read_dir(&at).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let name = path
                .strip_prefix(&dir)
                .unwrap()
                .iter()
                .map(|part| part.to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            files.push(wire::DumpFile {
                name,
                contents: std::fs::read(&path).unwrap(),
            });
        }
    }
    let packed = wire::DumpPart {
        dump_id: Vec::new(),
        files,
    }
    .encode_to_vec();
    let report = tempfile::tempdir().unwrap();
    let archive = report.path().join("dump.pb.zst");
    std::fs::write(&archive, zstd::bulk::compress(&packed, 3).unwrap()).unwrap();

    for named in [archive.as_path(), report.path()] {
        let opened = Bundle::open(named).expect("the packed dump opens");
        assert_eq!(opened.agents.len(), bundle().agents.len());
        assert_eq!(opened.agents[0].journal, bundle().agents[0].journal);
    }
}

/// A dump whose scope carries no store slice (a daemon core's, or a report
/// with no chat open) has nothing for the session and views stages, and
/// says so rather than holding over nothing.
#[test]
fn a_dump_without_slices_has_no_session_or_views_stage() {
    fn copy(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).unwrap();
        for entry in std::fs::read_dir(from).unwrap() {
            let path = entry.unwrap().path();
            let into = to.join(path.file_name().unwrap());
            if path.is_dir() {
                copy(&path, &into);
            } else if path.file_name().is_some_and(|name| name != "store.pb") {
                std::fs::copy(&path, &into).unwrap();
            }
        }
    }
    let dir = tempfile::tempdir().unwrap();
    copy(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bundle"),
        dir.path(),
    );
    let verdicts = Bundle::open(dir.path()).unwrap().replay_all().unwrap();
    for stage in [Stage::Session, Stage::Views] {
        let verdict = verdicts
            .iter()
            .find(|verdict| verdict.stage == stage)
            .unwrap();
        assert!(matches!(verdict.verdict, Verdict::Absent(_)), "{verdict}");
    }
}
