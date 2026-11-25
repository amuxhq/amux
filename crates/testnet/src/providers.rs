//! The `claude`, `codex` and `acp` commands a host's agents run.
//!
//! Every way an agent comes to exist ends with its host launching its
//! configured provider command: a spawn from the net, a create from the
//! terminal client or the phone, an agent's own spawn tool, a resume. Only
//! the first carries arguments the net chose, and clients must not be
//! changed for a test, so the net decides at the one point they share: the
//! command. Each host gets a `claude`, a `codex` and an `acp` command, each
//! the `fake-provider` binary with a plan beside it
//! ([`provider_fakes::dispatch`]), which finds which agent is starting it
//! (the agent directory amux names in the provider's tool server), reads
//! that agent's choice if the net recorded one, and plays the scripted
//! fake or runs the real provider. No shell runs, so the commands work on
//! every OS.
//!
//! The real provider runs through its recording probe, so what amux wrote
//! to it and read back is kept, and with the operator's own setup kept out:
//! Claude reads only project settings and only the tool server amux names,
//! with self-updates off, under the operator's own login (a scratch config
//! directory would lose it); Codex runs on a scratch home holding only the
//! operator's login.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use provider_fakes::dispatch::{self, Choice, HostCommand, Plan, Real, Runs};

use crate::topology::{FakeKind, Provider};

/// The real providers and their recording probes.
#[derive(Clone, Debug)]
pub(crate) struct RealTools {
    pub claude: PathBuf,
    pub codex: PathBuf,
    pub claude_probe: PathBuf,
    pub codex_probe: PathBuf,
}

impl RealTools {
    /// The first `claude` and `codex` on this process's PATH, as absolute
    /// paths, and the probes beside the net's binaries.
    pub(crate) fn find(binaries: &Path) -> Result<Self, String> {
        let on_path = |command: &str| {
            let path = std::env::var_os("PATH").unwrap_or_default();
            os::paths::find_program(command.as_ref(), &path)
                .ok_or_else(|| format!("no {command} on the PATH for a real provider"))
        };
        let probe = |name: &str| {
            let path = dispatch::program(binaries, name);
            if path.is_file() {
                Ok(path)
            } else {
                Err(format!(
                    "{} is not built; a real provider records through it \
                     (cargo build -p qualification --bins, or just qa)",
                    path.display()
                ))
            }
        };
        Ok(Self {
            claude: on_path("claude")?,
            codex: on_path("codex")?,
            claude_probe: probe("claude-probe")?,
            codex_probe: probe("codex-probe")?,
        })
    }
}

/// Where a host's provider commands come from and what they default to.
pub(crate) struct Wrappers<'a> {
    pub host: &'a str,
    /// The host's own directory: the commands go in `bin/` below it.
    pub dir: &'a Path,
    pub default: Provider,
    /// Where each agent's choice is recorded, as `<agent id>` files.
    pub choices: &'a Path,
    /// Where real agents' traffic is recorded, a folder per agent.
    pub recordings: &'a Path,
    pub real: Option<&'a RealTools>,
    /// The fakes' binaries, `fake-provider` among them.
    pub fakes: &'a Path,
    /// What a fake plays for an agent the net recorded no choice for.
    pub host_script: Option<&'a Path>,
}

/// The commands, in the host's `bin/`.
pub(crate) struct Commands {
    pub claude: PathBuf,
    pub codex: PathBuf,
    /// The fake ACP agent with the agent's script, which every host runs
    /// as the agent [`FAKE_ACP_AGENT`]. A net has no real ACP agents set
    /// up.
    pub acp: PathBuf,
}

/// The id a client creates an ACP agent by to have its host run the fake
/// ACP agent, playing the host's script.
pub const FAKE_ACP_AGENT: &str = "fake-acp";

/// Set in the environment of an agent a host's config adds: the script its
/// fake ACP agent plays, over the host's own. Its input log lies beside it.
pub(crate) const AGENT_SCRIPT_ENV: &str = "TESTNET_AGENT_SCRIPT";

/// Records an agent's provider for the host's commands to find when it
/// starts: the real one, or a fake of `kind` playing `script`.
pub(crate) fn choose(
    choices: &Path,
    agent: uuid::Uuid,
    provider: Provider,
    kind: FakeKind,
    script: &Path,
) -> std::io::Result<()> {
    let kind = match kind {
        FakeKind::ClaudePty => "claude_pty",
        FakeKind::ClaudeSdk => "claude_sdk",
        FakeKind::Codex => "codex",
    };
    dispatch::choose(
        choices,
        &agent.to_string(),
        &Choice {
            provider: runs(provider),
            kind: Some(kind.to_owned()),
            script: Some(script.to_owned()),
            input: Some(script.with_extension("input")),
        },
    )
}

fn runs(provider: Provider) -> Runs {
    match provider {
        Provider::Real => Runs::Real,
        Provider::Fake => Runs::Fake,
    }
}

/// The scratch Codex home a real host's agents run on: only the operator's
/// login, so their configuration, tool servers and history stay out.
pub(crate) fn seed_codex_home(dir: &Path) -> Result<PathBuf, String> {
    let source = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| os::paths::home_dir().map(|home| home.join(".codex")))
        .ok_or("no Codex login: neither CODEX_HOME nor a home folder is set")?;
    let auth = source.join("auth.json");
    if !auth.exists() {
        return Err(format!(
            "no Codex login: {} does not exist; run `codex login`",
            auth.display()
        ));
    }
    let home = dir.join("codex-home");
    std::fs::create_dir_all(&home).map_err(|error| error.to_string())?;
    std::fs::copy(&auth, home.join("auth.json")).map_err(|error| error.to_string())?;
    // Codex refuses a pristine home before it starts its app server; the
    // markers are its local setup, nothing of the operator's. The
    // operator's plugins and connected apps come with the account, not the
    // home, so the config switches them off: the agent then carries only
    // amux's tool server and Codex's own built-in skills.
    for (file, text) in [
        (
            "config.toml",
            // The model is what an agent gets when nobody chose one, such
            // as a child made by amux's spawn tool.
            format!(
                "model = \"{SMALL_CODEX}\"\n\n[features]\nplugins = false\nremote_plugin = false\napps = false\n"
            ),
        ),
        (".personality_migration", "v1\n".to_owned()),
        (".sandbox_migration", "v1\n".to_owned()),
        ("installation_id", format!("{}\n", uuid::Uuid::new_v4())),
    ] {
        std::fs::write(home.join(file), text).map_err(|error| error.to_string())?;
    }
    os::fs::make_private(&home).map_err(|error| error.to_string())?;
    Ok(home)
}

/// The small models a real host's agents start on, whoever creates them.
pub(crate) const SMALL_CLAUDE: &str = "haiku";
pub(crate) const SMALL_CODEX: &str = "gpt-5.6-luna";

/// Writes the host's `claude`, `codex` and `acp` into its `bin/`. A
/// command already there keeps its binary, which may be running, and takes
/// the new plan.
pub(crate) fn write(wrappers: &Wrappers<'_>) -> Result<Commands, String> {
    let bin = wrappers.dir.join("bin");
    std::fs::create_dir_all(&bin).map_err(|error| error.to_string())?;
    let codex_home = match wrappers.real {
        Some(_) => Some(seed_codex_home(wrappers.dir)?),
        None => None,
    };
    let default = Choice {
        provider: runs(wrappers.default),
        kind: None,
        script: wrappers.host_script.map(Path::to_owned),
        input: wrappers
            .host_script
            .map(|script| script.with_extension("input")),
    };
    let host = |command: dispatch::Command, fake_env: &[(&str, PathBuf)], real: Option<Real>| {
        HostCommand {
            command,
            host: wrappers.host.to_owned(),
            default: default.clone(),
            choices: wrappers.choices.to_owned(),
            fake_env: fake_env
                .iter()
                .map(|(key, value)| ((*key).to_owned(), value.to_string_lossy().into_owned()))
                .collect(),
            agent_script_env: None,
            real,
        }
    };
    let real = |capture: &str, probe: &Path, real: &Path| Real {
        probe: probe.to_owned(),
        real: real.to_owned(),
        capture: capture.to_owned(),
        recordings: wrappers.recordings.to_owned(),
        bin: bin.clone(),
        args: Vec::new(),
        set: BTreeMap::new(),
        unset: Vec::new(),
        make: Vec::new(),
        small_claude: None,
    };
    let claude = host(
        dispatch::Command::Claude,
        &[("CLAUDE_CONFIG_DIR", wrappers.dir.join("claude"))],
        wrappers.real.map(|tools| Real {
            args: ["--setting-sources", "project", "--strict-mcp-config"]
                .map(str::to_owned)
                .into(),
            set: [
                "DISABLE_AUTOUPDATER",
                "DISABLE_UPDATES",
                "DISABLE_INSTALLATION_CHECKS",
            ]
            .map(|key| (key.to_owned(), "1".to_owned()))
            .into(),
            unset: vec!["CLAUDE_CONFIG_DIR".to_owned()],
            small_claude: Some(SMALL_CLAUDE.to_owned()),
            ..real("CLAUDE", &tools.claude_probe, &tools.claude)
        }),
    );
    let codex = host(
        dispatch::Command::Codex,
        &[],
        wrappers
            .real
            .zip(codex_home.as_deref())
            .map(|(tools, home)| {
                // Codex also reads skills from the user's home, outside its
                // own: a home of its own keeps the operator's out.
                let user = home.with_file_name("codex-user-home");
                Real {
                    set: [
                        ("CODEX_HOME".to_owned(), home.to_string_lossy().into_owned()),
                        ("HOME".to_owned(), user.to_string_lossy().into_owned()),
                    ]
                    .into(),
                    make: vec![user],
                    ..real("CODEX", &tools.codex_probe, &tools.codex)
                }
            }),
    );
    let acp = HostCommand {
        // An agent the topology configures with a script of its own is the
        // fake playing it, on a real host too: there is no real ACP agent
        // behind this command to fall back on.
        agent_script_env: Some(AGENT_SCRIPT_ENV.to_owned()),
        ..host(
            dispatch::Command::Acp,
            // The host's own, so an agent restarted on it reopens its
            // session and nothing of the net's is left outside it.
            &[(
                provider_fakes::acp::SESSIONS_ENV,
                wrappers.dir.join("acp-sessions"),
            )],
            None,
        )
    };
    let commands = Commands {
        claude: dispatch::program(&bin, "claude"),
        codex: dispatch::program(&bin, "codex"),
        acp: dispatch::program(&bin, "acp"),
    };
    let binary = dispatch::program(wrappers.fakes, dispatch::BINARY);
    for (path, command) in [
        (&commands.claude, claude),
        (&commands.codex, codex),
        (&commands.acp, acp),
    ] {
        dispatch::install(&binary, path, &Plan::Host(Box::new(command)))
            .map_err(|error| format!("writing {}: {error}", path.display()))?;
    }
    Ok(commands)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A host's commands carry what the net set up for it: the host's
    /// script for an agent with no choice of its own, on a fake host no
    /// real provider to fall back on, and for `acp` the variable a
    /// configured agent's script comes in.
    #[test]
    fn a_hosts_commands_play_its_script() {
        let dir = tempfile::tempdir().expect("a folder");
        let fakes = dir.path().join("fakes");
        std::fs::create_dir_all(&fakes).expect("fakes");
        std::fs::write(dispatch::program(&fakes, dispatch::BINARY), "a program")
            .expect("the binary");
        let host_script = dir.path().join("studio.host.json");
        let commands = write(&Wrappers {
            host: "studio",
            dir: &dir.path().join("host"),
            default: Provider::Fake,
            choices: &dir.path().join("choices"),
            recordings: &dir.path().join("recordings"),
            real: None,
            fakes: &fakes,
            host_script: Some(&host_script),
        })
        .expect("the commands");
        for command in [&commands.claude, &commands.codex, &commands.acp] {
            assert!(command.is_file(), "{}", command.display());
            let Plan::Host(plan) = dispatch::read_plan(command).expect("a plan") else {
                panic!("a host command");
            };
            assert_eq!(plan.default.script.as_deref(), Some(host_script.as_path()));
            assert_eq!(plan.real, None);
            assert_eq!(
                plan.agent_script_env.as_deref(),
                (command == &commands.acp).then_some(AGENT_SCRIPT_ENV)
            );
        }
    }
}
