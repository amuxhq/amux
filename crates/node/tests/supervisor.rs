//! `amux supervise`, driven as processes: the real supervisor inside a
//! stand-in amux binary (`fake-amux`), whose daemon mode is scripted per
//! version, against a fake manifest server. Versions are re-stamped copies
//! of one build, and releases are signed with the test key debug builds
//! trust.

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::Duration;

use ::release::TEST_SEED;
use node::release::{self, Choice, Manifest, Release, Skip, VerifyError};
use node::supervisor::{Files, Request, SUPERVISOR_LOCK};
use semver::Version;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use uuid::Uuid;
const OTHER_SEED: [u8; 32] = [7; 32];
use patience::{PATIENCE, until};

/// Tests in this binary write executables and start processes on parallel
/// threads. A process forked while another thread holds a freshly written
/// executable open for writing inherits that descriptor until it execs, and
/// on Linux an exec of the file meanwhile fails with "Text file busy". So
/// writing an executable excludes every start in this process, and a start
/// holds the lock until the child has exec'd (`spawn` returns only then).
static EXECUTABLES: RwLock<()> = RwLock::new(());

/// Writes an executable with no process start in flight. `write` must not
/// start a process itself.
fn write_executable<T>(write: impl FnOnce() -> T) -> T {
    let _writing = EXECUTABLES
        .write()
        .unwrap_or_else(|poison| poison.into_inner());
    write()
}

/// Starts a process with no executable being written.
fn start<T>(start: impl FnOnce() -> T) -> T {
    let _starting = EXECUTABLES
        .read()
        .unwrap_or_else(|poison| poison.into_inner());
    start()
}

/// fake-amux and amux.
fn binaries() -> &'static Path {
    provider_fakes::binaries::dir()
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Event {
    what: String,
    version: String,
    pid: u32,
}

/// A fake manifest server: `/stable.json` and `/artifacts/<name>`.
#[derive(Clone, Default)]
struct Server {
    manifest: Arc<Mutex<String>>,
    artifacts: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    manifest_reads: Arc<Mutex<u32>>,
    base: Arc<OnceLock<String>>,
}

impl Server {
    async fn start() -> Self {
        let server = Self::default();
        *server.manifest.lock().unwrap() = r#"{"targets":{}}"#.to_owned();
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        server
            .base
            .set(format!("http://{}", listener.local_addr().unwrap()))
            .unwrap();
        let serving = server.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let serving = serving.clone();
                tokio::spawn(async move {
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
                    let body = if path == "/stable.json" {
                        *serving.manifest_reads.lock().unwrap() += 1;
                        Some(serving.manifest.lock().unwrap().clone().into_bytes())
                    } else {
                        path.strip_prefix("/artifacts/")
                            .and_then(|name| serving.artifacts.lock().unwrap().get(name).cloned())
                    };
                    let (status, body) = match body {
                        Some(body) => ("200 OK", body),
                        None => ("404 Not Found", Vec::new()),
                    };
                    let head = format!(
                        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = socket.write_all(head.as_bytes()).await;
                    let _ = socket.write_all(&body).await;
                });
            }
        });
        server
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base.get().unwrap())
    }

    fn reads(&self) -> u32 {
        *self.manifest_reads.lock().unwrap()
    }

    /// Serves `bytes` as `version`, signed with `seed`, to `rollout`.
    fn publish(&self, version: &str, bytes: Vec<u8>, seed: &[u8; 32], rollout: Option<u8>) {
        let name = format!("amux-{version}");
        let release = Release {
            version: version.to_owned(),
            url: self.url(&format!("/artifacts/{name}")),
            sha256: release::sha256_of(&bytes),
            size: bytes.len() as u64,
        };
        self.artifacts.lock().unwrap().insert(name, bytes);
        let mut manifest = Manifest {
            channel: "stable".into(),
            rollout,
            targets: [(release::TARGET.to_owned(), release)].into(),
            signature: String::new(),
        };
        manifest.signature = release::sign(seed, &manifest);
        *self.manifest.lock().unwrap() = serde_json::to_string(&manifest).unwrap();
    }

    /// Serves `bytes` as `version`, with `launcher` named beside it as the
    /// release's launcher, in one manifest signed with the test key.
    fn publish_with_launcher(&self, version: &str, bytes: Vec<u8>, launcher: Vec<u8>) {
        let entry = |name: String, bytes: Vec<u8>| {
            let release = Release {
                version: version.to_owned(),
                url: self.url(&format!("/artifacts/{name}")),
                sha256: release::sha256_of(&bytes),
                size: bytes.len() as u64,
            };
            self.artifacts.lock().unwrap().insert(name, bytes);
            release
        };
        let mut manifest = Manifest {
            channel: "stable".into(),
            rollout: None,
            targets: [
                (
                    release::TARGET.to_owned(),
                    entry(format!("amux-{version}"), bytes),
                ),
                (
                    release::beside_key(release::TARGET, release::LAUNCHER),
                    entry(format!("amux-launch-{version}"), launcher),
                ),
            ]
            .into(),
            signature: String::new(),
        };
        manifest.signature = release::sign(&TEST_SEED, &manifest);
        *self.manifest.lock().unwrap() = serde_json::to_string(&manifest).unwrap();
    }
}

/// `name` as an executable is named here: `amux.exe` on Windows.
fn exe(name: &str) -> String {
    os::paths::exe_name(name)
}

/// An install with fake-amux 1.0.0 at `<dir>/bin/amux`.
struct Fixture {
    _root: tempfile::TempDir,
    dir: PathBuf,
    bin: PathBuf,
    server: Server,
    supervisor: Option<Child>,
    /// `amux-launch` beside the binary, when a test starts the supervisor
    /// through it, as Windows' login item does.
    launcher: Option<Child>,
    /// Processes started from the binary at the path, standing in for the
    /// agents and tool servers of the build installed then.
    running: Vec<Child>,
    /// The supervisor core's step budget, when a test sets one: the
    /// stand-in otherwise gives it a second.
    step_budget_micros: Option<u64>,
}

impl Fixture {
    async fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let dir = std::fs::canonicalize(root.path()).unwrap();
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::create_dir_all(dir.join("builds")).unwrap();
        let fixture = Self {
            bin: dir.join("bin").join(exe("amux")),
            dir,
            _root: root,
            server: Server::start().await,
            supervisor: None,
            launcher: None,
            running: Vec::new(),
            step_budget_micros: None,
        };
        let build = fixture.build("1.0.0");
        write_executable(|| std::fs::copy(build, &fixture.bin)).unwrap();
        fixture
    }

    /// fake-amux stamped as `version`.
    fn build(&self, version: &str) -> PathBuf {
        let path = self.dir.join("builds").join(format!("amux-{version}"));
        if !path.exists() {
            let fake = binaries().join(exe("fake-amux"));
            write_executable(|| release::restamp(&fake, &path, version)).unwrap();
        }
        path
    }

    fn bytes(&self, version: &str) -> Vec<u8> {
        std::fs::read(self.build(version)).unwrap()
    }

    fn publish(&self, version: &str) {
        self.server
            .publish(version, self.bytes(version), &TEST_SEED, None);
    }

    fn behave(&self, version: &str, behaviour: &str) {
        std::fs::write(self.dir.join(format!("behave-{version}")), behaviour).unwrap();
    }

    /// The files the supervisor keeps beside the binary.
    fn beside(&self) -> Files {
        Files::beside(&self.bin)
    }

    fn prev(&self) -> PathBuf {
        self.beside().prev
    }

    fn rejected(&self) -> Option<String> {
        std::fs::read_to_string(self.beside().rejected).ok()
    }

    /// The version stamped into the binary at the path.
    fn installed(&self) -> &'static str {
        let bytes = std::fs::read(&self.bin).unwrap();
        for version in ["1.0.0", "2.0.0", "2.1.0"] {
            if bytes == self.bytes(version) {
                return Box::leak(version.to_owned().into_boxed_str());
            }
        }
        panic!("the binary at the path is none of the builds");
    }

    /// `program supervise` with the stand-in's environment, its standard
    /// error on the supervisor's log.
    fn supervise(&self, program: &Path) -> Command {
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join("supervisor.log"))
            .unwrap();
        let mut command = Command::new(program);
        command
            .arg("supervise")
            .env("FAKE_AMUX_DIR", &self.dir)
            .env("FAKE_AMUX_MANIFEST", self.server.url("/stable.json"))
            .env("RUST_LOG", "info")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log);
        if let Some(micros) = self.step_budget_micros {
            command.env("FAKE_AMUX_STEP_BUDGET_MICROS", micros.to_string());
        }
        command
    }

    /// Starts a process from the binary at the path, as the daemon starts
    /// an agent and an agent a tool server: it runs that build's image
    /// until it is killed, and on Windows keeps the file mapped. It is the
    /// stand-in's daemon with no supervisor, so it never exits on its own,
    /// with events of its own away from the supervisor's.
    fn run_installed(&mut self) -> u32 {
        let dir = self.dir.join("installed-runs");
        std::fs::create_dir_all(&dir).unwrap();
        let mut command = Command::new(&self.bin);
        command
            .arg("daemon")
            .env("FAKE_AMUX_DIR", &dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let child = start(|| command.spawn()).unwrap();
        let pid = child.id();
        self.running.push(child);
        pid
    }

    /// The binaries moved aside beside the binary: `amux.old.<n>`, on
    /// Windows while something still runs them.
    fn asides(&self) -> Vec<String> {
        let stem = format!("{}.old.", exe("amux"));
        let mut names: Vec<String> = std::fs::read_dir(self.dir.join("bin"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(&stem))
            .collect();
        names.sort();
        names
    }

    /// Where `start_launched` puts the launcher: beside the binary.
    fn launcher_path(&self) -> PathBuf {
        self.dir.join("bin").join(exe("amux-launch"))
    }

    fn start(&mut self) -> u32 {
        let mut command = self.supervise(&self.bin);
        let child = start(|| command.spawn()).unwrap();
        let pid = child.id();
        self.supervisor = Some(child);
        pid
    }

    /// Starts the supervisor as Windows' login item does: `amux-launch
    /// supervise`, with the launcher copied beside the binary it runs.
    fn start_launched(&mut self) {
        let launcher = self.launcher_path();
        let built = binaries().join(exe("amux-launch"));
        write_executable(|| std::fs::copy(built, &launcher)).unwrap();
        let mut command = self.supervise(&launcher);
        self.launcher = Some(start(|| command.spawn()).unwrap());
    }

    /// Waits for the launcher to exit, and says how.
    async fn launcher_exit(&mut self) -> std::process::ExitStatus {
        let launcher = self.launcher.as_mut().unwrap();
        until("the launcher to exit", || {
            std::future::ready(launcher.try_wait().unwrap().ok_or("still running"))
        })
        .await
        .unwrap()
    }

    fn launcher_runs(&mut self) -> bool {
        self.launcher
            .as_mut()
            .unwrap()
            .try_wait()
            .unwrap()
            .is_none()
    }

    /// The supervisor `handed` names took over from the one started as
    /// `supervisor`: the same process, exec'd in place, where the OS execs
    /// (Unix), or else the successor it started as it exited (Windows);
    /// either way the one whose pid the lock names now.
    async fn took_over(&self, supervisor: u32, handed: &Event) {
        if os::process::EXEC_KEEPS_PID {
            assert_eq!(
                handed.pid, supervisor,
                "the exec keeps the supervisor's pid"
            );
        } else {
            assert_ne!(handed.pid, supervisor, "a successor of its own");
            until_dead(supervisor).await;
        }
        let lock = self.dir.join("data").join(SUPERVISOR_LOCK);
        let named = handed.pid.to_string();
        until("the lock to name the supervisor that took over", || {
            let held = std::fs::read_to_string(&lock).unwrap_or_default();
            std::future::ready(if held == named { Ok(()) } else { Err(held) })
        })
        .await
        .unwrap();
        assert!(locked(&lock), "the lock is held");
    }

    fn events(&self) -> Vec<Event> {
        std::fs::read_to_string(self.dir.join("events"))
            .unwrap_or_default()
            .split_inclusive('\n')
            .filter_map(|line| line.strip_suffix('\n'))
            .map(|line| {
                let mut parts = line.split(' ');
                Event {
                    what: parts.next().unwrap().to_owned(),
                    version: parts.next().unwrap().to_owned(),
                    pid: parts.next().unwrap().parse().unwrap(),
                }
            })
            .collect()
    }

    fn count(&self, what: &str, version: &str) -> usize {
        self.events()
            .iter()
            .filter(|event| event.what == what && event.version == version)
            .count()
    }

    /// Waits until `done` holds of the events so far, and returns them. A
    /// timeout shows the events and the supervisor's log.
    async fn until(&self, what: &str, done: impl Fn(&[Event]) -> bool) -> Vec<Event> {
        until(what, || {
            let events = self.events();
            std::future::ready(if done(&events) {
                Ok(events)
            } else {
                Err(format!(
                    "events:\n{}\nsupervisor log:\n{}",
                    events
                        .iter()
                        .map(|event| format!("{} {} {}", event.what, event.version, event.pid))
                        .collect::<Vec<_>>()
                        .join("\n"),
                    std::fs::read_to_string(self.dir.join("supervisor.log")).unwrap_or_default()
                ))
            })
        })
        .await
        .unwrap()
    }

    /// Waits for the `n`-th event `what` of `version`, and returns it.
    async fn nth(&self, n: usize, what: &str, version: &str) -> Event {
        let events = self
            .until(&format!("{what} {version} #{n}"), |events| {
                events
                    .iter()
                    .filter(|event| event.what == what && event.version == version)
                    .count()
                    >= n
            })
            .await;
        events
            .into_iter()
            .filter(|event| event.what == what && event.version == version)
            .nth(n - 1)
            .unwrap()
    }

    /// What the processes did and what the supervisor logged, with pids
    /// named by role, for a reader.
    fn transcript(&self, title: &str) {
        // Every supervisor of the line: the one started, and on Windows
        // each successor.
        let supervisors: Vec<u32> = self
            .events()
            .iter()
            .filter(|event| event.what.starts_with("supervise"))
            .map(|event| event.pid)
            .collect();
        let mut daemons: Vec<u32> = Vec::new();
        println!("== {title}");
        for event in self.events() {
            let who = if supervisors.contains(&event.pid) {
                "supervisor".to_owned()
            } else {
                if !daemons.contains(&event.pid) {
                    daemons.push(event.pid);
                }
                let n = daemons.iter().position(|pid| *pid == event.pid).unwrap() + 1;
                format!("daemon#{n}")
            };
            println!("  {who:<11} {:<8} {}", event.version, event.what);
        }
        println!("  -- supervisor log");
        let log = std::fs::read_to_string(self.dir.join("supervisor.log")).unwrap_or_default();
        for line in log.lines() {
            // Drop the timestamp; keep level and message.
            let line = line
                .split_once(' ')
                .map_or(line, |(_, rest)| rest.trim_start());
            println!("  {line}");
        }
        println!("  -- files beside the binary");
        let mut names: Vec<String> = std::fs::read_dir(self.dir.join("bin"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        let file_name = |path: &Path| path.file_name().unwrap().to_string_lossy().into_owned();
        let (binary, rejected) = (file_name(&self.bin), file_name(&self.beside().rejected));
        for name in names {
            let detail = if name == binary {
                format!("version {}", self.installed())
            } else if name == rejected {
                format!("names {}", self.rejected().unwrap_or_default())
            } else {
                String::new()
            };
            println!("  {name} {detail}");
        }
    }

    /// Waits for the manifest to be read `more` more times: ticks passing.
    async fn ticks(&self, more: u32) {
        let target = self.server.reads() + more;
        until("the supervisor to keep checking", || {
            let reads = self.server.reads();
            std::future::ready(
                (reads >= target)
                    .then_some(())
                    .ok_or_else(|| format!("{reads} manifest reads of {target}")),
            )
        })
        .await
        .unwrap();
    }

    /// Asks the supervisor to stop over its control socket, as `amux
    /// server stop` does on Windows (on Unix the CLI sends SIGTERM, which
    /// `amux`'s own supervise_cli tests drive).
    async fn stop_supervisor(&self) {
        let answer = node::supervisor::ask(&self.dir.join("data"), Request::Stop, PATIENCE)
            .await
            .expect("the supervisor answers on its control socket");
        println!("asked the supervisor to stop: {answer}");
    }

    /// Waits for the `n`-th time a daemon of `version` was asked to stop:
    /// SIGTERM on Unix (`term`), the pipe closing on Windows (`eof`), which
    /// has no signals.
    async fn nth_stop(&self, n: usize, version: &str) -> Event {
        let asked = |event: &Event| {
            (event.what == "term" || event.what == "eof") && event.version == version
        };
        let events = self
            .until(&format!("a stop of {version} #{n}"), |events| {
                events.iter().filter(|event| asked(event)).count() >= n
            })
            .await;
        events.into_iter().filter(asked).nth(n - 1).unwrap()
    }

    /// Waits for the supervisor process to exit.
    async fn supervisor_exit(&mut self) -> std::process::ExitStatus {
        let child = self.supervisor.as_mut().unwrap();
        until("the supervisor to exit", || {
            std::future::ready(child.try_wait().unwrap().ok_or("still running"))
        })
        .await
        .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // First, or it would start another supervisor.
        if let Some(mut launcher) = self.launcher.take() {
            let _ = launcher.kill();
            let _ = launcher.wait();
        }
        if let Some(mut child) = self.supervisor.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        for mut child in self.running.drain(..) {
            let _ = child.kill();
            let _ = child.wait();
        }
        for event in self.events() {
            kill(event.pid);
        }
    }
}

/// Ends the process `pid` names at once, if it still runs.
fn kill(pid: u32) {
    if let Ok(process) = os::process::ProcessRef::open(pid) {
        let _ = process.kill();
    }
}

fn alive(pid: u32) -> bool {
    os::process::ProcessRef::open(pid).is_ok_and(|process| process.alive())
}

/// Whether something answers on the local socket at `path`. The test's
/// runtime has threads to spare, so this one blocks for the dial.
fn answers(path: &Path) -> bool {
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current()
            .block_on(agent_dir::local_socket::connect(path))
            .is_ok()
    })
}

async fn until_dead(pid: u32) {
    until(&format!("pid {pid} to die"), || {
        std::future::ready((!alive(pid)).then_some(()).ok_or("still alive"))
    })
    .await
    .unwrap();
}

fn locked(path: &Path) -> bool {
    assert!(path.exists(), "no lock file at {}", path.display());
    os::lock::FileLock::held(path).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn restarts_the_daemon_whenever_it_exits_and_stops_it_when_asked() {
    let mut fixture = Fixture::new().await;
    let supervisor = fixture.start();
    let first = fixture.nth(1, "go", "1.0.0").await;
    assert_ne!(first.pid, supervisor);
    let lock = fixture.dir.join("data").join(SUPERVISOR_LOCK);
    assert!(locked(&lock), "the supervisor holds its lock");
    assert_eq!(
        std::fs::read_to_string(&lock).unwrap(),
        supervisor.to_string(),
        "the lock names the supervisor's pid"
    );

    kill(first.pid);
    let second = fixture.nth(2, "go", "1.0.0").await;
    assert_ne!(second.pid, first.pid);

    // A crash right after go is restarted too, after a backoff.
    fixture.behave("1.0.0", "crash-after-go");
    kill(second.pid);
    fixture.nth(1, "crash", "1.0.0").await;
    let fourth = fixture.nth(4, "go", "1.0.0").await;

    fixture.stop_supervisor().await;
    fixture.nth_stop(1, "1.0.0").await;
    assert!(fixture.supervisor_exit().await.success());
    assert!(!alive(fourth.pid));
    assert!(!locked(&lock), "a stopped supervisor releases its lock");
}

/// The supervisor core times its steps against the budget in its
/// parameters: under a budget of zero the running supervisor soon takes a
/// step over it, and writes the text file of its state and inputs that
/// names the overrun, once in the interval however many steps run over.
#[tokio::test(flavor = "multi_thread")]
async fn a_step_over_its_budget_leaves_the_supervisors_state_and_inputs_file() {
    let mut fixture = Fixture::new().await;
    fixture.step_budget_micros = Some(0);
    fixture.start();
    fixture.nth(1, "go", "1.0.0").await;
    let reports = fixture.dir.join("data").join(node::REPORTS);
    let files = until("the supervisor's dump", || {
        let files: Vec<PathBuf> = std::fs::read_dir(&reports)
            .map(|entries| entries.map(|entry| entry.unwrap().path()).collect())
            .unwrap_or_default();
        std::future::ready((!files.is_empty()).then_some(files).ok_or("no file yet"))
    })
    .await
    .unwrap();
    assert_eq!(files.len(), 1, "{files:?}");
    let name = files[0].file_name().unwrap().to_string_lossy().into_owned();
    assert!(
        name.starts_with("supervisor-") && name.ends_with(".json"),
        "{name}"
    );
    let text = std::fs::read_to_string(&files[0]).unwrap();
    let written: serde_json::Value = serde_json::from_str(&text).unwrap();
    let reason = written["reason"].as_str().unwrap_or_default();
    println!("{}\n{reason}", files[0].display());
    assert!(
        reason.starts_with(&format!(
            "{}a step took ",
            node::supervisor::SUPERVISOR_CORE_DUMP
        )) && reason.contains(", over its budget of 0 µs"),
        "{text}"
    );
    assert_eq!(written["core"], "supervisor", "{text}");
    assert!(written["now"]["params"].is_object(), "{text}");
    fixture.stop_supervisor().await;
    assert!(fixture.supervisor_exit().await.success());
    // Every step ran over, and the file stayed one.
    assert_eq!(std::fs::read_dir(&reports).unwrap().count(), 1);
}

/// The supervisor becomes the installed build keeping its child, the pipe
/// and the lock: by an exec, keeping its pid too, on Unix; on Windows by a
/// successor it starts as it exits, which takes the lock again.
#[tokio::test(flavor = "multi_thread")]
async fn installs_a_newer_signed_release_and_execs_it_keeping_pid_child_pipe_and_lock() {
    let mut fixture = Fixture::new().await;
    let supervisor = fixture.start();
    let old = fixture.nth(1, "go", "1.0.0").await;
    fixture.publish("2.0.0");

    let handed = fixture.nth(1, "supervise-inherited", "2.0.0").await;
    fixture.took_over(supervisor, &handed).await;
    let events = fixture.events();
    let order: Vec<(String, String, u32)> = events
        .iter()
        .map(|event| (event.what.clone(), event.version.clone(), event.pid))
        .collect();
    let new = fixture.nth(1, "go", "2.0.0").await;
    let at = |what: &str, version: &str, pid: u32| {
        order
            .iter()
            .position(|event| *event == (what.to_owned(), version.to_owned(), pid))
            .unwrap_or_else(|| panic!("no {what} {version} {pid} in {order:?}"))
    };
    let stopped = fixture.nth_stop(1, "1.0.0").await;
    assert!(at(&stopped.what, "1.0.0", old.pid) < at("start", "2.0.0", new.pid));
    assert!(at("go", "2.0.0", new.pid) < at("supervise-inherited", "2.0.0", handed.pid));
    assert_eq!(fixture.installed(), "2.0.0");
    assert!(!fixture.prev().exists(), "activation deleted prev");
    assert!(!fixture.beside().staged.exists());

    // The same manifest again installs nothing: 2.0.0 is not newer.
    fixture.ticks(3).await;
    assert_eq!(fixture.count("start", "2.0.0"), 1);
    assert_eq!(
        fixture.count("eof", "2.0.0"),
        0,
        "the pipe crossed the exec"
    );

    // The exec'd supervisor still waits on and restarts the child.
    kill(new.pid);
    let restarted = fixture.nth(2, "go", "2.0.0").await;
    assert_ne!(restarted.pid, new.pid);
    fixture.transcript("an update to 2.0.0, activated, then a crash of the new daemon");

    // Killed, the supervisor takes its daemon with it: end of file on the
    // pipe.
    kill(handed.pid);
    until_dead(handed.pid).await;
    let eof = fixture.nth(1, "eof", "2.0.0").await;
    assert_eq!(eof.pid, restarted.pid);
    until_dead(restarted.pid).await;
}

/// Design goal #1 across releases: agents and tool servers keep the image
/// of the build they started from, and on Windows that file can be neither
/// deleted nor renamed over while they run. Two updates in a row, each
/// with a process of every earlier build still running, both install,
/// activate and hand over; each retired build moves aside to a name of its
/// own, and a check sweeps it once nothing runs it.
#[tokio::test(flavor = "multi_thread")]
async fn two_updates_in_a_row_install_while_every_earlier_build_still_runs() {
    let mut fixture = Fixture::new().await;
    let first = fixture.start();
    fixture.nth(1, "go", "1.0.0").await;
    let mut running = vec![fixture.run_installed()];

    fixture.publish("2.0.0");
    let second = fixture.nth(1, "supervise-inherited", "2.0.0").await;
    fixture.took_over(first, &second).await;
    running.push(fixture.run_installed());

    fixture.publish("2.1.0");
    fixture.nth(1, "go", "2.1.0").await;
    let third = fixture.nth(1, "supervise-inherited", "2.1.0").await;
    fixture.took_over(second.pid, &third).await;
    fixture.transcript("two updates in a row, a process of each earlier build running");
    assert_eq!(fixture.installed(), "2.1.0");
    assert!(!fixture.prev().exists(), "activation retired prev");
    for pid in &running {
        assert!(alive(*pid), "pid {pid} runs on from its build's image");
    }
    if !os::fs::RUNNING_IMAGE_REPLACEABLE {
        assert_eq!(
            fixture.asides().len(),
            2,
            "one build moved aside per update: {:?}",
            fixture.asides()
        );
    }

    // The earlier builds' last processes exit: a check sweeps their files.
    for pid in running {
        kill(pid);
        until_dead(pid).await;
    }
    until("the binaries moved aside to be swept", || {
        let asides = fixture.asides();
        std::future::ready(if asides.is_empty() {
            Ok(())
        } else {
            Err(format!("{asides:?}"))
        })
    })
    .await
    .unwrap();
    assert_eq!(fixture.count("start", "2.1.0"), 1);
    assert!(alive(third.pid), "the supervisor serves on");
}

/// The launcher beside the binary is updated from the same release, while
/// it runs: it is the login item's restart policy, and on Windows keeps
/// its image mapped. The line it started goes on across the update, and a
/// deliberate stop still ends it.
#[tokio::test(flavor = "multi_thread")]
async fn an_update_installs_the_releases_launcher_beside_the_binary_while_it_runs() {
    let mut fixture = Fixture::new().await;
    fixture.start_launched();
    let supervisor = fixture.nth(1, "supervise", "1.0.0").await;
    fixture.nth(1, "go", "1.0.0").await;
    let launcher = fixture.launcher_path();
    // Another build of the launcher: the same program with bytes after it
    // that nothing reads.
    let mut newer = std::fs::read(&launcher).unwrap();
    newer.extend_from_slice(b"the release's launcher");
    fixture
        .server
        .publish_with_launcher("2.0.0", fixture.bytes("2.0.0"), newer.clone());

    let handed = fixture.nth(1, "supervise-inherited", "2.0.0").await;
    fixture.took_over(supervisor.pid, &handed).await;
    fixture.transcript("an update to 2.0.0 with its launcher, under a running launcher");
    assert_eq!(fixture.installed(), "2.0.0");
    assert!(
        std::fs::read(&launcher).unwrap() == newer,
        "the release's launcher is beside the binary"
    );
    assert!(
        !launcher
            .with_file_name(format!("{}.staged", exe("amux-launch")))
            .exists(),
        "nothing is left staged"
    );
    assert!(
        fixture.launcher_runs(),
        "the running launcher kept its line"
    );

    fixture.stop_supervisor().await;
    assert!(fixture.launcher_exit().await.success());
    assert_eq!(
        fixture.count("supervise", "1.0.0"),
        1,
        "nothing was started again"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_release_with_a_bad_signature_or_hash_is_not_installed() {
    let mut fixture = Fixture::new().await;
    fixture.start();
    fixture.nth(1, "go", "1.0.0").await;

    fixture
        .server
        .publish("2.0.0", fixture.bytes("2.0.0"), &OTHER_SEED, None);
    fixture.ticks(3).await;
    // A manifest whose hash names other bytes than it serves.
    fixture.publish("2.0.0");
    fixture
        .server
        .artifacts
        .lock()
        .unwrap()
        .insert("amux-2.0.0".into(), fixture.bytes("1.0.0"));
    fixture.ticks(3).await;
    // Checks run one at a time: once an empty manifest has been read, the
    // last bad download is over.
    *fixture.server.manifest.lock().unwrap() = r#"{"targets":{}}"#.to_owned();
    fixture.ticks(2).await;

    assert_eq!(fixture.count("start", "2.0.0"), 0);
    assert_eq!(fixture.installed(), "1.0.0");
    assert!(!fixture.prev().exists());
    assert!(!fixture.beside().staged.exists());
    assert_eq!(
        fixture.count("start", "1.0.0"),
        1,
        "the daemon was never stopped"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn k_starts_that_never_prepare_roll_back_and_the_rejected_build_is_skipped() {
    let mut fixture = Fixture::new().await;
    let supervisor = fixture.start();
    fixture.nth(1, "go", "1.0.0").await;
    fixture.behave("2.0.0", "exit");
    fixture.publish("2.0.0");

    let back = fixture.nth(2, "go", "1.0.0").await;
    assert_eq!(fixture.count("start", "2.0.0"), 3);
    assert_eq!(fixture.rejected().as_deref(), Some("2.0.0"));
    // Record first, rename second: a crash between them leaves a state a
    // starting supervisor finishes.
    let log = std::fs::read_to_string(fixture.dir.join("supervisor.log")).unwrap();
    let recorded = log.find("recorded the rejected build").expect("recorded");
    let renamed = log.find("put prev back over the binary").expect("renamed");
    assert!(recorded < renamed, "{log}");
    assert_eq!(fixture.installed(), "1.0.0");
    assert!(!fixture.prev().exists());
    // prev was this supervisor's own binary: nothing to exec.
    assert_eq!(fixture.count("supervise-inherited", "1.0.0"), 0);
    assert!(alive(supervisor));

    // The rejected build stays skipped while the channel names it.
    fixture.ticks(4).await;
    assert_eq!(fixture.count("start", "2.0.0"), 3);
    assert!(alive(back.pid));

    // A different version is installed.
    fixture.publish("2.1.0");
    fixture.nth(1, "go", "2.1.0").await;
    fixture.nth(1, "supervise-inherited", "2.1.0").await;
    assert_eq!(fixture.installed(), "2.1.0");
    fixture.transcript("2.0.0 never prepares: rolled back, skipped, then 2.1.0 installed");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_child_that_hangs_before_prepared_is_stopped_at_the_start_deadline_and_counted() {
    let mut fixture = Fixture::new().await;
    fixture.start();
    fixture.nth(1, "go", "1.0.0").await;
    fixture.behave("2.0.0", "hang");
    fixture.publish("2.0.0");

    fixture.nth(2, "go", "1.0.0").await;
    let hung: Vec<u32> = fixture
        .events()
        .iter()
        .filter(|event| event.what == "start" && event.version == "2.0.0")
        .map(|event| event.pid)
        .collect();
    assert_eq!(hung.len(), 3);
    for pid in hung {
        until_dead(pid).await;
    }
    assert_eq!(fixture.rejected().as_deref(), Some("2.0.0"));
    assert_eq!(fixture.installed(), "1.0.0");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_daemon_that_ignores_the_stop_signal_is_killed_at_the_stop_deadline() {
    let mut fixture = Fixture::new().await;
    fixture.start();
    fixture.behave("1.0.0", "ignore-term");
    let old = fixture.nth(1, "go", "1.0.0").await;
    fixture.publish("2.0.0");

    fixture.nth(1, "go", "2.0.0").await;
    assert_eq!(fixture.count("ignored-term", "1.0.0"), 1);
    until_dead(old.pid).await;
    // The update ends with the supervisor becoming 2.0.0: an exec on Unix,
    // a successor of a new pid on Windows. Ended before that, the test
    // would leave the successor running, unnamed in the events.
    fixture.nth(1, "supervise-inherited", "2.0.0").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_starting_supervisor_counts_failed_starts_against_prev_and_rolls_back() {
    // A reboot in the middle of an update: 2.0.0 at the path, 1.0.0 as prev.
    let mut fixture = Fixture::new().await;
    let build = fixture.build("1.0.0");
    write_executable(|| std::fs::copy(build, fixture.prev())).unwrap();
    std::fs::remove_file(&fixture.bin).unwrap();
    let build = fixture.build("2.0.0");
    write_executable(|| std::fs::copy(build, &fixture.bin)).unwrap();
    fixture.behave("2.0.0", "exit");
    let supervisor = fixture.start();

    fixture.nth(1, "go", "1.0.0").await;
    assert_eq!(fixture.count("start", "2.0.0"), 3);
    assert_eq!(fixture.rejected().as_deref(), Some("2.0.0"));
    // The supervisor was running 2.0.0: after go it becomes 1.0.0.
    let handed = fixture.nth(1, "supervise-inherited", "1.0.0").await;
    fixture.took_over(supervisor, &handed).await;
    assert_eq!(fixture.installed(), "1.0.0");
    assert!(!fixture.prev().exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_starting_supervisor_finishes_a_recorded_rollback() {
    // A crash between recording the rejected build and renaming prev back.
    let mut fixture = Fixture::new().await;
    let build = fixture.build("1.0.0");
    write_executable(|| std::fs::copy(build, fixture.prev())).unwrap();
    std::fs::remove_file(&fixture.bin).unwrap();
    let build = fixture.build("2.0.0");
    write_executable(|| std::fs::copy(build, &fixture.bin)).unwrap();
    std::fs::write(fixture.beside().rejected, "2.0.0").unwrap();
    let supervisor = fixture.start();

    fixture.nth(1, "go", "1.0.0").await;
    assert_eq!(
        fixture.count("start", "2.0.0"),
        0,
        "the rejected build never runs"
    );
    let handed = fixture.nth(1, "supervise-inherited", "1.0.0").await;
    fixture.took_over(supervisor, &handed).await;
    assert_eq!(fixture.installed(), "1.0.0");
    assert!(!fixture.prev().exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_starting_supervisor_deletes_a_prev_that_is_the_binary() {
    // A crash between the hard link and the rename: the swap never happened.
    let mut fixture = Fixture::new().await;
    std::fs::hard_link(&fixture.bin, fixture.prev()).unwrap();
    fixture.behave("1.0.0", "exit");
    fixture.start();

    fixture
        .until("prev removed", |_| !fixture.prev().exists())
        .await;
    // Its failed starts are plain crashes now: nothing is rejected.
    fixture.nth(4, "start", "1.0.0").await;
    assert_eq!(fixture.rejected(), None);
    assert_eq!(fixture.installed(), "1.0.0");
}

/// Windows' login item, `amux-launch`, starts the supervisor again when it
/// dies: its lineage's pipe ends with nothing written. The waits double
/// from a second.
#[tokio::test(flavor = "multi_thread")]
async fn the_launcher_starts_a_supervisor_that_dies_again_after_a_backoff() {
    let mut fixture = Fixture::new().await;
    fixture.start_launched();
    let mut gaps = Vec::new();
    for n in 1..=2 {
        let supervisor = fixture.nth(n, "supervise", "1.0.0").await;
        fixture.nth(n, "go", "1.0.0").await;
        kill(supervisor.pid);
        let killed = std::time::Instant::now();
        let next = fixture.nth(n + 1, "supervise", "1.0.0").await;
        gaps.push(killed.elapsed());
        assert_ne!(next.pid, supervisor.pid);
    }
    fixture.nth(3, "go", "1.0.0").await;
    fixture.transcript("a launched supervisor killed twice");
    println!("restarted after {gaps:?}");
    assert!(gaps[0] >= Duration::from_secs(1), "{gaps:?}");
    assert!(gaps[1] >= Duration::from_secs(2), "{gaps:?}");
    assert!(fixture.launcher_runs(), "one launcher throughout");
}

/// The supervisor's self-update keeps the launcher's line open: the
/// supervisor becomes the installed build (a successor of a new pid on
/// Windows) holding the lineage, so the launcher starts nothing, and the
/// daemon the update activated serves on across it. A deliberate stop of
/// the successor then ends the launcher.
#[tokio::test(flavor = "multi_thread")]
async fn a_self_update_keeps_the_launchers_line_and_a_stop_after_it_ends_the_launcher() {
    let mut fixture = Fixture::new().await;
    fixture.start_launched();
    let first = fixture.nth(1, "supervise", "1.0.0").await;
    fixture.nth(1, "go", "1.0.0").await;
    fixture.publish("2.0.0");

    let daemon = fixture.nth(1, "go", "2.0.0").await;
    let handed = fixture.nth(1, "supervise-inherited", "2.0.0").await;
    fixture.took_over(first.pid, &handed).await;
    // Long past the launcher's first backoff: a line that had ended would
    // have been started again by now.
    fixture.ticks(10).await;
    fixture.transcript("a launched supervisor updates itself to 2.0.0");
    let fresh = fixture.count("supervise", "1.0.0") + fixture.count("supervise", "2.0.0");
    assert_eq!(fresh, 1, "the launcher started one supervisor");
    assert!(fixture.launcher_runs(), "one launcher throughout");
    assert_eq!(fixture.count("start", "2.0.0"), 1);
    assert_eq!(
        fixture.count("eof", "2.0.0"),
        0,
        "the daemon was never stopped"
    );
    assert!(alive(daemon.pid), "the daemon serves on");

    fixture.stop_supervisor().await;
    assert!(fixture.launcher_exit().await.success());
    fixture.nth(1, "supervisor-exit", "2.0.0").await;
    until_dead(daemon.pid).await;
    assert_eq!(
        fixture.count("supervise", "2.0.0"),
        0,
        "nothing was started again"
    );
}

/// A deliberate stop (`amux server stop`, over the control socket) ends
/// the launcher, which reads the `0` the supervisor wrote before it exited
/// and starts nothing again.
#[tokio::test(flavor = "multi_thread")]
async fn a_deliberate_stop_ends_the_launcher() {
    let mut fixture = Fixture::new().await;
    fixture.start_launched();
    let daemon = fixture.nth(1, "go", "1.0.0").await;
    fixture.stop_supervisor().await;
    assert!(fixture.launcher_exit().await.success());
    fixture.nth(1, "supervisor-exit", "1.0.0").await;
    until_dead(daemon.pid).await;
    assert_eq!(
        fixture.count("supervise", "1.0.0"),
        1,
        "nothing was started again"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_amux_daemon_exits_when_its_supervisor_dies() {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("data");
    let config = root.path().join("config.toml");
    let socket = data.join("amux.sock");
    std::fs::write(
        &config,
        format!(
            "root = {:?}\nsupervisor = true\nupdates = \"manual\"\n",
            data.display().to_string(),
        ),
    )
    .unwrap();
    let mut command = Command::new(binaries().join(exe("amux")));
    command
        .arg("supervise")
        .env("AMUX_CONFIG", &config)
        .env_remove("AMUX_LOG")
        .stdin(Stdio::null());
    let mut supervisor = start(|| command.spawn()).unwrap();
    let lock = data.join(node::INSTALLATION_LOCK);
    until("the daemon to answer on its socket", || {
        let answered = answers(&socket);
        let exited = supervisor.try_wait().unwrap();
        std::future::ready(match (answered, exited) {
            (true, _) => Ok(()),
            (false, Some(status)) => panic!("the supervisor exited: {status}"),
            (false, None) => Err("no answer on the socket yet".to_owned()),
        })
    })
    .await
    .unwrap();
    assert!(locked(&lock));
    assert!(locked(&data.join(SUPERVISOR_LOCK)));

    supervisor.kill().unwrap();
    supervisor.wait().unwrap();
    until("the daemon to release the installation lock", || {
        std::future::ready(if locked(&lock) {
            Err("still locked".to_owned())
        } else {
            Ok(())
        })
    })
    .await
    .expect("the daemon outlived its supervisor");
}

#[test]
fn choose_installs_only_a_newer_unrejected_build_inside_the_rollout() {
    let running = Version::new(1, 0, 0);
    let manifest = |version: &str, rollout| Manifest {
        channel: "stable".into(),
        rollout,
        targets: [(
            release::TARGET.to_owned(),
            Release {
                version: version.to_owned(),
                url: "http://example/amux".into(),
                sha256: "00".into(),
                size: 1,
            },
        )]
        .into(),
        signature: String::new(),
    };
    let host = Uuid::new_v4();
    let choose = |manifest: &Manifest, rejected: Option<&Version>, host: Option<&Uuid>| {
        release::choose(manifest, release::TARGET, &running, rejected, host)
    };
    assert!(matches!(
        choose(&manifest("1.1.0", None), None, None),
        Choice::Install { version, .. } if version == Version::new(1, 1, 0)
    ));
    assert_eq!(
        choose(&manifest("1.0.0", None), None, None),
        Choice::Skip(Skip::NotNewer(Version::new(1, 0, 0)))
    );
    assert_eq!(
        choose(&manifest("0.9.0", None), None, None),
        Choice::Skip(Skip::NotNewer(Version::new(0, 9, 0)))
    );
    // Prerelease ordering: a preview of 1.0.0 is below 1.0.0.
    assert!(matches!(
        choose(&manifest("1.0.0-preview.3", None), None, None),
        Choice::Skip(Skip::NotNewer(_))
    ));
    let rejected = Version::new(1, 1, 0);
    assert_eq!(
        choose(&manifest("1.1.0", None), Some(&rejected), None),
        Choice::Skip(Skip::Rejected(rejected.clone()))
    );
    assert!(matches!(
        choose(&manifest("1.2.0", None), Some(&rejected), None),
        Choice::Install { .. }
    ));
    let other_target = Manifest {
        targets: [(
            "another-triple".to_owned(),
            manifest("2.0.0", None)
                .targets
                .into_values()
                .next()
                .unwrap(),
        )]
        .into(),
        ..manifest("2.0.0", None)
    };
    assert_eq!(
        choose(&other_target, None, None),
        Choice::Skip(Skip::NoBuildForTarget)
    );

    // The rollout: inside when hash(host id) mod 100 is under the number.
    let slot = release::rollout_slot(&host);
    assert_eq!(
        slot,
        release::rollout_slot(&host),
        "a host's place is fixed"
    );
    assert!(matches!(
        choose(
            &manifest("1.1.0", Some((slot + 1) as u8)),
            None,
            Some(&host)
        ),
        Choice::Install { .. }
    ));
    assert_eq!(
        choose(&manifest("1.1.0", Some(slot as u8)), None, Some(&host)),
        Choice::Skip(Skip::OutsideRollout)
    );
    assert!(matches!(
        choose(&manifest("1.1.0", Some(100)), None, None),
        Choice::Install { .. }
    ));
    assert_eq!(
        choose(&manifest("1.1.0", Some(50)), None, None),
        Choice::Skip(Skip::OutsideRollout),
        "a host without an id waits for the full rollout"
    );
    // Across many hosts the slots cover the range.
    let slots: std::collections::BTreeSet<u64> = (0..2000)
        .map(|_| release::rollout_slot(&Uuid::new_v4()))
        .collect();
    assert_eq!(slots.len(), 100);
}

#[test]
fn the_manifest_is_verified_whole_and_the_artifact_against_its_entry() {
    let key = [release::TEST_RELEASE_KEY];
    let bytes = b"a build".to_vec();
    let entry = Release {
        version: "2.0.0".into(),
        url: String::new(),
        sha256: release::sha256_of(&bytes),
        size: bytes.len() as u64,
    };
    let signed = |seed: &[u8; 32], channel: &str, rollout| {
        let mut manifest = Manifest {
            channel: channel.into(),
            rollout,
            targets: [(release::TARGET.to_owned(), entry.clone())].into(),
            signature: String::new(),
        };
        manifest.signature = release::sign(seed, &manifest);
        manifest
    };
    let good = signed(&TEST_SEED, "stable", Some(10));
    assert_eq!(release::verify_manifest(&good, "stable", &key), Ok(()));
    assert_eq!(
        release::verify_manifest(&signed(&OTHER_SEED, "stable", Some(10)), "stable", &key),
        Err(VerifyError::Signature)
    );
    // A build trusts every key it was given: the backup signs as well as
    // the key it stands behind.
    let backup = release::public_key(&OTHER_SEED);
    assert_eq!(
        release::verify_manifest(
            &signed(&OTHER_SEED, "stable", Some(10)),
            "stable",
            &[release::TEST_RELEASE_KEY, backup]
        ),
        Ok(())
    );
    assert_eq!(
        release::verify_manifest(&good, "stable", &[]),
        Err(VerifyError::Signature)
    );
    // A build deployed to preview does not pass as stable.
    assert_eq!(
        release::verify_manifest(&signed(&TEST_SEED, "preview", None), "stable", &key),
        Err(VerifyError::Channel {
            expected: "stable".into(),
            actual: "preview".into()
        })
    );
    // The rollout, a version, the hash and the size are all under the
    // signature: changing any after signing breaks it.
    for tampered in [
        Manifest {
            rollout: Some(100),
            ..good.clone()
        },
        Manifest {
            targets: [(
                release::TARGET.to_owned(),
                Release {
                    version: "9.0.0".into(),
                    ..entry.clone()
                },
            )]
            .into(),
            ..good.clone()
        },
        Manifest {
            targets: [(
                release::TARGET.to_owned(),
                Release {
                    sha256: release::sha256_of(b"other"),
                    ..entry.clone()
                },
            )]
            .into(),
            ..good.clone()
        },
        Manifest {
            targets: [(
                release::TARGET.to_owned(),
                Release {
                    size: entry.size + 1,
                    ..entry.clone()
                },
            )]
            .into(),
            ..good.clone()
        },
    ] {
        assert_eq!(
            release::verify_manifest(&tampered, "stable", &key),
            Err(VerifyError::Signature)
        );
    }

    assert_eq!(
        release::verify_artifact(&entry, &release::sha256_of(&bytes), bytes.len() as u64),
        Ok(())
    );
    assert!(matches!(
        release::verify_artifact(&entry, &release::sha256_of(b"other"), bytes.len() as u64),
        Err(VerifyError::Hash { .. })
    ));
    assert!(matches!(
        release::verify_artifact(&entry, &release::sha256_of(&bytes), 3),
        Err(VerifyError::Size { .. })
    ));
}

#[test]
fn restamping_a_build_changes_the_version_it_reports() {
    let dir = tempfile::tempdir().unwrap();
    let copy = dir.path().join("amux");
    let amux = binaries().join(exe("amux"));
    write_executable(|| release::restamp(&amux, &copy, "0.0.1-previous")).unwrap();
    let output = start(|| {
        Command::new(&copy)
            .arg("--version")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
    })
    .unwrap()
    .wait_with_output()
    .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "amux 0.0.1-previous"
    );
}
