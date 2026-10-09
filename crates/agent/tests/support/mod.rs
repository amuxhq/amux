//! A harness for agent process tests: an agent directory with a spec that
//! runs a fake provider, the agent on a hand-driven clock, a stand-in daemon
//! on ctl.sock, and a reader of what the agent journaled.

#![allow(dead_code, unused_imports)]

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use agent::local_socket::{self, LocalStream};
use agent::{AgentError, ExitCause, ManualClock};
use prost::Message as _;
use provider_fakes::{SCRIPT_ENV, Script, Step};
use tokio::io::{ReadHalf, WriteHalf};
use tokio::task::JoinHandle;
use wire::{
    AgentHello, AgentParent, AgentSpec, ClaudePtyInput, ClaudePtyItem, ClaudeSdkInput,
    ClaudeSdkItem, ClaudeSdkSnapshot, CodexInput, CodexItem, CtlFrame, EffectiveConfig, Input,
    Kind, Phase, PromptInput, Stop, StopMode, claude_pty_input, claude_pty_item, claude_sdk_input,
    claude_sdk_item, codex_input, codex_item, ctl_frame, send_input_response,
};

/// When every agent in these tests starts.
pub const T0: i64 = 1_800_000_000_000;
pub const GRACE: i64 = 60_000;
pub const DRAIN: i64 = 120_000;

/// How long any one wait in a test may take before it is a hang.
pub use patience::{PATIENCE, holds_for, until};

/// Stands for the agent's release file in a script: a `wait_for` on it
/// holds the turn until the test calls [`Agent::release`].
const RELEASE: &str = "@release";

pub fn agent_release() -> PathBuf {
    PathBuf::from(RELEASE)
}

/// Runs the fake, then stays until the hold file exists: a provider that is
/// slow to go away after its input closes.
const HOLD_SCRIPT: &str = r#""$0" "$@"; while [ ! -e "$AMUX_TEST_HOLD" ]; do sleep 0.02; done"#;

/// The fake provider binaries and the install path's stand-in, built once
/// per test run.
pub fn fakes() -> &'static Path {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    BUILT.get_or_init(|| {
        let status = provider_fakes::cargo::command()
            .args([
                "build",
                "--locked",
                "-p",
                "provider-fakes",
                "-p",
                "claude",
                "--bins",
                "-p",
                "agent",
                "--example",
                "amux_stand_in",
            ])
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .status()
            .expect("cargo runs");
        assert!(status.success(), "building the fake providers failed");
        // target/<profile>/deps/<this test> -> target/<profile>
        let exe = std::env::current_exe().expect("the test binary's path");
        exe.parent()
            .and_then(Path::parent)
            .expect("a target directory")
            .to_owned()
    })
}

/// The stand-in for the amux binary at the install path: `hooks claude`
/// and `mcp <dir>`, as the harness launches them.
pub fn install_path() -> String {
    fakes()
        .join("examples")
        .join(format!("amux_stand_in{}", std::env::consts::EXE_SUFFIX))
        .to_str()
        .unwrap()
        .to_owned()
}

pub struct Setup {
    pub kind: Kind,
    /// The provider command; the kind's fake when None.
    pub command: Option<String>,
    pub provider_args: Vec<String>,
    /// More of the provider's environment, as the spec carries it.
    pub env: Vec<(&'static str, &'static str)>,
    pub steps: Vec<Step>,
    pub initial_prompt: Option<&'static str>,
    /// Whether the agent has a parent, which makes it one-shot.
    pub parent: bool,
    /// Keep the provider's process alive after the fake exits, until
    /// [`Agent::unhold`].
    pub hold: bool,
    /// The facts ring's size; the agent's default when zero.
    pub ring_bytes: u64,
    /// Terminal Claude without a messaging socket.
    pub socketless: bool,
    /// Play this recording from tests/replay/ instead of a script: the fake
    /// checks every byte the agent writes against it.
    pub replay: Option<&'static str>,
    /// The provider's own session id, as if an earlier start had made it.
    pub session: Option<&'static str>,
    /// The journal's segment size; the agent's default when zero.
    pub journal_bytes: u64,
    /// Terminal Claude's permission menu offers to switch to auto mode.
    pub offers_auto_mode: bool,
    /// Terminal Claude asks whether its folder is trusted first.
    pub untrusted_folder: bool,
    /// The agent's folder is a git repository on `main` with one commit.
    pub repository: bool,
}

impl Setup {
    pub fn sdk() -> Self {
        Self {
            kind: Kind::ClaudeSdk,
            command: None,
            provider_args: Vec::new(),
            env: Vec::new(),
            steps: Vec::new(),
            initial_prompt: None,
            parent: false,
            hold: false,
            ring_bytes: 0,
            socketless: false,
            replay: None,
            session: None,
            journal_bytes: 0,
            offers_auto_mode: false,
            untrusted_folder: false,
            repository: false,
        }
    }
}

pub struct Agent {
    kind: Kind,
    _root: tempfile::TempDir,
    pub dir: PathBuf,
    pub clock: ManualClock,
    release: PathBuf,
    hold: PathBuf,
    task: Mutex<Option<JoinHandle<Result<ExitCause, AgentError>>>>,
    reader: Mutex<(journal::Reader, Log)>,
}

/// Where the stdio fakes log every line the agent wrote them, under the
/// test's root.
const INPUT_LOG: &str = "provider-input.jsonl";

impl Agent {
    /// The agent's working folder.
    pub fn work(&self) -> PathBuf {
        self._root.path().join("work")
    }

    /// Every line the agent has written to its stdio provider so far.
    pub fn provider_input(&self) -> Vec<serde_json::Value> {
        std::fs::read_to_string(self._root.path().join(INPUT_LOG))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }

    /// The facts ring's entries, oldest first: every event the
    /// interpreter was fed, as private/facts/ holds them.
    pub fn facts(&self) -> Vec<serde_json::Value> {
        let dir = self.dir.join(agent::PRIVATE).join("facts");
        let mut segments: Vec<(u64, PathBuf)> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .filter_map(|entry| {
                let path = entry.ok()?.path();
                let start = path.file_name()?.to_str()?.parse().ok()?;
                Some((start, path))
            })
            .collect();
        segments.sort();
        segments
            .into_iter()
            .flat_map(|(_, path)| {
                std::fs::read_to_string(path)
                    .unwrap_or_default()
                    .lines()
                    .filter_map(|line| serde_json::from_str(line).ok())
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    pub async fn start(setup: Setup) -> Self {
        let root = tempfile::tempdir().expect("a temp dir");
        let dir = root.path().join("agents").join("a1");
        let work = root.path().join("work");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(&work).unwrap();
        if setup.repository {
            for args in [
                &["init", "-q", "-b", "main"][..],
                &["commit", "-q", "--allow-empty", "-m", "start"],
            ] {
                let status = std::process::Command::new("git")
                    .args([
                        "-c",
                        "user.name=amux",
                        "-c",
                        "user.email=amux@example.invalid",
                    ])
                    .args(["-c", "commit.gpgsign=false"])
                    .args(args)
                    .current_dir(&work)
                    .status()
                    .unwrap();
                assert!(status.success(), "git {args:?}");
            }
        }
        let release = root.path().join("release");
        let hold = root.path().join("hold");

        let script = Script {
            steps: setup.steps,
            offers_auto_mode: setup.offers_auto_mode,
            untrusted_folder: setup.untrusted_folder,
            ..Script::default()
        };
        // The placeholder is a whole string in the script, and the path
        // takes its place as one, escaped as JSON.
        let script = serde_json::to_string(&script).unwrap().replace(
            &serde_json::to_string(RELEASE).unwrap(),
            &serde_json::to_string(&release).unwrap(),
        );
        let script_path = root.path().join("script.json");
        std::fs::write(&script_path, script).unwrap();

        let fake = fakes().join(match setup.kind {
            Kind::ClaudePty => "fake-claude-pty",
            Kind::Codex => "fake-codex",
            _ => "fake-claude-sdk",
        });
        let (command, mut provider_args) = match (setup.command, setup.hold) {
            (Some(command), _) => (command, Vec::new()),
            (None, true) => (
                "/bin/sh".to_owned(),
                vec![
                    "-c".to_owned(),
                    HOLD_SCRIPT.to_owned(),
                    fake.to_str().unwrap().to_owned(),
                ],
            ),
            (None, false) => (fake.to_str().unwrap().to_owned(), Vec::new()),
        };
        provider_args.extend(setup.provider_args);
        let mut provider_env: std::collections::HashMap<String, String> = [
            (
                SCRIPT_ENV.to_owned(),
                script_path.to_str().unwrap().to_owned(),
            ),
            (
                "AMUX_TEST_HOLD".to_owned(),
                hold.to_str().unwrap().to_owned(),
            ),
            (
                provider_fakes::INPUT_LOG_ENV.to_owned(),
                root.path().join(INPUT_LOG).to_str().unwrap().to_owned(),
            ),
            // Claude's own directory, where terminal Claude's transcripts go.
            (
                "CLAUDE_CONFIG_DIR".to_owned(),
                root.path().join("claude").to_str().unwrap().to_owned(),
            ),
        ]
        .into();
        provider_env.extend(
            setup
                .env
                .iter()
                .map(|(key, value)| ((*key).to_owned(), (*value).to_owned())),
        );
        if let Some(name) = setup.replay {
            let played = root.path().join("replay");
            std::fs::create_dir_all(&played).unwrap();
            let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/replay")
                .join(name)
                .join("io.jsonl");
            let text = std::fs::read_to_string(&fixture).unwrap();
            std::fs::write(played.join("io.jsonl"), localize(&text, &work)).unwrap();
            provider_env.insert(
                provider_fakes::PLAYBACK_ENV.to_owned(),
                played.to_str().unwrap().to_owned(),
            );
        }
        if let Some(session) = setup.session {
            let private = dir.join(agent::PRIVATE);
            #[cfg_attr(not(unix), allow(unused_mut))]
            let mut builder = std::fs::DirBuilder::new();
            #[cfg(unix)]
            std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
            builder.create(&private).unwrap();
            std::fs::write(private.join("provider-session"), session).unwrap();
        }
        if setup.socketless {
            provider_env.insert(
                provider_fakes::pty::NO_MESSAGING_ENV.to_owned(),
                "1".to_owned(),
            );
        }
        let spec = AgentSpec {
            agent_id: uuid::Uuid::new_v4().as_bytes().to_vec(),
            profile_id: vec![9; 16],
            kind: setup.kind as i32,
            cwd: work.to_str().unwrap().to_owned(),
            name: "test".into(),
            parent: setup.parent.then(|| AgentParent {
                host_id: vec![1; 16],
                agent_id: vec![2; 16],
            }),
            provider_args,
            provider_env,
            provider_command: command,
            config: Some(EffectiveConfig {
                grace_ms: GRACE as u32,
                drain_ms: DRAIN as u32,
                facts_ring_bytes: setup.ring_bytes,
                journal_segment_bytes: setup.journal_bytes,
                // What the harness runs for hooks and the tool server.
                install_path: install_path(),
                ..Default::default()
            }),
            daemon_version: "test".into(),
            created_at_ms: T0,
            incarnation: 1,
            bootstrap: setup.initial_prompt.map(|text| bootstrap(b"p0", text)),
            ..Default::default()
        };
        std::fs::write(dir.join("spec.1"), spec.encode_to_vec()).unwrap();

        let clock = ManualClock::new(T0);
        let task = tokio::spawn(agent::run(dir.clone(), clock.clone()));
        let reader = journal::Reader::new(dir.join(agent::JOURNAL), 0);
        Self {
            kind: setup.kind,
            _root: root,
            dir,
            clock,
            release,
            hold,
            task: Mutex::new(Some(task)),
            reader: Mutex::new((reader, Log::default())),
        }
    }

    /// Starts the next incarnation from the newest spec with its number
    /// bumped, after this one has exited.
    pub fn resume(&self) {
        let (n, mut spec) = (1..)
            .map_while(|n| {
                std::fs::read(self.dir.join(format!("spec.{n}")))
                    .ok()
                    .map(|bytes| (n, AgentSpec::decode(bytes.as_slice()).unwrap()))
            })
            .last()
            .expect("a spec");
        spec.incarnation = n + 1;
        spec.bootstrap = None;
        std::fs::write(
            self.dir.join(format!("spec.{}", n + 1)),
            spec.encode_to_vec(),
        )
        .unwrap();
        let task = tokio::spawn(run_when_unlocked(self.dir.clone(), self.clock.clone()));
        *self.task.lock().unwrap() = Some(task);
    }

    pub fn provider_session(&self) -> String {
        std::fs::read_to_string(self.dir.join(agent::PRIVATE).join("provider-session")).unwrap()
    }

    /// Dials ctl.sock as the daemon does and reads the Hello.
    pub async fn dial(&self) -> Daemon {
        let path = self.dir.join(agent::CTL_SOCK);
        let stream = until("ctl.sock to accept a connection", || async {
            local_socket::connect(&path)
                .await
                .map_err(|error| error.to_string())
        })
        .await
        .unwrap();
        let (mut reader, writer) = tokio::io::split(stream);
        let hello = match next_frame(&mut reader).await.of {
            Some(ctl_frame::Of::Hello(hello)) => hello,
            other => panic!("the first frame is a Hello, not {other:?}"),
        };
        Daemon {
            kind: self.kind,
            reader,
            writer,
            hello,
            nudges: 0,
        }
    }

    /// Waits until the provider takes input, so a prompt runs at once rather
    /// than queueing.
    pub async fn ready(&self) {
        self.wait("the provider to take input", |log| {
            log.phase() == Some(Phase::Idle)
        })
        .await;
    }

    /// Waits until what the agent journaled satisfies `done`. A timeout
    /// shows the journal, the provider's log and the terminal.
    pub async fn wait(&self, what: &str, done: impl Fn(&Log) -> bool) {
        let waited = until(what, || {
            std::future::ready(done(&self.log()).then_some(()).ok_or("not yet"))
        })
        .await;
        if let Err(stuck) = waited {
            panic!(
                "{stuck}\njournal: {:#?}\nprovider log:\n{}\nterminal:\n{:?}",
                self.log().sequence(),
                self.provider_log(),
                self.terminal_bytes(),
            );
        }
    }

    /// Waits until `done` holds of something beside the journal.
    pub async fn until(&self, what: &str, done: impl Fn() -> bool) {
        until(what, || {
            std::future::ready(done().then_some(()).ok_or("not yet"))
        })
        .await
        .unwrap();
    }

    /// The journal's length: where a daemon's reader would stand once it
    /// had read everything.
    pub fn journal_end(&self) -> u64 {
        let mut reader = journal::Reader::new(self.dir.join(agent::JOURNAL), 0);
        reader.read_to_end().unwrap();
        reader.cursor()
    }

    /// Everything journaled so far.
    pub fn log(&self) -> Log {
        let mut reader = self.reader.lock().unwrap();
        let (journal, log) = &mut *reader;
        if let Ok(batch) = journal.read_to_end() {
            log.steps
                .extend(batch.frames.into_iter().map(|(_, step)| step));
        }
        log.clone()
    }

    pub fn finished(&self) -> bool {
        self.task
            .lock()
            .unwrap()
            .as_ref()
            .is_none_or(JoinHandle::is_finished)
    }

    /// The agent's exit cause, once it exits.
    pub async fn exit(&self) -> ExitCause {
        let task = self.task.lock().unwrap().take().expect("the agent runs");
        tokio::time::timeout(PATIENCE, task)
            .await
            .expect("the agent exits")
            .expect("the agent task does not panic")
            .expect("the agent runs without error")
    }

    /// Lets a turn waiting on the release file go on.
    pub fn release(&self) {
        std::fs::write(&self.release, b"").unwrap();
    }

    /// Lets a held provider process go.
    pub fn unhold(&self) {
        std::fs::write(&self.hold, b"").unwrap();
    }

    /// The directory is free for the next incarnation: the lock is released
    /// and the control socket is gone.
    pub fn assert_released(&self) {
        let lock = std::fs::OpenOptions::new()
            .write(true)
            .open(self.dir.join(agent::LOCK))
            .unwrap();
        // Other tests in this process spawn children at the same time, and
        // a child being spawned can hold a copy of every descriptor until it
        // execs; a lock the agent closed has been seen held for about a
        // millisecond that way. Allow a moment, never a hang.
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        while lock.try_lock().is_err() {
            assert!(
                std::time::Instant::now() < deadline,
                "the agent released its lock"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(
            !self.dir.join(agent::CTL_SOCK).exists(),
            "the agent removed ctl.sock"
        );
    }

    /// Everything the provider wrote to its terminal.
    pub fn kind(&self) -> Kind {
        self.kind
    }

    /// The provider's stderr: where a playing fake reports the first byte
    /// the agent wrote that the recording does not have.
    pub fn provider_log(&self) -> String {
        std::fs::read_to_string(self.dir.join(agent::PRIVATE).join("provider.log"))
            .unwrap_or_default()
    }

    pub fn terminal_bytes(&self) -> String {
        let pty = self.dir.join(agent::PTY);
        let mut bytes = Vec::new();
        for start in journal::segments(&pty).unwrap_or_default() {
            bytes.extend(std::fs::read(journal::segment_path(&pty, start)).unwrap());
        }
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

/// Runs the next incarnation once the last one's lock is free, as the
/// daemon waits for a dying process's lock. Every agent in a test shares
/// this process's descriptors, so a sibling test forking a provider can
/// hold a copy of the lock for the moment before its child execs.
async fn run_when_unlocked(dir: PathBuf, clock: ManualClock) -> Result<ExitCause, AgentError> {
    let started = std::time::Instant::now();
    loop {
        match agent::run(dir.clone(), clock.clone()).await {
            Err(AgentError::Locked(_)) if started.elapsed() < PATIENCE => {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            outcome => return outcome,
        }
    }
}

/// A replay fixture with its placeholders filled for this run: `{{cwd}}`
/// the agent's working directory, `{{version}}` the agent's version, and
/// `{{uuid:<input id>}}` the uuid headless Claude's message for that input
/// carries. `{{cwd}}` stands inside a JSON-RPC line the agent writes, which
/// is itself a string in the fixture's line, so the path is escaped as JSON
/// once for each.
fn localize(text: &str, work: &Path) -> String {
    let cwd = json_escaped(&json_escaped(work.to_str().unwrap()));
    let mut text = text
        .replace("{{cwd}}", &cwd)
        .replace("{{version}}", agent::VERSION);
    while let Some(start) = text.find("{{uuid:") {
        let end = start + text[start..].find("}}").expect("a closed placeholder");
        let id = &text[start + "{{uuid:".len()..end];
        let uuid = interpret::claude_sdk::client_uuid(id.as_bytes());
        text.replace_range(start..end + 2, &uuid);
    }
    text
}

/// `text` as it is written inside a JSON string, without the quotes.
fn json_escaped(text: &str) -> String {
    let quoted = serde_json::to_string(text).unwrap();
    quoted[1..quoted.len() - 1].to_owned()
}

/// Runs a test that hosts a terminal. Its runtime is shut down without
/// waiting for the terminal's reader thread, which stays blocked while any
/// process holds the terminal, so a failing test fails instead of hanging.
pub fn terminal_test<F: std::future::Future<Output = ()>>(test: F) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| runtime.block_on(test)));
    runtime.shutdown_background();
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

/// Whether a process whose command line contains `marker` is running.
pub fn running(marker: &str) -> bool {
    std::process::Command::new("pgrep")
        .args(["-f", marker])
        .stdout(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

pub fn prompt(kind: Kind, id: &[u8], text: &str) -> Input {
    let prompt = PromptInput {
        text: text.to_owned(),
        ..Default::default()
    };
    Input::new(
        id.to_vec(),
        match kind {
            Kind::Codex => wire::KindInput::Codex(CodexInput {
                of: Some(codex_input::Of::Prompt(prompt)),
            }),
            Kind::ClaudePty => wire::KindInput::ClaudePty(ClaudePtyInput {
                of: Some(claude_pty_input::Of::Prompt(prompt)),
            }),
            _ => wire::KindInput::ClaudeSdk(ClaudeSdkInput {
                of: Some(claude_sdk_input::Of::Prompt(prompt)),
            }),
        },
    )
}

/// The first prompt a spec seeds the queue with.
pub fn bootstrap(id: &[u8], text: &str) -> wire::Bootstrap {
    wire::Bootstrap {
        of: Some(wire::bootstrap::Of::Prompt(wire::PromptSubmission {
            input_id: id.to_vec(),
            prompt: Some(PromptInput {
                text: text.to_owned(),
                ..Default::default()
            }),
        })),
    }
}

async fn next_frame(reader: &mut ReadHalf<LocalStream>) -> CtlFrame {
    tokio::time::timeout(PATIENCE, agent::read_frame(reader))
        .await
        .expect("a frame arrives")
        .expect("the frame reads")
        .expect("the agent has not closed ctl.sock")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Accepted,
    Queued,
    Rejected(wire::Refusal),
}

/// A stand-in daemon on ctl.sock.
pub struct Daemon {
    kind: Kind,
    reader: ReadHalf<LocalStream>,
    writer: WriteHalf<LocalStream>,
    pub hello: AgentHello,
    /// Nudges read so far.
    pub nudges: usize,
}

impl Daemon {
    async fn send(&mut self, of: ctl_frame::Of) {
        agent::write_frame(&mut self.writer, &CtlFrame { of: Some(of) })
            .await
            .expect("ctl.sock takes the frame");
    }

    /// Sends a prompt and returns the agent's verdict on it.
    pub async fn prompt(&mut self, id: &[u8], text: &str) -> Verdict {
        self.input(prompt(self.kind, id, text)).await
    }

    /// Sends an agent message with envelope id `id`, as a parent would.
    pub async fn message(&mut self, id: &[u8], text: &str) -> Verdict {
        let envelope = wire::Envelope {
            id: id.to_vec(),
            from: Some(wire::Sender {
                value: Some(wire::sender::Value::Agent(wire::AgentSender {
                    agent_id: vec![2; 16],
                    host_id: vec![1; 16],
                    name: "parent".into(),
                })),
            }),
            text: text.to_owned(),
            ..Default::default()
        };
        self.verdict(id.to_vec(), ctl_frame::Of::AgentMessage(envelope))
            .await
    }

    /// Sends an input and waits for its verdict.
    pub async fn input(&mut self, input: Input) -> Verdict {
        let id = input.input_id.clone();
        self.verdict(id, ctl_frame::Of::Input(input)).await
    }

    /// Tells the agent it was renamed, as the daemon does.
    pub async fn rename(&mut self, name: &str) {
        self.send(ctl_frame::Of::Rename(wire::Rename {
            name: name.to_owned(),
        }))
        .await;
    }

    /// Sends a frame and waits for the verdict keyed by `id`.
    async fn verdict(&mut self, id: Vec<u8>, of: ctl_frame::Of) -> Verdict {
        self.send(of).await;
        loop {
            let frame = next_frame(&mut self.reader).await.of;
            if let Some(ctl_frame::Of::Nudge(_)) = frame {
                self.nudges += 1;
            }
            if let Some(ctl_frame::Of::Reply(reply)) = frame
                && reply.id == id
            {
                return match reply.verdict.and_then(|verdict| verdict.of) {
                    Some(send_input_response::Of::Accepted(accepted)) if accepted.queued => {
                        Verdict::Queued
                    }
                    Some(send_input_response::Of::Accepted(_)) => Verdict::Accepted,
                    Some(send_input_response::Of::Rejected(rejected)) => {
                        Verdict::Rejected(rejected.refusal())
                    }
                    None => panic!("a reply without a verdict"),
                };
            }
        }
    }

    /// Asks for the agent's part of a dump and waits for it.
    pub async fn dump(&mut self, id: &[u8]) -> wire::DumpPart {
        self.send(ctl_frame::Of::DumpRequest(wire::AgentDumpRequest {
            dump_id: id.to_vec(),
        }))
        .await;
        loop {
            match next_frame(&mut self.reader).await.of {
                Some(ctl_frame::Of::Dump(part)) if part.dump_id == id => return part,
                Some(ctl_frame::Of::Nudge(_)) => self.nudges += 1,
                _ => {}
            }
        }
    }

    pub async fn stop(&mut self, mode: StopMode) {
        self.send(ctl_frame::Of::Stop(Stop { mode: mode as i32 }))
            .await;
    }
}

/// What the agent journaled, read the way the goldens read it.
#[derive(Clone, Debug, Default)]
pub struct Log {
    pub steps: Vec<wire::Step>,
}

impl Log {
    /// The git facts of the newest snapshot that carried any.
    pub fn git(&self) -> Option<wire::Git> {
        self.steps
            .iter()
            .rev()
            .filter_map(|step| step.snapshot.as_ref())
            .find_map(|snapshot| snapshot.git.clone())
    }

    pub fn turn_ends(&self) -> usize {
        self.steps
            .iter()
            .filter(|step| step.turn_end.is_some())
            .count()
    }

    /// Terminal Claude's boundaries in order, as `KIND version keymap`.
    pub fn launches(&self) -> Vec<String> {
        self.items()
            .into_iter()
            .filter_map(|(_, item)| {
                match ClaudePtyItem::decode(item.body.as_slice()).ok()?.kind? {
                    claude_pty_item::Kind::Boundary(boundary) => Some(format!(
                        "{} {} {}",
                        boundary
                            .kind()
                            .as_str_name()
                            .trim_start_matches("BOUNDARY_KIND_"),
                        boundary.provider_version,
                        boundary.keymap
                    )),
                    _ => None,
                }
            })
            .collect()
    }

    /// Boundaries in order, as `KIND` or `KIND cause`.
    pub fn boundaries(&self) -> Vec<String> {
        self.sequence()
            .into_iter()
            .filter_map(|entry| entry.strip_prefix("boundary ").map(str::to_owned))
            .collect()
    }

    /// Boundaries and turn ends, in the order they were journaled. A
    /// boundary revised in place (headless Claude's, drawn above the first
    /// prompt before its init fills it in) counts once, where it was first
    /// written.
    pub fn sequence(&self) -> Vec<String> {
        let mut sequence = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        for step in &self.steps {
            for item in &step.items {
                if !seen.insert(item.key.clone()) {
                    continue;
                }
                if let Some(boundary) = boundary(item) {
                    sequence.push(format!("boundary {boundary}"));
                }
            }
            if step.turn_end.is_some() {
                sequence.push("turn end".to_owned());
            }
        }
        sequence
    }

    /// Every item emission, in journal order, with the index of the step
    /// that carried it.
    pub fn items(&self) -> Vec<(usize, wire::Item)> {
        self.steps
            .iter()
            .enumerate()
            .flat_map(|(index, step)| step.items.iter().map(move |item| (index, item.clone())))
            .collect()
    }

    /// Each key's item as it stands: its newest emission with every later
    /// append applied.
    pub fn full_items(&self) -> std::collections::BTreeMap<String, wire::Item> {
        let mut items = std::collections::BTreeMap::new();
        for step in &self.steps {
            for item in &step.items {
                items.insert(item.key.clone(), item.clone());
            }
            for append in &step.appends {
                if let Some(item) = items.get_mut(&append.key) {
                    let item: &mut wire::Item = item;
                    item.text.push_str(&append.text);
                    item.revision = append.revision;
                }
            }
        }
        items
    }

    pub fn has_text(&self, text: &str) -> bool {
        self.steps.iter().any(|step| {
            step.items.iter().any(|item| item.text.contains(text))
                || step.appends.iter().any(|append| append.text.contains(text))
        })
    }

    fn snapshot(&self) -> Option<&wire::Snapshot> {
        self.steps
            .iter()
            .rev()
            .find_map(|step| step.snapshot.as_ref())
    }

    /// What the newest snapshot says the agent is working on.
    pub fn working_on(&self) -> Option<String> {
        self.snapshot()?.working_on.clone()
    }

    /// The permission the newest terminal Claude snapshot reports.
    pub fn permission(&self) -> Option<String> {
        let snapshot = self.snapshot()?;
        wire::ClaudePtySnapshot::decode(snapshot.body.as_slice())
            .ok()?
            .permission
    }

    pub fn phase(&self) -> Option<Phase> {
        self.snapshot().map(wire::Snapshot::phase)
    }

    /// The keys of the asks the newest snapshot holds open.
    pub fn ask_keys(&self) -> Vec<String> {
        let Some(snapshot) = self.snapshot() else {
            return Vec::new();
        };
        let keys = |asks: Vec<wire::Ask>| asks.into_iter().map(|ask| ask.key).collect();
        match snapshot.kind() {
            Kind::ClaudeSdk => keys(
                ClaudeSdkSnapshot::decode(snapshot.body.as_slice())
                    .unwrap()
                    .asks,
            ),
            Kind::ClaudePty => keys(
                wire::ClaudePtySnapshot::decode(snapshot.body.as_slice())
                    .unwrap()
                    .asks,
            ),
            Kind::Codex => wire::CodexSnapshot::decode(snapshot.body.as_slice())
                .unwrap()
                .asks
                .into_iter()
                .map(|ask| ask.key)
                .collect(),
            other => panic!("no snapshot reader for {other:?}"),
        }
    }

    pub fn open_asks(&self) -> usize {
        let snapshot = self.snapshot().expect("a snapshot");
        match snapshot.kind() {
            Kind::ClaudeSdk => ClaudeSdkSnapshot::decode(snapshot.body.as_slice())
                .unwrap()
                .asks
                .len(),
            other => panic!("no snapshot reader for {other:?}"),
        }
    }
}

/// A boundary item's kind and cause as the log prints it.
pub fn boundary(item: &wire::Item) -> Option<String> {
    let boundary = match item.kind() {
        Kind::ClaudeSdk => match ClaudeSdkItem::decode(item.body.as_slice()).ok()?.kind? {
            claude_sdk_item::Kind::Boundary(boundary) => boundary,
            _ => return None,
        },
        Kind::ClaudePty => match ClaudePtyItem::decode(item.body.as_slice()).ok()?.kind? {
            claude_pty_item::Kind::Boundary(boundary) => boundary,
            _ => return None,
        },
        Kind::Codex => match CodexItem::decode(item.body.as_slice()).ok()?.kind? {
            codex_item::Kind::Boundary(boundary) => boundary,
            _ => return None,
        },
        _ => return None,
    };
    let kind = boundary
        .kind()
        .as_str_name()
        .trim_start_matches("BOUNDARY_KIND_")
        .to_owned();
    Some(if boundary.cause.is_empty() {
        kind
    } else {
        format!("{kind} {}", boundary.cause)
    })
}
