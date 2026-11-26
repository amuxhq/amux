//! The daemon entry and the server verbs.
//!
//! `amux daemon` is the daemon itself, in the foreground: what a supervisor
//! or a service manager runs. `amux server start` starts one by hand,
//! detached from the terminal, and returns once its front door answers.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use node::{HttpSender, Launch, StartOptions};
use settings::InstallationConfig;
use tracing_subscriber::EnvFilter;

use crate::connect;
use crate::supervise::SUPERVISOR_LOG;

/// The daemon log's path when the environment names none.
const DAEMON_LOG: &str = "daemon.log";
/// Names the daemon's log file, overriding the one under the data dir.
const LOG_ENV: &str = "AMUX_LOG";
/// How long `amux server start` and `stop` wait on the daemon.
const PATIENCE: Duration = Duration::from_secs(30);
/// How long `amux server stop` waits on a supervisor, which gives its
/// daemon the stop deadline before a kill.
const SUPERVISOR_PATIENCE: Duration = Duration::from_secs(45);
/// How long `amux server stop`, having recorded the stop, waits on a
/// supervisor the launcher started meanwhile to exit on it or open its
/// control socket.
const RECORDED_PATIENCE: Duration = Duration::from_secs(5);

pub(crate) fn daemon_log(config: &InstallationConfig) -> PathBuf {
    std::env::var_os(LOG_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| config.root.join(DAEMON_LOG))
}

/// Logs to `path`, capped as every amux log is, with the process's standard
/// error moved onto it so a panic lands there too.
pub(crate) fn log_to(path: &Path) -> Result<()> {
    let file = log_file::LogFile::open(path, log_file::PROCESS_CAP)
        .with_context(|| format!("opening the log {}", path.display()))?;
    file.claim_stderr()
        .with_context(|| format!("moving standard error onto {}", path.display()))?;
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_ansi(false)
        .with_writer(Mutex::new(file))
        .init();
    Ok(())
}

/// Logs to standard error, for a process whose service manager keeps and
/// rotates what it writes there.
pub(crate) fn log_to_stderr() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_ansi(false)
        .with_writer(std::io::stderr)
        .init();
}

/// What agents start with: this binary as the canonical install path, the
/// installation's agent settings, its repositories, which Claude it runs
/// and the ACP agents it knows, built in or added in `[agents.<id>]`;
/// and the experiments switched on, with their own settings, which the
/// daemon runs until it stops. The daemon asks Claude and Codex what they
/// offer as it starts.
fn launch(config: &InstallationConfig) -> Result<Launch> {
    let install_path = std::env::current_exe().context("finding the amux binary")?;
    let experiments = config.experiments.enabled();
    let (suspension, warnings) = settings::experiments::suspension::Suspension::read(
        config.experiments.table(&settings::experiments::SUSPENSION),
    );
    if experiments.is_on(&settings::experiments::SUSPENSION) {
        for warning in warnings {
            tracing::warn!("{warning}");
        }
    }
    let (agents, warnings) = node::agents::configured(config);
    for warning in warnings {
        tracing::warn!("{warning}");
    }
    Ok(Launch {
        install_path,
        agent: config.agent,
        repositories: config.repositories(),
        claude_kind: node::claude_kind(config),
        new_agent: config.new_agent.clone(),
        agents,
        experiments,
        suspension,
        ask_providers_at_start: true,
        ..Launch::default()
    })
}

/// How long a supervised daemon that asked its supervisor to restart it
/// waits to be stopped before it exits by itself.
const RESTART_PATIENCE: Duration = Duration::from_secs(10);

/// How a running daemon ends.
enum Ended {
    /// Asked to stop, or its supervisor went: it shuts down and exits.
    Stop,
    /// It restarts itself to run the experiments the config switches on:
    /// it shuts down, then starts again in place.
    Relaunch,
}

/// Runs the daemon in the foreground until it is asked to stop: by a
/// client's shutdown call, SIGTERM or SIGINT, or its supervisor's end of the
/// pipe closing. `named` is the config file named on the command line or
/// in the environment, as the daemon was started with it. It restarts itself when the experiments the config
/// switches on are no longer the ones it runs: under a supervisor by asking
/// it to, as the supervisor restarts it onto an installed update; without
/// one by shutting down and starting again in place ([`crate::relaunch`]).
pub fn run_daemon(
    config: &InstallationConfig,
    named: Option<&Path>,
    pipe: Option<node::InheritedPipe>,
) -> Result<()> {
    let log = daemon_log(config);
    log_to(&log)?;
    // Whether this process shows a console window: never, when it was
    // started in the background (§2.2 of the Windows design, P1).
    tracing::info!(window = os::process::console_window(), "console");
    // The children of the daemon this one replaced by exec, if it did
    // (crate::relaunch): nothing else waits for them.
    match os::process::reap_inherited() {
        Ok(0) => {}
        Ok(children) => {
            tracing::info!(
                children,
                "reaping the children of the daemon this one replaced"
            );
        }
        Err(error) => {
            tracing::warn!(%error, "could not start reaping the replaced daemon's children");
        }
    }
    for key in &config.ignored {
        tracing::warn!(key, "ignoring an unknown config key");
    }
    for name in config.experiments.unknown_names() {
        tracing::warn!(name, "ignoring an experiment this build does not have");
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let ended = runtime.block_on(async {
        let options = StartOptions {
            data_dir: config.root.clone(),
            boot_id: None,
            launch: launch(config)?,
            clock: Arc::new(agent_dir::SystemClock),
            push: Arc::new(HttpSender::new()),
            daemon_log: Some(log),
            // A supervised daemon is handed its supervisor's pipe.
            supervisor_log: pipe.is_some().then(|| config.root.join(SUPERVISOR_LOG)),
            front_door: Some(config.front_door()),
            edge: edge_options(config),
            analytics: telemetry(config),
            auto_reports: auto_reports(config),
            settings: settings(config),
        };
        let pipe = pipe
            .map(node::InheritedPipe::into_pipe)
            .transpose()
            .context("opening the supervisor pipe")?;
        let mut daemon = node::start(options, pipe)
            .await
            .context("starting the daemon")?;
        let supervisor = daemon.take_supervisor();
        let supervised = supervisor.is_some();
        tracing::info!(
            version = node::version(),
            root = %config.root.display(),
            front_door = %config.front_door().display(),
            generation = daemon.generation().counter,
            experiments = ?config.experiments.enabled().versions(),
            experiment_settings = ?config.experiments.in_effect().describe(),
            "serving"
        );
        // The waits borrow the daemon, so they end before it shuts down.
        let ended = {
        let gone = supervisor_gone(supervisor);
        let signalled = terminated();
        let restart = async {
            // The next start reads the config as this one did: one it would
            // refuse, a named file gone among them, holds the restart.
            let said = std::sync::atomic::AtomicBool::new(false);
            daemon
                .restart_wanted(|| match connect::read_config(named) {
                    Ok(_) => true,
                    Err(error) => {
                        if !said.swap(true, std::sync::atomic::Ordering::Relaxed) {
                            tracing::warn!(
                                error = format!("{error:#}"),
                                "the config would not read at a start; restarting once it does"
                            );
                        }
                        false
                    }
                })
                .await
        };
        tokio::pin!(gone, signalled, restart);
        let mut asked: Option<tokio::time::Instant> = None;
        loop {
            tokio::select! {
                () = daemon.shutdown_requested() => {
                    tracing::info!("a client asked the daemon to stop");
                    break Ended::Stop;
                }
                // Never resolves: each change of the machine's interfaces
                // moves every profile's links onto the paths that work now.
                () = daemon.follow_interfaces() => {}
                () = &mut signalled => {
                    tracing::info!("signalled to stop");
                    break Ended::Stop;
                }
                // The supervisor is gone: exit, so whoever starts a
                // supervisor again gets a fresh daemon under it.
                () = &mut gone => {
                    tracing::info!("the supervisor went away");
                    break Ended::Stop;
                }
                wanted = &mut restart, if asked.is_none() => {
                    tracing::info!(
                        running = ?wanted.running,
                        wanted = ?wanted.wanted,
                        settings = ?wanted.settings,
                        settled = wanted.settled,
                        "restarting to run the experiments switched on, with their settings"
                    );
                    if !wanted.settled {
                        tracing::warn!(
                            within = ?node::RESTART_SETTLE_WITHIN,
                            "work under way did not settle in time; restarting anyway"
                        );
                    }
                    if !supervised {
                        break Ended::Relaunch;
                    }
                    // The supervisor stops this daemon and starts it again,
                    // as it does onto an update; the stop is the signal.
                    match node::supervisor::ask(
                        &config.root,
                        node::supervisor::Request::Restart,
                        PATIENCE,
                    )
                    .await
                    {
                        Ok(answer) if answer == node::supervisor::RESTARTING => {
                            asked = Some(tokio::time::Instant::now() + RESTART_PATIENCE);
                        }
                        // An older supervisor, or none answering: exiting
                        // is a restart too, after its backoff.
                        answer => {
                            tracing::warn!(
                                ?answer,
                                "the supervisor did not restart the daemon; exiting for it to start a new one"
                            );
                            break Ended::Stop;
                        }
                    }
                }
                () = tokio::time::sleep_until(asked.unwrap_or_else(tokio::time::Instant::now)),
                    if asked.is_some() =>
                {
                    tracing::warn!("the supervisor did not stop the daemon; exiting for it to start a new one");
                    break Ended::Stop;
                }
            }
        }
        };
        daemon.shutdown().await.context("shutting down")?;
        Ok::<_, anyhow::Error>(ended)
    });
    // Every task and socket of this run goes with the runtime; the
    // installation lock went with the shutdown.
    runtime.shutdown_timeout(Duration::from_secs(5));
    // Shut down cleanly: a stop the OS asked for (Windows: a closed
    // console, logoff or shutdown) may end the process now.
    os::signal::finished();
    match ended? {
        Ended::Stop => Ok(()),
        Ended::Relaunch => {
            tracing::info!("starting again in place");
            Err(crate::relaunch::relaunch()).context("starting the daemon again")
        }
    }
}

/// Whether the daemon sends product analytics, and where: only a published
/// build or one `AMUX_ANALYTICS_URL` points somewhere sends, only while the
/// installation's `telemetry` setting is on, and never under `DO_NOT_TRACK`.
///
/// The uploader runs whatever the setting is at start, so turning it on
/// reaches a daemon that started with it off; while it is off the gate
/// drops each event as it is recorded, and nothing is sent.
fn telemetry(config: &InstallationConfig) -> node::Telemetry {
    telemetry_to(
        config,
        analytics::Endpoint::resolve(node::release::is_published()),
    )
}

fn telemetry_to(
    config: &InstallationConfig,
    endpoint: Option<analytics::Endpoint>,
) -> node::Telemetry {
    let Some(endpoint) = endpoint else {
        return node::Telemetry::Off;
    };
    // Read again as each event is recorded and before every upload, so
    // `amux config set telemetry` reaches a running daemon either way. A
    // file that no longer reads sends nothing.
    let path = config
        .path
        .clone()
        .unwrap_or_else(InstallationConfig::default_path);
    let gate = Arc::new(move || match InstallationConfig::from_file(&path) {
        Ok(config) => config.telemetry,
        Err(settings::ConfigError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            true
        }
        Err(_) => false,
    });
    node::Telemetry::Upload {
        endpoint,
        channel: match config.channel {
            settings::Channel::Stable => analytics::Channel::Stable,
            settings::Channel::Preview => analytics::Channel::Preview,
        },
        gate,
    }
}

/// Whether the dumps a check asks for are sent on: read again before every
/// one, so `amux config set auto_reports` reaches a running daemon. A file
/// that no longer reads sends nothing.
fn auto_reports(config: &InstallationConfig) -> node::AutoReports {
    let path = config
        .path
        .clone()
        .unwrap_or_else(InstallationConfig::default_path);
    node::AutoReports::new(Arc::new(move || {
        InstallationConfig::from_file(&path).is_ok_and(|config| config.auto_reports)
    }))
}

/// Where the everyday settings live: the config file, written in place
/// when there is none yet, and the login item `start_at_login` keeps when
/// a supervisor runs this install.
fn settings(config: &InstallationConfig) -> node::SettingsOptions {
    let file = config
        .path
        .clone()
        .unwrap_or_else(InstallationConfig::default_path);
    let named = Some(file.as_path()).filter(|file| *file != InstallationConfig::default_path());
    let login = config
        .supervisor
        .then(|| node::supervisor::login::StartAtLogin::here(&config.root, named))
        .and_then(|login| {
            login
                .inspect_err(|error| tracing::warn!(%error, "no login item can be kept"))
                .ok()
        });
    node::SettingsOptions {
        file: Some(file),
        initial: config.clone(),
        login,
        home: None,
    }
}

/// What each profile serves on the network: a LAN listener, discovery in
/// the installation's scope, dialling paired hosts, and the link socket SSH
/// relays use.
fn edge_options(config: &InstallationConfig) -> node::EdgeOptions {
    node::EdgeOptions {
        host_name: config.host_name().into(),
        kinds: node::local_kinds(),
        lan: Some(node::LanOptions::default()),
        discovery: Some(node::mdns_discovery()),
        discovery_scope: config.discovery.scope.clone(),
        dial: true,
        link_socket: true,
        substrate: None,
        cloud: node::CloudOptions::default(),
        interfaces: node::test_interfaces(),
        liveness: Default::default(),
        sign_in_client: node::CLI_CLIENT_ID.to_owned(),
    }
}

async fn supervisor_gone(pipe: Option<node::ActivationPipe>) {
    match pipe {
        Some(pipe) => pipe.closed().await,
        None => std::future::pending().await,
    }
}

/// Resolves when the OS asks this process to stop: SIGTERM, SIGINT or
/// SIGHUP; on Windows Ctrl+C, a closed console, logoff or shutdown, after
/// which the caller shuts down cleanly and calls
/// [`os::signal::finished`]. One it can't listen for is logged, and never
/// resolves.
pub(crate) async fn terminated() {
    match os::signal::terminated().await {
        Ok(stop) => tracing::info!(?stop, "the OS asked amux to stop"),
        Err(error) => {
            tracing::warn!(%error, "cannot hear the OS ask amux to stop");
            std::future::pending::<()>().await;
        }
    }
}

/// Starts amux, detached from this terminal, unless a daemon answers
/// already, and returns once its front door answers. An install with a
/// supervisor starts `amux supervise`, which starts the daemon; nothing
/// ever launches a daemon beside a supervisor.
pub async fn start(config: &InstallationConfig, config_path: Option<&Path>) -> Result<()> {
    if connect::front_door_now(config).await.is_some() {
        println!("The amux daemon is already running.");
        return Ok(());
    }
    let (mut child, said) = if config.supervisor {
        if running_supervisor(&config.root)?.is_some() {
            println!("amux supervise is running; waiting for its daemon.");
            (None, config.root.join(SUPERVISOR_LOG))
        } else {
            (
                Some(spawn_supervisor(config, config_path)?),
                config.root.join(SUPERVISOR_LOG),
            )
        }
    } else {
        std::fs::create_dir_all(&config.root)
            .with_context(|| format!("creating {}", config.root.display()))?;
        let startup = config.root.join("daemon-startup.log");
        let stderr =
            File::create(&startup).with_context(|| format!("creating {}", startup.display()))?;
        let mut command = std::process::Command::new(std::env::current_exe()?);
        if let Some(path) = config_path.or(config.path.as_deref()) {
            // The file this process read, named whether or not the caller
            // named it, and absolute: the daemon runs in the root, not
            // here, and would resolve a default under a relative
            // `XDG_CONFIG_HOME` from there.
            command.arg("--config").arg(std::path::absolute(path)?);
        }
        command.arg("daemon");
        (
            Some(
                spawn_detached(&mut command, &config.root, stderr)
                    .context("starting amux daemon")?,
            ),
            startup,
        )
    };
    let deadline = Instant::now() + PATIENCE;
    loop {
        if let Some(door) = connect::front_door_now(config).await
            && connect::installation(door)
                .get_installation(wire::GetInstallationRequest {})
                .await
                .is_ok()
        {
            println!("Started amux.");
            return Ok(());
        }
        if let Some(child) = child.as_mut()
            && let Some(status) = child.try_wait()?
        {
            let said = std::fs::read_to_string(&said).unwrap_or_default();
            bail!(
                "amux exited ({status}) before it answered:\n{}",
                said.trim_end()
            );
        }
        if Instant::now() >= deadline {
            bail!(
                "amux did not answer within {}s; its logs are {} and {}",
                PATIENCE.as_secs(),
                said.display(),
                daemon_log(config).display()
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Starts `amux supervise` detached from this terminal. Its early errors
/// go to the supervisor's log, where its own logging goes too.
pub(crate) fn spawn_supervisor(
    config: &InstallationConfig,
    config_path: Option<&Path>,
) -> Result<os::process::Background> {
    std::fs::create_dir_all(&config.root)
        .with_context(|| format!("creating {}", config.root.display()))?;
    let log = config.root.join(SUPERVISOR_LOG);
    let stderr = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log)
        .with_context(|| format!("opening {}", log.display()))?;
    let mut command = std::process::Command::new(std::env::current_exe()?);
    if let Some(path) = config_path.or(config.path.as_deref()) {
        // Absolute: the supervisor runs in the root, not here.
        command.arg("--config").arg(std::path::absolute(path)?);
    }
    command.arg("supervise");
    spawn_detached(&mut command, &config.root, stderr).context("starting amux supervise")
}

/// Starts `command` in the background, in its own session with no window
/// (`os::process::Detach::Session`), so it outlives the terminal that
/// started it, in `root`, with no standard input or output and `stderr` as
/// its standard error. A job that keeps it (Windows) is said in that log.
fn spawn_detached(
    command: &mut std::process::Command,
    root: &Path,
    stderr: File,
) -> std::io::Result<os::process::Background> {
    use std::io::Write as _;
    let mut note = stderr.try_clone()?;
    // The child runs in `root`, not here: a relative XDG folder, which
    // this process found its default config and root from, is made
    // absolute for it, so it finds the same ones when no file names them.
    for name in ["XDG_CONFIG_HOME", "XDG_DATA_HOME"] {
        if let Some(value) = std::env::var_os(name)
            && !Path::new(&value).is_absolute()
        {
            let here = if value.is_empty() {
                std::env::current_dir()?
            } else {
                std::path::absolute(&value)?
            };
            command.env(name, here);
        }
    }
    os::process::background(command, os::process::Detach::Session, root);
    let given = os::process::Given {
        stderr: os::process::Stream::File(stderr),
        ..os::process::Given::default()
    };
    let spawned = os::process::spawn_background(command, given)?;
    if spawned.breakaway_refused {
        let _ = writeln!(
            note,
            "amux: the job this was started in forbids leaving it, so it ends when that job does"
        );
    }
    Ok(spawned.child)
}

/// Stops amux: its owner first. A running supervisor is stopped, which
/// stops its daemon, or the daemon would come straight back; without one
/// the daemon is asked to shut down. Agents keep running and wait out
/// their grace for the next daemon.
///
/// With no supervisor running where the install has one, a launcher
/// (Windows' login item) whose supervisor died may be waiting to start it
/// again: the stop is recorded in the root ([`node::supervisor::STOPPED`])
/// for the supervisor it starts, which then exits at once and ends the
/// launcher.
pub async fn stop(config: &InstallationConfig) -> Result<()> {
    let mut running = running_supervisor(&config.root)?;
    let mut reached = Reached::Neither;
    if running.is_none() && config.supervisor && config.root.is_dir() {
        let stopped = config.root.join(node::supervisor::STOPPED);
        std::fs::write(&stopped, b"")
            .with_context(|| format!("recording the stop in {}", stopped.display()))?;
        // A supervisor started before that was written read nothing; one
        // started after it exits on it without opening its socket.
        if running_supervisor(&config.root)?.is_some() {
            reached = reach_restarted(&config.root, RECORDED_PATIENCE).await?;
        }
        if reached != Reached::Released {
            running = running_supervisor(&config.root)?;
        }
    }
    if let Some(pid) = running {
        if reached != Reached::Asked {
            stop_supervisor(config, &pid).await?;
        }
        let deadline = Instant::now() + SUPERVISOR_PATIENCE;
        while running_supervisor(&config.root)?.is_some() || installation_locked(&config.root)? {
            if Instant::now() >= deadline {
                bail!(
                    "amux supervise (pid {pid}) did not stop within {}s",
                    SUPERVISOR_PATIENCE.as_secs()
                );
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        println!("Stopped amux supervise and its daemon.");
        return Ok(());
    }
    let Some(door) = connect::front_door_now(config).await else {
        println!("The amux daemon is not running.");
        return Ok(());
    };
    connect::installation(door)
        .shutdown(wire::ShutdownRequest {})
        .await
        .map_err(crate::plain)?;
    // The front door closes first; the daemon has stopped once it has
    // flushed its stores and released the installation lock, and a daemon
    // started before then would find the lock still held.
    let deadline = Instant::now() + PATIENCE;
    while installation_locked(&config.root)? {
        if Instant::now() >= deadline {
            bail!("the daemon did not stop within {}s", PATIENCE.as_secs());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    println!("Stopped the amux daemon.");
    Ok(())
}

/// How a supervisor the launcher started while `amux server stop` recorded
/// the stop was reached.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reached {
    /// Its lock was released: it read the stop and exited.
    Released,
    /// Its control socket took the stop: it read nothing.
    Asked,
    /// Neither within the patience.
    Neither,
}

/// Waits up to `patience` for the supervisor holding the lock under `root`
/// to release it or to take a stop over its control socket. A launcher's
/// restart that read the recorded stop exits without ever opening its
/// socket, and one that started just before opens it soon; only after
/// neither is the stop asked for as for any other supervisor.
async fn reach_restarted(root: &Path, patience: Duration) -> Result<Reached> {
    let deadline = Instant::now() + patience;
    loop {
        if running_supervisor(root)?.is_none() {
            return Ok(Reached::Released);
        }
        if node::supervisor::ask(root, node::supervisor::Request::Stop, PATIENCE)
            .await
            .is_ok()
        {
            return Ok(Reached::Asked);
        }
        if Instant::now() >= deadline {
            return Ok(Reached::Neither);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Asks the supervisor to stop over its control socket, on every OS.
/// Where that fails (a supervisor that could not bind its socket, or one
/// that does not answer) and the OS has signals, the pid the supervisor's
/// lock holds is sent SIGTERM instead.
async fn stop_supervisor(config: &InstallationConfig, pid: &str) -> Result<()> {
    let asked = node::supervisor::ask(&config.root, node::supervisor::Request::Stop, PATIENCE)
        .await
        .context("asking amux supervise to stop");
    let Err(error) = asked else {
        return Ok(());
    };
    let pid: u32 = pid.parse().with_context(|| {
        format!("{error:#}; and the supervisor's lock holds {pid:?}, not a pid")
    })?;
    match os::process::ProcessRef::open(pid).and_then(|process| process.ask_to_stop()) {
        Ok(()) => Ok(()),
        Err(signal) if signal.kind() == std::io::ErrorKind::Unsupported => Err(error),
        Err(signal) => Err(error.context(format!(
            "and signalling amux supervise (pid {pid}) failed: {signal}"
        ))),
    }
}

/// The pid of the supervisor holding the installation's supervisor lock,
/// if one is running: the lock's data, readable while it is held.
pub(crate) fn running_supervisor(root: &Path) -> Result<Option<String>> {
    let path = root.join(node::supervisor::SUPERVISOR_LOCK);
    if !lock_held(&path)? {
        return Ok(None);
    }
    Ok(Some(
        std::fs::read_to_string(&path)
            .unwrap_or_default()
            .trim()
            .to_owned(),
    ))
}

/// Whether a daemon holds the installation lock under `root`.
fn installation_locked(root: &Path) -> Result<bool> {
    lock_held(&root.join(node::INSTALLATION_LOCK))
}

/// Whether some process holds the lock on the file at `path`.
fn lock_held(path: &Path) -> Result<bool> {
    os::lock::FileLock::held(path).with_context(|| format!("probing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A supervisor the launcher started while `amux server stop` recorded
    /// the stop: the stop waits for it to exit on the record (its lock
    /// released), or for its socket to take the stop, and only after
    /// neither asks as for any supervisor. The lock and the socket stand
    /// in for the supervisor here, since the race can't be staged.
    #[tokio::test]
    async fn a_stop_recorded_waits_for_the_restarted_supervisor_to_exit_or_answer() {
        let root = tempfile::tempdir().unwrap();
        let lock_path = root.path().join(node::supervisor::SUPERVISOR_LOCK);
        let hold = || {
            os::lock::FileLock::try_acquire(&lock_path)
                .unwrap()
                .unwrap()
        };

        // It exits on the record without opening its socket.
        let lock = hold();
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            drop(lock);
        });
        let reached = reach_restarted(root.path(), Duration::from_secs(10)).await;
        release.join().unwrap();
        assert_eq!(reached.unwrap(), Reached::Released);

        // It read nothing, and its socket opens a moment later.
        let lock = hold();
        let socket = root.path().join(node::supervisor::SUPERVISOR_SOCKET);
        let answer = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            let mut listener = agent_dir::local_socket::LocalListener::bind(&socket).unwrap();
            let stream = listener.accept().await.unwrap();
            let (read, mut write) = tokio::io::split(stream);
            let mut line = String::new();
            tokio::io::AsyncBufReadExt::read_line(&mut tokio::io::BufReader::new(read), &mut line)
                .await
                .unwrap();
            tokio::io::AsyncWriteExt::write_all(&mut write, b"stopping\n")
                .await
                .unwrap();
            line
        });
        let reached = reach_restarted(root.path(), Duration::from_secs(10)).await;
        assert_eq!(reached.unwrap(), Reached::Asked);
        assert_eq!(answer.await.unwrap().trim(), "stop");

        // Neither: the patience runs out, and the stop goes on as ever.
        let reached = reach_restarted(root.path(), Duration::from_millis(200)).await;
        assert_eq!(reached.unwrap(), Reached::Neither);
        drop(lock);
    }

    /// The daemon runs the experiments the file switches on when it
    /// starts, and only those this build implements.
    #[test]
    fn a_daemon_starts_the_experiments_switched_on() {
        let config = InstallationConfig::from_toml(
            "[experiments]\nenabled = [\"teleport\", \"workspaces\"]\n",
        )
        .unwrap();
        assert_eq!(
            launch(&config).unwrap().experiments.versions(),
            ["workspaces/1"]
        );
        assert!(
            launch(&InstallationConfig::default())
                .unwrap()
                .experiments
                .versions()
                .is_empty()
        );
    }

    /// A daemon that starts with usage counts off still runs the uploader,
    /// whose gate follows the file: turned on, it sends; off again, it
    /// stops.
    #[test]
    fn usage_counts_follow_the_setting_from_a_start_with_them_off() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "telemetry = false\n").unwrap();
        let config = InstallationConfig {
            path: Some(path.clone()),
            ..InstallationConfig::from_file(&path).unwrap()
        };
        assert!(!config.telemetry);
        let node::Telemetry::Upload { gate, .. } = telemetry_to(
            &config,
            Some(analytics::Endpoint::Fixed("http://127.0.0.1:9".into())),
        ) else {
            panic!("no uploader for a daemon started with usage counts off");
        };
        assert!(!gate(), "off at start");
        std::fs::write(&path, "telemetry = true\n").unwrap();
        assert!(gate(), "turned on while running");
        std::fs::write(&path, "telemetry = false\n").unwrap();
        assert!(!gate(), "off again");
        assert!(
            matches!(telemetry_to(&config, None), node::Telemetry::Off),
            "a build that sends nowhere runs no uploader"
        );
    }
}
