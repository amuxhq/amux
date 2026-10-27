//! What the profile file does beyond the reader and writer suite:
//! migrations, refusing a store with another history, the position
//! tripwire, one transaction per commit, and surviving a reopen.

use rusqlite::{Connection, params};
use store::{
    AgentKey, AgentRow, CommitClock, Delivery, MIGRATIONS, OpenError, SCHEMA_STAMP, Sqlite,
    StoreError, migration_hash,
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
const SHIPPED: &[&str] = &["4c959f7e9a4542f0f0d2602c9efe77cf7a53a19c207f7b5759182721969a4a70"];

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
        Err(OpenError::OtherHistory { id: 1, .. })
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
        matches!(error, OpenError::OtherHistory { id: 1, .. }),
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
fn a_store_of_amux_with_no_recorded_history_is_refused_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.sqlite");
    let conn = Connection::open(&path).unwrap();
    conn.pragma_update(None, "journal_mode", "WAL").unwrap();
    conn.execute_batch("CREATE TABLE agents (x); INSERT INTO agents VALUES (1);")
        .unwrap();
    conn.pragma_update(None, "application_id", 0x616d_7578)
        .unwrap();
    drop(conn);
    let before = std::fs::read(&path).unwrap();
    assert!(matches!(
        Sqlite::open(&path, OWN),
        Err(OpenError::OtherHistory { id: 0, .. })
    ));
    assert_eq!(std::fs::read(&path).unwrap(), before);
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

/// The store the first release's schema wrote, with rows in every table:
/// an own agent with a parent, items with an attachment and an input id, a
/// needs-you snapshot and its notification, a replica taken by a reset, a
/// peer's generation, both kinds of delivery, the peer's pairing, a draft
/// and a recent directory. Written once and never
/// rewritten, so every later build opens a store as the release left it.
const FIRST_RELEASE_STORE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/store-v1.sqlite"
);
const PEER: &[u8] = b"peer-host";

fn first_release_own() -> AgentKey {
    AgentKey::new(OWN, *b"agent-own-000001")
}

fn first_release_replica() -> AgentKey {
    AgentKey::new(PEER, *b"agent-peer-00001")
}

fn first_release_image() -> wire::Attachment {
    wire::Attachment {
        of: Some(wire::attachment::Of::Image(wire::BlobRef {
            hash: vec![7; 32],
            name: "shot.png".into(),
            mime: "image/png".into(),
            size: 12,
        })),
    }
}

fn first_release_deliveries() -> Vec<Delivery> {
    use wire::child_report::End;
    let parent = first_release_replica();
    vec![
        Delivery {
            child_id: first_release_own().agent,
            incarnation: 1,
            turn_id: 4,
            parent: parent.clone(),
            parent_incarnation: 2,
            end: End::TurnEnded(wire::TurnEnded {
                text: "the build is green".into(),
                outcome: wire::TurnOutcome::Completed as i32,
                last: true,
            }),
        },
        Delivery {
            child_id: first_release_own().agent,
            incarnation: 2,
            turn_id: 0,
            parent,
            parent_incarnation: 0,
            end: End::Exited(wire::Exited {
                ended: Some(wire::IncarnationEnd {
                    cause: wire::ExitCause::Finished as i32,
                    detail: Some("exit code 0".into()),
                }),
            }),
        },
    ]
}

#[test]
fn the_first_release_store_opens_reads_and_takes_writes() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::copy(FIRST_RELEASE_STORE, dir.path().join("store.sqlite")).unwrap();
    let mut store = open(&dir);
    let (own, replica) = (first_release_own(), first_release_replica());

    let row = store.agent(&own).unwrap().unwrap();
    assert_eq!(row.name, "builder");
    assert_eq!(row.cwd, "/src/app");
    assert_eq!(row.parent, Some(replica.clone()));
    assert_eq!(row.phase, Phase::NeedsYou as i32);
    assert_eq!(row.working_on.as_deref(), Some("the release"));
    assert_eq!(row.git.unwrap().branch.as_deref(), Some("main"));
    assert_eq!(row.background_jobs, 2);
    assert_eq!(row.ingest_cursor, 64);
    assert_eq!(row.snapshot.unwrap().phase, Phase::NeedsYou as i32);
    let k1 = store.get(&own, "k1").unwrap().unwrap();
    assert_eq!(k1.text, "text of k1");
    assert_eq!(k1.attachments, vec![first_release_image()]);
    assert_eq!(
        store.item_by_input(&own, b"input-1").unwrap().unwrap().key,
        "k1"
    );
    let notifications = store.notifications().unwrap();
    assert_eq!(notifications.len(), 1);
    assert_eq!(notifications[0].agent_id, own.agent);
    assert_eq!(notifications[0].working_on.as_deref(), Some("the release"));

    let row = store.agent(&replica).unwrap().unwrap();
    assert_eq!(row.name, "planner");
    assert_eq!(row.source_generation, 3);
    assert_eq!(
        store.get(&replica, "r1").unwrap().unwrap().text,
        "from the peer"
    );
    assert_eq!(store.host_generation(PEER).unwrap(), Some(3));
    assert_eq!(store.deliveries().unwrap(), first_release_deliveries());
    let trusted = store.trusted_hosts().unwrap();
    assert_eq!(trusted.len(), 1);
    assert_eq!(
        (trusted[0].host_id.as_slice(), trusted[0].name.as_str()),
        (PEER, "desk")
    );
    assert_eq!(trusted[0].ssh.as_deref(), Some("desk.local"));
    assert_eq!(trusted[0].direct, r#"["192.0.2.7:4433"]"#);
    assert_eq!(
        store.draft(&own.agent).unwrap().as_deref(),
        Some(&b"\x0a\x0ehalf a thought"[..])
    );
    assert_eq!(
        store.recent_directories().unwrap(),
        [("/src/app".to_owned(), 1_700_000_000_000)]
    );

    store.commit(&own, &[(65, step(&["k3"]))], CLOCK).unwrap();
    assert_eq!(store.get(&own, "k3").unwrap().unwrap().text, "text of k3");
    assert_eq!(store.cursor(&own).unwrap(), 65);
}

/// An older binary keeps writing a store a later one migrated: rewriting a
/// held delivery or notification leaves a column it does not know as the
/// later binary set it.
#[test]
fn a_rewritten_row_keeps_a_column_a_later_migration_added() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = open(&dir);
    let own = AgentKey::new(OWN, b"a".to_vec());
    let mut row = AgentRow::new(own.clone(), wire::Kind::Codex, "/");
    row.parent = Some(AgentKey::new(b"p-host".to_vec(), b"p".to_vec()));
    store.put_agent(&row).unwrap();
    let delivery = first_release_deliveries().remove(0);
    store.put_delivery(&delivery).unwrap();
    let needs_you = Step {
        snapshot: Some(Snapshot {
            phase: Phase::NeedsYou as i32,
            ..Default::default()
        }),
        ..step(&["k"])
    };
    store.commit(&own, &[(1, needs_you)], CLOCK).unwrap();
    let notification = store.notifications().unwrap().remove(0);
    store
        .connection()
        .execute_batch(
            "ALTER TABLE deliveries ADD COLUMN later INTEGER NOT NULL DEFAULT 0;
             ALTER TABLE notifications ADD COLUMN later INTEGER NOT NULL DEFAULT 0;
             UPDATE deliveries SET later = 7; UPDATE notifications SET later = 7;",
        )
        .unwrap();

    store.put_delivery(&delivery).unwrap();
    // A second turn into needs-you at the same revision rewrites the row.
    store
        .connection()
        .execute(
            "UPDATE agents SET phase = ?1, next_revision = ?2",
            params![Phase::Working as i32, notification.revision as i64],
        )
        .unwrap();
    let needs_you = Step {
        snapshot: Some(Snapshot {
            phase: Phase::NeedsYou as i32,
            ..Default::default()
        }),
        ..Default::default()
    };
    store.commit(&own, &[(2, needs_you)], CLOCK).unwrap();

    let later = |table: &str| -> Vec<i64> {
        let conn = store.connection();
        let mut statement = conn.prepare(&format!("SELECT later FROM {table}")).unwrap();
        statement
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    assert_eq!(later("deliveries"), vec![7]);
    assert_eq!(later("notifications"), vec![7]);
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
