//! The daemon's inputs, outputs and jobs.

use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;

use store::AgentKey;
use wire::{InventoryEvent, SessionEvent};

use crate::DumpReason;
use crate::ids::{
    AccountId, AgentId, CallId, ChannelId, HostId, JobId, LinkId, PairingId, TimerId,
};
use crate::payload::{
    Advertisement, AgentSpec, AnalyticsBatch, Answer, BlobHash, Caller, CarrierKind, ChannelClass,
    ChannelError, ClientRequest, ControlFrame, Hello, JobResult, LinkCloseReason, LinkFrame,
    PairMessage, PeerRequest, PublicKey, Push, Response, Status,
};

/// Everything that can happen to the daemon, decoded by the driver.
#[derive(Clone, Debug, PartialEq)]
pub enum Input {
    /// The clock, as an input: the core reads time from nothing else.
    Tick {
        now_ms: u64,
    },
    /// A timer the core armed has fired.
    Timer {
        id: TimerId,
    },
    LinkUp {
        link: LinkId,
        peer: HostId,
        kind: CarrierKind,
    },
    LinkDown {
        link: LinkId,
        reason: LinkCloseReason,
    },
    LinkControl {
        link: LinkId,
        frame: LinkFrame,
    },
    /// A channel the core asked for is open, after the driver's handshake
    /// verified the pin the core gave it.
    ChannelOpened {
        channel: ChannelId,
        link: LinkId,
        peer: HostId,
        class: ChannelClass,
    },
    ChannelClosed {
        channel: ChannelId,
        reason: ChannelError,
    },
    PeerCall {
        channel: ChannelId,
        call: CallId,
        request: PeerRequest,
    },
    PeerAnswer {
        call: CallId,
        answer: Answer,
    },
    PairingMessage {
        session: PairingId,
        message: PairMessage,
    },
    Discovered {
        found: Vec<Advertisement>,
    },
    AgentHello {
        agent: AgentId,
        hello: Hello,
    },
    /// An agent's journal grew.
    Nudge {
        agent: AgentId,
    },
    ControlClosed {
        agent: AgentId,
    },
    ClientCall {
        call: CallId,
        who: Caller,
        request: ClientRequest,
    },
    /// A client or peer dropped a call or its stream.
    CallCancelled {
        call: CallId,
    },
    /// The driver's bounded queue for a streamed answer overflowed.
    AnswerLagged {
        call: CallId,
    },
    JobDone {
        job: JobId,
        result: JobResult,
    },
    /// Randomness, as an input: every id the core mints comes from here.
    Entropy([u8; 32]),
    /// The settings the profile runs under now.
    Configure(Settings),
    /// What the profile's shell changed outside a step, for the core's
    /// subscribers. Each kind goes as the write behind it moves into the
    /// core.
    FromShell(FromShell),
}

/// The profile's settings the core reads, as last configured.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Settings {
    /// How long a turn's notification waits before it is sent.
    pub notify_delay_ms: i64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            notify_delay_ms: 30_000,
        }
    }
}

/// A change the profile's shell committed under the store it lends the
/// core, handed to the core while the store is still as that commit left
/// it, so subscribers read it in commit order and an opening never reads
/// it twice or misses it.
#[derive(Clone, Debug, PartialEq)]
pub enum FromShell {
    /// One of an agent's session events.
    Session {
        agent: AgentKey,
        event: Arc<SessionEvent>,
    },
    /// An agent's row as the inventory carries it.
    Row {
        agent: AgentKey,
        event: Arc<InventoryEvent>,
    },
    /// The host set changed.
    Hosts(Arc<InventoryEvent>),
    /// The agent is gone: its streams end and the inventory says why.
    Removed {
        agent: AgentKey,
        event: Arc<InventoryEvent>,
    },
    /// Nothing about the agent, its session or its row, reaches a
    /// subscriber until [`FromShell::Unmute`]; its streams stay open. A
    /// delete mutes the agent it stops, so no subscriber reads the stop as
    /// an exit it could resume from before the removal arrives.
    Mute(AgentKey),
    Unmute(AgentKey),
}

/// Everything the daemon wants done. The driver encodes, sends, arms,
/// spawns and writes; it decides nothing.
#[derive(Clone, Debug, PartialEq)]
pub enum Output {
    ArmTimer {
        id: TimerId,
        at_ms: u64,
    },
    CancelTimer {
        id: TimerId,
    },
    OpenChannel {
        link: LinkId,
        peer: HostId,
        class: ChannelClass,
        pin: PublicKey,
    },
    CloseChannel {
        channel: ChannelId,
    },
    Call {
        channel: ChannelId,
        call: CallId,
        request: PeerRequest,
    },
    Answer {
        call: CallId,
        response: Response,
    },
    EndCall {
        call: CallId,
        status: Status,
    },
    /// Whether any stream follows `agent` now: the shell keeps a replica
    /// current while one does. Said when it changes.
    Watching {
        agent: AgentKey,
        watched: bool,
    },
    /// What one ingest step did: for the shell's counters, and for whoever
    /// waits for the journal to be read to its end.
    Ingested {
        agent: AgentId,
        report: Ingested,
    },
    /// Wake the shell's own loop for `Wake`, until it moves into the core.
    Wake(Wake),
    /// Blob bytes go from the file to the call's stream without passing
    /// through the core.
    SendFile {
        call: CallId,
        path: PathBuf,
        range: Range<u64>,
    },
    LinkControl {
        link: LinkId,
        frame: LinkFrame,
    },
    DialAgent {
        agent: AgentId,
    },
    WriteControl {
        agent: AgentId,
        frame: ControlFrame,
    },
    SpawnAgent {
        dir: PathBuf,
        spec: Box<AgentSpec>,
    },
    Job {
        id: JobId,
        op: Job,
    },
    /// Take a dump: the core saw something it must not carry on from.
    Dump {
        reason: DumpReason,
    },
}

/// One ingest step's outcome.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ingested {
    /// Journal frames committed.
    pub frames: u32,
    /// Of those, the turns they ended.
    pub turn_ends: u32,
    /// The agent's kind, as the wire numbers it.
    pub kind: i32,
    pub end: IngestEnd,
}

/// Where an ingest step left the journal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IngestEnd {
    /// More frames wait; the next batch is read when its timer fires.
    More,
    /// Everything whole has been read.
    Read,
    /// Nothing was committed, in the step's words.
    Failed(String),
}

/// A loop the shell still runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Wake {
    /// The outbox drain: a turn ended, so a delivery may be due.
    Deliveries,
    /// The notification sender: an envelope changed.
    Notifications,
}

/// A result-only mechanism the driver runs off the core thread; its outcome
/// returns as [`Input::JobDone`].
#[derive(Clone, Debug, PartialEq)]
pub enum Job {
    Fsync(PathBuf),
    /// Write the serialized core beside the store's passive WAL checkpoint.
    Checkpoint {
        state: Vec<u8>,
    },
    WriteBlob {
        dir: PathBuf,
        hash: BlobHash,
        bytes: Vec<u8>,
    },
    RefreshToken {
        account: AccountId,
    },
    SendPush(Push),
    Upload(AnalyticsBatch),
    /// Ask a replica's origin for up to `limit` items below `before_order`
    /// and absorb what joins the block; answered as
    /// [`JobResult::Fetched`](crate::payload::JobResult::Fetched). One round
    /// trip with one result until peer channels are the core's.
    FetchFromOrigin {
        agent: AgentKey,
        before_order: Option<u64>,
        limit: u32,
    },
}
