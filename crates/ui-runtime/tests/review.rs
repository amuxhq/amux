//! The review driver against an in-memory runtime: every review call is
//! answered by hand, so a case states exactly what the agent's host said,
//! and that a read already made makes no call at all.

use std::sync::Arc;

use client::{ManualClock, NotSent, RpcError};
use support::*;
use ui_runtime::Session;
use ui_runtime::review::{
    BATCH_FILES, CACHE_BYTES, Followed, Review, ReviewCache, ReviewError, Unavailable,
};
use wire::ErrorCode;
use wire::experimental::{
    REVIEW_ROW, ReviewEdit, ReviewFileDiff, ReviewGetFilesResponse, ReviewLine, ReviewLineKind,
    ReviewLineRange, ReviewPutComment, ReviewRowEntry, ReviewSummary, ReviewView, ReviewerComment,
    review_edit,
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

/// The agent's row with the review's entry, or none.
fn row(entry: Option<(u64, bool)>) -> wire::Agent {
    let mut row = agent(wire::Kind::ClaudeSdk);
    if let Some((counter, moved)) = entry {
        let entry = ReviewRowEntry {
            drafts: 0,
            counter,
            moved,
        };
        REVIEW_ROW.put(&mut row.experimental, &entry);
    }
    row
}

/// Opens `review`, the host answering `answer`.
async fn opened(review: &Arc<Review>, calls: &mut Calls, answer: ReviewSummary) {
    let opening = tokio::spawn({
        let review = review.clone();
        async move { review.open(None, false).await }
    });
    let (request, reply) = calls.review_open().await;
    assert_eq!(request.agent_id, b"agent-1");
    reply.send(Ok(answer.clone())).ok();
    assert_eq!(*opening.await.unwrap().unwrap(), answer);
}

/// Reads `want` of the shown snapshot, the host answering every file each
/// batch asks for.
async fn read_all(review: &Arc<Review>, calls: &mut Calls, want: &[String]) {
    let reading = tokio::spawn({
        let (review, want) = (review.clone(), want.to_vec());
        async move { review.files(ReviewView::All, &want).await }
    });
    let mut answered = 0;
    while answered < want.len() {
        let (request, reply) = calls.review_files().await;
        answered += request.files.len();
        let files = request
            .files
            .iter()
            .map(|asked| file(&asked.path))
            .collect();
        reply.send(Ok(ReviewGetFilesResponse { files })).ok();
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
    read_all(&first, &mut calls, &want).await;

    let again = first.files(ReviewView::All, &want).await.unwrap();
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
    assert_eq!(second.fetched(ReviewView::All).len(), 3);
    second.files(ReviewView::All, &want).await.unwrap();
    calls.none().await;

    // The other view of the same snapshot is another read.
    let since = tokio::spawn({
        let (second, want) = (second.clone(), want.clone());
        async move { second.files(ReviewView::SinceLast, &want).await }
    });
    let (request, reply) = calls.review_files().await;
    assert_eq!(request.view, ReviewView::SinceLast as i32);
    reply
        .send(Ok(ReviewGetFilesResponse {
            files: want.iter().map(|path| file(path)).collect(),
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
        async move { review.files(ReviewView::All, &want).await }
    });
    // The first batch: the host fits ten of the sixteen.
    let (request, reply) = calls.review_files().await;
    assert_eq!(request.snapshot, "s1");
    assert_eq!(request.view, ReviewView::All as i32);
    let asked: Vec<String> = request.files.iter().map(|f| f.path.clone()).collect();
    assert_eq!(asked, want[..BATCH_FILES]);
    reply
        .send(Ok(ReviewGetFilesResponse {
            files: asked[..10].iter().map(|path| file(path)).collect(),
        }))
        .ok();
    // The page has those ten to draw before the next batch is answered.
    changed.changed().await.unwrap();
    let (request, reply) = calls.review_files().await;
    assert_eq!(review.fetched(ReviewView::All).len(), 10);
    let asked: Vec<String> = request.files.iter().map(|f| f.path.clone()).collect();
    assert_eq!(asked, want[10..10 + BATCH_FILES], "the six left out lead");
    reply
        .send(Ok(ReviewGetFilesResponse {
            files: asked.iter().map(|path| file(path)).collect(),
        }))
        .ok();
    let (request, reply) = calls.review_files().await;
    let asked: Vec<String> = request.files.iter().map(|f| f.path.clone()).collect();
    assert_eq!(asked, want[26..]);
    reply
        .send(Ok(ReviewGetFilesResponse {
            files: asked.iter().map(|path| file(path)).collect(),
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
    assert_eq!(review.fetched(ReviewView::All).len(), 40);
    calls.none().await;
}

#[tokio::test]
async fn with_the_host_gone_a_loaded_review_still_reads_and_an_edit_says_so() {
    let (client, mut calls) = runtime();
    let cache = ReviewCache::new(CACHE_BYTES);
    let want = paths(2);
    let loaded = review(&client, &cache);
    opened(&loaded, &mut calls, summary("s1", &want)).await;
    read_all(&loaded, &mut calls, &want).await;
    let lines = ReviewLineRange { start: 1, end: 9 };
    let context = tokio::spawn({
        let loaded = loaded.clone();
        async move { loaded.context("s1", "src/file00.rs", lines).await }
    });
    let (request, reply) = calls.review_files().await;
    assert_eq!(request.files[0].context_commit, "s1");
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
        }))
        .ok();
    assert_eq!(context.await.unwrap().unwrap().len(), 1);

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
    assert_eq!(page.files(ReviewView::All, &want).await.unwrap().len(), 2);
    assert_eq!(
        page.context("s1", "src/file00.rs", lines).await.unwrap()[0].text,
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
    assert!(!request.refresh, "following never refreshes");
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

/// A sent review opens its round from the review as the host holds it now:
/// the round's snapshot against the base it was shown against, and of the
/// comments it sent those still on the review, each on the lines it was
/// written on rather than where a later snapshot placed it.
#[test]
fn a_round_is_its_snapshot_and_the_comments_it_sent() {
    use wire::experimental::{ReviewAnchor, ReviewPlacement, ReviewRound, review_placement};
    let first = "1".repeat(40);
    let comment = |id: &[u8], round: Option<u32>| ReviewerComment {
        id: id.to_vec(),
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
        }),
        ..ReviewerComment::default()
    };
    let mut now = summary(&"9".repeat(40), &paths(2));
    now.rounds = vec![
        ReviewRound {
            snapshot: first.clone(),
            base_commit: "a".repeat(40),
            sent_at_ms: 1,
        },
        ReviewRound {
            snapshot: "2".repeat(40),
            base_commit: "b".repeat(40),
            sent_at_ms: 2,
        },
    ];
    now.stale = true;
    now.comments = vec![
        comment(b"sent", Some(1)),
        comment(b"other round", Some(2)),
        comment(b"draft", None),
    ];
    let sent = vec![b"sent".to_vec(), b"deleted since".to_vec()];

    let round = ui_runtime::review::round(&now, b"review-1", &first, &sent).expect("held");
    assert_eq!(round.snapshot, first);
    assert_eq!(
        round.base_commit,
        "a".repeat(40),
        "the base it was shown against"
    );
    assert!(round.rounds.is_empty() && round.files.is_empty() && !round.stale);
    assert_eq!(round.comments.len(), 1);
    assert_eq!(round.comments[0].id, b"sent");
    assert_eq!(round.comments[0].round, Some(1));
    assert_eq!(
        round.comments[0].placement, None,
        "on the lines it was written on"
    );

    // Started over since, or every comment it sent deleted: the host holds
    // none of the round.
    assert_eq!(
        ui_runtime::review::round(&now, b"review-2", &first, &sent),
        None
    );
    assert_eq!(
        ui_runtime::review::round(&now, b"review-1", &first, &[b"deleted since".to_vec()]),
        None
    );
    // A draft is not sent, whatever names it.
    assert_eq!(
        ui_runtime::review::round(&now, b"review-1", &first, &[b"draft".to_vec()]),
        None
    );
}
