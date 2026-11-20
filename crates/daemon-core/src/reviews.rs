//! The reviews experiment (`reviews/1`) in the core: the writes a review of
//! an own agent's working tree makes to the store, each in one step that
//! also rewrites the review's entry on the agent's row, and the check at
//! each turn end of whether the tree has moved since the review's latest
//! snapshot.
//!
//! The work that reads git (taking snapshots, comparing them) is the
//! daemon's, off the core thread; what the core keeps is the review's rows
//! and what every client is told through the row.

use serde::{Deserialize, Serialize};

use crate::Daemon;
use crate::ids::{AgentId, CallId, JobId};
use crate::io::Output;
use crate::payload::{CallError, JobResult};

/// The experiment version whose shapes this module reads and writes.
pub const VERSION: &str = wire::experimental::REVIEW_ROW.version();

/// A write to an own agent's review, as a client's call asks it of the
/// core: [`crate::payload::ClientRequest::Review`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Write {}

/// The experiment's work off the core thread: [`crate::io::Job::Review`].
#[derive(Clone, Debug, PartialEq)]
pub enum Job {}

/// What a [`Job`] came to: [`JobResult::Review`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Done {}

impl Daemon {
    /// Applies a client's write to a review.
    pub(crate) fn review_write(
        &mut self,
        _call: CallId,
        write: Write,
        _out: &mut Vec<Output>,
    ) -> Result<(), CallError> {
        match write {}
    }

    /// `agent` ended a turn: each review of an agent in its checkout learns
    /// whether its tree has moved since its latest snapshot.
    pub(crate) fn review_turn_ended(&mut self, _agent: AgentId, _out: &mut Vec<Output>) {}

    /// Takes a review job's result; false when `result` is not one.
    pub(crate) fn review_job_done(
        &mut self,
        _job: JobId,
        result: &JobResult,
        _out: &mut Vec<Output>,
    ) -> bool {
        let JobResult::Review(done) = result else {
            return false;
        };
        match *done {}
    }
}
