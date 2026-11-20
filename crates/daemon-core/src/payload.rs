//! What the core's inputs and outputs carry beyond names.
//!
//! Every payload is decoded: the driver turns bytes into these values on the
//! way in and these values into bytes on the way out, so the core never
//! sees a stream. Where the protocol already defines the message, the
//! payload is the generated message type. The rest are the core's own.

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
/// A frame to an agent process's control socket.
pub type ControlFrame = wire::CtlFrame;
/// What an agent is started with.
pub type AgentSpec = wire::AgentSpec;
/// Why a forwarder will not carry a link's packets to a destination.
pub type ForwardRefusal = wire::ForwardRefusal;

/// What a push carries: the notice as the outbox holds it, so producing it
/// interprets nothing, and who gets it.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Push {
    pub host_id: Vec<u8>,
    pub agent_id: Vec<u8>,
    /// The revision that raised it.
    pub revision: u64,
    pub name: String,
    /// An encoded `amux.v1.Notice`.
    pub notice: Vec<u8>,
    pub recipients: Vec<PushRecipient>,
}

/// A paired host that wants notifications.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PushRecipient {
    pub host_id: Vec<u8>,
    /// The public key a push to it is sealed to.
    pub push_key: Vec<u8>,
}

/// What a host's dial error says of one that refused this host's
/// certificate.
pub const TRUST_REVOKED: &str = "the host no longer trusts this machine";

/// How a link reaches its peer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CarrierKind {
    /// The peer's own connection, over the network.
    Direct,
    /// The peer's own connection, carried over SSH.
    Ssh,
    /// A connection to a relay, which reaches the hosts it claims.
    Relay,
    /// The peer's own connection, its packets forwarded by the relay or by
    /// a paired daemon.
    Forwarded,
}

/// Where a candidate address for a peer came from, which decides whether a
/// free account may probe it: a candidate learned only through the relay's
/// observation is paid.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// Local discovery found the peer at it.
    Discovery,
    /// The peer listed it in its `Hello` or `Addresses` over a direct link.
    Listed,
    /// It carried an earlier link to the peer.
    LastWorked,
    /// The relay saw the peer's packets come from it: the public address
    /// the peer passed on as the relay told it.
    Observed,
    /// The peer listed it over a forwarded link.
    Forwarding,
}

/// An address a peer can be reached at, and where it came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PeerAddress {
    pub addr: std::net::SocketAddr,
    pub source: Source,
}

/// Why the core would not open a channel.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ChannelError {
    /// The link it was asked on is not up.
    NoLink,
    /// No key is pinned for the peer: it is not paired.
    Unpinned,
}

/// A peer's identity key, as a resumed link's ticket names it.
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
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ClientRequest {
    /// Opens a stream of an agent's session events: the opening cut, then
    /// every change.
    SubscribeSession {
        agent_id: Vec<u8>,
        /// How many of the newest rows it opens with.
        tail: u32,
    },
    /// A page of older items.
    ListItems(#[serde(with = "codec::message")] wire::ListItemsRequest),
    /// One item in full.
    GetItem(#[serde(with = "codec::message")] wire::GetItemRequest),
    /// Opens a stream of the fleet: hosts and agent rows, CaughtUp, then
    /// every change.
    SubscribeInventory,
    /// Creates one of this host's agents and starts its process; answered
    /// with its row once the process said Hello, or ended. With the
    /// workspaces experiment, one in a new workspace is answered as soon as
    /// its row is written, its workspace still being made.
    CreateAgent(Box<Create>),
    /// Starts an exited agent's next incarnation; answered as a create is.
    ResumeAgent(Box<Resume>),
    /// Files the agent as the person asked; answered with its row. A
    /// settle of a live agent stops its process first and answers once it
    /// ended.
    SetFiling {
        agent: AgentId,
        filing: crate::Filing,
    },
    /// Kills the agent's process at once, as the stop deadline does.
    KillAgent {
        agent: AgentId,
    },
    /// Stops the agent and removes its rows and its directory; answered
    /// with nothing. `discard`: the person agreed to lose the uncommitted
    /// changes of a workspace the delete removes.
    DeleteAgent {
        agent: AgentId,
        #[serde(default)]
        discard: bool,
    },
    /// Renames the agent; answered with its row.
    RenameAgent {
        agent: AgentId,
        name: String,
    },
    /// A client's upload into one of this host's agents, with the hash of
    /// its bytes. The bytes travel apart from the request, which carries
    /// none, so the record keeps their length and not a copy.
    PutBlob {
        #[serde(with = "codec::message")]
        request: wire::PutBlobRequest,
        #[serde(with = "codec::hash")]
        hash: Vec<u8>,
        #[serde(with = "codec::sized")]
        bytes: Vec<u8>,
    },
    /// A replica's blob its origin sent, kept for the next reader.
    KeepBlob {
        agent_id: Vec<u8>,
        #[serde(with = "codec::hash")]
        hash: Vec<u8>,
        #[serde(with = "codec::sized")]
        bytes: Vec<u8>,
    },
    GetBlob(#[serde(with = "codec::message")] wire::GetBlobRequest),
    /// How one of this host's agents' working tree differs from a base.
    GetDiff(#[serde(with = "codec::message")] wire::GetDiffRequest),
    /// Keep this machine's client's unsent draft of the agent.
    UpdateDraft {
        agent_id: Vec<u8>,
        #[serde(with = "codec::message")]
        draft: wire::Draft,
    },
    GetDraft {
        agent_id: Vec<u8>,
    },
    /// A message for one of this host's agents, answered once the
    /// recipient accepted it. Its envelope id is set, and its sender is the
    /// caller as the host that took the call built it: a person, one of
    /// this host's agents, or a paired host's agent, a child's report
    /// included, which comes from the link's peer.
    SendMessage {
        #[serde(with = "codec::boxed")]
        envelope: Box<wire::Envelope>,
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

/// What a daemon asks a paired daemon for itself, on its PeerService,
/// decoded: one a paired host makes of this one, or one this host makes of
/// a paired host. A call a client makes of another host's services is
/// carried there unread and never passes through the core.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum PeerRequest {
    /// PeerService.FollowAgents: a stream of this host's entry and its own
    /// agents, then every change.
    FollowAgents,
    /// PeerService.FollowSession: a stream of one of this host's agents'
    /// session events, from what came after the revision the peer's copy
    /// holds, or a tail.
    FollowSession(#[serde(with = "codec::message")] wire::FollowSessionRequest),
    /// PeerService.DeliverReport: a child's report to its parent on the
    /// called host. The host it reaches hands it on as a message from the
    /// child, as [`report_envelope`] spells it.
    DeliverReport(#[serde(with = "codec::message")] wire::DeliverReportRequest),
    /// PeerService.ListItems: a page of one of this host's agents' older
    /// items, whose request is this one field for field.
    ListItems(#[serde(with = "codec::message")] wire::ListItemsRequest),
}

/// A child's report as the parent's host hands it on to the parent: a
/// message from the child, one of the agents of `from`, the host whose
/// daemon delivered it, to the parent, one of `here`'s. The sender is the
/// calling host's, whoever its link authenticated, so a daemon reports only
/// for its own agents.
pub fn report_envelope(
    from: &[u8],
    here: &[u8],
    report: wire::DeliverReportRequest,
) -> wire::Envelope {
    wire::Envelope {
        id: report.id,
        context: None,
        from: Some(wire::Sender {
            value: Some(wire::sender::Value::Agent(wire::AgentSender {
                agent: Some(wire::AgentRef {
                    host_id: from.to_vec(),
                    agent_id: report.child_agent_id,
                }),
                name: report.child_name,
            })),
        }),
        to: Some(wire::AgentRef {
            host_id: here.to_vec(),
            agent_id: report.parent_agent_id,
        }),
        body: report.report.map(wire::envelope::Body::Report),
    }
}

/// A peer's whole reply to one of its unary calls.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum PeerReply {
    /// A page, for ListItems.
    Fetched(#[serde(with = "codec::message")] wire::ListItemsResponse),
    /// DeliverReport answers with nothing.
    Done,
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
    #[serde(with = "codec::hash")]
    pub hash: Vec<u8>,
    #[serde(with = "codec::content")]
    pub bytes: Vec<u8>,
}

/// One reply, or one item of a streamed reply, to a call. Stream items are
/// shared, since the same event goes to every subscriber.
#[derive(Clone, Debug, PartialEq)]
pub enum Response {
    Session(Arc<wire::SessionEvent>),
    Inventory(Arc<wire::InventoryEvent>),
    Fetched(wire::ListItemsResponse),
    Item(wire::Item),
    /// A working tree compared, for GetDiff.
    Diff(Box<wire::Diff>),
    /// A blob written, for PutBlob.
    Written(wire::BlobRef),
    /// A blob's bytes, for GetBlob: the shell reads them off the drive.
    Bytes(Vec<u8>),
    /// One of this host's agents' rows, as a registry call left it.
    Agent(Box<wire::Agent>),
    Draft(wire::Draft),
    /// What a trust change did, once it is on the drive.
    Trust(crate::trust::TrustChanged),
    /// The pairing window is open, with this secret.
    PairingWindow(crate::pairing::WindowSecret),
    /// The pairing window is closed; whether one was open.
    PairingCancelled {
        was_active: bool,
    },
    /// The pairing is committed: `host` is trusted, and the core dials it
    /// on the paths the pairing left it.
    Paired {
        host: HostId,
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
    Spec(#[serde(with = "codec::optional_boxed")] Option<Box<wire::AgentSpec>>),
    /// What a workspace job came to.
    Workspace(crate::workspaces::WorkspaceDone),
}
