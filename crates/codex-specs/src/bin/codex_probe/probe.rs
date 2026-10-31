use std::io;
use std::path::{Path, PathBuf};

use chrono::Utc;
use codex_specs::specs::{
    CAPTURE_MODEL, SPEC_TOOL_SERVER, SpecSource, execute, fixtures_root, live_io_path, registry,
};
use redaction::Redaction;
use replay_support::live::{self, Captured, owner_home, write_events, write_manifest};
use replay_support::{Manifest, Recorded, SpecEntry, registry_rows, sanitize};
use semver::Version;
use uuid::Uuid;

#[tokio::main]
pub(super) async fn main() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(unix)]
    if std::env::var_os(super::capture::PROXY_ENV).is_some() {
        return super::capture::run().await;
    }
    if std::env::var_os(TOOL_SERVER_ENV).is_some() {
        live::serve_ask_tool(SPEC_TOOL_SERVER);
        return Ok(());
    }
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    match args.as_slice() {
        [command] if command == "list" => list(),
        [command, names @ ..] if command == "record" && !names.is_empty() => {
            record_selected(names).await
        }
        [command, capture, name] if command == "join" => join(Path::new(capture), name),
        _ => Err(usage().into()),
    }
}

fn usage() -> &'static str {
    "usage: codex-probe list | record <spec>... | join <capture dir> <name>"
}

fn list() -> Result<(), Box<dyn std::error::Error>> {
    for row in registry_rows(&fixtures_root(), registry())? {
        println!(
            "spec={} recording={} recorded={}",
            row.spec, row.recording, row.recorded
        );
    }
    Ok(())
}

async fn record_selected(names: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    for name in names {
        let entry = registry()
            .iter()
            .copied()
            .find(|entry| entry.name == name)
            .ok_or_else(|| format!("unknown Codex specification {name}; model={CAPTURE_MODEL}"))?;
        record_one(entry, &fixtures_root()).await?;
        println!("recorded {} model={CAPTURE_MODEL}", entry.name);
    }
    Ok(())
}

async fn record_one(entry: SpecEntry, root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let scratch = isolated_home(entry.name)?;
    let codex_home = scratch.path().join("codex-home");
    let report = execute(
        &entry,
        SpecSource::Live {
            codex_home: codex_home.clone(),
            model: CAPTURE_MODEL.to_string(),
        },
    )
    .await
    .map_err(|error| {
        format!(
            "{}; Codex rejected or failed capture model {}",
            error, CAPTURE_MODEL
        )
    })?;

    let source_io = live_io_path(&codex_home);
    let mut events = replay_support::load_script(&source_io);
    let redaction = sanitize(
        &mut events,
        &Redaction {
            home: owner_home(),
            extra_paths: vec![scratch.path().to_path_buf()],
            secret_env: live::secret_values(SECRETS),
            hostname: None,
            user: None,
            extra_personal_identifiers: Vec::new(),
            personal_identifier_keys: vec!["installationId".into(), "serverName".into()],
        },
    );
    let destination = root.join(entry.recording);
    if destination.exists() {
        std::fs::remove_dir_all(&destination)?;
    }
    std::fs::create_dir_all(&destination)?;
    write_events(&destination.join("io.jsonl"), &events)?;
    write_spawn(&destination.join("spawn.jsonl"), entry.name)?;

    let provider_version = report.provider_version.unwrap_or(installed_version()?);
    if report.server_model.is_empty() {
        return Err(format!(
            "{} did not report a server model for requested model {}",
            entry.name, CAPTURE_MODEL
        )
        .into());
    }
    let mut manifest = Manifest::inventory(
        &destination,
        entry.name,
        Recorded {
            provider: "codex".to_owned(),
            version: provider_version,
            model: report.server_model,
            at: Utc::now(),
        },
    )?;
    manifest.redaction = redaction;
    manifest.session_ids = report.session_ids;
    manifest.provider_extra.insert(
        "capture_model".to_string(),
        serde_json::Value::String(CAPTURE_MODEL.to_string()),
    );
    write_manifest(&destination.join("manifest.json"), &manifest)?;
    Ok(())
}

/// Join one live qualification capture to the unregistered live corpus as
/// `fixtures/live/<name>`. The capture is a folder the live lane wrote: one
/// app-server process's `io.jsonl` and `spawn.jsonl`, each line naming the
/// process it came from. It is sanitized as a recording is, the process
/// names are dropped, and its manifest records the Codex version and the
/// model the capture shows.
fn join(capture: &Path, name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let (spawns, events) = live::read_capture(capture)?;
    let rules = Redaction {
        home: owner_home(),
        extra_paths: live::live_scratch(&spawns),
        secret_env: live::secret_values(SECRETS),
        hostname: None,
        user: None,
        extra_personal_identifiers: Vec::new(),
        personal_identifier_keys: vec!["installationId".into(), "serverName".into()],
    };
    let mut transports = events
        .iter()
        .filter_map(|event| event.transport_id.clone())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter();
    let (Some(transport), None) = (transports.next(), transports.next()) else {
        return Err(format!("{}: only a capture of one client joins", capture.display()).into());
    };
    let spawns = spawns
        .into_iter()
        .map(|mut row| {
            serde_json::json!({
                "transport_id": transport,
                "argv": row["argv"].take(),
                "cwd": row["cwd"].take(),
            })
        })
        .collect();
    let root = codex_specs::specs::live_fixtures_root();
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

/// A scratch Codex home and project. The project holds the files the
/// specifications name; the home carries the owner's credentials unless the
/// specification is about being signed out, and registers the spec tool
/// server when the specification calls it.
fn isolated_home(spec: &str) -> Result<tempfile::TempDir, Box<dyn std::error::Error>> {
    let signed_in = !codex_specs::specs::SIGNED_OUT.contains(&spec);
    let scratch = tempfile::Builder::new().prefix("codex-spec-").tempdir()?;
    let codex_home = scratch.path().join("codex-home");
    let project = scratch.path().join("project");
    std::fs::create_dir_all(&codex_home)?;
    std::fs::create_dir_all(&project)?;
    std::fs::write(project.join("config.txt"), "VALUE=1\n")?;
    std::fs::write(project.join("old.txt"), "old\n")?;
    std::fs::write(project.join("square.png"), SQUARE_PNG)?;

    let source_auth = owner_codex_home().join("auth.json");
    if signed_in && !source_auth.is_file() {
        return Err(format!(
            "Codex authentication is unavailable at {}; run `codex login` first",
            source_auth.display()
        )
        .into());
    }
    if signed_in {
        std::fs::copy(&source_auth, codex_home.join("auth.json"))?;
    }
    std::fs::write(codex_home.join(".personality_migration"), "v1\n")?;
    std::fs::write(codex_home.join(".sandbox_migration"), "v1\n")?;
    std::fs::write(
        codex_home.join("installation_id"),
        format!("{}\n", Uuid::new_v4()),
    )?;
    let project_key = serde_json::to_string(&project.to_string_lossy())?;
    let mut config = format!("[projects.{project_key}]\ntrust_level = \"trusted\"\n");
    if codex_specs::specs::WITH_TOOL_SERVER.contains(&spec) {
        let command = serde_json::to_string(&std::env::current_exe()?.to_string_lossy())?;
        config.push_str(&format!(
            "\n[mcp_servers.{SPEC_TOOL_SERVER}]\ncommand = {command}\n\
             env = {{ {TOOL_SERVER_ENV} = \"1\" }}\n"
        ));
    }
    std::fs::write(codex_home.join("config.toml"), config)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&codex_home, std::fs::Permissions::from_mode(0o700))?;
        if signed_in {
            std::fs::set_permissions(
                codex_home.join("auth.json"),
                std::fs::Permissions::from_mode(0o600),
            )?;
        }
    }
    Ok(scratch)
}

/// The seed image the image specification asks Codex to look at.
const SQUARE_PNG: &[u8] = include_bytes!("../../../assets/square.png");

fn write_spawn(path: &Path, spec: &str) -> io::Result<()> {
    // Two clients share a server on a socket in a scratch folder.
    let listen = if codex_specs::specs::TWO_CLIENTS.contains(&spec) {
        "unix://<socket>"
    } else {
        "stdio://"
    };
    let row = serde_json::json!({
        "command": "codex",
        "args": ["--model", CAPTURE_MODEL, "app-server", "--listen", listen],
        "model": CAPTURE_MODEL,
    });
    std::fs::write(path, format!("{}\n", row))
}

fn installed_version() -> Result<Version, Box<dyn std::error::Error>> {
    let output = std::process::Command::new("codex")
        .arg("--version")
        .output()?;
    if !output.status.success() {
        return Err(format!("codex --version exited with {}", output.status).into());
    }
    let stdout = String::from_utf8(output.stdout)?;
    stdout
        .split_whitespace()
        .find_map(|part| Version::parse(part).ok())
        .ok_or_else(|| format!("could not parse Codex version from {stdout:?}").into())
}

fn owner_codex_home() -> PathBuf {
    std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex")))
        .unwrap_or_else(|| PathBuf::from(".codex"))
}

/// Set in the environment of the probe when Codex starts it as the spec tool
/// server.
const TOOL_SERVER_ENV: &str = "CODEX_SPEC_TOOL_SERVER";

/// The variables holding the credentials a recording redacts.
const SECRETS: &[&str] = &["OPENAI_API_KEY", "CODEX_API_KEY"];
