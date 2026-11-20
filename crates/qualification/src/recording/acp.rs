//! Joining a capture `acp-probe` wrote in front of an Agent Client Protocol
//! agent to the ACP corpus.
//!
//! Unlike Codex, one ACP recording may span several agent processes: a
//! conversation resumed or loaded in a new process is the point of the
//! recording. Each process keeps its own line in `spawn.jsonl` and every
//! I/O line names the process it belongs to by its ordinal (`"1"`, `"2"`,
//! in start order), so a reader can tell where one process ended and the
//! next began. Each process's `us` counts from its own start.

use std::collections::BTreeMap;
use std::error::Error;
use std::path::{Path, PathBuf};

use chrono::Utc;
use redaction::{Redaction, RedactionSummary};
use semver::Version;
use serde_json::{Value, json};

use super::live::{live_scratch, secret_values, write_manifest};
use super::{IoDirection, IoEvent, Manifest, Recorded, load_recording, sanitize};

/// The client an ACP recording records: amux, whichever driver stood in for
/// it.
pub const CLIENT: &str = "amux";

/// Where the ACP corpus lives, beside the interpreter goldens that will
/// replay it.
pub fn corpus_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../interpret/fixtures/acp/live")
}

/// Variables holding credentials an ACP agent may read, whose values no
/// recording keeps.
const SECRETS: &[&str] = &[
    "XAI_API_KEY",
    "GROK_API_KEY",
    "GEMINI_API_KEY",
    "GOOGLE_API_KEY",
    "OPENCODE_API_KEY",
    "OPENAI_API_KEY",
    "ANTHROPIC_API_KEY",
];

/// Fields ACP agents fill with something that identifies this machine or
/// this installation of the agent rather than the conversation: Grok's
/// `initialize` names the host and gives the installation and process
/// their own ids.
const IDENTIFYING_KEYS: &[&str] = &["hostname", "agentId", "agentInstanceId", "installationId"];

/// The rules every ACP recording is redacted by: this machine's home, user
/// and host names, the scratch folders the capture's processes ran in,
/// credentials from the environment, and the identifying fields above.
pub fn rules(spawns: &[Value]) -> Redaction {
    let mut rules = Redaction::for_this_machine();
    rules.extra_paths = live_scratch(spawns);
    // macOS's temporary folder is a link into /private, and agents report
    // both spellings; the longer must be known too, or redacting the
    // shorter inside it leaves a stray `/private` behind.
    if let Ok(canonical) = std::env::temp_dir().canonicalize() {
        rules.extra_paths.push(canonical);
    }
    rules.secret_env = secret_values(SECRETS);
    rules.personal_identifier_keys = IDENTIFYING_KEYS.iter().map(|key| key.to_string()).collect();
    // The shared rules skip a user name shorter than four letters, which
    // would be cut out of ordinary words; but an agent's own shell output
    // (`ls -l`) names the owner as a whole word, and only whole words are
    // replaced here.
    if rules.user.is_none()
        && let Ok(user) = std::env::var("USER")
        && user.len() >= 3
    {
        rules.extra_personal_identifiers.push(user);
    }
    rules
}

/// What the manifest says about a recording, read from its sanitized lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Facts {
    pub version: Version,
    pub model: String,
    pub session_ids: Vec<String>,
}

/// Joins the capture in `capture` (its `spawn.jsonl` and `io.jsonl`) to
/// `root/<name>`, replacing any recording of that name. `provider` is the
/// agent's registry id and `driven_by` says who stood in for amux. The
/// recording is staged beside its destination and checked before it
/// replaces it.
pub fn join(
    capture: &Path,
    root: &Path,
    name: &str,
    provider: &str,
    driven_by: &str,
    extra: serde_json::Map<String, Value>,
) -> Result<PathBuf, Box<dyn Error>> {
    let spawns = read_rows(&capture.join("spawn.jsonl"))?;
    let rows = read_rows(&capture.join("io.jsonl"))?;
    let ordinals: BTreeMap<String, String> = spawns
        .iter()
        .filter_map(|spawn| spawn["process"].as_str())
        .enumerate()
        .map(|(at, process)| (process.to_owned(), (at + 1).to_string()))
        .collect();
    let rules = rules(&spawns);

    let mut account = RedactionSummary::default();
    let mut events = Vec::new();
    let mut processes = Vec::new();
    for row in &rows {
        let direction = match row["dir"].as_str() {
            Some("stdin") => IoDirection::Write,
            Some("stdout") => IoDirection::Read,
            _ => continue,
        };
        let process = row["process"]
            .as_str()
            .and_then(|process| ordinals.get(process))
            .ok_or_else(|| format!("an I/O line names no process the capture started: {row}"))?;
        let mut line = row["line"].as_str().unwrap_or_default().to_owned();
        if let Ok(mut frame) = serde_json::from_str::<Value>(&line) {
            account.personal_identifiers += scrub_account(&mut frame).personal_identifiers;
            line = frame.to_string();
        }
        events.push(IoEvent {
            us: row["us"].as_u64().unwrap_or(0),
            direction,
            line,
            transport_id: Some(CLIENT.to_owned()),
            session_id: None,
        });
        processes.push(process.clone());
    }
    let mut redaction = sanitize(&mut events, &rules);
    redaction.personal_identifiers += account.personal_identifiers;
    let facts = facts(&events)?;

    std::fs::create_dir_all(root)?;
    let stage = root.join(format!(".{name}.{}", std::process::id()));
    if stage.exists() {
        std::fs::remove_dir_all(&stage)?;
    }
    std::fs::create_dir_all(&stage)?;
    let mut io = String::new();
    for (event, process) in events.iter().zip(&processes) {
        let row = json!({
            "us": event.us,
            "dir": match event.direction {
                IoDirection::Write => "stdin",
                IoDirection::Read => "stdout",
            },
            "line": event.line,
            "transport_id": CLIENT,
            "process": process,
        });
        io.push_str(&row.to_string());
        io.push('\n');
    }
    std::fs::write(stage.join("io.jsonl"), io)?;
    let mut spawn_lines = String::new();
    for spawn in &spawns {
        let mut row = json!({
            "transport_id": CLIENT,
            "process": spawn["process"].as_str().and_then(|process| ordinals.get(process)),
            "command": spawn["command"],
            "argv": spawn["argv"],
            "cwd": spawn["cwd"],
        });
        redaction::redact_value(&mut row, &rules, &mut redaction);
        spawn_lines.push_str(&row.to_string());
        spawn_lines.push('\n');
    }
    std::fs::write(stage.join("spawn.jsonl"), spawn_lines)?;

    let mut manifest = Manifest::inventory(
        &stage,
        name,
        Recorded {
            provider: provider.to_owned(),
            version: facts.version,
            model: facts.model,
            at: Utc::now(),
        },
    )?;
    manifest.session_ids = facts.session_ids;
    manifest.redaction = redaction;
    manifest
        .provider_extra
        .insert("driven_by".to_owned(), driven_by.into());
    manifest.provider_extra.extend(extra);
    write_manifest(&stage.join("manifest.json"), &manifest)?;
    load_recording(&stage)?;

    let destination = root.join(name);
    if destination.exists() {
        std::fs::remove_dir_all(&destination)?;
    }
    std::fs::rename(&stage, &destination)?;
    Ok(destination)
}

/// Agents name the owner's subscription: Antigravity's `session/new`
/// answer in `_meta.antigravity.tier` (its name, id and upgrade link),
/// Grok's settings notification in `subscription_tier_display`. The shape
/// is kept, as an interpreter may read it, but not what it says about the
/// owner's account: every string under a key naming a tier or a
/// subscription is replaced. `tier: true` in Antigravity's `initialize`
/// is a capability flag and stays.
pub fn scrub_account(frame: &mut Value) -> RedactionSummary {
    let mut summary = RedactionSummary::default();
    scrub_tiers(frame, false, &mut summary);
    summary
}

fn names_account(key: &str) -> bool {
    let key = redaction::normalized_key(key);
    key.contains("tier") || key.contains("subscription")
}

fn scrub_tiers(value: &mut Value, inside: bool, summary: &mut RedactionSummary) {
    match value {
        Value::Object(object) => {
            for (key, value) in object.iter_mut() {
                scrub_tiers(value, inside || names_account(key), summary);
            }
        }
        Value::Array(values) => values
            .iter_mut()
            .for_each(|value| scrub_tiers(value, inside, summary)),
        Value::String(text) if inside && text != redaction::IDENTIFIER_PLACEHOLDER => {
            *text = redaction::IDENTIFIER_PLACEHOLDER.to_owned();
            summary.personal_identifiers += 1;
        }
        _ => {}
    }
}

/// The agent's version (`agentInfo.version` in its `initialize` answer, or
/// Grok's `_meta.agentVersion`), the model the conversation last ran on
/// (the newest `model` config option's value, else the newest
/// `models.currentModelId`), and the sessions the capture created or
/// reopened.
pub fn facts(events: &[IoEvent]) -> Result<Facts, Box<dyn Error>> {
    let mut version = None;
    let mut model = None;
    let mut session_ids = Vec::<String>::new();
    let mut note_session = |id: Option<&str>| {
        if let Some(id) = id
            && !session_ids.iter().any(|known| known == id)
        {
            session_ids.push(id.to_owned());
        }
    };
    for event in events {
        let Ok(frame) = serde_json::from_str::<Value>(&event.line) else {
            continue;
        };
        if event.direction == IoDirection::Write {
            if matches!(
                frame["method"].as_str(),
                Some("session/load" | "session/resume")
            ) {
                note_session(frame.pointer("/params/sessionId").and_then(Value::as_str));
            }
            continue;
        }
        let result = &frame["result"];
        if version.is_none() {
            version = result
                .pointer("/agentInfo/version")
                .or_else(|| result.pointer("/_meta/agentVersion"))
                .and_then(Value::as_str)
                .map(str::to_owned);
        }
        note_session(result["sessionId"].as_str());
        let options = result["configOptions"]
            .as_array()
            .or_else(|| frame.pointer("/params/update/configOptions")?.as_array());
        if let Some(current) = options.and_then(|options| {
            options
                .iter()
                .find(|option| option["category"] == "model")
                .and_then(|option| option["currentValue"].as_str())
        }) {
            model = Some(current.to_owned());
        } else if let Some(current) = result
            .pointer("/models/currentModelId")
            .and_then(Value::as_str)
        {
            model = Some(current.to_owned());
        }
    }
    Ok(Facts {
        version: Version::parse(&version.ok_or("the capture shows no agent version")?)?,
        // A capture that never opened a session (a catalogue probe, a
        // signed-out start) ran on no model.
        model: model.unwrap_or_else(|| "none".to_owned()),
        session_ids,
    })
}

fn read_rows(path: &Path) -> Result<Vec<Value>, Box<dyn Error>> {
    let text =
        std::fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))?;
    Ok(text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(serde_json::from_str::<Value>)
        .collect::<Result<Vec<_>, _>>()?)
}
