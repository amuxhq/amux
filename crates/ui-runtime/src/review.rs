//! One agent's review, as a client's review page reads and edits it: the
//! review experiment's calls to the agent's host, what they fetched, and
//! the agent's row followed so the page changes when the review does.
//!
//! Every read names commits, and a commit's content never changes, so what
//! was fetched is kept, keyed by the commits it was read from, for as long
//! as the process runs ([`ReviewCache`]). A review already loaded reads
//! again with its host asleep; only edits need the host.
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
    REVIEW_ROW, ReviewEdit, ReviewFileDiff, ReviewFileRequest, ReviewLine, ReviewLineRange,
    ReviewSummary, ReviewView,
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
    /// The review has not been opened, so there is no snapshot to read.
    #[error("the review has not been opened")]
    NotOpen,
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

/// One agent's review. Made from its chat's [`Session`]; every one made in
/// a process shares that process's [`ReviewCache`].
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
    /// The row entry's counter as last followed: `None` until a row is
    /// followed, `Some(None)` for a row without the entry.
    seen: Option<Option<u64>>,
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

    /// Opens the review at the agent's host, which begins it on a first
    /// look and snapshots a moved tree when no drafts wait; `base_branch`
    /// picks its base anew and `refresh` takes a snapshot even with drafts,
    /// moving them onto it. The answer is what the page shows from now.
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

    /// Makes `edits`, all of them or none, at the agent's host, which must
    /// be reachable. The review as it is after them is shown when it is of
    /// the page's snapshot.
    pub async fn edit(&self, edits: Vec<ReviewEdit>) -> Result<Arc<ReviewSummary>, ReviewError> {
        let _call = self.calls.lock().await;
        let answer = self.reviews.edit(edits).await;
        let summary = self.answered(answer.map_err(ReviewError::of))?;
        Ok(self.show(summary, Taking::SameSnapshot))
    }

    /// Follows the agent's row, as every client receives it: once the page
    /// has opened the review, a move of the review's change counter reads
    /// the summary again; a working tree that only moved marks the page
    /// stale without a call.
    ///
    /// A row followed before the review is opened sets where following
    /// starts, so a page that follows the row it opened from re-reads
    /// nothing its open already read. The first row followed after an open
    /// with no row before it reads again, as it cannot tell whether the
    /// open saw that row's change.
    pub async fn follow(&self, row: &wire::Agent) -> Result<Followed, ReviewError> {
        let entry = REVIEW_ROW.get(&row.experimental);
        let counter = entry.as_ref().map(|entry| entry.counter);
        let moved = entry.is_some_and(|entry| entry.moved);
        let _call = self.calls.lock().await;
        let before = {
            let mut state = self.state();
            let before = state.seen.replace(counter);
            if !state.opened || before == Some(counter) {
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
        let answer = self.reviews.open(None, false).await;
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

    /// The bodies of `paths` in the shown snapshot, as `view` compares it,
    /// in the order asked: those already fetched from the cache, the rest
    /// from the host [`BATCH_FILES`] at a time, each batch kept and
    /// announced as it lands. A file the host left out of a full answer is
    /// asked for again in the next batch.
    pub async fn files(
        &self,
        view: ReviewView,
        paths: &[String],
    ) -> Result<Vec<Arc<ReviewFileDiff>>, ReviewError> {
        let summary = self.summary().ok_or(ReviewError::NotOpen)?;
        let mut got: HashMap<&str, Arc<ReviewFileDiff>> = HashMap::new();
        let mut wanted = VecDeque::new();
        let mut asked = HashSet::new();
        for path in paths {
            if !asked.insert(path.as_str()) {
                continue;
            }
            match self.cache.file(&FileKey::new(&summary, view, path)) {
                Some(file) => {
                    got.insert(path.as_str(), file);
                }
                None => wanted.push_back(path.as_str()),
            }
        }
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
                .get_files(&summary.snapshot, view, requests)
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
                self.cache
                    .put_file(FileKey::new(&summary, view, path), file.clone());
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
        }
        let mut answered = HashSet::new();
        Ok(paths
            .iter()
            .filter(|path| answered.insert(path.as_str()))
            .filter_map(|path| got.get(path.as_str()).cloned())
            .collect())
    }

    /// The files of the shown snapshot already fetched for `view`, by
    /// path: what the page draws while the rest arrive.
    pub fn fetched(&self, view: ReviewView) -> BTreeMap<String, Arc<ReviewFileDiff>> {
        let Some(summary) = self.summary() else {
            return BTreeMap::new();
        };
        summary
            .files
            .iter()
            .filter_map(|changed| {
                let file = self
                    .cache
                    .file(&FileKey::new(&summary, view, &changed.path))?;
                Some((changed.path.clone(), file))
            })
            .collect()
    }

    /// More lines of `path` around a change, read from `commit` (the
    /// snapshot, or a hunk's old commit for lines on its removed side), each
    /// numbered as a line of that commit.
    pub async fn context(
        &self,
        commit: &str,
        path: &str,
        lines: ReviewLineRange,
    ) -> Result<Arc<[ReviewLine]>, ReviewError> {
        let key = ContextKey::new(commit, path, lines);
        if let Some(held) = self.cache.context(&key) {
            return Ok(held);
        }
        let summary = self.summary().ok_or(ReviewError::NotOpen)?;
        let request = ReviewFileRequest {
            path: path.to_owned(),
            context: vec![lines],
            context_commit: commit.to_owned(),
        };
        let answer = self
            .reviews
            .get_files(&summary.snapshot, ReviewView::All, vec![request])
            .await;
        let answer = self.answered(answer.map_err(ReviewError::of))?;
        let Some(file) = answer.files.into_iter().find(|file| file.path == path) else {
            return Err(ReviewError::Rpc(RpcError::Refused(wire::Error::new(
                ErrorCode::ResourceExhausted,
                "the agent's host answered without the file asked for",
            ))));
        };
        let held: Arc<[ReviewLine]> = file.context.into();
        self.cache.put_context(key, held.clone());
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
        self.cache.context(&ContextKey::new(commit, path, lines))
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
        let summary = Arc::new(summary);
        let shown = {
            let mut state = self.state();
            let keep = match (&state.shown, taking) {
                (Some(shown), Taking::SameSnapshot) => {
                    shown.id == summary.id && shown.snapshot != summary.snapshot
                }
                _ => false,
            };
            if keep {
                let held = state.shown.as_ref().expect("kept only when shown");
                if held.stale {
                    return held.clone();
                }
                let mut stale = ReviewSummary::clone(held);
                stale.stale = true;
                state.shown = Some(Arc::new(stale));
            } else {
                if state.shown.as_deref() == Some(&*summary) {
                    return summary;
                }
                state.shown = Some(summary.clone());
                self.cache.put_summary(&self.agent_id, summary.clone());
            }
            state.shown.clone().expect("just shown")
        };
        self.notify();
        shown
    }
}

/// The review as one round sent it, from `summary`, the review as its host
/// holds it now: the snapshot the round's comments were made on, compared
/// with the base commit the round was shown against, and of the comments
/// the round sent, `comment_ids`, those still on the review, each on the
/// lines it was written on. A sent review's attachment names the review,
/// the snapshot and the comments, and the host numbered the round as the
/// message arrived, so the round is the one its comments were sent in.
///
/// The host lists changed files only for its latest snapshot, so the
/// round lists none: each comment shows on its file with the code stored
/// with it. None when `summary` is of another review, as after starting
/// over, or holds none of the comments, all deleted since.
pub fn round(
    summary: &ReviewSummary,
    review_id: &[u8],
    snapshot: &str,
    comment_ids: &[Vec<u8>],
) -> Option<ReviewSummary> {
    if summary.id != review_id {
        return None;
    }
    let comments: Vec<_> = summary
        .comments
        .iter()
        .filter(|comment| comment.round.is_some() && comment_ids.contains(&comment.id))
        .map(|comment| wire::experimental::ReviewerComment {
            // Where it sits on a later snapshot is not where it was sent:
            // with no placement it shows on its anchor's lines.
            placement: None,
            ..comment.clone()
        })
        .collect();
    let number = comments.iter().filter_map(|comment| comment.round).min()?;
    let base_commit = (number as usize)
        .checked_sub(1)
        .and_then(|at| summary.rounds.get(at))
        .map_or(&summary.base_commit, |round| &round.base_commit);
    Some(ReviewSummary {
        id: summary.id.clone(),
        base_branch: summary.base_branch.clone(),
        base_commit: base_commit.clone(),
        snapshot: snapshot.to_owned(),
        comments,
        objects: summary.objects.clone(),
        root: summary.root.clone(),
        began_at: summary.began_at.clone(),
        ..ReviewSummary::default()
    })
}

/// Which answers replace the summary the page shows.
#[derive(Clone, Copy)]
enum Taking {
    /// The person asked for it: any answer.
    Always,
    /// It came of something else: only an answer about the same snapshot of
    /// the same review, or of a review begun anew.
    SameSnapshot,
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

/// A file's hunks as one view of one snapshot shows them, which the
/// commits it compares fix: the base commit for all changes, and with it
/// the last round's snapshot for the change since then.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct FileKey {
    snapshot: String,
    view: i32,
    base_commit: String,
    since: String,
    path: String,
}

impl FileKey {
    fn new(summary: &ReviewSummary, view: ReviewView, path: &str) -> FileKey {
        let since = match view {
            ReviewView::SinceLast => summary
                .rounds
                .last()
                .map(|round| round.snapshot.clone())
                .unwrap_or_default(),
            _ => String::new(),
        };
        FileKey {
            snapshot: summary.snapshot.clone(),
            view: view as i32,
            base_commit: summary.base_commit.clone(),
            since,
            path: path.to_owned(),
        }
    }

    fn size(&self) -> usize {
        self.snapshot.len() + self.base_commit.len() + self.since.len() + self.path.len()
    }
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
    /// The last summary an agent's review answered.
    Summary(Vec<u8>),
}

#[derive(Clone)]
enum Held {
    File(Arc<ReviewFileDiff>),
    Context(Arc<[ReviewLine]>),
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
