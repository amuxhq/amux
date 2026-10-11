//! The capture proxy standing in front of Codex, for a host that runs
//! Codex itself (amux's live lane puts this binary first on the PATH as
//! `codex`, with `CODEX_CAPTURE_PROXY` set).
//!
//! `codex app-server` runs as asked, and every message between it and its
//! clients is recorded as a codex-specs recording records it: `stdin` for
//! what a client sent, `stdout` for what the server sent. Over stdio the
//! proxy carries the lines itself. On a socket the server listens on one of
//! the proxy's own and the proxy listens where the host asked, carrying each
//! client's WebSocket messages across; the first client is recorded as
//! `amux` and any later one (Codex's own app attached) as `other`. Every
//! other invocation runs Codex unrecorded. Every line carries the proxy's
//! `process` id, so one capture directory can hold many runs.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures_util::{SinkExt as _, StreamExt as _};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};

pub(super) const PROXY_ENV: &str = "CODEX_CAPTURE_PROXY";
/// How long a client may wait for the server's own socket to answer.
const UPSTREAM: Duration = Duration::from_secs(60);

#[derive(Clone)]
struct Recorder {
    dir: PathBuf,
    process: String,
    started: Instant,
}

impl Recorder {
    /// One recorded line, written in a single append so proxies sharing the
    /// file never interleave.
    fn record(&self, direction: &str, transport: &str, line: &str) {
        let row = serde_json::json!({
            "us": self.started.elapsed().as_micros() as u64,
            "dir": direction,
            "line": line,
            "transport_id": transport,
            "process": self.process,
        });
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join("io.jsonl"))
        {
            let _ = file.write_all(format!("{row}\n").as_bytes());
        }
    }
}

pub(super) async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let real = std::env::var_os("CODEX_REAL_PATH").unwrap_or_else(|| "codex".into());
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if !args.iter().any(|arg| arg == "app-server") {
        use std::os::unix::process::CommandExt as _;
        return Err(std::process::Command::new(real).args(&args).exec().into());
    }
    let dir =
        PathBuf::from(std::env::var_os("CODEX_CAPTURE_DIR").ok_or("CODEX_CAPTURE_DIR is not set")?);
    std::fs::create_dir_all(&dir)?;
    let process = format!(
        "{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    );
    let spawn = serde_json::json!({
        "us": 0,
        "transport_id": process,
        "process": process,
        "argv": args,
        "cwd": std::env::current_dir()?,
    });
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("spawn.jsonl"))?
        .write_all(format!("{spawn}\n").as_bytes())?;
    let recorder = Recorder {
        dir: dir.clone(),
        process: process.clone(),
        started: Instant::now(),
    };
    let listen = args
        .iter()
        .position(|arg| arg == "--listen")
        .map(|at| at + 1)
        .filter(|at| args.get(*at).is_some_and(|url| url.starts_with("unix://")));
    let status = match listen {
        Some(at) => {
            let asked = PathBuf::from(&args[at]["unix://".len()..]);
            // A Unix socket's path has a short limit (104 bytes on macOS),
            // which a capture directory deep in a run's folders overruns,
            // so the server's own socket goes in the temporary directory.
            let own = std::env::temp_dir().join(format!("codex-probe-{}.sock", std::process::id()));
            args[at] = format!("unix://{}", own.display());
            socket(real, args, &asked, own, recorder).await?
        }
        None => stdio(real, args, recorder).await?,
    };
    std::process::exit(status.code().unwrap_or(1));
}

async fn stdio(
    real: std::ffi::OsString,
    args: Vec<String>,
    recorder: Recorder,
) -> Result<std::process::ExitStatus, Box<dyn std::error::Error>> {
    let mut child = tokio::process::Command::new(real)
        .args(&args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()?;
    let mut to_server = child.stdin.take().ok_or("no stdin")?;
    let from_server = child.stdout.take().ok_or("no stdout")?;
    let input = tokio::spawn({
        let recorder = recorder.clone();
        async move {
            let mut lines = BufReader::new(tokio::io::stdin()).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                recorder.record("stdin", "amux", &line);
                if to_server
                    .write_all(format!("{line}\n").as_bytes())
                    .await
                    .is_err()
                {
                    break;
                }
            }
        }
    });
    let output = tokio::spawn(async move {
        let mut lines = BufReader::new(from_server).lines();
        let mut stdout = tokio::io::stdout();
        while let Ok(Some(line)) = lines.next_line().await {
            recorder.record("stdout", "amux", &line);
            if stdout
                .write_all(format!("{line}\n").as_bytes())
                .await
                .is_err()
            {
                break;
            }
            let _ = stdout.flush().await;
        }
    });
    let status = child.wait().await?;
    // The host may hold its end open until the server is gone.
    input.abort();
    let _ = output.await;
    Ok(status)
}

async fn socket(
    real: std::ffi::OsString,
    args: Vec<String>,
    asked: &Path,
    own: PathBuf,
    recorder: Recorder,
) -> Result<std::process::ExitStatus, Box<dyn std::error::Error>> {
    let _ = std::fs::remove_file(&own);
    // In the proxy's process group, so whatever ends the group ends both.
    let mut child = tokio::process::Command::new(real)
        .args(&args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .spawn()?;
    let _ = std::fs::remove_file(asked);
    let listener = UnixListener::bind(asked)?;
    let accept = tokio::spawn(async move {
        let mut clients = 0usize;
        while let Ok((client, _)) = listener.accept().await {
            let transport = if clients == 0 { "amux" } else { "other" };
            clients += 1;
            tokio::spawn(carry(client, own.clone(), transport, recorder.clone()));
        }
    });
    let status = child.wait().await?;
    accept.abort();
    Ok(status)
}

/// Carries one client's messages to the server and back, recording each.
async fn carry(client: UnixStream, own: PathBuf, transport: &'static str, recorder: Recorder) {
    let path = Arc::new(Mutex::new(String::from("/")));
    let asked = path.clone();
    // The handshake's own callback type; its error is the library's.
    #[allow(clippy::result_large_err)]
    let note_path = move |request: &Request, response: Response| {
        *asked.lock().unwrap() = request.uri().path().to_owned();
        Ok(response)
    };
    let Ok(client) = tokio_tungstenite::accept_hdr_async(client, note_path).await else {
        return;
    };
    let deadline = Instant::now() + UPSTREAM;
    let upstream = loop {
        match UnixStream::connect(&own).await {
            Ok(stream) => break stream,
            Err(_) if Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(50)).await
            }
            Err(_) => return,
        }
    };
    let url = format!("ws://localhost{}", path.lock().unwrap());
    let Ok((server, _)) = tokio_tungstenite::client_async(url, upstream).await else {
        return;
    };
    let (mut to_client, mut from_client) = client.split();
    let (mut to_server, mut from_server) = server.split();
    let up = {
        let recorder = recorder.clone();
        async move {
            while let Some(Ok(message)) = from_client.next().await {
                if let Message::Text(text) = &message {
                    recorder.record("stdin", transport, text.as_str());
                }
                let close = message.is_close();
                if to_server.send(message).await.is_err() || close {
                    break;
                }
            }
            let _ = to_server.close().await;
        }
    };
    let down = async move {
        while let Some(Ok(message)) = from_server.next().await {
            if let Message::Text(text) = &message {
                recorder.record("stdout", transport, text.as_str());
            }
            let close = message.is_close();
            if to_client.send(message).await.is_err() || close {
                break;
            }
        }
        let _ = to_client.close().await;
    };
    tokio::join!(up, down);
}
