//! A sent review as the agent reads it: a text attachment holding, for each
//! comment, where it sits and the code it is on before the person's words,
//! and the command that shows the snapshot the line numbers refer to; with
//! the review's entry in the attachment's `experimental` map, so a client
//! draws it as a review and the agent's host marks its comments sent.
//!
//! The person's summary is the message the attachment is sent in, so the
//! text holds only the comments. It reads like this:
//!
//! ````text
//! 1 comment from a review of your changes.
//!
//! ## src/store.rs, lines 12–13, in impl Store › fn open
//!
//! ```diff
//!      let path = dir.join("store");
//! -    if size < limit && open {
//! +    if size <= limit && open {
//! ```
//!
//! Keep the strict comparison.
//!
//! ---
//! Line numbers are those of snapshot 4f2a…, a commit of the working tree
//! taken for this review and kept outside the repository, so the files may
//! have changed since. …
//! ````
//!
//! A comment on lines the change removed names them by the commit they were
//! removed from: "lines 4–5 of commit 9c1e…".
//!
//! The command the text closes with diffs the working tree against the
//! snapshot, untracked files included, and lists the files made since,
//! which the snapshot does not hold. It reads the snapshot through an index
//! of it kept with the review and the review's own object store, so it
//! writes nothing, to the repository or anywhere else: it runs inside a
//! sandbox that allows no writes at all, as Codex's read-only one.

use wire::attachment::Of;
use wire::experimental::review_placement::At;
use wire::experimental::{
    ReviewDefinition, ReviewLine, ReviewLineKind, ReviewLineRange, ReviewSentComment,
    ReviewSummary, ReviewerComment, SENT_REVIEW, SentReview,
};
use wire::{Attachment, InlineText};

/// The name of a sent review's text attachment.
pub const NAME: &str = "review";

/// The review `attachment` says it sends; none for any other attachment.
pub fn entry(attachment: &Attachment) -> Option<SentReview> {
    SENT_REVIEW.get(&attachment.experimental)
}

/// The attachment that sends `comments` from the review `summary`
/// describes: the text the agent reads, and the entry naming exactly those
/// comments at the revision and on the anchor the text was written from,
/// the round the sender expects to close, and the snapshot the text's lines
/// are on with the base commit it was shown against, which the round is
/// recorded on.
pub fn attachment(summary: &ReviewSummary, comments: &[ReviewerComment]) -> Attachment {
    let mut attachment = Attachment {
        of: Some(Of::Text(InlineText {
            name: NAME.into(),
            text: text(summary, comments),
        })),
        ..Default::default()
    };
    SENT_REVIEW.put(
        &mut attachment.experimental,
        &SentReview {
            review_id: summary.id.clone(),
            round: u32::try_from(summary.rounds.len())
                .unwrap_or(u32::MAX)
                .saturating_add(1),
            snapshot: summary.snapshot.clone(),
            base_commit: summary.base_commit.clone(),
            comments: comments
                .iter()
                .map(|comment| ReviewSentComment {
                    id: comment.id.clone(),
                    revision: comment.revision,
                    anchor: comment.anchor.clone(),
                })
                .collect(),
        },
    );
    attachment
}

/// What the agent reads for `comments` of the review `summary` describes,
/// made on its snapshot, a commit in the snapshot store of the repository
/// whose top is the summary's root: each comment's path from the top, its
/// lines, the definition it is in, the code it is on in diff form and then
/// the person's words; last, what the line numbers refer to and the command
/// that diffs the working tree against the snapshot.
pub fn text(summary: &ReviewSummary, comments: &[ReviewerComment]) -> String {
    let (root, snapshot) = (summary.root.as_str(), summary.snapshot.as_str());
    let mut out = format!(
        "{} from a review of your changes.\n",
        counted(comments.len(), "comment")
    );
    let mut elsewhere = false;
    for comment in comments {
        let (commit, path, lines) = position(comment, snapshot);
        out.push_str("\n## ");
        out.push_str(path);
        match lines {
            Some(lines) => out.push_str(&format!(", {}", range(lines))),
            None => out.push_str(", the whole file"),
        }
        if commit != snapshot {
            elsewhere = true;
            out.push_str(&format!(" of commit {commit}"));
        }
        if !comment.scope.is_empty() {
            out.push_str(", in ");
            out.push_str(&scope(&comment.scope));
        }
        out.push('\n');
        if !comment.snippet.is_empty() {
            out.push('\n');
            out.push_str(&snippet(&comment.snippet));
        }
        let words = comment.text.trim_end();
        if !words.trim_start().is_empty() {
            out.push('\n');
            out.push_str(words);
            out.push('\n');
        }
    }
    out.push_str(&format!(
        "\n---\nLine numbers are those of snapshot {snapshot}{}, a commit of the working tree \
         taken for this review and kept outside the repository, so the files may have changed \
         since. Paths are relative to the repository's top, {root}.",
        if elsewhere {
            " unless a comment names another commit"
        } else {
            ""
        },
    ));
    if !summary.index.is_empty() {
        out.push_str(&format!(
            " To see what has changed since the snapshot, untracked files included, run this; \
             it writes nothing, and after the diff it lists the files made since the \
             snapshot:\n\n{}",
            fenced(
                &command(root, &summary.objects, &summary.index, snapshot),
                "sh"
            ),
        ));
    } else {
        out.push('\n');
    }
    out
}

/// The command that diffs the working tree of the repository at `root`
/// against `snapshot`, untracked files included, and then lists the files
/// made since, which the snapshot does not hold: reading the snapshot's
/// files through its index `index` and its objects from `objects`, from
/// any folder, and writing nothing. The snapshot's own index stands in for
/// the repository's, so a file untracked when the snapshot was taken is
/// compared like any other. git's diff writes the file times it finds back
/// into the index it read; it is told not to (optional locks alone do not
/// stop it), so that index is left as the host wrote it too.
pub fn command(root: &str, objects: &str, index: &str, snapshot: &str) -> String {
    let (root, index) = (quoted(root), quoted(index));
    format!(
        "GIT_INDEX_FILE={index} GIT_ALTERNATE_OBJECT_DIRECTORIES={} \
         git --no-optional-locks -c diff.autoRefreshIndex=false -C {root} diff {} && \
         GIT_INDEX_FILE={index} git --no-optional-locks -C {root} ls-files --others \
         --exclude-standard",
        quoted(objects),
        quoted(snapshot)
    )
}

/// The commit a comment's line numbers belong to, the file it is on, and
/// its lines there; no lines for one on the whole file. A comment placed on
/// lines or its file is on the snapshot; one on removed lines is on the
/// commit they were removed from; one with no place on the snapshot keeps
/// the lines it was written on.
fn position<'a>(
    comment: &'a ReviewerComment,
    snapshot: &'a str,
) -> (&'a str, &'a str, Option<ReviewLineRange>) {
    let whole = |lines: ReviewLineRange| (lines.start != 0).then_some(lines);
    let anchor = comment.anchor.as_ref();
    let anchor_path = anchor.map_or("", |anchor| anchor.path.as_str());
    let placement = comment.placement.as_ref();
    // Where the snapshot has the file, a rename followed.
    let placed_path = placement
        .map(|placement| placement.path.as_str())
        .filter(|path| !path.is_empty())
        .unwrap_or(anchor_path);
    match placement.and_then(|placement| placement.at.as_ref()) {
        Some(At::Lines(lines)) => (snapshot, placed_path, whole(*lines)),
        Some(At::File(_)) => (snapshot, placed_path, None),
        Some(At::Compared(compared)) => (
            compared.commit.as_str(),
            anchor_path,
            compared.lines.and_then(whole),
        ),
        Some(At::Outdated(_)) | None => {
            let commit = anchor
                .map(|anchor| anchor.commit.as_str())
                .filter(|commit| !commit.is_empty())
                .unwrap_or(snapshot);
            (
                commit,
                anchor_path,
                anchor.and_then(|anchor| anchor.lines).and_then(whole),
            )
        }
    }
}

fn range(lines: ReviewLineRange) -> String {
    if lines.end <= lines.start {
        format!("line {}", lines.start)
    } else {
        format!("lines {}–{}", lines.start, lines.end)
    }
}

fn scope(definitions: &[ReviewDefinition]) -> String {
    definitions
        .iter()
        .map(|definition| {
            if definition.label.is_empty() {
                definition.name.as_str()
            } else {
                definition.label.as_str()
            }
        })
        .collect::<Vec<_>>()
        .join(" › ")
}

/// The snippet as a diff: each line marked as git marks it.
fn snippet(lines: &[ReviewLine]) -> String {
    let body: String = lines
        .iter()
        .map(|line| {
            let mark = match line.kind() {
                ReviewLineKind::Added => '+',
                ReviewLineKind::Removed => '-',
                ReviewLineKind::Context | ReviewLineKind::Unspecified => ' ',
            };
            format!("{mark}{}\n", line.text)
        })
        .collect();
    fenced(&body, "diff")
}

/// `body` in a code fence longer than any run of backticks inside it.
fn fenced(body: &str, language: &str) -> String {
    let mut longest = 0;
    let mut run = 0;
    for character in body.chars() {
        run = if character == '`' { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    let fence = "`".repeat(longest.max(2) + 1);
    let body = body.strip_suffix('\n').unwrap_or(body);
    format!("{fence}{language}\n{body}\n{fence}\n")
}

/// `value` as one shell word.
fn quoted(value: &str) -> String {
    let plain = !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-+@%:,=".contains(c));
    if plain {
        value.to_owned()
    } else {
        format!("'{}'", value.replace('\'', r"'\''"))
    }
}

fn counted(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}
