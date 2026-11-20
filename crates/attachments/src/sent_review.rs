//! A sent review as the agent reads it: a text attachment holding, for each
//! comment, where it sits and the code it is on before the person's words,
//! and the command that shows the snapshot the line numbers refer to; with
//! the review's entry in the attachment's `experimental` map, so a client
//! draws it as a review and the agent's host marks its comments sent.

use wire::Attachment;
use wire::experimental::{SENT_REVIEW, SentReview};

/// The review `attachment` says it sends; none for any other attachment.
pub fn entry(attachment: &Attachment) -> Option<SentReview> {
    SENT_REVIEW.get(&attachment.experimental)
}
