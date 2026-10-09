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

use store::{AgentKey, Store as _, StoreError};
use tokio::sync::oneshot;
use uuid::Uuid;
/// The agent has exited, or has decided to exit, which the daemon treats as
/// exited.
pub use wire::refusal::{EXITED, EXITING};
use wire::{
    AgentSender, Bootstrap, Envelope, EnvelopeKind, ErrorCode, Human, Lifecycle, Rejected,
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
    #[error("rejected: {0}")]
    Rejected(String),
    #[error("the recipient's incarnation the message was for has ended")]
    Stale,
    #[error("resuming the agent: {0}")]
    Resume(Box<RegistryError>),
    #[error(transparent)]
    Forwarded(#[from] crate::forward::ForwardError),
    #[error("{0}")]
    Forbidden(&'static str),
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
            Self::Resume(_) | Self::Store(_) => ErrorCode::Internal,
            Self::Forbidden(_) => ErrorCode::PermissionDenied,
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

pub(crate) fn rejected(reason: &str) -> SendInputResponse {
    SendInputResponse {
        of: Some(send_input_response::Of::Rejected(Rejected {
            reason: reason.to_owned(),
        })),
    }
}

fn rejection(verdict: &SendInputResponse) -> Option<&str> {
    match &verdict.of {
        Some(send_input_response::Of::Rejected(rejected)) => Some(&rejected.reason),
        _ => None,
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
        let store = self.store.lock().await;
        if store.agent(&self.key(id))?.is_some() {
            return Ok(id);
        }
        if store
            .agents()?
            .iter()
            .any(|row| row.agent.agent == agent_id)
        {
            return Err(RelayError::OtherHost);
        }
        Err(RelayError::NoAgent)
    }

    /// Relays a person's input and returns the interpreter's verdict. An
    /// exited agent is answered here; one that answers "exiting" is from
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
            return Ok(rejected(EXITED));
        }
        let input_id = input.input_id.clone();
        let verdict = self
            .relay(id, input_id, ctl_frame::Of::Input(input))
            .await?;
        if rejection(&verdict) == Some(EXITING) {
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
            return Ok(rejected(EXITED));
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
                return Ok(rejected(EXITED));
            }
            if tokio::time::Instant::now() >= connect_by {
                handle.forget_reply(&input_id);
                return Err(RelayError::Unavailable);
            }
            tokio::time::sleep(POLL).await;
        }
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
            incarnation: None,
        };
        self.send_message_from(envelope, caller.map_or(From::Person, From::Agent))
            .await
    }

    /// A message a paired host sends on behalf of one of its agents, or of
    /// the person. An agent sender it names must be that host's own agent:
    /// a host speaks only for its own agents.
    pub async fn send_peer_message(
        &self,
        envelope: Envelope,
        host: crate::HostId,
    ) -> Result<SendMessageResponse, RelayError> {
        let from = match envelope.from.clone().and_then(|from| from.value) {
            Some(sender::Value::Agent(sender)) if sender.host_id == host.as_bytes() => {
                From::Peer(sender)
            }
            Some(sender::Value::Human(_)) => From::PeerPerson,
            _ => {
                return Err(RelayError::Forbidden(
                    "a host sends messages only from its own agents",
                ));
            }
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

    pub(crate) fn agent_sender(&self, id: AgentId, row: Option<&store::AgentRow>) -> Sender {
        Sender {
            value: Some(sender::Value::Agent(AgentSender {
                agent_id: id.as_bytes().to_vec(),
                host_id: self.host().as_bytes().to_vec(),
                name: row.and_then(|row| row.name.clone()).unwrap_or_default(),
                kind: row.map(|row| row.kind.clone()).unwrap_or_default(),
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
                Err(RelayError::Rejected(EXITED.to_owned()))
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
            Some(EXITING) => {
                self.mark_exiting(id);
                if resume {
                    self.resume_with(id, envelope).await
                } else {
                    Err(RelayError::Rejected(EXITED.to_owned()))
                }
            }
            Some(reason) => Err(RelayError::Rejected(reason.to_owned())),
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

    /// Whether an input to the agent is in hand: handed to its process and
    /// awaiting the verdict, or waiting its turn behind one.
    pub fn in_hand(&self, id: AgentId) -> bool {
        self.lane(id).try_lock().is_err()
    }
}
