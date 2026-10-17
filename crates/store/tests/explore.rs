//! The store as a value: in memory, dumped canonically and restored equal,
//! with the read-only views of a block that checks judge.

use std::hash::{DefaultHasher, Hash, Hasher};

use store::{Absorb, AgentKey, AgentRow, BlockOrders, CommitClock, Marker, SourceEvent, Sqlite};
use wire::{Item, Phase, Snapshot, Step};

const ORIGIN: &[u8] = b"origin-host";
const REPLICA: &[u8] = b"replica-host";
const CLOCK: CommitClock = CommitClock {
    now_ms: 0,
    notify_delay_ms: 0,
};

fn agent() -> AgentKey {
    AgentKey::new(ORIGIN, b"a".to_vec())
}

fn row(agent: &AgentKey) -> AgentRow {
    let mut row = AgentRow::new(agent.clone(), wire::Kind::Codex, "/src/amux");
    row.name = "worker".into();
    row
}

fn step(keys: &[&str], phase: Phase) -> Step {
    Step {
        items: keys
            .iter()
            .map(|key| Item {
                key: (*key).into(),
                kind: wire::Kind::Codex as i32,
                text: format!("text of {key}"),
                ..Default::default()
            })
            .collect(),
        snapshot: Some(Snapshot {
            kind: wire::Kind::Codex as i32,
            phase: phase as i32,
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// An origin holding agent `a` with `keys` committed in one step.
fn origin(keys: &[&str]) -> Sqlite {
    let mut store = Sqlite::open_in_memory(ORIGIN).unwrap();
    store.put_agent(&row(&agent())).unwrap();
    store
        .commit(&agent(), &[(1, step(keys, Phase::Working))], CLOCK)
        .unwrap();
    store
}

/// A replica holding agent `a`'s row and no block.
fn replica() -> Sqlite {
    let mut store = Sqlite::open_in_memory(REPLICA).unwrap();
    store.put_agent(&row(&agent())).unwrap();
    store
}

/// The reset an origin answers with: its block from `order` up and its
/// snapshot, under `generation`.
fn reset_from(origin: &Sqlite, order: u64, generation: u64) -> Absorb {
    Absorb::Reset {
        tail: origin.suffix_from(&agent(), order).unwrap(),
        snapshot: origin.agent(&agent()).unwrap().unwrap().snapshot.unwrap(),
        generation,
    }
}

fn hash(value: &impl Hash) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

fn orders(items: &[Item]) -> Vec<u64> {
    items.iter().map(|item| item.order).collect()
}

#[test]
fn a_store_in_memory_commits_and_serves_like_a_file() {
    let store = origin(&["a", "b", "c"]);
    let page = store.page(&agent(), None, 10).unwrap();
    assert_eq!(orders(&page.items), [3, 2, 1]);
    assert_eq!(store.agents().unwrap().len(), 1);
    assert_eq!(store.connection().path(), Some(""));
}

#[test]
fn a_dump_restores_to_a_store_that_dumps_equal_and_carries_on() {
    let origin = origin(&["a", "b", "c"]);
    let mut replica = replica();
    replica.absorb(&agent(), reset_from(&origin, 2, 1)).unwrap();
    replica.absorb(&agent(), Absorb::CaughtUp(4)).unwrap();

    for store in [&origin, &replica] {
        let dump = store.dump().unwrap();
        let restored = Sqlite::restore(&dump).unwrap();
        assert_eq!(restored.dump().unwrap(), dump);
        assert_eq!(restored.own_host(), store.own_host());
        assert_eq!(restored.agents().unwrap(), store.agents().unwrap());
        assert_eq!(
            restored.rows(&agent()).unwrap(),
            store.rows(&agent()).unwrap()
        );
        assert_eq!(restored.markers(), store.markers());
    }
    let restored = Sqlite::restore(&replica.dump().unwrap()).unwrap();
    assert_eq!(restored.markers().get(&agent()), Some(&Marker::CaughtUp));
    assert_eq!(restored.reset_count(&agent()).unwrap(), 1);

    // A restored origin mints the revision and order after the ones it held.
    let mut restored = Sqlite::restore(&origin.dump().unwrap()).unwrap();
    let committed = restored
        .commit(&agent(), &[(2, step(&["d"], Phase::Working))], CLOCK)
        .unwrap();
    let item = restored.get(&agent(), "d").unwrap().unwrap();
    assert_eq!((item.order, item.revision), (4, 5));
    assert_eq!(committed.cursor, 2);
}

#[test]
fn stores_holding_the_same_rows_dump_equal_whatever_order_wrote_them() {
    let one = AgentKey::new(ORIGIN, b"one".to_vec());
    let two = AgentKey::new(ORIGIN, b"two".to_vec());
    let mut first = Sqlite::open_in_memory(ORIGIN).unwrap();
    let mut second = Sqlite::open_in_memory(ORIGIN).unwrap();
    first.put_agent(&row(&one)).unwrap();
    first.put_agent(&row(&two)).unwrap();
    second.put_agent(&row(&two)).unwrap();
    second.put_agent(&row(&one)).unwrap();
    first
        .commit(&one, &[(1, step(&["x"], Phase::Working))], CLOCK)
        .unwrap();
    first
        .commit(&two, &[(1, step(&["y"], Phase::Working))], CLOCK)
        .unwrap();
    second
        .commit(&two, &[(1, step(&["y"], Phase::Working))], CLOCK)
        .unwrap();
    second
        .commit(&one, &[(1, step(&["x"], Phase::Working))], CLOCK)
        .unwrap();

    let (first_dump, second_dump) = (first.dump().unwrap(), second.dump().unwrap());
    assert_eq!(first_dump, second_dump);
    assert_eq!(hash(&first_dump), hash(&second_dump));
    assert_eq!(first_dump.clone(), first_dump);

    second
        .commit(&one, &[(2, step(&["z"], Phase::Working))], CLOCK)
        .unwrap();
    assert_ne!(first.dump().unwrap(), second.dump().unwrap());
    second.set_marker(&one, Some(Marker::Detached));
    let marked = second.dump().unwrap();
    second.set_marker(&one, None);
    assert_ne!(second.dump().unwrap(), marked);
    assert_ne!(Sqlite::open_in_memory(REPLICA).unwrap().dump().unwrap(), {
        Sqlite::open_in_memory(ORIGIN).unwrap().dump().unwrap()
    });
}

#[test]
fn a_replica_block_reads_through_the_views() {
    let mut origin = origin(&["a", "b", "c", "d"]);
    let mut replica = replica();
    let agent = agent();
    assert_eq!(replica.source_generation(&agent).unwrap(), None);
    assert_eq!(replica.reset_count(&agent).unwrap(), 0);
    assert!(!replica.current_with(&agent, &origin).unwrap());
    assert_eq!(
        replica.block_orders(&agent).unwrap(),
        BlockOrders {
            from: None,
            orders: vec![]
        }
    );

    // A reset to the origin's newest two rows starts the block there.
    replica.absorb(&agent, reset_from(&origin, 3, 1)).unwrap();
    let block = replica.block_orders(&agent).unwrap();
    assert_eq!(
        block,
        BlockOrders {
            from: Some(3),
            orders: vec![3, 4]
        }
    );
    assert!(block.is_contiguous());
    assert_eq!(replica.source_generation(&agent).unwrap(), Some(1));
    assert_eq!(replica.reset_count(&agent).unwrap(), 1);
    assert!(replica.current_with(&agent, &origin).unwrap());
    // Four items and the snapshot: the snapshot's revision is the newest.
    assert_eq!(replica.newest_revision_held(&agent).unwrap(), 5);
    assert_eq!(replica.source_cursor(&agent).unwrap(), 0);

    // The origin moves on, and the replica is current again once the new
    // rows arrive live.
    origin
        .commit(&agent, &[(2, step(&["e"], Phase::NeedsYou))], CLOCK)
        .unwrap();
    assert!(!replica.current_with(&agent, &origin).unwrap());
    let missing = origin.after(&agent, 5, 10).unwrap().unwrap();
    let mut events: Vec<SourceEvent> = missing.into_iter().map(SourceEvent::Item).collect();
    events.push(SourceEvent::Snapshot(
        origin.agent(&agent).unwrap().unwrap().snapshot.unwrap(),
    ));
    replica
        .absorb(&agent, Absorb::Delta { events, live: true })
        .unwrap();
    assert!(replica.current_with(&agent, &origin).unwrap());
    assert_eq!(replica.source_cursor(&agent).unwrap(), 7);
    assert_eq!(replica.newest_revision_held(&agent).unwrap(), 7);
    assert_eq!(orders(&replica.block(&agent).unwrap()), [3, 4, 5]);
    assert_eq!(orders(&origin.suffix_from(&agent, 4).unwrap()), [4, 5]);

    // A page that joins the boundary moves the block down to the oldest.
    let older = origin.page(&agent, Some(3), 10).unwrap().items;
    replica
        .absorb(
            &agent,
            Absorb::Page {
                before_order: 3,
                items: older,
                exhausted: true,
            },
        )
        .unwrap();
    assert_eq!(
        replica.block_orders(&agent).unwrap(),
        BlockOrders {
            from: Some(1),
            orders: vec![1, 2, 3, 4, 5]
        }
    );

    // A reset under the same generation keeps the rows below its tail
    // stored, out of the block; one under another generation drops them.
    replica.absorb(&agent, reset_from(&origin, 5, 1)).unwrap();
    assert_eq!(orders(&replica.block(&agent).unwrap()), [5]);
    assert_eq!(orders(&replica.rows(&agent).unwrap()), [1, 2, 3, 4, 5]);
    assert_eq!(replica.reset_count(&agent).unwrap(), 2);
    replica.absorb(&agent, reset_from(&origin, 4, 2)).unwrap();
    assert_eq!(orders(&replica.rows(&agent).unwrap()), [4, 5]);
    assert_eq!(replica.source_generation(&agent).unwrap(), Some(2));
    assert_eq!(replica.reset_count(&agent).unwrap(), 3);

    // The origin's own block is all its history.
    assert_eq!(
        origin.block_orders(&agent).unwrap(),
        BlockOrders {
            from: None,
            orders: vec![1, 2, 3, 4, 5]
        }
    );
}

#[test]
fn a_block_with_a_gap_or_a_wrong_start_is_not_contiguous() {
    let contiguous = |from, orders: &[u64]| {
        BlockOrders {
            from,
            orders: orders.to_vec(),
        }
        .is_contiguous()
    };
    assert!(contiguous(None, &[]));
    assert!(contiguous(Some(5), &[]));
    assert!(contiguous(None, &[2, 3, 4]));
    assert!(contiguous(Some(2), &[2, 3, 4]));
    assert!(!contiguous(Some(2), &[2, 4]));
    assert!(!contiguous(Some(2), &[3, 4]));
    assert!(!contiguous(None, &[1, 3]));
}
