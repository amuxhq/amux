//! What the profile file does beyond the reader and writer suite:
//! refusing a store of another schema, the position tripwire, one
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

/// A store written under another schema version, as every one written
/// before a schema change is, is refused with a message that names the
/// file and the version, and the file is left exactly as it was: the
/// person decides what to do with it.
#[test]
fn a_store_of_another_schema_version_is_refused_named_and_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.sqlite");
    drop(open(&dir));
    Connection::open(&path)
        .unwrap()
        .pragma_update(None, "user_version", SCHEMA_VERSION + 1)
        .unwrap();
    let before = std::fs::read(&path).unwrap();

    let error = Sqlite::open(&path, OWN).unwrap_err();
    assert!(
        matches!(error, OpenError::OtherSchema { version, .. } if version == SCHEMA_VERSION + 1),
        "{error:?}"
    );
    let message = error.to_string();
    assert!(
        message.contains(&path.display().to_string()),
        "names the file: {message}"
    );
    assert_eq!(
        std::fs::read(&path).unwrap(),
        before,
        "the refused store is not rewritten"
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
    let checkpointer = Connection::open(&path).unwrap();
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
        paired_at_ms: 1_700_000_000_000,
        on_account: false,
        signed_in: None,
        platform: 0,
        ssh: Some("desk.example".into()),
        direct: "[]".into(),
        listed: "[]".into(),
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
