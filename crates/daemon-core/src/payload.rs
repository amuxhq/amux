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
use crate::families::{Create, Refusal, Resume};
use crate::ids::{AgentId, HostId};

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
    /// A batch of analytics events to upload.
    AnalyticsBatch;
}

/// A host found on the local network: a hint where to dial it, which the
/// driver keeps. Whether the host is paired rides along until the trust
/// store moves into the core.
/// What a push carries: envelope fields only, so producing it interprets
/// nothing.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Push {
    pub host_id: Vec<u8>,
    pub agent_id: Vec<u8>,
    /// The revision of the snapshot that turned the agent to needs_you.
    pub revision: u64,
    pub name: Option<String>,
    pub working_on: Option<String>,
    /// The agent's newest item's text when it turned.
    pub text: String,
}

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
/// An inventory opening still reads the host set the profile's shell
/// publishes. Until that moves into the core the driver reads it as it
/// hands the call over, and it rides on the request.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ClientRequest {
    /// Opens a stream of an agent's session events: the opening cut, then
    /// every change.
    Subscribe {
        agent_id: Vec<u8>,
        /// How many of the newest rows it opens with.
        tail: u32,
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
    /// Creates one of this host's agents and starts its process; answered
    /// with its row once the process said Hello, or ended.
    CreateAgent(Box<Create>),
    /// Starts an exited agent's next incarnation; answered as a create is.
    ResumeAgent(Box<Resume>),
    /// Stops the agent's process; answered with its row once it ended.
    StopAgent {
        agent: AgentId,
    },
    /// Kills the agent's process at once, as the stop deadline does.
    KillAgent {
        agent: AgentId,
    },
    /// Stops the agent and removes its rows and its directory; answered
    /// with nothing.
    DeleteAgent {
        agent: AgentId,
    },
    /// Renames the agent; answered with its row.
    RenameAgent {
        agent: AgentId,
        name: String,
    },
    /// A client's upload into one of this host's agents, with the hash of
    /// its bytes.
    PutBlob {
        #[serde(with = "codec::message")]
        request: wire::PutBlobRequest,
        hash: Vec<u8>,
    },
    /// A replica's blob its origin sent, kept for the next reader.
    KeepBlob {
        agent_id: Vec<u8>,
        hash: Vec<u8>,
        bytes: Vec<u8>,
    },
    GetBlob(#[serde(with = "codec::message")] wire::GetBlobRequest),
    /// How one of this host's agents' working tree differs from a base.
    Diff(#[serde(with = "codec::message")] wire::DiffRequest),
    /// Keep this machine's client's unsent draft of the agent.
    SaveDraft {
        agent_id: Vec<u8>,
        #[serde(with = "codec::message")]
        draft: wire::ComposerDraft,
    },
    GetDraft {
        agent_id: Vec<u8>,
    },
    /// A message for one of this host's agents, answered once the
    /// recipient accepted it. Its envelope id is set.
    SendMessage {
        #[serde(with = "codec::message")]
        envelope: Box<wire::Envelope>,
        from: MessageFrom,
    },
    /// A change of the trust store, answered once it is on the drive.
    ChangeTrust(crate::trust::TrustChange),
    /// Opens the pairing window for `ttl_ms`.
    StartPairing {
        method: crate::pairing::WindowMethod,
        ttl_ms: u64,
    },
    /// Closes the pairing window.
    CancelPairing,
    /// Finishes the pending pairing a person confirmed.
    ConfirmPair {
        token: Vec<u8>,
    },
    /// Abandons the pending pairing a person turned down.
    AbandonPair {
        token: Vec<u8>,
    },
    /// Trusts a peer whose identity an SSH exchange carried; `reachability`
    /// is how this host reaches it, when this host dialled.
    PairOverSsh {
        #[serde(with = "codec::message")]
        local: wire::PairingIdentity,
        #[serde(with = "codec::message")]
        peer: wire::PairingIdentity,
        reachability: Option<crate::trust::Reachability>,
    },
}

/// Who sent a message, as the call it came on says.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum MessageFrom {
    /// A person, on the profile socket.
    Person,
    /// One of this host's agents, on its tools socket.
    Agent(AgentId),
    /// A paired host's agent, as that host stamped it.
    Peer(#[serde(with = "codec::message")] wire::AgentSender),
    /// The person, through a paired host.
    PeerPerson,
}

/// Why a message was not accepted.
#[derive(Clone, Debug, PartialEq)]
pub enum MessageRefusal {
    /// No agent with that id.
    NoAgent,
    /// The recipient lives on another host.
    OtherHost,
    /// The recipient's process was not connected in time.
    Unavailable,
    /// The recipient's answer was lost; the message may or may not have
    /// arrived.
    Lost,
    /// The recipient refused it.
    Rejected(wire::Rejected),
    /// The recipient's incarnation the message was for has ended.
    Stale,
    /// Resuming the recipient with it failed.
    Resume(Refusal),
    /// The store failed, in its own words.
    Store(String),
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

/// How a call this host made of a peer came back: its whole reply, one
/// event of the stream it opened, that stream's end, or why there is no
/// more. A unary call has one answer; a stream has an answer per event and
/// then [`Answer::Ended`] or [`Answer::Failed`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Answer {
    Reply(Box<PeerReply>),
    Session(#[serde(with = "codec::message")] wire::SessionEvent),
    Inventory(#[serde(with = "codec::message")] wire::InventoryEvent),
    Ended,
    Failed(PeerFailure),
}

impl Answer {
    /// Whether nothing more comes back of the call after this.
    pub fn is_last(&self) -> bool {
        !matches!(self, Answer::Session(_) | Answer::Inventory(_))
    }
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

/// A diff's patch, as a blob to write.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Patch {
    pub hash: Vec<u8>,
    pub bytes: Vec<u8>,
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
    /// One of this host's agents' rows, as a registry call left it.
    Agent(Box<wire::Agent>),
    Draft(wire::ComposerDraft),
    /// What a trust change did, once it is on the drive.
    Trust(crate::trust::TrustChanged),
    /// The pairing window is open, with this secret.
    PairingWindow(crate::pairing::WindowSecret),
    /// The pairing window is closed; whether one was open.
    PairingCancelled {
        was_active: bool,
    },
    /// The pairing is committed: `host` is trusted, and reached as
    /// `reachability` when this side dialled it.
    Paired {
        host: HostId,
        reachability: Option<crate::trust::Reachability>,
    },
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
    /// The registry refused the call or could not finish it.
    Registry(Refusal),
    /// The agent holds no blob with this hash, in hex.
    NoBlob(String),
    /// A blob could not be written or read.
    Blob(BlobRefusal),
    /// A message was not accepted.
    Message(MessageRefusal),
    /// A trust change was refused or could not be saved.
    Trust(crate::trust::TrustError),
    /// The pairing window could not open.
    Window(crate::pairing::WindowRefusal),
    /// A pairing did not finish.
    Pairing(crate::pairing::PairFailure),
}

/// Why a blob call failed.
#[derive(Clone, Debug, PartialEq)]
pub enum BlobRefusal {
    /// A blob is written only into an agent this host runs.
    NotOwn,
    /// A blob hash is 32 bytes, not this many.
    BadHash(usize),
    /// The blob is over the attachment limit.
    TooLarge { name: String, size: u64 },
    /// Writing it failed; `full` when the drive is.
    Write { full: bool, words: String },
    /// Reading it failed.
    Read(String),
    /// A diff names no base.
    NoBase,
    /// Git could not compare the working tree.
    Git(GitRefusal),
}

/// Why git could not compare a working tree.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum GitRefusal {
    NotARepository,
    /// The branch shares no history with this base.
    NoForkPoint(String),
    /// Anything else, in git's words.
    Failed(String),
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
    /// The worktree a create asked for is made at `path`, branched from
    /// `base_branch`.
    Worktree { path: String, base_branch: String },
    /// It was refused, as the driver said it to the caller.
    Refused(#[serde(with = "codec::message")] wire::Error),
    /// A working tree compared: the diff without its patch, and the patch
    /// with its hash when one was asked for and there is one.
    Compared {
        #[serde(with = "codec::message")]
        diff: wire::Diff,
        patch: Option<Patch>,
    },
    /// Git could not compare the working tree.
    CompareFailed(GitRefusal),
    /// The next spec a resume starts from; none when the agent has none.
    Spec(#[serde(with = "codec::optional")] Option<Box<wire::AgentSpec>>),
}
