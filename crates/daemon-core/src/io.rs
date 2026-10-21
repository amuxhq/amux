//! The daemon's inputs, outputs and jobs.

use std::net::SocketAddr;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use store::AgentKey;
use wire::{InventoryEvent, SessionEvent};

use crate::families::Ended;
use crate::ids::{AgentId, CallId, ChannelId, HostId, JobId, LinkId, PairingId, TimerId};
use crate::payload::{
    Advertisement, AgentSpec, AnalyticsBatch, Answer, CarrierKind, ChannelClass, ChannelError,
    ClientRequest, ControlFrame, Hello, JobResult, LinkCloseReason, LinkFrame, PairMessage,
    PeerRequest, PublicKey, Push, Response, Status,
};
use crate::presence::{Route, Tried};
use crate::{DumpReason, codec};

/// Everything that can happen to the daemon, decoded by the driver.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Input {
    /// The clock, as an input: the core reads time from nothing else.
    Tick {
        now_ms: u64,
    },
    /// A timer the core armed has fired.
    Timer {
        id: TimerId,
    },
    /// A link came up. `resumed` is the key a resumed handshake's ticket
    /// names, which no certificate proved this time: the core decides it
    /// before the driver serves any call the peer sent early.
    LinkUp {
        link: LinkId,
        peer: HostId,
        kind: CarrierKind,
        resumed: Option<PublicKey>,
    },
    LinkDown {
        link: LinkId,
        #[serde(with = "codec::numbered")]
        reason: LinkCloseReason,
    },
    LinkControl {
        link: LinkId,
        #[serde(with = "codec::message")]
        frame: LinkFrame,
    },
    /// The relay link presented a fresh credential, as it came up or after
    /// a refresh.
    Credential {
        link: LinkId,
        /// When the relay stops honouring it, on the core's clock.
        expires_at_ms: u64,
        /// When the account service issued it, on the core's clock: a
        /// credential kept across launches may be hours old.
        fetched_at_ms: u64,
        /// The free plan's refresh interval, when the account is on it and
        /// one is set: a purchase then reaches the link within it.
        free_every_ms: Option<u64>,
    },
    /// The profile's shell wants a channel to `peer` over `link`. Until
    /// the machines that open channels move into the core, the shell asks,
    /// and the key it pinned for the peer rides on the request; the core
    /// answers with [`Output::OpenChannel`] or [`Output::ChannelRefused`].
    WantChannel {
        link: LinkId,
        peer: HostId,
        class: ChannelClass,
        pin: Option<PublicKey>,
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
    /// The shell's try at an [`Output::Attempt`] concluded.
    AttemptConcluded {
        host: HostId,
        route: Route,
        tried: Tried,
    },
    /// The account's plan changed: what the relay carries changed with it.
    PlanChanged,
    /// A call to the host found no channel within its patience, or the
    /// channel it opened failed: `error` says why, as the driver saw it.
    CallsFailed {
        host: HostId,
        error: ChannelError,
    },
    /// A channel a call opened on `route` failed before it carried
    /// anything; the core answers with [`Output::NextRoute`], the route the
    /// call tries next.
    RouteFailed {
        host: HostId,
        route: Route,
        error: ChannelError,
    },
    /// Try the host now: what kept channels to it from opening is gone.
    TryAgain {
        host: HostId,
    },
    /// The host's trust ended or is being replaced.
    HostForgotten {
        host: HostId,
    },
    /// The trust file as the driver read it at start: the hosts this
    /// profile trusts and the keys it pins for them.
    TrustRead {
        store: crate::trust::TrustStore,
    },
    /// A change of the trust store nobody waits on: what a peer said of
    /// itself, or the address it answered at.
    ChangeTrust(crate::trust::TrustChange),
    /// Which replica agents keep a source open from now on.
    Policy(SourcePolicy),
    /// A paired host's call, from the host whose pinned key its channel's
    /// handshake presented.
    PeerCall {
        from: HostId,
        call: CallId,
        request: PeerRequest,
    },
    /// What came back of a call the core made with [`Output::Call`].
    PeerAnswer {
        call: CallId,
        answer: Answer,
    },
    /// A pairing conversation's carrier opened, and how.
    PairingOpened {
        session: PairingId,
        opened: crate::pairing::Opened,
    },
    /// One message of a pairing conversation, as its carrier read it.
    PairingMessage {
        session: PairingId,
        #[serde(with = "codec::message")]
        message: PairMessage,
    },
    /// A pairing conversation's carrier ended from the other side, with
    /// the status it ended with, if any.
    PairingClosed {
        session: PairingId,
        status: Option<crate::pairing::PairFailure>,
    },
    /// Hosts the local network browse resolved, as each resolves.
    Discovered {
        found: Vec<Advertisement>,
    },
    /// The host said it no longer trusts this one: it closed a link saying
    /// so, or refused this host's certificate as revoked on a channel.
    PeerRevoked {
        host: HostId,
    },
    /// The far host took this host's certificate on a channel the core
    /// asked for: its first answer was data.
    ChannelAnswered {
        channel: ChannelId,
    },
    /// This host's own entry as the driver describes it now: its name,
    /// sign-in, version, capabilities, platform and providers.
    OwnHost(#[serde(with = "codec::boxed")] Box<wire::HostEntry>),
    /// Every machine discovery holds in this profile's scope now, newest
    /// first.
    Candidates {
        found: Vec<crate::host_set::Candidate>,
    },
    /// Why the last dial to the host failed, or none once a route to it
    /// came up.
    DialError {
        host: HostId,
        error: Option<String>,
    },
    /// A connection to the agent's process said Hello.
    AgentHello {
        agent: AgentId,
        #[serde(with = "codec::message")]
        hello: Hello,
    },
    /// An agent's journal grew.
    Nudge {
        agent: AgentId,
    },
    /// The agent's process is gone, and how the driver knows.
    ControlClosed {
        agent: AgentId,
        ended: Ended,
    },
    /// The start's sweep found a process holding the agent's directory
    /// that has not answered yet; the driver holds it for dialing.
    Adopted {
        agent: AgentId,
    },
    /// The agent refused an input as exiting: it is on its way out.
    Exiting {
        agent: AgentId,
    },
    /// The interpreter's verdict on a message the core wrote to the agent,
    /// by the message's envelope id; none when the connection it was
    /// written on ended first.
    Verdict {
        agent: AgentId,
        input_id: Vec<u8>,
        #[serde(with = "codec::optional")]
        verdict: Option<wire::SendInputResponse>,
    },
    /// The start's sweep has reported every agent it found.
    Swept,
    /// Keep the store under its budgets now: the retention sweeps.
    Retain(crate::retention::Retain),
    /// A call by a client of this profile: its socket, an agent's tools
    /// socket or the in-process call.
    ClientCall {
        call: CallId,
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
    /// The drafts file as the driver read it at start.
    Drafts {
        #[serde(with = "codec::drafts::list")]
        drafts: Vec<(Vec<u8>, Vec<u8>)>,
    },
    /// What the profile's shell changed outside a step, for the core's
    /// subscribers. Each kind goes as the write behind it moves into the
    /// core.
    FromShell(FromShell),
}

impl Input {
    /// The input's kind, for logs.
    pub fn kind(&self) -> &'static str {
        match self {
            Input::Tick { .. } => "Tick",
            Input::Timer { .. } => "Timer",
            Input::LinkUp { .. } => "LinkUp",
            Input::LinkDown { .. } => "LinkDown",
            Input::LinkControl { .. } => "LinkControl",
            Input::Credential { .. } => "Credential",
            Input::WantChannel { .. } => "WantChannel",
            Input::ChannelOpened { .. } => "ChannelOpened",
            Input::ChannelClosed { .. } => "ChannelClosed",
            Input::AttemptConcluded { .. } => "AttemptConcluded",
            Input::PlanChanged => "PlanChanged",
            Input::CallsFailed { .. } => "CallsFailed",
            Input::RouteFailed { .. } => "RouteFailed",
            Input::TryAgain { .. } => "TryAgain",
            Input::HostForgotten { .. } => "HostForgotten",
            Input::TrustRead { .. } => "TrustRead",
            Input::ChangeTrust(_) => "ChangeTrust",
            Input::Policy(_) => "Policy",
            Input::PeerCall { .. } => "PeerCall",
            Input::PeerAnswer { .. } => "PeerAnswer",
            Input::PairingOpened { .. } => "PairingOpened",
            Input::PairingMessage { .. } => "PairingMessage",
            Input::PairingClosed { .. } => "PairingClosed",
            Input::Discovered { .. } => "Discovered",
            Input::PeerRevoked { .. } => "PeerRevoked",
            Input::ChannelAnswered { .. } => "ChannelAnswered",
            Input::OwnHost(_) => "OwnHost",
            Input::Candidates { .. } => "Candidates",
            Input::DialError { .. } => "DialError",
            Input::AgentHello { .. } => "AgentHello",
            Input::Nudge { .. } => "Nudge",
            Input::ControlClosed { .. } => "ControlClosed",
            Input::Adopted { .. } => "Adopted",
            Input::Exiting { .. } => "Exiting",
            Input::Verdict { .. } => "Verdict",
            Input::Swept => "Swept",
            Input::Retain(_) => "Retain",
            Input::ClientCall { .. } => "ClientCall",
            Input::CallCancelled { .. } => "CallCancelled",
            Input::AnswerLagged { .. } => "AnswerLagged",
            Input::JobDone { .. } => "JobDone",
            Input::Entropy(_) => "Entropy",
            Input::Configure(_) => "Configure",
            Input::Drafts { .. } => "Drafts",
            Input::FromShell(_) => "FromShell",
        }
    }
}

/// The profile's settings the core reads, as last configured.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Settings {
    /// How long a turn's notification waits before it is sent.
    pub notify_delay_ms: i64,
    /// How long a push that failed waits before it is tried again.
    pub push_retry_ms: u64,
    /// How long after a commit the core checkpoints: what a crash costs to
    /// replay, what the input ring holds, and how long the journal below
    /// the last checkpoint waits to be reclaimed.
    pub checkpoint_interval_ms: u64,
    /// The newest rows a replica keeps, K: no tail a stream opens with is
    /// longer.
    pub tail_rows: u32,
    /// How long a lost source or inventory stream waits before it is
    /// opened again, doubling after each loss up to the ceiling, and
    /// starting afresh once a stream has caught up.
    pub source_backoff_ms: u64,
    pub source_backoff_max_ms: u64,
    /// How long a new process has to say Hello.
    pub start_deadline_ms: u64,
    /// How long a stop may take before the process is killed, and how long
    /// a resume waits for a process on its way out.
    pub stop_deadline_ms: u64,
    /// How long a drain pass that kept rows waits before the next.
    pub delivery_retry_ms: u64,
    /// How long a message written to an agent waits for its verdict.
    pub reply_patience_ms: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            notify_delay_ms: 30_000,
            push_retry_ms: 60_000,
            checkpoint_interval_ms: 60_000,
            tail_rows: 200,
            source_backoff_ms: 1_000,
            source_backoff_max_ms: 30_000,
            start_deadline_ms: 20_000,
            stop_deadline_ms: 30_000,
            delivery_retry_ms: 30_000,
            reply_patience_ms: 30_000,
        }
    }
}

/// A change the profile's shell committed under the store it lends the
/// core, handed to the core while the store is still as that commit left
/// it, so subscribers read it in commit order and an opening never reads
/// it twice or misses it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum FromShell {
    /// One of an agent's session events.
    Session {
        #[serde(with = "codec::agent_key")]
        agent: AgentKey,
        #[serde(with = "codec::shared")]
        event: Arc<SessionEvent>,
    },
    /// An agent's row as the inventory carries it.
    Row {
        #[serde(with = "codec::agent_key")]
        agent: AgentKey,
        #[serde(with = "codec::shared")]
        event: Arc<InventoryEvent>,
    },
    /// Nothing about the agent, its session or its row, reaches a
    /// subscriber until [`FromShell::Unmute`]; its streams stay open. A
    /// delete mutes the agent it stops, so no subscriber reads the stop as
    /// an exit it could resume from before the removal arrives.
    Mute(#[serde(with = "codec::agent_key")] AgentKey),
    Unmute(#[serde(with = "codec::agent_key")] AgentKey),
}

/// Everything the daemon wants done. The driver encodes, sends, arms,
/// spawns and writes; it decides nothing.
#[derive(Clone, Debug, PartialEq)]
pub enum Output {
    /// Refuse the resumed link with the revocation alert: its ticket names
    /// a key no longer pinned for its peer. The driver resets the streams
    /// it held back; the core never took the link up.
    RefuseResumed {
        link: LinkId,
    },
    /// The addresses a trusted host last listed for itself changed: every
    /// one it can be reached at, in its order, each with where it came
    /// from.
    PeerAddresses {
        host: HostId,
        addrs: Vec<crate::payload::PeerAddress>,
    },
    /// The relay saw this host's packets come from a new address, which
    /// this host lists for itself from now on.
    ObservedAddress {
        addr: SocketAddr,
    },
    ArmTimer {
        id: TimerId,
        at_ms: u64,
    },
    CancelTimer {
        id: TimerId,
    },
    /// Open a channel to `peer` over `link`, handshaking against `pin`,
    /// and report it as [`Input::ChannelOpened`] or
    /// [`Input::ChannelClosed`] under `channel`.
    OpenChannel {
        channel: ChannelId,
        link: LinkId,
        peer: HostId,
        class: ChannelClass,
        pin: PublicKey,
    },
    /// The channel the shell wanted is not opened, and why.
    ChannelRefused {
        link: LinkId,
        peer: HostId,
        class: ChannelClass,
        reason: ChannelError,
    },
    CloseChannel {
        channel: ChannelId,
    },
    /// Try a calls channel to `host` on `route`, and report how it went as
    /// [`Input::AttemptConcluded`].
    Attempt {
        host: HostId,
        route: Route,
    },
    /// `route` no longer carries `host`'s traffic: forget the channels
    /// opened on it, so the next call opens a fresh one.
    DropRoute {
        host: HostId,
        route: Route,
    },
    /// Dial `host` at the addresses the local network last gave for it.
    Dial {
        host: HostId,
    },
    /// The route the call that reported [`Input::RouteFailed`] tries
    /// next, if any.
    NextRoute {
        host: HostId,
        route: Option<Route>,
    },
    /// Close `host`'s own links, as their idle timeout would, only sooner:
    /// calls over them find no channel. Redialling brings them back.
    CloseLinks {
        host: HostId,
    },
    /// Make `request` of `host` on a channel to it, and report what came
    /// back as [`Input::PeerAnswer`] under `call`: one answer for a unary
    /// call, one per event and then its end for a stream.
    Call {
        host: HostId,
        call: CallId,
        request: PeerRequest,
        /// What comes back is absorbed into the store, so the driver steps
        /// it with the store lent. A forwarded call's reply reads nothing
        /// of it, and is stepped at once.
        absorbed: bool,
    },
    /// The core wants nothing more of a call it made: the driver lets go
    /// of its stream, and reports nothing more of it.
    HangUp {
        call: CallId,
    },
    Answer {
        call: CallId,
        response: Response,
    },
    EndCall {
        call: CallId,
        status: Status,
    },
    /// What one ingest step did: for the shell's counters, and for whoever
    /// waits for the journal to be read to its end.
    Ingested {
        agent: AgentId,
        report: Ingested,
    },
    /// A pass over the deliveries outbox ended, the `pass`th since start.
    Drained {
        pass: u64,
        report: crate::DrainReport,
    },
    /// What a retention pass did.
    Retained(Box<crate::retention::Retained>),
    /// Work the profile's shell still does for the core.
    Shell(ShellWork),
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
    /// Dial the agent's process until a connection says Hello, reported
    /// as [`Input::AgentHello`], and report each Nudge on it; once the
    /// process is gone, report [`Input::ControlClosed`].
    DialAgent {
        agent: AgentId,
    },
    /// Write `frame` on the agent's control connection, in order with the
    /// frames before it; dropped if no connection is open.
    WriteControl {
        agent: AgentId,
        frame: ControlFrame,
    },
    /// Write `spec` into the agent's directory `dir` and start its process
    /// from it; a process that cannot be started is gone, reported as
    /// [`Input::ControlClosed`].
    SpawnAgent {
        dir: PathBuf,
        spec: Box<AgentSpec>,
    },
    /// The agent's exit is recorded: let go of its process, close its
    /// tools socket and tell whoever waits on it.
    ForgetAgent {
        agent: AgentId,
    },
    Job {
        id: JobId,
        op: Job,
    },
    /// Send `message` on the pairing conversation's carrier.
    Pair {
        session: PairingId,
        message: PairMessage,
    },
    /// The joining side's conversation learned who the peer is: it waits,
    /// under `token`, for a person to confirm or abandon it.
    PairPending {
        session: PairingId,
        token: Vec<u8>,
        peer: wire::PairingIdentity,
    },
    /// End the pairing conversation's carrier: cleanly, or with `failure`
    /// as its status.
    PairEnded {
        session: PairingId,
        failure: Option<crate::pairing::PairFailure>,
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
    /// How far the step found the agent's journal, as a global offset:
    /// the journal cut here reads as the step read it.
    pub seen: u64,
}

/// One line of a driver's record of what its core was fed: an input, or
/// what the step of the input before it found of the world it reads
/// inline. Ingest reads an agent's journal and commits to the drive inside
/// the step, so replaying a step needs each journal as far as that step
/// found it, and whether the drive refused its commit; one step may read
/// several agents' journals, each its own line.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Recorded {
    /// An input, written before the core stepped it.
    Input(Box<Input>),
    /// The step of the input recorded before this line read the agent's
    /// journal up to `seen`, and committed nothing when it `failed`, in its
    /// own words.
    Read {
        agent: AgentId,
        seen: u64,
        failed: Option<String>,
    },
}

impl Recorded {
    /// The line [`Recorded::Input`] writes, for an input held borrowed.
    pub fn input(input: &Input) -> impl Serialize + '_ {
        struct Line<'a>(&'a Input);
        impl Serialize for Line<'_> {
            fn serialize<S: serde::Serializer>(&self, to: S) -> Result<S::Ok, S::Error> {
                to.serialize_newtype_variant("Recorded", 0, "Input", self.0)
            }
        }
        Line(input)
    }

    /// What an ingest step's report says of the world it read.
    pub fn read(agent: AgentId, report: &Ingested) -> Recorded {
        Recorded::Read {
            agent,
            seen: report.seen,
            failed: match &report.end {
                IngestEnd::Failed(why) => Some(why.clone()),
                IngestEnd::More | IngestEnd::Read => None,
            },
        }
    }
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

/// What the replicas' sources leave to the profile's shell: what it owns
/// beside the store, until it moves into the core.
#[derive(Clone, Debug, PartialEq)]
pub enum ShellWork {
    /// The core wrote items to the store: an absorb, catch-up or live.
    Wrote,
    /// A source opened for the replica agent, or closed.
    Sourcing { agent: AgentKey, open: bool },
    /// The host set as the core published it now, for the shell's reads.
    Hosts { entries: Vec<wire::HostEntry> },
    /// The replica is gone from the store: its directory and blob files
    /// go with it.
    ReplicaDropped { agent: AgentKey },
    /// The trust store as the core serves it now, for the handshakes to
    /// read, and the change that made it, once that change is on the
    /// drive: what follows it on the connections is the shell's.
    Trust {
        store: Box<crate::trust::TrustStore>,
        changed: Option<(HostId, crate::trust::TrustChanged)>,
    },
    /// The host's key will not be replaced after all: what waited on the
    /// replacement goes on.
    ReplacementEnded { host: HostId },
    /// Whether the pairing window is open, and whether too many wrong
    /// secrets closed the last one, said when either moves.
    PairWindow { active: bool, locked_out: bool },
    /// What a pairing did, for the audit log and analytics.
    Pairing(crate::pairing::PairingEvent),
}

/// What a parent's row says to the children that follow it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ParentSays {
    /// The parent exited, whatever the cause: its children stop.
    Exited,
    /// The parent is gone from its host for good: its children are stopped
    /// and deleted.
    Deleted,
}

/// Which replica agents the profile keeps a source open for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SourcePolicy {
    /// Every agent a reachable host lists. A desktop daemon, and the
    /// phone's runtime in the foreground.
    #[default]
    Listed,
    /// Only agents a client subscribes to: the phone's runtime woken in the
    /// background, bringing the one chat a notification names current.
    OnDemand,
}

/// A result-only mechanism the driver runs off the core thread; its outcome
/// returns as [`Input::JobDone`].
#[derive(Clone, Debug, PartialEq)]
pub enum Job {
    /// Replace the file at `path` with `bytes` so that a crash or power
    /// cut leaves the old file or the new one, and the new one is on the
    /// drive before the job is done.
    Fsync {
        path: PathBuf,
        bytes: Vec<u8>,
    },
    /// Close every connection to `host`: its key is being replaced, and
    /// nothing opened under the old one may outlive it.
    TeardownHost {
        host: HostId,
    },
    /// Run a passive WAL checkpoint on a connection to the profile store
    /// of the job's own, so the store's main file holds every commit; one
    /// that cannot move the whole WAL fails.
    CheckpointWal,
    /// Write the serialized core beside the store's passive WAL checkpoint.
    Checkpoint {
        state: Vec<u8>,
    },
    /// Write `bytes` into `dir` under their hash, `hash`, unless a file of
    /// that name is there; answered as [`JobResult::Refused`] when the
    /// drive is full.
    WriteBlob {
        dir: PathBuf,
        hash: Vec<u8>,
        bytes: Vec<u8>,
    },
    /// Exchange the account's refresh token for a fresh relay credential
    /// and present it on `link`, which reports it as [`Input::Credential`];
    /// a refresh that fails ends the link.
    RefreshToken {
        link: LinkId,
    },
    /// Hand the push to the profile's sender.
    SendPush(Push),
    /// Build the exited agent's next spec from its newest, carrying the
    /// settings its newest `snapshot` body says it last ran with, its queue
    /// seeded by `bootstrap`; answered as [`JobResult::Spec`].
    NextSpec {
        agent: AgentId,
        bootstrap: Option<wire::Bootstrap>,
        snapshot: Option<Vec<u8>>,
    },
    Upload(AnalyticsBatch),
    /// Kill the agent's process group from outside: the process this
    /// daemon started, or the one on the other end of its control socket.
    /// Fails when there is none it can kill.
    KillAgent {
        agent: AgentId,
    },
    /// Wait until no process holds the agent's directory's lock, at most
    /// `patience_ms` on the profile's clock.
    AwaitUnlocked {
        agent: AgentId,
        patience_ms: u64,
    },
    /// Remove the directory and everything in it; one already gone is
    /// removed.
    RemoveDir {
        path: PathBuf,
    },
    /// Compare the working tree at `cwd` with `base`, answered as
    /// [`JobResult::Compared`] or [`JobResult::CompareFailed`]; the patch
    /// is built only `with_patch`.
    Compare {
        cwd: PathBuf,
        base: wire::diff_base::Base,
        with_patch: bool,
    },
    /// Make a worktree of the repository at `repo` named `name`, answered
    /// as [`JobResult::Worktree`] or [`JobResult::Refused`].
    MakeWorktree {
        repo: PathBuf,
        name: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::payload::{
        Answer, ClientRequest, ForwardRefusal, JobResult, PeerFailure, PeerReply, PeerRequest,
    };

    #[test]
    fn every_kind_of_input_reads_back_as_written() {
        let agent = AgentKey::new(b"host".to_vec(), b"agent".to_vec());
        let inputs = vec![
            Input::Tick { now_ms: 7 },
            Input::Timer {
                id: TimerId::Checkpoint,
            },
            Input::LinkUp {
                link: LinkId(1),
                peer: HostId([1; 16]),
                kind: CarrierKind::Direct,
                resumed: Some(PublicKey(vec![7; 32])),
            },
            Input::LinkControl {
                link: LinkId(1),
                frame: wire::Message {
                    body: Some(wire::message::Body::NeighborDown(wire::NeighborDown {
                        host_id: vec![2; 16],
                    })),
                },
            },
            Input::WantChannel {
                link: LinkId(1),
                peer: HostId([1; 16]),
                class: ChannelClass::Session,
                pin: Some(PublicKey(vec![7; 32])),
            },
            Input::ChannelOpened {
                channel: ChannelId(4),
                link: LinkId(1),
                peer: HostId([1; 16]),
                class: ChannelClass::Bulk,
            },
            Input::ChannelClosed {
                channel: ChannelId(4),
                reason: ChannelError::Refused(ForwardRefusal::PaymentRequired),
            },
            Input::LinkDown {
                link: LinkId(1),
                reason: LinkCloseReason::UserShutdown,
            },
            Input::AttemptConcluded {
                host: HostId([1; 16]),
                route: Route::Via(HostId([2; 16])),
                tried: Tried::Failed(ChannelError::Refused(ForwardRefusal::PaymentRequired)),
            },
            Input::Timer {
                id: TimerId::Retry(HostId([1; 16])),
            },
            Input::Credential {
                link: LinkId(1),
                expires_at_ms: 900_000,
                fetched_at_ms: 300_000,
                free_every_ms: Some(60_000),
            },
            Input::PlanChanged,
            Input::CallsFailed {
                host: HostId([1; 16]),
                error: ChannelError::Handshake("no channel opened in time".to_owned()),
            },
            Input::RouteFailed {
                host: HostId([1; 16]),
                route: Route::Direct(LinkId(2)),
                error: ChannelError::LinkClosed,
            },
            Input::ClientCall {
                call: CallId(2),
                request: ClientRequest::Subscribe {
                    agent_id: vec![1, 2],
                    tail: 6,
                },
            },
            Input::PeerCall {
                from: HostId([1; 16]),
                call: CallId(7),
                request: PeerRequest::Subscribe(wire::PeerSubscribeRequest {
                    agent_id: vec![1, 2],
                    from: Some(wire::peer_subscribe_request::From::After(wire::After {
                        revision: 3,
                        cap: 4,
                        generation: 5,
                    })),
                }),
            },
            Input::ClientCall {
                call: CallId(8),
                request: ClientRequest::Forward {
                    host: HostId([2; 16]),
                    request: Box::new(PeerRequest::StopAgent(wire::StopAgentRequest {
                        agent_id: vec![3],
                    })),
                },
            },
            Input::PeerAnswer {
                call: CallId(9),
                answer: Answer::Reply(Box::new(PeerReply::Done)),
            },
            Input::PeerAnswer {
                call: CallId(10),
                answer: Answer::Failed(PeerFailure::Refused(wire::Error {
                    code: wire::ErrorCode::NotFound as i32,
                    message: "gone".into(),
                    details: Vec::new(),
                })),
            },
            Input::Discovered {
                found: vec![Advertisement {
                    host: HostId([4; 16]),
                    trusted: true,
                }],
            },
            Input::ClientCall {
                call: CallId(3),
                request: ClientRequest::Fetch(wire::FetchRequest {
                    agent_id: vec![9],
                    limit: 3,
                    ..Default::default()
                }),
            },
            Input::ClientCall {
                call: CallId(4),
                request: ClientRequest::SubscribeInventory,
            },
            Input::PeerRevoked {
                host: HostId([1; 16]),
            },
            Input::ChannelAnswered {
                channel: ChannelId(4),
            },
            Input::OwnHost(Box::new(wire::HostEntry {
                host_id: vec![1],
                name: "desk".into(),
                account_signed_in: Some(true),
                ..Default::default()
            })),
            Input::Candidates {
                found: vec![crate::host_set::Candidate {
                    host: HostId([4; 16]),
                    name: "box".into(),
                    addrs: vec!["10.0.0.4:7000".into()],
                }],
            },
            Input::DialError {
                host: HostId([1; 16]),
                error: Some("refused".into()),
            },
            Input::JobDone {
                job: JobId(5),
                result: JobResult::Done,
            },
            Input::PeerAnswer {
                call: CallId(11),
                answer: Answer::Session(SessionEvent {
                    of: Some(wire::session_event::Of::CaughtUp(wire::CaughtUp {
                        revision: 4,
                    })),
                }),
            },
            Input::PeerAnswer {
                call: CallId(12),
                answer: Answer::Inventory(InventoryEvent {
                    of: Some(wire::inventory_event::Of::CaughtUp(
                        wire::CaughtUp::default(),
                    )),
                }),
            },
            Input::PeerAnswer {
                call: CallId(12),
                answer: Answer::Ended,
            },
            Input::Timer {
                id: TimerId::Source(HostId([1; 16]), AgentId([2; 16])),
            },
            Input::TrustRead {
                store: crate::trust::TrustStore::of_hosts([HostId([1; 16])]),
            },
            Input::Policy(SourcePolicy::OnDemand),
            Input::FromShell(FromShell::Session {
                agent: agent.clone(),
                event: Arc::new(SessionEvent {
                    of: Some(wire::session_event::Of::Item(wire::Item {
                        key: "k".into(),
                        text: "text".into(),
                        ..Default::default()
                    })),
                }),
            }),
            Input::FromShell(FromShell::Mute(agent)),
            Input::ClientCall {
                call: CallId(13),
                request: ClientRequest::CreateAgent(Box::new(crate::Create {
                    agent: AgentId([5; 16]),
                    cwd: "/src".into(),
                    given: None,
                    branches: ["main".to_owned()].into(),
                    new_worktree: true,
                    spec: wire::AgentSpec {
                        name: "lead".into(),
                        ..Default::default()
                    },
                })),
            },
            Input::ClientCall {
                call: CallId(14),
                request: ClientRequest::ResumeAgent(Box::new(crate::Resume {
                    agent: AgentId([5; 16]),
                    bootstrap: None,
                })),
            },
            Input::ControlClosed {
                agent: AgentId([5; 16]),
                ended: Ended::CouldNotStart("no such file".into()),
            },
            Input::Adopted {
                agent: AgentId([5; 16]),
            },
            Input::Swept,
            Input::JobDone {
                job: JobId(6),
                result: JobResult::Worktree {
                    path: "/src/.worktrees/lead".into(),
                    base_branch: "main".into(),
                },
            },
            Input::ClientCall {
                call: CallId(15),
                request: ClientRequest::PutBlob {
                    request: wire::PutBlobRequest {
                        agent_id: vec![5; 16],
                        name: "notes.txt".into(),
                        mime: "text/plain".into(),
                        bytes: b"notes".to_vec(),
                    },
                    hash: vec![9; 32],
                },
            },
            Input::ClientCall {
                call: CallId(16),
                request: ClientRequest::KeepBlob {
                    agent_id: vec![5; 16],
                    hash: vec![9; 32],
                    bytes: b"notes".to_vec(),
                },
            },
            Input::ClientCall {
                call: CallId(17),
                request: ClientRequest::SendMessage {
                    envelope: Box::new(wire::Envelope {
                        id: vec![4; 16],
                        ..Default::default()
                    }),
                    from: crate::payload::MessageFrom::Agent(AgentId([5; 16])),
                },
            },
            Input::Verdict {
                agent: AgentId([5; 16]),
                input_id: vec![4; 16],
                verdict: None,
            },
            Input::JobDone {
                job: JobId(7),
                result: JobResult::Spec(Some(Box::new(wire::AgentSpec {
                    name: "lead".into(),
                    ..Default::default()
                }))),
            },
            Input::Timer {
                id: TimerId::Lane(AgentId([5; 16])),
            },
            Input::Configure(Settings::default()),
            Input::Entropy([3; 32]),
        ];
        for input in inputs {
            let line = serde_json::to_string(&input).expect("an input writes");
            let read: Input = serde_json::from_str(&line).expect("an input reads back");
            assert_eq!(read, input, "{line}");
        }
    }
}
