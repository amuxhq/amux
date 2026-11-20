//! The bodies of a review's files: hunks between two commits, highlighted
//! from each side's whole file, where each hunk sits in the code, each
//! file's outline, and more context read from a commit on request. And the
//! snippet a comment is stored with, worked out the same way.
//!
//! A hunk highlighted alone is coloured wrongly whenever it starts inside a
//! block comment or a string, so each side's whole file is highlighted and
//! its definitions found, and the hunk's lines take their runs from it.
//! That work is the slow part, so what it gives is kept by the file's
//! object id (a review's snapshots share most of their files), and done on
//! a blocking pool a few files at a time, so a host's other work and its
//! other callers are not held up behind a large change. A file too large
//! to show is never highlighted.
//!
//! A call names the comparison it reads: the snapshot shown, the commit
//! compared against, and for what changed since a round the base each of
//! the two was shown against. Each must be one the review handed out, and
//! the review keeps every base a snapshot it keeps was shown against, so a
//! page reads exactly what its summary described, whatever the review's
//! base has become since, and a request answers the same each time.
//!
//! An answer holds at most [`MOST_FILES`] files and [`MOST_BYTES`]
//! encoded; the files past either are left out, for the client to ask for
//! again.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use git_facts::compare::{self, CompareError, DiffLine, FileHunks, LineKind, Shown};
use git_facts::objects::ObjectStore;
use highlight::{Definitions, Highlighter, Language, TokenKind};
use prost::Message as _;
use store::AgentRow;
use store::reviews::{Review, Taken};
use tokio::sync::Semaphore;
use tonic::Status;
use wire::ErrorCode;
use wire::experimental::{
    ReviewAnchor, ReviewDefinition, ReviewFileDiff, ReviewFileRequest, ReviewGetFilesRequest,
    ReviewGetFilesResponse, ReviewHunk, ReviewLine, ReviewLineKind, ReviewRun, ReviewSide,
    ReviewTokenKind, ReviewView,
};

use crate::api::Caller;
use crate::grpc::{status, wire_error};
use crate::runtime::{AgentId, ProfileRuntime};

/// Most files one answer holds. A client asks in smaller batches than
/// this, so the cap only bounds a client that asks for everything at once.
pub(crate) const MOST_FILES: usize = 64;

/// Most bytes of files one answer holds, encoded: well under what a
/// channel carries in one message, and small enough that the first files
/// of a large change reach a phone over a relay before the rest are done.
pub(crate) const MOST_BYTES: usize = 8 << 20;

/// Most lines of more context one file's answer reads, all its ranges
/// together: a long definition whole, and nothing like a whole generated
/// file.
pub(crate) const MOST_CONTEXT_LINES: u32 = 2_000;

/// Lines of unchanged code around each change in a hunk, as git shows.
pub(super) const HUNK_CONTEXT: u32 = 3;

/// Lines above a comment's own that its snippet shows, so it reads in
/// context once its lines are gone.
const SNIPPET_ABOVE: u32 = 3;

/// Most lines a snippet holds; a longer one keeps its start and its end,
/// the middle given as one marker line.
pub(crate) const MOST_SNIPPET_LINES: usize = 60;

/// Most bytes of one snippet line's text; a longer line, a minified file's,
/// keeps its start and ends with the marker.
pub(crate) const MOST_SNIPPET_LINE_BYTES: usize = 256;

/// What the marker standing for a snippet's elided middle says.
const ELIDED: &str = "…";

/// Bytes of highlighting and definitions kept by object id, across every
/// review this process serves. A 5,000-line file's runs and definitions
/// take a few hundred kilobytes, so this keeps a large change's files and
/// those of its earlier rounds.
pub(crate) const CACHE_BYTES: usize = 64 << 20;

/// Files highlighted at once. Each takes a core while it runs, and a
/// grammar built for the first time takes up to half a second, so a large
/// change keeps two cores busy and leaves the rest to the host's agents.
const HIGHLIGHT_JOBS: usize = 2;

/// A git diff's unified context wide enough that a file's diff is one hunk
/// holding every line of both sides: what a snippet is cut from.
const WHOLE_FILE: u32 = 1 << 24;

/// Reviews1.GetFiles: the files `caller` asks for of a comparison the
/// review of `row`'s agent handed out.
pub(crate) async fn get_files(
    runtime: &Arc<ProfileRuntime>,
    row: AgentRow,
    mut request: ReviewGetFilesRequest,
    _caller: &Caller,
) -> Result<ReviewGetFilesResponse, Status> {
    let asked: Vec<ReviewFileRequest> = std::mem::take(&mut request.files)
        .into_iter()
        .take(MOST_FILES)
        .collect();
    let repo = Repo::of(runtime, &row)?;
    let review = review_of(runtime, &row).await?;
    let view = request.view();
    let (taken, since) = comparison(&review, view, &request)?;
    let snapshot = taken.commit.clone();
    let from = request.from.clone();

    let mut answer = ReviewGetFilesResponse::default();
    if request.list {
        answer.changed = match since {
            Some((last, now)) => {
                let all = super::listed(&repo.cwd, &repo.store, taken, now.base).await?;
                super::listed_since(&repo.cwd, &repo.store, last, now, &taken.omitted, &all).await?
            }
            None => super::listed(&repo.cwd, &repo.store, taken, &from).await?,
        };
    }

    // A file the snapshot left out for its size holds only what the index
    // had, so it is not shown at all.
    let shown: Vec<&str> = asked
        .iter()
        .map(|file| file.path.as_str())
        .filter(|path| !taken.omitted.iter().any(|omitted| omitted == path))
        .collect();
    let diffed = match since {
        _ if shown.is_empty() => Vec::new(),
        Some((last, now)) => compare::since_last(
            &repo.cwd,
            &repo.store,
            last,
            now,
            Some(&shown),
            HUNK_CONTEXT,
        )
        .await
        .map_err(compared)?,
        None => compare::hunks(
            &repo.cwd,
            &repo.store,
            &from,
            &snapshot,
            &shown,
            HUNK_CONTEXT,
        )
        .await
        .map_err(compared)?,
    };
    let diffed: HashMap<&str, &FileHunks> = diffed
        .iter()
        .map(|file| (file.path.as_str(), file))
        .collect();

    // Every whole file the answer colours from: both sides of each diff,
    // and each file more context is read from.
    let mut wanted = Vec::new();
    for file in diffed.values().filter(|file| !file.binary) {
        if let Some(blob) = &file.old_blob {
            wanted.push((blob.clone(), old_path(file).to_owned()));
        }
        if let Some(blob) = &file.new_blob {
            wanted.push((blob.clone(), file.path.clone()));
        }
    }
    let mut context_blobs = HashMap::new();
    for file in &asked {
        if file.context.is_empty() || !shown.contains(&file.path.as_str()) {
            continue;
        }
        let at = diffed.get(file.path.as_str()).copied();
        let commit = file.context_commit.as_str();
        let (blob, path) = if commit == snapshot {
            (at.and_then(|file| file.new_blob.clone()), file.path.clone())
        } else if commit == from {
            (
                at.and_then(|file| file.old_blob.clone()),
                at.map_or(file.path.as_str(), old_path).to_owned(),
            )
        } else {
            (None, file.path.clone())
        };
        let blob = match blob {
            Some(blob) => Some(blob),
            None => blob_at(&repo, commit, &path).await?,
        };
        if let Some(blob) = &blob {
            wanted.push((blob.clone(), path.clone()));
        }
        context_blobs.insert(file.path.as_str(), (blob, path));
    }
    let bodies = bodies(&repo, wanted).await?;

    let mut bytes = answer.encoded_len();
    for file in &asked {
        let mut diff = ReviewFileDiff {
            path: file.path.clone(),
            ..ReviewFileDiff::default()
        };
        if shown.contains(&file.path.as_str()) {
            if let Some(hunks) = diffed.get(file.path.as_str()) {
                fill(&mut diff, &from, hunks, &bodies);
            }
            if let Some((blob, path)) = context_blobs.get(file.path.as_str()) {
                let body = blob.as_ref().and_then(|blob| bodies.get(blob, path));
                diff.context = context(&repo, file, path, &snapshot, body).await?;
            }
        }
        // The field's tag and length, besides the file itself. The first
        // file is answered whatever its size, so asking again always gets
        // further; files shown are under a megabyte a side, so one alone
        // stays well inside what a channel carries.
        let size = diff.encoded_len() + 6;
        if bytes + size > MOST_BYTES && (!answer.files.is_empty() || !answer.changed.is_empty()) {
            break;
        }
        bytes += size;
        answer.files.push(diff);
    }
    Ok(answer)
}

/// What changed since a round, as since-last compares it: the round's
/// snapshot and the base it was shown against, then the snapshot shown and
/// its base.
type SinceLast<'a> = (Shown<'a>, Shown<'a>);

/// The comparison a GetFiles call names, checked against what the review
/// handed out: the snapshot shown, one the review keeps; and for what
/// changed since a round, that round and the snapshot, each with the base
/// it was shown against, as since-last compares them.
fn comparison<'a>(
    review: &'a Review,
    view: ReviewView,
    request: &'a ReviewGetFilesRequest,
) -> Result<(&'a Taken, Option<SinceLast<'a>>), Status> {
    let (from, snapshot) = (request.from.as_str(), request.snapshot.as_str());
    let Some(taken) = review.snapshot(snapshot) else {
        return Err(refused(
            ErrorCode::NotFound,
            format!("{snapshot} is not a snapshot of this agent's review"),
        ));
    };
    match view {
        // From a base the snapshot has been shown against, or for a
        // round's snapshot the base that round records (one of those, as
        // the round took it from the snapshot when it closed).
        ReviewView::All => {
            let base = from == taken.base_commit
                || taken.bases.iter().any(|base| base == from)
                || review
                    .rounds
                    .iter()
                    .any(|round| round.snapshot == snapshot && round.base_commit == from);
            match base {
                true => Ok((taken, None)),
                false => Err(refused(
                    ErrorCode::InvalidArgument,
                    format!("all changes of {snapshot} run from its base commit, not {from}"),
                )),
            }
        }
        ReviewView::SinceLast => {
            let base = request.base.as_str();
            if base != taken.base_commit && !taken.bases.iter().any(|shown| shown == base) {
                return Err(refused(
                    ErrorCode::InvalidArgument,
                    format!("{snapshot} has not been shown against the base commit {base:?}"),
                ));
            }
            match review
                .rounds
                .iter()
                .find(|round| round.snapshot == from && round.base_commit == request.from_base)
            {
                Some(round) => Ok((
                    taken,
                    Some((
                        Shown {
                            base: &round.base_commit,
                            snapshot: &round.snapshot,
                        },
                        Shown {
                            base,
                            snapshot: &taken.commit,
                        },
                    )),
                )),
                None if review.rounds.is_empty() => Err(refused(
                    ErrorCode::InvalidArgument,
                    "no round has been sent, so nothing has changed since the last one",
                )),
                None => Err(refused(
                    ErrorCode::InvalidArgument,
                    format!(
                        "{from} shown against {:?} is not a round to compare {snapshot} with",
                        request.from_base
                    ),
                )),
            }
        }
        ReviewView::Unspecified => Err(refused(
            ErrorCode::InvalidArgument,
            "a view is needed: all changes, or since the last round",
        )),
    }
}

/// The snippet a comment on `anchor` is stored with, and the definitions
/// enclosing its lines, outermost first: the commented lines and up to
/// three above them in diff form, as the comparison it was written in
/// shows them, highlighted. A comment on a line of one of the review's
/// snapshots was written comparing it with that snapshot's base commit; one
/// on any other commit, on a removed line comparing that commit with the
/// latest snapshot. A comment on the whole file, or on a file too large or
/// not text, has neither.
pub(crate) async fn snippet(
    runtime: &Arc<ProfileRuntime>,
    row: &AgentRow,
    review: &Review,
    anchor: &ReviewAnchor,
) -> Result<(Vec<ReviewLine>, Vec<ReviewDefinition>), Status> {
    let (Some(lines), Some(latest)) = (anchor.lines, review.latest()) else {
        return Ok((Vec::new(), Vec::new()));
    };
    if lines.start == 0 {
        return Ok((Vec::new(), Vec::new()));
    }
    let (start, end) = (lines.start, lines.end.max(lines.start));
    let repo = Repo::of(runtime, row)?;
    match review.snapshot(&anchor.commit) {
        Some(taken) => cut(&repo, &taken.base_commit, &taken.commit, anchor, start, end).await,
        None => cut(&repo, &anchor.commit, &latest.commit, anchor, start, end).await,
    }
}

/// The snippet of lines `start` to `end` of `anchor`'s file at its commit,
/// as the comparison from `from` to the snapshot `to` shows them, and the
/// definitions enclosing them: [`snippet`] once the comparison is known.
async fn cut(
    repo: &Repo,
    from: &str,
    to: &str,
    anchor: &ReviewAnchor,
    start: u32,
    end: u32,
) -> Result<(Vec<ReviewLine>, Vec<ReviewDefinition>), Status> {
    let on_snapshot = anchor.commit == to;
    let file = compare::hunks(
        &repo.cwd,
        &repo.store,
        from,
        to,
        &[&anchor.path],
        WHOLE_FILE,
    )
    .await
    .map_err(compared)?
    .into_iter()
    .find(|file| file.path == anchor.path);
    if file.as_ref().is_some_and(|file| file.binary) {
        return Ok((Vec::new(), Vec::new()));
    }

    // The anchored side's whole file, and the other side's where the
    // snippet shows lines of it too.
    let mut wanted = Vec::new();
    let side_path = match (&file, on_snapshot) {
        (Some(file), false) => old_path(file).to_owned(),
        _ => anchor.path.clone(),
    };
    let side_blob = match (&file, on_snapshot) {
        (Some(file), true) => file.new_blob.clone(),
        (Some(file), false) => file.old_blob.clone(),
        (None, _) => blob_at(repo, &anchor.commit, &anchor.path).await?,
    };
    if let Some(blob) = &side_blob {
        wanted.push((blob.clone(), side_path.clone()));
    }
    if let Some(file) = &file {
        if let Some(blob) = &file.old_blob {
            wanted.push((blob.clone(), old_path(file).to_owned()));
        }
        if let Some(blob) = &file.new_blob {
            wanted.push((blob.clone(), file.path.clone()));
        }
    }
    let bodies = bodies(repo, wanted).await?;
    let Some(side) = side_blob.and_then(|blob| bodies.get(&blob, &side_path)) else {
        return Ok((Vec::new(), Vec::new()));
    };
    let Body::Code { definitions, .. } = side.as_ref() else {
        return Ok((Vec::new(), Vec::new()));
    };
    let scope_side = match on_snapshot {
        true => ReviewSide::Snapshot,
        false => ReviewSide::Compared,
    };
    let scope = chain(
        &definitions.enclosing(highlight::LineRange::new(start, end)),
        scope_side,
    );

    let first = start.saturating_sub(SNIPPET_ABOVE).max(1);
    let side_number = |line: &DiffLine| match on_snapshot {
        true => line.new_line,
        false => line.old_line,
    };
    let mut snippet: Vec<ReviewLine> = match &file {
        // One hunk holds the whole file: the lines from the first one on
        // the anchored side to the last, with the other side's lines that
        // fall between them.
        Some(file) => {
            let lines: Vec<&DiffLine> = file.hunks.iter().flat_map(|hunk| &hunk.lines).collect();
            let within = |line: &&DiffLine| {
                let number = side_number(line);
                number >= first && number <= end
            };
            match (
                lines.iter().position(within),
                lines.iter().rposition(within),
            ) {
                (Some(at), Some(to)) => lines[at..=to]
                    .iter()
                    .map(|line| {
                        let body = match line.kind {
                            LineKind::Removed => {
                                file.old_blob.as_ref().map(|blob| (blob, old_path(file)))
                            }
                            _ => file
                                .new_blob
                                .as_ref()
                                .map(|blob| (blob, file.path.as_str())),
                        };
                        let body = body.and_then(|(blob, path)| bodies.get(blob, path));
                        review_line(line, body.as_deref())
                    })
                    .collect(),
                _ => Vec::new(),
            }
        }
        // The file is the same on both sides: every line is context.
        None => compare::lines(
            &repo.cwd,
            &repo.store,
            &anchor.commit,
            &anchor.path,
            &[compare::LineRange::new(first, end)],
        )
        .await
        .map_err(compared)?
        .unwrap_or_default()
        .into_iter()
        .map(|line| {
            let runs = runs_of(Some(side.as_ref()), line.number, &line.text);
            ReviewLine {
                kind: ReviewLineKind::Context as i32,
                old_line: line.number,
                new_line: line.number,
                text: line.text,
                runs,
            }
        })
        .collect(),
    };
    if snippet.len() > MOST_SNIPPET_LINES {
        let tail = snippet.split_off(snippet.len() - (MOST_SNIPPET_LINES - 1) / 2);
        snippet.truncate(MOST_SNIPPET_LINES - 1 - tail.len());
        snippet.push(ReviewLine {
            kind: ReviewLineKind::Unspecified as i32,
            text: ELIDED.to_owned(),
            ..ReviewLine::default()
        });
        snippet.extend(tail);
    }
    for line in &mut snippet {
        clipped(line);
    }
    Ok((snippet, scope))
}

/// `line` with its text cut to [`MOST_SNIPPET_LINE_BYTES`], ending with the
/// marker, its runs cut with it.
fn clipped(line: &mut ReviewLine) {
    if line.text.len() <= MOST_SNIPPET_LINE_BYTES {
        return;
    }
    let mut cut = MOST_SNIPPET_LINE_BYTES - ELIDED.len();
    while !line.text.is_char_boundary(cut) {
        cut -= 1;
    }
    line.text.truncate(cut);
    let mut left = cut as u32;
    line.runs.retain_mut(|run| {
        let keep = left > 0;
        run.len = run.len.min(left);
        left -= run.len;
        keep
    });
    line.text.push_str(ELIDED);
    if !line.runs.is_empty() {
        line.runs.push(ReviewRun {
            kind: ReviewTokenKind::Plain as i32,
            len: ELIDED.len() as u32,
        });
    }
}

/// Whole files this process has highlighted, each file's every side and
/// every read counted once: what the cache by object id saves shows as
/// this not moving.
pub fn files_highlighted() -> u64 {
    HIGHLIGHTED.load(Ordering::Relaxed)
}

static HIGHLIGHTED: AtomicU64 = AtomicU64::new(0);

/// Where an agent's review reads git: the agent's folder, with its
/// snapshot store as an alternate.
struct Repo {
    cwd: PathBuf,
    store: ObjectStore,
}

impl Repo {
    fn of(runtime: &ProfileRuntime, row: &AgentRow) -> Result<Repo, Status> {
        let id = AgentId::from_slice(&row.agent.agent)
            .map_err(|_| refused(ErrorCode::NotFound, "no agent with that id"))?;
        Ok(Repo {
            cwd: PathBuf::from(&row.cwd),
            store: ObjectStore::at(daemon_core::reviews::objects(&runtime.agent_dir(id))),
        })
    }
}

async fn review_of(runtime: &ProfileRuntime, row: &AgentRow) -> Result<Review, Status> {
    let agent = row.agent.agent.clone();
    runtime
        .read_rows(move |store| store.review(&agent))
        .await
        .map_err(|error| refused(ErrorCode::Internal, format!("the store: {error}")))?
        .ok_or_else(|| {
            refused(
                ErrorCode::FailedPrecondition,
                "this agent has no review yet: open it first",
            )
        })
}

/// The object id of the file `path` at `commit`, None where the commit has
/// no file there.
async fn blob_at(repo: &Repo, commit: &str, path: &str) -> Result<Option<String>, Status> {
    // The commit's tree lists the path's mode, type and id, from the
    // repository's top whatever folder git runs in.
    let output = repo
        .store
        .reading(
            &repo.cwd,
            &[
                "--literal-pathspecs",
                "ls-tree",
                "-z",
                "--full-tree",
                commit,
                "--",
                path,
            ],
        )
        .output()
        .await
        .map_err(|error| refused(ErrorCode::Internal, format!("running git: {error}")))?;
    if !output.status.success() {
        return Err(compared(CompareError::Missing(commit.to_owned())));
    }
    let listed = String::from_utf8_lossy(&output.stdout);
    Ok(listed.split('\0').find_map(|entry| {
        let (info, listed_path) = entry.split_once('\t')?;
        let mut info = info.split(' ');
        let (_mode, kind, id) = (info.next()?, info.next()?, info.next()?);
        (kind == "blob" && listed_path == path).then(|| id.to_owned())
    }))
}

/// A comparison's failure as the caller is told it. A commit that is gone
/// is not found: the client shows that round from its comments' snippets.
fn compared(error: CompareError) -> Status {
    match error {
        CompareError::Missing(_) => refused(ErrorCode::NotFound, error.to_string()),
        // The folder is not a repository any more, or git cannot read it:
        // the agent's state, not this host's.
        CompareError::Git(_) => refused(ErrorCode::FailedPrecondition, error.to_string()),
    }
}

fn refused(code: ErrorCode, message: impl Into<String>) -> Status {
    status(wire_error(code, message))
}

fn old_path(file: &FileHunks) -> &str {
    file.old_path.as_deref().unwrap_or(&file.path)
}

/// One file's hunks, each line with its runs and each hunk with where its
/// changes sit, and the outline of what its changes touch on either side.
fn fill(diff: &mut ReviewFileDiff, from: &str, file: &FileHunks, bodies: &Bodies) {
    if file.binary {
        return;
    }
    let old = file
        .old_blob
        .as_ref()
        .and_then(|blob| bodies.get(blob, old_path(file)));
    let new = file
        .new_blob
        .as_ref()
        .and_then(|blob| bodies.get(blob, &file.path));
    let unshown = |body: &Option<Arc<Body>>| matches!(body.as_deref(), Some(Body::Unshown));
    if unshown(&old) || unshown(&new) {
        return;
    }
    let definitions = |body: &Option<Arc<Body>>| match body.as_deref() {
        Some(Body::Code { definitions, .. }) => Some(definitions.clone()),
        _ => None,
    };
    let (old_definitions, new_definitions) = (definitions(&old), definitions(&new));
    let mut added = Vec::new();
    let mut removed = Vec::new();
    for hunk in &file.hunks {
        let lines: Vec<ReviewLine> = hunk
            .lines
            .iter()
            .map(|line| {
                let body = match line.kind {
                    LineKind::Removed => old.as_deref(),
                    _ => new.as_deref(),
                };
                review_line(line, body)
            })
            .collect();
        let hunk_added = spans(&hunk.lines, LineKind::Added, |line| line.new_line);
        let hunk_removed = spans(&hunk.lines, LineKind::Removed, |line| line.old_line);
        // Where the changed lines sit: the added ones, or for a hunk that
        // only removes, the removed ones in the file they were removed from.
        let scope = match (hunk_added.first(), hunk_added.last()) {
            (Some(first), Some(last)) => new_definitions.as_ref().map(|defs| {
                chain(
                    &defs.enclosing(highlight::LineRange::new(first.start, last.end)),
                    ReviewSide::Snapshot,
                )
            }),
            _ => match (hunk_removed.first(), hunk_removed.last()) {
                (Some(first), Some(last)) => old_definitions.as_ref().map(|defs| {
                    chain(
                        &defs.enclosing(highlight::LineRange::new(first.start, last.end)),
                        ReviewSide::Compared,
                    )
                }),
                _ => None,
            },
        };
        added.extend(hunk_added);
        removed.extend(hunk_removed);
        diff.hunks.push(ReviewHunk {
            old_commit: from.to_owned(),
            old_start: hunk.old_start,
            new_start: hunk.new_start,
            scope: scope.unwrap_or_default(),
            lines,
        });
    }
    diff.outline = outline(
        new_definitions
            .map(|defs| defs.outline(&added))
            .unwrap_or_default(),
        old_definitions
            .map(|defs| defs.outline(&removed))
            .unwrap_or_default(),
    );
}

/// The runs of consecutive `kind` lines in a hunk, numbered by `number`.
fn spans(
    lines: &[DiffLine],
    kind: LineKind,
    number: impl Fn(&DiffLine) -> u32,
) -> Vec<highlight::LineRange> {
    let mut spans: Vec<highlight::LineRange> = Vec::new();
    for line in lines.iter().filter(|line| line.kind == kind) {
        let at = number(line);
        match spans.last_mut() {
            Some(span) if span.end + 1 == at => span.end = at,
            _ => spans.push(highlight::LineRange::line(at)),
        }
    }
    spans
}

/// One outline from the definitions the added lines touch and those the
/// removed lines touch: the new side's, then each of the old side's not
/// already there under the same parent, such as a function deleted whole.
/// A definition found only on the old side keeps its lines there, and says
/// so.
fn outline(
    new: Vec<highlight::Definition>,
    old: Vec<highlight::Definition>,
) -> Vec<ReviewDefinition> {
    let snapshot_side = new.len();
    let mut merged = new;
    let mut at = Vec::with_capacity(old.len());
    for definition in old {
        let parent = definition.parent.and_then(|parent| at[parent]);
        let same = merged.iter().position(|kept| {
            kept.kind == definition.kind && kept.label == definition.label && kept.parent == parent
        });
        at.push(Some(match same {
            Some(index) => index,
            None => {
                merged.push(highlight::Definition {
                    parent,
                    ..definition
                });
                merged.len() - 1
            }
        }));
    }
    merged
        .iter()
        .enumerate()
        .map(|(index, definition)| {
            let side = match index < snapshot_side {
                true => ReviewSide::Snapshot,
                false => ReviewSide::Compared,
            };
            review_definition(definition, definition.parent, side)
        })
        .collect()
}

/// A chain of definitions on `side`, outermost first, as the protocol lists
/// one: each one's parent the one before it.
fn chain(chain: &[&highlight::Definition], side: ReviewSide) -> Vec<ReviewDefinition> {
    chain
        .iter()
        .enumerate()
        .map(|(index, definition)| review_definition(definition, index.checked_sub(1), side))
        .collect()
}

fn review_definition(
    definition: &highlight::Definition,
    parent: Option<usize>,
    side: ReviewSide,
) -> ReviewDefinition {
    ReviewDefinition {
        // The protocol's numbers are the highlight crate's own.
        kind: definition.kind as i32,
        name: definition.name.clone(),
        label: definition.label.clone(),
        start_line: definition.lines.start,
        end_line: definition.lines.end,
        parent: parent.map(|parent| parent as u32),
        side: side as i32,
    }
}

fn review_line(line: &DiffLine, body: Option<&Body>) -> ReviewLine {
    let (kind, number) = match line.kind {
        LineKind::Context => (ReviewLineKind::Context, line.new_line),
        LineKind::Added => (ReviewLineKind::Added, line.new_line),
        LineKind::Removed => (ReviewLineKind::Removed, line.old_line),
    };
    ReviewLine {
        kind: kind as i32,
        old_line: line.old_line,
        new_line: line.new_line,
        text: line.text.clone(),
        runs: runs_of(body, number, &line.text),
    }
}

/// The runs of line `number` of a highlighted file, when they cover
/// `text` exactly; none, drawing it plain, when they do not.
fn runs_of(body: Option<&Body>, number: u32, text: &str) -> Vec<ReviewRun> {
    let Some(Body::Code { lines, .. }) = body else {
        return Vec::new();
    };
    let Some(runs) = number
        .checked_sub(1)
        .and_then(|index| lines.get(index as usize))
    else {
        return Vec::new();
    };
    let covered: usize = runs.iter().map(|run| run.len as usize).sum();
    match covered == text.len() {
        true => runs.to_vec(),
        false => Vec::new(),
    }
}

/// The context lines `file` asks for, read from its context commit, with
/// the runs of `body`, that file highlighted whole. A line of the snapshot
/// is numbered as a new line, one of any other commit as an old line.
async fn context(
    repo: &Repo,
    file: &ReviewFileRequest,
    path: &str,
    snapshot: &str,
    body: Option<Arc<Body>>,
) -> Result<Vec<ReviewLine>, Status> {
    if matches!(body.as_deref(), Some(Body::Unshown)) {
        return Ok(Vec::new());
    }
    let ranges = context_ranges(&file.context);
    if ranges.is_empty() {
        return Ok(Vec::new());
    }
    let commit = file.context_commit.as_str();
    let read = compare::lines(&repo.cwd, &repo.store, commit, path, &ranges)
        .await
        .map_err(compared)?
        .unwrap_or_default();
    let on_snapshot = commit == snapshot;
    Ok(read
        .into_iter()
        .map(|line| ReviewLine {
            kind: ReviewLineKind::Context as i32,
            old_line: if on_snapshot { 0 } else { line.number },
            new_line: if on_snapshot { line.number } else { 0 },
            runs: runs_of(body.as_deref(), line.number, &line.text),
            text: line.text,
        })
        .collect())
}

/// The ranges of more context asked for, merged where they overlap or
/// touch, in order, and cut so that together they read at most
/// [`MOST_CONTEXT_LINES`]: however a client splits its ask, one file's
/// answer holds no more. Zero to zero, the whole file, reads from its
/// start.
fn context_ranges(asked: &[wire::experimental::ReviewLineRange]) -> Vec<compare::LineRange> {
    let mut ranges: Vec<(u32, u32)> = asked
        .iter()
        .filter(|range| range.start == 0 || range.start <= range.end)
        .map(|range| match range.start {
            0 => (1, MOST_CONTEXT_LINES),
            start => (start, range.end),
        })
        .collect();
    ranges.sort_unstable();
    let mut merged: Vec<(u32, u32)> = Vec::with_capacity(ranges.len());
    for (start, end) in ranges {
        match merged.last_mut() {
            Some(last) if start <= last.1.saturating_add(1) => last.1 = last.1.max(end),
            _ => merged.push((start, end)),
        }
    }
    let mut left = MOST_CONTEXT_LINES;
    let mut cut = Vec::with_capacity(merged.len());
    for (start, end) in merged {
        if left == 0 {
            break;
        }
        let end = end.min(start.saturating_add(left - 1));
        left -= end - start + 1;
        cut.push(compare::LineRange::new(start, end));
    }
    cut
}

/// What is kept of one whole file, by its object id.
enum Body {
    /// Text: each line's runs, none for a line drawn plain, and the
    /// definitions it holds.
    Code {
        lines: Box<[Box<[ReviewRun]>]>,
        definitions: Definitions,
    },
    /// Too large to show, or not text: nothing of it is shown.
    Unshown,
}

impl Body {
    /// Roughly what it holds in memory, for the cache's bound.
    fn bytes(&self) -> usize {
        match self {
            Body::Code { lines, definitions } => {
                let lines: usize = lines
                    .iter()
                    .map(|runs| size_of::<Box<[ReviewRun]>>() + size_of_val(&**runs))
                    .sum();
                let definitions: usize = definitions
                    .all()
                    .iter()
                    .map(|definition| {
                        size_of::<highlight::Definition>()
                            + definition.name.len()
                            + definition.label.len()
                    })
                    .sum();
                lines + definitions + size_of::<Body>()
            }
            Body::Unshown => size_of::<Body>(),
        }
    }
}

/// A whole file's highlighting is kept by its object id and the language
/// its path names: one object at two paths can be two languages.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Key {
    blob: String,
    language: Option<Language>,
}

impl Key {
    fn of(blob: &str, path: &str) -> Key {
        Key {
            blob: blob.to_owned(),
            language: Language::from_path(path),
        }
    }
}

/// The whole files an answer colours from.
#[derive(Default)]
struct Bodies(HashMap<Key, Arc<Body>>);

impl Bodies {
    fn get(&self, blob: &str, path: &str) -> Option<Arc<Body>> {
        self.0.get(&Key::of(blob, path)).cloned()
    }
}

/// The whole files `wanted` names by object id and path, from the cache or
/// read and highlighted now on the blocking pool.
async fn bodies(repo: &Repo, wanted: Vec<(String, String)>) -> Result<Bodies, Status> {
    let mut found = Bodies::default();
    let mut missing: Vec<(Key, String)> = Vec::new();
    {
        let mut cache = cache();
        for (blob, path) in wanted {
            let key = Key::of(&blob, &path);
            if found.0.contains_key(&key) || missing.iter().any(|(kept, _)| *kept == key) {
                continue;
            }
            match cache.get(&key) {
                Some(body) => {
                    found.0.insert(key, body);
                }
                None => missing.push((key, path)),
            }
        }
    }
    if missing.is_empty() {
        return Ok(found);
    }
    let ids: Vec<&str> = missing.iter().map(|(key, _)| key.blob.as_str()).collect();
    let read = compare::blobs(&repo.cwd, &repo.store, &ids)
        .await
        .map_err(compared)?;
    let jobs = missing
        .into_iter()
        .zip(read)
        .filter_map(|((key, path), bytes)| Some((key, path, bytes?)))
        .map(|(key, path, bytes)| async move {
            let body = on_pool(move || highlighted(&path, &bytes)).await?;
            Ok::<_, Status>((key, Arc::new(body)))
        });
    for done in futures_util::future::join_all(jobs).await {
        let (key, body) = done?;
        cache().put(key.clone(), body.clone());
        found.0.insert(key, body);
    }
    Ok(found)
}

/// A whole file highlighted and its definitions found: on the calling
/// thread, which is the blocking pool's.
fn highlighted(path: &str, bytes: &[u8]) -> Body {
    // git's own test for a binary file: a zero byte near the start.
    let binary = bytes[..bytes.len().min(8000)].contains(&0);
    if binary || bytes.len() as u64 > git_facts::LARGE_FILE_BYTES {
        return Body::Unshown;
    }
    HIGHLIGHTED.fetch_add(1, Ordering::Relaxed);
    let text = String::from_utf8_lossy(bytes);
    let language = Language::from_file(path, &text);
    let lines = Highlighter::shared().highlight_now(&text, language);
    let definitions = Definitions::of(&text, language);
    Body::Code {
        lines: lines.into_iter().map(|line| runs(&line)).collect(),
        definitions,
    }
}

/// A highlighted line as the protocol's runs; none for a line all plain.
fn runs(line: &highlight::Line) -> Box<[ReviewRun]> {
    if line.iter().all(|run| run.kind == TokenKind::Plain) {
        return Box::new([]);
    }
    line.iter()
        .map(|run| ReviewRun {
            kind: token_kind(run.kind) as i32,
            len: run.text.len() as u32,
        })
        .collect()
}

fn token_kind(kind: TokenKind) -> ReviewTokenKind {
    match kind {
        TokenKind::Plain => ReviewTokenKind::Plain,
        TokenKind::Keyword => ReviewTokenKind::Keyword,
        TokenKind::String => ReviewTokenKind::String,
        TokenKind::Number => ReviewTokenKind::Number,
        TokenKind::Type => ReviewTokenKind::Type,
        TokenKind::Function => ReviewTokenKind::Function,
        TokenKind::Comment => ReviewTokenKind::Comment,
        TokenKind::Inserted => ReviewTokenKind::Inserted,
        TokenKind::Deleted => ReviewTokenKind::Deleted,
    }
}

/// `work` on the blocking pool, once one of the pool's few places is free.
async fn on_pool<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, Status> {
    static POOL: OnceLock<Arc<Semaphore>> = OnceLock::new();
    // The place goes with the work, so it is held for as long as the work
    // runs, even when the call that asked for it is dropped meanwhile.
    let place = POOL
        .get_or_init(|| Arc::new(Semaphore::new(HIGHLIGHT_JOBS)))
        .clone()
        .acquire_owned()
        .await
        .expect("the pool is never closed");
    tokio::task::spawn_blocking(move || {
        let _place = place;
        work()
    })
    .await
    .map_err(|error| refused(ErrorCode::Internal, format!("highlighting a file: {error}")))
}

/// Whole files' highlighting kept by object id, the least recently used
/// going first once the whole is over [`CACHE_BYTES`].
#[derive(Default)]
struct Cache {
    entries: HashMap<Key, Kept>,
    /// Each entry by when it was last used, oldest first.
    used: BTreeMap<u64, Key>,
    bytes: usize,
    clock: u64,
}

struct Kept {
    body: Arc<Body>,
    used: u64,
    bytes: usize,
}

impl Cache {
    fn get(&mut self, key: &Key) -> Option<Arc<Body>> {
        self.clock += 1;
        let kept = self.entries.get_mut(key)?;
        self.used.remove(&kept.used);
        kept.used = self.clock;
        self.used.insert(self.clock, key.clone());
        Some(kept.body.clone())
    }

    fn put(&mut self, key: Key, body: Arc<Body>) {
        self.clock += 1;
        let bytes = body.bytes() + key.blob.len() + size_of::<Kept>();
        if let Some(old) = self.entries.remove(&key) {
            self.used.remove(&old.used);
            self.bytes -= old.bytes;
        }
        while self.bytes + bytes > CACHE_BYTES {
            let Some((_, oldest)) = self.used.pop_first() else {
                break;
            };
            if let Some(gone) = self.entries.remove(&oldest) {
                self.bytes -= gone.bytes;
            }
        }
        self.used.insert(self.clock, key.clone());
        self.entries.insert(
            key,
            Kept {
                body,
                used: self.clock,
                bytes,
            },
        );
        self.bytes += bytes;
    }
}

fn cache() -> MutexGuard<'static, Cache> {
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    CACHE
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code(lines: usize) -> Arc<Body> {
        Arc::new(Body::Code {
            lines: (0..lines)
                .map(|_| {
                    vec![ReviewRun {
                        kind: ReviewTokenKind::Keyword as i32,
                        len: 2,
                    }]
                    .into_boxed_slice()
                })
                .collect(),
            definitions: Definitions::default(),
        })
    }

    fn key(blob: &str) -> Key {
        Key::of(blob, "a.rs")
    }

    #[test]
    fn the_cache_lets_the_least_recently_used_go_past_its_bound() {
        let mut cache = Cache::default();
        // Each takes a little over a tenth of the bound.
        let lines = CACHE_BYTES / 10 / (size_of::<Box<[ReviewRun]>>() + size_of::<ReviewRun>());
        for n in 0..12 {
            cache.put(key(&format!("{n:02}")), code(lines));
            assert!(cache.bytes <= CACHE_BYTES, "{} bytes kept", cache.bytes);
        }
        assert!(cache.get(&key("00")).is_none(), "the first went");
        assert!(cache.get(&key("11")).is_some(), "the last stays");
        // Within the bound, nothing goes.
        let mut small = Cache::default();
        small.put(key("a"), code(1));
        small.put(key("b"), code(1));
        assert!(small.get(&key("a")).is_some());
        assert_eq!(small.entries.len(), 2);
    }

    #[test]
    fn a_body_over_the_bound_evicts_the_oldest_first() {
        let mut cache = Cache::default();
        // Each takes a little over a third of the bound.
        let lines = CACHE_BYTES / 3 / (size_of::<Box<[ReviewRun]>>() + size_of::<ReviewRun>());
        cache.put(key("a"), code(lines));
        cache.put(key("b"), code(lines));
        // Using a makes b the oldest.
        assert!(cache.get(&key("a")).is_some());
        cache.put(key("c"), code(lines));
        assert!(cache.bytes <= CACHE_BYTES, "{} bytes kept", cache.bytes);
        assert!(cache.get(&key("a")).is_some());
        assert!(cache.get(&key("b")).is_none(), "b went first");
        assert!(cache.get(&key("c")).is_some());
    }

    #[test]
    fn one_object_at_two_paths_is_kept_twice_by_language() {
        assert_ne!(Key::of("abc", "x.rs"), Key::of("abc", "x.py"));
        assert_eq!(Key::of("abc", "x.rs"), Key::of("abc", "y/z.rs"));
    }

    #[test]
    fn a_file_too_large_or_not_text_is_never_highlighted() {
        let before = files_highlighted();
        let large = vec![b'a'; git_facts::LARGE_FILE_BYTES as usize + 1];
        assert!(matches!(highlighted("big.rs", &large), Body::Unshown));
        assert!(matches!(highlighted("x.bin", b"\0\x01\x02"), Body::Unshown));
        let Body::Code { lines, .. } = highlighted("a.rs", b"/* one\ntwo */ fn x() {}\n") else {
            panic!("text is highlighted");
        };
        assert!(files_highlighted() > before);
        // The second line starts inside a block comment, and is coloured so.
        assert_eq!(lines[1][0].kind, ReviewTokenKind::Comment as i32);
        assert_eq!(lines[1].iter().map(|run| run.len).sum::<u32>(), 16);
    }

    #[test]
    fn runs_that_do_not_cover_the_line_are_dropped() {
        let body = code(2);
        assert_eq!(runs_of(Some(&body), 1, "fn").len(), 1);
        assert!(runs_of(Some(&body), 1, "fn x").is_empty());
        assert!(runs_of(Some(&body), 3, "fn").is_empty());
        assert!(runs_of(Some(&body), 0, "fn").is_empty());
        assert!(runs_of(None, 1, "fn").is_empty());
    }

    fn git(cwd: &std::path::Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .args([
                "-c",
                "user.name=amux",
                "-c",
                "user.email=amux@example.invalid",
            ])
            .args(["-c", "commit.gpgsign=false", "-c", "core.fsmonitor=false"])
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("git runs");
        assert!(output.status.success(), "git {args:?}: {output:?}");
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    /// A repository with `before` committed, then `after`: both commits.
    fn two_commits(
        dir: &std::path::Path,
        path: &str,
        before: &str,
        after: &str,
    ) -> (Repo, String, String) {
        git(dir, &["init", "-q", "-b", "main"]);
        std::fs::write(dir.join(path), before).unwrap();
        git(dir, &["add", "."]);
        git(dir, &["commit", "-q", "-m", "before"]);
        let from = git(dir, &["rev-parse", "HEAD"]);
        std::fs::write(dir.join(path), after).unwrap();
        git(dir, &["commit", "-q", "--allow-empty", "-am", "after"]);
        let to = git(dir, &["rev-parse", "HEAD"]);
        let repo = Repo {
            cwd: dir.to_owned(),
            store: ObjectStore::at(dir.join("no-store")),
        };
        (repo, from, to)
    }

    fn anchor(commit: &str, path: &str, start: u32, end: u32) -> ReviewAnchor {
        ReviewAnchor {
            commit: commit.to_owned(),
            path: path.to_owned(),
            lines: Some(wire::experimental::ReviewLineRange { start, end }),
        }
    }

    fn shown(lines: &[ReviewLine]) -> Vec<(char, u32, u32, &str)> {
        lines
            .iter()
            .map(|line| {
                let mark = match line.kind() {
                    ReviewLineKind::Added => '+',
                    ReviewLineKind::Removed => '-',
                    ReviewLineKind::Context => ' ',
                    ReviewLineKind::Unspecified => '?',
                };
                (mark, line.old_line, line.new_line, line.text.as_str())
            })
            .collect()
    }

    const BEFORE: &str = "impl Store {\n    fn open() {\n        let a = 1;\n        let b = 2;\n        let c = 3;\n        let d = 4;\n    }\n}\n";
    const AFTER: &str = "impl Store {\n    fn open() {\n        let a = 1;\n        let b = 2;\n        let c = 3;\n        let d = 40;\n        let e = 5;\n    }\n}\n";

    #[tokio::test]
    async fn a_snippet_holds_the_lines_and_three_above_in_diff_form_with_its_scope() {
        let dir = tempfile::tempdir().unwrap();
        let (repo, from, to) = two_commits(dir.path(), "a.rs", BEFORE, AFTER);

        // On the snapshot's lines 6 and 7, compared with the base.
        let (lines, scope) = cut(&repo, &from, &to, &anchor(&to, "a.rs", 6, 7), 6, 7)
            .await
            .unwrap();
        assert_eq!(
            shown(&lines),
            [
                (' ', 3, 3, "        let a = 1;"),
                (' ', 4, 4, "        let b = 2;"),
                (' ', 5, 5, "        let c = 3;"),
                ('-', 6, 0, "        let d = 4;"),
                ('+', 0, 6, "        let d = 40;"),
                ('+', 0, 7, "        let e = 5;"),
            ]
        );
        // Highlighted from the whole file: `let` is a keyword on every side.
        for line in &lines {
            assert!(
                line.runs
                    .iter()
                    .any(|run| run.kind() == ReviewTokenKind::Keyword),
                "{line:?}"
            );
        }
        let labels: Vec<(&str, Option<u32>)> = scope
            .iter()
            .map(|definition| (definition.label.as_str(), definition.parent))
            .collect();
        assert_eq!(labels, [("impl Store", None), ("fn open", Some(0))]);
        assert_eq!((scope[1].start_line, scope[1].end_line), (2, 8));

        // On the base's removed line 6: the old side, scoped in the base.
        let (lines, scope) = cut(&repo, &from, &to, &anchor(&from, "a.rs", 6, 6), 6, 6)
            .await
            .unwrap();
        assert_eq!(
            shown(&lines),
            [
                (' ', 3, 3, "        let a = 1;"),
                (' ', 4, 4, "        let b = 2;"),
                (' ', 5, 5, "        let c = 3;"),
                ('-', 6, 0, "        let d = 4;"),
            ]
        );
        assert_eq!((scope[1].start_line, scope[1].end_line), (2, 7));

        // Near the top there are fewer lines above.
        let (lines, _) = cut(&repo, &from, &to, &anchor(&to, "a.rs", 2, 2), 2, 2)
            .await
            .unwrap();
        assert_eq!(
            shown(&lines),
            [(' ', 1, 1, "impl Store {"), (' ', 2, 2, "    fn open() {")]
        );
    }

    #[tokio::test]
    async fn a_snippet_of_an_unchanged_file_is_context_and_a_long_one_is_cut_in_the_middle() {
        let dir = tempfile::tempdir().unwrap();
        let long: String = (1..=100).map(|n| format!("line {n}\n")).collect();
        let (repo, from, to) = two_commits(dir.path(), "notes.txt", &long, &long);
        std::fs::write(dir.path().join("other.txt"), "x\n").unwrap();

        let (lines, scope) = cut(&repo, &from, &to, &anchor(&to, "notes.txt", 10, 90), 10, 90)
            .await
            .unwrap();
        assert!(scope.is_empty(), "plain text has no definitions");
        assert_eq!(lines.len(), MOST_SNIPPET_LINES);
        assert_eq!(shown(&lines[..1]), [(' ', 7, 7, "line 7")]);
        assert_eq!(shown(&lines[30..31]), [('?', 0, 0, ELIDED)]);
        assert_eq!(shown(&lines[29..30]), [(' ', 36, 36, "line 36")]);
        assert_eq!(shown(&lines[59..]), [(' ', 90, 90, "line 90")]);
    }

    #[test]
    fn the_outline_keeps_a_definition_deleted_whole() {
        let new = "impl Store {\n    fn open() {}\n}\n";
        let old = "impl Store {\n    fn open() {}\n    fn close() {}\n}\n";
        let new = Definitions::of(new, Some(Language::Rust));
        let old = Definitions::of(old, Some(Language::Rust));
        let outline = outline(
            new.outline(&[highlight::LineRange::line(2)]),
            old.outline(&[highlight::LineRange::line(3)]),
        );
        let labels: Vec<(&str, Option<u32>)> = outline
            .iter()
            .map(|definition| (definition.label.as_str(), definition.parent))
            .collect();
        assert_eq!(
            labels,
            [
                ("impl Store", None),
                ("fn open", Some(0)),
                ("fn close", Some(0))
            ]
        );
    }
}
