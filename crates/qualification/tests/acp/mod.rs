//! The live ACP lane: each built-in Agent Client Protocol agent, run under
//! its real program and the owner's own sign-in, driven through a scenario
//! by a scripted client (client.rs) speaking through `acp-probe`, which
//! records what crosses. A scenario that ran joins the ACP corpus,
//! sanitized, as `crates/interpret/fixtures/acp/live/<agent>-<scenario>/`.
//!
//! amux has no ACP interpreter to judge the traffic yet, so a scenario
//! asserts only what it needs to go on (an answer arrived, a turn ended)
//! and reports what the agent did; the recordings are the evidence. Each
//! result is pass (it went as the scenario asks), finding (it ran, but the
//! agent did something else, which the notes say), fail (it could not
//! run to its end; what was captured still joins), unavailable (no such
//! program) or not_run.
//!
//! Every scenario runs on the agent's cheapest model at its lowest
//! thought level, in a fresh temporary folder, with tiny prompts.

mod client;
mod scenarios;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use scenarios::Scenario;

/// A built-in agent and how the lane keeps it cheap.
#[derive(Clone, Copy, Debug)]
pub struct Agent {
    /// The prefix of its recordings and its name on the command line.
    pub name: &'static str,
    /// Its registry id, the manifest's `recorded.provider`.
    pub provider: &'static str,
    pub command: &'static [&'static str],
    /// The model every scenario switches to.
    pub model: &'static str,
    /// The thought level every scenario switches to, where it has one.
    pub thought_level: Option<&'static str>,
    /// How the permission scenarios start it so that it asks before a
    /// shell command, where its default (or the owner's settings) runs
    /// one without asking: another command line, and variables added to
    /// its environment. Both apply to that one process only; nothing of
    /// the owner's configuration changes.
    pub asking: Option<Asking>,
}

/// How to start an agent so that it asks before running a command.
#[derive(Clone, Copy, Debug)]
pub struct Asking {
    pub command: &'static [&'static str],
    pub env: &'static [(&'static str, &'static str)],
    /// `_meta` for `session/new`, as JSON.
    pub session_meta: &'static str,
}

pub const AGENTS: &[Agent] = &[
    Agent {
        name: "agy",
        provider: "antigravity",
        command: &["agy", "acp"],
        model: "gemini-3.8-flash",
        thought_level: Some("low"),
        // Its default mode asks before edits and commands.
        asking: None,
    },
    Agent {
        name: "opencode",
        provider: "opencode",
        command: &["opencode", "acp"],
        model: "opencode/deepseek-v4.1-flash-free",
        thought_level: None,
        // Its build mode runs commands unasked unless configured to ask;
        // `OPENCODE_CONFIG_CONTENT` is merged over its config files.
        asking: Some(Asking {
            command: &["opencode", "acp"],
            env: &[(
                "OPENCODE_CONFIG_CONTENT",
                r#"{"permission":{"bash":"ask","edit":"ask"}}"#,
            )],
            session_meta: "null",
        }),
    },
    Agent {
        name: "grok",
        provider: "grok",
        command: &["grok", "agent", "stdio"],
        model: "grok-4.7-build-fast",
        thought_level: Some("low"),
        // Nothing a process can be given makes it ask: under the owner's
        // auto-review setting (`[ui] permission_mode` in its config, which
        // neither `--permission-mode`, the `GROK_CONFIG` overlay nor
        // `autoMode: false` overrides for `grok agent stdio`) its classifier
        // approves or blocks each call itself, and on stdio never asks.
        asking: None,
    },
];

const DRIVEN_BY: &str = "the scripted ACP client, in the live qualification lane";
/// How long a whole scenario may take before it is a hang.
const SCENARIO: Duration = Duration::from_secs(480);

pub enum Verdict {
    Pass,
    Finding,
    Fail(String),
    Unavailable(String),
    NotRun(String),
}

/// What a scenario saw, for the report.
#[derive(Default)]
pub struct Notes(pub Vec<String>);

impl Notes {
    pub fn add(&mut self, note: impl Into<String>) {
        self.0.push(note.into());
    }
}

pub fn main() -> ExitCode {
    // Flags are cargo's or libtest's, handed to every target.
    let names: Vec<String> = std::env::args()
        .skip(1)
        .filter(|arg| !arg.starts_with('-'))
        .collect();
    let [agents, scenarios] = match names.as_slice() {
        [] => {
            for agent in AGENTS {
                for scenario in Scenario::ALL {
                    println!(
                        "live acp {}-{}: not_run (no scenario selected)",
                        agent.name,
                        scenario.name()
                    );
                }
            }
            return ExitCode::SUCCESS;
        }
        [agents] => [agents.as_str(), "all"],
        [agents, scenarios, ..] => [agents.as_str(), scenarios.as_str()],
    };
    let agents = match pick(agents, AGENTS, |agent| agent.name) {
        Ok(agents) => agents,
        Err(error) => return usage(&error),
    };
    let scenarios = match pick(scenarios, Scenario::ALL, |scenario| scenario.name()) {
        Ok(scenarios) => scenarios,
        Err(error) => return usage(&error),
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("a runtime");
    let mut failed = false;
    for agent in &agents {
        for scenario in &scenarios {
            let (verdict, notes) = runtime.block_on(run(agent, *scenario));
            let (word, why) = match &verdict {
                Verdict::Pass => ("pass", String::new()),
                Verdict::Finding => ("finding", String::new()),
                Verdict::Fail(why) => {
                    failed = true;
                    ("fail", format!(" ({why})"))
                }
                Verdict::Unavailable(why) => ("unavailable", format!(" ({why})")),
                Verdict::NotRun(why) => ("not_run", format!(" ({why})")),
            };
            println!("live acp {}-{}: {word}{why}", agent.name, scenario.name());
            for note in &notes.0 {
                println!("    {note}");
            }
        }
    }
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn usage(error: &str) -> ExitCode {
    eprintln!(
        "{error}\nusage: just live acp <agent>[,<agent>...]|all [<scenario>[,<scenario>...]|all]\n\
         agents: {}\nscenarios: {}",
        AGENTS.iter().map(|a| a.name).collect::<Vec<_>>().join(", "),
        Scenario::ALL
            .iter()
            .map(|s| s.name())
            .collect::<Vec<_>>()
            .join(", ")
    );
    ExitCode::from(2)
}

/// The entries of `all` a comma-separated list names, or every one for
/// `all`.
fn pick<T: Copy>(list: &str, all: &[T], name: impl Fn(&T) -> &str) -> Result<Vec<T>, String> {
    if list == "all" {
        return Ok(all.to_vec());
    }
    list.split(',')
        .map(|wanted| {
            all.iter()
                .find(|entry| name(entry) == wanted)
                .copied()
                .ok_or_else(|| format!("no agent or scenario named {wanted}"))
        })
        .collect()
}

/// What a scenario runs with.
pub struct Cx {
    pub agent: Agent,
    pub probe: PathBuf,
    /// The session's folder, empty but for what the scenario puts there.
    pub project: PathBuf,
    /// A folder beside the project for anything else the scenario needs
    /// (a signed-out home).
    pub scratch: PathBuf,
    /// Where the probe writes; raw and never committed.
    pub capture: PathBuf,
}

async fn run(agent: &Agent, scenario: Scenario) -> (Verdict, Notes) {
    let mut notes = Notes::default();
    if !on_path(agent.command[0]) {
        return (
            Verdict::Unavailable(format!("no {} on the PATH", agent.command[0])),
            notes,
        );
    }
    if let Some(why) = scenario.skipped_for(agent) {
        return (Verdict::NotRun(why.into()), notes);
    }
    let probe = provider_fakes::binaries::path("acp-probe");
    if !probe.is_file() {
        return (
            Verdict::Fail(format!(
                "{} is not built: run the lane with `just live acp`",
                probe.display()
            )),
            notes,
        );
    }
    let name = format!("{}-{}", agent.name, scenario.name());
    let scratch = match tempfile::Builder::new()
        .prefix(&format!("acp-live-{name}-"))
        .tempdir()
    {
        Ok(dir) => dir,
        Err(error) => return (Verdict::Fail(error.to_string()), notes),
    };
    // Canonical, so the folder the agent is told and the one it reports
    // are spelled alike (macOS's temporary folder is behind a link).
    let root = scratch
        .path()
        .canonicalize()
        .unwrap_or_else(|_| scratch.path().to_owned());
    let capture = live_dir().join(&name);
    let _ = std::fs::remove_dir_all(&capture);
    let cx = Cx {
        agent: *agent,
        probe,
        project: root.join("project"),
        scratch: root.join("scratch"),
        capture: capture.clone(),
    };
    for folder in [&cx.project, &cx.scratch, &cx.capture] {
        if let Err(error) = std::fs::create_dir_all(folder) {
            return (Verdict::Fail(error.to_string()), notes);
        }
    }
    let verdict = match tokio::time::timeout(SCENARIO, scenario.run(&cx, &mut notes)).await {
        Ok(Ok(verdict)) => verdict,
        Ok(Err(error)) => Verdict::Fail(error),
        Err(_) => Verdict::Fail(format!("the scenario took over {}s", SCENARIO.as_secs())),
    };
    if matches!(verdict, Verdict::NotRun(_)) {
        return (verdict, notes);
    }
    if capture.join("spawn.jsonl").is_file() {
        let mut extra = serde_json::Map::new();
        extra.insert("scenario".into(), scenario.describe().into());
        if let Some(asking) = agent.asking.filter(|_| scenario.asks()) {
            let env: Vec<String> = asking
                .env
                .iter()
                .map(|(name, value)| format!("{name}={value}"))
                .collect();
            extra.insert(
                "started_as".into(),
                serde_json::json!({
                    "command": asking.command,
                    "env": env,
                    "session_meta": serde_json::from_str::<serde_json::Value>(asking.session_meta)
                        .unwrap_or_default(),
                }),
            );
        }
        if let Verdict::Fail(why) = &verdict {
            extra.insert("incomplete".into(), why.clone().into());
        }
        match qualification::recording::acp::join(
            &capture,
            &qualification::recording::acp::corpus_root(),
            &name,
            agent.provider,
            DRIVEN_BY,
            extra,
        ) {
            Ok(destination) => notes.add(format!("joined as {}", short(&destination))),
            Err(error) => notes.add(format!("not joined: {error}")),
        }
    }
    (verdict, notes)
}

fn short(path: &Path) -> String {
    let text = path.display().to_string();
    match text.find("crates/") {
        Some(at) => text[at..].replace("qualification/../", ""),
        None => text,
    }
}

fn on_path(command: &str) -> bool {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path).any(|dir| dir.join(command).is_file())
}

/// The raw captures, beside the test binary's target directory.
fn live_dir() -> PathBuf {
    let exe = std::env::current_exe().expect("the test binary's path");
    exe.ancestors()
        .nth(3)
        .map(Path::to_owned)
        .unwrap_or_else(|| PathBuf::from("target"))
        .join("live")
        .join("acp")
}
