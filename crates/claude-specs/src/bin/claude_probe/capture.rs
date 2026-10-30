//! The capture proxy standing in front of Claude in a terminal, for a host
//! that runs Claude itself (amux's live lane puts this binary first on the
//! PATH as `claude`).
//!
//! Claude runs in a terminal of the proxy's own, sized as the proxy's: what
//! the host types and what Claude draws pass through unchanged and are
//! recorded as `pty` lines. The host's hook commands are wrapped so each
//! payload is recorded as a `hook` line on its way to the host, and the
//! transcript a hook names is tailed into `transcript` lines: the recording
//! has what a claude-specs terminal recording has. Every line carries the
//! proxy's `process` id, so one capture directory can hold many runs.

use std::io::{self, Read as _, Write as _};
use std::os::unix::io::AsRawFd as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use pty_host::{ProcessGroupSignal, PtySize, PtySpawn};
use tokio::io::AsyncReadExt as _;

/// The argument that runs this binary as a wrapped hook.
pub(super) const HOOK: &str = "__capture-hook";
const PROCESS_ENV: &str = "CLAUDE_CAPTURE_PROCESS";
const START_ENV: &str = "CLAUDE_CAPTURE_START_US";
/// How often the proxy looks for transcript rows.
const TAIL: Duration = Duration::from_millis(50);

fn unix_us() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|now| now.as_micros() as u64)
        .unwrap_or_default()
}

/// One recorded line, written in a single append so proxies sharing the
/// file never interleave.
fn record(dir: &Path, us: u64, direction: &str, transport: &str, line: &str, process: &str) {
    let row = serde_json::json!({
        "us": us,
        "dir": direction,
        "line": line,
        "transport_id": transport,
        "process": process,
    });
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("io.jsonl"))
    {
        let _ = file.write_all(format!("{row}\n").as_bytes());
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(4 + bytes.len() * 2);
    text.push_str("hex:");
    for byte in bytes {
        text.push_str(&format!("{byte:02x}"));
    }
    text
}

/// Wraps every hook command in `--settings` so its payload passes through
/// this binary first. Settings given as a file are rewritten into the
/// capture directory.
fn wrap_hooks(args: &mut [String], dir: &Path, process: &str) -> io::Result<()> {
    let me = std::env::current_exe()?.to_string_lossy().into_owned();
    let Some(at) = args.iter().position(|arg| arg == "--settings") else {
        return Ok(());
    };
    let Some(value) = args.get(at + 1).cloned() else {
        return Ok(());
    };
    let (mut settings, file) = match serde_json::from_str::<serde_json::Value>(&value) {
        Ok(settings) => (settings, false),
        Err(_) => (
            serde_json::from_slice(&std::fs::read(&value)?).map_err(io::Error::other)?,
            true,
        ),
    };
    if let Some(events) = settings
        .get_mut("hooks")
        .and_then(serde_json::Value::as_object_mut)
    {
        for matchers in events.values_mut().filter_map(|m| m.as_array_mut()) {
            for matcher in matchers {
                for hook in matcher
                    .get_mut("hooks")
                    .and_then(serde_json::Value::as_array_mut)
                    .into_iter()
                    .flatten()
                {
                    let Some(command) = hook.get("command").and_then(|c| c.as_str()) else {
                        continue;
                    };
                    // A hook with `args` is a program run as given; one
                    // without is a shell command line.
                    let mut wrapped = vec![HOOK.to_owned()];
                    match hook.get("args").and_then(serde_json::Value::as_array) {
                        Some(args) => {
                            wrapped.push(command.to_owned());
                            wrapped
                                .extend(args.iter().filter_map(|a| a.as_str()).map(str::to_owned));
                        }
                        None => wrapped.extend(["/bin/sh".into(), "-c".into(), command.to_owned()]),
                    }
                    hook["command"] = me.clone().into();
                    hook["args"] = wrapped.into();
                }
            }
        }
    }
    args[at + 1] = if file {
        let path = dir.join(format!("{process}.settings.json"));
        std::fs::write(&path, settings.to_string())?;
        path.to_string_lossy().into_owned()
    } else {
        settings.to_string()
    };
    Ok(())
}

/// Run as a wrapped hook: record the payload, note the transcript it
/// names for the proxy to tail, then hand the payload to the host's own
/// command, `program` run with `args`, and answer as it answers.
pub(super) fn hook(program: &str, args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let mut payload = Vec::new();
    io::stdin().read_to_end(&mut payload)?;
    if let (Some(dir), Ok(process)) = (
        std::env::var_os("CLAUDE_CAPTURE_DIR").map(PathBuf::from),
        std::env::var(PROCESS_ENV),
    ) {
        let start = std::env::var(START_ENV)
            .ok()
            .and_then(|start| start.parse::<u64>().ok())
            .unwrap_or_default();
        let text = String::from_utf8_lossy(&payload);
        record(
            &dir,
            unix_us().saturating_sub(start),
            "stdout",
            "hook",
            text.trim_end(),
            &process,
        );
        if let Some(path) = serde_json::from_slice::<serde_json::Value>(&payload)
            .ok()
            .and_then(|payload| payload["transcript_path"].as_str().map(str::to_owned))
        {
            let _ = std::fs::write(dir.join(format!("{process}.transcript")), path);
        }
    }
    let mut child = std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(&payload);
    }
    let status = child.wait()?;
    std::process::exit(status.code().unwrap_or(1));
}

/// The terminal's state before the proxy made it raw, put back on drop.
struct Raw(libc::termios);

impl Raw {
    fn enter() -> Option<Raw> {
        let fd = io::stdin().as_raw_fd();
        let mut saved = unsafe { std::mem::zeroed::<libc::termios>() };
        if unsafe { libc::tcgetattr(fd, &mut saved) } != 0 {
            return None;
        }
        let mut raw = saved;
        unsafe { libc::cfmakeraw(&mut raw) };
        unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) };
        Some(Raw(saved))
    }
}

impl Drop for Raw {
    fn drop(&mut self) {
        unsafe { libc::tcsetattr(io::stdin().as_raw_fd(), libc::TCSANOW, &self.0) };
    }
}

fn size() -> PtySize {
    let mut size = unsafe { std::mem::zeroed::<libc::winsize>() };
    if unsafe { libc::ioctl(io::stdout().as_raw_fd(), libc::TIOCGWINSZ, &mut size) } == 0
        && size.ws_row > 0
    {
        PtySize {
            rows: size.ws_row,
            cols: size.ws_col,
        }
    } else {
        PtySize::default()
    }
}

/// Whether the proxy was started in a terminal, as terminal Claude is.
pub(super) fn in_terminal() -> bool {
    unsafe { libc::isatty(io::stdin().as_raw_fd()) == 1 }
}

/// Tails the transcript the hooks named into `transcript` lines.
struct Tail {
    path: Option<PathBuf>,
    offset: u64,
    partial: Vec<u8>,
}

impl Tail {
    fn poll(&mut self, dir: &Path, process: &str, started: Instant) {
        let named = std::fs::read_to_string(dir.join(format!("{process}.transcript")))
            .ok()
            .map(PathBuf::from);
        if named.is_some() && named != self.path {
            self.path = named;
            self.offset = 0;
            self.partial.clear();
        }
        let Some(path) = &self.path else {
            return;
        };
        let Ok(mut file) = std::fs::File::open(path) else {
            return;
        };
        let mut fresh = Vec::new();
        if std::io::Seek::seek(&mut file, io::SeekFrom::Start(self.offset)).is_err()
            || file.read_to_end(&mut fresh).is_err()
        {
            return;
        }
        self.offset += fresh.len() as u64;
        self.partial.extend_from_slice(&fresh);
        while let Some(end) = self.partial.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = self.partial.drain(..=end).collect();
            let Ok(row) = serde_json::from_slice::<serde_json::Value>(&line) else {
                continue;
            };
            let line = serde_json::json!({"path": path, "row": row}).to_string();
            record(
                dir,
                started.elapsed().as_micros() as u64,
                "stdout",
                "transcript",
                &line,
                process,
            );
        }
    }
}

/// Runs `real` with `args` in a terminal of the proxy's own and records
/// what crosses; exits as Claude exits.
pub(super) async fn terminal(
    dir: PathBuf,
    real: std::ffi::OsString,
    mut args: Vec<String>,
    process: String,
) -> Result<(), Box<dyn std::error::Error>> {
    let started = Instant::now();
    let start_us = unix_us();
    wrap_hooks(&mut args, &dir, &process)?;
    let child = pty_host::spawn(PtySpawn {
        command: PathBuf::from(real),
        args,
        cwd: std::env::current_dir()?,
        env: vec![
            (PROCESS_ENV.into(), process.clone().into()),
            (START_ENV.into(), start_us.to_string().into()),
        ],
        env_remove: Vec::new(),
        size: size(),
    })?;
    let raw = Raw::enter();
    let handle = child.handle.clone();
    let mut output = handle.output();
    let mut exit = child.exit.clone();

    let input_dir = dir.clone();
    let input_process = process.clone();
    let input_handle = handle.clone();
    let input = tokio::spawn(async move {
        let mut stdin = tokio::io::stdin();
        let mut buffer = [0u8; 4096];
        loop {
            match stdin.read(&mut buffer).await {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    record(
                        &input_dir,
                        started.elapsed().as_micros() as u64,
                        "stdin",
                        "pty",
                        &hex(&buffer[..read]),
                        &input_process,
                    );
                    if input_handle.write(&buffer[..read]).await.is_err() {
                        break;
                    }
                }
            }
        }
        // The host closed the terminal: Claude goes with it.
        let _ = input_handle.signal_process_group(ProcessGroupSignal::Terminate);
    });

    let mut signals =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change())?;
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut hangup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())?;
    let mut tail = Tail {
        path: None,
        offset: 0,
        partial: Vec::new(),
    };
    let mut ticker = tokio::time::interval(TAIL);
    let mut stdout = io::stdout();
    let status = loop {
        tokio::select! {
            chunk = output.recv() => match chunk {
                Some(bytes) => {
                    record(&dir, started.elapsed().as_micros() as u64, "stdout", "pty", &hex(&bytes), &process);
                    let _ = stdout.write_all(&bytes);
                    let _ = stdout.flush();
                }
                None => break exit.wait().await,
            },
            status = exit.wait() => break status,
            _ = signals.recv() => {
                let _ = handle.resize(size());
            }
            _ = terminate.recv() => {
                let _ = handle.signal_process_group(ProcessGroupSignal::Terminate);
            }
            _ = hangup.recv() => {
                let _ = handle.signal_process_group(ProcessGroupSignal::Terminate);
            }
            _ = ticker.tick() => tail.poll(&dir, &process, started),
        }
    };
    // What Claude drew last and the rows it wrote as it went.
    while let Ok(bytes) = output.try_recv() {
        record(
            &dir,
            started.elapsed().as_micros() as u64,
            "stdout",
            "pty",
            &hex(&bytes),
            &process,
        );
        let _ = stdout.write_all(&bytes);
    }
    let _ = stdout.flush();
    tail.poll(&dir, &process, started);
    input.abort();
    drop(raw);
    std::process::exit(status.exit_code() as i32);
}
