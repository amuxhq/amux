//! `codex-probe`: the capture proxy the live lane puts in front of Codex,
//! the tool servers its form and subagent-tools scenarios configure, and
//! `join`, which sanitizes a passing capture into the corpus.

use std::io::{BufRead as _, Write as _};
use std::path::Path;

use qualification::recording::live::{self, Captured, owner_home};
use redaction::Redaction;
use semver::Version;

/// The name the tool server gives itself.
const TOOL_SERVER: &str = "spec";
/// The client the capture proxy records amux's agent as.
const AMUX: &str = "amux";

#[tokio::main]
pub(super) async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // The tool server the live lane's form scenario configures for Codex;
    // it runs under the capture proxy's environment, so it is told apart
    // first.
    if std::env::args().nth(1).as_deref() == Some("__tool-server") {
        live::serve_ask_tool(TOOL_SERVER);
        return Ok(());
    }
    if std::env::args().nth(1).as_deref() == Some("__meta-server") {
        let log = std::env::args()
            .nth(2)
            .ok_or("usage: __meta-server <log>")?;
        serve_meta_tool(Path::new(&log));
        return Ok(());
    }
    if std::env::var_os(super::capture::PROXY_ENV).is_some() {
        return super::capture::run().await;
    }
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    match args.as_slice() {
        [command, capture, name] if command == "join" => join(Path::new(capture), name),
        _ => Err("usage: codex-probe join <capture dir> <name>".into()),
    }
}

/// A tool server that keeps what it was sent: each request Codex writes it
/// is appended to `log` with the server's process id, so a scenario can
/// read which threads started a server of their own and what `_meta` a
/// call from each thread carries. Its one tool, `whoami`, answers with the
/// process id that heard it.
fn serve_meta_tool(log: &Path) {
    let pid = std::process::id();
    let keep = |line: &str| {
        let row = serde_json::json!({"pid": pid, "line": line});
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log)
        {
            let _ = file.write_all(format!("{row}\n").as_bytes());
        }
    };
    let mut stdout = std::io::stdout();
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        keep(&line);
        let Ok(message) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
            continue;
        };
        let Some(id) = message.get("id").cloned() else {
            continue;
        };
        let result = match message.get("method").and_then(serde_json::Value::as_str) {
            Some("initialize") => serde_json::json!({
                "protocolVersion": message.pointer("/params/protocolVersion")
                    .cloned()
                    .unwrap_or_else(|| "2025-06-18".into()),
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "meta", "version": "1.0.0"},
            }),
            Some("tools/list") => serde_json::json!({
                "tools": [{
                    "name": "whoami",
                    "description": "Says which tool-server process answered.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {"caller": {"type": "string"}},
                        "required": ["caller"],
                    },
                }],
            }),
            Some("tools/call") => {
                let caller = message
                    .pointer("/params/arguments/caller")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("nobody");
                serde_json::json!({
                    "content": [{"type": "text", "text": format!("process {pid} heard {caller}")}],
                })
            }
            _ => serde_json::json!(null),
        };
        let answer = if result.is_null() {
            serde_json::json!({"jsonrpc": "2.0", "id": id,
                "error": {"code": -32601, "message": "method not found"}})
        } else {
            serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result})
        };
        let _ = writeln!(stdout, "{answer}");
        let _ = stdout.flush();
    }
}

/// Join one live qualification capture to the live corpus as
/// `crates/interpret/fixtures/codex/live/<name>`. The capture is a folder
/// the live lane wrote: the scenario's app-server processes' `io.jsonl` and
/// `spawn.jsonl`, each line naming the process it came from and the client
/// it went to or came from.
/// Only amux's lines join: what Codex's own app, attached beside it, caused
/// reaches amux as what Codex tells it, and the app's own requests are not
/// amux's to answer. It is sanitized as a recording is, the process names
/// are dropped, and its manifest records the Codex version and the model
/// the capture shows.
fn join(capture: &Path, name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let (spawns, mut events) = live::read_capture(capture)?;
    events.retain(|event| event.transport_id.as_deref() == Some(AMUX));
    let rules = Redaction {
        home: owner_home(),
        extra_paths: live::live_scratch(&spawns),
        secret_env: live::secret_values(SECRETS),
        hostname: None,
        user: None,
        extra_personal_identifiers: Vec::new(),
        personal_identifier_keys: vec!["installationId".into(), "serverName".into()],
    };
    let spawns = spawns
        .into_iter()
        .map(|mut row| {
            serde_json::json!({
                "transport_id": AMUX,
                "argv": row["argv"].take(),
                "cwd": row["cwd"].take(),
            })
        })
        .collect();
    // The corpus lives beside the interpreter goldens that replay it.
    let root = source_tree::crate_dir!().join("../interpret/fixtures/codex/live");
    let destination = live::join(&root, name, "codex", events, spawns, &rules, |events| {
        let (version, model, session_ids) = captured_facts(events)?;
        Ok(Captured {
            version,
            model,
            session_ids,
            extra: serde_json::Map::new(),
        })
    })?;
    println!("joined {} as {}", capture.display(), destination.display());
    Ok(())
}

/// The Codex version, model and threads a capture shows, from the server's
/// answers about its threads.
fn captured_facts(
    events: &[qualification::recording::IoEvent],
) -> Result<(Version, String, Vec<String>), Box<dyn std::error::Error>> {
    let mut version = None;
    let mut model = None;
    let mut threads = Vec::<String>::new();
    for event in events {
        if event.direction != qualification::recording::IoDirection::Read {
            continue;
        }
        let Ok(frame) = serde_json::from_str::<serde_json::Value>(&event.line) else {
            continue;
        };
        let result = &frame["result"];
        model = model.or(result["model"].as_str().map(str::to_owned));
        let thread = &result["thread"];
        version = version.or(thread["cliVersion"].as_str().map(str::to_owned));
        if let Some(id) = thread["id"].as_str()
            && !threads.iter().any(|known| known == id)
        {
            threads.push(id.to_owned());
        }
    }
    Ok((
        Version::parse(&version.ok_or("the capture shows no Codex version")?)?,
        model.ok_or("the capture shows no model")?,
        threads,
    ))
}

/// The variables holding the credentials a recording redacts.
const SECRETS: &[&str] = &["OPENAI_API_KEY", "CODEX_API_KEY"];
