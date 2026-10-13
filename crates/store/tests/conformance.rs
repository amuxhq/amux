//! The store's reader and writer suite, against a temp-file SQLite store.

use store::{
    Absorb, AgentKey, AgentRow, CommitClock, Cut, Delivery, Marker, PageEnd, Record, SourceEvent,
    Sqlite, StoreError,
};
use wire::{Append, EnvelopeKind, Item, Phase, Snapshot, Step, TurnEnd};

const OWN: &[u8] = b"own-host";
const PEER: &[u8] = b"peer-host";
const CLOCK: CommitClock = CommitClock {
    now_ms: 1_000,
    notify_delay_ms: 30_000,
};

fn own(agent: &str) -> AgentKey {
    AgentKey::new(OWN, agent.as_bytes())
}

fn peer(agent: &str) -> AgentKey {
    AgentKey::new(PEER, agent.as_bytes())
}

fn item(key: &str, text: &str) -> Item {
    Item {
        key: key.into(),
        text: text.into(),
        kind: wire::Kind::ClaudeSdk as i32,
        body: vec![1, 2, 3],
        at_ms: 5,
        producer_version: "1.0.0".into(),
        ..Default::default()
    }
}

/// An item as an origin sends it: order and revision assigned.
fn origin_item(key: &str, order: u64, revision: u64, text: &str) -> Item {
    Item {
        agent_id: b"remote".to_vec(),
        order,
        revision,
        ..item(key, text)
    }
}

fn snapshot(phase: Phase, working_on: Option<&str>, at_ms: i64) -> Snapshot {
    Snapshot {
        kind: wire::Kind::ClaudeSdk as i32,
        body: vec![9],
        phase: phase as i32,
        working_on: working_on.map(str::to_owned),
        at_ms,
        ..Default::default()
    }
}

/// The revision of the snapshot a row holds: the snapshot's own.
fn held_snapshot(row: &AgentRow) -> u64 {
    row.snapshot
        .as_ref()
        .map_or(0, |snapshot| snapshot.revision)
}

fn items_step(items: &[Item]) -> Step {
    Step {
        items: items.to_vec(),
        ..Default::default()
    }
}

fn keys(items: &[Item]) -> Vec<&str> {
    items.iter().map(|item| item.key.as_str()).collect()
}

/// A store in its own temporary directory, which lives as long as it does.
struct Fresh {
    store: Sqlite,
    _dir: tempfile::TempDir,
}

impl std::ops::Deref for Fresh {
    type Target = Sqlite;
    fn deref(&self) -> &Sqlite {
        &self.store
    }
}

impl std::ops::DerefMut for Fresh {
    fn deref_mut(&mut self) -> &mut Sqlite {
        &mut self.store
    }
}

fn fresh() -> Fresh {
    let dir = tempfile::tempdir().unwrap();
    Fresh {
        store: Sqlite::open(&dir.path().join("store.sqlite"), OWN).unwrap(),
        _dir: dir,
    }
}

fn with_agent(mut store: Fresh, agent: &AgentKey) -> Fresh {
    let mut row = AgentRow::new(agent.clone(), wire::Kind::ClaudeSdk, "/src/amux");
    row.name = "worker".into();
    store.put_agent(&row).unwrap();
    store
}

/// What an origin answers a peer that holds everything through a revision:
/// the rows revised since, in revision order, each once in its latest
/// state, while they fit the cap; past it, nothing, and the peer is sent a
/// tail instead.
#[test]
fn after_answers_every_row_revised_since_up_to_the_cap() {
    let store = fresh();
    let agent = own("a");
    let mut store = with_agent(store, &agent);
    store
        .commit(
            &agent,
            &[
                (10, items_step(&[item("m1", "one"), item("m2", "two")])),
                (20, items_step(&[item("m3", "three")])),
                (30, items_step(&[item("m1", "one, revised")])),
            ],
            CLOCK,
        )
        .unwrap();
    // Revisions: m1 1, m2 2, m3 3, then m1 again at 4.
    let since = |store: &Sqlite, revision, cap| {
        store.after(&agent, revision, cap).unwrap().map(|items| {
            items
                .iter()
                .map(|item| (item.key.clone(), item.revision))
                .collect::<Vec<_>>()
        })
    };
    let owned = |pairs: &[(&str, u64)]| {
        Some(
            pairs
                .iter()
                .map(|(key, revision)| ((*key).to_owned(), *revision))
                .collect::<Vec<_>>(),
        )
    };
    assert_eq!(
        since(&store, 1, 10),
        owned(&[("m2", 2), ("m3", 3), ("m1", 4)])
    );
    assert_eq!(since(&store, 2, 2), owned(&[("m3", 3), ("m1", 4)]));
    assert_eq!(
        since(&store, 1, 2),
        None,
        "three rows do not fit a cap of two"
    );
    assert_eq!(since(&store, 4, 0), owned(&[]));
}

#[test]
fn rewind_cursor_moves_only_the_cursor_and_only_back() {
    let store = fresh();
    let agent = own("a");
    let mut store = with_agent(store, &agent);
    store
        .commit(
            &agent,
            &[
                (10, items_step(&[item("m1", "one")])),
                (20, items_step(&[item("m2", "two")])),
            ],
            CLOCK,
        )
        .unwrap();
    store.rewind_cursor(&agent, 10).unwrap();
    let row = store.agent(&agent).unwrap().unwrap();
    assert_eq!((row.ingest_cursor, row.next_revision), (10, 3));
    assert_eq!(store.page(&agent, None, 10).unwrap().items.len(), 2);
    store.rewind_cursor(&agent, 30).unwrap();
    assert_eq!(
        store.cursor(&agent).unwrap(),
        10,
        "a rewind never moves forward"
    );
    assert!(matches!(
        store.rewind_cursor(&peer("r"), 0),
        Err(StoreError::NotOwn)
    ));
}

#[test]
fn commit_assigns_revisions_per_record_and_orders_once_per_key() {
    let store = fresh();
    let agent = own("a");
    let mut store = with_agent(store, &agent);
    let committed = store
        .commit(
            &agent,
            &[
                (10, items_step(&[item("m1", "one"), item("m2", "two")])),
                (
                    20,
                    items_step(&[item("m1", "one, revised"), item("m3", "three")]),
                ),
            ],
            CLOCK,
        )
        .unwrap();
    let assigned = committed
        .records
        .iter()
        .map(|record| match record {
            Record::Item(item) => (item.key.as_str(), item.order, item.revision),
            other => panic!("{other:?}"),
        })
        .collect::<Vec<_>>();
    assert_eq!(
        assigned,
        [("m1", 1, 1), ("m2", 2, 2), ("m1", 1, 3), ("m3", 3, 4)]
    );
    assert_eq!(committed.cursor, 20);

    // A re-derivation after a restart re-emits a known key: same order, a
    // fresh revision; a new key takes the next order.
    let again = store
        .commit(
            &agent,
            &[(30, items_step(&[item("m2", "two"), item("m4", "four")]))],
            CLOCK,
        )
        .unwrap();
    let orders = again
        .records
        .iter()
        .map(|record| match record {
            Record::Item(item) => (item.order, item.revision),
            other => panic!("{other:?}"),
        })
        .collect::<Vec<_>>();
    assert_eq!(orders, [(2, 5), (4, 6)]);
    let stored = store.get(&agent, "m1").unwrap().unwrap();
    assert_eq!((stored.order, stored.revision), (1, 3));
    assert_eq!(stored.text, "one, revised");
    assert_eq!(stored.agent_id, b"a");
    assert_eq!(stored.body, vec![1, 2, 3]);
    assert_eq!(store.agent(&agent).unwrap().unwrap().next_revision, 7);
}

#[test]
fn appends_extend_text_and_carry_their_base() {
    let store = fresh();
    let agent = own("a");
    let mut store = with_agent(store, &agent);
    store
        .commit(&agent, &[(1, items_step(&[item("msg", "Hel")]))], CLOCK)
        .unwrap();
    let step = Step {
        appends: vec![
            Append {
                key: "msg".into(),
                text: "lo w".into(),
                ..Default::default()
            },
            Append {
                key: "missing".into(),
                text: "x".into(),
                ..Default::default()
            },
            Append {
                key: "msg".into(),
                text: "orld".into(),
                ..Default::default()
            },
        ],
        ..Default::default()
    };
    let committed = store.commit(&agent, &[(2, step)], CLOCK).unwrap();
    assert_eq!(committed.skipped_appends, 1);
    assert_eq!(
        committed.records,
        vec![
            Record::Append(Append {
                agent_id: b"a".to_vec(),
                key: "msg".into(),
                base_revision: 1,
                revision: 2,
                text: "lo w".into(),
            }),
            Record::Append(Append {
                agent_id: b"a".to_vec(),
                key: "msg".into(),
                base_revision: 2,
                revision: 3,
                text: "orld".into(),
            }),
        ]
    );
    let held = store.get(&agent, "msg").unwrap().unwrap();
    assert_eq!(
        (held.text.as_str(), held.revision, held.order),
        ("Hello world", 3, 1)
    );
}

#[test]
fn snapshots_copy_their_envelope_onto_the_row() {
    let store = fresh();
    let agent = own("a");
    let mut store = with_agent(store, &agent);
    let git = wire::Git {
        branch: Some("fix-build".into()),
        base_branch: Some("main".into()),
        uncommitted: Some(wire::ChangeTotals {
            files: 2,
            added: 10,
            removed: 3,
        }),
        on_branch: None,
    };
    let step = Step {
        items: vec![item("m1", "hi")],
        snapshot: Some(Snapshot {
            phase_since_ms: 700,
            git: Some(git.clone()),
            background_jobs: 2,
            ..snapshot(Phase::Working, Some("fixing the build"), 777)
        }),
        ..Default::default()
    };
    let committed = store.commit(&agent, &[(5, step)], CLOCK).unwrap();
    let Record::Snapshot(snap) = &committed.records[1] else {
        panic!("{:?}", committed.records)
    };
    assert_eq!(snap.revision, 2);
    assert_eq!(snap.agent_id, b"a");
    let row = store.agent(&agent).unwrap().unwrap();
    assert_eq!(row.phase, Phase::Working as i32);
    assert_eq!(row.working_on.as_deref(), Some("fixing the build"));
    assert_eq!(row.last_activity_ms, Some(777));
    assert_eq!(row.phase_since_ms, Some(700));
    assert_eq!(row.git, Some(git));
    assert_eq!(row.background_jobs, 2);
    assert_eq!(held_snapshot(&row), 2);
    assert_eq!(row.snapshot.as_ref(), Some(snap));
}

/// An own row's live descendants: own children and grandchildren walked
/// into, an exited one not counted but walked through, and a replica child
/// counting itself and what its origin counted below it, never walked into.
#[test]
fn live_descendants_walk_own_rows_and_take_a_replicas_own_count() {
    let mut store = fresh();
    let lead = own("lead");
    let put = |store: &mut Fresh, key: &AgentKey, parent: &AgentKey, live: bool, below: u32| {
        let mut row = AgentRow::new(key.clone(), wire::Kind::ClaudeSdk, "/src/amux");
        row.name = String::from_utf8_lossy(&key.agent).into_owned();
        row.parent = Some(parent.clone());
        row.descendants = below;
        if !live {
            row.lifecycle = wire::Lifecycle::Exited as i32;
        }
        store.put_agent(&row).unwrap();
    };
    store = with_agent(store, &lead);
    put(&mut store, &own("notes"), &lead, true, 0);
    put(&mut store, &own("done"), &lead, false, 0);
    put(&mut store, &own("grandchild"), &own("done"), true, 0);
    put(&mut store, &peer("helper"), &lead, true, 2);
    // The replica's own children are its origin's to count.
    put(&mut store, &peer("helpers-child"), &peer("helper"), true, 0);
    assert_eq!(store.live_descendants(&lead).unwrap(), 5);
    assert_eq!(store.live_descendants(&own("done")).unwrap(), 1);

    assert!(store.set_descendants(&lead, 5).unwrap());
    assert_eq!(store.agent(&lead).unwrap().unwrap().descendants, 5);
    // A registry write leaves the daemon's count alone.
    let mut row = store.agent(&lead).unwrap().unwrap();
    row.descendants = 0;
    store.put_agent(&row).unwrap();
    assert_eq!(store.agent(&lead).unwrap().unwrap().descendants, 5);
}

/// Whether the incarnation still owes its parent its last report: from its
/// start until a turn end marked last, and again should a later turn start.
/// A turn end that is not the last leaves it owed; what the snapshot's
/// phase says at exit does not matter.
#[test]
fn an_incarnation_owes_its_last_report_until_a_turn_end_marked_last() {
    let store = fresh();
    let agent = own("child");
    let mut store = with_agent(store, &agent);
    let owes = |store: &Sqlite| store.agent(&agent).unwrap().unwrap().owes_report;
    assert!(owes(&store), "a new incarnation owes its last report");
    let ended = |phase: Phase, last: bool| Step {
        turn_end: Some(TurnEnd {
            turn_id: 1,
            last_message_key: String::new(),
            outcome: wire::TurnOutcome::Completed as i32,
            last,
        }),
        ..phase_step(phase)
    };
    store
        .commit(&agent, &[(1, phase_step(Phase::Working))], CLOCK)
        .unwrap();
    assert!(owes(&store));
    store
        .commit(&agent, &[(2, ended(Phase::Idle, false))], CLOCK)
        .unwrap();
    assert!(owes(&store), "a turn ended that was not the last");
    store
        .commit(&agent, &[(3, ended(Phase::Idle, true))], CLOCK)
        .unwrap();
    assert!(!owes(&store), "the last turn ended");
    store
        .commit(&agent, &[(4, phase_step(Phase::Starting))], CLOCK)
        .unwrap();
    assert!(!owes(&store), "a provider's exit opens no turn");
    store
        .commit(&agent, &[(5, phase_step(Phase::NeedsYou))], CLOCK)
        .unwrap();
    assert!(owes(&store), "a later turn asks");
    store
        .commit(&agent, &[(6, ended(Phase::Idle, true))], CLOCK)
        .unwrap();
    let mut row = store.agent(&agent).unwrap().unwrap();
    assert!(!row.owes_report);
    row.incarnation += 1;
    store.put_agent(&row).unwrap();
    assert!(owes(&store), "a resumed incarnation owes one again");
}

fn phase_step(phase: Phase) -> Step {
    Step {
        snapshot: Some(snapshot(phase, None, 1)),
        ..Default::default()
    }
}

#[test]
fn a_turn_end_with_a_parent_inserts_a_delivery() {
    let store = fresh();
    let parent = peer("parent");
    let child = own("child");
    let lone = own("lone");
    let mut store = with_agent(with_agent(store, &child), &lone);
    let mut parent_row = AgentRow::new(parent.clone(), wire::Kind::Codex, "/p");
    parent_row.incarnation = 3;
    store.put_agent(&parent_row).unwrap();
    let mut child_row = store.agent(&child).unwrap().unwrap();
    child_row.parent = Some(parent.clone());
    child_row.incarnation = 2;
    store.put_agent(&child_row).unwrap();

    let turn = |key: &str| Step {
        items: vec![item(key, "the answer")],
        turn_end: Some(TurnEnd {
            turn_id: 7,
            last_message_key: key.into(),
            outcome: wire::TurnOutcome::Interrupted as i32,
            last: true,
        }),
        ..Default::default()
    };
    store.commit(&child, &[(1, turn("final"))], CLOCK).unwrap();
    store.commit(&lone, &[(1, turn("final"))], CLOCK).unwrap();
    // The same turn end ingested twice is one row.
    store.commit(&child, &[(2, turn("final"))], CLOCK).unwrap();
    assert_eq!(
        store.deliveries().unwrap(),
        vec![Delivery {
            child_id: b"child".to_vec(),
            incarnation: 2,
            turn_id: 7,
            parent: parent.clone(),
            parent_incarnation: 3,
            kind: EnvelopeKind::Finished as i32,
            body: "the answer".into(),
            ended: None,
            outcome: Some(wire::TurnOutcome::Interrupted as i32),
            last: true,
        }]
    );
    let delivery = store.deliveries().unwrap().remove(0);
    store.remove_delivery(&delivery).unwrap();
    assert!(store.deliveries().unwrap().is_empty());

    // A failed incarnation's row keeps its cause as data.
    let failed = Delivery {
        child_id: b"child".to_vec(),
        incarnation: 2,
        turn_id: 0,
        parent: parent.clone(),
        parent_incarnation: 3,
        kind: EnvelopeKind::Failed as i32,
        body: String::new(),
        ended: Some(wire::IncarnationEnd {
            cause: wire::ExitCause::ProviderExited as i32,
            detail: Some("exit code 1".into()),
        }),
        outcome: None,
        last: true,
    };
    store.put_delivery(&failed).unwrap();
    assert_eq!(store.deliveries().unwrap(), vec![failed]);
}

#[test]
fn turning_to_needs_you_inserts_one_notification_and_leaving_removes_it() {
    let store = fresh();
    let agent = own("a");
    let mut store = with_agent(store, &agent);
    let snap = |phase| Step {
        snapshot: Some(snapshot(phase, Some("deploying"), 9)),
        ..Default::default()
    };
    store
        .commit(
            &agent,
            &[
                (1, items_step(&[item("m1", "May I run the migration?")])),
                (2, snap(Phase::NeedsYou)),
            ],
            CLOCK,
        )
        .unwrap();
    // Still needs you: no second notification for the same transition.
    store
        .commit(&agent, &[(3, snap(Phase::NeedsYou))], CLOCK)
        .unwrap();
    let notifications = store.notifications().unwrap();
    assert_eq!(notifications.len(), 1);
    assert_eq!(notifications[0].revision, 2);
    assert_eq!(notifications[0].due_at_ms, 31_000);
    assert_eq!(
        (
            notifications[0].name.as_deref(),
            notifications[0].working_on.as_deref(),
            notifications[0].text.as_str()
        ),
        (
            Some("worker"),
            Some("deploying"),
            "May I run the migration?"
        )
    );
    store
        .commit(&agent, &[(4, snap(Phase::Working))], CLOCK)
        .unwrap();
    assert!(store.notifications().unwrap().is_empty());
    store
        .commit(&agent, &[(5, snap(Phase::NeedsYou))], CLOCK)
        .unwrap();
    assert_eq!(store.notifications().unwrap().len(), 1);
    store.remove_notifications(b"a").unwrap();
    assert!(store.notifications().unwrap().is_empty());
}

#[test]
fn commit_writes_the_cursor_and_serves_pages_newest_first() {
    let store = fresh();
    let agent = own("a");
    let mut store = with_agent(store, &agent);
    let frames = (1..=7)
        .map(|n| (n * 100, items_step(&[item(&format!("k{n}"), "")])))
        .collect::<Vec<_>>();
    store.commit(&agent, &frames, CLOCK).unwrap();
    assert_eq!(store.cursor(&agent).unwrap(), 700);

    let first = store.page(&agent, None, 3).unwrap();
    assert_eq!(keys(&first.items), ["k7", "k6", "k5"]);
    assert_eq!(first.end, PageEnd::More);
    let second = store.page(&agent, Some(5), 3).unwrap();
    assert_eq!(keys(&second.items), ["k4", "k3", "k2"]);
    let last = store.page(&agent, Some(2), 3).unwrap();
    assert_eq!(keys(&last.items), ["k1"]);
    assert_eq!(last.end, PageEnd::Exhausted);
    assert_eq!(keys(&store.last_n(&agent, 2).unwrap()), ["k6", "k7"]);
    assert!(store.get(&agent, "nope").unwrap().is_none());
}

#[test]
fn commit_refuses_replica_rows_and_unknown_agents() {
    let store = fresh();
    let replica = peer("r");
    let mut store = with_agent(store, &replica);
    assert!(matches!(
        store.commit(&replica, &[(1, items_step(&[item("k", "")]))], CLOCK),
        Err(StoreError::NotOwn)
    ));
    assert!(matches!(
        store.commit(&own("ghost"), &[(1, items_step(&[item("k", "")]))], CLOCK),
        Err(StoreError::UnknownAgent)
    ));
    assert!(store.get(&own("ghost"), "k").unwrap().is_none());
}

#[test]
fn delete_removes_an_agent_whole() {
    let store = fresh();
    let parent = own("parent");
    let child = own("child");
    let mut store = with_agent(with_agent(store, &parent), &child);
    let mut row = store.agent(&child).unwrap().unwrap();
    row.parent = Some(parent.clone());
    store.put_agent(&row).unwrap();
    let step = Step {
        items: vec![item("m", "done")],
        snapshot: Some(snapshot(Phase::NeedsYou, None, 1)),
        turn_end: Some(TurnEnd {
            turn_id: 1,
            last_message_key: "m".into(),
            outcome: wire::TurnOutcome::Completed as i32,
            last: false,
        }),
        ..Default::default()
    };
    store.commit(&child, &[(1, step)], CLOCK).unwrap();
    store.set_marker(&child, Some(Marker::CaughtUp));
    assert_eq!(store.deliveries().unwrap().len(), 1);
    assert_eq!(store.notifications().unwrap().len(), 1);

    store.delete_agent(&child).unwrap();
    assert!(store.agent(&child).unwrap().is_none());
    assert!(store.get(&child, "m").unwrap().is_none());
    assert!(store.deliveries().unwrap().is_empty());
    assert!(store.notifications().unwrap().is_empty());
    assert!(store.agent(&parent).unwrap().is_some());
}

#[test]
fn put_agent_keeps_committed_state() {
    let store = fresh();
    let agent = own("a");
    let mut store = with_agent(store, &agent);
    let step = Step {
        items: vec![item("m", "x")],
        snapshot: Some(snapshot(Phase::Idle, Some("w"), 3)),
        ..Default::default()
    };
    store.commit(&agent, &[(42, step)], CLOCK).unwrap();
    let mut rename = AgentRow::new(agent.clone(), wire::Kind::ClaudeSdk, "/src/amux");
    rename.name = "renamed".into();
    rename.lifecycle = wire::Lifecycle::Exited as i32;
    rename.exit_cause = wire::ExitCause::Stopped as i32;
    rename.exit_detail = Some("its words".into());
    store.put_agent(&rename).unwrap();
    let row = store.agent(&agent).unwrap().unwrap();
    assert_eq!(row.name, "renamed");
    assert_eq!(row.exit_cause, wire::ExitCause::Stopped as i32);
    assert_eq!(row.exit_detail.as_deref(), Some("its words"));
    assert_eq!((row.ingest_cursor, row.next_revision), (42, 3));
    assert_eq!(row.phase, Phase::Idle as i32);
    assert!(row.snapshot.is_some());

    // A resume: the next incarnation has said nothing yet.
    rename.lifecycle = wire::Lifecycle::Live as i32;
    rename.exit_cause = wire::ExitCause::Unspecified as i32;
    rename.exit_detail = None;
    rename.incarnation = 2;
    store.put_agent(&rename).unwrap();
    let row = store.agent(&agent).unwrap().unwrap();
    assert_eq!(row.incarnation, 2);
    assert_eq!(row.phase, Phase::Starting as i32);
    assert_eq!(
        row.working_on.as_deref(),
        Some("w"),
        "working_on survives a resume"
    );
    assert_eq!((row.ingest_cursor, row.next_revision), (42, 3));
    assert!(row.snapshot.is_some());
}

#[test]
fn item_by_input_finds_the_item_an_input_produced() {
    let store = fresh();
    let agent = own("a");
    let mut store = with_agent(store, &agent);
    let accepted = Item {
        input_id: b"envelope-1".to_vec(),
        ..item("message:1", "hello")
    };
    store
        .commit(
            &agent,
            &[(1, items_step(&[item("m", "x"), accepted]))],
            CLOCK,
        )
        .unwrap();
    let found = store.item_by_input(&agent, b"envelope-1").unwrap().unwrap();
    assert_eq!((found.key.as_str(), found.order), ("message:1", 2));
    assert!(
        store
            .item_by_input(&agent, b"envelope-2")
            .unwrap()
            .is_none()
    );
    assert!(
        store.item_by_input(&agent, b"").unwrap().is_none(),
        "an empty input id names nothing"
    );
    assert!(
        store
            .item_by_input(&own("b"), b"envelope-1")
            .unwrap()
            .is_none()
    );
}

#[test]
fn remove_notification_removes_only_that_one() {
    let store = fresh();
    let agent = own("a");
    let mut store = with_agent(store, &agent);
    let snap = |phase| Step {
        snapshot: Some(snapshot(phase, None, 9)),
        ..Default::default()
    };
    store
        .commit(&agent, &[(1, snap(Phase::NeedsYou))], CLOCK)
        .unwrap();
    let sent = store.notifications().unwrap().remove(0);
    store
        .commit(
            &agent,
            &[(2, snap(Phase::Working)), (3, snap(Phase::NeedsYou))],
            CLOCK,
        )
        .unwrap();
    // The first was already removed by the phase change; a later one stays.
    store.remove_notification(&sent).unwrap();
    let left = store.notifications().unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].revision, 3);
    store.remove_notification(&left[0]).unwrap();
    assert!(store.notifications().unwrap().is_empty());
}

#[test]
fn a_reset_replaces_the_block_and_keeps_older_rows_for_get() {
    let store = fresh();
    let agent = peer("r");
    let mut store = with_agent(store, &agent);
    let absorbed = store
        .absorb(
            &agent,
            Absorb::Reset {
                tail: vec![
                    origin_item("k1", 1, 10, "one"),
                    origin_item("k2", 2, 11, "two"),
                ],
                snapshot: Snapshot {
                    revision: 12,
                    ..snapshot(Phase::Idle, None, 50)
                },
                generation: 1,
            },
        )
        .unwrap();
    assert_eq!(absorbed.stored.len(), 3);
    assert_eq!(
        store.agent(&agent).unwrap().unwrap().complete_from_order,
        Some(1)
    );
    assert_eq!(keys(&store.last_n(&agent, 10).unwrap()), ["k1", "k2"]);

    // The origin moved on beyond the cap: a fresh tail replaces the block.
    store
        .absorb(
            &agent,
            Absorb::Reset {
                tail: vec![
                    origin_item("k8", 8, 40, "eight"),
                    origin_item("k9", 9, 41, "nine"),
                ],
                snapshot: Snapshot {
                    revision: 42,
                    ..snapshot(Phase::Working, Some("more"), 60)
                },
                generation: 1,
            },
        )
        .unwrap();
    let row = store.agent(&agent).unwrap().unwrap();
    assert_eq!(row.complete_from_order, Some(8));
    assert_eq!(row.phase, Phase::Working as i32);
    assert_eq!(held_snapshot(&row), 42);
    assert_eq!(keys(&store.last_n(&agent, 10).unwrap()), ["k8", "k9"]);
    let page = store.page(&agent, None, 10).unwrap();
    assert_eq!(keys(&page.items), ["k9", "k8"]);
    assert_eq!(page.end, PageEnd::Boundary);
    // The old rows are held, but not counted.
    assert_eq!(store.get(&agent, "k1").unwrap().unwrap().text, "one");
}

#[test]
fn a_delta_joins_above_and_only_live_records_move_the_cursor() {
    let store = fresh();
    let agent = peer("r");
    let mut store = with_agent(store, &agent);
    store
        .absorb(
            &agent,
            Absorb::Reset {
                tail: vec![origin_item("k1", 1, 10, "one")],
                snapshot: Snapshot {
                    revision: 11,
                    ..snapshot(Phase::Idle, None, 1)
                },
                generation: 1,
            },
        )
        .unwrap();
    // Catch-up records do not move the cursor; CaughtUp does.
    store
        .absorb(
            &agent,
            Absorb::Delta {
                events: vec![SourceEvent::Item(origin_item("k2", 2, 12, "two"))],
                live: false,
            },
        )
        .unwrap();
    assert_eq!(store.cursor(&agent).unwrap(), 0);
    store.absorb(&agent, Absorb::CaughtUp(12)).unwrap();
    assert_eq!(store.cursor(&agent).unwrap(), 12);

    let absorbed = store
        .absorb(
            &agent,
            Absorb::Delta {
                events: vec![
                    SourceEvent::Item(origin_item("k3", 3, 13, "Hel")),
                    SourceEvent::Append(Append {
                        agent_id: b"remote".to_vec(),
                        key: "k3".into(),
                        base_revision: 13,
                        revision: 14,
                        text: "lo".into(),
                    }),
                    // A base the replica does not hold is left for the full
                    // item every stream ends with.
                    SourceEvent::Append(Append {
                        agent_id: b"remote".to_vec(),
                        key: "k3".into(),
                        base_revision: 99,
                        revision: 100,
                        text: "??".into(),
                    }),
                    SourceEvent::Snapshot(Snapshot {
                        revision: 15,
                        ..snapshot(Phase::NeedsYou, Some("asking"), 70)
                    }),
                ],
                live: true,
            },
        )
        .unwrap();
    assert_eq!(absorbed.stored.len(), 3);
    assert_eq!(store.cursor(&agent).unwrap(), 15);
    assert_eq!(store.get(&agent, "k3").unwrap().unwrap().text, "Hello");
    assert_eq!(keys(&store.last_n(&agent, 10).unwrap()), ["k1", "k2", "k3"]);
    let row = store.agent(&agent).unwrap().unwrap();
    assert_eq!(row.phase, Phase::NeedsYou as i32);
    assert_eq!(row.working_on.as_deref(), Some("asking"));
    // Replicas never enqueue notifications: that is the origin's outbox.
    assert!(store.notifications().unwrap().is_empty());
}

#[test]
fn older_revisions_never_overwrite_newer() {
    let store = fresh();
    let agent = peer("r");
    let mut store = with_agent(store, &agent);
    store
        .absorb(
            &agent,
            Absorb::Reset {
                tail: vec![origin_item("k1", 1, 20, "new")],
                snapshot: Snapshot {
                    revision: 30,
                    ..snapshot(Phase::Idle, Some("newer"), 1)
                },
                generation: 1,
            },
        )
        .unwrap();
    let absorbed = store
        .absorb(
            &agent,
            Absorb::Delta {
                events: vec![
                    SourceEvent::Item(origin_item("k1", 1, 19, "old")),
                    SourceEvent::Snapshot(Snapshot {
                        revision: 29,
                        ..snapshot(Phase::Working, Some("older"), 1)
                    }),
                ],
                live: true,
            },
        )
        .unwrap();
    assert!(absorbed.stored.is_empty());
    assert_eq!(store.get(&agent, "k1").unwrap().unwrap().text, "new");
    let row = store.agent(&agent).unwrap().unwrap();
    assert_eq!(row.working_on.as_deref(), Some("newer"));
    assert_eq!(row.source_cursor, 0);
}

#[test]
fn pages_extend_the_block_downwards_only_when_they_join() {
    let store = fresh();
    let agent = peer("r");
    let mut store = with_agent(store, &agent);
    store
        .absorb(
            &agent,
            Absorb::Reset {
                tail: vec![origin_item("k5", 5, 50, ""), origin_item("k6", 6, 60, "")],
                snapshot: Snapshot {
                    revision: 61,
                    ..Default::default()
                },
                generation: 1,
            },
        )
        .unwrap();
    // A page for some other boundary does not join and is not stored.
    let stray = store
        .absorb(
            &agent,
            Absorb::Page {
                before_order: 3,
                items: vec![origin_item("k2", 2, 20, "")],
                exhausted: false,
            },
        )
        .unwrap();
    assert!(!stray.joined);
    assert!(store.get(&agent, "k2").unwrap().is_none());

    let joined = store
        .absorb(
            &agent,
            Absorb::Page {
                before_order: 5,
                items: vec![origin_item("k4", 4, 40, ""), origin_item("k3", 3, 30, "")],
                exhausted: false,
            },
        )
        .unwrap();
    assert!(joined.joined);
    let row = store.agent(&agent).unwrap().unwrap();
    assert_eq!(row.complete_from_order, Some(3));
    assert!(!row.exhausted);
    // A page never moves the source cursor.
    assert_eq!(row.source_cursor, 0);
    let page = store.page(&agent, None, 10).unwrap();
    assert_eq!(keys(&page.items), ["k6", "k5", "k4", "k3"]);
    assert_eq!(page.end, PageEnd::Boundary);

    store
        .absorb(
            &agent,
            Absorb::Page {
                before_order: 3,
                items: vec![origin_item("k2", 2, 20, ""), origin_item("k1", 1, 10, "")],
                exhausted: true,
            },
        )
        .unwrap();
    let row = store.agent(&agent).unwrap().unwrap();
    assert_eq!((row.complete_from_order, row.exhausted), (Some(1), true));
    let page = store.page(&agent, Some(3), 10).unwrap();
    assert_eq!(keys(&page.items), ["k2", "k1"]);
    assert_eq!(page.end, PageEnd::Exhausted);

    // A Reset forgets that the origin was exhausted below the new block.
    store
        .absorb(
            &agent,
            Absorb::Reset {
                tail: vec![origin_item("k7", 7, 70, "")],
                snapshot: Snapshot::default(),
                generation: 1,
            },
        )
        .unwrap();
    let row = store.agent(&agent).unwrap().unwrap();
    assert_eq!((row.complete_from_order, row.exhausted), (Some(7), false));
}

#[test]
fn a_replica_without_a_block_serves_nothing() {
    let store = fresh();
    let agent = peer("r");
    let mut store = with_agent(store, &agent);
    // Rows a Get or stray delta left behind do not make a block.
    store
        .absorb(
            &agent,
            Absorb::Delta {
                events: vec![SourceEvent::Item(origin_item("k1", 1, 1, ""))],
                live: false,
            },
        )
        .unwrap();
    assert!(store.last_n(&agent, 10).unwrap().is_empty());
    let page = store.page(&agent, None, 10).unwrap();
    assert!(page.items.is_empty());
    assert_eq!(page.end, PageEnd::Boundary);
    assert!(store.get(&agent, "k1").unwrap().is_some());
    let page = store
        .absorb(
            &agent,
            Absorb::Page {
                before_order: 1,
                items: vec![],
                exhausted: true,
            },
        )
        .unwrap();
    assert!(!page.joined);
}

#[test]
fn cut_reads_snapshot_rows_and_marker_together() {
    let store = fresh();
    let agent = own("a");
    let mut store = with_agent(store, &agent);
    assert_eq!(store.cut(&agent, 5).unwrap(), Cut::default());
    let step = Step {
        items: vec![item("k1", ""), item("k2", ""), item("k3", "")],
        snapshot: Some(snapshot(Phase::Idle, None, 1)),
        ..Default::default()
    };
    store.commit(&agent, &[(1, step)], CLOCK).unwrap();
    let cut = store.cut(&agent, 2).unwrap();
    assert_eq!(cut.snapshot.unwrap().revision, 4);
    assert_eq!(keys(&cut.held), ["k2", "k3"]);
    assert_eq!(cut.marker, None);
    store.set_marker(&agent, Some(Marker::CaughtUp));
    assert_eq!(store.cut(&agent, 2).unwrap().marker, Some(Marker::CaughtUp));

    let replica = peer("r");
    let mut store = with_agent(store, &replica);
    store.absorb(&replica, Absorb::CaughtUp(3)).unwrap();
    assert_eq!(
        store.cut(&replica, 2).unwrap().marker,
        Some(Marker::CaughtUp)
    );
    store.set_marker(&replica, Some(Marker::Detached));
    assert_eq!(
        store.cut(&replica, 2).unwrap().marker,
        Some(Marker::Detached)
    );
    store
        .absorb(
            &replica,
            Absorb::Reset {
                tail: vec![],
                snapshot: Snapshot::default(),
                generation: 1,
            },
        )
        .unwrap();
    assert_eq!(store.cut(&replica, 2).unwrap().marker, None);
    assert!(store.cut(&own("ghost"), 1).is_err());
}

/// A block belongs to the generation its Reset was taken under. A Reset
/// under the same generation keeps the older rows for Get; one under
/// another forgets everything held, because the origin numbers orders and
/// revisions afresh: a lower order and a lower revision land where higher
/// ones were, and a snapshot older by number is the newer one.
#[test]
fn a_reset_under_another_generation_forgets_every_held_row() {
    let store = fresh();
    let agent = peer("r");
    let mut store = with_agent(store, &agent);
    store
        .absorb(
            &agent,
            Absorb::Reset {
                tail: vec![
                    origin_item("k5", 5, 50, "five"),
                    origin_item("k6", 6, 60, "six"),
                    origin_item("k7", 7, 70, "seven"),
                ],
                snapshot: Snapshot {
                    revision: 71,
                    ..Default::default()
                },
                generation: 1,
            },
        )
        .unwrap();
    store.absorb(&agent, Absorb::CaughtUp(71)).unwrap();
    let row = store.agent(&agent).unwrap().unwrap();
    assert_eq!((row.source_cursor, row.source_generation), (71, 1));

    store
        .absorb(
            &agent,
            Absorb::Reset {
                tail: vec![
                    origin_item("k5", 5, 50, "five again"),
                    origin_item("k6", 6, 61, ""),
                ],
                snapshot: Snapshot {
                    revision: 62,
                    ..Default::default()
                },
                generation: 2,
            },
        )
        .unwrap();
    let row = store.agent(&agent).unwrap().unwrap();
    assert_eq!(row.source_generation, 2);
    assert_eq!(held_snapshot(&row), 62);
    assert_eq!(row.complete_from_order, Some(5));
    assert_eq!(keys(&store.cut(&agent, 10).unwrap().held), ["k5", "k6"]);
    assert_eq!(store.get(&agent, "k7").unwrap(), None);
    assert_eq!(
        store.get(&agent, "k5").unwrap().map(|item| item.text),
        Some("five again".to_owned())
    );
    store.absorb(&agent, Absorb::CaughtUp(62)).unwrap();
    assert_eq!(store.agent(&agent).unwrap().unwrap().source_cursor, 62);

    // The envelope too, even at revision 0: a rewound origin that has not
    // committed a snapshot since serves the one it holds, and the old
    // generation's phase must not outlive its rows.
    let mut row = store.agent(&agent).unwrap().unwrap();
    row.phase = Phase::Working as i32;
    row.working_on = Some("old generation".into());
    store.put_agent(&row).unwrap();
    store
        .absorb(
            &agent,
            Absorb::Reset {
                tail: vec![],
                snapshot: snapshot(Phase::Idle, None, 7),
                generation: 3,
            },
        )
        .unwrap();
    let row = store.agent(&agent).unwrap().unwrap();
    assert_eq!(row.source_generation, 3);
    assert_eq!(row.phase, Phase::Idle as i32);
    assert_eq!(row.working_on, None);
    assert_eq!(held_snapshot(&row), 0);
}

#[test]
fn a_hosts_generation_is_recorded_as_its_inventory_reports_it() {
    let store = fresh();
    let theirs = peer("r1");
    let mut store = with_agent(store, &theirs);
    store
        .absorb(
            &theirs,
            Absorb::Reset {
                tail: vec![origin_item("k", 1, 1, "")],
                snapshot: Snapshot::default(),
                generation: 1,
            },
        )
        .unwrap();
    assert_eq!(store.host_generation(PEER).unwrap(), None);
    store.record_host_generation(PEER, 4).unwrap();
    assert_eq!(store.host_generation(PEER).unwrap(), Some(4));
    store.record_host_generation(PEER, 5).unwrap();
    assert_eq!(store.host_generation(PEER).unwrap(), Some(5));
    // The replicas are the sources' to reset, stream by stream.
    assert!(store.agent(&theirs).unwrap().is_some());
    assert!(store.get(&theirs, "k").unwrap().is_some());
    assert!(matches!(
        store.record_host_generation(OWN, 1),
        Err(StoreError::OwnHost)
    ));
}

#[test]
fn absorb_refuses_own_rows() {
    let store = fresh();
    let agent = own("a");
    let mut store = with_agent(store, &agent);
    assert!(matches!(
        store.absorb(&agent, Absorb::CaughtUp(1)),
        Err(StoreError::NotReplica)
    ));
    assert!(matches!(
        store.absorb(&peer("ghost"), Absorb::CaughtUp(1)),
        Err(StoreError::UnknownAgent)
    ));
}

/// The lookups that answer for one agent id, a parent's children and a
/// name read only the rows they name.
#[test]
fn agents_are_found_by_id_parent_and_name() {
    let parent = own("parent");
    let mut store = with_agent(with_agent(fresh(), &parent), &peer("cousin"));
    let mut child = AgentRow::new(own("child"), wire::Kind::Codex, "/");
    child.name = "helper".into();
    child.parent = Some(parent.clone());
    store.put_agent(&child).unwrap();
    let mut remote_child = AgentRow::new(peer("remote-child"), wire::Kind::Codex, "/");
    remote_child.name = "helper".into();
    remote_child.parent = Some(parent.clone());
    store.put_agent(&remote_child).unwrap();
    // The same id on two hosts: this host's own row answers.
    let mut shadow = AgentRow::new(
        AgentKey::new(PEER, b"parent".to_vec()),
        wire::Kind::Codex,
        "/",
    );
    shadow.name = "shadow".into();
    store.put_agent(&shadow).unwrap();

    let by_id = |store: &Sqlite, id: &[u8]| store.agent_by_id(id).unwrap().map(|row| row.agent);
    assert_eq!(by_id(&store, b"parent"), Some(parent.clone()));
    assert_eq!(by_id(&store, b"remote-child"), Some(peer("remote-child")));
    assert_eq!(by_id(&store, b"nobody"), None);

    let children = store.agents_of_parent(&parent).unwrap();
    let mut keys = children
        .iter()
        .map(|row| row.agent.clone())
        .collect::<Vec<_>>();
    keys.sort();
    assert_eq!(keys, [own("child"), peer("remote-child")]);
    assert!(store.agents_of_parent(&own("child")).unwrap().is_empty());

    let named = store.agent_by_name("helper").unwrap();
    assert_eq!(named.len(), 2, "one name on two hosts is two rows");
    assert_eq!(store.agent_by_name("worker").unwrap().len(), 2);
    assert!(store.agent_by_name("Helper").unwrap().is_empty());
}

/// A replica's inventory row carries no last activity, so putting it again
/// keeps the one its newest absorbed snapshot gave the row.
#[test]
fn an_inventory_put_keeps_a_replicas_last_activity() {
    let agent = peer("r");
    let mut store = with_agent(fresh(), &agent);
    store
        .absorb(
            &agent,
            Absorb::Reset {
                tail: vec![],
                snapshot: Snapshot {
                    revision: 3,
                    ..snapshot(Phase::Idle, None, 4_000)
                },
                generation: 1,
            },
        )
        .unwrap();
    let mut listed = AgentRow::new(agent.clone(), wire::Kind::ClaudeSdk, "/src/amux");
    listed.name = "worker".into();
    listed.phase = Phase::Working as i32;
    listed.phase_since_ms = Some(4_500);
    store.put_agent(&listed).unwrap();
    let row = store.agent(&agent).unwrap().unwrap();
    assert_eq!(row.last_activity_ms, Some(4_000));
    assert_eq!(
        (row.phase, row.phase_since_ms),
        (Phase::Working as i32, Some(4_500))
    );
}

/// Retention's byte count in SQL and `item_bytes` agree on every row,
/// whatever its kind, text, attachments and input id.
#[test]
fn item_bytes_agrees_with_the_stores_count() {
    let agent = own("a");
    let mut store = with_agent(fresh(), &agent);
    let mut odd = item("é-key", "non-ascii text: ünïcödé");
    odd.kind = 99;
    odd.input_id = b"input".to_vec();
    odd.attachments = vec![wire::Attachment {
        of: Some(wire::attachment::Of::Image(wire::BlobRef {
            hash: vec![7; 32],
            name: "shot.png".into(),
            mime: "image/png".into(),
            size: 12,
        })),
    }];
    store
        .commit(
            &agent,
            &[(1, items_step(&[item("plain", "x"), odd]))],
            CLOCK,
        )
        .unwrap();
    let held = store.page(&agent, None, 10).unwrap().items;
    assert_eq!(held.len(), 2);
    assert_eq!(
        store.pool_bytes(true).unwrap(),
        held.iter().map(store::item_bytes).sum::<u64>()
    );
}
