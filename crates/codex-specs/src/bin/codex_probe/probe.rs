//! `codex-probe`: the capture proxy the live lane puts in front of Codex,
//! the tool server its form scenario configures, and `join`, which
//! sanitizes a passing capture into the corpus.

use std::path::Path;

use redaction::Redaction;
use replay_support::live::{self, Captured, owner_home};
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
    #[cfg(unix)]
    if std::env::var_os(super::capture::PROXY_ENV).is_some() {
        return super::capture::run().await;
    }
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    match args.as_slice() {
        [command, capture, name] if command == "join" => join(Path::new(capture), name),
        _ => Err("usage: codex-probe join <capture dir> <name>".into()),
    }
}

/// Join one live qualification capture to the live corpus as
/// `fixtures/live/<name>`. The capture is a folder the live lane wrote: the
/// scenario's app-server processes' `io.jsonl` and `spawn.jsonl`, each line
/// naming the process it came from and the client it went to or came from.
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
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/live");
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
    events: &[replay_support::IoEvent],
) -> Result<(Version, String, Vec<String>), Box<dyn std::error::Error>> {
    let mut version = None;
    let mut model = None;
    let mut threads = Vec::<String>::new();
    for event in events {
        if event.direction != replay_support::IoDirection::Read {
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
