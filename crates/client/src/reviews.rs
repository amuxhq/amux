//! The reviews experiment's calls on one agent's review, at the agent's
//! host: `amux.experimental.Reviews1`. A host that does not run the
//! experiment answers each one with an [`RpcError`] whose code is
//! `Unimplemented`.

use wire::experimental::reviews1_client::Reviews1Client;
use wire::experimental::{
    ReviewEdit, ReviewEditRequest, ReviewFileRequest, ReviewGetFilesRequest,
    ReviewGetFilesResponse, ReviewOpenRequest, ReviewSummary, ReviewView,
};

use crate::{Addressed, Conn, RpcError, stub};

/// One agent's review, each call addressed to the agent's host and naming
/// the agent by the id held there.
#[derive(Clone)]
pub struct AgentReviews {
    to: Addressed,
    agent_id: Vec<u8>,
}

impl AgentReviews {
    pub(crate) fn new(to: Addressed, agent_id: Vec<u8>) -> Self {
        Self { to, agent_id }
    }

    fn stub(&self) -> Result<Reviews1Client<Conn>, RpcError> {
        Ok(stub!(Reviews1Client<Conn>, self.to.conn()?))
    }

    /// The review, begun on the first look; `base_branch` picks its base
    /// anew, and `refresh` takes a snapshot even with drafts.
    pub async fn open(
        &self,
        base_branch: Option<String>,
        refresh: bool,
    ) -> Result<ReviewSummary, RpcError> {
        let request = ReviewOpenRequest {
            agent_id: self.agent_id.clone(),
            base_branch,
            refresh,
        };
        Ok(self.stub()?.open(request).await?.into_inner())
    }

    /// The bodies of `files` of `snapshot`, as `view` compares it; the files
    /// that did not fit are absent from the answer.
    pub async fn get_files(
        &self,
        snapshot: &str,
        view: ReviewView,
        files: Vec<ReviewFileRequest>,
    ) -> Result<ReviewGetFilesResponse, RpcError> {
        let request = ReviewGetFilesRequest {
            agent_id: self.agent_id.clone(),
            snapshot: snapshot.to_owned(),
            view: view as i32,
            files,
        };
        Ok(self.stub()?.get_files(request).await?.into_inner())
    }

    /// Makes `edits` to the review, all of them or none, and answers the
    /// review as it is after them.
    pub async fn edit(&self, edits: Vec<ReviewEdit>) -> Result<ReviewSummary, RpcError> {
        let request = ReviewEditRequest {
            agent_id: self.agent_id.clone(),
            edits,
        };
        Ok(self.stub()?.edit(request).await?.into_inner())
    }
}
