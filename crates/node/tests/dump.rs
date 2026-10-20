//! Dump assembly: one bundle under reports/ with the redacted store slice,
//! each agent's part, the last journal segments and the daemon's log, in
//! the documented layout, with every planted secret absent. Real agent
//! processes on the fake provider supply their own redacted parts.

mod support;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use node::{DAEMON_LOG, MANIFEST};
use prost::Message as _;
use support::*;
use wire::{DumpRequest, Lifecycle, Phase};

const TOKEN: &str = "ghp_PLANTEDdumptoken0001abcdef";
const KEY: &str = "sk-ant-api03-PLANTEDdumpkey0002";
const EMAIL: &str = "planted.dumper@example.com";
const ENV_SECRET: &str = "PLANTEDenvsecret0003";

const PLANTED: &[&str] = &[TOKEN, KEY, EMAIL, ENV_SECRET];

fn tree(root: &Path) -> Vec<(String, u64)> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_owned()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                stack.push(path);
            } else {
                // Named with '/' whatever the platform's separator, as the
                // assertions below spell them.
                let name = path
                    .strip_prefix(root)
                    .unwrap()
                    .iter()
                    .map(|part| part.to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/");
                files.push((name, entry.metadata().unwrap().len()));
            }
        }
    }
    files.sort();
    files
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle.as_bytes())
}

/// The secret as plain text or hex-encoded, the form the facts ring keeps
/// the bytes written to the provider in.
fn holds(haystack: &[u8], secret: &str) -> bool {
    contains(haystack, secret) || contains(haystack, &interpret::to_hex(secret.as_bytes()))
}

#[tokio::test(flavor = "multi_thread")]
async fn a_dump_bundles_the_redacted_slice_parts_journal_tails_and_log() {
    let install = Install::new();
    let mut launch = install.launch(
        "dump",
        vec![
            text(&format!("pushed with {TOKEN}; mailed {EMAIL}")),
            provider_fakes::script::Step::TurnEnd,
        ],
    );
    launch
        .provider_env
        .insert("DEPLOY_SECRET".to_owned(), ENV_SECRET.to_owned());
    let log = install.path("daemon.log");
    std::fs::write(
        &log,
        format!("INFO started\nWARN a relay call failed: Authorization: Bearer {KEY}\n"),
    )
    .unwrap();
    let options = node::StartOptions {
        daemon_log: Some(log),
        ..install.options("boot-1", launch)
    };
    let daemon = node::start(options, None).await.unwrap();
    let runtime = runtime(&daemon, &install);

    let prompt = format!("deploy using {KEY} and tell {EMAIL}");
    let live = id_of(
        &runtime
            .spawn(create(&install.work, "live", Some(&prompt)), None)
            .await
            .unwrap(),
    );
    let stopped = id_of(
        &runtime
            .spawn(create(&install.work, "stopped", Some(&prompt)), None)
            .await
            .unwrap(),
    );
    for id in [live, stopped] {
        until("the first turn to end", || async {
            let phase = runtime.agent(id).await.unwrap().phase;
            (phase == Phase::Idle as i32)
                .then_some(())
                .ok_or_else(|| format!("phase {phase}"))
        })
        .await
        .unwrap();
    }
    runtime.stop(stopped).await.unwrap();
    assert_eq!(
        runtime.agent(stopped).await.unwrap().lifecycle,
        Lifecycle::Exited as i32
    );

    // A checkpoint now: the core's part then carries a store holding the
    // prompts' rows, and the inputs after it.
    runtime.checkpoint().await.unwrap();

    // An input sent after start goes to the provider, and into the live
    // agent's facts ring, as bytes.
    let answer = runtime
        .send_input(&wire::SendInputRequest {
            agent_id: live.as_bytes().to_vec(),
            input: Some(sdk_prompt(
                b"later",
                &format!("retry with {TOKEN} and {KEY}; cc {EMAIL}"),
            )),
        })
        .await
        .unwrap();
    assert!(
        matches!(answer.of, Some(wire::send_input_response::Of::Accepted(_))),
        "{answer:?}"
    );
    let live_key =
        store::AgentKey::new(runtime.host().as_bytes().to_vec(), live.as_bytes().to_vec());
    until("the later input's reflection", || async {
        let live_key = live_key.clone();
        runtime
            .read_store(move |store| store.item_by_input(&live_key, b"later").unwrap())
            .await
            .unwrap()
            .map(|_| ())
            .ok_or("no row carries it")
    })
    .await
    .unwrap();

    // The asking client's open chat, as it hands it in unredacted: the
    // rows its window held, a send of its own since and a driver act whose
    // words name a secret.
    let held = runtime
        .read_store({
            let live_key = live_key.clone();
            move |store| store.cut(&live_key, 100).unwrap()
        })
        .await
        .unwrap();
    let client_msg = |of| wire::ClientTraced {
        seq: 0,
        at_ms: 0,
        event: Some(wire::client_traced::Event::Msg(wire::ClientSessionMsg {
            of: Some(of),
        })),
    };
    let session = wire::ClientSessionHistory {
        agent_id: live.as_bytes().to_vec(),
        start: Some(wire::ClientSession {
            agent: Some(runtime.agent(live).await.unwrap()),
            snapshot: held.snapshot.clone(),
            transcript: Some(wire::ClientTranscript {
                items: held.held.clone(),
                exhausted: true,
            }),
            ..wire::ClientSession::default()
        }),
        events: Some(wire::ClientSessionEvents {
            traced: vec![
                client_msg(wire::client_session_msg::Of::Send(sdk_prompt(
                    b"from-the-client",
                    &format!("again with {TOKEN}; cc {EMAIL}"),
                ))),
                wire::ClientTraced {
                    seq: 1,
                    at_ms: 0,
                    event: Some(wire::client_traced::Event::Driver(format!(
                        "GetFailed {{ error: \"Authorization: Bearer {KEY}\" }}"
                    ))),
                },
            ],
        }),
    };
    let elsewhere = wire::ClientSessionHistory {
        agent_id: uuid::Uuid::new_v4().as_bytes().to_vec(),
        ..wire::ClientSessionHistory::default()
    };
    let bundle = runtime
        .dump(DumpRequest {
            agent_ids: Vec::new(),
            reason: format!("the chat froze; my key is {KEY}"),
            automatic: false,
            sessions: vec![session, elsewhere.clone()],
        })
        .await
        .unwrap();
    assert_eq!(
        bundle.parent().unwrap(),
        install.data_dir.join(node::REPORTS)
    );
    let files = tree(&bundle);
    let mut transcript = vec![format!(
        "bundle {}",
        bundle.file_name().unwrap().to_string_lossy()
    )];
    transcript.extend(
        files
            .iter()
            .map(|(name, size)| format!("  {name:<60} {size:>7} bytes")),
    );

    let names: Vec<&str> = files.iter().map(|(name, _)| name.as_str()).collect();
    let has = |name: String| {
        assert!(names.contains(&name.as_str()), "{name} is in the bundle");
    };
    has(MANIFEST.to_owned());
    has(DAEMON_LOG.to_owned());
    for id in [live, stopped] {
        has(format!("agents/{id}/row.pb"));
        has(format!("agents/{id}/store.pb"));
        has(format!("agents/{id}/part/spec.1"));
        assert!(
            names
                .iter()
                .any(|name| name.starts_with(&format!("agents/{id}/journal/"))),
            "{id}'s journal tail"
        );
    }
    // The core's part: its checkpoint, the inputs since and the store as
    // of that checkpoint, each redacted, and the store alone, with no WAL
    // or rollback journal beside it.
    for name in ["checkpoint", "inputs", "store.sqlite"] {
        has(format!("core/{name}"));
    }
    assert!(
        !names
            .iter()
            .any(|name| name.starts_with("core/store.sqlite-")),
        "{names:?}"
    );
    let checkpoint = daemon_core::Checkpoint::from_bytes(
        &std::fs::read(bundle.join("core/checkpoint")).unwrap(),
    )
    .expect("the core's checkpoint reads");
    let lines: Vec<daemon_core::Recorded> =
        core_ring::inputs(&std::fs::read(bundle.join("core/inputs")).unwrap());
    let reads = lines
        .iter()
        .filter(|line| matches!(line, daemon_core::Recorded::Read { .. }))
        .count();
    assert!(
        reads > 0,
        "the ring says how far each ingest read: {} lines",
        lines.len()
    );
    let scratch = tempfile::tempdir().unwrap();
    let copy = scratch.path().join("store.sqlite");
    std::fs::copy(bundle.join("core/store.sqlite"), &copy).unwrap();
    let copy = store::Sqlite::open(&copy, runtime.host().as_bytes().to_vec())
        .expect("the store copy opens");
    assert_eq!(
        copy.agents().unwrap().len(),
        2,
        "the store as of the checkpoint holds both agents"
    );
    let restored = daemon_core::Daemon::restore(checkpoint, copy);
    assert!(restored.is_ok(), "the core restores from its part");

    let live_part: Vec<&&str> = names
        .iter()
        .filter(|name| name.starts_with(&format!("agents/{live}/part/facts/")))
        .collect();
    assert!(
        live_part.iter().any(|name| name.ends_with(".checkpoint")),
        "the live agent sent its ring and checkpoint: {live_part:?}"
    );

    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(bundle.join(MANIFEST)).unwrap()).unwrap();
    let errors = manifest["errors"].as_array().unwrap();
    assert_eq!(manifest["core"]["reads"].as_u64(), Some(reads as u64));
    assert!(
        errors
            .iter()
            .any(|error| error.as_str().unwrap().starts_with(&stopped.to_string())),
        "the stopped agent's missing ring is named: {errors:?}"
    );
    assert_eq!(manifest["agents"].as_array().unwrap().len(), 2);
    // The client's session, redacted through the agent's kind, beside a
    // session of an agent the dump does not cover, which is named instead.
    let client = format!("{}/{live}", node::CLIENT_SESSIONS);
    has(format!("{client}/{}", node::CLIENT_CHECKPOINT));
    has(format!("{client}/{}", node::CLIENT_EVENTS));
    let sessions = manifest["client"]["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1, "{sessions:?}");
    assert_eq!(
        sessions[0]["checkpoint"].as_str(),
        Some(format!("{client}/{}", node::CLIENT_CHECKPOINT).as_str())
    );
    let elsewhere = uuid::Uuid::from_slice(&elsewhere.agent_id).unwrap();
    assert!(
        errors.iter().any(|error| error
            .as_str()
            .unwrap()
            .starts_with(&format!("{elsewhere} client session"))),
        "{errors:?}"
    );
    let start = wire::ClientSession::decode(
        std::fs::read(bundle.join(&client).join(node::CLIENT_CHECKPOINT))
            .unwrap()
            .as_slice(),
    )
    .expect("the session's checkpoint reads");
    let window = start.transcript.unwrap_or_default().items;
    let shape = |items: &[wire::Item]| -> Vec<(String, u64, u64)> {
        items
            .iter()
            .map(|item| (item.key.clone(), item.order, item.revision))
            .collect()
    };
    assert_eq!(
        shape(&window),
        shape(&held.held),
        "the rows keep their places"
    );
    assert!(
        window.iter().any(|item| item.text.contains("<REDACTED>")),
        "the client's rows are kept, redacted"
    );
    let events = wire::ClientSessionEvents::decode(
        std::fs::read(bundle.join(&client).join(node::CLIENT_EVENTS))
            .unwrap()
            .as_slice(),
    )
    .expect("the session's events read");
    assert_eq!(events.traced.len(), 2);

    let slice = wire::Step::decode(
        std::fs::read(bundle.join(format!("agents/{live}/store.pb")))
            .unwrap()
            .as_slice(),
    )
    .unwrap();
    assert!(slice.snapshot.is_some(), "the slice holds the snapshot");
    assert!(
        slice
            .items
            .iter()
            .any(|item| item.text.contains("<REDACTED>")),
        "the prompt's reflection is kept, redacted"
    );
    assert!(
        slice
            .items
            .iter()
            .all(|item| item.order > 0 && item.revision > 0)
    );

    for (name, _) in &files {
        let bytes = std::fs::read(bundle.join(name)).unwrap();
        for secret in PLANTED {
            assert!(!holds(&bytes, secret), "{name} still holds {secret}");
        }
    }
    // The core's part is not clean for want of anything to redact: the
    // ring it was taken from holds the prompt's secret.
    let ring_holds = std::fs::read_dir(runtime.dir().join("core-ring"))
        .unwrap()
        .any(|entry| holds(&std::fs::read(entry.unwrap().path()).unwrap(), KEY));
    assert!(ring_holds, "the live ring holds {KEY}");
    transcript.push(format!(
        "planted {} secrets in the prompt, a later input, the model's reply, the provider's \
         environment, the dump reason, the daemon log and the client's session; none is in any \
         file of the bundle, as text or hex",
        PLANTED.len()
    ));
    transcript.push(format!(
        "manifest errors: {}",
        serde_json::to_string(errors).unwrap()
    ));
    transcript.push(format!(
        "daemon.log: {}",
        std::fs::read_to_string(bundle.join(DAEMON_LOG))
            .unwrap()
            .trim()
            .replace('\n', " | ")
    ));
    transcript.push(format!(
        "store slice for {live}: {} rows, first reads {:?}",
        slice.items.len(),
        slice
            .items
            .iter()
            .find(|item| !item.text.is_empty())
            .map(|item| &item.text)
    ));

    let packed = node::pack(&bundle).unwrap();
    assert_eq!(
        packed.files.len(),
        files.len(),
        "one message carries every file"
    );
    assert!(
        packed
            .files
            .iter()
            .any(|file| file.name == format!("agents/{live}/row.pb"))
    );
    transcript.push(format!(
        "packed for a remote client: {} files, {} bytes",
        packed.files.len(),
        packed.encoded_len()
    ));
    for line in &transcript {
        println!("{line}");
    }

    kill_all(&runtime).await;
    drop(runtime);
    daemon.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_dump_of_named_agents_covers_only_them_and_an_unknown_one_is_an_error() {
    let install = Install::new();
    let daemon = install
        .start("boot-1", support::synthetic::quiet_launch())
        .await;
    let runtime: Arc<node::ProfileRuntime> = runtime(&daemon, &install);
    let mut one = support::synthetic::SyntheticAgent::new(&install, "one", 1 << 20);
    let two = support::synthetic::SyntheticAgent::new(&install, "two", 1 << 20);
    one.register(&install, &runtime).await;
    two.register(&install, &runtime).await;
    one.append(&support::synthetic::item("a", "hello"));
    runtime.ingest(one.id).await.unwrap();

    let bundle = runtime
        .dump(DumpRequest {
            agent_ids: vec![one.id.as_bytes().to_vec()],
            reason: String::new(),
            automatic: true,
            sessions: Vec::new(),
        })
        .await
        .unwrap();
    let agents: Vec<String> = std::fs::read_dir(bundle.join("agents"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(agents, vec![one.id.to_string()]);

    let unknown = runtime
        .dump(DumpRequest {
            agent_ids: vec![uuid::Uuid::new_v4().as_bytes().to_vec()],
            ..DumpRequest::default()
        })
        .await
        .unwrap_err();
    assert!(matches!(unknown, node::DumpError::NoAgent(_)), "{unknown}");
    let reports: Vec<PathBuf> = std::fs::read_dir(install.data_dir.join(node::REPORTS))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(reports, vec![bundle], "a failed dump leaves nothing behind");
    drop(runtime);
    daemon.shutdown().await.unwrap();
}

/// A paired host's dump answer carries this host's side of its agents but
/// not the daemon's log: that log covers every profile the installation
/// serves, and the asking machine is trusted by only one of them.
#[tokio::test(flavor = "multi_thread")]
async fn a_peers_dump_answer_carries_no_daemon_log() {
    const OTHER: &str = "a line about another profile's agents";
    let install = Install::new();
    let log = install.path("daemon.log");
    std::fs::write(&log, format!("INFO {OTHER}\n")).unwrap();
    let options = node::StartOptions {
        daemon_log: Some(log),
        ..install.options("boot-1", support::synthetic::quiet_launch())
    };
    let daemon = node::start(options, None).await.unwrap();
    let runtime: Arc<node::ProfileRuntime> = runtime(&daemon, &install);
    let mut one = support::synthetic::SyntheticAgent::new(&install, "one", 1 << 20);
    one.register(&install, &runtime).await;
    one.append(&support::synthetic::item("a", "hello"));
    runtime.ingest(one.id).await.unwrap();

    let own = runtime.dump(DumpRequest::default()).await.unwrap();
    assert!(
        std::fs::read_to_string(own.join(DAEMON_LOG))
            .unwrap()
            .contains(OTHER),
        "a dump taken here carries the log"
    );

    let answer = runtime.dump_for_peer(DumpRequest::default()).await.unwrap();
    let names: Vec<&str> = answer.files.iter().map(|file| file.name.as_str()).collect();
    assert!(
        names.contains(&format!("agents/{}/row.pb", one.id).as_str()),
        "{names:?}"
    );
    assert!(!names.contains(&DAEMON_LOG), "{names:?}");
    for file in &answer.files {
        assert!(
            !contains(&file.contents, OTHER),
            "{} holds the log",
            file.name
        );
    }
    drop(runtime);
    daemon.shutdown().await.unwrap();
}

/// Waits for the installation to hold `count` bundles the daemon core asked
/// for, and returns their reasons.
async fn core_dumps(install: &Install, count: usize) -> Vec<String> {
    until("the bundles the core asked for", || {
        let dumps = install.core_dumps();
        async move {
            match dumps.len() >= count {
                true => Ok(dumps),
                false => Err("too few bundles yet"),
            }
        }
    })
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn steps_over_their_budget_are_one_dump_within_the_interval() {
    // Not told to expect dumps: a step over its budget is tolerated at
    // teardown, only a broken invariant fails it.
    let install = Install::new();
    // Every step that takes any time at all runs over this.
    let launch = node::Launch {
        step_budget: daemon_core::StepBudget { micros: 0 },
        ..support::synthetic::quiet_launch()
    };
    let daemon = install.start("boot-1", launch).await;
    let runtime: Arc<node::ProfileRuntime> = runtime(&daemon, &install);
    let mut agent = support::synthetic::SyntheticAgent::new(&install, "one", 1 << 20);
    agent.register(&install, &runtime).await;
    agent.append(&support::synthetic::item("a", "hello"));
    runtime.ingest(agent.id).await.unwrap();
    let first = core_dumps(&install, 1).await;
    assert!(
        first[0].starts_with(node::CORE_DUMP) && first[0].contains("over its budget of 0 µs"),
        "{first:?}"
    );
    // Many more steps, every one over its budget.
    for n in 0..20 {
        agent.append(&support::synthetic::item(&format!("more-{n}"), "again"));
        runtime.ingest(agent.id).await.unwrap();
    }
    runtime.checkpoint().await.unwrap();
    drop(runtime);
    // A clean shutdown waits for every dump under way.
    daemon.shutdown().await.unwrap();
    assert_eq!(install.core_dumps(), first);
}

#[tokio::test(flavor = "multi_thread")]
async fn steps_over_their_budget_dump_again_after_the_interval_counting_the_rest() {
    let install = Install::new();
    const INTERVAL_MS: u64 = 1_500;
    let launch = node::Launch {
        step_budget: daemon_core::StepBudget { micros: 0 },
        dump_interval_ms: INTERVAL_MS,
        ..support::synthetic::quiet_launch()
    };
    let daemon = install.start("boot-1", launch).await;
    let runtime: Arc<node::ProfileRuntime> = runtime(&daemon, &install);
    let mut agent = support::synthetic::SyntheticAgent::new(&install, "one", 1 << 20);
    agent.register(&install, &runtime).await;
    agent.append(&support::synthetic::item("a", "hello"));
    runtime.ingest(agent.id).await.unwrap();
    core_dumps(&install, 1).await;
    // Steps keep coming, each over its budget, until the interval is past.
    let started = std::time::Instant::now();
    let mut n = 0;
    while install.core_dumps().len() < 2 {
        assert!(
            started.elapsed() < std::time::Duration::from_secs(30),
            "no second bundle after the interval: {:?}",
            install.core_dumps()
        );
        n += 1;
        agent.append(&support::synthetic::item(&format!("more-{n}"), "again"));
        runtime.ingest(agent.id).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let bundles = bundles_by_time(&install);
    assert_eq!(bundles.len(), 2, "{bundles:?}");
    let (first_ms, first) = &bundles[0];
    let (second_ms, second) = &bundles[1];
    assert!(!first.contains("not dumped"), "{first}");
    assert!(
        second.contains("over its budget of 0 µs")
            && second.contains("more like it were not dumped since the last"),
        "{second}"
    );
    // A bundle is stamped as it is written, a moment after the dump was
    // let through.
    assert!(
        second_ms + 500 >= first_ms + INTERVAL_MS,
        "{first_ms} then {second_ms}"
    );
    drop(runtime);
    daemon.shutdown().await.unwrap();
}

/// The bundles the daemon core asked for, oldest first, with when each was
/// made.
fn bundles_by_time(install: &Install) -> Vec<(u64, String)> {
    let mut bundles: Vec<(u64, String)> = std::fs::read_dir(install.data_dir.join(node::REPORTS))
        .unwrap()
        .filter_map(|bundle| {
            let manifest = std::fs::read(bundle.ok()?.path().join(MANIFEST)).ok()?;
            let manifest: serde_json::Value = serde_json::from_slice(&manifest).ok()?;
            let reason = manifest["reason"].as_str()?;
            reason.starts_with(node::CORE_DUMP).then(|| {
                (
                    manifest["created_at_ms"].as_u64().unwrap(),
                    reason.to_owned(),
                )
            })
        })
        .collect();
    bundles.sort();
    bundles
}

#[tokio::test(flavor = "multi_thread")]
async fn a_clean_shutdown_ends_a_dump_still_waiting_for_an_agents_part() {
    let install = Install::new();
    // A live agent that takes the dump request and never answers it: the
    // dump waits on its part, holding the runtime.
    let mut silent = support::synthetic::SyntheticAgent::new(&install, "silent", 1 << 20);
    silent.register_offline(&install);
    silent.go_live();
    let launch = node::Launch {
        step_budget: daemon_core::StepBudget { micros: 0 },
        ..support::synthetic::quiet_launch()
    };
    let daemon = install.start("boot-1", launch).await;
    let runtime: Arc<node::ProfileRuntime> = runtime(&daemon, &install);
    until("the daemon to connect to the agent", || {
        let hellos = silent.hellos();
        async move { (hellos > 0).then_some(()).ok_or("no hello yet") }
    })
    .await
    .unwrap();
    silent.append(&support::synthetic::item("a", "hello"));
    runtime.ingest(silent.id).await.unwrap();
    let reports = install.data_dir.join(node::REPORTS);
    until("a dump under way", || {
        let partial = std::fs::read_dir(&reports)
            .into_iter()
            .flatten()
            .any(|entry| {
                entry.is_ok_and(|entry| entry.file_name().to_string_lossy().starts_with(".dump-"))
            });
        async move { partial.then_some(()).ok_or("no dump started yet") }
    })
    .await
    .unwrap();

    let gone = Arc::downgrade(&runtime);
    drop(runtime);
    let started = std::time::Instant::now();
    daemon.shutdown().await.unwrap();
    let took = started.elapsed();
    assert!(
        took < node::DUMPS_FINISH_WITHIN + std::time::Duration::from_secs(3),
        "the shutdown waited {took:?} for its dumps"
    );
    // Gone at once, not once the agent's part has been waited out.
    patience::until_within(
        "the runtime to be gone",
        std::time::Duration::from_secs(2),
        || {
            let live = gone.upgrade().is_some();
            async move { (!live).then_some(()).ok_or("a reference is still live") }
        },
    )
    .await
    .unwrap();
}

/// A bundle's core part holds the pairing randomness as of its checkpoint,
/// and the PIN of the window open then. A window opened after the dump
/// draws after fresh randomness, so the bundle cannot compute its PIN: a
/// core restored from the bundle's checkpoint and fed the randomness its
/// inputs recorded opens a window with another PIN than the live one.
#[tokio::test(flavor = "multi_thread")]
async fn a_dump_cannot_compute_the_pin_of_a_window_opened_after_it() {
    use daemon_core::pairing::{WindowMethod, WindowSecret};
    use daemon_core::payload::{ClientRequest, Response};
    use daemon_core::{CallId, Checkpoint, Daemon, Input, Output, Recorded, StepBudget};
    use tonic::Request;
    use wire::profile_service_server::ProfileService as _;
    use wire::{Empty, ProfileRequest, StartPairingRequest, start_pairing_request};

    let install = Install::new();
    let daemon = install
        .start("boot-1", support::synthetic::quiet_launch())
        .await;
    let runtime = runtime(&daemon, &install);
    let door = daemon.front_door();
    let profile = install.profile.as_bytes().to_vec();
    let open = || {
        let door = door.clone();
        let profile = profile.clone();
        async move {
            let window = door
                .start_pairing(Request::new(StartPairingRequest {
                    profile_id: profile,
                    method: Some(start_pairing_request::Method::Pin(Empty {})),
                    ttl_seconds: None,
                }))
                .await
                .unwrap()
                .into_inner();
            match window.secret {
                Some(wire::pairing_window::Secret::Pin(pin)) => pin,
                other => panic!("a PIN window, not {other:?}"),
            }
        }
    };

    // Mid-pairing: a window is open, a checkpoint holds it, and the dump
    // carries that checkpoint.
    open().await;
    runtime.checkpoint().await.unwrap();
    let bundle = runtime
        .dump(DumpRequest {
            reason: "mid-pairing".to_owned(),
            ..DumpRequest::default()
        })
        .await
        .unwrap();
    door.cancel_pairing(Request::new(ProfileRequest {
        profile_id: profile.clone(),
    }))
    .await
    .unwrap();
    let second = open().await;

    // What the bundle can compute: its checkpoint, every randomness its
    // inputs recorded, and the window opened next.
    let core = bundle.join("core");
    let checkpoint =
        Checkpoint::from_bytes(&std::fs::read(core.join("checkpoint")).unwrap()).unwrap();
    let store =
        store::Sqlite::open_in_memory(support::synthetic::host(&install).as_bytes().to_vec())
            .unwrap();
    let mut restored = Daemon::restore(checkpoint, store).unwrap();
    restored.set_budget(StepBudget::UNLIMITED);
    let mut out = Vec::new();
    let recorded: Vec<Recorded> = core_ring::inputs(&std::fs::read(core.join("inputs")).unwrap());
    for line in recorded {
        if let Recorded::Input(input) = line
            && let Input::Entropy(_) = *input
        {
            restored.step(*input, &mut out);
        }
    }
    for request in [
        ClientRequest::CancelPairing,
        ClientRequest::StartPairing {
            method: WindowMethod::Pin,
            ttl_ms: 60_000,
        },
    ] {
        restored.step(
            Input::ClientCall {
                call: CallId(u64::MAX),
                request,
            },
            &mut out,
        );
    }
    let computed = out
        .iter()
        .find_map(|output| match output {
            Output::Answer {
                response: Response::PairingWindow(WindowSecret::Pin(pin)),
                ..
            } => Some(pin.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("the restored core opens a window: {out:?}"));
    assert_ne!(
        computed, second,
        "the bundle computes the PIN of a window opened after it"
    );
    drop(runtime);
    daemon.shutdown().await.unwrap();
}

/// An agent's core asking for a dump is a bundle of that agent, written by
/// the daemon: its part from the process and the daemon's own beside it,
/// the reason saying which agent asked. The daemon holds a repeat of the
/// same kind back, as twenty agents breaking one invariant would send it.
#[tokio::test(flavor = "multi_thread")]
async fn an_agents_core_asking_for_a_dump_leaves_a_bundle_of_that_agent() {
    const INVARIANT: &str = "one open ask at most";
    let install = Install::new();
    // This test makes an agent's core ask on purpose.
    install.expect_core_dumps();
    let mut asking = support::synthetic::SyntheticAgent::new(&install, "asking", 1 << 20);
    let other = support::synthetic::SyntheticAgent::new(&install, "other", 1 << 20);
    asking.register_offline(&install);
    other.register_offline(&install);
    asking.go_live();
    let daemon = install
        .start("boot-1", support::synthetic::quiet_launch())
        .await;
    let runtime: Arc<node::ProfileRuntime> = runtime(&daemon, &install);

    let violation = format!("the agent core: {INVARIANT}: two asks open");
    asking.ask_for_dump(&violation, INVARIANT).await;
    asking.ask_for_dump(&violation, INVARIANT).await;
    asking
        .ask_for_dump(
            "the agent core: a step took 12 µs against its budget of 0 µs",
            "",
        )
        .await;
    let reasons = until("both kinds' bundles", || async {
        let reasons = install.core_dumps();
        (reasons.len() >= 2)
            .then_some(reasons)
            .ok_or("fewer than two bundles")
    })
    .await
    .unwrap();
    let id = asking.id.to_string();
    let violations: Vec<&String> = reasons
        .iter()
        .filter(|reason| reason.starts_with(node::AGENT_VIOLATION_DUMP))
        .collect();
    assert_eq!(violations.len(), 1, "{reasons:#?}");
    assert!(
        violations[0].contains(&id) && violations[0].ends_with(&violation),
        "{reasons:#?}"
    );
    assert!(
        reasons
            .iter()
            .any(|reason| reason.starts_with(node::AGENT_DUMP)
                && !reason.starts_with(node::AGENT_VIOLATION_DUMP)
                && reason.contains("its budget of 0 µs")),
        "{reasons:#?}"
    );

    for bundle in std::fs::read_dir(install.data_dir.join(node::REPORTS)).unwrap() {
        let bundle = bundle.unwrap().path();
        let agents: Vec<String> = std::fs::read_dir(bundle.join("agents"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(agents, vec![id.clone()], "only the agent that asked");
        let part = bundle
            .join("agents")
            .join(&id)
            .join("part")
            .join(support::synthetic::SYNTHETIC_PART);
        assert!(part.is_file(), "the agent's own part: {}", part.display());
        assert!(
            bundle.join("core/checkpoint").is_file(),
            "the daemon's parts"
        );
        println!(
            "{}: {}",
            bundle.file_name().unwrap().to_string_lossy(),
            serde_json::from_slice::<serde_json::Value>(
                &std::fs::read(bundle.join(MANIFEST)).unwrap()
            )
            .unwrap()["reason"]
        );
    }
    // The repeat stays held back.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(install.core_dumps().len(), 2);
    drop(other);
    drop(runtime);
    daemon.shutdown().await.unwrap();
}
