//! `amux supervise`, driven as processes: the real supervisor inside a
//! stand-in amux binary (`fake-amux`), whose daemon mode is scripted per
//! version, against a fake manifest server. Versions are re-stamped copies
//! of one build, and releases are signed with the test key debug builds
//! trust.

#![cfg(unix)]

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use ::release::TEST_SEED;
use node::release::{self, Choice, Manifest, Release, Skip, VerifyError};
use node::supervisor::SUPERVISOR_LOCK;
use semver::Version;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use uuid::Uuid;
const OTHER_SEED: [u8; 32] = [7; 32];
use patience::until;

/// Tests in this binary write executables and start processes on parallel
/// threads. A process forked while another thread holds a freshly written
/// executable open for writing inherits that descriptor until it execs, and
/// on Linux an exec of the file meanwhile fails with "Text file busy". So
/// writing an executable excludes every start in this process, and a start
/// holds the lock until the child has exec'd (`spawn` returns only then).
static EXECUTABLES: RwLock<()> = RwLock::new(());

/// Writes an executable with no process start in flight. `write` must not
/// start a process itself (resolve `binaries()` before calling).
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

/// fake-amux and amux, built once per run.
fn binaries() -> &'static Path {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    BUILT.get_or_init(|| {
        let mut cargo = provider_fakes::cargo::command();
        cargo
            .args([
                "build",
                "--locked",
                "-p",
                "fake-amux",
                "-p",
                "amux",
                "--bins",
            ])
            .current_dir(env!("CARGO_MANIFEST_DIR"));
        let status = start(|| cargo.spawn())
            .expect("cargo runs")
            .wait()
            .expect("cargo runs");
        assert!(status.success(), "building fake-amux and amux failed");
        let exe = std::env::current_exe().expect("the test binary's path");
        exe.parent()
            .and_then(Path::parent)
            .expect("a target directory")
            .to_owned()
    })
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
}

/// An install with fake-amux 1.0.0 at `<dir>/bin/amux`.
struct Fixture {
    _root: tempfile::TempDir,
    dir: PathBuf,
    bin: PathBuf,
    server: Server,
    supervisor: Option<Child>,
}

impl Fixture {
    async fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let dir = std::fs::canonicalize(root.path()).unwrap();
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::create_dir_all(dir.join("builds")).unwrap();
        let fixture = Self {
            bin: dir.join("bin").join("amux"),
            dir,
            _root: root,
            server: Server::start().await,
            supervisor: None,
        };
        let build = fixture.build("1.0.0");
        write_executable(|| std::fs::copy(build, &fixture.bin)).unwrap();
        fixture
    }

    /// fake-amux stamped as `version`.
    fn build(&self, version: &str) -> PathBuf {
        let path = self.dir.join("builds").join(format!("amux-{version}"));
        if !path.exists() {
            let fake = binaries().join("fake-amux");
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

    fn prev(&self) -> PathBuf {
        self.dir.join("bin").join("amux.prev")
    }

    fn rejected(&self) -> Option<String> {
        std::fs::read_to_string(self.dir.join("bin").join("amux.rejected")).ok()
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

    fn start(&mut self) -> u32 {
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join("supervisor.log"))
            .unwrap();
        let mut command = Command::new(&self.bin);
        command
            .arg("supervise")
            .env("FAKE_AMUX_DIR", &self.dir)
            .env("FAKE_AMUX_MANIFEST", self.server.url("/stable.json"))
            .env("RUST_LOG", "info")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log);
        let child = start(|| command.spawn()).unwrap();
        let pid = child.id();
        self.supervisor = Some(child);
        pid
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
        let supervisor = self.supervisor_pid();
        let mut daemons: Vec<u32> = Vec::new();
        println!("== {title}");
        for event in self.events() {
            let who = if event.pid == supervisor {
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
        for name in names {
            let detail = match name.as_str() {
                "amux" => format!("version {}", self.installed()),
                "amux.rejected" => format!("names {}", self.rejected().unwrap_or_default()),
                _ => String::new(),
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

    fn supervisor_pid(&self) -> u32 {
        self.supervisor.as_ref().unwrap().id()
    }

    fn signal_supervisor(&self, signal: libc::c_int) {
        kill(self.supervisor_pid(), signal);
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
        if let Some(mut child) = self.supervisor.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        for event in self.events() {
            kill(event.pid, libc::SIGKILL);
        }
    }
}

fn kill(pid: u32, signal: libc::c_int) {
    // SAFETY: a signal to a test's own process.
    unsafe {
        libc::kill(pid as libc::pid_t, signal);
    }
}

fn alive(pid: u32) -> bool {
    // SAFETY: signal 0 only probes.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

async fn until_dead(pid: u32) {
    until(&format!("pid {pid} to die"), || {
        std::future::ready((!alive(pid)).then_some(()).ok_or("still alive"))
    })
    .await
    .unwrap();
}

fn locked(path: &Path) -> bool {
    let file = OpenOptions::new().write(true).open(path).unwrap();
    matches!(file.try_lock(), Err(std::fs::TryLockError::WouldBlock))
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

    kill(first.pid, libc::SIGKILL);
    let second = fixture.nth(2, "go", "1.0.0").await;
    assert_ne!(second.pid, first.pid);

    // A crash right after go is restarted too, after a backoff.
    fixture.behave("1.0.0", "crash-after-go");
    kill(second.pid, libc::SIGKILL);
    fixture.nth(1, "crash", "1.0.0").await;
    let fourth = fixture.nth(4, "go", "1.0.0").await;

    fixture.signal_supervisor(libc::SIGTERM);
    fixture.nth(1, "term", "1.0.0").await;
    assert!(fixture.supervisor_exit().await.success());
    assert!(!alive(fourth.pid));
    assert!(!locked(&lock), "a stopped supervisor releases its lock");
}

#[tokio::test(flavor = "multi_thread")]
async fn installs_a_newer_signed_release_and_execs_it_keeping_pid_child_pipe_and_lock() {
    let mut fixture = Fixture::new().await;
    let supervisor = fixture.start();
    let old = fixture.nth(1, "go", "1.0.0").await;
    fixture.publish("2.0.0");

    let handed = fixture.nth(1, "supervise-inherited", "2.0.0").await;
    assert_eq!(
        handed.pid, supervisor,
        "the exec keeps the supervisor's pid"
    );
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
    assert!(at("term", "1.0.0", old.pid) < at("start", "2.0.0", new.pid));
    assert!(at("go", "2.0.0", new.pid) < at("supervise-inherited", "2.0.0", supervisor));
    assert_eq!(fixture.installed(), "2.0.0");
    assert!(!fixture.prev().exists(), "activation deleted prev");
    assert!(!fixture.dir.join("bin/amux.staged").exists());
    let lock = fixture.dir.join("data").join(SUPERVISOR_LOCK);
    assert!(locked(&lock), "the lock crossed the exec");
    assert_eq!(
        std::fs::read_to_string(&lock).unwrap(),
        supervisor.to_string()
    );

    // The same manifest again installs nothing: 2.0.0 is not newer.
    fixture.ticks(3).await;
    assert_eq!(fixture.count("start", "2.0.0"), 1);
    assert_eq!(
        fixture.count("eof", "2.0.0"),
        0,
        "the pipe crossed the exec"
    );

    // The exec'd supervisor still waits on and restarts the child.
    kill(new.pid, libc::SIGKILL);
    let restarted = fixture.nth(2, "go", "2.0.0").await;
    assert_ne!(restarted.pid, new.pid);
    fixture.transcript("an update to 2.0.0, activated, then a crash of the new daemon");

    // Killed, the supervisor takes its daemon with it: end of file on the
    // pipe.
    fixture.signal_supervisor(libc::SIGKILL);
    fixture.supervisor_exit().await;
    let eof = fixture.nth(1, "eof", "2.0.0").await;
    assert_eq!(eof.pid, restarted.pid);
    until_dead(restarted.pid).await;
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
    assert!(!fixture.dir.join("bin/amux.staged").exists());
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
    assert_eq!(handed.pid, supervisor);
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
    std::fs::write(fixture.dir.join("bin/amux.rejected"), "2.0.0").unwrap();
    let supervisor = fixture.start();

    fixture.nth(1, "go", "1.0.0").await;
    assert_eq!(
        fixture.count("start", "2.0.0"),
        0,
        "the rejected build never runs"
    );
    let handed = fixture.nth(1, "supervise-inherited", "1.0.0").await;
    assert_eq!(handed.pid, supervisor);
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

#[tokio::test(flavor = "multi_thread")]
async fn the_amux_daemon_exits_when_its_supervisor_dies() {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("data");
    let config = root.path().join("installation.yaml");
    let socket = root.path().join("amux.sock");
    std::fs::write(
        &config,
        format!(
            "root: {}\nfront_door_socket: {}\nsupervisor: on\nupdates: manual\n",
            data.display(),
            socket.display()
        ),
    )
    .unwrap();
    let mut command = Command::new(binaries().join("amux"));
    command
        .arg("supervise")
        .env("AMUX_CONFIG", &config)
        .env_remove("AMUX_LOG")
        .stdin(Stdio::null());
    let mut supervisor = start(|| command.spawn()).unwrap();
    let lock = data.join(node::INSTALLATION_LOCK);
    until("the daemon to answer on its socket", || {
        let answered = std::os::unix::net::UnixStream::connect(&socket).is_ok();
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
    let log = std::fs::read_to_string(data.join("daemon.log")).unwrap();
    assert!(log.contains("the supervisor went away"), "{log}");
    assert!(log.contains("stopped cleanly"), "{log}");
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
    let key = release::TEST_RELEASE_KEY;
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
    let amux = binaries().join("amux");
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
