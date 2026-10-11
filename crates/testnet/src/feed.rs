//! The release feed supervised hosts take updates from: a plain HTTP server
//! on loopback serving each channel's signed manifest at
//! `/<channel>.json` and each build at `/builds/<file>`, streamed from disk
//! because a debug `amux` is well over a hundred megabytes. Debug builds of
//! the supervisor accept a plain `http://127.0.0.1` feed and trust the test
//! release key the manifests are signed with.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use node::release::{self, Manifest, Release};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::task::JoinHandle;

pub(crate) struct Feed {
    addr: SocketAddr,
    /// Where the builds it serves are kept.
    dir: PathBuf,
    manifests: Arc<Mutex<BTreeMap<String, String>>>,
    task: JoinHandle<()>,
}

/// The script a broken build is: it exits at once, so a supervisor that
/// installs it never sees it come up and rolls back after its failed
/// starts, as it would for any build that cannot run on the machine.
const BROKEN: &str = "#!/bin/sh\necho \"this build is broken on purpose\" >&2\nexit 3\n";

impl Feed {
    pub(crate) fn start(dir: PathBuf) -> std::io::Result<Self> {
        std::fs::create_dir_all(&dir)?;
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let listener = tokio::net::TcpListener::from_std(listener)?;
        let addr = listener.local_addr()?;
        let manifests: Arc<Mutex<BTreeMap<String, String>>> = Arc::default();
        let task = tokio::spawn({
            let (manifests, dir) = (manifests.clone(), dir.clone());
            async move {
                while let Ok((socket, _)) = listener.accept().await {
                    tokio::spawn(answer(socket, manifests.clone(), dir.clone()));
                }
            }
        });
        Ok(Self {
            addr,
            dir,
            manifests,
            task,
        })
    }

    pub(crate) fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Lists `build` on `channel` as `version`, signed with the test key,
    /// to the rollout given (every host when none). A broken build is a
    /// script that exits at once. Answers the path of the build listed.
    pub(crate) fn publish(
        &self,
        build: &Path,
        version: &str,
        channel: &str,
        rollout: Option<u8>,
        broken: bool,
    ) -> Result<PathBuf, String> {
        let file = format!("amux-{version}");
        let listed = self.dir.join(&file);
        if broken {
            std::fs::write(&listed, BROKEN).map_err(|error| error.to_string())?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                std::fs::set_permissions(&listed, std::fs::Permissions::from_mode(0o755))
                    .map_err(|error| error.to_string())?;
            }
        } else {
            release::restamp(build, &listed, version)
                .map_err(|error| format!("stamping the build as {version}: {error}"))?;
            // A fresh binary's first launch can stall on macOS while it is
            // assessed; the supervisor's start deadline would count that
            // stall as a failed start.
            warm(&listed);
        }
        let bytes = std::fs::read(&listed).map_err(|error| error.to_string())?;
        let mut manifest = Manifest {
            channel: channel.to_owned(),
            rollout,
            targets: [(
                release::TARGET.to_owned(),
                Release {
                    version: version.to_owned(),
                    url: format!("{}/builds/{file}", self.url()),
                    sha256: release::sha256_of(&bytes),
                    size: bytes.len() as u64,
                },
            )]
            .into(),
            signature: String::new(),
        };
        manifest.signature = release::sign(&::release::TEST_SEED, &manifest);
        let text = serde_json::to_string(&manifest).map_err(|error| error.to_string())?;
        self.manifests
            .lock()
            .unwrap()
            .insert(channel.to_owned(), text);
        Ok(listed)
    }
}

impl Drop for Feed {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Runs a fresh binary once, so its first real launch does not stall.
pub(crate) fn warm(binary: &Path) {
    let _ = std::process::Command::new(binary)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

/// One request: a manifest, a build, or not found.
async fn answer(
    mut socket: tokio::net::TcpStream,
    manifests: Arc<Mutex<BTreeMap<String, String>>>,
    dir: PathBuf,
) {
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
        && !name.contains('/')
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
    let manifest = path
        .strip_prefix('/')
        .and_then(|name| name.strip_suffix(".json"))
        .and_then(|channel| manifests.lock().unwrap().get(channel).cloned());
    // A channel nothing was published on yet is an empty manifest, as a
    // fresh feed serves.
    let (status, body) = match manifest {
        Some(body) => ("200 OK", body),
        None if path.ends_with(".json") => ("200 OK", r#"{"targets":{}}"#.to_owned()),
        None => ("404 Not Found", String::new()),
    };
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = socket.write_all(head.as_bytes()).await;
    let _ = socket.write_all(body.as_bytes()).await;
}
