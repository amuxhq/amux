//! Text with attachments at their positions, for rows and the composer.

use attachments::PLACEHOLDER;
use schemars::JsonSchema;
use serde::Serialize;
use wire::{Attachment, BlobRef};

use crate::text::without_direction_controls;

/// A run of text or one attachment, in reading order. Each placeholder in
/// the text is the next attachment, so attachments keep their positions.
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub enum Segment {
    Text(String),
    Attachment(AttachmentView),
}

/// What a chip or placeholder shows before any bytes arrive: the
/// reference's name, type and size.
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
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
            None => AttachmentView::Empty,
        }
    }
}

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
