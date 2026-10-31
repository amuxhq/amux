//! What the two recording probes share: standing in for the provider on the
//! PATH, writing a recording's files, joining a live qualification capture
//! to the live corpus, and the one-tool server the form scenarios ask
//! through.

use std::error::Error;
use std::io::{self, BufRead as _, Write as _};
use std::path::{Path, PathBuf};

use chrono::Utc;
use redaction::Redaction;
use semver::Version;
use serde_json::{Value, json};

use crate::{IoDirection, IoEvent, Manifest, Recorded, load_recording, load_script, sanitize};

/// When `proxy_env` is set the probe is standing in for `command` on the
/// PATH: it takes itself off the PATH the real provider inherits, so a
/// wrapper that runs the next `command` it finds reaches the real one.
/// Called first thing in `main`, before any other thread runs.
pub fn stand_in_for(command: &str, proxy_env: &str) -> io::Result<()> {
    if std::env::var_os(proxy_env).is_some() {
        let path = crate::path_past_probe(
            &std::env::current_exe()?,
            std::ffi::OsStr::new(command),
            &std::env::var_os("PATH").unwrap_or_default(),
        );
        // SAFETY: the caller runs this before any other thread starts.
        unsafe { std::env::set_var("PATH", path) };
    }
    Ok(())
}

/// The owner's home folder, which every recording redacts.
pub fn owner_home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
}

/// The values of the variables `names` that are set: the provider's
/// credentials, which every recording redacts.
pub fn secret_values(names: &[&str]) -> Vec<String> {
    names
        .iter()
        .filter_map(|name| std::env::var(name).ok())
        .collect()
}

/// Writes `events` as an `io.jsonl`.
pub fn write_events(path: &Path, events: &[IoEvent]) -> io::Result<()> {
    let mut output = String::new();
    for event in events {
        let mut row = json!({
            "us": event.us,
            "dir": match event.direction {
                IoDirection::Write => "stdin",
                IoDirection::Read => "stdout",
            },
            "line": event.line,
        });
        if let Some(transport_id) = &event.transport_id {
            row["transport_id"] = Value::String(transport_id.clone());
        }
        if let Some(session_id) = &event.session_id {
            row["session_id"] = Value::String(session_id.clone());
        }
        output.push_str(&serde_json::to_string(&row).map_err(io::Error::other)?);
        output.push('\n');
    }
    std::fs::write(path, output)
}

pub fn write_manifest(path: &Path, manifest: &Manifest) -> io::Result<()> {
    let mut bytes = serde_json::to_vec_pretty(manifest).map_err(io::Error::other)?;
    bytes.push(b'\n');
    std::fs::write(path, bytes)
}

/// A capture folder the live lane wrote: each provider process's spawn row,
/// then the lines they read and wrote.
pub fn read_capture(capture: &Path) -> Result<(Vec<Value>, Vec<IoEvent>), Box<dyn Error>> {
    let spawns = std::fs::read_to_string(capture.join("spawn.jsonl"))?
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(serde_json::from_str::<Value>)
        .collect::<Result<Vec<_>, _>>()?;
    Ok((spawns, load_script(capture.join("io.jsonl"))))
}

/// The scratch installation a live capture ran in: the folder under the
/// temporary directory that holds each process's working directory, in both
/// of the spellings macOS gives it, and this machine's temporary directory,
/// where the lane keeps its sockets.
pub fn live_scratch(spawns: &[Value]) -> Vec<PathBuf> {
    let mut paths = vec![std::env::temp_dir()];
    for spawn in spawns {
        let Some(cwd) = spawn["cwd"].as_str() else {
            continue;
        };
        let Some(root) = Path::new(cwd).ancestors().find(|dir| {
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

/// What a provider reads from its sanitized capture for the manifest.
pub struct Captured {
    pub version: Version,
    pub model: String,
    pub session_ids: Vec<String>,
    /// Manifest fields beside the lane's `driven_by`.
    pub extra: serde_json::Map<String, Value>,
}

/// Joins a live capture to the live corpus as `root/<name>`, replacing any
/// recording of that name: `events` are sanitized by `rules`, `captured`
/// reads the manifest's facts from them, and each spawn row (already
/// shaped as the corpus keeps it) is redacted by the same rules. The
/// recording is staged beside its destination and checked before it
/// replaces it.
pub fn join(
    root: &Path,
    name: &str,
    provider: &str,
    mut events: Vec<IoEvent>,
    spawns: Vec<Value>,
    rules: &Redaction,
    captured: impl FnOnce(&[IoEvent]) -> Result<Captured, Box<dyn Error>>,
) -> Result<PathBuf, Box<dyn Error>> {
    let mut redaction = sanitize(&mut events, rules);
    let captured = captured(&events)?;
    std::fs::create_dir_all(root)?;
    let stage = root.join(format!(".{name}.{}", std::process::id()));
    std::fs::create_dir_all(&stage)?;
    write_events(&stage.join("io.jsonl"), &events)?;
    let mut spawn = String::new();
    for mut row in spawns {
        redaction::redact_value(&mut row, rules, &mut redaction);
        spawn.push_str(&serde_json::to_string(&row)?);
        spawn.push('\n');
    }
    std::fs::write(stage.join("spawn.jsonl"), spawn)?;
    let mut manifest = Manifest::inventory(
        &stage,
        name,
        Recorded {
            provider: provider.to_owned(),
            version: captured.version,
            model: captured.model,
            at: Utc::now(),
        },
    )?;
    manifest.session_ids = captured.session_ids;
    manifest.redaction = redaction;
    manifest.provider_extra.insert(
        "driven_by".to_owned(),
        "amux's interpreter, in the live qualification lane".into(),
    );
    manifest.provider_extra.extend(captured.extra);
    write_manifest(&stage.join("manifest.json"), &manifest)?;
    load_recording(&stage)?;

    let destination = root.join(name);
    if destination.exists() {
        std::fs::remove_dir_all(&destination)?;
    }
    std::fs::rename(&stage, &destination)?;
    Ok(destination)
}

/// The tool the form scenarios ask through.
pub const ASK_TOOL: &str = "ask_the_operator";

/// A stdio MCP server, named `server` at initialize, with one tool: called
/// with a word it asks the person, through an elicitation, to confirm that
/// word with a form, or, for the word `LINK`, with a link to a page.
pub fn serve_ask_tool(server: &str) {
    const PROTOCOL_VERSION: &str = "2025-11-25";
    let stdin = io::stdin();
    let mut lines = stdin.lock().lines();
    let mut stdout = io::stdout();
    let mut outgoing_id = 1000i64;
    while let Some(Ok(line)) = lines.next() {
        let Ok(message) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        let method = message.get("method").and_then(Value::as_str);
        let id = message.get("id").cloned();
        let result = match (method, &id) {
            (Some(_), None) => continue,
            (Some("initialize"), Some(_)) => json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": server, "version": "1.0.0"},
            }),
            (Some("tools/list"), Some(_)) => json!({
                "tools": [{
                    "name": ASK_TOOL,
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
                    .and_then(Value::as_str)
                    .unwrap_or("nothing")
                    .to_owned();
                outgoing_id += 1;
                let answer = elicit(&mut stdout, &mut lines, outgoing_id, &word);
                json!({"content": [{"type": "text", "text": answer}]})
            }
            _ => {
                send(
                    &mut stdout,
                    json!({
                        "jsonrpc": "2.0", "id": id,
                        "error": {"code": -32601, "message": "method not found"},
                    }),
                );
                continue;
            }
        };
        send(
            &mut stdout,
            json!({"jsonrpc": "2.0", "id": id, "result": result}),
        );
    }
}

fn elicit(
    stdout: &mut io::Stdout,
    lines: &mut io::Lines<io::StdinLock<'_>>,
    id: i64,
    word: &str,
) -> String {
    let params = if word == "LINK" {
        json!({
            "mode": "url",
            "message": "Open the page to confirm.",
            "url": "https://example.com/confirm",
            "elicitationId": "spec-link",
        })
    } else {
        json!({
            "message": format!("Confirm the word {word}."),
            "requestedSchema": {
                "type": "object",
                "properties": {"confirmed": {"type": "string"}},
                "required": ["confirmed"],
            },
        })
    };
    send(
        stdout,
        json!({
            "jsonrpc": "2.0", "id": id, "method": "elicitation/create", "params": params,
        }),
    );
    while let Some(Ok(line)) = lines.next() {
        let Ok(message) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        if message.get("id").and_then(Value::as_i64) != Some(id) {
            continue;
        }
        return match message.pointer("/result/action").and_then(Value::as_str) {
            Some("accept") => message
                .pointer("/result/content/confirmed")
                .and_then(Value::as_str)
                .unwrap_or(word)
                .to_owned(),
            Some(other) => format!("elicitation {other}"),
            None => format!("elicitation failed: {}", message["error"]),
        };
    }
    "elicitation abandoned".to_owned()
}

fn send(stdout: &mut io::Stdout, message: Value) {
    let _ = writeln!(stdout, "{message}");
    let _ = stdout.flush();
}
