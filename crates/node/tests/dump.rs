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

    let bundle = runtime
        .dump(DumpRequest {
            agent_ids: Vec::new(),
            reason: format!("the chat froze; my key is {KEY}"),
            automatic: false,
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
    assert!(
        errors
            .iter()
            .any(|error| error.as_str().unwrap().starts_with(&stopped.to_string())),
        "the stopped agent's missing ring is named: {errors:?}"
    );
    assert_eq!(manifest["agents"].as_array().unwrap().len(), 2);

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
    transcript.push(format!(
        "planted {} secrets in the prompt, a later input, the model's reply, the provider's \
         environment, the dump reason and the daemon log; none is in any file of the bundle, \
         as text or hex",
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

#[tokio::test(flavor = "multi_thread")]
async fn a_dump_the_daemon_core_asks_for_is_written_with_its_reason() {
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

    let reasons = until("a bundle the core asked for", || {
        let dumps = install.core_dumps();
        async move {
            match dumps.is_empty() {
                true => Err("no bundle yet"),
                false => Ok(dumps),
            }
        }
    })
    .await
    .unwrap();
    for reason in &reasons {
        assert!(
            reason.starts_with(node::CORE_DUMP) && reason.contains("over its budget of 0 µs"),
            "{reason}"
        );
    }
    drop(runtime);
    daemon.shutdown().await.unwrap();
}
