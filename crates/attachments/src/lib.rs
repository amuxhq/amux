//! Attachments as positioned text and as the element the model sees.
//!
//! Inputs and items carry one shape: text with one U+FFFC placeholder per
//! attachment and the ordered, typed list of attachments. The
//! `<amux-attachment>` element exists only inside a provider's
//! conversation: the interpreter formats it at each placeholder on the way
//! in, and parses it back out of whatever the provider reflects or the model
//! writes. No client ever sees the element.
//!
//! ```text
//! <amux-attachment kind="image" hash="sha256:…" name="shot.png" mime="image/png" size="1204" path="/…/blobs/…"/>
//! <amux-attachment kind="file" hash="sha256:…" name="trace.json" mime="application/json" size="8192" path="/…"/>
//! <amux-attachment kind="text" name="pasted-1">the pasted text…</amux-attachment>
//! <amux-attachment kind="review" hash="sha256:…" name="review.diff" mime="text/x-diff" size="512" base="working-tree" head="4f2a9c1" comments="1" path="/…">…</amux-attachment>
//! ```
//!
//! `path` is where the bytes are on the host that formats the element; the
//! parser accepts and ignores it, since a blob is identified by its hash.
//! Attribute values and bodies escape XML-significant characters. A
//! candidate element that does not parse stays in the text exactly as
//! written and is reported; the parser never drops text.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use wire::attachment::Of;
use wire::diff_base::Base;
use wire::{Attachment, BlobRef, Diff, DiffBase, Empty, InlineText, Review, ReviewComment};

/// Marks one attachment's position in text.
pub const PLACEHOLDER: char = '\u{FFFC}';
/// What a placeholder found where none belongs is replaced with.
pub const REPLACEMENT: char = '\u{FFFD}';

const OPEN: &str = "<amux-attachment";
const CLOSE: &str = "</amux-attachment>";
const HASH_PREFIX: &str = "sha256:";
const WORKING_TREE: &str = "working-tree";
const BRANCH_PREFIX: &str = "branch:";

/// Text with one placeholder per attachment, and the attachments in order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Positioned {
    pub text: String,
    pub attachments: Vec<Attachment>,
}

/// Why a positioned value cannot be handed to an agent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShapeError {
    /// The placeholder count must equal the attachment count.
    CountMismatch {
        placeholders: usize,
        attachments: usize,
    },
    /// An attachment with no arm set.
    Empty { index: usize },
    /// A blob reference whose hash is not a SHA-256 digest.
    BadHash { index: usize },
}

impl fmt::Display for ShapeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CountMismatch {
                placeholders,
                attachments,
            } => write!(
                f,
                "{placeholders} attachment placeholder(s) for {attachments} attachment(s)"
            ),
            Self::Empty { index } => write!(f, "attachment {index} is empty"),
            Self::BadHash { index } => write!(f, "attachment {index} has no SHA-256 hash"),
        }
    }
}

impl std::error::Error for ShapeError {}

/// Checks the shape every input and item must have before it reaches an
/// agent: one placeholder per attachment, each attachment set, each blob
/// named by a SHA-256 digest.
pub fn validate(p: &Positioned) -> Result<(), ShapeError> {
    let placeholders = p.text.chars().filter(|&c| c == PLACEHOLDER).count();
    if placeholders != p.attachments.len() {
        return Err(ShapeError::CountMismatch {
            placeholders,
            attachments: p.attachments.len(),
        });
    }
    for (index, attachment) in p.attachments.iter().enumerate() {
        let blob = match &attachment.of {
            None => return Err(ShapeError::Empty { index }),
            Some(Of::Image(blob) | Of::File(blob)) => Some(blob),
            Some(Of::Review(review)) => Some(
                review
                    .diff
                    .as_ref()
                    .and_then(|diff| diff.patch.as_ref())
                    .ok_or(ShapeError::BadHash { index })?,
            ),
            Some(Of::Text(_)) => None,
        };
        if blob.is_some_and(|blob| blob.hash.len() != 32) {
            return Err(ShapeError::BadHash { index });
        }
    }
    Ok(())
}

/// One piece of formatted text: prose, or an attachment's element with the
/// local path of its bytes, so an interpreter can put a native image block
/// at the element's position where its provider has one.
#[derive(Clone, Debug, PartialEq)]
pub enum Piece<'a> {
    Text(&'a str),
    Element {
        element: String,
        attachment: &'a Attachment,
        path: Option<PathBuf>,
    },
}

/// The formatted pieces of `p`, with blobs located in `dir` (the agent's
/// blobs directory).
pub fn pieces<'a>(p: &'a Positioned, dir: &Path) -> Vec<Piece<'a>> {
    let mut pieces = Vec::new();
    let mut attachments = p.attachments.iter();
    for (index, prose) in p.text.split(PLACEHOLDER).enumerate() {
        if index > 0
            && let Some(attachment) = attachments.next()
        {
            let path = blob_path(attachment, dir);
            pieces.push(Piece::Element {
                element: element(attachment, path.as_deref()),
                attachment,
                path,
            });
        }
        if !prose.is_empty() {
            pieces.push(Piece::Text(prose));
        }
    }
    pieces
}

/// The model-facing text: each placeholder replaced by its attachment's
/// element, with blob paths under `dir`.
pub fn format(p: &Positioned, dir: &Path) -> String {
    pieces(p, dir)
        .into_iter()
        .map(|piece| match piece {
            Piece::Text(text) => text.to_owned(),
            Piece::Element { element, .. } => element,
        })
        .collect()
}

/// Where an attachment's bytes are in `dir` (an agent's blobs directory),
/// for the kinds whose bytes are a blob.
pub fn blob_path(attachment: &Attachment, dir: &Path) -> Option<PathBuf> {
    blob_of(attachment).map(|blob| dir.join(hex(&blob.hash)))
}

fn blob_of(attachment: &Attachment) -> Option<&BlobRef> {
    match attachment.of.as_ref()? {
        Of::Image(blob) | Of::File(blob) => Some(blob),
        Of::Review(review) => review.diff.as_ref()?.patch.as_ref(),
        Of::Text(_) => None,
    }
}

/// One attachment's canonical element.
pub fn element(attachment: &Attachment, path: Option<&Path>) -> String {
    let mut out = String::from(OPEN);
    match &attachment.of {
        None => return String::new(),
        Some(Of::Image(blob)) => {
            attribute(&mut out, "kind", "image");
            blob_attributes(&mut out, blob);
        }
        Some(Of::File(blob)) => {
            attribute(&mut out, "kind", "file");
            blob_attributes(&mut out, blob);
        }
        Some(Of::Text(text)) => {
            attribute(&mut out, "kind", "text");
            attribute(&mut out, "name", &text.name);
            out.push('>');
            out.push_str(&escape(&text.text, false));
            out.push_str(CLOSE);
            return out;
        }
        Some(Of::Review(review)) => {
            attribute(&mut out, "kind", "review");
            let diff = review.diff.clone().unwrap_or_default();
            blob_attributes(&mut out, &diff.patch.unwrap_or_default());
            match diff.base.and_then(|base| base.base) {
                Some(Base::Head(_)) => attribute(&mut out, "base", WORKING_TREE),
                Some(Base::Branch(branch)) => {
                    attribute(&mut out, "base", &format!("{BRANCH_PREFIX}{branch}"));
                }
                None => {}
            }
            attribute(&mut out, "head", &diff.head);
            if let Some(merge_base) = &diff.merge_base {
                attribute(&mut out, "merge-base", merge_base);
            }
            attribute(&mut out, "comments", &review.comments.len().to_string());
            if let Some(path) = path {
                attribute(&mut out, "path", &path.to_string_lossy());
            }
            out.push('>');
            out.push_str(&escape(&review_body(&review.comments), false));
            out.push_str(CLOSE);
            return out;
        }
    }
    if let Some(path) = path {
        attribute(&mut out, "path", &path.to_string_lossy());
    }
    out.push_str("/>");
    out
}

fn blob_attributes(out: &mut String, blob: &BlobRef) {
    attribute(out, "hash", &format!("{HASH_PREFIX}{}", hex(&blob.hash)));
    attribute(out, "name", &blob.name);
    attribute(out, "mime", &blob.mime);
    attribute(out, "size", &blob.size.to_string());
}

fn attribute(out: &mut String, name: &str, value: &str) {
    out.push(' ');
    out.push_str(name);
    out.push_str("=\"");
    out.push_str(&escape(value, true));
    out.push('"');
}

/// A review's comments, each framed by byte counts so no path or comment
/// text can be mistaken for the next heading:
/// `## path-bytes=N line=N old-line=N text-bytes=N`, a newline, the path,
/// the text, a newline.
fn review_body(comments: &[ReviewComment]) -> String {
    let mut body = String::new();
    for comment in comments {
        body.push_str(&format!(
            "## path-bytes={} line={} old-line={} text-bytes={}\n{}{}\n",
            comment.path.len(),
            comment.line,
            comment.old_line,
            comment.text.len(),
            comment.path,
            comment.text,
        ));
    }
    body
}

fn parse_review_body(body: &str) -> Option<Vec<ReviewComment>> {
    let mut comments = Vec::new();
    let mut rest = body;
    while !rest.is_empty() {
        let (heading, after) = rest.strip_prefix("## ")?.split_once('\n')?;
        let mut fields = BTreeMap::new();
        for field in heading.split(' ') {
            let (name, value) = field.split_once('=')?;
            if fields.insert(name, value.parse::<usize>().ok()?).is_some() {
                return None;
            }
        }
        if fields.len() != 4 {
            return None;
        }
        let path_bytes = *fields.get("path-bytes")?;
        let text_bytes = *fields.get("text-bytes")?;
        let path = after.get(..path_bytes)?;
        let after = &after[path_bytes..];
        let text = after.get(..text_bytes)?;
        rest = after[text_bytes..].strip_prefix('\n')?;
        comments.push(ReviewComment {
            path: path.to_owned(),
            line: u32::try_from(*fields.get("line")?).ok()?,
            old_line: u32::try_from(*fields.get("old-line")?).ok()?,
            text: text.to_owned(),
        });
    }
    Some(comments)
}

/// Why a candidate element stayed text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Problem {
    /// No closing `>` for the opening tag, or no closing element for a body.
    Unterminated,
    /// Another element opens inside this one's body.
    Nested,
    /// `kind` is missing or not image, file, text or review.
    UnknownKind,
    /// An image, file or review with no `hash`.
    MissingHash,
    /// A `hash` that is not `sha256:` and 64 hex digits.
    BadHash,
    /// An attribute this kind does not have, or one given twice.
    UnknownAttribute(String),
    /// A required attribute is absent or an attribute value does not parse.
    BadAttribute(String),
    /// A body that is not valid for its kind, or a character reference
    /// that does not decode.
    BadBody,
    /// A self-closing element of a kind that needs a body, or the reverse.
    BadForm,
    /// A U+FFFC in the text itself, which would read as an attachment
    /// position; it is replaced with U+FFFD.
    StrayPlaceholder,
}

/// A problem at a byte offset of the parsed text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reported {
    pub at: usize,
    pub problem: Problem,
}

/// What parsing found: always a well-shaped value, plus what stayed text.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Parsed {
    pub positioned: Positioned,
    pub reported: Vec<Reported>,
}

/// Parses elements out of anything the provider reflects or the model
/// writes. Each valid element becomes a placeholder and an attachment;
/// every candidate that does not parse stays in the text as written and is
/// reported, and scanning resumes just after its opening marker so a later
/// valid element is still found.
pub fn parse(text: &str) -> Parsed {
    let mut parsed = Parsed::default();
    let mut prose_start = 0;
    let mut search_start = 0;
    while let Some(relative) = text[search_start..].find(OPEN) {
        let start = search_start + relative;
        match parse_element(text, start) {
            Ok((attachment, end)) => {
                push_prose(&mut parsed, text, prose_start, start);
                parsed.positioned.text.push(PLACEHOLDER);
                parsed.positioned.attachments.push(attachment);
                prose_start = end;
                search_start = end;
            }
            Err(problem) => {
                parsed.reported.push(Reported { at: start, problem });
                search_start = start + OPEN.len();
            }
        }
    }
    push_prose(&mut parsed, text, prose_start, text.len());
    parsed.reported.sort_by_key(|reported| reported.at);
    parsed
}

fn push_prose(parsed: &mut Parsed, text: &str, start: usize, end: usize) {
    for (offset, character) in text[start..end].char_indices() {
        if character == PLACEHOLDER {
            parsed.positioned.text.push(REPLACEMENT);
            parsed.reported.push(Reported {
                at: start + offset,
                problem: Problem::StrayPlaceholder,
            });
        } else {
            parsed.positioned.text.push(character);
        }
    }
}

fn parse_element(text: &str, start: usize) -> Result<(Attachment, usize), Problem> {
    let opening_end = tag_end(text, start).ok_or(Problem::Unterminated)?;
    let opening = &text[start..opening_end];
    let (self_closing, mut attributes) = parse_opening(opening)?;
    let kind = attributes.remove("kind").ok_or(Problem::UnknownKind)?;
    // The host-local path is informational; the hash names the bytes.
    attributes.remove("path");

    let (body, end) = if self_closing {
        (None, opening_end)
    } else {
        let close = text[opening_end..]
            .find(CLOSE)
            .ok_or(Problem::Unterminated)?
            + opening_end;
        let raw = &text[opening_end..close];
        if raw.contains(OPEN) {
            return Err(Problem::Nested);
        }
        (
            Some(decode(raw).ok_or(Problem::BadBody)?),
            close + CLOSE.len(),
        )
    };

    let of = match (kind.as_str(), body) {
        ("image" | "file", None) => {
            let blob = take_blob(&mut attributes)?;
            if kind == "image" {
                Of::Image(blob)
            } else {
                Of::File(blob)
            }
        }
        ("text", Some(body)) => Of::Text(InlineText {
            name: attributes.remove("name").unwrap_or_default(),
            text: body,
        }),
        ("review", Some(body)) => {
            let patch = take_blob(&mut attributes)?;
            let base = match attributes.remove("base") {
                None => None,
                Some(base) if base == WORKING_TREE => Some(Base::Head(Empty {})),
                Some(base) => Some(Base::Branch(
                    base.strip_prefix(BRANCH_PREFIX)
                        .ok_or_else(|| Problem::BadAttribute("base".into()))?
                        .to_owned(),
                )),
            };
            let head = attributes.remove("head").unwrap_or_default();
            let merge_base = attributes.remove("merge-base");
            let count: usize = attributes
                .remove("comments")
                .and_then(|count| count.parse().ok())
                .ok_or_else(|| Problem::BadAttribute("comments".into()))?;
            let comments = parse_review_body(&body).ok_or(Problem::BadBody)?;
            if comments.len() != count {
                return Err(Problem::BadBody);
            }
            Of::Review(Review {
                diff: Some(Diff {
                    patch: Some(patch),
                    base: base.map(|base| DiffBase { base: Some(base) }),
                    head,
                    merge_base,
                    // The patch names every file; the form does not repeat them.
                    files: Vec::new(),
                }),
                comments,
            })
        }
        ("image" | "file" | "text" | "review", _) => return Err(Problem::BadForm),
        _ => return Err(Problem::UnknownKind),
    };
    if let Some(name) = attributes.into_keys().next() {
        return Err(Problem::UnknownAttribute(name));
    }
    Ok((Attachment { of: Some(of) }, end))
}

fn take_blob(attributes: &mut BTreeMap<String, String>) -> Result<BlobRef, Problem> {
    let hash = attributes.remove("hash").ok_or(Problem::MissingHash)?;
    let hash = parse_hash(&hash).ok_or(Problem::BadHash)?;
    let size = match attributes.remove("size") {
        None => 0,
        Some(size) => size
            .parse()
            .map_err(|_| Problem::BadAttribute("size".into()))?,
    };
    Ok(BlobRef {
        hash,
        name: attributes.remove("name").unwrap_or_default(),
        mime: attributes.remove("mime").unwrap_or_default(),
        size,
    })
}

fn tag_end(text: &str, start: usize) -> Option<usize> {
    let mut quoted = false;
    for (relative, character) in text[start..].char_indices() {
        match character {
            '"' => quoted = !quoted,
            '>' if !quoted => return Some(start + relative + 1),
            _ => {}
        }
    }
    None
}

fn parse_opening(opening: &str) -> Result<(bool, BTreeMap<String, String>), Problem> {
    let inner = &opening[OPEN.len()..opening.len() - 1];
    // `<amux-attachmentx …>` is some other tag.
    if inner
        .chars()
        .next()
        .is_some_and(|c| !c.is_ascii_whitespace() && c != '/')
    {
        return Err(Problem::UnknownKind);
    }
    let trimmed = inner.trim_end();
    let (self_closing, mut rest) = match trimmed.strip_suffix('/') {
        Some(rest) => (true, rest),
        None => (false, trimmed),
    };
    let mut attributes = BTreeMap::new();
    loop {
        rest = rest.trim_start();
        if rest.is_empty() {
            break;
        }
        let name_len = rest
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
            .unwrap_or(rest.len());
        if name_len == 0 {
            return Err(Problem::BadAttribute(rest.chars().take(1).collect()));
        }
        let name = &rest[..name_len];
        let bad = || Problem::BadAttribute(name.to_owned());
        rest = rest[name_len..].trim_start();
        rest = rest.strip_prefix('=').ok_or_else(bad)?.trim_start();
        rest = rest.strip_prefix('"').ok_or_else(bad)?;
        let value_end = rest.find('"').ok_or_else(bad)?;
        let value = decode(&rest[..value_end]).ok_or_else(bad)?;
        rest = &rest[value_end + 1..];
        if attributes.insert(name.to_owned(), value).is_some() {
            return Err(Problem::UnknownAttribute(name.to_owned()));
        }
    }
    Ok((self_closing, attributes))
}

fn escape(value: &str, attribute: bool) -> String {
    let mut out = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' if attribute => out.push_str("&quot;"),
            '\n' if attribute => out.push_str("&#10;"),
            _ => out.push(character),
        }
    }
    out
}

fn decode(value: &str) -> Option<String> {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp + 1..];
        let semi = after.find(';')?;
        let entity = &after[..semi];
        match entity {
            "amp" => out.push('&'),
            "lt" => out.push('<'),
            "gt" => out.push('>'),
            "quot" => out.push('"'),
            "apos" => out.push('\''),
            _ => {
                let code = entity.strip_prefix('#')?.parse::<u32>().ok()?;
                out.push(char::from_u32(code)?);
            }
        }
        rest = &after[semi + 1..];
    }
    out.push_str(rest);
    Some(out)
}

/// Lowercase hex, the spelling of hashes in elements and blob file names.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn parse_hash(value: &str) -> Option<Vec<u8>> {
    let digits = value.strip_prefix(HASH_PREFIX)?;
    if digits.len() != 64 {
        return None;
    }
    (0..64)
        .step_by(2)
        .map(|at| {
            let pair = digits.get(at..at + 2)?;
            if !pair
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                return None;
            }
            u8::from_str_radix(pair, 16).ok()
        })
        .collect()
}
