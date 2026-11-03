//! `repro` on every form it takes that needs no network: a report's
//! folder, the archive it was sent as, a report holding an agent's record
//! that breaks a check, an installation folder on a testnet host while it
//! runs and after it stops, and a relay's and the supervisor's reports.
//! Fetching is held to the requests it makes, and to a local S3-compatible
//! stand-in when one is asked for.

use std::path::Path;

use interpret::claude_sdk::ClaudeSdk;
use interpret::{Channel, Event, Fact, Interpreter, checks};
use provider_fakes::script::Step;
use replay_support::fetch::{self, Bucket, Listing};
use replay_support::replay::{Stage, StageVerdict, Verdict};
use replay_support::repro::{Run, repro};
use replay_support::window;
use serde_json::{Value, json};
use testnet::{AgentDecl, Net, Topology};

use crate::support;

/// Every regular file under `from`, copied to the same place under `to`:
/// a running installation also holds sockets.
fn copy_files(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let kind = entry.file_type().unwrap();
        if kind.is_dir() {
            copy_files(&entry.path(), &to.join(entry.file_name()));
        } else if kind.is_file() {
            std::fs::copy(entry.path(), to.join(entry.file_name())).unwrap();
        }
    }
}

fn no_bucket() -> Result<Bucket, String> {
    panic!("nothing here is fetched")
}

fn verdict(run: &Run, stage: Stage) -> &StageVerdict {
    run.verdicts
        .iter()
        .find(|verdict| verdict.stage == stage)
        .unwrap_or_else(|| panic!("no {stage} stage in {:?}", run.verdicts))
}

fn print(runs: &[Run]) {
    for run in runs {
        println!("== {} ==", run.name);
        for verdict in &run.verdicts {
            println!("{verdict}");
        }
        println!("kept {}", run.kept.display());
    }
}

#[test]
fn a_report_folder_replays_every_stage_and_is_kept() {
    let dumped = support::healthy();
    let keep = tempfile::tempdir().unwrap();
    let runs = repro(&dumped.path().to_string_lossy(), keep.path(), no_bucket).unwrap();
    print(&runs);
    let [run] = &runs[..] else {
        panic!("one run: {runs:?}");
    };
    assert!(!run.failed());
    let stages: Vec<Stage> = run.verdicts.iter().map(|verdict| verdict.stage).collect();
    assert_eq!(stages, [Stage::Agent, Stage::Core, Stage::Session]);
    assert!(matches!(
        verdict(run, Stage::Agent).verdict,
        Verdict::Holds(_)
    ));
    assert!(matches!(
        verdict(run, Stage::Core).verdict,
        Verdict::Holds(_)
    ));
    // The copy is a bundle of its own: it replays as the original does,
    // and named again it replays where it is.
    assert!(run.kept.starts_with(keep.path()));
    let kept = replay_support::bundle::Bundle::open(&run.kept).unwrap();
    assert_eq!(kept.replay_all().unwrap(), run.verdicts);
    let again = repro(&run.kept.to_string_lossy(), keep.path(), no_bucket).unwrap();
    assert_eq!(again[0].kept, run.kept);
    assert_eq!(again[0].verdicts, run.verdicts);
}

#[test]
fn a_report_sent_as_its_archive_is_unpacked_where_it_is_kept() {
    let dumped = support::healthy();
    let report = tempfile::tempdir().unwrap();
    let sent = report.path().join("report-1-ab");
    std::fs::create_dir(&sent).unwrap();
    let archive = sent.join("dump.pb.zst");
    std::fs::write(&archive, support::pack(dumped.path())).unwrap();
    for named in [archive.as_path(), sent.as_path()] {
        let keep = tempfile::tempdir().unwrap();
        let runs = repro(&named.to_string_lossy(), keep.path(), no_bucket).unwrap();
        print(&runs);
        let [run] = &runs[..] else {
            panic!("one run: {runs:?}");
        };
        assert_eq!(run.name, "report-1-ab");
        assert!(!run.failed());
        assert!(
            run.kept.join("dump/manifest.json").is_file(),
            "the archive is unpacked beside itself"
        );
        assert!(matches!(
            verdict(run, Stage::Core).verdict,
            Verdict::Holds(_)
        ));
    }
}

/// A report whose agent's record says its interpreter answered a tool
/// result no call was seen for: the record turned into a stream breaks
/// the same check at the same event, from the record alone.
#[test]
fn an_agents_record_that_broke_a_check_reproduces_it_at_its_event() {
    let dumped = support::healthy();
    let agent = dumped.agent();
    let fact = |value: Value| {
        Event::Fact(Fact {
            channel: Channel::Stream,
            payload: serde_json::to_vec(&value).unwrap(),
        })
    };
    let events = [
        fact(
            json!({"type": "system", "subtype": "init", "session_id": "s",
            "claude_code_version": "2.1.312", "model": "claude-opus-5-5"}),
        ),
        fact(
            json!({"type": "user", "session_id": "s", "message": {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "toolu_never_called", "content": "secret"}
            ]}}),
        ),
    ];
    let spec = wire::AgentSpec {
        agent_id: b"agent".to_vec(),
        kind: wire::Kind::ClaudeSdk as i32,
        incarnation: 1,
        provider_command: "/usr/local/bin/claude".into(),
        ..wire::AgentSpec::default()
    };
    let (state, _) = ClaudeSdk::initial(&spec);
    let mut ring = Vec::new();
    for event in &events {
        ring.extend(serde_json::to_vec(&interpret::ring::Entry::of(event)).unwrap());
        ring.push(b'\n');
    }
    let record = interpret::record::record::<ClaudeSdk>(
        &spec,
        [7; 16],
        Value::Null,
        [(interpret::encode_checkpoint(&state), ring)],
    );
    std::fs::write(
        dumped.path().join(format!("agents/{agent}/record")),
        serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();

    let keep = tempfile::tempdir().unwrap();
    let runs = repro(&dumped.path().to_string_lossy(), keep.path(), no_bucket).unwrap();
    print(&runs);
    assert!(runs[0].failed());
    let Verdict::Fails(failing) = &verdict(&runs[0], Stage::Agent).verdict else {
        panic!("{:?}", runs[0].verdicts);
    };
    assert_eq!(failing.step, 2, "{failing}");
    assert_eq!(failing.property, checks::TOOL_RESULT_WITHOUT_CALL.sentence);
    assert!(failing.what.contains(&agent), "{failing}");
    assert!(
        failing.detail.ends_with("the live step broke it there too"),
        "{failing}"
    );
    // The binary says the same and exits non-zero.
    let output = support::repro(dumped.path());
    let stdout = String::from_utf8_lossy(&output.stdout);
    println!("{stdout}");
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stdout
            .lines()
            .any(|line| line.starts_with("agent") && line.contains("FAILS")),
        "{stdout}"
    );
}

/// A testnet host's installation, replayed while it runs: the daemon core
/// from its own ring over its store, and the agent's real facts ring
/// through its interpreter against what it journaled. A line planted in
/// the ring that breaks a check fails the interpreter at that line, and
/// after a clean stop the core says why it cannot be replayed; a journal
/// missing the agent's last step no longer matches the replay.
#[test]
fn an_installation_replays_its_core_and_its_agents_facts() {
    let text = |chunks: &[&str]| Step::Text {
        chunks: chunks.iter().map(|&chunk| chunk.to_owned()).collect(),
    };
    let keep = tempfile::tempdir().unwrap();
    let copy = tempfile::tempdir().unwrap();
    let cut = tempfile::tempdir().unwrap();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let (running, stopped) = runtime.block_on(async {
        let mut net = Net::start(
            Topology::new().host("desk").agent(
                AgentDecl::new("worker", "desk")
                    .steps(vec![
                        text(&["first answer", " more"]),
                        Step::TurnEnd,
                        text(&["second answer"]),
                        Step::TurnEnd,
                    ])
                    .prompt("first question"),
            ),
        )
        .await
        .unwrap();
        net.turn_ended("worker", 1).await.unwrap();
        net.send("worker", "second question").await.unwrap();
        net.turn_ended("worker", 2).await.unwrap();
        let profile = net.runtime("desk").unwrap().dir().to_owned();
        let root = profile.parent().unwrap().parent().unwrap().to_owned();
        let running = repro(&root.to_string_lossy(), keep.path(), no_bucket).unwrap();
        copy_files(&root, copy.path());
        copy_files(&root, cut.path());
        net.stop_daemon("desk").await.unwrap();
        let stopped = repro(&root.to_string_lossy(), keep.path(), no_bucket).unwrap();
        net.shutdown().await.unwrap();
        (running, stopped)
    });
    print(&running);
    let [run] = &running[..] else {
        panic!("one profile: {running:?}");
    };
    assert!(!run.failed());
    let Verdict::Holds(interpreter) = &verdict(run, Stage::Interpreter).verdict else {
        panic!("{:?}", run.verdicts);
    };
    assert!(interpreter.starts_with("1 agent, "), "{interpreter}");
    assert!(
        !interpreter.contains("last 0 frames"),
        "the replay is held to the journal: {interpreter}"
    );
    assert!(matches!(
        verdict(run, Stage::Core).verdict,
        Verdict::Holds(_)
    ));

    print(&stopped);
    let Verdict::Absent(why) = &verdict(&stopped[0], Stage::Core).verdict else {
        panic!("{:?}", stopped[0].verdicts);
    };
    assert!(why.contains("stopped cleanly"), "{why}");
    assert!(matches!(
        verdict(&stopped[0], Stage::Interpreter).verdict,
        Verdict::Holds(_)
    ));

    // A result for a call never made, planted at the end of the newest
    // segment of the copy's ring.
    let agents: Vec<_> = std::fs::read_dir(copy.path().join("profiles"))
        .unwrap()
        .map(|profile| profile.unwrap().path().join("agents"))
        .collect();
    let agent = std::fs::read_dir(&agents[0])
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let facts = agent.join("private/facts");
    let newest = *journal::segments(&facts).unwrap().last().unwrap();
    let segment = journal::segment_path(&facts, newest);
    let mut ring = std::fs::read(&segment).unwrap();
    let lines = ring.iter().filter(|byte| **byte == b'\n').count()
        + journal::segments(&facts)
            .unwrap()
            .iter()
            .filter(|start| **start != newest)
            .map(|start| {
                std::fs::read(journal::segment_path(&facts, *start))
                    .unwrap()
                    .iter()
                    .filter(|byte| **byte == b'\n')
                    .count()
            })
            .sum::<usize>();
    let planted = Event::Fact(Fact {
        channel: Channel::Stream,
        payload: serde_json::to_vec(&json!({"type": "user", "session_id": "s",
        "message": {"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": "toolu_never_called", "content": "x"}
        ]}}))
        .unwrap(),
    });
    ring.extend(serde_json::to_vec(&interpret::ring::Entry::of(&planted)).unwrap());
    ring.push(b'\n');
    std::fs::write(&segment, ring).unwrap();
    let broken = repro(&copy.path().to_string_lossy(), keep.path(), no_bucket).unwrap();
    print(&broken);
    let Verdict::Fails(failing) = &verdict(&broken[0], Stage::Interpreter).verdict else {
        panic!("{:?}", broken[0].verdicts);
    };
    assert_eq!(failing.step, lines + 1, "{failing}");
    assert_eq!(failing.property, checks::TOOL_RESULT_WITHOUT_CALL.sentence);

    // The last frame cut from the other copy's journal.
    let journal = std::fs::read_dir(cut.path().join("profiles"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path()
        .join("agents")
        .join(agent.file_name().unwrap())
        .join("journal");
    let newest = *journal::segments(&journal).unwrap().last().unwrap();
    let segment = journal::segment_path(&journal, newest);
    let bytes = std::fs::read(&segment).unwrap();
    let (mut at, mut last) = (0, 0);
    while let journal::Frame::Whole(_, length) = journal::decode_frame(&bytes[at..]) {
        last = at;
        at += length;
    }
    std::fs::write(&segment, &bytes[..last]).unwrap();
    let cut = repro(&cut.path().to_string_lossy(), keep.path(), no_bucket).unwrap();
    print(&cut);
    let Verdict::Fails(failing) = &verdict(&cut[0], Stage::Interpreter).verdict else {
        panic!("{:?}", cut[0].verdicts);
    };
    assert_eq!(failing.property, models::lifecycle::RESUME_CONTINUES);
}

const RELAY_HOST: daemon_core::HostId = daemon_core::HostId([1; 16]);
const DESK: daemon_core::HostId = daemon_core::HostId([2; 16]);
const LAPTOP: daemon_core::HostId = daemon_core::HostId([3; 16]);
const ALICE: relay_core::UserId = relay_core::UserId([4; 16]);

fn link_up(link: u64, host: daemon_core::HostId, tier: relay_core::Tier) -> relay_core::Input {
    relay_core::Input::LinkUp {
        link: daemon_core::LinkId(link),
        user: ALICE,
        host,
        tier: Some(tier),
        instance: [link as u8; 16],
        direct: None,
        advertised: Vec::new(),
    }
}

/// A window report as the relay's and the supervisor's drivers write one.
fn window_file(
    dir: &Path,
    core: &str,
    start: Value,
    inputs: Value,
    now: Value,
) -> std::path::PathBuf {
    let path = dir.join(format!("{core}-1-ab.json"));
    let written = json!({
        "core": core,
        "amux": "0.0.0",
        "reason": format!("the {core} core: a reason"),
        "start": start,
        "inputs": inputs,
        "now": now,
    });
    std::fs::write(&path, serde_json::to_vec_pretty(&written).unwrap()).unwrap();
    path
}

#[test]
fn a_relay_report_replays_to_the_input_that_broke_its_core() {
    let reports = tempfile::tempdir().unwrap();
    let mut relay = relay_core::Relay::new(RELAY_HOST);
    for input in [
        link_up(1, DESK, relay_core::Tier::Pro),
        link_up(2, LAPTOP, relay_core::Tier::Free),
    ] {
        relay.step(input, &mut Vec::new());
    }
    let inputs = vec![
        relay_core::Input::Tick { now_ms: 5 },
        relay_core::Input::Packet {
            from: daemon_core::LinkId(1),
            dst: LAPTOP,
        },
    ];
    let start = serde_json::to_value(&relay).unwrap();
    let mut now = relay.clone();
    for input in inputs.clone() {
        now.step(input, &mut Vec::new());
    }
    let healthy = window_file(
        reports.path(),
        "relay",
        start.clone(),
        serde_json::to_value(&inputs).unwrap(),
        serde_json::to_value(&now).unwrap(),
    );
    assert!(window::is_window(&healthy));
    let keep = tempfile::tempdir().unwrap();
    let runs = repro(&healthy.to_string_lossy(), keep.path(), no_bucket).unwrap();
    print(&runs);
    assert!(matches!(
        verdict(&runs[0], Stage::Relay).verdict,
        Verdict::Holds(_)
    ));
    assert!(runs[0].kept.is_file());
    let again = repro(&runs[0].kept.to_string_lossy(), keep.path(), no_bucket).unwrap();
    assert_eq!(
        again[0].verdicts, runs[0].verdicts,
        "a kept report replays where it is"
    );

    // A build with a bug that paired the free link wrote this start.
    let mut broken = start;
    broken["pairs"] = json!({"1": [2], "2": [1]});
    let path = window_file(
        reports.path(),
        "relay",
        broken,
        serde_json::to_value(&inputs).unwrap(),
        serde_json::to_value(&now).unwrap(),
    );
    let replayed = window::replay(&path).unwrap();
    println!("{replayed}");
    let Verdict::Fails(failing) = &replayed.verdict else {
        panic!("{replayed}");
    };
    assert_eq!(failing.step, 2, "{failing}");
    assert!(failing.what.starts_with("Packet"), "{failing}");
    assert_eq!(
        failing.property,
        relay_core::FORWARDS_ONLY_PAID_PAIRS.sentence
    );

    // An end the inputs do not step to.
    let mut elsewhere = serde_json::to_value(&now).unwrap();
    elsewhere["now_ms"] = json!(99);
    let path = window_file(
        reports.path(),
        "relay",
        serde_json::to_value(&relay).unwrap(),
        serde_json::to_value(&inputs).unwrap(),
        elsewhere,
    );
    let Verdict::Fails(failing) = window::replay(&path).unwrap().verdict else {
        panic!("the end differs");
    };
    assert_eq!(failing.property, window::ENDS_AS_WRITTEN);
}

#[test]
fn a_supervisor_report_replays_to_the_input_that_broke_its_core() {
    use supervisor_core::{DaemonId, Input, Params, PrevFound, Start, StepBudget, Supervisor};
    let mut out = Vec::new();
    let supervisor = Supervisor::start(
        Start {
            params: Params {
                check_interval_ms: 3_600_000,
                rollback_after: 3,
                start_deadline_ms: 60_000,
                stop_deadline_ms: 30_000,
                backoff_first_ms: 1_000,
                backoff_max_ms: 30_000,
                backoff_reset_ms: 60_000,
                replace_while_running: true,
                step_budget: StepBudget::UNLIMITED,
            },
            target: "aarch64-apple-darwin".into(),
            running: supervisor_core::Version::new(1, 2, 3),
            prev: PrevFound::None,
            rejected: None,
            inherited: false,
            now_ms: 1_000,
        },
        &mut out,
    );
    let inputs = vec![
        Input::Tick { now_ms: 1_005 },
        Input::Prepared {
            daemon: DaemonId(1),
        },
    ];
    let mut now = supervisor.clone();
    for input in inputs.clone() {
        now.step(input, &mut out);
    }
    let reports = tempfile::tempdir().unwrap();
    let start = serde_json::to_value(&supervisor).unwrap();
    let healthy = window_file(
        reports.path(),
        "supervisor",
        start.clone(),
        serde_json::to_value(&inputs).unwrap(),
        serde_json::to_value(&now).unwrap(),
    );
    let replayed = window::replay(&healthy).unwrap();
    println!("{replayed}");
    assert_eq!(replayed.stage, Stage::Supervisor);
    assert!(matches!(replayed.verdict, Verdict::Holds(_)), "{replayed}");

    // A build that lost track of which binary has served wrote this start.
    let mut broken = start;
    broken["installed"]["path"]["proven"] = json!(false);
    let path = window_file(
        reports.path(),
        "supervisor",
        broken,
        serde_json::to_value(&inputs).unwrap(),
        serde_json::to_value(&now).unwrap(),
    );
    let output = support::repro(&path);
    let stdout = String::from_utf8_lossy(&output.stdout);
    println!("{stdout}");
    assert_eq!(output.status.code(), Some(1));
    let line = stdout
        .lines()
        .find(|line| line.starts_with("supervisor"))
        .expect("a line for the supervisor");
    assert!(line.contains("FAILS") && line.contains("step 1"), "{line}");
    assert!(
        line.contains(supervisor_core::NO_STARTABLE_BINARY.sentence),
        "{line}"
    );
}

fn bucket(endpoint: &str) -> Bucket {
    Bucket::from_vars(|name| {
        Some(
            match name {
                fetch::ENDPOINT => endpoint,
                fetch::BUCKET => "the-bucket",
                fetch::ACCESS_KEY_ID => "AKID",
                fetch::SECRET_ACCESS_KEY => "se\"cret",
                _ => return None,
            }
            .to_owned(),
        )
    })
    .unwrap()
}

#[test]
fn a_fetch_signs_each_request_for_the_bucket_the_environment_names() {
    let missing = Bucket::from_vars(|name| (name == fetch::BUCKET).then(|| "b".to_owned()));
    let why = missing.unwrap_err();
    for name in [
        fetch::ENDPOINT,
        fetch::ACCESS_KEY_ID,
        fetch::SECRET_ACCESS_KEY,
    ] {
        assert!(why.contains(name), "{why}");
    }
    let bucket = bucket("https://store.example/");
    assert_eq!(bucket.region, "auto");
    assert!(!format!("{bucket:?}").contains("se\"cret"), "{bucket:?}");
    assert_eq!(
        bucket.list_url("reports/", None),
        "https://store.example/the-bucket?list-type=2&prefix=reports%2F"
    );
    assert_eq!(
        bucket.list_url("reports/", Some("a+b=")),
        "https://store.example/the-bucket?list-type=2&prefix=reports%2F&continuation-token=a%2Bb%3D"
    );
    assert_eq!(
        bucket.object_url("reports/user 1/0a/dump.pb.zst"),
        "https://store.example/the-bucket/reports/user%201/0a/dump.pb.zst"
    );
    let config = bucket.curl_config(
        "https://store.example/the-bucket/k",
        Some(Path::new("/tmp/out")),
    );
    assert_eq!(
        config,
        "silent\nshow-error\nfail\nuser = \"AKID:se\\\"cret\"\naws-sigv4 = \"aws:amz:auto:s3\"\n\
         url = \"https://store.example/the-bucket/k\"\noutput = \"/tmp/out\"\n"
    );
    assert_eq!(
        fetch::report_id("0A1B2C3D-0000-4000-8000-00000000000F").as_deref(),
        Some("0a1b2c3d00004000800000000000000f")
    );
    assert_eq!(fetch::report_id("reports/u/0a"), None);

    let page = fetch::listing(
        "<?xml version=\"1.0\"?><ListBucketResult><IsTruncated>true</IsTruncated>\
         <Contents><Key>reports/u/0a/report.json</Key><Size>3</Size></Contents>\
         <Contents><Key>reports/u/0a/a&amp;b</Key></Contents>\
         <NextContinuationToken>next</NextContinuationToken></ListBucketResult>",
    );
    assert_eq!(
        page,
        Listing {
            keys: vec![
                "reports/u/0a/report.json".to_owned(),
                "reports/u/0a/a&b".to_owned()
            ],
            next: Some("next".to_owned()),
        }
    );
    assert_eq!(
        fetch::listing("<ListBucketResult><IsTruncated>false</IsTruncated></ListBucketResult>"),
        Listing::default()
    );
}

/// A prefix names one report, `reports/<user>/<id>`, both plain names:
/// its id becomes the folder it is downloaded into, which is emptied
/// first, so nothing else names a folder. Anything else is refused before
/// a bucket is asked for.
#[test]
fn only_a_reports_user_and_id_prefix_is_fetched() {
    assert_eq!(fetch::report_prefix("reports/u/0a"), Some(("u", "0a")));
    assert_eq!(fetch::report_prefix("reports/u/0a/"), Some(("u", "0a")));
    for named in [
        "reports/",
        "reports",
        "reports/u",
        "reports/u/",
        "reports/x/..",
        "reports/../x",
        "reports/u/.",
        "reports//0a",
        "reports/u/0a/extra",
        "elsewhere/u/0a",
    ] {
        assert_eq!(fetch::report_prefix(named), None, "{named}");
    }
    let scratch = tempfile::tempdir().unwrap();
    let keep = scratch.path().join("repro");
    std::fs::create_dir_all(&keep).unwrap();
    let beside = scratch.path().join("beside");
    std::fs::write(&beside, b"kept").unwrap();
    for named in ["reports/x/..", "reports/"] {
        let refused = repro(named, &keep, || Err("a bucket was asked for".to_owned())).unwrap_err();
        assert!(
            refused.contains("reports/<user>/<id>"),
            "{named}: {refused}"
        );
        let refused = bucket("http://127.0.0.1:9")
            .fetch(named, &keep)
            .unwrap_err();
        assert!(
            refused.contains("reports/<user>/<id>"),
            "{named}: {refused}"
        );
    }
    assert!(
        beside.is_file() && keep.is_dir(),
        "nothing outside a report's folder goes"
    );
}

/// A report fetched by its id from a local S3-compatible store that checks
/// every request's signature, laid out as the account service lays
/// reports out, replays as the folder it was sent from. Needs docker; it
/// runs a SeaweedFS container of its own for the length of the test.
#[test]
#[ignore = "runs a SeaweedFS container: needs docker and the chrislusf/seaweedfs:3.80 image"]
fn a_report_fetched_by_its_id_from_an_s3_stand_in_replays() {
    use std::process::Command;
    let config = tempfile::tempdir().unwrap();
    let identities = config.path().join("s3.json");
    std::fs::write(
        &identities,
        br#"{"identities":[{"name":"repro","credentials":[{"accessKey":"AKID","secretKey":"secret-key"}],"actions":["Admin","Read","List","Write"]}]}"#,
    )
    .unwrap();
    let name = format!("amux-repro-{}", std::process::id());
    let started = Command::new("docker")
        .args([
            "run",
            "-d",
            "--rm",
            "--name",
            &name,
            "-p",
            "127.0.0.1::8333",
            "-v",
        ])
        .arg(format!("{}:/etc/s3.json:ro", identities.display()))
        .args([
            "chrislusf/seaweedfs:3.80",
            "server",
            "-s3",
            "-s3.config=/etc/s3.json",
        ])
        .status()
        .unwrap();
    assert!(started.success());
    struct Stop(String);
    impl Drop for Stop {
        fn drop(&mut self) {
            let _ = Command::new("docker").args(["stop", &self.0]).status();
        }
    }
    let _stop = Stop(name.clone());
    let port = Command::new("docker")
        .args(["port", &name, "8333"])
        .output()
        .unwrap();
    let endpoint = format!(
        "http://{}",
        String::from_utf8_lossy(&port.stdout)
            .lines()
            .next()
            .unwrap()
            .trim()
    );
    let with_secret = |secret: &'static str| {
        let endpoint = endpoint.clone();
        Bucket::from_vars(move |var| {
            Some(
                match var {
                    fetch::ENDPOINT => endpoint.as_str(),
                    fetch::BUCKET => "reports-test",
                    fetch::ACCESS_KEY_ID => "AKID",
                    fetch::SECRET_ACCESS_KEY => secret,
                    fetch::REGION => "us-east-1",
                    _ => return None,
                }
                .to_owned(),
            )
        })
        .unwrap()
    };
    let bucket = with_secret("secret-key");
    // The store answers once it is up; then the bucket is made and the
    // report put where the account service puts it.
    let curl = |method: &str, url: &str, body: Option<&Path>| {
        let mut command = Command::new("curl");
        command
            .args([
                "-s",
                "-f",
                "-X",
                method,
                "--aws-sigv4",
                "aws:amz:us-east-1:s3",
            ])
            .args(["--user", "AKID:secret-key", url]);
        if let Some(body) = body {
            command.arg("--upload-file").arg(body);
        }
        command.status().unwrap().success()
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while !curl("PUT", &format!("{endpoint}/reports-test"), None) {
        assert!(
            std::time::Instant::now() < deadline,
            "the store never answered"
        );
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    let dumped = support::healthy();
    let files = tempfile::tempdir().unwrap();
    let archive = files.path().join("dump.pb.zst");
    std::fs::write(&archive, support::pack(dumped.path())).unwrap();
    let header = files.path().join("report.json");
    std::fs::write(&header, b"{\"schema\":1}").unwrap();
    let id = "0123456789abcdef0123456789abcdef";
    for (file, path) in [("dump.pb.zst", &archive), ("report.json", &header)] {
        let key = format!("reports/some-user/{id}/{file}");
        assert!(curl("PUT", &bucket.object_url(&key), Some(path)));
    }
    // Another user's report, which the listing passes over.
    let other = format!("reports/another-user/{}/report.json", "f".repeat(32));
    assert!(curl("PUT", &bucket.object_url(&other), Some(&header)));

    let keep = tempfile::tempdir().unwrap();
    let hyphenated = "01234567-89ab-cdef-0123-456789abcdef";
    for named in [hyphenated.to_owned(), format!("reports/some-user/{id}")] {
        let runs = repro(&named, keep.path(), || Ok(bucket.clone())).unwrap();
        print(&runs);
        assert_eq!(runs[0].name, id);
        assert!(!runs[0].failed());
        assert!(runs[0].kept.join("report.json").is_file());
        assert!(runs[0].kept.join("dump/manifest.json").is_file());
        assert!(matches!(
            verdict(&runs[0], Stage::Core).verdict,
            Verdict::Holds(_)
        ));
    }
    let missing = repro(&"e".repeat(32), keep.path(), || Ok(bucket.clone())).unwrap_err();
    assert!(missing.contains("no report"), "{missing}");
    // The store checks the signature: a wrong key is refused, and the
    // refusal names the request, never the key.
    let refused = repro(hyphenated, keep.path(), || Ok(with_secret("wrong-key"))).unwrap_err();
    println!("{refused}");
    assert!(refused.contains("403"), "{refused}");
    assert!(!refused.contains("wrong-key"), "{refused}");
}
