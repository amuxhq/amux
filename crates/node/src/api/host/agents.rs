//! This host's own agents: AgentService.
//!
//! One set of handlers answers every entry, the profile's socket, an
//! agent's tools socket, the phone in memory and a link from a paired host
//! alike: the caller the entry settled is all that differs. Each handler
//! reads only this host's own rows, the authoritative ones, and applies
//! every rule about the caller against them:
//!
//! - a person may make every call;
//! - an agent, of this host or a paired one, may message any agent, and
//!   make every other call naming an agent only on its own direct children;
//! - an agent starts children with their kind's default settings only, and
//!   files them only as closed.
//!
//! An agent this node holds only a copy of is another host's to answer for,
//! so it is not found here, and the refusal says which host owns it.
//!
//! Usage is counted here, on the host that does the act, the one place
//! every act lands, once, under this host's telemetry setting. A person's
//! prompts and answers count; an agent's do not; every new agent counts,
//! saying who asked for it.

use std::sync::{Arc, Weak};

use store::AgentRow;
use tonic::service::RoutesBuilder;
use tonic::{Request, Response, Status};
use wire::agent_service_server::AgentService;
use wire::{
    CreateAgentRequest, CreateAgentResponse, DeleteAgentRequest, DeleteAgentResponse, ErrorCode,
    GetDiffRequest, GetDiffResponse, PreviewDeleteRequest, PreviewDeleteResponse, PutBlobRequest,
    PutBlobResponse, RenameAgentRequest, RenameAgentResponse, ResumeAgentRequest,
    ResumeAgentResponse, SendInputRequest, SendMessageRequest, SendMessageResponse,
    SetFilingRequest, SetFilingResponse, create_agent_request,
};

use super::changes::person_or_parent;
use crate::api::Entry;
use crate::api::caller::{At, Caller};
use crate::grpc::{status, upgrade, wire_error};
use crate::runtime::{AgentId, ProfileRuntime};
use crate::telemetry::{self, InputShape};

/// How long a send's id is: the caller's own, which the recipient's items
/// are searched for to drop a resend. A send of the shape before this one
/// decodes with its whole envelope where the id is, so its length is what
/// tells it apart and refuses it.
const SEND_ID_LEN: usize = 16;

/// Mounts AgentService for `entry`: the same handlers on every entry, as
/// the caller the entry settled is the only thing that differs.
pub(in crate::api) fn mount(
    routes: &mut RoutesBuilder,
    runtime: &Weak<ProfileRuntime>,
    _entry: Entry,
) {
    routes.add_service(wire::agent_service_server(Agents {
        runtime: runtime.clone(),
    }));
}

/// AgentService for one profile. It holds the runtime weakly: a connection
/// that outlives its runtime is answered unavailable, never kept alive.
#[derive(Clone)]
struct Agents {
    runtime: Weak<ProfileRuntime>,
}

impl Agents {
    fn runtime(&self) -> Result<Arc<ProfileRuntime>, Status> {
        upgrade(&self.runtime)
    }
}

/// One of this host's own agents, as a call names it.
struct Own {
    id: AgentId,
    row: AgentRow,
}

/// The agent `agent_id` names, when it is one of this host's own: an agent
/// this node holds only a copy of is its own host's to answer for, so it
/// is not found here, and the refusal names that host, so a client that
/// sent the call to the wrong host learns where to send it.
async fn own_agent(
    runtime: &ProfileRuntime,
    agent_id: &[u8],
) -> Result<(AgentId, AgentRow), Status> {
    let id = crate::grpc::agent_id(agent_id)?;
    let row = runtime
        .own_row(id)
        .await
        .map_err(|error| status(error.to_wire()))?;
    Ok((id, row))
}

impl Agents {
    /// The runtime, the caller and the own agent a call names, once the
    /// caller may act on it.
    async fn acting_on<T>(
        &self,
        request: &Request<T>,
        agent_id: &[u8],
        refusal: &str,
    ) -> Result<(Arc<ProfileRuntime>, Caller, Own), Status> {
        let caller = Caller::of(request)?;
        let runtime = self.runtime()?;
        let (id, row) = own_agent(&runtime, agent_id).await?;
        person_or_parent(&caller, &row, refusal)?;
        Ok((runtime, caller, Own { id, row }))
    }
}

/// Where an act came from, as this host's analytics records it.
fn origin(runtime: &ProfileRuntime, caller: &Caller) -> analytics::Origin {
    let from = match caller {
        Caller::Person { at: At::Here, .. } => None,
        Caller::Person {
            at: At::Paired(host),
            ..
        } => Some(*host),
        Caller::Agent { agent, .. } => Some(agent.host).filter(|host| *host != runtime.host()),
    };
    from.map_or(analytics::Origin::Here, analytics::Origin::PairedHost)
}

/// Records a person's accepted input: a prompt or an answer, and the page
/// reviews a prompt carries. An agent's input is its own business, and
/// input not accepted was not sent.
fn record_input(
    runtime: &ProfileRuntime,
    caller: &Caller,
    shape: Option<InputShape>,
    queued: Option<bool>,
) {
    let (Caller::Person { .. }, Some(shape), Some(queued)) = (caller, shape, queued) else {
        return;
    };
    runtime
        .analytics()
        .record(shape.event(origin(runtime, caller), queued));
    for event in shape.page_reviews() {
        runtime.analytics().record(event);
    }
}

/// A first prompt, as analytics counts it, for an agent of `kind`.
fn first_prompt(
    kind: Option<analytics::Kind>,
    prompt: Option<&wire::PromptSubmission>,
) -> Option<InputShape> {
    let (kind, prompt) = kind.zip(prompt)?;
    let pages = prompt
        .prompt
        .as_ref()
        .map(telemetry::pages)
        .unwrap_or_default();
    Some(InputShape::Prompt(kind, pages))
}

/// Whether a create asks for its kind with nothing but the defaults: a
/// permission mode, a model or provider arguments are the person's to
/// choose, never another agent's.
fn only_defaults(agent: &create_agent_request::Agent) -> bool {
    match agent {
        create_agent_request::Agent::ClaudePty(config)
        | create_agent_request::Agent::ClaudeSdk(config) => *config == Default::default(),
        create_agent_request::Agent::Codex(config) => *config == Default::default(),
    }
}

#[tonic::async_trait]
impl AgentService for Agents {
    async fn create_agent(
        &self,
        request: Request<CreateAgentRequest>,
    ) -> Result<Response<CreateAgentResponse>, Status> {
        let caller = Caller::of(&request)?;
        let request = request.into_inner();
        if let (Caller::Agent { .. }, Some(agent)) = (&caller, &request.agent)
            && !only_defaults(agent)
        {
            return Err(status(wire_error(
                ErrorCode::PermissionDenied,
                "an agent starts children with their kind's default settings only",
            )));
        }
        let runtime = self.runtime()?;
        let kind = request
            .agent
            .as_ref()
            .and_then(|agent| telemetry::kind(agent.kind()));
        let first = first_prompt(kind, request.initial_prompt.as_ref());
        let created = runtime
            .create(request, &caller)
            .await
            .map_err(|error| status(error.to_wire()))?;
        if let Some(kind) = kind {
            let by = match caller {
                Caller::Person { .. } => analytics::By::Person,
                Caller::Agent { .. } => analytics::By::Agent,
            };
            runtime.analytics().record(analytics::Event::AgentCreated {
                kind,
                from: origin(&runtime, &caller),
                by,
            });
            record_input(&runtime, &caller, first, Some(false));
        }
        // A person's agent started from a paired device, the phone or
        // another computer, is what finishes the onboarding's first-agent
        // step on this host.
        if let Caller::Person {
            at: At::Paired(_), ..
        } = caller
            && let Err(error) = runtime.onboarding().agent_from_device(runtime.profile())
        {
            tracing::warn!(%error, "recording the first agent from a device");
        }
        Ok(Response::new(CreateAgentResponse {
            agent: Some(created),
        }))
    }

    async fn rename_agent(
        &self,
        request: Request<RenameAgentRequest>,
    ) -> Result<Response<RenameAgentResponse>, Status> {
        let (runtime, _, own) = self
            .acting_on(
                &request,
                &request.get_ref().agent_id,
                "an agent renames only its own children",
            )
            .await?;
        runtime
            .rename(own.id, &request.get_ref().name)
            .await
            .map(|agent| Response::new(RenameAgentResponse { agent: Some(agent) }))
            .map_err(|error| status(error.to_wire()))
    }

    async fn set_filing(
        &self,
        request: Request<SetFilingRequest>,
    ) -> Result<Response<SetFilingResponse>, Status> {
        let caller = Caller::of(&request)?;
        let Some(filing) = daemon_core::Filing::from_wire(request.get_ref().filing()) else {
            return Err(status(wire_error(
                ErrorCode::InvalidArgument,
                "a filing is open, pinned or closed",
            )));
        };
        // An agent closes its own children, as it ends their work; filing
        // them any other way is the person's.
        if matches!(caller, Caller::Agent { .. }) && filing != daemon_core::Filing::Closed {
            return Err(status(wire_error(
                ErrorCode::PermissionDenied,
                "only the person pins or un-settles an agent",
            )));
        }
        let (runtime, _, own) = self
            .acting_on(
                &request,
                &request.get_ref().agent_id,
                "an agent settles only its own children",
            )
            .await?;
        runtime
            .file(own.id, filing)
            .await
            .map(|agent| Response::new(SetFilingResponse { agent: Some(agent) }))
            .map_err(|error| status(error.to_wire()))
    }

    async fn resume_agent(
        &self,
        request: Request<ResumeAgentRequest>,
    ) -> Result<Response<ResumeAgentResponse>, Status> {
        let (runtime, caller, own) = self
            .acting_on(
                &request,
                &request.get_ref().agent_id,
                "an agent resumes only its own children",
            )
            .await?;
        let request = request.into_inner();
        let first = first_prompt(
            telemetry::kind_coded(own.row.kind),
            request.initial_prompt.as_ref(),
        );
        let bootstrap = request
            .initial_prompt
            .map(crate::spec::prompt_bootstrap)
            .transpose()
            .map_err(|error| status(error.to_wire()))?;
        let resumed = if request.if_suspended {
            runtime.wake(own.id, bootstrap).await
        } else {
            runtime.resume(own.id, bootstrap).await
        }
        .map_err(|error| status(error.to_wire()))?;
        record_input(&runtime, &caller, first, Some(false));
        Ok(Response::new(ResumeAgentResponse {
            agent: Some(resumed),
        }))
    }

    async fn delete_agent(
        &self,
        request: Request<DeleteAgentRequest>,
    ) -> Result<Response<DeleteAgentResponse>, Status> {
        let (runtime, _, own) = self
            .acting_on(
                &request,
                &request.get_ref().agent_id,
                "an agent deletes only its own children",
            )
            .await?;
        // The consent covers only the uncommitted changes of a workspace
        // the delete removes; the host looks for them itself as it does.
        runtime
            .delete_discarding(own.id, request.get_ref().discard_workspace_changes)
            .await
            .map(|()| Response::new(DeleteAgentResponse {}))
            .map_err(|error| status(error.to_wire()))
    }

    async fn preview_delete(
        &self,
        request: Request<PreviewDeleteRequest>,
    ) -> Result<Response<PreviewDeleteResponse>, Status> {
        // Worked out beside the changes, where the rule that who may ask
        // is who may delete is applied too.
        let caller = Caller::of(&request)?;
        let runtime = self.runtime()?;
        super::changes::preview_delete(&runtime, request.into_inner(), &caller).await
    }

    async fn send_input(
        &self,
        request: Request<SendInputRequest>,
    ) -> Result<Response<wire::SendInputResponse>, Status> {
        let (runtime, caller, own) = self
            .acting_on(
                &request,
                &request.get_ref().agent_id,
                "an agent sends input only to its own children",
            )
            .await?;
        let request = request.into_inner();
        let shape = request.input.as_ref().and_then(InputShape::of);
        let request = wire::SendInputRequest {
            agent_id: own.id.as_bytes().to_vec(),
            input: request.input,
        };
        let verdict = runtime
            .send_input(&request)
            .await
            .map_err(|error| status(error.to_wire()))?;
        record_input(&runtime, &caller, shape, telemetry::accepted(&verdict));
        Ok(Response::new(verdict))
    }

    async fn send_message(
        &self,
        request: Request<SendMessageRequest>,
    ) -> Result<Response<SendMessageResponse>, Status> {
        let caller = Caller::of(&request)?;
        let request = request.into_inner();
        if request.id.len() != SEND_ID_LEN {
            return Err(status(wire_error(
                ErrorCode::InvalidArgument,
                format!(
                    "a message's id is {SEND_ID_LEN} bytes, not {}",
                    request.id.len()
                ),
            )));
        }
        if request.message.is_none() {
            return Err(status(wire_error(
                ErrorCode::InvalidArgument,
                "a message carries words or attachments",
            )));
        }
        let runtime = self.runtime()?;
        // Any agent may message any agent, so the only question about the
        // recipient is whether it is this host's to deliver to.
        own_agent(&runtime, &request.agent_id).await?;
        runtime
            .send_message_by(request, &caller)
            .await
            .map(|sent| {
                Response::new(SendMessageResponse {
                    envelope_id: sent.envelope_id,
                })
            })
            .map_err(|error| status(error.to_wire()))
    }

    async fn put_blob(
        &self,
        request: Request<PutBlobRequest>,
    ) -> Result<Response<PutBlobResponse>, Status> {
        let caller = Caller::of(&request)?;
        let request = request.into_inner();
        let put = wire::PutBlobRequest {
            agent_id: request.agent_id,
            name: request.name,
            mime: request.mime,
            bytes: request.bytes,
        };
        // The owner holds the limit, whoever carried the bytes here: refused
        // before anything is looked up, in the limit's own words.
        crate::blobs::within_limit(&put).map_err(|error| status(error.to_wire()))?;
        let runtime = self.runtime()?;
        let (_, row) = own_agent(&runtime, &put.agent_id).await?;
        person_or_parent(
            &caller,
            &row,
            "an agent adds files only to its own children",
        )?;
        runtime
            .put_blob(put)
            .await
            .map(|blob| Response::new(PutBlobResponse { blob: Some(blob) }))
            .map_err(|error| status(error.to_wire()))
    }

    async fn get_diff(
        &self,
        request: Request<GetDiffRequest>,
    ) -> Result<Response<GetDiffResponse>, Status> {
        let (runtime, _, own) = self
            .acting_on(
                &request,
                &request.get_ref().agent_id,
                "an agent reads only its own children's changes",
            )
            .await?;
        let request = request.into_inner();
        let request = wire::GetDiffRequest {
            agent_id: own.id.as_bytes().to_vec(),
            base: request.base,
            with_patch: request.with_patch,
        };
        runtime
            .get_diff(request)
            .await
            .map(|diff| Response::new(GetDiffResponse { diff: Some(diff) }))
            .map_err(|error| status(error.to_wire()))
    }
}
