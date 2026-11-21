//! Inputs and agent messages.
//!
//! A person's input is relayed to the agent over its control socket and
//! the interpreter's verdict comes back as the answer; the daemon answers
//! only for an agent that has exited, and wakes a suspended one with it. An agent message goes to the daemon
//! core, which hands it through the recipient's lane and answers once the
//! recipient has accepted it; what stays here is setting the sender of a
//! message for another host, and bringing the files a message carries to
//! the recipient's host before it is sent.

use std::time::Duration;

use daemon_core::payload::{
    CallError, ClientRequest, MessageFrom, MessageRefusal, Status as CoreStatus,
};
use store::StoreError;
use tokio::sync::oneshot;
use uuid::Uuid;
use wire::{
    AgentRef, AgentSender, Envelope, ErrorCode, Human, Lifecycle, Refusal, Rejected,
    SendInputRequest, SendInputResponse, SendMessageResponse, Sender, ctl_frame, envelope,
    send_input_response, sender,
};

use crate::driver::{Ask, Delivered};
use crate::runtime::{AgentId, ProfileRuntime, Sent, core_agent, ms};

/// How long an input waits for the agent to be connected.
const CONNECT_PATIENCE: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_millis(20);

#[derive(Debug, thiserror::Error)]
pub enum RelayError {
    #[error("no agent with that id")]
    NoAgent,
    #[error("the request carries no input")]
    NoInput,
    #[error("the agent lives on another host")]
    OtherHost,
    #[error("the agent is not connected")]
    Unavailable,
    #[error("the agent's answer was lost; the input may or may not have arrived")]
    Lost,
    #[error("{}", said(.0))]
    Rejected(Rejected),
    #[error("the recipient's incarnation the message was for has ended")]
    Stale,
    #[error("resuming the agent: {0:?}")]
    Resume(daemon_core::Refusal),
    /// A person's input to a suspended agent could not wake it.
    #[error("waking the agent: {0}")]
    Wake(crate::runtime::RegistryError),
    /// A file the message carries could not be brought to the recipient's
    /// host, so the recipient could not open it; the message is not sent.
    #[error("the message's attachment {name} could not be brought to the recipient: {why}")]
    Attachment { name: String, why: String },
    #[error(transparent)]
    Forwarded(#[from] crate::forward::ForwardError),
    #[error("the store: {0}")]
    Store(#[from] StoreError),
    /// The core could not take the message, in its words.
    #[error("{0}")]
    Core(String),
}

impl RelayError {
    pub fn to_wire(&self) -> wire::Error {
        let code = match self {
            Self::NoAgent => ErrorCode::NotFound,
            Self::NoInput => ErrorCode::InvalidArgument,
            Self::OtherHost => ErrorCode::Unimplemented,
            Self::Unavailable => ErrorCode::Unavailable,
            Self::Lost => ErrorCode::Aborted,
            Self::Rejected(_) => ErrorCode::FailedPrecondition,
            // The incarnation the message was for no longer exists, which
            // tells a sending host to drop its outbox row as surely as a
            // deleted recipient does.
            Self::Stale => ErrorCode::NotFound,
            Self::Forwarded(error) => return error.to_wire(),
            Self::Wake(error) => return error.to_wire(),
            Self::Attachment { .. } => ErrorCode::FailedPrecondition,
            Self::Resume(_) | Self::Store(_) | Self::Core(_) => ErrorCode::Internal,
        };
        wire::Error {
            code: code as i32,
            message: self.to_string(),
            details: Vec::new(),
        }
    }
}

pub(crate) fn rejected(refusal: Refusal) -> SendInputResponse {
    SendInputResponse {
        of: Some(send_input_response::Of::Rejected(refused(refusal))),
    }
}

fn refused(refusal: Refusal) -> Rejected {
    Rejected {
        refusal: refusal as i32,
        detail: None,
    }
}

/// The prompt a person's input carries, as a resume's first input takes
/// it; none for any other input, or a body this build cannot read.
fn prompt_of(input: &wire::Input) -> Option<wire::PromptSubmission> {
    use wire::{KindInput, acp_input, claude_pty_input, claude_sdk_input, codex_input};
    let prompt = match input.decode_body()? {
        KindInput::ClaudePty(input) => match input.of? {
            claude_pty_input::Of::Prompt(prompt) => prompt,
            _ => return None,
        },
        KindInput::ClaudeSdk(input) => match input.of? {
            claude_sdk_input::Of::Prompt(prompt) => prompt,
            _ => return None,
        },
        KindInput::Codex(input) => match input.of? {
            codex_input::Of::Prompt(prompt) => prompt,
            _ => return None,
        },
        KindInput::Acp(input) => match input.of? {
            acp_input::Of::Prompt(prompt) => prompt,
            _ => return None,
        },
    };
    Some(wire::PromptSubmission {
        input_id: input.input_id.clone(),
        prompt: Some(prompt),
    })
}

fn rejection(verdict: &SendInputResponse) -> Option<&Rejected> {
    match &verdict.of {
        Some(send_input_response::Of::Rejected(rejected)) => Some(rejected),
        _ => None,
    }
}

/// Refuses a message the recipient could not read whole. Where a later
/// build added a body or an attachment, this build decodes nothing in its
/// place: delivered, it would read as an empty message, or as one with a
/// file missing.
fn readable(envelope: &Envelope) -> Result<(), RelayError> {
    let refused = |detail: &str| {
        RelayError::Rejected(Rejected {
            refusal: Refusal::Unsupported as i32,
            detail: Some(detail.to_owned()),
        })
    };
    match &envelope.body {
        None => Err(refused(
            "this host can't read what the message holds; the host needs updating",
        )),
        Some(envelope::Body::Message(message))
            if !attachments::all_readable(&message.attachments) =>
        {
            Err(refused(attachments::UNREADABLE))
        }
        Some(_) => Ok(()),
    }
}

/// A refusal as an error says it: its own words, else a sentence a sending
/// agent's model can act on. The recipient goes unnamed because the sender
/// already knows whom it addressed.
fn said(rejected: &Rejected) -> String {
    if let Some(detail) = &rejected.detail {
        return detail.clone();
    }
    match rejected.refusal() {
        Refusal::Unspecified => "the recipient refused the message",
        Refusal::ClosedAsk => "the question the message answers is no longer open",
        Refusal::NotQueued => "the message it names is no longer waiting",
        Refusal::Unsupported => "the recipient cannot take that input",
        Refusal::Draining => "the recipient is finishing its last turn and takes nothing new",
        Refusal::Exiting => "the recipient is stopping",
        Refusal::Exited => "the recipient has stopped",
        Refusal::AgentClosed => {
            "the recipient is settled, done with; only the person or its parent start it again"
        }
        Refusal::HostUnreachable => "the recipient's host cannot be reached",
        Refusal::ConversationElsewhere => {
            "Claude's own view switched to another conversation, so the recipient's \
             conversation is not the one on its screen"
        }
        Refusal::TooLong => "the message is longer than the recipient takes",
    }
    .to_owned()
}

/// Whether a provider subagent takes this input through its host: an
/// answer to one of its asks, or a stop of a job, its own included. A body
/// that does not decode is not one.
fn passes_to_subagent(input: &wire::Input) -> bool {
    use prost::Message as _;
    let body = input.body.as_slice();
    match input.kind() {
        wire::Kind::ClaudeSdk => wire::ClaudeSdkInput::decode(body).is_ok_and(|input| {
            matches!(
                input.of,
                Some(
                    wire::claude_sdk_input::Of::Answer(_) | wire::claude_sdk_input::Of::StopJob(_)
                )
            )
        }),
        wire::Kind::ClaudePty => wire::ClaudePtyInput::decode(body)
            .is_ok_and(|input| matches!(input.of, Some(wire::claude_pty_input::Of::Answer(_)))),
        wire::Kind::Codex => wire::CodexInput::decode(body).is_ok_and(|input| {
            matches!(
                input.of,
                Some(wire::codex_input::Of::Answer(_) | wire::codex_input::Of::StopJob(_))
            )
        }),
        // An ACP agent's own subagents are not amux agents.
        wire::Kind::Acp | wire::Kind::Unspecified => false,
    }
}

impl ProfileRuntime {
    /// An own agent's id from request bytes: not found, or on another host.
    async fn own_agent(&self, agent_id: &[u8]) -> Result<AgentId, RelayError> {
        let id = Uuid::from_slice(agent_id).map_err(|_| RelayError::NoAgent)?;
        let by_id = agent_id.to_vec();
        match self
            .read_rows(move |store| store.agent_by_id(&by_id))
            .await?
        {
            Some(row) if row.agent.host == self.host().as_bytes() => Ok(id),
            Some(_) => Err(RelayError::OtherHost),
            None => Err(RelayError::NoAgent),
        }
    }

    /// Relays a person's input and returns the interpreter's verdict. An
    /// exited agent is answered here; one that refuses as exiting is from
    /// then on treated as leaving, so a resume waits for it. Every way a
    /// person sends input comes here, from this host's clients or forwarded
    /// by another's, so this is where input to an agent with no process,
    /// or to one leaving of its own accord, wakes it if it is or ends up
    /// suspended (see [`Self::wake_with`]).
    pub async fn send_input(
        &self,
        request: &SendInputRequest,
    ) -> Result<SendInputResponse, RelayError> {
        let input = request.input.clone().ok_or(RelayError::NoInput)?;
        let id = self.own_agent(&request.agent_id).await?;
        let key = self.key(id);
        let row = self
            .read_rows(move |store| store.agent(&key))
            .await?
            .ok_or(RelayError::NoAgent)?;
        if row.lifecycle == Lifecycle::Exited as i32 {
            return Ok(rejected(Refusal::Exited));
        }
        if row.native.is_some() {
            return self.send_subagent_input(&row, input).await;
        }
        // Only a person changes a chat's model or effort, so it is the same
        // act as picking them for a new agent, and sticks the same way.
        let picked = crate::spec::Picked::of_input(&input, row.provider_id());
        // A row that says suspended has no process to relay to: asked
        // first, the wake does not write to a connection its process left
        // behind for a moment, which would lose the input.
        let verdict = if row.exit_cause == wire::ExitCause::Suspended as i32 {
            rejected(Refusal::Exited)
        } else {
            self.relay(
                id,
                input.input_id.clone(),
                ctl_frame::Of::Input(input.clone()),
            )
            .await?
        };
        let verdict = match rejection(&verdict).map(Rejected::refusal) {
            Some(Refusal::Exited) => self.wake_with(id, input, verdict).await?,
            // On its way out: as it suspends the input waits for the exit
            // and wakes it. One the daemon is stopping is answered at once,
            // not after the stop's deadline.
            Some(Refusal::Exiting) => {
                self.mark_exiting(id);
                self.wake_with(id, input, verdict).await?
            }
            _ => verdict,
        };
        if rejection(&verdict).is_none()
            && let Some(picked) = picked
        {
            self.remember_pick(picked).await;
        }
        Ok(verdict)
    }

    /// Input the agent's process could not take, as it has none or is on
    /// its way out: a suspended agent, or one suspending, is woken by it. A
    /// prompt is the next incarnation's first input, so the answer is that
    /// it was taken; any other input goes to the new process once it is
    /// connected. The core decides: it waits out an exit the agent chose
    /// and then wakes it only if the exit left it suspended, else the
    /// input is refused as exited; an agent the daemon is stopping is
    /// answered at once with the refusal the input had.
    async fn wake_with(
        &self,
        id: AgentId,
        input: wire::Input,
        refused: SendInputResponse,
    ) -> Result<SendInputResponse, RelayError> {
        use crate::runtime::RegistryError;
        let first = prompt_of(&input)
            .map(crate::spec::prompt_bootstrap)
            .transpose()
            .map_err(RelayError::Wake)?;
        let seeded = first.is_some();
        let mut refused = refused;
        // Twice at most: a process found running is handed the input, and
        // one that refuses it as it suspends, or is gone by then, is woken
        // once more.
        for _ in 0..2 {
            match self.wake(id, first.clone()).await {
                Ok(_) if seeded => {
                    return Ok(SendInputResponse {
                        of: Some(send_input_response::Of::Accepted(wire::Accepted {
                            queued: false,
                        })),
                    });
                }
                // Running: started by something else first, still on its way
                // out, or woken for an input that is not a prompt.
                Ok(_) | Err(RegistryError::Live(_)) => {
                    let verdict = self
                        .relay(
                            id,
                            input.input_id.clone(),
                            ctl_frame::Of::Input(input.clone()),
                        )
                        .await?;
                    match rejection(&verdict).map(Rejected::refusal) {
                        Some(Refusal::Exiting) => self.mark_exiting(id),
                        // Gone between the two: the wake decides again.
                        Some(Refusal::Exited) => {}
                        _ => return Ok(verdict),
                    }
                    refused = verdict;
                }
                Err(RegistryError::NotSuspended(_)) => return Ok(rejected(Refusal::Exited)),
                // The daemon is stopping it, or it did not let go of its
                // directory in time: as refused.
                Err(RegistryError::Ending(_) | RegistryError::StillLocked(_)) => {
                    return Ok(refused);
                }
                Err(error) => return Err(RelayError::Wake(error)),
            }
        }
        Ok(refused)
    }

    /// A person's input to a provider subagent, which has no process: an
    /// answer to its ask or a stop of its own job goes to the process of
    /// the agent that runs it, whose interpreter answers the provider for
    /// it, and that process's verdict is the answer. Anything else is
    /// refused here, since neither provider lets anything but the parent's
    /// model talk to a subagent.
    async fn send_subagent_input(
        &self,
        row: &store::AgentRow,
        input: wire::Input,
    ) -> Result<SendInputResponse, RelayError> {
        if !passes_to_subagent(&input) {
            return Ok(SendInputResponse {
                of: Some(send_input_response::Of::Rejected(Rejected {
                    refusal: Refusal::Unsupported as i32,
                    detail: Some(daemon_core::subagents::NO_MESSAGES.to_owned()),
                })),
            });
        }
        let host = row
            .parent
            .as_ref()
            .and_then(|parent| Uuid::from_slice(&parent.agent).ok())
            .ok_or(RelayError::NoAgent)?;
        let input_id = input.input_id.clone();
        let child = wire::ChildInput {
            agent_id: row.agent.agent.clone(),
            input: Some(input),
        };
        self.relay(host, input_id, ctl_frame::Of::ChildInput(child))
            .await
    }

    /// Hands an input or an agent message to the agent over its control
    /// connection and waits for the verdict, which comes back keyed by
    /// `input_id`: the input's id, or the message's envelope id.
    pub(crate) async fn relay(
        &self,
        id: AgentId,
        input_id: Vec<u8>,
        of: ctl_frame::Of,
    ) -> Result<SendInputResponse, RelayError> {
        let Some(handle) = self.handle(id) else {
            return Ok(rejected(Refusal::Exited));
        };
        let (tx, rx) = oneshot::channel();
        let write = handle.expect_reply(input_id.clone(), tx);
        let frame = wire::CtlFrame { of: Some(of) };
        let launch = self.launch();
        let connect_by = tokio::time::Instant::now() + CONNECT_PATIENCE;
        loop {
            match handle.write_ctl(&frame, ms(launch.ctl_write_ms)).await {
                Sent::Written => break,
                Sent::Failed => {
                    handle.forget_reply(&input_id, write);
                    return Err(RelayError::Lost);
                }
                Sent::NoConnection => {}
            }
            if *handle.exited.borrow() {
                handle.forget_reply(&input_id, write);
                return Ok(rejected(Refusal::Exited));
            }
            if tokio::time::Instant::now() >= connect_by {
                handle.forget_reply(&input_id, write);
                return Err(RelayError::Unavailable);
            }
            tokio::time::sleep(POLL).await;
        }
        let _handed = Handed::new(&handle);
        // Reply patience is policy time, on the profile's clock.
        let patience = self.clock().sleep_until(
            self.clock_now()
                .saturating_add_unsigned(launch.core.reply_patience_ms),
        );
        tokio::select! {
            waited = rx => match waited {
                Ok(Some(verdict)) => Ok(verdict),
                // The connection ended before the answer, or a later write
                // of the input waits in this one's place.
                Ok(None) | Err(_) => Err(RelayError::Lost),
            },
            () = patience => {
                handle.forget_reply(&input_id, write);
                Err(RelayError::Lost)
            }
        }
    }

    /// Sends a message a person or one of this host's agents submitted.
    /// `caller` is the agent whose tools socket the call came in on, else
    /// the person; the envelope's sender is set from it, whatever the
    /// envelope says. Answers once the recipient has accepted.
    pub async fn send_message(
        &self,
        envelope: Envelope,
        caller: Option<AgentId>,
    ) -> Result<SendMessageResponse, RelayError> {
        let from = caller.map_or(MessageFrom::Person, |caller| {
            MessageFrom::Agent(core_agent(caller))
        });
        self.send_message_from(envelope, from).await
    }

    /// A message a paired host sends on behalf of one of its agents, or of
    /// the person. The sending daemon stamped the sender from its own
    /// caller, so it is taken as given; one that names no agent is the
    /// person's.
    pub async fn send_peer_message(
        &self,
        envelope: Envelope,
    ) -> Result<SendMessageResponse, RelayError> {
        let from = match envelope.from.clone().and_then(|from| from.value) {
            Some(sender::Value::Agent(sender)) => MessageFrom::Peer(sender),
            _ => MessageFrom::PeerPerson,
        };
        self.send_message_from(envelope, from).await
    }

    async fn send_message_from(
        &self,
        mut envelope: Envelope,
        from: MessageFrom,
    ) -> Result<SendMessageResponse, RelayError> {
        let to = envelope.to.clone().ok_or(RelayError::NoAgent)?;
        if envelope.id.is_empty() {
            envelope.id = Uuid::new_v4().as_bytes().to_vec();
        }
        if to.host_id != self.host().as_bytes() {
            return self.forward_message(envelope, from, &to.host_id).await;
        }
        let id = self.own_agent(&to.agent_id).await?;
        readable(&envelope)?;
        let sender = match &from {
            MessageFrom::Agent(caller) => {
                Some((self.host().as_bytes().to_vec(), caller.0.to_vec()))
            }
            MessageFrom::Peer(sender) => {
                let agent = sender.agent.clone().unwrap_or_default();
                Some((agent.host_id, agent.agent_id))
            }
            MessageFrom::Person | MessageFrom::PeerPerson => None,
        };
        if let Some((host, agent)) = sender {
            self.bring_attachments(id, &host, &agent, &envelope).await?;
        }
        let envelope_id = envelope.id.clone();
        let request = ClientRequest::SendMessage {
            envelope: Box::new(envelope),
            from,
        };
        let mut answers = self.driver().call(Ask::Ready(request), 1).await;
        loop {
            match answers.next().await {
                Some(Delivered::Answer(_)) => {}
                Some(Delivered::End(CoreStatus::Ok)) => {
                    return Ok(SendMessageResponse { envelope_id });
                }
                Some(Delivered::End(CoreStatus::Failed(CallError::Message(refusal)))) => {
                    return Err(refused_message(refusal));
                }
                Some(Delivered::End(CoreStatus::Failed(CallError::NoAgent))) => {
                    return Err(RelayError::NoAgent);
                }
                Some(Delivered::End(CoreStatus::Failed(other))) => {
                    return Err(RelayError::Core(format!("{other:?}")));
                }
                // The profile stopped before the recipient answered.
                None => return Err(RelayError::Lost),
            }
        }
    }

    /// A message for another host's agent: the sender is set here, from
    /// who called, and the recipient's own daemon does the rest. A message
    /// that arrived from a peer is not passed on.
    async fn forward_message(
        &self,
        mut envelope: Envelope,
        from: MessageFrom,
        host: &[u8],
    ) -> Result<SendMessageResponse, RelayError> {
        let host = Uuid::from_slice(host).map_err(|_| RelayError::NoAgent)?;
        envelope.from = Some(match from {
            MessageFrom::Person => Sender {
                value: Some(sender::Value::Human(Human {})),
            },
            MessageFrom::Agent(caller) => {
                let caller = Uuid::from_bytes(caller.0);
                let key = self.key(caller);
                let row = self.read_rows(move |store| store.agent(&key)).await?;
                self.agent_sender(caller, row.as_ref())
            }
            MessageFrom::Peer(_) | MessageFrom::PeerPerson => return Err(RelayError::OtherHost),
        });
        Ok(self.on_peer(host, envelope).await?)
    }

    /// Puts the bytes of every file a message carries into the recipient's
    /// own blobs, from the sending agent's: copied when it runs on this
    /// host, fetched from its host otherwise. The recipient then reads each
    /// file at a path on its own host, exactly as a person's attachment. A
    /// message from a person carries files already stored with the
    /// recipient.
    async fn bring_attachments(
        &self,
        recipient: AgentId,
        sender_host: &[u8],
        sender_agent: &[u8],
        envelope: &Envelope,
    ) -> Result<(), RelayError> {
        let blobs = self.agent_dir(recipient).join(agent_dir::BLOBS);
        let Some(envelope::Body::Message(message)) = &envelope.body else {
            return Ok(());
        };
        for blob in message.attachments.iter().flat_map(attachments::blobs) {
            let name = if blob.name.is_empty() {
                crate::blobs::hex(&blob.hash)
            } else {
                blob.name.clone()
            };
            let failed = |why: String| RelayError::Attachment {
                name: name.clone(),
                why,
            };
            if blobs.join(crate::blobs::hex(&blob.hash)).is_file() {
                continue;
            }
            let request = wire::GetBlobRequest {
                agent_id: sender_agent.to_vec(),
                hash: blob.hash.clone(),
            };
            let bytes = if sender_host == self.host().as_bytes() {
                self.get_blob(request)
                    .await
                    .map_err(|error| failed(error.to_string()))?
                    .bytes
            } else {
                let host = Uuid::from_slice(sender_host)
                    .map_err(|_| failed("its sender names no host".to_owned()))?;
                self.on_peer(host, request)
                    .await
                    .map_err(|error| failed(error.to_string()))?
                    .bytes
            };
            use sha2::Digest as _;
            if sha2::Sha256::digest(&bytes)[..] != blob.hash[..] {
                return Err(failed("the bytes that came are not the file".to_owned()));
            }
            let put = wire::PutBlobRequest {
                agent_id: recipient.as_bytes().to_vec(),
                name: blob.name.clone(),
                mime: blob.mime.clone(),
                bytes,
            };
            self.put_blob(put)
                .await
                .map_err(|error| failed(error.to_string()))?;
        }
        Ok(())
    }

    pub(crate) fn agent_sender(&self, id: AgentId, row: Option<&store::AgentRow>) -> Sender {
        Sender {
            value: Some(sender::Value::Agent(AgentSender {
                agent: Some(AgentRef {
                    host_id: self.host().as_bytes().to_vec(),
                    agent_id: id.as_bytes().to_vec(),
                }),
                name: row.map(|row| row.name.clone()).unwrap_or_default(),
            })),
        }
    }

    /// Whether an input to the agent is in hand: written to its process
    /// and awaiting the verdict. A message waiting in its lane is not in
    /// hand: a caller that goes away then takes it with it.
    pub fn in_hand(&self, id: AgentId) -> bool {
        self.handle(id)
            .is_some_and(|handle| handle.handed.load(std::sync::atomic::Ordering::SeqCst) > 0)
    }
}

/// An input written to an agent's process, counted until its wait for the
/// verdict ends, however it ends.
struct Handed<'a>(&'a crate::runtime::AgentHandle);

impl<'a> Handed<'a> {
    fn new(handle: &'a crate::runtime::AgentHandle) -> Self {
        handle
            .handed
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Self(handle)
    }
}

impl Drop for Handed<'_> {
    fn drop(&mut self) {
        self.0
            .handed
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// A message the core did not get accepted, as the shell's callers read it.
fn refused_message(refusal: MessageRefusal) -> RelayError {
    match refusal {
        MessageRefusal::NoAgent => RelayError::NoAgent,
        MessageRefusal::OtherHost => RelayError::OtherHost,
        MessageRefusal::Unavailable => RelayError::Unavailable,
        MessageRefusal::Lost => RelayError::Lost,
        MessageRefusal::Rejected(rejected) => RelayError::Rejected(rejected),
        MessageRefusal::Stale => RelayError::Stale,
        MessageRefusal::Resume(refusal) => RelayError::Resume(refusal),
        MessageRefusal::Store(words) => RelayError::Core(format!("the store: {words}")),
    }
}
