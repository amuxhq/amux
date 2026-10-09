//! The agent process as the daemon runs it: the real amux binary's
//! `amux agent <dir>` in its own process group, hosting a fake provider,
//! with the binary itself at the install path for hooks and tools. The test
//! stands in for the daemon on ctl.sock and for terminal clients on
//! pty.sock, and reads what the agent journaled from its directory.
//!
//! For each kind: the agent survives its daemon closing ctl.sock mid-turn
//! and finishes the turn; it serves two attached terminals at once; a
//! redialling daemon finds it caught up and stops it; it resumes from a
//! new spec as the same agent on the same provider session; and killed by
//! process group it leaves nothing running and a directory the next
//! incarnation starts from.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::OnceLock;

use agent::attach::Attached;
use agent::local_socket::{self, LocalStream};
use patience::{PATIENCE, until};
use prost::Message as _;
use provider_fakes::{SCRIPT_ENV, Script, Step};
use tokio::io::{ReadHalf, WriteHalf};
use wire::{
    AgentHello, AgentSpec, ClaudePtyInput, ClaudePtyItem, ClaudeSdkInput, ClaudeSdkItem,
    CodexInput, CodexItem, CtlFrame, EffectiveConfig, Input, Kind, Phase, PromptInput, PtyMode,
    Stop, StopMode, claude_pty_input, claude_pty_item, claude_sdk_input, claude_sdk_item,
    codex_input, codex_item, ctl_frame, send_input_response,
};
/// Long enough that no deadline fires during a test: the daemon's absence
/// is survived, never drained.
const GRACE_MS: u32 = 10 * 60 * 1000;

/// The fake providers, built once per run.
fn fakes() -> &'static Path {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    BUILT.get_or_init(|| {
        let status = provider_fakes::cargo::command()
            .args(["build", "--locked", "-p", "provider-fakes", "--bins"])
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .status()
            .expect("cargo runs");
        assert!(status.success(), "building the fake providers failed");
        Path::new(env!("CARGO_BIN_EXE_amux"))
            .parent()
            .expect("a target directory")
            .to_owned()
    })
}

struct Fixture {
    kind: Kind,
    root: tempfile::TempDir,
    dir: PathBuf,
    work: PathBuf,
    release: PathBuf,
    agent_id: Vec<u8>,
}

impl Fixture {
    fn new(kind: Kind) -> Self {
        let root = tempfile::tempdir().expect("a temp dir");
        let dir = root.path().join("agents").join("a1");
        let work = root.path().join("work");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(&work).unwrap();
        let release = root.path().join("release");
        Self {
            kind,
            dir,
            work,
            release,
            root,
            agent_id: uuid::Uuid::new_v4().as_bytes().to_vec(),
        }
    }

    /// Writes spec.<n>, whose provider plays `steps`.
    fn spec(&self, n: u32, name: &str, steps: Vec<Step>) {
        let script = serde_json::to_string(&Script {
            steps,
            ..Script::default()
        })
        .unwrap();
        let script_path = self.root.path().join(format!("script.{n}.json"));
        std::fs::write(&script_path, script).unwrap();
        let fake = fakes().join(match self.kind {
            Kind::ClaudePty => "fake-claude-pty",
            Kind::Codex => "fake-codex",
            _ => "fake-claude-sdk",
        });
        let spec = AgentSpec {
            agent_id: self.agent_id.clone(),
            profile_id: vec![9; 16],
            kind: self.kind as i32,
            cwd: self.work.to_str().unwrap().to_owned(),
            name: name.to_owned(),
            provider_command: fake.to_str().unwrap().to_owned(),
            provider_env: [
                (
                    SCRIPT_ENV.to_owned(),
                    script_path.to_str().unwrap().to_owned(),
                ),
                (
                    "CLAUDE_CONFIG_DIR".to_owned(),
                    self.root.path().join("claude").to_str().unwrap().to_owned(),
                ),
            ]
            .into(),
            config: Some(EffectiveConfig {
                grace_ms: GRACE_MS,
                drain_ms: GRACE_MS,
                install_path: env!("CARGO_BIN_EXE_amux").to_owned(),
                ..Default::default()
            }),
            daemon_version: "test".into(),
            incarnation: n,
            ..Default::default()
        };
        std::fs::write(self.dir.join(format!("spec.{n}")), spec.encode_to_vec()).unwrap();
    }

    /// Starts `amux agent <dir>` as the daemon does: in a process group of
    /// its own, detached from the test's terminal.
    fn spawn(&self, n: u32) -> Child {
        use std::os::unix::process::CommandExt as _;
        let log = std::fs::File::create(self.root.path().join(format!("agent.{n}.log"))).unwrap();
        Command::new(env!("CARGO_BIN_EXE_amux"))
            .arg("agent")
            .arg(&self.dir)
            .stdin(Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .process_group(0)
            .spawn()
            .expect("amux agent starts")
    }

    async fn dial(&self) -> Daemon {
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
        }
    }

    fn release(&self) {
        std::fs::write(&self.release, b"").unwrap();
    }

    /// Every step the agent has journaled, and where the journal ends.
    fn journal(&self) -> (Vec<wire::Step>, u64) {
        let mut reader = journal::Reader::new(self.dir.join(agent::JOURNAL), 0);
        let batch = reader.read_to_end().unwrap_or_default();
        (
            batch.frames.into_iter().map(|(_, step)| step).collect(),
            reader.cursor(),
        )
    }

    fn log(&self) -> Log {
        Log(self.journal().0)
    }

    /// Waits until what the agent journaled satisfies `done`. A timeout
    /// shows the journal's boundaries and the agent's logs.
    async fn wait(&self, what: &str, done: impl Fn(&Log) -> bool) {
        let waited = until(what, || {
            std::future::ready(done(&self.log()).then_some(()).ok_or("not yet"))
        })
        .await;
        if let Err(stuck) = waited {
            panic!(
                "{stuck}\nboundaries {:?}\nagent logs:\n{}",
                self.log().boundaries(),
                self.agent_logs()
            );
        }
    }

    fn agent_logs(&self) -> String {
        let mut logs = String::new();
        for n in 1..=4 {
            if let Ok(text) =
                std::fs::read_to_string(self.root.path().join(format!("agent.{n}.log")))
            {
                logs.push_str(&format!("-- agent.{n}.log\n{text}"));
            }
        }
        if let Ok(text) =
            std::fs::read_to_string(self.dir.join(agent::PRIVATE).join("provider.log"))
        {
            logs.push_str(&format!("-- provider.log\n{text}"));
        }
        logs
    }

    fn provider_session(&self) -> String {
        std::fs::read_to_string(self.dir.join(agent::PRIVATE).join("provider-session"))
            .unwrap()
            .trim()
            .to_owned()
    }

    /// The lock is free and ctl.sock is gone: the next incarnation may start.
    fn assert_released(&self) {
        let lock = std::fs::OpenOptions::new()
            .write(true)
            .open(self.dir.join(agent::LOCK))
            .unwrap();
        assert!(lock.try_lock().is_ok(), "the agent released its lock");
    }

    /// Whether any process mentioning this agent's directory still runs:
    /// the agent, its provider, or the provider's tool server.
    fn anything_running(&self) -> bool {
        Command::new("pgrep")
            .args(["-f", self.dir.to_str().unwrap()])
            .stdout(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }
}

async fn exits(child: &mut Child) -> std::process::ExitStatus {
    until("the agent process to exit", || {
        std::future::ready(child.try_wait().unwrap().ok_or("still running"))
    })
    .await
    .unwrap()
}

async fn next_frame(reader: &mut ReadHalf<LocalStream>) -> CtlFrame {
    tokio::time::timeout(PATIENCE, agent::read_frame(reader))
        .await
        .expect("a frame arrives")
        .expect("the frame reads")
        .expect("the agent has not closed ctl.sock")
}

/// The test standing in for the daemon on ctl.sock.
struct Daemon {
    kind: Kind,
    reader: ReadHalf<LocalStream>,
    writer: WriteHalf<LocalStream>,
    hello: AgentHello,
}

impl Daemon {
    async fn send(&mut self, of: ctl_frame::Of) {
        agent::write_frame(&mut self.writer, &CtlFrame { of: Some(of) })
            .await
            .expect("ctl.sock takes the frame");
    }

    /// Sends a prompt and asserts the agent accepted it.
    async fn prompt(&mut self, id: &[u8], text: &str) {
        let prompt = PromptInput {
            text: text.to_owned(),
            ..Default::default()
        };
        let of = match self.kind {
            Kind::Codex => wire::KindInput::Codex(CodexInput {
                of: Some(codex_input::Of::Prompt(prompt)),
            }),
            Kind::ClaudePty => wire::KindInput::ClaudePty(ClaudePtyInput {
                of: Some(claude_pty_input::Of::Prompt(prompt)),
            }),
            _ => wire::KindInput::ClaudeSdk(ClaudeSdkInput {
                of: Some(claude_sdk_input::Of::Prompt(prompt)),
            }),
        };
        self.send(ctl_frame::Of::Input(Input::new(id.to_vec(), of)))
            .await;
        loop {
            if let Some(ctl_frame::Of::Reply(reply)) = next_frame(&mut self.reader).await.of
                && reply.id == id
            {
                match reply.verdict.and_then(|verdict| verdict.of) {
                    Some(send_input_response::Of::Accepted(_)) => return,
                    other => panic!("the prompt was not accepted: {other:?}"),
                }
            }
        }
    }

    async fn stop(&mut self, mode: StopMode) {
        self.send(ctl_frame::Of::Stop(Stop { mode: mode as i32 }))
            .await;
    }
}

/// What the agent journaled.
struct Log(Vec<wire::Step>);

impl Log {
    fn phase(&self) -> Option<Phase> {
        self.0
            .iter()
            .rev()
            .find_map(|step| step.snapshot.as_ref())
            .map(wire::Snapshot::phase)
    }

    fn turn_ends(&self) -> usize {
        self.0.iter().filter(|step| step.turn_end.is_some()).count()
    }

    fn has_text(&self, text: &str) -> bool {
        self.0.iter().any(|step| {
            step.items.iter().any(|item| item.text.contains(text))
                || step.appends.iter().any(|append| append.text.contains(text))
        })
    }

    /// Boundaries in order, as `KIND` or `KIND cause`. A boundary revised in
    /// place (headless Claude's, drawn above the first prompt before its init
    /// fills it in) is one boundary, counted where it was first written.
    fn boundaries(&self) -> Vec<String> {
        let mut seen = std::collections::BTreeSet::new();
        self.0
            .iter()
            .flat_map(|step| step.items.iter())
            .filter(|item| seen.insert(item.key.clone()))
            .filter_map(boundary)
            .collect()
    }
}

fn boundary(item: &wire::Item) -> Option<String> {
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
    let cause = wire::exit_words(boundary.cause, boundary.cause_detail.as_deref());
    Some(if cause.is_empty() {
        kind
    } else {
        format!("{kind} {cause}")
    })
}

fn text(chunk: &str) -> Step {
    Step::Text {
        chunks: vec![chunk.to_owned()],
    }
}

/// Reads an attached terminal until it has drawn `text`.
async fn drawn(attached: &mut Attached, text: &str) {
    let mut seen = String::new();
    let waited = tokio::time::timeout(PATIENCE, async {
        while !seen.contains(text) {
            match attached.next().await.expect("the terminal reads") {
                Some(bytes) => seen.push_str(&String::from_utf8_lossy(&bytes)),
                None => panic!("the connection ended before {text:?}; drawn:\n{seen}"),
            }
        }
    })
    .await;
    assert!(
        waited.is_ok(),
        "timed out waiting for {text:?}; drawn:\n{seen}"
    );
}

/// Reads an attached terminal until the agent ends the connection.
async fn ended(attached: &mut Attached) {
    tokio::time::timeout(PATIENCE, async {
        while attached.next().await.expect("the terminal reads").is_some() {}
    })
    .await
    .expect("the connection ends");
}

async fn life_of_an_agent(kind: Kind) {
    let agent = Fixture::new(kind);
    agent.spec(
        1,
        "first",
        vec![
            text("working"),
            Step::WaitFor {
                path: agent.release.clone(),
            },
            text("finished with no daemon"),
            Step::TurnEnd,
        ],
    );
    let mut process = agent.spawn(1);

    // The daemon hands over a prompt, then goes away mid-turn.
    let mut daemon = agent.dial().await;
    assert_eq!(daemon.hello.agent_id, agent.agent_id);
    agent
        .wait("the provider to take input", |log| {
            log.phase() == Some(Phase::Idle)
        })
        .await;
    daemon.prompt(b"p1", "start").await;
    agent
        .wait("the turn is under way", |log| log.has_text("working"))
        .await;
    drop(daemon);
    agent.release();
    agent
        .wait("the turn finishes with no daemon", |log| {
            log.has_text("finished with no daemon") && log.turn_ends() == 1
        })
        .await;
    assert!(
        process.try_wait().unwrap().is_none(),
        "the agent outlives its daemon"
    );

    // Two terminals attach at once.
    let mut terminals = Vec::new();
    match kind {
        Kind::ClaudePty => {
            for _ in 0..2 {
                let mut attached = Attached::connect(&agent.dir).await.unwrap();
                assert_eq!(attached.mode(), PtyMode::Files);
                drawn(&mut attached, "finished with no daemon").await;
                terminals.push(attached);
            }
            terminals[1].resize(33, 111).await.unwrap();
            for attached in &mut terminals {
                drawn(attached, "size 33x111").await;
            }
        }
        Kind::Codex => {
            let thread = agent.provider_session();
            for _ in 0..2 {
                let mut attached = Attached::connect(&agent.dir).await.unwrap();
                assert_eq!(attached.mode(), PtyMode::Stream);
                drawn(&mut attached, &format!("fake codex resume {thread}")).await;
                terminals.push(attached);
            }
        }
        _ => assert!(
            !agent.dir.join(agent::PTY_SOCK).exists(),
            "headless Claude has no terminal to attach to"
        ),
    }

    // A daemon dialling back in finds it caught up, and stops it.
    let mut daemon = agent.dial().await;
    daemon.stop(StopMode::Graceful).await;
    assert!(exits(&mut process).await.success());
    for attached in &mut terminals {
        ended(attached).await;
    }
    agent.assert_released();
    assert!(!agent.dir.join(agent::CTL_SOCK).exists());
    let session = agent.provider_session();
    let (_, stopped_at) = agent.journal();

    // It resumes from a new spec as the same agent on the same session.
    agent.spec(
        2,
        "second",
        vec![text("resumed with a new spec"), Step::TurnEnd],
    );
    let mut process = agent.spawn(2);
    let mut daemon = agent.dial().await;
    assert_eq!(daemon.hello.agent_id, agent.agent_id);
    agent
        .wait("the resumed provider to take input", |_| {
            let (steps, end) = agent.journal();
            end > stopped_at && Log(steps).phase() == Some(Phase::Idle)
        })
        .await;
    daemon.prompt(b"p2", "again").await;
    agent
        .wait("the resumed turn ends", |log| {
            log.has_text("resumed with a new spec") && log.turn_ends() == 2
        })
        .await;
    assert_eq!(agent.provider_session(), session);
    assert_eq!(
        agent.log().boundaries(),
        ["STARTED", "EXITED stopped", "RESUMED"],
        "agent logs:\n{}",
        agent.agent_logs()
    );

    // Killed by process group, it leaves nothing running behind it.
    assert!(agent.anything_running(), "the search finds the live agent");
    let group = process.id() as libc::pid_t;
    // SAFETY: kill with a negative pid signals that process group; the
    // group is the agent process this test started as its leader.
    assert_eq!(unsafe { libc::kill(-group, libc::SIGKILL) }, 0);
    assert!(!exits(&mut process).await.success());
    until("every process of the killed agent to go", || {
        std::future::ready(
            (!agent.anything_running())
                .then_some(())
                .ok_or("one lives on"),
        )
    })
    .await
    .unwrap();
    agent.assert_released();

    // The killed Codex server's socket is left where the next one listens.
    let codex_socket = agent.dir.join(agent::PRIVATE).join("codex.sock");
    if kind == Kind::Codex {
        assert!(
            std::fs::symlink_metadata(&codex_socket).is_ok(),
            "the killed server left its socket"
        );
    }

    // The next incarnation records the end the killed one never wrote.
    agent.spec(3, "third", vec![]);
    let mut process = agent.spawn(3);
    let mut daemon = agent.dial().await;
    // Claude reports its resumed session only once it is prompted, so the
    // new incarnation's own boundary may not be there yet.
    agent
        .wait("the killed incarnation's end is recorded", |log| {
            log.boundaries().len() >= 4
        })
        .await;
    assert_eq!(
        agent.log().boundaries()[..4],
        [
            "STARTED",
            "EXITED stopped",
            "RESUMED",
            "EXITED ended unexpectedly",
        ]
    );
    if kind == Kind::Codex {
        // Codex resumes the thread at once: its server listens despite the
        // socket the killed one left.
        agent
            .wait("the third incarnation's Codex resumes", |log| {
                log.boundaries().get(4).map(String::as_str) == Some("RESUMED")
            })
            .await;
    }
    daemon.stop(StopMode::Kill).await;
    exits(&mut process).await;
    agent.assert_released();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_terminal_claude_agent_outlives_its_daemon_and_resumes() {
    life_of_an_agent(Kind::ClaudePty).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_headless_claude_agent_outlives_its_daemon_and_resumes() {
    life_of_an_agent(Kind::ClaudeSdk).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_codex_agent_outlives_its_daemon_and_resumes() {
    life_of_an_agent(Kind::Codex).await;
}
