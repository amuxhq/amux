//! What the profile file does beyond the reader and writer suite:
//! migrations, refusing a store with another history, the position
//! tripwire, one transaction per commit, and surviving a reopen.

use rusqlite::{Connection, params};
use store::{
    AgentKey, AgentRow, CommitClock, MIGRATIONS, OpenError, SCHEMA_STAMP, Sqlite, StoreError,
    migration_hash,
};
use wire::{Item, Phase, Snapshot, Step};

const OWN: &[u8] = b"own-host";
const CLOCK: CommitClock = CommitClock {
    now_ms: 0,
    notify_delay_ms: 0,
};

/// The hash of every migration shipped. From the first release on a
/// shipped migration is never edited; append a new one and pin its hash
/// here instead.
const SHIPPED: &[&str] = &["3e15cdda18c8c28286d2232e4f346710147f2002358f92ec5d12274c01b880ab"];

/// What stores written before the release schema recorded: seven
/// migrations, whose first differs from this build's.
const PRE_RELEASE: &[&str] = &[
    "c12ecbd4ad0f378ef242319c0f137863c19e1dc5ef5a7310deeb951276b15a04",
    "e7e1f74cead2bc6888769d611d8b6b898af1854422011a20ab6e021460b60a11",
    "8cd1a05a538c061607a3b2d959d6aebe18e8ffbe6df773b733c8d4b733ff8b37",
    "34ceffe9be496cf6e18ab79165f57986857a8d0606556580dde9f871fab48ea7",
    "7276e37dc4a8bf957b51e74d46e1d693931842acf807d32d480659cdb96347f9",
    "1606e14ec49d85ada1c2e62d8339027123a6dd77ad237fa2089b927ab9a4cebe",
    "0b971d0ce2daa44ab2c6eb0711dd1ed26b937cb74e7935c55b10b29fb81cb483",
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
        Err(OpenError::EditedMigration { id: 1, .. })
    ));
}

/// A store written before the release schema records a history this build
/// does not share. Opening it is refused with a message that names the
/// file and says why, and the file is left exactly as it was: the person
/// decides what to do with it.
#[test]
fn a_store_with_another_history_is_refused_and_named() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.sqlite");
    {
        let conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "journal_mode", "WAL").unwrap();
        conn.execute_batch(
            "CREATE TABLE schema_migrations (id INTEGER PRIMARY KEY, sha256 TEXT NOT NULL);
             CREATE TABLE agents (origin_host BLOB NOT NULL, agent_id BLOB NOT NULL,
               kind INTEGER NOT NULL, PRIMARY KEY (origin_host, agent_id));
             INSERT INTO agents VALUES (x'01', x'02', 2);",
        )
        .unwrap();
        for (index, hash) in PRE_RELEASE.iter().enumerate() {
            conn.execute(
                "INSERT INTO schema_migrations (id, sha256) VALUES (?1, ?2)",
                params![index as u32 + 1, hash],
            )
            .unwrap();
        }
        conn.pragma_update(None, "application_id", 0x616d_7578)
            .unwrap();
        conn.pragma_update(None, "user_version", PRE_RELEASE.len() as u32)
            .unwrap();
    }
    let before = std::fs::read(&path).unwrap();

    let error = Sqlite::open(&path, OWN).unwrap_err();
    assert!(
        matches!(error, OpenError::EditedMigration { id: 1, .. }),
        "refused for its history, not as too new: {error:?}"
    );
    let message = error.to_string();
    assert!(
        message.contains(&path.display().to_string()),
        "names the file: {message}"
    );
    assert!(
        message.contains("predates this release's store schema"),
        "says why: {message}"
    );

    assert_eq!(
        std::fs::read(&path).unwrap(),
        before,
        "the refused store is not rewritten"
    );
    let conn = Connection::open(&path).unwrap();
    let recorded: u32 = conn
        .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
            row.get(0)
        })
        .unwrap();
    let agents: u32 = conn
        .query_row("SELECT COUNT(*) FROM agents", [], |row| row.get(0))
        .unwrap();
    assert_eq!((recorded, agents), (PRE_RELEASE.len() as u32, 1));
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
fn the_unique_position_index_trips_on_a_second_key_at_one_position() {
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
            "INSERT INTO items (origin_host, agent_id, key, position, revision, at_ms, \
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
