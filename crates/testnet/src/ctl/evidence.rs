//! What a session keeps: a capture of everything about a problem at one
//! moment, the session's own log, the dumps a capture holds, and the run's
//! shared ledger.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use clap::Subcommand;
use serde_json::{Value, json};

use super::session::{Folder, Outcome, amux_command, binary};
use super::{phone, tui};

/// The finding template a capture starts `finding.md` from.
const FINDING: &str = include_str!("../../../../qa/FINDING.md");

/// Names the run folder whose `ledger.jsonl` the testers share.
pub const RUN_ENV: &str = "AMUX_QA_RUN";
/// Names the tester writing to the ledger.
pub const TESTER_ENV: &str = "AMUX_QA_TESTER";

#[derive(Subcommand, Debug)]
pub enum LedgerCommand {
    /// Every entry so far, oldest first.
    Read,
    /// Say what ground you are taking, so others go elsewhere.
    Claim { text: Vec<String> },
    /// A hunch with no evidence yet, for whoever has time.
    Idea { text: Vec<String> },
    /// Record a finding you captured (findings/NAME in your folder).
    Finding {
        name: String,
        /// The earlier finding in the ledger whose symptom this is, as
        /// `ledger read` names it (TESTER/NAME). A duplicate keeps its
        /// evidence but does not count as new.
        #[arg(long, value_name = "TESTER/NAME")]
        duplicate_of: Option<String>,
    },
    /// Your closing summary: what you covered, what you would try next.
    Done { summary: Vec<String> },
}

pub(crate) fn capture(folder: &Folder, name: &str, note: Option<&str>) -> Result<Outcome, String> {
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(format!(
            "{name:?} is not a capture name: use letters, digits, '-' or '_'"
        ));
    }
    let root = folder.dir.join("findings");
    let dir = (1..)
        .map(|n| {
            if n == 1 {
                root.join(name)
            } else {
                root.join(format!("{name}-{n}"))
            }
        })
        .find(|dir| !dir.exists())
        .expect("a free folder");
    std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    let mut errors: Vec<String> = Vec::new();
    let mut files: Vec<PathBuf> = Vec::new();
    let reason = note.unwrap_or(name);

    let tester = std::env::var(TESTER_ENV).unwrap_or_default();
    let template = FINDING
        .replace("{{name}}", name)
        .replace("{{tester}}", &tester)
        .replace("{{note}}", note.unwrap_or(""));
    std::fs::write(dir.join("finding.md"), template).map_err(|error| error.to_string())?;

    let status = folder.status()?;
    std::fs::write(
        dir.join("topology.json"),
        serde_json::to_string_pretty(&status["topology"]).unwrap_or_default(),
    )
    .map_err(|error| error.to_string())?;

    // Screens first: they are what is on screen now.
    let screens = dir.join("screens");
    std::fs::create_dir_all(&screens).map_err(|error| error.to_string())?;
    for pane in tui::open_panes(folder) {
        match tui::shot(folder, &pane) {
            Ok((png, text)) => {
                for file in [png, text] {
                    if let Some(file_name) = file.file_name() {
                        let _ = std::fs::copy(&file, screens.join(file_name));
                    }
                    files.push(file);
                }
            }
            Err(error) => errors.push(format!("pane {pane}: {error}")),
        }
    }
    if phone::is_open(folder) {
        match phone::shot(folder) {
            Ok(png) => {
                let _ = std::fs::copy(&png, screens.join(png.file_name().unwrap_or_default()));
                files.push(png);
            }
            Err(error) => errors.push(format!("phone shot: {error}")),
        }
        match phone::query_text(folder) {
            Ok(text) => {
                let _ = std::fs::write(screens.join("phone-query.txt"), text);
            }
            Err(error) => errors.push(format!("phone query: {error}")),
        }
        let into = dir.join("dumps").join("phone");
        match phone::dump(folder, &into) {
            Ok(_) => {}
            Err(error) => errors.push(format!("phone dump: {error}")),
        }
    }
    // Every screenshot the session took before, by the log's account.
    let log = folder.dir.join("session.jsonl");
    for line in std::fs::read_to_string(&log).unwrap_or_default().lines() {
        let Ok(entry) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        for file in entry["files"].as_array().into_iter().flatten() {
            if let Some(path) = file.as_str().map(Path::new)
                && path.is_file()
                && let Some(file_name) = path.file_name()
            {
                let _ = std::fs::copy(path, screens.join(file_name));
            }
        }
    }
    let _ = std::fs::copy(&log, dir.join("session.jsonl"));

    // A dump of every host that is up, with the note as its reason.
    for host in status["hosts"].as_array().into_iter().flatten() {
        let host_name = host["name"].as_str().unwrap_or("?");
        if host["up"] != true {
            errors.push(format!("{host_name}: not dumped, its daemon is down"));
            continue;
        }
        let config = host["config"].as_str().unwrap_or_default();
        let output = amux_command(config)
            .args(["dump", "--reason", reason])
            .stdin(std::process::Stdio::null())
            .output();
        match output {
            Ok(output) if output.status.success() => {
                let said = String::from_utf8_lossy(&output.stdout);
                let bundle = said.lines().map(str::trim).find_map(|line| {
                    line.split_whitespace()
                        .map(Path::new)
                        .find(|word| word.is_dir())
                        .map(Path::to_path_buf)
                });
                match bundle {
                    Some(bundle) => {
                        let into = dir
                            .join("dumps")
                            .join(host_name)
                            .join(bundle.file_name().unwrap_or_default());
                        if let Err(error) = phone::copy_dir(&bundle, &into) {
                            errors.push(format!("{host_name}: copying its dump: {error}"));
                        }
                    }
                    None => errors.push(format!(
                        "{host_name}: amux dump named no bundle: {}",
                        said.trim()
                    )),
                }
            }
            Ok(output) => errors.push(format!(
                "{host_name}: amux dump failed: {}{}",
                String::from_utf8_lossy(&output.stdout).trim(),
                String::from_utf8_lossy(&output.stderr).trim()
            )),
            Err(error) => errors.push(format!("{host_name}: running amux dump: {error}")),
        }
    }

    // Each real agent's provider traffic, by the agent's name.
    let names = agent_names(folder, &status);
    let recordings = folder.dir.join("providers");
    for host_dir in read_dirs(&recordings) {
        let host_name = host_dir
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        for agent_dir in read_dirs(&host_dir) {
            let id = agent_dir
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            let named = if id == "host" {
                format!("{host_name}-daemon")
            } else {
                names.get(&id).cloned().unwrap_or(id)
            };
            if let Err(error) = phone::copy_dir(&agent_dir, &dir.join("providers").join(named)) {
                errors.push(format!("copying provider traffic: {error}"));
            }
        }
    }

    // Logs: the net's own and each process host's.
    let logs = dir.join("logs");
    std::fs::create_dir_all(&logs).map_err(|error| error.to_string())?;
    let _ = std::fs::copy(
        folder.net_dir().join("testnet.log"),
        logs.join("testnet.log"),
    );
    for host in status["hosts"].as_array().into_iter().flatten() {
        if host["process"] != true {
            continue;
        }
        let host_name = host["name"].as_str().unwrap_or("?");
        let data = Path::new(host["dir"].as_str().unwrap_or_default()).join("data");
        for log in ["daemon.log", "supervisor.log", "daemon-startup.log"] {
            if data.join(log).is_file() {
                let _ = std::fs::copy(data.join(log), logs.join(format!("{host_name}-{log}")));
            }
        }
    }

    if !errors.is_empty() {
        std::fs::write(dir.join("errors.txt"), errors.join("\n") + "\n")
            .map_err(|error| error.to_string())?;
    }
    let mut said = format!(
        "captured into {}\nfill in {} now, while it is on screen",
        dir.display(),
        dir.join("finding.md").display()
    );
    if !errors.is_empty() {
        said.push_str(&format!(
            "\nsome evidence is missing (see errors.txt):\n  {}",
            errors.join("\n  ")
        ));
    }
    Ok(Outcome::new(said, json!({"dir": dir, "errors": errors})).with_files(files))
}

/// Every agent id any host lists, mapped to its name.
fn agent_names(folder: &Folder, status: &Value) -> std::collections::BTreeMap<String, String> {
    let mut names = std::collections::BTreeMap::new();
    for host in status["hosts"].as_array().into_iter().flatten() {
        if host["up"] != true {
            continue;
        }
        let Ok(fleet) = folder.call(json!({"Inventory": {"host": host["name"]}})) else {
            continue;
        };
        for agent in fleet["agents"].as_array().into_iter().flatten() {
            if let (Some(id), Some(name)) = (agent["id"].as_str(), agent["name"].as_str()) {
                names.insert(id.to_owned(), name.to_owned());
            }
        }
    }
    for agent in status["agents"].as_array().into_iter().flatten() {
        if let (Some(id), Some(name)) = (agent["id"].as_str(), agent["name"].as_str()) {
            names.insert(id.to_owned(), name.to_owned());
        }
    }
    names
}

fn read_dirs(dir: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    dirs.sort();
    dirs
}

pub(crate) fn log(folder: &Folder, tail: Option<usize>) -> Result<Outcome, String> {
    let text = std::fs::read_to_string(folder.dir.join("session.jsonl"))
        .map_err(|_| "this folder has no session log yet".to_owned())?;
    let entries: Vec<Value> = text
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    let skip = tail.map_or(0, |tail| entries.len().saturating_sub(tail));
    let mut out = String::new();
    for entry in &entries[skip..] {
        let args: Vec<&str> = entry["args"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        out.push_str(&format!(
            "{} {} ctl {}\n",
            entry["time"].as_str().unwrap_or("?"),
            if entry["ok"] == true { "ok  " } else { "FAIL" },
            args.join(" ")
        ));
        let reply = entry["reply"].as_str().unwrap_or("");
        for line in reply.lines().take(6) {
            out.push_str(&format!("    {line}\n"));
        }
        if reply.lines().count() > 6 {
            out.push_str("    ...\n");
        }
    }
    Ok(Outcome::new(out, Value::Array(entries[skip..].to_vec())))
}

pub(crate) fn dump_show(
    _folder: &Folder,
    bundle: &Path,
    agent: Option<&str>,
) -> Result<Outcome, String> {
    let mut command = std::process::Command::new(binary("dump-show"));
    command.arg(bundle);
    if let Some(agent) = agent {
        command.arg("--agent").arg(agent);
    }
    let output = command.output().map_err(|error| {
        format!("running dump-show (build it with cargo build -p replay-support): {error}")
    })?;
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    if output.status.success() {
        Ok(Outcome::new(text, json!({"bundle": bundle})))
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
    }
}

fn ledger_file() -> Result<PathBuf, String> {
    std::env::var_os(RUN_ENV)
        .map(|run| PathBuf::from(run).join("ledger.jsonl"))
        .ok_or_else(|| format!("no QA run: {RUN_ENV} names none, so there is no ledger"))
}

/// How the ledger names a finding to people: its tester and its folder's
/// name, which is where it lives in the run folder (`<run>/TESTER/findings/NAME`).
fn finding_handle(entry: &Value) -> Option<String> {
    let path = Path::new(entry["finding"].as_str()?);
    Some(format!(
        "{}/{}",
        entry["tester"].as_str().unwrap_or("?"),
        path.file_name()?.to_string_lossy()
    ))
}

fn append(kind: &str, fields: Value) -> Result<Value, String> {
    let file = ledger_file()?;
    let mut entry = json!({
        "time": chrono::Utc::now().to_rfc3339(),
        "tester": std::env::var(TESTER_ENV).unwrap_or_else(|_| "someone".to_owned()),
        "kind": kind,
    });
    if let (Some(entry), Some(fields)) = (entry.as_object_mut(), fields.as_object()) {
        entry.extend(fields.clone());
    }
    let mut ledger = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&file)
        .map_err(|error| format!("opening {}: {error}", file.display()))?;
    // One write per entry: every tester in a run appends to this file, and
    // an entry written piecemeal could interleave with another's.
    ledger
        .write_all(format!("{entry}\n").as_bytes())
        .map_err(|error| error.to_string())?;
    Ok(entry)
}

pub(crate) fn ledger(folder: &Folder, command: LedgerCommand) -> Result<Outcome, String> {
    let words = |text: Vec<String>| -> Result<String, String> {
        let text = text.join(" ");
        if text.trim().is_empty() {
            Err("say what, in words".to_owned())
        } else {
            Ok(text)
        }
    };
    match command {
        LedgerCommand::Read => {
            let text = std::fs::read_to_string(ledger_file()?).unwrap_or_default();
            let entries: Vec<Value> = text
                .lines()
                .filter_map(|line| serde_json::from_str(line).ok())
                .collect();
            let mut out = String::new();
            for entry in &entries {
                let what = entry["text"]
                    .as_str()
                    .or_else(|| entry["summary"].as_str())
                    .map(str::to_owned)
                    .or_else(|| {
                        finding_handle(entry).map(|handle| {
                            format!(
                                "{handle}{}{}",
                                entry["title"]
                                    .as_str()
                                    .map(|title| format!(" — {title}"))
                                    .unwrap_or_default(),
                                entry["duplicate_of"]
                                    .as_str()
                                    .map(|earlier| format!(" (duplicates {earlier})"))
                                    .unwrap_or_default()
                            )
                        })
                    })
                    .unwrap_or_default();
                out.push_str(&format!(
                    "{} {:<8} {:<8} {}\n",
                    entry["time"]
                        .as_str()
                        .unwrap_or("?")
                        .get(11..19)
                        .unwrap_or("?"),
                    entry["tester"].as_str().unwrap_or("?"),
                    entry["kind"].as_str().unwrap_or("?"),
                    what.replace('\n', " ")
                ));
            }
            if out.is_empty() {
                out.push_str("the ledger is empty: nobody has claimed anything yet");
            }
            Ok(Outcome::new(out, Value::Array(entries)))
        }
        LedgerCommand::Claim { text } => {
            let entry = append("claim", json!({"text": words(text)?}))?;
            Ok(Outcome::new("claimed", entry))
        }
        LedgerCommand::Idea { text } => {
            let entry = append("idea", json!({"text": words(text)?}))?;
            Ok(Outcome::new("idea recorded", entry))
        }
        LedgerCommand::Finding { name, duplicate_of } => {
            let dir = folder.dir.join("findings").join(&name);
            if !dir.join("finding.md").is_file() {
                return Err(format!(
                    "no finding at {}: `ctl capture {name}` first",
                    dir.display()
                ));
            }
            let finding = std::fs::read_to_string(dir.join("finding.md")).unwrap_or_default();
            let title = finding
                .lines()
                .find_map(|line| line.strip_prefix("# "))
                .map(str::trim)
                .unwrap_or("")
                .to_owned();
            let severity = finding
                .lines()
                .find_map(|line| line.strip_prefix("- **Severity:**"))
                .map(str::trim)
                .unwrap_or("")
                .to_owned();
            let mut fields = json!({"finding": dir, "title": title, "severity": severity});
            if let Some(earlier) = duplicate_of {
                let text = std::fs::read_to_string(ledger_file()?).unwrap_or_default();
                let known: Vec<String> = text
                    .lines()
                    .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                    .filter_map(|entry| finding_handle(&entry))
                    .collect();
                if !known.contains(&earlier) {
                    return Err(format!(
                        "the ledger has no finding {earlier}; its findings are: {}",
                        if known.is_empty() {
                            "none yet".to_owned()
                        } else {
                            known.join(", ")
                        }
                    ));
                }
                fields["duplicate_of"] = Value::String(earlier);
            }
            let entry = append("finding", fields)?;
            Ok(Outcome::new(
                format!("finding {} recorded", dir.display()),
                entry,
            ))
        }
        LedgerCommand::Done { summary } => {
            let entry = append("done", json!({"summary": words(summary)?}))?;
            Ok(Outcome::new("closing summary recorded", entry))
        }
    }
}
