//! Play a recording through a fake binary and compare every byte.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::playback::{self, Channel, Process, transcript_files};
use crate::{Kind, PLAYBACK_ENV};

/// How long one recorded process may take to play back. Recordings replay
/// without their recorded pauses, so this only bounds a hang.
const PROCESS_DEADLINE: Duration = Duration::from_secs(60);

/// Where a fake parted from its recording.
#[derive(Debug, thiserror::Error)]
#[error("{recording} ({transport}): {detail}")]
pub struct Drift {
    pub recording: PathBuf,
    pub transport: String,
    pub detail: String,
}

/// Play every process of `recording` through the fake `binary` for `kind`
/// and require the provider's bytes on every channel the host reads.
pub async fn conformance(kind: Kind, binary: &Path, recording: &Path) -> Result<(), Drift> {
    let processes = playback::load(recording).map_err(|error| Drift {
        recording: recording.to_owned(),
        transport: String::new(),
        detail: error.to_string(),
    })?;
    if processes.is_empty() {
        return Err(Drift {
            recording: recording.to_owned(),
            transport: String::new(),
            detail: "the recording has no provider process".into(),
        });
    }
    for process in &processes {
        let drift = |detail: String| Drift {
            recording: recording.to_owned(),
            transport: process.transport.clone(),
            detail,
        };
        let played = async {
            match kind {
                Kind::ClaudeSdk => stdio(binary, recording, process).await,
                // Codex's hosts reach it on a socket where they can.
                #[cfg(unix)]
                Kind::Codex => socket(binary, recording, process).await,
                #[cfg(not(unix))]
                Kind::Codex => stdio(binary, recording, process).await,
                Kind::ClaudePty => terminal(binary, recording, process).await,
            }
        };
        tokio::time::timeout(PROCESS_DEADLINE, played)
            .await
            .map_err(|_| drift(format!("no end within {PROCESS_DEADLINE:?}")))?
            .map_err(drift)?;
    }
    Ok(())
}

async fn stdio(binary: &Path, recording: &Path, process: &Process) -> Result<(), String> {
    // The fake writes its transcript under Claude's configuration directory,
    // as Claude does; a scratch one keeps every replay out of the person's
    // own `~/.claude`.
    let config = tempfile::Builder::new()
        .prefix("fake-claude-config-")
        .tempdir()
        .map_err(|error| format!("scratch directory: {error}"))?;
    let mut child = tokio::process::Command::new(binary)
        .args(&process.argv)
        .env(
            PLAYBACK_ENV,
            playback::selector(recording, &process.transport),
        )
        .env("CLAUDE_CONFIG_DIR", config.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("spawning {}: {error}", binary.display()))?;
    let mut stdin = child.stdin.take().expect("piped stdin");
    let mut stdout = BufReader::new(child.stdout.take().expect("piped stdout")).lines();
    for (index, event) in process.events.iter().enumerate() {
        match event.channel {
            Channel::Input => {
                let mut line = event.bytes.clone();
                line.push(b'\n');
                stdin
                    .write_all(&line)
                    .await
                    .map_err(|error| format!("event {index}: writing: {error}"))?;
            }
            Channel::Output => {
                let line = stdout
                    .next_line()
                    .await
                    .map_err(|error| format!("event {index}: reading: {error}"))?;
                let Some(line) = line else {
                    let stderr = stderr_of(child).await;
                    return Err(format!("event {index}: the fake closed stdout: {stderr}"));
                };
                if line.as_bytes() != event.bytes {
                    return Err(format!(
                        "event {index}: fake wrote\n  {line}\nrecording has\n  {}",
                        String::from_utf8_lossy(&event.bytes)
                    ));
                }
            }
            other => return Err(format!("event {index}: a stdio process has no {other:?}")),
        }
    }
    drop(stdin);
    if let Some(extra) = stdout
        .next_line()
        .await
        .map_err(|error| format!("reading past the end: {error}"))?
    {
        return Err(format!("the fake wrote past the recording: {extra}"));
    }
    let status = child
        .wait()
        .await
        .map_err(|error| format!("waiting: {error}"))?;
    if !status.success() {
        return Err(format!("the fake exited {status}"));
    }
    Ok(())
}

/// Plays a Codex process served on a Unix socket, the recording's host as
/// its one client.
#[cfg(unix)]
async fn socket(binary: &Path, recording: &Path, process: &Process) -> Result<(), String> {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    let scratch = tempfile::Builder::new()
        .prefix("fake-codex-")
        .tempdir()
        .map_err(|error| format!("scratch directory: {error}"))?;
    let path = scratch.path().join("codex.sock");
    let mut args = process.argv.clone();
    let url = format!("unix://{}", path.display());
    match args.iter().position(|arg| arg == "--listen") {
        Some(at) if at + 1 < args.len() => args[at + 1] = url,
        _ => args.extend(["--listen".to_owned(), url]),
    }
    let mut child = tokio::process::Command::new(binary)
        .args(&args)
        .env(
            PLAYBACK_ENV,
            playback::selector(recording, &process.transport),
        )
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("spawning {}: {error}", binary.display()))?;
    let mut socket = match crate::clients::connect(&path, Duration::from_secs(10)).await {
        Ok(socket) => socket,
        Err(error) => {
            let _ = child.start_kill();
            let stderr = stderr_of(child).await;
            return Err(format!(
                "connecting to {}: {error}: {stderr}",
                path.display()
            ));
        }
    };
    for (index, event) in process.events.iter().enumerate() {
        match event.channel {
            Channel::Input => {
                let text = String::from_utf8_lossy(&event.bytes).into_owned();
                socket
                    .send(Message::text(text))
                    .await
                    .map_err(|error| format!("event {index}: writing: {error}"))?;
            }
            Channel::Output => {
                let text = loop {
                    match socket.next().await {
                        Some(Ok(Message::Text(text))) => break text,
                        Some(Ok(Message::Close(_))) | None => {
                            let stderr = stderr_of(child).await;
                            return Err(format!("event {index}: the fake hung up: {stderr}"));
                        }
                        Some(Ok(_)) => {}
                        Some(Err(error)) => {
                            return Err(format!("event {index}: reading: {error}"));
                        }
                    }
                };
                if text.as_bytes() != event.bytes {
                    return Err(format!(
                        "event {index}: fake wrote\n  {text}\nrecording has\n  {}",
                        String::from_utf8_lossy(&event.bytes)
                    ));
                }
            }
            other => return Err(format!("event {index}: Codex has no {other:?} channel")),
        }
    }
    socket
        .close(None)
        .await
        .map_err(|error| format!("closing: {error}"))?;
    while let Some(message) = socket.next().await {
        if let Ok(Message::Text(extra)) = message {
            return Err(format!("the fake wrote past the recording: {extra}"));
        }
    }
    let status = child
        .wait()
        .await
        .map_err(|error| format!("waiting: {error}"))?;
    if !status.success() {
        let stderr = stderr_of(child).await;
        return Err(format!("the fake exited {status}: {stderr}"));
    }
    Ok(())
}

async fn stderr_of(mut child: tokio::process::Child) -> String {
    let _ = child.wait().await;
    let mut text = String::new();
    if let Some(mut stderr) = child.stderr.take() {
        use tokio::io::AsyncReadExt;
        let _ = stderr.read_to_string(&mut text).await;
    }
    text
}

async fn terminal(binary: &Path, recording: &Path, process: &Process) -> Result<(), String> {
    let scratch = tempfile::Builder::new()
        .prefix("fake-pty-")
        .tempdir()
        .map_err(|error| format!("scratch directory: {error}"))?;
    let config = scratch.path().join("config");
    let cwd = scratch.path().join("work");
    std::fs::create_dir_all(&cwd).map_err(|error| error.to_string())?;
    // A process's working directory is the resolved path.
    let cwd = cwd.canonicalize().map_err(|error| error.to_string())?;
    let hook_log = scratch.path().join("hooks.jsonl");
    let mut hooks = serde_json::Map::new();
    for event in &process.events {
        if event.channel != Channel::Hook {
            continue;
        }
        let payload: Value = serde_json::from_slice(&event.bytes).map_err(|e| e.to_string())?;
        let name = payload["hook_event_name"].as_str().unwrap_or_default();
        hooks.insert(
            name.to_owned(),
            json!([{"hooks": [{"type": "command", "command": recorder(&hook_log)}]}]),
        );
    }
    let settings = json!({ "hooks": hooks }).to_string();
    let spawned = pty_host::spawn(pty_host::PtySpawn {
        command: binary.to_owned(),
        args: vec!["--settings".into(), settings],
        cwd: cwd.clone(),
        env: vec![
            (OsString::from("CLAUDE_CONFIG_DIR"), config.clone().into()),
            (
                OsString::from(PLAYBACK_ENV),
                playback::selector(recording, &process.transport).into(),
            ),
        ],
        env_remove: Vec::new(),
        size: pty_host::PtySize::default(),
    })
    .map_err(|error| format!("spawning {}: {error}", binary.display()))?;
    let handle = spawned.handle;
    let mut exit = spawned.exit;
    let mut output = handle.output();
    let mut expected = Vec::new();
    let mut received = Vec::new();
    for (index, event) in process.events.iter().enumerate() {
        match event.channel {
            Channel::Input => handle
                .write(&event.bytes)
                .await
                .map_err(|error| format!("event {index}: writing: {error}"))?,
            Channel::Output => {
                expected.extend_from_slice(&event.bytes);
                while received.len() < expected.len() {
                    match output.recv().await {
                        Some(chunk) => received.extend_from_slice(&chunk),
                        None => {
                            return Err(format!(
                                "event {index}: terminal closed after {} of {} bytes",
                                received.len(),
                                expected.len()
                            ));
                        }
                    }
                }
                if received[..expected.len()] != expected[..] {
                    let at = received
                        .iter()
                        .zip(&expected)
                        .position(|(a, b)| a != b)
                        .unwrap_or(expected.len());
                    return Err(format!(
                        "event {index}: terminal bytes differ at offset {at}: fake {:?}, recording {:?}",
                        String::from_utf8_lossy(&received[at..(at + 40).min(received.len())]),
                        String::from_utf8_lossy(&expected[at..(at + 40).min(expected.len())]),
                    ));
                }
            }
            Channel::Transcript | Channel::Hook => {}
            Channel::Exit => {
                return Err(format!("event {index}: corpora end with EOF, not an exit"));
            }
        }
    }
    let status = exit.wait().await;
    while let Some(chunk) = output.recv().await {
        received.extend_from_slice(&chunk);
    }
    if received.len() > expected.len() {
        return Err(format!(
            "the fake wrote {} terminal bytes past the recording",
            received.len() - expected.len()
        ));
    }
    if !status.success() {
        return Err(format!("the fake exited {status:?}"));
    }
    let hooks_written = std::fs::read(&hook_log).unwrap_or_default();
    let hooks_recorded: Vec<u8> = playback::local_hooks(&process.events, &config, &cwd)
        .into_iter()
        .flat_map(|payload| payload.into_iter().chain(*b"\n"))
        .collect();
    if hooks_written != hooks_recorded {
        return Err(format!(
            "hook payloads differ:\nfake\n{}\nrecording\n{}",
            String::from_utf8_lossy(&hooks_written),
            String::from_utf8_lossy(&hooks_recorded)
        ));
    }
    for (session, rows) in transcript_files(&process.events) {
        let path = playback::transcript_path(&config, &cwd, &session);
        let written = std::fs::read(&path)
            .map_err(|error| format!("reading transcript {}: {error}", path.display()))?;
        let recorded: Vec<u8> = rows
            .iter()
            .flat_map(|row| row.iter().copied().chain(*b"\n"))
            .collect();
        if written != recorded {
            let at = written
                .iter()
                .zip(&recorded)
                .position(|(a, b)| a != b)
                .unwrap_or(written.len().min(recorded.len()));
            return Err(format!(
                "transcript {session} differs at byte {at} ({} written, {} recorded)",
                written.len(),
                recorded.len()
            ));
        }
    }
    Ok(())
}

/// A hook command that appends its stdin and a newline to `log`.
fn recorder(log: &Path) -> String {
    let log = log.display().to_string().replace('\'', r"'\''");
    format!("cat >> '{log}' && printf '\\n' >> '{log}'")
}
