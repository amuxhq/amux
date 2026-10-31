use std::fs::File;
use std::io::Write as _;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Instant;

use anyhow::{Context, Result};
use tokio::io::{
    AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader,
};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::client::config::CodexConfig;
use crate::client::dispatch::ServerInner;

#[derive(Clone)]
pub(crate) struct WireRecorder {
    started: Instant,
    file: Arc<StdMutex<File>>,
}

impl WireRecorder {
    pub(crate) fn new(path: &std::path::Path) -> std::io::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        Ok(Self {
            started: Instant::now(),
            file: Arc::new(StdMutex::new(file)),
        })
    }

    fn record(&self, direction: &str, line: &str) {
        let row = serde_json::json!({
            "us": self.started.elapsed().as_micros() as u64,
            "dir": direction,
            "line": line,
        });
        if let Ok(mut file) = self.file.lock() {
            let _ = writeln!(file, "{row}");
            let _ = file.flush();
        }
    }
}

// ── Process spawning ─────────────────────────────────────────────

pub(crate) struct Process {
    pub(crate) child: Child,
    pub(crate) stdin: ChildStdin,
    pub(crate) stdout: BufReader<ChildStdout>,
    pub(crate) stderr: ChildStderr,
}

pub(crate) fn process_command(config: &CodexConfig) -> tokio::process::Command {
    let program = config
        .codex_path
        .as_deref()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "codex".into());

    let mut cmd = tokio::process::Command::new(&program);

    if let Some(model) = &config.model {
        cmd.arg("--model");
        cmd.arg(model);
    }

    cmd.arg("app-server");
    cmd.arg("--listen");
    cmd.arg("stdio://");

    if let Some(ref cwd) = config.cwd {
        cmd.current_dir(cwd);
    }

    if let Some(ref env_map) = config.env {
        for (k, v) in env_map {
            cmd.env(k, v);
        }
    }

    cmd
}

pub(crate) fn spawn_process(mut cmd: tokio::process::Command) -> Result<Process> {
    cmd.stdin(std::process::Stdio::piped());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    configure_process_group(&mut cmd);

    let mut child = cmd.spawn().context("failed to spawn codex app-server")?;

    let stdin = child
        .stdin
        .take()
        .context("failed to capture codex stdin")?;
    let stdout = child
        .stdout
        .take()
        .context("failed to capture codex stdout")?;
    let stderr = child
        .stderr
        .take()
        .context("failed to capture codex stderr")?;

    Ok(Process {
        child,
        stdin,
        stdout: BufReader::new(stdout),
        stderr,
    })
}

// ── Background tasks ─────────────────────────────────────────────

/// Spawn the reader task that reads lines from stdout and dispatches them.
pub(crate) fn spawn_reader_task(
    reader: impl AsyncBufRead + Unpin + Send + 'static,
    inner: Arc<ServerInner>,
    cancel: CancellationToken,
    recorder: Option<WireRecorder>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        reader_loop(reader, inner, cancel, recorder).await;
    })
}

async fn reader_loop(
    mut reader: impl AsyncBufRead + Unpin,
    inner: Arc<ServerInner>,
    cancel: CancellationToken,
    recorder: Option<WireRecorder>,
) {
    let mut line = String::new();
    loop {
        line.clear();
        tokio::select! {
            _ = cancel.cancelled() => break,
            result = reader.read_line(&mut line) => {
                match result {
                    Ok(0) => break, // EOF
                    Ok(_) => {
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        if let Some(recorder) = &recorder {
                            recorder.record("stdout", trimmed);
                        }
                        inner.dispatch_line(trimmed).await;
                    }
                    Err(_) => break,
                }
            }
        }
    }

    // Drop all thread channel senders so TurnStreams see end-of-stream,
    // and resolve all pending requests so callers don't hang.
    inner.close_thread_channels().await;
    let pending = std::mem::take(&mut *inner.pending_requests.lock().await);
    drop(pending);
    cancel.cancel();
}

/// Spawn the writer task that drains the write channel to stdin.
pub(crate) fn spawn_writer_task(
    mut stdin: impl AsyncWrite + Unpin + Send + 'static,
    mut rx: mpsc::Receiver<Vec<u8>>,
    cancel: CancellationToken,
    recorder: Option<WireRecorder>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                msg = rx.recv() => {
                    match msg {
                        Some(data) => {
                            if let Some(recorder) = &recorder
                                && let Ok(line) = std::str::from_utf8(&data)
                            {
                                recorder.record("stdin", line);
                            }
                            if stdin.write_all(&data).await.is_err() {
                                break;
                            }
                            if stdin.write_all(b"\n").await.is_err() {
                                break;
                            }
                            if stdin.flush().await.is_err() {
                                break;
                            }
                        }
                        None => break,
                    }
                }
            }
        }
        drop(stdin);
        // A failed write cannot receive its RPC response. Wake the reader's
        // shutdown path so it resolves pending requests and closes sessions.
        cancel.cancel();
    })
}

/// Spawn a task that waits for the child process to exit.
pub(crate) fn spawn_child_waiter(
    mut child: Child,
    cancel: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        tokio::select! {
            _ = cancel.cancelled() => {
                terminate_child_group(&mut child).await;
            }
            _ = child.wait() => {}
        }
    })
}

/// Forward child stderr one bounded line at a time.
pub(crate) fn spawn_stderr_task(
    stderr: impl AsyncRead + Unpin + Send + 'static,
    process: &'static str,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        stderr_loop(stderr, process).await;
    })
}

const STDERR_CHUNK_BYTES: usize = 4096;
const STDERR_LINE_BYTES: usize = 16 * 1024;

async fn stderr_loop(mut stderr: impl AsyncRead + Unpin, process: &'static str) {
    let mut chunk = [0_u8; STDERR_CHUNK_BYTES];
    let mut line = Vec::with_capacity(STDERR_CHUNK_BYTES);
    let mut truncated = false;

    loop {
        let read = match stderr.read(&mut chunk).await {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) => {
                tracing::debug!(%error, process, "failed reading child stderr");
                return;
            }
        };

        for byte in &chunk[..read] {
            if *byte == b'\n' {
                trace_stderr_line(process, &line, truncated);
                line.clear();
                truncated = false;
            } else if line.len() < STDERR_LINE_BYTES {
                line.push(*byte);
            } else {
                truncated = true;
            }
        }
    }

    if !line.is_empty() || truncated {
        trace_stderr_line(process, &line, truncated);
    }
}

fn trace_stderr_line(process: &'static str, line: &[u8], truncated: bool) {
    let line = String::from_utf8_lossy(line);
    tracing::debug!(process, truncated, line = %line, "child stderr");
}

pub(crate) fn configure_process_group(command: &mut tokio::process::Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.as_std_mut().process_group(0);
    }
}

pub(crate) async fn terminate_child_group(child: &mut Child) {
    #[cfg(unix)]
    {
        use std::time::Duration;

        use nix::sys::signal::{Signal, killpg};
        use nix::unistd::Pid;

        if let Some(id) = child.id() {
            let group = Pid::from_raw(id as i32);
            let _ = killpg(group, Signal::SIGTERM);
            if tokio::time::timeout(Duration::from_secs(2), child.wait())
                .await
                .is_err()
            {
                let _ = killpg(group, Signal::SIGKILL);
                let _ = child.wait().await;
            }
        }
    }

    #[cfg(not(unix))]
    {
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    #[test]
    fn command_places_the_explicit_model_before_app_server() {
        let config = CodexConfig {
            codex_path: Some("custom-codex".into()),
            model: Some("gpt-5.6-luna".to_string()),
            cwd: Some("workspace".into()),
            env: Some(HashMap::from([(
                "MARKER".to_string(),
                "scenario".to_string(),
            )])),
            ..CodexConfig::default()
        };

        let command = process_command(&config);
        let command = command.as_std();
        assert_eq!(command.get_program(), "custom-codex");
        assert_eq!(
            command.get_current_dir(),
            Some(std::path::Path::new("workspace"))
        );
        assert_eq!(
            command.get_envs().collect::<Vec<_>>(),
            vec![(
                std::ffi::OsStr::new("MARKER"),
                Some(std::ffi::OsStr::new("scenario"))
            )]
        );
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            [
                "--model",
                "gpt-5.6-luna",
                "app-server",
                "--listen",
                "stdio://"
            ]
        );
    }
}
