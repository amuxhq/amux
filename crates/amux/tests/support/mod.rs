//! What the process tests share: a fake release channel, the signing key
//! debug builds trust, and reading an agent's chat back over its profile's
//! client socket.

pub mod desk;
#[cfg(not(windows))] // out of scope on Windows: attach
pub mod term;

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use futures_util::StreamExt as _;
use node::release::{self, Manifest, Release};
use provider_fakes::Step;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tonic::transport::{Channel as GrpcChannel, Endpoint};
use wire::installation_service_client::InstallationServiceClient;
use wire::session_event;

/// What every test daemon runs with: a scripted local network instead of
/// real mDNS, which would have macOS ask for Local Network access once per
/// rebuilt test binary.
pub const NO_DISCOVERY: (&str, &str) = ("AMUX_TEST_DISCOVERY_MODE", "disabled");

/// Config naming every built-in ACP agent by a command no machine has, so
/// a test's daemon, which runs with the machine's PATH, never starts the
/// Antigravity, OpenCode or Grok installed where the test runs: asking one
/// what it offers opens a session in it, which Antigravity keeps on disk.
pub const NO_REAL_AGENTS: &str = "[agents.antigravity]\ncommand = \"amux-test-no-agent\"\n\
     [agents.opencode]\ncommand = \"amux-test-no-agent\"\n\
     [agents.grok]\ncommand = \"amux-test-no-agent\"\n";

/// How long any one wait may take before it is a hang: twice the shared
/// default, since these tests spawn the built binary and real agent
/// processes under the supervisor, and a cold start under a loaded runner
/// takes longer than loopback traffic.
pub const PATIENCE: Duration = Duration::from_secs(60);

pub use ::release::TEST_SEED;

/// The amux binary cargo built for these tests, for a test that runs it,
/// which on Windows only Buck's test runner may do.
pub fn amux_binary() -> &'static Path {
    os::testing::require_test_runner("this test runs amux");
    Path::new(source_tree::bin!("amux"))
}

/// The fake providers, beside the amux binary.
pub fn binaries() -> &'static Path {
    provider_fakes::binaries::dir()
}

/// A fake release channel: `/stable.json` names one build, served at
/// `/amux-<version>`.
#[derive(Clone, Default)]
pub struct Channel {
    manifest: Arc<Mutex<String>>,
    artifact: Arc<Mutex<Vec<u8>>>,
    base: Arc<OnceLock<String>>,
}

impl Channel {
    pub async fn serve() -> Self {
        let channel = Self::default();
        *channel.manifest.lock().unwrap() = r#"{"targets":{}}"#.to_owned();
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        channel
            .base
            .set(format!("http://{}", listener.local_addr().unwrap()))
            .unwrap();
        let serving = channel.clone();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
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
                    let body = match request.split_whitespace().nth(1) {
                        Some("/stable.json") => {
                            serving.manifest.lock().unwrap().clone().into_bytes()
                        }
                        _ => serving.artifact.lock().unwrap().clone(),
                    };
                    let head = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = socket.write_all(head.as_bytes()).await;
                    let _ = socket.write_all(&body).await;
                });
            }
        });
        channel
    }

    /// Where `releases_url` points.
    pub fn url(&self) -> &str {
        self.base.get().unwrap()
    }

    /// Names `bytes` as `version` for this target, signed with the test key.
    pub fn publish(&self, version: &str, bytes: Vec<u8>) {
        let mut manifest = Manifest {
            channel: "stable".into(),
            rollout: None,
            targets: [(
                release::TARGET.to_owned(),
                Release {
                    version: version.to_owned(),
                    url: format!("{}/amux-{version}", self.url()),
                    sha256: release::sha256_of(&bytes),
                    size: bytes.len() as u64,
                },
            )]
            .into(),
            signature: String::new(),
        };
        manifest.signature = release::sign(&TEST_SEED, &manifest);
        *self.manifest.lock().unwrap() = serde_json::to_string(&manifest).unwrap();
        *self.artifact.lock().unwrap() = bytes;
    }
}

pub async fn grpc_channel(path: &Path) -> std::io::Result<GrpcChannel> {
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

/// A client of the first profile's node, through the front door at
/// `socket`.
pub async fn client(socket: &Path) -> client::Client {
    let door = grpc_channel(socket).await.expect("a daemon answers");
    let profile = InstallationServiceClient::new(door)
        .list_profiles(wire::ListProfilesRequest {})
        .await
        .unwrap()
        .into_inner()
        .profiles
        .into_iter()
        .next()
        .expect("a profile");
    let socket = Path::new(&profile.socket_path);
    client::Client::connect(socket, client::Surface::Terminal)
        .await
        .unwrap()
}

pub use patience::{Stuck, until_within};

/// The shared wait, with this crate's [`PATIENCE`].
pub async fn until<T, E, F>(what: &str, probe: impl FnMut() -> F) -> Result<T, Stuck>
where
    E: std::fmt::Display,
    F: std::future::Future<Output = Result<T, E>>,
{
    until_within(what, PATIENCE, probe).await
}

/// The agent's journal holds `n` ended turns, or how many it holds.
pub fn turns_ended(dir: &Path, n: usize) -> Result<(), String> {
    let turns = turns_journaled(dir);
    (turns == n)
        .then_some(())
        .ok_or_else(|| format!("{turns} turns journaled"))
}

/// The chat holds `n` items saying `text`, or what it holds.
pub fn says(chat: &[(String, String)], text: &str, n: usize) -> Result<(), String> {
    let found = count(chat, text);
    (found == n)
        .then_some(())
        .ok_or_else(|| format!("{found} items say {text:?}; chat: {chat:?}"))
}

/// An agent's chat as a client opening it reads it: every item's key and
/// text, in order, read to CaughtUp.
pub async fn chat(client: &client::Client, agent: &[u8]) -> Vec<(String, String)> {
    let mut stream = client
        .sessions()
        .subscribe(agent, 1000)
        .await
        .expect("the chat opens");
    let mut items: BTreeMap<u64, (String, String)> = BTreeMap::new();
    let mut order: BTreeMap<String, u64> = BTreeMap::new();
    while let Some(event) = tokio::time::timeout(PATIENCE, stream.next())
        .await
        .expect("the chat catches up")
    {
        match event.expect("the chat streams").of {
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

pub fn texts(chat: &[(String, String)]) -> Vec<&str> {
    chat.iter().map(|(_, text)| text.as_str()).collect()
}

pub fn count(chat: &[(String, String)], text: &str) -> usize {
    chat.iter().filter(|(_, said)| said == text).count()
}

/// How many turns the agent's journal says ended, read from its files.
pub fn turns_journaled(dir: &Path) -> usize {
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
pub fn created_id(output: &str) -> Vec<u8> {
    let start = output.find('(').expect("an id in parentheses") + 1;
    let end = output[start..].find(')').unwrap() + start;
    uuid::Uuid::parse_str(&output[start..end])
        .unwrap()
        .as_bytes()
        .to_vec()
}

pub fn line_of<'a>(listing: &'a str, name: &str) -> &'a str {
    listing
        .lines()
        .find(|line| line.split_whitespace().next() == Some(name))
        .unwrap_or_else(|| panic!("no {name} in:\n{listing}"))
}

/// Says something, holds the turn until the gate exists, then finishes.
pub fn gated_turn(gate: &Path) -> Vec<Step> {
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

/// The agent process hosting the directory `dir`: the holder of its lock,
/// which names its pid (`agent_dir::lock_holder`), on every OS.
pub fn agent_pid(dir: &Path) -> Option<u32> {
    match agent_dir::lock_holder(dir) {
        Ok(agent_dir::Holder::Process(process)) => Some(process.pid()),
        _ => None,
    }
}

/// Kills every agent process whose directory lies under `root`, found by
/// the lock it holds, and waits a while for each to go: what a failed
/// test leaves running, since agents outlive their daemon on purpose.
pub fn kill_agents_under(root: &Path) {
    let mut pending = vec![root.to_owned()];
    let mut killed = Vec::new();
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            if path.join(agent_dir::LOCK).exists() {
                if let Ok(agent_dir::Holder::Process(process)) = agent_dir::lock_holder(&path) {
                    let _ = process.kill();
                    killed.push(process);
                }
            } else {
                pending.push(path);
            }
        }
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while killed.iter().any(os::process::ProcessRef::alive) && std::time::Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Kills the agent `pid` leads and everything it started, as the daemon
/// does: `taskkill /F /T` where there is one (Windows, the process tree),
/// else SIGKILL to its process group (`pkill -KILL -g <pid>`). Says
/// whether the kill was made.
///
/// taskkill fails the whole tree when one member can't be ended, and a
/// member that ends on its own while the tree is ended, as a provider does
/// once its agent's job closes, is one it can't: on GitHub's Windows
/// runner a headless Claude agent's kill failed so (CI run 45718964949).
/// So a kill whose agent has gone was made, and the caller checks that
/// nothing of the tree is left; taskkill's words are printed either way.
pub fn kill_group(pid: u32) -> bool {
    let quiet = |command: &mut std::process::Command| {
        command
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
    };
    let agent = os::process::ProcessRef::open(pid).ok();
    match std::process::Command::new("taskkill")
        .args(["/F", "/T", "/PID", &pid.to_string()])
        .stdin(std::process::Stdio::null())
        .output()
    {
        Ok(output) if output.status.success() => true,
        Ok(output) => {
            eprintln!(
                "taskkill /F /T /PID {pid}: {}: {}{}",
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while agent.as_ref().is_some_and(os::process::ProcessRef::alive)
                && std::time::Instant::now() < deadline
            {
                std::thread::sleep(Duration::from_millis(50));
            }
            agent.is_some_and(|agent| !agent.alive())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            quiet(std::process::Command::new("pkill").args(["-KILL", "-g", &pid.to_string()]))
                .is_ok_and(|status| status.success())
        }
        Err(error) => panic!("taskkill: {error}"),
    }
}

/// Whether a process other than this one runs whose command line contains
/// `text`, asking whichever process lister this machine has: `pgrep` where
/// there is one, else the CIM process table through PowerShell (Windows).
pub fn running_mentioning(text: &str) -> bool {
    match std::process::Command::new("pgrep")
        .args(["-f", text])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
    {
        Ok(status) => status.success(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // The script names the text in single quotes, doubled within.
            let quoted = text.replace('\'', "''");
            let script = format!(
                "@(Get-CimInstance Win32_Process | Where-Object {{ $_.ProcessId -ne $PID -and \
                 $_.CommandLine -and $_.CommandLine.Contains('{quoted}') }}).Count"
            );
            let output = std::process::Command::new("powershell")
                .args(["-NoProfile", "-NonInteractive", "-Command", &script])
                .stdin(std::process::Stdio::null())
                .output()
                .expect("neither pgrep nor PowerShell runs here");
            let count = String::from_utf8_lossy(&output.stdout);
            count.trim().parse::<u32>().expect("PowerShell counts") > 0
        }
        Err(error) => panic!("pgrep: {error}"),
    }
}
