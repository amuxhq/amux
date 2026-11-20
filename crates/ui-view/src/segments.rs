//! Text with attachments at their positions, for rows and the composer.

use attachments::PLACEHOLDER;
#[cfg(feature = "schema")]
use schemars::JsonSchema;
use serde::Serialize;
use wire::{Attachment, BlobRef};

use crate::text::without_direction_controls;

/// A run of text or one attachment, in reading order. Each placeholder in
/// the text is the next attachment, so attachments keep their positions.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
pub enum Segment {
    Text(String),
    Attachment(AttachmentView),
}

/// What a chip or placeholder shows before any bytes arrive: the
/// reference's name, type and size.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(JsonSchema))]
pub enum AttachmentView {
    Image(BlobRef),
    File(BlobRef),
    /// Pasted text: its name, its length in lines, and the text itself,
    /// which a sent message shows in place of the chip.
    Text {
        name: String,
        lines: u32,
        text: String,
    },
    Review {
        patch: Option<BlobRef>,
        comments: u32,
    },
    /// Comments on a web page: the page's title, or its address when it
    /// has none, the comment count, and the screenshots of the regions the
    /// comments were made on.
    Page {
        title: String,
        comments: u32,
        images: Vec<BlobRef>,
    },
    Empty,
}

impl AttachmentView {
    /// The attachment as a chip shows it. Its name is drawn without
    /// direction controls, which would make `invoice\u{202E}fdp.exe` read
    /// as a PDF.
    pub fn of(attachment: &Attachment) -> AttachmentView {
        use wire::attachment::Of;
        let named = |blob: &BlobRef| BlobRef {
            name: without_direction_controls(&blob.name),
            ..blob.clone()
        };
        match &attachment.of {
            Some(Of::Image(blob)) => AttachmentView::Image(named(blob)),
            Some(Of::File(blob)) => AttachmentView::File(named(blob)),
            Some(Of::Text(text)) => AttachmentView::Text {
                name: without_direction_controls(&text.name),
                lines: text.text.lines().count() as u32,
                text: text.text.clone(),
            },
            Some(Of::Review(review)) => AttachmentView::Review {
                patch: review.diff.as_ref().and_then(|diff| diff.patch.clone()),
                comments: review.comments.len() as u32,
            },
            Some(Of::Page(page)) => AttachmentView::Page {
                title: without_direction_controls(if page.title.is_empty() {
                    &page.url
                } else {
                    &page.title
                }),
                comments: page.comments.len() as u32,
                images: page
                    .comments
                    .iter()
                    .filter_map(|comment| comment.image.as_ref().map(named))
                    .collect(),
            },
            None => AttachmentView::Empty,
        }
    }
}

/// The largest image a client downloads by itself to draw its thumbnail. A
/// chat full of big images should not spend someone's mobile data before
/// they ask; a larger one stays a chip until it is opened. The phone keeps
/// this number too, in `Thumbnail.sizeLimit`.
pub const THUMBNAIL_SIZE_LIMIT: u64 = 16 * 1024 * 1024;

/// Splits text at its placeholders. A placeholder with no attachment left
/// is dropped; attachments with no placeholder follow the text.
pub fn segments(text: &str, attachments: &[Attachment]) -> Vec<Segment> {
    let mut out = Vec::new();
    let mut attachments = attachments.iter();
    for (index, prose) in text.split(PLACEHOLDER).enumerate() {
        if index > 0
            && let Some(attachment) = attachments.next()
        {
            out.push(Segment::Attachment(AttachmentView::of(attachment)));
        }
        if !prose.is_empty() {
            out.push(Segment::Text(prose.to_owned()));
        }
    }
    out.extend(attachments.map(|attachment| Segment::Attachment(AttachmentView::of(attachment))));
    out
}
