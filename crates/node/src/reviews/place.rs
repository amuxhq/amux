//! Where a review's comments sit on a later snapshot, by one rule for the
//! summary and for a refresh: one diff from the commit that holds a
//! comment's lines to the snapshot, renames followed, as GitHub places
//! review comments. Lines no change touched land on their new range. A sent
//! comment whose lines were edited or deleted is outdated; a draft's
//! becomes a comment on its whole file, keeping its words and snippet.
//!
//! A comment written on removed lines names the commit its view compared
//! against, not a snapshot. While that commit is still what a view of the
//! review compares against, the lines are still on the page, on its removed
//! side: such a comment is on its file, where the page seats it on those
//! lines, rather than outdated by the very removal it was written on.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use daemon_core::reviews::{Anchor, MovedDraft};
use git_facts::compare::{CompareError, LineMap, LineRange, Mapped};
use git_facts::objects::ObjectStore;
use store::reviews::{Comment, Review};
use wire::Empty;
use wire::experimental::{ReviewLineRange, ReviewPlacement, review_placement};

/// How many lines either side of a comment count as near it when no
/// definition encloses it.
pub(crate) const NEAR_LINES: u32 = 3;

/// The line maps from each commit comments are anchored on to one target
/// snapshot, read with one diff per commit.
pub(crate) struct Maps {
    target: String,
    /// By anchor commit: each anchored path's map, or none when the commit
    /// can no longer be read.
    by_commit: HashMap<String, Option<HashMap<String, LineMap>>>,
}

impl Maps {
    /// The maps `anchors` need onto `target`; an anchor on `target` itself
    /// needs none.
    pub(crate) async fn read<'a>(
        cwd: &Path,
        store: &ObjectStore,
        target: &str,
        anchors: impl IntoIterator<Item = &'a store::reviews::Anchor>,
    ) -> Result<Self, CompareError> {
        let mut wanted: HashMap<&str, BTreeSet<&str>> = HashMap::new();
        for anchor in anchors {
            if anchor.commit != target {
                wanted
                    .entry(anchor.commit.as_str())
                    .or_default()
                    .insert(anchor.path.as_str());
            }
        }
        let mut by_commit = HashMap::new();
        for (commit, paths) in wanted {
            let paths: Vec<&str> = paths.into_iter().collect();
            let maps = match LineMap::for_paths(cwd, store, commit, target, &paths).await {
                Ok(maps) => Some(maps),
                // A round whose commits are gone: its comments are shown
                // with their snippets.
                Err(CompareError::Missing(_)) => None,
                Err(error) => return Err(error),
            };
            by_commit.insert(commit.to_owned(), maps);
        }
        Ok(Self {
            target: target.to_owned(),
            by_commit,
        })
    }

    /// Where `anchor`'s lines are on the target; none when its commit can
    /// no longer be read.
    fn map(&self, anchor: &store::reviews::Anchor) -> Option<Moved<'_>> {
        if anchor.commit == self.target {
            return Some(Moved::Same);
        }
        let maps = self.by_commit.get(&anchor.commit)?.as_ref()?;
        maps.get(&anchor.path).map(Moved::By)
    }
}

enum Moved<'a> {
    /// The anchor is on the target itself.
    Same,
    By(&'a LineMap),
}

/// What the placement of a review's comments needs besides the maps.
pub(crate) struct Views<'a> {
    /// The snapshot comments are placed on.
    pub target: &'a str,
    /// The commits a view of the review compares the target against: the
    /// base commit, and the last round's snapshot.
    pub compared: &'a [&'a str],
}

/// Each comment of `review`, in order, placed on `views.target`.
pub(crate) fn placements(review: &Review, views: &Views<'_>, maps: &Maps) -> Vec<ReviewPlacement> {
    review
        .comments
        .iter()
        .map(|comment| place(review, comment, views, maps))
        .collect()
}

/// The snapshot `comment` was written on: its round's for a sent one; a
/// draft is always on the latest, since a refresh moves the drafts.
fn written_on<'a>(review: &'a Review, comment: &Comment, target: &'a str) -> &'a str {
    match comment.round {
        Some(n) => review
            .rounds
            .iter()
            .find(|round| round.n == n)
            .map_or(target, |round| round.snapshot.as_str()),
        None => target,
    }
}

fn range(anchor: &store::reviews::Anchor) -> LineRange {
    match anchor.start {
        0 => LineRange::file(),
        start => LineRange::new(start, anchor.end.max(start)),
    }
}

fn place(review: &Review, comment: &Comment, views: &Views<'_>, maps: &Maps) -> ReviewPlacement {
    use review_placement::At;
    let anchor = &comment.anchor;
    let sent = comment.round.is_some();
    let on = written_on(review, comment, views.target);
    let removed_side = anchor.commit != on;
    let lines = |range: LineRange| match range.is_file() {
        true => At::File(Empty {}),
        false => At::Lines(ReviewLineRange {
            start: range.start,
            end: range.end,
        }),
    };
    let edited = || match sent {
        true => At::Outdated(Empty {}),
        false => At::File(Empty {}),
    };
    let (at, map) = match maps.map(anchor) {
        Some(Moved::Same) => (lines(range(anchor)), None),
        None => (edited(), None),
        Some(Moved::By(map)) => match map.map(range(anchor)) {
            Mapped::Untouched(moved) => (lines(moved), Some(map)),
            Mapped::Touched | Mapped::Gone
                if removed_side && views.compared.contains(&anchor.commit.as_str()) =>
            {
                (At::File(Empty {}), None)
            }
            Mapped::Touched | Mapped::Gone => (edited(), None),
        },
    };
    // Near: the comment's lines are where they were, and something around
    // them changed since the round it was sent in, which is the diff from
    // its anchor's commit when it was written on that round's snapshot.
    let changed_near = match (&at, map) {
        (At::Lines(_), Some(map)) if sent && !removed_side => map.touches(near(comment)),
        _ => false,
    };
    ReviewPlacement {
        at: Some(at),
        changed_near,
    }
}

/// The lines whose change counts as near `comment`, in its anchor's
/// commit: its innermost enclosing definition, or with none the lines
/// within [`NEAR_LINES`] of it.
fn near(comment: &Comment) -> LineRange {
    let lines = range(&comment.anchor);
    match comment.scope.last() {
        Some(definition) if definition.start_line > 0 => LineRange::new(
            definition.start_line,
            definition.end_line.max(definition.start_line),
        ),
        _ => lines.widened(NEAR_LINES),
    }
}

/// The drafts of `review` a refresh moves from `from`, its latest snapshot,
/// onto `to`, each with its anchor there: one whose lines no change touched
/// onto their new range, any other onto its whole file. A draft on removed
/// lines of a commit a view still compares against stays where it is.
pub(crate) fn moved_drafts(
    review: &Review,
    from: &str,
    to: &str,
    compared: &[&str],
    maps: &Maps,
) -> Vec<MovedDraft> {
    review
        .drafts()
        .filter(|draft| draft.anchor.commit != to)
        .filter(|draft| {
            draft.anchor.commit == from || !compared.contains(&draft.anchor.commit.as_str())
        })
        .map(|draft| {
            let anchor = &draft.anchor;
            let (path, lines) = match maps.map(anchor) {
                Some(Moved::By(map)) => match map.map(range(anchor)) {
                    Mapped::Untouched(lines) => (map.path().unwrap_or(&anchor.path), lines),
                    Mapped::Touched | Mapped::Gone => {
                        (map.path().unwrap_or(&anchor.path), LineRange::file())
                    }
                },
                Some(Moved::Same) | None => (anchor.path.as_str(), LineRange::file()),
            };
            MovedDraft {
                comment: draft.id.clone(),
                anchor: Anchor {
                    commit: to.to_owned(),
                    path: path.to_owned(),
                    start: lines.start,
                    end: lines.end,
                },
            }
        })
        .collect()
}
