//! What only the profile file does: migrations, the order tripwire, one
//! transaction per commit, and surviving a reopen.

use rusqlite::{Connection, params};
use store::{
    AgentKey, AgentRow, CommitClock, MIGRATIONS, OpenError, SCHEMA_STAMP, Sqlite, Store,
    StoreError, migration_hash,
};
use wire::{Item, Phase, Snapshot, Step};

const OWN: &[u8] = b"own-host";
const CLOCK: CommitClock = CommitClock {
    now_ms: 0,
    notify_delay_ms: 0,
};

/// The hash of every migration ever shipped. A shipped migration is never
/// edited; append a new one and its hash here instead.
const SHIPPED: &[&str] = &[
    "c12ecbd4ad0f378ef242319c0f137863c19e1dc5ef5a7310deeb951276b15a04",
    "e7e1f74cead2bc6888769d611d8b6b898af1854422011a20ab6e021460b60a11",
    "8cd1a05a538c061607a3b2d959d6aebe18e8ffbe6df773b733c8d4b733ff8b37",
    "34ceffe9be496cf6e18ab79165f57986857a8d0606556580dde9f871fab48ea7",
    "7276e37dc4a8bf957b51e74d46e1d693931842acf807d32d480659cdb96347f9",
];

fn step(keys: &[&str]) -> Step {
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
        ..Default::default()
    }
}

fn open(dir: &tempfile::TempDir) -> Sqlite {
    Sqlite::open(&dir.path().join("store.sqlite"), OWN).unwrap()
}

#[test]
fn shipped_migrations_are_never_edited() {
    let hashes = MIGRATIONS
        .iter()
        .map(|text| migration_hash(text))
        .collect::<Vec<_>>();
    assert_eq!(
        &hashes[..SHIPPED.len()],
        SHIPPED,
        "a shipped migration's text changed; add a new migration instead"
    );
    assert_eq!(
        hashes.len(),
        SHIPPED.len(),
        "a new migration needs its hash pinned in SHIPPED"
    );
}

#[test]
fn an_edited_migration_changes_its_hash() {
    let edited = MIGRATIONS[0].replace("exit_cause    TEXT", "exit_cause    TEXT NOT NULL");
    assert_ne!(migration_hash(&edited), SHIPPED[0]);
}

#[test]
fn a_store_migrated_by_different_text_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    drop(open(&dir));
    let conn = Connection::open(dir.path().join("store.sqlite")).unwrap();
    conn.execute(
        "UPDATE schema_migrations SET sha256 = ?1 WHERE id = 1",
        params![migration_hash("an edited migration")],
    )
    .unwrap();
    drop(conn);
    assert!(matches!(
        Sqlite::open(&dir.path().join("store.sqlite"), OWN),
        Err(OpenError::EditedMigration { id: 1 })
    ));
}

#[test]
fn a_fresh_store_is_stamped_in_wal_mode() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(&dir);
    let conn = store.connection();
    let stamp: u32 = conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(stamp, SCHEMA_STAMP);
    let mode: String = conn
        .pragma_query_value(None, "journal_mode", |row| row.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    let synchronous: i64 = conn
        .pragma_query_value(None, "synchronous", |row| row.get(0))
        .unwrap();
    assert_eq!(synchronous, 1, "NORMAL");
}

#[test]
fn a_newer_store_opens_when_its_stamp_allows_and_not_otherwise() {
    let dir = tempfile::tempdir().unwrap();
    drop(open(&dir));
    let path = dir.path().join("store.sqlite");
    // A newer build added a migration without raising the stamp.
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("ALTER TABLE agents ADD COLUMN future INTEGER NOT NULL DEFAULT 0;")
            .unwrap();
        conn.execute(
            "INSERT INTO schema_migrations (id, sha256) VALUES (?1, 'future')",
            params![MIGRATIONS.len() as u32 + 1],
        )
        .unwrap();
    }
    let mut store = Sqlite::open(&path, OWN).unwrap();
    let agent = AgentKey::new(OWN, b"a".to_vec());
    store
        .put_agent(&AgentRow::new(agent.clone(), wire::Kind::Codex, "/"))
        .unwrap();
    store.commit(&agent, &[(1, step(&["k"]))], CLOCK).unwrap();
    drop(store);
    // A newer build raised the stamp past what this build knows.
    Connection::open(&path)
        .unwrap()
        .pragma_update(None, "user_version", MIGRATIONS.len() as u32 + 1)
        .unwrap();
    assert!(matches!(
        Sqlite::open(&path, OWN),
        Err(OpenError::TooNew { .. })
    ));
}

#[test]
fn a_file_that_is_not_a_profile_store_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("other.sqlite");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("CREATE TABLE t (x); PRAGMA user_version = 7;")
        .unwrap();
    drop(conn);
    assert!(matches!(
        Sqlite::open(&path, OWN),
        Err(OpenError::Foreign(_))
    ));
}

#[test]
fn the_unique_order_index_trips_on_a_second_key_at_one_order() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = open(&dir);
    let agent = AgentKey::new(OWN, b"a".to_vec());
    store
        .put_agent(&AgentRow::new(agent.clone(), wire::Kind::Codex, "/"))
        .unwrap();
    store.commit(&agent, &[(1, step(&["k1"]))], CLOCK).unwrap();
    let error = store
        .connection()
        .execute(
            "INSERT INTO items (origin_host, agent_id, key, \"order\", revision, at_ms, \
             producer_version, kind, body) VALUES (?1, ?2, 'k2', 1, 9, 0, '', 3, x'')",
            params![OWN, b"a".to_vec()],
        )
        .unwrap_err();
    assert!(
        error.to_string().contains("UNIQUE constraint failed"),
        "{error}"
    );
}

#[test]
fn a_commit_that_fails_leaves_nothing_behind() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = open(&dir);
    let agent = AgentKey::new(OWN, b"a".to_vec());
    store
        .put_agent(&AgentRow::new(agent.clone(), wire::Kind::Codex, "/"))
        .unwrap();
    store.commit(&agent, &[(1, step(&["k1"]))], CLOCK).unwrap();
    let before = store.agent(&agent).unwrap().unwrap();
    let k1_before = store.get(&agent, "k1").unwrap().unwrap();
    // Refuse the batch's last write, standing in for any failure after the
    // revised item, the snapshot and a new item were already written.
    store
        .connection()
        .execute_batch(
            "CREATE TRIGGER refuse_k3 BEFORE INSERT ON items WHEN NEW.key = 'k3' \
             BEGIN SELECT RAISE(ABORT, 'refused'); END;",
        )
        .unwrap();
    let frames = [
        (
            2,
            Step {
                snapshot: Some(Snapshot {
                    phase: Phase::Working as i32,
                    ..Default::default()
                }),
                ..step(&["k1"])
            },
        ),
        (3, step(&["k2", "k3"])),
    ];
    let error = store.commit(&agent, &frames, CLOCK).unwrap_err();
    assert!(matches!(error, StoreError::Sqlite(_)), "{error}");
    assert_eq!(
        store.agent(&agent).unwrap().unwrap(),
        before,
        "the row, cursor and snapshot are untouched"
    );
    assert_eq!(store.get(&agent, "k1").unwrap().unwrap(), k1_before);
    assert!(store.get(&agent, "k2").unwrap().is_none());
}

#[test]
fn rows_survive_a_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let agent = AgentKey::new(OWN, b"a".to_vec());
    {
        let mut store = open(&dir);
        let mut row = AgentRow::new(agent.clone(), wire::Kind::Codex, "/src");
        row.parent = Some(AgentKey::new(b"p-host".to_vec(), b"p".to_vec()));
        store.put_agent(&row).unwrap();
        let mut frames = step(&["k1", "k2"]);
        frames.items[0].attachments = vec![wire::Attachment {
            of: Some(wire::attachment::Of::Image(wire::BlobRef {
                hash: vec![7; 32],
                name: "shot.png".into(),
                mime: "image/png".into(),
                size: 12,
            })),
        }];
        frames.items[0].input_id = b"input-1".to_vec();
        store.commit(&agent, &[(64, frames)], CLOCK).unwrap();
    }
    let store = open(&dir);
    let row = store.agent(&agent).unwrap().unwrap();
    assert_eq!(row.ingest_cursor, 64);
    assert_eq!(
        row.parent,
        Some(AgentKey::new(b"p-host".to_vec(), b"p".to_vec()))
    );
    let item = store.get(&agent, "k1").unwrap().unwrap();
    assert_eq!(item.attachments.len(), 1);
    assert_eq!(item.input_id, b"input-1");
    assert_eq!(store.get(&agent, "k2").unwrap().unwrap().input_id, b"");
}
