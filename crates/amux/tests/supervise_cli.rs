//! The CLI around the supervisor, driving the real binary: the login item
//! `start_at_login` keeps, stop going to the owner in both install cells, a
//! client starting the supervisor only where the install has one, stop
//! reaching an amux started with its stop signals blocked, the sleep
//! assertion, `amux update` and `amux config channel`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::Duration;

use node::release;
use node::supervisor::SUPERVISOR_LOCK;
use node::supervisor::login::{ScratchItems, StartAtLogin};
use patience::{holds_for, until_within};
use support::{Channel, PATIENCE, agent_pid, amux_binary};

use crate::support;

/// A temporary install.
struct Install {
    root: tempfile::TempDir,
    config: PathBuf,
    data: PathBuf,
    socket: PathBuf,
    binary: PathBuf,
    /// Where its login items go: its home on Unix, a scratch registry key
    /// on Windows, so no test reaches the person's own.
    login: ScratchItems,
}

impl Install {
    fn new(supervisor: bool, extra: &str) -> Self {
        Self::with_binary(supervisor, extra, amux_binary().to_owned())
    }

    fn with_binary(supervisor: bool, extra: &str, binary: PathBuf) -> Self {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("data");
        let socket = data.join("amux.sock");
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
                "root = {:?}\nsupervisor = {supervisor}\nupdates = \"manual\"\n{keep_awake}{extra}\n{}",
                data.display().to_string(),
                crate::support::NO_REAL_AGENTS,
            ),
        )
        .unwrap();
        let login = ScratchItems::new(&root.path().join("home"));
        Self {
            root,
            config,
            data,
            socket,
            binary,
            login,
        }
    }

    fn command(&self, args: &[&str]) -> Command {
        self.run(&self.binary, args)
    }

    /// `program <args>` in this install's environment.
    fn run(&self, program: &Path, args: &[&str]) -> Command {
        let mut command = Command::new(program);
        command
            .args(args)
            .env("AMUX_CONFIG", &self.config)
            .env("HOME", self.root.path().join("home"))
            // Windows' homes, so nothing a test runs reaches the person's.
            .env("USERPROFILE", self.root.path().join("home"))
            .env("APPDATA", self.root.path().join("home").join("AppData"))
            .env("LOCALAPPDATA", self.root.path().join("home").join("Local"))
            .env(support::NO_DISCOVERY.0, support::NO_DISCOVERY.1)
            .env(self.login.env().0, self.login.env().1)
            .env_remove("AMUX_LOG")
            .env_remove("XDG_CONFIG_HOME")
            .stdin(Stdio::null());
        command
    }

    /// This install's login item, as its amux would write it.
    fn login_item(&self) -> StartAtLogin {
        StartAtLogin {
            binary: self.binary.clone(),
            config: Some(self.config.clone()),
            path: String::new(),
            root: self.data.clone(),
            items: self.login.items().to_owned(),
            register: false,
        }
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

    /// Whether the daemon answers on its socket. The tests' runtimes have
    /// threads to spare, so this one blocks for the dial.
    fn answers(&self) -> bool {
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current()
                .block_on(agent_dir::local_socket::connect(&self.socket))
                .is_ok()
        })
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
    os::lock::FileLock::held(path).unwrap_or(false)
}

fn alive(pid: u32) -> bool {
    os::process::ProcessRef::open(pid).is_ok_and(|process| process.alive())
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

/// `amux server start` with its output piped ends when `start` does: the
/// supervisor and daemon it leaves running hold no end of the caller's
/// pipes, so a caller reading to the end (a desktop app's installer,
/// `amux server start | cat`) isn't kept waiting on them. On Windows, std
/// starts every child inheriting every inheritable handle, the explicit
/// `Stdio::null` notwithstanding, so this holds only because `main` clears
/// inheritance on its own standard handles first.
#[test]
fn a_piped_server_start_ends_when_start_does() {
    use std::io::Read as _;

    let install = Install::new(true, "start_at_login = false");
    let temp = install.root.path().join("temp");
    std::fs::create_dir_all(&temp).unwrap();
    let mut start = install
        .command(&["server", "start"])
        .env("TEMP", &temp)
        .env("TMP", &temp)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let to_the_end = |mut pipe: Box<dyn std::io::Read + Send>| {
        let (sent, read) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut text = String::new();
            let _ = pipe.read_to_string(&mut text);
            let _ = sent.send(text);
        });
        read
    };
    let out = to_the_end(Box::new(start.stdout.take().unwrap()));
    let err = to_the_end(Box::new(start.stderr.take().unwrap()));
    let bound = std::time::Instant::now() + Duration::from_secs(10);
    let within = || bound.saturating_duration_since(std::time::Instant::now());
    let said = out.recv_timeout(within());
    let complained = err.recv_timeout(within());
    let status = start.wait().unwrap();
    // What it left running shows no window either: each says so as it
    // starts, the daemon too, which on Windows a console-less supervisor
    // would otherwise start with a console of its own, and a visible one.
    let logs: Vec<(&str, String)> = ["supervisor.log", "daemon.log"]
        .into_iter()
        .map(|log| {
            let text = std::fs::read_to_string(install.data.join(log)).unwrap_or_default();
            (log, text)
        })
        .collect();
    // Stopped before any assertion, so a failure leaves nothing running;
    // the pipes' readers end once what held them is gone.
    let stopped = install.command(&["server", "stop"]).output().unwrap();
    let said = said.expect("stdout reached its end within 10 s of `server start`");
    let complained = complained.expect("stderr reached its end within 10 s of `server start`");
    assert!(status.success(), "{status}: {said}{complained}");
    assert!(said.contains("Started amux."), "{said}{complained}");
    assert!(stopped.status.success(), "{}", stdout(&stopped));
    for (log, text) in logs {
        assert!(text.contains("console window=false"), "{log}:\n{text}");
    }
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

/// Ends a process when dropped, so a test that fails leaves no launcher
/// starting supervisors.
struct Ending(std::process::Child);

impl Drop for Ending {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Windows' login item runs `amux-launch supervise --config <path>`: the
/// launcher beside amux, which starts the supervisor with no window and
/// starts it again when it dies. `amux server stop` ends it too: the
/// supervisor says the stop on the launcher's pipe as it exits, and the
/// launcher starts nothing again and exits.
#[tokio::test(flavor = "multi_thread")]
async fn server_stop_ends_the_launcher_that_started_the_supervisor() {
    // Both copied into one folder, as an install has them: the launcher
    // runs the amux beside it, and a Unix test's binaries are links to
    // folders of their own.
    let bin = tempfile::tempdir().unwrap();
    let (amux, launcher) = (
        bin.path().join(os::paths::exe_name("amux")),
        bin.path().join(os::paths::exe_name("amux-launch")),
    );
    std::fs::copy(amux_binary(), &amux).unwrap();
    std::fs::copy(provider_fakes::binaries::path("amux-launch"), &launcher).unwrap();
    let install = Install::with_binary(true, "", amux);
    let config = install.config.to_str().unwrap().to_owned();
    let launcher = install
        .run(&launcher, &["supervise", "--config", &config])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let launcher = std::cell::RefCell::new(Ending(launcher));
    until("the launched supervisor's daemon to answer", || {
        install.answers()
    })
    .await;
    let supervisor = install
        .supervisor_pid()
        .expect("the launcher ran amux supervise");

    let stopped = install.amux(&["server", "stop"]);
    assert!(
        stdout(&stopped).contains("Stopped amux supervise and its daemon."),
        "{}",
        stdout(&stopped)
    );
    until("the launcher to exit", || {
        launcher.borrow_mut().0.try_wait().unwrap().is_some()
    })
    .await;
    let status = launcher.borrow_mut().0.wait().unwrap();
    assert!(status.success(), "{status}");
    assert!(!alive(supervisor));
    holds_for("nothing to start again", Duration::from_secs(2), || async {
        install.supervisor_pid().is_none() && !install.answers()
    })
    .await
    .unwrap();
}

/// A supervisor that dies under the launcher is started again after a
/// backoff. `amux server stop` during that wait finds no supervisor to
/// tell, so it records the stop in the root; the supervisor the launcher
/// starts next reads it, says the stop on the launcher's pipe and exits
/// without starting a daemon, and the launcher exits. amux is moved away
/// while the stop runs, so each restart the launcher tries fails until
/// it is back, and the stop lands in the wait however the timing falls.
#[tokio::test(flavor = "multi_thread")]
async fn server_stop_while_the_launcher_waits_to_restart_ends_the_launcher() {
    let bin = tempfile::tempdir().unwrap();
    let (amux, away, launcher) = (
        bin.path().join(os::paths::exe_name("amux")),
        bin.path().join(os::paths::exe_name("amux-away")),
        bin.path().join(os::paths::exe_name("amux-launch")),
    );
    std::fs::copy(amux_binary(), &amux).unwrap();
    std::fs::copy(provider_fakes::binaries::path("amux-launch"), &launcher).unwrap();
    let install = Install::with_binary(true, "", amux.clone());
    let config = install.config.to_str().unwrap().to_owned();
    let launcher = install
        .run(&launcher, &["supervise", "--config", &config])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let launcher = std::cell::RefCell::new(Ending(launcher));
    until("the launched supervisor's daemon to answer", || {
        install.answers()
    })
    .await;
    let supervisor = install
        .supervisor_pid()
        .expect("the launcher ran amux supervise");

    // A running program can be renamed, on Windows too.
    std::fs::rename(&amux, &away).unwrap();
    os::process::ProcessRef::open(supervisor)
        .and_then(|process| process.kill())
        .expect("killing the supervisor");
    until("the supervisor and its daemon to end", || {
        !alive(supervisor) && install.supervisor_pid().is_none() && !install.daemon_running()
    })
    .await;
    let stopped = install.run(&away, &["server", "stop"]).output().unwrap();
    assert!(stopped.status.success(), "{}", stdout(&stopped));
    assert!(
        stdout(&stopped).contains("The amux daemon is not running."),
        "no supervisor answered: {}",
        stdout(&stopped)
    );
    assert!(install.data.join(node::supervisor::STOPPED).exists());
    std::fs::rename(&away, &amux).unwrap();

    until("the launcher to exit", || {
        launcher.borrow_mut().0.try_wait().unwrap().is_some()
    })
    .await;
    let status = launcher.borrow_mut().0.wait().unwrap();
    assert!(status.success(), "{status}");
    let log = std::fs::read_to_string(install.data.join("supervisor.log")).unwrap();
    assert!(
        log.contains("waited to start amux again: staying stopped"),
        "the restarted supervisor read the stop:\n{log}"
    );
    assert!(
        !install.data.join(node::supervisor::STOPPED).exists(),
        "the stop was taken back as it was honoured"
    );
    holds_for("nothing to start again", Duration::from_secs(2), || async {
        install.supervisor_pid().is_none() && !install.answers() && !install.daemon_running()
    })
    .await
    .unwrap();
}

/// Both programs an install puts on Windows carry the application manifest
/// (no elevation, Windows 10 and 11, long paths, UTF-8) and their version,
/// as built: the binaries under test are the ones a release ships. A
/// program built for anything else is no PE file and carries neither.
#[test]
fn the_windows_programs_carry_their_manifest_and_version() {
    // amux's own version, and the launcher's crate's.
    for (program, version) in [
        (amux_binary().to_owned(), Some(env!("CARGO_PKG_VERSION"))),
        (provider_fakes::binaries::path("amux-launch"), None),
    ] {
        let image = std::fs::read(&program).unwrap();
        let embedded = exe_resource::embedded(&image);
        assert_eq!(embedded.is_some(), cfg!(windows), "{}", program.display());
        let Some(embedded) = embedded else { continue };
        assert_eq!(embedded.manifest, exe_resource::MANIFEST);
        for needed in ["asInvoker", "longPathAware", "UTF-8", "supportedOS"] {
            assert!(embedded.manifest.contains(needed), "{needed}");
        }
        let shown = embedded.string("ProductVersion").expect("a version");
        assert_eq!(embedded.string("FileVersion"), Some(shown));
        assert_eq!(embedded.file_version, exe_resource::version_numbers(shown));
        if let Some(version) = version {
            assert_eq!(shown, version);
        }
        let stem = program.file_stem().unwrap().to_string_lossy();
        assert_eq!(
            embedded.string("OriginalFilename"),
            Some(format!("{stem}.exe").as_str())
        );
    }
}

/// A key of the test's own under `HKCU\Software\amux-test-…`, standing in
/// for `HKCU\Environment` (the user's PATH) on Windows, written and read
/// with reg.exe and deleted when dropped. Elsewhere reg.exe is missing and
/// it holds nothing.
struct ScratchEnvironment {
    key: String,
}

impl ScratchEnvironment {
    fn new() -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        Self {
            key: format!(r"Software\amux-test-env-{}-{nanos}", std::process::id()),
        }
    }

    fn reg(&self, verb: &str, arguments: &[&str]) -> Option<Output> {
        Command::new("reg")
            .arg(verb)
            .arg(format!(r"HKCU\{}", self.key))
            .args(arguments)
            .stdin(Stdio::null())
            .output()
            .ok()
            .filter(|output| output.status.success())
    }

    fn set_path(&self, value: &str) {
        self.reg(
            "add",
            &["/v", "Path", "/t", "REG_EXPAND_SZ", "/d", value, "/f"],
        )
        .expect("reg add");
    }

    /// The `Path` value's kind and value as stored, `%VARIABLES%` and all.
    fn path(&self) -> Option<String> {
        let output = self.reg("query", &["/v", "Path"])?;
        let text = String::from_utf8_lossy(&output.stdout).into_owned();
        let line = text
            .lines()
            .map(str::trim)
            .find(|line| line.starts_with("Path "))?;
        Some(line.split_once("    ")?.1.trim().to_owned())
    }
}

impl Drop for ScratchEnvironment {
    fn drop(&mut self) {
        let _ = self.reg("delete", &["/f"]);
    }
}

/// The default installation's login item under `items`, the `Run` value
/// `amux`, as reg.exe reads it; `None` where there is none (and wherever
/// there is no registry).
fn login_value(items: &Path) -> Option<String> {
    let output = Command::new("reg")
        .args(["query", &format!(r"HKCU\{}", items.display()), "/v", "amux"])
        .stdin(Stdio::null())
        .output()
        .ok()
        .filter(|output| output.status.success())?;
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    let line = text
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("amux "))?;
    let (_, value) = line.split_once("REG_SZ")?;
    Some(value.trim().to_owned())
}

/// `amux uninstall --yes --delete-data` of the default installation, run
/// by the installed amux as a person would, while the login item's
/// launcher runs it: it stops amux and so the launcher, removes the login
/// item and the scheduled task an earlier build registered, deletes both
/// programs and everything updates left beside them, takes their folder off
/// PATH, and deletes the root and the config. Windows refuses to delete a
/// running program, and the uninstalling amux is one: it is left as a
/// `amux.exe.old.<n>`, the one thing an uninstall leaves. Elsewhere
/// nothing is left at all.
#[tokio::test(flavor = "multi_thread")]
async fn uninstall_leaves_nothing_but_a_program_still_running() {
    let scratch = tempfile::tempdir().unwrap();
    let home = scratch.path().join("home");
    let bin = home.join(".amux").join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let (amux, launcher) = (
        bin.join(os::paths::exe_name("amux")),
        bin.join(os::paths::exe_name("amux-launch")),
    );
    std::fs::copy(amux_binary(), &amux).unwrap();
    std::fs::copy(provider_fakes::binaries::path("amux-launch"), &launcher).unwrap();
    // Windows keeps the login item and the user's PATH in the registry,
    // which this test points at keys of its own, and refuses to delete a
    // running program. Unix has a service manager no test registers with
    // (so no login item here), the PATH in a shell profile, and deletes
    // what runs.
    let windows = cfg!(windows);
    // On Windows, an amux that didn't read the test's keys would uninstall
    // the person's own login item and PATH entry: none runs here unless it
    // names both.
    let image = std::fs::read(&amux).unwrap();
    for seam in [
        os::paths::TEST_ENVIRONMENT_KEY,
        node::supervisor::login::TEST_RUN_KEY,
    ] {
        assert!(
            !windows
                || image
                    .windows(seam.len())
                    .any(|window| window == seam.as_bytes()),
            "the amux under test doesn't read {seam}: not run"
        );
    }
    // The default installation, in the scratch home: the config and root
    // amux finds by itself.
    let (appdata, local) = (home.join("AppData"), home.join("Local"));
    let (config, root) = if windows {
        (appdata.join("amux").join("config.toml"), local.join("amux"))
    } else {
        (
            home.join(".config").join("amux").join("config.toml"),
            home.join(".local").join("share").join("amux"),
        )
    };
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    std::fs::write(
        &config,
        format!(
            "supervisor = true\nupdates = \"manual\"\nkeep_awake = false\n{}",
            crate::support::NO_REAL_AGENTS
        ),
    )
    .unwrap();
    let login = ScratchItems::new(&home);
    let environment = ScratchEnvironment::new();
    let run = |program: &Path, args: &[&str]| {
        let mut command = Command::new(program);
        command
            .args(args)
            .env_remove("AMUX_CONFIG")
            .env_remove("AMUX_LOG")
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("XDG_DATA_HOME")
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .env("APPDATA", &appdata)
            .env("LOCALAPPDATA", &local)
            .env(support::NO_DISCOVERY.0, support::NO_DISCOVERY.1)
            .env(login.env().0, login.env().1)
            .env(os::paths::TEST_ENVIRONMENT_KEY, &environment.key)
            .stdin(Stdio::null());
        command
    };

    // The folder on PATH, as each platform's install script puts it.
    // amux runs with a scratch USERPROFILE: an entry expanded and written
    // back would name the scratch folder for good.
    let kept = r"%USERPROFILE%\AppData\Local\Microsoft\WindowsApps;%SystemRoot%\keep;C:\elsewhere";
    if windows {
        environment.set_path(&format!(
            r"%USERPROFILE%\AppData\Local\Microsoft\WindowsApps;%SystemRoot%\keep;{};C:\elsewhere",
            bin.display()
        ));
    }
    let profile = home.join(".zshrc");
    std::fs::write(
        &profile,
        "# mine\n\n# Added by amux installer\nexport PATH=\"$HOME/.amux/bin:$PATH\"\n",
    )
    .unwrap();

    if windows {
        let on = run(&amux, &["config", "set", "start_at_login", "true"])
            .output()
            .unwrap();
        assert!(on.status.success(), "{}", stdout(&on));
        let value = login_value(login.items()).unwrap_or_default();
        assert!(
            value.contains("amux-launch") && value.ends_with("supervise"),
            "{value}\n{}",
            stdout(&on)
        );
    }
    // The launcher runs amux supervise, as the login item would start it.
    let launched = run(&launcher, &["supervise"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let launched = std::cell::RefCell::new(Ending(launched));
    let socket = root.join("amux.sock");
    let answers = || {
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current()
                .block_on(agent_dir::local_socket::connect(&socket))
                .is_ok()
        })
    };
    until("the launched supervisor's daemon to answer", answers).await;
    let supervisor: u32 = std::fs::read_to_string(root.join(SUPERVISOR_LOCK))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    // What updates leave beside each program, and an earlier build's task.
    let files = node::supervisor::Files::beside(&amux);
    assert_eq!(files.launcher, launcher);
    for path in [
        &files.prev,
        &files.rejected,
        &files.staged,
        &files.launcher_staged,
    ] {
        std::fs::write(path, b"left by an update").unwrap();
    }
    for program in [&amux, &launcher] {
        let mut aside = program.clone().into_os_string();
        aside.push(".old.1");
        std::fs::write(aside, b"moved aside by an update").unwrap();
    }
    assert_eq!(files.asides().len(), 2);
    std::fs::write(root.join("amux-task.xml"), b"\xff\xfe").unwrap();

    let output = run(&amux, &["uninstall", "--yes", "--delete-data"])
        .output()
        .unwrap();
    let said = format!(
        "{}{}",
        stdout(&output),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.status.success(), "{said}");
    assert_eq!(said.contains("Removed the login item"), windows, "{said}");
    assert_eq!(
        said.contains("Removed the scheduled task an earlier amux registered."),
        windows,
        "{said}"
    );
    assert_eq!(login_value(login.items()), None, "{said}");
    until("the launcher to exit", || {
        launched.borrow_mut().0.try_wait().unwrap().is_some()
    })
    .await;
    assert!(!alive(supervisor), "{said}");
    assert!(!root.exists() && !config.exists(), "{said}");

    let left: Vec<String> = match std::fs::read_dir(&bin) {
        Ok(entries) => entries
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect(),
        Err(_) => Vec::new(),
    };
    if windows {
        assert!(
            left.len() == 1 && left[0].starts_with("amux.exe.old."),
            "{left:?}\n{said}"
        );
        assert!(
            said.contains(&format!("Left {}", bin.join(&left[0]).display())),
            "{said}"
        );
        assert_eq!(
            environment.path(),
            Some(format!("REG_EXPAND_SZ    {kept}")),
            "{said}"
        );
    } else {
        assert!(
            left.is_empty() && !home.join(".amux").exists(),
            "{left:?}\n{said}"
        );
    }
    assert_eq!(std::fs::read_to_string(&profile).unwrap(), "# mine\n\n");
}

// Windows stops amux through supervisor.sock, which no mask reaches, and
// `stop_goes_to_the_supervisor_first_...` drives it.
#[cfg(unix)] // Unix only by nature: a launcher's blocked signal mask, inherited, and a stop that is a signal
#[tokio::test(flavor = "multi_thread")]
async fn stop_reaches_an_amux_started_with_its_stop_signals_blocked() {
    use std::os::unix::process::CommandExt;

    // The Mac app starts amux from a Grand Central Dispatch worker, which
    // blocks signals, and a spawned process inherits its launcher's mask.
    let install = Install::new(true, "");
    let mut start = install.command(&["server", "start"]);
    // SAFETY: sigemptyset, sigaddset and pthread_sigmask are
    // async-signal-safe and touch only this child's own state.
    unsafe {
        start.pre_exec(|| {
            let mut blocked = std::mem::MaybeUninit::<libc::sigset_t>::uninit();
            libc::sigemptyset(blocked.as_mut_ptr());
            libc::sigaddset(blocked.as_mut_ptr(), libc::SIGTERM);
            libc::sigaddset(blocked.as_mut_ptr(), libc::SIGINT);
            libc::pthread_sigmask(libc::SIG_BLOCK, blocked.as_ptr(), std::ptr::null_mut());
            Ok(())
        });
    }
    assert!(start.output().unwrap().status.success());
    let supervisor = install
        .supervisor_pid()
        .expect("server start ran amux supervise");

    let stopped = install.amux(&["server", "stop"]);
    assert!(
        stdout(&stopped).contains("Stopped amux supervise and its daemon."),
        "{}",
        stdout(&stopped)
    );
    until("the supervisor to exit", || !alive(supervisor)).await;
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

// Built everywhere so the test listing names it wherever the contracts
// are checked.
// macOS only by nature: a power assertion, which only macOS shows (pmset)
#[cfg_attr(
    not(target_os = "macos"),
    ignore = "only macOS holds a sleep assertion"
)]
#[tokio::test(flavor = "multi_thread")]
async fn the_supervisor_holds_the_power_sleep_assertion_under_keep_awake() {
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
    until_within(
        "the supervisor to hold the assertion for power",
        PATIENCE,
        || {
            let held = assertions_of(pid);
            async move {
                held.iter()
                    .any(|line| line.contains("PreventSystemSleep"))
                    .then_some(())
                    .ok_or(format!("{held:?}"))
            }
        },
    )
    .await
    .unwrap();
    // The one battery honours is the daemon's, and only while an agent
    // works.
    assert!(
        !assertions_of(pid)
            .iter()
            .any(|line| line.contains("PreventUserIdleSystemSleep")),
        "{:?}",
        assertions_of(pid)
    );
    // Turned off, it lets go without a restart.
    awake.amux(&["config", "set", "keep_awake", "false"]);
    until_within("the supervisor to let go", PATIENCE, || {
        let held = assertions_of(pid);
        async move { held.is_empty().then_some(()).ok_or(format!("{held:?}")) }
    })
    .await
    .unwrap();
    awake.amux(&["config", "set", "keep_awake", "true"]);
    until_within("the supervisor to hold it again", PATIENCE, || {
        let held = assertions_of(pid);
        async move { (!held.is_empty()).then_some(()).ok_or(format!("{held:?}")) }
    })
    .await
    .unwrap();
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

/// `start_at_login` writes the login item for the next login and removes
/// it, with amux stopped; an install without a supervisor has none.
// Linux only by nature: systemd's user session, which registering the unit
// needs and a test runner has none of
#[cfg_attr(
    target_os = "linux",
    ignore = "registering the systemd unit needs a user session"
)]
#[test]
fn start_at_login_writes_and_removes_the_login_item() {
    let install = Install::new(true, "");
    let on = install.amux(&["config", "set", "start_at_login", "true"]);
    assert!(
        stdout(&on).contains("amux starts at login"),
        "{}",
        stdout(&on)
    );
    let login = install.login_item();
    let item = login.contents().expect("this install's login item");
    assert!(
        item.contains("supervise") && item.contains(&*install.config.to_string_lossy()),
        "{item}"
    );
    assert!(
        install.supervisor_pid().is_none(),
        "the item waits for the next login"
    );
    install.amux(&["config", "set", "start_at_login", "false"]);
    assert!(!login.is_on() && login.contents().is_none());

    let without = Install::new(false, "");
    let output = without.amux(&["config", "set", "start_at_login", "true"]);
    assert!(
        stdout(&output).contains("no login item to add"),
        "{}",
        stdout(&output)
    );
    assert!(!without.root.path().join("home").exists());
}

#[test]
fn config_set_changes_the_channel_and_keeps_the_rest() {
    let install = Install::new(
        true,
        "# the desk in the study\nhost_name = \"desk\"\n\n[ui]\ncolor = \"ansi\" # for tmux\n",
    );
    let output = install.amux(&["config", "set", "channel", "preview"]);
    assert!(stdout(&output).contains("channel = \"preview\""));
    assert_eq!(
        stdout(&install.amux(&["config", "get", "channel"])).trim(),
        "preview"
    );
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

    install.amux(&["config", "set", "channel", "stable"]);
    let config = settings::InstallationConfig::from_file(&install.config).unwrap();
    assert_eq!(config.channel, settings::Channel::Stable);

    let refused = install
        .command(&["config", "set", "channel", "nightly"])
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
    // The supervisor became 2.0.0 too: by an exec in place where the OS
    // has one (Unix), else by a successor it started as it exited.
    if os::process::EXEC_KEEPS_PID {
        assert_eq!(
            install.supervisor_pid(),
            Some(supervisor),
            "the supervisor exec'd in place"
        );
    } else {
        until("the old supervisor to hand over and exit", || {
            !alive(supervisor)
        })
        .await;
        let lock = install.data.join(SUPERVISOR_LOCK);
        until("its successor to take the lock", || {
            std::fs::read_to_string(&lock)
                .ok()
                .and_then(|named| named.trim().parse::<u32>().ok())
                .is_some_and(|successor| successor != supervisor && alive(successor))
                && locked(&lock)
        })
        .await;
    }

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

/// Under a supervisor, a client switching an experiment restarts the daemon
/// the way an update does: the daemon asks, the supervisor stops it and
/// starts the same binary again at once, and stays itself. The new daemon
/// serves the experiments switched on, says so to clients, and the agent in
/// the middle of a turn keeps its process across it.
#[tokio::test(flavor = "multi_thread")]
async fn under_a_supervisor_an_experiment_switch_restarts_the_daemon_through_it() {
    use support::desk::{Desk, LONG_GRACE_SECS};

    let gate_dir = tempfile::tempdir().unwrap();
    let gate = gate_dir.path().join("gate");
    let desk = Desk::new(
        true,
        LONG_GRACE_SECS,
        node::version(),
        "http://127.0.0.1:9",
        support::gated_turn(&gate),
    );
    desk.run(&["server", "start"]).await;
    let supervisor = desk.supervisor_pid().expect("a supervisor runs");
    let first = desk.daemon_pid();
    let work = desk.work.to_string_lossy().into_owned();
    let id = support::created_id(
        &desk
            .run(&[
                "new",
                "claude",
                "--in",
                "amux",
                "--name",
                "sdk",
                "--cwd",
                &work,
                "--prompt",
                "first task",
            ])
            .await,
    );
    let chats = support::client(&desk.socket).await;
    support::until("the agent is mid-turn", || {
        let chats = chats.clone();
        let id = &id;
        async move { support::says(&support::chat(&chats, id).await, "started", 1) }
    })
    .await
    .unwrap();
    let agent = agent_pid(&desk.agent_dir(&id)).expect("the agent runs");

    let door = client::Installation::connect(&desk.socket, client::Surface::Terminal)
        .await
        .unwrap();
    let switched = door
        .update_settings(wire::UpdateSettingsRequest {
            experiments: Some(wire::ExperimentList {
                names: vec!["suspension".into()],
            }),
            ..Default::default()
        })
        .await
        .unwrap();
    let suspension = |settings: &wire::Settings| {
        let found = settings
            .experiments
            .iter()
            .find(|experiment| experiment.name == "suspension")
            .expect("this build has suspension");
        (found.enabled, found.applying)
    };
    assert_eq!(suspension(&switched), (true, true), "being applied");

    until("the supervisor starts a new daemon", || {
        desk.daemon_pid() != first && desk.daemon_running()
    })
    .await;
    assert_eq!(
        desk.supervisor_pid(),
        Some(supervisor),
        "the same supervisor"
    );
    until("the new daemon serves", || {
        std::fs::read_to_string(desk.data.join("daemon.log"))
            .unwrap()
            .lines()
            .filter(|line| line.contains(" serving "))
            .count()
            == 2
    })
    .await;
    let log = std::fs::read_to_string(desk.data.join("daemon.log")).unwrap();
    let serving_now: Vec<&str> = log
        .lines()
        .filter(|line| line.contains(" serving "))
        .collect();
    let restarting = log
        .lines()
        .filter(|line| line.contains("restarting to run the experiments switched on"))
        .count();
    assert_eq!(restarting, 1, "{log}");
    // Through the supervisor's restart, not an exit it backs off from.
    assert!(!log.contains("did not restart the daemon"), "{log}");
    assert!(
        serving_now[1].contains(r#"experiments=["suspension/1"]"#),
        "{}",
        serving_now[1]
    );
    assert_eq!(
        agent_pid(&desk.agent_dir(&id)),
        Some(agent),
        "the agent kept its process"
    );

    let door = client::Installation::connect(&desk.socket, client::Surface::Terminal)
        .await
        .unwrap();
    let settings = door.get_settings().await.unwrap();
    assert_eq!(suspension(&settings), (true, false), "applied");
    std::fs::write(&gate, b"").unwrap();
    let chats = support::client(&desk.socket).await;
    support::until("the turn finishes under the new daemon", || {
        let chats = chats.clone();
        let id = &id;
        async move { support::says(&support::chat(&chats, id).await, "finished", 1) }
    })
    .await
    .unwrap();
    let second = desk.daemon_pid();
    holds_for(
        "no second restart",
        node::RESTART_DEBOUNCE + Duration::from_secs(3),
        || async { desk.daemon_pid() == second },
    )
    .await
    .expect("one restart");
    desk.run(&["settle", "sdk"]).await;
}
