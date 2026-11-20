//! Closing a round: when this host accepts a message for one of its own
//! agents, a person's prompt or another agent's message, the comments a
//! sent review in it names are marked sent, exactly those and no others.
//!
//! The message says which comments it sends, so a draft another client
//! wrote after the sender last looked stays a draft, and a review chip
//! taken out of the composer sends nothing. Only an accepted message closes
//! a round: one the agent refused leaves every draft as it was.

use std::collections::HashSet;

use daemon_core::reviews::Change;
use wire::Envelope;
use wire::experimental::SentReview;

use super::{Tree, review_base, write};
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

/// The sent reviews a person's prompt carries, read before the input is
/// handed to the agent.
pub(crate) fn prompted(prompt: &wire::PromptInput) -> Vec<SentReview> {
    prompt
        .attachments
        .iter()
        .filter_map(attachments::sent_review::entry)
        .collect()
}

/// A message for the own agent `agent` carrying the sent reviews `named`
/// was accepted: each that names the agent's current review closes a round
/// of it, on the snapshot the comments were made on, marking sent the
/// comments it names that are still drafts. One naming another review, an
/// earlier one started over, changes nothing.
///
/// The message is the agent's already, so nothing here can refuse it: a
/// review that cannot be marked is logged and left as it was.
pub(crate) async fn accepted(runtime: &ProfileRuntime, agent: AgentId, named: Vec<SentReview>) {
    if named.is_empty()
        || !runtime
            .launch()
            .experiments
            .is_on(&settings::experiments::REVIEWS)
    {
        return;
    }
    // An agent whose folder is gone, or is no repository, has no review to
    // open, so none to close a round of.
    let Ok(row) = runtime.own_row(agent).await else {
        return;
    };
    let Ok(tree) = Tree::of(runtime, &row).await else {
        return;
    };
    let turn = tree.turn();
    let _turn = turn.lock().await;
    let review = match tree.review(runtime).await {
        Ok(Some(review)) => review,
        Ok(None) => return,
        Err(error) => {
            tracing::warn!(%error, "marking a sent review: reading it failed");
            return;
        }
    };

    let mut drafts: HashSet<&[u8]> = review.drafts().map(|draft| draft.id.as_slice()).collect();
    let mut sending = Vec::new();
    for sent in named {
        if sent.review_id != review.id {
            continue;
        }
        // A person who resends a message resends it under a new id, which
        // this host accepts as a new message. Its comments were sent with
        // the first, so it closes no second round.
        let marks = sent
            .comment_ids
            .iter()
            .filter(|id| drafts.remove(id.as_slice()))
            .count();
        if marks == 0 && !sent.comment_ids.is_empty() {
            continue;
        }
        sending.push(sent);
    }
    if sending.is_empty() {
        return;
    }

    // The round records the base its snapshot was shown against, which is
    // the review's base as it stands: the sender read it moments ago.
    let base_commit = match review_base(runtime, agent, &tree.root, &review).await {
        Ok(base) => base.commit,
        Err(error) => {
            tracing::warn!(%error, "marking a sent review: its base could not be read");
            String::new()
        }
    };
    let changes = sending
        .into_iter()
        .map(|sent| Change::Sent {
            review: sent.review_id,
            snapshot: sent.snapshot,
            base_commit: base_commit.clone(),
            comments: sent.comment_ids,
        })
        .collect();
    if let Err(error) = write(runtime, agent, changes).await {
        tracing::warn!(%error, "marking a sent review's comments sent failed");
    }
}
