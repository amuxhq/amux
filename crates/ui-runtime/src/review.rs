//! The changed files an overview lists, and the diff a review page is
//! written against.
//!
//! The free functions take the flat client the terminal's and the apps'
//! own calls still use; a chat's session asks through its agent's handle.

use client::{Client, RpcError};
use wire::{Diff, DiffBase, GetBlobRequest, GetDiffRequest};

/// Asks the agent's host which files changed against `base`, with their
/// line counts. Builds no patch: only a review page needs one.
pub async fn changed_files(
    client: &dyn Client,
    agent_id: &[u8],
    base: DiffBase,
) -> Result<Diff, RpcError> {
    client
        .get_diff(GetDiffRequest {
            agent_id: agent_id.to_vec(),
            base: Some(base),
            with_patch: false,
        })
        .await
}

/// Asks the agent's host for its diff against `base` and fetches the patch
/// the diff names. The diff comes back exactly as the host froze it, so a
/// review sent with it names the same patch the page showed.
pub async fn review(
    client: &dyn Client,
    agent_id: &[u8],
    base: DiffBase,
) -> Result<(Diff, String), RpcError> {
    let diff = client
        .get_diff(GetDiffRequest {
            agent_id: agent_id.to_vec(),
            base: Some(base),
            with_patch: true,
        })
        .await?;
    let patch = match &diff.patch {
        Some(blob) => {
            let fetched = client
                .get_blob(GetBlobRequest {
                    agent_id: agent_id.to_vec(),
                    hash: blob.hash.clone(),
                })
                .await?;
            String::from_utf8_lossy(&fetched.bytes).into_owned()
        }
        None => String::new(),
    };
    Ok((diff, patch))
}

/// [`review`] for a chat's session: the diff from the agent's host, and the
/// patch it names read through this node's copy of the agent's files.
pub(crate) async fn reviewed(
    client: &client::next::Client,
    agent: &client::next::Agent,
    base: DiffBase,
) -> Result<(Diff, String), RpcError> {
    let diff = agent.get_diff(base, true).await?;
    let patch = match &diff.patch {
        Some(blob) => {
            let bytes = client.sessions().get_blob(agent.id(), &blob.hash).await?;
            String::from_utf8_lossy(&bytes).into_owned()
        }
        None => String::new(),
    };
    Ok((diff, patch))
}
