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
            ..Default::default()
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
    assert_eq!(experiment_tables(&path), ["exp_suspension_1_runs"]);
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
        3,
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

/// A blanking redactor: every text the same length of x, every message
/// kept, content zeroed.
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

/// No experiment says how its rows redact, so a dump's copy of the store
/// carries none of them.
#[test]
fn a_redacted_copy_carries_no_experiment_tables() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.sqlite");
    drop(Sqlite::open_with(&path, OWN, &["suspension/1"]).unwrap());
    assert_eq!(experiment_tables(&path), ["exp_suspension_1_runs"]);
    store::redact::redact_file(&path, &Blank).unwrap();
    assert!(experiment_tables(&path).is_empty());
    assert!(tables(&path).iter().any(|(name, _)| name == "agents"));
}

/// A map with an entry under each version, as a row's origin wrote it:
/// one this build runs, and one no build of this one knows.
fn entries(workspace: &[u8]) -> std::collections::BTreeMap<String, Vec<u8>> {
    [
        ("workspaces/1".to_owned(), workspace.to_vec()),
        ("teleport/3".to_owned(), b"another build's shape".to_vec()),
    ]
    .into()
}

/// What `agents.experimental` holds for each row, raw.
fn experimental_columns(path: &std::path::Path) -> Vec<Option<Vec<u8>>> {
    Connection::open(path)
        .unwrap()
        .prepare("SELECT experimental FROM agents ORDER BY origin_host, agent_id")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

/// No experiment says how its entries redact, and a workspace's names the
/// agent's folder, so a dump's copy of the store keeps no row's entries.
#[test]
fn a_redacted_copy_carries_no_experimental_entries() {
    const FOLDER: &[u8] = b"/Users/someone/src/PLANTEDfolder";
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.sqlite");
    let mut store = Sqlite::open_with(&path, OWN, &["workspaces/1"]).unwrap();
    for key in [
        AgentKey::new(OWN, b"agent".to_vec()),
        AgentKey::new(b"far-host".to_vec(), b"theirs".to_vec()),
    ] {
        let mut row = AgentRow::new(key, wire::Kind::Codex, "/src");
        row.experimental = entries(FOLDER);
        store.put_agent(&row).unwrap();
    }
    drop(store);
    assert!(
        experimental_columns(&path).iter().all(Option::is_some),
        "both rows hold their entries before"
    );
    store::redact::redact_file(&path, &Blank).unwrap();
    assert_eq!(experimental_columns(&path), [None, None]);
    let file = std::fs::read(&path).unwrap();
    assert!(
        !file.windows(FOLDER.len()).any(|window| window == FOLDER),
        "the file keeps the folder somewhere"
    );
    let store = Sqlite::open(&path, OWN).unwrap();
    assert!(
        store
            .agents()
            .unwrap()
            .iter()
            .all(|row| row.experimental.is_empty())
    );
}

/// A replica's entries are its origin's: stored whole and read back as
/// they came, whatever this store's owner runs and under versions it has
/// never heard of, and replaced whole by the origin's next row.
#[test]
fn a_replicas_experimental_entries_are_kept_as_its_origin_wrote_them() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.sqlite");
    let replica = AgentKey::new(b"far-host".to_vec(), b"theirs".to_vec());
    // Its owner runs no experiment, as the phone's does not.
    let mut store = Sqlite::open_with(&path, OWN, &[]).unwrap();
    let mut row = AgentRow::new(replica.clone(), wire::Kind::Codex, "/src");
    row.experimental = entries(&[1, 2, 3]);
    store.put_agent(&row).unwrap();
    assert_eq!(
        store.agent(&replica).unwrap().unwrap().experimental,
        entries(&[1, 2, 3])
    );
    assert_eq!(
        store.agent_by_id(b"theirs").unwrap().unwrap().experimental,
        entries(&[1, 2, 3])
    );
    drop(store);

    for on in [&[][..], &["workspaces/1"], &["suspension/1"]] {
        let store = Sqlite::open_with(&path, OWN, on).unwrap();
        assert_eq!(
            store.agents().unwrap()[0].experimental,
            entries(&[1, 2, 3]),
            "reopened running {on:?}"
        );
    }
    drop(Sqlite::open(&path, OWN).unwrap());

    let mut store = Sqlite::open_with(&path, OWN, &[]).unwrap();
    row.experimental = [("teleport/3".to_owned(), vec![7])].into();
    store.put_agent(&row).unwrap();
    assert_eq!(
        store.agent(&replica).unwrap().unwrap().experimental,
        row.experimental,
        "the origin's next row replaces the map whole"
    );
    row.experimental.clear();
    store.put_agent(&row).unwrap();
    assert!(
        store
            .agent(&replica)
            .unwrap()
            .unwrap()
            .experimental
            .is_empty()
    );
    assert_eq!(experimental_columns(&path), [None], "an empty map is NULL");
}

/// A host speaks only for its own rows: opening its store clears from them
/// the entries of every version it does not run, and leaves a replica's.
#[test]
fn opening_clears_own_rows_of_entries_it_does_not_run_and_never_a_replicas() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.sqlite");
    let own = AgentKey::new(OWN, b"agent".to_vec());
    let replica = AgentKey::new(b"far-host".to_vec(), b"theirs".to_vec());
    let mut store = Sqlite::open_with(&path, OWN, &["workspaces/1"]).unwrap();
    for key in [&own, &replica] {
        let mut row = AgentRow::new(key.clone(), wire::Kind::Codex, "/src");
        row.experimental = entries(&[1]);
        store.put_agent(&row).unwrap();
    }
    drop(store);

    let store = Sqlite::open_with(&path, OWN, &["workspaces/1"]).unwrap();
    assert_eq!(
        store.agent(&own).unwrap().unwrap().experimental,
        [("workspaces/1".to_owned(), vec![1])].into(),
        "an own row keeps only what this host runs"
    );
    assert_eq!(
        store.agent(&replica).unwrap().unwrap().experimental,
        entries(&[1])
    );
    drop(store);

    // A tool's open clears nothing.
    let store = Sqlite::open(&path, OWN).unwrap();
    assert!(!store.agent(&own).unwrap().unwrap().experimental.is_empty());
    drop(store);

    let store = Sqlite::open_with(&path, OWN, &[]).unwrap();
    assert!(store.agent(&own).unwrap().unwrap().experimental.is_empty());
    assert_eq!(
        store.agent(&replica).unwrap().unwrap().experimental,
        entries(&[1]),
        "a replica keeps its origin's entries"
    );
}

/// An own row's entries are its experiments': a registry write leaves them,
/// the experiments write the map whole, and a row removed takes them along.
/// Nothing but its origin's row writes a replica's.
#[test]
fn an_own_rows_entries_are_written_by_its_experiments_alone() {
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
    let map: std::collections::BTreeMap<String, Vec<u8>> =
        [("workspaces/1".to_owned(), vec![1, 2, 3])].into();
    assert!(store.set_experimental(&own, &map).unwrap());
    let held = store.agent(&own).unwrap().unwrap();
    assert_eq!(held.experimental, map);

    let mut renamed = held.clone();
    renamed.name = "renamed".into();
    renamed.experimental.clear();
    store.put_agent(&renamed).unwrap();
    let after = store.agent(&own).unwrap().unwrap();
    assert_eq!(after.name, "renamed");
    assert_eq!(after.experimental, map, "a registry write leaves the map");

    assert!(matches!(
        store.set_experimental(&replica, &map),
        Err(StoreError::NotOwn)
    ));
    assert!(
        !store
            .set_experimental(&AgentKey::new(OWN, b"nobody".to_vec()), &map)
            .unwrap()
    );
    assert!(store.set_experimental(&own, &Default::default()).unwrap());
    assert!(store.agent(&own).unwrap().unwrap().experimental.is_empty());

    store.set_experimental(&own, &map).unwrap();
    store.delete_agent(&own).unwrap();
    store
        .put_agent(&AgentRow::new(own.clone(), wire::Kind::Codex, "/src"))
        .unwrap();
    assert!(
        store.agent(&own).unwrap().unwrap().experimental.is_empty(),
        "the entries went with the row"
    );
}

/// A provider subagent's row keeps what marks it through a reopen, and a
/// commit, which does not own the column, leaves it as it was.
#[test]
fn a_provider_subagents_row_reads_back_marked() {
    let dir = tempfile::tempdir().unwrap();
    let key = AgentKey::new(OWN, b"subagent".to_vec());
    let native = wire::NativeChild {
        provider_child_id: "a1795690345bfafb1".into(),
    };
    {
        let mut store = open(&dir);
        let mut row = AgentRow::new(key.clone(), wire::Kind::ClaudeSdk, "/");
        row.name = "sub-1a2b3c4d".into();
        row.parent = Some(AgentKey::new(OWN, b"host".to_vec()));
        row.native = Some(native.clone());
        store.put_agent(&row).unwrap();
        store.commit(&key, &[(10, step(&["a"]))], CLOCK).unwrap();
    }
    let store = open(&dir);
    let row = store.agent(&key).unwrap().expect("the row is held");
    assert_eq!(row.native, Some(native));
    let unmarked = AgentKey::new(OWN, b"started-by-amux".to_vec());
    let mut store = store;
    store
        .put_agent(&AgentRow::new(unmarked.clone(), wire::Kind::Codex, "/"))
        .unwrap();
    assert_eq!(store.agent(&unmarked).unwrap().unwrap().native, None);
}

/// A provider subagent has no process for the registry to see exit: its
/// end and its continuation are its host's parts, and the commit that
/// moves the host's cursor past them moves the row too. A daemon stopping
/// right after the commit then leaves no live row whose end it will not
/// read again.
#[test]
fn a_commit_moves_a_provider_subagents_lifecycle_with_its_hosts_cursor() {
    let dir = tempfile::tempdir().unwrap();
    let host = AgentKey::new(OWN, b"host".to_vec());
    let sub = AgentKey::new(OWN, b"subagent".to_vec());
    let mut store = open(&dir);
    store
        .put_agent(&AgentRow::new(host.clone(), wire::Kind::ClaudeSdk, "/"))
        .unwrap();
    let mut row = AgentRow::new(sub.clone(), wire::Kind::ClaudeSdk, "/");
    row.parent = Some(host.clone());
    row.native = Some(wire::NativeChild {
        provider_child_id: "a1795690345bfafb1".into(),
    });
    store.put_agent(&row).unwrap();
    let part = |end: bool| Step {
        agents: vec![wire::AgentStep {
            agent_id: sub.agent.clone(),
            start: (!end).then(|| wire::NativeStart {
                provider_child_id: "a1795690345bfafb1".into(),
                ..Default::default()
            }),
            end: end.then_some(wire::IncarnationEnd {
                cause: wire::ExitCause::Finished as i32,
                detail: None,
            }),
            ..Default::default()
        }],
        ..Default::default()
    };
    let clock = CommitClock {
        now_ms: 7_000,
        ..CLOCK
    };

    store.commit(&host, &[(10, part(true))], clock).unwrap();
    let ended = open(&dir).agent(&sub).unwrap().unwrap();
    assert_eq!(ended.lifecycle, wire::Lifecycle::Exited as i32);
    assert_eq!(ended.exit_cause, wire::ExitCause::Finished as i32);
    assert!(ended.is_closed(), "one that did its work is filed closed");
    assert_eq!(ended.filed_at_ms, 7_000);

    store.commit(&host, &[(20, part(false))], clock).unwrap();
    let continued = open(&dir).agent(&sub).unwrap().unwrap();
    assert_eq!(continued.lifecycle, wire::Lifecycle::Live as i32);
    assert_eq!(continued.exit_cause, wire::ExitCause::Unspecified as i32);
    assert!(!continued.is_closed(), "open again");
    assert_eq!(store.cursor(&host).unwrap(), 20);
}
