//! Hosts that are real `amux` processes: the daemon alone, or the daemon
//! under `amux supervise`. The net starts and stops them the way a person
//! does, through the CLI against the host's installation config, kills the
//! daemon with a signal the way a crash does, and reaches it only through
//! its sockets, as any client must.

use std::path::{Path, PathBuf};
use std::time::Duration;

use tonic::transport::Channel;
use wire::installation_service_client::InstallationServiceClient;

use crate::net::local_channel;

/// How a process host runs.
#[derive(Clone, Debug)]
pub(crate) struct ProcessHost {
    /// The `amux` it runs: the build under test, or for a supervised host
    /// its own copy the supervisor replaces on an update.
    pub amux: PathBuf,
    pub config: PathBuf,
    pub data_dir: PathBuf,
    pub front_door: PathBuf,
    /// Put first on the daemon's PATH: the host's provider commands.
    pub bin: PathBuf,
    /// Whether its daemon advertises on the machine's real mDNS.
    pub bonjour: bool,
    /// The net relay's certificate, once the net has a relay: a daemon
    /// process dials the relay where the account service names it, and
    /// trusts its self-signed certificate only when told to.
    pub relay_ca: PathBuf,
}

/// How long a process host has to answer after a start: a fresh binary's
/// first launch can stall on macOS while it is assessed.
const START: Duration = Duration::from_secs(90);

impl ProcessHost {
    fn command(&self) -> tokio::process::Command {
        let mut command = tokio::process::Command::new(&self.amux);
        command.arg("--config").arg(&self.config);
        let path = std::env::var_os("PATH").unwrap_or_default();
        let path = std::env::join_paths(
            std::iter::once(self.bin.clone()).chain(std::env::split_paths(&path)),
        )
        .unwrap_or(path);
        command
            .env("PATH", path)
            .env(
                "AMUX_TEST_DISCOVERY_MODE",
                if self.bonjour { "mdns" } else { "disabled" },
            )
            // Listing the machine's own interfaces would hand peers a way
            // around the network the net imitates.
            .env("AMUX_TEST_INTERFACES", "")
            .env_remove("AMUX_LOG")
            .env_remove("AMUX_CONFIG")
            // The net reads the link log out of the daemon's log, which a
            // filter meant for the net's own trace would thin.
            .env_remove("RUST_LOG")
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true);
        if self.relay_ca.is_file() {
            command.env("AMUX_CLOUD_TLS_CA", &self.relay_ca);
        } else {
            command.env_remove("AMUX_CLOUD_TLS_CA");
        }
        command
    }

    /// Runs one CLI verb against the host and answers what it printed.
    pub(crate) async fn run(&self, args: &[&str], patience: Duration) -> Result<String, String> {
        let mut command = self.command();
        command.args(args);
        let ran = tokio::time::timeout(patience, command.output())
            .await
            .map_err(|_| format!("amux {} did not finish within {patience:?}", args.join(" ")))?
            .map_err(|error| format!("running amux {}: {error}", args.join(" ")))?;
        let said = format!(
            "{}{}",
            String::from_utf8_lossy(&ran.stdout),
            String::from_utf8_lossy(&ran.stderr)
        );
        if ran.status.success() {
            Ok(said.trim().to_owned())
        } else {
            Err(format!("amux {} failed: {}", args.join(" "), said.trim()))
        }
    }

    /// Starts the host as a person does, `amux server start`, which
    /// detaches the daemon (or its supervisor), and returns once the
    /// front door answers.
    pub(crate) async fn start(&self) -> Result<(), String> {
        self.run(&["server", "start"], START).await?;
        Ok(())
    }

    /// Stops it as a person does, `amux server stop`: the daemon shuts
    /// down cleanly, with its supervisor when it has one, and its agents
    /// keep running.
    pub(crate) async fn stop(&self) -> Result<(), String> {
        self.run(&["server", "stop"], Duration::from_secs(60))
            .await?;
        Ok(())
    }

    /// The daemon's process: whoever holds the installation lock.
    pub(crate) fn daemon_pid(&self) -> Option<u32> {
        lock_holder(&self.data_dir.join(node::INSTALLATION_LOCK))
    }

    /// Crashes the daemon: SIGKILL, so nothing is flushed or closed
    /// politely. A supervisor, if there is one, sees its daemon die as it
    /// would any crash.
    pub(crate) fn kill(&self) -> Result<u32, String> {
        let pid = self
            .daemon_pid()
            .ok_or("no daemon holds the installation lock")?;
        signal(pid, "-KILL")?;
        Ok(pid)
    }

    /// Waits until no daemon holds the installation lock.
    pub(crate) async fn wait_down(&self) -> Result<(), String> {
        let lock = self.data_dir.join(node::INSTALLATION_LOCK);
        patience::until("the daemon to let go of its lock", || {
            let held = lock_holder(&lock);
            async move {
                match held {
                    None => Ok(()),
                    Some(pid) => Err(format!("held by {pid}")),
                }
            }
        })
        .await
        .map_err(|error| error.to_string())
    }

    pub(crate) async fn front_door(&self) -> Result<InstallationServiceClient<Channel>, String> {
        let channel = local_channel(&self.front_door)
            .await
            .map_err(|error| format!("the front door does not answer: {error}"))?;
        Ok(InstallationServiceClient::new(channel))
    }

    /// The client service on the host's one profile, as the CLI and the
    /// terminal client reach it.
    pub(crate) async fn client(&self) -> Result<wire::ClientStubs, String> {
        // A daemon between a crash and its supervisor's next start answers
        // nothing for a moment; a person's client would retry, and so does
        // this one, for as long as such a start takes.
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        loop {
            match self.connect().await {
                Ok(client) => return Ok(client),
                Err(error) if std::time::Instant::now() >= deadline => return Err(error),
                Err(_) => tokio::time::sleep(Duration::from_millis(200)).await,
            }
        }
    }

    async fn connect(&self) -> Result<wire::ClientStubs, String> {
        let profile = self
            .front_door()
            .await?
            .list_profiles(wire::ListProfilesRequest {})
            .await
            .map_err(|status| status.message().to_owned())?
            .into_inner()
            .profiles
            .into_iter()
            .next()
            .ok_or("the host has no profile")?;
        let channel = local_channel(Path::new(&profile.socket_path))
            .await
            .map_err(|error| format!("the profile socket does not answer: {error}"))?;
        Ok(wire::client_stubs(channel))
    }
}

/// The process holding `lock` open, if any: the lock is held for as long
/// as its file is open.
fn lock_holder(lock: &Path) -> Option<u32> {
    if !lock.exists() {
        return None;
    }
    let listed = std::process::Command::new("lsof")
        .arg("-t")
        .arg(lock)
        .output()
        .ok()?;
    String::from_utf8_lossy(&listed.stdout)
        .lines()
        .filter_map(|line| line.trim().parse::<u32>().ok())
        .next()
}

pub(crate) fn signal(pid: u32, signal: &str) -> Result<(), String> {
    let status = std::process::Command::new("kill")
        .arg(signal)
        .arg(pid.to_string())
        .status()
        .map_err(|error| error.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("no process {pid} to signal"))
    }
}
