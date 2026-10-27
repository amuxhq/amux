//! The daemon entry and the server verbs.
//!
//! `amux daemon` is the daemon itself, in the foreground: what a supervisor
//! or a service manager runs. `amux server start` starts one by hand,
//! detached from the terminal, and returns once its front door answers.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use node::{Launch, NoopSender, StartOptions};
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

pub(crate) fn daemon_log(config: &InstallationConfig) -> PathBuf {
    std::env::var_os(LOG_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| config.root.join(DAEMON_LOG))
}

pub(crate) fn log_to(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("opening the daemon log {}", path.display()))?;
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
/// installation's agent settings and where it looks for repositories.
fn launch(config: &InstallationConfig) -> Result<Launch> {
    let install_path = std::env::current_exe().context("finding the amux binary")?;
    Ok(Launch {
        install_path,
        agent: config.agent,
        repository_roots: config.repository_roots.clone(),
        ..Launch::default()
    })
}

/// Runs the daemon in the foreground until it is asked to stop: by a
/// client's shutdown call, SIGTERM or SIGINT, or its supervisor's end of the
/// pipe closing.
pub fn run_daemon(config: &InstallationConfig, pipe: Option<node::InheritedPipe>) -> Result<()> {
    let log = daemon_log(config);
    log_to(&log)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let options = StartOptions {
            data_dir: config.root.clone(),
            boot_id: None,
            launch: launch(config)?,
            clock: Arc::new(agent_dir::SystemClock),
            // Needs-you pushes go through the relay, which this build does
            // not connect to yet.
            push: Arc::new(NoopSender),
            daemon_log: Some(log),
            front_door: Some(config.front_door_socket.clone()),
            edge: edge_options(config),
            analytics: telemetry(config),
        };
        let pipe = pipe
            .map(node::InheritedPipe::into_pipe)
            .transpose()
            .context("opening the supervisor pipe")?;
        let mut daemon = node::start(options, pipe)
            .await
            .context("starting the daemon")?;
        let supervisor = daemon.take_supervisor();
        tracing::info!(
            version = node::version(),
            root = %config.root.display(),
            front_door = %config.front_door_socket.display(),
            generation = daemon.generation().counter,
            "serving"
        );
        tokio::select! {
            () = daemon.shutdown_requested() => tracing::info!("a client asked the daemon to stop"),
            // Never resolves: each change of the machine's interfaces moves
            // every profile's links onto the paths that work now.
            () = daemon.follow_interfaces() => {}
            () = terminated() => tracing::info!("signalled to stop"),
            // The supervisor is gone: exit, so whoever starts a supervisor
            // again gets a fresh daemon under it.
            () = supervisor_gone(supervisor) => tracing::info!("the supervisor went away"),
        }
        daemon.shutdown().await.context("shutting down")?;
        Ok(())
    })
}

/// Whether the daemon sends product analytics, and where: only a published
/// build or one `AMUX_ANALYTICS_URL` points somewhere sends, only while the
/// installation's `telemetry` setting is on, and never under `DO_NOT_TRACK`.
fn telemetry(config: &InstallationConfig) -> node::Telemetry {
    if !config.telemetry {
        return node::Telemetry::Off;
    }
    let Some(endpoint) = analytics::Endpoint::resolve(node::release::is_published()) else {
        return node::Telemetry::Off;
    };
    // Read again before every upload, so `amux config telemetry off` stops
    // a running daemon at once. A file that no longer reads sends nothing.
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

/// What each profile serves on the network: a LAN listener, discovery in
/// the installation's scope, dialling paired hosts, and the link socket SSH
/// relays use.
fn edge_options(config: &InstallationConfig) -> node::EdgeOptions {
    node::EdgeOptions {
        host_name: config.host_name(),
        kinds: node::local_kinds(),
        lan: Some(node::LanOptions::default()),
        discovery: Some(node::mdns_discovery()),
        discovery_scope: config.discovery.scope.clone(),
        dial: true,
        link_socket: true,
        cloud: node::CloudOptions::default(),
        interfaces: node::test_interfaces(),
        liveness: Default::default(),
    }
}

async fn supervisor_gone(pipe: Option<node::ActivationPipe>) {
    match pipe {
        Some(pipe) => pipe.closed().await,
        None => std::future::pending().await,
    }
}

#[cfg(unix)]
pub(crate) async fn terminated() {
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
pub(crate) async fn terminated() {
    let _ = tokio::signal::ctrl_c().await;
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
        if let Some(path) = config_path {
            command.arg("--config").arg(path);
        }
        command
            .arg("daemon")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(stderr);
        detach(&mut command);
        (
            Some(command.spawn().context("starting amux daemon")?),
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
) -> Result<std::process::Child> {
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
        command.arg("--config").arg(path);
    }
    command
        .arg("supervise")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(stderr);
    detach(&mut command);
    command.spawn().context("starting amux supervise")
}

/// Its own session, so the daemon outlives the terminal that started it.
#[cfg(unix)]
fn detach(command: &mut std::process::Command) {
    use std::os::unix::process::CommandExt as _;
    // SAFETY: setsid is async-signal-safe and touches no parent state.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(windows)]
fn detach(command: &mut std::process::Command) {
    use std::os::windows::process::CommandExt as _;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
}

/// Stops amux: its owner first. A running supervisor is stopped, which
/// stops its daemon, or the daemon would come straight back; without one
/// the daemon is asked to shut down. Agents keep running and wait out
/// their grace for the next daemon.
pub async fn stop(config: &InstallationConfig) -> Result<()> {
    if let Some(pid) = running_supervisor(&config.root)? {
        stop_supervisor(config, &pid).await?;
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

/// Signals the pid the supervisor's lock file holds.
#[cfg(unix)]
async fn stop_supervisor(_config: &InstallationConfig, pid: &str) -> Result<()> {
    let pid: libc::pid_t = pid
        .parse()
        .with_context(|| format!("the supervisor's lock holds {pid:?}, not a pid"))?;
    // SAFETY: a signal to the process holding the supervisor's lock.
    if unsafe { libc::kill(pid, libc::SIGTERM) } == -1 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("signalling amux supervise (pid {pid})"));
    }
    Ok(())
}

/// Windows has no signals: the supervisor's control socket.
#[cfg(not(unix))]
async fn stop_supervisor(config: &InstallationConfig, _pid: &str) -> Result<()> {
    node::supervisor::ask(&config.root, node::supervisor::Request::Stop, PATIENCE)
        .await
        .context("asking amux supervise to stop")?;
    Ok(())
}

/// The pid of the supervisor holding the installation's supervisor lock,
/// if one is running.
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
    let file = match OpenOptions::new().write(true).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error).with_context(|| format!("opening {}", path.display())),
    };
    match file.try_lock() {
        Ok(()) => Ok(false),
        Err(std::fs::TryLockError::WouldBlock) => Ok(true),
        Err(std::fs::TryLockError::Error(error)) => {
            Err(error).with_context(|| format!("probing {}", path.display()))
        }
    }
}
