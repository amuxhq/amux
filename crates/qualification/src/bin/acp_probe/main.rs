//! `acp-probe`: a capture proxy for any Agent Client Protocol agent, the
//! one-tool server its form scenarios configure, and `join`, which
//! sanitizes a capture into the ACP corpus.
//!
//! Run in place of an agent, it starts the real agent and carries its
//! stdin, stdout and stderr through byte for byte, recording each
//! newline-framed line (as text, invalid UTF-8 replaced, a CR before the
//! newline dropped) as the corpus records it (`stdin` for what the client sent,
//! `stdout` for what the agent sent) in `$ACP_CAPTURE_DIR/io.jsonl`, the
//! process's command and folder in `spawn.jsonl`, and the agent's stderr
//! in `stderr.jsonl` (kept for reading, never joined). Every line carries
//! the proxy's `process` id, so one capture folder can hold many runs.
//! The files are opened before the agent starts, and a line that cannot be
//! recorded ends the run with a failure. When the agent exits the probe
//! exits with its status, whether or not the client has closed its stdin.
//!
//! It knows nothing of any one agent. The real command is, in order:
//! `$ACP_REAL_PATH` with every argument the probe was given (a host that
//! swaps the agent's program for the probe and keeps its arguments); the
//! arguments after `--` (`acp-probe -- grok agent stdio`); or, when the
//! probe was started under another name (a link named `grok` on the
//! PATH), the next program of that name on the PATH past the probe.

use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{ExitCode, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use qualification::recording::{acp, live};

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
    // Exited explicitly, with the agent's status: the thread reading the
    // client's stdin may still be blocked in a read.
    let code = match capture(program, args, dir) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("acp-probe: {error}");
            1
        }
    };
    let _ = std::io::stdout().flush();
    std::process::exit(code)
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

/// How often the probe looks at whether the agent has exited.
const POLL: Duration = Duration::from_millis(20);
/// How long the agent's output may keep flowing after it exits (a process
/// it left behind holding its stdout) before the probe stops waiting.
const DRAIN: Duration = Duration::from_secs(2);

/// One of the capture's files, opened before the agent starts so a capture
/// that cannot be written never runs the agent.
#[derive(Clone)]
struct Log(Arc<Mutex<File>>);

impl Log {
    fn open(path: &Path) -> std::io::Result<Log> {
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Log(Arc::new(Mutex::new(file))))
    }

    /// Appends one row in a single write, so probes sharing the folder
    /// never interleave.
    fn append(&self, row: &serde_json::Value) -> std::io::Result<()> {
        let mut file = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        file.write_all(format!("{row}\n").as_bytes())
    }
}

#[derive(Clone)]
struct Recorder {
    log: Log,
    direction: &'static str,
    process: Arc<str>,
    started: Instant,
    /// The first failure to record, which ends the run: a capture that
    /// silently lost frames would pass for a complete one.
    failed: Arc<Mutex<Option<String>>>,
}

impl Recorder {
    fn record(&self, line: &[u8]) -> bool {
        // The record frames lines; a CR before the newline is the line
        // ending's, not the message's.
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        let row = serde_json::json!({
            "us": self.started.elapsed().as_micros() as u64,
            "dir": self.direction,
            "line": String::from_utf8_lossy(line),
            "transport_id": acp::CLIENT,
            "process": &*self.process,
        });
        match self.log.append(&row) {
            Ok(()) => true,
            Err(error) => {
                let mut failed = self
                    .failed
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                failed
                    .get_or_insert_with(|| format!("recording a {} line: {error}", self.direction));
                false
            }
        }
    }
}

fn capture(
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
    let open =
        |name: &str| Log::open(&dir.join(name)).map_err(|error| format!("opening {name}: {error}"));
    let (spawns, io, errors) = (
        open("spawn.jsonl")?,
        open("io.jsonl")?,
        open("stderr.jsonl")?,
    );
    spawns.append(&serde_json::json!({
        "us": 0,
        "transport_id": process,
        "process": process,
        "command": program.to_string_lossy(),
        "argv": args.iter().map(|arg| arg.to_string_lossy()).collect::<Vec<_>>(),
        "cwd": std::env::current_dir()?,
    }))?;
    let failed = Arc::new(Mutex::new(None));
    let recorder = |log: &Log, direction| Recorder {
        log: log.clone(),
        direction,
        process: process.as_str().into(),
        started,
        failed: failed.clone(),
    };

    let mut child = std::process::Command::new(&program)
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("starting {}: {error}", program.to_string_lossy()))?;
    let to_agent = child.stdin.take().ok_or("no stdin")?;
    let from_agent = child.stdout.take().ok_or("no stdout")?;
    let agent_errors = child.stderr.take().ok_or("no stderr")?;

    // Plain threads, each carrying one stream as it comes. The one reading
    // the client is never joined: it may sit in a read of a stdin the
    // client holds open after the agent has gone, and the probe still has
    // to exit then, with the agent's status.
    {
        let recorder = recorder(&io, "stdin");
        std::thread::spawn(move || carry(std::io::stdin().lock(), to_agent, &recorder));
    }
    let output = {
        let recorder = recorder(&io, "stdout");
        std::thread::spawn(move || carry(from_agent, std::io::stdout().lock(), &recorder))
    };
    let errors = {
        let recorder = recorder(&errors, "stderr");
        std::thread::spawn(move || carry(agent_errors, std::io::stderr().lock(), &recorder))
    };
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if failed.lock().unwrap_or_else(|p| p.into_inner()).is_some() {
            let _ = child.kill();
            break child.wait()?;
        }
        std::thread::sleep(POLL);
    };
    let drained = Instant::now() + DRAIN;
    while !(output.is_finished() && errors.is_finished()) && Instant::now() < drained {
        std::thread::sleep(POLL);
    }
    if let Some(error) = failed.lock().unwrap_or_else(|p| p.into_inner()).take() {
        return Err(error.into());
    }
    Ok(code_of(status))
}

/// The exit code the probe passes on: the agent's, or for an agent ended by
/// a signal, the shell's 128 plus the signal.
fn code_of(status: std::process::ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt as _;
        if let Some(signal) = status.signal() {
            return 128 + signal;
        }
    }
    1
}

/// Carries one stream byte for byte, as it arrives, and records it framed
/// on newlines. A final line without one is recorded when the stream ends.
fn carry(mut from: impl Read, mut to: impl Write, recorder: &Recorder) {
    let mut buffer = [0u8; 64 * 1024];
    let mut pending = Vec::new();
    loop {
        let read = match from.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(read) => read,
        };
        let chunk = &buffer[..read];
        let mut rest = chunk;
        while let Some(at) = rest.iter().position(|byte| *byte == b'\n') {
            pending.extend_from_slice(&rest[..at]);
            if !recorder.record(&pending) {
                return;
            }
            pending.clear();
            rest = &rest[at + 1..];
        }
        pending.extend_from_slice(rest);
        if to.write_all(chunk).and_then(|()| to.flush()).is_err() {
            break;
        }
    }
    if !pending.is_empty() {
        recorder.record(&pending);
    }
    // Dropping `to` closes it: the agent's stdin, once the client's ends.
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
