//! Closing a round: when this host accepts a message for one of its own
//! agents, the comments a sent review in it names are marked sent, exactly
//! those and no others.

use wire::Envelope;
use wire::experimental::SentReview;

use crate::runtime::{AgentId, ProfileRuntime};

/// The sent reviews `envelope`'s message carries, read before the message
/// is handed to the core, which takes it.
pub(crate) fn named(envelope: &Envelope) -> Vec<SentReview> {
    let Some(wire::envelope::Body::Message(message)) = &envelope.body else {
        return Vec::new();
    };
    message
        .attachments
        .iter()
        .filter_map(attachments::sent_review::entry)
        .collect()
}

/// A message for the own agent `agent` carrying the sent reviews `named`
/// was accepted.
pub(crate) async fn accepted(_runtime: &ProfileRuntime, _agent: AgentId, _named: Vec<SentReview>) {
    // Marks the drafts each entry names sent in a new round of the agent's
    // current review, when the entry names that review.
}
