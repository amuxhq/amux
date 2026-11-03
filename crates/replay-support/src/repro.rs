//! `repro`: replays whatever a problem left behind through every stage it
//! carries, and keeps a copy of it where a test can be made from it.
//!
//! - A report (its directory, a person's report holding its `dump/`, or the
//!   `dump.pb.zst` it was sent as): each agent's record as a made-up
//!   stream, the daemon core's record and each chat's record
//!   ([`Bundle::replay_all`]).
//! - A report id, or the prefix its files are under, fetched first from
//!   the bucket the environment names ([`crate::fetch`]).
//! - An installation's folder on this machine: each profile's daemon core
//!   from its own ring's checkpoint over its store, and each agent's real
//!   facts ring through its interpreter ([`replay_facts`]).
//! - A relay's or the supervisor's report ([`crate::window`]).
//!
//! What was replayed is kept under the directory the caller names, one
//! folder each, so a failing replay can become a test.

use std::path::{Path, PathBuf};

use crate::bundle::{self, Bundle};
use crate::fetch::{self, Bucket};
use crate::replay::{Stage, StageVerdict, Verdict, replay_facts};
use crate::window;

/// One thing replayed: what it was, where its copy is kept, every stage's
/// verdict and how to turn it into a test.
#[derive(Debug)]
pub struct Run {
    pub name: String,
    pub kept: PathBuf,
    pub verdicts: Vec<StageVerdict>,
    pub test: &'static str,
}

impl Run {
    pub fn failed(&self) -> bool {
        self.verdicts.iter().any(|verdict| verdict.verdict.failed())
    }
}

/// Replays what `named` names, keeping each copy under `keep`. A name that
/// is no file or folder here is a report to fetch from `bucket`, which is
/// asked for only then.
pub fn repro(
    named: &str,
    keep: &Path,
    bucket: impl FnOnce() -> Result<Bucket, String>,
) -> Result<Vec<Run>, String> {
    let path = Path::new(named);
    if path.is_file() && window::is_window(path) {
        return Ok(vec![window_run(path, keep)?]);
    }
    if path.is_dir() && path.join(PROFILES).is_dir() {
        return installation(path, keep);
    }
    if path.exists() {
        let name = local_name(path);
        let kept = keep.join(&name);
        // A copy kept by an earlier run replays where it is.
        if !within(path, &kept) {
            fresh(&kept)?;
            copy_report(path, &kept)?;
        }
        unpack(&kept)?;
        return Ok(vec![report_run(name, kept)?]);
    }
    if fetch::report_id(named).is_none() && fetch::report_prefix(named).is_none() {
        return Err(format!(
            "{named} is no file or folder here, nor a report id or a reports/<user>/<id> prefix"
        ));
    }
    let kept = bucket()?.fetch(named, created(keep)?)?;
    unpack(&kept)?;
    Ok(vec![report_run(file_name(&kept), kept)?])
}

const PROFILES: &str = "profiles";
const TEST_REPORT: &str = "copy the kept folder under crates/replay-support/tests/fixtures/ \
     and replay it with Bundle::open(..).replay_all(), as \
     crates/replay-support/tests/core_replay.rs does; an agent's record alone \
     replays with synthetic::reproduce(&record), as crates/replay-support/tests/synthetic.rs \
     does with its fixture";
const TEST_WINDOW: &str = "copy the kept file under crates/replay-support/tests/fixtures/ and \
     replay it with window::replay(..), as crates/replay-support/tests/repro.rs does, or \
     step the core from its start through its inputs in a test of that core";
const TEST_INSTALLATION: &str = "the kept folder holds what the agents and people said: never \
     commit it. Reproduce the failing step in a fixture under crates/interpret/fixtures/ \
     (an interpreter's) or from a report of the same moment (`amux dump`), which carries no \
     words";

fn report_run(name: String, kept: PathBuf) -> Result<Run, String> {
    let bundle = Bundle::open(&kept).map_err(|error| error.to_string())?;
    let verdicts = bundle.replay_all().map_err(|error| error.to_string())?;
    Ok(Run {
        name,
        kept,
        verdicts,
        test: TEST_REPORT,
    })
}

fn window_run(path: &Path, keep: &Path) -> Result<Run, String> {
    let name = file_name(path);
    let kept = created(keep)?.join(&name);
    if !within(path, &kept) {
        std::fs::copy(path, &kept).map_err(|error| format!("{}: {error}", kept.display()))?;
    }
    Ok(Run {
        name,
        verdicts: vec![window::replay(&kept)?],
        kept,
        test: TEST_WINDOW,
    })
}

/// Each profile of the installation at `root`, copied and replayed: the
/// daemon core from its ring's newest checkpoint over the store's main
/// file, which holds what that checkpoint saw while the daemon runs or
/// after it was killed, and each agent's facts ring.
fn installation(root: &Path, keep: &Path) -> Result<Vec<Run>, String> {
    // A clean stop moves every commit into the store's main file, past the
    // ring's newest checkpoint, so the two no longer replay together.
    let clean = std::fs::read(root.join("generation"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .is_some_and(|generation| generation["clean"] == true);
    let mut runs = Vec::new();
    for (profile, dir) in
        bundle::listing(&root.join(PROFILES)).map_err(|error| error.to_string())?
    {
        if !dir.is_dir() {
            continue;
        }
        let name = format!("installation-{profile}");
        let kept = fresh(&keep.join(&name))?;
        let core = if clean {
            Some(Verdict::Absent(
                "the daemon stopped cleanly, which moves its store past the ring's checkpoint: \
                 replay the installation while it runs, or a report"
                    .to_owned(),
            ))
        } else {
            copy_core(&dir, &profile, &kept)?
        };
        copy_agents(&dir.join("agents"), &kept.join("agents"))?;
        let core = match core {
            Some(absent) => absent,
            None => {
                let bundle = Bundle::open(&kept).map_err(|error| error.to_string())?;
                match bundle.replay_core().map_err(|error| error.to_string())? {
                    Some(failing) => Verdict::Fails(failing),
                    None => Verdict::Holds(format!(
                        "{} inputs from the ring's checkpoint, each step judged",
                        bundle.core.as_ref().map_or(0, |core| core
                            .inputs
                            .iter()
                            .filter(|line| matches!(line, daemon_core::Recorded::Input(_)))
                            .count())
                    )),
                }
            }
        };
        runs.push(Run {
            name,
            verdicts: vec![
                StageVerdict {
                    stage: Stage::Interpreter,
                    verdict: replay_facts(&kept.join("agents")),
                },
                StageVerdict {
                    stage: Stage::Core,
                    verdict: core,
                },
            ],
            kept,
            test: TEST_INSTALLATION,
        });
    }
    if runs.is_empty() {
        return Err(format!("{} holds no profile", root.display()));
    }
    Ok(runs)
}

/// The profile's core laid out as a report's core record is, but whole:
/// the ring's newest checkpoint and every line since, a copy of the
/// store's main file and every journal. Some verdict when there is nothing
/// to replay.
fn copy_core(profile_dir: &Path, profile: &str, kept: &Path) -> Result<Option<Verdict>, String> {
    let Some((checkpoint, lines)) = core_ring::newest(&profile_dir.join("core-ring"))
        .map_err(|error| format!("the core's ring: {error}"))?
    else {
        return Ok(Some(Verdict::Absent(
            "the core's ring holds no checkpoint".to_owned(),
        )));
    };
    let core = kept.join(bundle::CORE);
    write(&core.join(bundle::CORE_CHECKPOINT), &checkpoint)?;
    write(&core.join(bundle::CORE_INPUTS), &lines)?;
    // The main file alone: the WAL beside it holds what came after the
    // checkpoint.
    copy(
        &profile_dir.join("store.sqlite"),
        &core.join(bundle::CORE_STORE),
    )?;
    for (agent, dir) in
        bundle::listing(&profile_dir.join("agents")).map_err(|error| error.to_string())?
    {
        copy_tree(
            &dir.join("journal"),
            &core.join(bundle::CORE_JOURNALS).join(agent),
        )?;
    }
    // The profile's id is its host's, the one its store is opened for.
    let manifest = serde_json::json!({ "host": profile, "trigger": "installation" });
    write(&kept.join("manifest.json"), manifest.to_string().as_bytes())?;
    Ok(None)
}

/// Of each agent directory, what its facts replay reads: its specs, its
/// journal and its facts ring.
fn copy_agents(agents: &Path, kept: &Path) -> Result<(), String> {
    for (id, dir) in bundle::listing(agents).map_err(|error| error.to_string())? {
        for (name, path) in bundle::listing(&dir).map_err(|error| error.to_string())? {
            if name.starts_with("spec.") {
                copy(&path, &kept.join(&id).join(&name))?;
            }
        }
        copy_tree(&dir.join("journal"), &kept.join(&id).join("journal"))?;
        copy_tree(
            &dir.join("private/facts"),
            &kept.join(&id).join("private/facts"),
        )?;
    }
    Ok(())
}

/// A report named here, copied whole into `kept`.
fn copy_report(named: &Path, kept: &Path) -> Result<(), String> {
    if named.is_dir() {
        copy_tree(named, kept)
    } else {
        copy(named, &kept.join("dump.pb.zst"))
    }
}

/// Whether `path` is `kept` or inside it.
fn within(path: &Path, kept: &Path) -> bool {
    match (path.canonicalize(), kept.canonicalize()) {
        (Ok(path), Ok(kept)) => path.starts_with(kept),
        _ => false,
    }
}

/// A report folder holding only its sent archive gets the bundle unpacked
/// beside it as `dump/`, so it opens as a folder from then on.
fn unpack(kept: &Path) -> Result<(), String> {
    if kept.join("dump").is_dir() || kept.join("manifest.json").is_file() {
        return Ok(());
    }
    let (dir, _unpacked) = bundle::locate(kept).map_err(|error| error.to_string())?;
    if dir != kept {
        copy_tree(&dir, &kept.join("dump"))?;
    }
    Ok(())
}

/// What a local report is kept as: its folder's name, or for an archive
/// the folder holding it.
fn local_name(path: &Path) -> String {
    if path.is_file()
        && let Some(parent) = path.parent().filter(|parent| parent.file_name().is_some())
    {
        return file_name(parent);
    }
    file_name(path)
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || "report".to_owned(),
        |name| name.to_string_lossy().into_owned(),
    )
}

fn created(dir: &Path) -> Result<&Path, String> {
    std::fs::create_dir_all(dir).map_err(|error| format!("{}: {error}", dir.display()))?;
    Ok(dir)
}

/// `dir`, emptied of an earlier run's copy.
fn fresh(dir: &Path) -> Result<PathBuf, String> {
    if dir.exists() {
        std::fs::remove_dir_all(dir).map_err(|error| format!("{}: {error}", dir.display()))?;
    }
    created(dir)?;
    Ok(dir.to_owned())
}

fn write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        created(parent)?;
    }
    std::fs::write(path, bytes).map_err(|error| format!("{}: {error}", path.display()))
}

fn copy(from: &Path, to: &Path) -> Result<(), String> {
    if let Some(parent) = to.parent() {
        created(parent)?;
    }
    std::fs::copy(from, to)
        .map(|_| ())
        .map_err(|error| format!("{}: {error}", from.display()))
}

/// Every file under `from` copied to the same place under `to`; nothing
/// when `from` is not there.
fn copy_tree(from: &Path, to: &Path) -> Result<(), String> {
    for (name, path) in bundle::listing(from).map_err(|error| error.to_string())? {
        if path.is_dir() {
            copy_tree(&path, &to.join(&name))?;
        } else if path.is_file() {
            copy(&path, &to.join(&name))?;
        }
    }
    Ok(())
}
