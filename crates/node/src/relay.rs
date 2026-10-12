//! Inputs and agent messages.
//!
//! A person's input is relayed to the agent over its control socket and
//! the interpreter's verdict comes back as the answer; the daemon answers
//! only for an agent that has exited. An agent message goes through the
//! recipient's lane: one message at a time, deduped against the
//! recipient's items by envelope id, and accepted only once the item the
//! recipient wrote on accepting it has committed. A retry therefore always
//! arrives after the original's answer, so that one store lookup is the
//! whole dedupe. A parent's message to its exited child resumes the child
//! with the message as the new incarnation's first input.

use std::sync::Arc;
use std::time::Duration;

use store::{AgentKey, StoreError};
use tokio::sync::oneshot;
use uuid::Uuid;
use wire::{
    AgentSender, Bootstrap, Envelope, EnvelopeKind, ErrorCode, Human, Lifecycle, Refusal, Rejected,
    SendInputRequest, SendInputResponse, SendMessageRequest, SendMessageResponse, Sender,
    bootstrap, ctl_frame, send_input_response, sender,
};

use crate::runtime::{AgentId, ProfileRuntime, RegistryError, Sent, ms};

/// How long an input waits for the agent to be connected.
const CONNECT_PATIENCE: Duration = Duration::from_secs(5);
/// How long an accepted message waits for its acceptance item to commit.
const ACCEPT_PATIENCE: Duration = Duration::from_secs(60);
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
    #[error("rejected: {}", said(.0))]
    Rejected(Rejected),
    #[error("the recipient's incarnation the message was for has ended")]
    Stale,
    #[error("resuming the agent: {0}")]
    Resume(Box<RegistryError>),
    /// A file the message carries could not be brought to the recipient's
    /// host, so the recipient could not open it; the message is not sent.
    #[error("the message's attachment {name} could not be brought to the recipient: {why}")]
    Attachment { name: String, why: String },
    #[error(transparent)]
    Forwarded(#[from] crate::forward::ForwardError),
    #[error("the store: {0}")]
    Store(#[from] StoreError),
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
            Self::Resume(_) | Self::Store(_) => ErrorCode::Internal,
        };
        wire::Error {
            code: code as i32,
            message: self.to_string(),
            details: Vec::new(),
        }
    }
}

/// Who a message is from.
enum From {
    /// A person, on the profile socket.
    Person,
    /// One of this host's agents, on its tools socket.
    Agent(AgentId),
    /// A paired host's agent.
    Peer(AgentSender),
    /// The person, through a paired host.
    PeerPerson,
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

/// A refusal as an error or a log line says it: its own words, else its
/// value's name in lower case ("exited").
fn said(rejected: &Rejected) -> String {
    rejected.detail.clone().unwrap_or_else(|| {
        rejected
            .refusal()
            .as_str_name()
            .trim_start_matches("REFUSAL_")
            .to_ascii_lowercase()
    })
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

/// The envelope id of an outbox row: the same for every re-send of one
/// row, so the receiver's lookup by envelope id catches a duplicate.
pub(crate) fn delivery_envelope_id(delivery: &store::Delivery) -> Vec<u8> {
    let mut name = delivery.child_id.clone();
    name.extend(delivery.incarnation.to_be_bytes());
    name.extend(delivery.kind.to_be_bytes());
    name.extend(delivery.turn_id.to_be_bytes());
    Uuid::new_v5(&Uuid::NAMESPACE_OID, &name)
        .as_bytes()
        .to_vec()
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
    /// cannot name one. It is an ordinary message: finished and failed
    /// messages come only from the outbox. Answers once the recipient has
    /// accepted.
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
            kind: EnvelopeKind::Message as i32,
            text: request.text,
            ended: None,
            incarnation: None,
            attachments: request.attachments,
            outcome: None,
        };
        self.send_message_from(envelope, caller.map_or(From::Person, From::Agent))
            .await
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
            Some(sender::Value::Agent(sender)) => From::Peer(sender),
            _ => From::PeerPerson,
        };
        self.send_message_from(envelope, from).await
    }

    async fn send_message_from(
        &self,
        mut envelope: Envelope,
        caller: From,
    ) -> Result<SendMessageResponse, RelayError> {
        let to = envelope.to.clone().ok_or(RelayError::NoAgent)?;
        if envelope.id.is_empty() {
            envelope.id = Uuid::new_v4().as_bytes().to_vec();
        }
        if to.host_id != self.host().as_bytes() {
            return self.forward_message(envelope, caller, &to.host_id).await;
        }
        let id = self.own_agent(&to.agent_id).await?;
        let (from, is_parent) = {
            let store = self.store.lock().await;
            let recipient = store.agent(&self.key(id))?.ok_or(RelayError::NoAgent)?;
            match caller {
                From::Person | From::PeerPerson => (
                    Sender {
                        value: Some(sender::Value::Human(Human {})),
                    },
                    false,
                ),
                From::Agent(caller) => {
                    let row = store.agent(&self.key(caller))?;
                    let is_parent = recipient.parent.as_ref() == Some(&self.key(caller));
                    (self.agent_sender(caller, row.as_ref()), is_parent)
                }
                From::Peer(sender) => {
                    let key = AgentKey::new(sender.host_id.clone(), sender.agent_id.clone());
                    let is_parent = recipient.parent.as_ref() == Some(&key);
                    (
                        Sender {
                            value: Some(sender::Value::Agent(sender)),
                        },
                        is_parent,
                    )
                }
            }
        };
        envelope.from = Some(from);
        self.bring_attachments(id, &envelope).await?;
        let envelope_id = envelope.id.clone();
        let incarnation = envelope.incarnation;
        self.deliver(id, envelope, is_parent, incarnation).await?;
        Ok(SendMessageResponse { envelope_id })
    }

    /// A message for another host's agent: the sender is set here, from
    /// who called, and the recipient's own daemon does the rest. A message
    /// that arrived from a peer is not passed on.
    async fn forward_message(
        &self,
        mut envelope: Envelope,
        caller: From,
        host: &[u8],
    ) -> Result<SendMessageResponse, RelayError> {
        let host = Uuid::from_slice(host).map_err(|_| RelayError::NoAgent)?;
        envelope.from = Some(match caller {
            From::Person => Sender {
                value: Some(sender::Value::Human(Human {})),
            },
            From::Agent(caller) => {
                let row = self.store.lock().await.agent(&self.key(caller))?;
                self.agent_sender(caller, row.as_ref())
            }
            From::Peer(_) | From::PeerPerson => return Err(RelayError::OtherHost),
        });
        Ok(self
            .on_peer(host, |mut client| async move {
                client.send_message(envelope).await
            })
            .await?)
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
        envelope: &Envelope,
    ) -> Result<(), RelayError> {
        let Some(sender::Value::Agent(sender)) =
            envelope.from.as_ref().and_then(|from| from.value.as_ref())
        else {
            return Ok(());
        };
        let blobs = self.agent_dir(recipient).join(agent_dir::BLOBS);
        for blob in envelope.attachments.iter().filter_map(blob_of) {
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
                agent_id: sender.agent_id.clone(),
                hash: blob.hash.clone(),
            };
            let bytes = if sender.host_id == self.host().as_bytes() {
                self.get_blob(request)
                    .await
                    .map_err(|error| failed(error.to_string()))?
                    .bytes
            } else {
                let host = Uuid::from_slice(&sender.host_id)
                    .map_err(|_| failed("its sender names no host".to_owned()))?;
                self.on_peer(
                    host,
                    |mut client| async move { client.get_blob(request).await },
                )
                .await
                .map_err(|error| failed(error.to_string()))?
                .bytes
            };
            use sha2::Digest as _;
            if sha2::Sha256::digest(&bytes)[..] != blob.hash[..] {
                return Err(failed("the bytes that came are not the file".to_owned()));
            }
            let (dir, hash) = (blobs.clone(), blob.hash.clone());
            tokio::task::spawn_blocking(move || crate::blobs::write_blob(&dir, &hash, &bytes))
                .await
                .map_err(|error| failed(error.to_string()))?
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

    /// The recipient's lane: one message at a time, answered only once
    /// accepted and committed. `resume` lets an exited recipient be resumed
    /// with the message, which only its parent may do. A message for one
    /// `incarnation` of the recipient is stale once another has begun.
    pub(crate) async fn deliver(
        &self,
        id: AgentId,
        envelope: Envelope,
        resume: bool,
        incarnation: Option<u32>,
    ) -> Result<(), RelayError> {
        let lane = self.lane(id);
        let _turn = lane.lock().await;
        // Until every journal has been read once after start, the lookup
        // below could miss an acceptance item still in a journal.
        let mut read = self.journals_read.subscribe();
        let _ = read.wait_for(|read| *read).await;

        let key = self.key(id);
        let row = {
            let store = self.store.lock().await;
            if store.item_by_input(&key, &envelope.id)?.is_some() {
                return Ok(());
            }
            store.agent(&key)?.ok_or(RelayError::NoAgent)?
        };
        // Checked again under the lane: whoever held it may have resumed
        // the recipient while this message waited.
        if incarnation.is_some_and(|incarnation| incarnation != row.incarnation) {
            return Err(RelayError::Stale);
        }
        if row.lifecycle == Lifecycle::Exited as i32 {
            return if resume {
                self.resume_with(id, envelope).await
            } else {
                Err(RelayError::Rejected(refused(Refusal::Exited)))
            };
        }
        let verdict = self
            .relay(
                id,
                envelope.id.clone(),
                ctl_frame::Of::AgentMessage(envelope.clone()),
            )
            .await?;
        match rejection(&verdict) {
            None => self.accepted(id, &key, &envelope.id).await,
            Some(rejected) if rejected.refusal() == Refusal::Exiting => {
                self.mark_exiting(id);
                if resume {
                    self.resume_with(id, envelope).await
                } else {
                    Err(RelayError::Rejected(refused(Refusal::Exited)))
                }
            }
            Some(rejected) => Err(RelayError::Rejected(rejected.clone())),
        }
    }

    /// Resumes an exited child with its parent's message as the new
    /// incarnation's first input.
    async fn resume_with(&self, id: AgentId, envelope: Envelope) -> Result<(), RelayError> {
        let envelope_id = envelope.id.clone();
        let bootstrap = Bootstrap {
            of: Some(bootstrap::Of::AgentMessage(envelope)),
        };
        self.resume(id, Some(bootstrap))
            .await
            .map_err(|error| RelayError::Resume(Box::new(error)))?;
        self.accepted(id, &self.key(id), &envelope_id).await
    }

    /// Waits for the item the recipient wrote on accepting the message.
    async fn accepted(
        &self,
        id: AgentId,
        key: &AgentKey,
        envelope_id: &[u8],
    ) -> Result<(), RelayError> {
        let mut commits = self.commits.subscribe();
        let deadline = tokio::time::Instant::now() + ACCEPT_PATIENCE;
        loop {
            commits.borrow_and_update();
            if self
                .store
                .lock()
                .await
                .item_by_input(key, envelope_id)?
                .is_some()
            {
                return Ok(());
            }
            let exited = self.handle(id).is_none_or(|handle| *handle.exited.borrow());
            if exited {
                // Everything the process wrote was ingested before its exit
                // was recorded, and the item was not in it.
                return Err(RelayError::Lost);
            }
            match tokio::time::timeout_at(deadline, commits.changed()).await {
                Ok(Ok(())) => {}
                _ => return Err(RelayError::Lost),
            }
        }
    }

    fn lane(&self, id: AgentId) -> Arc<tokio::sync::Mutex<()>> {
        self.lanes.lock().unwrap().entry(id).or_default().clone()
    }

    /// Whether an input to the agent is in hand: written to its process
    /// and awaiting the verdict. A message whose lane is taken but which is
    /// not written yet is not in hand: a caller that goes away then takes
    /// it with it.
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
