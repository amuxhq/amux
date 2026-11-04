//! Dump assembly: one bundle under reports/ with this host's records (the
//! daemon core's, each own agent's and the chats a client shows) and our
//! own logs, in the documented layout, the same whatever asked for it, with
//! every planted secret and word absent. Real agent processes on the fake
//! provider build their own records.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use daemon_core::Filing;
use node::{DAEMON_LOG, MANIFEST};
use prost::Message as _;
use support::*;
use wire::{DumpRequest, Lifecycle, Phase};

use crate::support;

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
async fn a_dump_carries_this_hosts_records_and_our_logs_and_no_secret() {
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
    // Stamped as the processes' formatter stamps lines: one from long
    // before the dump, which its window leaves out, one below the default
    // level, and the minute before it.
    let stamped = |ago_ms: i64| {
        let at = chrono::DateTime::from_timestamp_millis(log_file::unix_ms_now() - ago_ms).unwrap();
        at.format("%Y-%m-%dT%H:%M:%S%.6fZ").to_string()
    };
    let log = install.path("daemon.log");
    std::fs::write(
        &log,
        format!(
            "{}  INFO an hour ago\n{}  INFO started\n{} DEBUG wordy\n{}  WARN a relay call failed: \
             Authorization: Bearer {KEY}\n",
            stamped(3_600_000),
            stamped(1_000),
            stamped(1_000),
            stamped(1_000),
        ),
    )
    .unwrap();
    let supervisor_log = install.path("supervisor.log");
    std::fs::write(
        &supervisor_log,
        format!(
            "{}  INFO restarting the daemon with GITHUB_TOKEN={TOKEN}\n",
            stamped(1_000)
        ),
    )
    .unwrap();
    let options = node::StartOptions {
        daemon_log: Some(log),
        supervisor_log: Some(supervisor_log),
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
    runtime.file(stopped, Filing::Settled).await.unwrap();
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

    // The asking client's chats: one handed in whole, as no client sends
    // it, which the dump refuses, and one as its record.
    let whole = wire::dump::ClientSessionHistory {
        agent_id: live.as_bytes().to_vec(),
        start: Some(wire::dump::ClientSession {
            agent: Some(runtime.agent(live).await.unwrap()),
            ..wire::dump::ClientSession::default()
        }),
        structure_only: false,
        ..wire::dump::ClientSessionHistory::default()
    };
    let record = wire::dump::ClientSessionHistory {
        agent_id: live.as_bytes().to_vec(),
        events: Some(wire::dump::ClientSessionEvents {
            traced: vec![wire::dump::ClientTraced {
                seq: 1,
                at_ms: 0,
                size: 0,
                event: Some(wire::dump::client_traced::Event::Driver(
                    "Subscribed { tail: 16 }".to_owned(),
                )),
            }],
        }),
        structure_only: true,
        ..wire::dump::ClientSessionHistory::default()
    };
    let bundle = runtime
        .dump(DumpRequest {
            reason: format!("the chat froze; my key is {KEY}"),
            sessions: vec![whole.encode_to_vec(), record.encode_to_vec()],
            ..DumpRequest::default()
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
    has(node::SUPERVISOR_LOG.to_owned());
    // Each agent's own log goes beside its record; never its provider's.
    for id in [live, stopped] {
        has(format!("agents/{id}/{}", node::AGENT_LOG));
        assert!(
            !names.iter().any(|name| name.ends_with("provider.log")),
            "{names:?}"
        );
        assert!(
            names
                .iter()
                .any(|name| name.starts_with(&format!("core/{}/{id}/", node::CORE_JOURNALS))),
            "{id}'s journal from the core's cursor"
        );
    }
    has(format!("agents/{live}/{}", node::AGENT_RECORD));
    // Nothing that holds what was said: no row, slice, facts or spec.
    assert!(
        names.iter().all(|name| !name.ends_with(".pb")
            && !name.contains("facts")
            && !name.contains("spec.")),
        "{names:?}"
    );
    // Our logs reach back a minute, at the default level.
    let daemon_log = std::fs::read_to_string(bundle.join(DAEMON_LOG)).unwrap();
    assert!(daemon_log.contains("INFO started"), "{daemon_log}");
    assert!(!daemon_log.contains("an hour ago"), "{daemon_log}");
    assert!(!daemon_log.contains("wordy"), "{daemon_log}");
    // The core's record: its checkpoint, the inputs since and the store as
    // of that checkpoint, the store alone, with no WAL or rollback journal
    // beside it.
    for name in ["checkpoint", "inputs", "store.sqlite"] {
        has(format!("core/{name}"));
    }
    assert!(
        !names
            .iter()
            .any(|name| name.starts_with("core/store.sqlite-")),
        "{names:?}"
    );
    let checkpoint = std::fs::read(bundle.join("core/checkpoint")).unwrap();
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
    let restored = daemon_core::Daemon::restore(&checkpoint, copy, scratch.path().join("agents"));
    assert!(restored.is_ok(), "the core restores from its record");

    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(bundle.join(MANIFEST)).unwrap()).unwrap();
    let errors = manifest["errors"].as_array().unwrap();
    assert_eq!(manifest["trigger"], node::PERSON);
    assert_eq!(manifest["core"]["reads"].as_u64(), Some(reads as u64));
    assert!(
        errors.iter().any(|error| error
            .as_str()
            .unwrap()
            .starts_with(&format!("{stopped} record"))),
        "the stopped agent's missing record is named: {errors:?}"
    );
    assert_eq!(manifest["agents"].as_array().unwrap().len(), 2);
    // The chat's record, beside the whole session, which is named instead.
    let client = format!("{}/{live}", node::CLIENT_SESSIONS);
    has(format!("{client}/{}", node::CLIENT_CHECKPOINT));
    has(format!("{client}/{}", node::CLIENT_EVENTS));
    let sessions = manifest["client"]["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1, "{sessions:?}");
    assert!(
        errors
            .iter()
            .any(|error| error.as_str().unwrap().contains("a whole history")),
        "{errors:?}"
    );

    for (name, _) in &files {
        let bytes = std::fs::read(bundle.join(name)).unwrap();
        for secret in PLANTED {
            assert!(!holds(&bytes, secret), "{name} still holds {secret}");
        }
    }
    // The core's record is not clean for want of anything to redact: the
    // ring it was taken from holds the prompt's secret.
    let ring_holds = std::fs::read_dir(runtime.dir().join("core-ring"))
        .unwrap()
        .any(|entry| holds(&std::fs::read(entry.unwrap().path()).unwrap(), KEY));
    assert!(ring_holds, "the live ring holds {KEY}");
    transcript.push(format!(
        "planted {} secrets in the prompt, a later input, the model's reply, the provider's \
         environment, the dump reason and the daemon and supervisor logs; none is in any file \
         of the bundle, as text or hex",
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

    let packed = node::pack(&bundle).unwrap();
    assert_eq!(
        packed.files.len(),
        files.len(),
        "one message carries every file"
    );
    transcript.push(format!(
        "packed to send: {} files, {} bytes",
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

/// Our logs carry no conversation: a phrase in a prompt, the model's reply
/// and a later input reaches neither what the daemon logs, at its default
/// level, nor the agent's own log.
#[tokio::test(flavor = "multi_thread")]
async fn a_conversation_reaches_no_log_amux_writes() {
    const MARKER: &str = "PLANTEDmarkerphrase0004";
    let log = support::captured_log();
    let install = Install::new();
    let launch = install.launch(
        "quiet",
        vec![
            text(&format!("the answer is {MARKER}")),
            provider_fakes::script::Step::TurnEnd,
        ],
    );
    let daemon = install.start("boot-1", launch).await;
    let runtime = runtime(&daemon, &install);
    let prompt = format!("please say {MARKER}");
    let agent = id_of(
        &runtime
            .spawn(create(&install.work, "talker", Some(&prompt)), None)
            .await
            .unwrap(),
    );
    until("the first turn to end", || async {
        let phase = runtime.agent(agent).await.unwrap().phase;
        (phase == Phase::Idle as i32)
            .then_some(())
            .ok_or_else(|| format!("phase {phase}"))
    })
    .await
    .unwrap();
    runtime
        .send_input(&wire::SendInputRequest {
            agent_id: agent.as_bytes().to_vec(),
            input: Some(sdk_prompt(b"later", &format!("and {MARKER} again"))),
        })
        .await
        .unwrap();
    let key = store::AgentKey::new(
        runtime.host().as_bytes().to_vec(),
        agent.as_bytes().to_vec(),
    );
    until("the later input's reflection", || async {
        let key = key.clone();
        runtime
            .read_store(move |store| store.item_by_input(&key, b"later").unwrap())
            .await
            .unwrap()
            .map(|_| ())
            .ok_or("no row carries it")
    })
    .await
    .unwrap();
    let agent_log = std::fs::read(install.agent_dir(agent).join(node::AGENT_LOG)).unwrap();
    kill_all(&runtime).await;
    drop(runtime);
    daemon.shutdown().await.unwrap();

    let daemon_log = log.lock().unwrap().clone();
    assert!(!daemon_log.is_empty(), "the daemon logged something");
    for (name, bytes) in [("the daemon's log", &daemon_log), ("agent.log", &agent_log)] {
        assert!(
            !contains(bytes, MARKER),
            "{name} holds the conversation: {}",
            String::from_utf8_lossy(bytes)
        );
    }
}

/// Runs git in `cwd` with a fixed identity.
fn git(cwd: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .output()
        .expect("git runs");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A report keeps what a session did and none of what was said: a phrase
/// in an agent's folder, its branch, a file's name, its prompts, a tool's
/// input and output and the replies reaches no file of any bundle,
/// whatever asked for it, while the agent's record, its facts run again
/// through the interpreter when the dump was made, holds the session's
/// shape and both turns' ends.
#[tokio::test(flavor = "multi_thread")]
async fn no_report_carries_a_word_and_an_agents_record_holds_what_its_interpreter_concluded() {
    const MARKER: &str = "PLANTEDmarkerphrase0006";
    let install = Install::new();
    let repo = install.work.join(format!("{MARKER}-folder"));
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", &format!("{MARKER}-branch")]);
    std::fs::write(repo.join("README"), "first\n").unwrap();
    git(&repo, &["add", "README"]);
    git(&repo, &["commit", "-q", "-m", MARKER]);
    let file = repo.join(format!("{MARKER}.txt"));
    std::fs::write(&file, format!("{MARKER}\n")).unwrap();
    std::fs::write(repo.join("README"), format!("first\n{MARKER}\n")).unwrap();
    let launch = install.launch(
        "recorded",
        vec![
            text(&format!("reading {MARKER}")),
            provider_fakes::script::Step::Tool(provider_fakes::script::Tool {
                name: Some("Read".into()),
                class: provider_fakes::script::ToolClass::Exploration,
                input: Some(serde_json::json!({ "file_path": file })),
                outcome: provider_fakes::script::Outcome {
                    output: format!("the file says {MARKER}"),
                    ..Default::default()
                },
                wait_for: None,
            }),
            text(&format!("it said {MARKER}")),
            provider_fakes::script::Step::TurnEnd,
        ],
    );
    let daemon = install.start("boot-1", launch).await;
    let runtime = runtime(&daemon, &install);
    let agent = id_of(
        &runtime
            .spawn(
                create(&repo, "reader", Some(&format!("read {MARKER}"))),
                None,
            )
            .await
            .unwrap(),
    );
    until("the first turn to end", || async {
        let phase = runtime.agent(agent).await.unwrap().phase;
        (phase == Phase::Idle as i32)
            .then_some(())
            .ok_or_else(|| format!("phase {phase}"))
    })
    .await
    .unwrap();
    runtime
        .send_input(&wire::SendInputRequest {
            agent_id: agent.as_bytes().to_vec(),
            input: Some(sdk_prompt(b"later", &format!("and {MARKER} again"))),
        })
        .await
        .unwrap();
    let key = store::AgentKey::new(
        runtime.host().as_bytes().to_vec(),
        agent.as_bytes().to_vec(),
    );
    until("the later input's reflection", || async {
        let key = key.clone();
        runtime
            .read_store(move |store| store.item_by_input(&key, b"later").unwrap())
            .await
            .unwrap()
            .map(|_| ())
            .ok_or("no row carries it")
    })
    .await
    .unwrap();
    until("the folder's git facts", || async {
        let row = runtime.agent(agent).await.unwrap();
        row.git
            .as_ref()
            .and_then(|git| git.uncommitted)
            .map(|_| ())
            .ok_or("no change totals on the row")
    })
    .await
    .unwrap();

    // Whatever asks, a person or a check on any of the cores, the bundle
    // carries the same parts.
    let mut bundles = Vec::new();
    for (automatic, check) in [
        (false, ""),
        (true, "daemon.test"),
        (true, "agent.test"),
        (true, "transcript.window"),
    ] {
        let bundle = runtime
            .dump(DumpRequest {
                reason: format!("asked by {check}"),
                automatic,
                check: check.to_owned(),
                ..DumpRequest::default()
            })
            .await
            .unwrap();
        bundles.push(bundle);
    }
    kill_all(&runtime).await;
    drop(runtime);
    daemon.shutdown().await.unwrap();

    let dir = format!("agents/{agent}");
    let parts =
        |bundle: &Path| -> Vec<String> { tree(bundle).into_iter().map(|(name, _)| name).collect() };
    for bundle in &bundles {
        let files = tree(bundle);
        assert!(
            files
                .iter()
                .any(|(name, _)| *name == format!("{dir}/record")),
            "{files:?}"
        );
        assert_eq!(parts(bundle), parts(&bundles[0]), "the same parts");
        for (name, _) in &files {
            let bytes = std::fs::read(bundle.join(name)).unwrap();
            for planted in [
                MARKER.to_owned(),
                MARKER.to_uppercase(),
                repo.to_string_lossy().into_owned(),
            ] {
                assert!(
                    !holds(&bytes, &planted),
                    "{name} holds {planted}: {}",
                    String::from_utf8_lossy(&bytes)
                );
            }
        }
    }
    let bundle = &bundles[0];

    let bytes = std::fs::read(bundle.join(&dir).join(node::AGENT_RECORD)).unwrap();
    let record: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(record["settings"]["kind"], "KIND_CLAUDE_SDK");
    assert_eq!(record["core"]["mode"], "running", "{}", record["core"]);
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let events: Vec<&serde_json::Value> = record["segments"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|segment| segment["events"].as_array().unwrap())
        .collect();
    // What the session did, by kind: the tool and its input's keys, both
    // prompts, the replies.
    let read = serde_json::json!({"name": "Read", "keys": ["file_path"]});
    assert!(
        events.iter().any(|fed| fed["event"]["tools"]
            .as_array()
            .is_some_and(|tools| tools.contains(&read))),
        "{text}"
    );
    for shape in [r#""arm":"prompt""#, r#""variant":"assistant""#] {
        assert!(text.contains(shape), "no {shape} in {text}");
    }
    // The folder's changes as counts, the branch only as being there.
    assert!(
        events.iter().any(|fed| fed["event"]["event"] == "git"
            && fed["event"]["git"]["branch"] == true
            && fed["event"]["git"]["uncommitted"][0].as_u64() >= Some(2)),
        "{text}"
    );
    // Both turns ended in the replay as they did live, and no check broke.
    let turn_ends = events
        .iter()
        .filter(|fed| !fed["step"]["turn_end"].is_null())
        .count();
    assert_eq!(turn_ends, 2, "{text}");
    assert!(events.iter().all(|fed| fed["broken"].is_null()), "{text}");
}

/// A client's chats go into a dump as their records, whatever asked for
/// it; a whole session, which holds its chat's conversation, never does.
#[tokio::test(flavor = "multi_thread")]
async fn a_dump_carries_a_chats_record_and_never_its_whole_session() {
    let install = Install::new();
    let daemon = install
        .start("boot-1", support::synthetic::quiet_launch())
        .await;
    let runtime: Arc<node::ProfileRuntime> = runtime(&daemon, &install);
    let mut one = support::synthetic::SyntheticAgent::new(&install, "one", 1 << 20);
    one.register(&install);
    one.append(&support::synthetic::item("a", "hello"));
    runtime.ingest(one.id).await.unwrap();
    runtime.checkpoint().await.unwrap();

    let open_chat = wire::dump::ClientSessionHistory {
        agent_id: one.id.as_bytes().to_vec(),
        ..wire::dump::ClientSessionHistory::default()
    };
    let record = wire::dump::ClientSessionHistory {
        structure_only: true,
        ..open_chat.clone()
    };
    let sessions_dir = format!("{}/{}/", node::CLIENT_SESSIONS, one.id);
    for (automatic, check) in [(false, ""), (true, "transcript.window")] {
        for (session, carried) in [(&record, true), (&open_chat, false)] {
            let bundle = runtime
                .dump(DumpRequest {
                    automatic,
                    check: check.to_owned(),
                    sessions: vec![session.encode_to_vec()],
                    ..DumpRequest::default()
                })
                .await
                .unwrap();
            let files: Vec<String> = tree(&bundle).into_iter().map(|(name, _)| name).collect();
            assert_eq!(
                files.iter().any(|file| file.starts_with(&sessions_dir)),
                carried,
                "{check:?}: {files:?}"
            );
            assert!(
                files.iter().any(|file| file == "core/checkpoint"),
                "{files:?}"
            );
        }
    }

    kill_all(&runtime).await;
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
    agent.register(&install);
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
    agent.register(&install);
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
    // A live agent that never reads the dump request, so never answers it:
    // the dump waits on its part, holding the runtime.
    let mut silent = support::synthetic::SyntheticAgent::new(&install, "silent", 1 << 20);
    silent.register(&install);
    silent.go_live();
    let daemon = install
        .start("boot-1", support::synthetic::quiet_launch())
        .await;
    let runtime: Arc<node::ProfileRuntime> = runtime(&daemon, &install);
    until("the daemon to connect to the agent", || {
        let hellos = silent.hellos();
        async move { (hellos > 0).then_some(()).ok_or("no hello yet") }
    })
    .await
    .unwrap();
    silent.stop_reading();
    // The agent asks once it is connected, so the dump asks it for its part:
    // one started before the connection would skip the part and finish at
    // once.
    // Without a record, as from an agent whose record could not be built.
    silent.ask_for_dump("a step ran long", "", &[]).await;
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
    use daemon_core::{CallId, Daemon, Input, Output, Recorded, StepBudget};
    use tonic::Request;
    use wire::profile_admin_service_server::ProfileAdminService as _;
    use wire::{Empty, StartPairingRequest, start_pairing_request};

    let install = Install::new();
    let daemon = install
        .start("boot-1", support::synthetic::quiet_launch())
        .await;
    let runtime = runtime(&daemon, &install);
    let admin = daemon.profile_admin(install.profile);
    let open = || {
        let admin = admin.clone();
        async move {
            let window = admin
                .start_pairing(Request::new(StartPairingRequest {
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
    admin
        .cancel_pairing(Request::new(wire::Empty {}))
        .await
        .unwrap();
    let second = open().await;

    // What the bundle can compute: its checkpoint, every randomness its
    // inputs recorded, and the window opened next.
    let core = bundle.join("core");
    let checkpoint = std::fs::read(core.join("checkpoint")).unwrap();
    let store =
        store::Sqlite::open_in_memory(support::synthetic::host(&install).as_bytes().to_vec())
            .unwrap();
    let mut restored = Daemon::restore(&checkpoint, store, Default::default()).unwrap();
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

/// An agent's core asking for a dump is a bundle written by the daemon
/// with the record the agent sent, the reason saying which agent asked. The daemon holds a repeat of the
/// same kind back, as twenty agents breaking one invariant would send it.
#[tokio::test(flavor = "multi_thread")]
async fn an_agents_core_asking_for_a_dump_leaves_a_bundle_of_that_agent() {
    const INVARIANT: &str = "one open ask at most";
    let install = Install::new();
    // This test makes an agent's core ask on purpose.
    install.expect_core_dumps();
    let mut asking = support::synthetic::SyntheticAgent::new(&install, "asking", 1 << 20);
    let other = support::synthetic::SyntheticAgent::new(&install, "other", 1 << 20);
    asking.register(&install);
    other.register(&install);
    asking.go_live();
    // The bundles here are the agent's alone: the daemon's core never
    // dumps for a slow step of its own.
    let daemon = install
        .start("boot-1", support::synthetic::quiet_launch())
        .await;
    let runtime: Arc<node::ProfileRuntime> = runtime(&daemon, &install);

    let violation = format!("the agent core: {INVARIANT}: two asks open");
    asking
        .ask_for_dump(&violation, INVARIANT, support::synthetic::SYNTHETIC_RECORD)
        .await;
    asking
        .ask_for_dump(&violation, INVARIANT, support::synthetic::SYNTHETIC_RECORD)
        .await;
    asking
        .ask_for_dump(
            "the agent core: a step took 12 µs, over its budget of 0 µs",
            "",
            support::synthetic::SYNTHETIC_RECORD,
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

    // Each names its check, a step over budget by its core's id for one.
    let mut checks = Vec::new();
    for bundle in std::fs::read_dir(install.data_dir.join(node::REPORTS)).unwrap() {
        let bundle = bundle.unwrap().path();
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(bundle.join(MANIFEST)).unwrap()).unwrap();
        checks.push(manifest["trigger"].as_str().unwrap_or_default().to_owned());
        // The record the agent's core sent, beside every other part a
        // dump carries.
        let agent = bundle.join("agents").join(&id);
        assert_eq!(
            std::fs::read(agent.join(node::AGENT_RECORD)).unwrap(),
            support::synthetic::SYNTHETIC_RECORD
        );
        assert!(bundle.join("core").join("checkpoint").is_file());
        println!(
            "{}: {}",
            bundle.file_name().unwrap().to_string_lossy(),
            serde_json::from_slice::<serde_json::Value>(
                &std::fs::read(bundle.join(MANIFEST)).unwrap()
            )
            .unwrap()["reason"]
        );
    }
    checks.sort();
    assert_eq!(checks, ["agent.over_budget", "agent.test"]);
    // A window: a dump held back leaves no mark to wait on.
    holds_for(
        "the repeat to stay held back",
        std::time::Duration::from_millis(300),
        || async { install.core_dumps().len() == 2 },
    )
    .await
    .unwrap();
    drop(other);
    drop(runtime);
    daemon.shutdown().await.unwrap();
}

/// A report is held to one limit: the record of what asked goes in first,
/// and one that leaves no room puts the core's record out, counted.
#[tokio::test(flavor = "multi_thread")]
async fn the_asking_agents_record_goes_first_and_what_finds_no_room_is_counted() {
    let install = Install::new();
    install.expect_core_dumps();
    let mut asking = support::synthetic::SyntheticAgent::new(&install, "asking", 1 << 20);
    asking.register(&install);
    asking.go_live();
    let daemon = install
        .start("boot-1", support::synthetic::quiet_launch())
        .await;
    let runtime: Arc<node::ProfileRuntime> = runtime(&daemon, &install);

    // Bytes that do not compress, nearly the whole limit of them.
    let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
    let record: Vec<u8> = (0..node::REPORT_LIMIT - (1 << 10))
        .map(|_| {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (seed >> 56) as u8
        })
        .collect();
    asking
        .ask_for_dump(
            "the agent core: one open ask at most",
            "one open ask at most",
            &record,
        )
        .await;
    until("the bundle", || async {
        (!install.core_dumps().is_empty())
            .then_some(())
            .ok_or("no bundle")
    })
    .await
    .unwrap();
    let bundle = std::fs::read_dir(install.data_dir.join(node::REPORTS))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.join(MANIFEST).is_file())
        .unwrap();
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(bundle.join(MANIFEST)).unwrap()).unwrap();
    let agent = bundle.join("agents").join(asking.id.to_string());
    assert_eq!(
        std::fs::read(agent.join(node::AGENT_RECORD)).unwrap(),
        record
    );
    assert!(
        !bundle.join("core").exists(),
        "the core's record found no room"
    );
    let over = &manifest["core"]["over_limit"];
    assert!(
        over["bytes"].as_u64().unwrap() > over["room"].as_u64().unwrap(),
        "{manifest:#}"
    );
    assert_eq!(manifest["left_out"], 1, "{manifest:#}");
    assert_eq!(manifest["limit"], node::REPORT_LIMIT, "{manifest:#}");

    drop(runtime);
    daemon.shutdown().await.unwrap();
}

/// An agent's core times its steps against the budget its spec carries: a
/// real agent process on the fake provider, under a budget of one
/// microsecond (zero is the agent's own), soon takes a step over it, and
/// the daemon writes the bundle the agent asked for, of that agent.
#[tokio::test(flavor = "multi_thread")]
async fn an_agents_step_over_its_budget_leaves_a_bundle_of_that_agent() {
    // Not told to expect dumps: a step over its budget is tolerated at
    // teardown, only a broken invariant fails it.
    let install = Install::new();
    // The one bundle is the agent's: the daemon's core, untimed as in
    // every test installation, never dumps for a slow step of its own.
    let launch = node::Launch {
        agent_step_budget_micros: 1,
        ..install.launch(
            "slow",
            vec![text("working on it"), provider_fakes::script::Step::TurnEnd],
        )
    };
    let daemon = install.start("boot-1", launch).await;
    let runtime = runtime(&daemon, &install);
    let id = id_of(
        &runtime
            .spawn(create(&install.work, "slow", Some("go")), None)
            .await
            .unwrap(),
    );
    let reasons = core_dumps(&install, 1).await;
    let id_text = id.to_string();
    assert!(
        reasons
            .iter()
            .any(|reason| reason.starts_with(node::AGENT_DUMP)
                && !reason.starts_with(node::AGENT_VIOLATION_DUMP)
                && reason.contains(&id_text)
                && reason.contains("over its budget of 1 µs")),
        "{reasons:#?}"
    );
    let bundles: Vec<PathBuf> = std::fs::read_dir(install.data_dir.join(node::REPORTS))
        .unwrap()
        .map(|bundle| bundle.unwrap().path())
        .collect();
    assert_eq!(bundles.len(), 1, "{bundles:?}");
    let agents: Vec<String> = std::fs::read_dir(bundles[0].join("agents"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(agents, vec![id_text.clone()], "the agent that asked");
    // The record the agent sent with its ask, built from its facts ring
    // when it asked.
    let record = std::fs::read(
        bundles[0]
            .join("agents")
            .join(&id_text)
            .join(node::AGENT_RECORD),
    )
    .unwrap();
    let record: serde_json::Value = serde_json::from_slice(&record).unwrap();
    assert!(
        !record["segments"].as_array().unwrap().is_empty(),
        "{record}"
    );
    println!(
        "{}: {}",
        bundles[0].file_name().unwrap().to_string_lossy(),
        reasons[0]
    );
    kill_all(&runtime).await;
    drop(runtime);
    daemon.shutdown().await.unwrap();
}
