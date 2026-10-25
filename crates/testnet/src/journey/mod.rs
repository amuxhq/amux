//! `testnet journey`: the shared journey stories, run on the real clients
//! a person uses. Each story in `journeys/manifest.json` names the clients
//! it runs on and the served topology it runs against; a run drives the
//! client as a person would, compares what it showed with reviewed goldens
//! under `journeys/goldens/<client>/<story>/` (`UPDATE_JOURNEY_GOLDENS=1`
//! rewrites them) and asks the hosts, through the net's door, what they
//! recorded. Results land in `target/journeys/`.

mod common;
mod device;
mod frame;
mod goldens;
mod perf;
mod phone;
mod phone_stories;
mod terminal;
mod terminal_stories;

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, Subcommand};
use serde::Deserialize;
use serde_json::Value;

use crate::door::Control;

/// Run the shared journey stories on the real clients.
#[derive(Parser, Debug)]
#[command(name = "testnet journey")]
struct Journey {
    #[command(subcommand)]
    suite: Suite,
}

#[derive(Subcommand, Debug)]
enum Suite {
    /// The terminal client in a private tmux server: one story, or `all`,
    /// one after another, a failure not stopping the rest.
    Terminal { story: String },
    /// The phone app on the leased golden simulator: the named stories, or
    /// every one written for the phone, one after another.
    Phone {
        /// The phone's own stories: those the manifest declares for the
        /// phone and no other client.
        #[arg(long, conflicts_with = "stories")]
        native: bool,
        stories: Vec<String>,
    },
    /// The phone's whole screens, reached the way a person reaches them,
    /// compared with apps/apple/Goldens in light and dark, every frosted
    /// surface drawn flat.
    Goldens {
        /// Photograph only the screens named; the way through every other
        /// screen is still taken, so each is reached in the same state.
        #[arg(long, value_name = "ID")]
        only: Vec<String>,
        /// Rewrite every golden that differs and leave the rest alone.
        #[arg(long, conflicts_with = "perturb")]
        update: bool,
        /// Move one design colour token (or, named `needs-you-dot`, take
        /// that one small mark away) before anything is drawn, and fail
        /// unless every photograph comes back different with a difference
        /// image.
        #[arg(long, value_name = "TOKEN")]
        perturb: Option<String>,
        /// Take the same way with the flag off and write every screen, glass
        /// and all, to DIR for a person to look at; nothing is compared.
        #[arg(long, value_name = "DIR", conflicts_with_all = ["update", "perturb"])]
        review: Option<PathBuf>,
    },
    /// The golden run with something changed on purpose, once per change,
    /// each required to notice: a suite nobody has seen fail is not
    /// evidence that it would.
    GoldensPerturb {
        #[arg(long, value_name = "ID", default_values = ["fleet"])]
        only: Vec<String>,
        /// A design colour token, and the one small mark a driving build can
        /// hide.
        #[arg(long = "perturb", value_name = "TOKEN", default_values = ["accent", "needs-you-dot"])]
        changes: Vec<String>,
    },
    /// Every control on the app's live pages audited by a UI test for a
    /// name VoiceOver can read and at least 44 pt to hit.
    Accessibility,
    /// The optimised app measured on the enrolled Mac's simulator against
    /// the served performance network; budgets and drift per
    /// docs/PERFORMANCE.md.
    Perf {
        /// Say which machine this is and whether its baseline exists.
        #[arg(long)]
        describe: bool,
        #[arg(long, value_enum, value_name = "GROUP")]
        only: Vec<perf::Group>,
        /// Record this run's medians as the machine's baseline, after every
        /// budget passes; needs a whole run.
        #[arg(long)]
        baseline: bool,
        /// Draw every surface flat for the whole run: a diagnostic, never a
        /// baseline, that says how much of a number is the glass.
        #[arg(long)]
        flat: bool,
        /// Record the first sample of one group under Instruments (xctrace)
        /// into PATH, a .trace bundle, and export its tables beside it as
        /// XML. On a phone the recording attaches to the app, or for a
        /// launch records the whole phone. On the simulator it is of the
        /// whole Mac, with the app's pid printed to filter by: xctrace hangs
        /// attaching to a simulator process through the simulator device,
        /// and on the host it refuses the pid, but a host recording of every
        /// process carries the app with its symbols and thread names.
        #[arg(long, value_name = "PATH")]
        trace: Option<PathBuf>,
        /// The Instruments template the trace is made with, as
        /// `xcrun xctrace list templates` names it.
        #[arg(
            long,
            value_name = "NAME",
            default_value = "Time Profiler",
            requires = "trace"
        )]
        template: String,
        /// The group whose first sample the trace records.
        #[arg(
            long,
            value_enum,
            value_name = "GROUP",
            default_value = "streaming",
            requires = "trace"
        )]
        during: perf::Group,
        /// Measure on the phone plugged into this Mac over USB rather than
        /// the simulator.
        #[arg(long)]
        device: bool,
    },
}

/// `testnet journey …` with its own arguments.
pub fn main(arguments: Vec<String>) -> ExitCode {
    let journey = match Journey::try_parse_from(
        std::iter::once("testnet journey".to_owned()).chain(arguments),
    ) {
        Ok(journey) => journey,
        Err(error) => error.exit(),
    };
    match journey.suite {
        Suite::Terminal { story } if story == "all" => terminal_stories::run_all(),
        Suite::Terminal { story } => terminal_stories::run(&story),
        Suite::Phone { native, stories } => phone_stories::run(native, stories),
        Suite::Goldens {
            only,
            update,
            perturb,
            review,
        } => goldens::run(only, update, perturb, review),
        Suite::GoldensPerturb { only, changes } => goldens::perturb(only, changes),
        Suite::Accessibility => goldens::accessibility(),
        Suite::Perf {
            describe,
            only,
            baseline,
            flat,
            trace,
            template,
            during,
            device,
        } => perf::run(perf::Asked {
            describe,
            only,
            baseline,
            flat,
            trace: trace.map(|path| perf::Trace {
                path,
                template,
                during,
            }),
            device,
        }),
    }
}

/// The checkout the journeys, their goldens and the built binaries live in.
pub(crate) fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the checkout")
}

/// One story the manifest declares.
#[derive(Clone, Debug, Deserialize)]
pub(crate) struct Story {
    pub id: String,
    #[serde(default)]
    pub clients: Vec<String>,
    #[serde(default)]
    pub topology: Option<String>,
    /// The topology the phone runs the story against, where it differs.
    #[serde(default)]
    pub phone_topology: Option<String>,
}

#[derive(Deserialize)]
struct Manifest {
    journeys: Vec<Story>,
}

pub(crate) fn manifest() -> Result<Vec<Story>, String> {
    let path = root().join("journeys/manifest.json");
    let text =
        std::fs::read_to_string(&path).map_err(|error| format!("{}: {error}", path.display()))?;
    serde_json::from_str::<Manifest>(&text)
        .map(|manifest| manifest.journeys)
        .map_err(|error| format!("{}: {error}", path.display()))
}

/// The story `name` declared for `client`.
pub(crate) fn story(name: &str, client: &str) -> Result<Story, String> {
    let matching: Vec<Story> = manifest()?
        .into_iter()
        .filter(|story| story.id == name)
        .collect();
    match matching.as_slice() {
        [story] if story.clients.iter().any(|c| c == client) => Ok(story.clone()),
        _ => Err(format!("no {client} journey named {name:?}")),
    }
}

/// One control request to the net's door; the reply's value.
pub(crate) fn door(
    address: SocketAddr,
    request: &Control,
    timeout: Duration,
) -> Result<Value, String> {
    let request = serde_json::to_value(request).map_err(|error| error.to_string())?;
    crate::ctl::door::call(address, &request, timeout)
}

/// What a story found, one line per claim it proved.
pub(crate) type Proved = Result<Vec<String>, String>;

/// Prompts in a host's chat whose text is exactly `text`.
pub(crate) fn prompts<'a>(chat: &'a Value, text: &str) -> Vec<&'a Value> {
    items(chat)
        .iter()
        .filter(|item| item["text"] == text)
        .collect()
}

/// A chat's items.
pub(crate) fn items(chat: &Value) -> &[Value] {
    chat["items"].as_array().map_or(&[], Vec::as_slice)
}

/// Whether any of a chat's items holds `text`.
pub(crate) fn holds(chat: &Value, text: &str) -> bool {
    items(chat).iter().any(|item| {
        item["text"]
            .as_str()
            .is_some_and(|said| said.contains(text))
    })
}

pub(crate) fn phase(chat: &Value) -> &str {
    chat["phase"].as_str().unwrap_or_default()
}

/// The prompt the person sent is in the host's chat exactly once.
pub(crate) fn reflected_once(chat: &Value, text: &str) -> Result<(), String> {
    match prompts(chat, text).len() {
        1 => Ok(()),
        found => Err(format!(
            "{} is in the host's chat {found} times",
            quoted(text)
        )),
    }
}

/// The host never received `text`.
pub(crate) fn never_sent(chat: &Value, text: &str) -> Result<(), String> {
    if prompts(chat, text).is_empty() {
        Ok(())
    } else {
        Err(format!("the host received {}", quoted(text)))
    }
}

/// A deliberately wrong expectation must fail the same check.
pub(crate) fn negative_control(wrong: Result<(), String>) -> Result<String, String> {
    match wrong {
        Err(error) => Ok(format!("a wrong expectation fails: {error}")),
        Ok(()) => Err("the check accepted a wrong outcome".to_owned()),
    }
}

/// `text` in single quotes, as the claims name what was typed.
pub(crate) fn quoted(text: &str) -> String {
    format!("'{text}'")
}

/// Waits on an observation, every `every`, for up to `timeout`: the first
/// value `ready` accepts, or the last one with the reason.
pub(crate) fn poll<T>(
    timeout: Duration,
    every: Duration,
    mut observe: impl FnMut() -> Result<T, String>,
    mut ready: impl FnMut(&T) -> bool,
) -> Result<T, (Option<T>, String)> {
    let deadline = std::time::Instant::now() + timeout;
    let mut last = None;
    loop {
        match observe() {
            Ok(value) if ready(&value) => return Ok(value),
            Ok(value) => last = Some(value),
            Err(error) if std::time::Instant::now() >= deadline => return Err((last, error)),
            Err(_) => {}
        }
        if std::time::Instant::now() >= deadline {
            return Err((last, "timed out".to_owned()));
        }
        std::thread::sleep(every);
    }
}

/// What a run keeps beside its result: every act and what the hosts said.
#[derive(Default)]
pub(crate) struct Record {
    pub actions: Vec<String>,
    pub observations: BTreeMap<String, Value>,
}

impl Record {
    pub(crate) fn write(&self, output: &Path, result: &str) {
        let _ = std::fs::write(output.join("actions.txt"), self.actions.join("\n") + "\n");
        let observations = serde_json::to_string_pretty(&self.observations).unwrap_or_default();
        let _ = std::fs::write(output.join("observations.json"), observations + "\n");
        let _ = std::fs::write(output.join("result.txt"), result);
    }
}

/// A golden check: `actual` against the reviewed file at `path`, or the
/// file rewritten when `UPDATE_JOURNEY_GOLDENS=1` (refused in CI).
pub(crate) fn compare_golden(path: &Path, actual: &str) -> Result<(), String> {
    let update = std::env::var("UPDATE_JOURNEY_GOLDENS").is_ok_and(|value| value == "1");
    if update {
        if std::env::var_os("CI").is_some() {
            return Err("UPDATE_JOURNEY_GOLDENS is refused in CI".to_owned());
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        return std::fs::write(path, actual).map_err(|error| error.to_string());
    }
    let approved = std::fs::read_to_string(path).map_err(|_| {
        format!(
            "missing journey golden {}; review with UPDATE_JOURNEY_GOLDENS=1",
            path.display()
        )
    })?;
    if approved == actual {
        return Ok(());
    }
    Err(format!(
        "journey golden differs: {}\n{}",
        path.display(),
        line_diff(&approved, actual)
    ))
}

/// The lines that differ, approved then actual, each with its number.
pub(crate) fn line_diff(approved: &str, actual: &str) -> String {
    let (old, new): (Vec<&str>, Vec<&str>) = (approved.lines().collect(), actual.lines().collect());
    let mut out = Vec::new();
    for at in 0..old.len().max(new.len()) {
        let (a, b) = (old.get(at), new.get(at));
        if a != b {
            if let Some(a) = a {
                out.push(format!("-{:>3} {a}", at + 1));
            }
            if let Some(b) = b {
                out.push(format!("+{:>3} {b}", at + 1));
            }
        }
    }
    out.join("\n")
}
