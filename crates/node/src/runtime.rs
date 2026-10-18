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
use daemon_core::FromShell;
pub use daemon_core::ingest::{INGEST_BATCH, KEPT_SEGMENTS};
use store::{AgentKey, AgentRow, Delivery, Marker, Sqlite, StoreError};
use tokio::io::{AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::sync::{Notify, oneshot, watch};
use tokio::task::JoinHandle;
use uuid::Uuid;
use wire::{
    Agent, AgentHello, AgentRef, AgentRemoved, Bootstrap, CaughtUp, CreateAgentRequest, CtlFrame,
    DumpPart, ErrorCode, ExitCause, Kind, Lifecycle, RemovalReason, Rename, SendInputResponse,
    Stop, ctl_frame, inventory_event, session_event,
};

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
/// How long an agent past its stop deadline has to act on a kill stop sent
/// over its control socket before its process is killed from outside.
const KILL_ASK_PATIENCE: Duration = Duration::from_secs(2);
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

/// An exit cause the daemon records itself, where the agent's journal does
/// not say why: the process could not be spawned, its directory is gone,
/// the daemon killed it, or it ended without writing its incarnation's end.
fn daemon_cause(cause: ExitCause, detail: Option<String>) -> wire::IncarnationEnd {
    wire::IncarnationEnd {
        cause: cause as i32,
        detail,
    }
}

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
}

/// What [`ProfileRuntime::ingest`] committed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Ingested {
    /// Journal frames committed while it waited.
    pub frames: u64,
}

/// The names this host's own agents hold, exited ones included.
fn own_names(store: &Sqlite, host: &[u8]) -> Result<HashSet<String>, StoreError> {
    Ok(store
        .agents()?
        .into_iter()
        .filter(|row| row.agent.host == host)
        .map(|row| row.name)
        .collect())
}

/// What a `Children` frame tells `parent`: the live agents directly below
/// it on any host with their incarnations, in agent id order so two frames
/// compare equal when nothing changed.
fn children_frame(store: &Sqlite, parent: &AgentKey) -> Result<wire::Children, StoreError> {
    let mut children: Vec<wire::LiveChild> = store
        .agents_of_parent(parent)?
        .into_iter()
        .filter(|row| row.lifecycle == Lifecycle::Live as i32)
        .map(|row| wire::LiveChild {
            agent_id: row.agent.agent,
            incarnation: row.incarnation,
        })
        .collect();
    children.sort_by(|a, b| a.agent_id.cmp(&b.agent_id));
    Ok(wire::Children { children })
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
            Self::Store(_) | Self::Io(_) | Self::Ingest(_) => ErrorCode::Internal,
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
    host: Uuid,
    /// The installation's generation, carried on this host's entry.
    generation: u64,
    dir: PathBuf,
    clock: Arc<dyn Clock>,
    launch: Mutex<Launch>,
    /// Every write hands its change to the core while it still holds this
    /// lock, so subscribers see changes in commit order and a subscribe,
    /// answered with it held, reads one point in that order.
    pub(crate) store: tokio::sync::Mutex<Sqlite>,
    /// The profile's core thread, started by the first call it answers.
    driver: std::sync::OnceLock<crate::driver::Driver>,
    /// For tests: awaited by a subscribe before the core reads its cut,
    /// with the store held.
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
    /// This machine's clients' unsent drafts.
    drafts: Mutex<crate::drafts::Drafts>,
    analytics: analytics::Analytics,
    /// Turns ended since the last check-in.
    turns: crate::telemetry::Turns,
    /// What each provider offers on this host.
    offers: crate::offers::Offers,
    /// What each trusted host last said its providers offer, from its own
    /// entry on its inventory.
    pub(crate) peer_providers: Mutex<HashMap<Vec<u8>, Vec<wire::ProviderOnHost>>>,
}

/// How the daemon is ending an agent's process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stopping {
    /// The turn cancelled the provider's own way, then the agent exits.
    Stop,
    /// The process group ended at once; nothing more is written.
    Kill,
}

/// What a parent's row, as this host holds it, says its children here must
/// do. See [`ProfileRuntime::parent_changed`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ParentSays {
    /// The parent exited, whatever the cause: its children stop.
    Exited,
    /// The parent is gone from its host for good: its children are
    /// stopped and deleted.
    Deleted,
}

/// An agent whose changes are not published while a delete removes it,
/// until this is dropped. A delete dropped before it settled (its caller
/// went away) unmutes too and publishes the row as it then stands, since
/// what the delete changed meanwhile reached no subscriber.
struct Muted<'a> {
    runtime: &'a ProfileRuntime,
    key: AgentKey,
    settled: bool,
}

impl<'a> Muted<'a> {
    fn new(runtime: &'a ProfileRuntime, key: AgentKey) -> Self {
        runtime.hand_over(FromShell::Mute(key.clone()));
        Self {
            runtime,
            key,
            settled: false,
        }
    }
}

impl Drop for Muted<'_> {
    fn drop(&mut self) {
        self.runtime.hand_over(FromShell::Unmute(self.key.clone()));
        if self.settled {
            return;
        }
        let (Some(me), Ok(handle)) = (
            self.runtime.me.upgrade(),
            tokio::runtime::Handle::try_current(),
        ) else {
            return;
        };
        let key = self.key.clone();
        handle.spawn(async move {
            let store = me.store.lock().await;
            let _ = me.publish_row(&store, &key);
        });
    }
}

pub(crate) struct AgentHandle {
    dir: PathBuf,
    /// The write half of the control connection, while one is open.
    pub(crate) ctl: tokio::sync::Mutex<Option<WriteHalf<LocalStream>>>,
    /// Inputs waiting for their verdict on the current connection, by
    /// input id; dropped when the connection ends.
    replies: Mutex<HashMap<Vec<u8>, oneshot::Sender<SendInputResponse>>>,
    /// Inputs written to the process and not yet answered.
    pub(crate) handed: std::sync::atomic::AtomicUsize,
    /// Dumps waiting for the agent's part, by dump id; dropped when the
    /// connection ends.
    dumps: Mutex<HashMap<Vec<u8>, oneshot::Sender<DumpPart>>>,
    hello: watch::Sender<Option<AgentHello>>,
    /// The stop the daemon asked for: a kill names the exit's cause, since
    /// a killed process writes nothing more.
    stopping: Mutex<Option<Stopping>>,
    /// The agent refused an input as exiting: it is on its way out.
    exiting: AtomicBool,
    /// The process id, when this daemon started the process.
    pid: Option<u32>,
    /// The process id on the other end of the control socket, as the
    /// operating system names it: known for an agent an earlier daemon
    /// started too.
    peer: Mutex<Option<u32>>,
    /// Whether the process this daemon started is still running. A new
    /// process has not taken its lock yet, so a free lock says nothing
    /// about it until it has exited.
    running: watch::Sender<bool>,
    pub(crate) exited: watch::Sender<bool>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
    /// The agent's live direct children, as last listed: what the next
    /// `Children` frame says.
    children: Mutex<wire::Children>,
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
            handed: std::sync::atomic::AtomicUsize::new(0),
            pid,
            peer: Mutex::new(None),
            running: watch::Sender::new(pid.is_some()),
            exited: watch::Sender::new(false),
            tasks: Mutex::new(Vec::new()),
            children: Mutex::new(wire::Children::default()),
        })
    }

    /// Tells the agent which agents directly below it are live: the list
    /// as it stands when the connection is free, so writes racing each
    /// other still end on the newest list.
    pub(crate) async fn send_children(&self, patience: Duration) {
        let _ = tokio::time::timeout(patience, async {
            if let Some(ctl) = self.ctl.lock().await.as_mut() {
                let children = self.children.lock().unwrap().clone();
                let frame = CtlFrame {
                    of: Some(ctl_frame::Of::Children(children)),
                };
                let _ = agent_dir::write_frame(ctl, &frame).await;
            }
        })
        .await;
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
            driver: std::sync::OnceLock::new(),
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
            drafts: Mutex::new(crate::drafts::Drafts::load(&dir)),
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

    /// The profile's core, from the shell's side; its thread starts here
    /// on first use.
    pub(crate) fn driver(&self) -> &crate::driver::Driver {
        self.driver.get_or_init(|| {
            crate::driver::Driver::start(
                self.me.clone(),
                self.host.as_bytes().to_vec(),
                daemon_core::StartOptions {
                    generation: self.generation,
                    agents: self.dir.join(AGENTS),
                },
            )
        })
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
            row.name = name.to_owned();
            row.parent = parent
                .as_ref()
                .map(|parent| AgentKey::new(parent.host_id.clone(), parent.agent_id.clone()));
            row.created_at_ms = now;
            row.incarnation = 1;
            row
        };
        let name = {
            let mut store = self.store.lock().await;
            if store.agent(&key)?.is_some() {
                return Err(RegistryError::AlreadyExists(id));
            }
            // Checked under the same lock as the row is written: a parent
            // that ends after this sees the child and stops it, so none
            // slips between the check and the family rule.
            if let Some(parent) = &parent
                && self.parent_ended(&store, parent)?.is_some()
            {
                return Err(RegistryError::ParentEnded(
                    Uuid::from_slice(&parent.agent_id).unwrap_or_default(),
                ));
            }
            // Chosen under the store lock, so two creates at once cannot
            // both take the same free name. Names are unique on this host,
            // counting exited agents, which stay addressable by name: a
            // given name already taken gets the next free suffix, and the
            // caller learns the name from the agent it gets back.
            let names = own_names(&store, &key.host)?;
            let name = match given {
                Some(name) => {
                    crate::names::free_suffix(&name, &|candidate| names.contains(candidate))
                }
                None => crate::names::assign_name(&|candidate| {
                    names.contains(candidate) || branches.contains(candidate)
                }),
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
            let row = row_for(&name, &request.cwd);
            self.put_row(&mut store, &row)?;
            // The worktree took a while; a parent that ended meanwhile is
            // followed now rather than leave the worktree behind.
            if let Some(parent) = &parent
                && let Some(says) = self.parent_ended(&store, parent)?
            {
                self.follow(id, says);
            }
            Some(made.base_branch)
        } else {
            None
        };

        let dir = self.agent_dir(id);
        private_dir(&dir)?;
        let launch = self.launch();
        let offered = self.offered_to(kind, &request.cwd).await;
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

    /// An agent's unsent draft, as this machine's clients last saved it.
    pub fn draft(&self, agent_id: &[u8]) -> wire::ComposerDraft {
        self.drafts.lock().unwrap().get(agent_id)
    }

    /// Forgets the draft of an agent gone for good: deleted or removed by
    /// retention, on this host or on another.
    pub(crate) fn forget_draft(&self, agent_id: &[u8]) {
        self.drafts.lock().unwrap().forget(agent_id);
    }

    /// Keeps an agent's unsent draft for this machine's clients; an empty
    /// one forgets it.
    pub fn save_draft(&self, agent_id: &[u8], draft: &wire::ComposerDraft) {
        self.drafts.lock().unwrap().save(agent_id, draft);
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
        // A host that announced which kinds it runs is not asked to start
        // another: a phone runs none.
        if let Some(entry) = self.host_entry_of(host)
            && let Some(capabilities) = &entry.capabilities
            && let Some(kind) = request.agent.as_ref().map(|agent| agent.kind())
        {
            self.runs_kind(capabilities, &entry.name, kind)?;
        }
        // No folder there is a default worth guessing: the caller asks the
        // person or names a path it knows on that host.
        if request.cwd.is_empty() {
            return Err(RegistryError::NoFarCwd(self.host_name(host)));
        }
        let mut request = request;
        if let Some(caller) = caller {
            request.parent = Some(AgentRef {
                host_id: self.host.as_bytes().to_vec(),
                agent_id: caller.as_bytes().to_vec(),
            });
        }
        request.host = Some(wire::HostRef::id(host.as_bytes().to_vec()));
        if request.agent_id.is_empty() {
            request.agent_id = Uuid::new_v4().as_bytes().to_vec();
        }
        Ok(self
            .on_peer(host, |mut client| async move {
                client.create_agent(request).await
            })
            .await?)
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
        let latest = self.last_settings(&key, previous.kind()).await?;
        let resolved = spec::carry(&previous, latest);
        let offered = self.offered_to(previous.kind(), &previous.cwd).await;
        let spec = spec::build(
            &launch,
            spec::Incarnation {
                agent_id: id.as_bytes(),
                profile_id: self.profile.as_bytes(),
                kind: previous.kind(),
                cwd: &previous.cwd,
                // The name it has now: a rename while it was not running
                // reaches its Codex thread this way.
                name: &row.name,
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
            row.exit_cause = ExitCause::Unspecified as i32;
            row.exit_detail = None;
            row.incarnation = next;
            self.put_row(&mut store, &row)?;
            // The next CaughtUp comes from this incarnation's journal.
            store.set_marker(&key, None);
        }
        self.start_process(id, &dir, &launch).await
    }

    /// The model, permission, mode and effort the agent last ran with, as
    /// its newest snapshot says them; nothing for a setting it never said.
    async fn last_settings(
        &self,
        key: &AgentKey,
        kind: Kind,
    ) -> Result<spec::Settings, RegistryError> {
        use prost::Message as _;
        let snapshot = self.store.lock().await.cut(key, 0)?.snapshot;
        let Some(body) = snapshot.map(|snapshot| snapshot.body) else {
            return Ok(spec::Settings::default());
        };
        let body = body.as_slice();
        Ok(match kind {
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
        })
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
                let cause = daemon_cause(
                    ExitCause::CouldNotStart,
                    Some(format!("the agent process: {error}")),
                );
                self.mark_exited(id, cause).await?;
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

    /// The handle for an agent an earlier daemon started, serving its
    /// tools socket again: the agent's MCP server dials it for every fleet
    /// tool, and the earlier daemon's listener went with that daemon. An
    /// agent whose socket cannot be bound keeps running without its fleet
    /// tools, as it did before, and says so in the log.
    fn adopted_handle(&self, id: AgentId) -> Arc<AgentHandle> {
        let dir = self.agent_dir(id);
        let handle = AgentHandle::new(dir.clone(), None);
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
        ))
    }

    // --- stop, delete, rename --------------------------------------------

    /// Stops a live agent's process now: the running turn is cancelled the
    /// provider's own way and the agent exits, resumable. A stop that
    /// overruns the stop deadline kills. An exited agent is left as it is.
    ///
    /// The stop acts on this agent alone. Its children, on this host or
    /// another, follow by the family rule once their host sees it exited
    /// (see [`Self::parent_changed`]).
    pub async fn stop(&self, id: AgentId) -> Result<Agent, RegistryError> {
        let operation = self.operation(id);
        let _operation = operation.lock().await;
        self.stop_locked(id).await
    }

    /// Kills a live agent's process group at once, writing nothing more,
    /// as the stop deadline does. For tearing a runtime down: no client
    /// call reaches it.
    pub async fn kill_agent(&self, id: AgentId) -> Result<Agent, RegistryError> {
        let operation = self.operation(id);
        let _operation = operation.lock().await;
        let Some(handle) = self.agents.lock().unwrap().get(&id).cloned() else {
            return self.agent(id).await;
        };
        self.kill_locked(id, &handle).await?;
        self.agent(id).await
    }

    async fn stop_locked(&self, id: AgentId) -> Result<Agent, RegistryError> {
        let Some(handle) = self.agents.lock().unwrap().get(&id).cloned() else {
            return self.agent(id).await;
        };
        *handle.stopping.lock().unwrap() = Some(Stopping::Stop);
        let frame = CtlFrame {
            of: Some(ctl_frame::Of::Stop(Stop { kill: false })),
        };
        let launch = self.launch();
        // A failed write means the connection is already going; the
        // deadline below still applies.
        let _ = handle.write_ctl(&frame, ms(launch.ctl_write_ms)).await;
        if self
            .wait_exit(&handle, launch.stop_deadline_ms)
            .await
            .is_err()
        {
            // Past the deadline the stop becomes a kill.
            self.kill_locked(id, &handle).await?;
        }
        self.agent(id).await
    }

    /// Kills the agent: asked of the agent first, which kills its
    /// provider's group and exits, and only if it does not, done to its
    /// process from outside, as the process on the other end of its
    /// control socket.
    async fn kill_locked(
        &self,
        id: AgentId,
        handle: &Arc<AgentHandle>,
    ) -> Result<(), RegistryError> {
        *handle.stopping.lock().unwrap() = Some(Stopping::Kill);
        let kill = CtlFrame {
            of: Some(ctl_frame::Of::Stop(Stop { kill: true })),
        };
        let _ = handle
            .write_ctl(&kill, ms(self.launch().ctl_write_ms))
            .await;
        if self
            .wait_exit(handle, KILL_ASK_PATIENCE.as_millis() as i64)
            .await
            .is_err()
        {
            if !self.kill(handle) {
                return Err(RegistryError::StopTimeout(id));
            }
            self.wait_exit(handle, KILL_PATIENCE.as_millis() as i64)
                .await
                .map_err(|_| RegistryError::StopTimeout(id))?;
        }
        Ok(())
    }

    /// Deletes an agent: stops its process if live, then removes its rows
    /// and its directory. Its children, on this host or another, follow by
    /// the family rule once their host sees it gone (see
    /// [`Self::parent_changed`]).
    pub async fn delete(&self, id: AgentId) -> Result<(), RegistryError> {
        let operation = self.operation(id);
        let _operation = operation.lock().await;
        let key = self.key(id);
        if self.store.lock().await.agent(&key)?.is_none() {
            return Err(RegistryError::NotFound(id));
        }
        // The stop is part of the delete: a subscriber that read its exit
        // would offer to resume an agent about to be removed. Nothing about
        // the agent is published until its removal is; a delete that fails
        // publishes the row as it then stands.
        let mut muted = Muted::new(self, key.clone());
        let deleted = self.delete_muted(id, key.clone()).await;
        muted.settled = true;
        drop(muted);
        if deleted.is_err() {
            let store = self.store.lock().await;
            let _ = self.publish_row(&store, &key);
        }
        deleted
    }

    async fn delete_muted(&self, id: AgentId, key: AgentKey) -> Result<(), RegistryError> {
        self.stop_locked(id).await?;
        // Rows first: a crash between the two leaves a directory with no
        // row, which the next sweep removes.
        {
            let mut store = self.store.lock().await;
            let parent = store.agent(&key)?.and_then(|row| row.parent);
            store.delete_agent(&key)?;
            self.removed(&key, RemovalReason::Deleted);
            self.recount_from(&mut store, parent)?;
            self.parent_changed(&store, &key, ParentSays::Deleted)?;
        }
        match std::fs::remove_dir_all(self.agent_dir(id)) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        self.operations.lock().unwrap().remove(&id);
        self.forget_draft(id.as_bytes());
        Ok(())
    }

    // --- the family rule -------------------------------------------------

    /// Applies the family rule to this host's own children of `parent`,
    /// whose row has just changed as `says`. A host enforces the rule only
    /// for its own agents and only on what the parent's row says: exited
    /// stops each live child, deleted deletes each child (stopping it
    /// first). A parent that is merely out of reach says neither, so its
    /// children carry on and their reports wait in the outbox. Each act
    /// runs on a task of its own, once the caller lets go of the store.
    pub(crate) fn parent_changed(
        &self,
        store: &Sqlite,
        parent: &AgentKey,
        says: ParentSays,
    ) -> Result<(), StoreError> {
        for child in store.agents_of_parent(parent)? {
            if child.agent.host != self.host.as_bytes()
                || (says == ParentSays::Exited && child.lifecycle != Lifecycle::Live as i32)
            {
                continue;
            }
            if let Ok(id) = Uuid::from_slice(&child.agent.agent) {
                self.follow(id, says);
            }
        }
        Ok(())
    }

    /// Whether `parent` has already ended as this host sees it: exited by
    /// its row, or deleted. Another host's row counts only once that host's
    /// listing has caught up, since a row kept from before may be stale;
    /// and a missing row means deleted only for this host's own agents,
    /// since another host's row may simply not have arrived yet.
    fn parent_ended(
        &self,
        store: &Sqlite,
        parent: &AgentRef,
    ) -> Result<Option<ParentSays>, StoreError> {
        let key = AgentKey::new(parent.host_id.clone(), parent.agent_id.clone());
        let own = key.host == self.host.as_bytes();
        if !own && !Uuid::from_slice(&key.host).is_ok_and(|host| self.host_ready(host)) {
            return Ok(None);
        }
        Ok(match store.agent(&key)? {
            Some(row) if row.lifecycle == Lifecycle::Exited as i32 => Some(ParentSays::Exited),
            None if own => Some(ParentSays::Deleted),
            _ => None,
        })
    }

    /// Stops or deletes one of this host's own agents because its parent's
    /// row said so.
    pub(crate) fn follow(&self, child: AgentId, says: ParentSays) {
        let me = self.me.clone();
        let mut read = self.journals_read.subscribe();
        tokio::spawn(async move {
            // Not before the start's sweep has adopted every running agent:
            // a stop then would find no process to end.
            let _ = read.wait_for(|read| *read).await;
            let Some(runtime) = me.upgrade() else { return };
            let done = match says {
                ParentSays::Exited => runtime.stop(child).await.map(drop),
                ParentSays::Deleted => runtime.delete(child).await,
            };
            match done {
                Ok(()) | Err(RegistryError::NotFound(_)) => {}
                Err(error) => {
                    tracing::warn!(%child, %error, ?says, "following its parent failed");
                }
            }
        });
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
            if row.name != name && own_names(&store, &key.host)?.contains(name) {
                return Err(RegistryError::NameTaken(name.to_owned()));
            }
            row.name = name.to_owned();
            self.put_row(&mut store, &row)?;
            row
        };
        let handle = self.agents.lock().unwrap().get(&id).cloned();
        if let Some(handle) = handle
            && row.kind == Kind::Codex as i32
        {
            let frame = CtlFrame {
                of: Some(ctl_frame::Of::Rename(Rename {
                    name: row.name.clone(),
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
                    self.agents.lock().unwrap().insert(id, handle.clone());
                    self.watch(id, handle, None);
                    report.unanswered.push(id);
                }
                Look::Exited => {
                    self.ingest(id).await?;
                    // Unless its journal said why, it ended while no daemon
                    // was there to see.
                    let cause = daemon_cause(ExitCause::EndedWhileAway, None);
                    self.mark_exited_if_live(id, cause).await?;
                    report.exited.push(id);
                }
                Look::NoDirectory => {
                    let cause = daemon_cause(ExitCause::DirectoryFailed, Some("it is gone".into()));
                    self.mark_exited_if_live(id, cause).await?;
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

    /// Asks the core to read the agent's journal from its row's cursor to
    /// the end, and waits until it has: a batch at a time, each committed
    /// in one transaction and then told to its subscribers. At the end of
    /// the journal, the first time after each Hello, the core tells them
    /// CaughtUp. Segments that lie wholly below a cursor that has reached
    /// the drive are deleted, but for the newest few.
    pub async fn ingest(&self, id: AgentId) -> Result<Ingested, RegistryError> {
        match self.driver().ingest(id).await {
            Ok(frames) => Ok(Ingested { frames }),
            Err(why) => Err(RegistryError::Ingest(why)),
        }
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

    /// Wakes a loop the shell still runs, at the core's word.
    pub(crate) fn wake(&self, wake: daemon_core::Wake) {
        match wake {
            daemon_core::Wake::Deliveries => self.deliveries_due.notify_one(),
            daemon_core::Wake::Notifications => self.notifications_due.notify_one(),
        }
    }

    /// Hands the core a change committed under the store, which the caller
    /// still holds.
    pub(crate) fn hand_over(&self, change: FromShell) {
        self.driver().hand_over(change);
    }

    /// Tells the agent's subscribers of one event, committed under the
    /// store the caller still holds.
    pub(crate) fn publish(&self, agent: &AgentKey, of: session_event::Of) {
        self.hand_over(FromShell::Session {
            agent: agent.clone(),
            event: Arc::new(event(of)),
        });
    }

    /// The agent is gone from the store the caller still holds: its
    /// streams end and the inventory says why.
    pub(crate) fn removed(&self, agent: &AgentKey, reason: RemovalReason) {
        let removed = AgentRemoved {
            host_id: agent.host.clone(),
            agent_id: agent.agent.clone(),
            reason: reason as i32,
        };
        self.hand_over(FromShell::Removed {
            agent: agent.clone(),
            event: Arc::new(inventory(inventory_event::Of::AgentRemoved(removed))),
        });
    }

    /// Whether any stream follows the agent now.
    pub(crate) fn watched(&self, agent: &AgentKey) -> bool {
        self.driver().watched(agent)
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
            peer,
        } = connected;
        *handle.peer.lock().unwrap() = peer;
        *handle.ctl.lock().await = Some(writer);
        handle.hello.send_replace(Some(hello.clone()));
        self.driver()
            .feed(daemon_core::Input::AgentHello {
                agent: daemon_core::AgentId(*id.as_bytes()),
                hello,
            })
            .await;
        let key = self.key(id);
        let children = children_frame(&*self.store.lock().await, &key).unwrap_or_default();
        let any = !children.children.is_empty();
        *handle.children.lock().unwrap() = children;
        if any {
            handle.send_children(ms(self.launch().ctl_write_ms)).await;
        }
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

    /// The process is gone: ingest what it left, record the exit, close its
    /// tools socket and let whoever waits on it go.
    async fn exited(&self, id: AgentId, handle: &Arc<AgentHandle>) {
        if let Err(error) = self.ingest(id).await {
            tracing::warn!(agent = %id, %error, "ingesting an exited agent's journal failed");
        }
        // Unless the journal, now read to its end, said why: a kill the
        // daemon made, else the process ended without saying.
        let cause = match *handle.stopping.lock().unwrap() {
            Some(Stopping::Kill) => ExitCause::Killed,
            _ => ExitCause::EndedUnexpectedly,
        };
        let cause = daemon_cause(cause, None);
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

    async fn mark_exited(
        &self,
        id: AgentId,
        fallback: wire::IncarnationEnd,
    ) -> Result<(), StoreError> {
        let key = self.key(id);
        let mut store = self.store.lock().await;
        let Some(row) = store.agent(&key)? else {
            return Ok(());
        };
        self.record_exit(&mut store, row, fallback)?;
        // Everything the process wrote is committed: nothing more comes.
        self.caught_up(&mut store, &key)
    }

    async fn mark_exited_if_live(
        &self,
        id: AgentId,
        fallback: wire::IncarnationEnd,
    ) -> Result<(), StoreError> {
        let key = self.key(id);
        let mut store = self.store.lock().await;
        let Some(row) = store.agent(&key)? else {
            return Ok(());
        };
        if row.lifecycle != Lifecycle::Exited as i32 {
            self.record_exit(&mut store, row, fallback)?;
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
        self.publish_row(store, &row.agent)?;
        self.recount_from(store, row.parent.clone())
    }

    /// Counts the live descendants of `parent` again, and of each own
    /// ancestor above it, writing and publishing every count that changed;
    /// a count that did not change leaves those above it unchanged too. A
    /// replica ancestor's count is its origin's, which hears of the change
    /// from this host's inventory. A running `parent` also hears which of
    /// its children are live, when that changed.
    pub(crate) fn recount_from(
        &self,
        store: &mut Sqlite,
        mut parent: Option<AgentKey>,
    ) -> Result<(), StoreError> {
        if let Some(key) = &parent
            && key.host == self.host.as_bytes()
        {
            self.tell_children(store, key)?;
        }
        while let Some(key) = parent {
            if key.host != self.host.as_bytes() {
                break;
            }
            let Some(row) = store.agent(&key)? else {
                break;
            };
            let count = store.live_descendants(&key)?;
            if count == row.descendants {
                break;
            }
            store.set_descendants(&key, count)?;
            self.publish_row(store, &key)?;
            parent = row.parent;
        }
        Ok(())
    }

    /// Tells a running agent which agents directly below it are now live,
    /// when that changed: a one-shot agent waits for the last report of
    /// every child incarnation it was told of.
    fn tell_children(&self, store: &Sqlite, key: &AgentKey) -> Result<(), StoreError> {
        let Ok(id) = AgentId::from_slice(&key.agent) else {
            return Ok(());
        };
        let Some(handle) = self.agents.lock().unwrap().get(&id).cloned() else {
            return Ok(());
        };
        let children = children_frame(store, key)?;
        {
            let mut told = handle.children.lock().unwrap();
            if *told == children {
                return Ok(());
            }
            *told = children;
        }
        let patience = ms(self.launch().ctl_write_ms);
        tokio::spawn(async move { handle.send_children(patience).await });
        Ok(())
    }

    /// Marks the row exited, then settles what the process leaves: its
    /// unsent notifications go, since no push may name an agent that is
    /// gone, and a child whose incarnation ended without sending its last
    /// report tells its parent it failed: its turn was open, or it ended
    /// idle, as a sub-agent stopped while it waits for its own child does.
    /// A turn end marked last already told it.
    ///
    /// The row keeps the cause its journal's last step put there at commit;
    /// `fallback` is the daemon's own, for an incarnation whose journal
    /// stops without saying why.
    fn record_exit(
        &self,
        store: &mut Sqlite,
        mut row: AgentRow,
        fallback: wire::IncarnationEnd,
    ) -> Result<(), StoreError> {
        row.lifecycle = Lifecycle::Exited as i32;
        if row.exit_cause == ExitCause::Unspecified as i32 {
            row.exit_cause = fallback.cause;
            row.exit_detail = fallback.detail;
        }
        let cause = wire::IncarnationEnd {
            cause: row.exit_cause,
            detail: row.exit_detail.clone(),
        };
        self.put_row(store, &row)?;
        self.parent_changed(store, &row.agent, ParentSays::Exited)?;
        store.remove_notifications(&row.agent.agent)?;
        let Some(parent) = &row.parent else {
            return Ok(());
        };
        // As committed: the phase at exit says nothing here, since a
        // provider's exit leaves it starting whether or not the turn ended.
        let held = store.agent(&row.agent)?.unwrap_or(row.clone());
        if !held.owes_report {
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
            end: wire::child_report::End::Exited(wire::Exited { ended: Some(cause) }),
        })?;
        self.deliveries_due.notify_one();
        Ok(())
    }

    /// Tells inventory subscribers what the row now says.
    pub(crate) fn publish_row(&self, store: &Sqlite, key: &AgentKey) -> Result<(), StoreError> {
        if let Some(row) = store.agent(key)? {
            self.hand_over(FromShell::Row {
                agent: key.clone(),
                event: Arc::new(inventory(inventory_event::Of::Agent(to_wire(&row)))),
            });
        }
        Ok(())
    }

    /// Sets the agent's CaughtUp flag and tells subscribers the marker,
    /// unless the flag is already set: a subscriber that joined since read
    /// it at its cut, and one that joined before was told then.
    fn caught_up(&self, store: &mut Sqlite, key: &AgentKey) -> Result<(), StoreError> {
        if store.markers().get(key) == Some(&Marker::CaughtUp) {
            return Ok(());
        }
        let Some(row) = store.agent(key)? else {
            return Ok(());
        };
        store.set_marker(key, Some(Marker::CaughtUp));
        let revision = row.next_revision.saturating_sub(1);
        self.publish(key, session_event::Of::CaughtUp(CaughtUp { revision }));
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

    /// Kills the agent's process group: the process this daemon started,
    /// or the one on the other end of its control socket. Returns whether
    /// it could.
    fn kill(&self, handle: &AgentHandle) -> bool {
        let Some(pid) = handle.pid.or(*handle.peer.lock().unwrap()) else {
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

/// What the agent process writes to stderr, beside its directory's other
/// files: it has no terminal and no daemon pipe to write to.
pub const AGENT_LOG: &str = "agent.log";

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

pub(crate) fn ms(ms: i64) -> Duration {
    Duration::from_millis(ms.max(0) as u64)
}
