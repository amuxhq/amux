//! Inputs and agent messages.
//!
//! A person's input is relayed to the agent over its control socket and
//! the interpreter's verdict comes back as the answer; the daemon answers
//! only for an agent that has exited. An agent message goes to the daemon
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
    AgentSender, Envelope, ErrorCode, Human, Lifecycle, MessageBody, Refusal, Rejected,
    SendInputRequest, SendInputResponse, SendMessageRequest, SendMessageResponse, Sender,
    ctl_frame, envelope, send_input_response, sender,
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

fn rejection(verdict: &SendInputResponse) -> Option<&Rejected> {
    match &verdict.of {
        Some(send_input_response::Of::Rejected(rejected)) => Some(rejected),
        _ => None,
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
        Refusal::Exited => {
            "the recipient has stopped; only a message from its parent starts it again"
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

/// The blob an attachment's bytes are, for the kinds that have one.
fn blob_of(attachment: &wire::Attachment) -> Option<&wire::BlobRef> {
    use wire::attachment::Of;
    match attachment.of.as_ref()? {
        Of::Image(blob) | Of::File(blob) => Some(blob),
        Of::Review(review) => review.diff.as_ref()?.patch.as_ref(),
        Of::Text(_) => None,
    }
}

impl ProfileRuntime {
    /// An own agent's id from request bytes: not found, or on another host.
    async fn own_agent(&self, agent_id: &[u8]) -> Result<AgentId, RelayError> {
        let id = Uuid::from_slice(agent_id).map_err(|_| RelayError::NoAgent)?;
        match self.store.lock().await.agent_by_id(agent_id)? {
            Some(row) if row.agent.host == self.host().as_bytes() => Ok(id),
            Some(_) => Err(RelayError::OtherHost),
            None => Err(RelayError::NoAgent),
        }
    }

    /// Relays a person's input and returns the interpreter's verdict. An
    /// exited agent is answered here; one that refuses as exiting is from
    /// then on treated as leaving, so a resume waits for it.
    pub async fn send_input(
        &self,
        request: &SendInputRequest,
    ) -> Result<SendInputResponse, RelayError> {
        let input = request.input.clone().ok_or(RelayError::NoInput)?;
        let id = self.own_agent(&request.agent_id).await?;
        let row = self
            .store
            .lock()
            .await
            .agent(&self.key(id))?
            .ok_or(RelayError::NoAgent)?;
        if row.lifecycle == Lifecycle::Exited as i32 {
            return Ok(rejected(Refusal::Exited));
        }
        let input_id = input.input_id.clone();
        let verdict = self
            .relay(id, input_id, ctl_frame::Of::Input(input))
            .await?;
        if rejection(&verdict).is_some_and(|rejected| rejected.refusal() == Refusal::Exiting) {
            self.mark_exiting(id);
        }
        Ok(verdict)
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
        handle.expect_reply(input_id.clone(), tx);
        let frame = wire::CtlFrame { of: Some(of) };
        let launch = self.launch();
        let connect_by = tokio::time::Instant::now() + CONNECT_PATIENCE;
        loop {
            match handle.write_ctl(&frame, ms(launch.ctl_write_ms)).await {
                Sent::Written => break,
                Sent::Failed => {
                    handle.forget_reply(&input_id);
                    return Err(RelayError::Lost);
                }
                Sent::NoConnection => {}
            }
            if *handle.exited.borrow() {
                handle.forget_reply(&input_id);
                return Ok(rejected(Refusal::Exited));
            }
            if tokio::time::Instant::now() >= connect_by {
                handle.forget_reply(&input_id);
                return Err(RelayError::Unavailable);
            }
            tokio::time::sleep(POLL).await;
        }
        let _handed = Handed::new(&handle);
        match tokio::time::timeout(ms(launch.reply_patience_ms), rx).await {
            Ok(Ok(verdict)) => Ok(verdict),
            // The connection ended before the answer: the watcher dropped
            // every waiting reply.
            Ok(Err(_)) => Err(RelayError::Lost),
            Err(_) => {
                handle.forget_reply(&input_id);
                Err(RelayError::Lost)
            }
        }
    }

    /// Sends a message a person or one of this host's agents submitted.
    /// `caller` is the agent whose tools socket the call came in on, else
    /// the person; the envelope's sender is set from it, since the request
    /// cannot name one. It is an ordinary message: a child's reports come
    /// only from the outbox. Answers once the recipient has accepted.
    pub async fn send_message(
        &self,
        request: SendMessageRequest,
        caller: Option<AgentId>,
    ) -> Result<SendMessageResponse, RelayError> {
        let envelope = Envelope {
            id: request.id,
            context: request.context,
            from: None,
            to: request.to,
            body: Some(envelope::Body::Message(MessageBody {
                text: request.text,
                attachments: request.attachments,
            })),
        };
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
        let sender = match &from {
            MessageFrom::Agent(caller) => {
                Some((self.host().as_bytes().to_vec(), caller.0.to_vec()))
            }
            MessageFrom::Peer(sender) => Some((sender.host_id.clone(), sender.agent_id.clone())),
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
                let row = self.store.lock().await.agent(&self.key(caller))?;
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
        for blob in message.attachments.iter().filter_map(blob_of) {
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
                agent_id: id.as_bytes().to_vec(),
                host_id: self.host().as_bytes().to_vec(),
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
