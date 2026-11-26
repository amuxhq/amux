//! Hosts that are real `amux` processes: the daemon alone, or the daemon
//! under `amux supervise`. The net starts and stops them the way a person
//! does, through the CLI against the host's installation config, kills the
//! daemon outright the way a crash does, and reaches it only through its
//! sockets, as any client must. The daemon is the process serving the
//! front door while the installation lock is held, as the operating system
//! names the socket's server; the supervisor is the pid its lock holds.

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

/// How long the front door has to answer the dial that names its server: a
/// serving daemon answers at once.
const DOOR_PATIENCE: Duration = Duration::from_secs(2);

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

    /// The daemon's process, pinned: the one serving the front door, as
    /// the operating system names the socket's server, while a daemon
    /// holds the installation lock across the look. `None` when no daemon
    /// holds the lock, or one does without serving the front door (it is
    /// starting or shutting down).
    pub(crate) async fn daemon(&self) -> Result<Option<os::process::ProcessRef>, String> {
        let lock = self.data_dir.join(node::INSTALLATION_LOCK);
        if !lock_held(&lock)? {
            return Ok(None);
        }
        let Ok((stream, Some(pid))) =
            agent_dir::local_socket::connect_to_process_within(&self.front_door, DOOR_PATIENCE)
                .await
        else {
            return Ok(None);
        };
        stream.close();
        let daemon = match os::process::ProcessRef::open(pid) {
            Ok(daemon) => daemon,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("opening the daemon (pid {pid}): {error}")),
        };
        // Pinned, it is believed only while the lock is still held: a
        // daemon that exited meanwhile is gone, not whoever has its pid now.
        Ok(lock_held(&lock)?.then_some(daemon))
    }

    /// The supervisor's process, pinned: the pid its lock holds, believed
    /// only while the lock stays held across the look and still names it.
    pub(crate) fn supervisor(&self) -> Result<Option<os::process::ProcessRef>, String> {
        holder(
            &self.data_dir.join(node::supervisor::SUPERVISOR_LOCK),
            "the supervisor",
        )
    }

    /// The daemon's process as its lock names it, pinned and believed as
    /// the supervisor's is: found from the moment the daemon takes the
    /// lock, before it serves the front door, so a daemon stalled in its
    /// start can still be killed.
    fn locked_daemon(&self) -> Result<Option<os::process::ProcessRef>, String> {
        holder(&self.data_dir.join(node::INSTALLATION_LOCK), "the daemon")
    }

    /// Crashes the daemon: killed outright (SIGKILL, `TerminateProcess`),
    /// so nothing is flushed or closed politely. A supervisor, if there is
    /// one, sees its daemon die as it would any crash.
    pub(crate) async fn kill(&self) -> Result<u32, String> {
        let daemon = self
            .daemon()
            .await?
            .ok_or("no daemon serves the front door")?;
        let pid = daemon.pid();
        daemon
            .kill()
            .map_err(|error| format!("killing the daemon (pid {pid}): {error}"))?;
        Ok(pid)
    }

    /// Kills the host's supervisor and daemon, whoever started them, and
    /// waits until neither lock is held: what outlived a stop, a daemon a
    /// supervisor started as the stop came, or one that never got as far
    /// as serving its front door. The supervisor goes first, so it starts
    /// nothing more.
    pub(crate) async fn kill_all(&self) -> Result<(), String> {
        let locks = [
            self.data_dir.join(node::supervisor::SUPERVISOR_LOCK),
            self.data_dir.join(node::INSTALLATION_LOCK),
        ];
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(supervisor) = self.supervisor()? {
                let _ = supervisor.kill();
            }
            if let Some(daemon) = self.daemon().await? {
                let _ = daemon.kill();
            } else if let Some(daemon) = self.locked_daemon()? {
                let _ = daemon.kill();
            }
            let mut held = Vec::new();
            for lock in &locks {
                if lock_held(lock)? {
                    held.push(lock.display().to_string());
                }
            }
            if held.is_empty() {
                return Ok(());
            }
            if tokio::time::Instant::now() > deadline {
                return Err(format!(
                    "the host's processes would not die: {} still held",
                    held.join(" and ")
                ));
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    /// Waits until no daemon holds the installation lock.
    pub(crate) async fn wait_down(&self) -> Result<(), String> {
        let lock = self.data_dir.join(node::INSTALLATION_LOCK);
        patience::until("the daemon to let go of its lock", || {
            let held = lock_held(&lock);
            async move {
                match held {
                    Ok(false) => Ok(()),
                    Ok(true) => Err("still held".to_owned()),
                    Err(error) => Err(error),
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

    /// A person's client of the host's one profile, as the CLI and the
    /// terminal client reach it: over the profile's socket.
    pub(crate) async fn client(&self) -> Result<client::Client, String> {
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

    async fn connect(&self) -> Result<client::Client, String> {
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
        client::Client::connect(Path::new(&profile.socket_path), client::Surface::Terminal)
            .await
            .map_err(|error| format!("the profile socket does not answer: {error}"))
    }
}

/// Whether a process holds the lock on `lock`, probed as every amux lock
/// is ([`os::lock::FileLock::held`]): no file, no lock.
fn lock_held(lock: &Path) -> Result<bool, String> {
    os::lock::FileLock::held(lock).map_err(|error| format!("probing {}: {error}", lock.display()))
}

/// The process the pid in `lock` names, pinned, and believed only while
/// the lock stays held across the look and still names it: a holder that
/// exited meanwhile is gone, not whoever has its pid now. `what` names it
/// in an error.
fn holder(lock: &Path, what: &str) -> Result<Option<os::process::ProcessRef>, String> {
    let named = || {
        std::fs::read_to_string(lock)
            .ok()
            .and_then(|named| named.trim().parse::<u32>().ok())
    };
    if !lock_held(lock)? {
        return Ok(None);
    }
    let Some(pid) = named() else {
        return Ok(None);
    };
    let process = match os::process::ProcessRef::open(pid) {
        Ok(process) => process,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("opening {what} (pid {pid}): {error}")),
    };
    Ok((lock_held(lock)? && named() == Some(pid)).then_some(process))
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    /// What the stalled daemon below holds its lock in.
    const STALLED_DIR: &str = "AMUX_TEST_STALLED_DAEMON_DIR";

    /// A daemon stalled in its start: it takes the installation lock and
    /// names itself in it, as a daemon does first, and then serves
    /// nothing. This test's binary runs it as a child of the test below.
    #[test]
    fn stalled_daemon() {
        let Some(dir) = std::env::var_os(STALLED_DIR) else {
            return;
        };
        std::fs::create_dir_all(&dir).unwrap();
        let mut lock =
            os::lock::FileLock::try_acquire(&Path::new(&dir).join(node::INSTALLATION_LOCK))
                .unwrap()
                .expect("the lock is free");
        lock.write_data(std::process::id().to_string().as_bytes())
            .unwrap();
        std::thread::sleep(Duration::from_secs(600));
    }

    /// A daemon that took its lock and never opened its front door is
    /// still killed: there is no front door to name it, so its lock does.
    #[tokio::test]
    async fn killing_a_host_kills_a_daemon_stalled_before_its_front_door() {
        let root = tempfile::tempdir().unwrap();
        let host = ProcessHost {
            amux: PathBuf::from("amux"),
            config: root.path().join("config.toml"),
            data_dir: root.path().join("data"),
            front_door: root.path().join("data").join("amux.sock"),
            bin: root.path().join("bin"),
            bonjour: false,
            relay_ca: root.path().join("relay-ca.pem"),
        };
        let mut daemon = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "process::tests::stalled_daemon", "--nocapture"])
            .env(STALLED_DIR, &host.data_dir)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let lock = host.data_dir.join(node::INSTALLATION_LOCK);
        let deadline = Instant::now() + Duration::from_secs(30);
        while !(lock_held(&lock).unwrap() && host.locked_daemon().unwrap().is_some()) {
            assert!(
                Instant::now() < deadline,
                "the stalled daemon never took its lock"
            );
            assert!(
                daemon.try_wait().unwrap().is_none(),
                "the stalled daemon exited"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            host.daemon().await.unwrap().is_none(),
            "it serves no front door"
        );
        let killed = host.kill_all().await;
        if killed.is_err() {
            let _ = daemon.kill();
        }
        let status = daemon.wait().unwrap();
        killed.unwrap();
        assert!(!status.success(), "killed, not exited: {status:?}");
        assert!(!lock_held(&lock).unwrap());
    }
}
