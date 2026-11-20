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

use wire::attachment::Of;
use wire::experimental::review_placement::At;
use wire::experimental::{
    ReviewDefinition, ReviewLine, ReviewLineKind, ReviewLineRange, ReviewSummary, ReviewerComment,
    SENT_REVIEW, SentReview,
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
/// comments, the round the sender expects to close, and the snapshot they
/// were made on.
pub fn attachment(summary: &ReviewSummary, comments: &[ReviewerComment]) -> Attachment {
    let mut attachment = Attachment {
        of: Some(Of::Text(InlineText {
            name: NAME.into(),
            text: text(&summary.root, &summary.objects, &summary.snapshot, comments),
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
            comment_ids: comments.iter().map(|comment| comment.id.clone()).collect(),
        },
    );
    attachment
}

/// What the agent reads for `comments` made on `snapshot`, a commit in the
/// snapshot store `objects` of the repository whose top is `root`: each
/// comment's path from the top, its lines, the definition it is in, the
/// code it is on in diff form and then the person's words; last, what the
/// line numbers refer to and the command that diffs the working tree
/// against the snapshot.
pub fn text(root: &str, objects: &str, snapshot: &str, comments: &[ReviewerComment]) -> String {
    let mut out = format!(
        "{} from a review of your changes.\n",
        counted(comments.len(), "comment")
    );
    let mut elsewhere = false;
    for comment in comments {
        let anchor = comment.anchor.clone().unwrap_or_default();
        let (commit, lines) = position(comment, snapshot);
        out.push_str("\n## ");
        out.push_str(&anchor.path);
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
         since. Paths are relative to the repository's top, {root}. To see what has changed \
         since the snapshot, run:\n\n{}",
        if elsewhere {
            " unless a comment names another commit"
        } else {
            ""
        },
        fenced(&command(root, objects, snapshot), "sh"),
    ));
    out
}

/// The command that diffs the working tree of the repository at `root`
/// against `snapshot`, reading the snapshot's objects from `objects`, from
/// any folder.
pub fn command(root: &str, objects: &str, snapshot: &str) -> String {
    format!(
        "GIT_ALTERNATE_OBJECT_DIRECTORIES={} git -C {} diff {}",
        quoted(objects),
        quoted(root),
        quoted(snapshot)
    )
}

/// The commit a comment's line numbers belong to, and its lines there; no
/// lines for one on the whole file. A placed comment is on the snapshot;
/// one with no place on it keeps the lines it was written on.
fn position<'a>(
    comment: &'a ReviewerComment,
    snapshot: &'a str,
) -> (&'a str, Option<ReviewLineRange>) {
    let whole = |lines: ReviewLineRange| (lines.start != 0).then_some(lines);
    match comment
        .placement
        .as_ref()
        .and_then(|placement| placement.at)
    {
        Some(At::Lines(lines)) => (snapshot, whole(lines)),
        Some(At::File(_)) => (snapshot, None),
        Some(At::Outdated(_)) | None => {
            let anchor = comment.anchor.as_ref();
            let commit = anchor
                .map(|anchor| anchor.commit.as_str())
                .filter(|commit| !commit.is_empty())
                .unwrap_or(snapshot);
            (
                commit,
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
