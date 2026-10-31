use std::collections::BTreeMap;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use chrono::Utc;
use claude_specs::specs::pty::registry as pty_registry;
use redaction::Redaction;
use replay_support::{Manifest, Recorded, SpecEntry, load_recording, registry_rows, sanitize};
use semver::Version;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
use uuid::Uuid;

#[tokio::main]
pub(super) async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::args().nth(1).as_deref() == Some("__pty-hook") {
        return forward_pty_hook();
    }
    #[cfg(unix)]
    if std::env::args().nth(1).as_deref() == Some(super::capture::HOOK) {
        let mut command = std::env::args().skip(2);
        let program = command.next().ok_or("a wrapped hook names its command")?;
        return super::capture::hook(&program, &command.collect::<Vec<_>>());
    }
    // The tool server the live lane's form scenarios hand Claude; it runs
    // under the capture proxy's environment, so it is told apart first.
    if std::env::args().nth(1).as_deref() == Some("__tool-server") {
        run_tool_server();
        return Ok(());
    }
    if std::env::var_os("CLAUDE_CAPTURE_PROXY").is_some() {
        return run_capture_proxy().await;
    }
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    match args.as_slice() {
        [command, rest @ ..] if command == "list" => list(rest),
        [command, rest @ ..] if command == "record" => record_command(rest).await,
        [command, capture, name] if command == "join" => join(Path::new(capture), name),
        _ => Err(usage().into()),
    }
}

fn usage() -> &'static str {
    "usage: claude-probe list | record <spec>... | join <capture dir> <name>"
}

/// The terminal specifications and whether each is recorded.
fn list(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    if !args.is_empty() {
        return Err(usage().into());
    }
    let root = claude_specs::specs::pty::fixtures_root();
    for row in registry_rows(&root, pty_registry())? {
        println!(
            "spec={} recording={} recorded={}",
            row.spec, row.recording, row.recorded
        );
    }
    Ok(())
}

async fn record_command(names: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    if names.is_empty() || names.iter().any(|name| name.starts_with('-')) {
        return Err(usage().into());
    }
    for name in names {
        let entry = find_pty_entry(name)?;
        record_pty_one(entry).await?;
        println!("spec={} outcome=recorded", entry.name);
    }
    Ok(())
}

fn find_pty_entry(name: &str) -> Result<SpecEntry, Box<dyn std::error::Error>> {
    pty_registry()
        .iter()
        .copied()
        .find(|entry| entry.name == name || entry.recording == name)
        .ok_or_else(|| format!("unknown Claude PTY specification {name}").into())
}

struct PtyCapture {
    report: claude_specs::specs::pty::RunReport,
    spawn: Vec<u8>,
    scratch: PathBuf,
}

async fn capture_pty(entry: SpecEntry) -> Result<PtyCapture, Box<dyn std::error::Error>> {
    let scratch = tempfile::Builder::new()
        .prefix("claude-pty-spec-")
        .tempdir_in("/tmp")?;
    let work = scratch.path().join("work");
    std::fs::create_dir_all(&work)?;
    std::fs::write(work.join("config.txt"), "VALUE=1\n")?;
    std::fs::write(work.join("README.md"), "CURRENT\n")?;
    std::fs::write(work.join("square.png"), claude_specs::specs::SQUARE_PNG)?;
    let hook_command = vec![
        std::env::current_exe()?.display().to_string(),
        "__pty-hook".to_owned(),
    ];
    let managed = claude::launch::ManagedSettings {
        hook_command,
        ..Default::default()
    };
    let launch = claude_specs::terminal::Launch {
        binary: claude_binary(),
        cwd: work,
        args: Vec::new(),
        session_id: Uuid::new_v4(),
        settings: claude::launch::merged_settings(None, &managed),
        env_scrub: claude::launch::CHILD_SESSION_ENV_SCRUB,
    };
    let mut spawn_launch = launch.clone();
    claude_specs::specs::pty::prepare_launch(&entry, &mut spawn_launch)?;
    let spawn = serde_json::to_vec(&serde_json::json!({
        "transport_id": "pty",
        "argv": claude_specs::terminal::pty_spawn_args(&spawn_launch),
    }))?;
    let report = claude_specs::specs::pty::run(
        &entry,
        claude_specs::specs::pty::Source::Live {
            launch,
            size: pty_host::PtySize {
                rows: 40,
                cols: 120,
            },
        },
    )
    .await?;
    Ok(PtyCapture {
        report,
        spawn,
        scratch: scratch.path().to_path_buf(),
    })
}

async fn record_pty_one(entry: SpecEntry) -> Result<(), Box<dyn std::error::Error>> {
    let mut capture = capture_pty(entry).await?;
    stabilize_pty_transcript_paths(&mut capture.report.io)?;
    claude_specs::scrub::transcript(&mut capture.report.io)?;
    let redaction = sanitize(
        &mut capture.report.io,
        &Redaction {
            home: owner_home(),
            extra_paths: vec![capture.scratch.clone()],
            secret_env: secret_values(),
            hostname: None,
            user: None,
            extra_personal_identifiers: Vec::new(),
            personal_identifier_keys: Vec::new(),
        },
    );
    let root = claude_specs::specs::pty::fixtures_root();
    std::fs::create_dir_all(&root)?;
    let stage = root.join(format!(".{}.{}", entry.recording, Uuid::new_v4()));
    std::fs::create_dir_all(&stage)?;
    write_events(&stage.join("io.jsonl"), &capture.report.io)?;
    let mut spawn = capture.spawn;
    spawn.push(b'\n');
    let spawn = sanitize_json_lines(&spawn, &capture.scratch)?;
    std::fs::write(stage.join("spawn.jsonl"), spawn)?;
    let mut manifest = Manifest::inventory(
        &stage,
        entry.name,
        Recorded {
            provider: "claude".to_owned(),
            version: capture.report.provider_version.clone(),
            model: capture.report.model.clone(),
            at: Utc::now(),
        },
    )?;
    manifest.session_ids = vec![capture.report.session_id.clone()];
    manifest.redaction = redaction;
    write_manifest(&stage.join("manifest.json"), &manifest)?;

    let recording = load_recording(&stage)?;
    let replay = replay_support::strict_replay(&recording);
    claude_specs::specs::pty::run(
        &entry,
        claude_specs::specs::pty::Source::Recorded {
            replay,
            manifest: Box::new(recording.manifest),
        },
    )
    .await?;

    let destination = root.join(entry.recording);
    if destination.exists() {
        std::fs::remove_dir_all(&destination)?;
    }
    std::fs::rename(&stage, &destination)?;
    Ok(())
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

fn forward_pty_hook() -> Result<(), Box<dyn std::error::Error>> {
    let socket = std::env::var_os("CLAUDE_HOOK_SOCKET")
        .map(PathBuf::from)
        .ok_or("CLAUDE_HOOK_SOCKET is not set")?;
    let mut payload = Vec::new();
    std::io::Read::read_to_end(&mut std::io::stdin(), &mut payload)?;
    claude::hooks::forward(&payload, &socket)?;
    Ok(())
}

/// Join one live qualification capture to the unregistered live corpus as
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
    let mut spawns = std::fs::read_to_string(capture.join("spawn.jsonl"))?
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(serde_json::from_str::<serde_json::Value>)
        .collect::<Result<Vec<_>, _>>()?;
    let mut events = replay_support::load_script(capture.join("io.jsonl"));
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
        extra_paths: live_scratch(&spawns),
        secret_env: secret_values(),
        hostname: None,
        user: None,
        extra_personal_identifiers: Vec::new(),
        personal_identifier_keys: Vec::new(),
    };
    let mut redaction = sanitize(&mut events, &rules);
    let facts = captured_facts(&events, pty)?;

    let root = claude_specs::specs::live_fixtures_root().join(if pty { "pty" } else { "sdk" });
    std::fs::create_dir_all(&root)?;
    let stage = root.join(format!(".{name}.{}", Uuid::new_v4()));
    std::fs::create_dir_all(&stage)?;
    write_events(&stage.join("io.jsonl"), &events)?;
    let mut spawn = String::new();
    for mut row in spawns {
        let mut kept = serde_json::Map::new();
        // A terminal process's channels are all `pty`; a stdio process is
        // named by its transport.
        kept.insert(
            "transport_id".into(),
            if pty {
                "pty".into()
            } else {
                row["transport_id"].take()
            },
        );
        kept.insert("argv".into(), row["argv"].take());
        kept.insert("cwd".into(), row["cwd"].take());
        let mut row = serde_json::Value::Object(kept);
        redaction::redact_value(&mut row, &rules, &mut redaction);
        spawn.push_str(&serde_json::to_string(&row)?);
        spawn.push('\n');
    }
    std::fs::write(stage.join("spawn.jsonl"), spawn)?;
    let mut manifest = Manifest::inventory(
        &stage,
        name,
        Recorded {
            provider: "claude".to_owned(),
            version: facts.version.clone(),
            model: facts.model.clone(),
            at: Utc::now(),
        },
    )?;
    manifest.session_ids = facts.session_ids.clone();
    manifest.redaction = redaction;
    manifest.provider_extra.insert(
        "driven_by".to_owned(),
        "amux's interpreter, in the live qualification lane".into(),
    );
    if facts.models.len() > 1 {
        manifest
            .provider_extra
            .insert("models".to_owned(), facts.models.clone().into());
    }
    write_manifest(&stage.join("manifest.json"), &manifest)?;
    load_recording(&stage)?;

    let destination = root.join(name);
    if destination.exists() {
        std::fs::remove_dir_all(&destination)?;
    }
    std::fs::rename(&stage, &destination)?;
    println!("joined {} as {}", capture.display(), destination.display());
    Ok(())
}

/// The scratch installation a live capture ran in: the folder under the
/// temporary directory that holds each process's working directory, in both
/// of the spellings macOS gives it, and this machine's temporary directory,
/// where the lane keeps its sockets.
fn live_scratch(spawns: &[serde_json::Value]) -> Vec<PathBuf> {
    let mut paths = vec![std::env::temp_dir()];
    for spawn in spawns {
        let Some(cwd) = spawn["cwd"].as_str() else {
            continue;
        };
        let cwd = Path::new(cwd);
        let Some(root) = cwd.ancestors().find(|dir| {
            dir.parent().is_some_and(|parent| {
                parent == Path::new("/tmp") || parent == Path::new("/private/tmp")
            })
        }) else {
            continue;
        };
        let tail = root.strip_prefix("/private").unwrap_or(root);
        paths.push(tail.to_path_buf());
        paths.push(Path::new("/private").join(tail.strip_prefix("/").unwrap_or(tail)));
    }
    paths
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
        .ok_or("the capture shows no model")?;
    Ok(CapturedFacts {
        version: Version::parse(&version.ok_or("the capture shows no Claude version")?)?,
        model,
        models: models.into_keys().collect(),
        session_ids,
    })
}

fn write_events(path: &Path, events: &[replay_support::IoEvent]) -> io::Result<()> {
    let mut output = String::new();
    for event in events {
        let mut row = serde_json::json!({
            "us": event.us,
            "dir": match event.direction {
                replay_support::IoDirection::Write => "stdin",
                replay_support::IoDirection::Read => "stdout",
            },
            "line": event.line,
        });
        if let Some(transport_id) = &event.transport_id {
            row["transport_id"] = serde_json::Value::String(transport_id.clone());
        }
        if let Some(session_id) = &event.session_id {
            row["session_id"] = serde_json::Value::String(session_id.clone());
        }
        output.push_str(&serde_json::to_string(&row).map_err(io::Error::other)?);
        output.push('\n');
    }
    std::fs::write(path, output)
}

fn write_manifest(path: &Path, manifest: &Manifest) -> io::Result<()> {
    let mut bytes = serde_json::to_vec_pretty(manifest).map_err(io::Error::other)?;
    bytes.push(b'\n');
    std::fs::write(path, bytes)
}

fn sanitize_json_lines(bytes: &[u8], scratch: &Path) -> io::Result<Vec<u8>> {
    let text = std::str::from_utf8(bytes).map_err(io::Error::other)?;
    let home = owner_home().to_string_lossy().into_owned();
    let scratch = scratch.to_string_lossy().into_owned();
    let secrets = secret_values();
    let mut output = String::new();
    for line in text.lines() {
        let mut value: serde_json::Value = serde_json::from_str(line).map_err(io::Error::other)?;
        redact_value(&mut value, &home, &scratch, &secrets);
        output.push_str(&serde_json::to_string(&value).map_err(io::Error::other)?);
        output.push('\n');
    }
    Ok(output.into_bytes())
}

fn redact_value(value: &mut serde_json::Value, home: &str, scratch: &str, secrets: &[String]) {
    match value {
        serde_json::Value::String(text) => {
            *text = text.replace(scratch, "<MACHINE_PATH>");
            if !home.is_empty() {
                *text = text.replace(home, "<HOME>");
            }
            for secret in secrets {
                if !secret.is_empty() {
                    *text = text.replace(secret, "<REDACTED>");
                }
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                redact_value(value, home, scratch, secrets);
            }
        }
        serde_json::Value::Object(values) => {
            for value in values.values_mut() {
                redact_value(value, home, scratch, secrets);
            }
        }
        _ => {}
    }
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

/// The tool server the live lane's form scenarios hand Claude: one tool
/// that asks the person to confirm a word with a form, or, for the word
/// `LINK`, with a link.
fn run_tool_server() {
    const PROTOCOL_VERSION: &str = "2025-11-25";
    const TOOL: &str = "ask_the_operator";
    let stdin = std::io::stdin();
    let mut lines = stdin.lock().lines();
    let mut stdout = std::io::stdout();
    let mut outgoing_id = 1000i64;
    while let Some(Ok(line)) = lines.next() {
        let Ok(message) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
            continue;
        };
        let method = message.get("method").and_then(serde_json::Value::as_str);
        let id = message.get("id").cloned();
        let result = match (method, &id) {
            (Some(_), None) => continue,
            (Some("initialize"), Some(_)) => serde_json::json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "spec-external", "version": "1.0.0"},
            }),
            (Some("tools/list"), Some(_)) => serde_json::json!({
                "tools": [{
                    "name": TOOL,
                    "description": "Ask the operator to confirm a word, then return it.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {"word": {"type": "string"}},
                        "required": ["word"],
                    },
                }],
            }),
            (Some("tools/call"), Some(_)) => {
                let word = message
                    .pointer("/params/arguments/word")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("nothing")
                    .to_owned();
                outgoing_id += 1;
                let answer = elicit(&mut stdout, &mut lines, outgoing_id, &word);
                serde_json::json!({"content": [{"type": "text", "text": answer}]})
            }
            _ => {
                respond(
                    &mut stdout,
                    serde_json::json!({
                        "jsonrpc": "2.0", "id": id,
                        "error": {"code": -32601, "message": "method not found"},
                    }),
                );
                continue;
            }
        };
        respond(
            &mut stdout,
            serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result}),
        );
    }
}

fn elicit(
    stdout: &mut std::io::Stdout,
    lines: &mut std::io::Lines<std::io::StdinLock<'_>>,
    id: i64,
    word: &str,
) -> String {
    // The word LINK asks for the link form of elicitation: the person is
    // sent to a page instead of filling in fields.
    let params = if word == "LINK" {
        serde_json::json!({
            "mode": "url",
            "message": "Open the page to confirm.",
            "url": "https://example.com/confirm",
            "elicitationId": "spec-link",
        })
    } else {
        serde_json::json!({
            "message": format!("Confirm the word {word}."),
            "requestedSchema": {
                "type": "object",
                "properties": {"confirmed": {"type": "string"}},
                "required": ["confirmed"],
            },
        })
    };
    respond(
        stdout,
        serde_json::json!({
            "jsonrpc": "2.0", "id": id, "method": "elicitation/create",
            "params": params,
        }),
    );
    while let Some(Ok(line)) = lines.next() {
        let Ok(message) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
            continue;
        };
        if message.get("id").and_then(serde_json::Value::as_i64) != Some(id) {
            continue;
        }
        return match message
            .pointer("/result/action")
            .and_then(serde_json::Value::as_str)
        {
            Some("accept") => message
                .pointer("/result/content/confirmed")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(word)
                .to_owned(),
            Some(other) => format!("elicitation {other}"),
            None => format!("elicitation failed: {}", message["error"]),
        };
    }
    "elicitation abandoned".to_owned()
}

fn respond(stdout: &mut std::io::Stdout, message: serde_json::Value) {
    let _ = writeln!(stdout, "{message}");
    let _ = stdout.flush();
}

fn claude_binary() -> PathBuf {
    std::env::var_os("CLAUDE_REAL_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("claude"))
}

fn owner_home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
}

fn secret_values() -> Vec<String> {
    ["ANTHROPIC_API_KEY", "CLAUDE_CODE_OAUTH_TOKEN"]
        .into_iter()
        .filter_map(|name| std::env::var(name).ok())
        .collect()
}
