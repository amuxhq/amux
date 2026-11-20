//! The reviews experiment on the agent's host: a person's review of one of
//! this host's agents' working trees, round by round. Opening it takes a
//! snapshot when the tree has moved and no drafts wait, and answers what the
//! snapshot holds; editing puts, deletes and discards comments, or starts the
//! review over. The rows are the core's, written through it; the snapshots
//! are git's, in a store in the agent's directory.
//!
//! Only a person reaches these, for an own agent: the service checks both
//! before it calls in (`crate::api`).

pub(crate) mod cleanup;
pub(crate) mod files;
pub(crate) mod sent;

use std::sync::{Arc, Weak};

use daemon_core::payload::JobResult;
use store::AgentRow;
use tonic::Status;
use wire::ErrorCode;
use wire::experimental::{ReviewEditRequest, ReviewOpenRequest, ReviewSummary};

use crate::api::Caller;
use crate::grpc::{status, wire_error};
use crate::runtime::ProfileRuntime;

/// Reviews1.Open: the review of `row`'s agent as `caller` opens it.
pub(crate) async fn open(
    _runtime: &Arc<ProfileRuntime>,
    _row: AgentRow,
    _request: ReviewOpenRequest,
    _caller: &Caller,
) -> Result<ReviewSummary, Status> {
    // Begins the review on a first look, snapshots a moved tree when no
    // drafts wait, and answers the summary with its comments placed.
    Err(not_yet())
}

/// Reviews1.Edit: `caller`'s edits to the review of `row`'s agent, all of
/// them or none.
pub(crate) async fn edit(
    _runtime: &Arc<ProfileRuntime>,
    _row: AgentRow,
    _request: ReviewEditRequest,
    _caller: &Caller,
) -> Result<ReviewSummary, Status> {
    // Puts and deletes comments, discards drafts or starts the review over,
    // and answers the review as it is after them.
    Err(not_yet())
}

/// Carries out a reviews job of the core's; none when the profile has
/// stopped.
pub(crate) async fn run_job(
    _runtime: Weak<ProfileRuntime>,
    job: daemon_core::reviews::Job,
) -> Option<JobResult> {
    match job {}
}

/// The answer of a call this build does not carry out yet.
fn not_yet() -> Status {
    status(wire_error(
        ErrorCode::Unimplemented,
        "this host does not answer reviews yet",
    ))
}
