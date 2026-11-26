//! Windows' installer, run as a person runs it, end to end, with the built
//! binaries: `install.ps1` from a release feed on the loopback, `amux
//! server start` and `stop`, the login item turned on and a logon
//! simulated by starting what it names, an agent on the fake providers,
//! two updates from the feed while that agent is mid-turn, and `amux
//! uninstall`. After every step no window has appeared that any process of
//! the test owns: amux, the launcher, the fakes, PowerShell or a console
//! any of them was given.
//!
//! Everything runs in a scratch machine: the homes, the temporary folder
//! and the root are a temporary folder's, and the two registry keys amux
//! and the script write (the `Run` key and the user's `Environment`) are
//! keys of the test's own under `HKCU\Software\amux-test-…`, deleted at the
//! end. Nothing that writes the registry runs unless what it runs reads
//! those keys.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use node::release::{self, Manifest, Release};
use node::supervisor::login::{ScratchItems, TEST_RUN_KEY};
use os::testing::desktop::{self, Window};
use provider_fakes::dispatch::{self, program};
use provider_fakes::{SCRIPT_ENV, Script, Step};
use support::{
    TEST_SEED, agent_pid, amux_binary, binaries, chat, client, created_id, gated_turn, says, until,
};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use crate::support;

/// The target `install.ps1` asks the manifest for: the x64 build, which
/// Windows on Arm runs emulated. The build under test is this machine's
/// own, so the feed names it for both targets.
const INSTALLER_TARGET: &str = "x86_64-pc-windows-msvc";

/// The prefix every key the test lets amux write starts with.
const TEST_KEY_PREFIX: &str = r"Software\amux-test-";

#[tokio::test(flavor = "multi_thread")]
async fn install_start_log_on_update_twice_and_uninstall_showing_no_window() {
    let running = node::version();
    let first = release::version_stamp::previous(running).expect("a release version to go below");
    // semver orders `-previous` before `-previous.2`, and both before the
    // release.
    let second = format!("{first}.2");

    let machine = Machine::new();
    let watch = Watch::start();
    let feed = Feed::start(machine.scratch.path().join("feed")).await;
    feed.publish(&first);
    machine.refuse_unless_the_seams_are_read(&feed.build(&first));

    // install.ps1, as `irm https://amux.sh/install | iex` runs it.
    let installed = machine.install(&feed);
    assert!(installed.status.success(), "{}", said(&installed));
    assert!(
        said(&installed).contains(&format!("amux {first} has been installed")),
        "{}",
        said(&installed)
    );
    assert_eq!(
        machine.listing(),
        ["amux-launch.exe", "amux.exe"],
        "{}",
        said(&installed)
    );
    assert_eq!(
        machine.user_path(),
        Some(format!(
            "REG_EXPAND_SZ    {};{KEPT_PATH}",
            machine.bin.display()
        )),
        "the folder goes first on the user's PATH, the rest as written"
    );
    assert_eq!(machine.version().await, first);
    watch.after("install.ps1");

    machine.configure(&feed);
    let started = machine.run(&["server", "start"]).await;
    assert!(started.contains("Started amux."), "{started}");
    assert!(
        machine.answers().await,
        "the daemon answers once start returns"
    );
    let stopped = machine.run(&["server", "stop"]).await;
    assert!(
        stopped.contains("Stopped amux supervise and its daemon."),
        "{stopped}"
    );
    watch.after("amux server start, then stop");

    // The login item, for the next logon.
    machine.refuse_unless_the_seams_are_read(&machine.amux());
    let on = machine
        .run(&["config", "set", "start_at_login", "true"])
        .await;
    let item = machine
        .login_value()
        .unwrap_or_else(|| panic!("no login item was written: {on}"));
    let launcher = machine.bin.join("amux-launch.exe");
    assert_eq!(item, format!("\"{}\" supervise", launcher.display()));
    watch.after("amux config set start_at_login true");

    // The logon: the command line the item names, started as Explorer
    // starts it, with the session's environment.
    let (program, arguments) = command_line(&item);
    let mut logon = machine.command(Path::new(&program));
    logon
        .args(&arguments)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let logon = Ending(logon.spawn().unwrap());
    let launcher_pid = logon.0.id();
    until("the supervisor the logon started to answer", || async {
        machine
            .answers()
            .await
            .then_some(())
            .ok_or("no daemon answers yet")
    })
    .await
    .unwrap();
    watch.after("the logon");

    // An agent on the fakes, held mid-turn.
    let gate = machine.scratch.path().join("gate");
    let work = machine.work.to_string_lossy().into_owned();
    let created = machine
        .run(&[
            "new",
            "claude",
            "--in",
            "amux",
            "--name",
            "worker",
            "--cwd",
            &work,
            "--prompt",
            "first task",
        ])
        .await;
    let agent = created_id(&created);
    let chats = client(&machine.socket).await;
    until("the agent to answer", || {
        let chats = chats.clone();
        let agent = agent.clone();
        async move { says(&chat(&chats, &agent).await, "started", 1) }
    })
    .await
    .unwrap();
    let agent_dir = machine.agent_dir(&agent);
    let agent_process = agent_pid(&agent_dir).expect("the agent runs");
    watch.after("amux new");

    // Two updates from the feed, with the agent mid-turn throughout.
    for version in [second.as_str(), running] {
        feed.publish(version);
        machine.refuse_unless_the_seams_are_read(&feed.build(version));
        let updated = machine.run(&["update"]).await;
        assert!(
            updated.contains(&format!("amux {version} is running.")),
            "{updated}"
        );
        assert_eq!(machine.version().await, version);
        assert_eq!(
            agent_pid(&agent_dir),
            Some(agent_process),
            "the agent kept its process through the update to {version}"
        );
        assert!(
            alive(launcher_pid),
            "the launcher's line stayed open through the update to {version}"
        );
        watch.after(&format!("amux update to {version}"));
    }
    println!("after both updates: {:?}", machine.listing());

    // The agent carries on: the turn the updates ran through finishes, and
    // it takes another.
    std::fs::write(&gate, b"").unwrap();
    let chats = client(&machine.socket).await;
    until(
        "the agent to finish the turn the updates ran through",
        || {
            let chats = chats.clone();
            let agent = agent.clone();
            async move { says(&chat(&chats, &agent).await, "finished", 1) }
        },
    )
    .await
    .unwrap();
    machine.run(&["send", "worker", "second", "task"]).await;
    until("the agent to answer a turn after the updates", || {
        let chats = chats.clone();
        let agent = agent.clone();
        async move { says(&chat(&chats, &agent).await, "carried on", 1) }
    })
    .await
    .unwrap();
    assert_eq!(agent_pid(&agent_dir), Some(agent_process));
    watch.after("the agent's turns after the updates");

    // Every supervisor and daemon of the three builds started with no
    // console window, and said so.
    for log in ["supervisor.log", "daemon.log"] {
        let text = std::fs::read_to_string(machine.root.join(log)).unwrap();
        let said: Vec<&str> = text
            .lines()
            .filter(|line| line.contains("console window="))
            .collect();
        assert!(
            said.len() >= 3,
            "{log} names a start of each build:\n{text}"
        );
        assert!(
            said.iter().all(|line| line.contains("window=false")),
            "{log}:\n{}",
            said.join("\n")
        );
    }

    // Uninstall, as a person runs it, with the login item's launcher
    // running it and the agent running.
    machine.refuse_unless_the_seams_are_read(&machine.amux());
    let uninstalled = machine
        .amux_output(&["uninstall", "--yes", "--delete-data"])
        .await;
    let text = said(&uninstalled);
    assert!(uninstalled.status.success(), "{text}");
    for expected in ["Stopped worker.", "Removed the login item", "off your PATH"] {
        assert!(text.contains(expected), "{expected}: {text}");
    }
    assert_eq!(machine.login_value(), None, "{text}");
    let mut logon = logon;
    until("the launcher to exit", || {
        let exited = logon.0.try_wait().unwrap().is_some();
        std::future::ready(exited.then_some(()).ok_or("still running"))
    })
    .await
    .unwrap();
    until("the stopped agent to exit", || {
        let gone = !alive(agent_process);
        std::future::ready(gone.then_some(()).ok_or("still running"))
    })
    .await
    .unwrap();
    assert!(!machine.root.exists() && !machine.config.exists(), "{text}");
    assert_eq!(
        machine.user_path(),
        Some(format!("REG_EXPAND_SZ    {KEPT_PATH}")),
        "the folder is off the user's PATH, the rest as written: {text}"
    );
    let left = machine.listing();
    assert!(!left.is_empty(), "the uninstalling amux is left: {text}");
    for name in &left {
        let path = machine.bin.join(name);
        assert!(
            name.contains(".exe.old."),
            "{name} is no program moved aside: {left:?}\n{text}"
        );
        assert!(
            text.contains(&format!(
                "Left {}, which a running process holds",
                path.display()
            )),
            "{name} was left unsaid: {text}"
        );
    }
    watch.after("amux uninstall");
    // What was left was held only while what ran it ran.
    until("what uninstall left to be deletable", || {
        let gone = left.iter().all(|name| {
            matches!(
                os::fs::remove_patiently(&machine.bin.join(name), Duration::ZERO),
                Ok(os::fs::Removed::Gone)
            )
        });
        std::future::ready(gone.then_some(()).ok_or("still held"))
    })
    .await
    .unwrap();
    watch.finish();
}

/// What the user's PATH holds before the install: entries as a person's
/// own PATH has them, `%VARIABLES%` unexpanded, which the run must leave as
/// written though it runs with a scratch USERPROFILE.
const KEPT_PATH: &str =
    r"%USERPROFILE%\AppData\Local\Microsoft\WindowsApps;%SystemRoot%\keep;C:\elsewhere";

/// A scratch machine: a person's homes and temporary folder, where the
/// installer puts amux, the default installation's config and root, the
/// fake providers on PATH, and keys of the test's own for the `Run` key and
/// the user's `Environment`.
struct Machine {
    scratch: tempfile::TempDir,
    home: PathBuf,
    appdata: PathBuf,
    local: PathBuf,
    temp: PathBuf,
    /// Where install.ps1 installs: `~\.amux\bin`.
    bin: PathBuf,
    /// The fakes, as `claude.exe` and `codex.exe`.
    providers: PathBuf,
    work: PathBuf,
    /// The default installation's config and root, under the scratch
    /// homes.
    config: PathBuf,
    root: PathBuf,
    socket: PathBuf,
    run_key: ScratchItems,
    environment_key: ScratchItems,
}

impl Machine {
    fn new() -> Self {
        let scratch = tempfile::tempdir().unwrap();
        let home = scratch.path().join("home");
        let appdata = home.join("AppData").join("Roaming");
        let local = home.join("AppData").join("Local");
        let temp = local.join("Temp");
        let providers = scratch.path().join("providers");
        let work = scratch.path().join("work");
        for dir in [&appdata, &local, &temp, &providers, &work] {
            std::fs::create_dir_all(dir).unwrap();
        }
        // One `claude` for both Claude kinds, as on a real machine.
        for (name, fake) in [("claude", "fake-claude-sdk"), ("codex", "fake-codex")] {
            dispatch::link(&program(binaries(), fake), &program(&providers, name)).unwrap();
        }
        let steps: Vec<Step> = [
            gated_turn(&scratch.path().join("gate")),
            vec![
                Step::Text {
                    chunks: vec!["carried on".to_owned()],
                },
                Step::TurnEnd,
            ],
        ]
        .concat();
        std::fs::write(
            scratch.path().join("script.json"),
            serde_json::to_string(&Script {
                steps,
                ..Script::default()
            })
            .unwrap(),
        )
        .unwrap();
        let root = local.join("amux");
        let machine = Self {
            bin: home.join(".amux").join("bin"),
            config: appdata.join("amux").join("config.toml"),
            socket: root.join("amux.sock"),
            root,
            run_key: ScratchItems::new(&home),
            environment_key: ScratchItems::new(&home),
            scratch,
            home,
            appdata,
            local,
            temp,
            providers,
            work,
        };
        for key in [machine.run_key.items(), machine.environment_key.items()] {
            assert!(
                key.to_string_lossy().starts_with(TEST_KEY_PREFIX),
                "{} is no key of the test's own",
                key.display()
            );
        }
        machine.reg(
            "add",
            &["/v", "Path", "/t", "REG_EXPAND_SZ", "/d", KEPT_PATH, "/f"],
        );
        machine
    }

    /// Refuses to go on unless `image`, an amux about to run here, reads
    /// both keys the test points it at, and install.ps1 the one it writes:
    /// one that didn't would write the person's own login item and PATH.
    fn refuse_unless_the_seams_are_read(&self, image: &Path) {
        let bytes = std::fs::read(image).unwrap();
        for seam in [os::paths::TEST_ENVIRONMENT_KEY, TEST_RUN_KEY] {
            assert!(
                bytes
                    .windows(seam.len())
                    .any(|window| window == seam.as_bytes()),
                "{} doesn't read {seam}: not run",
                image.display()
            );
        }
        let script = std::fs::read_to_string(install_script()).unwrap();
        assert!(
            script.contains("$env:AMUX_TEST_ENVIRONMENT_KEY")
                && script.contains(r#".StartsWith("Software\amux-test-")"#),
            "install.ps1 doesn't keep the user PATH under the test's key: not run"
        );
    }

    /// `program` in this machine's environment, as everything the test
    /// runs is.
    fn command(&self, program: &Path) -> std::process::Command {
        let path = std::env::var_os("PATH").unwrap_or_default();
        let path = std::env::join_paths(
            std::iter::once(self.providers.clone()).chain(std::env::split_paths(&path)),
        )
        .unwrap();
        let mut command = std::process::Command::new(program);
        command
            .env_remove("AMUX_CONFIG")
            .env_remove("AMUX_LOG")
            .env_remove("AMUX_NO_MODIFY_PATH")
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("XDG_DATA_HOME")
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("APPDATA", &self.appdata)
            .env("LOCALAPPDATA", &self.local)
            .env("TEMP", &self.temp)
            .env("TMP", &self.temp)
            .env("PATH", path)
            .env(SCRIPT_ENV, self.scratch.path().join("script.json"))
            .env("CLAUDE_CONFIG_DIR", self.scratch.path().join("claude"))
            .env(support::NO_DISCOVERY.0, support::NO_DISCOVERY.1)
            .env(TEST_RUN_KEY, self.run_key.items())
            .env(
                os::paths::TEST_ENVIRONMENT_KEY,
                self.environment_key.items(),
            )
            .stdin(Stdio::null());
        command
    }

    /// reg.exe `verb` on the test's environment key.
    fn reg(&self, verb: &str, arguments: &[&str]) -> Output {
        let output = self
            .command(Path::new("reg"))
            .arg(verb)
            .arg(format!(r"HKCU\{}", self.environment_key.items().display()))
            .args(arguments)
            .output()
            .unwrap();
        assert!(output.status.success(), "reg {verb}: {}", said(&output));
        output
    }

    /// The user's PATH as stored, its kind and value, `%VARIABLES%` and
    /// all.
    fn user_path(&self) -> Option<String> {
        let output = self.reg("query", &["/v", "Path"]);
        let text = String::from_utf8_lossy(&output.stdout).into_owned();
        let line = text
            .lines()
            .map(str::trim)
            .find(|line| line.starts_with("Path "))?;
        Some(line.split_once("    ")?.1.trim().to_owned())
    }

    /// The default installation's login item, the value `amux` under the
    /// test's `Run` key, as reg.exe reads it.
    fn login_value(&self) -> Option<String> {
        let output = self
            .command(Path::new("reg"))
            .args([
                "query",
                &format!(r"HKCU\{}", self.run_key.items().display()),
            ])
            .args(["/v", "amux"])
            .output()
            .unwrap();
        if !output.status.success() {
            return None;
        }
        let text = String::from_utf8_lossy(&output.stdout).into_owned();
        let line = text
            .lines()
            .map(str::trim)
            .find(|line| line.starts_with("amux "))?;
        Some(line.split_once("REG_SZ")?.1.trim().to_owned())
    }

    /// install.ps1 from `feed`, in Windows PowerShell, as a person's
    /// `irm | iex` runs it.
    fn install(&self, feed: &Feed) -> Output {
        self.command(Path::new("powershell"))
            .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
            .arg(install_script())
            .env("AMUX_RELEASES_URL", &feed.url)
            .output()
            .unwrap()
    }

    /// The installation config a person who follows the feed would write:
    /// updates when asked, the test's own feed, no agent the machine has.
    fn configure(&self, feed: &Feed) {
        std::fs::create_dir_all(self.config.parent().unwrap()).unwrap();
        std::fs::write(
            &self.config,
            format!(
                "supervisor = true\nupdates = \"manual\"\nkeep_awake = false\n\
                 releases_url = {:?}\n[agent]\ngrace_secs = 30\ndrain_secs = 5\n{}",
                feed.url,
                support::NO_REAL_AGENTS,
            ),
        )
        .unwrap();
    }

    fn amux(&self) -> PathBuf {
        self.bin.join("amux.exe")
    }

    async fn amux_output(&self, args: &[&str]) -> Output {
        let mut command = tokio::process::Command::from(self.command(&self.amux()));
        command.args(args).kill_on_drop(true);
        tokio::time::timeout(Duration::from_secs(240), command.output())
            .await
            .unwrap_or_else(|_| panic!("amux {args:?} did not finish"))
            .expect("amux runs")
    }

    /// The installed amux's `args`, which must succeed; what it said.
    async fn run(&self, args: &[&str]) -> String {
        let output = self.amux_output(args).await;
        let text = said(&output);
        println!("$ amux {}\n{text}", args.join(" "));
        if !output.status.success() {
            let mut logs = String::new();
            for log in ["supervisor.log", "daemon.log"] {
                if let Ok(log_text) = std::fs::read_to_string(self.root.join(log)) {
                    let tail: Vec<&str> = log_text.lines().rev().take(40).collect();
                    logs.push_str(&format!("\n--- {log} (tail) ---\n"));
                    for line in tail.into_iter().rev() {
                        logs.push_str(line);
                        logs.push('\n');
                    }
                }
            }
            panic!("amux {args:?} failed: {text}{logs}");
        }
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    async fn version(&self) -> String {
        let said = self.run(&["--version"]).await;
        said.trim()
            .strip_prefix("amux ")
            .unwrap_or_else(|| panic!("{said}"))
            .to_owned()
    }

    async fn answers(&self) -> bool {
        support::grpc_channel(&self.socket).await.is_ok()
    }

    fn agent_dir(&self, id: &[u8]) -> PathBuf {
        let profile = std::fs::read_dir(self.root.join("profiles"))
            .unwrap()
            .next()
            .expect("a profile directory")
            .unwrap()
            .path();
        profile
            .join("agents")
            .join(uuid::Uuid::from_slice(id).unwrap().to_string())
    }

    /// The names in the install folder, sorted; none once it has gone.
    fn listing(&self) -> Vec<String> {
        let mut names: Vec<String> = match std::fs::read_dir(&self.bin) {
            Ok(entries) => entries
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .collect(),
            Err(_) => Vec::new(),
        };
        names.sort();
        names
    }
}

impl Drop for Machine {
    /// Stops whatever a failed run left: amux, then its agents, which
    /// outlive their daemon on purpose. The keys go with their
    /// `ScratchItems`, after this.
    fn drop(&mut self) {
        if self.amux().exists() {
            let _ = self
                .command(&self.amux())
                .args(["server", "stop"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        support::kill_agents_under(&self.root);
    }
}

fn install_script() -> PathBuf {
    source_tree::crate_dir!().join("../../scripts/install/install.ps1")
}

/// A `Run` value's command line as Windows splits it: the program, quoted
/// when it has to be, then its arguments.
fn command_line(line: &str) -> (String, Vec<String>) {
    let (program, rest) = match line.strip_prefix('"') {
        Some(quoted) => quoted.split_once('"').expect("a closing quote"),
        None => line.split_once(' ').unwrap_or((line, "")),
    };
    let arguments = rest.split_whitespace().map(str::to_owned).collect();
    (program.to_owned(), arguments)
}

fn said(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn alive(pid: u32) -> bool {
    os::process::ProcessRef::open(pid).is_ok_and(|process| process.alive())
}

/// Ends a process when dropped, so a failed run leaves no launcher
/// starting supervisors.
struct Ending(std::process::Child);

impl Drop for Ending {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The release feed, on the loopback: `/stable.json`, the stable channel's
/// manifest signed with the test key debug builds trust from a feed there,
/// and each release's two programs under `/builds/`, streamed from disk. A
/// release is a copy of the build under test stamped with its version,
/// and the launcher under test beside it.
struct Feed {
    url: String,
    dir: PathBuf,
    manifest: Arc<Mutex<String>>,
}

impl Feed {
    async fn start(dir: PathBuf) -> Self {
        std::fs::create_dir_all(&dir).unwrap();
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let manifest = Arc::new(Mutex::new(r#"{"targets":{}}"#.to_owned()));
        let serving = (manifest.clone(), dir.clone());
        tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                tokio::spawn(answer(socket, serving.0.clone(), serving.1.clone()));
            }
        });
        Self { url, dir, manifest }
    }

    /// The release `version`'s amux.
    fn build(&self, version: &str) -> PathBuf {
        self.dir.join(format!("amux-{version}.exe"))
    }

    /// Names `version` on the stable channel: amux and its launcher, for
    /// this machine's target and the one the installer asks for.
    fn publish(&self, version: &str) {
        let amux = self.build(version);
        release::restamp(amux_binary(), &amux, version).unwrap();
        let launcher = self.dir.join(format!("amux-launch-{version}.exe"));
        std::fs::copy(provider_fakes::binaries::path("amux-launch"), &launcher).unwrap();
        let entry = |file: &Path| {
            let bytes = std::fs::read(file).unwrap();
            Release {
                version: version.to_owned(),
                url: format!(
                    "{}/builds/{}",
                    self.url,
                    file.file_name().unwrap().to_string_lossy()
                ),
                sha256: release::sha256_of(&bytes),
                size: bytes.len() as u64,
            }
        };
        let (amux, launcher) = (entry(&amux), entry(&launcher));
        let mut targets = std::collections::BTreeMap::new();
        for target in [release::TARGET, INSTALLER_TARGET] {
            targets.insert(target.to_owned(), amux.clone());
            targets.insert(
                release::beside_key(target, release::LAUNCHER),
                launcher.clone(),
            );
        }
        let mut manifest = Manifest {
            channel: "stable".into(),
            rollout: None,
            targets,
            signature: String::new(),
        };
        manifest.signature = release::sign(&TEST_SEED, &manifest);
        *self.manifest.lock().unwrap() = serde_json::to_string(&manifest).unwrap();
    }
}

/// One request: the manifest, a build, or not found.
async fn answer(mut socket: tokio::net::TcpStream, manifest: Arc<Mutex<String>>, dir: PathBuf) {
    let mut request = Vec::new();
    let mut buffer = [0; 1024];
    while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
        match socket.read(&mut buffer).await {
            Ok(0) | Err(_) => return,
            Ok(count) => request.extend_from_slice(&buffer[..count]),
        }
    }
    let request = String::from_utf8_lossy(&request);
    let path = request.split_whitespace().nth(1).unwrap_or("").to_owned();
    if let Some(name) = path.strip_prefix("/builds/")
        && !name.contains(['/', '\\'])
        && let Ok(mut file) = tokio::fs::File::open(dir.join(name)).await
    {
        let size = file.metadata().await.map(|meta| meta.len()).unwrap_or(0);
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {size}\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n\r\n"
        );
        if socket.write_all(head.as_bytes()).await.is_ok() {
            let _ = tokio::io::copy(&mut file, &mut socket).await;
        }
        return;
    }
    let (status, body) = match path.as_str() {
        "/stable.json" => ("200 OK", manifest.lock().unwrap().clone()),
        _ => ("404 Not Found", String::new()),
    };
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = socket.write_all(head.as_bytes()).await;
    let _ = socket.write_all(body.as_bytes()).await;
}

/// Watches the desktop the whole run, every 50 ms, for a window that shows
/// and wasn't there when the run began, and is the test's: owned by a
/// process in the test's job (what Buck's runner started it in, which
/// nothing the test starts can leave: amux, the launcher, the fakes,
/// PowerShell, reg.exe, and the console host of any console one of them
/// was given), or by Windows Terminal, which hosts a new console in a
/// window of its own outside any job. A window that comes and goes between
/// two steps is caught too. It starts by seeing a window the test shows
/// itself, so a desktop where it could see nothing fails the run.
struct Watch {
    seen: Arc<Mutex<Vec<(String, Window)>>>,
    samples: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

/// The console hosts that show a console in a window no job holds.
const TERMINALS: &[&str] = &["windowsterminal.exe", "openconsole.exe"];

impl Watch {
    fn start() -> Self {
        let baseline: BTreeSet<usize> = desktop::visible_windows()
            .expect("the desktop's windows can be listed")
            .iter()
            .map(|window| window.id)
            .collect();
        let members = desktop::job_members();
        println!(
            "the desktop shows {} windows; this test is in a job of {}; it has a console \
             window: {}",
            baseline.len(),
            match &members {
                Ok(members) => format!("{} processes", members.len()),
                Err(error) => format!("none ({error}): every new window counts"),
            },
            os::process::console_window(),
        );
        let seen: Arc<Mutex<Vec<(String, Window)>>> = Arc::default();
        let samples = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = std::thread::spawn({
            let (seen, samples, stop) = (seen.clone(), samples.clone(), stop.clone());
            move || {
                let mut reported = baseline;
                while !stop.load(Ordering::Relaxed) {
                    let windows = desktop::visible_windows().unwrap_or_default();
                    let members = desktop::job_members().ok();
                    for window in windows {
                        if reported.contains(&window.id) {
                            continue;
                        }
                        let ours = members
                            .as_ref()
                            .is_none_or(|members| members.contains(&window.pid))
                            || window
                                .program
                                .as_deref()
                                .and_then(Path::file_name)
                                .is_some_and(|name| {
                                    TERMINALS
                                        .iter()
                                        .any(|terminal| name.eq_ignore_ascii_case(terminal))
                                });
                        if ours {
                            reported.insert(window.id);
                            seen.lock()
                                .unwrap()
                                .push((format!("{:?}", std::time::SystemTime::now()), window));
                        }
                    }
                    samples.fetch_add(1, Ordering::Relaxed);
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        });
        let watch = Self {
            seen,
            samples,
            stop,
            thread: Some(thread),
        };
        watch.sees_a_window_the_test_shows();
        watch
    }

    /// The watch sees a window of the test's own, the one way to know it
    /// would see another on this desktop, and then forgets it.
    fn sees_a_window_the_test_shows(&self) {
        let title = format!("windows-install-canary-{}", std::process::id());
        let caught = || {
            self.seen
                .lock()
                .unwrap()
                .iter()
                .any(|(_, window)| window.title == title)
        };
        let mut seen = false;
        desktop::show_a_window(&title, || {
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while !caught() && std::time::Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            seen = caught();
        })
        .expect("the test shows a window of its own");
        assert!(
            seen,
            "the watch never saw the window the test showed ({} looks)",
            self.samples.load(Ordering::Relaxed)
        );
        self.seen
            .lock()
            .unwrap()
            .retain(|(_, window)| window.title != title);
    }

    /// No window of the test's has shown since the run began, up to the end
    /// of `step`.
    fn after(&self, step: &str) {
        let seen = self.seen.lock().unwrap().clone();
        assert!(
            seen.is_empty(),
            "a window showed by the end of {step}: {seen:#?}"
        );
        println!(
            "-- {step}: no window shown ({} looks at the desktop so far)",
            self.samples.load(Ordering::Relaxed)
        );
    }

    fn finish(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
        self.after("the run");
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}
