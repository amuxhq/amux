//! The host services: answered by the host a call is addressed to, for what
//! it owns. A handle is made only from a host's id or an agent's address,
//! so every call it makes names its host.

use wire::amux::staging::agent_service_client::AgentServiceClient;
use wire::amux::staging::host_service_client::HostServiceClient;
use wire::amux::staging::review_service_client::ReviewServiceClient;
use wire::amux::staging::{self, get_changes_request};
use wire::{
    BlobRef, Diff, DiffBase, Filing, Input, MessageBody, PromptSubmission, SendInputResponse,
};

use crate::{Addressed, Conn, RpcError, held, stub};

/// One host's services: what it offers before any agent exists, and making
/// an agent on it.
#[derive(Clone)]
pub struct Host {
    to: Addressed,
}

impl Host {
    pub(crate) fn new(to: Addressed) -> Host {
        Host { to }
    }

    fn stub(&self) -> Result<HostServiceClient<Conn>, RpcError> {
        Ok(stub!(HostServiceClient<Conn>, self.to.conn()?))
    }

    /// Making agents on this host. Every other act on an agent is through
    /// its row ([`crate::Client::agent`]); a new agent has none yet.
    pub fn agents(&self) -> HostAgents {
        HostAgents {
            to: self.to.clone(),
        }
    }

    /// What `provider` offers on this host with no agent running.
    pub async fn catalogue(
        &self,
        provider: wire::Provider,
    ) -> Result<staging::GetCatalogueResponse, RpcError> {
        let request = staging::GetCatalogueRequest {
            provider: provider as i32,
        };
        Ok(self.stub()?.get_catalogue(request).await?.into_inner())
    }

    /// Pins `path` at the end of this host's repositories, or unpins it.
    /// The folders themselves are read from the host's entry in the
    /// inventory, which says the change once the host has made it.
    pub async fn pin_folder(&self, path: &str, pinned: bool) -> Result<(), RpcError> {
        let request = staging::PinFolderRequest {
            path: path.to_owned(),
            pinned,
        };
        self.stub()?.pin_folder(request).await?;
        Ok(())
    }
}

/// Making agents on one host.
#[derive(Clone)]
pub struct HostAgents {
    to: Addressed,
}

impl HostAgents {
    /// Makes an agent on this host. The call's address is the host; an
    /// agent that calls is the new agent's parent.
    pub async fn create(
        &self,
        request: staging::CreateAgentRequest,
    ) -> Result<wire::Agent, RpcError> {
        let mut stub = stub!(AgentServiceClient<Conn>, self.to.conn()?);
        held(stub.create_agent(request).await?.into_inner().agent)
    }
}

/// One agent's host services, at its host, each call naming the agent by
/// the id held with that host.
#[derive(Clone)]
pub struct Agent {
    to: Addressed,
    agent_id: Vec<u8>,
}

impl Agent {
    pub(crate) fn new(to: Addressed, agent_id: Vec<u8>) -> Agent {
        Agent { to, agent_id }
    }

    /// The agent's id, as each call names it.
    pub fn id(&self) -> &[u8] {
        &self.agent_id
    }

    fn agents(&self) -> Result<AgentServiceClient<Conn>, RpcError> {
        Ok(stub!(AgentServiceClient<Conn>, self.to.conn()?))
    }

    fn id_owned(&self) -> Vec<u8> {
        self.agent_id.clone()
    }

    /// Input for the agent's interpreter, answered with its verdict.
    pub async fn send_input(&self, input: Input) -> Result<SendInputResponse, RpcError> {
        let request = staging::SendInputRequest {
            agent_id: self.id_owned(),
            input: Some(input),
        };
        Ok(self.agents()?.send_input(request).await?.into_inner())
    }

    /// A message to the agent from the caller, under `id` (16 bytes), so a
    /// resend of the same message is dropped. Answers the id it is stored
    /// under.
    pub async fn send_message(
        &self,
        id: Vec<u8>,
        context: Option<Vec<u8>>,
        message: MessageBody,
    ) -> Result<Vec<u8>, RpcError> {
        let request = staging::SendMessageRequest {
            id,
            agent_id: self.id_owned(),
            context,
            message: Some(message),
        };
        Ok(self
            .agents()?
            .send_message(request)
            .await?
            .into_inner()
            .envelope_id)
    }

    pub async fn rename(&self, name: &str) -> Result<wire::Agent, RpcError> {
        let request = staging::RenameAgentRequest {
            agent_id: self.id_owned(),
            name: name.to_owned(),
        };
        held(
            self.agents()?
                .rename_agent(request)
                .await?
                .into_inner()
                .agent,
        )
    }

    /// Files the agent open, pinned or closed; closing a live one stops it.
    pub async fn set_filing(&self, filing: Filing) -> Result<wire::Agent, RpcError> {
        let request = staging::SetFilingRequest {
            agent_id: self.id_owned(),
            filing: filing as i32,
        };
        held(self.agents()?.set_filing(request).await?.into_inner().agent)
    }

    /// Spawns the agent again, its first prompt `initial_prompt` when one
    /// is given; with `if_suspended`, only if it is live with no process.
    pub async fn resume(
        &self,
        initial_prompt: Option<PromptSubmission>,
        if_suspended: bool,
    ) -> Result<wire::Agent, RpcError> {
        let request = staging::ResumeAgentRequest {
            agent_id: self.id_owned(),
            initial_prompt,
            if_suspended,
        };
        held(
            self.agents()?
                .resume_agent(request)
                .await?
                .into_inner()
                .agent,
        )
    }

    /// What deleting the agent now would take with it. Changes nothing.
    pub async fn preview_delete(&self) -> Result<staging::PreviewDeleteResponse, RpcError> {
        let request = staging::PreviewDeleteRequest {
            agent_id: self.id_owned(),
        };
        Ok(self.agents()?.preview_delete(request).await?.into_inner())
    }

    /// Removes the agent for good. `discard_workspace_changes` is the
    /// person's consent to lose the uncommitted changes the preview named.
    pub async fn delete(&self, discard_workspace_changes: bool) -> Result<(), RpcError> {
        let request = staging::DeleteAgentRequest {
            agent_id: self.id_owned(),
            discard_workspace_changes,
        };
        self.agents()?.delete_agent(request).await?;
        Ok(())
    }

    /// Writes a file into the agent's directory, for a prompt or a message
    /// to reference.
    pub async fn put_blob(
        &self,
        name: &str,
        mime: &str,
        bytes: Vec<u8>,
    ) -> Result<BlobRef, RpcError> {
        let request = staging::PutBlobRequest {
            agent_id: self.id_owned(),
            name: name.to_owned(),
            mime: mime.to_owned(),
            bytes,
        };
        held(self.agents()?.put_blob(request).await?.into_inner().blob)
    }

    /// The changed files against `base` (absent, the default rule), and
    /// which base that was.
    pub async fn changes(
        &self,
        base: Option<get_changes_request::Base>,
    ) -> Result<staging::GetChangesResponse, RpcError> {
        let request = staging::GetChangesRequest {
            agent_id: self.id_owned(),
            base,
        };
        let mut reviews = stub!(ReviewServiceClient<Conn>, self.to.conn()?);
        Ok(reviews.get_changes(request).await?.into_inner())
    }

    /// The changed files against `base`; `with_patch` also has the host
    /// build the patch and keep it as one of the agent's files, which the
    /// diff names.
    pub async fn get_diff(&self, base: DiffBase, with_patch: bool) -> Result<Diff, RpcError> {
        let request = staging::GetDiffRequest {
            agent_id: self.id_owned(),
            base: Some(base),
            with_patch,
        };
        held(self.agents()?.get_diff(request).await?.into_inner().diff)
    }
}
