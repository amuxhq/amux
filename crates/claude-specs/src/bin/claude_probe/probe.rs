use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use redaction::Redaction;
use replay_support::live::{self, Captured, owner_home};
use semver::Version;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

#[tokio::main]
pub(super) async fn main() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(unix)]
    if std::env::args().nth(1).as_deref() == Some(super::capture::HOOK) {
        let mut command = std::env::args().skip(2);
        let program = command.next().ok_or("a wrapped hook names its command")?;
        return super::capture::hook(&program, &command.collect::<Vec<_>>());
    }
    // The tool server the live lane's form scenarios hand Claude; it runs
    // under the capture proxy's environment, so it is told apart first.
    if std::env::args().nth(1).as_deref() == Some("__tool-server") {
        live::serve_ask_tool("spec-external");
        return Ok(());
    }
    if std::env::var_os("CLAUDE_CAPTURE_PROXY").is_some() {
        return run_capture_proxy().await;
    }
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    match args.as_slice() {
        [command, capture, name] if command == "join" => join(Path::new(capture), name),
        _ => Err(usage().into()),
    }
}

fn usage() -> &'static str {
    "usage: claude-probe join <capture dir> <name>"
}

/// A `get_settings` answer repeats the recording machine's whole settings
/// (its environment, plugins, notes about its owner); only what Claude
/// applied, the model and effort, is kept.
fn scrub_settings(events: &mut [replay_support::IoEvent]) -> io::Result<()> {
    for event in events {
        if event.direction != replay_support::IoDirection::Read
            || !event.line.contains("\"applied\"")
        {
            continue;
        }
        let mut frame: serde_json::Value =
            serde_json::from_str(&event.line).map_err(io::Error::other)?;
        let Some(body) = frame.pointer_mut("/response/response") else {
            continue;
        };
        if body.get("applied").is_none() || body.get("effective").is_none() {
            continue;
        }
        let applied = body["applied"].take();
        *body = serde_json::json!({ "applied": applied });
        event.line = serde_json::to_string(&frame).map_err(io::Error::other)?;
    }
    Ok(())
}

fn stabilize_pty_transcript_paths(events: &mut [replay_support::IoEvent]) -> io::Result<()> {
    let mut labels = BTreeMap::<String, String>::new();
    for event in events {
        let path_key = match event.transport_id.as_deref() {
            Some("hook") => "transcript_path",
            Some("transcript") => "path",
            _ => continue,
        };
        let mut frame: serde_json::Value =
            serde_json::from_str(&event.line).map_err(io::Error::other)?;
        let path = frame.get_mut(path_key);
        if let Some(serde_json::Value::String(path)) = path {
            let next = labels.len() + 1;
            let label = labels
                .entry(path.clone())
                .or_insert_with(|| format!("<TRANSCRIPT_{next}>"));
            *path = label.clone();
        }
        event.line = serde_json::to_string(&frame).map_err(io::Error::other)?;
    }
    Ok(())
}

/// Join one live qualification capture to the live corpus as
/// `fixtures/live/{sdk,pty}/<name>`. The capture is a folder the live lane
/// wrote: one provider process's `io.jsonl` and `spawn.jsonl`, and the
/// `fixture.json` that names its format. It is sanitized as a recording is,
/// and its manifest records the Claude version and model the capture shows.
fn join(capture: &Path, name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let fixture: serde_json::Value =
        serde_json::from_slice(&std::fs::read(capture.join("fixture.json"))?)?;
    let pty = match fixture
        .pointer("/recording/format")
        .and_then(|f| f.as_str())
    {
        Some("claude_sdk_io") => false,
        Some("claude_pty_io") => true,
        other => {
            return Err(format!("{}: not a Claude capture ({other:?})", capture.display()).into());
        }
    };
    let (mut spawns, mut events) = live::read_capture(capture)?;
    // A session whose every request failed writes no row naming its model:
    // the one it was launched on stands in.
    let launched = spawns.iter().find_map(|row| {
        let argv = row["argv"].as_array()?;
        let at = argv.iter().position(|arg| arg == "--model")?;
        argv.get(at + 1)?.as_str().map(str::to_owned)
    });
    if pty {
        // A terminal capture is the terminal session. The headless Claude
        // the agent asks for the host's catalogue runs beside it under its
        // own transport, with the owner's settings: its answer lists their
        // plugins, skills and agents, so it stays out.
        let beside: Vec<String> = events
            .iter()
            .filter_map(|event| event.transport_id.clone())
            .filter(|id| !matches!(id.as_str(), "pty" | "transcript" | "hook"))
            .collect();
        events.retain(|event| {
            event
                .transport_id
                .as_ref()
                .is_none_or(|id| !beside.contains(id))
        });
        spawns.retain(|row| {
            row["transport_id"]
                .as_str()
                .is_none_or(|id| !beside.iter().any(|beside| beside == id))
        });
        stabilize_pty_transcript_paths(&mut events)?;
        claude_specs::scrub::transcript(&mut events)?;
    } else {
        scrub_settings(&mut events)?;
    }
    let rules = Redaction {
        home: owner_home(),
        extra_paths: live::live_scratch(&spawns),
        secret_env: live::secret_values(SECRETS),
        hostname: None,
        user: None,
        extra_personal_identifiers: Vec::new(),
        personal_identifier_keys: Vec::new(),
    };
    // A terminal process's channels are all `pty`; a stdio process is
    // named by its transport.
    let spawns = spawns
        .into_iter()
        .map(|mut row| {
            serde_json::json!({
                "transport_id": if pty { "pty".into() } else { row["transport_id"].take() },
                "argv": row["argv"].take(),
                "cwd": row["cwd"].take(),
            })
        })
        .collect();
    let root = claude_specs::live_fixtures_root().join(if pty { "pty" } else { "sdk" });
    let destination = live::join(&root, name, "claude", events, spawns, &rules, |events| {
        let facts = captured_facts(events, pty, launched)?;
        let mut extra = serde_json::Map::new();
        if facts.models.len() > 1 {
            extra.insert("models".to_owned(), facts.models.into());
        }
        Ok(Captured {
            version: facts.version,
            model: facts.model,
            session_ids: facts.session_ids,
            extra,
        })
    })?;
    println!("joined {} as {}", capture.display(), destination.display());
    Ok(())
}

struct CapturedFacts {
    version: Version,
    model: String,
    /// Every model the capture shows.
    models: Vec<String>,
    session_ids: Vec<String>,
}

/// The Claude version, model and sessions a capture shows: the SDK's init
/// message, or a terminal session's transcript rows.
fn captured_facts(
    events: &[replay_support::IoEvent],
    pty: bool,
    launched: Option<String>,
) -> Result<CapturedFacts, Box<dyn std::error::Error>> {
    let mut version = None;
    let mut models = BTreeMap::<String, usize>::new();
    let mut session_ids = Vec::<String>::new();
    for event in events {
        if event.direction != replay_support::IoDirection::Read {
            continue;
        }
        let Ok(frame) = serde_json::from_str::<serde_json::Value>(&event.line) else {
            continue;
        };
        let (found_version, found_model, session) = if pty {
            if event.transport_id.as_deref() != Some("transcript") {
                continue;
            }
            let row = &frame["row"];
            (
                row["version"].as_str(),
                row.pointer("/message/model").and_then(|m| m.as_str()),
                row["sessionId"].as_str(),
            )
        } else {
            if frame["type"] != "system" || frame["subtype"] != "init" {
                continue;
            }
            (
                frame["claude_code_version"].as_str(),
                frame["model"].as_str(),
                frame["session_id"].as_str(),
            )
        };
        version = version.or(found_version.map(str::to_owned));
        // Claude marks rows it wrote itself with a synthetic model.
        if let Some(found) = found_model.filter(|m| !m.starts_with('<')) {
            *models.entry(found.to_owned()).or_insert(0) += 1;
        }
        if let Some(session) = session
            && !session_ids.iter().any(|known| known == session)
        {
            session_ids.push(session.to_owned());
        }
    }
    // The session's own model is the one most rows show; Claude may plan on
    // another.
    let model = models
        .iter()
        .max_by_key(|(_, rows)| **rows)
        .map(|(model, _)| model.clone())
        .or(launched)
        .ok_or("the capture shows no model")?;
    Ok(CapturedFacts {
        version: Version::parse(&version.ok_or("the capture shows no Claude version")?)?,
        model,
        models: models.into_keys().collect(),
        session_ids,
    })
}

async fn run_capture_proxy() -> Result<(), Box<dyn std::error::Error>> {
    let capture_dir = PathBuf::from(std::env::var("CLAUDE_CAPTURE_DIR")?);
    let real = std::env::var_os("CLAUDE_REAL_PATH").unwrap_or_else(|| "claude".into());
    std::fs::create_dir_all(&capture_dir)?;
    let start = Instant::now();
    let transport_id = format!(
        "{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    );
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    append_json(
        &capture_dir.join("spawn.jsonl"),
        &serde_json::json!({
            "us": 0,
            "transport_id": transport_id,
            "process": transport_id,
            "argv": args.iter().map(|arg| arg.to_string_lossy()).collect::<Vec<_>>(),
            "cwd": std::env::current_dir()?,
        }),
    )?;
    #[cfg(unix)]
    if super::capture::in_terminal() {
        let args = args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        return super::capture::terminal(capture_dir, real, args, transport_id).await;
    }
    let mut child = tokio::process::Command::new(real)
        .args(&args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    let child_stdin = child.stdin.take().ok_or("child stdin unavailable")?;
    let child_stdout = child.stdout.take().ok_or("child stdout unavailable")?;
    let mut child_stderr = child.stderr.take().ok_or("child stderr unavailable")?;
    let (io_tx, mut io_rx) = mpsc::channel::<String>(1024);
    let io_path = capture_dir.join("io.jsonl");
    let io_writer = tokio::spawn(async move {
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(io_path)
            .await?;
        // One append per line: proxies sharing the file never interleave.
        while let Some(line) = io_rx.recv().await {
            file.write_all(format!("{line}\n").as_bytes()).await?;
        }
        file.flush().await
    });
    let stdin_tx = io_tx.clone();
    let stdin_transport = transport_id.clone();
    let stdin_task = tokio::spawn(async move {
        let mut input = BufReader::new(tokio::io::stdin());
        let mut child_input = child_stdin;
        let mut line = String::new();
        loop {
            line.clear();
            if input.read_line(&mut line).await? == 0 {
                break;
            }
            let content = line.trim_end_matches('\n');
            let row = io_row(
                start.elapsed().as_micros() as u64,
                "stdin",
                content,
                &stdin_transport,
            );
            stdin_tx
                .send(row.to_string())
                .await
                .map_err(io::Error::other)?;
            child_input.write_all(line.as_bytes()).await?;
            child_input.flush().await?;
        }
        Ok::<(), io::Error>(())
    });
    let stdout_tx = io_tx.clone();
    let stdout_transport = transport_id;
    let stdout_task = tokio::spawn(async move {
        let mut child_output = BufReader::new(child_stdout);
        let mut output = tokio::io::stdout();
        let mut line = String::new();
        loop {
            line.clear();
            if child_output.read_line(&mut line).await? == 0 {
                break;
            }
            let content = line.trim_end_matches('\n');
            let row = io_row(
                start.elapsed().as_micros() as u64,
                "stdout",
                content,
                &stdout_transport,
            );
            stdout_tx
                .send(row.to_string())
                .await
                .map_err(io::Error::other)?;
            output.write_all(line.as_bytes()).await?;
            output.flush().await?;
        }
        Ok::<(), io::Error>(())
    });
    let stderr_task = tokio::spawn(async move {
        let mut stderr = tokio::io::stderr();
        tokio::io::copy(&mut child_stderr, &mut stderr).await
    });
    drop(io_tx);
    let status = child.wait().await?;
    // The host may hold its end open until the provider is gone.
    stdin_task.abort();
    stdout_task.await??;
    let _ = stderr_task.await?;
    io_writer.await??;
    if !status.success() {
        // The host reads the provider's own exit.
        std::process::exit(status.code().unwrap_or(1));
    }
    Ok(())
}

fn io_row(us: u64, direction: &str, line: &str, transport: &str) -> serde_json::Value {
    let mut row = serde_json::json!({
        "us": us,
        "dir": direction,
        "line": line,
        "transport_id": transport,
        "process": transport,
    });
    if let Some(session_id) = serde_json::from_str::<serde_json::Value>(line)
        .ok()
        .and_then(|value| {
            value
                .get("session_id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
    {
        row["session_id"] = serde_json::Value::String(session_id);
    }
    row
}

fn append_json(path: &Path, value: &serde_json::Value) -> io::Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(file, "{value}")
}

/// The variables holding the credentials a recording redacts.
const SECRETS: &[&str] = &["ANTHROPIC_API_KEY", "CLAUDE_CODE_OAUTH_TOKEN"];
