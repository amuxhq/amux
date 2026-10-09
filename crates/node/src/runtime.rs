//! One profile runtime: a complete amux with its own host id, store and
//! agents. This module is its agent registry: spawn, resume, stop, delete
//! and rename, and the startup sweep that rebuilds the registry from disk.
//!
//! The registry keeps one [`AgentHandle`] per agent whose process is live
//! (or whose directory is locked by one): the directory, the control
//! connection, and the tools socket the agent's MCP server dials. Nothing
//! here is persisted; after a restart the sweep rebuilds it from the agent
//! directories and the store's rows.
//!
//! Each handle has one watcher task. It dials `ctl.sock`, reads the Hello,
//! ingests on every Nudge, and when the connection ends works out whether
//! the process is gone (its lock is free) or the connection merely
//! dropped (it redials). A process that is gone has its journal remainder
//! ingested, its row marked exited and its tools socket removed.

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use agent_dir::Clock;
use agent_dir::local_socket::{self, LocalListener, LocalStream};
use store::{
    AgentKey, AgentRow, Backend as _, CommitClock, Committed, Delivery, Marker, Record, Sqlite,
    Store as _, StoreError,
};
use tokio::io::{AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::sync::{Notify, oneshot, watch};
use tokio::task::JoinHandle;
use uuid::Uuid;
use wire::{
    Agent, AgentHello, AgentParent, AgentRemoved, Bootstrap, CaughtUp, CreateAgentRequest,
    CtlFrame, DeleteAgentResponse, DumpPart, EnvelopeKind, ErrorCode, Kind, Lifecycle, Rename,
    SendInputResponse, Stop, StopMode, WorkingOn, ctl_frame, inventory_event, session_event,
};

use crate::fanout::Fanout;
use crate::grpc::ClientApi;
use crate::install::{AGENTS, private_dir};
use crate::outbox::PushSender;
use crate::profiles::ProfileId;
use crate::retention::Retention;
use crate::serve::{event, inventory};
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
/// How long a stop waits after killing the process group before it gives
/// up on seeing the lock released.
const KILL_PATIENCE: Duration = Duration::from_secs(5);
/// Fully ingested journal segments kept below the cursor, for dumps.
pub const KEPT_SEGMENTS: usize = 2;
/// The most journal frames one ingest transaction commits. This is a bound
/// on how long a backlog drain holds the store lock: at the measured ingest
/// cost of about 23 µs a frame, a full batch holds it for about 6 ms, which
/// is the most a subscribe, an input or a spawn waits behind a draining
/// agent. Larger batches commit a backlog slightly faster per frame and
/// hold everything else off for longer; smaller ones pay a transaction per
/// fewer frames. The size is not derived from any ring or reader: the
/// fan-out ring is sized from it, not the other way round.
pub const INGEST_BATCH: usize = 256;
/// Events the per-agent fan-out ring holds. A batch's records are
/// published in one burst at memory speed while the store lock is still
/// held, so a reader's slack is counted in batches: a subscriber that has
/// read everything when a batch lands is closed with Lagged only after
/// four whole batches (a second or so of a busy agent) arrive without it
/// reading any, which absorbs a late wakeup by design rather than by luck.
/// A ring of one batch would close a caught-up reader whenever a short
/// batch followed a full one before its task was scheduled.
pub const FANOUT_CAPACITY: usize = 4 * INGEST_BATCH;
/// The tail size K: the rows a replica keeps and asks for, and the newest
/// rows own retention never trims. About an hour of a busy agent and
/// several screens of scroll-back.
pub const TAIL_ROWS: u32 = 200;

/// The exit causes the daemon records. It knows only what it asked for and
/// what it saw; the agent's own account of its exit is its final boundary
/// item. Clients show a cause after the word "exited", so none repeats it.
pub const CAUSE_STOPPED: &str = "stopped";
pub const CAUSE_ABORTED: &str = "aborted";
pub const CAUSE_KILLED: &str = "killed";
pub const CAUSE_EXITED: &str = "exited";
/// A one-shot child that exited by its own rule after its turn ended: it
/// finished its work, and clients say so in place of "exited".
pub const CAUSE_FINISHED: &str = "finished";
pub const CAUSE_EXITED_AWAY: &str = "while the daemon was away";
pub const CAUSE_NO_DIRECTORY: &str = "its directory is gone";
pub const CAUSE_UNSTARTED: &str = "the agent process could not start";

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
}

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
            own_budget_bytes: settings::RetentionSettings::default().own_budget_mib << 20,
            // A few MiB: a small overrun costs a small trim.
            retention_chunk_bytes: 4 << 20,
            tail_rows: TAIL_ROWS,
            retention_interval_ms: 10 * 60_000,
            replica_budget_bytes: settings::RetentionSettings::default().replica_rows_mib << 20,
            replica_blob_budget_bytes: settings::RetentionSettings::default().replica_blobs_mib
                << 20,
            source_backoff_ms: 1_000,
            source_backoff_max_ms: 30_000,
            repository_roots: Vec::new(),
            catalogue_max_age_ms: 30 * 60_000,
            catalogue_recheck_ms: 60_000,
        }
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
    #[error("agents of kind {0} are unknown")]
    UnknownKind(i32),
    #[error(transparent)]
    Config(#[from] spec::ConfigError),
    #[error("the working directory {0} is not a directory on this host")]
    BadCwd(String),
    #[error("{}", name_words(*.0))]
    BadName(wire::AgentNameProblem),
    #[error("starting in a new worktree: {0}")]
    Worktree(#[from] git_facts::WorktreeError),
    #[error("spawning on another host goes through that host's daemon")]
    OtherHost,
    #[error(transparent)]
    HostName(#[from] crate::forward::HostNameError),
    #[error(transparent)]
    Forwarded(#[from] crate::forward::ForwardError),
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
            Self::AlreadyExists(_) => ErrorCode::AlreadyExists,
            Self::Live(_) | Self::StillLocked(_) => ErrorCode::FailedPrecondition,
            Self::BadId(_)
            | Self::UnknownKind(_)
            | Self::Config(_)
            | Self::BadCwd(_)
            | Self::BadName(_) => ErrorCode::InvalidArgument,
            Self::OtherHost => ErrorCode::Unimplemented,
            Self::StartTimeout(_) | Self::StopTimeout(_) => ErrorCode::Aborted,
            Self::Worktree(error) => match error {
                git_facts::WorktreeError::BranchExists(_)
                | git_facts::WorktreeError::DestinationExists(_) => ErrorCode::AlreadyExists,
                git_facts::WorktreeError::NotARepository(_)
                | git_facts::WorktreeError::NoBranch(_)
                | git_facts::WorktreeError::BadBranchName(_) => ErrorCode::FailedPrecondition,
                git_facts::WorktreeError::Git(_) => ErrorCode::Internal,
            },
            Self::Store(_) | Self::Io(_) => ErrorCode::Internal,
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
}

pub struct ProfileRuntime {
    profile: ProfileId,
    host: Uuid,
    /// The installation's generation, carried on this host's entry.
    generation: u64,
    dir: PathBuf,
    clock: Arc<dyn Clock>,
    launch: Mutex<Launch>,
    /// Every write publishes to `fanout` while it still holds this lock, so
    /// subscribers see changes in commit order and a subscribe that holds
    /// it reads one point in that order.
    pub(crate) store: tokio::sync::Mutex<Sqlite>,
    pub(crate) fanout: Fanout,
    /// For tests: awaited by a subscribe between joining the channel and
    /// reading the cut, with the store held.
    pub(crate) join_hook: Mutex<Option<JoinHook>>,
    /// Each own agent's message lane: one agent message at a time.
    pub(crate) lanes: Mutex<HashMap<AgentId, Arc<tokio::sync::Mutex<()>>>>,
    /// Counts the store's writes, own commits and absorbed replica rows
    /// alike, so a waiter wakes on the write it is waiting for instead of
    /// polling: a message waiting for its acceptance item, a test fencing
    /// a host to an order.
    pub(crate) commits: watch::Sender<u64>,
    /// Journal frames committed since start: what ingest has done, so its
    /// cost per frame can be measured from outside.
    ingested_frames: AtomicU64,
    /// Set once every own journal has been read to its end after start.
    pub(crate) journals_read: watch::Sender<bool>,
    pub(crate) deliveries_due: Arc<Notify>,
    notifications_due: Arc<Notify>,
    pub(crate) push: Arc<dyn PushSender>,
    /// The outbox drains and the retention sweep.
    pub(crate) background: Mutex<Vec<JoinHandle<()>>>,
    agents: Mutex<HashMap<AgentId, Arc<AgentHandle>>>,
    /// One operation at a time per agent: spawn, resume, stop and delete
    /// each finish before the next starts.
    operations: Mutex<HashMap<AgentId, Arc<tokio::sync::Mutex<()>>>>,
    pub(crate) retention: watch::Sender<Retention>,
    pub(crate) reports: PathBuf,
    pub(crate) daemon_log: Option<PathBuf>,
    worktrees: Arc<dyn crate::worktree::MakeWorktree>,
    pub(crate) me: Weak<ProfileRuntime>,
    /// The network edge, once the profile is in service.
    edge: std::sync::OnceLock<Arc<crate::edge::Edge>>,
    /// Followers of trusted hosts and the sources of replica agents.
    pub(crate) sources: Mutex<crate::sources::Sources>,
    /// Replica blob files by last read, indexed at the first sweep.
    pub(crate) replica_blobs: Mutex<Option<store::BlobLru>>,
    /// The host set as last published on the inventory.
    pub(crate) hosts: Mutex<crate::hosts::HostSet>,
    /// The directories this profile's agents were started in.
    recent: Arc<Mutex<crate::repositories::Recent>>,
    analytics: analytics::Analytics,
    /// Turns ended since the last check-in.
    turns: crate::telemetry::Turns,
    /// What each provider offers on this host.
    offers: crate::offers::Offers,
    /// What each trusted host last said its providers offer, from its own
    /// entry on its inventory.
    pub(crate) peer_providers: Mutex<HashMap<Vec<u8>, Vec<wire::ProviderOnHost>>>,
}

pub(crate) struct AgentHandle {
    dir: PathBuf,
    /// The write half of the control connection, while one is open.
    pub(crate) ctl: tokio::sync::Mutex<Option<WriteHalf<LocalStream>>>,
    /// Inputs waiting for their verdict on the current connection, by
    /// input id; dropped when the connection ends.
    replies: Mutex<HashMap<Vec<u8>, oneshot::Sender<SendInputResponse>>>,
    /// Dumps waiting for the agent's part, by dump id; dropped when the
    /// connection ends.
    dumps: Mutex<HashMap<Vec<u8>, oneshot::Sender<DumpPart>>>,
    hello: watch::Sender<Option<AgentHello>>,
    /// The stop the daemon asked for, which names the exit's cause.
    stopping: Mutex<Option<StopMode>>,
    /// The agent refused an input as exiting: it is on its way out.
    exiting: AtomicBool,
    /// The process id, when this daemon started the process.
    pid: Option<u32>,
    /// Set at each Hello and cleared when ingest next reaches the end of
    /// the journal, which is when CaughtUp is broadcast: once per Hello.
    caught_up_due: AtomicBool,
    /// Whether the process this daemon started is still running. A new
    /// process has not taken its lock yet, so a free lock says nothing
    /// about it until it has exited.
    running: watch::Sender<bool>,
    pub(crate) exited: watch::Sender<bool>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
}

impl AgentHandle {
    fn new(dir: PathBuf, pid: Option<u32>) -> Arc<Self> {
        Arc::new(Self {
            dir,
            ctl: tokio::sync::Mutex::new(None),
            replies: Mutex::new(HashMap::new()),
            dumps: Mutex::new(HashMap::new()),
            hello: watch::Sender::new(None),
            stopping: Mutex::new(None),
            exiting: AtomicBool::new(false),
            caught_up_due: AtomicBool::new(false),
            pid,
            running: watch::Sender::new(pid.is_some()),
            exited: watch::Sender::new(false),
            tasks: Mutex::new(Vec::new()),
        })
    }

    fn abort(&self) {
        for task in self.tasks.lock().unwrap().drain(..) {
            task.abort();
        }
    }

    pub(crate) fn expect_reply(
        &self,
        input_id: Vec<u8>,
        reply: oneshot::Sender<SendInputResponse>,
    ) {
        self.replies.lock().unwrap().insert(input_id, reply);
    }

    pub(crate) fn forget_reply(&self, input_id: &[u8]) {
        self.replies.lock().unwrap().remove(input_id);
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
        self.sources.get_mut().unwrap().abort_all();
    }
}

/// What a profile runtime is made of.
pub struct Profile {
    pub profile: ProfileId,
    pub host: Uuid,
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
    /// Where the profile records product analytics.
    pub analytics: analytics::Analytics,
    /// What makes the worktree an agent asked to start in one starts in.
    pub worktrees: Arc<dyn crate::worktree::MakeWorktree>,
}

impl ProfileRuntime {
    /// A runtime over an opened profile. Writes nothing.
    pub fn open(profile: Profile) -> Arc<Self> {
        let Profile {
            profile,
            host,
            generation,
            dir,
            store,
            launch,
            clock,
            push,
            reports,
            daemon_log,
            analytics,
            worktrees,
        } = profile;
        Arc::new_cyclic(|me| Self {
            profile,
            host,
            generation,
            clock,
            fanout: Fanout::new(launch.fanout_capacity, launch.inventory_capacity),
            join_hook: Mutex::new(None),
            lanes: Mutex::new(HashMap::new()),
            commits: watch::Sender::new(0),
            ingested_frames: AtomicU64::new(0),
            journals_read: watch::Sender::new(false),
            deliveries_due: Arc::new(Notify::new()),
            notifications_due: Arc::new(Notify::new()),
            push,
            background: Mutex::new(Vec::new()),
            launch: Mutex::new(launch),
            store: tokio::sync::Mutex::new(store),
            agents: Mutex::new(HashMap::new()),
            operations: Mutex::new(HashMap::new()),
            retention: watch::Sender::new(Retention::default()),
            reports,
            daemon_log,
            worktrees,
            me: me.clone(),
            edge: std::sync::OnceLock::new(),
            sources: Mutex::new(crate::sources::Sources::default()),
            replica_blobs: Mutex::new(None),
            hosts: Mutex::new(crate::hosts::HostSet::new()),
            recent: Arc::new(Mutex::new(crate::repositories::Recent::load(&dir))),
            offers: crate::offers::Offers::load(&dir),
            peer_providers: Mutex::new(HashMap::new()),
            dir,
            analytics,
            turns: crate::telemetry::Turns::default(),
        })
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
            options,
            self.clock.clone(),
            Arc::downgrade(self),
            self.analytics.clone(),
        )
        .await?;
        let edge = self.edge.get_or_init(|| edge).clone();
        self.start_replication();
        Ok(edge)
    }

    /// The profile's network edge, once started.
    pub fn edge(&self) -> Option<Arc<crate::edge::Edge>> {
        self.edge.get().cloned()
    }

    /// Closes the edge's links with `reason` and stops serving the network.
    pub async fn stop_edge(&self, reason: wire::LinkCloseReason) {
        self.sources.lock().unwrap().abort_all();
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

    pub fn host(&self) -> Uuid {
        self.host
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

    /// The store, held exclusively while the guard lives.
    pub async fn store(&self) -> tokio::sync::MutexGuard<'_, Sqlite> {
        self.store.lock().await
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
        AgentKey::new(self.host.as_bytes().to_vec(), id.as_bytes().to_vec())
    }

    /// The inventory row for an agent.
    pub async fn agent(&self, id: AgentId) -> Result<Agent, RegistryError> {
        let row = self
            .store
            .lock()
            .await
            .agent(&self.key(id))?
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
        if let Some(handle) = self.agents.lock().unwrap().get(&id) {
            handle.exiting.store(true, Ordering::SeqCst);
        }
    }

    pub(crate) fn operation(&self, id: AgentId) -> Arc<tokio::sync::Mutex<()>> {
        self.operations
            .lock()
            .unwrap()
            .entry(id)
            .or_default()
            .clone()
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
        if self.target_host(&request)? != self.host {
            return Err(RegistryError::OtherHost);
        }
        let kind = match Kind::try_from(request.kind) {
            Ok(Kind::Unspecified) | Err(_) => {
                return Err(RegistryError::UnknownKind(request.kind));
            }
            Ok(kind) => kind,
        };
        let resolved = spec::resolve(kind, &request.config)?;
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
        if request.cwd.is_empty() {
            // A spawn from another host names no path here unless it knows
            // one: the child starts in the home directory of the user this
            // daemon runs as.
            request.cwd = home_dir()
                .map(|home| home.to_string_lossy().into_owned())
                .unwrap_or_default();
        }
        if request.cwd.is_empty() || !Path::new(&request.cwd).is_dir() {
            return Err(RegistryError::BadCwd(request.cwd));
        }
        let parent = match caller {
            Some(caller) => Some(AgentParent {
                host_id: self.host.as_bytes().to_vec(),
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

        let operation = self.operation(id);
        let _operation = operation.lock().await;
        let key = self.key(id);
        let now = self.clock.now_ms();
        let row_for = |name: &str, cwd: &str| {
            let mut row = AgentRow::new(key.clone(), kind, cwd);
            row.name = Some(name.to_owned());
            row.parent = parent
                .as_ref()
                .map(|parent| AgentKey::new(parent.host_id.clone(), parent.agent_id.clone()));
            row.created_at = now;
            row.incarnation = 1;
            row
        };
        let name = {
            let mut store = self.store.lock().await;
            if store.agent(&key)?.is_some() {
                return Err(RegistryError::AlreadyExists(id));
            }
            // Chosen under the store lock, so two creates at once cannot
            // both take the same free name.
            let name = match given {
                Some(name) => name,
                None => {
                    let names: HashSet<String> = store
                        .agents()?
                        .into_iter()
                        .filter(|row| row.agent.host == key.host)
                        .filter_map(|row| row.name.map(|name| name.to_lowercase()))
                        .collect();
                    crate::names::assign_name(&|candidate| {
                        names.contains(candidate) || branches.contains(candidate)
                    })
                }
            };
            // The row comes first: a crash after it leaves a row the sweep
            // marks exited, never a directory nothing lists.
            if !request.new_worktree {
                self.put_row(&mut store, &row_for(&name, &request.cwd))?;
            }
            name
        };
        // A worktree is made before the row, outside the store's lock: a
        // folder it cannot be made in fails the create with no agent made.
        // Two unnamed creates at once in one repository could pick the same
        // pair; the second then finds the branch taken and fails.
        let chosen = request.cwd.clone();
        let base_branch = if request.new_worktree {
            let made = self.worktrees.make(Path::new(&chosen), &name).await?;
            request.cwd = made.path.to_string_lossy().into_owned();
            let mut store = self.store.lock().await;
            self.put_row(&mut store, &row_for(&name, &request.cwd))?;
            Some(made.base_branch)
        } else {
            None
        };

        let dir = self.agent_dir(id);
        private_dir(&dir)?;
        let launch = self.launch();
        let offered = self.offered_to(kind).await;
        let spec = spec::build(
            &launch,
            spec::Incarnation {
                agent_id: id.as_bytes(),
                profile_id: self.profile.as_bytes(),
                kind,
                cwd: &request.cwd,
                name: &name,
                parent,
                resolved: &resolved,
                created_at_ms: now,
                incarnation: 1,
                bootstrap,
                offered,
                base_branch,
            },
        );
        spec::write(&dir, &spec)?;
        let started = self.start_process(id, &dir, &launch).await;
        if started.is_ok() {
            self.recent.lock().unwrap().record(Path::new(&chosen), now);
        }
        started
    }

    /// Where this host offers to start an agent: see
    /// [`crate::repositories::list`].
    pub async fn list_repositories(
        &self,
        query: Option<String>,
        limit: u32,
    ) -> wire::ListRepositoriesResponse {
        let roots = self.launch().repository_roots;
        let recent = self.recent.clone();
        tokio::task::spawn_blocking(move || {
            let recent = recent.lock().unwrap();
            crate::repositories::list(&roots, &recent, query.as_deref(), limit)
        })
        .await
        .expect("listing repositories does not panic")
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
            _ => Ok(self.host),
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
        let parent = caller.map(|caller| AgentParent {
            host_id: self.host.as_bytes().to_vec(),
            agent_id: caller.as_bytes().to_vec(),
        });
        let request = crate::forward::forwarded_create(request, host, parent);
        Ok(self
            .on_peer(host, |mut client| async move {
                client.create_agent(request).await
            })
            .await?)
    }

    /// Starts the agent's next incarnation: waits for a dying process to
    /// release the directory, writes `spec.<n+1>` from the current
    /// configuration with `prompt` as its first input, and spawns it.
    /// Starts an exited agent's next incarnation, its queue seeded by
    /// `bootstrap` when one is given.
    pub async fn resume(
        &self,
        id: AgentId,
        bootstrap: Option<Bootstrap>,
    ) -> Result<Agent, RegistryError> {
        let operation = self.operation(id);
        let _operation = operation.lock().await;
        let key = self.key(id);
        let mut row = self
            .store
            .lock()
            .await
            .agent(&key)?
            .ok_or(RegistryError::NotFound(id))?;
        let launch = self.launch();
        let handle = self.agents.lock().unwrap().get(&id).cloned();
        if let Some(handle) = handle {
            let leaving =
                handle.exiting.load(Ordering::SeqCst) || handle.stopping.lock().unwrap().is_some();
            if !leaving {
                return Err(RegistryError::Live(id));
            }
            self.wait_exit(&handle, launch.stop_deadline_ms)
                .await
                .map_err(|_| RegistryError::StillLocked(id))?;
        }
        let dir = self.agent_dir(id);
        // The process may be gone from the registry and still dying: the
        // new one cannot take a lock the old one holds.
        self.wait_unlocked(&dir, launch.stop_deadline_ms)
            .await
            .map_err(|_| RegistryError::StillLocked(id))?;

        let previous = spec::newest(&dir)?.ok_or(RegistryError::NoSpec(id))?;
        let next = previous.incarnation + 1;
        let resolved = spec::Resolved {
            provider_args: previous.provider_args.clone(),
            model: previous.config.as_ref().and_then(|c| c.model.clone()),
            permission: previous.config.as_ref().and_then(|c| c.permission.clone()),
            mode: previous.config.as_ref().and_then(|c| c.mode.clone()),
        };
        let offered = self.offered_to(previous.kind()).await;
        let spec = spec::build(
            &launch,
            spec::Incarnation {
                agent_id: id.as_bytes(),
                profile_id: self.profile.as_bytes(),
                kind: previous.kind(),
                cwd: &previous.cwd,
                // The name it has now: a rename while it was not running
                // reaches its Codex thread this way.
                name: row.name.as_deref().unwrap_or(&previous.name),
                parent: previous.parent.clone(),
                resolved: &resolved,
                created_at_ms: self.clock.now_ms(),
                incarnation: next,
                bootstrap,
                offered,
                base_branch: previous.base_branch.clone(),
            },
        );
        spec::write(&dir, &spec)?;
        {
            let mut store = self.store.lock().await;
            row.lifecycle = Lifecycle::Live as i32;
            row.exit_cause = None;
            row.incarnation = next;
            self.put_row(&mut store, &row)?;
            // The next CaughtUp comes from this incarnation's journal.
            store.set_marker(&key, None);
        }
        self.start_process(id, &dir, &launch).await
    }

    /// What the host's provider offers, for a kind that cannot ask its
    /// provider itself: terminal Claude takes the host's Claude's.
    async fn offered_to(&self, kind: Kind) -> Option<wire::Catalogue> {
        if kind != Kind::ClaudePty {
            return None;
        }
        self.host_catalogue(crate::Provider::Claude)
            .await
            .ok()
            .map(|offered| offered.catalogue)
    }

    /// Binds the tools socket, spawns `amux agent <dir>`, and waits for its
    /// Hello or its exit.
    async fn start_process(
        &self,
        id: AgentId,
        dir: &Path,
        launch: &Launch,
    ) -> Result<Agent, RegistryError> {
        let tools = self.bind_tools(id, dir)?;
        let child = match spawn_agent(&launch.install_path, dir) {
            Ok(child) => child,
            Err(error) => {
                drop(tools);
                self.mark_exited(id, CAUSE_UNSTARTED).await?;
                return Err(error.into());
            }
        };
        let pid = child.id();
        let handle = AgentHandle::new(dir.to_owned(), pid);
        handle.tasks.lock().unwrap().push(tools);
        let running = handle.running.clone();
        handle.tasks.lock().unwrap().push(tokio::spawn(async move {
            reap(child).await;
            running.send_replace(false);
        }));
        self.agents.lock().unwrap().insert(id, handle.clone());
        self.watch(id, handle.clone(), None);

        let mut hello = handle.hello.subscribe();
        let mut exited = handle.exited.subscribe();
        let deadline = self
            .clock
            .sleep_until(self.clock.now_ms() + launch.start_deadline_ms);
        tokio::select! {
            _ = hello.wait_for(Option::is_some) => {}
            _ = exited.wait_for(|exited| *exited) => {}
            () = deadline => {
                self.kill(&handle);
                return Err(RegistryError::StartTimeout(id));
            }
        }
        self.agent(id).await
    }

    /// Serves the client service on `<dir>/tools.sock` for the agent's MCP
    /// server. The socket is its caller's identity: whatever arrives on it
    /// is from `id`.
    fn bind_tools(&self, id: AgentId, dir: &Path) -> io::Result<JoinHandle<()>> {
        let listener = LocalListener::bind(&dir.join(agent_dir::TOOLS_SOCK))?;
        Ok(crate::grpc::serve_client(
            listener,
            ClientApi::weak(self.me.clone(), Some(id)),
        ))
    }

    // --- stop, delete, rename --------------------------------------------

    /// Stops a live agent's process: graceful finishes the turn, abort
    /// cancels it, kill ends the process group at once. A stop that
    /// overruns the stop deadline kills. An exited agent is left as it is.
    pub async fn stop(&self, id: AgentId, mode: StopMode) -> Result<Agent, RegistryError> {
        let operation = self.operation(id);
        let _operation = operation.lock().await;
        self.stop_locked(id, mode).await
    }

    async fn stop_locked(&self, id: AgentId, mode: StopMode) -> Result<Agent, RegistryError> {
        let Some(handle) = self.agents.lock().unwrap().get(&id).cloned() else {
            return self.agent(id).await;
        };
        *handle.stopping.lock().unwrap() = Some(mode);
        let frame = CtlFrame {
            of: Some(ctl_frame::Of::Stop(Stop { mode: mode as i32 })),
        };
        let launch = self.launch();
        // A failed write means the connection is already going; the
        // deadline below still applies.
        let _ = handle.write_ctl(&frame, ms(launch.ctl_write_ms)).await;
        let deadline = launch.stop_deadline_ms;
        if self.wait_exit(&handle, deadline).await.is_err() {
            *handle.stopping.lock().unwrap() = Some(StopMode::Kill);
            if !self.kill(&handle) {
                return Err(RegistryError::StopTimeout(id));
            }
            self.wait_exit(&handle, KILL_PATIENCE.as_millis() as i64)
                .await
                .map_err(|_| RegistryError::StopTimeout(id))?;
        }
        self.agent(id).await
    }

    /// Deletes an agent: aborts its process if live, deletes its children
    /// on this host the same way and forwards the delete of each child on
    /// another host to that host, then removes its rows and its directory.
    /// A child whose host cannot be reached is reported as unreachable.
    pub async fn delete(&self, id: AgentId) -> Result<DeleteAgentResponse, RegistryError> {
        let mut response = DeleteAgentResponse::default();
        self.delete_into(id, &mut response).await?;
        Ok(response)
    }

    async fn delete_into(
        &self,
        id: AgentId,
        response: &mut DeleteAgentResponse,
    ) -> Result<(), RegistryError> {
        let operation = self.operation(id);
        let _operation = operation.lock().await;
        let key = self.key(id);
        if self.store.lock().await.agent(&key)?.is_none() {
            return Err(RegistryError::NotFound(id));
        }
        // Abort, not graceful: a turn allowed to finish into a directory
        // about to be removed is work thrown away, and a delete must not
        // wait on an ask nobody will answer.
        self.stop_locked(id, StopMode::Abort).await?;

        let children: Vec<AgentRow> = self
            .store
            .lock()
            .await
            .agents()?
            .into_iter()
            .filter(|row| row.parent.as_ref() == Some(&key))
            .collect();
        for child in children {
            let wire = to_wire(&child);
            if child.agent.host != self.host.as_bytes() {
                self.delete_remote_child(child, response).await;
                continue;
            }
            let Ok(child_id) = Uuid::from_slice(&child.agent.agent) else {
                continue;
            };
            // Best effort: a child that cannot be deleted stays listed as an
            // orphan the person can delete.
            match Box::pin(self.delete_into(child_id, response)).await {
                Ok(()) => response.removed_children.push(wire),
                Err(error) => {
                    tracing::warn!(child = %child_id, %error, "a cascaded delete failed");
                    response.unreachable_children.push(wire);
                }
            }
        }

        // Rows first: a crash between the two leaves a directory with no
        // row, which the next sweep removes.
        {
            let mut store = self.store.lock().await;
            store.delete_agent(&key)?;
            self.fanout.close(&key);
            self.fanout
                .publish_inventory(inventory(inventory_event::Of::AgentRemoved(AgentRemoved {
                    host_id: key.host.clone(),
                    agent_id: key.agent.clone(),
                    reason: Some("deleted".to_owned()),
                })));
        }
        match std::fs::remove_dir_all(self.agent_dir(id)) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        self.operations.lock().unwrap().remove(&id);
        Ok(())
    }

    /// Renames an agent. The row takes the name now and the next
    /// incarnation's spec when it is resumed; a spec already written keeps
    /// the name it was spawned with. A live Codex agent is told, so its
    /// thread takes the name; a missed word is made good at the next
    /// resume.
    pub async fn rename(&self, id: AgentId, name: &str) -> Result<Agent, RegistryError> {
        if let Some(problem) = wire::agent_name_problem(name) {
            return Err(RegistryError::BadName(problem));
        }
        let key = self.key(id);
        let row = {
            let mut store = self.store.lock().await;
            let mut row = store.agent(&key)?.ok_or(RegistryError::NotFound(id))?;
            row.name = Some(name.to_owned());
            self.put_row(&mut store, &row)?;
            row
        };
        let handle = self.agents.lock().unwrap().get(&id).cloned();
        if let Some(handle) = handle
            && row.kind == Kind::Codex as i32
        {
            let frame = CtlFrame {
                of: Some(ctl_frame::Of::Rename(Rename {
                    name: row.name.clone().unwrap_or_default(),
                })),
            };
            let _ = handle
                .write_ctl(&frame, ms(self.launch().ctl_write_ms))
                .await;
        }
        Ok(to_wire(&row))
    }

    // --- the sweep ---------------------------------------------------------

    /// The read-only half of the sweep: try the lock of every agent
    /// directory, dial the live ones and read their Hello. Writes nothing.
    pub async fn look(&self) -> Result<Looked, RegistryError> {
        let rows: HashMap<AgentId, AgentRow> = self
            .store
            .lock()
            .await
            .agents()?
            .into_iter()
            .filter(|row| row.agent.host == self.host.as_bytes())
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
    /// agents and ingests from their stored cursors, marks the rest
    /// exited once their journal remainder is ingested and sets their
    /// CaughtUp flag, and removes directories no row lists.
    pub async fn finish_sweep(&self, looked: Looked) -> Result<SweepReport, RegistryError> {
        let mut report = SweepReport::default();
        for (id, look) in looked.agents {
            match look {
                Look::Live(connected) => {
                    let handle = AgentHandle::new(self.agent_dir(id), None);
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
                    let handle = AgentHandle::new(self.agent_dir(id), None);
                    self.agents.lock().unwrap().insert(id, handle.clone());
                    self.watch(id, handle, None);
                    report.unanswered.push(id);
                }
                Look::Exited => {
                    self.ingest(id).await?;
                    self.mark_exited_if_live(id, CAUSE_EXITED_AWAY).await?;
                    report.exited.push(id);
                }
                Look::NoDirectory => {
                    self.mark_exited_if_live(id, CAUSE_NO_DIRECTORY).await?;
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
        self.journals_read.send_replace(true);
        Ok(report)
    }

    /// Starts the background work: the deliveries outbox now and whenever
    /// a row may have become deliverable, notifications as each falls due,
    /// and the retention sweep now and then on its interval.
    pub fn start_background(&self) {
        let retention = self.start_retention();
        self.background.lock().unwrap().push(retention);
        let deliveries = {
            let runtime = self.me.clone();
            let due = self.deliveries_due.clone();
            tokio::spawn(async move {
                loop {
                    let Some(me) = runtime.upgrade() else { return };
                    let wake = due.notified();
                    tokio::pin!(wake);
                    wake.as_mut().enable();
                    let report = me.drain_deliveries().await;
                    let retry = (report.kept > 0).then(|| {
                        me.clock
                            .sleep_until(me.clock.now_ms() + me.launch().delivery_retry_ms)
                    });
                    drop(me);
                    match retry {
                        Some(retry) => {
                            tokio::select! {
                                () = wake => {}
                                () = retry => {}
                            }
                        }
                        None => wake.await,
                    }
                }
            })
        };
        let notifications = {
            let runtime = self.me.clone();
            let due_now = self.notifications_due.clone();
            tokio::spawn(async move {
                loop {
                    let Some(me) = runtime.upgrade() else { return };
                    let wake = due_now.notified();
                    tokio::pin!(wake);
                    wake.as_mut().enable();
                    let next = me.drain_notifications().await;
                    let due = next.map(|at| me.clock.sleep_until(at));
                    drop(me);
                    match due {
                        Some(due) => {
                            tokio::select! {
                                () = wake => {}
                                () = due => {}
                            }
                        }
                        None => wake.await,
                    }
                }
            })
        };
        self.background
            .lock()
            .unwrap()
            .extend([deliveries, notifications]);
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
        for handle in self.agents.lock().unwrap().values() {
            for task in handle.tasks.lock().unwrap().drain(..) {
                task.abort();
            }
        }
        self.sources.lock().unwrap().abort_all();
        if let Some(edge) = self.edge.get() {
            edge.abort();
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

    /// Reads the agent's journal from its row's cursor to the end, commits
    /// what it finds in one transaction, then broadcasts each record once.
    /// At the end of the journal, the first time after each Hello, it
    /// broadcasts CaughtUp. Segments that lie wholly below a cursor that
    /// has reached the drive are deleted, but for the newest few.
    ///
    /// A backlog is committed [`INGEST_BATCH`] frames at a time, with the
    /// store released between batches, so a journal that ran ahead of the
    /// daemon never holds every other reader and writer off for the whole
    /// of its catch-up.
    pub async fn ingest(&self, id: AgentId) -> Result<Committed, RegistryError> {
        loop {
            let (committed, more) = self.ingest_batch(id).await?;
            if !more {
                return Ok(committed);
            }
            tokio::task::yield_now().await;
        }
    }

    async fn ingest_batch(&self, id: AgentId) -> Result<(Committed, bool), RegistryError> {
        let key = self.key(id);
        let journal_dir = self.agent_dir(id).join(agent_dir::JOURNAL);
        // Held through the broadcast: commit order is broadcast order, and
        // a subscribe waiting on the lock reads a cut either wholly before
        // this batch or wholly after it.
        let mut store = self.store.lock().await;
        let mut cursor = store.cursor(&key)?;
        let mut reader = journal::Reader::new(&journal_dir, cursor);
        let mut batch = reader.read_up_to(INGEST_BATCH)?;
        if batch.frames.is_empty() {
            // Nothing past the cursor may mean the cursor is past the
            // journal: the store reached the drive after committing frames
            // the unsynced journal then lost to a power cut. The agent
            // re-derives from the last whole frame, so that is where the
            // cursor goes; revisions keep counting up, so what it writes
            // again is new to every subscriber.
            let end = journal::end(&journal_dir)?;
            if end < cursor {
                tracing::warn!(agent = %id, cursor, end, "the journal ends below the cursor; rewinding");
                store.rewind_cursor(&key, end)?;
                cursor = end;
                reader.seek(end);
                batch = reader.read_up_to(INGEST_BATCH)?;
            }
        }
        // A torn frame in the newest segment is a write still under way:
        // the journal has not ended yet, and its Nudge will come.
        let at_end =
            !batch.more && !matches!(batch.torn, Some(journal::Torn { skipped: false, .. }));
        let committed = if batch.frames.is_empty() {
            Committed {
                cursor,
                ..Committed::default()
            }
        } else {
            let clock = CommitClock {
                now_ms: self.clock.now_ms(),
                notify_delay_ms: self.launch.lock().unwrap().notify_delay_ms,
            };
            let committed = store.commit(&key, &batch.frames, clock)?;
            self.wrote();
            self.ingested_frames
                .fetch_add(batch.frames.len() as u64, Ordering::Relaxed);
            let turns = batch
                .frames
                .iter()
                .filter(|(_, step)| step.turn_end.is_some())
                .count();
            if turns > 0 {
                self.deliveries_due.notify_one();
                if self.analytics.is_on()
                    && let Some(kind) = store
                        .agent(&key)?
                        .and_then(|row| crate::telemetry::kind_coded(row.kind))
                {
                    self.turns.add(kind, turns as u32);
                }
            }
            // Only now, with the transaction committed: a subscriber never
            // sees a revision the store could not serve it.
            let mut envelope = false;
            for record in &committed.records {
                let of = match record {
                    Record::Item(item) => session_event::Of::Item(item.clone()),
                    Record::Append(append) => session_event::Of::Append(append.clone()),
                    Record::Snapshot(snapshot) => {
                        envelope = true;
                        session_event::Of::Snapshot(snapshot.clone())
                    }
                };
                self.fanout.publish(&key, event(of));
            }
            if envelope {
                // Phase, working_on, since-when and git ride the inventory row.
                self.publish_row(&store, &key)?;
                self.notifications_due.notify_one();
            }
            self.reclaim(&store, &journal_dir, committed.cursor);
            committed
        };
        let due = self
            .agents
            .lock()
            .unwrap()
            .get(&id)
            .is_some_and(|handle| handle.caught_up_due.load(Ordering::SeqCst));
        if at_end && due {
            self.announce_caught_up(&mut store, &key)?;
            if let Some(handle) = self.agents.lock().unwrap().get(&id) {
                handle.caught_up_due.store(false, Ordering::SeqCst);
            }
        }
        Ok((committed, batch.more))
    }

    /// Deletes the segments that lie wholly below `cursor`, keeping the
    /// newest [`KEPT_SEGMENTS`] of them. The cursor was committed without a
    /// sync, and a power cut could take the store back to before it while
    /// the deleted frames were its only copy, so the store is flushed to the
    /// drive first and nothing is deleted if the flush fails. That costs one
    /// flush per segment the agent fills.
    fn reclaim(&self, store: &Sqlite, journal_dir: &Path, cursor: u64) {
        let starts = match journal::reclaimable(journal_dir, cursor, KEPT_SEGMENTS) {
            Ok(starts) if !starts.is_empty() => starts,
            Ok(_) => return,
            Err(error) => {
                tracing::warn!(%error, "listing journal segments failed");
                return;
            }
        };
        if let Err(error) = store.flush_to_drive() {
            tracing::warn!(%error, "flushing the store before reclaiming segments failed");
            return;
        }
        for start in starts {
            if let Err(error) = std::fs::remove_file(journal::segment_path(journal_dir, start)) {
                tracing::warn!(%error, start, "deleting an ingested journal segment failed");
            }
        }
    }

    // --- the watcher ---------------------------------------------------------

    /// Takes a connection as the agent's current one: records its Hello,
    /// keeps its write half for stops and inputs, and ingests what the
    /// journal holds beyond the stored cursor. Returns the read half.
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
        } = connected;
        if let Err(error) = self.hello_received(id, &hello).await {
            tracing::warn!(agent = %id, %error, "recording an agent's Hello failed");
        }
        *handle.ctl.lock().await = Some(writer);
        handle.hello.send_replace(Some(hello));
        handle.caught_up_due.store(true, Ordering::SeqCst);
        if let Err(error) = self.ingest(id).await {
            tracing::warn!(agent = %id, %error, "ingest failed");
        }
        reader
    }

    /// Runs the agent's watcher: from `reader` if a connection is already
    /// adopted, else from the first successful dial.
    fn watch(&self, id: AgentId, handle: Arc<AgentHandle>, reader: Option<ReadHalf<LocalStream>>) {
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
                            if let (Some(waiting), Some(verdict)) = (waiting, reply.verdict) {
                                let _ = waiting.send(verdict);
                            }
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
            if let Some(me) = runtime.upgrade() {
                me.exited(id, &watched).await;
            }
        });
        handle.tasks.lock().unwrap().push(task);
    }

    async fn hello_received(&self, id: AgentId, hello: &AgentHello) -> Result<(), StoreError> {
        let key = self.key(id);
        let mut store = self.store.lock().await;
        if let Some(mut row) = store.agent(&key)?
            && row.producer_version != hello.agent_version
        {
            row.producer_version = hello.agent_version.clone();
            self.put_row(&mut store, &row)?;
        }
        Ok(())
    }

    /// The process is gone: ingest what it left, record the exit, close its
    /// tools socket and let whoever waits on it go.
    async fn exited(&self, id: AgentId, handle: &Arc<AgentHandle>) {
        if let Err(error) = self.ingest(id).await {
            tracing::warn!(agent = %id, %error, "ingesting an exited agent's journal failed");
        }
        let cause = match *handle.stopping.lock().unwrap() {
            Some(StopMode::Graceful) => CAUSE_STOPPED,
            Some(StopMode::Abort) => CAUSE_ABORTED,
            Some(StopMode::Kill) => CAUSE_KILLED,
            None => CAUSE_EXITED,
        };
        if let Err(error) = self.mark_exited(id, cause).await {
            tracing::warn!(agent = %id, %error, "marking an agent exited failed");
        }
        {
            let mut agents = self.agents.lock().unwrap();
            if agents
                .get(&id)
                .is_some_and(|current| Arc::ptr_eq(current, handle))
            {
                agents.remove(&id);
            }
        }
        // Every task but this watcher: the tools listener's socket goes with
        // it, and the reaper has nothing left to wait for.
        let tasks: Vec<_> = handle.tasks.lock().unwrap().drain(..).collect();
        let current = tokio::task::try_id();
        for task in tasks {
            if Some(task.id()) != current {
                task.abort();
                let _ = task.await;
            }
        }
        let _ = std::fs::remove_file(handle.dir.join(agent_dir::TOOLS_SOCK));
        handle.exited.send_replace(true);
    }

    async fn mark_exited(&self, id: AgentId, cause: &str) -> Result<(), StoreError> {
        let key = self.key(id);
        let mut store = self.store.lock().await;
        let Some(row) = store.agent(&key)? else {
            return Ok(());
        };
        self.record_exit(&mut store, row, cause)?;
        // Everything the process wrote is committed: nothing more comes.
        self.caught_up(&mut store, &key)
    }

    async fn mark_exited_if_live(&self, id: AgentId, cause: &str) -> Result<(), StoreError> {
        let key = self.key(id);
        let mut store = self.store.lock().await;
        let Some(row) = store.agent(&key)? else {
            return Ok(());
        };
        if row.lifecycle != Lifecycle::Exited as i32 {
            self.record_exit(&mut store, row, cause)?;
        }
        self.caught_up(&mut store, &key)
    }

    /// Writes a row's registry fields and tells inventory subscribers,
    /// with the store still held so they see changes in store order.
    fn put_row(&self, store: &mut Sqlite, row: &AgentRow) -> Result<(), StoreError> {
        store.put_agent(row)?;
        // A lifecycle or incarnation change can make a waiting delivery
        // deliverable, or stale.
        self.deliveries_due.notify_one();
        self.publish_row(store, &row.agent)
    }

    /// Marks the row exited, then settles what the process leaves: its
    /// unsent notifications go, since no push may name an agent that is
    /// gone, and a child whose incarnation ended without finishing its turn
    /// tells its parent it failed. A finished turn already told it.
    fn record_exit(
        &self,
        store: &mut Sqlite,
        mut row: AgentRow,
        cause: &str,
    ) -> Result<(), StoreError> {
        // A child exits unasked only once its turn ended with nothing left
        // to do, or when its process failed mid-turn; the first is its
        // finish.
        let cause = if cause == CAUSE_EXITED && row.parent.is_some() && !row.turn_open {
            CAUSE_FINISHED
        } else {
            cause
        };
        row.lifecycle = Lifecycle::Exited as i32;
        row.exit_cause = Some(cause.to_owned());
        self.put_row(store, &row)?;
        store.remove_notifications(&row.agent.agent)?;
        let Some(parent) = &row.parent else {
            return Ok(());
        };
        // As committed: the phase at exit says nothing here, since a
        // provider's exit leaves it starting whether or not the turn ended.
        let held = store.agent(&row.agent)?.unwrap_or(row.clone());
        if !held.turn_open {
            return Ok(());
        }
        // Zero when the parent's row is not held: stamped by the drain.
        let parent_incarnation = store.agent(parent)?.map_or(0, |parent| parent.incarnation);
        store.put_delivery(&Delivery {
            child_id: row.agent.agent.clone(),
            incarnation: row.incarnation,
            turn_id: 0,
            parent: parent.clone(),
            parent_incarnation,
            kind: EnvelopeKind::Failed as i32,
            body: cause.to_owned(),
        })?;
        self.deliveries_due.notify_one();
        Ok(())
    }

    /// Tells inventory subscribers what the row now says.
    pub(crate) fn publish_row(&self, store: &Sqlite, key: &AgentKey) -> Result<(), StoreError> {
        if let Some(row) = store.agent(key)? {
            self.fanout
                .publish_inventory(inventory(inventory_event::Of::Agent(to_wire(&row))));
        }
        Ok(())
    }

    /// Sets the agent's CaughtUp flag and broadcasts the marker, unless the
    /// flag is already set: a subscriber that joined since read it at its
    /// cut, and one that joined before got the broadcast then.
    fn caught_up(&self, store: &mut Sqlite, key: &AgentKey) -> Result<(), StoreError> {
        if store.markers().get(key) == Some(&Marker::CaughtUp) {
            return Ok(());
        }
        self.announce_caught_up(store, key)
    }

    fn announce_caught_up(&self, store: &mut Sqlite, key: &AgentKey) -> Result<(), StoreError> {
        let Some(row) = store.agent(key)? else {
            return Ok(());
        };
        store.set_marker(key, Some(Marker::CaughtUp));
        self.fanout.publish(
            key,
            event(session_event::Of::CaughtUp(CaughtUp {
                revision: row.next_revision.saturating_sub(1),
            })),
        );
        Ok(())
    }

    // --- waiting on processes ----------------------------------------------

    async fn wait_exit(&self, handle: &AgentHandle, deadline_ms: i64) -> Result<(), ()> {
        let mut exited = handle.exited.subscribe();
        let deadline = self.clock.sleep_until(self.clock.now_ms() + deadline_ms);
        tokio::select! {
            _ = exited.wait_for(|exited| *exited) => Ok(()),
            () = deadline => Err(()),
        }
    }

    async fn wait_unlocked(&self, dir: &Path, deadline_ms: i64) -> Result<(), ()> {
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

    /// Kills the agent's process group, when this daemon started it and so
    /// knows it. Returns whether it could.
    fn kill(&self, handle: &AgentHandle) -> bool {
        let Some(pid) = handle.pid else { return false };
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
    let stream = local_socket::connect(&dir.join(agent_dir::CTL_SOCK))
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

/// What the agent process writes to stderr, beside its directory's other
/// files: it has no terminal and no daemon pipe to write to.
pub const AGENT_LOG: &str = "agent.log";

/// Waits on a child this daemon started, so it never lingers as a zombie.
async fn reap(mut child: tokio::process::Child) {
    let _ = child.wait().await;
}

/// The home directory of the user this daemon runs as.
fn home_dir() -> Option<PathBuf> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
}

#[cfg(unix)]
pub(crate) fn kill_group(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // SAFETY: a plain signal to the process group the agent leads.
    unsafe { libc::killpg(pid, libc::SIGKILL) == 0 }
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
pub fn to_wire(row: &AgentRow) -> Agent {
    Agent {
        agent_id: row.agent.agent.clone(),
        host_id: row.agent.host.clone(),
        kind: row.kind,
        name: row.name.clone().unwrap_or_default(),
        cwd: row.cwd.clone(),
        parent: row.parent.as_ref().map(|parent| AgentParent {
            host_id: parent.host.clone(),
            agent_id: parent.agent.clone(),
        }),
        created_at_ms: row.created_at,
        lifecycle: row.lifecycle,
        exit_cause: row.exit_cause.clone(),
        phase: row.phase,
        working_on: row.working_on.as_ref().map(|text| WorkingOn {
            text: text.clone(),
            updated_at_ms: row.last_activity.unwrap_or_default(),
        }),
        // Before its first snapshot an agent has been starting since it
        // was created.
        phase_since_ms: row.phase_since.unwrap_or(row.created_at),
        producer_version: row.producer_version.clone(),
        incarnation: row.incarnation,
        git: row.git.clone(),
    }
}

pub(crate) fn ms(ms: i64) -> Duration {
    Duration::from_millis(ms.max(0) as u64)
}
