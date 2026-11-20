//! The review page: an agent's changes as one snapshot of its working tree
//! shows them, with the person's comments placed on their lines, composed
//! once here so the terminal, the Mac and the iPhone draw the same rows.
//!
//! The page is a function of the review's summary as its host answered it,
//! the file bodies and extra lines fetched for it, and the person's own
//! choices ([`ReviewPageState`]). Highlighting arrives with the file bodies,
//! worked out on the host from whole files; changed words are worked out
//! here. A page is drawn as it comes: the rows are in reading order, and a
//! selection of them becomes a comment's anchor through [`anchor`].

use std::collections::{BTreeMap, HashSet};
use std::ops::Range;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use wire::experimental::{
    REVIEW_ROW, ReviewDefinition, ReviewDefinitionKind, ReviewFileDiff, ReviewHunk, ReviewLine,
    ReviewLineKind, ReviewSide, ReviewSummary, ReviewTokenKind, ReviewerComment, review_placement,
};

use crate::changed_words::changed_words;
use crate::code::{LineKind, TokenKind};
use crate::overview::FileStatus;

/// How many lines one ask for more context loads when no edge of a
/// definition is near: enough to read the code around a change without
/// asking again and again.
pub const CONTEXT_STEP: u32 = 20;

/// The most lines one ask for more context reaches, the same bound the
/// agent's host keeps: a definition longer than this loads in parts.
pub const MOST_CONTEXT: u32 = 2_000;

/// The lines of context the agent's host gives each hunk on either side of
/// its changes: a hunk ending in fewer ends at the file's end.
pub const HUNK_CONTEXT: u32 = 3;

/// Which comparison the page shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum ReviewView {
    /// Everything on the branch: from the base commit to the snapshot.
    All,
    /// From the last round's snapshot to this one, limited to what is
    /// still part of the agent's change.
    SinceLast,
}

/// The person's own choices on a page, which the client keeps and hands
/// back each time the page is composed. Everything else comes from the
/// agent's host.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReviewPageState {
    /// The view switched to; none for the one a look opens on: since your
    /// last review once a round has been sent, all changes before.
    pub view: Option<ReviewView>,
    /// Files the person folded or opened, by path: each flips how the file
    /// starts. Generated files, lock files, vendored code and large changes
    /// start folded; every other file starts open.
    pub toggled_files: Vec<String>,
    /// Outdated comments the person opened, by id; they start folded.
    pub opened_comments: Vec<Vec<u8>>,
    /// More context asked for, from the gaps' asks, answered or not.
    pub asked: Vec<ReviewContextAsk>,
    /// What the client learned that the summary does not say.
    pub notices: Vec<ReviewNotice>,
}

impl ReviewPageState {
    /// Folds an open file or opens a folded one.
    pub fn toggle_file(&mut self, path: &str) {
        match self
            .toggled_files
            .iter()
            .position(|toggled| toggled == path)
        {
            Some(at) => {
                self.toggled_files.remove(at);
            }
            None => self.toggled_files.push(path.to_owned()),
        }
    }

    /// Opens a folded outdated comment or folds an opened one.
    pub fn toggle_comment(&mut self, id: &[u8]) {
        match self.opened_comments.iter().position(|opened| opened == id) {
            Some(at) => {
                self.opened_comments.remove(at);
            }
            None => self.opened_comments.push(id.to_owned()),
        }
    }
}

/// Something the client learned about the review that its summary does not
/// carry: from the agent's row, or from a call that failed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum ReviewNotice {
    /// The working tree changed after the summary was read: the agent's
    /// row says so, or a newer summary was held back so the page does not
    /// change under the reader.
    Moved,
    /// The agent's host does not run the reviews experiment.
    NotOffered,
    /// The agent's folder is not a git repository, so it has no review.
    NotARepository,
    /// A round's commits can no longer be read on the agent's host, as
    /// after a rebase and garbage collection: since your last review, or the
    /// round's own page, shows each comment with the code stored with it
    /// instead.
    RoundGone,
    /// A snapshot of the working tree could not be taken; what the host
    /// said.
    SnapshotFailed { message: String },
    /// The agent's host cannot be reached: the page reads what was loaded,
    /// and changes wait for the host.
    Unreachable,
}

/// The review page, in reading order.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ReviewPage {
    /// None until the review has opened, and where there is none.
    pub header: Option<ReviewHeader>,
    /// Said above the files, in order.
    pub banners: Vec<ReviewBanner>,
    pub rows: Vec<ReviewRow>,
    /// Set when the view shown has nothing to review. Since your last
    /// review can be empty of changes and still list earlier comments.
    pub empty: Option<ReviewEmpty>,
    /// The files whose bodies the page still needs for its view, in page
    /// order: shown open and not yet fetched. A client asks for them in
    /// batches and composes the page again as each answer arrives.
    pub to_fetch: Vec<String>,
    /// The review is still being opened: there is no summary yet and
    /// nothing went wrong.
    pub opening: bool,
}

/// What the page is of, and what can be done to it as a whole.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ReviewHeader {
    /// The branch the change is counted against; empty when none was found
    /// and the change is the uncommitted work alone.
    pub base_branch: String,
    /// The commit all changes are compared against.
    pub base_commit: String,
    /// The snapshot the page shows: every line number on it is the
    /// snapshot's, but a removed line's, which is the compared commit's.
    pub snapshot: String,
    pub view: ReviewView,
    /// The commit the view shown compares the snapshot against: the base
    /// commit, or the last round's snapshot.
    pub compared_with: String,
    /// For since your last review, the base commit the last round's
    /// snapshot was shown against, which with `base_commit` decides what of
    /// the change since is the agent's; empty for all changes.
    pub compared_base: String,
    /// The views the person can switch between, in order: since your last
    /// review is offered once a round has been sent.
    pub views: Vec<ReviewViewTab>,
    /// Rounds sent so far.
    pub rounds: u32,
    /// The files the view shown lists as changed, and their lines.
    pub files: u32,
    pub added: u32,
    pub removed: u32,
    /// Comments not yet sent.
    pub drafts: u32,
    /// Sent comments still on the review.
    pub sent: u32,
    /// Discarding drafts is offered: there are some.
    pub discard_drafts: bool,
    /// Starting over, which deletes the review, is offered.
    pub start_over: bool,
}

/// One of the views, as the switch between them shows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ReviewViewTab {
    pub view: ReviewView,
    /// The changed files it lists.
    pub files: u32,
}

/// Something said above the files.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub enum ReviewBanner {
    /// Drafts hold the page on the snapshot they were written on, and the
    /// tree has changed since: refreshing moves them onto a new snapshot.
    ChangedSinceYouStarted,
    /// The agent changed code since this snapshot: refreshing shows it.
    ChangedSince,
    /// The refresh that took this snapshot moved this many drafts onto it;
    /// those whose lines were edited are now comments on their files.
    MovedDrafts {
        count: u32,
    },
    /// A merge or rebase was under way when the snapshot was taken, so
    /// conflicted files hold their markers as content.
    MergeUnderway,
    NotOffered,
    NotARepository,
    /// The round's snapshot is gone from the agent's host: its comments
    /// show with the code stored with them.
    RoundGone,
    SnapshotFailed {
        message: String,
    },
    Unreachable,
}

/// Why the view shown has nothing in it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub enum ReviewEmpty {
    /// Nothing is left to review: no change and no comment.
    Nothing,
    /// Nothing changed since the last round; all changes still has the
    /// change.
    NothingSince,
}

/// One row of the page, with a key that names it across compositions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ReviewRow {
    pub key: String,
    pub of: ReviewRowOf,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub enum ReviewRowOf {
    /// A file's head; its rows follow until the next head.
    File(ReviewFileHead),
    Comment(ReviewCommentRow),
    /// Where a hunk sits in the code; its lines follow.
    Hunk(ReviewHunkHead),
    Line(ReviewCodeLine),
    /// Lines of the snapshot not shown, with the asks that load more.
    Gap(ReviewGap),
    /// The file's body is still being fetched.
    Loading {
        path: String,
    },
    /// The file's change has no lines to show.
    NoLines {
        path: String,
        why: ReviewNoLines,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub enum ReviewNoLines {
    Binary,
    /// Over the size the host shows without asking.
    TooLarge,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ReviewFileHead {
    /// From the repository's top.
    pub path: String,
    /// Where a renamed file was before.
    pub old_path: Option<String>,
    pub status: FileStatus,
    /// Its lines added and removed in the view shown.
    pub added: u32,
    pub removed: u32,
    pub binary: bool,
    /// The language its code is highlighted as ("rust", "tsx"); empty when
    /// none is known for it.
    pub language: String,
    pub flags: Vec<ReviewFileFlag>,
    /// Its body is folded away; its head still counts its comments.
    pub folded: bool,
    /// It changed since the last round's snapshot; false before the first.
    pub changed_since_last: bool,
    /// False for a file no longer part of the change, listed only for the
    /// comments on it.
    pub in_change: bool,
    /// Comments on the file, on its lines or on it whole.
    pub comments: u32,
    /// The definitions its change touches, on either side, so a deleted one
    /// still shows; empty until its body is fetched.
    pub outline: Vec<ReviewOutlineEntry>,
}

/// What a reader may want to pass over; a flagged file starts folded.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub enum ReviewFileFlag {
    Generated,
    Lockfile,
    Vendored,
    Large,
}

/// One definition of a file's outline.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ReviewOutlineEntry {
    pub kind: DefinitionKind,
    /// "impl Display for Store", "fn open".
    pub label: String,
    /// How many definitions it sits in.
    pub depth: u32,
    /// Its lines, numbered in the snapshot, or in the compared commit when
    /// `compared`.
    pub start_line: u32,
    pub end_line: u32,
    /// Its lines are the compared commit's: a definition only removed lines
    /// reach, as one deleted.
    pub compared: bool,
    /// The row of the first hunk whose changes it encloses, for jumping to
    /// it; none for one only a hunk's context reaches.
    pub row: Option<u32>,
}

/// What a definition is, across languages, for a client to mark it by.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub enum DefinitionKind {
    Module,
    /// A class, and its relatives: Swift's actor, Java's record.
    Class,
    Struct,
    Enum,
    Union,
    /// An interface, Rust's trait, Swift's protocol.
    Interface,
    /// Methods given to a type declared elsewhere: Rust's impl, Swift's
    /// extension.
    Impl,
    TypeAlias,
    Function,
    Method,
    Constructor,
    Constant,
    Static,
    Macro,
    /// A kind this build has no name for.
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ReviewHunkHead {
    pub path: String,
    /// The commit its removed lines belong to.
    pub old_commit: String,
    pub old_start: u32,
    pub new_start: u32,
    /// Where its changed lines sit, outermost first: `impl Store`, `fn
    /// open`. Empty where no definition encloses them.
    pub scope: Vec<String>,
}

/// One line of code.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ReviewCodeLine {
    pub path: String,
    pub kind: LineKind,
    /// Its number in the compared commit, for a removed or context line.
    pub old_line: Option<u32>,
    /// Its number in the snapshot, for an added or context line.
    pub new_line: Option<u32>,
    /// Without its line ending.
    pub text: String,
    /// The text in order, each run one token kind and either changed or
    /// not: on a line edited rather than written anew, the words that
    /// differ from its partner on the other side are changed.
    pub runs: Vec<ReviewRun>,
    /// A comment placed on the page covers this line.
    pub commented: bool,
    /// Read for more context, outside the change's hunks.
    pub extra: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ReviewRun {
    pub text: String,
    pub kind: TokenKind,
    pub changed: bool,
}

/// Lines of the snapshot between what is shown, folded away.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ReviewGap {
    pub path: String,
    /// How many lines it holds; none after the file's last hunk, where the
    /// file's length is not known until the lines are asked for.
    pub hidden: Option<u32>,
    /// Loads lines upward from the code below it: up to the start of that
    /// code's definition when it starts in the gap, else a step; or the
    /// whole gap when it is short. None after the last hunk.
    pub up: Option<ReviewContextAsk>,
    /// Loads lines downward from the code above it: to the end of that
    /// code's definition when it ends in the gap, else a step. None at the
    /// top of the file and for a gap loaded whole by `up`.
    pub down: Option<ReviewContextAsk>,
    /// An ask in it is waiting for its answer.
    pub loading: bool,
}

/// Lines to read for more context: a client asks the review for them and
/// keeps the ask in [`ReviewPageState::asked`].
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct ReviewContextAsk {
    pub path: String,
    /// The commit to read: the snapshot.
    pub commit: String,
    /// Both ends included, from 1.
    pub start: u32,
    pub end: u32,
    /// The label of the definition whose edge it reaches; none for a step
    /// or a whole gap.
    pub definition: Option<String>,
}

/// A comment, where the page places it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ReviewCommentRow {
    pub id: Vec<u8>,
    /// The file it is on in the snapshot.
    pub path: String,
    pub text: String,
    pub state: ReviewCommentState,
    pub at: ReviewCommentAt,
    /// A sent comment whose code nearby changed since the round it was
    /// sent in.
    pub changed_near: bool,
    /// Where its lines sat in the code, outermost first.
    pub scope: Vec<String>,
    /// The code it was written on, as stored when it was saved, when it is
    /// not drawn under its lines: outdated, moved to its file, on lines the
    /// view shown does not reach, or on a file not yet fetched. Empty for a
    /// comment drawn under its lines and for one written on a whole file.
    pub snippet: Vec<ReviewCodeLine>,
    /// Folded to one line: an outdated comment until the person opens it.
    pub folded: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub enum ReviewCommentState {
    Draft,
    /// Sent in the round numbered, from 1.
    Sent {
        round: u32,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub enum ReviewCommentAt {
    /// On these lines: the snapshot's numbers, or the compared commit's for
    /// a comment on removed lines.
    Lines { start: u32, end: u32 },
    /// On the whole file: written so, or a draft whose lines were edited.
    File,
    /// A sent comment whose lines were edited or deleted since.
    Outdated,
}

/// Where a new comment goes: the commit holding its lines, the file and
/// the lines, both ends included; zero to zero for the whole file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReviewAnchor {
    pub commit: String,
    pub path: String,
    pub start: u32,
    pub end: u32,
}

impl ReviewAnchor {
    /// As the review's edit takes it.
    pub fn to_wire(&self) -> wire::experimental::ReviewAnchor {
        wire::experimental::ReviewAnchor {
            commit: self.commit.clone(),
            path: self.path.clone(),
            lines: Some(wire::experimental::ReviewLineRange {
                start: self.start,
                end: self.end,
            }),
        }
    }
}

/// The review chip an agent's composer shows while its review holds
/// drafts: "Review · 3 comments".
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ReviewChip {
    /// Drafts it would send.
    pub comments: u32,
    /// The tree has changed since the drafts' snapshot.
    pub moved: bool,
}

/// The chip, from the review's entry on the agent's row; none while the
/// agent has no review or its review holds no drafts.
pub fn review_chip(agent: &wire::Agent) -> Option<ReviewChip> {
    let entry = REVIEW_ROW.get(&agent.experimental)?;
    (entry.drafts > 0).then_some(ReviewChip {
        comments: entry.drafts,
        moved: entry.moved,
    })
}

/// The page, from the review's summary (none until it opens, or where
/// there is none), the bodies fetched for the view the page shows, the
/// answers to asks for more context, and the person's choices.
pub fn review_page(
    summary: Option<&ReviewSummary>,
    files: &[ReviewFileDiff],
    context: &[(ReviewContextAsk, Vec<ReviewLine>)],
    state: &ReviewPageState,
) -> ReviewPage {
    review_page_of(
        summary,
        |_| files.iter().cloned().map(std::sync::Arc::new).collect(),
        context,
        state,
    )
}

/// [`review_page`], with the bodies asked for once the page has decided
/// which view it shows: `files` answers those a client fetched for it. The
/// view is the one the person switched to, or the one a look opens on, and
/// the page's header says which ([`ReviewHeader::view`]).
pub fn review_page_of(
    summary: Option<&ReviewSummary>,
    files: impl FnOnce(ReviewView) -> Vec<std::sync::Arc<ReviewFileDiff>>,
    context: &[(ReviewContextAsk, Vec<ReviewLine>)],
    state: &ReviewPageState,
) -> ReviewPage {
    let mut page = ReviewPage::default();
    let noticed = |notice: &ReviewNotice| state.notices.contains(notice);
    let failures = state.notices.iter().filter_map(|notice| {
        Some(match notice {
            ReviewNotice::Moved => return None,
            ReviewNotice::NotOffered => ReviewBanner::NotOffered,
            ReviewNotice::NotARepository => ReviewBanner::NotARepository,
            ReviewNotice::RoundGone => ReviewBanner::RoundGone,
            ReviewNotice::SnapshotFailed { message } => ReviewBanner::SnapshotFailed {
                message: message.clone(),
            },
            ReviewNotice::Unreachable => ReviewBanner::Unreachable,
        })
    });
    let Some(summary) = summary else {
        page.banners.extend(failures);
        page.opening = page.banners.is_empty();
        return page;
    };
    let drafts = summary
        .comments
        .iter()
        .filter(|comment| comment.round.is_none())
        .count() as u32;
    if summary.stale || noticed(&ReviewNotice::Moved) {
        page.banners.push(if drafts > 0 {
            ReviewBanner::ChangedSinceYouStarted
        } else {
            ReviewBanner::ChangedSince
        });
    }
    if summary.moved_drafts > 0 {
        page.banners.push(ReviewBanner::MovedDrafts {
            count: summary.moved_drafts,
        });
    }
    if summary.merge_in_progress {
        page.banners.push(ReviewBanner::MergeUnderway);
    }
    page.banners.extend(failures);

    let view = match state.view {
        _ if summary.rounds.is_empty() => ReviewView::All,
        Some(view) => view,
        None => ReviewView::SinceLast,
    };
    let (compared_with, compared_base) = match view {
        ReviewView::All => (summary.base_commit.clone(), String::new()),
        ReviewView::SinceLast => summary
            .rounds
            .last()
            .map(|round| (round.snapshot.clone(), round.base_commit.clone()))
            .unwrap_or_default(),
    };
    // What changed since the last round, as that comparison lists it: a
    // file the agent put back as it was is here, though all changes no
    // longer lists it.
    let since: BTreeMap<&str, &wire::ChangedFile> = summary
        .since_last
        .iter()
        .map(|file| (file.path.as_str(), file))
        .collect();
    let changed_since = |path: &str| since.contains_key(path);

    // Comments by the file they sit in, as the host placed them: for a file
    // renamed since a comment was written, its new path. One whose commit
    // can no longer be read has no place, and shows on the file it was
    // written on.
    let mut by_file: BTreeMap<String, Vec<&ReviewerComment>> = BTreeMap::new();
    for comment in &summary.comments {
        let path = comment
            .placement
            .as_ref()
            .map(|placement| placement.path.as_str())
            .filter(|path| !path.is_empty())
            .or_else(|| comment.anchor.as_ref().map(|anchor| anchor.path.as_str()))
            .unwrap_or_default();
        by_file.entry(path.to_owned()).or_default().push(comment);
    }

    // The files the view lists, each as its own comparison has it: all
    // changes; or since the last round, every file that comparison lists,
    // and besides, for the comments on them, the files of all changes that
    // did not change since.
    let listed: Vec<&wire::ChangedFile> = match view {
        ReviewView::All => summary.files.iter().collect(),
        ReviewView::SinceLast => {
            let mut listed: Vec<&wire::ChangedFile> = summary.since_last.iter().collect();
            listed.extend(
                summary
                    .files
                    .iter()
                    .filter(|file| !changed_since(&file.path) && by_file.contains_key(&file.path)),
            );
            listed.sort_by(|a, b| a.path.cmp(&b.path));
            listed
        }
    };
    let outside: Vec<String> = by_file
        .keys()
        .filter(|path| !listed.iter().any(|file| file.path == **path))
        .cloned()
        .collect();
    let files = files(view);
    let fetched: BTreeMap<&str, &ReviewFileDiff> = files
        .iter()
        .map(|file| (file.path.as_str(), &**file))
        .collect();
    let toggled: HashSet<&str> = state.toggled_files.iter().map(String::as_str).collect();
    let opened: HashSet<&[u8]> = state.opened_comments.iter().map(Vec::as_slice).collect();
    let snippets_only = view == ReviewView::SinceLast && noticed(&ReviewNotice::RoundGone);
    let place = Place {
        snapshot: &summary.snapshot,
        compared: &compared_with,
        context,
        asked: &state.asked,
        opened: &opened,
    };

    let (mut files_shown, mut added, mut removed) = (0, 0, 0);
    for file in listed {
        let in_view = view == ReviewView::All || changed_since(&file.path);
        let (file_added, file_removed) = match in_view {
            true => (file.added, file.removed),
            false => (0, 0),
        };
        if in_view {
            files_shown += 1;
            added += file_added;
            removed += file_removed;
        }
        let flags: Vec<ReviewFileFlag> = file
            .flags()
            .filter_map(|flag| match flag {
                wire::FileFlag::Generated => Some(ReviewFileFlag::Generated),
                wire::FileFlag::Lockfile => Some(ReviewFileFlag::Lockfile),
                wire::FileFlag::Vendored => Some(ReviewFileFlag::Vendored),
                wire::FileFlag::Large => Some(ReviewFileFlag::Large),
                wire::FileFlag::Unspecified => None,
            })
            .collect();
        let folded = !flags.is_empty() != toggled.contains(file.path.as_str());
        let comments = by_file.get(&file.path).map_or(&[][..], Vec::as_slice);
        let head = ReviewFileHead {
            path: file.path.clone(),
            old_path: file.old_path.clone(),
            status: match file.status() {
                wire::FileStatus::Changed | wire::FileStatus::Unspecified => FileStatus::Modified,
                wire::FileStatus::Created => FileStatus::Added,
                wire::FileStatus::Deleted => FileStatus::Deleted,
                wire::FileStatus::Renamed => FileStatus::Renamed,
            },
            added: file_added,
            removed: file_removed,
            binary: file.binary,
            language: file.language.clone(),
            folded,
            changed_since_last: changed_since(&file.path),
            in_change: true,
            comments: comments.len() as u32,
            outline: Vec::new(),
            flags,
        };
        let large = head.flags.contains(&ReviewFileFlag::Large);
        let head_at = page.rows.len();
        page.rows
            .push(row(format!("f/{}", file.path), ReviewRowOf::File(head)));
        if folded {
            continue;
        }
        let body = if !in_view || snippets_only {
            Body::CommentsOnly
        } else if file.binary {
            Body::NoLines(ReviewNoLines::Binary)
        } else if large {
            Body::NoLines(ReviewNoLines::TooLarge)
        } else {
            match fetched.get(file.path.as_str()) {
                Some(diff) => Body::Diff(diff),
                None => {
                    page.to_fetch.push(file.path.clone());
                    Body::Loading
                }
            }
        };
        // A created or deleted file's one hunk is all of it.
        let gaps = !matches!(
            file.status(),
            wire::FileStatus::Created | wire::FileStatus::Deleted
        );
        let outline = place.file(&mut page.rows, &file.path, comments, body, gaps);
        if let ReviewRowOf::File(head) = &mut page.rows[head_at].of {
            head.outline = outline;
        }
    }
    for path in &outside {
        let comments = by_file.get(path).map_or(&[][..], Vec::as_slice);
        let folded = toggled.contains(path.as_str());
        page.rows.push(row(
            format!("f/{path}"),
            ReviewRowOf::File(ReviewFileHead {
                path: path.clone(),
                old_path: None,
                status: FileStatus::Modified,
                added: 0,
                removed: 0,
                binary: false,
                language: String::new(),
                flags: Vec::new(),
                folded,
                changed_since_last: false,
                in_change: false,
                comments: comments.len() as u32,
                outline: Vec::new(),
            }),
        ));
        if !folded {
            place.file(&mut page.rows, path, comments, Body::CommentsOnly, false);
        }
    }

    let mut views = vec![ReviewViewTab {
        view: ReviewView::All,
        files: summary.files.len() as u32,
    }];
    if !summary.rounds.is_empty() {
        views.push(ReviewViewTab {
            view: ReviewView::SinceLast,
            files: summary.since_last.len() as u32,
        });
    }
    page.empty = match view {
        _ if summary.files.is_empty() && page.rows.is_empty() => Some(ReviewEmpty::Nothing),
        ReviewView::SinceLast if files_shown == 0 => Some(ReviewEmpty::NothingSince),
        _ => None,
    };
    page.header = Some(ReviewHeader {
        base_branch: summary.base_branch.clone(),
        base_commit: summary.base_commit.clone(),
        snapshot: summary.snapshot.clone(),
        view,
        compared_with,
        compared_base,
        views,
        rounds: summary.rounds.len() as u32,
        files: files_shown,
        added,
        removed,
        drafts,
        sent: summary.comments.len() as u32 - drafts,
        discard_drafts: drafts > 0,
        start_over: true,
    });
    page
}

/// The commit and lines a selection of rows, from one row to another in
/// either order, would be commented on: a file's head alone is the whole
/// file; otherwise the lines of one file among the rows. Added and context
/// lines are the snapshot's, so a selection holding any is on the
/// snapshot's lines among them; one of removed lines alone is on those
/// lines of the commit their hunk compared against. None for a selection
/// with no line, or across files.
pub fn anchor(page: &ReviewPage, from: usize, to: usize) -> Option<ReviewAnchor> {
    let header = page.header.as_ref()?;
    let (from, to) = (from.min(to), from.max(to));
    let rows = page.rows.get(from..=to)?;
    if let [only] = rows
        && let ReviewRowOf::File(head) = &only.of
    {
        let commit = if head.status == FileStatus::Deleted {
            &header.compared_with
        } else {
            &header.snapshot
        };
        return Some(ReviewAnchor {
            commit: commit.clone(),
            path: head.path.clone(),
            start: 0,
            end: 0,
        });
    }
    // The commit removed lines belong to is their hunk's, from the head
    // above the first selected row.
    let mut old_commit = page.rows[..from]
        .iter()
        .rev()
        .find_map(|row| match &row.of {
            ReviewRowOf::Hunk(hunk) => Some(&hunk.old_commit),
            ReviewRowOf::File(_) => Some(&header.compared_with),
            _ => None,
        });
    let mut path: Option<&str> = None;
    // The first and last line selected on each side, both included.
    let mut new: Option<(u32, u32)> = None;
    let mut old: Option<(u32, u32)> = None;
    let mut removed_commit: Option<&String> = None;
    let widen = |lines: &mut Option<(u32, u32)>, line: u32| {
        *lines = Some(match *lines {
            Some((first, last)) => (first.min(line), last.max(line)),
            None => (line, line),
        });
    };
    for row in rows {
        match &row.of {
            ReviewRowOf::Hunk(hunk) => old_commit = Some(&hunk.old_commit),
            ReviewRowOf::File(head) => {
                if path.is_some_and(|path| path != head.path) {
                    return None;
                }
                path = Some(&head.path);
            }
            ReviewRowOf::Line(line) => {
                if path.is_some_and(|path| path != line.path) {
                    return None;
                }
                path = Some(&line.path);
                match (line.kind, line.new_line, line.old_line) {
                    (LineKind::Removed, _, Some(number)) => {
                        let commit = old_commit?;
                        if removed_commit.is_some_and(|seen| seen != commit) {
                            return None;
                        }
                        removed_commit = Some(commit);
                        widen(&mut old, number);
                    }
                    (_, Some(number), _) => widen(&mut new, number),
                    _ => {}
                }
            }
            _ => {}
        }
    }
    let path = path?.to_owned();
    let (commit, (start, end)) = match (new, old, removed_commit) {
        (Some(lines), _, _) => (&header.snapshot, lines),
        (None, Some(lines), Some(commit)) => (commit, lines),
        _ => return None,
    };
    Some(ReviewAnchor {
        commit: commit.clone(),
        path,
        start,
        end,
    })
}

/// What an open file's body is.
enum Body<'a> {
    Diff(&'a ReviewFileDiff),
    Loading,
    NoLines(ReviewNoLines),
    /// Only its comments, each with its snippet: a file the view shown has
    /// no change of, or a round whose code is gone.
    CommentsOnly,
}

/// What placing files' rows reads beside each file.
struct Place<'a> {
    snapshot: &'a str,
    /// The commit the view shown compares the snapshot against.
    compared: &'a str,
    context: &'a [(ReviewContextAsk, Vec<ReviewLine>)],
    asked: &'a [ReviewContextAsk],
    opened: &'a HashSet<&'a [u8]>,
}

/// The line a comment sits under.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Seat {
    /// The snapshot's line with this number.
    New(u32),
    /// The removed line with this number of the commit the view compares
    /// against.
    Removed(u32),
}

/// The comments of one file seated under its lines, and the lines they
/// cover.
#[derive(Default)]
struct Seating<'a> {
    seated: Vec<(Seat, &'a ReviewerComment, ReviewCommentAt)>,
    new: HashSet<u32>,
    old: HashSet<u32>,
}

impl Seating<'_> {
    /// Pushes a row, marked when a seated comment covers its line, and the
    /// comments seated under it.
    fn push(
        &self,
        place: &Place,
        rows: &mut Vec<ReviewRow>,
        path: &str,
        mut drawn: ReviewRow,
        seat: Option<Seat>,
    ) {
        if let (ReviewRowOf::Line(line), Some(seat)) = (&mut drawn.of, seat) {
            line.commented = match seat {
                Seat::New(number) => self.new.contains(&number),
                Seat::Removed(number) => self.old.contains(&number),
            };
        }
        rows.push(drawn);
        let Some(seat) = seat else {
            return;
        };
        for (under, comment, at) in &self.seated {
            if *under == seat {
                rows.push(place.comment(path, comment, *at, false));
            }
        }
    }
}

impl<'a> Place<'a> {
    /// Pushes an open file's rows after its head, and answers its outline.
    /// `gaps` is false for a file wholly created or deleted, whose hunk is
    /// all of it.
    fn file(
        &self,
        rows: &mut Vec<ReviewRow>,
        path: &str,
        comments: &[&'a ReviewerComment],
        body: Body<'a>,
        gaps: bool,
    ) -> Vec<ReviewOutlineEntry> {
        let diff = match body {
            Body::Diff(diff) => Some(diff),
            _ => None,
        };
        // Removed lines each hunk shows, by the commit they belong to.
        let mut removed_shown: BTreeMap<&str, HashSet<u32>> = BTreeMap::new();
        for hunk in diff.iter().flat_map(|diff| &diff.hunks) {
            removed_shown
                .entry(hunk.old_commit.as_str())
                .or_default()
                .extend(
                    hunk.lines
                        .iter()
                        .filter(|line| line.kind() == ReviewLineKind::Removed && line.old_line > 0)
                        .map(|line| line.old_line),
                );
        }
        let mut whole = Vec::new();
        let mut on_lines = Vec::new();
        for &comment in comments {
            match placed(comment) {
                (at @ ReviewCommentAt::Lines { .. }, None) => on_lines.push((comment, at, None)),
                // On removed lines of the commit this view compares
                // against: on those lines, on the removed side. Of any
                // other commit, the view does not show them.
                (at @ ReviewCommentAt::Lines { .. }, Some(commit)) if commit == self.compared => {
                    on_lines.push((comment, at, Some(commit)))
                }
                (at, _) => whole.push((comment, at)),
            }
        }
        // Comments on the file come first under its head, then outdated
        // ones.
        whole.sort_by_key(|(_, at)| *at == ReviewCommentAt::Outdated);
        for (comment, at) in whole {
            rows.push(self.comment(path, comment, at, true));
        }
        let end_of = |at: &ReviewCommentAt| match at {
            ReviewCommentAt::Lines { end, .. } => *end,
            _ => 0,
        };
        let Some(diff) = diff else {
            match body {
                Body::Loading => rows.push(row(
                    format!("w/{path}"),
                    ReviewRowOf::Loading {
                        path: path.to_owned(),
                    },
                )),
                Body::NoLines(why) => rows.push(row(
                    format!("n/{path}"),
                    ReviewRowOf::NoLines {
                        path: path.to_owned(),
                        why,
                    },
                )),
                Body::Diff(_) | Body::CommentsOnly => {}
            }
            on_lines.sort_by_key(|(_, at, _)| end_of(at));
            for (comment, at, _) in on_lines {
                rows.push(self.comment(path, comment, at, true));
            }
            return Vec::new();
        };

        let extra = self.extra_lines(path, diff);
        let shown: HashSet<u32> = diff
            .hunks
            .iter()
            .flat_map(|hunk| &hunk.lines)
            .filter(|line| line.kind() != ReviewLineKind::Removed && line.new_line > 0)
            .map(|line| line.new_line)
            .chain(extra.lines.keys().copied())
            .collect();
        let mut seating = Seating::default();
        let mut unseated = Vec::new();
        for (comment, at, removed) in on_lines {
            let ReviewCommentAt::Lines { start, end } = at else {
                continue;
            };
            match removed {
                Some(commit)
                    if removed_shown
                        .get(commit)
                        .is_some_and(|shown| shown.contains(&end)) =>
                {
                    seating.old.extend(start..=end);
                    seating.seated.push((Seat::Removed(end), comment, at));
                }
                None if shown.contains(&end) => {
                    seating.new.extend(start..=end);
                    seating.seated.push((Seat::New(end), comment, at));
                }
                _ => unseated.push((end, comment, at)),
            }
        }
        unseated.sort_by_key(|(end, ..)| *end);
        let mut unseated = unseated.into_iter().peekable();

        let hunks: Vec<(&ReviewHunk, Edges)> =
            diff.hunks.iter().map(|hunk| (hunk, edges(hunk))).collect();
        let mut hunk_rows: Vec<(usize, &[ReviewDefinition])> = Vec::new();
        let mut cursor = 0;
        for (index, (hunk, edge)) in hunks.iter().enumerate() {
            if gaps {
                let above = index.checked_sub(1).map(|at| hunks[at].0);
                let region = self.region(
                    path,
                    &extra,
                    cursor,
                    Some(edge.new_first),
                    above,
                    Some(hunk),
                    edge.delta,
                );
                for (drawn, seat) in region {
                    seating.push(self, rows, path, drawn, seat);
                }
            }
            while let Some((_, comment, at)) = unseated.next_if(|(end, ..)| *end < edge.new_first) {
                rows.push(self.comment(path, comment, at, true));
            }
            hunk_rows.push((rows.len(), &hunk.scope));
            rows.push(row(
                format!("h/{path}/{}/{}", hunk.old_start, hunk.new_start),
                ReviewRowOf::Hunk(ReviewHunkHead {
                    path: path.to_owned(),
                    old_commit: hunk.old_commit.clone(),
                    old_start: hunk.old_start,
                    new_start: hunk.new_start,
                    scope: hunk.scope.iter().map(|def| def.label.clone()).collect(),
                }),
            ));
            let texts: Vec<(LineKind, &str)> = hunk
                .lines
                .iter()
                .map(|line| (line_kind(line.kind()), line.text.as_str()))
                .collect();
            for (line, spans) in hunk.lines.iter().zip(changed_words(&texts)) {
                let drawn = code_line(path, line, &spans, false);
                let seat = match drawn.kind {
                    LineKind::Removed => Seat::Removed(line.old_line),
                    LineKind::Added | LineKind::Context => Seat::New(line.new_line),
                };
                let key = line_key(path, line);
                seating.push(
                    self,
                    rows,
                    path,
                    row(key, ReviewRowOf::Line(drawn)),
                    Some(seat),
                );
            }
            cursor = edge.new_last;
        }
        if gaps && let Some((last, edge)) = hunks.last() {
            for (drawn, seat) in self.region(
                path,
                &extra,
                cursor,
                None,
                Some(last),
                None,
                edge.trailing_delta,
            ) {
                seating.push(self, rows, path, drawn, seat);
            }
        }
        for (_, comment, at) in unseated {
            rows.push(self.comment(path, comment, at, true));
        }
        outline(&diff.outline, &hunk_rows)
    }

    /// The snapshot's lines between the shown lines `after` and `before`
    /// (none: the end of the file) that were read for more context, and the
    /// gaps left among them, each line with the seat a comment under it
    /// takes. `above` and `below` are the hunks either side; `delta` turns
    /// the snapshot's numbers here into the compared commit's.
    #[allow(clippy::too_many_arguments)]
    fn region(
        &self,
        path: &str,
        extra: &Extra,
        after: u32,
        before: Option<u32>,
        above: Option<&ReviewHunk>,
        below: Option<&ReviewHunk>,
        delta: i64,
    ) -> Vec<(ReviewRow, Option<Seat>)> {
        let mut out = Vec::new();
        let upper = before.map_or(u32::MAX, |before| before.saturating_sub(1));
        if after >= upper {
            return out;
        }
        let mut shown = after;
        for (&number, line) in extra.lines.range(after + 1..=upper) {
            if number > shown + 1 {
                out.push((
                    self.gap(path, extra, shown, Some(number), above, below, delta),
                    None,
                ));
            }
            let mut drawn = code_line(path, line, &[], true);
            drawn.kind = LineKind::Context;
            drawn.new_line = Some(number);
            drawn.old_line = u32::try_from(i64::from(number) + delta)
                .ok()
                .filter(|old| *old > 0);
            let key = format!("l/{path}/{}/{number}", drawn.old_line.unwrap_or(0));
            out.push((row(key, ReviewRowOf::Line(drawn)), Some(Seat::New(number))));
            shown = number;
        }
        match before {
            Some(before) if before > shown + 1 => {
                out.push((
                    self.gap(path, extra, shown, Some(before), above, below, delta),
                    None,
                ));
            }
            None if extra.end.is_none_or(|end| shown < end)
                && above.is_none_or(|hunk| !ends_the_file(hunk)) =>
            {
                out.push((
                    self.gap(path, extra, shown, None, above, below, delta),
                    None,
                ));
            }
            _ => {}
        }
        out
    }

    /// The gap between the shown lines `after` and `before` (none: the end
    /// of the file), below the hunk `above` and above the hunk `below`;
    /// `delta` turns the snapshot's numbers in it into the compared
    /// commit's.
    #[allow(clippy::too_many_arguments)]
    fn gap(
        &self,
        path: &str,
        extra: &Extra,
        after: u32,
        before: Option<u32>,
        above: Option<&ReviewHunk>,
        below: Option<&ReviewHunk>,
        delta: i64,
    ) -> ReviewRow {
        // A definition's edge as a line of the snapshot: one of the compared
        // commit's, enclosing only removed lines, has its unchanged edges in
        // the gap numbered there.
        let edge = |def: &ReviewDefinition, line: u32| match def.side() {
            ReviewSide::Compared => u32::try_from(i64::from(line) - delta)
                .ok()
                .filter(|line| *line > 0),
            ReviewSide::Snapshot | ReviewSide::Unspecified => Some(line),
        };
        let first = after + 1;
        let last = before.map_or(u32::MAX, |before| before - 1);
        let hidden = before.map(|before| before - first);
        let ask = |start: u32, end: u32, definition: Option<&ReviewDefinition>| ReviewContextAsk {
            path: path.to_owned(),
            commit: self.snapshot.to_owned(),
            start,
            end,
            definition: definition.map(|def| def.label.clone()),
        };
        let (up, down) = match hidden {
            Some(hidden) if hidden <= CONTEXT_STEP => (Some(ask(first, last, None)), None),
            _ => {
                let up = before.map(|before| {
                    match below
                        .and_then(|hunk| hunk.scope.last())
                        .and_then(|def| Some((def, edge(def, def.start_line)?)))
                    {
                        Some((def, start)) if start >= first && start <= last => ask(
                            start.max(before.saturating_sub(MOST_CONTEXT)),
                            last,
                            Some(def),
                        ),
                        _ => ask(before.saturating_sub(CONTEXT_STEP).max(first), last, None),
                    }
                });
                let down = (after > 0).then(|| {
                    match above
                        .and_then(|hunk| hunk.scope.last())
                        .and_then(|def| Some((def, edge(def, def.end_line)?)))
                    {
                        Some((def, end)) if end >= first && end <= last => {
                            ask(first, end.min(first + MOST_CONTEXT - 1), Some(def))
                        }
                        _ => ask(first, (first + CONTEXT_STEP - 1).min(last), None),
                    }
                });
                (up, down)
            }
        };
        let loading = self.asked.iter().any(|asked| {
            asked.path == path
                && asked.commit == self.snapshot
                && asked.start <= last
                && asked.end >= first
                && !extra.answered.contains(asked)
        });
        row(
            format!("g/{path}/{after}"),
            ReviewRowOf::Gap(ReviewGap {
                path: path.to_owned(),
                hidden,
                up,
                down,
                loading,
            }),
        )
    }

    /// The snapshot's lines read for more context of one file, outside its
    /// hunks, and where its last line is known to be.
    fn extra_lines(&self, path: &str, diff: &ReviewFileDiff) -> Extra<'a> {
        let spans: Vec<Edges> = diff.hunks.iter().map(edges).collect();
        let in_hunks = |number: u32| {
            spans
                .iter()
                .any(|edge| number >= edge.new_first && number <= edge.new_last)
        };
        let mut extra = Extra::default();
        for (ask, lines) in self.context {
            if ask.path != path || ask.commit != self.snapshot {
                continue;
            }
            extra.answered.insert(ask.clone());
            // An answer short of what was asked ran past the file's end.
            let last = lines.iter().map(|line| line.new_line).max();
            if last.is_none_or(|last| last < ask.end) {
                let end = last.unwrap_or(ask.start.saturating_sub(1));
                extra.end = Some(extra.end.map_or(end, |known| known.min(end)));
            }
            for line in lines {
                if line.new_line > 0 && !in_hunks(line.new_line) {
                    extra.lines.insert(line.new_line, line);
                }
            }
        }
        extra
    }

    fn comment(
        &self,
        path: &str,
        comment: &ReviewerComment,
        at: ReviewCommentAt,
        with_snippet: bool,
    ) -> ReviewRow {
        let snippet = if with_snippet {
            let texts: Vec<(LineKind, &str)> = comment
                .snippet
                .iter()
                .map(|line| (line_kind(line.kind()), line.text.as_str()))
                .collect();
            comment
                .snippet
                .iter()
                .zip(changed_words(&texts))
                .map(|(line, spans)| code_line(path, line, &spans, false))
                .collect()
        } else {
            Vec::new()
        };
        row(
            format!("c/{}", hex(&comment.id)),
            ReviewRowOf::Comment(ReviewCommentRow {
                id: comment.id.clone(),
                path: path.to_owned(),
                text: comment.text.clone(),
                state: match comment.round {
                    Some(round) => ReviewCommentState::Sent { round },
                    None => ReviewCommentState::Draft,
                },
                at,
                changed_near: comment
                    .placement
                    .as_ref()
                    .is_some_and(|placement| placement.changed_near),
                scope: comment.scope.iter().map(|def| def.label.clone()).collect(),
                snippet,
                folded: at == ReviewCommentAt::Outdated
                    && !self.opened.contains(comment.id.as_slice()),
            }),
        )
    }
}

/// A file's lines read for more context.
#[derive(Default)]
struct Extra<'a> {
    lines: BTreeMap<u32, &'a ReviewLine>,
    /// The file's last line, once an ask reached past it.
    end: Option<u32>,
    answered: HashSet<ReviewContextAsk>,
}

/// A hunk's first and last line in the snapshot, and how far its numbers
/// are from the compared commit's before it and after it.
struct Edges {
    new_first: u32,
    new_last: u32,
    /// Old number minus new number for the lines above the hunk.
    delta: i64,
    /// The same for the lines below it.
    trailing_delta: i64,
}

/// Whether `hunk` reaches the end of its file: the host gives each hunk
/// [`HUNK_CONTEXT`] lines after its last change wherever the file has them.
fn ends_the_file(hunk: &ReviewHunk) -> bool {
    let after = hunk
        .lines
        .iter()
        .rev()
        .take_while(|line| line.kind() == ReviewLineKind::Context)
        .count();
    (after as u32) < HUNK_CONTEXT
}

fn edges(hunk: &ReviewHunk) -> Edges {
    let new: Vec<u32> = hunk
        .lines
        .iter()
        .map(|line| line.new_line)
        .filter(|n| *n > 0)
        .collect();
    let old: Vec<u32> = hunk
        .lines
        .iter()
        .map(|line| line.old_line)
        .filter(|n| *n > 0)
        .collect();
    // A hunk with no line on one side starts after the line its start
    // names on that side, as git counts it.
    let new_first = new.first().copied().unwrap_or(hunk.new_start + 1);
    let new_last = new.last().copied().unwrap_or(hunk.new_start);
    let old_first = old.first().copied().unwrap_or(hunk.old_start + 1);
    let old_last = old.last().copied().unwrap_or(hunk.old_start);
    Edges {
        new_first,
        new_last,
        delta: i64::from(old_first) - i64::from(new_first),
        trailing_delta: i64::from(old_last) - i64::from(new_last),
    }
}

/// Where a comment's placement puts it: on lines of the snapshot, on
/// removed lines of the commit named beside them, on its file, or outdated.
/// One with no placement sits on the lines it was written on, or on its
/// file when it was written on the whole file.
fn placed(comment: &ReviewerComment) -> (ReviewCommentAt, Option<&str>) {
    let lines = |range: wire::experimental::ReviewLineRange| ReviewCommentAt::Lines {
        start: range.start,
        end: range.end.max(range.start),
    };
    match comment
        .placement
        .as_ref()
        .and_then(|placement| placement.at.as_ref())
    {
        Some(review_placement::At::Outdated(_)) => (ReviewCommentAt::Outdated, None),
        Some(review_placement::At::File(_)) => (ReviewCommentAt::File, None),
        Some(review_placement::At::Lines(range)) if range.start > 0 => (lines(*range), None),
        Some(review_placement::At::Compared(compared)) => match compared.lines {
            Some(range) if range.start > 0 => (lines(range), Some(compared.commit.as_str())),
            _ => (ReviewCommentAt::File, None),
        },
        _ => match comment.anchor.as_ref().and_then(|anchor| anchor.lines) {
            Some(range) if range.start > 0 => (lines(range), None),
            _ => (ReviewCommentAt::File, None),
        },
    }
}

fn code_line(path: &str, line: &ReviewLine, spans: &[Range<usize>], extra: bool) -> ReviewCodeLine {
    let kind = line_kind(line.kind());
    ReviewCodeLine {
        path: path.to_owned(),
        kind,
        old_line: (kind != LineKind::Added && line.old_line > 0).then_some(line.old_line),
        new_line: (kind != LineKind::Removed && line.new_line > 0).then_some(line.new_line),
        text: line.text.clone(),
        runs: runs(&line.text, &line.runs, spans),
        commented: false,
        extra,
    }
}

/// The text cut at every run's end and every changed span's edge, each
/// piece with its run's kind and whether a span covers it; neighbouring
/// pieces alike are joined. A run's length past the text, or ending inside
/// a character, is cut at the next character's start; text past the runs
/// is plain.
fn runs(
    text: &str,
    given: &[wire::experimental::ReviewRun],
    spans: &[Range<usize>],
) -> Vec<ReviewRun> {
    let boundary = |at: usize| {
        let mut at = at.min(text.len());
        while !text.is_char_boundary(at) {
            at += 1;
        }
        at
    };
    let mut kinds: Vec<(usize, TokenKind)> = Vec::new();
    let mut at = 0;
    for run in given {
        let end = boundary(at + run.len as usize);
        if end > at {
            kinds.push((end, token_kind(run.kind())));
            at = end;
        }
    }
    if at < text.len() {
        kinds.push((text.len(), TokenKind::Plain));
    }
    let mut cuts: Vec<usize> = kinds.iter().map(|(end, _)| *end).collect();
    for span in spans {
        cuts.push(boundary(span.start));
        cuts.push(boundary(span.end));
    }
    cuts.sort_unstable();
    cuts.dedup();
    let mut out: Vec<ReviewRun> = Vec::new();
    let mut start = 0;
    for cut in cuts {
        if cut <= start {
            continue;
        }
        let kind = kinds
            .iter()
            .find(|(end, _)| *end > start)
            .map_or(TokenKind::Plain, |(_, kind)| *kind);
        let changed = spans
            .iter()
            .any(|span| span.start <= start && start < span.end);
        match out.last_mut() {
            Some(last) if last.kind == kind && last.changed == changed => {
                last.text.push_str(&text[start..cut]);
            }
            _ => out.push(ReviewRun {
                text: text[start..cut].to_owned(),
                kind,
                changed,
            }),
        }
        start = cut;
    }
    out
}

/// The outline as a client lists it: each definition with its depth and
/// the first hunk whose changes it encloses.
fn outline(
    definitions: &[ReviewDefinition],
    hunks: &[(usize, &[ReviewDefinition])],
) -> Vec<ReviewOutlineEntry> {
    definitions
        .iter()
        .map(|def| {
            let mut depth = 0;
            let mut parent = def.parent;
            while let Some(at) = parent
                && depth < definitions.len() as u32
            {
                depth += 1;
                parent = definitions.get(at as usize).and_then(|def| def.parent);
            }
            let same = |other: &ReviewDefinition| {
                other.label == def.label
                    && other.start_line == def.start_line
                    && other.end_line == def.end_line
            };
            ReviewOutlineEntry {
                kind: definition_kind(def.kind()),
                label: def.label.clone(),
                depth,
                start_line: def.start_line,
                end_line: def.end_line,
                compared: def.side() == ReviewSide::Compared,
                row: hunks
                    .iter()
                    .find(|(_, scope)| scope.iter().any(same))
                    .map(|(row, _)| *row as u32),
            }
        })
        .collect()
}

fn row(key: String, of: ReviewRowOf) -> ReviewRow {
    ReviewRow { key, of }
}

fn line_key(path: &str, line: &ReviewLine) -> String {
    format!("l/{path}/{}/{}", line.old_line, line.new_line)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn line_kind(kind: ReviewLineKind) -> LineKind {
    match kind {
        ReviewLineKind::Added => LineKind::Added,
        ReviewLineKind::Removed => LineKind::Removed,
        ReviewLineKind::Context | ReviewLineKind::Unspecified => LineKind::Context,
    }
}

fn token_kind(kind: ReviewTokenKind) -> TokenKind {
    match kind {
        ReviewTokenKind::Plain | ReviewTokenKind::Unspecified => TokenKind::Plain,
        ReviewTokenKind::Keyword => TokenKind::Keyword,
        ReviewTokenKind::String => TokenKind::String,
        ReviewTokenKind::Number => TokenKind::Number,
        ReviewTokenKind::Type => TokenKind::Type,
        ReviewTokenKind::Function => TokenKind::Function,
        ReviewTokenKind::Comment => TokenKind::Comment,
        ReviewTokenKind::Inserted => TokenKind::Inserted,
        ReviewTokenKind::Deleted => TokenKind::Deleted,
    }
}

fn definition_kind(kind: ReviewDefinitionKind) -> DefinitionKind {
    match kind {
        ReviewDefinitionKind::Module => DefinitionKind::Module,
        ReviewDefinitionKind::Class => DefinitionKind::Class,
        ReviewDefinitionKind::Struct => DefinitionKind::Struct,
        ReviewDefinitionKind::Enum => DefinitionKind::Enum,
        ReviewDefinitionKind::Union => DefinitionKind::Union,
        ReviewDefinitionKind::Interface => DefinitionKind::Interface,
        ReviewDefinitionKind::Impl => DefinitionKind::Impl,
        ReviewDefinitionKind::TypeAlias => DefinitionKind::TypeAlias,
        ReviewDefinitionKind::Function => DefinitionKind::Function,
        ReviewDefinitionKind::Method => DefinitionKind::Method,
        ReviewDefinitionKind::Constructor => DefinitionKind::Constructor,
        ReviewDefinitionKind::Constant => DefinitionKind::Constant,
        ReviewDefinitionKind::Static => DefinitionKind::Static,
        ReviewDefinitionKind::Macro => DefinitionKind::Macro,
        ReviewDefinitionKind::Unspecified => DefinitionKind::Other,
    }
}
