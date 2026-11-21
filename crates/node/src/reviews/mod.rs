//! The reviews experiment on the agent's host: a person's review of one of
//! this host's agents' working trees, round by round. Opening it takes a
//! snapshot when the tree has moved and no drafts wait, and answers what the
//! snapshot holds; editing puts, deletes and discards comments, or starts the
//! review over. The rows are the core's, written through it; the snapshots
//! are git's, in a store in the agent's directory.
//!
//! Only a person reaches these, for an own agent: the service checks both
//! before it calls in (`crate::api`).
//!
//! One call at a time works on an agent's review. A second Open waits for
//! the first and then finds the tree as the first's snapshot left it, so two
//! opens at once take one snapshot, and an edit never lands between a
//! refresh reading the drafts and moving them.

pub(crate) mod cleanup;
pub(crate) mod files;
mod place;

use std::collections::{BTreeSet, HashMap};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, Weak};
use std::time::{Duration, SystemTime};

use daemon_core::payload::{CallError, ClientRequest, JobResult, Response, Status as Ended};
use daemon_core::reviews::{Change, Put, Refusal, Write, Written};
use git_facts::compare::{self, CompareError, Shown};
use git_facts::objects::ObjectStore;
use git_facts::snapshot::{self, Limits};
use git_facts::{GitError, UsedBase};
use highlight::Language;
use prost::Message as _;
use store::AgentRow;
use store::reviews::{Review, Taken};
use tonic::Status;
use wire::experimental::{
    REVIEW_ROW, ReviewAnchor, ReviewEditRequest, ReviewLineRange, ReviewOpenRequest, ReviewRound,
    ReviewSummary, ReviewerComment, review_edit,
};
use wire::{ChangedFile, ErrorCode, FileFlag, FileStatus};

use self::place::{Maps, Views};
use crate::api::Caller;
use crate::driver::{Ask, Delivered};
use crate::grpc::{status, wire_error};
use crate::runtime::{AgentId, ProfileRuntime, core_agent};
use crate::spec;

/// The most comments a review holds, drafts and sent together: far past
/// what a person writes on one change, and short of a summary too large to
/// carry.
pub(crate) const MAX_COMMENTS: usize = 500;

/// The most bytes of words one comment holds.
pub(crate) const MAX_WORDS: usize = 16 * 1024;

/// The most bytes a review's comments take encoded, words, snippets and
/// scopes together: what keeps a summary well inside one message a channel
/// carries, whatever the comments hold. Checked before an edit is made.
pub(crate) const MAX_COMMENT_BYTES: usize = 4 << 20;

/// The most bytes a client's id for a comment takes; clients make UUIDs.
const MAX_COMMENT_ID: usize = 64;

/// Reviews1.Open: the review of `row`'s agent as `caller` opens it.
pub(crate) async fn open(
    runtime: &Arc<ProfileRuntime>,
    row: AgentRow,
    request: ReviewOpenRequest,
    _caller: &Caller,
) -> Result<ReviewSummary, Status> {
    if request.base_branch.as_deref() == Some("") {
        return Err(refused(
            ErrorCode::InvalidArgument,
            "a base branch to compare with has a name",
        ));
    }
    if request.read_only {
        if request.base_branch.is_some() || request.refresh {
            return Err(refused(
                ErrorCode::InvalidArgument,
                "a read-only open neither picks a base nor refreshes",
            ));
        }
        return read_only(runtime, &row).await;
    }
    let tree = Tree::of(runtime, &row).await?;
    let turn = tree.turn();
    let _turn = turn.lock().await;
    let review = tree.review(runtime).await?;

    let mut changes = Vec::new();
    let (chosen, began_at) = match &review {
        None => {
            let head = tree.head().await?;
            changes.push(Change::Begin {
                review: uuid::Uuid::new_v4().as_bytes().to_vec(),
                base_branch: request.base_branch.clone().unwrap_or_default(),
                began_at: head.clone().unwrap_or_default(),
                common_dir: tree.common_dir().await?,
            });
            (request.base_branch.clone(), head)
        }
        Some(review) => {
            if let Some(branch) = &request.base_branch
                && *branch != review.base_branch
            {
                changes.push(Change::Base {
                    base_branch: branch.clone(),
                });
            }
            let chosen = request
                .base_branch
                .clone()
                .or_else(|| Some(review.base_branch.clone()).filter(|branch| !branch.is_empty()));
            let began_at = Some(review.began_at.clone()).filter(|commit| !commit.is_empty());
            (chosen, began_at)
        }
    };
    // The base is found anew only here, when a person opens the review: a
    // snapshot taken now records it, and the latest is compared against it
    // from now on. Every other read uses what a snapshot recorded.
    let base = base(
        runtime,
        tree.agent,
        &tree.root,
        chosen.as_deref(),
        began_at.as_deref(),
    )
    .await?;

    // A snapshot is taken on a first look, and when the tree has moved
    // since the latest one and no drafts wait on it, or a refresh moves
    // them on.
    let latest = review.as_ref().and_then(Review::latest).cloned();
    let drafts = review.as_ref().map_or(0, |review| review.drafts().count());
    let moved = match &latest {
        Some(latest) => snapshot::moved(
            &tree.root,
            &tree.store,
            &Limits::default(),
            &latest.commit,
            &latest.omitted,
        )
        .await
        .map_err(git_refused)?,
        None => true,
    };
    let take = latest.is_none() || (moved && (drafts == 0 || request.refresh));
    let mut moved_drafts = 0;
    if take {
        let taken_at = SystemTime::UNIX_EPOCH
            + Duration::from_millis(u64::try_from(runtime.clock_now()).unwrap_or(0));
        let taken = snapshot::take(&tree.root, &tree.store, &Limits::default(), taken_at)
            .await
            .map_err(git_refused)?;
        let onto = taken.commit.clone();
        changes.push(Change::Snapshot {
            commit: taken.commit,
            base_commit: base.commit.clone(),
            base_name: base.branch.clone(),
            omitted: taken.omitted,
            merging: taken.merging,
        });
        if let (Some(review), Some(latest)) = (&review, &latest)
            && drafts > 0
        {
            let maps = Maps::read(
                &tree.root,
                &tree.store,
                &onto,
                review.drafts().map(|draft| &draft.anchor),
            )
            .await
            .map_err(compare_refused)?;
            let compared = compared(review, &base.commit);
            let moving = place::moved_drafts(review, &latest.commit, &onto, &compared, &maps);
            moved_drafts = moving.len() as u32;
            if !moving.is_empty() {
                changes.push(Change::DraftsMoved { drafts: moving });
            }
        }
    } else if let Some(latest) = &latest {
        changes.push(Change::Rebased {
            snapshot: latest.commit.clone(),
            base_commit: base.commit.clone(),
            base_name: base.branch.clone(),
        });
        // Waiting drafts keep the page on their snapshot; the row says
        // the tree has moved under it.
        let told = REVIEW_ROW
            .get(&row.experimental)
            .is_some_and(|entry| entry.moved);
        if told != moved {
            changes.push(Change::Moved {
                snapshot: latest.commit.clone(),
                moved,
            });
        }
    }

    let review = match changes.is_empty() {
        true => review,
        false => write(runtime, tree.agent, changes).await?.review,
    };
    let review = review.ok_or_else(|| {
        refused(
            ErrorCode::Aborted,
            "the review was started over while it was opened",
        )
    })?;
    if take {
        tree.drop_unkept(&review).await;
    }
    summary(&tree, &review, moved && !take, moved_drafts).await
}

/// A read-only Open: the review as it stands, changing nothing, and an
/// empty summary for an agent with none. It waits for no other call on the
/// review, so a client following the row is never held behind a snapshot
/// another is taking; what it reads is one store read, whole. The base is
/// named as it was when the snapshot was shown against it, so a base
/// branch deleted or rewritten since never stops the review being read.
async fn read_only(runtime: &Arc<ProfileRuntime>, row: &AgentRow) -> Result<ReviewSummary, Status> {
    let tree = Tree::of(runtime, row).await?;
    let Some(review) = tree.review(runtime).await? else {
        return Ok(tree.empty());
    };
    if review.latest().is_none() {
        return Ok(tree.empty());
    }
    let stale = REVIEW_ROW
        .get(&row.experimental)
        .is_some_and(|entry| entry.moved);
    summary(&tree, &review, stale, 0).await
}

/// Reviews1.Edit: `caller`'s edits to the review of `row`'s agent, all of
/// them or none.
pub(crate) async fn edit(
    runtime: &Arc<ProfileRuntime>,
    row: AgentRow,
    request: ReviewEditRequest,
    _caller: &Caller,
) -> Result<ReviewSummary, Status> {
    let tree = Tree::of(runtime, &row).await?;
    let turn = tree.turn();
    let _turn = turn.lock().await;
    let Some(review) = tree.review(runtime).await? else {
        return without_review(&tree, &request);
    };
    if review.latest().is_none() {
        return Err(refused(ErrorCode::Internal, "the review holds no snapshot"));
    }
    let handed = handed_out(&review);

    // Every edit is checked, and what the review would hold counted,
    // before any snippet is read.
    let mut held: BTreeSet<&[u8]> = review
        .comments
        .iter()
        .map(|comment| comment.id.as_slice())
        .collect();
    let mut drafts: BTreeSet<&[u8]> = review
        .drafts()
        .map(|comment| comment.id.as_slice())
        .collect();
    let mut started_over = false;
    for edit in &request.edits {
        match &edit.of {
            None => {
                return Err(refused(
                    ErrorCode::InvalidArgument,
                    "an edit says what it does",
                ));
            }
            Some(review_edit::Of::Put(put)) => {
                let anchor = checked(&put.id, put.anchor.as_ref(), &put.text)?;
                let unmoved = review
                    .comments
                    .iter()
                    .any(|comment| comment.id == put.id && same_anchor(&comment.anchor, anchor));
                if !unmoved && !handed.contains(anchor.commit.as_str()) {
                    return Err(refused(
                        ErrorCode::FailedPrecondition,
                        "the comment's lines are in a commit the review no longer shows; \
                         open the review again",
                    ));
                }
                if held.insert(&put.id) {
                    drafts.insert(&put.id);
                }
            }
            Some(review_edit::Of::Delete(comment)) => {
                held.remove(comment.as_slice());
                drafts.remove(comment.as_slice());
            }
            Some(review_edit::Of::DiscardDrafts(_)) => {
                for draft in std::mem::take(&mut drafts) {
                    held.remove(draft);
                }
            }
            Some(review_edit::Of::StartOver(_)) => {
                held.clear();
                drafts.clear();
                started_over = true;
            }
        }
    }
    if held.len() > MAX_COMMENTS {
        return Err(too_much(format!(
            "a review holds at most {MAX_COMMENTS} comments"
        )));
    }

    let held: BTreeSet<Vec<u8>> = held.into_iter().map(<[u8]>::to_vec).collect();
    let mut changes = Vec::with_capacity(request.edits.len());
    // What each comment the review would hold takes, by id: the review's
    // own, replaced by each put as it comes.
    let mut sizes: HashMap<Vec<u8>, usize> = review
        .comments
        .iter()
        .map(|comment| {
            (
                comment.id.clone(),
                comment_bytes(&comment.text, &comment.snippet, &comment.scope),
            )
        })
        .collect();
    for of in request.edits.into_iter().filter_map(|edit| edit.of) {
        changes.push(match of {
            review_edit::Of::Put(put) => {
                let anchor = put.anchor.unwrap_or_default();
                // Words changed on lines already read keep what was read.
                let unmoved = review
                    .comments
                    .iter()
                    .find(|comment| comment.id == put.id && same_anchor(&comment.anchor, &anchor));
                let (snippet, scope) = match unmoved {
                    Some(comment) => (comment.snippet.clone(), comment.scope.clone()),
                    None => files::snippet(runtime, &row, &review, &anchor).await?,
                };
                sizes.insert(put.id.clone(), comment_bytes(&put.text, &snippet, &scope));
                Change::Put(Box::new(Put {
                    comment: put.id,
                    anchor: core_anchor(&anchor),
                    text: put.text,
                    snippet,
                    scope,
                }))
            }
            review_edit::Of::Delete(comment) => {
                sizes.remove(&comment);
                Change::Delete { comment }
            }
            review_edit::Of::DiscardDrafts(_) => {
                sizes.retain(|id, _| !review.drafts().any(|draft| draft.id == *id));
                Change::DiscardDrafts
            }
            review_edit::Of::StartOver(_) => {
                sizes.clear();
                Change::StartOver
            }
        });
    }
    let total: usize = sizes
        .iter()
        .filter(|(id, _)| held.contains(*id))
        .map(|(_, bytes)| bytes)
        .sum();
    if total > MAX_COMMENT_BYTES {
        return Err(too_much(format!(
            "a review's comments take at most {} MiB, their code with them",
            MAX_COMMENT_BYTES >> 20
        )));
    }

    let written = match changes.is_empty() {
        true => None,
        false => Some(write(runtime, tree.agent, changes).await?),
    };
    let (review, entry) = match written {
        Some(written) => (written.review, written.entry),
        None => (Some(review), REVIEW_ROW.get(&row.experimental)),
    };
    let Some(review) = review else {
        if started_over {
            tree.remove_store();
        }
        return Ok(tree.empty());
    };
    // The turn-end check keeps the row's word on whether the tree moved;
    // an edit does not look again, nor at the base branch: what the edit
    // made is answered from the store alone, so a save that landed is
    // never reported failed.
    let stale = entry.is_some_and(|entry| entry.moved);
    summary(&tree, &review, stale, 0).await
}

/// What one comment takes in a summary, encoded.
fn comment_bytes(
    text: &str,
    snippet: &[wire::experimental::ReviewLine],
    scope: &[wire::experimental::ReviewDefinition],
) -> usize {
    text.len()
        + snippet
            .iter()
            .map(|line| line.encoded_len() + 2)
            .sum::<usize>()
        + scope
            .iter()
            .map(|definition| definition.encoded_len() + 2)
            .sum::<usize>()
        + MAX_COMMENT_ID
}

/// An Edit of an agent with no review: deleting and discarding find
/// nothing to remove, starting over has nothing to start over, and a
/// comment has no review to go in.
fn without_review(tree: &Tree, request: &ReviewEditRequest) -> Result<ReviewSummary, Status> {
    let puts = request
        .edits
        .iter()
        .any(|edit| matches!(edit.of, Some(review_edit::Of::Put(_))));
    if puts {
        return Err(refused(
            ErrorCode::FailedPrecondition,
            "the agent has no review to comment in; open it first",
        ));
    }
    if request
        .edits
        .iter()
        .any(|edit| matches!(edit.of, Some(review_edit::Of::StartOver(_))))
    {
        tree.remove_store();
    }
    Ok(tree.empty())
}

/// The anchor of a comment a client puts, checked: an id within bounds,
/// words within bounds, a path, and lines from 1 or the whole file.
fn checked<'a>(
    id: &[u8],
    anchor: Option<&'a ReviewAnchor>,
    text: &str,
) -> Result<&'a ReviewAnchor, Status> {
    let invalid = |words: String| Err(refused(ErrorCode::InvalidArgument, words));
    if id.is_empty() || id.len() > MAX_COMMENT_ID {
        return invalid(format!(
            "a comment's id is 1 to {MAX_COMMENT_ID} bytes long"
        ));
    }
    // A cap, answered as every cap on what a review holds is.
    if text.len() > MAX_WORDS {
        return Err(too_much(format!(
            "a comment holds at most {MAX_WORDS} bytes of words"
        )));
    }
    let Some(anchor) = anchor.filter(|anchor| !anchor.commit.is_empty() && !anchor.path.is_empty())
    else {
        return invalid("a comment names the commit and file its lines are in".to_owned());
    };
    let lines = anchor.lines.unwrap_or_default();
    if lines.start == 0 && lines.end != 0 {
        return invalid("a comment's lines start from line 1, or are the whole file".to_owned());
    }
    if lines.start > 0 && lines.end != 0 && lines.end < lines.start {
        return invalid("a comment's lines end where they start or after".to_owned());
    }
    Ok(anchor)
}

/// A cap on what a review holds was reached.
fn too_much(words: String) -> Status {
    refused(ErrorCode::ResourceExhausted, words)
}

fn core_anchor(anchor: &ReviewAnchor) -> daemon_core::reviews::Anchor {
    let lines = anchor.lines.unwrap_or_default();
    daemon_core::reviews::Anchor {
        commit: anchor.commit.clone(),
        path: anchor.path.clone(),
        start: lines.start,
        end: match lines.start {
            0 => 0,
            start => lines.end.max(start),
        },
    }
}

fn same_anchor(stored: &store::reviews::Anchor, anchor: &ReviewAnchor) -> bool {
    let anchor = core_anchor(anchor);
    stored.commit == anchor.commit
        && stored.path == anchor.path
        && stored.start == anchor.start
        && stored.end == anchor.end
}

/// Every commit the review has handed a client lines of: each snapshot it
/// keeps, and every base commit each has been shown against. A comment is
/// put on one of these; GetFiles diffs between them.
fn handed_out(review: &Review) -> BTreeSet<&str> {
    review
        .snapshots
        .iter()
        .flat_map(|taken| {
            std::iter::once(taken.commit.as_str())
                .chain(std::iter::once(taken.base_commit.as_str()))
                .chain(taken.bases.iter().map(String::as_str))
        })
        .collect()
}

/// The commits a view of `review`'s latest snapshot compares it against:
/// `base`, the commit its all changes run from, and the last round's
/// snapshot for what changed since it.
fn compared<'a>(review: &'a Review, base: &'a str) -> Vec<&'a str> {
    std::iter::once(base)
        .chain(review.rounds.last().map(|round| round.snapshot.as_str()))
        .collect()
}

/// The last round as since-last compares against it: its snapshot and the
/// base that snapshot ran from.
fn last_shown(review: &Review) -> Option<Shown<'_>> {
    review.rounds.last().map(|round| Shown {
        base: &round.base_commit,
        snapshot: &round.snapshot,
    })
}

/// The review as of its latest snapshot, its comments placed there, its
/// base named as it was found with the base commit.
async fn summary(
    tree: &Tree,
    review: &Review,
    stale: bool,
    moved_drafts: u32,
) -> Result<ReviewSummary, Status> {
    let latest = review
        .latest()
        .ok_or_else(|| refused(ErrorCode::Internal, "the review holds no snapshot"))?;
    let snapshot = latest.commit.as_str();
    let (root, store) = (tree.root.as_path(), &tree.store);

    let wire_files = listed(root, store, latest, &latest.base_commit).await?;
    let since_last = match last_shown(review) {
        Some(last) => {
            let now = Shown {
                base: &latest.base_commit,
                snapshot: &latest.commit,
            };
            match listed_since(root, store, last, now, &latest.omitted, &wire_files).await {
                Ok(since) => since,
                // The last round's snapshot can no longer be read: nothing
                // is said of what changed since it.
                Err(status) if status.code() == tonic::Code::DataLoss => Vec::new(),
                Err(status) => return Err(status),
            }
        }
        None => Vec::new(),
    };

    let compared = compared(review, &latest.base_commit);
    let maps = Maps::read(
        root,
        store,
        snapshot,
        review.comments.iter().map(|comment| &comment.anchor),
    )
    .await
    .map_err(compare_refused)?;
    let views = Views {
        target: snapshot,
        compared: &compared,
    };
    let comments = review
        .comments
        .iter()
        .zip(place::placements(review, &views, &maps))
        .map(|(comment, placement)| ReviewerComment {
            id: comment.id.clone(),
            anchor: Some(ReviewAnchor {
                commit: comment.anchor.commit.clone(),
                path: comment.anchor.path.clone(),
                lines: Some(ReviewLineRange {
                    start: comment.anchor.start,
                    end: comment.anchor.end,
                }),
            }),
            text: comment.text.clone(),
            snippet: comment.snippet.clone(),
            scope: comment.scope.clone(),
            round: comment.round,
            placement: Some(placement),
            revision: comment.revision,
            sent_revision: comment.sent_revision,
        })
        .collect();

    Ok(ReviewSummary {
        id: review.id.clone(),
        base_branch: latest.base_name.clone(),
        base_commit: latest.base_commit.clone(),
        snapshot: snapshot.to_owned(),
        stale,
        rounds: review
            .rounds
            .iter()
            .map(|round| ReviewRound {
                snapshot: round.snapshot.clone(),
                base_commit: round.base_commit.clone(),
                input_id: round.input_id.clone(),
            })
            .collect(),
        files: wire_files,
        since_last,
        comments,
        merge_in_progress: latest.merging,
        objects: tree.store.dir().to_string_lossy().into_owned(),
        root: tree.root.to_string_lossy().into_owned(),
        moved_drafts,
        index: tree.index_of(snapshot).await,
    })
}

/// Every file that changed from `from` to the snapshot `taken`, as the
/// summary lists all changes and GetFiles lists a comparison: renames
/// followed, flagged, each with its language; a file the snapshot left out
/// for its size listed as large.
pub(super) async fn listed(
    root: &Path,
    store: &ObjectStore,
    taken: &Taken,
    from: &str,
) -> Result<Vec<ChangedFile>, Status> {
    let snapshot = taken.commit.as_str();
    let mut files = compare::files(root, store, from, snapshot)
        .await
        .map_err(compare_refused)?;
    // A file left out of the snapshot for its size is listed as large: its
    // change is not in the snapshot to be counted or shown.
    let listed: BTreeSet<String> = files.iter().map(|file| file.path.clone()).collect();
    for path in &taken.omitted {
        if !listed.contains(path) {
            let at_base = compare::read(root, store, from, path)
                .await
                .is_ok_and(|bytes| bytes.is_some());
            files.push(git_facts::ChangedFile {
                path: path.clone(),
                old_path: None,
                status: match at_base {
                    true => git_facts::Status::Changed,
                    false => git_facts::Status::Created,
                },
                added: 0,
                removed: 0,
                binary: false,
                flags: git_facts::Flags::default(),
            });
        }
    }
    for file in &mut files {
        if taken.omitted.contains(&file.path) {
            file.flags.large = true;
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let languages = languages(root, store, snapshot, &files).await;
    Ok(files
        .into_iter()
        .zip(languages)
        .map(|(file, language)| changed_file(file, language))
        .collect())
}

/// Every file that changed since the `last` round to `now`, a snapshot
/// shown against its base, limited as since-last's hunks are and counted
/// from them: renames followed, flagged as all changes `all` flags them. A
/// file the agent put back as it was, or made and then deleted, is here
/// though all changes no longer lists it; one the snapshot left out for its
/// size, `omitted`, is not.
pub(super) async fn listed_since(
    root: &Path,
    store: &ObjectStore,
    last: Shown<'_>,
    now: Shown<'_>,
    omitted: &[String],
    all: &[ChangedFile],
) -> Result<Vec<ChangedFile>, Status> {
    let since = match compare::since_last(root, store, last, now, None, files::HUNK_CONTEXT).await {
        Ok(since) => since,
        Err(error) => return Err(compare_refused(error)),
    };
    let flagged: HashMap<&str, &ChangedFile> =
        all.iter().map(|file| (file.path.as_str(), file)).collect();
    let mut listed = Vec::with_capacity(since.len());
    // Not in all changes, so not flagged: one the agent put back is shown
    // open, as the undo it is. Its language is read with the others'.
    let mut unlisted = Vec::new();
    for file in since {
        if omitted.contains(&file.path) {
            continue;
        }
        let (added, removed) = file.counts();
        let changed = git_facts::ChangedFile {
            path: file.path.clone(),
            old_path: file.old_path.clone(),
            status: file.status,
            added,
            removed,
            binary: file.binary,
            flags: git_facts::Flags::default(),
        };
        match flagged.get(file.path.as_str()) {
            Some(all) => listed.push(ChangedFile {
                flags: all.flags.clone(),
                ..changed_file(changed, all.language.clone())
            }),
            None => unlisted.push(changed),
        }
    }
    let languages = languages(root, store, now.snapshot, &unlisted).await;
    listed.extend(
        unlisted
            .into_iter()
            .zip(languages)
            .map(|(file, language)| changed_file(file, language)),
    );
    listed.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(listed)
}

/// The language each of `files` is coloured by, in order, by the one rule
/// every client uses: its path, then for a script its `#!` line, read from
/// the snapshot. A summary is made on every open, edit and list, so the
/// `#!` lines of every file its path does not name are read with one git
/// process, and what they said is kept by snapshot and path, which name
/// the same bytes for good: a summary of a snapshot read before runs none.
async fn languages(
    root: &Path,
    store: &ObjectStore,
    snapshot: &str,
    files: &[git_facts::ChangedFile],
) -> Vec<String> {
    let mut named: Vec<Option<String>> = files
        .iter()
        .map(|file| {
            if let Some(language) = Language::from_path(&file.path) {
                return Some(language.name().to_owned());
            }
            let readable =
                !file.binary && !file.flags.large && file.status != git_facts::Status::Deleted;
            match readable {
                true => sniffed(snapshot, &file.path),
                false => Some(String::new()),
            }
        })
        .collect();
    let unread: Vec<&str> = files
        .iter()
        .zip(&named)
        .filter(|(_, language)| language.is_none())
        .map(|(file, _)| file.path.as_str())
        .collect();
    if !unread.is_empty() {
        // A file that cannot be read is drawn plain, as one with no `#!`
        // line is, and asked about again by the next summary.
        if let Ok(heads) =
            compare::heads(root, store, snapshot, &unread, SHEBANG_BYTES).await
        {
            let read: HashMap<&str, String> = unread
                .iter()
                .zip(heads)
                .map(|(path, head)| {
                    let language = head
                        .and_then(|head| {
                            Language::from_file(path, &String::from_utf8_lossy(&head))
                        })
                        .map_or_else(String::new, |language| language.name().to_owned());
                    remember_sniffed(snapshot, path, &language);
                    (*path, language)
                })
                .collect();
            for (file, language) in files.iter().zip(&mut named) {
                if language.is_none() {
                    *language = read.get(file.path.as_str()).cloned();
                }
            }
        }
    }
    named.into_iter().map(Option::unwrap_or_default).collect()
}

/// How much of a file's start holds the `#!` line it is named by.
const SHEBANG_BYTES: usize = 256;

/// The most files whose `#!` lines [`languages`] keeps what it read of: a
/// few large changes' worth, a few hundred kilobytes at most.
const SNIFFED_FILES: usize = 4096;

/// What [`languages`] read of files their paths name no language for, by
/// snapshot and path.
static SNIFFED: LazyLock<Mutex<HashMap<(String, String), String>>> =
    LazyLock::new(Default::default);

/// The language the `#!` line of `path` at `snapshot` named, when read
/// before.
fn sniffed(snapshot: &str, path: &str) -> Option<String> {
    SNIFFED
        .lock()
        .unwrap()
        .get(&(snapshot.to_owned(), path.to_owned()))
        .cloned()
}

fn remember_sniffed(snapshot: &str, path: &str, language: &str) {
    let mut sniffed = SNIFFED.lock().unwrap();
    // Past the bound it starts afresh: the next summaries read again.
    if sniffed.len() >= SNIFFED_FILES {
        sniffed.clear();
    }
    sniffed.insert((snapshot.to_owned(), path.to_owned()), language.to_owned());
}

fn changed_file(file: git_facts::ChangedFile, language: String) -> ChangedFile {
    let status = match file.status {
        git_facts::Status::Changed => FileStatus::Changed,
        git_facts::Status::Created => FileStatus::Created,
        git_facts::Status::Deleted => FileStatus::Deleted,
        git_facts::Status::Renamed => FileStatus::Renamed,
    };
    let flags = [
        (file.flags.generated, FileFlag::Generated),
        (file.flags.lockfile, FileFlag::Lockfile),
        (file.flags.vendored, FileFlag::Vendored),
        (file.flags.large, FileFlag::Large),
    ]
    .into_iter()
    .filter_map(|(set, flag)| set.then_some(flag as i32))
    .collect();
    ChangedFile {
        path: file.path,
        old_path: file.old_path,
        status: status as i32,
        added: file.added,
        removed: file.removed,
        binary: file.binary,
        language,
        flags,
    }
}

/// The base a review's all changes run from.
pub(super) struct Base {
    /// As the summary names it: on the base branch itself, the upstream it
    /// ran from; else the base branch; empty when none was found.
    pub(super) branch: String,
    /// The commit, or before the first commit the empty tree.
    pub(super) commit: String,
}

/// The base all changes run from, by the review's rule: the base branch
/// `chosen`, else the one the agent's spec recorded, else the repository's
/// default; on a base branch with no upstream, `began_at`, the commit HEAD
/// was at when the review began; before the first commit, the empty tree.
async fn base(
    runtime: &ProfileRuntime,
    agent: AgentId,
    cwd: &Path,
    chosen: Option<&str>,
    began_at: Option<&str>,
) -> Result<Base, Status> {
    let recorded = match spec::newest(&runtime.agent_dir(agent)) {
        Ok(spec) => spec.and_then(|spec| spec.base_branch),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(refused(
                ErrorCode::Internal,
                format!("reading the agent's spec: {error}"),
            ));
        }
    };
    let UsedBase {
        branch,
        upstream,
        commit,
    } = compare::base_for_review(cwd, recorded.as_deref(), chosen, began_at)
        .await
        .map_err(git_refused)?;
    let commit = match commit {
        Some(commit) => commit,
        None => compare::empty_tree(cwd).await.map_err(git_refused)?,
    };
    Ok(Base {
        branch: upstream.or(branch).unwrap_or_default(),
        commit,
    })
}

/// An own agent's working tree and review store, as one call uses them.
struct Tree {
    agent: AgentId,
    dir: PathBuf,
    /// The repository's top folder, where every git call runs.
    root: PathBuf,
    store: ObjectStore,
}

impl Tree {
    /// The tree of `row`'s agent; refused when its folder is gone or is not
    /// a repository.
    async fn of(runtime: &ProfileRuntime, row: &AgentRow) -> Result<Self, Status> {
        let agent = AgentId::from_slice(&row.agent.agent)
            .map_err(|_| refused(ErrorCode::InvalidArgument, "an agent id is 16 bytes"))?;
        let cwd = PathBuf::from(&row.cwd);
        if !cwd.is_dir() {
            return Err(refused(
                ErrorCode::FailedPrecondition,
                "the agent's folder is gone, so it has nothing to review",
            ));
        }
        let root = rev_parse(&cwd, &["--show-toplevel"])
            .await?
            .map(PathBuf::from)
            .ok_or_else(|| git_refused(GitError::NotARepository))?;
        let dir = runtime.agent_dir(agent);
        let store = ObjectStore::at(daemon_core::reviews::objects(&dir));
        Ok(Self {
            agent,
            dir,
            root,
            store,
        })
    }

    /// What one call at a time on this agent's review holds.
    fn turn(&self) -> Arc<tokio::sync::Mutex<()>> {
        type Turns = Mutex<HashMap<PathBuf, Weak<tokio::sync::Mutex<()>>>>;
        static TURNS: LazyLock<Turns> = LazyLock::new(Default::default);
        let mut turns = TURNS.lock().unwrap();
        turns.retain(|_, turn| turn.strong_count() > 0);
        if let Some(turn) = turns.get(&self.dir).and_then(Weak::upgrade) {
            return turn;
        }
        let turn = Arc::new(tokio::sync::Mutex::new(()));
        turns.insert(self.dir.clone(), Arc::downgrade(&turn));
        turn
    }

    async fn review(&self, runtime: &ProfileRuntime) -> Result<Option<Review>, Status> {
        let agent = self.agent.as_bytes().to_vec();
        runtime
            .read_rows(move |store| store.review(&agent))
            .await
            .map_err(|error| refused(ErrorCode::Internal, format!("the store: {error}")))
    }

    /// The commit HEAD is at; none before the first commit.
    async fn head(&self) -> Result<Option<String>, Status> {
        rev_parse(&self.root, &["--verify", "--quiet", "HEAD^{commit}"]).await
    }

    async fn common_dir(&self) -> Result<String, Status> {
        rev_parse(&self.root, &["--path-format=absolute", "--git-common-dir"])
            .await?
            .ok_or_else(|| git_refused(GitError::NotARepository))
    }

    /// Removes from the store every snapshot `review` no longer keeps, and
    /// the index of each. A failure leaves them, to go with the next.
    async fn drop_unkept(&self, review: &Review) {
        let keep = review.snapshots.iter().map(|taken| taken.commit.as_str());
        if let Err(error) = snapshot::drop(&self.root, &self.store, keep).await {
            tracing::warn!(%error, "dropping a review's unkept snapshots failed");
        }
        let kept: BTreeSet<&str> = review
            .snapshots
            .iter()
            .map(|taken| taken.commit.as_str())
            .collect();
        let Ok(indexes) = std::fs::read_dir(self.indexes()) else {
            return;
        };
        for entry in indexes.flatten() {
            let name = entry.file_name();
            if !kept.contains(name.to_string_lossy().as_ref()) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }

    /// The folder holding an index of each snapshot.
    fn indexes(&self) -> PathBuf {
        self.dir
            .join(daemon_core::reviews::REVIEW)
            .join(daemon_core::reviews::INDEXES)
    }

    /// The index of `snapshot`, made now if it is not there yet: what the
    /// agent's diff command reads the snapshot's files through, so that
    /// git compares the working tree with the snapshot, untracked files
    /// and all, without an index of the repository's own. Empty when it
    /// cannot be made, and the agent's text then names none.
    async fn index_of(&self, snapshot: &str) -> String {
        let index = self.indexes().join(snapshot);
        if index.is_file() {
            return index.to_string_lossy().into_owned();
        }
        if let Err(error) = std::fs::create_dir_all(self.indexes()) {
            tracing::warn!(%error, "making a review's index folder failed");
            return String::new();
        }
        // Written under another name and moved into place, so a diff the
        // agent runs meanwhile never reads half an index.
        let making = self
            .indexes()
            .join(format!(".{snapshot}.{}", uuid::Uuid::new_v4()));
        let made = self
            .store
            .reading(&self.root, &["read-tree", snapshot])
            .env("GIT_INDEX_FILE", &making)
            .output()
            .await;
        match made {
            Ok(output) if output.status.success() => {}
            Ok(output) => {
                tracing::warn!(
                    stderr = %String::from_utf8_lossy(&output.stderr),
                    "making a snapshot's index failed"
                );
                let _ = std::fs::remove_file(&making);
                return String::new();
            }
            Err(error) => {
                tracing::warn!(%error, "making a snapshot's index failed");
                return String::new();
            }
        }
        match std::fs::rename(&making, &index) {
            Ok(()) => index.to_string_lossy().into_owned(),
            Err(error) => {
                tracing::warn!(%error, "making a snapshot's index failed");
                let _ = std::fs::remove_file(&making);
                String::new()
            }
        }
    }

    /// Removes the review's folder, its snapshot store with it: nothing
    /// names its objects once the review is gone. Never follows a link out
    /// of the agent's directory.
    fn remove_store(&self) {
        if let Err(error) = cleanup::remove(&self.dir) {
            tracing::warn!(%error, "removing a review's snapshot store failed");
        }
    }

    /// The answer for an agent with no review: only where it would be.
    fn empty(&self) -> ReviewSummary {
        ReviewSummary {
            objects: self.store.dir().to_string_lossy().into_owned(),
            root: self.root.to_string_lossy().into_owned(),
            ..ReviewSummary::default()
        }
    }
}

/// What `git rev-parse args` answers in `cwd`; none when git refuses.
async fn rev_parse(cwd: &Path, args: &[&str]) -> Result<Option<String>, Status> {
    let output = tokio::process::Command::new("git")
        .args(["-c", "core.fsmonitor=false", "rev-parse"])
        .args(args)
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|error| git_refused(GitError::Spawn(error)))?;
    let answer = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    Ok((output.status.success() && !answer.is_empty()).then_some(answer))
}

/// Makes `changes` to the review of the own agent `agent` through the
/// core, all of them or none.
async fn write(
    runtime: &ProfileRuntime,
    agent: AgentId,
    changes: Vec<Change>,
) -> Result<Written, Status> {
    let request = ClientRequest::Review(Box::new(Write {
        agent: core_agent(agent),
        changes,
    }));
    let mut answers = runtime.driver().call(Ask::Ready(request), 1).await;
    let answered = match answers.next().await {
        Some(Delivered::Answer(Response::Review(written))) => *written,
        Some(Delivered::Answer(other)) => {
            return Err(refused(
                ErrorCode::Internal,
                format!("a review write was answered with {other:?}"),
            ));
        }
        Some(Delivered::End(Ended::Failed(error))) => return Err(core_refused(error)),
        Some(Delivered::End(Ended::Ok)) | None => {
            return Err(refused(
                ErrorCode::Unavailable,
                "the profile is no longer running",
            ));
        }
    };
    // Its end follows its answer at once.
    let _ = answers.next().await;
    Ok(answered)
}

/// Carries out a reviews job of the core's; none when the profile has
/// stopped.
pub(crate) async fn run_job(
    runtime: Weak<ProfileRuntime>,
    job: daemon_core::reviews::Job,
) -> Option<JobResult> {
    runtime.upgrade()?;
    Some(JobResult::Review(carried_out(job).await))
}

async fn carried_out(job: daemon_core::reviews::Job) -> daemon_core::reviews::Done {
    use daemon_core::reviews::{Done, Job};
    match job {
        // Whether the tree moved since the review's latest snapshot,
        // writing nothing: what the row tells every client at a turn end.
        Job::Moved {
            agent,
            cwd,
            objects,
            snapshot,
            omitted,
        } => {
            let store = ObjectStore::at(objects);
            let moved = snapshot::moved(&cwd, &store, &Limits::default(), &snapshot, &omitted)
                .await
                .map_err(|error| error.to_string());
            Done::Moved {
                agent,
                snapshot,
                moved,
            }
        }
    }
}

fn refused(code: ErrorCode, words: impl Into<String>) -> Status {
    status(wire_error(code, words))
}

/// A git failure as the caller is told it. Open's FAILED_PRECONDITION
/// means the folder is no repository, so nothing else answers with it.
fn git_refused(error: GitError) -> Status {
    let code = match &error {
        GitError::NotARepository => ErrorCode::FailedPrecondition,
        // The base branch, picked or recorded, shares no history with the
        // agent's: another can be picked.
        GitError::NoForkPoint(_) => ErrorCode::InvalidArgument,
        // Something in the folder keeps making and deleting files: trying
        // again later may well succeed.
        GitError::Vanished { .. } => ErrorCode::Aborted,
        GitError::TooManyPaths { .. } => ErrorCode::ResourceExhausted,
        GitError::Spawn(_) | GitError::Failed { .. } | GitError::Scratch(_) => ErrorCode::Internal,
    };
    refused(code, error.to_string())
}

/// A comparison's failure, from Open and GetFiles alike: a commit the
/// review needs can no longer be read (its snapshot store removed, the
/// repository cloned again, or a rebase's old commits collected), which is
/// data lost; or git failed, as any git failure is told.
pub(super) fn compare_refused(error: CompareError) -> Status {
    match error {
        CompareError::Missing(_) => refused(
            ErrorCode::DataLoss,
            format!("{error}; start the review over to begin again"),
        ),
        CompareError::Git(error) => git_refused(error),
    }
}

/// The core's refusal of a review write.
fn core_refused(error: CallError) -> Status {
    match error {
        CallError::Review(Refusal::Off) => refused(
            ErrorCode::Unimplemented,
            "this host does not run the reviews experiment",
        ),
        CallError::Review(Refusal::NoReview) => refused(
            ErrorCode::FailedPrecondition,
            "the agent has no review to change; open it first",
        ),
        CallError::Review(Refusal::Begun) => refused(
            ErrorCode::Aborted,
            "the agent's review began meanwhile; open it again",
        ),
        CallError::Review(Refusal::Taken(_)) => refused(
            ErrorCode::AlreadyExists,
            "another review holds a comment with that id",
        ),
        CallError::NoAgent => refused(ErrorCode::NotFound, "no agent with that id"),
        CallError::Store(words) => refused(ErrorCode::Internal, format!("the store: {words}")),
        other => refused(ErrorCode::Internal, format!("{other:?}")),
    }
}

#[cfg(test)]
mod tests {
    use daemon_core::reviews::{Done, Job};

    use super::*;

    fn git(cwd: &Path, args: &[&str]) {
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
            .unwrap();
        assert!(output.status.success(), "git {args:?}: {output:?}");
    }

    async fn moved(work: &Path, objects: &Path, snapshot: &str) -> Result<bool, String> {
        let agent = daemon_core::AgentId([7; 16]);
        let job = Job::Moved {
            agent,
            cwd: work.to_owned(),
            objects: objects.to_owned(),
            snapshot: snapshot.to_owned(),
            omitted: Vec::new(),
        };
        let Done::Moved {
            agent: told,
            snapshot: of,
            moved,
        } = carried_out(job).await;
        assert_eq!((told, of.as_str()), (agent, snapshot));
        moved
    }

    /// A summary is made on every open, edit and list: the `#!` lines of
    /// the files their paths name no language for are read once for a
    /// snapshot, all in one batch, and kept, so the next summary of it reads
    /// none, here with the snapshot store gone from under it.
    #[tokio::test]
    async fn languages_by_shebang_are_read_once_per_snapshot() {
        let root = tempfile::tempdir().unwrap();
        let work = root.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        git(&work, &["init", "-q", "-b", "main"]);
        std::fs::write(work.join("readme.md"), "# hi\n").unwrap();
        git(&work, &["add", "."]);
        git(&work, &["commit", "-q", "-m", "first"]);
        std::fs::write(work.join("tool"), "#!/usr/bin/env python3\nprint(1)\n").unwrap();
        std::fs::write(work.join("run"), "#!/bin/sh\necho hi\n").unwrap();
        std::fs::write(work.join("notes"), "just words\n").unwrap();
        std::fs::write(work.join("lib.rs"), "fn main() {}\n").unwrap();
        let objects = root.path().join("objects");
        let store = ObjectStore::at(&objects);
        let taken = snapshot::take(&work, &store, &Limits::default(), SystemTime::now())
            .await
            .unwrap();
        let changed = |path: &str| git_facts::ChangedFile {
            path: path.to_owned(),
            old_path: None,
            status: git_facts::Status::Created,
            added: 1,
            removed: 0,
            binary: false,
            flags: git_facts::Flags::default(),
        };
        let files: Vec<_> = ["lib.rs", "notes", "run", "tool"]
            .into_iter()
            .map(changed)
            .collect();
        let expected = ["rust", "", "bash", "python"];
        assert_eq!(
            languages(&work, &store, &taken.commit, &files).await,
            expected
        );
        std::fs::remove_dir_all(&objects).unwrap();
        assert_eq!(
            languages(&work, &store, &taken.commit, &files).await,
            expected,
            "read once: the next summary asks git nothing"
        );
    }

    #[tokio::test]
    async fn the_moved_job_tells_whether_the_tree_moved_since_the_snapshot() {
        let root = tempfile::tempdir().unwrap();
        let work = root.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        git(&work, &["init", "-q", "-b", "main"]);
        std::fs::write(work.join("notes.txt"), "one\n").unwrap();
        git(&work, &["add", "."]);
        git(&work, &["commit", "-q", "-m", "first"]);
        let objects = root.path().join("objects");
        let taken = snapshot::take(
            &work,
            &ObjectStore::at(&objects),
            &Limits::default(),
            SystemTime::now(),
        )
        .await
        .unwrap();

        assert_eq!(moved(&work, &objects, &taken.commit).await, Ok(false));
        std::fs::write(work.join("new.txt"), "untracked\n").unwrap();
        assert_eq!(moved(&work, &objects, &taken.commit).await, Ok(true));
        std::fs::remove_file(work.join("new.txt")).unwrap();
        assert_eq!(moved(&work, &objects, &taken.commit).await, Ok(false));
        std::fs::write(work.join("notes.txt"), "one\ntwo\n").unwrap();
        assert_eq!(moved(&work, &objects, &taken.commit).await, Ok(true));

        let elsewhere = root.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        assert!(moved(&elsewhere, &objects, &taken.commit).await.is_err());
    }
}
