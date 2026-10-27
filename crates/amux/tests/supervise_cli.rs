//! The CLI around the supervisor, driving the real binary: the login units
//! `amux init` writes, stop going to the owner in both install cells, a
//! client starting the supervisor only where the install has one, the sleep
//! assertion, `amux update` and `amux config channel`.

#![cfg(unix)]

mod support;

use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::Duration;

use node::release;
use node::supervisor::SUPERVISOR_LOCK;
use node::supervisor::login::LoginItem;
use patience::{holds_for, until_within};
use support::{Channel, PATIENCE, amux_binary};

fn goldens() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/goldens/login")
}

fn golden(name: &str, actual: &str) {
    let path = goldens().join(name);
    if std::env::var_os("AMUX_UPDATE_GOLDENS").is_some() {
        std::fs::create_dir_all(goldens()).unwrap();
        std::fs::write(&path, actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!(
            "{} is missing; AMUX_UPDATE_GOLDENS=1 writes it",
            path.display()
        )
    });
    assert_eq!(actual, expected, "{name} differs from its golden");
}

#[test]
fn the_login_units_match_their_goldens() {
    let item = LoginItem {
        binary: Path::new("/Users/someone/.amux/bin/amux"),
        config: Some(Path::new(
            "/Users/someone/Library/Application Support/amux/config & more.toml",
        )),
        path: "/opt/homebrew/bin:/usr/bin:/bin",
        log: Path::new("/Users/someone/.amux/supervisor.log"),
    };
    let launch_agent = item.launch_agent();
    golden("sh.amux.supervise.plist", &launch_agent);
    let unit = item.systemd_unit();
    golden("amux.service", &unit);
    let task = item.windows_task();
    golden("amux-task.xml", &task);

    // What keeps the agents alive and the supervisor coming back on
    // failure only.
    assert!(launch_agent.contains("<key>AbandonProcessGroup</key>\n\t<true/>"));
    assert!(launch_agent.contains("<key>SuccessfulExit</key>\n\t\t<false/>"));
    assert!(unit.contains("\nKillMode=process\n"));
    assert!(unit.contains("\nRestart=on-failure\n"));
    assert!(!unit.contains("Restart=always"));
    assert!(task.contains("<LogonTrigger>") && task.contains("<RestartOnFailure>"));
}

/// A temporary install.
struct Install {
    root: tempfile::TempDir,
    config: PathBuf,
    data: PathBuf,
    socket: PathBuf,
    binary: PathBuf,
}

impl Install {
    fn new(supervisor: bool, extra: &str) -> Self {
        Self::with_binary(supervisor, extra, amux_binary().to_owned())
    }

    fn with_binary(supervisor: bool, extra: &str, binary: PathBuf) -> Self {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("data");
        let socket = root.path().join("amux.sock");
        let config = root.path().join("config.toml");
        // Tests do not keep the machine awake unless they say so.
        let keep_awake = if extra.contains("keep_awake") {
            ""
        } else {
            "keep_awake = false\n"
        };
        std::fs::write(
            &config,
            format!(
                "root = {:?}\nfront_door_socket = {:?}\nsupervisor = {supervisor}\nupdates = \"manual\"\n{keep_awake}{extra}",
                data.display().to_string(),
                socket.display().to_string(),
            ),
        )
        .unwrap();
        Self {
            root,
            config,
            data,
            socket,
            binary,
        }
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(&self.binary);
        command
            .args(args)
            .env("AMUX_CONFIG", &self.config)
            .env("HOME", self.root.path().join("home"))
            .env(support::NO_DISCOVERY.0, support::NO_DISCOVERY.1)
            .env_remove("AMUX_LOG")
            .env_remove("XDG_CONFIG_HOME")
            .stdin(Stdio::null());
        command
    }

    fn amux(&self, args: &[&str]) -> Output {
        let output = self.command(args).output().unwrap();
        assert!(
            output.status.success(),
            "amux {args:?} failed:\n{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn supervisor_pid(&self) -> Option<u32> {
        let path = self.data.join(SUPERVISOR_LOCK);
        locked(&path).then(|| {
            std::fs::read_to_string(&path)
                .unwrap()
                .trim()
                .parse()
                .unwrap()
        })
    }

    fn daemon_running(&self) -> bool {
        locked(&self.data.join(node::INSTALLATION_LOCK))
    }

    fn answers(&self) -> bool {
        std::os::unix::net::UnixStream::connect(&self.socket).is_ok()
    }
}

impl Drop for Install {
    fn drop(&mut self) {
        if self.supervisor_pid().is_some() || self.daemon_running() {
            let _ = self.command(&["server", "stop"]).output();
        }
    }
}

fn locked(path: &Path) -> bool {
    let Ok(file) = OpenOptions::new().write(true).open(path) else {
        return false;
    };
    matches!(file.try_lock(), Err(std::fs::TryLockError::WouldBlock))
}

fn alive(pid: u32) -> bool {
    // SAFETY: signal 0 only probes.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// The shared wait at this crate's patience, over a plain check.
async fn until(what: &str, done: impl Fn() -> bool) {
    until_within(what, PATIENCE, || {
        std::future::ready(done().then_some(()).ok_or("not yet"))
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn stop_goes_to_the_supervisor_first_where_the_install_has_one() {
    let install = Install::new(true, "");
    let started = install.amux(&["server", "start"]);
    assert!(
        stdout(&started).contains("Started amux."),
        "{}",
        stdout(&started)
    );
    let supervisor = install
        .supervisor_pid()
        .expect("server start ran amux supervise");
    assert!(install.daemon_running());

    let stopped = install.amux(&["server", "stop"]);
    assert!(
        stdout(&stopped).contains("Stopped amux supervise and its daemon."),
        "{}",
        stdout(&stopped)
    );
    assert!(install.supervisor_pid().is_none());
    assert!(!install.daemon_running());
    until("the supervisor to exit", || !alive(supervisor)).await;
    // Stopped for good: nothing brings the daemon back. A window, since a
    // restart that must not happen leaves no mark to wait on.
    holds_for(
        "the daemon to stay down",
        Duration::from_secs(2),
        || async { !install.answers() && !install.daemon_running() },
    )
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn stop_goes_to_the_daemon_where_the_install_has_no_supervisor() {
    let install = Install::new(false, "");
    install.amux(&["server", "start"]);
    assert!(install.daemon_running());
    assert!(
        install.supervisor_pid().is_none(),
        "no supervisor without one"
    );

    let stopped = install.amux(&["server", "stop"]);
    assert!(
        stdout(&stopped).contains("Stopped the amux daemon."),
        "{}",
        stdout(&stopped)
    );
    assert!(!install.daemon_running());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_client_starts_the_supervisor_only_where_the_install_has_one() {
    let with = Install::new(true, "");
    let listed = with.command(&["ls"]).output().unwrap();
    assert!(
        listed.status.success(),
        "{}",
        String::from_utf8_lossy(&listed.stderr)
    );
    assert!(
        String::from_utf8_lossy(&listed.stderr).contains("Starting amux supervise"),
        "{}",
        String::from_utf8_lossy(&listed.stderr)
    );
    assert!(with.supervisor_pid().is_some());
    assert!(with.daemon_running());

    let without = Install::new(false, "");
    let mut waiting = without
        .command(&["ls"])
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // A window: a client that must keep waiting leaves no mark to wait on.
    holds_for("the client to keep waiting", Duration::from_secs(2), || {
        let running = waiting.try_wait().unwrap().is_none();
        async move { running }
    })
    .await
    .expect("the client waits for the service manager");
    waiting.kill().unwrap();
    let output = waiting.wait_with_output().unwrap();
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("belongs to the service manager"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(without.supervisor_pid().is_none());
    assert!(
        !without.daemon_running(),
        "a client never starts the daemon itself"
    );
}

// Built on every Unix so the test listing names it wherever the contracts
// are checked; only macOS has the assertion to look for.
#[cfg_attr(
    not(target_os = "macos"),
    ignore = "only macOS holds a sleep assertion"
)]
#[tokio::test(flavor = "multi_thread")]
async fn the_supervisor_holds_the_sleep_assertion_under_keep_awake() {
    fn assertions_of(pid: u32) -> Vec<String> {
        let output = Command::new("pmset")
            .args(["-g", "assertions"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|line| {
                line.contains(&format!("pid {pid}("))
                    && line.contains(node::supervisor::KEEP_AWAKE_REASON)
            })
            .map(str::to_owned)
            .collect()
    }

    let awake = Install::new(true, "keep_awake = true\n");
    awake.amux(&["server", "start"]);
    let pid = awake.supervisor_pid().unwrap();
    let held = assertions_of(pid);
    assert!(
        held.iter().any(|line| line.contains("PreventSystemSleep")),
        "{held:?}"
    );
    assert!(
        held.iter()
            .any(|line| line.contains("PreventUserIdleSystemSleep")),
        "{held:?}"
    );
    awake.amux(&["server", "stop"]);
    assert!(
        assertions_of(pid).is_empty(),
        "released with the supervisor"
    );

    let asleep = Install::new(true, "keep_awake = false\n");
    asleep.amux(&["server", "start"]);
    let pid = asleep.supervisor_pid().unwrap();
    assert!(assertions_of(pid).is_empty());
}

#[test]
fn init_writes_a_login_item_that_runs_amux_supervise() {
    let install = Install::new(true, "");
    let output = install.amux(&["init", "--login-item", "yes", "--dry-run"]);
    let home = install.root.path().join("home");
    let path = std::env::var("PATH").unwrap_or_default();
    let item = LoginItem {
        binary: amux_binary(),
        config: Some(&install.config),
        path: &path,
        // The config resolves its root; the login item logs under it.
        log: &settings::InstallationConfig::from_file(&install.config)
            .unwrap()
            .root
            .join("supervisor.log"),
    };
    #[cfg(target_os = "macos")]
    let (file, expected, registers) = (
        home.join("Library/LaunchAgents/sh.amux.supervise.plist"),
        item.launch_agent(),
        "launchctl bootstrap gui/",
    );
    #[cfg(not(target_os = "macos"))]
    let (file, expected, registers) = (
        home.join(".config/systemd/user/amux.service"),
        item.systemd_unit(),
        "systemctl --user enable --now amux.service",
    );
    assert_eq!(std::fs::read_to_string(&file).unwrap(), expected);
    assert!(stdout(&output).contains(registers), "{}", stdout(&output));

    let declined = install.amux(&["init", "--login-item", "no"]);
    assert!(stdout(&declined).contains("No login item"));

    let without = Install::new(false, "");
    let output = without.amux(&["init", "--login-item", "yes", "--dry-run"]);
    assert!(
        stdout(&output).contains("no login item to add"),
        "{}",
        stdout(&output)
    );
    assert!(!without.root.path().join("home").exists());
}

#[test]
fn config_channel_sets_the_channel_and_keeps_the_rest() {
    let install = Install::new(
        true,
        "# the desk in the study\nhost_name = \"desk\"\n\n[ui]\ncolor = \"ansi\" # for tmux\n",
    );
    let output = install.amux(&["config", "channel", "preview"]);
    assert!(stdout(&output).contains("Channel: preview."));
    let config = settings::InstallationConfig::from_file(&install.config).unwrap();
    assert_eq!(config.channel, settings::Channel::Preview);
    assert_eq!(config.host_name(), "desk");
    assert_eq!(config.ui.color, settings::ColorSetting::Ansi);
    assert!(config.manifest_url().ends_with("/preview.json"));
    let text = std::fs::read_to_string(&install.config).unwrap();
    assert!(
        text.contains("# the desk in the study") && text.contains("# for tmux"),
        "{text}"
    );

    install.amux(&["config", "channel", "stable"]);
    let config = settings::InstallationConfig::from_file(&install.config).unwrap();
    assert_eq!(config.channel, settings::Channel::Stable);

    let refused = install
        .command(&["config", "channel", "nightly"])
        .output()
        .unwrap();
    assert!(!refused.status.success());
}

#[tokio::test(flavor = "multi_thread")]
async fn update_says_updates_are_deploys_without_a_supervisor() {
    let install = Install::new(false, "");
    let output = install.amux(&["update"]);
    assert!(
        stdout(&output).contains("updates are deploys"),
        "{}",
        stdout(&output)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn update_retry_installs_the_channel_build_now_even_one_rolled_back_here() {
    let builds = tempfile::tempdir().unwrap();
    let bin = builds.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let installed = bin.join("amux");
    release::restamp(amux_binary(), &installed, "1.0.0").unwrap();
    let newer = builds.path().join("amux-2.0.0");
    release::restamp(amux_binary(), &newer, "2.0.0").unwrap();
    let channel = Channel::serve().await;
    channel.publish("2.0.0", std::fs::read(&newer).unwrap());
    // Rolled back here before: the hourly check would skip it.
    std::fs::write(bin.join("amux.rejected"), "2.0.0").unwrap();

    let install = Install::with_binary(
        true,
        &format!("releases_url = \"{}\"\n", channel.url()),
        installed.clone(),
    );
    install.amux(&["server", "start"]);
    let supervisor = install.supervisor_pid().unwrap();

    let output = install.amux(&["update", "--retry"]);
    let said = stdout(&output);
    println!(
        "$ amux update --retry   (1.0.0 installed; the channel names 2.0.0, rolled back here before)\n{said}"
    );
    assert!(said.contains("Installing amux 2.0.0."), "{said}");
    assert!(said.contains("amux 2.0.0 is running."), "{said}");
    let version = Command::new(&installed).arg("--version").output().unwrap();
    assert_eq!(stdout(&version).trim(), "amux 2.0.0");
    assert_eq!(
        install.supervisor_pid(),
        Some(supervisor),
        "the supervisor exec'd in place"
    );

    let again = install.amux(&["update"]);
    println!("$ amux update\n{}", stdout(&again));
    assert!(
        stdout(&again).contains("Up to date: 2.0.0 is running and the channel names 2.0.0."),
        "{}",
        stdout(&again)
    );
}

/// A build that never starts is rolled back by the supervisor; `amux
/// update` says so and fails, rather than leaving the person to read the
/// log, and a later `amux update` leaves that build alone unless retried.
#[tokio::test(flavor = "multi_thread")]
async fn update_reports_a_build_that_never_started_and_then_leaves_it_alone() {
    let builds = tempfile::tempdir().unwrap();
    let bin = builds.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let installed = bin.join("amux");
    release::restamp(amux_binary(), &installed, "1.0.0").unwrap();
    let channel = Channel::serve().await;
    channel.publish("2.0.0", b"#!/bin/sh\nexit 3\n".to_vec());

    let install = Install::with_binary(
        true,
        &format!("releases_url = \"{}\"\n", channel.url()),
        installed.clone(),
    );
    install.amux(&["server", "start"]);

    let output = install.command(&["update"]).output().unwrap();
    let said = format!(
        "{}{}",
        stdout(&output),
        String::from_utf8_lossy(&output.stderr)
    );
    println!(
        "$ amux update   (1.0.0 installed; the channel names 2.0.0, which exits at once)\n{said}"
    );
    assert!(!output.status.success(), "{said}");
    assert!(said.contains("Installing amux 2.0.0."), "{said}");
    assert!(
        said.contains("amux 2.0.0 did not start, so the supervisor put amux 1.0.0 back."),
        "{said}"
    );
    assert!(
        said.contains("`amux update --retry` tries 2.0.0 again"),
        "{said}"
    );
    let version = Command::new(&installed).arg("--version").output().unwrap();
    assert_eq!(stdout(&version).trim(), "amux 1.0.0");

    let again = install.amux(&["update"]);
    println!("$ amux update\n{}", stdout(&again));
    assert!(
        stdout(&again)
            .contains("2.0.0 was rolled back here; `amux update --retry` tries it again."),
        "{}",
        stdout(&again)
    );
}
