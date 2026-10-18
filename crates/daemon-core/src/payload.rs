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
use crate::ids::HostId;

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
    /// A blob's content hash.
    BlobHash;
    /// A push notification to send.
    Push;
    /// A batch of analytics events to upload.
    AnalyticsBatch;
}

/// A host found on the local network: a hint where to dial it, which the
/// driver keeps. Whether the host is paired rides along until the trust
/// store moves into the core.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Advertisement {
    pub host: HostId,
    pub trusted: bool,
}

/// How a link reaches its peer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CarrierKind {
    /// The peer's own connection, over the network.
    Direct,
    /// The peer's own connection, carried over SSH.
    Ssh,
    /// A connection to a relay, which reaches the hosts it claims.
    Relay,
}

/// What a channel carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ChannelClass {
    /// Calls to the peer.
    Calls,
    /// Session subscriptions, apart from the calls so that neither waits
    /// behind the other.
    Session,
    /// One bulk transfer.
    Bulk,
}

/// Why a channel closed or could not open.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ChannelError {
    /// The link it was asked on is not up.
    NoLink,
    /// No key is pinned for the peer: it is not paired, or its pairing is
    /// being replaced.
    Unpinned,
    /// The link went before the channel opened.
    LinkClosed,
    /// The far end refused the stream before carrying any of it.
    Refused(#[serde(with = "codec::numbered")] StreamRefusal),
    /// The handshake failed or took too long, in the driver's words.
    Handshake(String),
    /// The connection under an open channel ended.
    Ended,
}

/// The key a channel's handshake must find at the other end: the peer's
/// identity key as it was pinned at pairing.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PublicKey(pub Vec<u8>);

/// Where a Subscribe stream starts.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) enum Opening {
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
/// source follows and the host set it publishes. Until those move into the core the driver reads them as it hands
/// the call over, and they ride on the request.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ClientRequest {
    /// Opens a stream of an agent's session events: the opening cut, then
    /// every change.
    Subscribe {
        agent_id: Vec<u8>,
        /// How many of the newest rows it opens with.
        tail: u32,
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
    },
    /// A call on an agent of another host, made on that host's daemon and
    /// answered with what it answered.
    Forward {
        host: HostId,
        request: Box<PeerRequest>,
    },
}

/// A request on a peer's PeerService, decoded: one a paired host makes of
/// this one, or one this host makes of a paired host.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum PeerRequest {
    /// A stream of one of this host's agents' session events: from what
    /// came after the revision the peer's replica holds, or a tail.
    Subscribe(#[serde(with = "codec::message")] wire::PeerSubscribeRequest),
    /// A stream of this host's entry and its own agents.
    SubscribeInventory {
        /// The host set as last published, which holds this host's entry.
        #[serde(with = "codec::messages")]
        hosts: Vec<wire::HostEntry>,
    },
    Fetch(#[serde(with = "codec::message")] wire::FetchRequest),
    Get(#[serde(with = "codec::message")] wire::GetRequest),
    SendInput(#[serde(with = "codec::message")] wire::SendInputRequest),
    CreateAgent(#[serde(with = "codec::message")] wire::CreateAgentRequest),
    RenameAgent(#[serde(with = "codec::message")] wire::RenameAgentRequest),
    StopAgent(#[serde(with = "codec::message")] wire::StopAgentRequest),
    ResumeAgent(#[serde(with = "codec::message")] wire::ResumeAgentRequest),
    DeleteAgent(#[serde(with = "codec::message")] wire::DeleteAgentRequest),
    SendMessage(#[serde(with = "codec::message")] wire::Envelope),
    PutBlob(#[serde(with = "codec::message")] wire::PutBlobRequest),
    GetBlob(#[serde(with = "codec::message")] wire::GetBlobRequest),
    Diff(#[serde(with = "codec::message")] wire::DiffRequest),
    GetCatalogue(#[serde(with = "codec::message")] wire::GetCatalogueRequest),
    ListRepositories(#[serde(with = "codec::message")] wire::ListRepositoriesRequest),
    Dump(#[serde(with = "codec::message")] wire::DumpRequest),
}

/// A peer's whole reply to one of its unary calls.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum PeerReply {
    Fetched(#[serde(with = "codec::message")] wire::FetchResponse),
    Item(#[serde(with = "codec::message")] wire::Item),
    SendInput(#[serde(with = "codec::message")] wire::SendInputResponse),
    /// CreateAgent, RenameAgent and ResumeAgent answer with the agent.
    Agent(#[serde(with = "codec::message")] wire::Agent),
    /// StopAgent and DeleteAgent answer with nothing.
    Done,
    SendMessage(#[serde(with = "codec::message")] wire::SendMessageResponse),
    Blob(#[serde(with = "codec::message")] wire::BlobRef),
    GetBlob(#[serde(with = "codec::message")] wire::GetBlobResponse),
    Diff(#[serde(with = "codec::message")] wire::Diff),
    Catalogue(#[serde(with = "codec::message")] wire::CatalogueBlob),
    Repositories(#[serde(with = "codec::message")] wire::ListRepositoriesResponse),
    Dump(#[serde(with = "codec::message")] wire::DumpPart),
}

/// How a call this host made of a peer came back.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Answer {
    Reply(Box<PeerReply>),
    Failed(PeerFailure),
}

/// Why a call made of a peer has no reply. Whether it went out is what a
/// caller decides on: one that never left may be tried again or held, one
/// that left may have been acted on.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum PeerFailure {
    /// It never left: no link to the peer, the peer is not trusted, or no
    /// channel to it opened in time.
    NotSent,
    /// It never left: the relay carries no calls between hosts on the
    /// account's lapsed plan.
    Lapsed,
    /// It went out and no answer came back in time, or the link failed
    /// under it.
    Uncertain,
    /// The peer refused it, in its own words.
    Refused(#[serde(with = "codec::message")] wire::Error),
}

/// One reply, or one item of a streamed reply, to a call. Stream items are
/// shared, since the same event goes to every subscriber.
#[derive(Clone, Debug, PartialEq)]
pub enum Response {
    Session(Arc<wire::SessionEvent>),
    Inventory(Arc<wire::InventoryEvent>),
    Fetched(wire::FetchResponse),
    Item(wire::Item),
    /// A forwarded call's reply, from the host it went to.
    Peer(Box<PeerReply>),
}

/// How a call ended.
#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    /// Answered in full; a stream ends here.
    Ok,
    Failed(CallError),
}

/// Why a call was refused or could not be answered.
#[derive(Clone, Debug, PartialEq)]
pub enum CallError {
    /// No agent with that id.
    NoAgent,
    /// The agent holds no item with this key.
    NoItem(String),
    /// Older history is held by the agent's host, which cannot be reached.
    OriginUnreachable,
    /// The store failed, in its own words.
    Store(String),
    /// A forwarded call got no reply from the host it went to.
    Peer(PeerFailure),
    /// A paired host asked for what this host answers only to its own
    /// clients.
    NotServed,
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
