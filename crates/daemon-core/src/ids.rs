//! The names the core gives the things the driver holds for it.
//!
//! The core cannot hold a socket, a response stream, an armed timer or a
//! running job, so it names each and the driver keeps the name beside the
//! thing. Every name is minted by the core and is a plain value, so it
//! survives a checkpoint. The one exception is a call that arrives: the
//! driver holds its stream before the core hears of it, so the driver names
//! it, from a counter of its own.

use serde::{Deserialize, Serialize};

/// A call the driver holds open: a client's or peer's request awaiting its
/// answer, or a request the core made to a peer awaiting the reply.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CallId(pub u64);

/// A timer the core asked the driver to arm. Each names what firing it
/// does, so arming one already armed moves it rather than adding another.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum TimerId {
    /// The agent's journal holds more than one batch: read the next.
    Ingest(AgentId),
    /// A drain pass kept rows: try them again.
    Deliveries,
    /// The message under way in the agent's lane has waited its patience.
    Lane(AgentId),
    /// A notification falls due, or a push that failed is tried again.
    Notifications,
    /// Something was committed since the last checkpoint: take the next.
    Checkpoint,
    /// Refresh the credential the relay link presents.
    Refresh(LinkId),
    /// Follow the host's inventory: now, or once a lost stream's backoff
    /// is waited out.
    Follow(HostId),
    /// Open the replica agent's source again once its backoff is waited
    /// out.
    Source(HostId, AgentId),
    /// The agent's new process has had its start deadline to say Hello.
    Start(AgentId),
    /// What the agent's process was given to end in is up: its stop
    /// deadline, or a kill's patience.
    Exit(AgentId),
    /// The pairing window expires, or its lockout lifts.
    PairWindow,
    /// A pairing conversation's deadline, or a pending pairing's token
    /// expiry.
    Pairing(PairingId),
    /// The attempt out to the host has had its time.
    Attempt(HostId),
    /// The host's pause before its next attempt is over.
    Retry(HostId),
}

impl TimerId {
    /// Whether the deadline is in link time rather than policy time; see
    /// [`Input::Tick`](crate::Input::Tick).
    pub fn on_link_clock(&self) -> bool {
        matches!(self, TimerId::Attempt(_) | TimerId::Retry(_))
    }
}

/// A job the driver runs off the core thread; its outcome returns as
/// [`Input::JobDone`](crate::Input::JobDone) under this id.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct JobId(pub u64);

/// A host, by the id its identity key was minted with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct HostId(pub [u8; 16]);

impl HostId {
    /// The host an id's bytes name, when they are an id.
    pub fn from_slice(bytes: &[u8]) -> Option<HostId> {
        bytes.try_into().ok().map(HostId)
    }
}

/// One link to a peer: the driver's connection, which the driver names as
/// it reports it up, since it holds the connection first. Two links to one
/// peer have different ids.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct LinkId(pub u64);

/// A channel the driver opened over a link at the core's request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ChannelId(pub u64);

/// An agent, by its id: for one of this host's own, its directory's.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AgentId(pub [u8; 16]);

impl AgentId {
    /// The agent an id's bytes name, when they are an id.
    pub fn from_slice(bytes: &[u8]) -> Option<AgentId> {
        bytes.try_into().ok().map(AgentId)
    }
}

/// One attempt to reach a host, from its dial until it ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AttemptId(pub u64);

/// One pairing conversation, from its first message to its end.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PairingId(pub u64);

/// An account this installation is signed in to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AccountId(pub [u8; 16]);
