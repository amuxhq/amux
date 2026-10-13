//! The review document: files, counts and hunks parsed from a diff's patch,
//! with the person's comments placed on their lines.

use schemars::JsonSchema;
use serde::Serialize;
use wire::{Diff, ReviewComment};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ReviewDoc {
    pub base: String,
    pub head: String,
    pub files: Vec<ReviewFile>,
    pub added: u32,
    pub removed: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ReviewFile {
    pub path: String,
    /// Set for a rename or copy.
    pub old_path: Option<String>,
    pub status: FileStatus,
    pub added: u32,
    pub removed: u32,
    pub binary: bool,
    pub hunks: Vec<Hunk>,
    /// Comments on the file as a whole.
    pub comments: Vec<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, JsonSchema)]
pub enum FileStatus {
    #[default]
    Modified,
    Added,
    Deleted,
    Renamed,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Hunk {
    pub header: String,
    pub lines: Vec<DiffLine>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct DiffLine {
    pub kind: LineKind,
    pub old_line: Option<u32>,
    pub new_line: Option<u32>,
    pub text: String,
    pub comments: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub enum LineKind {
    Context,
    Added,
    Removed,
}

/// Parses a `git diff` patch into files and hunks and places comments: on
/// the new-side line, on an old-side line for a removed one, or on the
/// file when the line is zero.
pub fn review_doc(diff: &Diff, patch: &str, comments: &[ReviewComment]) -> ReviewDoc {
    let base = match diff.base.as_ref().and_then(|base| base.base.as_ref()) {
        Some(wire::diff_base::Base::Branch(branch)) => branch.clone(),
        Some(wire::diff_base::Base::Head(_)) | None => String::new(),
    };
    let mut doc = ReviewDoc {
        base,
        head: diff.head.clone(),
        ..ReviewDoc::default()
    };
    let mut old_line = 0;
    let mut new_line = 0;
    for line in patch.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            let path = new_path(rest);
            doc.files.push(ReviewFile {
                path,
                ..ReviewFile::default()
            });
            continue;
        }
        let Some(file) = doc.files.last_mut() else {
            continue;
        };
        if file.hunks.is_empty() {
            if line.starts_with("new file mode") {
                file.status = FileStatus::Added;
            } else if line.starts_with("deleted file mode") {
                file.status = FileStatus::Deleted;
            } else if let Some(from) = line.strip_prefix("rename from ") {
                file.status = FileStatus::Renamed;
                file.old_path = Some(git_path(from));
            } else if let Some(to) = line.strip_prefix("rename to ") {
                file.path = git_path(to);
            } else if line.starts_with("Binary files ") {
                file.binary = true;
            }
        }
        if let Some(header) = line.strip_prefix("@@ ") {
            let (old, new) = hunk_starts(header);
            old_line = old;
            new_line = new;
            file.hunks.push(Hunk {
                header: line.to_owned(),
                lines: Vec::new(),
            });
            continue;
        }
        let Some(hunk) = file.hunks.last_mut() else {
            continue;
        };
        let (kind, text) = match line.chars().next() {
            Some('+') => (LineKind::Added, &line[1..]),
            Some('-') => (LineKind::Removed, &line[1..]),
            Some(' ') => (LineKind::Context, &line[1..]),
            Some('\\') => continue,
            _ => (LineKind::Context, line),
        };
        let (old, new) = match kind {
            LineKind::Added => {
                new_line += 1;
                file.added += 1;
                (None, Some(new_line - 1))
            }
            LineKind::Removed => {
                old_line += 1;
                file.removed += 1;
                (Some(old_line - 1), None)
            }
            LineKind::Context => {
                old_line += 1;
                new_line += 1;
                (Some(old_line - 1), Some(new_line - 1))
            }
        };
        hunk.lines.push(DiffLine {
            kind,
            old_line: old,
            new_line: new,
            text: text.to_owned(),
            comments: Vec::new(),
        });
    }
    for comment in comments {
        let Some(file) = doc.files.iter_mut().find(|file| file.path == comment.path) else {
            continue;
        };
        if comment.line == 0 && comment.old_line == 0 {
            file.comments.push(comment.text.clone());
            continue;
        }
        let target = file
            .hunks
            .iter_mut()
            .flat_map(|hunk| hunk.lines.iter_mut())
            .find(|line| {
                if comment.line > 0 {
                    line.new_line == Some(comment.line)
                } else {
                    line.kind == LineKind::Removed && line.old_line == Some(comment.old_line)
                }
            });
        match target {
            Some(line) => line.comments.push(comment.text.clone()),
            None => file.comments.push(comment.text.clone()),
        }
    }
    doc.added = doc.files.iter().map(|file| file.added).sum();
    doc.removed = doc.files.iter().map(|file| file.removed).sum();
    doc
}

/// The new side's path from a `diff --git a/… b/…` header, as it is on
/// disk: git quotes it when it must (see [`git_path`]), else it follows the
/// last " b/". A rename names its paths again on their own lines.
fn new_path(header: &str) -> String {
    match header.rfind(" \"b/") {
        Some(at) => {
            let path = git_path(&header[at + 1..]);
            path.strip_prefix("b/").unwrap_or(&path).to_owned()
        }
        None => header
            .rsplit_once(" b/")
            .map(|(_, path)| path)
            .unwrap_or(header)
            .to_owned(),
    }
}

/// A path as git writes it in a patch. Git quotes a path holding a quote,
/// a backslash, a control character or (by default) any byte past ASCII,
/// writing those as C escapes and octal bytes (`"we ird\"\303\251.txt"`);
/// unquoted, the path is the text as it is.
fn git_path(text: &str) -> String {
    let Some(quoted) = text.strip_prefix('"') else {
        return text.to_owned();
    };
    let mut bytes = Vec::new();
    let mut chars = quoted.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return String::from_utf8_lossy(&bytes).into_owned(),
            '\\' => match chars.next() {
                Some(digit @ '0'..='7') => {
                    let mut value = digit as u32 - '0' as u32;
                    for _ in 0..2 {
                        let rest = chars.as_str();
                        match rest.chars().next() {
                            Some(next @ '0'..='7') => {
                                value = value * 8 + (next as u32 - '0' as u32);
                                chars.next();
                            }
                            _ => break,
                        }
                    }
                    bytes.push(value as u8);
                }
                Some(escaped) => {
                    let byte = match escaped {
                        'a' => 0x07,
                        'b' => 0x08,
                        'f' => 0x0c,
                        'n' => b'\n',
                        'r' => b'\r',
                        't' => b'\t',
                        'v' => 0x0b,
                        other => {
                            let mut buf = [0; 4];
                            bytes.extend_from_slice(other.encode_utf8(&mut buf).as_bytes());
                            continue;
                        }
                    };
                    bytes.push(byte);
                }
                None => break,
            },
            c => {
                let mut buf = [0; 4];
                bytes.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            }
        }
    }
    // No closing quote: the text was not a quoted path after all.
    text.to_owned()
}

/// "-12,7 +12,9 @@ …" → (12, 12).
fn hunk_starts(header: &str) -> (u32, u32) {
    let mut old = 1;
    let mut new = 1;
    for part in header.split_whitespace() {
        let number = |s: &str| s.split(',').next().and_then(|n| n.parse().ok());
        if let Some(rest) = part.strip_prefix('-') {
            old = number(rest).unwrap_or(old);
        } else if let Some(rest) = part.strip_prefix('+') {
            new = number(rest).unwrap_or(new);
        } else if part == "@@" {
            break;
        }
    }
    (old, new)
}
