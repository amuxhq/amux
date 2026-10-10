//! The daemon's agent registry, generation file and startup path, against
//! real `amux agent` processes on the scripted fake providers.

mod support;

use std::time::Duration;

use node::{ActivationError, ActivationPipe, Generation, LockError, RegistryError, StartError};
use prost::Message as _;
use store::{AgentKey, AgentRow, Marker};
use support::*;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use wire::{AgentSpec, ExitCause, Lifecycle, Phase, StopMode};

fn spec(dir: &std::path::Path, n: u32) -> AgentSpec {
    let bytes = std::fs::read(dir.join(format!("spec.{n}"))).expect("the spec exists");
    AgentSpec::decode(bytes.as_slice()).unwrap()
}

fn key(runtime: &node::ProfileRuntime, id: uuid::Uuid) -> AgentKey {
    AgentKey::new(runtime.host().as_bytes().to_vec(), id.as_bytes().to_vec())
}

/// Where the agent's journal ends: the offset past its last whole frame.
fn journal_end(dir: &std::path::Path) -> u64 {
    let mut reader = journal::Reader::new(dir.join(agent_dir::JOURNAL), 0);
    reader.read_to_end().unwrap();
    reader.cursor()
}

/// The agent's row says a turn is running, or what it says instead.
async fn working(runtime: &node::ProfileRuntime, id: uuid::Uuid) -> Result<(), String> {
    let phase = runtime.agent(id).await.unwrap().phase;
    (phase == Phase::Working as i32)
        .then_some(())
        .ok_or_else(|| format!("phase {phase}"))
}

/// A profile store written before the release schema stops the daemon:
/// the error names the file and says why, and the file stays as it was
/// for the person to move aside.
#[tokio::test(flavor = "multi_thread")]
async fn a_store_from_before_the_release_schema_stops_the_daemon_and_is_kept() {
    let install = Install::new();
    let host = node::host_id(&install.profile_dir()).unwrap();
    let path = install.profile_dir().join(node::STORE);
    {
        // The history a store written before the release recorded: seven
        // migrations, the first of them not this build's.
        let store = store::Sqlite::open(&path, host.as_bytes().to_vec()).unwrap();
        let conn = store.connection();
        conn.execute(
            "UPDATE schema_migrations SET sha256 = \
             'c12ecbd4ad0f378ef242319c0f137863c19e1dc5ef5a7310deeb951276b15a04' WHERE id = 1",
            [],
        )
        .unwrap();
        for id in 2..=7 {
            conn.execute(
                "INSERT INTO schema_migrations (id, sha256) VALUES (?1, 'pre-release')",
                [id],
            )
            .unwrap();
        }
        conn.pragma_update(None, "user_version", 7).unwrap();
    }
    let before = std::fs::read(&path).unwrap();

    let started = node::start(
        install.options("boot-1", install.launch("idle", vec![])),
        None,
    )
    .await;
    let Err(error) = started else {
        panic!("a daemon started on a store from before the release schema");
    };
    assert!(
        matches!(
            error,
            StartError::Store {
                error: store::OpenError::EditedMigration { id: 1, .. },
                ..
            }
        ),
        "{error}"
    );
    let message = error.to_string();
    assert!(message.contains(&path.display().to_string()), "{message}");
    assert!(
        message.contains("predates this release's store schema"),
        "{message}"
    );
    assert_eq!(std::fs::read(&path).unwrap(), before, "the store is kept");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_installation_lock_admits_one_daemon() {
    let install = Install::new();
    let first = install
        .start("boot-1", install.launch("idle", vec![]))
        .await;
    let second = node::start(
        install.options("boot-1", install.launch("idle", vec![])),
        None,
    )
    .await;
    assert!(
        matches!(second, Err(StartError::Lock(LockError::Busy(_)))),
        "a second daemon on the same data directory is refused"
    );
    first.shutdown().await.unwrap();
    let third = install
        .start("boot-1", install.launch("idle", vec![]))
        .await;
    third.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn the_generation_bumps_only_after_an_unclean_reboot() {
    let install = Install::new();
    let launch = || install.launch("idle", vec![]);
    let on_disk = || Generation::read(&install.data_dir).unwrap().unwrap();

    let daemon = install.start("boot-1", launch()).await;
    assert_eq!(
        daemon.generation().counter,
        1,
        "the first start is generation 1"
    );
    assert!(
        !on_disk().clean,
        "the flag is cleared before anything is served"
    );
    daemon.shutdown().await.unwrap();
    assert!(on_disk().clean, "a clean shutdown sets the flag last");

    // A clean reboot: new boot id, flag set. Nothing was lost.
    let daemon = install.start("boot-2", launch()).await;
    assert_eq!(
        daemon.generation().counter,
        1,
        "a clean reboot keeps the generation"
    );
    drop(daemon);

    // A daemon crash: same boot id, flag clear. The page cache survived.
    let daemon = install.start("boot-2", launch()).await;
    assert_eq!(
        daemon.generation().counter,
        1,
        "a daemon crash keeps the generation"
    );
    drop(daemon);

    // Power loss: the machine came back with a new boot id and the last
    // run never shut down cleanly.
    let daemon = install.start("boot-3", launch()).await;
    assert_eq!(
        daemon.generation().counter,
        2,
        "an unclean reboot bumps the generation"
    );
    assert_eq!(on_disk().boot_id, "boot-3");
    daemon.shutdown().await.unwrap();

    let leftovers: Vec<_> = std::fs::read_dir(&install.data_dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".tmp"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "no temporary file is left: {leftovers:?}"
    );
    println!(
        "generation file after an unclean reboot and a clean shutdown: {}",
        std::fs::read_to_string(install.data_dir.join(node::GENERATION)).unwrap()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn startup_prepares_after_looking_and_writes_only_after_go() {
    let install = Install::new();
    let host = node::host_id(&install.profile_dir()).unwrap();
    let store_path = install.profile_dir().join(node::STORE);

    // An agent that ran while no daemon was up: a live row, a directory with
    // a journal nobody ingested, and no process holding its lock.
    let away = uuid::Uuid::new_v4();
    // A row whose directory is gone, and a directory no row lists.
    let missing = uuid::Uuid::new_v4();
    let orphan = uuid::Uuid::new_v4();
    {
        let mut store = store::Sqlite::open(&store_path, host.as_bytes().to_vec()).unwrap();
        for id in [away, missing] {
            let key = AgentKey::new(host.as_bytes().to_vec(), id.as_bytes().to_vec());
            store
                .put_agent(&AgentRow::new(key, wire::Kind::ClaudeSdk, "/tmp"))
                .unwrap();
        }
    }
    let away_dir = install.agent_dir(away);
    let mut writer = journal::Writer::open(away_dir.join(agent_dir::JOURNAL), 1 << 20).unwrap();
    writer
        .append(&wire::Step {
            snapshot: Some(wire::Snapshot {
                phase: Phase::Idle as i32,
                at_ms: 1_000,
                ..Default::default()
            }),
            ..Default::default()
        })
        .unwrap();
    let written = writer.offset();
    std::fs::create_dir_all(install.agent_dir(orphan)).unwrap();

    // A supervisor that answers go only when the test says so.
    let (daemon_end, supervisor_end) = tokio::io::duplex(64);
    let (daemon_read, daemon_write) = tokio::io::split(daemon_end);
    let (supervisor_read, mut supervisor_write) = tokio::io::split(supervisor_end);
    let pipe = ActivationPipe::new(daemon_read, daemon_write);
    let options = install.options("boot-1", install.launch("idle", vec![]));
    let starting = tokio::spawn(node::start(options, Some(pipe)));

    let mut lines = BufReader::new(supervisor_read).lines();
    let prepared = tokio::time::timeout(PATIENCE, lines.next_line())
        .await
        .expect("the daemon prepares")
        .unwrap();
    assert_eq!(prepared.as_deref(), Some(node::PREPARED));

    // Prepared: the store is migrated and nothing else is written yet.
    {
        let look = store::Sqlite::open(&store_path, host.as_bytes().to_vec()).unwrap();
        let row = look
            .agent(&AgentKey::new(
                host.as_bytes().to_vec(),
                away.as_bytes().to_vec(),
            ))
            .unwrap()
            .unwrap();
        assert_eq!(
            row.lifecycle,
            Lifecycle::Live as i32,
            "not marked exited before go"
        );
        assert_eq!(row.ingest_cursor, 0, "nothing ingested before go");
        let stamp: u32 = look
            .connection()
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(stamp, store::SCHEMA_STAMP, "migrated before prepared");
    }
    assert!(
        install.agent_dir(orphan).exists(),
        "nothing removed before go"
    );
    assert!(!starting.is_finished(), "the daemon waits for go");

    supervisor_write.write_all(b"go\n").await.unwrap();
    let daemon = tokio::time::timeout(PATIENCE, starting)
        .await
        .expect("the daemon starts after go")
        .unwrap()
        .expect("the start succeeds");
    let runtime = runtime(&daemon, &install);
    let report = daemon.sweep(install.profile).unwrap().clone();
    println!("sweep after go: {report:#?}");

    let mut exited = vec![away, missing];
    exited.sort();
    assert_eq!(report.exited, exited);
    assert_eq!(report.removed, vec![orphan]);
    assert!(report.live.is_empty() && report.unanswered.is_empty());
    assert!(!install.agent_dir(orphan).exists());

    let store = runtime.store().await;
    let row = store.agent(&key(&runtime, away)).unwrap().unwrap();
    assert_eq!(row.lifecycle, Lifecycle::Exited as i32);
    assert_eq!(row.exit_cause, ExitCause::EndedWhileAway as i32);
    assert_eq!(row.ingest_cursor, written, "the remainder is ingested");
    assert_eq!(row.phase, Phase::Idle as i32);
    assert_eq!(
        store.cut(&key(&runtime, away), 10).unwrap().marker,
        Some(Marker::CaughtUp),
        "an agent the sweep finds exited is caught up"
    );
    let row = store.agent(&key(&runtime, missing)).unwrap().unwrap();
    assert_eq!(row.exit_cause, ExitCause::DirectoryFailed as i32);
    assert_eq!(row.exit_detail.as_deref(), Some("it is gone"));
    drop(store);
    drop(runtime);
    daemon.shutdown().await.unwrap();

    // A supervisor that goes away before go: the daemon does not start.
    let (daemon_end, supervisor_end) = tokio::io::duplex(64);
    let (daemon_read, daemon_write) = tokio::io::split(daemon_end);
    let pipe = ActivationPipe::new(daemon_read, daemon_write);
    let options = install.options("boot-1", install.launch("idle", vec![]));
    let starting = tokio::spawn(node::start(options, Some(pipe)));
    let (supervisor_read, supervisor_write) = tokio::io::split(supervisor_end);
    let mut lines = BufReader::new(supervisor_read).lines();
    assert_eq!(
        lines.next_line().await.unwrap().as_deref(),
        Some(node::PREPARED)
    );
    drop(supervisor_write);
    drop(lines);
    let result = tokio::time::timeout(PATIENCE, starting)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        result,
        Err(StartError::Activation(ActivationError::SupervisorGone))
    ));
}

/// The cause an agent's journal ends with reaches its row in the commit
/// that ingests that step, not when the daemon marks the agent exited: a
/// daemon that ingested the final step and died before marking leaves the
/// cause on the row, and the next daemon's sweep, with nothing left to
/// ingest, keeps it rather than recording that the agent ended while away.
#[tokio::test(flavor = "multi_thread")]
async fn an_ingested_cause_survives_a_daemon_that_died_before_marking() {
    let install = Install::new();
    let host = node::host_id(&install.profile_dir()).unwrap();
    let id = uuid::Uuid::new_v4();
    let key = AgentKey::new(host.as_bytes().to_vec(), id.as_bytes().to_vec());
    let mut writer =
        journal::Writer::open(install.agent_dir(id).join(agent_dir::JOURNAL), 1 << 20).unwrap();
    let last = wire::Step {
        snapshot: Some(wire::Snapshot {
            phase: Phase::Idle as i32,
            at_ms: 1_000,
            ..Default::default()
        }),
        incarnation_end: Some(wire::IncarnationEnd {
            cause: ExitCause::ProviderExited as i32,
            detail: Some("exit code 3".into()),
        }),
        ..Default::default()
    };
    writer.append(&last).unwrap();
    let written = writer.offset();
    drop(writer);

    // What the earlier daemon got done: the row, and the journal's last
    // step committed. It died before it marked the agent exited.
    {
        let mut store = store::Sqlite::open(
            &install.profile_dir().join(node::STORE),
            host.as_bytes().to_vec(),
        )
        .unwrap();
        store
            .put_agent(&AgentRow::new(key.clone(), wire::Kind::ClaudeSdk, "/tmp"))
            .unwrap();
        let clock = store::CommitClock {
            now_ms: 1_000,
            notify_delay_ms: 0,
        };
        store.commit(&key, &[(written, last)], clock).unwrap();
        let row = store.agent(&key).unwrap().unwrap();
        assert_eq!(row.lifecycle, Lifecycle::Live as i32, "not marked exited");
        assert_eq!(
            (row.exit_cause, row.exit_detail.as_deref()),
            (ExitCause::ProviderExited as i32, Some("exit code 3")),
            "the ingested step put its cause on the row"
        );
    }

    let daemon = install
        .start("boot-1", install.launch("idle", vec![]))
        .await;
    let runtime = runtime(&daemon, &install);
    let report = daemon.sweep(install.profile).unwrap().clone();
    assert_eq!(report.exited, vec![id]);
    let row = runtime.store().await.agent(&key).unwrap().unwrap();
    assert_eq!(row.lifecycle, Lifecycle::Exited as i32);
    assert_eq!(row.ingest_cursor, written, "nothing was left to ingest");
    assert_eq!(
        (row.exit_cause, row.exit_detail.as_deref()),
        (ExitCause::ProviderExited as i32, Some("exit code 3")),
        "the sweep keeps the agent's own cause"
    );
    drop(runtime);
    daemon.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn spawn_writes_spec_one_and_starts_the_agent_process() {
    let install = Install::new();
    let launch = install.launch("reply", vec![text("hello"), wire_turn_end()]);
    let daemon = install.start("boot-1", launch.clone()).await;
    let runtime = runtime(&daemon, &install);

    let agent = runtime
        .spawn(create(&install.work, "first", Some("say hello")), None)
        .await
        .expect("the agent spawns");
    let id = id_of(&agent);
    let dir = install.agent_dir(id);
    assert_eq!(agent.lifecycle, Lifecycle::Live as i32);
    assert_eq!(agent.incarnation, 1);
    assert_eq!(agent.name, "first");
    assert!(
        node::locked(&dir),
        "the agent process holds its directory's lock"
    );
    assert!(runtime.hello(id).is_some(), "the agent said Hello");
    assert!(
        listens(&dir.join(agent_dir::TOOLS_SOCK)).await,
        "the daemon listens on the agent's tools socket"
    );

    let spec = spec(&dir, 1);
    assert_eq!(spec.agent_id, id.as_bytes().to_vec());
    assert_eq!(spec.profile_id, install.profile.as_bytes().to_vec());
    assert_eq!(spec.kind(), wire::Kind::ClaudeSdk);
    assert_eq!(spec.cwd, install.work.to_string_lossy());
    assert_eq!(spec.name, "first");
    assert_eq!(spec.incarnation, 1);
    assert_eq!(spec.provider_command, launch.claude_command);
    assert_eq!(spec.daemon_version, node::version());
    assert!(spec.bootstrap.is_some(), "spec.1 carries the first prompt");
    let config = spec.config.as_ref().unwrap();
    assert_eq!(config.install_path, launch.install_path.to_string_lossy());
    assert_eq!(config.grace_ms, 20_000);
    println!("spec.1 of a spawned agent: {spec:#?}");

    // The journal is ingested as the agent writes it: the prompt runs to
    // the end of its turn and the row follows the snapshot to idle.
    until("the reply to be committed", || async {
        let store = runtime.store().await;
        let row = store.agent(&key(&runtime, id)).unwrap().unwrap();
        let said = store
            .last_n(&key(&runtime, id), 50)
            .unwrap()
            .iter()
            .any(|item| item.text.contains("hello"));
        (row.phase == Phase::Idle as i32 && said)
            .then_some(())
            .ok_or_else(|| format!("phase {}, said hello {said}", row.phase))
    })
    .await
    .unwrap();

    let again = runtime
        .spawn(
            wire::CreateAgentRequest {
                agent_id: id.as_bytes().to_vec(),
                ..create(&install.work, "again", None)
            },
            None,
        )
        .await;
    assert!(matches!(again, Err(RegistryError::AlreadyExists(_))));
    let nowhere = runtime
        .spawn(create(&install.path("missing"), "nowhere", None), None)
        .await;
    assert!(matches!(nowhere, Err(RegistryError::BadCwd(_))));

    kill_all(&runtime).await;
    assert!(
        !listens(&dir.join(agent_dir::TOOLS_SOCK)).await
            && !dir.join(agent_dir::TOOLS_SOCK).exists(),
        "the tools socket goes when the agent exits"
    );
    drop(runtime);
    daemon.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn stop_modes_end_the_process_and_record_why() {
    let install = Install::new();
    let never = install.path("never");
    let daemon = install
        .start("boot-1", install.launch("held", vec![wait_for(&never)]))
        .await;
    let runtime = runtime(&daemon, &install);

    for (mode, cause, name, prompt) in [
        (StopMode::Graceful, ExitCause::Stopped, "stopped", None),
        (
            StopMode::Abort,
            ExitCause::Aborted,
            "aborted",
            Some("work until stopped"),
        ),
        (
            StopMode::Kill,
            ExitCause::Killed,
            "killed",
            Some("work until killed"),
        ),
    ] {
        let agent = runtime
            .spawn(create(&install.work, name, prompt), None)
            .await
            .unwrap();
        let id = id_of(&agent);
        let dir = install.agent_dir(id);
        if prompt.is_some() {
            until("the turn to start", || working(&runtime, id))
                .await
                .unwrap();
        }
        let stopped = runtime.stop(id, mode).await.expect("the stop completes");
        assert_eq!(stopped.lifecycle, Lifecycle::Exited as i32, "{mode:?}");
        assert_eq!(stopped.exit_cause(), cause, "{mode:?}");
        assert!(!node::locked(&dir), "{mode:?}: the lock is released");
        assert!(
            !listens(&dir.join(agent_dir::TOOLS_SOCK)).await
                && !dir.join(agent_dir::TOOLS_SOCK).exists(),
            "{mode:?}"
        );
        assert!(!runtime.live().contains(&id), "{mode:?}");
        assert_eq!(
            runtime
                .store()
                .await
                .cut(&key(&runtime, id), 1)
                .unwrap()
                .marker,
            Some(Marker::CaughtUp),
            "{mode:?}: an exited agent's journal is read to its end"
        );
        // Stopping an exited agent leaves it as it is.
        let again = runtime.stop(id, StopMode::Kill).await.unwrap();
        assert_eq!(again.exit_cause(), cause);
    }
    drop(runtime);
    daemon.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn resume_writes_the_next_spec_and_waits_for_a_dying_process() {
    let install = Install::new();
    let gate = install.path("gate");
    let daemon = install
        .start(
            "boot-1",
            install.launch("gated-exit", vec![wait_for(&gate), exit(0)]),
        )
        .await;
    let runtime = runtime(&daemon, &install);
    let agent = runtime
        .spawn(
            create(&install.work, "resumable", Some("wait at the gate")),
            None,
        )
        .await
        .unwrap();
    let id = id_of(&agent);
    let dir = install.agent_dir(id);

    let live = runtime.resume(id, None).await;
    assert!(
        matches!(live, Err(RegistryError::Live(_))),
        "a live agent is not resumed"
    );

    // The agent has said it is leaving (it refused an input as exiting),
    // but its process still holds the directory: the resume waits.
    runtime.mark_exiting(id);
    let resuming = {
        let runtime = runtime.clone();
        tokio::spawn(async move {
            runtime
                .resume(
                    id,
                    Some(wire::Bootstrap {
                        of: Some(wire::bootstrap::Of::Prompt(wire::PromptSubmission {
                            input_id: b"p1".to_vec(),
                            prompt: Some(wire::PromptInput {
                                text: "carry on".into(),
                                ..Default::default()
                            }),
                        })),
                    }),
                )
                .await
        })
    };
    // A window: a resume blocked on the lock leaves no mark to wait on.
    holds_for(
        "the resume to wait for the lock",
        Duration::from_millis(500),
        || async { !resuming.is_finished() && !dir.join("spec.2").exists() },
    )
    .await
    .expect("the resume waits for the lock, and no spec is written under a live one");

    std::fs::write(&gate, "").unwrap();
    let resumed = tokio::time::timeout(PATIENCE, resuming)
        .await
        .expect("the resume completes once the old process is gone")
        .unwrap()
        .expect("the resume succeeds");
    assert_eq!(resumed.incarnation, 2);
    let spec2 = spec(&dir, 2);
    assert_eq!(spec2.incarnation, 2);
    assert_eq!(spec2.agent_id, id.as_bytes().to_vec());
    assert_eq!(spec2.name, "resumable", "a spec keeps the spawn's name");
    assert_eq!(
        spec2.bootstrap.and_then(|bootstrap| match bootstrap.of {
            Some(wire::bootstrap::Of::Prompt(prompt)) => Some(prompt.input_id),
            _ => None,
        }),
        Some(b"p1".to_vec()),
        "the resume's prompt seeds the new incarnation"
    );
    assert_eq!(spec(&dir, 1).incarnation, 1, "spec.1 is never rewritten");

    kill_all(&runtime).await;
    drop(runtime);
    daemon.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn delete_aborts_first_and_cascades_to_children() {
    let install = Install::new();
    let never = install.path("never");
    let daemon = install
        .start("boot-1", install.launch("held", vec![wait_for(&never)]))
        .await;
    let runtime = runtime(&daemon, &install);

    let parent = id_of(
        &runtime
            .spawn(create(&install.work, "parent", Some("hold")), None)
            .await
            .unwrap(),
    );
    let child = runtime
        .spawn(create(&install.work, "child", Some("hold")), Some(parent))
        .await
        .unwrap();
    assert_eq!(
        parent_of(&child).map(|p| p.agent_id),
        Some(parent.as_bytes().to_vec()),
        "the caller is the parent"
    );
    let child = id_of(&child);
    let bystander = id_of(
        &runtime
            .spawn(create(&install.work, "bystander", None), None)
            .await
            .unwrap(),
    );
    // A child on another host: listed, but out of this daemon's reach.
    let remote = AgentKey::new(vec![7; 16], uuid::Uuid::new_v4().as_bytes().to_vec());
    {
        let mut row = AgentRow::new(remote.clone(), wire::Kind::Codex, "/");
        row.parent = Some(key(&runtime, parent));
        runtime.store().await.put_agent(&row).unwrap();
    }
    for id in [parent, child] {
        until("the turn to start", || working(&runtime, id))
            .await
            .unwrap();
    }

    let response = runtime.delete(parent).await.expect("the delete completes");
    println!("delete response: {response:#?}");
    assert_eq!(
        response
            .removed_children
            .iter()
            .map(|agent| agent.agent_id.clone())
            .collect::<Vec<_>>(),
        vec![child.as_bytes().to_vec()]
    );
    assert_eq!(
        response
            .unreachable_children
            .iter()
            .map(|agent| agent.agent_id.clone())
            .collect::<Vec<_>>(),
        vec![remote.agent.clone()]
    );
    for id in [parent, child] {
        assert!(!install.agent_dir(id).exists(), "the directory is removed");
        assert!(matches!(
            runtime.agent(id).await,
            Err(RegistryError::NotFound(_))
        ));
        assert!(!runtime.live().contains(&id));
    }
    assert!(runtime.store().await.agent(&remote).unwrap().is_some());
    assert_eq!(
        runtime.agent(bystander).await.unwrap().lifecycle,
        Lifecycle::Live as i32
    );
    assert!(matches!(
        runtime.delete(parent).await,
        Err(RegistryError::NotFound(_))
    ));

    kill_all(&runtime).await;
    drop(runtime);
    daemon.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn rename_changes_the_row_and_the_next_spec_never_a_written_one() {
    let install = Install::new();
    let daemon = install
        .start("boot-1", install.launch("idle", vec![]))
        .await;
    let runtime = runtime(&daemon, &install);
    let id = id_of(
        &runtime
            .spawn(create(&install.work, "before", None), None)
            .await
            .unwrap(),
    );
    let renamed = runtime.rename(id, "after").await.unwrap();
    assert_eq!(renamed.name, "after");
    assert_eq!(runtime.agent(id).await.unwrap().name, "after");
    assert_eq!(spec(&install.agent_dir(id), 1).name, "before");
    kill_all(&runtime).await;
    runtime.resume(id, None).await.unwrap();
    assert_eq!(
        spec(&install.agent_dir(id), 2).name,
        "after",
        "the next incarnation runs under the name it has now"
    );
    kill_all(&runtime).await;
    drop(runtime);
    daemon.shutdown().await.unwrap();
}

/// The names a fake Codex was asked to give its thread, in order.
fn thread_names(log: &std::path::Path) -> Vec<String> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|line| line["method"] == "thread/name/set")
        .map(|line| {
            line["params"]["name"]
                .as_str()
                .unwrap_or_default()
                .to_owned()
        })
        .collect()
}

/// The arm that carries a create's settings is its kind, so a create with
/// no arm names no interpreter and is refused before anything is made.
#[tokio::test]
async fn a_create_without_a_kind_is_invalid() {
    let install = Install::new();
    let daemon = install
        .start("boot-1", install.launch("idle", vec![]))
        .await;
    let runtime = runtime(&daemon, &install);
    let refused = runtime
        .spawn(
            wire::CreateAgentRequest {
                agent: None,
                ..create(&install.work, "kindless", None)
            },
            None,
        )
        .await
        .unwrap_err();
    assert!(matches!(refused, RegistryError::NoKind), "{refused:?}");
    assert_eq!(
        refused.to_wire().code,
        wire::ErrorCode::InvalidArgument as i32
    );
    assert!(
        runtime.store().await.agents().unwrap().is_empty(),
        "no agent was made"
    );
    drop(runtime);
    daemon.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn renaming_a_live_codex_agent_renames_its_thread() {
    let install = Install::new();
    let log = install.path("codex-input.jsonl");
    let mut launch = install.launch("idle", vec![]);
    launch.provider_env.insert(
        provider_fakes::INPUT_LOG_ENV.to_owned(),
        log.to_string_lossy().into_owned(),
    );
    let daemon = install.start("boot-1", launch).await;
    let runtime = runtime(&daemon, &install);
    let id = id_of(
        &runtime
            .spawn(
                wire::CreateAgentRequest {
                    agent: wire::create_agent_request::Agent::defaults(wire::Kind::Codex),
                    ..create(&install.work, "before", None)
                },
                None,
            )
            .await
            .unwrap(),
    );
    let named = |want: &'static [&'static str]| {
        let log = log.clone();
        move || {
            let names = thread_names(&log);
            std::future::ready((names == want).then_some(()).ok_or(format!("{names:?}")))
        }
    };
    until("the thread named at start", named(&["before"]))
        .await
        .unwrap();
    runtime.rename(id, "after").await.unwrap();
    until("the thread renamed", named(&["before", "after"]))
        .await
        .unwrap();
    kill_all(&runtime).await;
    drop(runtime);
    daemon.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_restarted_daemon_adopts_live_agents_and_sweeps_exited_ones() {
    let install = Install::new();
    let gate = install.path("gate");
    let launch = install.launch(
        "finish-while-away",
        // The provider plays steps only within a turn, so the exit is part
        // of the turn the gate holds open.
        vec![wait_for(&gate), text("done while away"), exit(0)],
    );
    let daemon = install.start("boot-1", launch.clone()).await;
    let runtime = runtime(&daemon, &install);
    let stays = id_of(
        &runtime
            .spawn(create(&install.work, "stays", None), None)
            .await
            .unwrap(),
    );
    let leaves = id_of(
        &runtime
            .spawn(create(&install.work, "leaves", Some("finish later")), None)
            .await
            .unwrap(),
    );
    until("the turn to start", || working(&runtime, leaves))
        .await
        .unwrap();

    // The daemon dies without shutting down; one agent finishes its turn
    // and exits while it is away, the other keeps running.
    drop(runtime);
    drop(daemon);
    std::fs::write(&gate, "").unwrap();
    let leaves_dir = install.agent_dir(leaves);
    until("the agent to exit while the daemon is away", || async {
        (!node::locked(&leaves_dir))
            .then_some(())
            .ok_or("the directory is still locked")
    })
    .await
    .unwrap();
    assert!(node::locked(&install.agent_dir(stays)));

    let daemon = install.start("boot-1", launch).await;
    assert_eq!(
        daemon.generation().counter,
        1,
        "a daemon crash keeps the generation"
    );
    let runtime = support::runtime(&daemon, &install);
    let report = daemon.sweep(install.profile).unwrap().clone();
    println!("sweep after a daemon crash: {report:#?}");
    assert_eq!(
        report.live,
        vec![stays],
        "the live agent answered with a Hello"
    );
    assert_eq!(report.exited, vec![leaves]);
    assert!(runtime.hello(stays).is_some());
    assert!(runtime.live().contains(&stays));

    let store = runtime.store().await;
    let row = store.agent(&key(&runtime, leaves)).unwrap().unwrap();
    assert_eq!(row.lifecycle, Lifecycle::Exited as i32);
    // Its journal says why it ended, though no daemon saw it go.
    assert_eq!(row.exit_cause, ExitCause::ProviderExited as i32);
    assert_eq!(row.exit_detail.as_deref(), Some("exit code 0"));
    assert_eq!(
        row.ingest_cursor,
        journal_end(&leaves_dir),
        "the journal written while the daemon was away is ingested"
    );
    assert!(
        store
            .last_n(&key(&runtime, leaves), 50)
            .unwrap()
            .iter()
            .any(|item| item.text.contains("done while away"))
    );
    assert_eq!(
        store.cut(&key(&runtime, leaves), 1).unwrap().marker,
        Some(Marker::CaughtUp)
    );
    drop(store);

    // The adopted agent is the registry's like any other: it stops.
    let stopped = runtime.stop(stays, StopMode::Graceful).await.unwrap();
    assert_eq!(stopped.exit_cause(), ExitCause::Stopped);
    drop(runtime);
    daemon.shutdown().await.unwrap();
}

/// Whether the agent's journal holds a daemon-lost boundary: the agent
/// gave up on its daemon and is draining.
fn lost_its_daemon(dir: &std::path::Path) -> Result<(), String> {
    let mut reader = journal::Reader::new(dir.join(agent_dir::JOURNAL), 0);
    let frames = reader
        .read_to_end()
        .map_err(|error| error.to_string())?
        .frames;
    frames
        .into_iter()
        .flat_map(|(_, step)| step.items)
        .filter_map(|item| {
            match wire::ClaudeSdkItem::decode(item.body.as_slice())
                .ok()?
                .kind?
            {
                wire::claude_sdk_item::Kind::Boundary(boundary) => Some(boundary.kind()),
                _ => None,
            }
        })
        .any(|kind| kind == wire::BoundaryKind::DaemonLost)
        .then_some(())
        .ok_or_else(|| "no daemon-lost boundary yet".to_owned())
}

/// The agent's own account of its exit reaches its row through its
/// journal: an agent that drains after losing its daemon, and one whose
/// open ask is orphaned, both end while no daemon runs, and the next daemon
/// reads why from what they wrote last. It could not tell either from a
/// plain exit.
#[tokio::test(flavor = "multi_thread")]
async fn agent_cause_reaches_the_row() {
    let install = Install::new();
    let gate = install.path("gate");
    let short = |mut launch: node::Launch| {
        launch.agent.grace_secs = 1;
        launch.agent.drain_secs = 5;
        launch
    };
    let held = short(install.launch(
        "held",
        vec![
            wait_for(&gate),
            text("done after the daemon left"),
            wire_turn_end(),
        ],
    ));
    let asking = short(install.launch(
        "asking",
        vec![provider_fakes::script::Step::Ask(
            provider_fakes::script::Ask::Permission(provider_fakes::script::Tool {
                name: None,
                class: provider_fakes::script::ToolClass::Consequential,
                input: None,
                outcome: Default::default(),
                wait_for: None,
            }),
        )],
    ));
    let daemon = install.start("boot-1", held.clone()).await;
    let runtime = runtime(&daemon, &install);
    let drains = id_of(
        &runtime
            .spawn(
                create(&install.work, "drains", Some("work through the gap")),
                None,
            )
            .await
            .unwrap(),
    );
    runtime.set_launch(asking);
    let orphaned = id_of(
        &runtime
            .spawn(
                create(&install.work, "orphaned", Some("ask me first")),
                None,
            )
            .await
            .unwrap(),
    );
    until("the held turn to start", || working(&runtime, drains))
        .await
        .unwrap();
    until("the ask to open", || async {
        let phase = runtime.agent(orphaned).await.unwrap().phase;
        (phase == Phase::NeedsYou as i32)
            .then_some(())
            .ok_or_else(|| format!("phase {phase}"))
    })
    .await
    .unwrap();

    // The daemon dies; past the grace both agents drain, one through its
    // running turn, one with an ask nobody can answer.
    let gone = std::sync::Arc::downgrade(&runtime);
    drop(runtime);
    drop(daemon);
    until("the crashed daemon to let go", || async {
        gone.upgrade()
            .map_or(Ok(()), |_| Err("the runtime is still held"))
    })
    .await
    .unwrap();
    for id in [drains, orphaned] {
        let dir = install.agent_dir(id);
        until("the agent to lose its daemon", || {
            std::future::ready(lost_its_daemon(&dir))
        })
        .await
        .unwrap();
    }

    // With no daemon back, the held turn finishes and its agent drains;
    // the other's ask has nobody to answer it. Both end and let go of
    // their directories before any daemon returns.
    std::fs::write(&gate, "").unwrap();
    for id in [drains, orphaned] {
        let dir = install.agent_dir(id);
        until("the agent to exit with no daemon", || {
            std::future::ready(
                (!agent_dir::locked(&dir))
                    .then_some(())
                    .ok_or("its process still holds the directory"),
            )
        })
        .await
        .unwrap();
    }

    let daemon = install.start("boot-1", held).await;
    let runtime = support::runtime(&daemon, &install);
    let report = daemon.sweep(install.profile).unwrap().clone();
    let mut exited = report.exited.clone();
    exited.sort();
    let mut both = vec![drains, orphaned];
    both.sort();
    assert_eq!(exited, both, "the sweep found both ended");
    let drained = runtime.agent(drains).await.unwrap();
    assert_eq!(drained.lifecycle(), Lifecycle::Exited);
    assert_eq!(drained.exit_cause(), ExitCause::DaemonLost);
    assert_eq!(drained.exit_detail, None);
    let orphan = runtime.agent(orphaned).await.unwrap();
    assert_eq!(orphan.exit_cause(), ExitCause::Orphaned);
    assert_eq!(orphan.exit_detail, None);
    drop(runtime);
    daemon.shutdown().await.unwrap();
}

fn wire_turn_end() -> provider_fakes::script::Step {
    provider_fakes::script::Step::TurnEnd
}

fn exit(code: i32) -> provider_fakes::script::Step {
    provider_fakes::script::Step::Exit { code }
}

fn names(entries: &[wire::ProjectEntry]) -> Vec<&str> {
    entries.iter().map(|entry| entry.name.as_str()).collect()
}

fn listing(
    query: Option<&str>,
    limit: u32,
    host: Option<uuid::Uuid>,
) -> tonic::Request<wire::ListRepositoriesRequest> {
    tonic::Request::new(wire::ListRepositoriesRequest {
        query: query.map(str::to_owned),
        limit,
        host_id: host.map(|host| host.as_bytes().to_vec()),
    })
}

/// Where a host offers to start an agent: the directories its agents ran
/// in, newest first and still after the agents are deleted, then the Git
/// repositories under its roots. A paired host naming it gets the same
/// answer through it; once it no longer trusts that host, nothing.
#[tokio::test(flavor = "multi_thread")]
async fn a_host_lists_where_its_agents_ran_and_its_repositories_for_trusted_hosts() {
    use wire::client_service_server::ClientService as _;

    let desk = Install::new();
    let roots = desk.path("code");
    for repository in ["amux", "notes", "nested/deep/site", "amux/vendor/lib"] {
        std::fs::create_dir_all(roots.join(repository).join(".git")).unwrap();
    }
    // A worktree's .git is a file.
    std::fs::create_dir_all(roots.join("amux-review")).unwrap();
    std::fs::write(roots.join("amux-review/.git"), "gitdir: ../amux/.git\n").unwrap();
    std::fs::create_dir_all(roots.join("plain")).unwrap();
    let (older, newer) = (desk.work.join("older"), desk.work.join("newer"));
    std::fs::create_dir_all(&older).unwrap();
    std::fs::create_dir_all(&newer).unwrap();
    let mut launch = desk.launch("idle", Vec::new());
    launch.repository_roots = vec![roots.clone(), desk.path("missing")];
    let desk_daemon = desk.start("boot-1", launch).await;
    let desk_runtime = runtime(&desk_daemon, &desk);
    let first = id_of(
        &desk_runtime
            .spawn(create(&older, "first", None), None)
            .await
            .unwrap(),
    );
    // A forced delay: the listing orders repositories by the agents'
    // creation times, which the clock stamps in milliseconds.
    tokio::time::sleep(Duration::from_millis(5)).await;
    desk_runtime
        .spawn(create(&newer, "second", None), None)
        .await
        .unwrap();
    desk_runtime.delete(first).await.unwrap();

    let person = node::ClientApi::new(&desk_runtime, None);
    let local = person
        .list_repositories(listing(None, 50, None))
        .await
        .unwrap()
        .into_inner();
    println!("local listing: {local:#?}");
    assert_eq!(
        names(&local.recent),
        ["newer", "older"],
        "newest first, deleted agents too"
    );
    assert!(local.recent[0].last_used_ms > local.recent[1].last_used_ms);
    assert_eq!(
        local.recent[0].path,
        newer.canonicalize().unwrap().to_str().unwrap()
    );
    // Nothing inside a repository is searched, and plain directories are
    // not repositories.
    assert_eq!(
        names(&local.repositories),
        ["amux", "amux-review", "site", "notes"]
    );
    assert!(
        local
            .repositories
            .iter()
            .all(|entry| entry.last_used_ms.is_none())
    );
    assert_eq!(
        local.roots,
        [roots.canonicalize().unwrap().to_str().unwrap()]
    );

    let found = person
        .list_repositories(listing(Some("REVIEW"), 50, Some(desk_runtime.host())))
        .await
        .unwrap()
        .into_inner();
    assert!(found.recent.is_empty());
    assert_eq!(names(&found.repositories), ["amux-review"]);
    let capped = person
        .list_repositories(listing(None, 3, None))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(names(&capped.recent), ["newer", "older"]);
    assert_eq!(
        names(&capped.repositories),
        ["amux"],
        "the limit counts both lists"
    );

    // A paired laptop asks the desk by its host id.
    let laptop = Install::new();
    let laptop_daemon = laptop
        .start("boot-1", laptop.launch("idle", Vec::new()))
        .await;
    let laptop_runtime = runtime(&laptop_daemon, &laptop);
    let (desk_edge, laptop_edge) = (desk_runtime.edge().unwrap(), laptop_runtime.edge().unwrap());
    desk_edge.trust(&laptop_edge).await.unwrap();
    laptop_edge.trust(&desk_edge).await.unwrap();
    let link = laptop_edge.link_in_process(&desk_edge).unwrap();
    assert!(
        laptop_edge
            .wait_for_route(desk_runtime.host(), PATIENCE)
            .await
    );
    let from_laptop = node::ClientApi::new(&laptop_runtime, None);
    let forwarded = from_laptop
        .list_repositories(listing(None, 50, Some(desk_runtime.host())))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        forwarded, local,
        "the desk answers the laptop as it answers its own clients"
    );
    let own = from_laptop
        .list_repositories(listing(None, 50, Some(laptop_runtime.host())))
        .await
        .unwrap()
        .into_inner();
    assert!(own.recent.is_empty() && own.repositories.is_empty() && own.roots.is_empty());
    let stranger = from_laptop
        .list_repositories(listing(None, 50, Some(uuid::Uuid::new_v4())))
        .await
        .unwrap_err();
    assert_eq!(
        stranger.code(),
        tonic::Code::FailedPrecondition,
        "{stranger:?}"
    );

    // The desk stops trusting the laptop: its listing is refused.
    desk_edge
        .unpair(
            wire::HostRef {
                of: Some(wire::host_ref::Of::HostId(
                    laptop_runtime.host().as_bytes().to_vec(),
                )),
            },
            "test".to_owned(),
        )
        .await
        .unwrap();
    // A call that crosses the link while the desk is closing it comes back
    // uncertain rather than refused; ask again until the desk's answer.
    let deadline = tokio::time::Instant::now() + PATIENCE;
    let untrusted = loop {
        let refused = from_laptop
            .list_repositories(listing(None, 50, Some(desk_runtime.host())))
            .await
            .expect_err("a host that stopped trusting the caller answers nothing");
        if refused.code() != tonic::Code::Aborted || tokio::time::Instant::now() > deadline {
            break refused;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    assert!(
        matches!(
            untrusted.code(),
            tonic::Code::FailedPrecondition | tonic::Code::Unauthenticated
        ),
        "a host that stopped trusting the caller answers nothing: {untrusted:?}"
    );

    drop(link);
    kill_all(&desk_runtime).await;
    drop((desk_runtime, laptop_runtime, desk_edge, laptop_edge));
    desk_daemon.shutdown().await.unwrap();
    laptop_daemon.shutdown().await.unwrap();
}
