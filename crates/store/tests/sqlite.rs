//! What the profile file does beyond the reader and writer suite:
//! opening a store a newer build added to, the position tripwire, one
//! transaction per commit, and surviving a reopen.

use rusqlite::{Connection, params};
use store::{AgentKey, AgentRow, CommitClock, OpenError, SCHEMA_VERSION, Sqlite, StoreError};
use wire::{Item, Phase, Snapshot, Step};

const OWN: &[u8] = b"own-host";
const CLOCK: CommitClock = CommitClock {
    now_ms: 0,
    notify_delay_ms: 0,
};

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

/// A rollback meets this: the release after this one added to the store,
/// and this build is started on it again. Migrations only add, so the store
/// opens, works, and keeps both the newer build's stamp and what it added.
#[test]
fn a_store_a_newer_build_added_to_opens_and_works() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.sqlite");
    drop(open(&dir));
    Connection::open(&path)
        .unwrap()
        .execute_batch(&format!(
            "ALTER TABLE agents ADD COLUMN added_later TEXT;
             ALTER TABLE items ADD COLUMN counted_later INTEGER NOT NULL DEFAULT 7;
             CREATE TABLE made_later (id INTEGER PRIMARY KEY, note TEXT) STRICT;
             INSERT INTO made_later (note) VALUES ('the newer build''s');
             PRAGMA user_version = {};",
            SCHEMA_VERSION + 1
        ))
        .unwrap();

    let mut store = Sqlite::open(&path, OWN).unwrap();
    let key = AgentKey::new(OWN, b"agent".to_vec());
    store
        .put_agent(&AgentRow::new(key.clone(), wire::Kind::Codex, "/"))
        .unwrap();
    store
        .commit(&key, &[(10, step(&["a", "b"]))], CLOCK)
        .unwrap();
    let page = store.page(&key, None, 10).unwrap();
    assert_eq!(page.items.len(), 2, "this build reads what it wrote");

    let conn = store.connection();
    let stamp: u32 = conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(
        stamp,
        SCHEMA_VERSION + 1,
        "the stamp stays the newer build's"
    );
    let kept: String = conn
        .query_row("SELECT note FROM made_later", [], |row| row.get(0))
        .unwrap();
    assert_eq!(kept, "the newer build's");
    let defaulted: i64 = conn
        .query_row("SELECT min(counted_later) FROM items", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        defaulted, 7,
        "rows this build writes take the new column's default"
    );
}

#[test]
fn a_fresh_store_is_stamped_in_wal_mode() {
    let dir = tempfile::tempdir().unwrap();
    let store = open(&dir);
    let conn = store.connection();
    let stamp: u32 = conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(stamp, SCHEMA_VERSION);
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
             kind, body) VALUES (?1, ?2, 'k2', 1, 9, 0, 3, x'')",
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

#[test]
fn a_passive_checkpoint_puts_every_commit_before_it_in_the_main_file_and_none_after() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.sqlite");
    let mut store = open(&dir);
    store.leave_checkpoints_to_owner().unwrap();
    let key = AgentKey::new(OWN, *b"agent-0000000000");
    store
        .put_agent(&AgentRow::new(key.clone(), wire::Kind::Codex, "/src"))
        .unwrap();
    store
        .commit(&key, &[(10, step(&["a", "b"]))], CLOCK)
        .unwrap();
    let changes = store.changes();

    // On a connection of its own, the store staying open on this one.
    Sqlite::checkpoint_wal(&path).unwrap();
    store.commit(&key, &[(20, step(&["c"]))], CLOCK).unwrap();
    assert!(store.changes() > changes, "a commit moves the count");

    // The main file alone, as a power cut would leave it with the WAL gone.
    let copy = dir.path().join("copy.sqlite");
    std::fs::copy(&path, &copy).unwrap();
    let copied = Sqlite::open(&copy, OWN).unwrap();
    assert!(copied.get(&key, "b").unwrap().is_some());
    assert!(copied.get(&key, "c").unwrap().is_none());
    assert_eq!(copied.cursor(&key).unwrap(), 10);
}

#[test]
fn a_passive_checkpoint_that_cannot_take_the_checkpoint_lock_fails_as_busy() {
    use std::sync::Mutex;

    /// The store the second checkpoint runs on, and what it returned.
    static STORE: Mutex<Option<std::path::PathBuf>> = Mutex::new(None);
    static RETURNED: Mutex<Option<Result<(), rusqlite::Error>>> = Mutex::new(None);

    /// Called while a full checkpoint holds SQLite's checkpoint lock and
    /// waits for the writer: the passive one cannot run at all, and SQLite
    /// says so as busy with no pages counted.
    fn while_the_lock_is_held(_: i32) -> bool {
        let path = STORE.lock().unwrap().clone().unwrap();
        *RETURNED.lock().unwrap() = Some(Sqlite::checkpoint_wal(&path));
        false
    }

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.sqlite");
    let mut store = open(&dir);
    store.leave_checkpoints_to_owner().unwrap();
    let key = AgentKey::new(OWN, *b"agent-0000000000");
    store
        .put_agent(&AgentRow::new(key.clone(), wire::Kind::Codex, "/src"))
        .unwrap();
    store
        .commit(&key, &[(10, step(&["a", "b"]))], CLOCK)
        .unwrap();
    *STORE.lock().unwrap() = Some(path.clone());

    // A writer holds the store, so a full checkpoint takes the checkpoint
    // lock and then waits on it.
    let writer = Connection::open(&path).unwrap();
    writer.execute_batch("BEGIN IMMEDIATE").unwrap();
    let mut checkpointer = Connection::open(&path).unwrap();
    checkpointer
        .busy_handler(Some(while_the_lock_is_held))
        .unwrap();
    let _ = checkpointer.query_row("PRAGMA wal_checkpoint(FULL)", [], |_| Ok(()));
    writer.execute_batch("ROLLBACK").unwrap();

    let returned = RETURNED
        .lock()
        .unwrap()
        .take()
        .expect("the full checkpoint waited on the writer");
    let error = returned.expect_err("a checkpoint that could not run reports it");
    assert_eq!(
        error.sqlite_error_code(),
        Some(rusqlite::ErrorCode::DatabaseBusy),
        "{error}"
    );
}

fn trusted(host: &[u8], name: &str) -> store::TrustedHost {
    store::TrustedHost {
        host_id: host.to_vec(),
        pubkey: vec![7; 32],
        name: name.into(),
        on_account: false,
        signed_in: None,
        platform: 0,
        ssh: Some("desk.example".into()),
        direct: "[]".into(),
        listed: "[]".into(),
        push_key: None,
    }
}

#[test]
fn a_paired_host_is_kept_rewritten_and_forgotten_with_its_generation() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = open(&dir);
    let desk = b"desk-host".as_slice();
    store
        .put_trusted_host(&trusted(desk, "desk"), store::Durability::Synced)
        .unwrap();
    store.record_host_generation(desk, 3).unwrap();
    let mut renamed = trusted(desk, "study desk");
    renamed.signed_in = Some(true);
    renamed.platform = 2;
    store
        .put_trusted_host(&renamed, store::Durability::Normal)
        .unwrap();
    drop(store);

    let mut store = open(&dir);
    assert_eq!(store.trusted_hosts().unwrap(), vec![renamed]);
    assert_eq!(store.host_generation(desk).unwrap(), Some(3));
    store
        .remove_trusted_host(desk, store::Durability::Synced)
        .unwrap();
    assert!(store.trusted_hosts().unwrap().is_empty());
    assert_eq!(store.host_generation(desk).unwrap(), None);
    assert!(matches!(
        store.put_trusted_host(&trusted(OWN, "me"), store::Durability::Normal),
        Err(StoreError::OwnHost)
    ));
}

#[test]
fn a_synced_write_leaves_the_store_unsynced_after_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = open(&dir);
    store
        .put_trusted_host(&trusted(b"desk-host", "desk"), store::Durability::Synced)
        .unwrap();
    let synchronous: i64 = store
        .connection()
        .query_row("PRAGMA synchronous", [], |row| row.get(0))
        .unwrap();
    let fullfsync: i64 = store
        .connection()
        .query_row("PRAGMA fullfsync", [], |row| row.get(0))
        .unwrap();
    assert_eq!((synchronous, fullfsync), (1, 0), "NORMAL and no fullfsync");
}

#[test]
fn drafts_are_kept_per_agent_and_forgotten() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = open(&dir);
    store.put_draft(b"a1", b"first").unwrap();
    store.put_draft(b"a1", b"second").unwrap();
    store.put_draft(b"a2", b"other").unwrap();
    assert_eq!(store.draft(b"a1").unwrap().as_deref(), Some(&b"second"[..]));
    assert!(store.delete_draft(b"a1").unwrap());
    assert!(!store.delete_draft(b"a1").unwrap());
    assert_eq!(store.draft(b"a1").unwrap(), None);
    assert_eq!(store.draft(b"a2").unwrap().as_deref(), Some(&b"other"[..]));
}

#[test]
fn recent_directories_keep_the_latest_use_and_the_newest_two_hundred() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = open(&dir);
    store.record_recent_directory("/src/a", 20).unwrap();
    store.record_recent_directory("/src/b", 10).unwrap();
    store.record_recent_directory("/src/a", 5).unwrap();
    assert_eq!(
        store.recent_directories().unwrap(),
        vec![("/src/a".to_owned(), 20), ("/src/b".to_owned(), 10)]
    );
    for n in 0..store::RECENT_DIRECTORIES as i64 {
        store
            .record_recent_directory(&format!("/many/{n}"), 100 + n)
            .unwrap();
    }
    let kept = store.recent_directories().unwrap();
    assert_eq!(kept.len(), store::RECENT_DIRECTORIES);
    assert!(kept.iter().all(|(path, _)| path.starts_with("/many/")));
}

/// Every table in the file, by name, with whether it has rows.
fn tables(path: &std::path::Path) -> Vec<(String, bool)> {
    let conn = Connection::open(path).unwrap();
    let names = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    names
        .into_iter()
        .map(|name| {
            let rows: i64 = conn
                .query_row(&format!("SELECT count(*) FROM \"{name}\""), [], |row| {
                    row.get(0)
                })
                .unwrap();
            (name, rows > 0)
        })
        .collect()
}

fn experiment_tables(path: &std::path::Path) -> Vec<String> {
    tables(path)
        .into_iter()
        .map(|(name, _)| name)
        .filter(|name| name.starts_with("exp_"))
        .collect()
}

/// Opening a store for the experiments a daemon runs makes their tables and
/// drops those of an experiment switched off or at another version, data
/// and all, and leaves every other table as it was. A plain open, which
/// every tool but the store's owner uses, touches none of them.
#[test]
fn opening_keeps_the_tables_of_the_experiments_on_and_drops_the_rest() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.sqlite");
    let mut store = Sqlite::open_with(&path, OWN, &["workspaces/1", "suspension/1"]).unwrap();
    store
        .put_agent(&AgentRow::new(
            AgentKey::new(OWN, b"agent".to_vec()),
            wire::Kind::Codex,
            "/",
        ))
        .unwrap();
    store.put_draft(b"agent", b"draft").unwrap();
    drop(store);
    assert_eq!(
        experiment_tables(&path),
        ["exp_suspension_1_runs", "exp_workspaces_1_members"]
    );
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute(
            "INSERT INTO exp_suspension_1_runs (agent_id, incarnation) VALUES (?1, 3)",
            params![b"agent".to_vec()],
        )
        .unwrap();
        // What an earlier version of workspaces, or another build's
        // experiment, left behind.
        conn.execute_batch(
            "CREATE TABLE exp_workspaces_0_members (id INTEGER PRIMARY KEY) STRICT;
             CREATE INDEX exp_workspaces_0_by_id ON exp_workspaces_0_members (id);
             CREATE TABLE exp_teleport_1_jumps (id INTEGER PRIMARY KEY) STRICT;
             INSERT INTO exp_teleport_1_jumps (id) VALUES (1);",
        )
        .unwrap();
    }
    let before: Vec<_> = tables(&path)
        .into_iter()
        .filter(|(name, _)| !name.starts_with("exp_"))
        .collect();

    drop(Sqlite::open(&path, OWN).unwrap());
    assert_eq!(
        experiment_tables(&path).len(),
        4,
        "a plain open drops nothing"
    );

    drop(Sqlite::open_with(&path, OWN, &["suspension/1"]).unwrap());
    assert_eq!(experiment_tables(&path), ["exp_suspension_1_runs"]);
    assert!(
        tables(&path).contains(&("exp_suspension_1_runs".to_owned(), true)),
        "the rows of an experiment still on stay"
    );
    let after: Vec<_> = tables(&path)
        .into_iter()
        .filter(|(name, _)| !name.starts_with("exp_"))
        .collect();
    assert_eq!(after, before, "every other table is untouched");
    assert!(after.contains(&("agents".to_owned(), true)));
    assert!(after.contains(&("drafts".to_owned(), true)));

    // Switched off, then on again: its data went with it.
    drop(Sqlite::open_with(&path, OWN, &[]).unwrap());
    assert!(experiment_tables(&path).is_empty());
    drop(Sqlite::open_with(&path, OWN, &["suspension/1"]).unwrap());
    assert_eq!(
        tables(&path)
            .into_iter()
            .find(|(name, _)| name == "exp_suspension_1_runs"),
        Some(("exp_suspension_1_runs".to_owned(), false))
    );
    let conn = Connection::open(&path).unwrap();
    let indexes: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'index' AND name LIKE 'exp%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(indexes, 0, "a dropped table's indexes go with it");
}

/// Under `suspension/1` each own agent's run is kept, rewritten in place
/// and removed with its row; with the experiment off nothing is held, so
/// every reader falls back to the incarnation alone.
#[test]
fn an_agents_run_is_kept_under_suspension_and_goes_with_its_row() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.sqlite");
    let key = AgentKey::new(OWN, b"agent".to_vec());
    let mut store = Sqlite::open_with(&path, OWN, &["suspension/1"]).unwrap();
    store
        .put_agent(&AgentRow::new(key.clone(), wire::Kind::Codex, "/"))
        .unwrap();
    assert_eq!(store.run_start(b"agent").unwrap(), None);
    store.put_run_start(b"agent", Some(2)).unwrap();
    store.put_run_start(b"agent", Some(5)).unwrap();
    assert_eq!(store.run_start(b"agent").unwrap(), Some(5));
    store.delete_agent(&key).unwrap();
    assert_eq!(store.run_start(b"agent").unwrap(), None);
    store.put_run_start(b"agent", Some(1)).unwrap();
    drop(store);

    let mut store = Sqlite::open_with(&path, OWN, &[]).unwrap();
    store.put_run_start(b"agent", Some(3)).unwrap();
    assert_eq!(store.run_start(b"agent").unwrap(), None);
}

/// Each experiment version's tables and indexes carry its prefix, are
/// strict, and are made again harmlessly; no promised migration names an
/// experiment's table.
#[test]
fn experiment_tables_are_named_by_their_version_and_kept_out_of_the_migrations() {
    use store::experiments::{EXPERIMENT_TABLES, prefix};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.sqlite");
    for tables in EXPERIMENT_TABLES {
        assert_eq!(tables.prefix(), prefix(tables.version));
        drop(Sqlite::open_with(&path, OWN, &[tables.version]).unwrap());
        drop(Sqlite::open_with(&path, OWN, &[tables.version]).unwrap());
        let made = experiment_tables(&path);
        assert!(!made.is_empty(), "{} declares no table", tables.version);
        let conn = Connection::open(&path).unwrap();
        let named = conn
            .prepare("SELECT name, sql FROM sqlite_master WHERE name LIKE 'exp%' AND sql NOT NULL")
            .unwrap()
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        for (name, sql) in named {
            assert!(
                name.starts_with(&tables.prefix()),
                "{name} in {}",
                tables.version
            );
            if sql.starts_with("CREATE TABLE") {
                assert!(sql.trim_end().ends_with("STRICT"), "{name} is not STRICT");
            }
        }
    }
    let schema =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/schema.rs")).unwrap();
    assert!(
        !schema.contains("exp_"),
        "a migration names an experiment's table"
    );
}

/// No experiment says how its rows redact, so a dump's copy of the store
/// carries none of them.
#[test]
fn a_redacted_copy_carries_no_experiment_tables() {
    struct Blank;
    impl store::redact::Redactor for Blank {
        fn text(&self, _: &str, _: &str, text: &str) -> String {
            "x".repeat(text.len())
        }
        fn message(&self, _: &str, bytes: &[u8]) -> Vec<u8> {
            bytes.to_vec()
        }
        fn content(&self, bytes: &[u8]) -> Vec<u8> {
            vec![0; bytes.len()]
        }
        fn hash(&self, bytes: &[u8]) -> Vec<u8> {
            bytes.to_vec()
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.sqlite");
    drop(Sqlite::open_with(&path, OWN, &["workspaces/1"]).unwrap());
    assert_eq!(experiment_tables(&path), ["exp_workspaces_1_members"]);
    store::redact::redact_file(&path, &Blank).unwrap();
    assert!(experiment_tables(&path).is_empty());
    assert!(tables(&path).iter().any(|(name, _)| name == "agents"));
}

/// A row read while an experiment's table is there carries the agent's
/// entry from it, keyed by the version; a row written does not touch it,
/// and a row removed, by a delete or by retention, takes it along. A store
/// without the table writes and reads nothing of it.
#[test]
fn an_experiments_entry_rides_on_the_row_and_goes_with_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.sqlite");
    let mut store = Sqlite::open_with(&path, OWN, &["workspaces/1"]).unwrap();
    let own = AgentKey::new(OWN, b"agent".to_vec());
    let replica = AgentKey::new(b"far-host".to_vec(), b"theirs".to_vec());
    for key in [&own, &replica] {
        store
            .put_agent(&AgentRow::new(key.clone(), wire::Kind::Codex, "/src"))
            .unwrap();
    }
    assert!(store.agent(&own).unwrap().unwrap().experimental.is_empty());

    store
        .put_experimental(&own, "workspaces/1", Some(&[1, 2, 3]))
        .unwrap();
    store
        .put_experimental(&replica, "workspaces/1", Some(&[4]))
        .unwrap();
    // A version the store keeps no table for is not written.
    store
        .put_experimental(&own, "workspaces/2", Some(&[9]))
        .unwrap();
    let held = store.agent(&own).unwrap().unwrap();
    assert_eq!(
        held.experimental,
        [("workspaces/1".to_owned(), vec![1, 2, 3])].into()
    );
    let by_id = store.agent_by_id(b"theirs").unwrap().unwrap();
    assert_eq!(
        by_id.experimental,
        [("workspaces/1".to_owned(), vec![4])].into()
    );
    assert_eq!(
        store
            .agents()
            .unwrap()
            .iter()
            .filter(|row| !row.experimental.is_empty())
            .count(),
        2
    );

    // Writing the row again leaves its entry.
    let mut renamed = held.clone();
    renamed.name = "renamed".into();
    renamed.experimental.clear();
    store.put_agent(&renamed).unwrap();
    assert_eq!(
        store.agent(&own).unwrap().unwrap().experimental,
        held.experimental
    );
    store.put_experimental(&own, "workspaces/1", None).unwrap();
    assert!(store.agent(&own).unwrap().unwrap().experimental.is_empty());

    store.delete_agent(&replica).unwrap();
    let left: i64 = store
        .connection()
        .query_row("SELECT count(*) FROM exp_workspaces_1_members", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(left, 0, "the entry went with its row");
    drop(store);

    let mut plain = Sqlite::open(&path, OWN).unwrap();
    plain
        .put_experimental(&own, "workspaces/1", Some(&[1]))
        .unwrap();
    assert!(plain.agent(&own).unwrap().unwrap().experimental.is_empty());
}
