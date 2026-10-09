//! What the process tests share: a fake release channel, the signing key
//! debug builds trust, and reading an agent's chat back over its profile's
//! client socket.

#![allow(dead_code, unused_imports)]

pub mod desk;
pub mod term;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use node::release::{self, Manifest, Release};
use provider_fakes::Step;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tonic::transport::{Channel as GrpcChannel, Endpoint};
use wire::client_service_client::ClientServiceClient;
use wire::profile_service_client::ProfileServiceClient;
use wire::{Empty, SubscribeRequest, session_event};

/// What every test daemon runs with: a scripted local network instead of
/// real mDNS, which would have macOS ask for Local Network access once per
/// rebuilt test binary.
pub const NO_DISCOVERY: (&str, &str) = ("AMUX_TEST_DISCOVERY_MODE", "disabled");

/// How long any one wait may take before it is a hang: twice the shared
/// default, since these tests spawn the built binary and real agent
/// processes under the supervisor, and a cold start under a loaded runner
/// takes longer than loopback traffic.
pub const PATIENCE: Duration = Duration::from_secs(60);

/// The private half of the test release key debug builds trust.
pub const TEST_SEED: [u8; 32] = [
    0xfe, 0x91, 0xb0, 0xb9, 0x1e, 0xa7, 0x94, 0x55, 0xea, 0x7c, 0xb1, 0xa7, 0x83, 0xec, 0x33, 0x47,
    0x28, 0x61, 0x70, 0x17, 0x17, 0xc9, 0x9b, 0x2a, 0xaa, 0x45, 0xe9, 0x43, 0xbb, 0xd6, 0x48, 0x12,
];

/// The amux binary cargo built for these tests.
pub fn amux_binary() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_amux"))
}

/// The fake providers, built once per run, beside the amux binary.
pub fn binaries() -> &'static Path {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    BUILT.get_or_init(|| {
        let status = provider_fakes::cargo::command()
            .args(["build", "--locked", "-p", "provider-fakes", "--bins"])
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .status()
            .expect("cargo runs");
        assert!(status.success(), "building the fake providers failed");
        amux_binary()
            .parent()
            .expect("a target directory")
            .to_owned()
    })
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

/// The first profile's client service, through the front door at `socket`.
pub async fn client(socket: &Path) -> ClientServiceClient<GrpcChannel> {
    let door = grpc_channel(socket).await.expect("a daemon answers");
    let profile = ProfileServiceClient::new(door)
        .list_profiles(Empty {})
        .await
        .unwrap()
        .into_inner()
        .profiles
        .into_iter()
        .next()
        .expect("a profile");
    wire::client_service_client(grpc_channel(Path::new(&profile.socket_path)).await.unwrap())
}

pub use patience::{Stuck, holds_for, until_within};

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
pub async fn chat(
    client: &mut ClientServiceClient<GrpcChannel>,
    agent: &[u8],
) -> Vec<(String, String)> {
    let mut stream = client
        .subscribe(SubscribeRequest {
            agent_id: agent.to_vec(),
            tail: 1000,
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
