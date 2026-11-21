//! One agent's review, as a client's review page reads and edits it: the
//! review experiment's calls to the agent's host, what they fetched, and
//! the agent's row followed so the page changes when the review does.
//!
//! Every read names commits, and a commit's content never changes, so what
//! was fetched is kept, keyed by the request that read it ([`Comparison`]
//! and path), for as long as the process runs ([`ReviewCache`]), and by the
//! page that fetched it for as long as the page shows it. A review already
//! loaded reads again with its host asleep; only edits need the host.
//!
//! The page never changes under its reader: a summary re-read because the
//! review changed is taken only while it describes the snapshot the page
//! shows. One that has moved on to a newer snapshot leaves the page where
//! it is, marked stale, until the person refreshes or opens it again.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use client::RpcError;
use prost::Message as _;
use tokio::sync::watch;
use wire::ErrorCode;
use wire::experimental::{
    REVIEW_ROW, ReviewAnchor, ReviewComparedLines, ReviewEdit, ReviewFileDiff, ReviewFileRequest,
    ReviewGetFilesRequest, ReviewLine, ReviewLineRange, ReviewPlacement, ReviewRound,
    ReviewSummary, ReviewView, ReviewerComment, review_placement,
};

use crate::Session;

/// How many files one GetFiles asks for: few enough that the first files
/// of a large change reach the screen before the host has highlighted the
/// rest, over a relay too.
pub const BATCH_FILES: usize = 16;

/// The most a process keeps of what reviews fetched, in encoded bytes: a
/// few large changes' worth of highlighted files. Reasoned, not measured.
pub const CACHE_BYTES: usize = 32 << 20;

/// Why the review page has nothing of the host's to show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unavailable {
    /// The agent's host does not run reviews: the experiment is off there,
    /// or its build predates them.
    NotOffered,
    /// The agent's folder is not a repository, so it has no review.
    NotARepository,
}

impl std::fmt::Display for Unavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Unavailable::NotOffered => "reviews are not offered on this agent's host",
            Unavailable::NotARepository => "the agent's folder is not a repository",
        })
    }
}

/// Why a review call did not give its answer.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum ReviewError {
    #[error("{0}")]
    Unavailable(Unavailable),
    /// The call failed: [`RpcError::not_sent`] says when the host could not
    /// be reached, and [`RpcError::uncertain`] when an edit may have landed.
    #[error(transparent)]
    Rpc(RpcError),
}

impl ReviewError {
    /// The error of a call to the review service: a host that does not
    /// serve it answers unimplemented.
    fn of(error: RpcError) -> ReviewError {
        match error.code() {
            Some(ErrorCode::Unimplemented) => ReviewError::Unavailable(Unavailable::NotOffered),
            _ => ReviewError::Rpc(error),
        }
    }

    /// The error of an Open, which answers a failed precondition of its own
    /// for a folder that is not a repository. A call that never reached the
    /// host says so with a reason, and is not that.
    fn of_open(error: RpcError) -> ReviewError {
        if error.code() == Some(ErrorCode::FailedPrecondition) && error.not_sent().is_none() {
            return ReviewError::Unavailable(Unavailable::NotARepository);
        }
        ReviewError::of(error)
    }
}

/// What following the agent's row did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Followed {
    /// Nothing the page shows changed.
    Same,
    /// The review changed, and its summary was read again.
    Reread,
    /// The page's snapshot is behind the working tree or the review, and
    /// says so; nothing on it changed.
    Stale,
}

/// What of an agent's row a review page follows: the review its entry
/// describes, the entry's change counter, and whether the working tree
/// moved since the latest snapshot. A client hands [`Review::follow`] a row
/// whose mark differs from the last one it handed, and no other: two rows
/// with the same mark have nothing new for the page.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RowMark {
    pub review_id: Vec<u8>,
    /// Counts afresh when a review starts over, so it means something only
    /// beside the review's id.
    pub counter: u64,
    pub moved: bool,
}

impl RowMark {
    /// The mark of `row`; none for a row without the review's entry.
    pub fn of(row: &wire::Agent) -> Option<RowMark> {
        let entry = REVIEW_ROW.get(&row.experimental)?;
        Some(RowMark {
            review_id: entry.review_id,
            counter: entry.counter,
            moved: entry.moved,
        })
    }

    /// The review as of this mark: what changes when the review does, as
    /// against the working tree moving under it.
    fn version(&self) -> (Vec<u8>, u64) {
        (self.review_id.clone(), self.counter)
    }
}

/// One comparison a review handed out, as GetFiles names it: the files of
/// `snapshot`, compared with `from` as `view` shows them; for what changed
/// since a round, with the base commit each of the two was shown against,
/// which decide what of the change is the agent's. It names everything an
/// answer depends on, so the same comparison always answers the same:
/// every body read is of one comparison, and kept by it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Comparison {
    pub from: String,
    pub snapshot: String,
    pub view: ReviewView,
    /// For since-last, the base `snapshot` was shown against; else empty.
    pub base: String,
    /// For since-last, the base the round's snapshot `from` was shown
    /// against; else empty.
    pub from_base: String,
}

impl Comparison {
    /// What `summary` shows for `view`: all changes, from the base commit
    /// recorded with its snapshot; or what changed since the last round,
    /// from that round's snapshot. None for the second before any round.
    pub fn shown(summary: &ReviewSummary, view: ReviewView) -> Option<Comparison> {
        Some(match view {
            ReviewView::SinceLast => {
                let last = summary.rounds.last()?;
                Comparison {
                    from: last.snapshot.clone(),
                    snapshot: summary.snapshot.clone(),
                    view,
                    base: summary.base_commit.clone(),
                    from_base: last.base_commit.clone(),
                }
            }
            _ => Comparison {
                from: summary.base_commit.clone(),
                snapshot: summary.snapshot.clone(),
                view,
                base: String::new(),
                from_base: String::new(),
            },
        })
    }

    /// A past round's whole diff: its snapshot against the base commit it
    /// was recorded with.
    pub fn round(round: &ReviewRound) -> Comparison {
        Comparison {
            from: round.base_commit.clone(),
            snapshot: round.snapshot.clone(),
            view: ReviewView::All,
            base: String::new(),
            from_base: String::new(),
        }
    }

    /// The comparison a composed page shows, as its header names it.
    pub fn of_page(header: &ui_view::ReviewHeader) -> Comparison {
        let since = header.view == ui_view::ReviewView::SinceLast;
        let only_since = |commit: &str| match since {
            true => commit.to_owned(),
            false => String::new(),
        };
        Comparison {
            from: header.compared_with.clone(),
            snapshot: header.snapshot.clone(),
            view: match since {
                false => ReviewView::All,
                true => ReviewView::SinceLast,
            },
            base: only_since(&header.base_commit),
            from_base: only_since(&header.compared_base),
        }
    }

    /// The GetFiles request reading `files` of this comparison; with
    /// `list`, also every file it changed.
    fn request(&self, files: Vec<ReviewFileRequest>, list: bool) -> ReviewGetFilesRequest {
        ReviewGetFilesRequest {
            snapshot: self.snapshot.clone(),
            view: self.view as i32,
            files,
            from: self.from.clone(),
            list,
            base: self.base.clone(),
            from_base: self.from_base.clone(),
            ..ReviewGetFilesRequest::default()
        }
    }
}

/// A round a sent review closed, read for its page: [`Review::round`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Round {
    /// The round as a review of its own: its snapshot against the base
    /// commit it was recorded with, the files that comparison changed, and
    /// the comments it sent, each on its lines of that snapshot. Composed
    /// with no view chosen, it shows the round's whole diff.
    pub summary: ReviewSummary,
    /// The bodies fetched of its files.
    pub files: Vec<ReviewFileDiff>,
    /// What reading it taught the page: the host out of reach, so this is
    /// what the process had loaded; or the round's snapshot gone from the
    /// host, so its comments show with the code stored with them.
    pub notices: Vec<ui_view::ReviewNotice>,
    /// What its page is composed with: those notices, and every file open.
    /// A past round is read to see what was sent, so a file the review
    /// folds by default, generated, vendored or a lockfile, opens with its
    /// body and the comments on it like any other.
    pub state: ui_view::ReviewPageState,
}

/// One agent's review. Made from its chat's [`Session`]; every one made in
/// a process shares that process's [`ReviewCache`], and keeps besides what
/// its own page fetched for as long as the page shows it.
pub struct Review {
    reviews: client::AgentReviews,
    agent_id: Vec<u8>,
    cache: ReviewCache,
    state: Mutex<State>,
    /// Open, edit and re-read one at a time, so the summary the page shows
    /// is the answer of the call made last.
    calls: tokio::sync::Mutex<()>,
    changed: watch::Sender<()>,
}

#[derive(Default)]
struct State {
    /// The summary the page shows.
    shown: Option<Arc<ReviewSummary>>,
    unavailable: Option<Unavailable>,
    /// The page asked for the review: from then on a change to it is read.
    opened: bool,
    /// The review and its counter as last followed: `None` until a row is
    /// followed, `Some(None)` for a row without the entry.
    seen: Option<Option<(Vec<u8>, u64)>>,
    /// What this page fetched, held apart from the process's cache so a
    /// page larger than that cache never loses what it shows and asks for
    /// it again. Kept while the shown summary names its commits.
    files: HashMap<FileKey, Arc<ReviewFileDiff>>,
    context: HashMap<ContextKey, Arc<[ReviewLine]>>,
    lists: HashMap<Comparison, Arc<[wire::ChangedFile]>>,
}

impl State {
    /// Drops what the page fetched of commits the shown summary no longer
    /// names: its snapshot, its base commit, and its rounds'.
    fn prune(&mut self) {
        let Some(shown) = &self.shown else {
            return;
        };
        let named: HashSet<&str> = [shown.snapshot.as_str(), shown.base_commit.as_str()]
            .into_iter()
            .chain(
                shown
                    .rounds
                    .iter()
                    .flat_map(|round| [round.snapshot.as_str(), round.base_commit.as_str()]),
            )
            .collect();
        self.files
            .retain(|key, _| named.contains(key.comparison.snapshot.as_str()));
        self.lists
            .retain(|comparison, _| named.contains(comparison.snapshot.as_str()));
        self.context
            .retain(|key, _| named.contains(key.commit.as_str()));
    }
}

impl Session {
    /// The agent's review, for its review page.
    pub fn reviews(&self) -> Review {
        Review::new(self.host_services().clone())
    }
}

impl Review {
    /// The review of `agent`, keeping what it fetches in the process's
    /// cache.
    pub fn new(agent: client::Agent) -> Review {
        Review::with_cache(agent, ReviewCache::process().clone())
    }

    /// The review of `agent`, keeping what it fetches in `cache`.
    pub fn with_cache(agent: client::Agent, cache: ReviewCache) -> Review {
        let (changed, _) = watch::channel(());
        let agent_id = agent.id().to_vec();
        let shown = cache.summary(&agent_id);
        Review {
            reviews: agent.reviews(),
            agent_id,
            cache,
            state: Mutex::new(State {
                shown,
                ..State::default()
            }),
            calls: tokio::sync::Mutex::new(()),
            changed,
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    /// Marked whenever what the page draws from changes: the summary, a
    /// batch of files, more context, or the review becoming unavailable.
    pub fn changed(&self) -> watch::Receiver<()> {
        self.changed.subscribe()
    }

    fn notify(&self) {
        self.changed.send_replace(());
    }

    /// The summary the page shows: the last one this process loaded for the
    /// agent, so a page opened with the host away still has one.
    pub fn summary(&self) -> Option<Arc<ReviewSummary>> {
        self.state().shown.clone()
    }

    /// Why the review has nothing to show, when the host said so last.
    pub fn unavailable(&self) -> Option<Unavailable> {
        self.state().unavailable
    }

    /// Opens the review as a person does, at the agent's host, which begins
    /// it on a first look and snapshots a moved tree when no drafts wait;
    /// `base_branch` picks its base anew and `refresh` takes a snapshot even
    /// with drafts, moving them onto it. The answer is what the page shows
    /// from now. Only a person's open or refresh does this: following the
    /// row and sending read the review without changing it ([`Review::read`]).
    ///
    /// On failure the page keeps what it showed, which for a review this
    /// process loaded before is its last summary.
    pub async fn open(
        &self,
        base_branch: Option<String>,
        refresh: bool,
    ) -> Result<Arc<ReviewSummary>, ReviewError> {
        let _call = self.calls.lock().await;
        self.state().opened = true;
        let answer = self.reviews.open(base_branch, refresh).await;
        let summary = self.answered(answer.map_err(ReviewError::of_open))?;
        Ok(self.show(summary, Taking::Always))
    }

    /// Reads the review as its host holds it, changing nothing there, and
    /// answers that: what a send takes its drafts from. The page shows it
    /// only while it is of the page's snapshot. An agent with no review
    /// answers a summary whose id is empty.
    pub async fn read(&self) -> Result<Arc<ReviewSummary>, ReviewError> {
        let _call = self.calls.lock().await;
        let answer = self.reviews.read().await;
        let summary = Arc::new(self.answered(answer.map_err(ReviewError::of_open))?);
        self.show(ReviewSummary::clone(&summary), Taking::SameSnapshot);
        Ok(summary)
    }

    /// Makes `edits`, all of them or none, at the agent's host, which must
    /// be reachable. The comments the answer holds replace the page's, even
    /// on a page held on an earlier snapshot, where each sits by the lines
    /// it was written on; the rest of the answer is shown when it is of the
    /// page's snapshot.
    pub async fn edit(&self, edits: Vec<ReviewEdit>) -> Result<Arc<ReviewSummary>, ReviewError> {
        let _call = self.calls.lock().await;
        let answer = self.reviews.edit(edits).await;
        let summary = self.answered(answer.map_err(ReviewError::of))?;
        Ok(self.show(summary, Taking::Edit))
    }

    /// Follows the agent's row, as every client receives it: once the page
    /// has opened the review, a change of the review its entry names, or of
    /// the entry's counter, reads the summary again, without changing the
    /// review; a working tree that only moved marks the page stale without
    /// a call. A client hands over rows whose [`RowMark`] moved.
    ///
    /// A row followed before the review is opened sets where following
    /// starts, so a page that follows the row it opened from re-reads
    /// nothing its open already read. The first row followed after an open
    /// with no row before it reads again, as it cannot tell whether the
    /// open saw that row's change.
    pub async fn follow(&self, row: &wire::Agent) -> Result<Followed, ReviewError> {
        let mark = RowMark::of(row);
        let version = mark.as_ref().map(RowMark::version);
        let moved = mark.is_some_and(|mark| mark.moved);
        let _call = self.calls.lock().await;
        let before = {
            let mut state = self.state();
            let before = state.seen.replace(version.clone());
            if !state.opened || before.as_ref() == Some(&version) {
                let Some(shown) = state.shown.as_ref().filter(|shown| moved && !shown.stale) else {
                    return Ok(Followed::Same);
                };
                let mut stale = ReviewSummary::clone(shown);
                stale.stale = true;
                state.shown = Some(Arc::new(stale));
                drop(state);
                self.notify();
                return Ok(Followed::Stale);
            }
            before
        };
        let answer = self.reviews.read().await;
        let summary = match self.answered(answer.map_err(ReviewError::of_open)) {
            Ok(summary) => summary,
            Err(error) => {
                // Not read: the next row asks again.
                self.state().seen = before;
                return Err(error);
            }
        };
        let shown = self.show(summary.clone(), Taking::SameSnapshot);
        Ok(if shown.snapshot == summary.snapshot {
            Followed::Reread
        } else {
            Followed::Stale
        })
    }

    /// The bodies of `paths` in `comparison`, in the order asked: those
    /// this page or the process already fetched, the rest from the host
    /// [`BATCH_FILES`] at a time, each batch kept and announced as it
    /// lands. A file the host left out of a full answer is asked for again
    /// in the next batch.
    pub async fn files(
        &self,
        comparison: &Comparison,
        paths: &[String],
    ) -> Result<Vec<Arc<ReviewFileDiff>>, ReviewError> {
        let mut got: HashMap<&str, Arc<ReviewFileDiff>> = HashMap::new();
        let mut wanted = VecDeque::new();
        let mut asked = HashSet::new();
        for path in paths {
            if !asked.insert(path.as_str()) {
                continue;
            }
            match self.file(&FileKey::new(comparison, path)) {
                Some(file) => {
                    got.insert(path.as_str(), file);
                }
                None => wanted.push_back(path.as_str()),
            }
        }
        let mut notified = false;
        while !wanted.is_empty() {
            let batch: Vec<&str> = wanted.drain(..wanted.len().min(BATCH_FILES)).collect();
            let requests = batch
                .iter()
                .map(|path| ReviewFileRequest {
                    path: (*path).to_owned(),
                    ..ReviewFileRequest::default()
                })
                .collect();
            let answer = self
                .reviews
                .get_files(comparison.request(requests, false))
                .await;
            let answer = self.answered(answer.map_err(ReviewError::of))?;
            let mut landed = 0;
            for file in answer.files {
                let Some(path) = batch.iter().copied().find(|path| *path == file.path) else {
                    continue;
                };
                if got.contains_key(path) {
                    continue;
                }
                let file = Arc::new(file);
                self.keep_file(FileKey::new(comparison, path), file.clone());
                got.insert(path, file);
                landed += 1;
            }
            if landed == 0 {
                return Err(ReviewError::Rpc(RpcError::Refused(wire::Error::new(
                    ErrorCode::ResourceExhausted,
                    "the agent's host answered none of the files asked for",
                ))));
            }
            for path in batch.into_iter().rev() {
                if !got.contains_key(path) {
                    wanted.push_front(path);
                }
            }
            self.notify();
            notified = true;
        }
        if !notified && !got.is_empty() {
            // Read from the process's cache into the page's own: the page
            // draws them from there.
            self.notify();
        }
        let mut answered = HashSet::new();
        Ok(paths
            .iter()
            .filter(|path| answered.insert(path.as_str()))
            .filter_map(|path| got.get(path.as_str()).cloned())
            .collect())
    }

    /// The files of `comparison` this page fetched, by path: what the page
    /// draws while the rest arrive.
    pub fn fetched(&self, comparison: &Comparison) -> BTreeMap<String, Arc<ReviewFileDiff>> {
        self.state()
            .files
            .iter()
            .filter(|(key, _)| key.comparison == *comparison)
            .map(|(key, file)| (key.path.clone(), file.clone()))
            .collect()
    }

    /// Every file `comparison` changed, as the host lists them; with the
    /// host out of reach, the list this process read before.
    pub async fn list(
        &self,
        comparison: &Comparison,
    ) -> Result<Arc<[wire::ChangedFile]>, ReviewError> {
        if let Some(held) = self.state().lists.get(comparison) {
            return Ok(held.clone());
        }
        let answer = self
            .reviews
            .get_files(comparison.request(Vec::new(), true))
            .await
            .map_err(ReviewError::of);
        let listed: Arc<[wire::ChangedFile]> = match self.answered(answer) {
            Ok(answer) => answer.changed.into(),
            Err(error) => {
                let held = unreachable(&error)
                    .then(|| self.cache.list(comparison))
                    .flatten();
                return held.ok_or(error);
            }
        };
        self.cache.put_list(comparison.clone(), listed.clone());
        self.state()
            .lists
            .insert(comparison.clone(), listed.clone());
        Ok(listed)
    }

    /// More lines of `path` around a change in `comparison`, read from
    /// `commit` (its snapshot, or for lines on a hunk's removed side the
    /// commit it compares against), each numbered as a line of that commit.
    pub async fn context(
        &self,
        comparison: &Comparison,
        commit: &str,
        path: &str,
        lines: ReviewLineRange,
    ) -> Result<Arc<[ReviewLine]>, ReviewError> {
        let key = ContextKey::new(commit, path, lines);
        if let Some(held) = self.context_held(commit, path, lines) {
            return Ok(held);
        }
        let request = ReviewFileRequest {
            path: path.to_owned(),
            context: vec![lines],
            context_commit: commit.to_owned(),
        };
        let answer = self
            .reviews
            .get_files(comparison.request(vec![request], false))
            .await;
        let answer = self.answered(answer.map_err(ReviewError::of))?;
        let Some(file) = answer.files.into_iter().find(|file| file.path == path) else {
            return Err(ReviewError::Rpc(RpcError::Refused(wire::Error::new(
                ErrorCode::ResourceExhausted,
                "the agent's host answered without the file asked for",
            ))));
        };
        let held: Arc<[ReviewLine]> = file.context.into();
        self.cache.put_context(key.clone(), held.clone());
        self.state().context.insert(key, held.clone());
        self.notify();
        Ok(held)
    }

    /// Lines [`Review::context`] already fetched, for drawing.
    pub fn context_held(
        &self,
        commit: &str,
        path: &str,
        lines: ReviewLineRange,
    ) -> Option<Arc<[ReviewLine]>> {
        let key = ContextKey::new(commit, path, lines);
        let mut state = self.state();
        if let Some(held) = state.context.get(&key) {
            return Some(held.clone());
        }
        let held = self.cache.context(&key)?;
        state.context.insert(key, held.clone());
        Some(held)
    }

    /// The round a sent review closed, read for its page: the review as its
    /// host holds it now, without changing it, and of it the round the
    /// input `input_id` closed when the agent took it, the message holding
    /// the sent review; its whole diff is listed and the body of every file
    /// it shows fetched. Two sends queued at once may carry the same
    /// comments, so the round is found by the input that closed it, never
    /// by the comments it names.
    ///
    /// With the host out of reach, the round is read from what this process
    /// loaded, and the error is the host's when that holds none of it. When
    /// the host no longer keeps the round's snapshot, its comments show with
    /// the code stored with them. None when the message closed no round (its
    /// comments were edited before the agent took it, and a later message
    /// sent them), or the review holds none of them, as after starting over
    /// or deleting them, and the chat shows the text the agent read instead.
    pub async fn round(
        &self,
        review_id: &[u8],
        input_id: &[u8],
    ) -> Result<Option<Round>, ReviewError> {
        let mut notices = Vec::new();
        let (summary, (number, mut round)) = match self.read().await {
            Ok(summary) => match round_of(&summary, review_id, input_id) {
                Some(round) => (summary, round),
                None => return Ok(None),
            },
            Err(error) if unreachable(&error) => {
                notices.push(ui_view::ReviewNotice::Unreachable);
                let held = self.summary().and_then(|summary| {
                    let round = round_of(&summary, review_id, input_id)?;
                    Some((summary, round))
                });
                match held {
                    Some(held) => held,
                    None => return Err(error),
                }
            }
            Err(error) => return Err(error),
        };
        let held = summary
            .rounds
            .get(number as usize - 1)
            .cloned()
            .unwrap_or_default();
        let comparison = Comparison::round(&held);
        match self.list(&comparison).await {
            Ok(listed) => round.files = listed.to_vec(),
            Err(error) if unreachable(&error) => {
                if !notices.contains(&ui_view::ReviewNotice::Unreachable) {
                    notices.push(ui_view::ReviewNotice::Unreachable);
                }
            }
            Err(_) => notices.push(ui_view::ReviewNotice::RoundGone),
        }
        let state = ui_view::ReviewPageState {
            notices: notices.clone(),
            toggled_files: round
                .files
                .iter()
                .filter(|file| file.flags().next().is_some())
                .map(|file| file.path.clone())
                .collect(),
            ..ui_view::ReviewPageState::default()
        };
        let wanted = ui_view::review_page(Some(&round), &[], &[], &state).to_fetch;
        let files = match self.files(&comparison, &wanted).await {
            Ok(files) => files,
            // What was kept before the failure still draws; the rest draw
            // as loading, never asked for again on a round.
            Err(_) => self.fetched(&comparison).into_values().collect(),
        };
        Ok(Some(Round {
            summary: round,
            files: files
                .iter()
                .map(|file| ReviewFileDiff::clone(file))
                .collect(),
            notices,
            state,
        }))
    }

    fn file(&self, key: &FileKey) -> Option<Arc<ReviewFileDiff>> {
        let mut state = self.state();
        if let Some(held) = state.files.get(key) {
            return Some(held.clone());
        }
        let held = self.cache.file(key)?;
        state.files.insert(key.clone(), held.clone());
        Some(held)
    }

    fn keep_file(&self, key: FileKey, file: Arc<ReviewFileDiff>) {
        self.cache.put_file(key.clone(), file.clone());
        self.state().files.insert(key, file);
    }

    /// The answer of a call, noting when it says the review is not there to
    /// be had, and that it is once it is.
    fn answered<T>(&self, answer: Result<T, ReviewError>) -> Result<T, ReviewError> {
        let unavailable = match &answer {
            Ok(_) => None,
            Err(ReviewError::Unavailable(why)) => Some(*why),
            Err(_) => return answer,
        };
        let changed = {
            let mut state = self.state();
            std::mem::replace(&mut state.unavailable, unavailable) != unavailable
        };
        if changed {
            self.notify();
        }
        answer
    }

    /// Shows `summary` as `taking` allows, and answers what the page shows
    /// after.
    fn show(&self, summary: ReviewSummary, taking: Taking) -> Arc<ReviewSummary> {
        let shown = {
            let mut state = self.state();
            let held = state
                .shown
                .clone()
                .filter(|shown| shown.id == summary.id && shown.snapshot != summary.snapshot);
            let next = match (held, taking) {
                (Some(held), Taking::SameSnapshot | Taking::Edit) => {
                    let mut kept = ReviewSummary::clone(&held);
                    kept.stale = true;
                    if matches!(taking, Taking::Edit) {
                        kept.comments = seated_on(&held, summary.comments);
                    }
                    if kept == *held {
                        return held;
                    }
                    Arc::new(kept)
                }
                _ => {
                    if state.shown.as_deref() == Some(&summary) {
                        return state.shown.clone().expect("just compared");
                    }
                    Arc::new(summary)
                }
            };
            state.shown = Some(next.clone());
            state.prune();
            self.cache.put_summary(&self.agent_id, next.clone());
            next
        };
        self.notify();
        shown
    }
}

/// `comments`, as the agent's host placed them on a later snapshot, seated
/// instead on the snapshot of `held`, the summary a page keeps showing: a
/// comment the page already held keeps where it sat, and any other sits on
/// the lines it was written on, of the snapshot or of a commit compared
/// against, or on its file.
fn seated_on(held: &ReviewSummary, comments: Vec<ReviewerComment>) -> Vec<ReviewerComment> {
    comments
        .into_iter()
        .map(|mut comment| {
            let before = held
                .comments
                .iter()
                .find(|seen| seen.id == comment.id && seen.anchor == comment.anchor);
            comment.placement = match before {
                Some(before) => before.placement.clone(),
                None => comment
                    .anchor
                    .as_ref()
                    .map(|anchor| written_on(anchor, &held.snapshot)),
            };
            comment
        })
        .collect()
}

/// A comment on the lines `anchor` names, as it sits on `snapshot`: on the
/// snapshot's lines when they are its, on the lines of the commit compared
/// against when they are that commit's, and on its file when it names no
/// lines.
fn written_on(anchor: &ReviewAnchor, snapshot: &str) -> ReviewPlacement {
    let at = match anchor.lines.filter(|lines| lines.start > 0) {
        None => review_placement::At::File(wire::Empty {}),
        Some(lines) if anchor.commit == snapshot => review_placement::At::Lines(lines),
        Some(lines) => review_placement::At::Compared(ReviewComparedLines {
            commit: anchor.commit.clone(),
            lines: Some(lines),
        }),
    };
    ReviewPlacement {
        at: Some(at),
        changed_near: false,
        path: anchor.path.clone(),
    }
}

/// The round of `summary`, the review as its host holds it now, that the
/// input `input_id` closed when the agent took it, sending comments of the
/// review `review_id`, as a review of its own, and its number. It is the
/// round's snapshot compared with the base commit it was recorded with, and
/// the comments sent in it still on the review, each on the lines it was
/// written on. The round's files are for the caller to list
/// ([`Review::list`]). None when `summary` is of another review, as after
/// starting over; when the input closed no round of it, its comments all
/// sent or edited before the agent took it; or when the review no longer
/// holds any comment of the round.
pub fn round_of(
    summary: &ReviewSummary,
    review_id: &[u8],
    input_id: &[u8],
) -> Option<(u32, ReviewSummary)> {
    if summary.id != review_id || review_id.is_empty() || input_id.is_empty() {
        return None;
    }
    let at = summary
        .rounds
        .iter()
        .position(|round| round.input_id == input_id)?;
    let round = &summary.rounds[at];
    let number = u32::try_from(at + 1).ok()?;
    let comments: Vec<ReviewerComment> = summary
        .comments
        .iter()
        .filter(|comment| comment.round == Some(number))
        .map(|comment| ReviewerComment {
            // Where it sits on a later snapshot is not where it was sent.
            placement: comment
                .anchor
                .as_ref()
                .map(|anchor| written_on(anchor, &round.snapshot)),
            ..comment.clone()
        })
        .collect();
    if comments.is_empty() {
        return None;
    }
    Some((
        number,
        ReviewSummary {
            id: summary.id.clone(),
            base_branch: summary.base_branch.clone(),
            base_commit: round.base_commit.clone(),
            snapshot: round.snapshot.clone(),
            comments,
            objects: summary.objects.clone(),
            root: summary.root.clone(),
            ..ReviewSummary::default()
        },
    ))
}

/// Whether a review call failed for not reaching the agent's host.
pub fn unreachable(error: &ReviewError) -> bool {
    matches!(error, ReviewError::Rpc(rpc) if rpc.not_sent().is_some() || rpc.uncertain() || matches!(rpc, RpcError::Transport(_)))
}

/// Which answers replace the summary the page shows.
#[derive(Clone, Copy)]
enum Taking {
    /// The person asked for it: any answer.
    Always,
    /// It came of something else: only an answer about the same snapshot of
    /// the same review, or of a review begun anew.
    SameSnapshot,
    /// An edit's: as [`Taking::SameSnapshot`], but its comments are taken
    /// whatever snapshot it is about, so what the person just wrote shows.
    Edit,
}

/// What reviews fetched in this process, kept by the commits it was read
/// from, the least recently used dropped past its size. Clones share it.
#[derive(Clone)]
pub struct ReviewCache {
    held: Arc<Mutex<Lru>>,
}

impl ReviewCache {
    /// A cache keeping at most `bytes` of encoded answers.
    pub fn new(bytes: usize) -> ReviewCache {
        ReviewCache {
            held: Arc::new(Mutex::new(Lru {
                cap: bytes,
                ..Lru::default()
            })),
        }
    }

    /// The one cache every review of this process shares, of
    /// [`CACHE_BYTES`].
    pub fn process() -> &'static ReviewCache {
        static PROCESS: OnceLock<ReviewCache> = OnceLock::new();
        PROCESS.get_or_init(|| ReviewCache::new(CACHE_BYTES))
    }

    /// The encoded bytes held now.
    pub fn bytes(&self) -> usize {
        self.held().bytes
    }

    fn held(&self) -> MutexGuard<'_, Lru> {
        self.held
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    fn file(&self, key: &FileKey) -> Option<Arc<ReviewFileDiff>> {
        match self.held().get(&Key::File(key.clone()))? {
            Held::File(file) => Some(file),
            _ => None,
        }
    }

    fn put_file(&self, key: FileKey, file: Arc<ReviewFileDiff>) {
        let size = key.size() + file.encoded_len();
        self.held().put(Key::File(key), Held::File(file), size);
    }

    fn context(&self, key: &ContextKey) -> Option<Arc<[ReviewLine]>> {
        match self.held().get(&Key::Context(key.clone()))? {
            Held::Context(lines) => Some(lines),
            _ => None,
        }
    }

    fn put_context(&self, key: ContextKey, lines: Arc<[ReviewLine]>) {
        let size = key.size() + lines.iter().map(|line| line.encoded_len()).sum::<usize>();
        self.held()
            .put(Key::Context(key), Held::Context(lines), size);
    }

    fn list(&self, comparison: &Comparison) -> Option<Arc<[wire::ChangedFile]>> {
        match self.held().get(&Key::List(comparison.clone()))? {
            Held::List(files) => Some(files),
            _ => None,
        }
    }

    fn put_list(&self, comparison: Comparison, files: Arc<[wire::ChangedFile]>) {
        let size = comparison_size(&comparison)
            + files.iter().map(|file| file.encoded_len()).sum::<usize>();
        self.held()
            .put(Key::List(comparison), Held::List(files), size);
    }

    fn summary(&self, agent_id: &[u8]) -> Option<Arc<ReviewSummary>> {
        match self.held().get(&Key::Summary(agent_id.to_vec()))? {
            Held::Summary(summary) => Some(summary),
            _ => None,
        }
    }

    fn put_summary(&self, agent_id: &[u8], summary: Arc<ReviewSummary>) {
        let size = agent_id.len() + summary.encoded_len();
        self.held().put(
            Key::Summary(agent_id.to_vec()),
            Held::Summary(summary),
            size,
        );
    }
}

/// A file's hunks as one comparison shows them.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct FileKey {
    comparison: Comparison,
    path: String,
}

impl FileKey {
    fn new(comparison: &Comparison, path: &str) -> FileKey {
        FileKey {
            comparison: comparison.clone(),
            path: path.to_owned(),
        }
    }

    fn size(&self) -> usize {
        comparison_size(&self.comparison) + self.path.len()
    }
}

fn comparison_size(comparison: &Comparison) -> usize {
    comparison.from.len()
        + comparison.snapshot.len()
        + comparison.base.len()
        + comparison.from_base.len()
        + 4
}

/// Lines of one file of one commit.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct ContextKey {
    commit: String,
    path: String,
    start: u32,
    end: u32,
}

impl ContextKey {
    fn new(commit: &str, path: &str, lines: ReviewLineRange) -> ContextKey {
        ContextKey {
            commit: commit.to_owned(),
            path: path.to_owned(),
            start: lines.start,
            end: lines.end,
        }
    }

    fn size(&self) -> usize {
        self.commit.len() + self.path.len() + 8
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Key {
    File(FileKey),
    Context(ContextKey),
    /// The files a comparison changed.
    List(Comparison),
    /// The last summary an agent's review answered.
    Summary(Vec<u8>),
}

#[derive(Clone)]
enum Held {
    File(Arc<ReviewFileDiff>),
    Context(Arc<[ReviewLine]>),
    List(Arc<[wire::ChangedFile]>),
    Summary(Arc<ReviewSummary>),
}

/// Entries by key, each with its size and when it was last used.
#[derive(Default)]
struct Lru {
    cap: usize,
    bytes: usize,
    tick: u64,
    entries: HashMap<Key, (Held, usize, u64)>,
    by_use: BTreeMap<u64, Key>,
}

impl Lru {
    fn get(&mut self, key: &Key) -> Option<Held> {
        self.tick += 1;
        let tick = self.tick;
        let (held, _, used) = self.entries.get_mut(key)?;
        self.by_use.remove(used);
        *used = tick;
        self.by_use.insert(tick, key.clone());
        Some(held.clone())
    }

    /// Keeps `held` under `key`, dropping the least recently used until it
    /// fits. Something larger than the whole cache is not kept.
    fn put(&mut self, key: Key, held: Held, size: usize) {
        self.remove(&key);
        if size > self.cap {
            return;
        }
        while self.bytes + size > self.cap {
            let Some((_, oldest)) = self.by_use.pop_first() else {
                break;
            };
            if let Some((_, size, _)) = self.entries.remove(&oldest) {
                self.bytes -= size;
            }
        }
        self.tick += 1;
        self.by_use.insert(self.tick, key.clone());
        self.entries.insert(key, (held, size, self.tick));
        self.bytes += size;
    }

    fn remove(&mut self, key: &Key) {
        if let Some((_, size, used)) = self.entries.remove(key) {
            self.by_use.remove(&used);
            self.bytes -= size;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context(commit: &str) -> (ContextKey, Arc<[ReviewLine]>) {
        let key = ContextKey::new(commit, "src/lib.rs", ReviewLineRange { start: 1, end: 2 });
        let lines: Arc<[ReviewLine]> = vec![ReviewLine {
            text: "x".repeat(100),
            ..ReviewLine::default()
        }]
        .into();
        (key, lines)
    }

    #[test]
    fn the_cache_drops_the_least_recently_used_past_its_size() {
        let (a, lines) = context("a");
        let one = a.size() + lines.iter().map(|line| line.encoded_len()).sum::<usize>();
        let cache = ReviewCache::new(one * 2);
        cache.put_context(a.clone(), lines.clone());
        let (b, _) = context("b");
        cache.put_context(b.clone(), lines.clone());
        // Reading a makes b the least recently used.
        assert!(cache.context(&a).is_some());
        let (c, _) = context("c");
        cache.put_context(c.clone(), lines.clone());
        assert!(cache.context(&a).is_some());
        assert!(cache.context(&b).is_none());
        assert!(cache.context(&c).is_some());
        assert_eq!(cache.bytes(), one * 2);
    }

    #[test]
    fn an_answer_larger_than_the_cache_is_not_kept() {
        let (a, lines) = context("a");
        let cache = ReviewCache::new(10);
        cache.put_context(a.clone(), lines);
        assert!(cache.context(&a).is_none());
        assert_eq!(cache.bytes(), 0);
    }
}
