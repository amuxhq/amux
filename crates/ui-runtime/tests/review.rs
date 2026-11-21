//! The review driver against an in-memory runtime: every review call is
//! answered by hand, so a case states exactly what the agent's host said,
//! and that a read already made makes no call at all.

use std::sync::Arc;

use client::{ManualClock, NotSent, RpcError};
use support::*;
use ui_runtime::Session;
use ui_runtime::review::{
    BATCH_FILES, CACHE_BYTES, Comparison, Followed, Review, ReviewCache, ReviewError, Unavailable,
};
use wire::ErrorCode;
use wire::experimental::{
    REVIEW_ROW, ReviewAnchor, ReviewEdit, ReviewFileDiff, ReviewGetFilesResponse, ReviewLine,
    ReviewLineKind, ReviewLineRange, ReviewPlacement, ReviewPutComment, ReviewRound,
    ReviewRowEntry, ReviewSummary, ReviewView, ReviewerComment, review_edit, review_placement,
};

use crate::support;

/// The review of the authored agent, keeping its reads in `cache`.
fn review(client: &client::Client, cache: &ReviewCache) -> Arc<Review> {
    Arc::new(Review::with_cache(
        client.agent(&agent(wire::Kind::ClaudeSdk)),
        cache.clone(),
    ))
}

fn paths(n: usize) -> Vec<String> {
    (0..n).map(|i| format!("src/file{i:02}.rs")).collect()
}

/// A review of `files` at `snapshot`.
fn summary(snapshot: &str, files: &[String]) -> ReviewSummary {
    ReviewSummary {
        id: b"review-1".to_vec(),
        base_branch: "main".into(),
        base_commit: "b".repeat(40),
        snapshot: snapshot.into(),
        files: files
            .iter()
            .map(|path| wire::ChangedFile {
                path: path.clone(),
                added: 1,
                ..wire::ChangedFile::default()
            })
            .collect(),
        ..ReviewSummary::default()
    }
}

fn file(path: &str) -> ReviewFileDiff {
    ReviewFileDiff {
        path: path.into(),
        hunks: vec![wire::experimental::ReviewHunk {
            old_commit: "b".repeat(40),
            old_start: 1,
            new_start: 1,
            lines: vec![ReviewLine {
                kind: ReviewLineKind::Added as i32,
                new_line: 1,
                text: format!("// {path}"),
                ..ReviewLine::default()
            }],
            ..Default::default()
        }],
        ..ReviewFileDiff::default()
    }
}

/// The agent's row with the entry of the review `review-1`, or none.
fn row(entry: Option<(u64, bool)>) -> wire::Agent {
    row_of(b"review-1", entry)
}

/// The agent's row with the entry of the review `id`, or none.
fn row_of(id: &[u8], entry: Option<(u64, bool)>) -> wire::Agent {
    let mut row = agent(wire::Kind::ClaudeSdk);
    if let Some((counter, moved)) = entry {
        let entry = ReviewRowEntry {
            drafts: 0,
            counter,
            moved,
            review_id: id.to_vec(),
        };
        REVIEW_ROW.put(&mut row.experimental, &entry);
    }
    row
}

/// All changes of `summary`.
fn all(summary: &ReviewSummary) -> Comparison {
    Comparison::shown(summary, ReviewView::All).unwrap()
}

/// The comparison all changes of the summary `summary("s1", ..)` shows.
fn all_of(snapshot: &str) -> Comparison {
    Comparison {
        from: "b".repeat(40),
        snapshot: snapshot.into(),
        view: ReviewView::All,
        base: String::new(),
        from_base: String::new(),
    }
}

/// Opens `review`, the host answering `answer`.
async fn opened(review: &Arc<Review>, calls: &mut Calls, answer: ReviewSummary) {
    let opening = tokio::spawn({
        let review = review.clone();
        async move { review.open(None, false).await }
    });
    let (request, reply) = calls.review_open().await;
    assert_eq!(request.agent_id, b"agent-1");
    assert!(!request.read_only, "a person's open");
    reply.send(Ok(answer.clone())).ok();
    assert_eq!(*opening.await.unwrap().unwrap(), answer);
}

/// Reads `want` of `comparison`, the host answering every file each batch
/// asks for.
async fn read_all(
    review: &Arc<Review>,
    calls: &mut Calls,
    comparison: &Comparison,
    want: &[String],
) {
    let reading = tokio::spawn({
        let (review, want, comparison) = (review.clone(), want.to_vec(), comparison.clone());
        async move { review.files(&comparison, &want).await }
    });
    let mut answered = 0;
    while answered < want.len() {
        let (request, reply) = calls.review_files().await;
        assert_eq!(
            (request.from.as_str(), request.snapshot.as_str()),
            (comparison.from.as_str(), comparison.snapshot.as_str()),
            "the request names the comparison"
        );
        assert_eq!(request.view, comparison.view as i32);
        assert!(!request.list);
        answered += request.files.len();
        let files = request
            .files
            .iter()
            .map(|asked| file(&asked.path))
            .collect();
        reply
            .send(Ok(ReviewGetFilesResponse {
                files,
                ..Default::default()
            }))
            .ok();
    }
    assert_eq!(reading.await.unwrap().unwrap().len(), want.len());
}

#[tokio::test]
async fn a_second_read_of_the_same_snapshot_makes_no_call() {
    let (client, mut calls) = runtime();
    let cache = ReviewCache::new(CACHE_BYTES);
    let want = paths(3);
    let first = review(&client, &cache);
    opened(&first, &mut calls, summary("s1", &want)).await;
    read_all(&first, &mut calls, &all_of("s1"), &want).await;

    let again = first.files(&all_of("s1"), &want).await.unwrap();
    assert_eq!(
        again
            .iter()
            .map(|file| file.path.clone())
            .collect::<Vec<_>>(),
        want
    );
    calls.none().await;

    // A page opened anew for the agent starts on the summary loaded last
    // and reads the same files without asking.
    let second = review(&client, &cache);
    assert_eq!(second.summary().unwrap().snapshot, "s1");
    second.files(&all_of("s1"), &want).await.unwrap();
    assert_eq!(second.fetched(&all_of("s1")).len(), 3);
    calls.none().await;

    // Another comparison of the same snapshot is another read.
    let since_round = Comparison {
        from: "r".repeat(40),
        snapshot: "s1".into(),
        view: ReviewView::SinceLast,
        base: "b".repeat(40),
        from_base: "q".repeat(40),
    };
    let since = tokio::spawn({
        let (second, want, since_round) = (second.clone(), want.clone(), since_round.clone());
        async move { second.files(&since_round, &want).await }
    });
    let (request, reply) = calls.review_files().await;
    assert_eq!(request.view, ReviewView::SinceLast as i32);
    assert_eq!(request.from, "r".repeat(40));
    assert_eq!(
        (request.base.as_str(), request.from_base.as_str()),
        ("b".repeat(40).as_str(), "q".repeat(40).as_str()),
        "since-last names both bases"
    );
    reply
        .send(Ok(ReviewGetFilesResponse {
            files: want.iter().map(|path| file(path)).collect(),
            ..Default::default()
        }))
        .ok();
    since.await.unwrap().unwrap();
}

#[tokio::test]
async fn files_arrive_in_batches_and_those_left_out_are_asked_again() {
    let (client, mut calls) = runtime();
    let cache = ReviewCache::new(CACHE_BYTES);
    let want = paths(40);
    let review = review(&client, &cache);
    opened(&review, &mut calls, summary("s1", &want)).await;
    let mut changed = review.changed();
    changed.borrow_and_update();

    let reading = tokio::spawn({
        let (review, want) = (review.clone(), want.clone());
        async move { review.files(&all_of("s1"), &want).await }
    });
    // The first batch: the host fits ten of the sixteen.
    let (request, reply) = calls.review_files().await;
    assert_eq!(request.snapshot, "s1");
    assert_eq!(request.from, "b".repeat(40));
    assert_eq!(request.view, ReviewView::All as i32);
    let asked: Vec<String> = request.files.iter().map(|f| f.path.clone()).collect();
    assert_eq!(asked, want[..BATCH_FILES]);
    reply
        .send(Ok(ReviewGetFilesResponse {
            files: asked[..10].iter().map(|path| file(path)).collect(),
            ..Default::default()
        }))
        .ok();
    // The page has those ten to draw before the next batch is answered.
    changed.changed().await.unwrap();
    let (request, reply) = calls.review_files().await;
    assert_eq!(review.fetched(&all_of("s1")).len(), 10);
    let asked: Vec<String> = request.files.iter().map(|f| f.path.clone()).collect();
    assert_eq!(asked, want[10..10 + BATCH_FILES], "the six left out lead");
    reply
        .send(Ok(ReviewGetFilesResponse {
            files: asked.iter().map(|path| file(path)).collect(),
            ..Default::default()
        }))
        .ok();
    let (request, reply) = calls.review_files().await;
    let asked: Vec<String> = request.files.iter().map(|f| f.path.clone()).collect();
    assert_eq!(asked, want[26..]);
    reply
        .send(Ok(ReviewGetFilesResponse {
            files: asked.iter().map(|path| file(path)).collect(),
            ..Default::default()
        }))
        .ok();
    let files = reading.await.unwrap().unwrap();
    assert_eq!(
        files
            .iter()
            .map(|file| file.path.clone())
            .collect::<Vec<_>>(),
        want,
        "answered in the order asked"
    );
    assert_eq!(review.fetched(&all_of("s1")).len(), 40);
    calls.none().await;
}

#[tokio::test]
async fn with_the_host_gone_a_loaded_review_still_reads_and_an_edit_says_so() {
    let (client, mut calls) = runtime();
    let cache = ReviewCache::new(CACHE_BYTES);
    let want = paths(2);
    let loaded = review(&client, &cache);
    opened(&loaded, &mut calls, summary("s1", &want)).await;
    read_all(&loaded, &mut calls, &all_of("s1"), &want).await;
    let lines = ReviewLineRange { start: 1, end: 9 };
    let context = tokio::spawn({
        let loaded = loaded.clone();
        async move {
            loaded
                .context(&all_of("s1"), "s1", "src/file00.rs", lines)
                .await
        }
    });
    let (request, reply) = calls.review_files().await;
    assert_eq!(request.from, "b".repeat(40));
    assert_eq!(
        request.files[0].context_side(),
        wire::experimental::ReviewSide::Snapshot,
        "the snapshot's side, named as a side"
    );
    assert_eq!(request.files[0].context, [lines]);
    let mut answer = file("src/file00.rs");
    answer.context = vec![ReviewLine {
        kind: ReviewLineKind::Context as i32,
        new_line: 1,
        text: "fn main() {".into(),
        ..ReviewLine::default()
    }];
    reply
        .send(Ok(ReviewGetFilesResponse {
            files: vec![answer],
            ..Default::default()
        }))
        .ok();
    assert_eq!(context.await.unwrap().unwrap().len(), 1);
    // A commit that is neither side of the comparison is never asked for.
    let elsewhere = loaded
        .context(&all_of("s1"), "elsewhere", "src/file00.rs", lines)
        .await;
    assert!(
        matches!(&elsewhere, Err(ReviewError::Rpc(RpcError::Refused(error))) if error.code() == ErrorCode::InvalidArgument),
        "{elsewhere:?}"
    );

    // The page is opened again with the host asleep.
    let page = review(&client, &cache);
    let opening = tokio::spawn({
        let page = page.clone();
        async move { page.open(None, false).await }
    });
    let (_, reply) = calls.review_open().await;
    reply.send(Err(host_unreachable())).ok();
    let refused = opening.await.unwrap().unwrap_err();
    assert!(
        matches!(&refused, ReviewError::Rpc(error) if error.not_sent() == Some(NotSent::Unreachable)),
        "{refused:?}"
    );
    assert_eq!(
        page.summary().unwrap().snapshot,
        "s1",
        "it keeps what it loaded"
    );
    assert_eq!(page.unavailable(), None);
    assert_eq!(page.files(&all_of("s1"), &want).await.unwrap().len(), 2);
    assert_eq!(
        page.context(&all_of("s1"), "s1", "src/file00.rs", lines)
            .await
            .unwrap()[0]
            .text,
        "fn main() {"
    );
    assert!(page.context_held("s1", "src/file00.rs", lines).is_some());
    calls.none().await;

    // Only an edit needs the host.
    let editing = tokio::spawn({
        let page = page.clone();
        async move {
            page.edit(vec![ReviewEdit {
                of: Some(review_edit::Of::Put(ReviewPutComment {
                    id: b"c1".to_vec(),
                    text: "why?".into(),
                    ..ReviewPutComment::default()
                })),
            }])
            .await
        }
    });
    let (request, reply) = calls.review_edit().await;
    assert_eq!(request.agent_id, b"agent-1");
    reply.send(Err(host_unreachable())).ok();
    match editing.await.unwrap() {
        Err(ReviewError::Rpc(error)) => {
            assert_eq!(error.not_sent(), Some(NotSent::Unreachable));
            assert!(!error.uncertain());
        }
        other => panic!("an edit with the host away is refused: {other:?}"),
    }
    assert_eq!(page.summary().unwrap().snapshot, "s1");
}

#[tokio::test]
async fn following_the_row_reads_a_changed_review_and_marks_a_moved_tree_stale() {
    let (client, mut calls) = runtime();
    let cache = ReviewCache::new(CACHE_BYTES);
    let review = review(&client, &cache);

    // The row the page opened from sets where following starts.
    assert_eq!(
        review.follow(&row(Some((4, false)))).await.unwrap(),
        Followed::Same
    );
    calls.none().await;
    opened(&review, &mut calls, summary("s1", &paths(1))).await;
    assert_eq!(
        review.follow(&row(Some((4, false)))).await.unwrap(),
        Followed::Same
    );
    calls.none().await;

    // The agent's turn ended with the tree moved: stale, no call.
    let mut changed = review.changed();
    changed.borrow_and_update();
    assert_eq!(
        review.follow(&row(Some((4, true)))).await.unwrap(),
        Followed::Stale
    );
    assert!(changed.has_changed().unwrap());
    assert!(review.summary().unwrap().stale);
    assert_eq!(
        review.follow(&row(Some((4, true)))).await.unwrap(),
        Followed::Same
    );
    calls.none().await;

    // Another client put a comment: the counter moved, and the review is
    // read again, on the same snapshot.
    let following = tokio::spawn({
        let review = review.clone();
        async move { review.follow(&row(Some((5, true)))).await }
    });
    let (request, reply) = calls.review_open().await;
    assert!(
        request.read_only,
        "following reads without changing the review"
    );
    assert!(!request.refresh);
    assert_eq!(request.base_branch, None);
    let mut commented = summary("s1", &paths(1));
    commented.stale = true;
    commented.comments = vec![ReviewerComment {
        id: b"theirs".to_vec(),
        text: "from the phone".into(),
        ..ReviewerComment::default()
    }];
    reply.send(Ok(commented.clone())).ok();
    assert_eq!(following.await.unwrap().unwrap(), Followed::Reread);
    assert_eq!(*review.summary().unwrap(), commented);

    // A change read again that landed on a newer snapshot leaves the page
    // on its own, marked stale.
    let following = tokio::spawn({
        let review = review.clone();
        async move { review.follow(&row(Some((6, false)))).await }
    });
    let (_, reply) = calls.review_open().await;
    reply.send(Ok(summary("s2", &paths(2)))).ok();
    assert_eq!(following.await.unwrap().unwrap(), Followed::Stale);
    let shown = review.summary().unwrap();
    assert_eq!(shown.snapshot, "s1");
    assert_eq!(shown.comments, commented.comments);
    assert!(shown.stale);

    // Refreshing is the person's own open, which takes what it answers.
    let refreshing = tokio::spawn({
        let review = review.clone();
        async move { review.open(None, true).await }
    });
    let (request, reply) = calls.review_open().await;
    assert!(request.refresh);
    reply.send(Ok(summary("s2", &paths(2)))).ok();
    assert_eq!(refreshing.await.unwrap().unwrap().snapshot, "s2");
    assert_eq!(review.summary().unwrap().snapshot, "s2");

    // A failed re-read is tried again on the next row.
    let following = tokio::spawn({
        let review = review.clone();
        async move { review.follow(&row(Some((7, false)))).await }
    });
    let (_, reply) = calls.review_open().await;
    reply.send(Err(aborted())).ok();
    assert!(following.await.unwrap().is_err());
    let following = tokio::spawn({
        let review = review.clone();
        async move { review.follow(&row(Some((7, false)))).await }
    });
    let (_, reply) = calls.review_open().await;
    reply.send(Ok(summary("s2", &paths(2)))).ok();
    assert_eq!(following.await.unwrap().unwrap(), Followed::Reread);
    calls.none().await;

    // Started over elsewhere, its counter begun again at the same number:
    // another review, read again, and shown as it is.
    let following = tokio::spawn({
        let review = review.clone();
        async move { review.follow(&row_of(b"review-2", Some((7, false)))).await }
    });
    let (request, reply) = calls.review_open().await;
    assert!(request.read_only);
    let begun = ReviewSummary {
        id: b"review-2".to_vec(),
        ..summary("s3", &paths(1))
    };
    reply.send(Ok(begun.clone())).ok();
    assert_eq!(following.await.unwrap().unwrap(), Followed::Reread);
    assert_eq!(*review.summary().unwrap(), begun);
    calls.none().await;
}

/// A page larger than the process's cache keeps what it fetched: nothing
/// it shows is evicted under it and asked for again.
#[tokio::test]
async fn the_open_page_keeps_its_files_apart_from_the_cache() {
    let (client, mut calls) = runtime();
    let want = paths(4);
    let one = {
        use prost::Message as _;
        file(&want[0]).encoded_len() + 2 * 40 + 4 + want[0].len()
    };
    // Room for two files of the four.
    let cache = ReviewCache::new(one * 2 + one / 2);
    let review = review(&client, &cache);
    opened(&review, &mut calls, summary("s1", &want)).await;
    read_all(&review, &mut calls, &all_of("s1"), &want).await;
    assert!(cache.bytes() <= one * 2 + one / 2);
    assert_eq!(review.fetched(&all_of("s1")).len(), 4);
    review.files(&all_of("s1"), &want).await.unwrap();
    calls.none().await;

    // A refresh onto a new snapshot lets the old one's files go.
    let refreshing = tokio::spawn({
        let review = review.clone();
        async move { review.open(None, true).await }
    });
    let (_, reply) = calls.review_open().await;
    reply.send(Ok(summary("s2", &want))).ok();
    refreshing.await.unwrap().unwrap();
    assert!(review.fetched(&all_of("s1")).is_empty());
}

/// An edit's answer on a newer snapshot than the page's: the page stays on
/// its own snapshot, and what the person wrote shows on it, on the lines it
/// was written on.
#[tokio::test]
async fn an_edit_on_a_page_held_behind_shows_its_comments() {
    let (client, mut calls) = runtime();
    let cache = ReviewCache::new(CACHE_BYTES);
    let review = review(&client, &cache);
    let mut held = summary("s1", &paths(1));
    held.comments = vec![ReviewerComment {
        id: b"old".to_vec(),
        anchor: Some(ReviewAnchor {
            commit: "s1".into(),
            path: "src/file00.rs".into(),
            lines: Some(ReviewLineRange { start: 3, end: 3 }),
        }),
        placement: Some(ReviewPlacement {
            at: Some(review_placement::At::Lines(ReviewLineRange {
                start: 3,
                end: 3,
            })),
            changed_near: false,
            path: "src/file00.rs".into(),
        }),
        ..ReviewerComment::default()
    }];
    opened(&review, &mut calls, held.clone()).await;

    let editing = tokio::spawn({
        let review = review.clone();
        async move {
            review
                .edit(vec![ReviewEdit {
                    of: Some(review_edit::Of::Put(ReviewPutComment::default())),
                }])
                .await
        }
    });
    let (_, reply) = calls.review_edit().await;
    // Another client refreshed meanwhile: the host answers on s2, where the
    // old comment moved down two lines.
    let mut answer = summary("s2", &paths(1));
    let mut old = held.comments[0].clone();
    old.text = "reworded".into();
    old.placement = Some(ReviewPlacement {
        at: Some(review_placement::At::Lines(ReviewLineRange {
            start: 5,
            end: 5,
        })),
        changed_near: false,
        path: "src/file00.rs".into(),
    });
    let new = ReviewerComment {
        id: b"new".to_vec(),
        anchor: Some(ReviewAnchor {
            commit: "b".repeat(40),
            path: "src/file00.rs".into(),
            lines: Some(ReviewLineRange { start: 7, end: 8 }),
        }),
        text: "on removed lines".into(),
        ..ReviewerComment::default()
    };
    answer.comments = vec![old, new];
    reply.send(Ok(answer)).ok();
    let shown = editing.await.unwrap().unwrap();
    assert_eq!(shown.snapshot, "s1", "the page stays where it is");
    assert!(shown.stale);
    assert_eq!(shown.comments.len(), 2);
    assert_eq!(shown.comments[0].text, "reworded");
    assert_eq!(
        shown.comments[0].placement, held.comments[0].placement,
        "where it sat on the page"
    );
    assert_eq!(
        shown.comments[1]
            .placement
            .as_ref()
            .and_then(|p| p.at.clone()),
        Some(review_placement::At::Compared(
            wire::experimental::ReviewComparedLines {
                commit: "b".repeat(40),
                lines: Some(ReviewLineRange { start: 7, end: 8 }),
            }
        )),
        "on the lines it was written on"
    );
}

/// A sent review opens its round: read without changing the review, found
/// by the input that closed it, its whole diff listed and the body of every file of it
/// fetched from the round's base to its snapshot, a lockfile the review
/// folds included, so the round's page opens it with the comment on it.
#[tokio::test]
async fn a_round_reads_its_whole_diff() {
    let (client, mut calls) = runtime();
    let cache = ReviewCache::new(CACHE_BYTES);
    let review = review(&client, &cache);
    let first = "1".repeat(40);
    let mut now = summary(&"9".repeat(40), &paths(1));
    now.rounds = vec![ReviewRound {
        snapshot: first.clone(),
        base_commit: "a".repeat(40),
        input_id: b"the prompt".to_vec(),
    }];
    now.comments = vec![
        ReviewerComment {
            id: b"sent".to_vec(),
            anchor: Some(ReviewAnchor {
                commit: first.clone(),
                path: "src/file00.rs".into(),
                lines: Some(ReviewLineRange { start: 1, end: 1 }),
            }),
            round: Some(1),
            revision: 1,
            sent_revision: Some(1),
            ..ReviewerComment::default()
        },
        ReviewerComment {
            id: b"on the lock".to_vec(),
            anchor: Some(ReviewAnchor {
                commit: first.clone(),
                path: "Cargo.lock".into(),
                lines: Some(ReviewLineRange { start: 1, end: 1 }),
            }),
            text: "why this bump?".into(),
            round: Some(1),
            revision: 1,
            sent_revision: Some(1),
            ..ReviewerComment::default()
        },
    ];
    let reading = tokio::spawn({
        let review = review.clone();
        async move { review.round(b"review-1", b"the prompt").await }
    });
    let (request, reply) = calls.review_open().await;
    assert!(request.read_only);
    reply.send(Ok(now.clone())).ok();
    let (request, reply) = calls.review_files().await;
    assert!(request.list && request.files.is_empty(), "the list alone");
    assert_eq!(request.from, "a".repeat(40));
    assert_eq!(request.snapshot, first);
    assert_eq!(request.view, ReviewView::All as i32);
    let listed = vec![
        wire::ChangedFile {
            path: "Cargo.lock".into(),
            added: 1,
            flags: vec![wire::FileFlag::Lockfile as i32],
            ..wire::ChangedFile::default()
        },
        wire::ChangedFile {
            path: "src/file00.rs".into(),
            added: 1,
            ..wire::ChangedFile::default()
        },
        wire::ChangedFile {
            path: "src/gone.rs".into(),
            added: 1,
            ..wire::ChangedFile::default()
        },
    ];
    reply
        .send(Ok(ReviewGetFilesResponse {
            changed: listed.clone(),
            ..Default::default()
        }))
        .ok();
    let (request, reply) = calls.review_files().await;
    assert_eq!(request.from, "a".repeat(40));
    assert_eq!(request.snapshot, first);
    assert!(!request.list);
    let asked: Vec<&str> = request.files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(asked, ["Cargo.lock", "src/file00.rs", "src/gone.rs"]);
    reply
        .send(Ok(ReviewGetFilesResponse {
            files: vec![
                file("Cargo.lock"),
                file("src/file00.rs"),
                file("src/gone.rs"),
            ],
            ..Default::default()
        }))
        .ok();
    let round = reading.await.unwrap().unwrap().expect("the round");
    assert_eq!(round.summary.snapshot, first);
    assert_eq!(round.summary.base_commit, "a".repeat(40));
    assert_eq!(round.summary.files, listed);
    assert_eq!(round.files.len(), 3);
    assert!(round.notices.is_empty());
    // Its page opens the lockfile, its body and the comment on it.
    let page = ui_view::review_page(Some(&round.summary), &round.files, &[], &round.state);
    assert!(page.to_fetch.is_empty(), "{:?}", page.to_fetch);
    let lock = page
        .rows
        .iter()
        .position(
            |row| matches!(&row.of, ui_view::ReviewRowOf::File(head) if head.path == "Cargo.lock"),
        )
        .expect("the lockfile's head");
    let ui_view::ReviewRowOf::File(head) = &page.rows[lock].of else {
        unreachable!()
    };
    assert!(!head.folded, "a past round opens every file");
    let body = &page.rows[lock + 1..];
    assert!(body.iter().take_while(|row| !matches!(row.of, ui_view::ReviewRowOf::File(_))).any(
        |row| matches!(&row.of, ui_view::ReviewRowOf::Comment(comment) if comment.text == "why this bump?")
    ));
    assert!(body.iter().take_while(|row| !matches!(row.of, ui_view::ReviewRowOf::File(_))).any(
        |row| matches!(&row.of, ui_view::ReviewRowOf::Line(line) if line.path == "Cargo.lock")
    ));
    calls.none().await;

    // The host has since dropped the round's snapshot: the comments show
    // with their stored code.
    let again = Arc::new(Review::with_cache(
        client.agent(&agent(wire::Kind::ClaudeSdk)),
        ReviewCache::new(CACHE_BYTES),
    ));
    let reading = tokio::spawn({
        let again = again.clone();
        async move { again.round(b"review-1", b"the prompt").await }
    });
    let (_, reply) = calls.review_open().await;
    reply.send(Ok(now.clone())).ok();
    let (_, reply) = calls.review_files().await;
    reply
        .send(Err(RpcError::Refused(wire::Error::new(
            ErrorCode::NotFound,
            "no longer kept",
        ))))
        .ok();
    let round = reading.await.unwrap().unwrap().expect("the round");
    assert!(round.summary.files.is_empty());
    assert_eq!(round.notices, [ui_view::ReviewNotice::RoundGone]);
    assert_eq!(round.summary.comments.len(), 2, "the round's comments");

    // With the host out of reach and nothing loaded, the host's error.
    let empty = Arc::new(Review::with_cache(
        client.agent(&agent(wire::Kind::ClaudeSdk)),
        ReviewCache::new(CACHE_BYTES),
    ));
    let reading = tokio::spawn({
        let empty = empty.clone();
        async move { empty.round(b"review-1", b"the prompt").await }
    });
    let (_, reply) = calls.review_open().await;
    reply.send(Err(host_unreachable())).ok();
    assert!(matches!(
        reading.await.unwrap(),
        Err(error) if ui_runtime::review::unreachable(&error)
    ));
}

#[tokio::test]
async fn an_unimplemented_answer_is_the_review_not_offered() {
    let (client, mut calls) = runtime();
    let cache = ReviewCache::new(CACHE_BYTES);
    let review = review(&client, &cache);
    let mut changed = review.changed();
    changed.borrow_and_update();
    let opening = tokio::spawn({
        let review = review.clone();
        async move { review.open(None, false).await }
    });
    let (_, reply) = calls.review_open().await;
    // What the client reads of a host whose routes have no reviews service.
    reply
        .send(Err(RpcError::Refused(wire::Error::new(
            ErrorCode::Unimplemented,
            wire::HOST_NEEDS_UPDATE,
        ))))
        .ok();
    assert_eq!(
        opening.await.unwrap().unwrap_err(),
        ReviewError::Unavailable(Unavailable::NotOffered)
    );
    assert_eq!(review.unavailable(), Some(Unavailable::NotOffered));
    assert!(changed.has_changed().unwrap());
    assert!(review.summary().is_none());

    // A folder that is not a repository is the host's own answer to Open;
    // a host that could not be reached is not that.
    for (answer, unavailable) in [
        (
            RpcError::Refused(wire::Error::new(
                ErrorCode::FailedPrecondition,
                "not a repository",
            )),
            Some(Unavailable::NotARepository),
        ),
        // Says nothing new of the review, so what was said stands.
        (host_unreachable(), Some(Unavailable::NotARepository)),
    ] {
        let opening = tokio::spawn({
            let review = review.clone();
            async move { review.open(None, false).await }
        });
        let (_, reply) = calls.review_open().await;
        let unreachable = answer.not_sent().is_some();
        reply.send(Err(answer)).ok();
        let refused = opening.await.unwrap().unwrap_err();
        if unreachable {
            assert!(matches!(refused, ReviewError::Rpc(_)), "{refused:?}");
        } else {
            assert_eq!(
                refused,
                ReviewError::Unavailable(Unavailable::NotARepository)
            );
        }
        assert_eq!(review.unavailable(), unavailable);
    }

    // Once the host answers, the review is there.
    opened(&review, &mut calls, summary("s1", &[])).await;
    assert_eq!(review.unavailable(), None);
}

/// An agent's chat gives its review page, addressed to the agent's host.
#[tokio::test]
async fn a_chats_review_is_addressed_to_the_agents_host() {
    let (client, mut calls) = runtime();
    let opening = tokio::spawn(Session::open(
        client,
        agent(wire::Kind::ClaudeSdk),
        40,
        200,
        ManualClock::new(0),
    ));
    let (_, feed) = calls.subscribe_session().await;
    feed.send(snapshot(wire::Kind::ClaudeSdk, 0, &[]));
    feed.send(caught_up(0));
    let session = opening.await.unwrap().unwrap();
    let review = Arc::new(session.reviews());
    let reading = tokio::spawn({
        let review = review.clone();
        async move { review.open(None, false).await }
    });
    let (_, reply) = calls.review_open().await;
    reply.send(Ok(summary("s9", &[]))).ok();
    reading.await.unwrap().unwrap();
    let host = uuid::Uuid::from_slice(HOST_A).unwrap();
    assert_eq!(calls.addressed("Open"), [wire::authority(&host)]);
    drop(feed);
}

/// A round is the round's snapshot against the base it was recorded with,
/// and the comments sent in it still on the review, each on the lines it
/// was written on rather than where a later snapshot placed it. It is found
/// by the input whose taking closed it, never by the comments a chip names:
/// two sends queued at once can carry the same comment.
#[test]
fn a_round_is_found_by_the_input_that_closed_it() {
    let first = "1".repeat(40);
    let comment = |id: &[u8], round: Option<u32>| ReviewerComment {
        id: id.to_vec(),
        revision: 1,
        sent_revision: round.map(|_| 1),
        anchor: Some(ReviewAnchor {
            commit: first.clone(),
            path: "src/lib.rs".into(),
            lines: Some(ReviewLineRange { start: 4, end: 5 }),
        }),
        text: "why".into(),
        round,
        placement: Some(ReviewPlacement {
            at: Some(review_placement::At::Outdated(wire::Empty {})),
            changed_near: true,
            path: "src/lib.rs".into(),
        }),
        ..ReviewerComment::default()
    };
    let mut now = summary(&"9".repeat(40), &paths(2));
    // Two sends were queued at once: the first carried "a", the second
    // "a" and "b". The first, taken first, closed round 1 with "a"; the
    // second closed round 2 with "b" alone.
    now.rounds = vec![
        ReviewRound {
            snapshot: first.clone(),
            base_commit: "a".repeat(40),
            input_id: b"first send".to_vec(),
        },
        ReviewRound {
            snapshot: first.clone(),
            base_commit: "a".repeat(40),
            input_id: b"second send".to_vec(),
        },
        ReviewRound {
            snapshot: "2".repeat(40),
            base_commit: "b".repeat(40),
            input_id: b"all deleted".to_vec(),
        },
    ];
    now.stale = true;
    now.comments = vec![
        comment(b"a", Some(1)),
        comment(b"b", Some(2)),
        comment(b"draft", None),
    ];

    let (number, round) =
        ui_runtime::review::round_of(&now, b"review-1", b"first send").expect("held");
    assert_eq!(number, 1);
    assert_eq!(round.snapshot, first);
    assert_eq!(
        round.base_commit,
        "a".repeat(40),
        "the base it was recorded with"
    );
    assert!(round.rounds.is_empty() && round.files.is_empty() && !round.stale);
    assert_eq!(round.comments.len(), 1);
    assert_eq!(round.comments[0].id, b"a");
    assert_eq!(round.comments[0].round, Some(1));
    assert_eq!(
        round.comments[0].placement,
        Some(ReviewPlacement {
            at: Some(review_placement::At::Lines(ReviewLineRange {
                start: 4,
                end: 5
            })),
            changed_near: false,
            path: "src/lib.rs".into(),
        }),
        "on the lines it was written on"
    );
    assert_eq!(all(&round).from, "a".repeat(40));

    // A comment on removed lines of a file renamed since the compared
    // commit names the old path; on the round's page it sits in the file
    // as the host placed it, by its new name.
    let mut gone = comment(b"gone", Some(1));
    gone.anchor = Some(ReviewAnchor {
        commit: "a".repeat(40),
        path: "src/old.rs".into(),
        lines: Some(ReviewLineRange { start: 2, end: 2 }),
    });
    let mut renamed = now.clone();
    renamed.comments.push(gone);
    let (_, round) =
        ui_runtime::review::round_of(&renamed, b"review-1", b"first send").expect("held");
    let placed = round.comments[1].placement.clone().unwrap();
    assert_eq!(placed.path, "src/lib.rs", "the file the host placed it in");
    assert!(matches!(
        placed.at,
        Some(review_placement::At::Compared(ref compared)) if compared.commit == "a".repeat(40)
    ));

    // The second send's chip opens the round it closed, though it carried
    // "a" too.
    let (number, round) =
        ui_runtime::review::round_of(&now, b"review-1", b"second send").expect("held");
    assert_eq!(number, 2);
    let ids: Vec<&[u8]> = round.comments.iter().map(|c| c.id.as_slice()).collect();
    assert_eq!(ids, [b"b".as_slice()]);

    // Started over since, a round whose comments were all deleted, an
    // input that closed no round, or none named: the chat shows the text.
    assert_eq!(
        ui_runtime::review::round_of(&now, b"review-2", b"first send"),
        None
    );
    assert_eq!(
        ui_runtime::review::round_of(&now, b"review-1", b"all deleted"),
        None
    );
    assert_eq!(
        ui_runtime::review::round_of(&now, b"review-1", b"closed nothing"),
        None
    );
    assert_eq!(ui_runtime::review::round_of(&now, b"review-1", b""), None);
}
