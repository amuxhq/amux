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
//! <amux-attachment kind="page" url="http://localhost:5173/" title="Pricing" comments="1">…</amux-attachment>
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
use wire::{
    Attachment, BlobRef, Diff, DiffBase, Empty, InlineText, PageComment, PageElement, PageReview,
    Review, ReviewComment,
};

/// The type of an image both providers read natively (PNG, JPEG, GIF or
/// WebP), judged by its bytes; None for anything else, which reaches the
/// model as a file its element names. Claude and Codex both judge an image
/// by its bytes too, so a label or extension is never trusted.
pub fn native_image(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

/// [`native_image`] of the file at `path`, from its first bytes; None when
/// it cannot be read.
pub fn native_image_at(path: &Path) -> Option<&'static str> {
    use std::io::Read as _;
    let mut head = Vec::with_capacity(12);
    std::fs::File::open(path)
        .ok()?
        .take(12)
        .read_to_end(&mut head)
        .ok()?;
    native_image(&head)
}

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

/// What a refusal says when [`all_readable`] is false, as the end of "not
/// sent: ...".
pub const UNREADABLE: &str =
    "one of its attachments is of a kind this host can't read; the host needs updating";

/// Whether this build can read every attachment. An arm a later build added
/// decodes here as no arm at all, and [`format`] would drop it without a
/// word, so a send holding one is refused instead. Only this is refused:
/// the placeholder count is not, because a person's words can hold the
/// placeholder character by themselves, pasted from a rich text view.
pub fn all_readable(attachments: &[Attachment]) -> bool {
    attachments.iter().all(|attachment| attachment.of.is_some())
}

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
        match &attachment.of {
            None => return Err(ShapeError::Empty { index }),
            Some(Of::Review(review))
                if review
                    .diff
                    .as_ref()
                    .and_then(|diff| diff.patch.as_ref())
                    .is_none() =>
            {
                return Err(ShapeError::BadHash { index });
            }
            Some(_) => {}
        }
        if blobs(attachment).iter().any(|blob| blob.hash.len() != 32) {
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
                element: element_in(attachment, dir),
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

/// The one blob an attachment is, for the kinds that are a single file: an
/// image, a file, or a review's patch. A page review is none; its comments'
/// screenshots are each a blob of their own (see [`blobs`]).
fn blob_of(attachment: &Attachment) -> Option<&BlobRef> {
    match attachment.of.as_ref()? {
        Of::Image(blob) | Of::File(blob) => Some(blob),
        Of::Review(review) => review.diff.as_ref()?.patch.as_ref(),
        Of::Text(_) | Of::Page(_) => None,
    }
}

/// Every blob an attachment names: what must be with an agent before the
/// attachment can reach it.
pub fn blobs(attachment: &Attachment) -> Vec<&BlobRef> {
    match &attachment.of {
        Some(Of::Page(page)) => page
            .comments
            .iter()
            .filter_map(|comment| comment.image.as_ref())
            .collect(),
        _ => blob_of(attachment).into_iter().collect(),
    }
}

/// The images an attachment shows the model: an image, or a page review's
/// screenshots. A provider that takes images natively puts each one's bytes
/// right after the attachment's element, which names them too.
pub fn images(attachment: &Attachment) -> Vec<&BlobRef> {
    match &attachment.of {
        Some(Of::Image(blob)) => vec![blob],
        Some(Of::Page(_)) => blobs(attachment),
        _ => Vec::new(),
    }
}

/// One attachment's canonical element, with the paths of its bytes under
/// `dir`, the agent's blobs directory.
pub fn element_in(attachment: &Attachment, dir: &Path) -> String {
    written(attachment, blob_path(attachment, dir).as_deref(), Some(dir))
}

/// One attachment's canonical element, naming `path` as where its bytes
/// are. A page review's screenshots are named only by [`element_in`].
pub fn element(attachment: &Attachment, path: Option<&Path>) -> String {
    written(attachment, path, None)
}

fn written(attachment: &Attachment, path: Option<&Path>, dir: Option<&Path>) -> String {
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
        Some(Of::Page(page)) => {
            attribute(&mut out, "kind", "page");
            attribute(&mut out, "url", &page.url);
            attribute(&mut out, "title", &page.title);
            attribute(&mut out, "comments", &page.comments.len().to_string());
            out.push('>');
            out.push_str(&escape(&page_body(&page.comments, dir), false));
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

/// A page review's comments. Each is a heading that names the byte count of
/// every line it has, then those lines in a fixed order, each `label: value`,
/// so the model reads labelled fields and no value can be mistaken for the
/// next line or heading:
///
/// ```text
/// ## comment selector-bytes=8 element-bytes=13 quote-bytes=7 text-bytes=19
/// selector: #pricing
/// element: Pricing cards
/// quote: monthly
/// text: Say annual pricing.
/// ```
///
/// A line with an empty value is left out, heading and all. A screenshot
/// adds `image-hash` and `image-size` to the heading and its name, mime type
/// and, when the blobs directory is known, its path as lines.
fn page_body(comments: &[PageComment], dir: Option<&Path>) -> String {
    let mut body = String::new();
    for comment in comments {
        let element = comment.element.clone().unwrap_or_default();
        let mut heading = String::from("## comment");
        let mut lines = String::new();
        let mut line = |heading: &mut String, label: &str, value: &str| {
            if value.is_empty() {
                return;
            }
            heading.push_str(&format!(" {label}-bytes={}", value.len()));
            lines.push_str(&format!("{label}: {value}\n"));
        };
        line(&mut heading, "selector", &element.selector);
        line(&mut heading, "element", &element.text);
        line(&mut heading, "source", &element.source);
        line(&mut heading, "quote", &comment.quote);
        if let Some(image) = &comment.image {
            heading.push_str(&format!(
                " image-hash={HASH_PREFIX}{} image-size={}",
                hex(&image.hash),
                image.size
            ));
            line(&mut heading, "image", &image.name);
            line(&mut heading, "image-mime", &image.mime);
            if let Some(dir) = dir {
                let path = dir.join(hex(&image.hash));
                line(&mut heading, "image-path", &path.to_string_lossy());
            }
        }
        line(&mut heading, "text", &comment.text);
        body.push_str(&heading);
        body.push('\n');
        body.push_str(&lines);
    }
    body
}

/// The lines of a page comment, in the order [`page_body`] writes them.
const PAGE_LINES: [&str; 8] = [
    "selector",
    "element",
    "source",
    "quote",
    "image",
    "image-mime",
    "image-path",
    "text",
];

fn parse_page_body(body: &str) -> Option<Vec<PageComment>> {
    let mut comments = Vec::new();
    let mut rest = body;
    while !rest.is_empty() {
        let (heading, after) = rest.strip_prefix("## comment")?.split_once('\n')?;
        let mut counts = BTreeMap::new();
        let mut image_hash = None;
        let mut image_size = None;
        for field in heading.split(' ').filter(|field| !field.is_empty()) {
            let (name, value) = field.split_once('=')?;
            match name {
                "image-hash" if image_hash.is_none() => image_hash = Some(parse_hash(value)?),
                "image-size" if image_size.is_none() => image_size = Some(value.parse().ok()?),
                _ => {
                    let label = name.strip_suffix("-bytes")?;
                    if !PAGE_LINES.contains(&label)
                        || counts.insert(label, value.parse::<usize>().ok()?).is_some()
                    {
                        return None;
                    }
                }
            }
        }
        let mut values = BTreeMap::new();
        let mut after = after;
        for label in PAGE_LINES {
            let Some(&count) = counts.get(label) else {
                continue;
            };
            let value_start = after.strip_prefix(label)?.strip_prefix(": ")?;
            let value = value_start.get(..count)?;
            after = value_start[count..].strip_prefix('\n')?;
            values.insert(label, value.to_owned());
        }
        rest = after;
        let mut take = |label: &str| values.remove(label).unwrap_or_default();
        let element = PageElement {
            selector: take("selector"),
            text: take("element"),
            source: take("source"),
        };
        let quote = take("quote");
        // The path is where the bytes were on the host that wrote the
        // element; the hash names them.
        take("image-path");
        let image = match (image_hash, image_size) {
            (Some(hash), size) => Some(BlobRef {
                hash,
                name: take("image"),
                mime: take("image-mime"),
                size: size.unwrap_or(0),
            }),
            (None, None) if !counts.contains_key("image") && !counts.contains_key("image-mime") => {
                None
            }
            (None, _) => return None,
        };
        comments.push(PageComment {
            text: take("text"),
            element: Some(element),
            quote,
            image,
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
    /// `kind` is missing or not image, file, text, review or page.
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
                    // Of the branch when the diff was taken, not the review's.
                }),
                comments,
            })
        }
        ("page", Some(body)) => {
            let url = attributes.remove("url").unwrap_or_default();
            let title = attributes.remove("title").unwrap_or_default();
            let count: usize = attributes
                .remove("comments")
                .and_then(|count| count.parse().ok())
                .ok_or_else(|| Problem::BadAttribute("comments".into()))?;
            let comments = parse_page_body(&body).ok_or(Problem::BadBody)?;
            if comments.len() != count {
                return Err(Problem::BadBody);
            }
            Of::Page(PageReview {
                url,
                title,
                comments,
            })
        }
        ("image" | "file" | "text" | "review" | "page", _) => return Err(Problem::BadForm),
        _ => return Err(Problem::UnknownKind),
    };
    if let Some(name) = attributes.into_keys().next() {
        return Err(Problem::UnknownAttribute(name));
    }
    Ok((
        Attachment {
            of: Some(of),
            ..Default::default()
        },
        end,
    ))
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
