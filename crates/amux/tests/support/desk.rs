//! A desktop install in a temporary directory, driven through the amux CLI
//! the way a person runs it, for the system tests and journeys.

use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};

use provider_fakes::{SCRIPT_ENV, Script};
use tokio::process::Command;

use super::{amux_binary, binaries, texts};

/// How long agents outlive their daemon before they drain and exit, when a
/// test wants to see them go.
pub const GRACE_SECS: u64 = 3;
/// Long enough to ride out any restart a test makes.
pub const LONG_GRACE_SECS: u64 = 30;

/// Prints one line of the transcript.
pub fn say(line: impl AsRef<str>) {
    println!("{}", line.as_ref());
}

/// A desktop install in a temporary directory: its binary a copy the
/// supervisor may replace, `claude` and `codex` the fakes, all playing one
/// script.
pub struct Desk {
    pub root: tempfile::TempDir,
    pub bin: PathBuf,
    pub config: PathBuf,
    pub socket: PathBuf,
    pub data: PathBuf,
    pub work: PathBuf,
    /// Left running for other clients: nothing is stopped or removed.
    kept: bool,
}

impl Desk {
    pub fn new(
        supervised: bool,
        grace_secs: u64,
        installed_version: &str,
        releases_url: &str,
        steps: Vec<provider_fakes::Step>,
    ) -> Self {
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join("bin");
        let work = root.path().join("work");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&work).unwrap();
        node::release::restamp(amux_binary(), &bin.join("amux"), installed_version).unwrap();
        // One `claude` for both Claude kinds, as on a real machine: the
        // headless kind is the one started with --print.
        let claude = bin.join("claude");
        std::fs::write(
            &claude,
            format!(
                "#!/bin/sh\nfor arg in \"$@\"; do [ \"$arg\" = --print ] && exec {sdk} \"$@\"; done\nexec {pty} \"$@\"\n",
                sdk = binaries().join("fake-claude-sdk").display(),
                pty = binaries().join("fake-claude-pty").display(),
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::os::unix::fs::symlink(binaries().join("fake-codex"), bin.join("codex")).unwrap();
        std::fs::write(
            root.path().join("script.json"),
            serde_json::to_string(&Script {
                steps,
                ..Script::default()
            })
            .unwrap(),
        )
        .unwrap();
        let data = root.path().join("data");
        let socket = root.path().join("amux.sock");
        let config = root.path().join("config.toml");
        std::fs::write(
            &config,
            format!(
                "root = {:?}\nfront_door_socket = {:?}\nsupervisor = {supervised}\nupdates = \"manual\"\n\
                 keep_awake = false\nreleases_url = {releases_url:?}\n\
                 [agent]\ngrace_secs = {grace_secs}\ndrain_secs = 5\n",
                data.display().to_string(),
                socket.display().to_string(),
            ),
        )
        .unwrap();
        Self {
            bin,
            config,
            socket,
            data,
            work,
            root,
            kept: false,
        }
    }

    pub fn amux_path(&self) -> PathBuf {
        self.bin.join("amux")
    }

    pub fn command(&self, args: &[&str]) -> Command {
        let path = format!(
            "{}:{}",
            self.bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut command = Command::new(self.amux_path());
        command
            .args(args)
            .env("AMUX_CONFIG", &self.config)
            .env("PATH", path)
            .env(SCRIPT_ENV, self.root.path().join("script.json"))
            .env("CLAUDE_CONFIG_DIR", self.root.path().join("claude"))
            .env(super::NO_DISCOVERY.0, super::NO_DISCOVERY.1)
            .env_remove("AMUX_LOG")
            .stdin(Stdio::null())
            .kill_on_drop(true);
        command
    }

    pub async fn amux(&self, args: &[&str]) -> Output {
        tokio::time::timeout(super::PATIENCE, self.command(args).output())
            .await
            .unwrap_or_else(|_| panic!("amux {args:?} did not finish"))
            .expect("amux runs")
    }

    /// Runs a verb that must succeed, printing it and what it said.
    pub async fn run(&self, args: &[&str]) -> String {
        let output = self.amux(args).await;
        assert!(
            output.status.success(),
            "amux {args:?} failed: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let said = String::from_utf8(output.stdout).unwrap();
        let root = self.root.path().to_string_lossy().into_owned();
        say(format!("$ amux {}", args.join(" ")).replace(&root, "$DESK"));
        for line in said.lines() {
            say(format!("  {line}").replace(&root, "$DESK"));
        }
        said
    }

    pub fn agent_dir(&self, id: &[u8]) -> PathBuf {
        let profiles = self.data.join("profiles");
        let profile = std::fs::read_dir(&profiles)
            .unwrap()
            .next()
            .expect("a profile directory")
            .unwrap()
            .path();
        profile
            .join("agents")
            .join(uuid::Uuid::from_slice(id).unwrap().to_string())
    }

    pub fn supervisor_pid(&self) -> Option<i32> {
        let path = self.data.join(node::supervisor::SUPERVISOR_LOCK);
        lock_held(&path)
            .then(|| std::fs::read_to_string(&path).ok()?.trim().parse().ok())
            .flatten()
    }

    /// The daemon the supervisor started last, from its log.
    pub fn daemon_pid(&self) -> i32 {
        let log = std::fs::read_to_string(self.data.join("supervisor.log")).unwrap();
        log.lines()
            .rev()
            .find_map(|line| {
                line.contains("started the daemon")
                    .then(|| line.rsplit_once("pid=")?.1.trim().parse().ok())
                    .flatten()
            })
            .expect("the supervisor started a daemon")
    }

    pub fn daemon_running(&self) -> bool {
        lock_held(&self.data.join(node::INSTALLATION_LOCK))
    }
}

impl Desk {
    /// Leaves the install and everything it runs in place, and says where.
    pub fn keep(mut self) -> PathBuf {
        self.kept = true;
        self.root.disable_cleanup(true);
        self.root.path().to_owned()
    }
}

impl Drop for Desk {
    /// Stops amux, then kills whatever a failed test left running from this
    /// install: agents mid-turn outlive their daemon on purpose.
    fn drop(&mut self) {
        if self.kept {
            return;
        }
        let _ = std::process::Command::new(self.amux_path())
            .args(["server", "stop"])
            .env("AMUX_CONFIG", &self.config)
            .env(super::NO_DISCOVERY.0, super::NO_DISCOVERY.1)
            .env_remove("AMUX_LOG")
            .output();
        let _ = std::process::Command::new("pkill")
            .args(["-9", "-f"])
            .arg(self.root.path())
            .output();
    }
}

/// The chat's items that say something, for the transcript.
pub fn spoken(chat: &[(String, String)]) -> Vec<&str> {
    texts(chat)
        .into_iter()
        .filter(|text| !text.is_empty())
        .collect()
}

/// Whether some process holds the lock on the file at `path`.
pub fn lock_held(path: &Path) -> bool {
    let Ok(file) = OpenOptions::new().write(true).open(path) else {
        return false;
    };
    matches!(file.try_lock(), Err(std::fs::TryLockError::WouldBlock))
}

pub fn kill(pid: i32) {
    // SAFETY: a signal to a process this test started.
    unsafe {
        libc::kill(pid, libc::SIGKILL);
    }
}
