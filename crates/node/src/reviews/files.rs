//! The bodies of a review's files: hunks between two commits, highlighted
//! from each side's whole file, where each hunk sits in the code, each
//! file's outline, and more context read from a commit on request.

use std::sync::Arc;

use store::AgentRow;
use tonic::Status;
use wire::experimental::{ReviewGetFilesRequest, ReviewGetFilesResponse};

use crate::api::Caller;
use crate::runtime::ProfileRuntime;

/// Reviews1.GetFiles: the files `caller` asks for of a snapshot of the
/// review of `row`'s agent.
pub(crate) async fn get_files(
    _runtime: &Arc<ProfileRuntime>,
    _row: AgentRow,
    _request: ReviewGetFilesRequest,
    _caller: &Caller,
) -> Result<ReviewGetFilesResponse, Status> {
    // Answers the files that fit in one answer, each with its hunks for the
    // view asked for, its outline and the context lines asked for.
    Err(super::not_yet())
}
