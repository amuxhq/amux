//! Retention against a temp-file SQLite store, on a driven clock: every
//! time here is a snapshot's at_ms or a value handed to the sweep.

use std::collections::{HashMap, HashSet};

use store::{
    Absorb, AgentKey, AgentRow, BlobLru, CommitClock, ITEM_OVERHEAD_BYTES, KIND_BYTES, PageEnd,
    Sqlite, Sweep, SweepStep,
};
use wire::{Item, Lifecycle, Phase, Snapshot, Step, TurnEnd};

const OWN: &[u8] = b"own-host";
const PEER: &[u8] = b"peer-host";
const CLOCK: CommitClock = CommitClock {
    now_ms: 0,
    notify_delay_ms: 0,
};
/// Text bytes per row; each row then costs ROW bytes.
const TEXT: usize = 1000;
const ROW: u64 = 2 + TEXT as u64 + KIND_BYTES + ITEM_OVERHEAD_BYTES;

fn own(name: &str) -> AgentKey {
    AgentKey::new(OWN, name.as_bytes())
}

fn peer(name: &str) -> AgentKey {
    AgentKey::new(PEER, name.as_bytes())
}

/// An own agent with `rows` rows of ROW bytes, last active at `at_ms`.
fn add(
    store: &mut Sqlite,
    agent: &AgentKey,
    rows: usize,
    at_ms: i64,
    live: bool,
    parent: Option<&AgentKey>,
) {
    let mut row = AgentRow::new(agent.clone(), wire::Kind::Codex, "/src");
    row.parent = parent.cloned();
    store.put_agent(&row).unwrap();
    let items = (0..rows)
        .map(|n| Item {
            key: format!("{n:02}"),
            text: "x".repeat(TEXT),
            kind: wire::Kind::Codex as i32,
            ..Default::default()
        })
        .collect();
    let step = Step {
        items,
        snapshot: Some(Snapshot {
            phase: Phase::Idle as i32,
            at_ms,
            ..Default::default()
        }),
        ..Default::default()
    };
    store.commit(agent, &[(1, step)], CLOCK).unwrap();
    if !live {
        let mut row = store.agent(agent).unwrap().unwrap();
        row.lifecycle = Lifecycle::Exited as i32;
        store.put_agent(&row).unwrap();
    }
}

/// A replica with rows at orders 1..=rows, as a source's Reset left it.
fn replica(store: &mut Sqlite, agent: &AgentKey, rows: u64, at_ms: i64) {
    store
        .put_agent(&AgentRow::new(agent.clone(), wire::Kind::Codex, "/src"))
        .unwrap();
    let tail = (1..=rows)
        .map(|order| Item {
            key: format!("{order:02}"),
            order,
            revision: order,
            text: "x".repeat(TEXT),
            kind: wire::Kind::Codex as i32,
            ..Default::default()
        })
        .collect();
    store
        .absorb(
            agent,
            Absorb::Reset {
                tail,
                snapshot: Snapshot {
                    revision: rows + 1,
                    at_ms,
                    ..Default::default()
                },
                generation: 1,
            },
        )
        .unwrap();
}

fn name(agent: &AgentKey) -> String {
    String::from_utf8_lossy(&agent.agent).into_owned()
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

fn describe(sweep: &Sweep) -> Vec<String> {
    sweep
        .steps
        .iter()
        .map(|step| match step {
            SweepStep::Removed { agent, bytes } => {
                format!("removed {} whole ({bytes} bytes)", name(agent))
            }
            SweepStep::Trimmed {
                agent,
                rows,
                bytes,
                from_order,
            } => format!(
                "trimmed {} by {rows} rows ({bytes} bytes); history now starts at order {from_order}",
                name(agent)
            ),
            SweepStep::Floor => "every live agent is at its protected rows; stopping".into(),
        })
        .collect()
}

/// A Reset whose tail is smaller than the floor, over stale rows: the
/// stale rows are not the block's, so they go whole and the block, under
/// the floor, is never trimmed. Counted as the block's oldest rows they
/// would lift it over the floor and the boundary would land among them,
/// below the hole, claiming history the replica does not hold.
#[test]
fn retention_of_a_replica_block_under_the_floor_takes_only_the_stale_rows_below_it() {
    let mut store = fresh();
    let followed = peer("followed");
    // Orders 1..=4, then a Reset whose tail is orders 13..=14.
    replica(&mut store, &followed, 4, 100);
    let tail = (13..=14)
        .map(|order| Item {
            key: format!("{order:02}"),
            order,
            revision: 20 + order,
            text: "x".repeat(TEXT),
            kind: wire::Kind::Codex as i32,
            ..Default::default()
        })
        .collect();
    store
        .absorb(
            &followed,
            Absorb::Reset {
                tail,
                snapshot: Snapshot {
                    revision: 40,
                    at_ms: 200,
                    ..Default::default()
                },
                generation: 1,
            },
        )
        .unwrap();
    assert_eq!(store.pool_bytes(false).unwrap(), 6 * ROW);
    let sourced = HashSet::from([followed.clone()]);
    let block = |store: &Sqlite| {
        let row = store.agent(&followed).unwrap().unwrap();
        let page = store.page(&followed, None, 10).unwrap();
        let orders: Vec<u64> = page.items.iter().map(|item| item.order).collect();
        (row.complete_from_order, orders, page.end)
    };

    // A budget the stale rows alone satisfy.
    let sweep = store
        .sweep_replicas(2 * ROW, 3, &sourced, &HashMap::new())
        .unwrap();
    assert_eq!(
        describe(&sweep),
        [format!(
            "trimmed followed by 4 rows ({} bytes); history now starts at order 13",
            4 * ROW
        )],
        "the stale rows go whole, and the block is left alone"
    );
    assert_eq!(
        block(&store),
        (Some(13), vec![14, 13], PageEnd::Boundary),
        "the boundary stays on the block and paging down skips nothing"
    );

    // No budget: a block under the floor still keeps every row.
    let sweep = store
        .sweep_replicas(0, 3, &sourced, &HashMap::new())
        .unwrap();
    assert_eq!(
        describe(&sweep),
        ["every live agent is at its protected rows; stopping"]
    );
    assert_eq!(block(&store), (Some(13), vec![14, 13], PageEnd::Boundary));
}

/// A Reset leaves the old block's rows below the new one: stored for Get,
/// not history. Trimming takes them first and whole, then trims the block
/// itself, and the boundary only ever lands on a row inside the block, so
/// paging down from the top never skips an order.
#[test]
fn retention_of_a_replica_drops_rows_a_reset_left_below_the_block_before_trimming_it() {
    let mut store = fresh();
    let followed = peer("followed");
    // Orders 1..=4, then a Reset whose tail is orders 9..=14: 5..=8 are
    // missing, and 1..=4 are stale rows below the new block.
    replica(&mut store, &followed, 4, 100);
    let tail = (9..=14)
        .map(|order| Item {
            key: format!("{order:02}"),
            order,
            revision: 20 + order,
            text: "x".repeat(TEXT),
            kind: wire::Kind::Codex as i32,
            ..Default::default()
        })
        .collect();
    store
        .absorb(
            &followed,
            Absorb::Reset {
                tail,
                snapshot: Snapshot {
                    revision: 40,
                    at_ms: 200,
                    ..Default::default()
                },
                generation: 1,
            },
        )
        .unwrap();
    assert_eq!(store.pool_bytes(false).unwrap(), 10 * ROW);
    let sourced = HashSet::from([followed.clone()]);

    let sweep = store
        .sweep_replicas(8 * ROW, 3, &sourced, &HashMap::new())
        .unwrap();
    assert_eq!(
        describe(&sweep),
        [format!(
            "trimmed followed by 4 rows ({} bytes); history now starts at order 9",
            4 * ROW
        )],
        "the stale rows go first and whole, and that is enough"
    );
    assert!(store.get(&followed, "01").unwrap().is_none());

    let sweep = store
        .sweep_replicas(0, 3, &sourced, &HashMap::new())
        .unwrap();
    assert_eq!(
        describe(&sweep),
        [
            format!(
                "trimmed followed by 3 rows ({} bytes); history now starts at order 12",
                3 * ROW
            ),
            "every live agent is at its protected rows; stopping".to_owned(),
        ]
    );
    let row = store.agent(&followed).unwrap().unwrap();
    assert_eq!((row.complete_from_order, row.exhausted), (Some(12), false));
    let page = store.page(&followed, None, 10).unwrap();
    let orders: Vec<u64> = page.items.iter().map(|item| item.order).collect();
    assert_eq!(orders, [14, 13, 12], "paging down yields no gap");
    assert_eq!(page.end, PageEnd::Boundary);
}

/// The accepted scenario: a profile over its budget with a live parent,
/// its finished children and old exited agents. The old exited agents go
/// first, the children stay, the largest live agent is trimmed in chunks
/// down to its protected rows, and the parent can still resume a child.
#[test]
fn retention_sweep_order() {
    let mut store = fresh();
    let parent = own("parent");
    let children = [own("child-1"), own("child-2")];
    // Oldest activity of all: they would go first if they were not live work.
    add(&mut store, &children[0], 5, 100, false, Some(&parent));
    add(&mut store, &children[1], 5, 200, false, Some(&parent));
    add(&mut store, &own("old-exited-1"), 5, 300, false, None);
    add(&mut store, &own("old-exited-2"), 5, 400, false, None);
    add(&mut store, &parent, 20, 900, true, None);
    add(&mut store, &own("small-live"), 3, 950, true, None);
    let pool = store.pool_bytes(true).unwrap();
    assert_eq!(pool, 43 * ROW);

    // A budget the protected rows alone exceed, so the sweep reaches the
    // floor; a chunk of three rows' worth; every live agent keeps its
    // newest 4 rows.
    let budget = 15 * ROW;
    let sweep = store.sweep_own(budget, 3 * ROW, 4).unwrap();
    let described = describe(&sweep);
    println!(
        "pool {} bytes over a budget of {budget}:",
        sweep.pool_before
    );
    for line in &described {
        println!("  {line}");
    }
    println!("pool after: {} bytes", sweep.pool_after);

    assert_eq!(
        described,
        [
            format!("removed old-exited-1 whole ({} bytes)", 5 * ROW),
            format!("removed old-exited-2 whole ({} bytes)", 5 * ROW),
            format!(
                "trimmed parent by 3 rows ({} bytes); history now starts at order 4",
                3 * ROW
            ),
            format!(
                "trimmed parent by 3 rows ({} bytes); history now starts at order 7",
                3 * ROW
            ),
            format!(
                "trimmed parent by 3 rows ({} bytes); history now starts at order 10",
                3 * ROW
            ),
            format!(
                "trimmed parent by 3 rows ({} bytes); history now starts at order 13",
                3 * ROW
            ),
            format!(
                "trimmed parent by 3 rows ({} bytes); history now starts at order 16",
                3 * ROW
            ),
            format!("trimmed parent by 1 rows ({ROW} bytes); history now starts at order 17"),
            "every live agent is at its protected rows; stopping".to_owned(),
        ]
    );
    assert_eq!(sweep.removed, [own("old-exited-1"), own("old-exited-2")]);
    assert_eq!(sweep.pool_after, 17 * ROW);
    assert_eq!(store.pool_bytes(true).unwrap(), sweep.pool_after);

    // The parent's trimmed history is gone for good and says so.
    let row = store.agent(&parent).unwrap().unwrap();
    assert_eq!((row.complete_from_order, row.exhausted), (Some(17), true));
    let page = store.page(&parent, None, 10).unwrap();
    assert_eq!(page.items.len(), 4);
    assert_eq!(page.end, PageEnd::Exhausted);

    // The children are whole, and the parent can resume one: the next
    // incarnation commits on top of its history.
    for child in &children {
        assert_eq!(store.page(child, None, 10).unwrap().items.len(), 5);
        println!("kept {} whole: its parent is live", name(child));
    }
    let mut resumed = store.agent(&children[0]).unwrap().unwrap();
    resumed.lifecycle = Lifecycle::Live as i32;
    resumed.incarnation = 2;
    store.put_agent(&resumed).unwrap();
    let step = Step {
        items: vec![Item {
            key: "resumed".into(),
            text: "continuing".into(),
            kind: wire::Kind::Codex as i32,
            ..Default::default()
        }],
        ..Default::default()
    };
    let committed = store.commit(&children[0], &[(2, step)], CLOCK).unwrap();
    assert_eq!(committed.cursor, 2);
    let tail = store.last_n(&children[0], 2).unwrap();
    assert_eq!(tail[1].key, "resumed");
    assert_eq!(tail[1].order, 6);
    println!(
        "resumed {} as incarnation 2; its new item took order {}",
        name(&children[0]),
        tail[1].order
    );
}

#[test]
fn retention_stops_as_soon_as_the_pool_fits() {
    let mut store = fresh();
    add(&mut store, &own("a"), 2, 10, false, None);
    add(&mut store, &own("b"), 2, 20, false, None);
    add(&mut store, &own("c"), 2, 30, false, None);
    let untouched = store.sweep_own(6 * ROW, ROW, 1).unwrap();
    assert!(untouched.steps.is_empty());
    let sweep = store.sweep_own(4 * ROW, ROW, 1).unwrap();
    assert_eq!(sweep.removed, [own("a")]);
    assert_eq!(sweep.pool_after, 4 * ROW);
}

#[test]
fn retention_keeps_a_live_parents_exited_children_and_frees_an_exited_parents() {
    let mut store = fresh();
    // A parent on another host whose replica row says live protects its
    // children here; its stale row errs toward keeping them.
    let remote_parent = peer("remote-parent");
    store
        .put_agent(&AgentRow::new(
            remote_parent.clone(),
            wire::Kind::Codex,
            "/",
        ))
        .unwrap();
    add(&mut store, &own("kept"), 2, 1, false, Some(&remote_parent));
    // A parent that has exited no longer protects anything.
    let gone_parent = own("gone-parent");
    add(&mut store, &gone_parent, 1, 900, false, None);
    add(&mut store, &own("orphan"), 2, 2, false, Some(&gone_parent));
    let sweep = store.sweep_own(0, ROW, 1).unwrap();
    assert!(!sweep.removed.contains(&own("kept")));
    assert!(sweep.removed.contains(&own("orphan")));
    assert!(sweep.removed.contains(&gone_parent));
    assert!(store.agent(&own("kept")).unwrap().is_some());
}

#[test]
fn retention_takes_a_family_whole_unless_a_member_is_live() {
    let mut store = fresh();
    let parent = own("parent");
    add(&mut store, &parent, 1, 10, false, None);
    add(&mut store, &own("child"), 1, 500, false, Some(&parent));
    add(
        &mut store,
        &own("grandchild"),
        1,
        600,
        false,
        Some(&own("child")),
    );
    let busy = own("busy-parent");
    add(&mut store, &busy, 1, 5, false, None);
    add(&mut store, &own("running-child"), 1, 700, true, Some(&busy));
    let sweep = store.sweep_own(ROW, ROW, 1).unwrap();
    // The family goes together, as delete takes it, at the parent's turn.
    assert_eq!(
        sweep.removed,
        [own("parent"), own("child"), own("grandchild")]
    );
    assert!(store.agent(&busy).unwrap().is_some());
    assert!(store.agent(&own("running-child")).unwrap().is_some());
}

#[test]
fn retention_never_trims_below_the_protected_rows() {
    let mut store = fresh();
    add(&mut store, &own("a"), 10, 1, true, None);
    add(&mut store, &own("b"), 6, 2, true, None);
    add(&mut store, &own("c"), 2, 3, true, None);
    let sweep = store.sweep_own(0, 100 * ROW, 3).unwrap();
    assert_eq!(
        describe(&sweep),
        [
            format!(
                "trimmed a by 7 rows ({} bytes); history now starts at order 8",
                7 * ROW
            ),
            format!(
                "trimmed b by 3 rows ({} bytes); history now starts at order 4",
                3 * ROW
            ),
            "every live agent is at its protected rows; stopping".to_owned(),
        ]
    );
    assert_eq!(store.last_n(&own("a"), 10).unwrap().len(), 3);
    assert_eq!(store.last_n(&own("c"), 10).unwrap().len(), 2);
    assert!(sweep.removed.is_empty());
}

#[test]
fn retention_of_replicas_evicts_unfollowed_agents_by_last_use_then_trims_to_k() {
    let mut store = fresh();
    add(&mut store, &own("mine"), 10, 1, true, None);
    replica(&mut store, &peer("read-long-ago"), 4, 900);
    replica(&mut store, &peer("read-recently"), 4, 100);
    replica(&mut store, &peer("followed"), 10, 50);
    assert_eq!(store.pool_bytes(false).unwrap(), 18 * ROW);
    let sourced = HashSet::from([peer("followed")]);
    // The runtime's clock says which were read last, whatever the origin's
    // activity says.
    let last_used = HashMap::from([
        (peer("read-long-ago"), 1_000),
        (peer("read-recently"), 5_000),
    ]);
    let sweep = store
        .sweep_replicas(14 * ROW, 3, &sourced, &last_used)
        .unwrap();
    assert_eq!(sweep.removed, [peer("read-long-ago")]);
    assert_eq!(sweep.pool_after, 14 * ROW);

    let sweep = store.sweep_replicas(0, 3, &sourced, &last_used).unwrap();
    assert_eq!(
        describe(&sweep),
        [
            format!("removed read-recently whole ({} bytes)", 4 * ROW),
            format!(
                "trimmed followed by 7 rows ({} bytes); history now starts at order 8",
                7 * ROW
            ),
            "every live agent is at its protected rows; stopping".to_owned(),
        ]
    );
    // The followed agent keeps a contiguous block of its newest rows, and
    // older history is still at the origin.
    let row = store.agent(&peer("followed")).unwrap().unwrap();
    assert_eq!((row.complete_from_order, row.exhausted), (Some(8), false));
    let page = store.page(&peer("followed"), None, 10).unwrap();
    assert_eq!(page.items.len(), 3);
    assert_eq!(page.end, PageEnd::Boundary);
    // Own rows are a different pool.
    assert_eq!(store.pool_bytes(true).unwrap(), 10 * ROW);
}

/// Evicting a live parent's replica frees its rows, not its registry entry:
/// its exited child here is still live work, and the child's next turn end
/// still names the parent's incarnation.
#[test]
fn retention_of_a_replica_keeps_its_agents_row_for_the_children_here() {
    let mut store = fresh();
    let parent = peer("parent");
    replica(&mut store, &parent, 4, 100);
    store.absorb(&parent, Absorb::CaughtUp(5)).unwrap();
    let mut row = store.agent(&parent).unwrap().unwrap();
    row.incarnation = 3;
    store.put_agent(&row).unwrap();
    let child = own("child");
    add(&mut store, &child, 2, 50, false, Some(&parent));

    let sweep = store
        .sweep_replicas(0, 3, &HashSet::new(), &HashMap::new())
        .unwrap();
    assert_eq!(sweep.removed, std::slice::from_ref(&parent));
    assert_eq!(
        describe(&sweep),
        [format!("removed parent whole ({} bytes)", 4 * ROW)]
    );
    assert_eq!(store.pool_bytes(false).unwrap(), 0);
    let row = store.agent(&parent).unwrap().unwrap();
    assert_eq!(
        (
            row.complete_from_order,
            row.exhausted,
            row.source_cursor,
            row.lifecycle,
            row.incarnation
        ),
        (None, false, 0, Lifecycle::Live as i32, 3)
    );
    assert!(store.get(&parent, "01").unwrap().is_none());
    let page = store.page(&parent, None, 10).unwrap();
    assert!(page.items.is_empty());
    assert_eq!(page.end, PageEnd::Boundary);

    // (a) The parent's row still says live, so its exited child stays.
    let sweep = store.sweep_own(0, ROW, 1).unwrap();
    assert!(sweep.removed.is_empty());
    assert_eq!(store.page(&child, None, 10).unwrap().items.len(), 2);

    // (b) The child's next turn end names the parent's real incarnation.
    let step = Step {
        items: vec![Item {
            key: "final".into(),
            text: "done".into(),
            kind: wire::Kind::Codex as i32,
            ..Default::default()
        }],
        turn_end: Some(TurnEnd {
            turn_id: 9,
            last_message_key: "final".into(),
            outcome: wire::TurnOutcome::Completed as i32,
            last: false,
        }),
        ..Default::default()
    };
    store.commit(&child, &[(2, step)], CLOCK).unwrap();
    let deliveries = store.deliveries().unwrap();
    assert_eq!(deliveries.len(), 1);
    assert_eq!(deliveries[0].parent, parent);
    assert_eq!(deliveries[0].parent_incarnation, 3);
}

#[test]
fn retention_of_replica_blobs_evicts_the_least_recently_read_file() {
    let dir = tempfile::tempdir().unwrap();
    let blobs = dir.path().join("peer/agents/a1/blobs");
    std::fs::create_dir_all(&blobs).unwrap();
    for (name, size) in [("old", 100), ("new", 100), ("big", 300)] {
        std::fs::write(blobs.join(name), vec![0; size]).unwrap();
    }
    std::fs::write(dir.path().join("peer/not-a-blob"), b"x").unwrap();
    let mut lru = BlobLru::scan(dir.path(), 0).unwrap();
    assert_eq!(lru.bytes(), 500);
    lru.touch(&blobs.join("new"), 30);
    lru.touch(&blobs.join("big"), 20);
    lru.touch(&blobs.join("old"), 10);
    lru.insert(blobs.join("fetched"), 50, 40);
    std::fs::write(blobs.join("fetched"), vec![0; 50]).unwrap();

    let removed = lru.sweep(250, &HashSet::new()).unwrap();
    assert_eq!(removed, [blobs.join("old"), blobs.join("big")]);
    assert!(!blobs.join("old").exists());
    assert!(blobs.join("new").exists() && blobs.join("fetched").exists());
    assert_eq!(lru.bytes(), 150);
    assert!(lru.sweep(1_000, &HashSet::new()).unwrap().is_empty());
    lru.forget_under(&dir.path().join("peer/agents/a1"));
    assert_eq!(lru.bytes(), 0);
}
