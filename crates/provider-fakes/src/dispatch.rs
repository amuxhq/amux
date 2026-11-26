//! `fake-provider`: the program a test puts where a command is expected,
//! doing what the plan beside it says.
//!
//! Tests used to write `#!/bin/sh` scripts for these commands, which
//! Windows cannot run. Instead the one binary is linked (or copied, where
//! a link can't be made) to the name the command needs, `claude`, `codex`,
//! `acp` or `amux` with the platform's executable suffix, and reads its
//! plan from `<name>.plan.json` beside itself ([`plan_path`]). Nothing is
//! made executable and nothing is a symlink, so the same files work on
//! every OS. [`install`] puts one in place.
//!
//! A plan is one of:
//! - [`Plan::Host`], a testnet host's provider command: it finds which
//!   agent started it, reads the choice the net recorded for that agent,
//!   and plays the fake or runs the real provider through its recording
//!   probe. The fakes run in this process, so the command *is* the fake,
//!   as the script's `exec` made it.
//! - [`Plan::Run`], another program with arguments before the caller's, as
//!   a login naming a config file.
//! - [`Plan::Answer`], a stand-in that reads some input, answers and exits.
//! - [`Plan::Next`], a wrapper that runs the next program of its name on
//!   the PATH past its own folder, as a usage meter or a version manager
//!   does.
//! - [`Plan::Leave`], a start that leaves a process of its own holding its
//!   standard streams, as a launcher that leaves a daemon running does.
//!
//! The binary with no plan beside it says so and exits at once with status
//! 3: a build broken on purpose is this program alone.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::script::{INPUT_LOG_ENV, SCRIPT_ENV};

/// The binary's name, without the platform's suffix.
pub const BINARY: &str = "fake-provider";

/// The status the binary exits with when it has no plan.
pub const NO_PLAN_EXIT: i32 = 3;

/// The status a host command refuses an agent with: the agent asked for
/// the real provider on a net that has none set up.
pub const NO_REAL_EXIT: i32 = 70;

/// What the program does.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "plan", rename_all = "snake_case")]
pub enum Plan {
    /// A testnet host's provider command, boxed as it is several times
    /// the size of the other plans.
    Host(Box<HostCommand>),
    /// Runs `program` with `args` before the ones this was given.
    Run { program: PathBuf, args: Vec<String> },
    /// Reads `lines` lines of input, writes `stderr`, then `stdout`, and
    /// exits with `code`.
    Answer {
        #[serde(default)]
        lines: usize,
        #[serde(default)]
        stdout: String,
        #[serde(default)]
        stderr: String,
        #[serde(default)]
        code: i32,
    },
    /// Runs the next `name` on the PATH past this program's own folder,
    /// with this one's arguments. Each round counts itself in the
    /// environment variable `rounds_env`; past `max_rounds` it prints
    /// `looped` and exits with status 3, and with nothing past it, it says
    /// so and exits with status 4.
    Next {
        name: String,
        rounds_env: String,
        max_rounds: u32,
    },
    /// Starts a copy of itself on this process's standard streams, which
    /// holds them for `linger_secs` seconds, then writes `stderr` and exits
    /// with `code` at once. The copy adds its pid, a line, to `pids` when
    /// given, so a test can see whether it still runs. Started with
    /// `--version` alone, it prints `version` instead when given, and
    /// exits leaving nothing, as a wrapper answers a version check itself.
    Leave {
        linger_secs: u64,
        #[serde(default)]
        stderr: String,
        #[serde(default)]
        code: i32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pids: Option<PathBuf>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        version: Option<String>,
    },
}

/// Set in the copy a [`Plan::Leave`] starts, which lingers instead.
const LINGERING_ENV: &str = "FAKE_PROVIDER_LINGERING";

/// Which provider a host command stands for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Command {
    /// Terminal or headless Claude: one `claude` for both, as on a real
    /// machine, headless when started with `--input-format`.
    Claude,
    Codex,
    /// The fake ACP agent.
    Acp,
}

/// The real provider or a fake.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Runs {
    Real,
    Fake,
}

/// What an agent runs: the net's choice for it, or its host's default.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Choice {
    pub provider: Runs,
    /// The fake's kind as the net declared it (`claude_pty`, `claude_sdk`,
    /// `codex`), kept for whoever reads the file: both Claude fakes are one
    /// program, which plays the terminal or headless by its arguments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// The script a fake plays, and where its input is logged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<PathBuf>,
}

/// A testnet host's `claude`, `codex` or `acp` command.
///
/// Every way an agent comes to exist ends with its host launching this
/// command, and only a spawn from the net carries arguments the net chose,
/// so the command decides: it finds which agent is starting it
/// ([`agent_of`]), reads that agent's [`Choice`] if the net recorded one
/// (`<choices>/<agent id>`), and runs the fake or the real provider.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HostCommand {
    pub command: Command,
    /// The host's name, for messages.
    pub host: String,
    /// What an agent the net recorded no choice for runs, and what a
    /// command the daemon runs itself (a version check) runs.
    pub default: Choice,
    /// Where the net records each agent's choice, as `<agent id>` files.
    pub choices: PathBuf,
    /// Set for the fake, whatever it plays.
    #[serde(default)]
    pub fake_env: BTreeMap<String, String>,
    /// An environment variable naming a script of the agent's own, which
    /// makes this command the fake playing it, on a real host too: what a
    /// host's config sets for an ACP agent it adds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_script_env: Option<String>,
    /// How the real provider runs; none on a net with no real providers
    /// set up, where an agent asking for one is refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub real: Option<Real>,
}

/// The real provider, run through its recording probe.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Real {
    /// The recording probe the provider runs behind.
    pub probe: PathBuf,
    /// The real provider the probe runs.
    pub real: PathBuf,
    /// The probe's variables' prefix: `CLAUDE` or `CODEX`.
    pub capture: String,
    /// Where traffic is recorded, a folder per agent (`host` for a command
    /// the daemon runs itself).
    pub recordings: PathBuf,
    /// The folder holding the host's commands, taken off the PATH the real
    /// provider gets so that it never finds this command again.
    pub bin: PathBuf,
    /// Arguments before the caller's.
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub set: BTreeMap<String, String>,
    #[serde(default)]
    pub unset: Vec<String>,
    /// Folders made before the provider starts.
    #[serde(default)]
    pub make: Vec<PathBuf>,
    /// Claude: the model an agent that chose none starts on. amux leaves
    /// the model and permission off the command line when nobody chose
    /// them, as for a child made by amux's spawn tool; Claude would then
    /// take a large model and the operator's own default mode, which may
    /// act without asking. With this set, an agent starts small and asking
    /// instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub small_claude: Option<String>,
}

/// Where the plan of the program at `program` lies: `<name>.plan.json`
/// beside it, `<name>` without the platform's executable suffix.
pub fn plan_path(program: &Path) -> PathBuf {
    let name = program
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let stem = name
        .strip_suffix(std::env::consts::EXE_SUFFIX)
        .filter(|stem| !stem.is_empty())
        .unwrap_or(&name);
    program.with_file_name(format!("{stem}.plan.json"))
}

/// The program `name` in `dir`, with the platform's executable suffix.
pub fn program(dir: &Path, name: &str) -> PathBuf {
    dir.join(os::paths::exe_name(name))
}

/// Puts `binary` at `at`: a hard link where the volume allows one, a copy
/// otherwise. A file already there is left as it is, since it is the same
/// program and may be running: only its plan changes.
///
/// The link is to the file `binary` resolves to. Buck hands a Unix test
/// its binaries as relative symbolic links, and a hard link to one of
/// those is the link itself, which names nothing from another folder.
pub fn link(binary: &Path, at: &Path) -> std::io::Result<()> {
    if at.exists() {
        return Ok(());
    }
    if let Some(dir) = at.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let binary = std::fs::canonicalize(binary)?;
    std::fs::hard_link(&binary, at).or_else(|_| std::fs::copy(&binary, at).map(|_| ()))
}

/// Makes `at` the `fake-provider` binary (`binary`) running `plan`. The
/// plan is replaced whole, so a command starting meanwhile reads the old
/// one or the new.
pub fn install(binary: &Path, at: &Path, plan: &Plan) -> std::io::Result<()> {
    link(binary, at)?;
    let text = serde_json::to_vec_pretty(plan).map_err(std::io::Error::other)?;
    os::fs::write_durably(&plan_path(at), &text)
}

/// The plan of the program at `program`.
pub fn read_plan(program: &Path) -> Result<Plan, String> {
    let path = plan_path(program);
    let text = std::fs::read_to_string(&path)
        .map_err(|error| format!("reading {}: {error}", path.display()))?;
    serde_json::from_str(&text).map_err(|error| format!("reading {}: {error}", path.display()))
}

/// Records `choice` for `agent`, for the host commands to find when it
/// starts.
pub fn choose(choices: &Path, agent: &str, choice: &Choice) -> std::io::Result<()> {
    std::fs::create_dir_all(choices)?;
    let text = serde_json::to_vec_pretty(choice).map_err(std::io::Error::other)?;
    os::fs::write_durably(&choices.join(agent), &text)
}

/// The agent a provider command was started for, from its arguments: amux
/// gives every provider an agent runs its tool server, `<install path>
/// internal mcp <agent dir>`, in Claude's `--mcp-config` JSON and in
/// Codex's `--config mcp_servers.<name>.args=[...]`. The agent's id is
/// that directory's name. None for a command the daemon runs itself, such
/// as a version check or a catalogue refresh, which names no agent.
pub fn agent_of(args: &[String]) -> Option<String> {
    args.iter().find_map(|arg| {
        server_args(arg)
            .into_iter()
            .find_map(|words| agent_dir(&words))
            .and_then(|dir| {
                Path::new(&dir)
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
            })
    })
}

/// The tool servers' argument lists `arg` holds, if it is Claude's MCP
/// config or a Codex `mcp_servers.<name>.args` override.
fn server_args(arg: &str) -> Vec<Vec<String>> {
    let words = |value: &serde_json::Value| -> Option<Vec<String>> {
        value
            .as_array()?
            .iter()
            .map(|word| word.as_str().map(str::to_owned))
            .collect()
    };
    let json = arg.strip_prefix("--mcp-config=").unwrap_or(arg);
    if json.trim_start().starts_with('{') {
        let Ok(config) = serde_json::from_str::<serde_json::Value>(json) else {
            return Vec::new();
        };
        return config
            .get("mcpServers")
            .and_then(|servers| servers.as_object())
            .map(|servers| {
                servers
                    .values()
                    .filter_map(|server| words(server.get("args")?))
                    .collect()
            })
            .unwrap_or_default();
    }
    if let Some((key, value)) = arg.split_once('=')
        && key.starts_with("mcp_servers.")
        && key.ends_with(".args")
        && let Ok(value) = serde_json::from_str::<serde_json::Value>(value)
    {
        return words(&value).into_iter().collect();
    }
    Vec::new()
}

/// The agent directory in a tool server's arguments: the word after
/// `internal mcp`.
fn agent_dir(words: &[String]) -> Option<String> {
    words
        .windows(3)
        .find(|window| window[0] == "internal" && window[1] == "mcp")
        .map(|window| window[2].clone())
}

/// What a host command does for one start.
#[derive(Clone, Debug, PartialEq)]
pub enum Decision {
    /// The fake, with these variables set.
    Fake { env: Vec<(String, OsString)> },
    /// The real provider through its probe.
    Real {
        program: PathBuf,
        args: Vec<String>,
        set: Vec<(String, OsString)>,
        unset: Vec<String>,
        make: Vec<PathBuf>,
    },
}

/// Decides what `command` does when started with `args`, `agent_script`
/// being the value of its [`HostCommand::agent_script_env`] and `path` this
/// process's PATH. An error is the refusal to print, with its status.
pub fn decide(
    command: &HostCommand,
    args: &[String],
    agent_script: Option<OsString>,
    path: Option<OsString>,
) -> Result<Decision, (String, i32)> {
    let agent = agent_of(args);
    let mut choice = command.default.clone();
    if let Some(agent) = &agent {
        let file = command.choices.join(agent);
        match std::fs::read_to_string(&file) {
            Ok(text) => {
                choice = serde_json::from_str(&text).map_err(|error| {
                    (
                        format!("testnet: reading {}: {error}", file.display()),
                        NO_REAL_EXIT,
                    )
                })?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err((
                    format!("testnet: reading {}: {error}", file.display()),
                    NO_REAL_EXIT,
                ));
            }
        }
    }
    // An agent configured with a script of its own is the fake playing it,
    // on a real host too: there is no real agent behind this command to
    // fall back on.
    if let Some(script) = agent_script.filter(|script| !script.is_empty()) {
        let text = script.to_string_lossy();
        let input = format!("{}.input", text.strip_suffix(".json").unwrap_or(&text));
        choice.script = Some(PathBuf::from(script));
        choice.input = Some(PathBuf::from(input));
        choice.provider = Runs::Fake;
    }
    match choice.provider {
        Runs::Fake => {
            let mut env: Vec<(String, OsString)> = command
                .fake_env
                .iter()
                .map(|(key, value)| (key.clone(), value.into()))
                .collect();
            if let Some(script) = &choice.script {
                env.push((SCRIPT_ENV.to_owned(), script.into()));
                env.push((
                    INPUT_LOG_ENV.to_owned(),
                    choice.input.clone().unwrap_or_default().into(),
                ));
            }
            Ok(Decision::Fake { env })
        }
        Runs::Real => {
            let Some(real) = &command.real else {
                return Err((
                    "testnet: this net has no real providers set up".to_owned(),
                    NO_REAL_EXIT,
                ));
            };
            let recordings = real.recordings.join(agent.as_deref().unwrap_or("host"));
            let path = std::env::join_paths(
                std::env::split_paths(&path.unwrap_or_default()).filter(|dir| *dir != real.bin),
            )
            .unwrap_or_default();
            let capture = &real.capture;
            let mut set = vec![
                ("PATH".to_owned(), path),
                (format!("{capture}_CAPTURE_PROXY"), "1".into()),
                (format!("{capture}_CAPTURE_DIR"), recordings.clone().into()),
                (format!("{capture}_REAL_PATH"), real.real.clone().into()),
            ];
            set.extend(
                real.set
                    .iter()
                    .map(|(key, value)| (key.clone(), value.into())),
            );
            let mut unset = vec![SCRIPT_ENV.to_owned(), INPUT_LOG_ENV.to_owned()];
            unset.extend(real.unset.iter().cloned());
            let mut make = vec![recordings];
            make.extend(real.make.iter().cloned());
            Ok(Decision::Real {
                program: real.probe.clone(),
                args: real_args(real, args),
                set,
                unset,
                make,
            })
        }
    }
}

/// The real provider's arguments: the plan's, then, for Claude, the small
/// model and the asking mode where amux chose neither, then the caller's.
fn real_args(real: &Real, args: &[String]) -> Vec<String> {
    let mut started = real.args.clone();
    if let Some(model) = &real.small_claude {
        let chose_model = args
            .iter()
            .any(|arg| arg == "--model" || arg.starts_with("--model="));
        let chose_permission = args.iter().any(|arg| {
            arg == "--permission-mode"
                || arg.starts_with("--permission-mode=")
                || arg == "--dangerously-skip-permissions"
        });
        if !chose_model {
            started.extend(["--model".to_owned(), model.clone()]);
        }
        if !chose_permission {
            started.extend(["--permission-mode".to_owned(), "default".to_owned()]);
        }
    }
    started.extend(args.iter().cloned());
    started
}

/// The `fake-provider` binary's `main`.
pub fn main() -> i32 {
    let exe = std::env::current_exe()
        .ok()
        .and_then(|exe| std::path::absolute(exe).ok())
        .unwrap_or_else(|| PathBuf::from(BINARY));
    let path = plan_path(&exe);
    let plan = match std::fs::read_to_string(&path) {
        Ok(text) => match serde_json::from_str::<Plan>(&text) {
            Ok(plan) => plan,
            Err(error) => {
                eprintln!("{BINARY}: reading {}: {error}", path.display());
                return NO_PLAN_EXIT;
            }
        },
        Err(error) => {
            eprintln!(
                "{}: nothing to run, as no plan is at {} ({error}); this build is broken on purpose",
                exe.display(),
                path.display()
            );
            return NO_PLAN_EXIT;
        }
    };
    let args: Vec<String> = std::env::args().skip(1).collect();
    match plan {
        Plan::Host(command) => host(&command, &args),
        Plan::Run {
            program,
            args: before,
        } => {
            let mut command = std::process::Command::new(program);
            command.args(before).args(&args);
            run(command)
        }
        Plan::Answer {
            lines,
            stdout,
            stderr,
            code,
        } => answer(lines, &stdout, &stderr, code),
        Plan::Next {
            name,
            rounds_env,
            max_rounds,
        } => next(&exe, &name, &rounds_env, max_rounds, &args),
        Plan::Leave {
            linger_secs,
            stderr,
            code,
            pids,
            version,
        } => match version {
            Some(version) if args == ["--version"] => {
                println!("{version}");
                0
            }
            _ => leave(&exe, linger_secs, &stderr, code, pids.as_deref()),
        },
    }
}

fn leave(exe: &Path, linger_secs: u64, stderr: &str, code: i32, pids: Option<&Path>) -> i32 {
    if std::env::var_os(LINGERING_ENV).is_some() {
        if let Some(pids) = pids {
            use std::io::Write as _;
            let added = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(pids)
                .and_then(|mut file| writeln!(file, "{}", std::process::id()));
            if let Err(error) = added {
                eprintln!("{BINARY}: adding to {}: {error}", pids.display());
            }
        }
        std::thread::sleep(std::time::Duration::from_secs(linger_secs));
        return 0;
    }
    // Every standard stream inherited, as a launcher's careless start
    // leaves them.
    if let Err(error) = std::process::Command::new(exe)
        .env(LINGERING_ENV, "1")
        .spawn()
    {
        eprintln!("{BINARY}: leaving a copy behind: {error}");
        return 127;
    }
    eprint!("{stderr}");
    code
}

fn host(command: &HostCommand, args: &[String]) -> i32 {
    let agent_script = command
        .agent_script_env
        .as_deref()
        .and_then(std::env::var_os);
    match decide(command, args, agent_script, std::env::var_os("PATH")) {
        Err((refusal, code)) => {
            eprintln!("{refusal}");
            code
        }
        Ok(Decision::Fake { env }) => {
            for (key, value) in env {
                // SAFETY: the first thing this process does, before the
                // fake starts any thread: nothing reads the environment
                // concurrently.
                unsafe { std::env::set_var(key, value) };
            }
            match command.command {
                Command::Claude if args.iter().any(|arg| arg == "--input-format") => {
                    crate::sdk::main()
                }
                Command::Claude => crate::pty::main(),
                Command::Codex => crate::codex::main(),
                Command::Acp => crate::acp::main(),
            }
        }
        Ok(Decision::Real {
            program,
            args,
            set,
            unset,
            make,
        }) => {
            for dir in &make {
                if let Err(error) = std::fs::create_dir_all(dir) {
                    eprintln!("testnet: making {}: {error}", dir.display());
                    return NO_REAL_EXIT;
                }
            }
            let mut started = std::process::Command::new(program);
            started.args(args);
            for key in unset {
                started.env_remove(key);
            }
            started.envs(set);
            run(started)
        }
    }
}

/// Becomes `command`, as a script's `exec` does, and answers the status to
/// exit with if this process is still here: on Unix it is the same
/// process, so whatever signals or waits on the command's pid reaches the
/// program; Windows has no exec, so the program runs as its successor
/// ([`os::process::exec_or_spawn`]) with this process's standard streams,
/// and this one waits for it and passes its status on.
fn run(command: std::process::Command) -> i32 {
    let program = command.get_program().to_string_lossy().into_owned();
    match os::process::exec_or_spawn(command, &[]) {
        Ok(mut replaced) => match replaced.child.wait() {
            Ok(status) => status.code().unwrap_or(128),
            Err(error) => {
                eprintln!("{BINARY}: waiting for {program}: {error}");
                128
            }
        },
        Err(error) => {
            eprintln!("{BINARY}: starting {program}: {error}");
            127
        }
    }
}

fn answer(lines: usize, stdout: &str, stderr: &str, code: i32) -> i32 {
    use std::io::{BufRead as _, Write as _};
    let mut input = std::io::stdin().lock();
    for _ in 0..lines {
        let mut line = String::new();
        if input.read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
    }
    let _ = std::io::stderr().write_all(stderr.as_bytes());
    let _ = std::io::stdout().write_all(stdout.as_bytes());
    let _ = std::io::stdout().flush();
    code
}

fn next(exe: &Path, name: &str, rounds_env: &str, max_rounds: u32, args: &[String]) -> i32 {
    let rounds = std::env::var(rounds_env)
        .ok()
        .and_then(|rounds| rounds.parse::<u32>().ok())
        .unwrap_or(0)
        + 1;
    if rounds > max_rounds {
        println!("looped");
        return 3;
    }
    let here = exe.parent().map(Path::to_owned).unwrap_or_default();
    let path = std::env::var_os("PATH").unwrap_or_default();
    let past = std::env::join_paths(
        std::env::split_paths(&path)
            .filter(|dir| std::path::absolute(dir).map_or(true, |dir| dir != here)),
    )
    .unwrap_or_default();
    let Some(found) = os::paths::find_program(name.as_ref(), &past) else {
        println!("no {name} past the wrapper");
        return 4;
    };
    let mut command = std::process::Command::new(found);
    command.args(args).env(rounds_env, rounds.to_string());
    run(command)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn real_claude() -> Real {
        Real {
            probe: "probe".into(),
            real: "claude".into(),
            capture: "CLAUDE".into(),
            recordings: "recordings".into(),
            bin: "bin".into(),
            args: ["--setting-sources", "project", "--strict-mcp-config"]
                .map(str::to_owned)
                .into(),
            set: BTreeMap::new(),
            unset: Vec::new(),
            make: Vec::new(),
            small_claude: Some("haiku".into()),
        }
    }

    fn words(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| (*arg).to_owned()).collect()
    }

    #[test]
    fn claude_left_unset_starts_small_and_asking() {
        assert_eq!(
            real_args(&real_claude(), &words(&["-p", "hi"])),
            [
                "--setting-sources",
                "project",
                "--strict-mcp-config",
                "--model",
                "haiku",
                "--permission-mode",
                "default",
                "-p",
                "hi"
            ]
        );
    }

    #[test]
    fn claude_keeps_what_amux_chose() {
        let chosen = ["--dangerously-skip-permissions", "--model", "sonnet"];
        assert_eq!(real_args(&real_claude(), &words(&chosen))[3..], chosen);
        let chosen = ["--permission-mode", "plan", "--model=opus"];
        assert_eq!(real_args(&real_claude(), &words(&chosen))[3..], chosen);
    }

    /// The agent is found in either provider's tool server arguments, and
    /// a command naming no agent finds none.
    #[test]
    fn the_agent_is_the_one_its_tool_server_names() {
        let dir = Path::new("profiles")
            .join("p")
            .join("agents")
            .join("0d4c-agent");
        let dir = dir.to_string_lossy();
        let claude = serde_json::json!({
            "mcpServers": {"amux": {"command": "amux", "args": ["internal", "mcp", dir]}},
        })
        .to_string();
        assert_eq!(
            agent_of(&words(&["--print", "--mcp-config", &claude])).as_deref(),
            Some("0d4c-agent")
        );
        let codex = format!(
            "mcp_servers.amux.args={}",
            serde_json::json!(["internal", "mcp", dir])
        );
        assert_eq!(
            agent_of(&words(&[
                "--config",
                "mcp_servers.amux.command=\"amux\"",
                "--config",
                &codex
            ]))
            .as_deref(),
            Some("0d4c-agent")
        );
        assert_eq!(agent_of(&words(&["--version"])), None);
    }

    fn command(choices: &Path, default: Runs) -> HostCommand {
        HostCommand {
            command: Command::Claude,
            host: "studio".into(),
            default: Choice {
                provider: default,
                kind: None,
                script: Some("host.json".into()),
                input: Some("host.input".into()),
            },
            choices: choices.to_owned(),
            fake_env: [("CLAUDE_CONFIG_DIR".to_owned(), "claude".to_owned())].into(),
            agent_script_env: None,
            real: None,
        }
    }

    fn mcp_args(agent: &str) -> Vec<String> {
        let config = serde_json::json!({
            "mcpServers": {"amux": {"args": ["internal", "mcp", format!("agents/{agent}")]}},
        });
        words(&[
            "--input-format",
            "stream-json",
            "--mcp-config",
            &config.to_string(),
        ])
    }

    /// An agent the net chose a script for plays it; another plays the
    /// host's.
    #[test]
    fn an_agent_plays_the_choice_recorded_for_it() {
        let dir = tempfile::tempdir().unwrap();
        let choices = dir.path().join("choices");
        choose(
            &choices,
            "chosen",
            &Choice {
                provider: Runs::Fake,
                kind: Some("claude_sdk".into()),
                script: Some("mine.json".into()),
                input: Some("mine.input".into()),
            },
        )
        .unwrap();
        let command = command(&choices, Runs::Fake);
        let script = |decision: Decision| match decision {
            Decision::Fake { env } => env
                .into_iter()
                .find(|(key, _)| key == SCRIPT_ENV)
                .map(|(_, value)| value),
            real => panic!("a fake, not {real:?}"),
        };
        assert_eq!(
            script(decide(&command, &mcp_args("chosen"), None, None).unwrap()),
            Some("mine.json".into())
        );
        assert_eq!(
            script(decide(&command, &mcp_args("other"), None, None).unwrap()),
            Some("host.json".into())
        );
    }

    /// A configured ACP agent's script makes the command the fake that
    /// plays it, even on a host whose agents default to the real ones.
    #[test]
    fn a_configured_acp_script_plays_on_a_real_host() {
        let dir = tempfile::tempdir().unwrap();
        let mut command = command(&dir.path().join("choices"), Runs::Real);
        command.command = Command::Acp;
        command.agent_script_env = Some("TESTNET_AGENT_SCRIPT".into());
        assert_eq!(
            decide(&command, &[], None, None),
            Err((
                "testnet: this net has no real providers set up".to_owned(),
                NO_REAL_EXIT
            ))
        );
        let Decision::Fake { env } =
            decide(&command, &[], Some("/scripts/helper.json".into()), None).unwrap()
        else {
            panic!("the fake");
        };
        assert!(env.contains(&(SCRIPT_ENV.to_owned(), "/scripts/helper.json".into())));
        assert!(env.contains(&(INPUT_LOG_ENV.to_owned(), "/scripts/helper.input".into())));
    }

    /// The real provider runs through its probe, recorded under its agent,
    /// on a PATH without the host's commands.
    #[test]
    fn the_real_provider_runs_through_its_probe() {
        let dir = tempfile::tempdir().unwrap();
        let mut command = command(&dir.path().join("choices"), Runs::Real);
        command.real = Some(real_claude());
        let path = std::env::join_paths([Path::new("bin"), Path::new("elsewhere")]).unwrap();
        let Decision::Real {
            program,
            set,
            unset,
            make,
            ..
        } = decide(&command, &mcp_args("a1"), None, Some(path)).unwrap()
        else {
            panic!("the real provider");
        };
        assert_eq!(program, Path::new("probe"));
        assert!(set.contains(&("PATH".to_owned(), "elsewhere".into())));
        assert!(set.contains(&(
            "CLAUDE_CAPTURE_DIR".to_owned(),
            Path::new("recordings").join("a1").into()
        )));
        assert!(unset.contains(&SCRIPT_ENV.to_owned()));
        assert_eq!(make, [Path::new("recordings").join("a1")]);
    }

    #[test]
    fn a_plan_lies_beside_its_program() {
        let program = program(Path::new("bin"), "claude");
        assert_eq!(
            plan_path(&program),
            Path::new("bin").join("claude.plan.json")
        );
    }

    /// Buck hands a Unix test its binaries as relative symbolic links
    /// (`symlinked_dir`). A hard link made from one is a second name for
    /// the link, not for the binary, so in another folder it names nothing:
    /// on macOS, where the temp folder shares the work folder's volume, the
    /// link was made, and every installed command failed to start with "No
    /// such file or directory" (CI run 45718964949).
    #[test]
    fn a_program_linked_from_a_relative_symbolic_link_is_the_binary() {
        let root = tempfile::tempdir().unwrap();
        let built = root.path().join("out");
        std::fs::create_dir_all(&built).unwrap();
        std::fs::write(built.join("binary"), b"the binary").unwrap();
        let tree = root.path().join("binaries");
        std::fs::create_dir_all(&tree).unwrap();
        let named = tree.join("fake-provider");
        match os::testing::faults::symlink(&Path::new("..").join("out").join("binary"), &named)
            .unwrap()
        {
            os::testing::faults::Linked::Made => {}
            os::testing::faults::Linked::Skipped(why) => {
                eprintln!("skipped: {why}");
                return;
            }
        }
        let at = root.path().join("bin").join("claude");
        link(&named, &at).unwrap();
        assert!(
            !std::fs::symlink_metadata(&at)
                .unwrap()
                .file_type()
                .is_symlink(),
            "the program is a link, not the binary"
        );
        assert_eq!(std::fs::read(&at).unwrap(), b"the binary");
    }
}
