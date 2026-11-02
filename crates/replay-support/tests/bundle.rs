//! A dump bundle a real daemon wrote for a real agent on the fake Claude
//! SDK provider (two turns, thinking and streamed text), opened as the
//! records it carries.

use prost::Message as _;
use replay_support::bundle::Bundle;
use replay_support::replay::{Stage, Verdict};

use crate::support;

fn bundle() -> (support::Dumped, Bundle) {
    let dumped = support::healthy();
    let bundle = Bundle::open(dumped.path()).expect("the bundle opens");
    (dumped, bundle)
}

/// The bundle holds the core's record with the journal its inputs read,
/// and the agent's record as its process built it: the session's events
/// by kind and what the interpreter concluded, both turns' ends among
/// them.
#[test]
fn a_bundle_opens_as_the_cores_record_and_the_agents() {
    let (dumped, bundle) = bundle();
    let core = bundle.core.as_ref().expect("the core's record");
    let agent = uuid::Uuid::parse_str(&dumped.agent()).unwrap().into_bytes();
    assert!(
        core.journals
            .get(&daemon_core::AgentId(agent))
            .is_some_and(|segments| !segments.is_empty()),
        "the agent's journal from the checkpoint's cursor"
    );
    let [record] = &bundle.agents[..] else {
        panic!("{} agent records", bundle.agents.len());
    };
    assert_eq!(record.id, dumped.agent());
    assert_eq!(record.record["settings"]["kind"], "KIND_CLAUDE_SDK");
    let turn_ends = record.record["segments"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|segment| segment["events"].as_array().unwrap())
        .filter(|event| !event["step"]["turn_end"].is_null())
        .count();
    assert_eq!(turn_ends, 2, "{}", record.record);
}

/// A report is sent with its dump packed into one compressed
/// `wire.DumpPart`; the downloaded file opens as the directory it was packed
/// from.
#[test]
fn a_packed_dump_opens_as_the_bundle_it_was_packed_from() {
    let (dumped, bundle) = bundle();
    let dir = dumped.path();
    let mut files = Vec::new();
    let mut stack = vec![dir.to_owned()];
    while let Some(at) = stack.pop() {
        for entry in std::fs::read_dir(&at).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let name = path
                .strip_prefix(dir)
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
        assert_eq!(opened.agents.len(), bundle.agents.len());
        assert_eq!(opened.agents[0].record, bundle.agents[0].record);
        assert_eq!(
            opened.core.as_ref().unwrap().journals,
            bundle.core.as_ref().unwrap().journals
        );
    }
}

/// A dump from the command line shows no chat, so the session stage has
/// nothing to replay and says so rather than holding over nothing.
#[test]
fn a_dump_with_no_chat_on_screen_has_no_session_stage() {
    let (_dumped, bundle) = bundle();
    let verdicts = bundle.replay_all().unwrap();
    let session = verdicts
        .iter()
        .find(|verdict| verdict.stage == Stage::Session)
        .unwrap();
    assert!(matches!(session.verdict, Verdict::Absent(_)), "{session}");
}
