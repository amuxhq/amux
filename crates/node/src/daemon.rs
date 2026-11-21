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
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::activation::{ActivationError, ActivationPipe};
use crate::edge::EdgeOptions;
use crate::generation::{self, Generation};
use crate::grpc::{self, ClientApi};
use crate::install::{InstallationLock, LockError, REPORTS, STORE, private_dir};
use crate::outbox::PushSender;
use crate::profiles::{self, PROFILE_SOCKET, ProfileEntry, ProfileId};
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
    /// Where the everyday settings live, which the front door serves.
    pub settings: crate::host_settings::SettingsOptions,
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
    /// The machine has booted since the daemon before this one started.
    rebooted: bool,
    /// What each profile opens with; its repositories and Claude kind
    /// follow the settings, so a profile opened after a change starts with
    /// it.
    launch: Mutex<Launch>,
    clock: Arc<dyn Clock>,
    push: Arc<dyn PushSender>,
    daemon_log: Option<PathBuf>,
    supervisor_log: Option<PathBuf>,
    auto_reports: crate::reports::AutoReports,
    /// What each profile's edge starts with; its name follows the settings.
    edge: Mutex<EdgeOptions>,
    /// The settings as last applied.
    applied: Mutex<settings::InstallationConfig>,
    /// The experiments the settings switch on, with their own settings as
    /// their modules read them, as last applied: a restart follows them
    /// when they move away from what this daemon runs.
    wanted: watch::Sender<settings::experiments::InEffect>,
    /// What this daemon runs: the experiments it started with, and their
    /// settings as the settings it started with give them.
    started: settings::experiments::InEffect,
    /// Whether an agent works while `keep_awake` is on.
    pub(crate) awake: Arc<crate::awake::Awake>,
    pub(crate) settings: crate::host_settings::HostSettings,
    pub(crate) onboarding: Arc<crate::onboarding::Onboarding>,
    pub(crate) hosted: Mutex<BTreeMap<ProfileId, Hosted>>,
    /// One change to the profile set at a time: create, rename, delete.
    pub(crate) profile_changes: tokio::sync::Mutex<()>,
    /// Rings whenever the profile list moves.
    pub(crate) moved: watch::Sender<()>,
    shutdown: watch::Sender<bool>,
    /// Where profiles record analytics; unset records nothing.
    analytics: std::sync::OnceLock<Arc<dyn analytics::Sink>>,
    /// Where profiles keep a person's model and effort picks.
    pick_keeper: std::sync::OnceLock<crate::spec::PickKeeper>,
    pins: std::sync::OnceLock<crate::repositories::Pins>,
    /// Logins refused only because their profile held data, by the token
    /// they were staged with, until bound again with adopt or forgotten.
    staged_logins: Mutex<Vec<(String, std::time::Instant, crate::edge::account::Login)>>,
}

impl Installation {
    pub(crate) fn host_name(&self) -> String {
        self.edge.lock().unwrap().host_name.clone()
    }

    /// The experiments this daemon runs and their settings: those it
    /// started with.
    pub(crate) fn running_experiments(&self) -> &settings::experiments::InEffect {
        &self.started
    }

    /// Whether no profile has work under way that a restart waits for.
    fn settled(&self) -> bool {
        self.hosted
            .lock()
            .unwrap()
            .values()
            .all(|hosted| *hosted.runtime.busy.borrow() == 0)
    }

    /// Puts what changed in the settings into force: every profile's
    /// repositories, Claude kind, name and the ACP agents it knows. Only
    /// what changed since they were last applied moves, so what an embedder
    /// gave a host at start stands until the person changes that setting.
    pub(crate) fn apply(&self, config: &settings::InstallationConfig) {
        let (repositories, kind, renamed, new_agent, agents) = {
            let mut last = self.applied.lock().unwrap();
            let new_agent = (last.new_agent != config.new_agent).then(|| config.new_agent.clone());
            // The next spawn or resume starts an agent's program as the
            // config says it now; a running one keeps the one it started.
            let agents = (last.agents != config.agents).then(|| {
                let (agents, warnings) = crate::agents::configured(config);
                for warning in warnings {
                    tracing::warn!("{warning}");
                }
                agents
            });
            let repositories =
                (last.repositories != config.repositories).then(|| config.repositories());
            let kind = (crate::host_settings::claude_kind(&last)
                != crate::host_settings::claude_kind(config))
            .then(|| crate::host_settings::claude_kind(config));
            let renamed = (last.host_name != config.host_name).then(|| {
                let name = config.host_name().into_string();
                self.edge.lock().unwrap().host_name.clone_from(&name);
                name
            });
            *last = config.clone();
            (repositories, kind, renamed, new_agent, agents)
        };
        self.awake.keep_awake(config.keep_awake);
        let wanted = config.experiments.in_effect();
        self.wanted.send_if_modified(|was| {
            let moved = *was != wanted;
            *was = wanted;
            moved
        });
        if repositories.is_none()
            && kind.is_none()
            && renamed.is_none()
            && new_agent.is_none()
            && agents.is_none()
        {
            return;
        }
        {
            let mut launch = self.launch.lock().unwrap();
            if let Some(repositories) = &repositories {
                launch.repositories.clone_from(repositories);
            }
            if let Some(kind) = kind {
                launch.claude_kind = kind;
            }
            if let Some(new_agent) = &new_agent {
                launch.new_agent.clone_from(new_agent);
            }
            if let Some(agents) = &agents {
                launch.agents.clone_from(agents);
            }
        }
        let runtimes: Vec<Arc<ProfileRuntime>> = self
            .hosted
            .lock()
            .unwrap()
            .values()
            .map(|hosted| hosted.runtime.clone())
            .collect();
        for runtime in runtimes {
            let mut launch = runtime.launch();
            if let Some(repositories) = &repositories {
                launch.repositories.clone_from(repositories);
            }
            if let Some(kind) = kind {
                launch.claude_kind = kind;
            }
            if let Some(new_agent) = &new_agent {
                launch.new_agent.clone_from(new_agent);
            }
            if let Some(agents) = &agents {
                launch.agents.clone_from(agents);
            }
            if kind.is_some() || new_agent.is_some() {
                runtime.offers().remembered_changed(&launch);
            }
            runtime.set_launch(launch);
            if repositories.is_some() {
                runtime.refresh_folders();
            }
            if let (Some(name), Some(edge)) = (&renamed, runtime.edge()) {
                edge.rename(name);
            }
            runtime.tell_own_host();
        }
        if renamed.is_some() {
            self.publish();
        }
    }
}

/// How often the daemon looks at the config file for a change made beside
/// it, by `amux config` or an editor: such a change is in force within it.
const SETTINGS_POLL: std::time::Duration = std::time::Duration::from_secs(2);

/// Applies each change to the config file made beside the daemon until the
/// installation goes.
fn follow_settings(installation: std::sync::Weak<Installation>) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut every = tokio::time::interval(SETTINGS_POLL);
        every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            every.tick().await;
            let Some(installation) = installation.upgrade() else {
                return;
            };
            if let Some(config) = installation.settings.reread() {
                installation.apply(&config);
            }
        }
    })
}

/// How long the experiments switched on must stay put before the daemon
/// restarts to run them, so several switches in a row are one restart.
pub const RESTART_DEBOUNCE: std::time::Duration = std::time::Duration::from_secs(3);

// A file that reads again is applied within a poll, inside the quiet that
// follows it, before a restart is decided on what it said.
const _: () = assert!(SETTINGS_POLL.as_millis() < RESTART_DEBOUNCE.as_millis());

/// How long a restart to run the experiments switched on waits for the
/// daemon's own work under way to settle (a workspace being made, a hook
/// running) before it restarts anyway.
pub const RESTART_SETTLE_WITHIN: std::time::Duration = std::time::Duration::from_secs(5 * 60);

/// Why the daemon wants to restart: the experiments its settings switch on,
/// or the settings of one it runs, are not what it runs, and stayed so for
/// [`RESTART_DEBOUNCE`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RestartWanted {
    /// The versions it runs now.
    pub running: Vec<&'static str>,
    /// The versions the settings switch on, which the next start runs.
    pub wanted: Vec<&'static str>,
    /// Their own settings, as their modules read them, which the next start
    /// runs with (`suspension.after = 30m`).
    pub settings: Vec<String>,
    /// The work it waited for settled; false when the wait ran out at
    /// [`RESTART_SETTLE_WITHIN`] with some still under way.
    pub settled: bool,
}

/// How long a login refused for its profile's data waits to be adopted.
const STAGED_LOGIN_LIFETIME: std::time::Duration = std::time::Duration::from_secs(15 * 60);

pub(crate) struct Hosted {
    pub(crate) runtime: Arc<ProfileRuntime>,
    pub(crate) entry: ProfileEntry,
    socket: Option<JoinHandle<()>>,
    /// Republishes the profile when its cloud link's state moves.
    observer: Option<JoinHandle<()>>,
    /// Tells the installation whether one of its agents is working.
    working: Option<JoinHandle<()>>,
}

impl Drop for Hosted {
    fn drop(&mut self) {
        for task in [
            self.socket.take(),
            self.observer.take(),
            self.working.take(),
        ]
        .into_iter()
        .flatten()
        {
            task.abort();
        }
    }
}

impl Installation {
    /// Opens a profile's store and its runtime. Writes nothing but the
    /// store: a new one, or the migrations an existing one lacks, which
    /// only add, so the previous binary still reads it after a rollback.
    fn open(&self, profile: ProfileId) -> Result<Arc<ProfileRuntime>, StartError> {
        let dir = profiles::profile_dir(&self.data_dir, profile);
        // The profile's id is its host id, the one its peers know it by.
        let host = profile;
        // The experiments the daemon started with keep their tables; every
        // other experiment's are dropped.
        let experiments = self.launch.lock().unwrap().experiments.versions();
        let store =
            store::Sqlite::open_with(&dir.join(STORE), host.as_bytes().to_vec(), &experiments)
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
        let launch = self.launch.lock().unwrap().clone();
        let workspaces = std::sync::Arc::new(crate::workspaces::Workspaces::new(
            profile,
            self.data_dir.join(crate::workspaces::WORKTREES),
            launch.wt_home.clone(),
        ));
        Ok(ProfileRuntime::open(Profile {
            profile,
            generation: self.generation,
            rebooted: self.rebooted,
            dir,
            store,
            launch,
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
            workspaces,
            onboarding: self.onboarding.clone(),
            remember_pick: self.pick_keeper.get().cloned().unwrap_or_default(),
            pins: self.pins.get().cloned().unwrap_or_default(),
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
        let options = self.edge.lock().unwrap().clone();
        let edge = runtime
            .start_edge(&options)
            .await
            .map_err(|error| StartError::Edge {
                profile: entry.id,
                error,
            })?;
        let observer = {
            let installation = Arc::downgrade(self);
            let mut relay = edge.subscribe_relay();
            let mut account = edge.account().subscribe();
            tokio::spawn(async move {
                loop {
                    let moved = tokio::select! {
                        moved = relay.changed() => moved,
                        moved = account.changed() => moved,
                    };
                    if moved.is_err() {
                        return;
                    }
                    let Some(installation) = installation.upgrade() else {
                        return;
                    };
                    installation.publish();
                }
            })
        };
        let socket = self.bind(&runtime)?;
        let working = crate::awake::follow(
            Arc::downgrade(&runtime),
            entry.id,
            Arc::downgrade(&self.awake),
        );
        self.hosted.lock().unwrap().insert(
            entry.id,
            Hosted {
                runtime,
                entry,
                socket,
                observer: Some(observer),
                working: Some(working),
            },
        );
        self.publish();
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

    pub(crate) fn stage_login(&self, token: String, login: crate::edge::account::Login) {
        let mut staged = self.staged_logins.lock().unwrap();
        staged.retain(|(_, at, _)| at.elapsed() < STAGED_LOGIN_LIFETIME);
        staged.push((token, std::time::Instant::now(), login));
    }

    pub(crate) fn take_staged_login(&self, token: &str) -> Option<crate::edge::account::Login> {
        let mut staged = self.staged_logins.lock().unwrap();
        staged.retain(|(_, at, _)| at.elapsed() < STAGED_LOGIN_LIFETIME);
        let index = staged.iter().position(|(staged, ..)| staged == token)?;
        Some(staged.remove(index).2)
    }

    /// Tells the registry's watchers the profile list moved.
    pub(crate) fn publish(&self) {
        self.moved.send_replace(());
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
    /// Applies changes made to the config file beside the daemon.
    following: JoinHandle<()>,
    /// Holds the machine awake while an agent works, on a supervised
    /// install.
    awake: Option<JoinHandle<()>>,
    /// The experiments the settings switch on and their settings, seen as
    /// of the start: any change since reads as changed.
    wanted: watch::Receiver<settings::experiments::InEffect>,
    // Dropped last: the lock outlives everything that writes under it.
    lock: Option<InstallationLock>,
}

impl Drop for Daemon {
    /// A daemon dropped without a shutdown is a crash: nothing is flushed
    /// and the installation stays dirty, but no task of this run goes on
    /// writing once its lock is released.
    fn drop(&mut self) {
        self.following.abort();
        if let Some(awake) = self.awake.take() {
            awake.abort();
        }
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
        settings,
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

    let onboarding = Arc::new(crate::onboarding::Onboarding::open(&data_dir));
    let launch_experiments = launch.experiments.clone();
    let installation = Arc::new(Installation {
        data_dir,
        front_door,
        generation: generation.counter,
        rebooted: last_run
            .as_ref()
            .is_some_and(|last| last.boot_id != boot_id),
        launch: Mutex::new(launch),
        clock,
        push,
        daemon_log,
        supervisor_log,
        auto_reports,
        edge: Mutex::new(edge),
        applied: Mutex::new(settings.initial.clone()),
        wanted: watch::Sender::new(settings.initial.experiments.in_effect()),
        started: settings::experiments::InEffect::of(
            &launch_experiments,
            &settings.initial.experiments,
        ),
        awake: Arc::new(crate::awake::Awake::new(settings.initial.keep_awake)),
        settings: crate::host_settings::HostSettings::new(settings),
        onboarding,
        hosted: Mutex::new(BTreeMap::new()),
        profile_changes: tokio::sync::Mutex::new(()),
        moved: watch::Sender::new(()),
        shutdown: watch::Sender::new(false),
        staged_logins: Mutex::new(Vec::new()),
        pick_keeper: std::sync::OnceLock::new(),
        pins: std::sync::OnceLock::new(),
        analytics: std::sync::OnceLock::new(),
    });
    let keeper = Arc::downgrade(&installation);
    let _ = installation
        .pick_keeper
        .set(crate::spec::PickKeeper::new(move |picked| {
            let Some(installation) = keeper.upgrade() else {
                return;
            };
            match installation.settings.remember(&picked) {
                Ok(Some(config)) => installation.apply(&config),
                Ok(None) => {}
                Err(error) => tracing::warn!(%error, "keeping a new agent default"),
            }
        }));
    let pinner = Arc::downgrade(&installation);
    let _ = installation.pins.set(crate::repositories::Pins::new(
        installation.settings.home().map(std::path::Path::to_owned),
        move |path, pinned| {
            let Some(installation) = pinner.upgrade() else {
                return Ok(());
            };
            if let Some(config) = installation.settings.pin(path, pinned)? {
                installation.apply(&config);
            }
            Ok(())
        },
    ));
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
    // This installation's lock is held, so a front door that answers
    // belongs to another installation's daemon, whose config names the same
    // socket; taking it would strand that daemon's clients.
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

    let following = follow_settings(Arc::downgrade(&installation));
    let awake = installation
        .settings
        .supervised()
        .then(|| installation.awake.hold());
    let wanted = installation.wanted.subscribe();
    Ok(Daemon {
        wanted,
        installation,
        generation,
        uploader,
        sweep,
        supervisor,
        front_door,
        following,
        awake,
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
    pub fn front_door(&self) -> crate::front_door::FrontDoor {
        crate::front_door::FrontDoor::new(self.registry())
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

    /// Whether the daemon keeps the machine awake now: `keep_awake` is on
    /// and one of its agents is working. Only a supervised install takes
    /// the system's assertion for it.
    pub fn keeps_awake(&self) -> bool {
        self.installation.awake.wanted()
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

    /// Resolves once the daemon should restart to run the experiments its
    /// settings switch on, with their own settings. It is one decision,
    /// made again whenever anything it rests on moves, and taken only when
    /// all of it holds at once:
    ///
    /// - what the settings ask (whichever way they changed: a client, `amux
    ///   config`, an edit of the file) is not what the daemon runs;
    /// - that ask has stayed the same for [`RESTART_DEBOUNCE`]: any change
    ///   to it starts the quiet over, so switches in a row are one restart,
    ///   and a change undone in time is none;
    /// - `reads` says the next start can read its config, and has for the
    ///   quiet: one that could not would not start at all, and the quiet
    ///   gives the settings poll time to apply the file once it reads;
    /// - no work that a restart would cut off is under way, unless it has
    ///   been wanted for [`RESTART_SETTLE_WITHIN`] since the ask first
    ///   differed from what runs.
    ///
    /// A daemon never restarts for the settings it started with: what it
    /// runs is what they asked. Settings compare as each experiment's
    /// module reads them, so a value it cannot use, a key it does not read
    /// or the settings of an experiment that is off change nothing it runs
    /// and restart nothing, nor does a name this build does not have. Only
    /// `amux daemon` acts on it; an embedder runs what it was started with.
    pub async fn restart_wanted(&self, reads: impl Fn() -> bool) -> RestartWanted {
        /// How often the decision is made again while it waits: what it
        /// rests on beyond the settings (work under way, the file reading)
        /// is looked at, not watched.
        const LOOK_AGAIN: std::time::Duration = std::time::Duration::from_millis(250);
        let running = self.installation.running_experiments().clone();
        let mut wanted = self.wanted.clone();
        let mut asked_since = tokio::time::Instant::now();
        let mut differs_since: Option<tokio::time::Instant> = None;
        loop {
            let now = tokio::time::Instant::now();
            let asked = wanted.borrow_and_update().clone();
            if asked == running {
                differs_since = None;
            } else {
                let since = *differs_since.get_or_insert(now);
                // A config that does not read also starts the quiet over:
                // once it reads again the daemon's poll has the time to
                // apply what it now says before anything is decided on it.
                if !reads() {
                    asked_since = now;
                }
                let settled = self.installation.settled();
                if now.duration_since(asked_since) >= RESTART_DEBOUNCE
                    && (settled || now.duration_since(since) >= RESTART_SETTLE_WITHIN)
                {
                    return RestartWanted {
                        running: running.enabled().versions(),
                        wanted: asked.enabled().versions(),
                        settings: asked.describe(),
                        settled,
                    };
                }
            }
            let changed = if differs_since.is_some() {
                tokio::select! {
                    changed = wanted.changed() => Some(changed),
                    () = tokio::time::sleep(LOOK_AGAIN) => None,
                }
            } else {
                Some(wanted.changed().await)
            };
            match changed {
                Some(Ok(())) => asked_since = tokio::time::Instant::now(),
                Some(Err(_)) => return std::future::pending().await,
                None => {}
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
        // First: the system may be signalling the agents too, and an exit
        // recorded from here on would reach paired hosts before the links
        // close, and have them stop its children.
        let runtimes: Vec<Arc<ProfileRuntime>> = self
            .installation
            .hosted
            .lock()
            .unwrap()
            .values()
            .map(|hosted| hosted.runtime.clone())
            .collect();
        for runtime in runtimes {
            runtime.stopping().await;
        }
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
