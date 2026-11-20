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

    /// Opens the review as a person does: begun on the first look, and on
    /// a new snapshot when the tree changed and no drafts wait;
    /// `base_branch` picks its base anew, and `refresh` takes a snapshot
    /// even with drafts.
    pub async fn open(
        &self,
        base_branch: Option<String>,
        refresh: bool,
    ) -> Result<ReviewSummary, RpcError> {
        self.open_as(ReviewOpenRequest {
            agent_id: self.agent_id.clone(),
            base_branch,
            refresh,
            read_only: false,
        })
        .await
    }

    /// Reads the review as it stands, changing nothing: no review is begun,
    /// no snapshot taken and no base picked. An agent with no review
    /// answers a summary with an empty id.
    pub async fn read(&self) -> Result<ReviewSummary, RpcError> {
        self.open_as(ReviewOpenRequest {
            agent_id: self.agent_id.clone(),
            read_only: true,
            ..ReviewOpenRequest::default()
        })
        .await
    }

    async fn open_as(&self, request: ReviewOpenRequest) -> Result<ReviewSummary, RpcError> {
        Ok(self.stub()?.open(request).await?.into_inner())
    }

    /// The bodies of `files` of the comparison from `from` to `snapshot`
    /// that `view` names; the files that did not fit are absent from the
    /// answer. `list` also answers every file the comparison changed.
    pub async fn get_files(
        &self,
        from: &str,
        snapshot: &str,
        view: ReviewView,
        files: Vec<ReviewFileRequest>,
        list: bool,
    ) -> Result<ReviewGetFilesResponse, RpcError> {
        let request = ReviewGetFilesRequest {
            agent_id: self.agent_id.clone(),
            snapshot: snapshot.to_owned(),
            view: view as i32,
            files,
            from: from.to_owned(),
            list,
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
