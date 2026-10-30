//! `amux supervise`: the daemon's parent.
//!
//! It restarts the daemon whenever it exits, so agents survive any daemon
//! death, and under `updates: auto` it installs releases. What to start,
//! stop, install, activate and roll back is decided by
//! [`supervisor_core::Supervisor`], whose module documentation states the
//! rules; this module is its driver: the daemon processes and their
//! activation pipe, the timers the core asks for, the control socket, the
//! feed (read within its size limits and verified here) and the files
//! beside the binary.
//!
//! - One pipe joins it to the child: the child writes `prepared` after
//!   migrating and looking without writing, the supervisor answers `go`,
//!   and end of file means the supervisor is gone and the daemon exits.
//! - The swap stages the release beside the binary, verifies hash and
//!   signature, hard-links the current binary to prev and renames the
//!   staged binary over the path; activation deletes prev and fsyncs the
//!   directory before `go`; a rollback writes the rejected version and
//!   fsyncs it before renaming prev back.
//! - After `go`, a supervisor running a binary other than the installed one
//!   execs the installed binary in supervise mode, keeping pid, child, pipe
//!   and lock, so the supervisor is never the stale part for long.
//! - Stopping the child is a signal, a stop deadline, then a kill.
//!
//! Counters and timers live in the core's memory. The supervisor holds its
//! own lock; the installation lock stays the daemon's.
//!
//! On Windows a running executable cannot be renamed over, so the
//! supervisor stops the child before the swap and moves the binary aside
//! to prev by rename, and its own update is a spawn-then-exit.

mod child;
mod control;
mod files;
mod keep_awake;
pub mod login;

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::ffi::OsString;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use agent_dir::Clock;
use child::Child;
pub use child::Inherited;
use control::Asked;
pub use control::{Request, SUPERVISOR_SOCKET, ask};
use core_ring::DumpLimit;
pub use files::{FileId, Files, SUPERVISOR_LOCK};
pub use keep_awake::{KeepAwake, REASON as KEEP_AWAKE_REASON};
use semver::Version;
use sha2::{Digest as _, Sha256};
pub use supervisor_core::StepBudget;
use supervisor_core::{
    self as core, AskId, DaemonId, INHERITED_DAEMON, JobId, JobResult, Listed, Output, PrevFound,
    Start, Supervisor, TimerId,
};
use tokio::io::AsyncWriteExt as _;
use tokio::sync::{mpsc, oneshot};

use crate::install::{private_dir, sync_dir};
use crate::profiles;
use crate::release::{self, Manifest, Release};
use crate::window::{self, Window};

/// The supervisor's timings and thresholds; the defaults are the
/// parameters table's starting points.
#[derive(Clone, Debug)]
pub struct Params {
    pub check_interval: Duration,
    /// Starts that never activate, while prev exists, before rolling back.
    pub rollback_after: u32,
    /// From spawn to `prepared`.
    pub start_deadline: Duration,
    /// From the stop signal to the kill.
    pub stop_deadline: Duration,
    pub backoff_first: Duration,
    pub backoff_max: Duration,
    /// A child up this long starts the backoff over.
    pub backoff_reset: Duration,
    /// How long one step of the supervisor's core may take before it is
    /// reported with a dump.
    pub step_budget: StepBudget,
}

impl Default for Params {
    fn default() -> Self {
        Self {
            check_interval: Duration::from_secs(60 * 60),
            // 3 until failed starts in the field say otherwise.
            rollback_after: 3,
            start_deadline: Duration::from_secs(60),
            stop_deadline: Duration::from_secs(30),
            backoff_first: Duration::from_secs(1),
            backoff_max: Duration::from_secs(30),
            backoff_reset: Duration::from_secs(60),
            step_budget: StepBudget::SUPERVISOR,
        }
    }
}

/// What the supervisor may install, read again before every check so a
/// changed channel takes effect without a restart.
#[derive(Clone, Debug)]
pub struct UpdatePolicy {
    /// Whether the hourly tick installs releases (`updates: auto`); without
    /// it only `amux update` does.
    pub auto: bool,
    /// None when this build trusts no release key: nothing can be installed.
    pub source: Option<UpdateSource>,
}

pub type UpdatesFn = Arc<dyn Fn() -> UpdatePolicy + Send + Sync>;

/// Where releases come from.
#[derive(Clone, Debug)]
pub struct UpdateSource {
    /// The channel followed, which the manifest must be signed for.
    pub channel: String,
    /// The channel's manifest.
    pub manifest_url: String,
    /// The keys a release signature must verify against one of.
    pub keys: Vec<[u8; 32]>,
}

pub struct SuperviseOptions {
    /// The canonical install path: what the child runs, what the swap
    /// replaces, and what the supervisor execs after an update.
    pub binary: PathBuf,
    /// Arguments before the subcommand, passed to the child and to the
    /// next supervisor, such as `--config <path>`.
    pub args: Vec<OsString>,
    /// The installation's data directory: the supervisor's lock, and the
    /// host id that places it in a rollout.
    pub data_dir: PathBuf,
    /// This binary's version.
    pub running: Version,
    pub target: String,
    pub updates: UpdatesFn,
    /// Whether to hold the machine's sleep assertion (`keep_awake`).
    pub keep_awake: bool,
    pub clock: Arc<dyn Clock>,
    pub params: Params,
    /// What the supervisor that exec'd this one handed over.
    pub inherited: Option<Inherited>,
}

#[derive(Debug, thiserror::Error)]
pub enum SuperviseError {
    #[error("another amux supervisor (pid {pid}) holds {}", .path.display())]
    Busy { path: PathBuf, pid: String },
    #[error("the supervisor's lock {}: {error}", .path.display())]
    Lock { path: PathBuf, error: io::Error },
    #[error("taking over from the previous supervisor: {0}")]
    Inherit(io::Error),
    #[error("{what}: {error}")]
    Files {
        what: &'static str,
        error: io::Error,
    },
}

fn files_error(what: &'static str) -> impl FnOnce(io::Error) -> SuperviseError {
    move |error| SuperviseError::Files { what, error }
}

/// Supervises the daemon until the supervisor is asked to stop, then stops
/// the daemon and returns. After an update it execs the new binary instead
/// and does not return.
pub async fn supervise(options: SuperviseOptions) -> Result<(), SuperviseError> {
    let SuperviseOptions {
        binary,
        args,
        data_dir,
        running,
        target,
        updates,
        keep_awake,
        clock,
        params,
        inherited,
    } = options;
    let _awake = keep_awake.then(KeepAwake::hold);
    let lock = match inherited.and_then(|inherited| inherited.lock) {
        Some(fd) => SupervisorLock::inherit(fd, &data_dir)?,
        None => SupervisorLock::acquire(&data_dir, inherited.is_some())?,
    };
    let files = Files::beside(&binary);
    // What this process runs: the binary at the path when it started.
    let mine = FileId::of(&binary).map_err(files_error("reading the installed binary"))?;
    files.clear_leftovers();
    let prev = if !files.prev_exists() {
        PrevFound::None
    } else if files.prev_is(&binary) {
        tracing::info!("the swap never happened: removing prev");
        PrevFound::SameAsBinary
    } else {
        PrevFound::Other
    };
    let rejected = files.rejected();
    if prev == PrevFound::Other && inherited.is_none() {
        if rejected.as_ref() == Some(&running) {
            tracing::info!(rejected = %running, "finishing a rollback");
        } else {
            tracing::info!(
                installed = %running,
                "an update is in progress: counting failed starts against it"
            );
        }
    }
    let mut daemons = HashMap::new();
    if let Some(inherited) = inherited {
        let child = Child::inherit(&inherited).map_err(SuperviseError::Inherit)?;
        daemons.insert(INHERITED_DAEMON, child);
    }
    let mut out = Vec::new();
    let core = Supervisor::start(
        Start {
            params: params.for_core(),
            target,
            running,
            prev,
            rejected,
            inherited: inherited.is_some(),
            now_ms: millis_now(&*clock),
        },
        &mut out,
    );
    let (jobs, finished) = mpsc::unbounded_channel();
    let mut driver = Driver {
        core,
        files,
        binary,
        args,
        data_dir,
        updates,
        clock,
        mine,
        lock,
        daemons,
        timers: BTreeMap::new(),
        asks: HashMap::new(),
        next_ask: 0,
        jobs,
        pending: VecDeque::new(),
        dumps: Dumps::default(),
    };
    if driver.execute(out)?.is_some() {
        return Ok(());
    }
    driver.run(finished).await
}

/// The supervisor core's driver: the daemon processes and their pipes, the
/// timers it asked for, the control socket's waiting callers, the feed and
/// the files beside the binary.
struct Driver {
    core: Supervisor,
    files: Files,
    binary: PathBuf,
    args: Vec<OsString>,
    data_dir: PathBuf,
    updates: UpdatesFn,
    clock: Arc<dyn Clock>,
    mine: FileId,
    lock: SupervisorLock,
    /// The daemons the core started, by its name for them. The core starts
    /// one only when none lives.
    daemons: HashMap<DaemonId, Child>,
    timers: BTreeMap<TimerId, u64>,
    asks: HashMap<AskId, oneshot::Sender<String>>,
    next_ask: u64,
    /// Where the feed's jobs report.
    jobs: mpsc::UnboundedSender<(JobId, JobResult)>,
    /// What executing outputs produced, stepped before anything new.
    pending: VecDeque<core::Input>,
    dumps: Dumps,
}

/// Prefixes the reason the supervisor core asked for a dump.
pub const SUPERVISOR_CORE_DUMP: &str = "the supervisor core: ";

/// The dumps the supervisor core asked for, written once per kind in each
/// interval (a daemon restarting in a loop meets the same broken invariant
/// at every restart), and the window each is written from.
#[derive(Default)]
struct Dumps {
    limit: DumpLimit,
    window: Window<Supervisor, core::Input>,
}

impl Dumps {
    /// Steps one input through the core, keeping it in the window.
    fn step(&mut self, core: &mut Supervisor, input: core::Input, out: &mut Vec<Output>) {
        self.window.record(core, &input);
        core.step(input, out);
    }

    /// Writes the dump the core asked for at `now_ms` under `data_dir`'s
    /// reports, unless the limit holds it back: the reason, the core's
    /// state when the window began, every input since and the state now.
    /// The supervisor's state holds manifests, paths, exit codes and
    /// failure strings, the same as its log, and no secret. Returns the
    /// file, or why it could not be written.
    fn dump(
        &mut self,
        reason: &core::DumpReason,
        now_ms: u64,
        core: &Supervisor,
        data_dir: &Path,
    ) -> Option<io::Result<PathBuf>> {
        let said = self.reason(reason, now_ms)?;
        let text = self.window.render("the supervisor core", &said, core);
        let written = window::write(&data_dir.join(crate::REPORTS), "supervisor", &text);
        match &written {
            Ok(file) => tracing::error!(
                reason = %said,
                file = %file.display(),
                "the supervisor core asked for a dump"
            ),
            Err(error) => tracing::error!(
                reason = %said,
                %error,
                "the supervisor core asked for a dump, which could not be written"
            ),
        }
        Some(written)
    }

    /// The reason to report for `reason` asked for at `now_ms`, or `None`
    /// when it is held back.
    fn reason(&mut self, reason: &core::DumpReason, now_ms: u64) -> Option<String> {
        self.limit
            .reason(SUPERVISOR_CORE_DUMP, reason, now_ms)
            .map(|admitted| admitted.reason)
    }
}

/// The supervisor is done.
struct Done;

impl Driver {
    async fn run(
        mut self,
        mut finished: mpsc::UnboundedReceiver<(JobId, JobResult)>,
    ) -> Result<(), SuperviseError> {
        let mut terminate = std::pin::pin!(terminated());
        // The signal is heard once; the daemon's stop that follows is
        // stepped like anything else.
        let mut signalled = false;
        let mut asked = control::serve(&self.data_dir);
        loop {
            while let Some(input) = self.pending.pop_front() {
                if self.step(input)?.is_some() {
                    return Ok(());
                }
            }
            let timer = self
                .timers
                .iter()
                .min_by_key(|(_, at)| **at)
                .map(|(id, at)| (*id, *at));
            let sleep = timer.map(|(_, at)| {
                self.clock
                    .sleep_until(i64::try_from(at).unwrap_or(i64::MAX))
            });
            // The core keeps at most one daemon alive.
            let watched = self.daemons.iter_mut().next();
            let input = match watched {
                Some((&daemon, child)) => {
                    let exited = child.exited();
                    tokio::select! {
                        () = &mut terminate, if !signalled => {
                            signalled = true;
                            core::Input::Terminate
                        }
                        Some(asked) = asked.recv() => self.asked(asked),
                        Some((job, result)) = finished.recv() => core::Input::JobDone { job, result },
                        code = exited => {
                            self.daemons.remove(&daemon);
                            core::Input::DaemonExited { daemon, code }
                        }
                        () = child.prepared() => core::Input::Prepared { daemon },
                        () = async { sleep.expect("a timer").await }, if sleep.is_some() => {
                            let (id, _) = timer.expect("a timer");
                            self.timers.remove(&id);
                            core::Input::Timer(id)
                        }
                    }
                }
                None => tokio::select! {
                    () = &mut terminate, if !signalled => {
                            signalled = true;
                            core::Input::Terminate
                        }
                    Some(asked) = asked.recv() => self.asked(asked),
                    Some((job, result)) = finished.recv() => core::Input::JobDone { job, result },
                    () = async { sleep.expect("a timer").await }, if sleep.is_some() => {
                        let (id, _) = timer.expect("a timer");
                        self.timers.remove(&id);
                        core::Input::Timer(id)
                    }
                },
            };
            if self.step(input)?.is_some() {
                return Ok(());
            }
        }
    }

    fn asked(&mut self, asked: Asked) -> core::Input {
        self.next_ask += 1;
        let ask = AskId(self.next_ask);
        self.asks.insert(ask, asked.answer);
        core::Input::Asked {
            ask,
            request: match asked.request {
                Request::Check => core::Request::Check { retry: false },
                Request::Retry => core::Request::Check { retry: true },
                Request::Stop => core::Request::Stop,
            },
        }
    }

    /// The clock, then one input, through the core; its outputs done.
    fn step(&mut self, input: core::Input) -> Result<Option<Done>, SuperviseError> {
        let mut out = Vec::new();
        let now_ms = millis_now(&*self.clock);
        let core = &mut self.core;
        self.dumps
            .step(core, core::Input::Tick { now_ms }, &mut out);
        self.dumps.step(core, input, &mut out);
        self.execute(out)
    }

    fn execute(&mut self, out: Vec<Output>) -> Result<Option<Done>, SuperviseError> {
        for output in out {
            match output {
                Output::Spawn { daemon } => match Child::spawn(&self.binary, &self.args) {
                    Ok(spawned) => {
                        tracing::info!(pid = spawned.pid, "started the daemon");
                        self.daemons.insert(daemon, spawned);
                    }
                    Err(error) => {
                        tracing::warn!(%error, "could not start the daemon");
                        self.pending.push_back(core::Input::StartFailed {
                            daemon,
                            why: error.to_string(),
                        });
                    }
                },
                Output::Go { daemon } => {
                    let Some(child) = self.daemons.get_mut(&daemon) else {
                        continue;
                    };
                    match child.go() {
                        Ok(()) => {
                            tracing::info!(pid = child.pid, "the daemon is prepared: go");
                            self.pending.push_back(core::Input::Activated { daemon });
                        }
                        // It is going away; its exit comes next.
                        Err(error) => tracing::warn!(%error, "could not tell the daemon go"),
                    }
                }
                Output::Stop { daemon } => {
                    if let Some(child) = self.daemons.get_mut(&daemon) {
                        tracing::info!(pid = child.pid, "stopping the daemon");
                        child.ask_to_stop();
                    }
                }
                Output::Kill { daemon } => {
                    if let Some(child) = self.daemons.get_mut(&daemon) {
                        tracing::warn!(
                            pid = child.pid,
                            "the daemon did not stop in time: killing it"
                        );
                        child.kill();
                    }
                }
                Output::List { job, auto_only } => self.list(job, auto_only),
                Output::Fetch { job, release } => self.fetch(job, release),
                Output::RemovePrev => self
                    .files
                    .remove_prev()
                    .map_err(files_error("removing prev"))?,
                Output::RetirePrev => self
                    .files
                    .retire_prev(&self.mine)
                    .map_err(files_error("removing prev"))?,
                Output::WriteRejected { version } => {
                    tracing::warn!(rejected = %version, "rolling back to prev");
                    self.files
                        .write_rejected(&version)
                        .map_err(files_error("recording the rejected build"))?;
                    tracing::info!(rejected = %version, "recorded the rejected build");
                }
                Output::RestorePrev => {
                    self.files
                        .restore_prev(&self.binary, &self.mine)
                        .map_err(files_error("putting prev back"))?;
                    tracing::info!("put prev back over the binary");
                }
                Output::InstallStaged => {
                    tracing::info!("installing the staged release");
                    self.files
                        .install_staged(&self.binary)
                        .map_err(files_error("installing the staged binary"))?;
                }
                Output::BecomeInstalled { daemon } => {
                    if let Some(child) = self.daemons.get(&daemon) {
                        self.become_installed(child);
                    }
                }
                Output::Answer { ask, text } => {
                    if let Some(answer) = self.asks.remove(&ask) {
                        let _ = answer.send(text);
                    }
                }
                Output::ArmTimer { id, at_ms } => {
                    self.timers.insert(id, at_ms);
                }
                Output::CancelTimer { id } => {
                    self.timers.remove(&id);
                }
                Output::Exit => return Ok(Some(Done)),
                Output::Fail { what, why } => {
                    return Err(SuperviseError::Files {
                        what,
                        error: io::Error::other(why),
                    });
                }
                Output::Dump { reason } => {
                    let now_ms = millis_now(&*self.clock);
                    self.dumps.dump(&reason, now_ms, &self.core, &self.data_dir);
                }
            }
        }
        Ok(None)
    }

    /// Reads the channel's manifest and verifies it, off the loop.
    fn list(&self, job: JobId, auto_only: bool) {
        let policy = (self.updates)();
        let jobs = self.jobs.clone();
        let data_dir = self.data_dir.clone();
        tokio::spawn(async move {
            let listed = if auto_only && !policy.auto {
                Listed::Off
            } else {
                match policy.source {
                    None => Listed::Nothing(
                        "this build trusts no release key, so it installs nothing".into(),
                    ),
                    Some(source) => match list(&source).await {
                        Ok(manifest) => Listed::Manifest {
                            manifest,
                            host: rollout_host(&data_dir),
                        },
                        Err(error) => {
                            tracing::warn!(%error, "the update check failed");
                            Listed::Failed(error.to_string())
                        }
                    },
                }
            };
            let _ = jobs.send((job, JobResult::Listed(listed)));
        });
    }

    /// Downloads and verifies a release beside the binary, off the loop.
    fn fetch(&self, job: JobId, release: Release) {
        let jobs = self.jobs.clone();
        let staged = self.files.staged.clone();
        tokio::spawn(async move {
            let fetched = match fetch(&release, &staged).await {
                Ok(()) => Ok(()),
                Err(error) => {
                    tracing::warn!(%error, "the update check failed");
                    let _ = std::fs::remove_file(&staged);
                    Err(error.to_string())
                }
            };
            let _ = jobs.send((job, JobResult::Fetched(fetched)));
        });
    }

    /// Replaces this process with the installed binary in supervise mode,
    /// keeping the child, the pipe and the lock. A failure leaves this
    /// supervisor running, which is still correct.
    fn become_installed(&self, child: &Child) {
        let inherited = match child.hand_over(&self.lock.file) {
            Ok(inherited) => inherited,
            Err(error) => {
                tracing::warn!(%error, "could not hand over to the installed binary");
                child.keep(&self.lock.file);
                return;
            }
        };
        tracing::info!(binary = %self.binary.display(), "handing over to the installed binary");
        #[cfg(unix)]
        {
            let error = child::exec_supervisor(&self.binary, &self.args, &inherited);
            tracing::warn!(%error, "could not exec the installed binary");
        }
        #[cfg(windows)]
        match child::spawn_supervisor(&self.binary, &self.args, &inherited) {
            Ok(()) => std::process::exit(0),
            Err(error) => tracing::warn!(%error, "could not start the installed binary"),
        }
        child.keep(&self.lock.file);
    }
}

impl Params {
    fn for_core(&self) -> core::Params {
        let ms = |duration: Duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX);
        core::Params {
            check_interval_ms: ms(self.check_interval),
            rollback_after: self.rollback_after,
            start_deadline_ms: ms(self.start_deadline),
            stop_deadline_ms: ms(self.stop_deadline),
            backoff_first_ms: ms(self.backoff_first),
            backoff_max_ms: ms(self.backoff_max),
            backoff_reset_ms: ms(self.backoff_reset),
            // A running executable cannot be renamed over on Windows.
            replace_while_running: cfg!(unix),
            step_budget: self.step_budget,
        }
    }
}

fn millis_now(clock: &dyn Clock) -> u64 {
    u64::try_from(clock.now_ms()).unwrap_or(0)
}

/// The most a channel manifest may be: a few entries of a few hundred
/// bytes each, with room to spare. A server that answers with more is not
/// serving a manifest, whatever it says, and is not buffered.
const MANIFEST_LIMIT: u64 = 1 << 20;
/// The most an artifact may be: the binary is tens of megabytes. The
/// manifest's signed size bounds what is downloaded; this bounds what a
/// manifest may ask for, against a mistake of our own.
const ARTIFACT_LIMIT: u64 = 256 << 20;

/// Reads a body of at most `limit` bytes; one declared or streamed longer
/// is refused before it is held whole.
async fn read_bounded(
    mut response: reqwest::Response,
    limit: u64,
    what: &'static str,
    url: &str,
) -> Result<Vec<u8>, CheckError> {
    let too_large = || CheckError::TooLarge {
        what,
        url: url.to_owned(),
        limit,
    };
    if response
        .content_length()
        .is_some_and(|length| length > limit)
    {
        return Err(too_large());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|error| CheckError::Fetch {
        url: url.to_owned(),
        error,
    })? {
        if body.len() as u64 + chunk.len() as u64 > limit {
            return Err(too_large());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[derive(Debug, thiserror::Error)]
enum CheckError {
    #[error("fetching {url}: {error}")]
    Fetch { url: String, error: reqwest::Error },
    #[error("reading {url}: {error}")]
    Manifest {
        url: String,
        error: serde_json::Error,
    },
    #[error("{what} at {url} is over {limit} bytes")]
    TooLarge {
        what: &'static str,
        url: String,
        limit: u64,
    },
    #[error("staging: {0}")]
    Stage(io::Error),
    #[error(transparent)]
    Verify(#[from] release::VerifyError),
}

fn client(url: &str) -> Result<reqwest::Client, CheckError> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(30))
        .timeout(Duration::from_secs(15 * 60))
        .build()
        .map_err(|error| CheckError::Fetch {
            url: url.to_owned(),
            error,
        })
}

fn fetch_error(url: &str) -> impl FnOnce(reqwest::Error) -> CheckError {
    let url = url.to_owned();
    move |error| CheckError::Fetch { url, error }
}

/// The channel's manifest, at most its size limit, verified whole.
async fn list(source: &UpdateSource) -> Result<Manifest, CheckError> {
    let response = client(&source.manifest_url)?
        .get(&source.manifest_url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(fetch_error(&source.manifest_url))?;
    let body = read_bounded(
        response,
        MANIFEST_LIMIT,
        "the manifest",
        &source.manifest_url,
    )
    .await?;
    let manifest: Manifest =
        serde_json::from_slice(&body).map_err(|error| CheckError::Manifest {
            url: source.manifest_url.clone(),
            error,
        })?;
    release::verify_manifest(&manifest, &source.channel, &source.keys)?;
    Ok(manifest)
}

/// Stages the release at `staged`, verified against its manifest entry and
/// executable.
async fn fetch(release: &Release, staged: &Path) -> Result<(), CheckError> {
    if release.size > ARTIFACT_LIMIT {
        return Err(CheckError::TooLarge {
            what: "the artifact",
            url: release.url.clone(),
            limit: ARTIFACT_LIMIT,
        });
    }
    let mut response = client(&release.url)?
        .get(&release.url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(fetch_error(&release.url))?;
    let mut file = tokio::fs::File::create(staged)
        .await
        .map_err(CheckError::Stage)?;
    let mut hash = Sha256::new();
    let mut written: u64 = 0;
    while let Some(chunk) = response.chunk().await.map_err(fetch_error(&release.url))? {
        written += chunk.len() as u64;
        // The signed size is the most that is written: a server serving
        // more than the manifest promised is not serving the artifact.
        if written > release.size {
            return Err(release::VerifyError::Size {
                expected: release.size,
                actual: written,
            }
            .into());
        }
        hash.update(&chunk);
        file.write_all(&chunk).await.map_err(CheckError::Stage)?;
    }
    file.sync_all().await.map_err(CheckError::Stage)?;
    drop(file);
    release::verify_artifact(release, &release::sha256_hex(&hash.finalize()), written)?;
    files::make_executable(staged).map_err(CheckError::Stage)?;
    Ok(())
}

/// The host id that places this installation in a rollout: its first
/// profile's.
fn rollout_host(data_dir: &Path) -> Option<uuid::Uuid> {
    // A profile's host id is its id.
    Some(profiles::list(data_dir).ok()?.first()?.id)
}

/// The supervisor's own lock, `<data_dir>/supervisor.lock`, holding its
/// pid for `amux server stop`. It crosses the exec after an update with the
/// rest of the supervisor's state.
struct SupervisorLock {
    file: File,
}

impl SupervisorLock {
    /// Takes the lock. A supervisor started by the previous one on Windows
    /// waits for it to exit and release it.
    fn acquire(data_dir: &Path, handed_over: bool) -> Result<Self, SuperviseError> {
        use std::io::{Read as _, Seek as _, Write as _};
        let path = data_dir.join(SUPERVISOR_LOCK);
        let lock_error = |error| SuperviseError::Lock {
            path: path.clone(),
            error,
        };
        private_dir(data_dir).map_err(lock_error)?;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .map_err(lock_error)?;
        let mut tries = if handed_over { 100 } else { 1 };
        loop {
            match file.try_lock() {
                Ok(()) => break,
                Err(std::fs::TryLockError::WouldBlock) if tries > 1 => {
                    tries -= 1;
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(std::fs::TryLockError::WouldBlock) => {
                    let mut pid = String::new();
                    let _ = file.read_to_string(&mut pid);
                    return Err(SuperviseError::Busy {
                        path,
                        pid: pid.trim().to_owned(),
                    });
                }
                Err(std::fs::TryLockError::Error(error)) => return Err(lock_error(error)),
            }
        }
        file.set_len(0).map_err(lock_error)?;
        file.rewind().map_err(lock_error)?;
        write!(file, "{}", std::process::id()).map_err(lock_error)?;
        file.sync_all().map_err(lock_error)?;
        Ok(Self { file })
    }

    fn inherit(fd: u64, data_dir: &Path) -> Result<Self, SuperviseError> {
        let file = files::inherited_file(fd).map_err(|error| SuperviseError::Lock {
            path: data_dir.join(SUPERVISOR_LOCK),
            error,
        })?;
        Ok(Self { file })
    }
}

#[cfg(unix)]
async fn terminated() {
    use tokio::signal::unix::{SignalKind, signal};
    let (Ok(mut term), Ok(mut int)) = (
        signal(SignalKind::terminate()),
        signal(SignalKind::interrupt()),
    ) else {
        return std::future::pending().await;
    };
    tokio::select! {
        _ = term.recv() => {}
        _ = int.recv() => {}
    }
}

#[cfg(not(unix))]
async fn terminated() {
    let _ = tokio::signal::ctrl_c().await;
}

/// Makes a rename or removal of `path` durable.
fn sync_parent(path: &Path) -> io::Result<()> {
    match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => sync_dir(dir),
        _ => sync_dir(Path::new(".")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_violation_the_supervisor_core_repeats_is_reported_once_per_interval() {
        let broke = core::DumpReason::Violation(core::Violation {
            invariant: core::TWO_SERVING,
            detail: "two serve".into(),
        });
        let mut dumps = Dumps::default();
        let first = dumps.reason(&broke, 0).unwrap();
        assert!(first.starts_with(SUPERVISOR_CORE_DUMP), "{first}");
        assert_eq!(dumps.reason(&broke, DumpLimit::INTERVAL_MS - 1), None);
        let second = dumps.reason(&broke, DumpLimit::INTERVAL_MS).unwrap();
        assert!(
            second.ends_with("(1 more like it was not dumped since the last)"),
            "{second}"
        );
    }

    /// A dump the core asks for leaves a file under the data directory's
    /// reports: the reason, the core's state when the window began, every
    /// input stepped since and the state now. The violation is forced at
    /// the driver: the core's own invariants hold for every input the
    /// supervisor model explores.
    #[test]
    fn a_dump_the_supervisor_core_asks_for_is_a_file_of_its_state_and_inputs() {
        let data_dir = tempfile::tempdir().unwrap();
        let mut out = Vec::new();
        let mut core = Supervisor::start(
            Start {
                params: Params::default().for_core(),
                target: "aarch64-apple-darwin".into(),
                running: Version::new(1, 2, 3),
                prev: core::PrevFound::None,
                rejected: None,
                inherited: false,
                now_ms: 1_000,
            },
            &mut out,
        );
        let mut dumps = Dumps::default();
        for input in [
            core::Input::Tick { now_ms: 1_005 },
            core::Input::Prepared {
                daemon: DaemonId(1),
            },
            core::Input::Tick { now_ms: 1_010 },
            core::Input::Terminate,
        ] {
            dumps.step(&mut core, input, &mut out);
        }
        let broke = core::DumpReason::Violation(core::Violation {
            invariant: core::TWO_SERVING,
            detail: "forced by the test".into(),
        });
        let file = dumps
            .dump(&broke, 1_010, &core, data_dir.path())
            .expect("admitted")
            .expect("written");
        assert_eq!(
            file.parent(),
            Some(data_dir.path().join(crate::REPORTS).as_path())
        );
        let name = file.file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            name.starts_with("supervisor-") && name.ends_with(".txt"),
            "{name}"
        );
        let text = std::fs::read_to_string(&file).unwrap();
        println!("{}\n{text}", file.display());
        let header = format!("the supervisor core, amux {}\n", crate::version());
        assert!(text.starts_with(&header), "{text}");
        assert!(
            text.contains(&format!(
                "reason: {SUPERVISOR_CORE_DUMP}an invariant failed: at most one daemon is in \
                 the serving phase: forced by the test"
            )),
            "{text}"
        );
        assert!(
            text.contains("state at the window's start:\nSupervisor {"),
            "{text}"
        );
        assert!(
            text.contains("inputs since (4):\n1 Tick { now_ms: 1005 }\n"),
            "{text}"
        );
        assert!(text.contains("4 Terminate\n"), "{text}");
        assert!(text.contains("state now:\nSupervisor {"), "{text}");
        assert!(text.contains("stopping: true"), "the state now: {text}");
        // Held back within the interval: no second file.
        assert!(dumps.dump(&broke, 1_011, &core, data_dir.path()).is_none());
    }

    /// Serves one response to one connection.
    async fn serve(head: &'static str, body: Vec<u8>) -> String {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 1024];
            let _ = tokio::io::AsyncReadExt::read(&mut socket, &mut request).await;
            socket.write_all(head.as_bytes()).await.unwrap();
            socket.write_all(&body).await.unwrap();
            let _ = socket.shutdown().await;
        });
        url
    }

    async fn get(url: &str) -> reqwest::Response {
        reqwest::Client::new().get(url).send().await.unwrap()
    }

    #[tokio::test]
    async fn a_body_declared_over_the_limit_is_refused_unread() {
        let url = serve(
            "HTTP/1.1 200 OK\r\nContent-Length: 2048\r\n\r\n",
            vec![b'x'; 2048],
        )
        .await;
        let error = read_bounded(get(&url).await, 1024, "the manifest", &url)
            .await
            .unwrap_err();
        assert!(
            matches!(error, CheckError::TooLarge { limit: 1024, .. }),
            "{error}"
        );
    }

    #[tokio::test]
    async fn a_body_streamed_past_the_limit_is_refused() {
        let chunk = |bytes: &[u8]| {
            let mut out = format!("{:x}\r\n", bytes.len()).into_bytes();
            out.extend_from_slice(bytes);
            out.extend_from_slice(b"\r\n");
            out
        };
        let mut body = chunk(&[b'x'; 700]);
        body.extend(chunk(&[b'x'; 700]));
        body.extend(b"0\r\n\r\n");
        let url = serve(
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n",
            body,
        )
        .await;
        let error = read_bounded(get(&url).await, 1024, "the manifest", &url)
            .await
            .unwrap_err();
        assert!(matches!(error, CheckError::TooLarge { .. }), "{error}");
    }
}
