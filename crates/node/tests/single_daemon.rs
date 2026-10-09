//! One daemon over synthetic journals and a real SQLite store: ingest,
//! fan-out, the subscribe cut, Fetch and Get, the inventory, name
//! resolution, and what survives a crash, a failed commit or a power cut.
//!
//! No agent process runs here. An agent is a directory with a journal the
//! synthetic writer fills; a "live" one also holds the directory's lock and
//! answers the daemon's dial with a Hello, so the daemon adopts it and
//! ingests on its Nudges exactly as it does a real agent's.

mod support;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use node::{Daemon, JoinHook, Launch, ProfileRuntime, ServeError};
use prost::{Message as _, Name as _};
use store::Store as _;
use support::synthetic::*;
use support::*;
use wire::{
    AmbiguousAgentName, ErrorCode, FetchRequest, GetRequest, InventoryEvent, Lifecycle, Phase,
    Presence, SessionEvent, Step, Trust, inventory_event, session_event,
};

const BIG_SEGMENTS: u64 = 1 << 20;

/// A transcript with a streamed item, a queued prompt later withdrawn, and
/// a re-derivation that emits a known key again.
fn steps() -> Vec<Step> {
    vec![
        item("a", "one"),
        item("b", "tw"),
        append("b", "o"),
        snapshot(Phase::Working, &["q1"], 2_000),
        item("c", "three"),
        item("b", "two, again"),
        snapshot(Phase::Idle, &[], 3_000),
        item("d", "four"),
    ]
}

/// What the store holds for one agent: each key's order, revision and
/// text, the snapshot's revision and queue, and the cursor.
#[derive(Clone, Debug, PartialEq)]
struct Held {
    items: BTreeMap<String, (u64, u64, String)>,
    snapshot: Option<(u64, Vec<String>)>,
    cursor: u64,
    next_revision: u64,
}

async fn held(runtime: &ProfileRuntime, agent: &SyntheticAgent, install: &Install) -> Held {
    let store = runtime.store().await;
    let key = agent.key(install);
    let row = store.agent(&key).unwrap().expect("the row");
    let items = store
        .page(&key, None, 1_000)
        .unwrap()
        .items
        .into_iter()
        .map(|item| (item.key.clone(), (item.order, item.revision, item.text)))
        .collect();
    Held {
        items,
        snapshot: row.snapshot.map(|snapshot| {
            (
                snapshot.revision,
                snapshot
                    .queue
                    .iter()
                    .map(|q| String::from_utf8_lossy(&q.input_id).into_owned())
                    .collect(),
            )
        }),
        cursor: row.ingest_cursor,
        next_revision: row.next_revision,
    }
}

async fn start(install: &Install, boot: &str, launch: Launch) -> (Daemon, Arc<ProfileRuntime>) {
    let daemon = install.start(boot, launch).await;
    let runtime = runtime(&daemon, install);
    (daemon, runtime)
}

/// A daemon crash: nothing is flushed and nothing marks the installation
/// clean. The page cache, and so the store's WAL, survives.
///
/// Returns only once the store is closed, as a dead process's is. Dropping
/// the daemon aborts its tasks without waiting for them, and one caught
/// holding the runtime would close the store later on its own thread;
/// closing checkpoints the WAL into the store file, and done after
/// [`lose_power`] rewrote that file it would put back what the power cut
/// took. So the runtime is dropped here, on this thread, once this is its
/// last reference.
async fn crash(daemon: Daemon, runtime: Arc<ProfileRuntime>) {
    drop(daemon);
    let mut runtime = Some(runtime);
    until("the crashed daemon's tasks to release its store", || {
        std::future::ready(match Arc::try_unwrap(runtime.take().unwrap()) {
            Ok(last) => {
                drop(last);
                Ok(())
            }
            Err(shared) => {
                let held = Arc::strong_count(&shared);
                runtime = Some(shared);
                Err(format!("{held} references held"))
            }
        })
    })
    .await
    .unwrap();
}

async fn write_and_ingest(
    runtime: &ProfileRuntime,
    agent: &mut SyntheticAgent,
    steps: &[Step],
) -> Vec<u64> {
    let mut offsets = Vec::new();
    for step in steps {
        offsets.push(agent.append(step));
        runtime.ingest(agent.id).await.unwrap();
    }
    offsets
}

/// The clean run every failure cut is compared against.
async fn reference() -> Held {
    let install = Install::new();
    let mut agent = SyntheticAgent::new(&install, "reference", BIG_SEGMENTS);
    agent.register_offline(&install);
    let (daemon, runtime) = start(&install, "boot-1", quiet_launch()).await;
    write_and_ingest(&runtime, &mut agent, &steps()).await;
    let held = held(&runtime, &agent, &install).await;
    crash(daemon, runtime).await;
    held
}

fn items_of(held: &Held) -> Vec<(&str, u64, u64, &str)> {
    held.items
        .iter()
        .map(|(key, (order, revision, text))| (key.as_str(), *order, *revision, text.as_str()))
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn order_is_assigned_once_per_key_and_kept_across_a_re_derivation() {
    let reference = reference().await;
    assert_eq!(
        items_of(&reference),
        vec![
            ("a", 1, 1, "one"),
            // Streamed as "tw" + "o" at revisions 2 and 3, then emitted
            // again by a re-derivation at revision 6: same order.
            ("b", 2, 6, "two, again"),
            ("c", 3, 5, "three"),
            ("d", 4, 8, "four"),
        ]
    );
    assert_eq!(reference.snapshot, Some((7, vec![])));
    assert_eq!(reference.next_revision, 9);

    // A re-derivation after a daemon restart: the agent's next process
    // emits known keys again with new content, and a new one.
    let install = Install::new();
    let mut agent = SyntheticAgent::new(&install, "rederived", BIG_SEGMENTS);
    agent.register_offline(&install);
    let (daemon, runtime) = start(&install, "boot-1", quiet_launch()).await;
    write_and_ingest(&runtime, &mut agent, &steps()[..5]).await;
    crash(daemon, runtime).await;
    let (daemon, runtime) = start(&install, "boot-1", quiet_launch()).await;
    write_and_ingest(
        &runtime,
        &mut agent,
        &[
            item("c", "three, re-derived"),
            item("a", "one, re-derived"),
            item("e", "five"),
        ],
    )
    .await;
    let after = held(&runtime, &agent, &install).await;
    assert_eq!(
        items_of(&after),
        vec![
            ("a", 1, 7, "one, re-derived"),
            ("b", 2, 3, "two"),
            ("c", 3, 6, "three, re-derived"),
            ("e", 4, 8, "five"),
        ],
        "known keys keep the order they were first committed at; a new key takes the next"
    );
    crash(daemon, runtime).await;
}

/// Where a run is cut.
#[derive(Clone, Copy, Debug)]
enum Cut {
    /// The daemon dies before step i is written.
    CrashBeforeWrite,
    /// Step i is half written when the agent dies; its next process
    /// reopens the journal, which drops the torn frame, and writes it again.
    TornWriteWriterDies,
    /// Step i is written but the daemon dies before reading it.
    CrashAfterWrite,
    /// The commit holding step i fails writing its items.
    CommitFailsAtItemWrite,
    /// The commit holding step i fails writing the row and cursor.
    CommitFailsAtCursorWrite,
    /// The daemon dies right after committing step i.
    CrashAfterCommit,
}

const ITEM_CUT: &str =
    "CREATE TEMP TRIGGER cut BEFORE INSERT ON items BEGIN SELECT RAISE(ABORT, 'cut'); END;";
const CURSOR_CUT: &str =
    "CREATE TEMP TRIGGER cut BEFORE INSERT ON agents BEGIN SELECT RAISE(ABORT, 'cut'); END;";

async fn cut_run(cut: Cut, at: usize) -> Held {
    let steps = steps();
    let install = Install::new();
    let mut agent = SyntheticAgent::new(&install, "cut", BIG_SEGMENTS);
    agent.register_offline(&install);
    let (mut daemon, mut runtime) = start(&install, "boot-1", quiet_launch()).await;
    write_and_ingest(&runtime, &mut agent, &steps[..at]).await;

    let resume_from = match cut {
        Cut::CrashBeforeWrite => {
            crash(daemon, runtime).await;
            (daemon, runtime) = start(&install, "boot-1", quiet_launch()).await;
            at
        }
        Cut::TornWriteWriterDies => {
            let before = held(&runtime, &agent, &install).await;
            let frame = journal::encode_frame(&steps[at]).len();
            agent
                .journal()
                .write_partial(&steps[at], frame / 2)
                .unwrap();
            runtime.ingest(agent.id).await.unwrap();
            assert_eq!(
                held(&runtime, &agent, &install).await,
                before,
                "a torn frame is never read"
            );
            agent.reopen_journal();
            at
        }
        Cut::CrashAfterWrite => {
            agent.append(&steps[at]);
            crash(daemon, runtime).await;
            // The sweep reads what the agent wrote while nobody was reading.
            (daemon, runtime) = start(&install, "boot-1", quiet_launch()).await;
            at + 1
        }
        Cut::CommitFailsAtItemWrite | Cut::CommitFailsAtCursorWrite => {
            let mut subscription = runtime.subscribe(agent.id.as_bytes(), 0).await.unwrap();
            let mut opening = Vec::new();
            drain(&mut subscription, &mut opening, Duration::from_millis(20)).await;
            let before = held(&runtime, &agent, &install).await;
            agent.append(&steps[at]);
            let trigger = match cut {
                Cut::CommitFailsAtItemWrite => ITEM_CUT,
                _ => CURSOR_CUT,
            };
            runtime
                .store()
                .await
                .connection()
                .execute_batch(trigger)
                .unwrap();
            assert!(
                runtime.ingest(agent.id).await.is_err(),
                "{cut:?} at step {at} fails the commit"
            );
            assert_eq!(
                held(&runtime, &agent, &install).await,
                before,
                "a failed commit leaves no row, no revision and no cursor behind"
            );
            let mut live = Vec::new();
            drain(&mut subscription, &mut live, Duration::from_millis(50)).await;
            assert!(
                live.is_empty(),
                "nothing is broadcast for a commit that failed: {:?}",
                log(&live)
            );
            runtime
                .store()
                .await
                .connection()
                .execute_batch("DROP TRIGGER temp.cut;")
                .unwrap();
            runtime.ingest(agent.id).await.unwrap();
            at + 1
        }
        Cut::CrashAfterCommit => {
            agent.append(&steps[at]);
            runtime.ingest(agent.id).await.unwrap();
            crash(daemon, runtime).await;
            (daemon, runtime) = start(&install, "boot-1", quiet_launch()).await;
            at + 1
        }
    };
    write_and_ingest(&runtime, &mut agent, &steps[resume_from..]).await;
    let held = held(&runtime, &agent, &install).await;
    crash(daemon, runtime).await;
    held
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failure_before_or_after_each_write_commit_and_cursor_step_changes_nothing() {
    let reference = reference().await;
    let steps = steps();
    let mut runs = 0;
    for (at, step) in steps.iter().enumerate() {
        let writes_items = !step.items.is_empty() || !step.appends.is_empty();
        for cut in [
            Cut::CrashBeforeWrite,
            Cut::TornWriteWriterDies,
            Cut::CrashAfterWrite,
            Cut::CommitFailsAtItemWrite,
            Cut::CommitFailsAtCursorWrite,
            Cut::CrashAfterCommit,
        ] {
            if matches!(cut, Cut::CommitFailsAtItemWrite) && !writes_items {
                continue;
            }
            let held = cut_run(cut, at).await;
            assert_eq!(
                held, reference,
                "{cut:?} at step {at} ends where the uncut run ends"
            );
            runs += 1;
        }
    }
    println!("{runs} cut runs each ended with the reference rows: {reference:#?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn torn_tails_are_never_read_whether_the_writer_lives_or_dies() {
    // Alive: the frame is still being written when the daemon adopts the
    // agent. CaughtUp waits for it.
    let install = Install::new();
    let mut agent = SyntheticAgent::new(&install, "alive", BIG_SEGMENTS);
    agent.register_offline(&install);
    agent.append(&item("a", "one"));
    let frame = journal::encode_frame(&item("b", "two")).len();
    agent
        .journal()
        .write_partial(&item("b", "two"), frame - 1)
        .unwrap();
    agent.go_live();
    let (daemon, runtime) = start(&install, "boot-1", quiet_launch()).await;
    let mut subscription = runtime.subscribe(agent.id.as_bytes(), 10).await.unwrap();
    let mut seen = Vec::new();
    drain(&mut subscription, &mut seen, Duration::from_millis(100)).await;
    assert_eq!(
        log(&seen),
        vec![
            "opening g1",
            "snapshot r0 Starting queue=[]",
            "item a o1 r1 \"one\""
        ],
        "a torn frame is not read, and the journal has not ended while one is being written"
    );
    agent.journal().finish_partial().unwrap();
    agent.nudge().await;
    read_until(
        &mut subscription,
        &mut seen,
        "the finished frame and CaughtUp",
        |seen| caught_ups(seen) == 1,
    )
    .await;
    assert_eq!(
        log(&seen[3..]),
        vec!["item b o2 r2 \"two\"", "caught_up r2"]
    );
    agent.die().await;
    crash(daemon, runtime).await;

    // Dead: the agent died mid-frame. The sweep reads what is whole; the
    // agent's next process reopens the journal, which drops the torn bytes.
    let install = Install::new();
    let mut agent = SyntheticAgent::new(&install, "dead", BIG_SEGMENTS);
    agent.register_offline(&install);
    agent.append(&item("a", "one"));
    let frame = journal::encode_frame(&item("b", "two")).len();
    agent
        .journal()
        .write_partial(&item("b", "two"), frame / 2)
        .unwrap();
    let (daemon, runtime) = start(&install, "boot-1", quiet_launch()).await;
    let mut subscription = runtime.subscribe(agent.id.as_bytes(), 10).await.unwrap();
    let mut seen = Vec::new();
    read_until(&mut subscription, &mut seen, "the opening", |seen| {
        caught_ups(seen) == 1
    })
    .await;
    assert_eq!(
        log(&seen),
        vec![
            "opening g1",
            "snapshot r0 Starting queue=[]",
            "item a o1 r1 \"one\"",
            "caught_up r1"
        ],
        "an agent found exited is caught up once its whole frames are ingested"
    );
    agent.reopen_journal();
    write_and_ingest(
        &runtime,
        &mut agent,
        &[item("b", "two"), item("c", "three")],
    )
    .await;
    assert_eq!(
        items_of(&held(&runtime, &agent, &install).await),
        vec![("a", 1, 1, "one"), ("b", 2, 2, "two"), ("c", 3, 3, "three")]
    );
    crash(daemon, runtime).await;

    // A zero-filled tail left in a segment that is already final, as a
    // file system can leave one after power loss: skipped, and the
    // journal continues in the next segment.
    let install = Install::new();
    let mut agent = SyntheticAgent::new(&install, "zeros", 1);
    agent.register_offline(&install);
    agent.append(&item("a", "one"));
    agent.append(&item("b", "two"));
    let segments = journal::segments(&agent.dir.join(agent_dir::JOURNAL)).unwrap();
    assert_eq!(segments.len(), 2, "one frame per segment");
    let first = journal::segment_path(&agent.dir.join(agent_dir::JOURNAL), segments[0]);
    let mut bytes = std::fs::read(&first).unwrap();
    bytes.extend([0u8; 16]);
    std::fs::write(&first, bytes).unwrap();
    let (daemon, runtime) = start(&install, "boot-1", quiet_launch()).await;
    assert_eq!(
        items_of(&held(&runtime, &agent, &install).await),
        vec![("a", 1, 1, "one"), ("b", 2, 2, "two")]
    );
    crash(daemon, runtime).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn each_record_is_broadcast_once_after_its_commit_and_caught_up_once_per_hello() {
    let install = Install::new();
    let mut agent = SyntheticAgent::new(&install, "live", BIG_SEGMENTS);
    agent.register_offline(&install);
    // A backlog written while no daemon ran.
    for step in &steps()[..3] {
        agent.append(step);
    }
    agent.go_live();
    let (daemon, runtime) = start(&install, "boot-1", quiet_launch()).await;
    assert_eq!(agent.hellos(), 1);

    let mut subscription = runtime.subscribe(agent.id.as_bytes(), 10).await.unwrap();
    let mut seen = Vec::new();
    read_until(&mut subscription, &mut seen, "the opening", |seen| {
        caught_ups(seen) == 1
    })
    .await;
    assert_eq!(
        log(&seen),
        vec![
            "opening g1",
            "snapshot r0 Starting queue=[]",
            "item a o1 r1 \"one\"",
            "item b o2 r3 \"two\"",
            "caught_up r3",
        ],
        "the opening: the kind's empty Snapshot, the held rows in their latest state, the marker"
    );

    // Live: one event per committed record, in commit order, each only
    // after the store can serve it.
    let rest = steps();
    for step in &rest[3..6] {
        agent.append(step);
        agent.nudge().await;
    }
    read_until(&mut subscription, &mut seen, "three live records", |seen| {
        seen.len() == 8
    })
    .await;
    for event in &seen[4..] {
        if let Some(session_event::Of::Item(item)) = &event.of {
            let stored = runtime
                .get(&GetRequest {
                    agent_id: agent.id.as_bytes().to_vec(),
                    key: item.key.clone(),
                })
                .await
                .unwrap();
            assert!(
                stored.revision >= item.revision,
                "commit precedes broadcast"
            );
        }
    }
    assert_eq!(
        log(&seen[5..]),
        vec![
            "snapshot r4 Working queue=[\"q1\"]",
            "item c o3 r5 \"three\"",
            "item b o2 r6 \"two, again\"",
        ]
    );

    // A connection that drops with the process alive: the daemon dials
    // again, gets a new Hello, reads what was written meanwhile, and says
    // CaughtUp once more, after those rows.
    for step in &rest[6..] {
        agent.append(step);
    }
    agent.drop_connection().await;
    read_until(
        &mut subscription,
        &mut seen,
        "the second CaughtUp",
        |seen| caught_ups(seen) == 2,
    )
    .await;
    assert_eq!(agent.hellos(), 2);
    assert_eq!(
        log(&seen[8..]),
        vec![
            "snapshot r7 Idle queue=[]",
            "item d o4 r8 \"four\"",
            "caught_up r8"
        ]
    );

    // More Nudges under the same Hello bring rows, never another marker.
    agent.append(&item("e", "five"));
    agent.nudge().await;
    agent.append(&append("e", "!"));
    agent.nudge().await;
    read_until(&mut subscription, &mut seen, "two more records", |seen| {
        seen.len() == 13
    })
    .await;
    drain(&mut subscription, &mut seen, Duration::from_millis(100)).await;
    assert_eq!(
        log(&seen[11..]),
        vec!["item e o5 r9 \"five\"", "append e r10 base r9 \"!\""]
    );

    // The process dies after its marker: nothing more to announce.
    agent.die().await;
    until("the row says exited", || async {
        let lifecycle = runtime
            .store()
            .await
            .agent(&agent.key(&install))
            .unwrap()
            .unwrap()
            .lifecycle;
        (lifecycle == Lifecycle::Exited as i32)
            .then_some(())
            .ok_or_else(|| format!("lifecycle {lifecycle}"))
    })
    .await
    .unwrap();
    drain(&mut subscription, &mut seen, Duration::from_millis(100)).await;
    assert_eq!(
        caught_ups(&seen),
        2,
        "CaughtUp once per Hello: {:#?}",
        log(&seen)
    );
    println!("one subscription across two Hellos: {:#?}", log(&seen));
    crash(daemon, runtime).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_lagging_subscriber_is_closed_with_lagged_and_ingest_never_waits() {
    let install = Install::new();
    let mut agent = SyntheticAgent::new(&install, "flood", BIG_SEGMENTS);
    agent.register_offline(&install);
    let launch = Launch {
        fanout_capacity: 4,
        ..quiet_launch()
    };
    let (daemon, runtime) = start(&install, "boot-1", launch).await;
    let mut stalled = runtime.subscribe(agent.id.as_bytes(), 10).await.unwrap();
    let mut keeping_up = runtime.subscribe(agent.id.as_bytes(), 10).await.unwrap();
    let mut kept = Vec::new();
    read_until(&mut keeping_up, &mut kept, "the opening", |seen| {
        seen.len() == 2
    })
    .await;

    for n in 0..20 {
        agent.append(&item(&format!("k{n:02}"), "x"));
        tokio::time::timeout(Duration::from_secs(5), runtime.ingest(agent.id))
            .await
            .expect("ingest never waits on a subscriber")
            .unwrap();
        read_until(&mut keeping_up, &mut kept, "the live record", |seen| {
            seen.len() == 3 + n
        })
        .await;
    }
    assert_eq!(
        kept.len(),
        22,
        "a reader that keeps up sees every record exactly once"
    );

    let mut seen = Vec::new();
    while let Some(event) = tokio::time::timeout(PATIENCE, stalled.next())
        .await
        .unwrap()
    {
        seen.push((*event).clone());
    }
    assert_eq!(
        log(&seen),
        vec![
            "opening g1",
            "snapshot r0 Starting queue=[]",
            "caught_up r0",
            "lagged"
        ],
        "the opening, then one Lagged, then the stream ends"
    );

    // Re-tailing through the ordinary path reads the store.
    let mut again = runtime.subscribe(agent.id.as_bytes(), 3).await.unwrap();
    let mut seen = Vec::new();
    read_until(&mut again, &mut seen, "the re-tail", |seen| {
        caught_ups(seen) == 1
    })
    .await;
    assert_eq!(
        log(&seen),
        vec![
            "opening g1",
            "snapshot r0 Starting queue=[]",
            "item k17 o18 r18 \"x\"",
            "item k18 o19 r19 \"x\"",
            "item k19 o20 r20 \"x\"",
            "caught_up r20",
        ]
    );
    crash(daemon, runtime).await;
}

/// Installs a join hook that starts an ingest of `agent` and gives it time
/// to reach the store before the subscribe reads its cut. A forced delay:
/// the subscribe holds the store lock through its opening, so the ingest
/// cannot commit until the hook returns and nothing it writes can be
/// waited on; the sleep only makes the contention real rather than
/// absent, and the test's outcome is the same either way.
fn ingest_between_join_and_cut(runtime: &Arc<ProfileRuntime>, agent: uuid::Uuid) {
    let target = Arc::downgrade(runtime);
    let hook: JoinHook = Arc::new(move || {
        let target = target.clone();
        Box::pin(async move {
            tokio::spawn(async move {
                if let Some(runtime) = target.upgrade() {
                    runtime.ingest(agent).await.unwrap();
                }
            });
            tokio::time::sleep(Duration::from_millis(100)).await;
        })
    });
    runtime.set_join_hook(Some(hook));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_snapshot_committed_between_join_and_cut_arrives_once() {
    let install = Install::new();
    let mut agent = SyntheticAgent::new(&install, "cut", BIG_SEGMENTS);
    agent.register_offline(&install);
    let (daemon, runtime) = start(&install, "boot-1", quiet_launch()).await;
    write_and_ingest(&runtime, &mut agent, &[item("a", "one")]).await;

    agent.append(&snapshot(Phase::Working, &[], 2_000));
    ingest_between_join_and_cut(&runtime, agent.id);
    let mut subscription = runtime.subscribe(agent.id.as_bytes(), 10).await.unwrap();
    runtime.set_join_hook(None);
    let mut seen = Vec::new();
    read_until(&mut subscription, &mut seen, "the snapshot", |seen| {
        seen.iter().any(
            |event| matches!(&event.of, Some(session_event::Of::Snapshot(s)) if s.revision == 2),
        )
    })
    .await;
    drain(&mut subscription, &mut seen, Duration::from_millis(200)).await;
    let snapshots = seen
        .iter()
        .filter(
            |event| matches!(&event.of, Some(session_event::Of::Snapshot(s)) if s.revision == 2),
        )
        .count();
    assert_eq!(
        snapshots,
        1,
        "the commit waited for the cut and arrived live, once: {:#?}",
        log(&seen)
    );
    assert_eq!(
        log(&seen),
        vec![
            "opening g1",
            "snapshot r0 Starting queue=[]",
            "item a o1 r1 \"one\"",
            "caught_up r1",
            "snapshot r2 Working queue=[]",
        ]
    );
    crash(daemon, runtime).await;
}

/// The newest snapshot a stream has delivered at each CaughtUp must be
/// the newest the store had committed through that marker's revision.
fn assert_markers_describe_their_snapshot(
    events: &[SessionEvent],
    committed: &[(u64, Vec<String>)],
) {
    let mut holding: Option<u64> = None;
    for event in events {
        match &event.of {
            Some(session_event::Of::Snapshot(s)) => holding = Some(s.revision),
            Some(session_event::Of::CaughtUp(c)) => {
                let newest = committed
                    .iter()
                    .filter(|(revision, _)| *revision <= c.revision)
                    .map(|(revision, _)| *revision)
                    .max();
                assert_eq!(
                    holding,
                    newest,
                    "at CaughtUp r{} the stream holds the snapshot committed through it: {:#?}",
                    c.revision,
                    log(events)
                );
            }
            _ => {}
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_withdrawn_prompt_is_never_read_as_queued() {
    let queued = snapshot(Phase::Working, &["q1"], 2_000);
    let withdrawn = snapshot(Phase::Working, &[], 3_000);
    let committed = vec![(1, vec!["q1".to_owned()]), (2, vec![])];

    // Withdrawn before the subscribe: the opening never shows it queued.
    let install = Install::new();
    let mut agent = SyntheticAgent::new(&install, "withdraw", BIG_SEGMENTS);
    agent.register_offline(&install);
    let (daemon, runtime) = start(&install, "boot-1", quiet_launch()).await;
    write_and_ingest(&runtime, &mut agent, &[queued.clone(), withdrawn.clone()]).await;
    let mut subscription = runtime.subscribe(agent.id.as_bytes(), 10).await.unwrap();
    let mut seen = Vec::new();
    read_until(&mut subscription, &mut seen, "the opening", |seen| {
        caught_ups(seen) == 1
    })
    .await;
    assert_eq!(
        log(&seen),
        vec!["opening g1", "snapshot r2 Working queue=[]", "caught_up r2"]
    );
    assert_markers_describe_their_snapshot(&seen, &committed);
    crash(daemon, runtime).await;

    // Withdrawn while the subscribe is between its join and its cut: the
    // marker read with the cut never claims the withdrawal while the
    // stream still holds the queued snapshot.
    let install = Install::new();
    let mut agent = SyntheticAgent::new(&install, "withdraw", BIG_SEGMENTS);
    agent.register_offline(&install);
    let (daemon, runtime) = start(&install, "boot-1", quiet_launch()).await;
    write_and_ingest(&runtime, &mut agent, &[queued]).await;
    agent.append(&withdrawn);
    ingest_between_join_and_cut(&runtime, agent.id);
    let mut subscription = runtime.subscribe(agent.id.as_bytes(), 10).await.unwrap();
    runtime.set_join_hook(None);
    let mut seen = Vec::new();
    read_until(&mut subscription, &mut seen, "the withdrawal", |seen| {
        seen.len() == 4
    })
    .await;
    drain(&mut subscription, &mut seen, Duration::from_millis(200)).await;
    assert_eq!(
        log(&seen),
        vec![
            "opening g1",
            "snapshot r1 Working queue=[\"q1\"]",
            "caught_up r1",
            "snapshot r2 Working queue=[]",
        ]
    );
    assert_markers_describe_their_snapshot(&seen, &committed);
    crash(daemon, runtime).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn segments_are_deleted_only_below_a_cursor_on_the_drive() {
    let install = Install::new();
    let mut agent = SyntheticAgent::new(&install, "segments", 120);
    agent.register_offline(&install);
    let (daemon, runtime) = start(&install, "boot-1", quiet_launch()).await;
    let journal_dir = agent.dir.join(agent_dir::JOURNAL);

    let many: Vec<Step> = (0..30)
        .map(|n| item(&format!("k{n:02}"), "some text to fill segments"))
        .collect();
    write_and_ingest(&runtime, &mut agent, &many).await;
    let cursor = held(&runtime, &agent, &install).await.cursor;
    let remaining = journal::segments(&journal_dir).unwrap();
    assert!(
        remaining[0] > 0,
        "ingested segments were deleted: {remaining:?}"
    );
    let holding_cursor = remaining
        .iter()
        .rposition(|&start| start <= cursor)
        .unwrap();
    assert!(
        holding_cursor >= node::KEPT_SEGMENTS,
        "the newest {} fully ingested segments are kept: {remaining:?} at cursor {cursor}",
        node::KEPT_SEGMENTS
    );
    assert_eq!(
        journal::reclaimable(&journal_dir, cursor, node::KEPT_SEGMENTS).unwrap(),
        Vec::<u64>::new(),
        "nothing left that could have gone"
    );

    // A commit that fails deletes nothing.
    let before = journal::segments(&journal_dir).unwrap();
    let more: Vec<Step> = (30..36)
        .map(|n| item(&format!("k{n:02}"), "some text to fill segments"))
        .collect();
    for step in &more {
        agent.append(step);
    }
    runtime
        .store()
        .await
        .connection()
        .execute_batch(CURSOR_CUT)
        .unwrap();
    assert!(runtime.ingest(agent.id).await.is_err());
    let after_failure = journal::segments(&journal_dir).unwrap();
    assert!(
        before.iter().all(|start| after_failure.contains(start)),
        "no segment goes below a cursor that was never committed"
    );
    runtime
        .store()
        .await
        .connection()
        .execute_batch("DROP TRIGGER temp.cut;")
        .unwrap();
    runtime.ingest(agent.id).await.unwrap();

    // Power loss now: the store goes back to its last checkpoint. Every
    // deleted segment lay below the cursor that checkpoint holds, so the
    // re-ingest from there finds every frame it needs.
    let lost = held(&runtime, &agent, &install).await;
    let checkpointed = checkpointed_store(&runtime, &install).await;
    crash(daemon, runtime).await;
    lose_power(&install, &checkpointed);
    let (daemon, runtime) = start(&install, "boot-2", quiet_launch()).await;
    assert_eq!(daemon.generation().counter, 2);
    assert_eq!(
        held(&runtime, &agent, &install).await,
        lost,
        "re-ingesting from the durable cursor rebuilt every row with the same revisions"
    );
    crash(daemon, runtime).await;
}

/// The store file as the drive holds it: what was checkpointed, without
/// what the WAL holds beyond that.
async fn checkpointed_store(runtime: &ProfileRuntime, install: &Install) -> Vec<u8> {
    // Held so no checkpoint runs while the file is read.
    let _store = runtime.store().await;
    std::fs::read(install.profile_dir().join(node::STORE)).unwrap()
}

/// The WAL truncated to its last checkpoint: the store file as it was
/// then, and no WAL.
fn lose_power(install: &Install, checkpointed: &[u8]) {
    let db = install.profile_dir().join(node::STORE);
    std::fs::write(&db, checkpointed).unwrap();
    for suffix in ["-wal", "-shm"] {
        let mut path = db.clone().into_os_string();
        path.push(suffix);
        let _ = std::fs::remove_file(path);
    }
}

async fn host_generation(runtime: &ProfileRuntime) -> (u64, i32, i32) {
    let mut inventory = runtime.subscribe_inventory().await.unwrap();
    let mut seen = Vec::new();
    read_inventory_until(&mut inventory, &mut seen, "the host entry", |seen| {
        !seen.is_empty()
    })
    .await;
    match &seen[0].of {
        Some(inventory_event::Of::Host(host)) => (host.generation, host.trust, host.presence),
        other => panic!("the inventory opens with this host, not {other:?}"),
    }
}

/// Up to the moment of the cut: four steps and a checkpoint, then the rest.
async fn run_to_the_cut(
    install: &Install,
    agent: &mut SyntheticAgent,
) -> (Daemon, Arc<ProfileRuntime>, Vec<u64>, Held) {
    let (daemon, runtime) = start(install, "boot-1", quiet_launch()).await;
    let steps = steps();
    let mut offsets = write_and_ingest(&runtime, agent, &steps[..4]).await;
    // The last checkpoint before the power goes: SQLite's own, as a
    // clean shutdown or reclaiming a segment would run it.
    runtime.store().await.flush_to_drive().unwrap();
    offsets.extend(write_and_ingest(&runtime, agent, &steps[4..]).await);
    let before = held(&runtime, agent, install).await;
    (daemon, runtime, offsets, before)
}

#[tokio::test(flavor = "multi_thread")]
async fn power_loss_bumps_the_generation_and_re_ingests_from_the_durable_cursor() {
    let install = Install::new();
    let mut agent = SyntheticAgent::new(&install, "power", BIG_SEGMENTS);
    agent.register_offline(&install);
    let (daemon, runtime, offsets, before) = run_to_the_cut(&install, &mut agent).await;
    assert_eq!(host_generation(&runtime).await.0, 1);

    // The cut: the WAL back to its last checkpoint, the journal cut inside
    // step 6's frame, and the machine back under a new boot id with the
    // clean flag never set.
    let checkpointed = checkpointed_store(&runtime, &install).await;
    crash(daemon, runtime).await;
    lose_power(&install, &checkpointed);
    journal::synthetic::cut(&agent.dir.join(agent_dir::JOURNAL), offsets[5] + 3).unwrap();

    let (daemon, runtime) = start(&install, "boot-2", quiet_launch()).await;
    assert_eq!(
        daemon.generation().counter,
        2,
        "an unclean reboot bumps the generation"
    );
    assert_eq!(
        host_generation(&runtime).await,
        (2, Trust::Trusted as i32, Presence::Online as i32),
        "this host's entry carries the new generation"
    );
    assert_eq!(
        daemon.sweep(install.profile).unwrap().exited,
        vec![agent.id],
        "the sweep re-ingested the agent's journal remainder"
    );
    let after = held(&runtime, &agent, &install).await;
    assert_eq!(
        after.cursor, offsets[5],
        "the cursor rewound to the last whole frame"
    );
    let survived: BTreeMap<_, _> = before
        .items
        .iter()
        .filter(|(key, _)| key.as_str() != "d")
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    assert_eq!(
        after.items, survived,
        "re-ingesting the surviving frames reproduced their revisions exactly"
    );
    assert_eq!(after.snapshot, Some((4, vec!["q1".to_owned()])));
    assert_eq!(after.next_revision, 7);

    // The agent re-derives on resume: the torn frame is dropped when its
    // journal reopens, and what it emits again takes fresh revisions.
    agent.reopen_journal();
    write_and_ingest(
        &runtime,
        &mut agent,
        &[
            snapshot(Phase::Idle, &[], 3_000),
            item("b", "two, again"),
            item("d", "four"),
        ],
    )
    .await;
    assert_eq!(
        items_of(&held(&runtime, &agent, &install).await),
        vec![
            ("a", 1, 1, "one"),
            ("b", 2, 8, "two, again"),
            ("c", 3, 5, "three"),
            ("d", 4, 9, "four"),
        ],
        "re-emitted keys collapse onto their rows"
    );
    println!(
        "after power loss: generation {} and rows {:#?}",
        daemon.generation().counter,
        items_of(&held(&runtime, &agent, &install).await)
    );
    crash(daemon, runtime).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_journal_cut_below_a_durable_cursor_rewinds_it_and_re_derived_steps_commit_anew() {
    let install = Install::new();
    let mut agent = SyntheticAgent::new(&install, "rewind", BIG_SEGMENTS);
    agent.register_offline(&install);
    let (daemon, runtime) = start(&install, "boot-1", quiet_launch()).await;
    let offsets = write_and_ingest(&runtime, &mut agent, &steps()).await;
    // The store reaches the drive after the last step; the journal, never
    // synced, keeps only what the cut leaves of it.
    runtime.store().await.flush_to_drive().unwrap();
    let before = held(&runtime, &agent, &install).await;
    assert_eq!(before.cursor, offsets[7]);
    let checkpointed = checkpointed_store(&runtime, &install).await;
    crash(daemon, runtime).await;
    lose_power(&install, &checkpointed);
    journal::synthetic::cut(&agent.dir.join(agent_dir::JOURNAL), offsets[5] + 3).unwrap();

    let (daemon, runtime) = start(&install, "boot-2", quiet_launch()).await;
    assert_eq!(daemon.generation().counter, 2);
    let after = held(&runtime, &agent, &install).await;
    assert_eq!(
        after.cursor, offsets[5],
        "the cursor rewound to the journal's last whole frame"
    );
    assert_eq!(after.items, before.items, "nothing committed is lost");
    assert_eq!(after.snapshot, before.snapshot);
    assert_eq!(
        after.next_revision, before.next_revision,
        "revisions keep counting up from where they were"
    );

    let mut subscription = runtime.subscribe(agent.id.as_bytes(), 10).await.unwrap();
    let mut opening = Vec::new();
    read_until(&mut subscription, &mut opening, "the opening", |seen| {
        caught_ups(seen) == 1
    })
    .await;
    // The agent re-derives what the journal lost.
    agent.reopen_journal();
    write_and_ingest(
        &runtime,
        &mut agent,
        &[snapshot(Phase::Idle, &[], 3_000), item("d", "four, again")],
    )
    .await;
    let revision = before.next_revision;
    let mut live = Vec::new();
    read_until(
        &mut subscription,
        &mut live,
        "the re-derived steps",
        |seen| seen.len() == 2,
    )
    .await;
    assert_eq!(
        log(&live),
        vec![
            format!("snapshot r{revision} Idle queue=[]"),
            format!("item d o4 r{} \"four, again\"", revision + 1),
        ],
        "the re-derived steps are broadcast under fresh revisions"
    );
    let rederived = held(&runtime, &agent, &install).await;
    assert_eq!(
        rederived.items["d"],
        (4, revision + 1, "four, again".to_owned())
    );
    assert_eq!(rederived.snapshot, Some((revision, Vec::new())));
    crash(daemon, runtime).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_batch_that_fails_on_a_later_frame_commits_and_broadcasts_none_of_it() {
    let install = Install::new();
    let mut agent = SyntheticAgent::new(&install, "batch", BIG_SEGMENTS);
    agent.register_offline(&install);
    let (daemon, runtime) = start(&install, "boot-1", quiet_launch()).await;
    let steps = steps();
    write_and_ingest(&runtime, &mut agent, &steps[..1]).await;
    let mut subscription = runtime.subscribe(agent.id.as_bytes(), 0).await.unwrap();
    let mut opening = Vec::new();
    drain(&mut subscription, &mut opening, Duration::from_millis(20)).await;
    let before = held(&runtime, &agent, &install).await;

    // Four frames read in one batch; the fourth's item write fails.
    for step in &steps[1..5] {
        agent.append(step);
    }
    runtime
        .store()
        .await
        .connection()
        .execute_batch(
            "CREATE TEMP TRIGGER cut BEFORE INSERT ON items WHEN NEW.key = 'c' \
             BEGIN SELECT RAISE(ABORT, 'cut'); END;",
        )
        .unwrap();
    assert!(runtime.ingest(agent.id).await.is_err());
    assert_eq!(
        held(&runtime, &agent, &install).await,
        before,
        "none of the batch's earlier frames committed"
    );
    let mut live = Vec::new();
    drain(&mut subscription, &mut live, Duration::from_millis(50)).await;
    assert!(
        live.is_empty(),
        "nothing of a failed batch is broadcast: {:?}",
        log(&live)
    );

    runtime
        .store()
        .await
        .connection()
        .execute_batch("DROP TRIGGER temp.cut;")
        .unwrap();
    runtime.ingest(agent.id).await.unwrap();
    read_until(&mut subscription, &mut live, "the batch", |seen| {
        seen.len() == 4
    })
    .await;
    assert_eq!(
        log(&live),
        vec![
            "item b o2 r2 \"tw\"",
            "append b r3 base r2 \"o\"",
            "snapshot r4 Working queue=[\"q1\"]",
            "item c o3 r5 \"three\"",
        ],
        "the retried batch commits whole"
    );
    crash(daemon, runtime).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_sweep_ingests_a_finished_childs_journal_before_marking_it_exited() {
    let install = Install::new();
    let parent = SyntheticAgent::new(&install, "parent", BIG_SEGMENTS);
    parent.register_offline(&install);
    let mut child = SyntheticAgent::new(&install, "child", BIG_SEGMENTS);
    child.parent = Some(parent.key(&install));
    child.register_offline(&install);
    // The child finished its turn and exited while no daemon ran.
    child.append(&snapshot(Phase::Working, &[], 2_000));
    child.append(&item("last", "the tests pass"));
    child.append(&turn_end(1, "last"));

    let (daemon, runtime) = start(&install, "boot-1", quiet_launch()).await;
    assert!(
        daemon
            .sweep(install.profile)
            .unwrap()
            .exited
            .contains(&child.id)
    );
    // The parent has exited too, so the outbox keeps what the sweep wrote.
    let deliveries: Vec<_> = runtime
        .store()
        .await
        .deliveries()
        .unwrap()
        .into_iter()
        .map(|row| (wire::EnvelopeKind::try_from(row.kind).unwrap(), row.body))
        .collect();
    assert_eq!(
        deliveries,
        vec![(wire::EnvelopeKind::Finished, "the tests pass".to_owned())],
        "one finished delivery and no failed one"
    );
    let mut subscription = runtime.subscribe(child.id.as_bytes(), 10).await.unwrap();
    let mut opening = Vec::new();
    read_until(&mut subscription, &mut opening, "the opening", |seen| {
        caught_ups(seen) == 1
    })
    .await;
    assert_eq!(
        log(&opening),
        vec![
            "opening g1",
            "snapshot r3 Idle queue=[]",
            "item last o1 r2 \"the tests pass\"",
            "caught_up r3",
        ],
        "CaughtUp carries the last committed revision"
    );
    crash(daemon, runtime).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_daemon_crash_under_the_same_boot_id_bumps_nothing() {
    let install = Install::new();
    let mut agent = SyntheticAgent::new(&install, "crash", BIG_SEGMENTS);
    agent.register_offline(&install);
    let (daemon, runtime, _, before) = run_to_the_cut(&install, &mut agent).await;
    // The page cache survives a daemon crash: the WAL is intact.
    crash(daemon, runtime).await;
    let (daemon, runtime) = start(&install, "boot-1", quiet_launch()).await;
    assert_eq!(daemon.generation().counter, 1);
    assert_eq!(host_generation(&runtime).await.0, 1);
    assert_eq!(held(&runtime, &agent, &install).await, before);
    assert!(
        runtime.ingest(agent.id).await.unwrap().records.is_empty(),
        "nothing is re-ingested"
    );
    crash(daemon, runtime).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_clean_reboot_bumps_nothing() {
    let install = Install::new();
    let mut agent = SyntheticAgent::new(&install, "clean", BIG_SEGMENTS);
    agent.register_offline(&install);
    let (daemon, runtime, _, before) = run_to_the_cut(&install, &mut agent).await;
    drop(runtime);
    daemon.shutdown().await.unwrap();
    let (daemon, runtime) = start(&install, "boot-2", quiet_launch()).await;
    assert_eq!(
        daemon.generation().counter,
        1,
        "the flag was set at shutdown"
    );
    assert_eq!(host_generation(&runtime).await.0, 1);
    assert_eq!(held(&runtime, &agent, &install).await, before);
    assert!(runtime.ingest(agent.id).await.unwrap().records.is_empty());
    crash(daemon, runtime).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn fetch_and_get_are_store_reads() {
    let install = Install::new();
    let mut agent = SyntheticAgent::new(&install, "pages", BIG_SEGMENTS);
    agent.register_offline(&install);
    let (daemon, runtime) = start(&install, "boot-1", quiet_launch()).await;
    write_and_ingest(&runtime, &mut agent, &steps()).await;
    let id = agent.id.as_bytes().to_vec();
    let fetch = |before_order: Option<u64>, limit: u32| FetchRequest {
        agent_id: id.clone(),
        before_order,
        limit,
    };
    let keys = |response: &wire::FetchResponse| {
        response
            .items
            .iter()
            .map(|item| item.key.clone())
            .collect::<Vec<_>>()
    };

    let newest = runtime.fetch(&fetch(None, 2)).await.unwrap();
    assert_eq!(
        (keys(&newest), newest.exhausted),
        (vec!["d".into(), "c".into()], false)
    );
    let older = runtime.fetch(&fetch(Some(3), 2)).await.unwrap();
    assert_eq!(
        (keys(&older), older.exhausted),
        (vec!["b".into(), "a".into()], false)
    );
    let oldest = runtime.fetch(&fetch(Some(1), 2)).await.unwrap();
    assert_eq!((keys(&oldest), oldest.exhausted), (vec![], true));

    let item = runtime
        .get(&GetRequest {
            agent_id: id.clone(),
            key: "b".into(),
        })
        .await
        .unwrap();
    assert_eq!(
        (item.order, item.revision, item.text.as_str()),
        (2, 6, "two, again")
    );
    let missing = runtime
        .get(&GetRequest {
            agent_id: id.clone(),
            key: "zz".into(),
        })
        .await
        .unwrap_err();
    assert!(matches!(missing, ServeError::NoItem(_)));
    assert_eq!(missing.to_wire().code, ErrorCode::NotFound as i32);
    let nobody = runtime
        .fetch(&FetchRequest {
            agent_id: uuid::Uuid::new_v4().as_bytes().to_vec(),
            before_order: None,
            limit: 5,
        })
        .await
        .unwrap_err();
    assert!(matches!(nobody, ServeError::NoAgent));
    crash(daemon, runtime).await;
}

fn describe_inventory(event: &InventoryEvent) -> String {
    match event.of.as_ref().expect("an event") {
        inventory_event::Of::Host(host) => format!(
            "host generation={} trust={:?} presence={:?}",
            host.generation,
            Trust::try_from(host.trust).unwrap(),
            Presence::try_from(host.presence).unwrap()
        ),
        inventory_event::Of::HostRemoved(_) => "host removed".into(),
        inventory_event::Of::Agent(agent) => format!(
            "agent {} {:?} {:?} working_on={:?}",
            agent.name,
            Lifecycle::try_from(agent.lifecycle).unwrap(),
            Phase::try_from(agent.phase).unwrap(),
            agent.working_on.as_deref()
        ),
        inventory_event::Of::AgentRemoved(removed) => {
            format!("agent removed {:?}", removed.reason())
        }
        inventory_event::Of::CaughtUp(_) => "caught_up".into(),
        inventory_event::Of::Lagged(_) => "lagged".into(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_inventory_streams_hosts_and_rows_then_caught_up_then_deltas() {
    let install = Install::new();
    let mut agent = SyntheticAgent::new(&install, "alpha", BIG_SEGMENTS);
    agent.register_offline(&install);
    let (daemon, runtime) = start(&install, "boot-1", quiet_launch()).await;
    let mut inventory = runtime.subscribe_inventory().await.unwrap();
    let mut seen = Vec::new();
    read_inventory_until(&mut inventory, &mut seen, "CaughtUp", |seen| {
        seen.last()
            .is_some_and(|event| matches!(event.of, Some(inventory_event::Of::CaughtUp(_))))
    })
    .await;

    runtime.rename(agent.id, "beta").await.unwrap();
    let git = wire::Git {
        branch: Some("beta".into()),
        base_branch: Some("main".into()),
        uncommitted: Some(wire::ChangeTotals {
            files: 1,
            added: 4,
            removed: 2,
        }),
        on_branch: Some(wire::ChangeTotals {
            files: 3,
            added: 40,
            removed: 9,
        }),
    };
    let mut working = snapshot(Phase::Working, &[], 5_000);
    let envelope = working.snapshot.as_mut().unwrap();
    envelope.phase_since_ms = 4_000;
    envelope.git = Some(git.clone());
    write_and_ingest(&runtime, &mut agent, &[working]).await;
    runtime.delete(agent.id).await.unwrap();
    read_inventory_until(&mut inventory, &mut seen, "the removal", |seen| {
        seen.last()
            .is_some_and(|event| matches!(event.of, Some(inventory_event::Of::AgentRemoved(_))))
    })
    .await;
    let described: Vec<String> = seen.iter().map(describe_inventory).collect();
    assert_eq!(
        described,
        vec![
            "host generation=1 trust=Trusted presence=Online",
            "agent alpha Exited Starting working_on=None",
            "caught_up",
            "agent beta Exited Starting working_on=None",
            "agent beta Exited Working working_on=Some(\"the task\")",
            "agent removed Deleted",
        ]
    );
    if let Some(inventory_event::Of::Agent(row)) = &seen[4].of {
        assert_eq!(
            row.phase_since_ms, 4_000,
            "since when is the snapshot's phase_since_ms, not its at_ms"
        );
        assert_eq!(row.git, Some(git), "git facts are the snapshot's");
    }
    println!("inventory: {described:#?}");
    crash(daemon, runtime).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_lagging_inventory_subscriber_is_told_it_lagged() {
    let install = Install::new();
    let agent = SyntheticAgent::new(&install, "alpha", BIG_SEGMENTS);
    agent.register_offline(&install);
    let launch = Launch {
        inventory_capacity: 2,
        ..quiet_launch()
    };
    let (daemon, runtime) = start(&install, "boot-1", launch).await;
    let mut stalled = runtime.subscribe_inventory().await.unwrap();
    for n in 0..5 {
        runtime.rename(agent.id, &format!("name{n}")).await.unwrap();
    }
    let mut seen = Vec::new();
    while let Some(event) = tokio::time::timeout(PATIENCE, stalled.next())
        .await
        .unwrap()
    {
        seen.push((*event).clone());
    }
    let described: Vec<String> = seen.iter().map(describe_inventory).collect();
    assert_eq!(
        described,
        vec![
            "host generation=1 trust=Trusted presence=Online",
            "agent alpha Exited Starting working_on=None",
            "caught_up",
            "lagged",
        ],
        "the opening, then one Lagged, then the stream ends"
    );
    crash(daemon, runtime).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn resolve_agent_finds_one_name_or_says_why_not() {
    let install = Install::new();
    let agents: Vec<SyntheticAgent> = ["alpha", "beta", "beta"]
        .iter()
        .map(|name| SyntheticAgent::new(&install, name, BIG_SEGMENTS))
        .collect();
    for agent in &agents {
        agent.register_offline(&install);
    }
    let (daemon, runtime) = start(&install, "boot-1", quiet_launch()).await;

    let alpha = runtime.resolve_agent("alpha").await.unwrap();
    assert_eq!(alpha.agent_id, agents[0].id.as_bytes().to_vec());

    let nobody = runtime.resolve_agent("gamma").await.unwrap_err();
    assert!(matches!(nobody, ServeError::NoAgentNamed(_)));
    assert_eq!(nobody.to_wire().code, ErrorCode::NotFound as i32);

    let ambiguous = runtime.resolve_agent("beta").await.unwrap_err();
    let error = ambiguous.to_wire();
    assert_eq!(error.code, ErrorCode::FailedPrecondition as i32);
    assert_eq!(error.details.len(), 1);
    assert_eq!(error.details[0].r#type, AmbiguousAgentName::full_name());
    let detail = AmbiguousAgentName::decode(error.details[0].value.as_slice()).unwrap();
    let mut candidates: Vec<_> = detail
        .candidates
        .iter()
        .map(|a| a.agent_id.clone())
        .collect();
    candidates.sort();
    let mut expected = vec![
        agents[1].id.as_bytes().to_vec(),
        agents[2].id.as_bytes().to_vec(),
    ];
    expected.sort();
    assert_eq!((detail.name.as_str(), candidates), ("beta", expected));
    crash(daemon, runtime).await;
}

// --- retention -------------------------------------------------------------

/// The newest rows retention never trims, shrunk for the test.
const K: u32 = 10;
/// Roughly a kilobyte of text per row, so sizes read plainly.
fn kilobyte(n: usize) -> String {
    format!("{n:04} {}", "x".repeat(1019))
}

struct Named {
    names: BTreeMap<Vec<u8>, String>,
}

impl Named {
    fn of(&self, key: &store::AgentKey) -> &str {
        self.names.get(&key.agent).map_or("?", String::as_str)
    }
}

async fn table(runtime: &ProfileRuntime, named: &Named) -> Vec<String> {
    let store = runtime.store().await;
    let mut rows = store.agents().unwrap();
    rows.sort_by_key(|row| named.of(&row.agent).to_owned());
    let mut lines = vec![format!(
        "  {:<9} {:<8} {:<9} {:>5} {:>9}",
        "agent", "state", "parent", "rows", "activity"
    )];
    for row in rows {
        let held = store.page(&row.agent, None, 10_000).unwrap().items.len();
        let state = if row.lifecycle == Lifecycle::Live as i32 {
            "live"
        } else {
            "exited"
        };
        let activity = match row.last_activity {
            Some(at) if at < 1_000_000 => format!("t={at}"),
            Some(_) => "recent".to_owned(),
            None => "-".to_owned(),
        };
        lines.push(format!(
            "  {:<9} {:<8} {:<9} {:>5} {:>9}",
            named.of(&row.agent),
            state,
            row.parent.as_ref().map_or("-", |parent| named.of(parent)),
            held,
            activity
        ));
    }
    lines.push(format!(
        "  pool: {} KiB of own rows",
        store.pool_bytes(true).unwrap() / 1024
    ));
    lines
}

#[tokio::test(flavor = "multi_thread")]
async fn retention_sweep() {
    use agent_dir::ManualClock;
    use node::StartOptions;
    use store::SweepStep;
    use wire::AgentRef;

    let say = |line: String| println!("{line}");
    let install = Install::new();

    // Two old agents that exited long ago, and one live agent that has run
    // for a long time and is by far the largest.
    let mut old = Vec::new();
    for (name, at) in [("old-1", 1_000), ("old-2", 2_000)] {
        let mut agent = SyntheticAgent::new(&install, name, BIG_SEGMENTS);
        agent.register_offline(&install);
        for n in 0..30 {
            agent.append(&item(&format!("{name}-{n}"), &kilobyte(n)));
        }
        agent.append(&snapshot(Phase::Idle, &[], at));
        old.push(agent);
    }
    let mut big = SyntheticAgent::new(&install, "big", BIG_SEGMENTS);
    big.register_offline(&install);
    for n in 0..100 {
        big.append(&item(&format!("big-{n}"), &kilobyte(n)));
    }
    big.append(&snapshot(Phase::Working, &[], 500_000));
    big.go_live();

    // Real processes for the family, so the kept child can be resumed.
    let clock = ManualClock::new(1_000_000);
    let mut launch = install.launch(
        "retention",
        vec![text("done"), provider_fakes::script::Step::TurnEnd],
    );
    launch.tail_rows = K;
    launch.retention_chunk_bytes = 6 * 1024;
    launch.retention_interval_ms = 60_000;
    let options = StartOptions {
        clock: Arc::new(clock.clone()),
        ..install.options("boot-1", launch.clone())
    };
    let daemon = node::start(options, None).await.unwrap();
    let runtime = runtime(&daemon, &install);
    let parent = id_of(
        &runtime
            .spawn(create(&install.work, "parent", None), None)
            .await
            .unwrap(),
    );
    let mut children = Vec::new();
    for name in ["child-1", "child-2"] {
        children.push(id_of(
            &runtime
                .spawn(create(&install.work, name, Some("go")), Some(parent))
                .await
                .unwrap(),
        ));
    }
    for &child in &children {
        until("each one-shot child to finish and exit", || async {
            let lifecycle = runtime.agent(child).await.unwrap().lifecycle;
            (lifecycle == Lifecycle::Exited as i32)
                .then_some(())
                .ok_or_else(|| format!("lifecycle {lifecycle}"))
        })
        .await
        .unwrap();
    }
    until("the parent to hear from both", || async {
        let rows = runtime.store().await.deliveries().unwrap().len();
        (rows == 0)
            .then_some(())
            .ok_or_else(|| format!("{rows} delivery rows"))
    })
    .await
    .unwrap();

    let named = Named {
        names: old
            .iter()
            .map(|agent| (agent.id.as_bytes().to_vec(), agent.name.clone()))
            .chain([(big.id.as_bytes().to_vec(), "big".to_owned())])
            .chain([(parent.as_bytes().to_vec(), "parent".to_owned())])
            .chain(
                children
                    .iter()
                    .zip(["child-1", "child-2"])
                    .map(|(id, name)| (id.as_bytes().to_vec(), name.to_owned())),
            )
            .collect(),
    };
    let mut retention = runtime.retention();
    assert_eq!(
        retention.borrow().runs,
        1,
        "the sweep ran once at start, under a budget nothing exceeds"
    );
    say("before the sweep:".to_owned());
    for line in table(&runtime, &named).await {
        say(line);
    }

    // Shrink the budget and let the sweep's interval pass.
    let budget = 8 * 1024;
    runtime.set_launch(node::Launch {
        own_budget_bytes: budget,
        ..launch
    });
    say(format!(
        "budget shrunk to {} KiB; chunk {} KiB; protected newest rows K = {K}; clock +60 s",
        budget / 1024,
        6
    ));
    clock.advance(60_000);
    retention.wait_for(|run| run.runs == 2).await.unwrap();
    let sweep = retention.borrow().last.clone().unwrap();
    say(format!(
        "sweep: pool {} KiB -> {} KiB",
        sweep.pool_before / 1024,
        sweep.pool_after / 1024
    ));
    for step in &sweep.steps {
        say(match step {
            SweepStep::Removed { agent, bytes } => format!(
                "  removed {} whole ({} KiB): rows and directory",
                named.of(agent),
                bytes / 1024
            ),
            SweepStep::Trimmed {
                agent,
                rows,
                bytes,
                from_order,
            } => format!(
                "  trimmed {} by {rows} rows ({} KiB); it now starts at order {from_order}",
                named.of(agent),
                bytes / 1024
            ),
            SweepStep::Floor => {
                "  floor: every live agent is at its protected rows; the budget is soft from here"
                    .to_owned()
            }
        });
    }
    say("after the sweep:".to_owned());
    for line in table(&runtime, &named).await {
        say(line);
    }

    // Old exited agents first, least recent first; the children stay; then
    // the largest live agent in chunks down to K; then the floor.
    let removed: Vec<&str> = sweep
        .steps
        .iter()
        .filter_map(|step| match step {
            SweepStep::Removed { agent, .. } => Some(named.of(agent)),
            _ => None,
        })
        .collect();
    assert_eq!(removed, vec!["old-1", "old-2"]);
    assert!(matches!(sweep.steps[0], SweepStep::Removed { .. }));
    let trims: Vec<&str> = sweep
        .steps
        .iter()
        .filter_map(|step| match step {
            SweepStep::Trimmed { agent, .. } => Some(named.of(agent)),
            _ => None,
        })
        .collect();
    assert!(trims.len() > 1, "trimmed a chunk at a time: {trims:?}");
    assert!(trims.iter().all(|name| *name == "big"));
    assert_eq!(sweep.steps.last(), Some(&SweepStep::Floor));
    for agent in &old {
        assert!(!agent.dir.exists(), "{}'s directory is gone", agent.name);
        assert!(runtime.agent(agent.id).await.is_err());
    }
    for &child in &children {
        assert!(
            install.agent_dir(child).exists(),
            "a live parent's child stays"
        );
    }
    let big_row = runtime
        .store()
        .await
        .agent(&big.key(&install))
        .unwrap()
        .unwrap();
    assert_eq!(
        runtime
            .store()
            .await
            .page(&big.key(&install), None, 1_000)
            .unwrap()
            .items
            .len(),
        K as usize,
        "never below the newest K rows"
    );
    assert!(big_row.exhausted && big_row.complete_from_order.is_some());

    // The parent continues a kept child: its message resumes it.
    let child = children[0];
    runtime
        .send_message(
            wire::SendMessageRequest {
                id: b"after-retention".to_vec(),
                to: Some(AgentRef {
                    host_id: runtime.host().as_bytes().to_vec(),
                    agent_id: child.as_bytes().to_vec(),
                }),
                text: "one more thing".into(),
                context: None,
            },
            Some(parent),
        )
        .await
        .expect("the parent's message resumes its kept child");
    let row = runtime.agent(child).await.unwrap();
    assert_eq!(row.incarnation, 2);
    until("the resumed child to finish again", || async {
        let lifecycle = runtime.agent(child).await.unwrap().lifecycle;
        (lifecycle == Lifecycle::Exited as i32)
            .then_some(())
            .ok_or_else(|| format!("lifecycle {lifecycle}"))
    })
    .await
    .unwrap();
    say(format!(
        "the parent sent child-1 a message: it resumed as incarnation {} and finished again",
        row.incarnation
    ));

    // The synthetic agent ignores Stop, and a stop's deadline runs on the
    // test's clock: it goes first, on its own.
    big.die().await;
    until("big's exit", || async {
        let live = runtime.live();
        (!live.contains(&big.id))
            .then_some(())
            .ok_or_else(|| format!("live: {live:?}"))
    })
    .await
    .unwrap();
    kill_all(&runtime).await;
    drop(runtime);
    daemon.shutdown().await.unwrap();
}

/// A journal far ahead of the daemon is committed in batches of at most
/// INGEST_BATCH frames, the store released between them: every frame
/// lands once and in order, and CaughtUp comes once, after the last, never
/// at a batch boundary.
#[tokio::test(flavor = "multi_thread")]
async fn a_backlog_is_committed_in_batches_and_caught_up_once_at_its_end() {
    let install = Install::new();
    let mut agent = SyntheticAgent::new(&install, "backlog", BIG_SEGMENTS);
    agent.register_offline(&install);
    agent.append(&item("first", "row"));
    agent.go_live();
    let (daemon, runtime) = start(&install, "boot-1", quiet_launch()).await;
    let mut subscription = runtime.subscribe(agent.id.as_bytes(), 0).await.unwrap();
    let mut seen = Vec::new();
    read_until(&mut subscription, &mut seen, "the opening", |seen| {
        caught_ups(seen) == 1
    })
    .await;
    seen.clear();
    let frames = node::INGEST_BATCH * 3 + 7;
    for n in 0..frames {
        agent.append(&item(&format!("k{n:05}"), "row"));
    }
    // A new Hello makes CaughtUp due again, at the journal's end.
    agent.drop_connection().await;
    read_until(
        &mut subscription,
        &mut seen,
        "the backlog and CaughtUp",
        |seen| caught_ups(seen) == 1,
    )
    .await;
    let keys: Vec<String> = seen
        .iter()
        .filter_map(|event| match &event.of {
            Some(session_event::Of::Item(item)) => Some(item.key.clone()),
            _ => None,
        })
        .collect();
    let expected: Vec<String> = (0..frames).map(|n| format!("k{n:05}")).collect();
    assert_eq!(keys, expected, "every frame, once, in journal order");
    assert!(
        matches!(
            seen.last().and_then(|event| event.of.as_ref()),
            Some(session_event::Of::CaughtUp(_))
        ),
        "CaughtUp comes after the last row, never between batches"
    );
    drain(&mut subscription, &mut seen, Duration::from_millis(100)).await;
    assert_eq!(caught_ups(&seen), 1);
    agent.die().await;
    crash(daemon, runtime).await;
}
