//! `acp-probe`: a capture proxy for any Agent Client Protocol agent, the
//! one-tool server its form scenarios configure, and `join`, which
//! sanitizes a capture into the ACP corpus.
//!
//! Run in place of an agent, it starts the real agent and carries its
//! stdin, stdout and stderr through unchanged, recording each JSON-RPC
//! line as the corpus records it (`stdin` for what the client sent,
//! `stdout` for what the agent sent) in `$ACP_CAPTURE_DIR/io.jsonl`, the
//! process's command and folder in `spawn.jsonl`, and the agent's stderr
//! in `stderr.jsonl` (kept for reading, never joined). Every line carries
//! the proxy's `process` id, so one capture folder can hold many runs.
//!
//! It knows nothing of any one agent. The real command is, in order:
//! `$ACP_REAL_PATH` with every argument the probe was given (a host that
//! swaps the agent's program for the probe and keeps its arguments); the
//! arguments after `--` (`acp-probe -- grok agent stdio`); or, when the
//! probe was started under another name (a link named `grok` on the
//! PATH), the next program of that name on the PATH past the probe.

use std::ffi::OsString;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{ExitCode, Stdio};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use qualification::recording::{acp, live};
use tokio::io::{AsyncBufReadExt as _, AsyncRead, AsyncWriteExt as _, BufReader};

/// Names the folder a capture is written to; without it the probe refuses
/// to run, so an agent is never driven unrecorded by mistake.
const CAPTURE_DIR: &str = "ACP_CAPTURE_DIR";
/// Names the real agent program, when the probe stands in for it under
/// the agent's own arguments.
const REAL_PATH: &str = "ACP_REAL_PATH";
/// The name the tool server gives itself.
const TOOL_SERVER: &str = "spec";
const USAGE: &str = "usage: acp-probe -- <agent> [args...]  (with ACP_CAPTURE_DIR set)\n       \
                     acp-probe join <capture dir> <name> <provider>";

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    match args.first().and_then(|arg| arg.to_str()) {
        Some("__tool-server") => {
            live::serve_ask_tool(TOOL_SERVER);
            return ExitCode::SUCCESS;
        }
        Some("join") => {
            return match join(&args[1..]) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("acp-probe join: {error}");
                    ExitCode::FAILURE
                }
            };
        }
        _ => {}
    }
    let (program, args) = match real_command(args) {
        Ok(command) => command,
        Err(error) => {
            eprintln!("acp-probe: {error}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let Some(dir) = std::env::var_os(CAPTURE_DIR).map(PathBuf::from) else {
        eprintln!("acp-probe: {CAPTURE_DIR} is not set\n{USAGE}");
        return ExitCode::from(2);
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("a runtime");
    match runtime.block_on(capture(program, args, dir)) {
        Ok(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
        Err(error) => {
            eprintln!("acp-probe: {error}");
            ExitCode::FAILURE
        }
    }
}

/// The program to run and its arguments, as the module comment orders them.
fn real_command(args: Vec<OsString>) -> Result<(OsString, Vec<OsString>), String> {
    if let Some(real) = std::env::var_os(REAL_PATH) {
        return Ok((real, args));
    }
    if args.first().is_some_and(|arg| arg == "--") {
        let mut rest = args.into_iter().skip(1);
        let program = rest.next().ok_or("no agent command after --")?;
        return Ok((program, rest.collect()));
    }
    let exe = std::env::current_exe().map_err(|error| error.to_string())?;
    let called = std::env::args_os()
        .next()
        .and_then(|zero| Path::new(&zero).file_name().map(OsString::from))
        .ok_or("no program name")?;
    if exe.file_name() == Some(called.as_os_str()) {
        return Err("no agent command".into());
    }
    // Standing in under the agent's name: the real one is the next of that
    // name past every folder holding this probe under it, and the agent
    // inherits that PATH, so one that runs itself again by name reaches
    // itself rather than another probe. Nothing else runs yet.
    live::stand_in_for(&called.to_string_lossy(), CAPTURE_DIR)
        .map_err(|error| error.to_string())?;
    let path = std::env::var_os("PATH").unwrap_or_default();
    let real = std::env::split_paths(&path)
        .map(|folder| folder.join(&called))
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| format!("no {} on the PATH past the probe", called.to_string_lossy()))?;
    Ok((real.into_os_string(), args))
}

#[derive(Clone)]
struct Recorder {
    dir: PathBuf,
    process: String,
    started: Instant,
}

impl Recorder {
    /// One recorded line, written in a single append so proxies sharing the
    /// folder never interleave.
    fn record(&self, file: &str, direction: &str, line: &str) {
        let row = serde_json::json!({
            "us": self.started.elapsed().as_micros() as u64,
            "dir": direction,
            "line": line,
            "transport_id": acp::CLIENT,
            "process": self.process,
        });
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join(file))
        {
            let _ = file.write_all(format!("{row}\n").as_bytes());
        }
    }
}

async fn capture(
    program: OsString,
    args: Vec<OsString>,
    dir: PathBuf,
) -> Result<i32, Box<dyn std::error::Error>> {
    std::fs::create_dir_all(&dir)?;
    let process = format!(
        "{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    );
    let started = Instant::now();
    let spawn = serde_json::json!({
        "us": 0,
        "transport_id": process,
        "process": process,
        "command": program.to_string_lossy(),
        "argv": args.iter().map(|arg| arg.to_string_lossy()).collect::<Vec<_>>(),
        "cwd": std::env::current_dir()?,
    });
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("spawn.jsonl"))?
        .write_all(format!("{spawn}\n").as_bytes())?;
    let recorder = Recorder {
        dir,
        process,
        started,
    };

    let mut child = tokio::process::Command::new(&program)
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("starting {}: {error}", program.to_string_lossy()))?;
    let mut to_agent = child.stdin.take().ok_or("no stdin")?;
    let from_agent = child.stdout.take().ok_or("no stdout")?;
    let agent_errors = child.stderr.take().ok_or("no stderr")?;

    // The client's lines, carried to the agent. When the client closes its
    // end the agent's is closed too: an ACP agent ends when its stdin does.
    let input = tokio::spawn({
        let recorder = recorder.clone();
        async move {
            let mut lines = BufReader::new(tokio::io::stdin()).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                recorder.record("io.jsonl", "stdin", &line);
                if to_agent
                    .write_all(format!("{line}\n").as_bytes())
                    .await
                    .is_err()
                {
                    break;
                }
                let _ = to_agent.flush().await;
            }
        }
    });
    let output = tokio::spawn(carry(
        from_agent,
        tokio::io::stdout(),
        recorder.clone(),
        "io.jsonl",
        "stdout",
    ));
    let errors = tokio::spawn(carry(
        agent_errors,
        tokio::io::stderr(),
        recorder,
        "stderr.jsonl",
        "stderr",
    ));
    let status = child.wait().await?;
    // The client may hold its end open until the agent is gone.
    input.abort();
    let _ = output.await;
    let _ = errors.await;
    Ok(status.code().unwrap_or(1))
}

/// Carries one of the agent's output streams to ours line by line,
/// recording each line in `file`.
async fn carry(
    from: impl AsyncRead + Unpin,
    mut to: impl tokio::io::AsyncWrite + Unpin,
    recorder: Recorder,
    file: &'static str,
    direction: &'static str,
) {
    let mut lines = BufReader::new(from).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        recorder.record(file, direction, &line);
        if to.write_all(format!("{line}\n").as_bytes()).await.is_err() {
            break;
        }
        let _ = to.flush().await;
    }
}

/// `join <capture dir> <name> <provider>`: the capture, sanitized, as
/// `crates/interpret/fixtures/acp/live/<name>`. Joining over a recording of
/// that name (to sanitize its capture again) keeps what its manifest said
/// about how it was driven.
fn join(args: &[OsString]) -> Result<(), Box<dyn std::error::Error>> {
    let [capture, name, provider] = args else {
        return Err(USAGE.into());
    };
    let name = name.to_str().ok_or("the name is not UTF-8")?;
    let provider = provider.to_str().ok_or("the provider is not UTF-8")?;
    let root = acp::corpus_root();
    let mut extra = std::fs::read(root.join(name).join("manifest.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|manifest| manifest["provider_extra"].as_object().cloned())
        .unwrap_or_default();
    let driven_by = extra
        .remove("driven_by")
        .and_then(|driven_by| driven_by.as_str().map(str::to_owned))
        .unwrap_or_else(|| "acp-probe join, by hand".to_owned());
    let destination = acp::join(Path::new(capture), &root, name, provider, &driven_by, extra)?;
    println!(
        "joined {} as {}",
        Path::new(capture).display(),
        destination.display()
    );
    Ok(())
}
