//! A dump bundle as text: everything `node::dump` wrote, in sections an
//! agent triaging a bug can read and grep, with nothing in the bundle left
//! out.
//!
//! Journaled steps print in the interpreter's emission-golden format, so a
//! journal compares line for line with a replay of the facts behind it;
//! when the bundle carries facts, that replay is printed too, or only
//! confirmed when it matches the end of the journal. Facts lines and
//! checkpoints are JSON already and print as written, unparsed, so a line
//! the redactor replaced with `{"event":"unreadable"}` hides nothing after
//! it.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use interpret::Interpreter;
use interpret::claude_pty::ClaudePty;
use interpret::claude_sdk::ClaudeSdk;
use interpret::codex::Codex;
use serde_json::Value;
use wire::{Agent, AgentSpec, Item, Kind, Step};

use crate::bundle::{
    AgentDump, Bundle, BundleError, CORE, CORE_CHECKPOINT, CORE_INPUTS, CORE_STORE, CoreDump,
    decode, read, segments,
};

/// The largest file of no known shape printed whole; anything larger is
/// named with its path for the reader to open.
const SHORT_TEXT: usize = 16 << 10;

/// Every frame of a journal or replay carries this label: a journal
/// records no event, and a shared label keeps the two comparable.
const LABEL: &str = "step";

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
/// starts with it or whose name is it.
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
    let chosen: Vec<&AgentDump> = bundle
        .agents
        .iter()
        .filter(|dump| {
            agent.is_none_or(|wanted| dump.id.starts_with(wanted) || dump.row.name == wanted)
        })
        .collect();
    if let Some(wanted) = agent
        && chosen.is_empty()
    {
        let held = bundle
            .agents
            .iter()
            .map(|dump| format!("{} ({})", dump.id, dump.row.name))
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
    // the daemon's log, a client's parts.
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

/// Steps in the emission-golden format for an agent of `kind`, numbered
/// from `first`. None for a kind this build does not interpret.
pub fn render_steps(kind: Kind, first: usize, steps: &[Step]) -> Option<String> {
    let labelled = steps.iter().map(|step| (LABEL, step));
    Some(match kind {
        Kind::ClaudePty => interpret::render_steps::<ClaudePty>(first, labelled),
        Kind::ClaudeSdk => interpret::render_steps::<ClaudeSdk>(first, labelled),
        Kind::Codex => interpret::render_steps::<Codex>(first, labelled),
        Kind::Unspecified => return None,
    })
}

fn render_item(kind: Kind, item: &Item) -> String {
    fn of<I: Interpreter>(item: &Item) -> String {
        interpret::render_item::<I>(item)
    }
    match kind {
        Kind::ClaudePty => of::<ClaudePty>(item),
        Kind::ClaudeSdk => of::<ClaudeSdk>(item),
        Kind::Codex => of::<Codex>(item),
        Kind::Unspecified => format!(
            "item {} at={} text={}",
            item.key,
            item.at_ms,
            Value::String(item.text.clone())
        ),
    }
}

fn summary(out: &mut String, manifest: &Value) {
    let text = |key: &str| match &manifest[key] {
        Value::String(text) => text.clone(),
        Value::Null => "-".into(),
        other => other.to_string(),
    };
    let _ = writeln!(out, "== dump {} ==", text("dump_id"));
    let _ = writeln!(out, "reason {}", manifest["reason"]);
    let _ = writeln!(out, "automatic {}", text("automatic"));
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

/// The daemon core's part: its checkpoint, every recorded line as written,
/// and each agent row of the store as of the checkpoint, with how far its
/// journal was committed.
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
    for item in rows {
        let _ = writeln!(
            out,
            "  {} order {} rev {}",
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
        let _ = writeln!(out, "{} @{} {what}", traced.seq, time(traced.at_ms));
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
    let _ = match daemon_core::Checkpoint::from_bytes(&core.checkpoint) {
        Ok(checkpoint) => writeln!(
            out,
            "-- checkpoint (schema {}, {} bytes of state) --",
            checkpoint.schema,
            checkpoint.state.len()
        ),
        Err(error) => writeln!(out, "-- checkpoint ({error}) --"),
    };
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

fn agent_section(
    out: &mut String,
    shown: &mut BTreeSet<PathBuf>,
    dir: &Path,
    dump: &AgentDump,
    entry: Option<&Value>,
) -> Result<(), BundleError> {
    let at = dir.join("agents").join(&dump.id);
    let kind = dump.row.kind();
    let _ = writeln!(
        out,
        "\n== agent {} ({}, {}) ==",
        dump.id,
        dump.row.name,
        named(kind.as_str_name(), "KIND_")
    );
    if let Some(entry) = entry {
        let _ = writeln!(out, "manifest {entry}");
    }

    let _ = writeln!(out, "-- row --");
    row(out, &dump.row);
    shown.insert(at.join("row.pb"));

    let _ = writeln!(out, "-- journal ({} frames) --", dump.journal.len());
    match render_steps(kind, 0, &dump.journal) {
        Some(text) => out.push_str(&text),
        None => {
            let _ = writeln!(out, "not rendered: no interpreter for this kind");
        }
    }
    for (_, path) in segments(&at.join("journal"))? {
        shown.insert(path);
    }
    if !dump.facts.is_empty() {
        replay(out, dump);
    }

    if let Some(slice) = &dump.store {
        let _ = writeln!(out, "-- store ({} rows) --", slice.items.len());
        store(out, kind, slice);
        shown.insert(at.join("store.pb"));
    }

    let facts_dir = at.join("part").join("facts");
    for (_, path) in segments(&facts_dir)? {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let text = String::from_utf8_lossy(&read(&path)?).into_owned();
        let lines: Vec<&str> = text.lines().filter(|line| !line.is_empty()).collect();
        let _ = writeln!(out, "-- facts {name} ({} lines) --", lines.len());
        let checkpoint = facts_dir.join(format!("{name}.checkpoint"));
        if checkpoint.is_file() {
            let bytes = read(&checkpoint)?;
            let _ = writeln!(out, "checkpoint {}", String::from_utf8_lossy(&bytes).trim());
            shown.insert(checkpoint);
        } else {
            let _ = writeln!(out, "checkpoint none");
        }
        for line in lines {
            let _ = writeln!(out, "{line}");
        }
        shown.insert(path);
    }

    let mut part = Vec::new();
    walk(&at.join("part"), &mut part);
    part.retain(|path| !shown.contains(path));
    for path in part {
        let name = path
            .strip_prefix(at.join("part"))
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        if name.starts_with("spec.")
            && let Ok(spec) = decode::<AgentSpec>(&path, "wire.AgentSpec")
        {
            let _ = writeln!(out, "-- part {name} (wire.AgentSpec) --");
            spec_lines(out, spec);
        } else {
            file(out, &format!("part {name}"), &path)?;
        }
        shown.insert(path);
    }
    Ok(())
}

fn row(out: &mut String, agent: &Agent) {
    let mut line = |key: &str, value: String| {
        let _ = writeln!(out, "{key} {value}");
    };
    line("agent_id", id(&agent.agent_id));
    line("host_id", id(&agent.host_id));
    line("kind", named(agent.kind().as_str_name(), "KIND_"));
    line("name", Value::String(agent.name.clone()).to_string());
    line("cwd", agent.cwd.clone());
    line(
        "parent",
        agent.parent.as_ref().map_or("-".into(), |parent| {
            format!("{} on {}", id(&parent.agent_id), id(&parent.host_id))
        }),
    );
    line("created", time(agent.created_at_ms));
    line(
        "lifecycle",
        named(agent.lifecycle().as_str_name(), "LIFECYCLE_"),
    );
    line(
        "exit_cause",
        format!(
            "{}{}",
            named(agent.exit_cause().as_str_name(), "EXIT_CAUSE_"),
            agent
                .exit_detail
                .as_ref()
                .map(|detail| format!(" {detail:?}"))
                .unwrap_or_default()
        ),
    );
    line(
        "phase",
        format!(
            "{} since {}",
            named(agent.phase().as_str_name(), "PHASE_"),
            time(agent.phase_since_ms)
        ),
    );
    line(
        "working_on",
        agent
            .working_on
            .as_ref()
            .map_or("-".into(), |text| Value::String(text.clone()).to_string()),
    );
    line("incarnation", agent.incarnation.to_string());
    line(
        "git",
        agent.git.as_ref().map_or("-".into(), |git| {
            let totals = |totals: &Option<wire::ChangeTotals>| {
                totals.as_ref().map_or("-".to_owned(), |totals| {
                    format!("{}f+{}-{}", totals.files, totals.added, totals.removed)
                })
            };
            format!(
                "branch={} base={} uncommitted={} on_branch={}",
                git.branch.as_deref().unwrap_or("-"),
                git.base_branch.as_deref().unwrap_or("-"),
                totals(&git.uncommitted),
                totals(&git.on_branch)
            )
        }),
    );
}

/// A spec with its ids readable and its environment in a stable order; the
/// rest as its debug form, on one line.
fn spec_lines(out: &mut String, mut spec: AgentSpec) {
    let env: BTreeMap<_, _> = std::mem::take(&mut spec.provider_env).into_iter().collect();
    let _ = writeln!(out, "agent_id {}", id(&std::mem::take(&mut spec.agent_id)));
    let _ = writeln!(
        out,
        "profile_id {}",
        id(&std::mem::take(&mut spec.profile_id))
    );
    let _ = writeln!(out, "provider_env {env:?}");
    let _ = writeln!(out, "{spec:?}");
}

/// The facts replayed through the interpreter. Journal and facts ring both
/// end at the dump, so the replay is numbered to end where the journal
/// does, and compared with the journal's frames it overlaps.
fn replay(out: &mut String, dump: &AgentDump) {
    let replayed = match dump.replay_facts() {
        Ok(replayed) => replayed,
        Err(error) => {
            let _ = writeln!(out, "-- replay --\nnot replayed: {error}");
            return;
        }
    };
    let kind = dump.row.kind();
    let overlap = replayed.len().min(dump.journal.len());
    let first = dump.journal.len() - overlap;
    let ours = render_steps(kind, first, &replayed[replayed.len() - overlap..]);
    let theirs = render_steps(kind, first, &dump.journal[first..]);
    let _ = writeln!(out, "-- replay ({} steps) --", replayed.len());
    if overlap == replayed.len() && ours == theirs {
        let _ = writeln!(
            out,
            "the replay of the facts is the journal from frame {first} on, line for line"
        );
        return;
    }
    let _ = writeln!(
        out,
        "the replay of the facts differs from the journal; frames numbered to end with it"
    );
    if let (Some(ours), Some(theirs)) = (&ours, &theirs) {
        let (mut frame, mut lines) = ("-", (ours.lines(), theirs.lines()));
        loop {
            match (lines.0.next(), lines.1.next()) {
                (Some(a), Some(b)) if a == b => {
                    if let Some(header) = a.strip_prefix("## ") {
                        frame = header.split(' ').next().unwrap_or("-");
                    }
                }
                (None, None) => break,
                (a, b) => {
                    let _ = writeln!(
                        out,
                        "first difference in frame {frame}: journal {:?}, replay {:?}",
                        b.unwrap_or("<end>"),
                        a.unwrap_or("<end>")
                    );
                    break;
                }
            }
        }
    }
    let first = dump.journal.len().saturating_sub(replayed.len());
    if let Some(text) = render_steps(kind, first, &replayed) {
        out.push_str(&text);
    }
}

fn store(out: &mut String, kind: Kind, slice: &Step) {
    for item in &slice.items {
        let _ = writeln!(
            out,
            "order={} revision={} {}",
            item.order,
            item.revision,
            render_item(kind, item)
        );
    }
    match &slice.snapshot {
        Some(snapshot) => {
            let _ = writeln!(out, "snapshot revision={}", snapshot.revision);
            let alone = Step {
                snapshot: Some(snapshot.clone()),
                ..Step::default()
            };
            // The snapshot's lines without the frame header.
            if let Some(text) = render_steps(kind, 0, &[alone]) {
                for line in text.lines().skip(1) {
                    let _ = writeln!(out, "{line}");
                }
            }
        }
        None => {
            let _ = writeln!(out, "snapshot none");
        }
    }
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

fn named(name: &str, prefix: &str) -> String {
    name.trim_start_matches(prefix).to_owned()
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
