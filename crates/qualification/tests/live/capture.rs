//! A run's provider traffic, recorded through the probes so each
//! scenario's recording can join the corpus.
//!
//! The daemon finds the provider command on its PATH. The lane puts a
//! folder first on that PATH holding the probe under the provider's name
//! (`claude` is claude-probe, `codex` is codex-probe), told by its
//! environment to run the real provider and record what crosses: headless
//! Claude's stream lines, terminal Claude's screen, keys, hooks and
//! transcript rows, Codex's app-server messages. Each provider process is
//! one spawn, a line in the capture's `spawn.jsonl` with its arguments and
//! folder, and its lines in `io.jsonl` carry its `process` id.
//!
//! A scenario's recording is the spawns started in its own project folder:
//! `target/live/recordings/<kind>/<scenario>/` gets their `io.jsonl` and
//! `spawn.jsonl` and a `fixture.json` that replays them through the
//! interpreter. A passing one joins the live captures beside the spec
//! corpus through the probe's sanitizing `join` command (`claude-probe join`,
//! `codex-probe join`); the recordings here are unsanitized and never
//! committed.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use wire::Kind;

use super::Scenario;

pub struct Capture {
    kind: Kind,
    /// Where the proxies write.
    dir: PathBuf,
    /// Where each scenario's recording goes.
    out: PathBuf,
}

/// The probe binaries, built beside this test binary.
fn probe(name: &str) -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|error| error.to_string())?;
    // target/<profile>/deps/<binary>
    let target = exe
        .ancestors()
        .nth(2)
        .ok_or("the test binary is not in a target directory")?;
    let mut args = vec!["build", "--locked"];
    if target.file_name().and_then(|name| name.to_str()) == Some("release") {
        args.push("--release");
    }
    args.extend(match name {
        "codex-probe" => ["-p", "codex-specs", "--bin", "codex-probe"],
        _ => ["-p", "claude-specs", "--bin", "claude-probe"],
    });
    let status = provider_fakes::cargo::command()
        .args(&args)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .stdout(std::process::Stdio::null())
        .status()
        .map_err(|error| format!("cargo: {error}"))?;
    if !status.success() {
        return Err(format!("building {name} failed"));
    }
    Ok(target.join(name))
}

/// The first `command` on the PATH.
fn on_path(command: &str) -> Result<PathBuf, String> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .map(|dir| dir.join(command))
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| format!("no {command} on the PATH"))
}

/// The recordings folder for `kind`, next to the test binary's target
/// directory.
pub fn recordings_dir(tag: &str) -> PathBuf {
    let exe = std::env::current_exe().expect("the test binary's path");
    let target = exe
        .ancestors()
        .nth(3)
        .map(Path::to_owned)
        .unwrap_or_else(|| PathBuf::from("target"));
    target.join("live").join("recordings").join(tag)
}

impl Capture {
    /// Puts the probe first on the PATH as `command` and returns the
    /// environment the daemon needs for it.
    pub fn install(
        root: &Path,
        kind: Kind,
        tag: &str,
        command: &str,
    ) -> Result<(Capture, Vec<(String, OsString)>), String> {
        let real = on_path(command)?;
        let (probe, prefix) = match kind {
            Kind::Codex => (probe("codex-probe")?, "CODEX"),
            _ => (probe("claude-probe")?, "CLAUDE"),
        };
        let shim = root.join("shim");
        let dir = root.join("capture");
        for folder in [&shim, &dir] {
            std::fs::create_dir_all(folder).map_err(|error| error.to_string())?;
        }
        std::os::unix::fs::symlink(&probe, shim.join(command)).map_err(|e| e.to_string())?;
        let path = std::env::var_os("PATH").unwrap_or_default();
        let path = std::env::join_paths(std::iter::once(shim).chain(std::env::split_paths(&path)))
            .map_err(|error| error.to_string())?;
        let env = vec![
            ("PATH".to_owned(), path),
            (format!("{prefix}_CAPTURE_PROXY"), "1".into()),
            (format!("{prefix}_CAPTURE_DIR"), dir.clone().into()),
            (format!("{prefix}_REAL_PATH"), real.into()),
        ];
        let out = recordings_dir(tag);
        let _ = std::fs::remove_dir_all(&out);
        std::fs::create_dir_all(&out).map_err(|error| error.to_string())?;
        Ok((Capture { kind, dir, out }, env))
    }

    /// Writes the recording of every provider process started in `project`,
    /// with a fixture replaying it after `prelude`, and returns the
    /// fixture's path.
    pub fn collect(
        &self,
        scenario: Scenario,
        project: &Path,
        prelude: Vec<serde_json::Value>,
    ) -> Result<PathBuf, String> {
        let project = project.canonicalize().map_err(|error| error.to_string())?;
        let spawns = read_lines(&self.dir.join("spawn.jsonl"));
        let mut processes = BTreeSet::new();
        let mut kept = String::new();
        for spawn in &spawns {
            let cwd = spawn["cwd"].as_str().map(PathBuf::from);
            let asked_version = spawn["argv"]
                .as_array()
                .is_some_and(|argv| argv.iter().any(|arg| arg == "--version"));
            if cwd.and_then(|cwd| cwd.canonicalize().ok()).as_deref() == Some(project.as_path())
                && !asked_version
                && let Some(process) = spawn["process"].as_str()
            {
                processes.insert(process.to_owned());
                kept.push_str(&format!("{spawn}\n"));
            }
        }
        if processes.is_empty() {
            return Err(format!(
                "no provider process was recorded in {}",
                project.display()
            ));
        }
        let mut io = String::new();
        for line in read_lines(&self.dir.join("io.jsonl")) {
            if line["process"]
                .as_str()
                .is_some_and(|process| processes.contains(process))
            {
                io.push_str(&format!("{line}\n"));
            }
        }
        let out = self.out.join(scenario.name());
        std::fs::create_dir_all(&out).map_err(|error| error.to_string())?;
        std::fs::write(out.join("io.jsonl"), io).map_err(|error| error.to_string())?;
        std::fs::write(out.join("spawn.jsonl"), kept).map_err(|error| error.to_string())?;
        let format = match self.kind {
            Kind::Codex => "codex_io",
            Kind::ClaudePty => "claude_pty_io",
            _ => "claude_sdk_io",
        };
        let fixture = serde_json::json!({
            "about": format!("The live {} scenario's provider traffic.", scenario.name()),
            "prelude": prelude,
            "recording": {"path": "io.jsonl", "format": format},
        });
        let path = out.join("fixture.json");
        std::fs::write(
            &path,
            serde_json::to_vec_pretty(&fixture).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        Ok(path)
    }
}

fn read_lines(path: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

/// What the agent process told its interpreter about the provider it
/// launched (terminal Claude's version), from the agent's facts
/// ring: a terminal recording replays after it.
pub fn launch_facts(root: &Path, agent: &[u8]) -> Vec<serde_json::Value> {
    let Ok(id) = uuid::Uuid::from_slice(agent) else {
        return Vec::new();
    };
    let Some(dir) = find_dir(root, &id.to_string()) else {
        return Vec::new();
    };
    let facts = dir.join("private").join("facts");
    let mut segments: Vec<PathBuf> = std::fs::read_dir(&facts)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_none())
        .collect();
    segments.sort();
    for segment in segments {
        for line in std::fs::read_to_string(&segment)
            .unwrap_or_default()
            .lines()
        {
            let Ok(entry) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            if entry["event"] == "fact"
                && entry["channel"] == "agent"
                && let Some(json) = entry["text"]
                    .as_str()
                    .and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
            {
                return vec![serde_json::json!({"fact": {"channel": "agent", "json": json}})];
            }
        }
    }
    Vec::new()
}

fn find_dir(root: &Path, name: &str) -> Option<PathBuf> {
    for entry in std::fs::read_dir(root).ok()?.flatten() {
        let path = entry.path();
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        if entry.file_name() == name {
            return Some(path);
        }
        if let Some(found) = find_dir(&path, name) {
            return Some(found);
        }
    }
    None
}
