//! The reviews experiment (`reviews/1`) in the core: the writes a review of
//! an own agent's working tree makes to the store, each in one step that
//! also rewrites the review's entry on the agent's row, and the check at
//! each turn end of whether the tree has moved since the review's latest
//! snapshot.
//!
//! The work that reads git (taking snapshots, comparing them) is the
//! daemon's, off the core thread; what the core keeps is the review's rows
//! and what every client is told through the row: how many drafts wait, a
//! counter that goes up with every change to the review, and whether the
//! tree has moved since the latest snapshot. A client that read the review
//! at another counter reads it again; one that sees only `moved` change
//! knows the tree changed under the snapshot it shows.
//!
//! A write is all of its changes or none of them: one store transaction,
//! one rise of the counter, one row published.
//!
//! A round closes when the agent takes the message sending it: when the
//! item its prompt or message became is ingested from its journal. A prompt
//! still queued, and so one a person can withdraw, has closed nothing.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use store::reviews::{Anchor as StoredAnchor, Comment, Review, ReviewWriter, Round, Taken};
use wire::experimental::{
    REVIEW_ROW, ReviewDefinition, ReviewLine, ReviewRowEntry, SENT_REVIEW, SentReview,
};

use crate::ids::{AgentId, CallId, JobId};
use crate::io::{Job as CoreJob, Output};
use crate::payload::{CallError, JobResult, Response};
use crate::serve::answered;
use crate::{Daemon, codec};

/// The experiment version whose shapes this module reads and writes.
pub const VERSION: &str = REVIEW_ROW.version();

/// The folder in an own agent's directory that holds its review's data,
/// written by the daemon alone: the snapshot store and an index of each
/// snapshot. Removed with the review, and when the daemon opens the profile
/// without the experiment.
pub const REVIEW: &str = "review";
/// The snapshot store, inside [`REVIEW`].
pub const OBJECTS: &str = "objects";
/// A git index of each snapshot the review keeps, named by its commit,
/// inside [`REVIEW`]: what the agent's diff command reads.
pub const INDEXES: &str = "indexes";

/// The snapshot store of the own agent whose directory is `agent_dir`.
pub fn objects(agent_dir: &Path) -> PathBuf {
    agent_dir.join(REVIEW).join(OBJECTS)
}

/// A write to an own agent's review, as a client's call asks it of the
/// core: [`crate::payload::ClientRequest::Review`]. Answered with
/// [`Response::Review`] once every change is in the store, or refused with
/// none of them made.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Write {
    pub agent: AgentId,
    /// Applied in order.
    pub changes: Vec<Change>,
}

/// One change a [`Write`] makes to a review.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Change {
    /// Begins the agent's review, which has none: refused with
    /// [`Refusal::Begun`] otherwise.
    Begin {
        /// The new review's id.
        review: Vec<u8>,
        base_branch: String,
        /// The commit HEAD is at; empty before the first commit.
        began_at: String,
        /// The repository's common git directory.
        common_dir: String,
    },
    /// Compares the review with another branch from now on.
    Base { base_branch: String },
    /// The snapshot `snapshot` is compared against `base_commit` for all
    /// changes from now on: a person picked another base, or the base rule
    /// found another, without a snapshot being taken.
    Rebased {
        snapshot: String,
        base_commit: String,
    },
    /// A snapshot was taken: it is the latest, the tree has not moved since
    /// it, and once the write's other changes are made the review forgets
    /// every snapshot it no longer keeps (see [`Review::kept`]), which the
    /// answer lists.
    Snapshot {
        commit: String,
        /// The commit its all changes run from, as the base rule found it
        /// when it was taken.
        base_commit: String,
        /// The paths left out of it for their size.
        omitted: Vec<String>,
        /// A merge or rebase was under way.
        merging: bool,
    },
    /// Writes a draft, or changes the words, anchor, snippet and scope of
    /// a comment the review holds, draft or sent, keeping its round and
    /// raising its revision. Refused with [`Refusal::Taken`] when the id is
    /// another review's.
    Put(Box<Put>),
    /// Deletes a comment, draft or sent; one the review does not hold is
    /// already gone.
    Delete { comment: Vec<u8> },
    /// Deletes every draft, leaving the sent comments.
    DiscardDrafts,
    /// A refresh moved drafts onto a later snapshot: each named one that
    /// is still a draft takes its new anchor.
    DraftsMoved { drafts: Vec<MovedDraft> },
    /// Deletes the review: its agent has none until the next begins.
    StartOver,
    /// The agent took a message sending comments of the review `review`:
    /// exactly the comments named that are still drafts at the revision
    /// named are marked sent, in a round numbered next and recorded on the
    /// review's latest snapshot, which is where its drafts sit. A send that
    /// marks none (another review's, one sent already, or one whose
    /// comments were all edited since) closes no round.
    Sent {
        review: Vec<u8>,
        /// Each comment's id and the revision the sent text was written
        /// from.
        comments: Vec<(Vec<u8>, u64)>,
    },
    /// Whether the tree has moved since `snapshot`, when that is still the
    /// latest. Says nothing new about the review itself, so the counter
    /// stays.
    Moved { snapshot: String, moved: bool },
}

/// A comment as a client writes it, with the snippet and scope its host
/// read for it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Put {
    pub comment: Vec<u8>,
    pub anchor: Anchor,
    pub text: String,
    #[serde(with = "codec::messages")]
    pub snippet: Vec<ReviewLine>,
    #[serde(with = "codec::messages")]
    pub scope: Vec<ReviewDefinition>,
}

/// The lines a comment is on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Anchor {
    /// The commit that holds them.
    pub commit: String,
    /// From the repository's top folder.
    pub path: String,
    /// From 1, both ends included; zero to zero is the whole file.
    pub start: u32,
    pub end: u32,
}

/// A draft's anchor on a later snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MovedDraft {
    pub comment: Vec<u8>,
    pub anchor: Anchor,
}

/// Why a write to a review was refused, every change of it unmade.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// This host does not run the experiment.
    Off,
    /// The agent has no review to change.
    NoReview,
    /// The agent's review has begun already.
    Begun,
    /// The comment id is held by another review.
    Taken(Vec<u8>),
}

/// What a write left: [`Response::Review`].
#[derive(Clone, Debug, PartialEq)]
pub struct Written {
    /// The review as the write left it; none once it was started over.
    /// After a [`Change::Snapshot`] its snapshots are exactly those it
    /// keeps, so the snapshot store may drop every other.
    pub review: Option<Review>,
    /// The review's entry on its agent's row; none without a review.
    pub entry: Option<ReviewRowEntry>,
    /// The round a [`Change::Sent`] closed.
    pub round: Option<u32>,
}

/// What the agent's journal said it took, read for the sent reviews in it:
/// the attachments of each item a prompt or message became.
pub(crate) fn sent_in<'a>(items: impl IntoIterator<Item = &'a wire::Item>) -> Vec<SentReview> {
    items
        .into_iter()
        .filter(|item| !item.input_id.is_empty())
        .flat_map(|item| &item.attachments)
        .filter_map(|attachment| SENT_REVIEW.get(&attachment.experimental))
        .collect()
}

/// The experiment's work off the core thread: [`crate::io::Job::Review`].
#[derive(Clone, Debug, PartialEq)]
pub enum Job {
    /// Tell whether the working tree at `cwd` has moved since `snapshot`,
    /// writing no object: answered as [`Done::Moved`].
    Moved {
        agent: AgentId,
        cwd: PathBuf,
        /// The review's snapshot store.
        objects: PathBuf,
        snapshot: String,
        /// The paths the snapshot left out, whose changes do not count.
        omitted: Vec<String>,
    },
}

/// What a [`Job`] came to: [`JobResult::Review`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Done {
    /// Whether the agent's tree has moved since `snapshot`, or why that
    /// could not be told.
    Moved {
        agent: AgentId,
        snapshot: String,
        moved: Result<bool, String>,
    },
}

impl Daemon {
    /// Applies a client's write to a review, answering what it left.
    pub(crate) fn review_write(
        &mut self,
        call: CallId,
        write: Write,
        out: &mut Vec<Output>,
    ) -> Result<(), CallError> {
        let written = self.apply_review(write.agent, &write.changes, out)?;
        answered(call, Response::Review(Box::new(written)), out);
        Ok(())
    }

    /// `agent` ended a turn: each review of an agent in its checkout, its
    /// own among them, learns whether its tree has moved since its latest
    /// snapshot.
    pub(crate) fn review_turn_ended(&mut self, agent: AgentId, out: &mut Vec<Output>) {
        if !self.options.reviews {
            return;
        }
        let Ok(Some(ended)) = self.store.agent(&self.own_key(agent)) else {
            return;
        };
        let latest = match self.store.latest_snapshots() {
            Ok(latest) => latest,
            Err(error) => {
                tracing::warn!(%error, "reading the reviews' snapshots failed");
                return;
            }
        };
        for (id, taken) in latest {
            let Some(reviewed) = AgentId::from_slice(&id) else {
                continue;
            };
            let cwd = if reviewed == agent {
                ended.cwd.clone()
            } else {
                match self.store.agent(&self.own_key(reviewed)) {
                    Ok(Some(row))
                        if crate::git::one_checkout(Path::new(&ended.cwd), Path::new(&row.cwd)) =>
                    {
                        row.cwd
                    }
                    _ => continue,
                }
            };
            let id = self.pending.mint();
            out.push(Output::Job {
                id,
                op: CoreJob::Review(Job::Moved {
                    agent: reviewed,
                    cwd: PathBuf::from(cwd),
                    objects: objects(&self.agent_dir(reviewed)),
                    snapshot: taken.commit,
                    omitted: taken.omitted,
                }),
            });
        }
    }

    /// Items of the own agent `agent` were committed: each sent review in
    /// a prompt or message it took closes a round of its review.
    pub(crate) fn review_items_committed(
        &mut self,
        agent: AgentId,
        sent: Vec<SentReview>,
        out: &mut Vec<Output>,
    ) {
        if !self.options.reviews || sent.is_empty() {
            return;
        }
        let changes: Vec<Change> = sent
            .into_iter()
            .map(|sent| Change::Sent {
                review: sent.review_id,
                comments: sent
                    .comments
                    .into_iter()
                    .map(|comment| (comment.id, comment.revision))
                    .collect(),
            })
            .collect();
        // An agent with no review, or one started over, has nothing to
        // close: the message is the agent's already either way.
        if let Err(error) = self.apply_review(agent, &changes, out) {
            tracing::debug!(?error, "a sent review closed no round");
        }
    }

    /// Takes a review job's result; false when `result` is not one.
    pub(crate) fn review_job_done(
        &mut self,
        _job: JobId,
        result: &JobResult,
        out: &mut Vec<Output>,
    ) -> bool {
        let JobResult::Review(done) = result else {
            return false;
        };
        match done {
            Done::Moved {
                agent,
                snapshot,
                moved,
            } => match moved {
                Ok(moved) => {
                    let change = Change::Moved {
                        snapshot: snapshot.clone(),
                        moved: *moved,
                    };
                    // A review started over or switched off since the check
                    // was asked for has nothing to learn from it.
                    if let Err(error) = self.apply_review(*agent, &[change], out) {
                        tracing::debug!(?error, "a review's moved check found no review");
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, "telling whether a review's tree moved failed");
                }
            },
        }
        true
    }

    /// Makes `changes` to the review of the own agent `agent` in one
    /// transaction with its row's entry, and tells subscribers the row when
    /// the entry changed.
    fn apply_review(
        &mut self,
        agent: AgentId,
        changes: &[Change],
        out: &mut Vec<Output>,
    ) -> Result<Written, CallError> {
        if !self.options.reviews {
            return Err(CallError::Review(Refusal::Off));
        }
        let key = self.own_key(agent);
        let now_ms = self.timers.now_ms as i64;
        let (written, changed) = self
            .store
            .write_review(&key, |writer| apply(writer, changes, now_ms))?;
        if changed && let Err(error) = self.publish_row(&key, out) {
            tracing::warn!(%error, "publishing an agent's review failed");
        }
        Ok(written)
    }
}

/// What the changes made so far did, for the entry they leave.
#[derive(Default)]
struct Effect {
    /// Something of the review itself changed: the counter goes up.
    changed: bool,
    /// The tree has moved since the latest snapshot, as far as is known.
    moved: bool,
    /// A snapshot was taken, after which the unkept are forgotten.
    snapshot: bool,
    round: Option<u32>,
}

/// Makes `changes` inside the write's transaction and rewrites the row's
/// entry; whether the entry changed beside what was written.
fn apply(
    writer: &ReviewWriter<'_>,
    changes: &[Change],
    now_ms: i64,
) -> Result<(Written, bool), CallError> {
    let Some(row) = writer.row()? else {
        return Err(CallError::NoAgent);
    };
    let before = REVIEW_ROW.get(&row.experimental);
    let mut effect = Effect {
        moved: before.as_ref().is_some_and(|entry| entry.moved),
        ..Effect::default()
    };
    for change in changes {
        apply_one(writer, change, now_ms, &mut effect)?;
    }
    let review = writer.review()?;
    if effect.snapshot
        && let Some(review) = &review
    {
        writer.keep_snapshots(&review.id, &review.kept())?;
    }
    let review = if effect.snapshot {
        writer.review()?
    } else {
        review
    };
    let entry = review.as_ref().map(|review| ReviewRowEntry {
        drafts: review.drafts().count() as u32,
        counter: before.as_ref().map_or(0, |entry| entry.counter) + u64::from(effect.changed),
        moved: effect.moved,
        review_id: review.id.clone(),
    });
    let mut map = row.experimental.clone();
    match &entry {
        Some(entry) => REVIEW_ROW.put(&mut map, entry),
        None => REVIEW_ROW.clear(&mut map),
    }
    let changed = map != row.experimental;
    if changed {
        writer.set_experimental(&map)?;
    }
    Ok((
        Written {
            review,
            entry,
            round: effect.round,
        },
        changed,
    ))
}

fn apply_one(
    writer: &ReviewWriter<'_>,
    change: &Change,
    now_ms: i64,
    effect: &mut Effect,
) -> Result<(), CallError> {
    let Some(review) = writer.review()? else {
        let Change::Begin {
            review,
            base_branch,
            began_at,
            common_dir,
        } = change
        else {
            return Err(CallError::Review(Refusal::NoReview));
        };
        writer.begin(review, base_branch, began_at, common_dir, now_ms)?;
        effect.changed = true;
        effect.moved = false;
        return Ok(());
    };
    match change {
        Change::Begin { .. } => return Err(CallError::Review(Refusal::Begun)),
        Change::Base { base_branch } => {
            if *base_branch != review.base_branch {
                writer.set_base(&review.id, base_branch)?;
                effect.changed = true;
            }
        }
        Change::Rebased {
            snapshot,
            base_commit,
        } => {
            if review
                .snapshot(snapshot)
                .is_some_and(|taken| taken.base_commit != *base_commit)
            {
                writer.set_snapshot_base(&review.id, snapshot, base_commit)?;
                effect.changed = true;
            }
        }
        Change::Snapshot {
            commit,
            base_commit,
            omitted,
            merging,
        } => {
            writer.add_snapshot(
                &review.id,
                &Taken {
                    commit: commit.clone(),
                    base_commit: base_commit.clone(),
                    taken_ms: now_ms,
                    omitted: omitted.clone(),
                    merging: *merging,
                },
            )?;
            effect.changed = true;
            effect.moved = false;
            effect.snapshot = true;
        }
        Change::Put(put) => {
            let held = match writer.comment_owner(&put.comment)? {
                None => None,
                Some(owner) if owner == review.id => review
                    .comments
                    .iter()
                    .find(|comment| comment.id == put.comment),
                Some(_) => return Err(CallError::Review(Refusal::Taken(put.comment.clone()))),
            };
            let round = held.and_then(|comment| comment.round);
            let revision = held.map_or(0, |comment| comment.revision) + 1;
            writer.put_comment(
                &review.id,
                &Comment {
                    id: put.comment.clone(),
                    anchor: stored(&put.anchor),
                    text: put.text.clone(),
                    snippet: put.snippet.clone(),
                    scope: put.scope.clone(),
                    round,
                    revision,
                    updated_ms: now_ms,
                },
            )?;
            effect.changed = true;
        }
        Change::Delete { comment } => {
            effect.changed |= writer.delete_comment(&review.id, comment)?;
        }
        Change::DiscardDrafts => {
            effect.changed |= writer.discard_drafts(&review.id)? > 0;
        }
        Change::DraftsMoved { drafts } => {
            for moved in drafts {
                let Some(draft) = review.drafts().find(|draft| draft.id == moved.comment) else {
                    continue;
                };
                writer.put_comment(
                    &review.id,
                    &Comment {
                        anchor: stored(&moved.anchor),
                        updated_ms: now_ms,
                        ..draft.clone()
                    },
                )?;
                effect.changed = true;
            }
        }
        Change::StartOver => {
            writer.remove()?;
            effect.changed = true;
            effect.moved = false;
        }
        Change::Sent {
            review: sent,
            comments,
        } => {
            let Some(latest) = review.latest().filter(|_| *sent == review.id) else {
                return Ok(());
            };
            let named: BTreeSet<(&[u8], u64)> = comments
                .iter()
                .map(|(id, revision)| (id.as_slice(), *revision))
                .collect();
            let marked: Vec<&Comment> = review
                .drafts()
                .filter(|draft| named.contains(&(draft.id.as_slice(), draft.revision)))
                .collect();
            if marked.is_empty() {
                return Ok(());
            }
            let n = review.rounds.last().map_or(0, |round| round.n) + 1;
            writer.add_round(
                &review.id,
                &Round {
                    n,
                    snapshot: latest.commit.clone(),
                    base_commit: latest.base_commit.clone(),
                    sent_ms: now_ms,
                },
            )?;
            for draft in marked {
                writer.put_comment(
                    &review.id,
                    &Comment {
                        round: Some(n),
                        ..draft.clone()
                    },
                )?;
            }
            effect.changed = true;
            effect.round = Some(n);
        }
        Change::Moved { snapshot, moved } => {
            if review
                .latest()
                .is_some_and(|latest| latest.commit == *snapshot)
            {
                effect.moved = *moved;
            }
        }
    }
    Ok(())
}

fn stored(anchor: &Anchor) -> StoredAnchor {
    StoredAnchor {
        commit: anchor.commit.clone(),
        path: anchor.path.clone(),
        start: anchor.start,
        end: anchor.end,
    }
}

#[cfg(test)]
mod tests;
