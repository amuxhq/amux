//! The amux binary as a person and a service manager run it: a real
//! daemon (`amux daemon`, or `amux server start`) hosting real agent
//! processes on the fake providers, driven through the CLI verbs, with the
//! test reading the chat back over the profile's client socket.
//!
//! The daemon is killed mid-turn and its agents finish their turns
//! without it; a restarted daemon adopts them and ingests what they wrote
//! meanwhile; an agent that exited while no daemon was running is found
//! exited; and resuming either loses nothing of what came before.

#![cfg(unix)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::time::Duration;

use patience::{PATIENCE, holds_for, until, until_within};
use provider_fakes::{SCRIPT_ENV, Script, Step};
use tokio::io::AsyncBufReadExt as _;
use tokio::process::{Child, Command};
use tonic::transport::{Channel, Endpoint};
use wire::installation_service_client::InstallationServiceClient;
use wire::{SubscribeSessionRequest, session_event};
/// How long agents outlive their daemon. Long enough to restart a daemon
/// under them in a test, short enough to wait out when a test wants them
/// gone.
const GRACE_SECS: u64 = 10;

/// The fake providers, beside the amux binary.
fn binaries() -> &'static Path {
    provider_fakes::binaries::dir()
}

/// An installation in a temporary directory whose `claude` and `codex` are
/// the fakes, all playing one script.
struct Install {
    root: tempfile::TempDir,
    config: PathBuf,
    socket: PathBuf,
    work: PathBuf,
    daemon: Option<Child>,
    /// Started by `amux server start`: stopped with `amux server stop`.
    detached: bool,
}

impl Install {
    fn new(steps: Vec<Step>) -> Self {
        let root = tempfile::tempdir().expect("a temp dir");
        let bin = root.path().join("bin");
        let work = root.path().join("work");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&work).unwrap();
        for (name, fake) in [("claude", "fake-claude-sdk"), ("codex", "fake-codex")] {
            std::os::unix::fs::symlink(binaries().join(fake), bin.join(name)).unwrap();
        }
        let script = root.path().join("script.json");
        std::fs::write(
            &script,
            serde_json::to_string(&Script {
                steps,
                ..Script::default()
            })
            .unwrap(),
        )
        .unwrap();
        let socket = root.path().join("amux.sock");
        let config = root.path().join("config.toml");
        std::fs::write(
            &config,
            format!(
                "root = {:?}\nfront_door_socket = {:?}\nsupervisor = false\n\
                 [agent]\ngrace_secs = {GRACE_SECS}\ndrain_secs = 5\n",
                root.path().join("data").display().to_string(),
                socket.display().to_string(),
            ),
        )
        .unwrap();
        Self {
            root,
            config,
            socket,
            work,
            daemon: None,
            detached: false,
        }
    }

    /// A command for the amux binary in this installation's environment.
    fn command(&self, args: &[&str]) -> Command {
        let path = format!(
            "{}:{}",
            self.root.path().join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut command = Command::new(env!("CARGO_BIN_EXE_amux"));
        command
            .args(args)
            .env("AMUX_CONFIG", &self.config)
            .env("PATH", path)
            .env(
                SCRIPT_ENV,
                self.root
                    .path()
                    .join("script.json")
                    .to_string_lossy()
                    .as_ref(),
            )
            .env("CLAUDE_CONFIG_DIR", self.root.path().join("claude"))
            // Real mDNS would have macOS ask for Local Network access once
            // per rebuilt test binary.
            .env("AMUX_TEST_DISCOVERY_MODE", "disabled")
            .env_remove("AMUX_LOG")
            .stdin(Stdio::null())
            .kill_on_drop(true);
        command
    }

    /// Runs a CLI verb to completion.
    async fn amux(&self, args: &[&str]) -> Output {
        let run = self.command(args).output();
        tokio::time::timeout(PATIENCE, run)
            .await
            .unwrap_or_else(|_| panic!("amux {args:?} did not finish"))
            .expect("amux runs")
    }

    /// Runs a CLI verb that must succeed and returns what it printed.
    async fn ok(&self, args: &[&str]) -> String {
        let output = self.amux(args).await;
        assert!(
            output.status.success(),
            "amux {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    /// Runs the daemon in the foreground as a service manager would, and
    /// waits for its front door.
    async fn run_daemon(&mut self) {
        assert!(self.daemon.is_none(), "one daemon at a time");
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.root.path().join("daemon.stderr"))
            .unwrap();
        let child = self
            .command(&["daemon"])
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .expect("the daemon starts");
        self.daemon = Some(child);
        until("the front door answers", || async {
            self.front_door().await.map(|_| ()).ok_or("no front door")
        })
        .await
        .unwrap();
    }

    /// SIGKILLs the daemon: no clean shutdown, no chance to tell anyone.
    async fn kill_daemon(&mut self) {
        let mut daemon = self.daemon.take().expect("a daemon runs");
        daemon.kill().await.expect("the daemon is killed");
    }

    async fn front_door(&self) -> Option<Channel> {
        channel(&self.socket).await.ok()
    }

    /// The first profile's client service.
    async fn client(&self) -> wire::ClientStubs {
        let door = self.front_door().await.expect("a daemon answers");
        let profile = InstallationServiceClient::new(door)
            .list_profiles(wire::Empty {})
            .await
            .unwrap()
            .into_inner()
            .profiles
            .into_iter()
            .next()
            .expect("a profile");
        wire::client_stubs(channel(Path::new(&profile.socket_path)).await.unwrap())
    }

    fn agent_dir(&self, id: &[u8]) -> PathBuf {
        let profiles = self.root.path().join("data").join("profiles");
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
}

impl Drop for Install {
    fn drop(&mut self) {
        if let Some(daemon) = self.daemon.as_mut() {
            let _ = daemon.start_kill();
        }
        if self.detached {
            let _ = std::process::Command::new(env!("CARGO_BIN_EXE_amux"))
                .args(["server", "stop"])
                .env("AMUX_CONFIG", &self.config)
                .env("AMUX_TEST_DISCOVERY_MODE", "disabled")
                .output();
        }
    }
}

async fn channel(path: &Path) -> std::io::Result<Channel> {
    let path = path.to_owned();
    Endpoint::from_static("http://amux.test")
        .connect_with_connector(tower::service_fn(move |_| {
            let path = path.clone();
            async move {
                agent::local_socket::connect(&path)
                    .await
                    .map(hyper_util::rt::TokioIo::new)
            }
        }))
        .await
        .map_err(std::io::Error::other)
}

/// An agent's chat as a client opening it reads it: every item's key and
/// text, in order, read to CaughtUp.
async fn chat(client: &mut wire::ClientStubs, agent: &[u8]) -> Vec<(String, String)> {
    let mut stream = client
        .sessions
        .subscribe_session(SubscribeSessionRequest {
            agent_id: agent.to_vec(),
            from: Some(wire::subscribe_session_request::From::Tail(1000)),
        })
        .await
        .expect("the chat opens")
        .into_inner();
    let mut items: BTreeMap<u64, (String, String)> = BTreeMap::new();
    let mut order: BTreeMap<String, u64> = BTreeMap::new();
    while let Some(event) = tokio::time::timeout(PATIENCE, stream.message())
        .await
        .expect("the chat catches up")
        .expect("the chat streams")
    {
        match event.of {
            Some(session_event::Of::Item(item)) => {
                if let Some(old) = order.insert(item.key.clone(), item.order) {
                    items.remove(&old);
                }
                items.insert(item.order, (item.key, item.text));
            }
            Some(session_event::Of::Append(append)) => {
                if let Some(entry) = order
                    .get(&append.key)
                    .and_then(|order| items.get_mut(order))
                {
                    entry.1.push_str(&append.text);
                }
            }
            Some(session_event::Of::CaughtUp(_)) => break,
            _ => {}
        }
    }
    items.into_values().collect()
}

fn texts(chat: &[(String, String)]) -> Vec<&str> {
    chat.iter().map(|(_, text)| text.as_str()).collect()
}

fn count(chat: &[(String, String)], text: &str) -> usize {
    chat.iter().filter(|(_, said)| said == text).count()
}

/// How many turns the agent's journal says ended, read from its files.
fn turns_journaled(dir: &Path) -> usize {
    journal::Reader::new(dir.join("journal"), 0)
        .read_to_end()
        .map(|batch| {
            batch
                .frames
                .iter()
                .filter(|(_, step)| step.turn_end.is_some())
                .count()
        })
        .unwrap_or(0)
}

/// The id of the agent `amux create` just created, from its output.
fn created_id(output: &str) -> Vec<u8> {
    let start = output.find('(').expect("an id in parentheses") + 1;
    let end = output[start..].find(')').unwrap() + start;
    uuid::Uuid::parse_str(&output[start..end])
        .unwrap()
        .as_bytes()
        .to_vec()
}

fn line_of<'a>(listing: &'a str, name: &str) -> &'a str {
    listing
        .lines()
        .find(|line| line.split_whitespace().next() == Some(name))
        .unwrap_or_else(|| panic!("no {name} in:\n{listing}"))
}

/// Says something, holds the turn until the gate exists, then finishes.
fn gated_turn(gate: &Path) -> Vec<Step> {
    vec![
        Step::Text {
            chunks: vec!["started".to_owned()],
        },
        Step::WaitFor {
            path: gate.to_owned(),
        },
        Step::Text {
            chunks: vec!["finished".to_owned()],
        },
        Step::TurnEnd,
    ]
}

#[tokio::test(flavor = "multi_thread")]
async fn a_daemon_killed_mid_turn_loses_nothing() {
    let gate = tempfile::tempdir().unwrap();
    let gate = gate.path().join("gate");
    let mut install = Install::new(gated_turn(&gate));
    install.run_daemon().await;
    let work = install.work.to_string_lossy().into_owned();

    let sdk = created_id(
        &install
            .ok(&[
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
    let codex = created_id(
        &install
            .ok(&[
                "new",
                "codex",
                "--name",
                "codex",
                "--cwd",
                &work,
                "--prompt",
                "first task",
            ])
            .await,
    );
    let client = install.client().await;
    for agent in [&sdk, &codex] {
        until("both agents are mid-turn", || {
            let mut client = client.clone();
            async move {
                let chat = chat(&mut client, agent).await;
                texts(&chat)
                    .contains(&"started")
                    .then_some(())
                    .ok_or_else(|| format!("chat: {chat:?}"))
            }
        })
        .await
        .unwrap();
    }
    assert!(line_of(&install.ok(&["ls"]).await, "sdk").contains("working"));

    // The daemon dies mid-turn. Nothing tells the agents but their control
    // sockets closing; they carry on and finish their turns into their
    // journals with no daemon to read them.
    install.kill_daemon().await;
    std::fs::write(&gate, b"").unwrap();
    for agent in [&sdk, &codex] {
        let dir = install.agent_dir(agent);
        until("each agent finishes its turn with no daemon", || async {
            let turns = turns_journaled(&dir);
            (turns == 1)
                .then_some(())
                .ok_or_else(|| format!("{turns} turns journaled"))
        })
        .await
        .unwrap();
        assert!(agent_dir::locked(&dir), "the agent is still running");
    }

    // A new daemon adopts them and ingests what they wrote meanwhile.
    install.run_daemon().await;
    let mut client = install.client().await;
    let before: BTreeMap<&str, Vec<(String, String)>> = [
        ("sdk", chat(&mut client, &sdk).await),
        ("codex", chat(&mut client, &codex).await),
    ]
    .into();
    for (name, chat) in &before {
        assert_eq!(
            count(chat, "first task") + count(chat, "started") + count(chat, "finished"),
            3,
            "{name}'s whole first turn is in the store: {:?}",
            texts(chat)
        );
    }
    let listing = install.ok(&["ls"]).await;
    for name in ["sdk", "codex"] {
        assert!(
            line_of(&listing, name).contains("idle"),
            "{name} is adopted, live and idle:\n{listing}"
        );
    }

    // Stopped and resumed through the CLI, the headless agent keeps every
    // item of its first incarnation and adds its second turn.
    install.ok(&["stop", "sdk"]).await;
    assert!(line_of(&install.ok(&["ls"]).await, "sdk").contains("exited: stopped"));
    install.ok(&["resume", "sdk", "second", "task"]).await;
    until("the resumed agent finishes its second turn", || {
        let mut client = client.clone();
        let sdk = &sdk;
        async move {
            let chat = chat(&mut client, sdk).await;
            (count(&chat, "finished") == 2)
                .then_some(())
                .ok_or_else(|| format!("chat: {chat:?}"))
        }
    })
    .await
    .unwrap();
    let after = chat(&mut client, &sdk).await;
    for item in &before["sdk"] {
        assert!(after.contains(item), "{item:?} survives the resume");
    }
    assert!(texts(&after).contains(&"second task"));

    // Codex exits while no daemon runs: the next daemon finds it exited,
    // with everything it wrote, and a resume picks up from there.
    install.kill_daemon().await;
    let codex_dir = install.agent_dir(&codex);
    until_within(
        "codex drains and exits once its grace runs out",
        Duration::from_secs(GRACE_SECS) + PATIENCE,
        || async {
            (!agent_dir::locked(&codex_dir))
                .then_some(())
                .ok_or("the directory is still locked")
        },
    )
    .await
    .unwrap();
    install.run_daemon().await;
    let mut client = install.client().await;
    assert!(
        line_of(&install.ok(&["ls"]).await, "codex").contains("exited: while the daemon was away")
    );
    let found = chat(&mut client, &codex).await;
    for item in &before["codex"] {
        assert!(
            found.contains(item),
            "{item:?} is still there after the exit"
        );
    }
    install.ok(&["resume", "codex", "second", "task"]).await;
    until("the resumed codex agent finishes its second turn", || {
        let mut client = client.clone();
        let codex = &codex;
        async move {
            let chat = chat(&mut client, codex).await;
            (count(&chat, "finished") == 2)
                .then_some(())
                .ok_or_else(|| format!("chat: {chat:?}"))
        }
    })
    .await
    .unwrap();
    let after = chat(&mut client, &codex).await;
    for item in &before["codex"] {
        assert!(after.contains(item), "{item:?} survives the resume");
    }

    for name in ["sdk", "codex"] {
        install.ok(&["stop", name]).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_cli_verbs_work_against_a_started_daemon() {
    let gate = tempfile::tempdir().unwrap();
    let gate = gate.path().join("gate");
    std::fs::write(&gate, b"").unwrap();
    let mut install = Install::new(gated_turn(&gate));
    let work = install.work.to_string_lossy().into_owned();

    // With no daemon and no supervisor a client starts nothing: it says
    // the service manager owns the daemon and waits for one to appear.
    let mut waiting = install
        .command(&["ls"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut said = tokio::io::BufReader::new(waiting.stderr.take().unwrap()).lines();
    let line = tokio::time::timeout(PATIENCE, said.next_line())
        .await
        .expect("the client says why it waits")
        .unwrap()
        .expect("a line");
    assert!(
        line.contains("no supervisor") && line.contains("service manager"),
        "{line}"
    );
    // A window: a daemon that must not start leaves no mark to wait on.
    holds_for("no daemon started", Duration::from_millis(500), || async {
        install.front_door().await.is_none()
    })
    .await
    .expect("the waiting client started no daemon");

    // `amux server start` runs the startup path detached, and the waiting
    // client carries on once it answers.
    install.detached = true;
    assert!(install.ok(&["server", "start"]).await.contains("Started"));
    let listed = tokio::time::timeout(PATIENCE, waiting.wait_with_output())
        .await
        .expect("the waiting client finishes")
        .unwrap();
    assert!(listed.status.success());
    assert_eq!(String::from_utf8_lossy(&listed.stdout), "No agents.\n");
    assert!(
        install
            .ok(&["server", "start"])
            .await
            .contains("already running")
    );

    let profiles = install.ok(&["profile", "ls"]).await;
    assert!(profiles.contains("default"), "{profiles}");

    let helper = created_id(
        &install
            .ok(&[
                "new", "claude", "--in", "amux", "--name", "helper", "--cwd", &work,
            ])
            .await,
    );
    let client = install.client().await;
    assert!(
        install
            .ok(&["send", "helper", "hello", "there"])
            .await
            .contains("Sent")
    );
    until("the prompt and its answer are in the chat", || {
        let mut client = client.clone();
        let helper = &helper;
        async move {
            let chat = chat(&mut client, helper).await;
            (count(&chat, "hello there") == 1 && count(&chat, "finished") == 1)
                .then_some(())
                .ok_or_else(|| format!("chat: {chat:?}"))
        }
    })
    .await
    .unwrap();

    install.ok(&["rename", "helper", "aide"]).await;
    let listing = install.ok(&["ls"]).await;
    assert!(line_of(&listing, "aide").contains("claude"), "{listing}");

    let bundle = install.ok(&["dump", "aide", "--reason", "a test"]).await;
    let bundle = PathBuf::from(bundle.trim());
    assert!(
        bundle.join(node::MANIFEST).is_file(),
        "{}",
        bundle.display()
    );

    // A missing agent is a plain refusal, not a crash.
    let missing = install.amux(&["send", "nobody", "hi"]).await;
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("no agent named nobody"));

    // Profiles are made, renamed and removed at the front door; a new one
    // does not take over as the profile verbs act in by default.
    let helper_dir = install.agent_dir(&helper);
    install.ok(&["profile", "new", "work"]).await;
    let profiles = install.ok(&["profile", "ls"]).await;
    assert!(
        profiles.find("default") < profiles.find("work"),
        "{profiles}"
    );
    assert!(install.ok(&["ls"]).await.contains("aide"));
    install.ok(&["profile", "rename", "work", "office"]).await;
    assert_eq!(
        install.ok(&["--profile", "office", "ls"]).await,
        "No agents.\n"
    );
    install.ok(&["profile", "rm", "office"]).await;
    assert!(!install.ok(&["profile", "ls"]).await.contains("office"));

    install.ok(&["rm", "aide"]).await;
    assert_eq!(install.ok(&["ls"]).await, "No agents.\n");
    assert!(!helper_dir.exists());

    // Stopped means gone: the lock is free for the next daemon at once.
    assert!(install.ok(&["server", "stop"]).await.contains("Stopped"));
    install.detached = false;
    assert!(install.front_door().await.is_none());
    install.run_daemon().await;
}
