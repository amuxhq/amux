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
    /// The run's findings so far, each once, with the later findings
    /// recorded as its duplicates beneath it. Read it before you record one.
    Findings,
    /// Say what ground you are taking, so others go elsewhere.
    Claim { text: Vec<String> },
    /// A hunch with no evidence yet, for whoever has time.
    Idea { text: Vec<String> },
    /// Record a finding you captured (findings/NAME in your folder), once
    /// its finding.md is filled in.
    ///
    /// Once the run has any finding, say which this is: --duplicate-of the
    /// earlier finding that shows the same symptom, or --new when none
    /// does. A duplicate keeps its evidence but does not count as new.
    Finding {
        name: String,
        /// The earlier finding whose symptom this is, as `ledger findings`
        /// names it (TESTER/NAME).
        #[arg(long, value_name = "TESTER/NAME", conflicts_with = "new")]
        duplicate_of: Option<String>,
        /// No finding in `ledger findings` shows this symptom.
        #[arg(long)]
        new: bool,
    },
    /// Your closing sign-off: what you covered, your verdict for your
    /// persona, and what you could not do and why.
    Done { summary: Vec<String> },
}

/// The words a finding's severity is given in, worst first.
pub const SEVERITIES: [&str; 5] = [
    "data loss",
    "broken flow",
    "wrong state",
    "visual",
    "wording",
];

/// The template's lines a finding must fill in, and its sections that must
/// say something; "Suspected stage" may stay empty when the tester cannot
/// tell.
const FIELDS: [&str; 5] = ["Persona", "Severity", "Label", "Surfaces", "Topology"];
const SECTIONS: [&str; 4] = ["What I did", "What I expected", "What I saw", "Repro"];

/// What a filled-in `finding.md` records in the ledger.
#[derive(Debug, PartialEq, Eq)]
pub struct Checked {
    pub title: String,
    pub severity: String,
}

/// Checks a `finding.md` against the template: a title of its own, the
/// header lines filled in with a severity from [`SEVERITIES`], the sections
/// written, and no template comment left behind. Answers everything that
/// is missing at once, so one refusal is enough to fix it.
pub fn check_finding(text: &str, folder: &str) -> Result<Checked, Vec<String>> {
    let mut missing = Vec::new();
    let title = text
        .lines()
        .find_map(|line| line.strip_prefix("# "))
        .map(str::trim)
        .unwrap_or("")
        .to_owned();
    if title.is_empty() {
        missing.push("a `# ` title on the first line, saying the symptom in a sentence".to_owned());
    } else if title == folder || title == "{{name}}" {
        missing.push(format!(
            "a title of its own: `# {title}` is still the folder's name; say the symptom in a sentence"
        ));
    }
    let commented: Vec<String> = text
        .lines()
        .enumerate()
        .filter(|(_, line)| line.contains("<!--") || line.contains("-->"))
        .map(|(at, _)| (at + 1).to_string())
        .collect();
    if !commented.is_empty() {
        missing.push(format!(
            "the template's <!-- comments --> deleted (line {})",
            commented.join(", ")
        ));
    }
    let mut severity = String::new();
    for field in FIELDS {
        let prefix = format!("- **{field}:**");
        match text.lines().find_map(|line| line.strip_prefix(&prefix)) {
            None => missing.push(format!("its `{prefix}` line")),
            Some(value) => {
                let value = uncommented(value);
                if value.is_empty() {
                    missing.push(format!("`{prefix}` filled in"));
                } else if field == "Severity" {
                    let word = value.to_lowercase();
                    if SEVERITIES.contains(&word.as_str()) {
                        severity = word;
                    } else {
                        missing.push(format!(
                            "a severity that is one of {}, alone on its line (it says {value:?})",
                            SEVERITIES.join(", ")
                        ));
                    }
                }
            }
        }
    }
    for section in SECTIONS {
        match section_body(text, section) {
            None => missing.push(format!("its `## {section}` section")),
            Some(body) if uncommented(&body).is_empty() => {
                missing.push(format!("the `## {section}` section written"))
            }
            Some(_) => {}
        }
    }
    if missing.is_empty() {
        Ok(Checked { title, severity })
    } else {
        Err(missing)
    }
}

/// Text with any template comment taken out, trimmed.
fn uncommented(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("<!--") {
        out.push_str(&rest[..start]);
        rest = rest[start..]
            .find("-->")
            .map_or("", |end| &rest[start + end + 3..]);
    }
    out.push_str(rest);
    out.trim().to_owned()
}

/// The text under a `## ` heading, up to the next one.
fn section_body(text: &str, heading: &str) -> Option<String> {
    let mut lines = text.lines();
    lines.find(|line| line.trim_end() == format!("## {heading}"))?;
    Some(
        lines
            .take_while(|line| !line.starts_with("## "))
            .collect::<Vec<_>>()
            .join("\n"),
    )
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

fn ledger_entries() -> Result<Vec<Value>, String> {
    let text = std::fs::read_to_string(ledger_file()?).unwrap_or_default();
    Ok(text
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect())
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

/// One finding the run holds, new or a duplicate.
struct Found {
    handle: String,
    title: String,
    severity: String,
    duplicate_of: Option<String>,
}

fn found(entries: &[Value]) -> Vec<Found> {
    entries
        .iter()
        .filter(|entry| entry["kind"] == "finding")
        .filter_map(|entry| {
            Some(Found {
                handle: finding_handle(entry)?,
                title: entry["title"].as_str().unwrap_or("").to_owned(),
                severity: entry["severity"].as_str().unwrap_or("").to_owned(),
                duplicate_of: entry["duplicate_of"].as_str().map(str::to_owned),
            })
        })
        .collect()
}

/// The run's findings as `ledger findings` lists them: each new one once,
/// with the handles recorded as its duplicates under it.
fn describe_findings(all: &[Found], order: &[&Found]) -> String {
    let mut out = String::new();
    for finding in order {
        out.push_str(&format!(
            "{}  [{}]  {}\n",
            finding.handle,
            if finding.severity.is_empty() {
                "no severity"
            } else {
                &finding.severity
            },
            finding.title
        ));
        let copies: Vec<&str> = all
            .iter()
            .filter(|other| other.duplicate_of.as_deref() == Some(finding.handle.as_str()))
            .map(|other| other.handle.as_str())
            .collect();
        if !copies.is_empty() {
            out.push_str(&format!("    seen again by {}\n", copies.join(", ")));
        }
    }
    out
}

/// The words of a title that say what it is about.
fn words(text: &str) -> std::collections::BTreeSet<String> {
    const SMALL: [&str; 24] = [
        "the", "and", "for", "but", "not", "its", "it's", "with", "from", "that", "this", "when",
        "while", "after", "into", "than", "then", "are", "was", "has", "have", "does", "still",
        "one",
    ];
    text.split(|c: char| !c.is_alphanumeric() && c != '\'')
        .map(str::to_lowercase)
        .filter(|word| word.chars().count() > 2 && !SMALL.contains(&word.as_str()))
        .collect()
}

/// The run's new findings, the most like `title` first.
fn closest<'a>(news: &[&'a Found], title: &str) -> Vec<&'a Found> {
    let mine = words(title);
    let mut ranked: Vec<(usize, &Found)> = news
        .iter()
        .map(|finding| (mine.intersection(&words(&finding.title)).count(), *finding))
        .collect();
    ranked.sort_by_key(|(shared, _)| std::cmp::Reverse(*shared));
    ranked.into_iter().map(|(_, finding)| finding).collect()
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
    let words_of = |text: Vec<String>| -> Result<String, String> {
        let text = text.join(" ");
        if text.trim().is_empty() {
            Err("say what, in words".to_owned())
        } else {
            Ok(text)
        }
    };
    match command {
        LedgerCommand::Read => {
            let entries = ledger_entries()?;
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
        LedgerCommand::Findings => {
            let all = found(&ledger_entries()?);
            let news: Vec<&Found> = all.iter().filter(|f| f.duplicate_of.is_none()).collect();
            let text = if news.is_empty() {
                "no findings yet in this run".to_owned()
            } else {
                format!(
                    "{} so far in this run{}; a finding showing one of these symptoms is \
                     recorded with --duplicate-of TESTER/NAME\n{}",
                    count(news.len(), "finding"),
                    match all.len() - news.len() {
                        0 => String::new(),
                        copies => format!(" ({} more seen again)", copies),
                    },
                    describe_findings(&all, &news)
                )
            };
            let value = news
                .iter()
                .map(|f| json!({"finding": f.handle, "title": f.title, "severity": f.severity}))
                .collect();
            Ok(Outcome::new(text, Value::Array(value)))
        }
        LedgerCommand::Claim { text } => {
            let entry = append("claim", json!({"text": words_of(text)?}))?;
            Ok(Outcome::new("claimed", entry))
        }
        LedgerCommand::Idea { text } => {
            let entry = append("idea", json!({"text": words_of(text)?}))?;
            Ok(Outcome::new("idea recorded", entry))
        }
        LedgerCommand::Finding {
            name,
            duplicate_of,
            new,
        } => record_finding(folder, &name, duplicate_of, new),
        LedgerCommand::Done { summary } => {
            let entry = append("done", json!({"summary": words_of(summary)?}))?;
            Ok(Outcome::new("closing summary recorded", entry))
        }
    }
}

fn record_finding(
    folder: &Folder,
    name: &str,
    duplicate_of: Option<String>,
    new: bool,
) -> Result<Outcome, String> {
    let dir = folder.dir.join("findings").join(name);
    let file = dir.join("finding.md");
    if !file.is_file() {
        return Err(format!(
            "no finding at {}: `ctl capture {name}` first",
            dir.display()
        ));
    }
    let text = std::fs::read_to_string(&file).map_err(|error| error.to_string())?;
    let checked = check_finding(&text, name).map_err(|missing| {
        format!(
            "{} is not filled in yet (reference/qa/FINDING.md says how); it needs:\n  {}",
            file.display(),
            missing.join("\n  ")
        )
    })?;
    let entries = ledger_entries()?;
    if let Some(earlier) = entries
        .iter()
        .find(|entry| entry["kind"] == "finding" && entry["finding"] == json!(dir))
    {
        return Err(format!(
            "{} is already in the ledger, recorded at {}; a finding is recorded once \
             (capture again under a new name for another symptom)",
            finding_handle(earlier).unwrap_or_default(),
            earlier["time"].as_str().unwrap_or("?")
        ));
    }
    let all = found(&entries);
    let news: Vec<&Found> = all.iter().filter(|f| f.duplicate_of.is_none()).collect();
    let mut fields = json!({"finding": dir, "title": checked.title, "severity": checked.severity});
    let said = match duplicate_of {
        Some(earlier) => {
            let Some(original) = all.iter().find(|f| f.handle == earlier) else {
                return Err(format!(
                    "the ledger has no finding {earlier}; the run's findings are:\n{}",
                    if news.is_empty() {
                        "  none yet\n".to_owned()
                    } else {
                        describe_findings(&all, &news)
                    }
                ));
            };
            if let Some(root) = &original.duplicate_of {
                return Err(format!(
                    "{earlier} is itself recorded as a duplicate of {root}; name the original: \
                     --duplicate-of {root}"
                ));
            }
            fields["duplicate_of"] = Value::String(earlier.clone());
            format!(
                "finding {} recorded as a duplicate of {earlier} — {}",
                dir.display(),
                original.title
            )
        }
        None if !new && !news.is_empty() => {
            let ranked = closest(&news, &checked.title);
            return Err(format!(
                "the run has {} already; say whether this is one of them: \
                 --duplicate-of TESTER/NAME when one shows the same symptom, or --new when none \
                 does. The most like \"{}\" first:\n{}(all of them, with who saw each again: \
                 `testnet ctl ledger findings`)",
                count(news.len(), "finding"),
                checked.title,
                describe_findings(&all, &ranked[..ranked.len().min(8)])
            ));
        }
        None => format!("finding {} recorded as new", dir.display()),
    };
    let entry = append("finding", fields)?;
    Ok(Outcome::new(said, entry))
}

fn count(number: usize, noun: &str) -> String {
    format!("{number} {noun}{}", if number == 1 { "" } else { "s" })
}
