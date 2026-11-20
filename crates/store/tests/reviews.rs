//! The reviews experiment's tables: one review per own agent, its rows read
//! whole and written in one transaction, gone with the agent's row and with
//! the experiment.

use std::collections::BTreeSet;

use rusqlite::Connection;
use store::reviews::{Anchor, Comment, Review, ReviewWriter, Round, Taken};
use store::{AgentKey, AgentRow, Sqlite, StoreError};
use wire::experimental::{
    REVIEW_ROW, ReviewDefinition, ReviewLine, ReviewLineKind, ReviewRowEntry,
};

const OWN: &[u8] = b"own-host";
const ON: &[&str] = &["reviews/1"];
const REVIEW: &[u8] = &[0x5e; 16];

fn agent(id: &[u8]) -> AgentKey {
    AgentKey::new(OWN, id.to_vec())
}

fn open(path: &std::path::Path, on: &[&str]) -> Sqlite {
    Sqlite::open_with(path, OWN, on).unwrap()
}

fn with_agent(path: &std::path::Path, id: &[u8]) -> Sqlite {
    let mut store = open(path, ON);
    store
        .put_agent(&AgentRow::new(agent(id), wire::Kind::Codex, "/src"))
        .unwrap();
    store
}

fn taken(commit: &str) -> Taken {
    Taken {
        commit: commit.into(),
        taken_ms: 5,
        omitted: vec!["big.bin".into(), "assets/huge.png".into()],
        merging: false,
    }
}

fn comment(id: u8, commit: &str, round: Option<u32>) -> Comment {
    Comment {
        id: vec![id; 16],
        anchor: Anchor {
            commit: commit.into(),
            path: "src/lib.rs".into(),
            start: 3,
            end: 4,
        },
        text: format!("comment {id}"),
        snippet: vec![ReviewLine {
            kind: ReviewLineKind::Added as i32,
            new_line: 3,
            text: "fn open() {}".into(),
            ..Default::default()
        }],
        scope: vec![ReviewDefinition {
            name: "open".into(),
            label: "fn open".into(),
            start_line: 3,
            end_line: 4,
            ..Default::default()
        }],
        round,
        updated_ms: 9,
    }
}

fn write<T>(
    store: &mut Sqlite,
    id: &[u8],
    write: impl FnOnce(&ReviewWriter<'_>) -> Result<T, StoreError>,
) -> Result<T, StoreError> {
    store.write_review(&agent(id), write)
}

fn begin(store: &mut Sqlite, id: &[u8], review: &[u8]) -> Result<(), StoreError> {
    write(store, id, |w| {
        w.begin(review, "main", "c0ffee", "/src/.git", 1)
    })
}

fn review(store: &Sqlite, id: &[u8]) -> Review {
    store.review(id).unwrap().expect("the agent has a review")
}

fn tables(path: &std::path::Path) -> Vec<String> {
    Connection::open(path)
        .unwrap()
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name LIKE 'exp_reviews_1_%' ORDER BY name")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

/// A review reads back whole as it was written: its base, its snapshots
/// oldest first, its rounds and its comments with their snippets.
#[test]
fn a_review_reads_back_as_written() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = with_agent(&dir.path().join("store.sqlite"), b"agent");
    assert_eq!(store.review(b"agent").unwrap(), None);
    begin(&mut store, b"agent", REVIEW).unwrap();
    write(&mut store, b"agent", |w| {
        w.add_snapshot(REVIEW, &taken("s1"))?;
        w.add_snapshot(REVIEW, &taken("s2"))?;
        w.put_comment(REVIEW, &comment(1, "s1", None))?;
        w.put_comment(REVIEW, &comment(2, "s2", Some(1)))?;
        w.add_round(
            REVIEW,
            &Round {
                n: 1,
                snapshot: "s2".into(),
                base_commit: "base".into(),
                sent_ms: 7,
            },
        )?;
        w.set_base(REVIEW, "develop")
    })
    .unwrap();
    let read = review(&store, b"agent");
    assert_eq!(read.id, REVIEW);
    assert_eq!(read.agent_id, b"agent");
    assert_eq!(read.base_branch, "develop");
    assert_eq!(
        (read.began_at.as_str(), read.common_dir.as_str()),
        ("c0ffee", "/src/.git")
    );
    assert_eq!(read.snapshots, [taken("s1"), taken("s2")]);
    assert_eq!(read.latest(), Some(&taken("s2")));
    assert_eq!(
        read.comments,
        [comment(1, "s1", None), comment(2, "s2", Some(1))]
    );
    assert_eq!(read.drafts().count(), 1);
    assert_eq!(read.rounds.len(), 1);
    assert_eq!(
        store.latest_snapshots().unwrap(),
        [(b"agent".to_vec(), taken("s2"))]
    );

    // Taken again, a snapshot is the latest once more.
    write(&mut store, b"agent", |w| {
        w.add_snapshot(REVIEW, &taken("s1"))
    })
    .unwrap();
    assert_eq!(review(&store, b"agent").latest(), Some(&taken("s1")));

    // A comment put again is changed in place, keeping its place.
    let mut edited = comment(1, "s1", None);
    edited.text = "said better".into();
    write(&mut store, b"agent", |w| w.put_comment(REVIEW, &edited)).unwrap();
    assert_eq!(review(&store, b"agent").comments[0], edited);
}

/// Each own agent has one review at most, and a comment id is one
/// comment's wherever it is.
#[test]
fn an_agent_has_one_review() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = with_agent(&dir.path().join("store.sqlite"), b"agent");
    begin(&mut store, b"agent", REVIEW).unwrap();
    assert!(matches!(
        begin(&mut store, b"agent", &[0x6f; 16]),
        Err(StoreError::Sqlite(_))
    ));
    assert_eq!(review(&store, b"agent").id, REVIEW);
    write(&mut store, b"agent", |w| {
        w.put_comment(REVIEW, &comment(1, "s1", None))
    })
    .unwrap();
    write(&mut store, b"agent", |w| {
        assert_eq!(w.comment_owner(&[1; 16])?, Some(REVIEW.to_vec()));
        assert_eq!(w.comment_owner(&[2; 16])?, None);
        Ok(())
    })
    .unwrap();
}

/// A write that fails leaves the review as it was before it began.
#[test]
fn a_failed_write_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = with_agent(&dir.path().join("store.sqlite"), b"agent");
    begin(&mut store, b"agent", REVIEW).unwrap();
    let failed: Result<(), StoreError> = write(&mut store, b"agent", |w| {
        w.put_comment(REVIEW, &comment(1, "s1", None))?;
        w.add_snapshot(REVIEW, &taken("s1"))?;
        Err(StoreError::Corrupt("stopped halfway".into()))
    });
    assert!(failed.is_err());
    let read = review(&store, b"agent");
    assert!(read.comments.is_empty() && read.snapshots.is_empty());
}

/// Drafts are discarded with sent comments left, one comment is deleted
/// by id, and a review removed takes every row with it.
#[test]
fn comments_are_discarded_and_deleted_and_a_review_removed() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = with_agent(&dir.path().join("store.sqlite"), b"agent");
    begin(&mut store, b"agent", REVIEW).unwrap();
    write(&mut store, b"agent", |w| {
        w.put_comment(REVIEW, &comment(1, "s1", None))?;
        w.put_comment(REVIEW, &comment(2, "s1", None))?;
        w.put_comment(REVIEW, &comment(3, "s1", Some(1)))?;
        w.put_comment(REVIEW, &comment(4, "s1", Some(1)))
    })
    .unwrap();
    assert_eq!(
        write(&mut store, b"agent", |w| w.discard_drafts(REVIEW)).unwrap(),
        2
    );
    assert!(write(&mut store, b"agent", |w| w.delete_comment(REVIEW, &[3; 16])).unwrap());
    assert!(!write(&mut store, b"agent", |w| w.delete_comment(REVIEW, &[3; 16])).unwrap());
    assert_eq!(
        review(&store, b"agent").comments,
        [comment(4, "s1", Some(1))]
    );

    write(&mut store, b"agent", |w| {
        w.add_snapshot(REVIEW, &taken("s1"))?;
        w.remove()
    })
    .unwrap();
    assert_eq!(store.review(b"agent").unwrap(), None);
    assert!(store.latest_snapshots().unwrap().is_empty());
    begin(&mut store, b"agent", &[0x6f; 16]).unwrap();
    assert!(
        review(&store, b"agent").comments.is_empty(),
        "a new review starts empty"
    );
}

/// The snapshots a review keeps are those a round or a comment names, and
/// the latest; the rest may be forgotten.
#[test]
fn the_kept_snapshots_are_those_named_and_the_latest() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = with_agent(&dir.path().join("store.sqlite"), b"agent");
    begin(&mut store, b"agent", REVIEW).unwrap();
    write(&mut store, b"agent", |w| {
        for commit in ["s1", "s2", "s3", "s4", "s5"] {
            w.add_snapshot(REVIEW, &taken(commit))?;
        }
        w.put_comment(REVIEW, &comment(1, "s2", None))?;
        // A comment on a removed line anchors to a commit of the
        // repository, which is no snapshot.
        w.put_comment(REVIEW, &comment(2, "base", Some(1)))?;
        w.add_round(
            REVIEW,
            &Round {
                n: 1,
                snapshot: "s3".into(),
                base_commit: "base".into(),
                sent_ms: 7,
            },
        )
    })
    .unwrap();
    let kept = review(&store, b"agent").kept();
    let expected: BTreeSet<String> = ["s2", "s3", "s5"].map(String::from).into();
    assert_eq!(kept, expected);

    write(&mut store, b"agent", |w| w.keep_snapshots(REVIEW, &kept)).unwrap();
    let read = review(&store, b"agent");
    assert_eq!(
        read.snapshots
            .iter()
            .map(|taken| taken.commit.as_str())
            .collect::<Vec<_>>(),
        ["s2", "s3", "s5"]
    );
    assert_eq!(read.kept(), expected, "keeping is stable");
}

/// An agent's review goes with its row; another agent's stays.
#[test]
fn a_review_goes_with_its_agents_row() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = with_agent(&dir.path().join("store.sqlite"), b"agent");
    store
        .put_agent(&AgentRow::new(agent(b"other"), wire::Kind::Codex, "/src"))
        .unwrap();
    begin(&mut store, b"agent", REVIEW).unwrap();
    begin(&mut store, b"other", &[0x6f; 16]).unwrap();
    for id in [&b"agent"[..], b"other"] {
        write(&mut store, id, |w| {
            let review = w.review()?.unwrap().id;
            w.add_snapshot(&review, &taken("s1"))?;
            w.put_comment(&review, &comment(id[0], "s1", None))?;
            w.add_round(
                &review,
                &Round {
                    n: 1,
                    snapshot: "s1".into(),
                    base_commit: "base".into(),
                    sent_ms: 7,
                },
            )
        })
        .unwrap();
    }
    store.delete_agent(&agent(b"agent")).unwrap();
    assert_eq!(store.review(b"agent").unwrap(), None);
    let other = review(&store, b"other");
    assert_eq!(
        (
            other.snapshots.len(),
            other.comments.len(),
            other.rounds.len()
        ),
        (1, 1, 1)
    );
    let conn = store.connection();
    for table in [
        "exp_reviews_1_reviews",
        "exp_reviews_1_snapshots",
        "exp_reviews_1_rounds",
        "exp_reviews_1_comments",
    ] {
        let held: i64 = conn
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(held, 1, "{table} holds only the other agent's");
    }
}

/// With the experiment off a store refuses review writes and reads none;
/// reopened so, it drops every table of the experiment and clears this
/// host's rows of their entries, while a replica's entry, its origin's,
/// stays.
#[test]
fn switched_off_the_tables_and_own_entries_go() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.sqlite");
    let mut store = with_agent(&path, b"agent");
    let entry = ReviewRowEntry {
        drafts: 1,
        counter: 3,
        moved: true,
    };
    let mut replica = AgentRow::new(
        AgentKey::new(b"far-host".to_vec(), b"theirs".to_vec()),
        wire::Kind::Codex,
        "/src",
    );
    REVIEW_ROW.put(&mut replica.experimental, &entry);
    store.put_agent(&replica).unwrap();
    begin(&mut store, b"agent", REVIEW).unwrap();
    write(&mut store, b"agent", |w| {
        let mut map = w.row()?.unwrap().experimental;
        REVIEW_ROW.put(&mut map, &entry);
        w.set_experimental(&map)?;
        w.put_comment(REVIEW, &comment(1, "s1", None))
    })
    .unwrap();
    assert!(matches!(
        store.write_review(&replica.agent, |_| Ok::<_, StoreError>(())),
        Err(StoreError::NotOwn)
    ));
    drop(store);
    assert_eq!(tables(&path).len(), 4);

    let mut store = open(&path, &[]);
    assert!(
        tables(&path).is_empty(),
        "every table of the experiment is dropped"
    );
    assert_eq!(store.review(b"agent").unwrap(), None);
    assert!(store.latest_snapshots().unwrap().is_empty());
    assert!(matches!(
        begin(&mut store, b"agent", REVIEW),
        Err(StoreError::ExperimentOff("reviews/1"))
    ));
    let own = store.agent(&agent(b"agent")).unwrap().unwrap();
    assert_eq!(
        REVIEW_ROW.get(&own.experimental),
        None,
        "the own row's entry is cleared"
    );
    let theirs = store.agent(&replica.agent).unwrap().unwrap();
    assert_eq!(
        REVIEW_ROW.get(&theirs.experimental),
        Some(entry),
        "a replica keeps its own"
    );
    drop(store);

    let store = open(&path, ON);
    assert_eq!(tables(&path).len(), 4);
    assert_eq!(
        store.review(b"agent").unwrap(),
        None,
        "switched on again, it starts empty"
    );
}

/// A dump's copy of the store carries no review.
#[test]
fn a_redacted_copy_carries_no_review() {
    struct Keep;
    impl store::redact::Redactor for Keep {
        fn text(&self, _: &str, _: &str, text: &str) -> String {
            text.to_owned()
        }
        fn message(&self, _: &str, bytes: &[u8]) -> Vec<u8> {
            bytes.to_vec()
        }
        fn hash(&self, bytes: &[u8]) -> Vec<u8> {
            bytes.to_vec()
        }
    }
    const WORDS: &str = "the PLANTED words of a comment";
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.sqlite");
    let mut store = with_agent(&path, b"agent");
    begin(&mut store, b"agent", REVIEW).unwrap();
    let mut planted = comment(1, "s1", None);
    planted.text = WORDS.into();
    write(&mut store, b"agent", |w| w.put_comment(REVIEW, &planted)).unwrap();
    drop(store);
    store::redact::redact_file(&path, &Keep).unwrap();
    assert!(tables(&path).is_empty());
    let file = std::fs::read(&path).unwrap();
    assert!(
        !file
            .windows(WORDS.len())
            .any(|window| window == WORDS.as_bytes()),
        "the file keeps the comment's words somewhere"
    );
}
