//! `testnet journey`: the shared journey stories, run on the real clients
//! a person uses. Each story in `journeys/manifest.json` names the clients
//! it runs on and the served topology it runs against; a run drives the
//! client as a person would, compares what it showed with reviewed goldens
//! under `journeys/goldens/<client>/<story>/` (`UPDATE_JOURNEY_GOLDENS=1`
//! rewrites them) and asks the hosts, through the net's door, what they
//! recorded. Results land in `target/journeys/`.

mod common;
mod frame;
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
    client: Client,
}

#[derive(Subcommand, Debug)]
enum Client {
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
}

/// `testnet journey …` with its own arguments.
pub fn main(arguments: Vec<String>) -> ExitCode {
    let journey = match Journey::try_parse_from(
        std::iter::once("testnet journey".to_owned()).chain(arguments),
    ) {
        Ok(journey) => journey,
        Err(error) => error.exit(),
    };
    match journey.client {
        Client::Terminal { story } if story == "all" => terminal_stories::run_all(),
        Client::Terminal { story } => terminal_stories::run(&story),
        Client::Phone { native, stories } => phone_stories::run(native, stories),
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
