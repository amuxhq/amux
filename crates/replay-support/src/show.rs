//! A dump bundle as text: everything `node::dump` wrote, in sections an
//! agent triaging a bug can read and grep, with nothing in the bundle left
//! out. Records print as written: what this build cannot read still shows.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde_json::Value;
use wire::Kind;

use crate::bundle::{
    AgentRecord, Bundle, BundleError, CORE, CORE_CHECKPOINT, CORE_INPUTS, CORE_JOURNALS,
    CORE_STORE, CoreDump, journal_frames, read, segments,
};

/// The largest file of no known shape printed whole; anything larger is
/// named with its path for the reader to open.
const SHORT_TEXT: usize = 16 << 10;

#[derive(Debug, thiserror::Error)]
pub enum ShowError {
    #[error("{0} is not a dump bundle: it has no manifest.json")]
    NotABundle(PathBuf),
    #[error(transparent)]
    Bundle(#[from] BundleError),
    #[error("no agent in the bundle matches {wanted:?}; it holds {held}")]
    NoAgent { wanted: String, held: String },
}

/// The bundle at `dir` as text; with `agent`, only the agents whose id
/// starts with it.
pub fn show(named: &Path, agent: Option<&str>) -> Result<String, ShowError> {
    let (dir, _unpacked) = crate::bundle::locate(named)?;
    let dir = dir.as_path();
    let manifest_path = dir.join("manifest.json");
    if !manifest_path.is_file() {
        return Err(ShowError::NotABundle(dir.to_owned()));
    }
    let manifest: Value =
        serde_json::from_slice(&read(&manifest_path)?).map_err(|error| BundleError::Decode {
            path: manifest_path.clone(),
            what: "JSON manifest",
            error: error.to_string(),
        })?;
    let bundle = Bundle::open(dir)?;
    let chosen: Vec<&AgentRecord> = bundle
        .agents
        .iter()
        .filter(|dump| agent.is_none_or(|wanted| dump.id.starts_with(wanted)))
        .collect();
    if let Some(wanted) = agent
        && chosen.is_empty()
    {
        let held = bundle
            .agents
            .iter()
            .map(|dump| dump.id.clone())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(ShowError::NoAgent {
            wanted: wanted.to_owned(),
            held: if held.is_empty() { "none".into() } else { held },
        });
    }

    let mut out = String::new();
    let mut shown = BTreeSet::from([manifest_path]);
    summary(&mut out, &manifest);
    for dump in &chosen {
        let entry = manifest["agents"]
            .as_array()
            .and_then(|agents| agents.iter().find(|entry| entry["agent_id"] == *dump.id));
        agent_section(&mut out, &mut shown, dir, dump, entry)?;
    }
    for client in &bundle.clients {
        client_section(&mut out, &mut shown, dir, client);
    }
    if let Some(core) = &bundle.core {
        core_section(&mut out, &mut shown, dir, core, &manifest["core"])?;
    }

    // What no section above showed, so nothing in the bundle is invisible:
    // our logs, a client's parts.
    let skipped: Vec<PathBuf> = bundle
        .agents
        .iter()
        .filter(|dump| !chosen.iter().any(|chosen| chosen.id == dump.id))
        .map(|dump| dir.join("agents").join(&dump.id))
        .collect();
    let mut rest = Vec::new();
    walk(dir, &mut rest);
    rest.retain(|path| {
        !shown.contains(path) && !skipped.iter().any(|skipped| path.starts_with(skipped))
    });
    let _ = writeln!(out, "\n== other files ({}) ==", rest.len());
    for path in rest {
        // Named with `/` on every platform, so a printout reads the same
        // from a dump taken on Windows.
        let name = path
            .strip_prefix(dir)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        file(&mut out, &name, &path)?;
    }
    Ok(out)
}

fn summary(out: &mut String, manifest: &Value) {
    let text = |key: &str| match &manifest[key] {
        Value::String(text) => text.clone(),
        Value::Null => "-".into(),
        other => other.to_string(),
    };
    let _ = writeln!(out, "== dump {} ==", text("dump_id"));
    let _ = writeln!(out, "reason {}", manifest["reason"]);
    let _ = writeln!(out, "trigger {}", text("trigger"));
    let _ = writeln!(
        out,
        "created {}",
        time(manifest["created_at_ms"].as_i64().unwrap_or_default())
    );
    for key in ["host", "profile", "generation", "daemon_version"] {
        let _ = writeln!(out, "{key} {}", text(key));
    }
    match manifest["errors"].as_array() {
        Some(errors) if !errors.is_empty() => {
            for error in errors {
                let _ = writeln!(
                    out,
                    "error {}",
                    error.as_str().unwrap_or(&error.to_string())
                );
            }
        }
        _ => {
            let _ = writeln!(out, "errors none");
        }
    }
    let _ = writeln!(
        out,
        "agents {}",
        manifest["agents"].as_array().map_or(0, Vec::len)
    );
}

/// A client's session: the state its trace began from, then every event
/// its driver saw, one line each.
fn client_section(
    out: &mut String,
    shown: &mut BTreeSet<PathBuf>,
    dir: &Path,
    client: &crate::bundle::ClientDump,
) {
    let _ = writeln!(out, "\n== client session {} ==", client.agent);
    let sessions = dir.join("client").join("sessions").join(&client.agent);
    for file in ["checkpoint", "events"] {
        shown.insert(sessions.join(file));
    }
    let history = match &client.history {
        Ok(history) => history,
        Err(why) => {
            let _ = writeln!(out, "unreadable: {why}");
            return;
        }
    };
    let start = &history.start;
    let rows = start
        .transcript
        .as_ref()
        .map(|transcript| transcript.items.as_slice())
        .unwrap_or_default();
    let _ = writeln!(
        out,
        "-- checkpoint --\nconnection {:?} caught_up {} generation {} epoch {} cap {} following {}",
        start.connection(),
        start.caught_up,
        start
            .generation
            .map_or_else(|| "-".to_owned(), |generation| generation.to_string()),
        start.epoch,
        start.cap,
        start.following
    );
    let _ = writeln!(
        out,
        "snapshot {} inputs {} pending {} rows {}",
        start.snapshot.as_ref().map_or_else(
            || "none".to_owned(),
            |snapshot| format!("rev {}", snapshot.revision)
        ),
        start.inputs.len(),
        start.pending.is_some(),
        rows.len()
    );
    let kind = start
        .agent
        .as_ref()
        .map_or(Kind::Unspecified, wire::Agent::kind);
    for item in rows {
        // What the session model concluded from the body, which is all a
        // record keeps of it.
        let kind = Kind::try_from(item.kind)
            .ok()
            .filter(|kind| *kind != Kind::Unspecified)
            .unwrap_or(kind);
        let class = ui_state::ItemBody::decode(kind, &item.body).class();
        let _ = writeln!(
            out,
            "  {} order {} rev {} {class:?}",
            item.key, item.order, item.revision
        );
    }
    let _ = writeln!(out, "-- events ({}) --", history.events.len());
    for traced in &history.events {
        let what = match &traced.event {
            Some(wire::dump::client_traced::Event::Msg(msg)) => {
                match ui_state::Msg::from_wire(msg.clone()) {
                    Ok(msg) => crate::replay::message_kind(&msg),
                    Err(why) => format!("unreadable message: {why}"),
                }
            }
            Some(wire::dump::client_traced::Event::Driver(driver)) => format!("driver {driver}"),
            None => "nothing".to_owned(),
        };
        let _ = writeln!(
            out,
            "{} @{} {}B {what}",
            traced.seq,
            time(traced.at_ms),
            traced.size
        );
    }
}

fn core_section(
    out: &mut String,
    shown: &mut BTreeSet<PathBuf>,
    dir: &Path,
    core: &CoreDump,
    entry: &Value,
) -> Result<(), BundleError> {
    let at = dir.join(CORE);
    let _ = writeln!(out, "\n== daemon core ==");
    if !entry.is_null() {
        let _ = writeln!(out, "manifest {entry}");
    }
    let _ = writeln!(
        out,
        "-- checkpoint ({} bytes of state) --",
        core.checkpoint.len()
    );
    shown.insert(at.join(CORE_CHECKPOINT));

    // The lines as written, as the facts are: what this build cannot read
    // is still shown.
    let text = String::from_utf8_lossy(&read(&at.join(CORE_INPUTS))?).into_owned();
    let lines: Vec<&str> = text.lines().filter(|line| !line.is_empty()).collect();
    let reads = core
        .inputs
        .iter()
        .filter(|line| matches!(line, daemon_core::Recorded::Read { .. }))
        .count();
    let _ = writeln!(
        out,
        "-- inputs ({} inputs, {reads} journal reads) --",
        core.inputs.len() - reads
    );
    for line in &lines {
        let _ = writeln!(out, "{line}");
    }
    if lines.len() > core.inputs.len() {
        let _ = writeln!(
            out,
            "this build reads the first {} lines only",
            core.inputs.len()
        );
    }
    shown.insert(at.join(CORE_INPUTS));

    let _ = writeln!(out, "-- store as of the checkpoint --");
    match core_store_rows(&core.store) {
        Ok(rows) => {
            for row in rows {
                let _ = writeln!(
                    out,
                    "agent {} {} host {} lifecycle {} ingest_cursor={} next_revision={} \
                     source_cursor={}",
                    id(&row.agent.agent),
                    Value::String(row.name.clone()),
                    id(&row.agent.host),
                    row.lifecycle,
                    row.ingest_cursor,
                    row.next_revision,
                    row.source_cursor
                );
            }
        }
        Err(error) => {
            let _ = writeln!(out, "not read: {error}");
        }
    }
    shown.insert(at.join(CORE_STORE));

    for (agent, held) in &core.journals {
        let frames: usize = held
            .iter()
            .map(|(_, bytes)| journal_frames(bytes).len())
            .sum();
        let _ = writeln!(
            out,
            "-- journal of {} ({} segments, {frames} frames from the checkpoint's cursor) --",
            id(&agent.0),
            held.len()
        );
        let dir = at.join(CORE_JOURNALS).join(id(&agent.0));
        for (start, bytes) in held {
            for step in journal_frames(bytes) {
                let _ = writeln!(
                    out,
                    "segment {start}: {} items, {} appends, snapshot {}",
                    step.items.len(),
                    step.appends.len(),
                    step.snapshot.is_some()
                );
            }
        }
        for (_, path) in segments(&dir)? {
            shown.insert(path);
        }
    }
    Ok(())
}

/// The agent rows of a store file, read from a copy so the bundle is left
/// as it was.
fn core_store_rows(path: &Path) -> Result<Vec<store::AgentRow>, String> {
    let scratch = tempfile::tempdir().map_err(|error| error.to_string())?;
    let copy = scratch.path().join(CORE_STORE);
    std::fs::copy(path, &copy).map_err(|error| error.to_string())?;
    let store = store::Sqlite::open(&copy, Vec::new()).map_err(|error| error.to_string())?;
    store.agents().map_err(|error| error.to_string())
}

/// An agent's record: what its manifest entry says, the shape of the
/// record, and the record as written.
fn agent_section(
    out: &mut String,
    shown: &mut BTreeSet<PathBuf>,
    dir: &Path,
    dump: &AgentRecord,
    entry: Option<&Value>,
) -> Result<(), BundleError> {
    let path = dir
        .join(crate::bundle::AGENTS)
        .join(&dump.id)
        .join(crate::bundle::AGENT_RECORD);
    let record = &dump.record;
    let _ = writeln!(
        out,
        "\n== agent {} ({}) ==",
        dump.id,
        record["settings"]["kind"]
            .as_str()
            .map_or("-", |kind| kind.trim_start_matches("KIND_"))
    );
    if let Some(entry) = entry {
        let _ = writeln!(out, "manifest {entry}");
    }
    let segments = record["segments"].as_array().map_or(&[][..], Vec::as_slice);
    let events: usize = segments
        .iter()
        .map(|segment| segment["events"].as_array().map_or(0, Vec::len))
        .sum();
    let broken: Vec<&Value> = segments
        .iter()
        .flat_map(|segment| segment["events"].as_array().map_or(&[][..], Vec::as_slice))
        .filter(|event| !event["broken"].is_null())
        .map(|event| &event["broken"])
        .collect();
    let _ = writeln!(
        out,
        "{} segments, {events} events, {} broken checks",
        segments.len(),
        broken.len()
    );
    for check in broken {
        let _ = writeln!(out, "broken {check}");
    }
    file(out, "record", &path)?;
    shown.insert(path);
    Ok(())
}

/// A file of no known shape: whole when it is short text, else its path.
fn file(out: &mut String, name: &str, path: &Path) -> Result<(), BundleError> {
    let bytes = read(path)?;
    let _ = writeln!(out, "-- {name} ({} bytes) --", bytes.len());
    match std::str::from_utf8(&bytes) {
        Ok(text) if bytes.len() <= SHORT_TEXT => {
            out.push_str(text);
            if !text.is_empty() && !text.ends_with('\n') {
                out.push('\n');
            }
        }
        Ok(_) => {
            let _ = writeln!(out, "text, not printed: {}", path.display());
        }
        Err(_) => {
            let _ = writeln!(out, "binary, not printed: {}", path.display());
        }
    }
    Ok(())
}

/// Every file under `dir`, sorted.
fn walk(dir: &Path, found: &mut Vec<PathBuf>) {
    let Ok(listing) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = listing
        .filter_map(|entry| Some(entry.ok()?.path()))
        .collect();
    paths.sort();
    for path in paths {
        if path.is_dir() {
            walk(&path, found);
        } else {
            found.push(path);
        }
    }
}

/// A 16-byte id as a UUID, anything else as hex.
fn id(bytes: &[u8]) -> String {
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    match bytes.len() {
        0 => "-".into(),
        16 => format!(
            "{}-{}-{}-{}-{}",
            &hex[..8],
            &hex[8..12],
            &hex[12..16],
            &hex[16..20],
            &hex[20..]
        ),
        _ => hex,
    }
}

/// Milliseconds since the epoch, with the UTC time they name.
fn time(ms: i64) -> String {
    match chrono::DateTime::from_timestamp_millis(ms) {
        Some(at) if ms != 0 => format!("{ms} ({})", at.format("%Y-%m-%dT%H:%M:%S%.3fZ")),
        _ => ms.to_string(),
    }
}
