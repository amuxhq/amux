//! What the core's inputs and outputs carry beyond names.
//!
//! Every payload is decoded: the driver turns bytes into these values on the
//! way in and these values into bytes on the way out, so the core never
//! sees a stream. Where the protocol already defines the message, the
//! payload is the generated message type. The rest are the core's own, and
//! a payload no machine in the core reads carries nothing until one does.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::codec;

/// A link control frame: the hello, its acknowledgement, a neighbor coming
/// up or going down.
pub type LinkFrame = wire::Message;
/// Why a link closed, as the link protocol states it.
pub type LinkCloseReason = wire::LinkCloseReason;
/// One message of a pairing conversation.
pub type PairMessage = wire::PairMessage;
/// An agent process's hello on its control socket.
pub type Hello = wire::AgentHello;
/// A frame to an agent process's control socket.
pub type ControlFrame = wire::CtlFrame;
/// What an agent is started with.
pub type AgentSpec = wire::AgentSpec;
/// The preface that opens a stream over a link, naming where it goes.
pub type StreamPreface = wire::StreamPreface;
/// Why a stream was refused before any byte of it was carried.
pub type StreamRefusal = wire::StreamRefusal;

macro_rules! payload {
    ($($(#[$doc:meta])* $name:ident;)*) => {
        $(
            $(#[$doc])*
            #[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
            pub struct $name;
        )*
    };
}

payload! {
    /// How a link reaches its peer: directly, over SSH or through a relay.
    CarrierKind;
    /// What a channel carries: calls, a session or bulk transfer.
    ChannelClass;
    /// Why a channel closed or could not open.
    ChannelError;
    /// A peer's decoded request on a channel.
    PeerRequest;
    /// A peer's decoded reply, or one item of its streamed reply, to a call
    /// the core made.
    Answer;
    /// A host found on the local network.
    Advertisement;
    /// The key a channel's handshake must find at the other end.
    PublicKey;
    /// A blob's content hash.
    BlobHash;
    /// A push notification to send.
    Push;
    /// A batch of analytics events to upload.
    AnalyticsBatch;
}

/// Who made a client call.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Caller {
    /// A client of this profile: its socket, an agent's tools socket or
    /// the in-process call.
    Local,
    /// A paired host, answered only about this host's own agents.
    Peer,
}

/// Where a Subscribe stream starts.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Opening {
    /// A client's: the newest `tail` rows.
    Tail(u32),
    /// A peer source's: what came after the revision it holds, taken
    /// under `generation` of this origin, when there are at most `cap`.
    After {
        revision: u64,
        cap: u32,
        generation: u64,
    },
}

/// A client's decoded request.
///
/// Some answers still read what the profile's shell owns: which replicas a
/// source follows, the host set it publishes and the replica bound it runs
/// under. Until those move into the core the driver reads them as it hands
/// the call over, and they ride on the request.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ClientRequest {
    /// Opens a stream of an agent's session events: the opening cut, then
    /// every change.
    Subscribe {
        agent_id: Vec<u8>,
        from: Opening,
        /// The newest rows a replica keeps, K: a client's tail never asks
        /// for more.
        tail_rows: u32,
        /// Whether a source follows the agent, when it is a replica: one
        /// nobody follows is served as detached.
        followed: bool,
    },
    /// A page of older items.
    Fetch(#[serde(with = "codec::message")] wire::FetchRequest),
    /// One item in full.
    Get(#[serde(with = "codec::message")] wire::GetRequest),
    /// Opens a stream of the fleet: hosts and agent rows, CaughtUp, then
    /// every change.
    SubscribeInventory {
        /// The host set as last published.
        #[serde(with = "codec::messages")]
        hosts: Vec<wire::HostEntry>,
        /// The one host whose entry and agents the stream carries, for a
        /// paired host's stream; every host's for a client's.
        only: Option<Vec<u8>>,
    },
}

/// One reply, or one item of a streamed reply, to a call. Stream items are
/// shared, since the same event goes to every subscriber.
#[derive(Clone, Debug, PartialEq)]
pub enum Response {
    Session(Arc<wire::SessionEvent>),
    Inventory(Arc<wire::InventoryEvent>),
    Fetched(wire::FetchResponse),
    Item(wire::Item),
}

/// How a call ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// Answered in full; a stream ends here.
    Ok,
    Failed(CallError),
}

/// Why a call was refused or could not be answered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CallError {
    /// No agent with that id.
    NoAgent,
    /// The agent holds no item with this key.
    NoItem(String),
    /// Older history is held by the agent's host, which cannot be reached.
    OriginUnreachable,
    /// The store failed, in its own words.
    Store(String),
}

impl From<store::StoreError> for CallError {
    fn from(error: store::StoreError) -> Self {
        CallError::Store(error.to_string())
    }
}

/// What a job produced.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum JobResult {
    /// It did what it was asked.
    Done,
    /// It could not, in the driver's words.
    Failed(String),
    /// The origin's page below a replica's block, or `None` when the origin
    /// could not be asked.
    Fetched(#[serde(with = "codec::optional")] Option<wire::FetchResponse>),
}
