//! The one startup path and the clean shutdown.
//!
//! There is no "started by an update" or "recovering from a crash" mode.
//! Every start takes the installation lock; rewrites the generation file,
//! bumping the counter after an unclean reboot; opens or creates each
//! profile's store; looks at every agent directory without writing; tells
//! the supervisor it is prepared and waits for go, or goes at once with no
//! supervisor; and only then finishes the sweep, which is the first write
//! a rollback would have to undo, and binds the sockets clients dial. Crash
//! recovery, updates and a reboot after power loss all run this same code.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use agent_dir::Clock;
use agent_dir::local_socket::{self, LocalListener};
use tokio::sync::{broadcast, watch};
use tokio::task::JoinHandle;

use crate::activation::{ActivationError, ActivationPipe};
use crate::edge::EdgeOptions;
use crate::generation::{self, Generation};
use crate::grpc::{self, ClientApi};
use crate::install::{InstallationLock, LockError, REPORTS, STORE, private_dir};
use crate::outbox::PushSender;
use crate::profiles::{self, PROFILE_SOCKET, ProfileEntry, ProfileId};
use crate::registry::{self, ProfileEvent};
use crate::runtime::{Launch, Looked, Profile, ProfileRuntime, RegistryError, SweepReport};
use crate::telemetry::{self, Telemetry};

pub struct StartOptions {
    pub data_dir: PathBuf,
    /// This boot of the machine; read from the system when None.
    pub boot_id: Option<String>,
    pub launch: Launch,
    pub clock: Arc<dyn Clock>,
    /// Where needs-you pushes go.
    pub push: Arc<dyn PushSender>,
    /// The daemon's own log file, which dumps include.
    pub daemon_log: Option<PathBuf>,
    /// Its supervisor's log, which dumps include too, when it has one.
    pub supervisor_log: Option<PathBuf>,
    /// Where the front door listens. None serves nothing on sockets: the
    /// runtimes are reached in process.
    pub front_door: Option<PathBuf>,
    /// What each profile's network edge serves.
    pub edge: EdgeOptions,
    /// Whether profiles record product analytics, and where it goes.
    pub analytics: Telemetry,
    /// Whether the dumps a check asks for are sent to the account service.
    pub auto_reports: crate::reports::AutoReports,
}

#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error(transparent)]
    Lock(#[from] LockError),
    #[error("reading the boot id: {0}")]
    BootId(io::Error),
    #[error("the generation file: {0}")]
    Generation(io::Error),
    #[error("the profiles directory: {0}")]
    Profiles(io::Error),
    #[error("profile {profile}: {error}")]
    Profile {
        profile: ProfileId,
        error: io::Error,
    },
    #[error("profile {profile}'s store: {error}")]
    Store {
        profile: ProfileId,
        error: store::OpenError,
    },
    #[error("profile {profile}'s agents: {error}")]
    Sweep {
        profile: ProfileId,
        error: RegistryError,
    },
    #[error(transparent)]
    Activation(#[from] ActivationError),
    #[error("another daemon answers on {}", .0.display())]
    FrontDoorTaken(PathBuf),
    #[error("binding {}: {error}", .path.display())]
    Bind { path: PathBuf, error: io::Error },
    #[error("profile {profile}'s network edge: {error}")]
    Edge {
        profile: ProfileId,
        error: crate::edge::EdgeError,
    },
}

/// What the daemon shares with its front door: how to open a profile, the
/// profiles it hosts, and the shutdown request.
pub(crate) struct Installation {
    pub(crate) data_dir: PathBuf,
    pub(crate) front_door: Option<PathBuf>,
    generation: u64,
    launch: Launch,
    clock: Arc<dyn Clock>,
    push: Arc<dyn PushSender>,
    daemon_log: Option<PathBuf>,
    supervisor_log: Option<PathBuf>,
    auto_reports: crate::reports::AutoReports,
    edge: EdgeOptions,
    pub(crate) hosted: Mutex<BTreeMap<ProfileId, Hosted>>,
    /// One change to the profile set at a time: create, rename, delete.
    pub(crate) profile_changes: tokio::sync::Mutex<()>,
    /// Profile changes in order.
    pub(crate) events: broadcast::Sender<ProfileEvent>,
    /// Held while a change is published, so a watcher's snapshot and its
    /// subscription see the same moment.
    pub(crate) publishing: Mutex<()>,
    shutdown: watch::Sender<bool>,
    /// Where profiles record analytics; unset records nothing.
    analytics: std::sync::OnceLock<Arc<dyn analytics::Sink>>,
}

pub(crate) struct Hosted {
    pub(crate) runtime: Arc<ProfileRuntime>,
    pub(crate) entry: ProfileEntry,
    socket: Option<JoinHandle<()>>,
    /// Republishes the profile when its cloud link's state moves.
    observer: Option<JoinHandle<()>>,
}

impl Drop for Hosted {
    fn drop(&mut self) {
        if let Some(socket) = self.socket.take() {
            socket.abort();
        }
        if let Some(observer) = self.observer.take() {
            observer.abort();
        }
    }
}

impl Installation {
    /// Opens a profile's store and its runtime. Writes nothing but a
    /// store that did not exist.
    fn open(&self, profile: ProfileId) -> Result<Arc<ProfileRuntime>, StartError> {
        let dir = profiles::profile_dir(&self.data_dir, profile);
        // The profile's id is its host id, the one its peers know it by.
        let host = profile;
        let store = store::Sqlite::open(&dir.join(STORE), host.as_bytes().to_vec())
            .map_err(|error| StartError::Store { profile, error })?;
        // Every write is the core's, which holds its commits while its
        // checkpoint's WAL job runs; that job and the clean shutdown's
        // flush are the only checkpoints.
        store
            .leave_checkpoints_to_owner()
            .map_err(|error| StartError::Store {
                profile,
                error: error.into(),
            })?;
        Ok(ProfileRuntime::open(Profile {
            profile,
            generation: self.generation,
            dir,
            store,
            launch: self.launch.clone(),
            clock: self.clock.clone(),
            push: self.push.clone(),
            reports: self.data_dir.join(REPORTS),
            daemon_log: self.daemon_log.clone(),
            supervisor_log: self.supervisor_log.clone(),
            analytics: match self.analytics.get() {
                Some(sink) => analytics::Analytics::new(host, sink.clone()),
                None => analytics::Analytics::off(),
            },
            auto_reports: self.auto_reports.clone(),
            worktrees: Arc::new(crate::worktree::GitWorktrees::new(
                self.data_dir.join(crate::worktree::WORKTREES),
            )),
        }))
    }

    /// Binds a profile's client socket, which also serves its
    /// administration, when the daemon serves sockets.
    fn bind(
        self: &Arc<Self>,
        runtime: &Arc<ProfileRuntime>,
    ) -> Result<Option<JoinHandle<()>>, StartError> {
        if self.front_door.is_none() {
            return Ok(None);
        }
        let path = runtime.dir().join(PROFILE_SOCKET);
        let listener = LocalListener::bind(&path).map_err(|error| StartError::Bind {
            path: path.clone(),
            error,
        })?;
        Ok(Some(grpc::serve_client(
            listener,
            ClientApi::new(runtime, None).opened_as(analytics::Client::Terminal),
            Some(
                crate::ProfileAdmin::new(
                    crate::Registry::new(Arc::downgrade(self)),
                    runtime.profile(),
                )
                .server(),
            ),
        )))
    }

    /// Puts a profile whose sweep is finished into service: its outboxes,
    /// its network edge, its socket, its place in the front door's list.
    async fn host(
        self: &Arc<Self>,
        runtime: Arc<ProfileRuntime>,
        entry: ProfileEntry,
    ) -> Result<(), StartError> {
        runtime.start_background();
        let edge = runtime
            .start_edge(&self.edge)
            .await
            .map_err(|error| StartError::Edge {
                profile: entry.id,
                error,
            })?;
        let observer = {
            let installation = Arc::downgrade(self);
            let profile = entry.id;
            let mut observed = edge.subscribe_relay();
            tokio::spawn(async move {
                while observed.changed().await.is_ok() {
                    let Some(installation) = installation.upgrade() else {
                        return;
                    };
                    installation.republish(profile);
                }
            })
        };
        let socket = self.bind(&runtime)?;
        let state = {
            let mut all = self.hosted.lock().unwrap();
            let hosted = Hosted {
                runtime,
                entry,
                socket,
                observer: Some(observer),
            };
            let state = registry::state(&hosted);
            all.insert(hosted.entry.id, hosted);
            state
        };
        self.publish(ProfileEvent::Upserted(Box::new(state)));
        Ok(())
    }

    /// Creates, opens, sweeps and hosts a new profile.
    pub(crate) async fn create(self: &Arc<Self>, label: &str) -> Result<ProfileId, StartError> {
        let entry =
            profiles::create_labelled(&self.data_dir, label).map_err(StartError::Profiles)?;
        let profile = entry.id;
        match self.bring_up(entry).await {
            Ok(()) => Ok(profile),
            Err(error) => {
                // The entry is durable already; a profile that never came
                // to be hosted is taken back, or the next start would host
                // one the caller was told nothing of.
                if let Err(discard) = profiles::discard(&self.data_dir, profile) {
                    tracing::warn!(%profile, error = %discard, "could not take back the failed profile");
                }
                Err(error)
            }
        }
    }

    /// Opens, sweeps and hosts a profile the directory holds.
    async fn bring_up(self: &Arc<Self>, entry: ProfileEntry) -> Result<(), StartError> {
        let profile = entry.id;
        let runtime = self.open(profile)?;
        let looked = runtime
            .look()
            .await
            .map_err(|error| StartError::Sweep { profile, error })?;
        runtime
            .finish_sweep(looked)
            .await
            .map_err(|error| StartError::Sweep { profile, error })?;
        self.host(runtime, entry).await
    }

    pub(crate) fn publish(&self, event: ProfileEvent) {
        let _publishing = self.publishing.lock().unwrap();
        let _ = self.events.send(event);
    }

    /// Tells the front door's watchers a profile's description moved.
    pub(crate) fn republish(&self, profile: ProfileId) {
        let state = self
            .hosted
            .lock()
            .unwrap()
            .get(&profile)
            .map(registry::state);
        if let Some(state) = state {
            self.publish(ProfileEvent::Upserted(Box::new(state)));
        }
    }

    pub(crate) fn request_shutdown(&self) {
        self.shutdown.send_replace(true);
    }

    /// The oldest profile's analytics, which records what happens to the
    /// installation as a whole: every event is filed under a host id.
    pub(crate) fn first_analytics(&self) -> Option<analytics::Analytics> {
        self.hosted
            .lock()
            .unwrap()
            .values()
            .min_by_key(|hosted| (hosted.entry.created_ms, hosted.entry.id))
            .map(|hosted| hosted.runtime.analytics().clone())
    }

    /// Starts recording analytics: into a test's sink, or into an uploader
    /// this installation runs. Analytics never stops a start: an id that
    /// cannot be written leaves the installation recording nothing.
    fn start_analytics(self: &Arc<Self>, telemetry: Telemetry) -> Option<analytics::Uploader> {
        let (sink, uploader) = match telemetry {
            Telemetry::Off => return None,
            Telemetry::Record(sink) => (sink, None),
            Telemetry::Upload {
                endpoint,
                channel,
                gate,
            } => {
                let installation_id = match crate::install::installation_id(&self.data_dir) {
                    Ok(id) => id,
                    Err(error) => {
                        tracing::warn!(%error, "no installation id; recording no analytics");
                        return None;
                    }
                };
                let uploader = analytics::Uploader::start(
                    analytics::Upload {
                        installation_id,
                        context: analytics::Context::detect(crate::version(), channel),
                        endpoint,
                        default_base: settings::DEFAULT_CLOUD_URL.to_owned(),
                        gate,
                    },
                    Arc::new(telemetry::Profiles(Arc::downgrade(self))),
                    Arc::new(analytics::Http::new(
                        analytics::Params::default().request_timeout,
                    )),
                    analytics::Params::default(),
                );
                (uploader.sink(), Some(uploader))
            }
        };
        let _ = self.analytics.set(sink);
        uploader
    }
}

/// What a start says about the run before it, from the generation file
/// that run left: a first start, a crash, or another build.
fn since_last_run(
    last: Option<&Generation>,
    boot_id: &str,
    version: &str,
) -> Vec<analytics::Event> {
    use analytics::Event;
    let Some(last) = last else {
        return vec![Event::Installed];
    };
    let mut events = Vec::new();
    let previous = semver::Version::parse(&last.version).ok();
    if last.crashed_before(boot_id) {
        events.push(Event::DaemonCrashed {
            version: previous.clone(),
        });
    }
    if let (Some(from), Ok(to)) = (previous, semver::Version::parse(version)) {
        if to > from {
            events.push(Event::Updated { from, to });
        } else if to < from {
            events.push(Event::UpdateRolledBack { from, to });
        }
    }
    events
}

/// How often the desktop daemon reads the machine's interface addresses
/// for a change: a network change is noticed within it.
const INTERFACE_POLL: std::time::Duration = std::time::Duration::from_secs(2);

pub struct Daemon {
    installation: Arc<Installation>,
    generation: Generation,
    uploader: Option<analytics::Uploader>,
    sweep: BTreeMap<ProfileId, SweepReport>,
    supervisor: Option<ActivationPipe>,
    front_door: Option<JoinHandle<()>>,
    // Dropped last: the lock outlives everything that writes under it.
    lock: Option<InstallationLock>,
}

impl Drop for Daemon {
    /// A daemon dropped without a shutdown is a crash: nothing is flushed
    /// and the installation stays dirty, but no task of this run goes on
    /// writing once its lock is released.
    fn drop(&mut self) {
        if let Some(front_door) = self.front_door.take() {
            front_door.abort();
        }
        let hosted = std::mem::take(&mut *self.installation.hosted.lock().unwrap());
        for hosted in hosted.values() {
            hosted.runtime.abort_all();
        }
    }
}

/// Starts the daemon for one installation. With a pipe, writes `prepared`
/// once the stores are open and the agent directories looked at, and
/// waits for `go` before anything else is written.
pub async fn start(
    options: StartOptions,
    pipe: Option<ActivationPipe>,
) -> Result<Daemon, StartError> {
    let StartOptions {
        data_dir,
        boot_id,
        launch,
        clock,
        push,
        daemon_log,
        supervisor_log,
        front_door,
        edge,
        analytics: telemetry,
        auto_reports,
    } = options;
    let lock = InstallationLock::acquire(&data_dir)?;
    let boot_id = match boot_id {
        Some(boot_id) => boot_id,
        None => generation::boot_id().map_err(StartError::BootId)?,
    };
    // Before anything is served: a peer must never see this run's
    // revisions under the previous generation.
    let (generation, last_run) = Generation::start(&data_dir, &boot_id, crate::version())
        .await
        .map_err(StartError::Generation)?;

    let installation = Arc::new(Installation {
        data_dir,
        front_door,
        generation: generation.counter,
        launch,
        clock,
        push,
        daemon_log,
        supervisor_log,
        auto_reports,
        edge,
        hosted: Mutex::new(BTreeMap::new()),
        profile_changes: tokio::sync::Mutex::new(()),
        events: broadcast::channel(256).0,
        publishing: Mutex::new(()),
        shutdown: watch::Sender::new(false),
        analytics: std::sync::OnceLock::new(),
    });
    let uploader = installation.start_analytics(telemetry);

    let listed = profiles::list(&installation.data_dir).map_err(StartError::Profiles)?;
    let profile_dirs: Vec<PathBuf> = listed
        .iter()
        .map(|entry| profiles::profile_dir(&installation.data_dir, entry.id))
        .collect();
    crate::install::sweep_dump_leftovers(&installation.data_dir, &profile_dirs);
    let mut opened: Vec<(ProfileEntry, Arc<ProfileRuntime>, Looked)> = Vec::new();
    for entry in listed {
        let profile = entry.id;
        let runtime = installation.open(profile)?;
        let look = runtime
            .look()
            .await
            .map_err(|error| StartError::Sweep { profile, error })?;
        opened.push((entry, runtime, look));
    }

    let mut supervisor = pipe;
    if let Some(pipe) = supervisor.as_mut() {
        pipe.activate().await?;
    }

    let mut sweep = BTreeMap::new();
    let mut swept = Vec::new();
    for (entry, runtime, look) in opened {
        let profile = entry.id;
        let report = runtime
            .finish_sweep(look)
            .await
            .map_err(|error| StartError::Sweep { profile, error })?;
        sweep.insert(profile, report);
        swept.push((entry, runtime));
    }
    // A front door that answers belongs to another installation's daemon;
    // taking its socket would strand that daemon's clients.
    if let Some(path) = &installation.front_door
        && local_socket::connect(path).await.is_ok()
    {
        return Err(StartError::FrontDoorTaken(path.clone()));
    }
    // Every own journal has been read to its end: the outboxes may run.
    for (entry, runtime) in swept {
        installation.host(runtime, entry).await?;
    }
    // An installation always has a profile to serve.
    if installation.hosted.lock().unwrap().is_empty() {
        let profile = installation.create("default").await?;
        sweep.insert(profile, SweepReport::default());
    }
    let front_door = match &installation.front_door {
        Some(path) => Some(bind_front_door(&installation, path)?),
        None => None,
    };
    if let Some(analytics) = installation.first_analytics() {
        for event in since_last_run(last_run.as_ref(), &boot_id, crate::version()) {
            analytics.record(event);
        }
    }

    Ok(Daemon {
        installation,
        generation,
        uploader,
        sweep,
        supervisor,
        front_door,
        lock: Some(lock),
    })
}

fn bind_front_door(
    installation: &Arc<Installation>,
    path: &Path,
) -> Result<JoinHandle<()>, StartError> {
    let bind = |error| StartError::Bind {
        path: path.to_owned(),
        error,
    };
    if let Some(parent) = path.parent() {
        private_dir(parent).map_err(bind)?;
    }
    let listener = LocalListener::bind(path).map_err(bind)?;
    Ok(crate::front_door::serve(
        listener,
        crate::Registry::new(Arc::downgrade(installation)),
    ))
}

impl Daemon {
    pub fn data_dir(&self) -> &Path {
        &self.installation.data_dir
    }

    /// This run's generation file, as written at start.
    pub fn generation(&self) -> &Generation {
        &self.generation
    }

    pub fn profiles(&self) -> Vec<Arc<ProfileRuntime>> {
        self.installation
            .hosted
            .lock()
            .unwrap()
            .values()
            .map(|hosted| hosted.runtime.clone())
            .collect()
    }

    pub fn profile(&self, profile: ProfileId) -> Option<Arc<ProfileRuntime>> {
        self.installation
            .hosted
            .lock()
            .unwrap()
            .get(&profile)
            .map(|hosted| hosted.runtime.clone())
    }

    /// The front door's services in process: the installation's profiles,
    /// as its socket serves them.
    pub fn front_door(&self) -> crate::FrontDoor {
        crate::FrontDoor::new(self.registry())
    }

    /// A profile's administration, called in process as its socket serves
    /// it.
    pub fn profile_admin(&self, profile: ProfileId) -> crate::ProfileAdmin {
        crate::ProfileAdmin::new(self.registry(), profile)
    }

    /// The installation's profiles, called in process.
    pub fn registry(&self) -> crate::Registry {
        crate::Registry::new(Arc::downgrade(&self.installation))
    }

    /// What the startup sweep found, per profile.
    pub fn sweep(&self, profile: ProfileId) -> Option<&SweepReport> {
        self.sweep.get(&profile)
    }

    /// The supervisor pipe, once activation is done: its end of file is
    /// the signal to shut down.
    pub fn take_supervisor(&mut self) -> Option<ActivationPipe> {
        self.supervisor.take()
    }

    /// What sends the analytics waiting, when this installation uploads.
    pub fn analytics_flusher(&self) -> Option<analytics::Flusher> {
        self.uploader.as_ref().map(analytics::Uploader::flusher)
    }

    /// Watches the machine's interface addresses and, whenever they change,
    /// tells every profile's edge its network changed. Never resolves.
    pub async fn follow_interfaces(&self) {
        let mut seen = crate::edge::interface_addrs();
        let mut every = tokio::time::interval(INTERFACE_POLL);
        every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            every.tick().await;
            let now = crate::edge::interface_addrs();
            if now == seen {
                continue;
            }
            seen = now;
            for runtime in self.profiles() {
                let Some(edge) = runtime.edge() else {
                    continue;
                };
                if let Err(error) = edge.network_changed() {
                    tracing::warn!(
                        profile = %runtime.host(),
                        %error,
                        "binding the link socket afresh failed"
                    );
                }
            }
        }
    }

    /// Resolves once a client has asked the daemon to shut down.
    pub async fn shutdown_requested(&self) {
        let mut shutdown = self.installation.shutdown.subscribe();
        let _ = shutdown.wait_for(|requested| *requested).await;
    }

    /// Shuts down cleanly: stops serving, stops every writer, flushes each
    /// store to the drive, and sets the clean flag as the very last write.
    /// Agents keep running; they wait out their grace for the next daemon.
    /// A failure before the flag leaves the installation dirty, the safe
    /// error.
    pub async fn shutdown(mut self) -> io::Result<()> {
        if let Some(front_door) = self.front_door.take() {
            front_door.abort();
        }
        // While the profiles are still hosted: the uploader asks them for
        // their accounts.
        if let Some(flusher) = self.analytics_flusher() {
            stop_step("flushing analytics");
            flusher.flush(telemetry::SHUTDOWN_FLUSH).await;
        }
        let hosted = std::mem::take(&mut *self.installation.hosted.lock().unwrap());
        for hosted in hosted.values() {
            hosted
                .runtime
                .stop_edge(wire::LinkCloseReason::UserShutdown)
                .await;
        }
        for hosted in hosted.values() {
            // Before the watchers stop: a dump in flight still gets its
            // agents' parts until its time is up.
            stop_step("finishing dumps");
            hosted.runtime.finish_dumps().await;
            stop_step("stopping background work");
            hosted.runtime.stop_background().await;
            stop_step("stopping watchers");
            hosted.runtime.stop_watching().await;
        }
        stop_step("flushing stores");
        for hosted in hosted.values() {
            hosted
                .runtime
                .read_store(|store| store.flush_to_drive())
                .await
                .map_err(io::Error::other)?
                .map_err(io::Error::other)?;
        }
        stop_step("marking the installation clean");
        self.generation
            .mark_clean(&self.installation.data_dir)
            .await?;
        drop(hosted);
        // Said while the lock is still held: whoever waits for the lock to
        // free reads a log that already ends here.
        tracing::info!("stopped cleanly");
        drop(self.lock.take());
        Ok(())
    }
}

/// The log target of a clean stop's steps: one line as each begins, so a
/// stop that hangs is read off the log at the step it is stuck in.
pub const STOP_TARGET: &str = "amux::stop";

pub(crate) fn stop_step(step: &str) {
    tracing::info!(target: STOP_TARGET, "{step}");
}
