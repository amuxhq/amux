//! The daemon's inputs, outputs and jobs.

use std::ops::Range;
use std::path::PathBuf;

use store::AgentKey;

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
    /// Carry what the profile's shell publishes about `agent` on to the
    /// stream `call` opened, after the opening the core answered. Joined
    /// before the store is next written, so the opening and what follows
    /// it meet at one point; the shell's publishing moves into the core
    /// with ingest.
    Follow {
        call: CallId,
        agent: AgentKey,
    },
    /// Carry what the shell publishes about the fleet on to the inventory
    /// stream `call` opened, `only` one host's when it names one.
    FollowInventory {
        call: CallId,
        only: Option<Vec<u8>>,
    },
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
