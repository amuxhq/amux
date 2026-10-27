//! One profile runtime: a complete amux with its own host id, store and
//! agents. This module is its agent registry's shell: spawn, resume, stop,
//! delete and rename read what they need of the world here and are decided
//! by the profile's core, and the agents' processes are kept here for the
//! driver, as is the startup sweep that finds them on disk.
//!
//! The runtime keeps one [`AgentHandle`] per agent whose process is live
//! (or whose directory is locked by one): the directory, the control
//! connection, and the tools socket the agent's MCP server dials. Nothing
//! here is persisted; after a restart the sweep finds them again from the
//! agent directories and the store's rows, and reports what it found to
//! the core.
//!
//! Each handle has one watcher task. It dials `ctl.sock`, reads the Hello,
//! ingests on every Nudge, and when the connection ends works out whether
//! the process is gone (its lock is free) or the connection merely
//! dropped (it redials). A process that is gone is reported to the core,
//! which ingests its journal's remainder and records its exit, and then
//! lets the handle go.

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use agent_dir::Clock;
use agent_dir::local_socket::{self, LocalListener, LocalStream};
use daemon_core::Refusal;
pub use daemon_core::ingest::{INGEST_BATCH, KEPT_SEGMENTS};
use daemon_core::payload::{CallError, ClientRequest, Response, Status};
use store::{AgentKey, AgentRow, Sqlite, StoreError};
use tokio::io::{AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use uuid::Uuid;
use wire::{
    Agent, AgentHello, AgentRef, AgentSpec, Bootstrap, CreateAgentRequest, CtlFrame, DumpPart,
    ErrorCode, Kind, SendInputResponse, ctl_frame,
};

use crate::driver::{Ask, Delivered};
use crate::grpc::ClientApi;
use crate::install::{AGENTS, private_dir};
use crate::outbox::PushSender;
use crate::profiles::ProfileId;
use crate::retention::Retention;
use crate::{HostId, spec};

pub type AgentId = Uuid;

/// What [`ProfileRuntime::set_join_hook`] installs.
pub type JoinHook =
    Arc<dyn Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> + Send + Sync>;

/// How long between looks at a directory whose process is starting,
/// stopping or not answering: short, because a start and a resume wait on
/// it, and each look is one lock attempt and one dial.
const PROBE_INTERVAL: Duration = Duration::from_millis(20);
/// How long the sweep waits for a locked directory's process to answer
/// before leaving it to its watcher.
const LOOK_PATIENCE: Duration = Duration::from_secs(2);
/// How long a connected agent has to send its Hello.
const HELLO_PATIENCE: Duration = Duration::from_secs(5);
/// How long a clean shutdown lets the dumps the core asked for go on
/// before it abandons them, leaving their bundles unfinished. A dump waits
/// on agents and paired hosts for their parts, ten seconds each, and on a
/// loaded machine a shutdown would otherwise wait out every one of them.
pub const DUMPS_FINISH_WITHIN: Duration = Duration::from_secs(5);
/// Live events a subscriber's queue holds unread. A batch's records are
/// answered in one burst at memory speed by the step that commits it, so a
/// reader's slack is counted in batches: a subscriber that has read
/// everything when a batch lands is closed with Lagged only after four
/// whole batches (a second or so of a busy agent) arrive without it
/// reading any, which absorbs a late wakeup by design rather than by luck.
/// A queue of one batch would close a caught-up reader whenever a short
/// batch followed a full one before its task was scheduled.
pub const FANOUT_CAPACITY: usize = 4 * INGEST_BATCH;
/// The tail size K: the rows a replica keeps and asks for, and the newest
/// rows own retention never trims. About an hour of a busy agent and
/// several screens of scroll-back.
pub const TAIL_ROWS: u32 = 200;

/// What the daemon starts agents with; read at every spawn and resume, so
/// a change affects the next incarnation and never a running agent.
#[derive(Clone, Debug)]
pub struct Launch {
    /// The canonical install path: `amux agent <dir>` runs from it, and the
    /// agent's hooks and tool server run from it too.
    pub install_path: PathBuf,
    pub claude_command: String,
    pub codex_command: String,
    /// Environment additions for every provider child.
    pub provider_env: HashMap<String, String>,
    pub agent: settings::AgentSettings,
    /// The agent's journal segment size; its default when zero.
    pub journal_segment_bytes: u64,
    /// How long a new process has to take its lock and send its Hello.
    pub start_deadline_ms: i64,
    /// How long a stop may take before the daemon kills the process group.
    pub stop_deadline_ms: i64,
    /// How long one write on an agent's control connection may take,
    /// waiting for the connection included. Past it the agent is taken to
    /// have stopped reading, and the connection is closed.
    pub ctl_write_ms: i64,
    /// How long an input waits for the interpreter's verdict, which it
    /// gives at once; past this the answer is taken to be lost.
    pub reply_patience_ms: i64,
    /// How long a notification waits before it is sent, so an answer from
    /// another client cancels it.
    pub notify_delay_ms: i64,
    /// How far a subscription may fall behind its agent's broadcast before
    /// it is closed with Lagged. Read when a runtime opens.
    pub fanout_capacity: usize,
    /// The same for inventory subscriptions.
    pub inventory_capacity: usize,
    /// How often the deliveries outbox is tried again while rows wait.
    pub delivery_retry_ms: i64,
    /// How long a push that failed waits before it is tried again.
    pub push_retry_ms: i64,
    /// The bytes this profile's own rows may hold before retention removes
    /// exited agents and trims live ones.
    pub own_budget_bytes: u64,
    /// About how much one trimming round takes from the largest live agent.
    pub retention_chunk_bytes: u64,
    /// The newest rows retention never trims: the tail size replicas keep.
    pub tail_rows: u32,
    /// How often the retention sweep runs.
    pub retention_interval_ms: i64,
    /// The bytes rows replicated from other hosts may hold.
    pub replica_budget_bytes: u64,
    /// The bytes replica blob files may hold.
    pub replica_blob_budget_bytes: u64,
    /// A source's first wait before reconnecting to a reachable origin;
    /// it doubles on each failure up to the ceiling.
    pub source_backoff_ms: i64,
    pub source_backoff_max_ms: i64,
    /// Where the host looks for Git repositories to offer new agents.
    pub repository_roots: Vec<PathBuf>,
    /// How old the host's copy of what a provider offers may grow before
    /// it is asked again.
    pub catalogue_max_age_ms: i64,
    /// How often a copy's provider version is read to see whether it
    /// changed, and how soon a copy that says signed out is asked again.
    pub catalogue_recheck_ms: i64,
    /// How long after a commit the profile's core checkpoints: what the
    /// input ring holds, and how long ingested journal waits to be
    /// reclaimed.
    pub checkpoint_interval_ms: i64,
    /// How long one step of the profile's core may take before it is
    /// reported with a dump.
    pub step_budget: daemon_core::StepBudget,
    /// How long one step of an agent's core may take before it asks for a
    /// dump, in microseconds; 0 leaves the agent its own budget.
    pub agent_step_budget_micros: u64,
    /// How long after a dump the profile's core asked for another of the
    /// same kind is held back, and counted in the next.
    pub dump_interval_ms: u64,
    /// Where the profile's core is stepped by its caller; none gives it a
    /// thread of its own.
    pub cores: Option<crate::Cores>,
}

/// The bytes of stored rows a profile keeps for the agents this host runs;
/// an agent's directory and blobs go with its row. Basis: a generous multiple
/// of the largest transcripts seen so far, pending measurement against
/// typical sizes.
pub const OWN_BUDGET_BYTES: u64 = 2048 << 20;
/// The bytes of rows replicated from other hosts a runtime keeps. Basis: the
/// bound the old client cache had.
pub const REPLICA_BUDGET_BYTES: u64 = 256 << 20;
/// The bytes of blobs fetched from other hosts a runtime keeps, evicted
/// least recently read first. Basis: twice the row budget, since images
/// dominate what is fetched; to be measured.
pub const REPLICA_BLOB_BUDGET_BYTES: u64 = 512 << 20;

impl Default for Launch {
    fn default() -> Self {
        Self {
            install_path: std::env::current_exe().unwrap_or_else(|_| PathBuf::from("amux")),
            claude_command: "claude".to_owned(),
            codex_command: "codex".to_owned(),
            provider_env: HashMap::new(),
            agent: settings::AgentSettings::default(),
            journal_segment_bytes: 0,
            start_deadline_ms: 20_000,
            stop_deadline_ms: 30_000,
            ctl_write_ms: 5_000,
            reply_patience_ms: 30_000,
            notify_delay_ms: 30_000,
            fanout_capacity: FANOUT_CAPACITY,
            inventory_capacity: 1024,
            delivery_retry_ms: 30_000,
            push_retry_ms: 60_000,
            own_budget_bytes: OWN_BUDGET_BYTES,
            // A few MiB: a small overrun costs a small trim.
            retention_chunk_bytes: 4 << 20,
            tail_rows: TAIL_ROWS,
            retention_interval_ms: 10 * 60_000,
            replica_budget_bytes: REPLICA_BUDGET_BYTES,
            replica_blob_budget_bytes: REPLICA_BLOB_BUDGET_BYTES,
            source_backoff_ms: 1_000,
            source_backoff_max_ms: 30_000,
            repository_roots: Vec::new(),
            catalogue_max_age_ms: 30 * 60_000,
            catalogue_recheck_ms: 60_000,
            checkpoint_interval_ms: 60_000,
            step_budget: daemon_core::StepBudget::DAEMON,
            agent_step_budget_micros: 0,
            dump_interval_ms: core_ring::DumpLimit::INTERVAL_MS,
            cores: None,
        }
    }
}

/// The profile's core has stopped, and its store with it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("the profile is no longer running")]
pub struct Stopped;

impl From<Stopped> for RegistryError {
    fn from(stopped: Stopped) -> Self {
        RegistryError::Core(stopped.to_string())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("no agent {0}")]
    NotFound(AgentId),
    #[error("an agent with id {0} exists")]
    AlreadyExists(AgentId),
    #[error("agent {0} is live; stop it before resuming it")]
    Live(AgentId),
    #[error("an agent id is 16 bytes, not {0}")]
    BadId(usize),
    #[error("a new agent names its kind")]
    NoKind,
    #[error("the working directory {0} is not a directory on this host")]
    BadCwd(String),
    #[error(
        "starting an agent on {0} needs a folder there: pass cwd as a path on {0}, \
         asking the person which one if you do not know one"
    )]
    NoFarCwd(String),
    #[error("{}", name_words(*.0))]
    BadName(wire::AgentNameProblem),
    #[error("another agent on this host is already named {0}; names are unique on a host")]
    NameTaken(String),
    #[error("starting in a new worktree: {0}")]
    Worktree(#[from] git_facts::WorktreeError),
    #[error("spawning on another host goes through that host's daemon")]
    OtherHost,
    #[error("{}", kind_not_run_words(.host, *.kind, *.runs_none))]
    KindNotRun {
        host: String,
        kind: Kind,
        /// The host runs no agents at all, as a phone does.
        runs_none: bool,
    },
    #[error(transparent)]
    HostName(#[from] crate::forward::HostNameError),
    #[error(transparent)]
    Forwarded(#[from] crate::forward::ForwardError),
    /// A create whose parent this host already sees as exited or deleted:
    /// the child would only be stopped again by the family rule.
    #[error("agent {0} has ended and starts no more agents")]
    ParentEnded(AgentId),
    #[error("agent {0} did not start within the start deadline")]
    StartTimeout(AgentId),
    #[error("agent {0} did not stop within the stop deadline")]
    StopTimeout(AgentId),
    #[error("agent {0}'s previous process still holds its directory")]
    StillLocked(AgentId),
    #[error("agent {0} has no spec to resume from")]
    NoSpec(AgentId),
    #[error("the store: {0}")]
    Store(#[from] StoreError),
    #[error("the agent directory: {0}")]
    Io(#[from] io::Error),
    #[error("ingesting the journal: {0}")]
    Ingest(String),
    /// Refused as the driver said it, such as a worktree that could not be
    /// made.
    #[error("{}", .0.message)]
    Refused(wire::Error),
    #[error("the profile's core: {0}")]
    Core(String),
}

/// What [`ProfileRuntime::ingest`] committed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Ingested {
    /// Journal frames committed while it waited.
    pub frames: u64,
}

/// Why a host will not start an agent of a kind, as a sentence.
fn kind_not_run_words(host: &str, kind: Kind, runs_none: bool) -> String {
    if runs_none {
        return format!("{host} runs no agents; start the agent on a host that runs them");
    }
    let kind = match kind {
        Kind::ClaudePty => "terminal Claude",
        Kind::ClaudeSdk => "headless Claude",
        Kind::Codex => "Codex",
        Kind::Unspecified => "these",
    };
    format!("{host} does not run {kind} agents")
}

/// A refused name, as the daemon's error says it.
fn name_words(problem: wire::AgentNameProblem) -> String {
    match problem {
        wire::AgentNameProblem::Empty => "an agent's name cannot be empty".to_owned(),
        wire::AgentNameProblem::TooLong => {
            format!("an agent's name is at most {} characters", wire::AGENT_NAME_MOST)
        }
        wire::AgentNameProblem::Characters => {
            "an agent's name is lowercase letters, digits and hyphens, starting with a letter or digit"
                .to_owned()
        }
    }
}

impl RegistryError {
    pub fn to_wire(&self) -> wire::Error {
        let code = match self {
            Self::NotFound(_) | Self::NoSpec(_) => ErrorCode::NotFound,
            Self::AlreadyExists(_) | Self::NameTaken(_) => ErrorCode::AlreadyExists,
            Self::Live(_) | Self::StillLocked(_) => ErrorCode::FailedPrecondition,
            Self::BadId(_)
            | Self::NoKind
            | Self::BadCwd(_)
            | Self::NoFarCwd(_)
            | Self::BadName(_) => ErrorCode::InvalidArgument,
            Self::OtherHost => ErrorCode::Unimplemented,
            Self::KindNotRun { .. } | Self::ParentEnded(_) => ErrorCode::FailedPrecondition,
            Self::StartTimeout(_) | Self::StopTimeout(_) => ErrorCode::Aborted,
            Self::Worktree(error) => match error {
                git_facts::WorktreeError::BranchExists(_)
                | git_facts::WorktreeError::DestinationExists(_) => ErrorCode::AlreadyExists,
                git_facts::WorktreeError::NotARepository(_)
                | git_facts::WorktreeError::NoBranch(_)
                | git_facts::WorktreeError::BadBranchName(_) => ErrorCode::FailedPrecondition,
                git_facts::WorktreeError::Git(_) => ErrorCode::Internal,
            },
            Self::Store(_) | Self::Io(_) | Self::Ingest(_) | Self::Core(_) => ErrorCode::Internal,
            Self::Refused(error) => return error.clone(),
            Self::HostName(error) => return error.to_wire(),
            Self::Forwarded(error) => return error.to_wire(),
        };
        crate::grpc::wire_error(code, self.to_string())
    }
}

/// What the sweep found, each agent listed once.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SweepReport {
    /// Locked directories whose process answered with a Hello.
    pub live: Vec<AgentId>,
    /// Locked directories whose process did not answer yet; their watcher
    /// keeps trying.
    pub unanswered: Vec<AgentId>,
    /// Rows the sweep found with no process: marked exited, their journal
    /// remainder ingested and their CaughtUp flag set.
    pub exited: Vec<AgentId>,
    /// Directories with no row: what a delete left when it stopped between
    /// removing the rows and the directory. Removed.
    pub removed: Vec<AgentId>,
}

/// The read-only half of the sweep, done before activation.
pub struct Looked {
    agents: Vec<(AgentId, Look)>,
    orphans: Vec<AgentId>,
}

enum Look {
    Live(Box<Connected>),
    Unanswered,
    Exited,
    NoDirectory,
}

struct Connected {
    reader: ReadHalf<LocalStream>,
    writer: WriteHalf<LocalStream>,
    hello: AgentHello,
    /// The agent process's id, from the socket: whoever started it.
    peer: Option<u32>,
}

pub struct ProfileRuntime {
    profile: ProfileId,
    /// The installation's generation, carried on this host's entry.
    generation: u64,
    dir: PathBuf,
    clock: Arc<dyn Clock>,
    launch: Mutex<Launch>,
    /// The profile's core thread, which owns the store.
    driver: crate::driver::Driver,
    /// For tests: awaited by a subscribe on the core thread before the
    /// core reads its cut.
    pub(crate) join_hook: Mutex<Option<JoinHook>>,
    /// Counts the store's writes, own commits and absorbed replica rows
    /// alike, so a waiter wakes on the write it is waiting for instead of
    /// polling: a message waiting for its acceptance item, a test fencing
    /// a host to an order.
    pub(crate) commits: watch::Sender<u64>,
    /// Journal frames committed since start: what ingest has done, so its
    /// cost per frame can be measured from outside.
    ingested_frames: AtomicU64,
    pub(crate) push: Arc<dyn PushSender>,
    /// The outbox drains and the retention sweep.
    pub(crate) background: Mutex<Vec<JoinHandle<()>>>,
    /// The dumps the core asked for that are still being written; none
    /// once the profile stops waiting for them, so no dump starts after. A
    /// crash aborts them, as it does every other task of the run; a
    /// shutdown lets them finish for [`DUMPS_FINISH_WITHIN`].
    pub(crate) core_dumps: Mutex<Option<Vec<JoinHandle<()>>>>,
    /// The dumps this host's agents asked for that were written, by kind:
    /// twenty agents meeting one broken invariant are one bundle in each
    /// interval.
    pub(crate) agent_dumps: Mutex<core_ring::DumpLimit>,
    agents: Mutex<HashMap<AgentId, Arc<AgentHandle>>>,
    pub(crate) retention: watch::Sender<Retention>,
    pub(crate) reports: PathBuf,
    pub(crate) daemon_log: Option<PathBuf>,
    pub(crate) supervisor_log: Option<PathBuf>,
    worktrees: Arc<dyn crate::worktree::MakeWorktree>,
    pub(crate) me: Weak<ProfileRuntime>,
    /// The network edge, once the profile is in service.
    edge: std::sync::OnceLock<Arc<crate::edge::Edge>>,
    /// What the shell keeps of replication; the core keeps the rest.
    pub(crate) replicas: Mutex<crate::replicas::Replicas>,
    /// Replica blob files by last read, indexed at the first sweep.
    pub(crate) replica_blobs: Mutex<Option<store::BlobLru>>,
    /// The host set as the core last published it on the inventory.
    pub(crate) hosts: Mutex<Vec<wire::HostEntry>>,
    analytics: analytics::Analytics,
    /// Turns ended since the last check-in.
    turns: crate::telemetry::Turns,
    /// What each provider offers on this host.
    offers: crate::offers::Offers,
}

/// A write waiting for its verdict: its number, and where the verdict
/// goes, or none once no verdict will be reported for the write.
type Waiting = (u64, oneshot::Sender<Option<SendInputResponse>>);

pub(crate) struct AgentHandle {
    dir: PathBuf,
    /// The write half of the control connection, while one is open.
    pub(crate) ctl: tokio::sync::Mutex<Option<WriteHalf<LocalStream>>>,
    /// The frames the core asked to write, in its order.
    writes: mpsc::UnboundedSender<CtlFrame>,
    /// Inputs waiting for their verdict on the current connection, by
    /// input id, with the number of the write that waits; dropped when the
    /// connection ends.
    replies: Mutex<HashMap<Vec<u8>, Waiting>>,
    /// The number the next write waiting for a verdict takes.
    next_write: std::sync::atomic::AtomicU64,
    /// Inputs written to the process and not yet answered.
    pub(crate) handed: std::sync::atomic::AtomicUsize,
    /// Dumps waiting for the agent's part, by dump id; dropped when the
    /// connection ends.
    dumps: Mutex<HashMap<Vec<u8>, oneshot::Sender<DumpPart>>>,
    hello: watch::Sender<Option<AgentHello>>,
    /// The process id, once this daemon started the process.
    pid: Mutex<Option<u32>>,
    /// The process id on the other end of the control socket, as the
    /// operating system names it: known for an agent an earlier daemon
    /// started too.
    peer: Mutex<Option<u32>>,
    /// Whether the process this daemon starts is still starting or
    /// running. A new process has not taken its lock yet, so a free lock
    /// says nothing about it until it has exited.
    running: watch::Sender<bool>,
    /// Why the process could not be started, when it could not.
    failure: Mutex<Option<String>>,
    /// Whether a watcher dials the process.
    watching: AtomicBool,
    /// Set once the core recorded the process's exit.
    pub(crate) exited: watch::Sender<bool>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
}

impl AgentHandle {
    /// A handle on `dir`'s process, writing the core's frames in order, at
    /// most `patience` each. `starting` when this daemon is starting the
    /// process.
    fn new(dir: PathBuf, starting: bool, patience: Duration) -> Arc<Self> {
        let (writes, mut frames) = mpsc::unbounded_channel::<CtlFrame>();
        let handle = Arc::new(Self {
            dir,
            ctl: tokio::sync::Mutex::new(None),
            writes,
            replies: Mutex::new(HashMap::new()),
            next_write: std::sync::atomic::AtomicU64::new(0),
            dumps: Mutex::new(HashMap::new()),
            hello: watch::Sender::new(None),
            handed: std::sync::atomic::AtomicUsize::new(0),
            pid: Mutex::new(None),
            peer: Mutex::new(None),
            running: watch::Sender::new(starting),
            failure: Mutex::new(None),
            watching: AtomicBool::new(false),
            exited: watch::Sender::new(false),
            tasks: Mutex::new(Vec::new()),
        });
        let writer = Arc::downgrade(&handle);
        let task = tokio::spawn(async move {
            while let Some(frame) = frames.recv().await {
                let Some(handle) = writer.upgrade() else {
                    return;
                };
                // A frame that cannot be written is dropped: the core's
                // deadlines cover what it waited for.
                let _ = handle.write_ctl(&frame, patience).await;
            }
        });
        handle.tasks.lock().unwrap().push(task);
        handle
    }

    fn abort(&self) {
        for task in self.tasks.lock().unwrap().drain(..) {
            task.abort();
        }
    }

    /// Waits for the verdict on `input_id`, in place of any write of it
    /// still waiting, which is told it will get none: the number of this
    /// write.
    pub(crate) fn expect_reply(
        &self,
        input_id: Vec<u8>,
        reply: oneshot::Sender<Option<SendInputResponse>>,
    ) -> u64 {
        let write = self.next_write.fetch_add(1, Ordering::SeqCst);
        let replaced = self
            .replies
            .lock()
            .unwrap()
            .insert(input_id, (write, reply));
        if let Some((_, replaced)) = replaced {
            let _ = replaced.send(None);
        }
        write
    }

    /// Tells the write waiting for the verdict on `input_id` that it will
    /// get none.
    fn give_up_reply(&self, input_id: &[u8]) {
        let waiting = self.replies.lock().unwrap().remove(input_id);
        if let Some((_, waiting)) = waiting {
            let _ = waiting.send(None);
        }
    }

    /// Stops waiting for the verdict on `write`, unless a later write of
    /// the same input took its place.
    pub(crate) fn forget_reply(&self, input_id: &[u8], write: u64) {
        let mut replies = self.replies.lock().unwrap();
        if replies
            .get(input_id)
            .is_some_and(|(waiting, _)| *waiting == write)
        {
            replies.remove(input_id);
        }
    }

    pub(crate) fn expect_dump(&self, dump_id: Vec<u8>, part: oneshot::Sender<DumpPart>) {
        self.dumps.lock().unwrap().insert(dump_id, part);
    }

    pub(crate) fn forget_dump(&self, dump_id: &[u8]) {
        self.dumps.lock().unwrap().remove(dump_id);
    }

    /// Writes one frame on the control connection, waiting for the
    /// connection and the write together at most `patience`, so an agent
    /// that stopped reading never holds a caller. A write that overruns may
    /// have sent part of its frame and the agent is not reading anyway: the
    /// write half is shut down, which an agent that does read sees as the
    /// daemon going away, and the watcher dials it again.
    pub(crate) async fn write_ctl(&self, frame: &CtlFrame, patience: Duration) -> Sent {
        let written = tokio::time::timeout(patience, async {
            match self.ctl.lock().await.as_mut() {
                None => Sent::NoConnection,
                Some(ctl) => match agent_dir::write_frame(ctl, frame).await {
                    Ok(()) => Sent::Written,
                    Err(_) => Sent::Failed,
                },
            }
        })
        .await;
        match written {
            Ok(sent) => sent,
            Err(_) => {
                // Held by another writer that overran too: it closes it.
                if let Ok(mut ctl) = self.ctl.try_lock()
                    && let Some(mut writer) = ctl.take()
                {
                    let _ = writer.shutdown().await;
                }
                Sent::Failed
            }
        }
    }
}

/// What a write on an agent's control connection came to.
pub(crate) enum Sent {
    Written,
    /// No connection is open now.
    NoConnection,
    /// The write failed or overran; the frame may or may not have arrived.
    Failed,
}

impl Drop for ProfileRuntime {
    fn drop(&mut self) {
        for handle in self.agents.get_mut().unwrap().values() {
            handle.abort();
        }
        for task in self.background.get_mut().unwrap().drain(..) {
            task.abort();
        }
    }
}

/// What a profile runtime is made of.
pub struct Profile {
    pub profile: ProfileId,
    /// The installation's generation as of this run.
    pub generation: u64,
    /// The profile's directory.
    pub dir: PathBuf,
    /// The profile's store, open and migrated.
    pub store: Sqlite,
    pub launch: Launch,
    pub clock: Arc<dyn Clock>,
    pub push: Arc<dyn PushSender>,
    /// Where dumps are written: the installation's reports directory.
    pub reports: PathBuf,
    /// The daemon's own log, which a dump includes.
    pub daemon_log: Option<PathBuf>,
    /// Its supervisor's log, which a dump includes too, when it has one.
    pub supervisor_log: Option<PathBuf>,
    /// Where the profile records product analytics.
    pub analytics: analytics::Analytics,
    /// What makes the worktree an agent asked to start in one starts in.
    pub worktrees: Arc<dyn crate::worktree::MakeWorktree>,
}

impl ProfileRuntime {
    /// A runtime over an opened profile, its core thread started over the
    /// profile's store.
    pub fn open(profile: Profile) -> Arc<Self> {
        let Profile {
            profile,
            generation,
            dir,
            store,
            launch,
            clock,
            push,
            reports,
            daemon_log,
            supervisor_log,
            analytics,
            worktrees,
        } = profile;
        let options = daemon_core::StartOptions {
            generation,
            agents: dir.join(AGENTS),
            replicas: dir.join(crate::install::REPLICAS),
            budget: launch.step_budget,
        };
        let mut core = None;
        let runtime = Arc::new_cyclic(|me| Self {
            profile,
            generation,
            clock,
            driver: {
                let (driver, thread) = crate::driver::Driver::new(
                    me.clone(),
                    store,
                    dir.clone(),
                    options,
                    launch.dump_interval_ms,
                    launch.cores.clone(),
                );
                core = Some(thread);
                driver
            },
            join_hook: Mutex::new(None),
            commits: watch::Sender::new(0),
            ingested_frames: AtomicU64::new(0),
            push,
            background: Mutex::new(Vec::new()),
            core_dumps: Mutex::new(Some(Vec::new())),
            agent_dumps: Mutex::new(core_ring::DumpLimit::default()),
            launch: Mutex::new(launch),
            agents: Mutex::new(HashMap::new()),
            retention: watch::Sender::new(Retention::default()),
            reports,
            daemon_log,
            supervisor_log,
            worktrees,
            me: me.clone(),
            edge: std::sync::OnceLock::new(),
            replicas: Mutex::new(crate::replicas::Replicas::default()),
            replica_blobs: Mutex::new(None),
            hosts: Mutex::new(Vec::new()),
            offers: crate::offers::Offers::load(&dir),
            dir,
            analytics,
            turns: crate::telemetry::Turns::default(),
        });
        core.expect("built with the runtime").start(&runtime.driver);
        runtime.tell_own_host();
        runtime
    }

    /// Starts the profile's network edge: its identity and trust, and the
    /// listeners and links the options ask for. Once per runtime.
    pub async fn start_edge(
        self: &Arc<Self>,
        options: &crate::edge::EdgeOptions,
    ) -> Result<Arc<crate::edge::Edge>, crate::edge::EdgeError> {
        if let Some(edge) = self.edge.get() {
            return Ok(edge.clone());
        }
        let edge = crate::edge::Edge::start(
            &self.dir,
            self.profile,
            options,
            self.clock.clone(),
            Arc::downgrade(self),
            self.analytics.clone(),
        )
        .await?;
        let edge = self.edge.get_or_init(|| edge).clone();
        self.tell_own_host();
        Ok(edge)
    }

    /// The profile's network edge, once started.
    pub fn edge(&self) -> Option<Arc<crate::edge::Edge>> {
        self.edge.get().cloned()
    }

    /// Closes the edge's links with `reason` and stops serving the network.
    pub async fn stop_edge(&self, reason: wire::LinkCloseReason) {
        self.stop_replication();
        if let Some(edge) = self.edge.get() {
            edge.stop(reason).await;
        }
    }

    pub fn profile(&self) -> ProfileId {
        self.profile
    }

    /// Where this profile records product analytics.
    pub fn analytics(&self) -> &analytics::Analytics {
        &self.analytics
    }

    pub(crate) fn take_turns(&self) -> analytics::Counts<analytics::Kind> {
        self.turns.take()
    }

    /// The profile's host id, the one its peers know it by: its own id.
    pub fn host(&self) -> Uuid {
        self.profile
    }

    /// The installation's generation as of this run's start.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Wakes on every item write to the store: an own commit or a replica
    /// absorb, catch-up or live. Markers, agent rows, deliveries,
    /// notifications and the retention sweeps do not wake it. A waiter
    /// reads the store after each change; the count itself only orders
    /// them.
    pub fn committed(&self) -> watch::Receiver<u64> {
        self.commits.subscribe()
    }

    /// Notes one write to the store for [`ProfileRuntime::committed`].
    pub(crate) fn wrote(&self) {
        self.commits.send_modify(|writes| *writes += 1);
    }

    /// How many journal frames ingest has committed since this runtime
    /// started.
    pub fn ingested_frames(&self) -> u64 {
        self.ingested_frames.load(Ordering::Relaxed)
    }

    /// The profile's core, from the shell's side.
    pub(crate) fn driver(&self) -> &crate::driver::Driver {
        &self.driver
    }

    /// Reads the store on the core thread, between steps: one point in the
    /// order of the core's writes.
    pub async fn read_store<T: Send + 'static>(
        &self,
        read: impl FnOnce(&Sqlite) -> T + Send + 'static,
    ) -> Result<T, Stopped> {
        self.driver.read(read).await.ok_or(Stopped)
    }

    /// Reads rows on the core thread, as [`ProfileRuntime::read_store`]
    /// does; once the core has stopped the store reads as closed.
    pub(crate) async fn read_rows<T: Send + 'static>(
        &self,
        read: impl FnOnce(&Sqlite) -> Result<T, StoreError> + Send + 'static,
    ) -> Result<T, StoreError> {
        self.read_store(read)
            .await
            .unwrap_or(Err(StoreError::Closed))
    }

    /// For tests: writes the store beneath the core's own steps, between
    /// them, to set up what no input produces.
    #[doc(hidden)]
    pub async fn write_store_beneath<T: Send + 'static>(
        &self,
        write: impl FnOnce(&mut Sqlite) -> T + Send + 'static,
    ) -> Result<T, Stopped> {
        self.driver.beneath(write).await.ok_or(Stopped)
    }

    /// For tests: runs `hook` inside every subscribe between joining the
    /// agent's channel and reading the cut, so a test can try to commit in
    /// that window.
    #[doc(hidden)]
    pub fn set_join_hook(&self, hook: Option<JoinHook>) {
        *self.join_hook.lock().unwrap() = hook;
    }

    /// Replaces what the next spawn or resume starts with.
    pub fn set_launch(&self, launch: Launch) {
        *self.launch.lock().unwrap() = launch;
    }

    pub fn launch(&self) -> Launch {
        self.launch.lock().unwrap().clone()
    }

    /// The profile's directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub(crate) fn offers(&self) -> &crate::offers::Offers {
        &self.offers
    }

    pub fn agent_dir(&self, id: AgentId) -> PathBuf {
        self.dir.join(AGENTS).join(id.to_string())
    }

    pub(crate) fn clock(&self) -> &Arc<dyn Clock> {
        &self.clock
    }

    pub(crate) fn clock_now(&self) -> i64 {
        self.clock.now_ms()
    }

    pub(crate) fn handle(&self, id: AgentId) -> Option<Arc<AgentHandle>> {
        self.agents.lock().unwrap().get(&id).cloned()
    }

    pub(crate) fn key(&self, id: AgentId) -> AgentKey {
        AgentKey::new(self.host().as_bytes().to_vec(), id.as_bytes().to_vec())
    }

    /// The inventory row for an agent.
    pub async fn agent(&self, id: AgentId) -> Result<Agent, RegistryError> {
        let key = self.key(id);
        let row = self
            .read_rows(move |store| store.agent(&key))
            .await?
            .ok_or(RegistryError::NotFound(id))?;
        Ok(to_wire(&row))
    }

    /// Own agents whose process this runtime is connected to or waiting on.
    pub fn live(&self) -> Vec<AgentId> {
        self.agents.lock().unwrap().keys().copied().collect()
    }

    /// The Hello of an agent's current connection, once it has one.
    pub fn hello(&self, id: AgentId) -> Option<AgentHello> {
        let handle = self.agents.lock().unwrap().get(&id).cloned()?;
        handle.hello.borrow().clone()
    }

    /// Marks a live agent as leaving: it refused an input as exiting, so a
    /// resume waits for its lock instead of refusing it as live.
    pub fn mark_exiting(&self, id: AgentId) {
        self.driver().post(daemon_core::Input::Exiting {
            agent: core_agent(id),
        });
    }

    /// Hands a registry call to the core and waits for its answer: the
    /// agent's row, or nothing, as a delete answers.
    async fn registry(
        &self,
        id: AgentId,
        request: ClientRequest,
    ) -> Result<Option<Agent>, RegistryError> {
        let mut answers = self.driver().call(Ask::Ready(request), 1).await;
        let answered = match answers.next().await {
            Some(Delivered::Answer(Response::Agent(agent))) => Some(*agent),
            Some(Delivered::Answer(other)) => {
                return Err(RegistryError::Core(format!(
                    "a registry call was answered with {other:?}"
                )));
            }
            Some(Delivered::End(Status::Ok)) => return Ok(None),
            Some(Delivered::End(Status::Failed(error))) => return Err(registry_error(id, error)),
            None => {
                return Err(RegistryError::Core(
                    "the profile is no longer running".into(),
                ));
            }
        };
        // Its end follows its answer at once; taken, so letting the call go
        // tells the core nothing.
        let _ = answers.next().await;
        Ok(answered)
    }

    /// A registry call answered with the agent's row.
    async fn registry_row(
        &self,
        id: AgentId,
        request: ClientRequest,
    ) -> Result<Agent, RegistryError> {
        self.registry(id, request)
            .await?
            .ok_or_else(|| RegistryError::Core("a registry call was answered with no row".into()))
    }

    // --- spawn and resume ------------------------------------------------

    /// Creates an agent: its row, its directory and `spec.1`, its tools
    /// socket, and its process. `caller` is the agent whose tools socket
    /// the request arrived on; it becomes the parent. Returns once the
    /// process has sent its Hello, or has already exited.
    pub async fn spawn(
        &self,
        request: CreateAgentRequest,
        caller: Option<AgentId>,
    ) -> Result<Agent, RegistryError> {
        let id = match request.agent_id.len() {
            0 => Uuid::new_v4(),
            16 => Uuid::from_slice(&request.agent_id).expect("sixteen bytes"),
            n => return Err(RegistryError::BadId(n)),
        };
        if self.target_host(&request)? != self.host() {
            return Err(RegistryError::OtherHost);
        }
        let Some(agent) = &request.agent else {
            return Err(RegistryError::NoKind);
        };
        let kind = agent.kind();
        if let Some(edge) = self.edge() {
            self.runs_kind(&edge.capabilities(), edge.host_name(), kind)?;
        }
        let resolved = spec::resolve(agent);
        let mut request = request;
        let bootstrap = request.initial_prompt.take().map(spec::prompt_bootstrap);
        if request.cwd.is_empty()
            && let Some(caller) = caller
        {
            // One of this host's agents spawning here, whatever name it gave
            // this host, starts its child where it works itself.
            request.cwd = self
                .agent(caller)
                .await
                .map(|parent| parent.cwd)
                .unwrap_or_default();
        }
        if !Path::new(&request.cwd).is_dir() {
            return Err(RegistryError::BadCwd(request.cwd));
        }
        let parent = match caller {
            Some(caller) => Some(AgentRef {
                host_id: self.host().as_bytes().to_vec(),
                agent_id: caller.as_bytes().to_vec(),
            }),
            None => request.parent.clone(),
        };

        let given = request.name.clone().filter(|name| !name.is_empty());
        if let Some(problem) = given.as_deref().and_then(wire::agent_name_problem) {
            return Err(RegistryError::BadName(problem));
        }
        let branches = match given {
            Some(_) => Default::default(),
            None => crate::names::branch_names(Path::new(&request.cwd)).await,
        };

        let launch = self.launch();
        let spec = spec::build(
            &launch,
            spec::Incarnation {
                agent_id: id.as_bytes(),
                profile_id: self.profile.as_bytes(),
                kind,
                cwd: &request.cwd,
                name: "",
                parent,
                resolved: &resolved,
                created_at_ms: 0,
                incarnation: 1,
                bootstrap,
                offered: None,
                base_branch: None,
            },
        );
        let recent = Path::new(&request.cwd)
            .canonicalize()
            .ok()
            .and_then(|path| path.to_str().map(str::to_owned));
        let create = daemon_core::Create {
            agent: core_agent(id),
            cwd: request.cwd,
            given,
            branches: branches.into_iter().collect(),
            new_worktree: request.new_worktree,
            recent,
            spec,
        };
        self.registry_row(id, ClientRequest::CreateAgent(Box::new(create)))
            .await
    }

    /// Where this host offers to start an agent: see
    /// [`crate::repositories::list`].
    pub async fn list_repositories(
        &self,
        query: Option<String>,
        limit: u32,
    ) -> wire::ListRepositoriesResponse {
        let roots = self.launch().repository_roots;
        let recent = self
            .driver()
            .read(|store| store.recent_directories())
            .await
            .and_then(|read| {
                read.inspect_err(
                    |error| tracing::warn!(%error, "cannot read the recent directories"),
                )
                .ok()
            })
            .unwrap_or_default();
        tokio::task::spawn_blocking(move || {
            crate::repositories::list(&roots, &recent, query.as_deref(), limit)
        })
        .await
        .expect("listing repositories does not panic")
    }

    /// An agent's unsent draft, as this machine's clients last saved it.
    pub async fn draft(&self, agent_id: &[u8]) -> wire::Draft {
        let request = ClientRequest::GetDraft {
            agent_id: agent_id.to_vec(),
        };
        let mut answers = self.driver().call(Ask::Ready(request), 1).await;
        match answers.next().await {
            Some(Delivered::Answer(Response::Draft(draft))) => draft,
            _ => wire::Draft::default(),
        }
    }

    /// Keeps an agent's unsent draft for this machine's clients; an empty
    /// one forgets it.
    pub async fn update_draft(&self, agent_id: &[u8], draft: &wire::Draft) {
        let request = ClientRequest::UpdateDraft {
            agent_id: agent_id.to_vec(),
            draft: draft.clone(),
        };
        let mut answers = self.driver().call(Ask::Ready(request), 1).await;
        let _ = answers.next().await;
    }

    /// The host a create request is for: the one it names, by id or by name
    /// resolved against this host and the trusted ones, else this host.
    pub fn target_host(&self, request: &CreateAgentRequest) -> Result<HostId, RegistryError> {
        use wire::host_ref::Of;
        match request.host.as_ref().and_then(|host| host.of.as_ref()) {
            Some(Of::HostId(host)) => {
                Uuid::from_slice(host).map_err(|_| RegistryError::BadId(host.len()))
            }
            Some(Of::Name(name)) if !name.is_empty() => Ok(self.resolve_host(name)?),
            _ => Ok(self.host()),
        }
    }

    /// Creates an agent on another host, forwarding the request to that
    /// host's daemon. `caller`, the agent whose tools socket the request
    /// came in on, becomes the parent, named by this host and its id.
    pub async fn spawn_on(
        &self,
        host: HostId,
        request: CreateAgentRequest,
        caller: Option<AgentId>,
    ) -> Result<Agent, RegistryError> {
        // A host that announced which kinds it runs is not asked to start
        // another: a phone runs none.
        if let Some(entry) = self.host_entry_of(host)
            && let Some(capabilities) = entry
                .host
                .as_ref()
                .and_then(|host| host.capabilities.as_ref())
            && let Some(kind) = request.agent.as_ref().map(|agent| agent.kind())
        {
            self.runs_kind(capabilities, entry.name(), kind)?;
        }
        // No folder there is a default worth guessing: the caller asks the
        // person or names a path it knows on that host.
        if request.cwd.is_empty() {
            return Err(RegistryError::NoFarCwd(self.host_name(host)));
        }
        let mut request = request;
        if let Some(caller) = caller {
            request.parent = Some(AgentRef {
                host_id: self.host().as_bytes().to_vec(),
                agent_id: caller.as_bytes().to_vec(),
            });
        }
        request.host = Some(wire::HostRef::id(host.as_bytes().to_vec()));
        if request.agent_id.is_empty() {
            request.agent_id = Uuid::new_v4().as_bytes().to_vec();
        }
        Ok(self.on_peer(host, request).await?)
    }

    /// Refuses a kind `capabilities` do not list, in words naming `host`.
    fn runs_kind(
        &self,
        capabilities: &wire::Capabilities,
        host: &str,
        kind: Kind,
    ) -> Result<(), RegistryError> {
        if capabilities.kinds.contains(&(kind as i32)) {
            return Ok(());
        }
        Err(RegistryError::KindNotRun {
            host: host.to_owned(),
            kind,
            runs_none: capabilities.kinds.is_empty(),
        })
    }

    /// Starts an exited agent's next incarnation, its queue seeded by
    /// `bootstrap` when one is given: the core waits for a dying process to
    /// release the directory, and `spec.<n+1>` is written from the current
    /// configuration and the settings the agent last ran with.
    pub async fn resume(
        &self,
        id: AgentId,
        bootstrap: Option<Bootstrap>,
    ) -> Result<Agent, RegistryError> {
        // An agent the store does not hold is the core's to refuse; the
        // core asks for the next spec once the previous process has ended.
        let resume = daemon_core::Resume {
            agent: core_agent(id),
            bootstrap,
        };
        self.registry_row(id, ClientRequest::ResumeAgent(Box::new(resume)))
            .await
    }

    /// The agent's next spec, from its newest: the current configuration,
    /// the settings its newest `snapshot` body says it last ran with, and
    /// `bootstrap` to seed its queue. None when it has no spec.
    pub(crate) fn next_spec(
        &self,
        id: AgentId,
        bootstrap: Option<Bootstrap>,
        snapshot: Option<&[u8]>,
    ) -> Result<Option<AgentSpec>, RegistryError> {
        let previous = match spec::newest(&self.agent_dir(id)) {
            Ok(previous) => previous,
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        let Some(previous) = previous else {
            return Ok(None);
        };
        let latest = last_settings(snapshot, previous.kind());
        let resolved = spec::carry(&previous, latest);
        Ok(Some(spec::build(
            &self.launch(),
            spec::Incarnation {
                agent_id: id.as_bytes(),
                profile_id: self.profile.as_bytes(),
                kind: previous.kind(),
                cwd: &previous.cwd,
                name: "",
                parent: previous.parent.clone(),
                resolved: &resolved,
                created_at_ms: 0,
                incarnation: previous.incarnation + 1,
                bootstrap,
                offered: None,
                base_branch: previous.base_branch.clone(),
            },
        )))
    }

    /// What the host's provider offers, for a kind that cannot ask its
    /// provider itself: terminal Claude takes what the host's Claude offers
    /// in the folder the agent works in, which includes the project's own
    /// commands.
    async fn offered_to(&self, kind: Kind, cwd: &str) -> Option<wire::Catalogue> {
        if kind != Kind::ClaudePty {
            return None;
        }
        self.folder_catalogue(crate::Provider::Claude, Path::new(cwd))
            .await
            .ok()
            .map(|offered| offered.catalogue)
    }

    /// Starts the process the core asked for: the handle at once, so the
    /// core's next outputs find it, and the spec, tools socket and process
    /// on a task of their own. A process that cannot be started leaves its
    /// handle unlocked and not running, which its watcher reports as gone.
    pub(crate) fn spawn_process(&self, dir: PathBuf, spec: AgentSpec) {
        let Ok(id) = Uuid::from_slice(&spec.agent_id) else {
            return;
        };
        let handle = AgentHandle::new(dir.clone(), true, ms(self.launch().ctl_write_ms));
        if let Some(previous) = self.agents.lock().unwrap().insert(id, handle.clone()) {
            previous.abort();
        }
        let runtime = self.me.clone();
        let starting = handle.clone();
        let task = tokio::spawn(async move {
            let Some(me) = runtime.upgrade() else {
                return;
            };
            if let Err(error) = me.start_process(id, &dir, spec, &starting).await {
                *starting.failure.lock().unwrap() = Some(error.to_string());
                starting.running.send_replace(false);
            }
        });
        handle.tasks.lock().unwrap().push(task);
    }

    /// Writes the spec, binds the tools socket and spawns `amux agent
    /// <dir>`. A terminal Claude spec takes what the host's Claude offers
    /// in the folder the agent works in.
    async fn start_process(
        &self,
        id: AgentId,
        dir: &Path,
        mut spec: AgentSpec,
        handle: &Arc<AgentHandle>,
    ) -> io::Result<()> {
        if spec.offered.is_none() {
            spec.offered = self.offered_to(spec.kind(), &spec.cwd).await;
        }
        private_dir(dir)?;
        spec::write(dir, &spec)?;
        let tools = self.bind_tools(id, dir)?;
        handle.tasks.lock().unwrap().push(tools);
        let child = spawn_agent(&self.launch().install_path, dir)?;
        *handle.pid.lock().unwrap() = child.id();
        let running = handle.running.clone();
        handle.tasks.lock().unwrap().push(tokio::spawn(async move {
            reap(child).await;
            running.send_replace(false);
        }));
        Ok(())
    }

    /// Dials the agent's process, if no watcher does already.
    pub(crate) fn dial_agent(&self, id: AgentId) {
        let Some(handle) = self.handle(id) else {
            return;
        };
        self.watch(id, handle, None);
    }

    /// Writes a frame the core asked for on the agent's control connection,
    /// in order with the ones before it.
    pub(crate) fn write_control(&self, id: AgentId, frame: CtlFrame) {
        if let Some(handle) = self.handle(id) {
            let _ = handle.writes.send(frame);
        }
    }

    /// Writes a message the core handed to the agent, and reports the
    /// interpreter's verdict on it to the core, or that the connection
    /// ended before one came. The message is in hand until then, or until
    /// the core gives up on its verdict (its reply patience runs on the
    /// policy clock) or writes it again, when the later write waits afresh.
    pub(crate) fn write_message(
        &self,
        id: AgentId,
        frame: CtlFrame,
        inputs: crate::driver::WeakFeed,
    ) {
        let agent = core_agent(id);
        let input_id = input_id_of(&frame);
        let report = move |verdict| {
            let input_id = input_id.clone();
            let inputs = inputs.clone();
            async move {
                if let Some(inputs) = inputs.upgrade() {
                    let verdict = daemon_core::Input::Verdict {
                        agent,
                        input_id,
                        verdict,
                    };
                    let _ = inputs.send(crate::driver::Fed::Input(verdict)).await;
                }
            }
        };
        let Some(handle) = self.handle(id) else {
            tokio::spawn(report(None));
            return;
        };
        let (tx, rx) = oneshot::channel();
        handle.expect_reply(input_id_of(&frame), tx);
        let _ = handle.writes.send(frame);
        handle.handed.fetch_add(1, Ordering::SeqCst);
        tokio::spawn(async move {
            let waited = rx.await;
            handle.handed.fetch_sub(1, Ordering::SeqCst);
            match waited {
                Ok(Some(verdict)) => report(Some(verdict)).await,
                // The core gave up on this write, or a later write of the
                // message waits in its place and is answered on its own.
                Ok(None) => {}
                Err(_) => report(None).await,
            }
        });
    }

    /// The core gave up on the verdict for the message it wrote under
    /// `input_id`: its write stops waiting and reports nothing.
    pub(crate) fn forget_verdict(&self, id: AgentId, input_id: &[u8]) {
        if let Some(handle) = self.handle(id) {
            handle.give_up_reply(input_id);
        }
    }

    /// The core recorded the agent's exit: its handle goes, and with it the
    /// tools socket, and whoever waits on it is told.
    pub(crate) fn forget_agent(&self, id: AgentId) {
        let Some(handle) = self.agents.lock().unwrap().remove(&id) else {
            return;
        };
        handle.abort();
        // Its connection's watcher is gone with its tasks: writes still
        // waiting for a verdict learn here that none will come.
        handle.replies.lock().unwrap().clear();
        let _ = std::fs::remove_file(handle.dir.join(agent_dir::TOOLS_SOCK));
        handle.exited.send_replace(true);
    }

    /// Makes the worktree a create asked for, as the core's job.
    pub(crate) async fn make_worktree(
        &self,
        repo: &Path,
        name: &str,
    ) -> daemon_core::payload::JobResult {
        use daemon_core::payload::JobResult;
        match self.worktrees.make(repo, name).await {
            Ok(made) => JobResult::Worktree {
                path: made.path.to_string_lossy().into_owned(),
                base_branch: made.base_branch,
            },
            Err(error) => JobResult::Refused(RegistryError::Worktree(error).to_wire()),
        }
    }

    /// Kills the agent's process group, as the core's job.
    pub(crate) fn kill_process(&self, id: AgentId) -> bool {
        self.handle(id).is_some_and(|handle| self.kill(&handle))
    }

    /// The handle for an agent an earlier daemon started, serving its
    /// tools socket again: the agent's MCP server dials it for every fleet
    /// tool, and the earlier daemon's listener went with that daemon. An
    /// agent whose socket cannot be bound keeps running without its fleet
    /// tools, as it did before, and says so in the log.
    fn adopted_handle(&self, id: AgentId) -> Arc<AgentHandle> {
        let dir = self.agent_dir(id);
        let handle = AgentHandle::new(dir.clone(), false, ms(self.launch().ctl_write_ms));
        match self.bind_tools(id, &dir) {
            Ok(tools) => handle.tasks.lock().unwrap().push(tools),
            Err(error) => {
                tracing::warn!(agent = %id, %error, "serving an adopted agent's tools failed");
            }
        }
        handle
    }

    /// Serves the client service on `<dir>/tools.sock` for the agent's MCP
    /// server. The socket is its caller's identity: whatever arrives on it
    /// is from `id`.
    fn bind_tools(&self, id: AgentId, dir: &Path) -> io::Result<JoinHandle<()>> {
        let listener = LocalListener::bind(&dir.join(agent_dir::TOOLS_SOCK))?;
        Ok(crate::grpc::serve_client(
            listener,
            ClientApi::weak(self.me.clone(), Some(id)),
            None,
        ))
    }

    // --- stop, delete, rename --------------------------------------------

    /// Stops a live agent's process now: the running turn is cancelled the
    /// provider's own way and the agent exits, resumable. A stop that
    /// overruns the stop deadline kills. An exited agent is left as it is.
    ///
    /// The stop acts on this agent alone. Its children, on this host or
    /// another, follow by the family rule once their host sees it exited.
    pub async fn stop(&self, id: AgentId) -> Result<Agent, RegistryError> {
        let agent = core_agent(id);
        self.registry_row(id, ClientRequest::StopAgent { agent })
            .await
    }

    /// Kills a live agent's process group at once, writing nothing more,
    /// as the stop deadline does. For tearing a runtime down: no client
    /// call reaches it.
    pub async fn kill_agent(&self, id: AgentId) -> Result<Agent, RegistryError> {
        let agent = core_agent(id);
        self.registry_row(id, ClientRequest::KillAgent { agent })
            .await
    }

    /// Deletes an agent: stops its process if live, then removes its rows
    /// and its directory. Its children, on this host or another, follow by
    /// the family rule once their host sees it gone. A delete whose caller
    /// goes away still finishes.
    pub async fn delete(&self, id: AgentId) -> Result<(), RegistryError> {
        let agent = core_agent(id);
        self.registry(id, ClientRequest::DeleteAgent { agent })
            .await
            .map(drop)
    }

    /// Renames an agent. The row takes the name now and the next
    /// incarnation's spec when it is resumed; a spec already written keeps
    /// the name it was spawned with. A live Codex agent is told, so its
    /// thread takes the name.
    pub async fn rename(&self, id: AgentId, name: &str) -> Result<Agent, RegistryError> {
        if let Some(problem) = wire::agent_name_problem(name) {
            return Err(RegistryError::BadName(problem));
        }
        let agent = core_agent(id);
        let name = name.to_owned();
        self.registry_row(id, ClientRequest::RenameAgent { agent, name })
            .await
    }

    // --- the sweep ---------------------------------------------------------

    /// The read-only half of the sweep: try the lock of every agent
    /// directory, dial the live ones and read their Hello. Writes nothing.
    pub async fn look(&self) -> Result<Looked, RegistryError> {
        let rows: HashMap<AgentId, AgentRow> = self
            .read_rows(|store| store.agents())
            .await?
            .into_iter()
            .filter(|row| row.agent.host == self.host().as_bytes())
            .filter_map(|row| Some((Uuid::from_slice(&row.agent.agent).ok()?, row)))
            .collect();
        let mut dirs = HashSet::new();
        let agents_dir = self.dir.join(AGENTS);
        match std::fs::read_dir(&agents_dir) {
            Ok(entries) => {
                for entry in entries {
                    let entry = entry?;
                    if let Some(id) = entry
                        .file_name()
                        .to_str()
                        .and_then(|name| Uuid::parse_str(name).ok())
                        && entry.file_type()?.is_dir()
                    {
                        dirs.insert(id);
                    }
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }

        let mut agents = Vec::new();
        for &id in rows.keys() {
            if !dirs.contains(&id) {
                agents.push((id, Look::NoDirectory));
                continue;
            }
            let dir = self.agent_dir(id);
            let look = match look_once(&dir, LOOK_PATIENCE).await {
                Probe::Connected(connected) => Look::Live(connected),
                Probe::Unlocked => Look::Exited,
                Probe::Waiting => Look::Unanswered,
            };
            agents.push((id, look));
        }
        let orphans = dirs
            .into_iter()
            .filter(|id| !rows.contains_key(id))
            .collect();
        Ok(Looked { agents, orphans })
    }

    /// The writing half of the sweep, after activation: adopts the live
    /// agents and ingests from their stored cursors, has the core mark the
    /// rest exited once their journal remainder is ingested and set their
    /// CaughtUp flag, and removes directories no row lists.
    pub async fn finish_sweep(&self, looked: Looked) -> Result<SweepReport, RegistryError> {
        let mut report = SweepReport::default();
        for (id, look) in looked.agents {
            let agent = core_agent(id);
            match look {
                Look::Live(connected) => {
                    let handle = self.adopted_handle(id);
                    self.agents.lock().unwrap().insert(id, handle.clone());
                    let reader = self.adopt(id, &handle, *connected).await;
                    self.watch(id, handle, Some(reader));
                    report.live.push(id);
                }
                Look::Unanswered => {
                    // Its journal is read now rather than at its Hello, so
                    // every journal has been read once before messages and
                    // deliveries run.
                    self.ingest(id).await?;
                    let handle = self.adopted_handle(id);
                    self.agents.lock().unwrap().insert(id, handle);
                    self.driver()
                        .feed(daemon_core::Input::Adopted { agent })
                        .await;
                    report.unanswered.push(id);
                }
                Look::Exited => {
                    // Unless its journal said why, it ended while no daemon
                    // was there to see. The core marks it once its journal
                    // is read to the end, which the ingest waits for.
                    let ended = daemon_core::Ended::Away;
                    self.driver()
                        .feed(daemon_core::Input::ControlClosed { agent, ended })
                        .await;
                    self.ingest(id).await?;
                    report.exited.push(id);
                }
                Look::NoDirectory => {
                    let ended = daemon_core::Ended::NoDirectory;
                    self.driver()
                        .feed(daemon_core::Input::ControlClosed { agent, ended })
                        .await;
                    // There is no journal to read; the ingest waits for the
                    // core to have marked it.
                    let _ = self.ingest(id).await;
                    report.exited.push(id);
                }
            }
        }
        for id in looked.orphans {
            match std::fs::remove_dir_all(self.agent_dir(id)) {
                Ok(()) => report.removed.push(id),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        for list in [
            &mut report.live,
            &mut report.unanswered,
            &mut report.exited,
            &mut report.removed,
        ] {
            list.sort();
        }
        self.driver().feed(daemon_core::Input::Swept).await;
        Ok(report)
    }

    /// Starts the background work: the retention sweep now and then on its
    /// interval, and the analytics check-ins.
    pub fn start_background(&self) {
        let retention = self.start_retention();
        self.background.lock().unwrap().push(retention);
        if self.analytics.is_on() {
            let check_ins = self.start_check_ins();
            self.background.lock().unwrap().push(check_ins);
        }
    }

    /// Records the profile's check-in once a day while it runs.
    fn start_check_ins(&self) -> JoinHandle<()> {
        let runtime = self.me.clone();
        let clock = self.clock.clone();
        let mut due = crate::telemetry::next_check_in(
            crate::telemetry::last_check_in(&self.dir),
            clock.now_ms(),
        );
        tokio::spawn(async move {
            loop {
                clock.sleep_until(due).await;
                let Some(me) = runtime.upgrade() else { return };
                let check_in = me.check_in().await;
                me.analytics.record(analytics::Event::CheckedIn(check_in));
                let now = clock.now_ms();
                crate::telemetry::mark_checked_in(&me.dir, now);
                due = now + crate::telemetry::CHECK_IN_EVERY_MS;
            }
        })
    }

    /// Stops every task of this run without waiting, as a crash would:
    /// background work, agent watchers, sources and the edge. The agents
    /// keep running.
    pub fn abort_all(&self) {
        for task in self.background.lock().unwrap().drain(..) {
            task.abort();
        }
        for task in self.core_dumps.lock().unwrap().take().into_iter().flatten() {
            task.abort();
        }
        for handle in self.agents.lock().unwrap().values() {
            for task in handle.tasks.lock().unwrap().drain(..) {
                task.abort();
            }
        }
        self.stop_replication();
        if let Some(edge) = self.edge.get() {
            edge.abort();
        }
    }

    /// Lets the dumps the core asked for finish within
    /// [`DUMPS_FINISH_WITHIN`], aborts the rest, and starts no more; returns
    /// once none is left, so none holds the runtime past a shutdown.
    pub async fn finish_dumps(&self) {
        let tasks = self.core_dumps.lock().unwrap().take().unwrap_or_default();
        let deadline = tokio::time::Instant::now() + DUMPS_FINISH_WITHIN;
        for mut task in tasks {
            if tokio::time::timeout_at(deadline, &mut task).await.is_err() {
                task.abort();
                let _ = task.await;
            }
        }
    }

    /// Stops the background work.
    pub async fn stop_background(&self) {
        let tasks: Vec<_> = self.background.lock().unwrap().drain(..).collect();
        for task in tasks {
            task.abort();
            let _ = task.await;
        }
    }

    /// Stops every watcher and closes every control connection, so nothing
    /// writes the store any more. The agents keep running.
    pub async fn stop_watching(&self) {
        let handles: Vec<_> = self
            .agents
            .lock()
            .unwrap()
            .drain()
            .map(|(_, h)| h)
            .collect();
        for handle in handles {
            let tasks: Vec<_> = handle.tasks.lock().unwrap().drain(..).collect();
            for task in tasks {
                task.abort();
                let _ = task.await;
            }
            handle.ctl.lock().await.take();
        }
    }

    // --- ingest --------------------------------------------------------------

    /// Asks the core to read the agent's journal from its row's cursor to
    /// the end, and waits until it has: a batch at a time, each committed
    /// in one transaction and then told to its subscribers. At the end of
    /// the journal, the first time after each Hello, the core tells them
    /// CaughtUp. Segments that lie wholly below the cursor of the core's
    /// last checkpoint on the drive are deleted, but for the newest few.
    pub async fn ingest(&self, id: AgentId) -> Result<Ingested, RegistryError> {
        match self.driver().ingest(id).await {
            Ok(frames) => Ok(Ingested { frames }),
            Err(why) => Err(RegistryError::Ingest(why)),
        }
    }

    /// Takes a checkpoint of the profile's core now, at a passive WAL
    /// checkpoint of its store, and waits for it to be on the drive: the
    /// journal below every cursor it holds may then go. Joins a checkpoint
    /// already being taken.
    pub async fn checkpoint(&self) -> Result<(), String> {
        self.driver().checkpoint().await
    }

    /// Counts what one ingest step committed.
    pub(crate) fn ingested(&self, report: &daemon_core::Ingested) {
        if report.frames == 0 {
            return;
        }
        self.wrote();
        self.ingested_frames
            .fetch_add(u64::from(report.frames), Ordering::Relaxed);
        if report.turn_ends > 0
            && self.analytics.is_on()
            && let Some(kind) = crate::telemetry::kind_coded(report.kind)
        {
            self.turns.add(kind, report.turn_ends);
        }
    }

    // --- the watcher ---------------------------------------------------------

    /// Takes a connection as the agent's current one: records its Hello,
    /// keeps its write half for the core's frames and inputs, tells the
    /// core, and ingests what the journal holds beyond the stored cursor.
    /// Returns the read half.
    async fn adopt(
        &self,
        id: AgentId,
        handle: &AgentHandle,
        connected: Connected,
    ) -> ReadHalf<LocalStream> {
        let Connected {
            reader,
            writer,
            hello,
            peer,
        } = connected;
        *handle.peer.lock().unwrap() = peer;
        *handle.ctl.lock().await = Some(writer);
        handle.hello.send_replace(Some(hello.clone()));
        self.driver()
            .feed(daemon_core::Input::AgentHello {
                agent: core_agent(id),
                hello,
            })
            .await;
        if let Err(error) = self.ingest(id).await {
            tracing::warn!(agent = %id, %error, "ingest failed");
        }
        reader
    }

    /// Runs the agent's watcher, unless one runs already: from `reader` if
    /// a connection is already adopted, else from the first successful
    /// dial. Once the process is gone, the core hears of it.
    fn watch(&self, id: AgentId, handle: Arc<AgentHandle>, reader: Option<ReadHalf<LocalStream>>) {
        if handle.watching.swap(true, Ordering::SeqCst) {
            return;
        }
        let runtime = self.me.clone();
        let watched = handle.clone();
        let task = tokio::spawn(async move {
            let mut adopted = reader;
            loop {
                let mut reader = match adopted.take() {
                    Some(reader) => reader,
                    None => {
                        let Some(connected) = connect(&watched.dir, &watched.running).await else {
                            break;
                        };
                        let Some(me) = runtime.upgrade() else { return };
                        me.adopt(id, &watched, connected).await
                    }
                };
                loop {
                    match agent_dir::read_frame(&mut reader).await {
                        Ok(Some(CtlFrame {
                            of: Some(ctl_frame::Of::Nudge(_)),
                        })) => {
                            let Some(me) = runtime.upgrade() else { return };
                            if let Err(error) = me.ingest(id).await {
                                tracing::warn!(agent = %id, %error, "ingest failed");
                            }
                        }
                        Ok(Some(CtlFrame {
                            of: Some(ctl_frame::Of::Reply(reply)),
                        })) => {
                            let waiting = watched.replies.lock().unwrap().remove(&reply.id);
                            if let (Some((_, waiting)), Some(verdict)) = (waiting, reply.verdict) {
                                let _ = waiting.send(Some(verdict));
                            }
                        }
                        Ok(Some(CtlFrame {
                            of: Some(ctl_frame::Of::DumpAsked(asked)),
                        })) => {
                            let Some(me) = runtime.upgrade() else { return };
                            me.agent_asked(id, asked);
                        }
                        Ok(Some(CtlFrame {
                            of: Some(ctl_frame::Of::Dump(part)),
                        })) => {
                            let waiting = watched.dumps.lock().unwrap().remove(&part.dump_id);
                            if let Some(waiting) = waiting {
                                let _ = waiting.send(part);
                            }
                        }
                        Ok(Some(_)) => {}
                        Ok(None) | Err(_) => break,
                    }
                }
                // Inputs and dumps sent on this connection get no answer
                // now: their senders learn the answer was lost.
                watched.replies.lock().unwrap().clear();
                watched.dumps.lock().unwrap().clear();
                if let Some(mut ctl) = watched.ctl.lock().await.take() {
                    let _ = ctl.shutdown().await;
                }
            }
            let ended = match watched.failure.lock().unwrap().take() {
                Some(words) => daemon_core::Ended::CouldNotStart(words),
                None => daemon_core::Ended::Exited,
            };
            if let Some(me) = runtime.upgrade() {
                me.driver()
                    .feed(daemon_core::Input::ControlClosed {
                        agent: core_agent(id),
                        ended,
                    })
                    .await;
            }
        });
        handle.tasks.lock().unwrap().push(task);
    }

    // --- waiting on processes ----------------------------------------------

    /// Waits until no process holds the directory's lock, at most
    /// `deadline_ms` on the profile's clock.
    pub(crate) async fn wait_unlocked(&self, dir: &Path, deadline_ms: i64) -> Result<(), ()> {
        let deadline = self.clock.sleep_until(self.clock.now_ms() + deadline_ms);
        tokio::pin!(deadline);
        loop {
            if !locked(dir) {
                return Ok(());
            }
            tokio::select! {
                () = tokio::time::sleep(PROBE_INTERVAL) => {}
                () = &mut deadline => return Err(()),
            }
        }
    }

    /// Kills the agent's process group: the process this daemon started,
    /// or the one on the other end of its control socket. Returns whether
    /// it could.
    fn kill(&self, handle: &AgentHandle) -> bool {
        let pid = *handle.pid.lock().unwrap();
        let Some(pid) = pid.or(*handle.peer.lock().unwrap()) else {
            return false;
        };
        // An agent served from inside this process has no process of its
        // own to end, and killing the one on the other end of its socket
        // would end the daemon instead.
        if pid == std::process::id() {
            return false;
        }
        kill_group(pid)
    }
}

pub use agent_dir::locked;

enum Probe {
    Connected(Box<Connected>),
    Unlocked,
    Waiting,
}

/// Looks at a directory until its process answers or its lock is free, or
/// `patience` runs out.
async fn look_once(dir: &Path, patience: Duration) -> Probe {
    let deadline = tokio::time::Instant::now() + patience;
    loop {
        if !locked(dir) {
            return Probe::Unlocked;
        }
        if let Some(connected) = dial(dir).await {
            return Probe::Connected(Box::new(connected));
        }
        if tokio::time::Instant::now() >= deadline {
            return Probe::Waiting;
        }
        tokio::time::sleep(PROBE_INTERVAL).await;
    }
}

/// Dials the directory's process until it answers with a Hello, or returns
/// None once its lock is free: the process is gone.
async fn connect(dir: &Path, running: &watch::Sender<bool>) -> Option<Connected> {
    let mut wait = PROBE_INTERVAL;
    let mut exits = running.subscribe();
    loop {
        if !*exits.borrow_and_update() && !locked(dir) {
            return None;
        }
        if let Some(connected) = dial(dir).await {
            return Some(connected);
        }
        tokio::select! {
            () = tokio::time::sleep(wait) => {}
            // The process ended: look again at once.
            _ = exits.changed() => {}
        }
        wait = (wait * 2).min(Duration::from_secs(1));
    }
}

async fn dial(dir: &Path) -> Option<Connected> {
    let (stream, peer) = local_socket::connect_to_process(&dir.join(agent_dir::CTL_SOCK))
        .await
        .ok()?;
    let (mut reader, writer) = tokio::io::split(stream);
    let hello = tokio::time::timeout(HELLO_PATIENCE, agent_dir::read_frame(&mut reader))
        .await
        .ok()?
        .ok()??;
    match hello.of {
        Some(ctl_frame::Of::Hello(hello)) => Some(Connected {
            reader,
            writer,
            hello,
            peer,
        }),
        _ => None,
    }
}

/// Starts `amux agent <dir>` detached from the daemon: its own process
/// group, so the daemon's terminal and death do not reach it, and no pipe
/// to the daemon, which would break when the daemon goes.
fn spawn_agent(install_path: &Path, dir: &Path) -> io::Result<tokio::process::Child> {
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(AGENT_LOG))?;
    let mut command = tokio::process::Command::new(install_path);
    command
        .arg("agent")
        .arg(dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(log)
        .kill_on_drop(false);
    #[cfg(unix)]
    command.process_group(0);
    #[cfg(windows)]
    {
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        command.creation_flags(CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS);
    }
    command.spawn()
}

pub use agent_dir::AGENT_LOG;

/// Waits on a child this daemon started, so it never lingers as a zombie.
async fn reap(mut child: tokio::process::Child) {
    let _ = child.wait().await;
}

#[cfg(unix)]
pub(crate) fn kill_group(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // SAFETY: plain signals to the process group the agent leads, or to
    // the process alone when it leads none.
    unsafe { libc::killpg(pid, libc::SIGKILL) == 0 || libc::kill(pid, libc::SIGKILL) == 0 }
}

#[cfg(not(unix))]
pub(crate) fn kill_group(pid: u32) -> bool {
    std::process::Command::new("taskkill")
        .args(["/F", "/T", "/PID", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// The inventory row for an agents row.
pub use daemon_core::serve::to_wire;

/// The core's name for one of this host's agents.
pub(crate) fn core_agent(id: AgentId) -> daemon_core::AgentId {
    daemon_core::AgentId(*id.as_bytes())
}

/// Why the core refused a registry call on `id`, as the registry says it.
fn registry_error(id: AgentId, error: CallError) -> RegistryError {
    let CallError::Registry(refusal) = error else {
        return RegistryError::Core(format!("{error:?}"));
    };
    match refusal {
        Refusal::NotFound => RegistryError::NotFound(id),
        Refusal::AlreadyExists => RegistryError::AlreadyExists(id),
        Refusal::Live => RegistryError::Live(id),
        Refusal::NameTaken(name) => RegistryError::NameTaken(name),
        Refusal::ParentEnded(parent) => RegistryError::ParentEnded(Uuid::from_bytes(parent.0)),
        Refusal::StartTimeout => RegistryError::StartTimeout(id),
        Refusal::StopTimeout => RegistryError::StopTimeout(id),
        Refusal::StillLocked => RegistryError::StillLocked(id),
        Refusal::NoSpec => RegistryError::NoSpec(id),
        Refusal::CouldNotStart(words) | Refusal::Directory(words) => {
            RegistryError::Io(io::Error::other(words))
        }
        Refusal::Worktree(error) => RegistryError::Refused(error),
        Refusal::Store(words) => RegistryError::Core(words),
    }
}

pub(crate) fn ms(ms: i64) -> Duration {
    Duration::from_millis(ms.max(0) as u64)
}

/// The model, permission, mode and effort an agent last ran with, as its
/// newest snapshot's body says them; nothing for a setting it never said.
fn last_settings(body: Option<&[u8]>, kind: Kind) -> spec::Settings {
    use prost::Message as _;
    let Some(body) = body else {
        return spec::Settings::default();
    };
    match kind {
        Kind::ClaudeSdk => wire::ClaudeSdkSnapshot::decode(body)
            .map(|snapshot| spec::Settings {
                model: snapshot.model,
                permission: snapshot.permission,
                mode: None,
                effort: snapshot.effort,
            })
            .unwrap_or_default(),
        Kind::ClaudePty => wire::ClaudePtySnapshot::decode(body)
            .map(|snapshot| spec::Settings {
                model: snapshot.model,
                permission: snapshot.permission,
                mode: None,
                effort: snapshot.effort,
            })
            .unwrap_or_default(),
        Kind::Codex => wire::CodexSnapshot::decode(body)
            .map(|snapshot| spec::Settings {
                model: snapshot.model,
                permission: snapshot.permission,
                mode: snapshot.mode,
                effort: snapshot.effort,
            })
            .unwrap_or_default(),
        Kind::Unspecified => spec::Settings::default(),
    }
}

/// The id a frame's verdict comes back under: a message's envelope id.
fn input_id_of(frame: &CtlFrame) -> Vec<u8> {
    match &frame.of {
        Some(ctl_frame::Of::AgentMessage(envelope)) => envelope.id.clone(),
        Some(ctl_frame::Of::Input(input)) => input.input_id.clone(),
        _ => Vec::new(),
    }
}
