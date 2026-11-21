//! The review page open on a chat, as an app draws it: the agent's review
//! read and edited through the shared runtime's [`Review`], composed into
//! the shared view's [`ReviewPage`] with the person's own choices, and
//! following the agent's row so the page changes when the review does.
//!
//! The page is handed over whole whenever it moves, and an anchor's row
//! indices count the rows of the page handed over last, so a selection
//! always names the lines the person saw.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};

use tokio::sync::watch;
use tokio::task::JoinHandle;
use ui_runtime::review::{Comparison, Followed, Review, ReviewError, RowMark, Unavailable};
use ui_view::{
    ReviewAnchor, ReviewContextAsk, ReviewNotice, ReviewPage, ReviewPageState, ReviewView,
};
use wire::ErrorCode;
use wire::experimental::{
    ReviewEdit, ReviewLineRange, ReviewPutComment, ReviewSummary, review_edit,
};

use crate::coalesce::WakeFn;
use crate::values::{ReviewChange, ReviewChoice, ReviewOutcome};

/// The agent's row as following the review reads it: only its entries.
pub(crate) type RowFn = Arc<dyn Fn() -> wire::Agent + Send + Sync>;

/// A review page open on a chat.
pub struct ReviewSession {
    review: Arc<Review>,
    held: Mutex<Held>,
    /// Something the page draws from changed since it was last composed.
    moved: AtomicBool,
    /// The agent's row has been followed once, before the first open.
    began: AtomicBool,
    row: RowFn,
    wake: WakeFn,
    follower: Mutex<Option<JoinHandle<()>>>,
}

#[derive(Default)]
struct Held {
    /// The person's choices; notices are kept apart, in `notices`.
    choices: ReviewPageState,
    /// What calls that failed taught the page.
    notices: Vec<ReviewNotice>,
    /// The files being fetched now, by comparison and path, so asking
    /// again while they are on their way asks for nothing twice.
    fetching: HashSet<(Comparison, String)>,
    /// The page as it was handed over last.
    handed: Option<ReviewPage>,
}

/// Which call failed, for what the page learns of it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Call {
    Open,
    Files(ReviewView),
    Other,
}

impl ReviewSession {
    /// A page on `review`, following the agent's row: `rows` is marked
    /// whenever it may have changed and `row` reads it. `wake` tells the
    /// chat's host the page moved.
    pub(crate) fn start(
        review: Arc<Review>,
        rows: watch::Receiver<()>,
        row: RowFn,
        wake: WakeFn,
    ) -> Arc<ReviewSession> {
        let changed = review.changed();
        let session = Arc::new(ReviewSession {
            review,
            held: Mutex::new(Held::default()),
            moved: AtomicBool::new(true),
            began: AtomicBool::new(false),
            row: row.clone(),
            wake,
            follower: Mutex::new(None),
        });
        // Read now, so a change to the row from here on is followed however
        // late the follower first runs.
        let followed = RowMark::of(&row());
        let follower = tokio::spawn(follow(
            Arc::downgrade(&session),
            changed,
            rows,
            row,
            followed,
        ));
        *session
            .follower
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = Some(follower);
        session
    }

    /// The review the page reads, which a send takes its drafts from.
    pub fn review(&self) -> &Arc<Review> {
        &self.review
    }

    fn held(&self) -> MutexGuard<'_, Held> {
        self.held
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    fn mark(&self) {
        self.moved.store(true, Ordering::SeqCst);
        (self.wake)();
    }

    /// Opens the review at the agent's host: begun on a first look, on a
    /// new snapshot when the tree changed and no drafts wait. `base_branch`
    /// picks the base anew; `refresh` takes a snapshot even with drafts,
    /// moving them onto it.
    pub async fn open(&self, base_branch: Option<String>, refresh: bool) -> ReviewOutcome {
        // The row the page opens from is where following starts, so the
        // open's own answer is not read again.
        if !self.began.swap(true, Ordering::SeqCst) {
            let answer = self.review.follow(&(self.row)()).await;
            self.followed(answer);
        }
        let answer = self.review.open(base_branch, refresh).await.map(drop);
        self.answered(Call::Open, answer)
    }

    /// Fetches the bodies of `paths` for the view the page shows, or with
    /// none given every file the page still needs; those already on their
    /// way are not asked for again. The page moves as each batch lands.
    pub async fn files(&self, paths: Vec<String>) -> ReviewOutcome {
        let page = self.page();
        let Some(header) = &page.header else {
            return ReviewOutcome::Done;
        };
        let comparison = Comparison::of_page(header);
        let paths = if paths.is_empty() {
            page.to_fetch.clone()
        } else {
            paths
        };
        let asking: Vec<String> = {
            let mut held = self.held();
            paths
                .into_iter()
                .filter(|path| held.fetching.insert((comparison.clone(), path.clone())))
                .collect()
        };
        if asking.is_empty() {
            return ReviewOutcome::Done;
        }
        let answer = self.review.files(&comparison, &asking).await.map(drop);
        {
            let mut held = self.held();
            for path in asking {
                held.fetching.remove(&(comparison.clone(), path));
            }
        }
        self.answered(Call::Files(header.view), answer)
    }

    /// Loads the lines `ask` names, from one of the page's gaps; the gap
    /// says it is loading until they arrive.
    pub async fn context(&self, ask: ReviewContextAsk) -> ReviewOutcome {
        let Some(comparison) = self.page().header.as_ref().map(Comparison::of_page) else {
            return ReviewOutcome::Failed("the review has not been opened".into());
        };
        {
            let mut held = self.held();
            if !held.choices.asked.contains(&ask) {
                held.choices.asked.push(ask.clone());
            }
        }
        self.mark();
        let lines = ReviewLineRange {
            start: ask.start,
            end: ask.end,
        };
        let answer = self
            .review
            .context(&comparison, &ask.commit, &ask.path, lines)
            .await
            .map(drop);
        self.answered(Call::Other, answer)
    }

    /// Makes `changes` at the agent's host, all of them or none.
    pub async fn edit(&self, changes: Vec<ReviewChange>) -> ReviewOutcome {
        let edits = match edits(&self.review, changes) {
            Ok(edits) => edits,
            Err(failed) => return failed,
        };
        let answer = self.review.edit(edits).await.map(drop);
        self.answered(Call::Other, answer)
    }

    /// Takes a choice the person made on the page.
    pub fn choose(&self, choice: ReviewChoice) {
        {
            let mut held = self.held();
            match choice {
                ReviewChoice::View(view) => held.choices.view = Some(view),
                ReviewChoice::ToggleFile(path) => held.choices.toggle_file(&path),
                ReviewChoice::ToggleComment(id) => held.choices.toggle_comment(&id),
            }
        }
        self.mark();
    }

    /// The page as it stands, composed from what was fetched.
    pub fn page(&self) -> ReviewPage {
        let summary = self.review.summary();
        let mut state = {
            let held = self.held();
            let mut state = held.choices.clone();
            state.notices = held.notices.clone();
            state
        };
        state.notices.extend(match self.review.unavailable() {
            Some(Unavailable::NotOffered) => Some(ReviewNotice::NotOffered),
            Some(Unavailable::NotARepository) => Some(ReviewNotice::NotARepository),
            None => None,
        });
        let files = |view| {
            summary
                .as_deref()
                .and_then(|summary| Comparison::shown(summary, wire_view(view)))
                .map(|comparison| self.review.fetched(&comparison).into_values().collect())
                .unwrap_or_default()
        };
        let context: Vec<_> = state
            .asked
            .iter()
            .filter_map(|ask| {
                let lines = ReviewLineRange {
                    start: ask.start,
                    end: ask.end,
                };
                let held = self.review.context_held(&ask.commit, &ask.path, lines)?;
                Some((ask.clone(), held.to_vec()))
            })
            .collect();
        ui_view::review_page_of(summary.as_deref(), files, &context, &state)
    }

    /// The page to hand over: when it moved since it was handed over last,
    /// or whatever it is when `whole`, as for a model opening on the chat.
    /// What is returned is the page handed over from now.
    pub fn hand_over(&self, whole: bool) -> Option<ReviewPage> {
        if !self.moved.swap(false, Ordering::SeqCst) && !whole {
            return None;
        }
        let page = self.page();
        let mut held = self.held();
        if !whole && held.handed.as_ref() == Some(&page) {
            return None;
        }
        held.handed = Some(page.clone());
        Some(page)
    }

    /// The page as it stands, handed over from now.
    pub fn read(&self) -> ReviewPage {
        self.moved.store(false, Ordering::SeqCst);
        let page = self.page();
        self.held().handed = Some(page.clone());
        page
    }

    /// The lines a selection of the handed page's rows, from one index to
    /// another in either order, would be commented on; none for a
    /// selection with no line, or across files.
    pub fn anchor(&self, from: usize, to: usize) -> Option<ReviewAnchor> {
        let held = self.held();
        ui_view::anchor(held.handed.as_ref()?, from, to)
    }

    /// What a call's answer teaches the page, and what it came to. The
    /// page is marked moved before the call is answered, so the delta that
    /// carries what it moved comes before its answer.
    fn answered(&self, call: Call, answer: Result<(), ReviewError>) -> ReviewOutcome {
        let outcome = outcome(&answer);
        {
            let mut held = self.held();
            let notices = &mut held.notices;
            match &answer {
                Ok(()) => {
                    notices.retain(|notice| *notice != ReviewNotice::Unreachable);
                    if call == Call::Open {
                        notices.retain(|notice| {
                            !matches!(
                                notice,
                                ReviewNotice::SnapshotFailed { .. } | ReviewNotice::RoundGone
                            )
                        });
                    }
                }
                Err(ReviewError::Rpc(error)) => {
                    let notice = if error.not_sent().is_some() || error.uncertain() {
                        Some(ReviewNotice::Unreachable)
                    } else if call == Call::Open {
                        notices.retain(|notice| {
                            !matches!(notice, ReviewNotice::SnapshotFailed { .. })
                        });
                        Some(ReviewNotice::SnapshotFailed {
                            message: error.to_string(),
                        })
                    } else if call == Call::Files(ReviewView::SinceLast)
                        && matches!(
                            error.code(),
                            Some(ErrorCode::NotFound | ErrorCode::DataLoss)
                        )
                    {
                        Some(ReviewNotice::RoundGone)
                    } else {
                        None
                    };
                    if let Some(notice) = notice
                        && !notices.contains(&notice)
                    {
                        notices.push(notice);
                    }
                }
                Err(_) => {}
            }
        }
        self.mark();
        outcome
    }

    /// What following the row came to.
    fn followed(&self, answer: Result<Followed, ReviewError>) {
        match answer {
            Ok(Followed::Reread) => {
                self.answered(Call::Other, Ok(()));
            }
            Ok(_) => {}
            Err(error) => {
                self.answered(Call::Other, Err(error));
            }
        }
    }
}

impl Drop for ReviewSession {
    fn drop(&mut self) {
        if let Some(follower) = self
            .follower
            .get_mut()
            .unwrap_or_else(|poison| poison.into_inner())
            .take()
        {
            follower.abort();
        }
    }
}

/// Follows the review and the agent's row for the page until it closes:
/// anything the review fetched or learned moves the page, and a change to
/// the review's entry on the row is followed.
async fn follow(
    session: Weak<ReviewSession>,
    mut changed: watch::Receiver<()>,
    mut rows: watch::Receiver<()>,
    row: RowFn,
    mut followed: Option<RowMark>,
) {
    loop {
        tokio::select! {
            moved = changed.changed() => {
                if moved.is_err() {
                    return;
                }
                let Some(session) = session.upgrade() else {
                    return;
                };
                session.mark();
            }
            moved = rows.changed() => {
                if moved.is_err() {
                    return;
                }
                let agent = row();
                let mark = RowMark::of(&agent);
                if mark == followed {
                    continue;
                }
                followed = mark;
                let Some(session) = session.upgrade() else {
                    return;
                };
                let answer = session.review.follow(&agent).await;
                session.followed(answer);
            }
        }
    }
}

/// `changes` as the review's edits. A new comment's id is made here; a
/// reworded one keeps where it was written, from the summary last read.
fn edits(review: &Review, changes: Vec<ReviewChange>) -> Result<Vec<ReviewEdit>, ReviewOutcome> {
    let summary = review.summary();
    changes
        .into_iter()
        .map(|change| {
            let of = match change {
                ReviewChange::Comment { anchor, text } => review_edit::Of::Put(ReviewPutComment {
                    id: ui_runtime::inputs::input_id(),
                    anchor: Some(anchor.to_wire()),
                    text,
                }),
                ReviewChange::Reword { id, text } => {
                    let written = summary
                        .as_deref()
                        .and_then(|summary| {
                            summary.comments.iter().find(|comment| comment.id == id)
                        })
                        .ok_or_else(|| {
                            ReviewOutcome::Failed("the review has no such comment".into())
                        })?;
                    review_edit::Of::Put(ReviewPutComment {
                        id,
                        anchor: written.anchor.clone(),
                        text,
                    })
                }
                ReviewChange::Delete(id) => review_edit::Of::Delete(id),
                ReviewChange::DiscardDrafts => review_edit::Of::DiscardDrafts(wire::Empty {}),
                ReviewChange::StartOver => review_edit::Of::StartOver(wire::Empty {}),
            };
            Ok(ReviewEdit { of: Some(of) })
        })
        .collect()
}

/// Makes `changes` on the agent's review with no page open on the chat:
/// the chip's ✕ discarding its drafts.
pub(crate) async fn edit_closed(review: &Review, changes: Vec<ReviewChange>) -> ReviewOutcome {
    match edits(review, changes) {
        Ok(edits) => outcome(&review.edit(edits).await.map(drop)),
        Err(failed) => failed,
    }
}

/// The page of the round of the review `review_id` that the input
/// `input_id` closed, the message holding the sent review: its whole diff,
/// every file open, with the comments it sent, read from the agent's host
/// as any look at it is; with the host out of reach, from what this process
/// loaded of it, and the page says so. With the host out of reach and none
/// of the round loaded, a page with nothing but that to say. None when the
/// input closed no round or the host holds none of it, and the chat shows
/// the text the agent read instead.
pub(crate) async fn round_page(
    review: &Review,
    review_id: &[u8],
    input_id: &[u8],
) -> Option<ReviewPage> {
    let round = match review.round(review_id, input_id).await {
        Ok(round) => round?,
        Err(error) if ui_runtime::review::unreachable(&error) => {
            let state = ReviewPageState {
                notices: vec![ReviewNotice::Unreachable],
                ..ReviewPageState::default()
            };
            return Some(ui_view::review_page(None, &[], &[], &state));
        }
        Err(_) => return None,
    };
    Some(ui_view::review_page(
        Some(&round.summary),
        &round.files,
        &[],
        &round.state,
    ))
}

/// What a call came to, as the app hears it.
fn outcome(answer: &Result<(), ReviewError>) -> ReviewOutcome {
    match answer {
        Ok(()) => ReviewOutcome::Done,
        Err(ReviewError::Unavailable(_)) => ReviewOutcome::Unavailable,
        Err(ReviewError::Rpc(error)) if error.not_sent().is_some() => ReviewOutcome::Unreachable,
        Err(ReviewError::Rpc(error)) if error.uncertain() => ReviewOutcome::NotConfirmed,
        Err(error) => ReviewOutcome::Failed(error.to_string()),
    }
}

fn wire_view(view: ReviewView) -> wire::experimental::ReviewView {
    match view {
        ReviewView::All => wire::experimental::ReviewView::All,
        ReviewView::SinceLast => wire::experimental::ReviewView::SinceLast,
    }
}

/// The text attachment that sends the drafts of `summary`, naming exactly
/// those; none while it holds no drafts.
pub fn sent_review(summary: &ReviewSummary) -> Option<wire::Attachment> {
    let drafts: Vec<_> = summary
        .comments
        .iter()
        .filter(|comment| comment.round.is_none())
        .cloned()
        .collect();
    (!drafts.is_empty()).then(|| ui_view::sent_review::attachment(summary, &drafts))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::atomic::AtomicUsize;
    use std::time::Duration;

    use client::RpcError;
    use tonic::{Request, Response, Status};
    use ui_runtime::review::ReviewCache;
    use ui_view::{ReviewBanner, ReviewRowOf};
    use wire::experimental::reviews1_server::Reviews1;
    use wire::experimental::{
        REVIEW_ROW, ReviewEditRequest, ReviewFileDiff, ReviewGetFilesRequest,
        ReviewGetFilesResponse, ReviewHunk, ReviewLine, ReviewLineKind, ReviewOpenRequest,
        ReviewPlacement, ReviewRound, ReviewRowEntry, ReviewerComment, SENT_REVIEW,
        review_placement,
    };

    use super::*;

    const SNAPSHOT: &str = "5555555555555555555555555555555555555555";
    const BASE: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const PATIENCE: Duration = Duration::from_secs(5);

    /// The agent's host as the review sees it: what it answers, and the
    /// calls it was asked.
    #[derive(Default)]
    struct Host {
        summary: ReviewSummary,
        files: BTreeMap<String, ReviewFileDiff>,
        /// Every call is answered with this while it is set.
        failing: Option<RpcError>,
        opens: usize,
        /// Of `opens`, those that read without changing the review.
        reads: usize,
        gets: Vec<ReviewGetFilesRequest>,
        edits: Vec<ReviewEditRequest>,
    }

    #[derive(Clone, Default)]
    struct Fake(Arc<Mutex<Host>>);

    impl Fake {
        fn host(&self) -> MutexGuard<'_, Host> {
            self.0.lock().unwrap()
        }

        fn failing(&self) -> Result<(), Status> {
            match self.host().failing.clone() {
                Some(error) => Err(error.into_status()),
                None => Ok(()),
            }
        }
    }

    #[tonic::async_trait]
    impl Reviews1 for Fake {
        async fn open(
            &self,
            request: Request<ReviewOpenRequest>,
        ) -> Result<Response<ReviewSummary>, Status> {
            self.failing()?;
            let mut host = self.host();
            host.opens += 1;
            host.reads += usize::from(request.into_inner().read_only);
            Ok(Response::new(host.summary.clone()))
        }

        async fn get_files(
            &self,
            request: Request<ReviewGetFilesRequest>,
        ) -> Result<Response<ReviewGetFilesResponse>, Status> {
            self.failing()?;
            let request = request.into_inner();
            let mut host = self.host();
            let files = request
                .files
                .iter()
                .filter_map(|asked| {
                    let mut file = host.files.get(&asked.path)?.clone();
                    file.context = asked
                        .context
                        .iter()
                        .flat_map(|range| range.start..=range.end)
                        .map(|line| ReviewLine {
                            kind: ReviewLineKind::Context as i32,
                            old_line: line,
                            new_line: line,
                            text: format!("read from {:?} at {line}", asked.context_side()),
                            ..ReviewLine::default()
                        })
                        .collect();
                    Some(file)
                })
                .collect();
            // The fake's one comparison lists what its summary does.
            let changed = match request.list {
                true => host.summary.files.clone(),
                false => Vec::new(),
            };
            host.gets.push(request);
            Ok(Response::new(ReviewGetFilesResponse { files, changed }))
        }

        async fn edit(
            &self,
            request: Request<ReviewEditRequest>,
        ) -> Result<Response<ReviewSummary>, Status> {
            self.failing()?;
            let request = request.into_inner();
            let mut host = self.host();
            for edit in &request.edits {
                match edit.of.clone() {
                    Some(review_edit::Of::Put(put)) => {
                        let lines = put.anchor.as_ref().and_then(|anchor| anchor.lines);
                        let comment = ReviewerComment {
                            id: put.id.clone(),
                            anchor: put.anchor,
                            text: put.text,
                            placement: Some(ReviewPlacement {
                                at: lines.map(review_placement::At::Lines),
                                changed_near: false,
                                path: "src/lib.rs".into(),
                            }),
                            ..ReviewerComment::default()
                        };
                        let comments = &mut host.summary.comments;
                        match comments.iter_mut().find(|held| held.id == put.id) {
                            Some(held) => *held = comment,
                            None => comments.push(comment),
                        }
                    }
                    Some(review_edit::Of::Delete(id)) => {
                        host.summary.comments.retain(|comment| comment.id != id);
                    }
                    Some(review_edit::Of::DiscardDrafts(_)) => {
                        host.summary
                            .comments
                            .retain(|comment| comment.round.is_some());
                    }
                    Some(review_edit::Of::StartOver(_)) | None => {}
                }
            }
            host.edits.push(request);
            Ok(Response::new(host.summary.clone()))
        }
    }

    fn line(kind: ReviewLineKind, old_line: u32, new_line: u32, text: &str) -> ReviewLine {
        ReviewLine {
            kind: kind as i32,
            old_line,
            new_line,
            text: text.into(),
            ..ReviewLine::default()
        }
    }

    /// A review of one changed file, `src/lib.rs`, whose second line was
    /// rewritten, at line 40 of a longer file.
    fn reviewed() -> Fake {
        let fake = Fake::default();
        {
            let mut host = fake.host();
            host.summary = ReviewSummary {
                id: b"review-1".to_vec(),
                base_branch: "main".into(),
                base_commit: BASE.into(),
                snapshot: SNAPSHOT.into(),
                files: vec![wire::ChangedFile {
                    path: "src/lib.rs".into(),
                    added: 1,
                    removed: 1,
                    ..wire::ChangedFile::default()
                }],
                objects: "/agents/a/review/objects".into(),
                root: "/work/repo".into(),
                ..ReviewSummary::default()
            };
            host.files.insert(
                "src/lib.rs".into(),
                ReviewFileDiff {
                    path: "src/lib.rs".into(),
                    hunks: vec![ReviewHunk {
                        old_commit: BASE.into(),
                        old_start: 40,
                        new_start: 40,
                        lines: vec![
                            line(ReviewLineKind::Context, 40, 40, "fn open() {"),
                            line(ReviewLineKind::Removed, 41, 0, "    let a = 1;"),
                            line(ReviewLineKind::Added, 0, 41, "    let a = 2;"),
                            line(ReviewLineKind::Context, 42, 42, "}"),
                        ],
                        ..ReviewHunk::default()
                    }],
                    ..ReviewFileDiff::default()
                },
            );
        }
        fake
    }

    /// The test's end of a page: the agent's row it follows, and the wakes
    /// it gave.
    struct Opened {
        page: Arc<ReviewSession>,
        row: Arc<Mutex<wire::Agent>>,
        rows: watch::Sender<()>,
        wakes: Arc<AtomicUsize>,
    }

    impl Opened {
        /// Puts the review's entry on the agent's row, as its host would.
        fn entry(&self, drafts: u32, counter: u64, moved: bool) {
            let entry = ReviewRowEntry {
                drafts,
                counter,
                moved,
                review_id: b"review-1".to_vec(),
            };
            REVIEW_ROW.put(&mut self.row.lock().unwrap().experimental, &entry);
            self.rows.send_replace(());
        }
    }

    /// A page on the review `fake` answers, not yet opened.
    fn page(fake: &Fake) -> Opened {
        let routes = tonic::service::Routes::default()
            .add_service(wire::experimental::reviews1_server(fake.clone()));
        let client = client::Client::in_memory(routes, client::Surface::Ios);
        let agent = client.agent(&wire::AgentRef {
            host_id: vec![7; 16],
            agent_id: b"agent-1".to_vec(),
        });
        let review = Arc::new(Review::with_cache(agent, ReviewCache::new(1 << 20)));
        let row = Arc::new(Mutex::new(wire::Agent::default()));
        let (rows, watched) = watch::channel(());
        let wakes = Arc::new(AtomicUsize::new(0));
        let page = ReviewSession::start(
            review,
            watched,
            {
                let row = row.clone();
                Arc::new(move || row.lock().unwrap().clone())
            },
            {
                let wakes = wakes.clone();
                Arc::new(move || {
                    wakes.fetch_add(1, Ordering::SeqCst);
                })
            },
        );
        Opened {
            page,
            row,
            rows,
            wakes,
        }
    }

    /// Waits until `check` holds of the page.
    async fn until(page: &ReviewSession, what: &str, check: impl Fn(&ReviewPage) -> bool) {
        tokio::time::timeout(PATIENCE, async {
            while !check(&page.page()) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("the page never showed {what}"));
    }

    fn index(page: &ReviewPage, wanted: impl Fn(&ReviewRowOf) -> bool) -> usize {
        page.rows
            .iter()
            .position(|row| wanted(&row.of))
            .expect("the row is on the page")
    }

    /// Opening draws the page from the summary first and the bodies as they
    /// arrive; a selection of the handed page's rows is the anchor a new
    /// comment is written on, and the comment the host then holds is on
    /// the page.
    #[tokio::test]
    async fn a_page_opens_fetches_its_files_and_comments_on_a_selection() {
        let fake = reviewed();
        let opened = page(&fake);
        let page = &opened.page;
        assert_eq!(page.open(None, false).await, ReviewOutcome::Done);
        let first = page.hand_over(false).expect("the open moved the page");
        assert_eq!(first.to_fetch, vec!["src/lib.rs".to_owned()]);
        assert!(
            first
                .rows
                .iter()
                .any(|row| matches!(row.of, ReviewRowOf::Loading { .. })),
            "{first:#?}"
        );
        assert!(opened.wakes.load(Ordering::SeqCst) > 0);

        assert_eq!(page.files(Vec::new()).await, ReviewOutcome::Done);
        assert_eq!(fake.host().gets.len(), 1);
        let drawn = page.hand_over(false).expect("the bodies moved the page");
        assert!(drawn.to_fetch.is_empty());
        assert_eq!(page.hand_over(false), None, "nothing moved since");
        // Asking again asks the host for nothing: the bodies are held.
        assert_eq!(
            page.files(vec!["src/lib.rs".into()]).await,
            ReviewOutcome::Done
        );
        assert_eq!(fake.host().gets.len(), 1);

        let added = index(
            &drawn,
            |of| matches!(of, ReviewRowOf::Line(line) if line.new_line == Some(41) && line.old_line.is_none()),
        );
        let closing = index(
            &drawn,
            |of| matches!(of, ReviewRowOf::Line(line) if line.new_line == Some(42)),
        );
        let anchor = page.anchor(closing, added).expect("lines are selected");
        assert_eq!(
            anchor,
            ReviewAnchor {
                commit: SNAPSHOT.into(),
                path: "src/lib.rs".into(),
                start: 41,
                end: 42,
            }
        );
        let removed = index(
            &drawn,
            |of| matches!(of, ReviewRowOf::Line(line) if line.old_line == Some(41) && line.new_line.is_none()),
        );
        assert_eq!(
            page.anchor(removed, removed).map(|anchor| anchor.commit),
            Some(BASE.to_owned()),
            "a removed line is the compared commit's"
        );

        let comment = ReviewChange::Comment {
            anchor: anchor.clone(),
            text: "Keep it at one.".into(),
        };
        assert_eq!(page.edit(vec![comment]).await, ReviewOutcome::Done);
        let edit = fake.host().edits.remove(0);
        let Some(review_edit::Of::Put(put)) = edit.edits[0].of.clone() else {
            panic!("a put: {edit:?}");
        };
        assert_eq!(put.anchor, Some(anchor.to_wire()));
        assert_eq!(put.id.len(), 16, "a new comment's id is made here");
        let commented = page.hand_over(false).expect("the comment moved the page");
        assert!(
            commented.rows.iter().any(|row| matches!(
                &row.of,
                ReviewRowOf::Comment(comment) if comment.text == "Keep it at one."
            )),
            "{commented:#?}"
        );
        assert_eq!(
            commented.header.as_ref().map(|header| header.drafts),
            Some(1)
        );

        // New words keep where the comment was written.
        let reword = ReviewChange::Reword {
            id: put.id.clone(),
            text: "Keep it at one, please.".into(),
        };
        assert_eq!(page.edit(vec![reword]).await, ReviewOutcome::Done);
        let edit = fake.host().edits.remove(0);
        let Some(review_edit::Of::Put(again)) = edit.edits[0].of.clone() else {
            panic!("a put: {edit:?}");
        };
        assert_eq!((again.id, again.anchor), (put.id, put.anchor));
        let nobody = ReviewChange::Reword {
            id: b"nobody".to_vec(),
            text: "?".into(),
        };
        assert!(matches!(
            page.edit(vec![nobody]).await,
            ReviewOutcome::Failed(_)
        ));
        assert!(fake.host().edits.is_empty(), "nothing asked of the host");
    }

    /// Following the agent's row: a change counter that moved reads the
    /// review again, and a tree that only moved says so without a call.
    #[tokio::test]
    async fn the_page_follows_the_reviews_entry_on_the_agents_row() {
        let fake = reviewed();
        let opened = page(&fake);
        let page = &opened.page;
        opened.entry(0, 1, false);
        page.open(None, false).await;
        assert_eq!(fake.host().opens, 1);

        fake.host().summary.comments.push(ReviewerComment {
            id: b"c1".to_vec(),
            anchor: Some(
                ReviewAnchor {
                    commit: SNAPSHOT.into(),
                    path: "src/lib.rs".into(),
                    start: 41,
                    end: 41,
                }
                .to_wire(),
            ),
            text: "from another client".into(),
            ..ReviewerComment::default()
        });
        opened.entry(1, 2, false);
        until(page, "the other client's draft", |page| {
            page.header
                .as_ref()
                .is_some_and(|header| header.drafts == 1)
        })
        .await;
        assert_eq!(fake.host().opens, 2);
        assert_eq!(fake.host().reads, 1, "following reads, changing nothing");

        opened.entry(1, 2, true);
        until(page, "the tree moving", |page| {
            page.banners.contains(&ReviewBanner::ChangedSinceYouStarted)
        })
        .await;
        assert_eq!(fake.host().opens, 2, "a moved tree alone asks nothing");
    }

    /// What a failed call teaches the page: a host that does not offer
    /// reviews, one that cannot be reached until it can, and a snapshot
    /// that could not be taken.
    #[tokio::test]
    async fn the_page_says_what_failed_calls_taught_it() {
        let fake = reviewed();
        let opened = page(&fake);
        let page = &opened.page;

        fake.host().failing = Some(RpcError::Refused(wire::Error::new(
            wire::ErrorCode::Internal,
            "a file vanished while the snapshot was taken",
        )));
        assert!(matches!(
            page.open(None, false).await,
            ReviewOutcome::Failed(_)
        ));
        assert_eq!(
            page.page().banners,
            vec![ReviewBanner::SnapshotFailed {
                message: "a file vanished while the snapshot was taken".into()
            }]
        );

        fake.host().failing = None;
        assert_eq!(page.open(None, false).await, ReviewOutcome::Done);
        assert!(page.page().banners.is_empty());

        fake.host().failing = Some(RpcError::Refused(wire::Error::because(
            wire::ErrorReason::HostUnreachable,
            "the desk is away",
        )));
        let discard = vec![ReviewChange::DiscardDrafts];
        assert_eq!(page.edit(discard.clone()).await, ReviewOutcome::Unreachable);
        let away = page.page();
        assert_eq!(away.banners, vec![ReviewBanner::Unreachable]);
        assert!(away.header.is_some(), "what was loaded still reads");

        fake.host().failing = None;
        assert_eq!(page.edit(discard).await, ReviewOutcome::Done);
        assert!(page.page().banners.is_empty());

        // A host that does not serve reviews says so until one answers.
        fake.host().failing = Some(RpcError::Refused(wire::Error::new(
            wire::ErrorCode::Unimplemented,
            "no such service",
        )));
        assert_eq!(page.open(None, false).await, ReviewOutcome::Unavailable);
        assert_eq!(page.page().banners, vec![ReviewBanner::NotOffered]);

        fake.host().failing = None;
        assert_eq!(page.open(None, false).await, ReviewOutcome::Done);
        assert!(page.page().banners.is_empty());
    }

    /// More context from a gap is read from the side of the comparison the
    /// ask's commit is, and drawn once it arrives.
    #[tokio::test]
    async fn more_context_is_read_from_the_commit_its_ask_names() {
        let fake = reviewed();
        let opened = page(&fake);
        let page = &opened.page;
        page.open(None, false).await;
        page.files(Vec::new()).await;
        let drawn = page.read();
        let ask = drawn
            .rows
            .iter()
            .find_map(|row| match &row.of {
                ReviewRowOf::Gap(gap) => gap.up.clone(),
                _ => None,
            })
            .expect("a gap above the hunk loads lines upward");
        assert_eq!(page.context(ask.clone()).await, ReviewOutcome::Done);
        let asked = fake.host().gets.last().cloned().unwrap();
        let side = match ask.commit.as_str() {
            SNAPSHOT => wire::experimental::ReviewSide::Snapshot,
            _ => wire::experimental::ReviewSide::Compared,
        };
        assert_eq!(
            asked.files[0].context_side(),
            side,
            "the side its commit is"
        );
        let read = page.page();
        assert!(
            read.rows.iter().any(|row| matches!(
                &row.of,
                ReviewRowOf::Line(line) if line.extra && line.text.starts_with("read from")
            )),
            "{read:#?}"
        );
    }

    /// The person's choices fold a file and switch the view.
    #[tokio::test]
    async fn choices_are_drawn_at_once() {
        let fake = reviewed();
        fake.host().summary.rounds.push(ReviewRound {
            snapshot: "4444444444444444444444444444444444444444".into(),
            base_commit: BASE.into(),
            input_id: b"the prompt".to_vec(),
        });
        let opened = page(&fake);
        let page = &opened.page;
        page.open(None, false).await;
        let view = |page: &ReviewPage| page.header.as_ref().map(|header| header.view);
        assert_eq!(view(&page.read()), Some(ReviewView::SinceLast));
        let woken = opened.wakes.load(Ordering::SeqCst);
        page.choose(ReviewChoice::View(ReviewView::All));
        assert!(opened.wakes.load(Ordering::SeqCst) > woken);
        let all = page.hand_over(false).expect("the switch moved the page");
        assert_eq!(view(&all), Some(ReviewView::All));
        page.choose(ReviewChoice::ToggleFile("src/lib.rs".into()));
        let folded = page.hand_over(false).expect("the fold moved the page");
        assert!(matches!(
            &folded.rows[0].of,
            ReviewRowOf::File(head) if head.folded
        ));
        assert!(folded.to_fetch.is_empty(), "a folded file needs no body");
    }

    /// A sent review names exactly the drafts its text holds, never a
    /// comment sent before.
    #[test]
    fn a_sent_review_names_exactly_the_drafts() {
        let comment = |id: &[u8], round: Option<u32>| ReviewerComment {
            id: id.to_vec(),
            text: format!("comment {}", String::from_utf8_lossy(id)),
            round,
            anchor: Some(wire::experimental::ReviewAnchor {
                commit: SNAPSHOT.into(),
                path: "src/lib.rs".into(),
                lines: Some(ReviewLineRange { start: 41, end: 41 }),
            }),
            ..ReviewerComment::default()
        };
        let mut summary = reviewed().host().summary.clone();
        summary.comments = vec![comment(b"sent", Some(1)), comment(b"one", None)];
        summary.rounds.push(ReviewRound::default());
        assert_eq!(
            sent_review(&ReviewSummary {
                comments: vec![comment(b"sent", Some(1))],
                ..summary.clone()
            }),
            None,
            "no drafts, nothing to send"
        );
        summary.comments.push(comment(b"two", None));
        let attachment = sent_review(&summary).expect("drafts to send");
        let entry = SENT_REVIEW.get(&attachment.experimental).unwrap();
        assert_eq!(
            entry
                .comments
                .iter()
                .map(|comment| comment.id.clone())
                .collect::<Vec<_>>(),
            vec![b"one".to_vec(), b"two".to_vec()]
        );
        assert_eq!(entry.round, 2);
        let Some(wire::attachment::Of::Text(text)) = attachment.of else {
            panic!("a text attachment");
        };
        assert!(text.text.contains("comment one") && text.text.contains("comment two"));
        assert!(!text.text.contains("comment sent"));
    }

    /// A sent review opens its round from the agent's host: its whole diff,
    /// listed and fetched from the round's base to its snapshot, with the
    /// comments it sent that are still on the review on the lines they were
    /// written on; with the host out of reach, from what was loaded, saying
    /// so, or with nothing loaded only that; and nothing once the review
    /// holds none of it.
    #[tokio::test]
    async fn a_sent_review_opens_its_round() {
        let fake = reviewed();
        let opened = page(&fake);
        let review = opened.page.review().clone();
        {
            let mut host = fake.host();
            host.summary.rounds.push(ReviewRound {
                snapshot: SNAPSHOT.into(),
                base_commit: BASE.into(),
                input_id: b"the prompt".to_vec(),
            });
            host.summary.comments.push(ReviewerComment {
                id: b"sent".to_vec(),
                anchor: Some(wire::experimental::ReviewAnchor {
                    commit: SNAPSHOT.into(),
                    path: "src/lib.rs".into(),
                    lines: Some(ReviewLineRange { start: 41, end: 41 }),
                }),
                text: "why two".into(),
                snippet: vec![line(ReviewLineKind::Added, 0, 41, "    let a = 2;")],
                round: Some(1),
                placement: Some(ReviewPlacement {
                    at: Some(review_placement::At::Outdated(wire::Empty {})),
                    changed_near: false,
                    path: "src/lib.rs".into(),
                }),
                revision: 1,
                sent_revision: Some(1),
                ..ReviewerComment::default()
            });
        }
        let sent = b"the prompt";

        // With the host out of reach before anything was loaded, the page
        // says only that.
        fake.host().failing = Some(RpcError::Refused(wire::Error::because(
            wire::ErrorReason::HostUnreachable,
            "the desk is away",
        )));
        let unloaded = round_page(&review, b"review-1", sent)
            .await
            .expect("a page saying the host is away");
        assert_eq!(unloaded.banners, vec![ReviewBanner::Unreachable]);
        assert!(unloaded.header.is_none());
        fake.host().failing = None;

        let round = round_page(&review, b"review-1", sent)
            .await
            .expect("the host holds the round");
        assert!(round.banners.is_empty(), "{:?}", round.banners);
        let header = round.header.as_ref().expect("a header");
        assert_eq!(header.snapshot, SNAPSHOT);
        assert_eq!(header.compared_with, BASE);
        assert_eq!((header.sent, header.drafts, header.rounds), (1, 0, 0));
        let gets = fake.host().gets.clone();
        assert!(gets[0].list && gets[0].files.is_empty(), "{gets:?}");
        assert_eq!(
            (gets[0].from.as_str(), gets[0].snapshot.as_str()),
            (BASE, SNAPSHOT)
        );
        assert_eq!(gets[1].files[0].path, "src/lib.rs");
        assert!(
            round.rows.iter().any(|row| matches!(
                &row.of,
                ReviewRowOf::Line(line) if line.text == "    let a = 2;"
            )),
            "its whole diff: {round:#?}"
        );
        let comment = round
            .rows
            .iter()
            .find_map(|row| match &row.of {
                ReviewRowOf::Comment(comment) => Some(comment),
                _ => None,
            })
            .expect("the comment it sent");
        assert_eq!(
            comment.at,
            ui_view::ReviewCommentAt::Lines { start: 41, end: 41 }
        );
        assert!(comment.snippet.is_empty(), "drawn under its line");
        assert!(round.to_fetch.is_empty(), "its files were fetched");

        fake.host().failing = Some(RpcError::Refused(wire::Error::because(
            wire::ErrorReason::HostUnreachable,
            "the desk is away",
        )));
        let away = round_page(&review, b"review-1", sent)
            .await
            .expect("what was loaded still reads");
        assert_eq!(away.banners, vec![ReviewBanner::Unreachable]);
        assert!(away.to_fetch.is_empty(), "its files were kept");

        fake.host().failing = None;
        assert_eq!(
            round_page(&review, b"review-2", sent).await,
            None,
            "the review was started over"
        );
        fake.host()
            .summary
            .comments
            .retain(|comment| comment.id != b"sent");
        assert_eq!(
            round_page(&review, b"review-1", sent).await,
            None,
            "every comment it sent is deleted"
        );
    }
}
