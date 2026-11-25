//! `agents/<id>/spec.<n>`: one immutable file per incarnation, written by
//! the daemon before it spawns that incarnation and never rewritten.
//!
//! A spec holds everything the agent process needs to start, so any later
//! daemon can start it identically and a person can read why it was
//! started the way it was. A resume writes the next file from the current
//! configuration; config is frozen per incarnation.

use std::collections::{BTreeMap, HashMap};
use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use prost::Message as _;
use wire::{
    AgentRef, AgentSettings, AgentSpec, Bootstrap, Catalogue, EffectiveConfig, Kind,
    PromptSubmission, bootstrap,
};

use crate::runtime::{Launch, RegistryError};

/// A client's first prompt as what seeds the new incarnation's queue.
///
/// Daemons never read a later prompt, which goes to the interpreter as it
/// is, but a first prompt starts with them: one holding an attachment this
/// build cannot read is refused here, before any agent starts for it.
pub fn prompt_bootstrap(prompt: PromptSubmission) -> Result<Bootstrap, RegistryError> {
    let readable = prompt
        .prompt
        .as_ref()
        .is_none_or(|input| attachments::all_readable(&input.attachments));
    if !readable {
        return Err(RegistryError::UnreadableAttachment);
    }
    Ok(Bootstrap {
        of: Some(bootstrap::Of::Prompt(prompt)),
    })
}

/// What a creation asked for, resolved into provider arguments and the
/// agent's configuration. Carried from one incarnation's spec to the next.
#[derive(Clone, Debug, Default)]
pub struct Resolved {
    /// The agent program's provider id: `claude`, `codex`, or an ACP
    /// agent's.
    pub provider: String,
    pub provider_args: Vec<String>,
    /// The model, effort, permission and mode it starts on; never a level,
    /// which a create maps onto a permission before it resolves.
    pub settings: AgentSettings,
}

/// What a resumed incarnation starts with: the previous one's extra
/// arguments, and the model, permission, mode and effort the agent last
/// ran with (`latest`, as its newest snapshot says them), not the ones it
/// was created with; each one `latest` leaves unknown is the previous
/// spec's. A setting changed in the chat survives a stop and a resume. An
/// ACP agent is started as its program is configured now ([`build`]), so
/// nothing of how the previous one started is carried but which agent it
/// is and its settings.
pub fn carry(previous: &AgentSpec, latest: &AgentSettings) -> Resolved {
    let config = previous.config.clone().unwrap_or_default();
    let started = config.settings.unwrap_or_default();
    let args = without_settings(&previous.provider_args, &started);
    let settings = AgentSettings {
        model: latest.model.clone().or(started.model),
        effort: latest.effort.clone().or(started.effort),
        permission: latest.permission.clone().or(started.permission),
        permission_level: 0,
        mode: latest.mode.clone().or(started.mode),
    };
    resolve(previous.kind(), &previous.provider, &settings, &args)
}

/// Claude's arguments without the settings [`resolve`] appended for the
/// previous incarnation, leaving exactly the ones the person gave, their
/// own `--model` or `--effort` among them: [`resolve`] appends the
/// settings again, after the person's, so the ones last run with win.
fn without_settings(args: &[String], settings: &AgentSettings) -> Vec<String> {
    let appended: Vec<String> = [
        ("--model", &settings.model),
        ("--permission-mode", &settings.permission),
        ("--effort", &settings.effort),
    ]
    .into_iter()
    .filter_map(|(flag, value)| value.as_ref().map(|value| [flag.to_owned(), value.clone()]))
    .flatten()
    .collect();
    match args.strip_suffix(appended.as_slice()) {
        Some(persons) => persons.to_vec(),
        None => args.to_vec(),
    }
}

/// Resolves a creation's settings for the program `kind` runs. Claude takes
/// the person's arguments, then its model, permission and effort as
/// arguments; Codex takes the model in its thread start, the settings a
/// permission names as configuration overrides, and its mode in the first
/// turn the interpreter starts; an ACP agent takes its settings in its
/// session, from the interpreter, never as arguments.
pub fn resolve(kind: Kind, provider: &str, settings: &AgentSettings, args: &[String]) -> Resolved {
    let settings = AgentSettings {
        permission_level: 0,
        ..settings.clone()
    };
    let provider_args = match kind {
        Kind::ClaudePty | Kind::ClaudeSdk => {
            let mut args = args.to_vec();
            for (flag, value) in [
                ("--model", &settings.model),
                ("--permission-mode", &settings.permission),
                ("--effort", &settings.effort),
            ] {
                if let Some(value) = value {
                    args.extend([flag.to_owned(), value.clone()]);
                }
            }
            args
        }
        Kind::Codex => {
            let mut args = Vec::new();
            let named = settings
                .permission
                .as_deref()
                .and_then(wire::codex::permission);
            for (key, value) in [
                ("approval_policy", named.map(|named| named.approval_policy)),
                ("sandbox_mode", named.map(|named| named.sandbox)),
                (
                    "approvals_reviewer",
                    named.map(|named| named.approvals_reviewer),
                ),
                ("model_reasoning_effort", settings.effort.as_deref()),
            ] {
                if let Some(value) = value {
                    args.extend([
                        "--config".to_owned(),
                        format!("{key}={}", serde_json::json!(value)),
                    ]);
                }
            }
            args
        }
        Kind::Acp | Kind::Unspecified => Vec::new(),
    };
    Resolved {
        provider: provider.to_owned(),
        provider_args,
        settings,
    }
}

/// A person's model or effort pick, for the host to start later agents of
/// its program on. None leaves that setting as it was.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Picked {
    pub kind: Kind,
    /// The agent program's provider id, which the pick is kept under.
    pub provider: String,
    pub model: Option<String>,
    pub effort: Option<String>,
}

impl Picked {
    /// A pick worth keeping: only amux's own chat keeps them, since Claude
    /// in its own terminal follows Claude's settings. An ACP agent's are
    /// kept by its id; never its mode, whose default is the agent's own.
    fn worth_keeping(self) -> Option<Self> {
        let kept = match self.kind {
            Kind::ClaudeSdk | Kind::Codex | Kind::Acp => !self.provider.is_empty(),
            Kind::ClaudePty | Kind::Unspecified => false,
        };
        (kept && (self.model.is_some() || self.effort.is_some())).then_some(self)
    }

    /// The pick a mid-chat input makes as it is accepted, if it makes one:
    /// Codex's alone, which takes a model or effort without asking its
    /// program. Headless Claude and an ACP agent ask theirs, which may
    /// refuse, so their picks are kept once the program took them
    /// ([`Picked::taken`]).
    pub fn of_input(input: &wire::Input, provider: &str) -> Option<Self> {
        use wire::{KindInput, codex_input as codex};
        let (model, effort) = match input.decode_body()? {
            KindInput::Codex(input) => match input.of? {
                codex::Of::Model(set) => (set.model, None),
                codex::Of::Effort(set) => (None, set.effort),
                _ => return None,
            },
            KindInput::ClaudeSdk(_) | KindInput::Acp(_) | KindInput::ClaudePty(_) => return None,
        };
        Self {
            kind: Kind::Codex,
            provider: provider.to_owned(),
            model,
            effort,
        }
        .worth_keeping()
    }

    /// The pick an agent's program took, as its journal says, if it is one
    /// to keep.
    pub fn taken(kind: Kind, provider: &str, taken: &AgentSettings) -> Option<Self> {
        Self {
            kind,
            provider: provider.to_owned(),
            model: taken.model.clone(),
            effort: taken.effort.clone(),
        }
        .worth_keeping()
    }
}

/// The pick a creation asks the host to keep, if it asks: its model with
/// `remember_model`, its effort with `remember_effort`.
pub fn picked_at_creation(
    kind: Kind,
    provider: &str,
    settings: &AgentSettings,
    remember_model: bool,
    remember_effort: bool,
) -> Option<Picked> {
    Picked {
        kind,
        provider: provider.to_owned(),
        model: settings.model.clone().filter(|_| remember_model),
        effort: settings.effort.clone().filter(|_| remember_effort),
    }
    .worth_keeping()
}

/// Where a person's picks go: the installation's settings, or nowhere in a
/// test that has none.
#[derive(Clone)]
pub struct PickKeeper(std::sync::Arc<dyn Fn(Picked) + Send + Sync>);

impl PickKeeper {
    pub fn new(keep: impl Fn(Picked) + Send + Sync + 'static) -> Self {
        Self(std::sync::Arc::new(keep))
    }

    pub(crate) fn keep(&self, picked: Picked) {
        (self.0)(picked);
    }
}

impl Default for PickKeeper {
    fn default() -> Self {
        Self::new(|_| {})
    }
}

/// `settings` with what they do not choose filled from the host's
/// remembered picks for `provider`. Claude in its own terminal keeps to
/// Claude's settings, a model or effort the person passed as Claude's own
/// arguments wins, and a remembered effort its model does not take is left
/// for the provider to decide. A mode is never remembered.
pub fn with_remembered(
    kind: Kind,
    provider: &str,
    settings: &AgentSettings,
    args: &[String],
    remembered: &settings::NewAgentSettings,
    offered: Option<&wire::Catalogue>,
) -> AgentSettings {
    let mut filled = settings.clone();
    if kind == Kind::ClaudePty {
        return filled;
    }
    let named = |flag: &str| {
        args.iter()
            .any(|arg| arg == flag || arg.starts_with(&format!("{flag}=")))
    };
    let defaults = remembered.of(provider);
    if filled.model.is_none() && !named("--model") {
        filled.model.clone_from(&defaults.model);
    }
    if filled.effort.is_none() && !named("--effort") {
        let starting = filled
            .model
            .clone()
            .or_else(|| offered.and_then(|catalogue| catalogue.new_agent.as_ref()?.model.clone()));
        let entry = starting.as_deref().and_then(|starting| {
            offered?
                .models
                .iter()
                .find(|listed| listed.value == starting || listed.resolved_model == starting)
        });
        let taken = |value: &String| {
            entry.is_none_or(|entry| entry.efforts.is_empty() || entry.efforts.contains(value))
        };
        filled.effort = defaults.effort.clone().filter(taken);
    }
    filled
}

/// How the agent's program is started: its command, the arguments it is
/// started with and the environment added to the daemon's.
struct Program {
    command: String,
    args: Vec<String>,
    env: HashMap<String, String>,
}

/// The program `kind` runs as configured now. Claude and Codex are each one
/// configured command, with the arguments the creation resolved to. An ACP
/// agent is the program its id names among the host's, found where it will
/// be started from, with that program's own arguments and environment over
/// the host's: an agent whose program is gone, or no longer configured, is
/// refused rather than started as something else.
fn program(launch: &Launch, kind: Kind, resolved: &Resolved) -> Result<Program, RegistryError> {
    let common = |command: &str| Program {
        command: command.to_owned(),
        args: resolved.provider_args.clone(),
        env: launch.provider_env.clone(),
    };
    match kind {
        Kind::Codex => Ok(common(&launch.codex_command)),
        Kind::ClaudePty | Kind::ClaudeSdk => Ok(common(&launch.claude_command)),
        Kind::Acp => {
            let agent = launch
                .agents
                .iter()
                .find(|agent| agent.id == resolved.provider)
                .ok_or_else(|| RegistryError::UnknownAgent(resolved.provider.clone()))?;
            let path = agent.find(&launch.provider_env).ok_or_else(|| {
                RegistryError::AgentNotInstalled {
                    name: agent.name.clone(),
                    command: agent.command.clone(),
                }
            })?;
            Ok(Program {
                command: path.to_string_lossy().into_owned(),
                args: agent.args.clone(),
                env: agent.environment(&launch.provider_env),
            })
        }
        // An agent with no kind runs no program; its agent process refuses
        // to start.
        Kind::Unspecified => Ok(common("")),
    }
}

/// Everything one incarnation's spec is built from.
pub struct Incarnation<'a> {
    pub agent_id: &'a [u8],
    pub kind: Kind,
    pub cwd: &'a str,
    pub name: &'a str,
    pub parent: Option<AgentRef>,
    pub resolved: &'a Resolved,
    pub created_at_ms: i64,
    pub incarnation: u32,
    pub bootstrap: Option<Bootstrap>,
    /// What the host's provider offers, for a kind that cannot be asked.
    pub offered: Option<Catalogue>,
    /// The branch the agent's worktree was made from, when its host made one.
    pub base_branch: Option<String>,
    /// The parent's conversation a forked agent's first incarnation copies.
    pub fork: Option<wire::ForkFrom>,
}

/// The spec of one incarnation, its program as configured now; refused for
/// an ACP agent this host cannot start.
pub fn build(launch: &Launch, at: Incarnation<'_>) -> Result<AgentSpec, RegistryError> {
    let program = program(launch, at.kind, at.resolved)?;
    let agent = &launch.agent;
    let parentless = at.parent.is_none();
    Ok(AgentSpec {
        agent_id: at.agent_id.to_vec(),
        kind: at.kind as i32,
        cwd: at.cwd.to_owned(),
        name: at.name.to_owned(),
        // The daemon sets these from what the create said and the row.
        name_given: false,
        title: None,
        parent: at.parent,
        provider: at.resolved.provider.clone(),
        provider_args: program.args,
        provider_env: program.env,
        provider_command: program.command,
        config: Some(EffectiveConfig {
            grace_ms: millis(agent.grace_secs),
            drain_ms: millis(agent.drain_secs),
            facts_ring_bytes: agent.facts_ring_mib.saturating_mul(1024 * 1024),
            settings: Some(at.resolved.settings.clone()),
            install_path: launch.install_path.to_string_lossy().into_owned(),
            step_budget_micros: launch.agent_step_budget_micros,
        }),
        created_at_ms: at.created_at_ms,
        incarnation: at.incarnation,
        bootstrap: at.bootstrap,
        offered: at.offered,
        base_branch: at.base_branch,
        fork: at.fork,
        experimental: experimental(launch, parentless),
    })
}

/// What experiments tell the agent process, each under its version, while
/// it is on: for suspension, how long an agent without a parent waits with
/// nothing to do before it suspends. A child already ends itself after its
/// work, so it is told nothing.
fn experimental(launch: &Launch, parentless: bool) -> BTreeMap<String, Vec<u8>> {
    let mut map = BTreeMap::new();
    if parentless && launch.experiments.is_on(&settings::experiments::SUSPENSION) {
        wire::experimental::SUSPENSION.put(
            &mut map,
            &wire::experimental::SuspensionSpec {
                after_ms: launch.suspension.after_ms(),
            },
        );
    }
    map
}

fn millis(secs: u64) -> u32 {
    u32::try_from(secs.saturating_mul(1000)).unwrap_or(u32::MAX)
}

/// The path of incarnation `n`'s spec.
pub fn path(dir: &Path, n: u32) -> PathBuf {
    dir.join(format!("spec.{n}"))
}

/// Writes `spec.<n>`, refusing to replace one that exists: a spec is
/// never rewritten. Written through a temporary file so a reader never
/// sees half a spec.
pub fn write(dir: &Path, spec: &AgentSpec) -> io::Result<()> {
    let target = path(dir, spec.incarnation);
    if target.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{} exists and is never rewritten", target.display()),
        ));
    }
    let temp = dir.join(format!(".spec.{}.tmp", spec.incarnation));
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&temp)?;
    file.write_all(&spec.encode_to_vec())?;
    drop(file);
    fs::rename(&temp, &target)
}

/// The newest spec in a directory and its number.
pub fn newest(dir: &Path) -> io::Result<Option<AgentSpec>> {
    let mut newest: Option<u32> = None;
    for entry in fs::read_dir(dir)? {
        let name = entry?.file_name();
        if let Some(n) = name
            .to_str()
            .and_then(|name| name.strip_prefix("spec."))
            .and_then(|n| n.parse::<u32>().ok())
        {
            newest = newest.max(Some(n));
        }
    }
    let Some(n) = newest else { return Ok(None) };
    let bytes = fs::read(path(dir, n))?;
    AgentSpec::decode(bytes.as_slice())
        .map(Some)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Settings naming what is given.
    fn settings(
        model: Option<&str>,
        effort: Option<&str>,
        permission: Option<&str>,
        mode: Option<&str>,
    ) -> AgentSettings {
        AgentSettings {
            model: model.map(str::to_owned),
            effort: effort.map(str::to_owned),
            permission: permission.map(str::to_owned),
            permission_level: 0,
            mode: mode.map(str::to_owned),
        }
    }

    fn args(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| (*arg).to_owned()).collect()
    }

    /// A spec as the daemon writes it for `resolved`, run by `kind`.
    fn previous(kind: Kind, resolved: Resolved) -> AgentSpec {
        AgentSpec {
            kind: kind as i32,
            provider: resolved.provider,
            provider_args: resolved.provider_args,
            config: Some(EffectiveConfig {
                settings: Some(resolved.settings),
                ..EffectiveConfig::default()
            }),
            ..AgentSpec::default()
        }
    }

    #[test]
    fn a_resume_carries_the_persons_arguments_and_the_settings_last_run_with() {
        let created = resolve(
            Kind::ClaudePty,
            "claude",
            &settings(Some("opus"), None, Some("plan"), None),
            &args(&["--verbose", "--add-dir", "/tmp"]),
        );
        let previous = previous(Kind::ClaudePty, created);
        let resumed = carry(
            &previous,
            &settings(None, Some("high"), Some("acceptEdits"), None),
        );
        assert_eq!(
            resumed.provider_args,
            [
                "--verbose",
                "--add-dir",
                "/tmp",
                "--model",
                "opus",
                "--permission-mode",
                "acceptEdits",
                "--effort",
                "high"
            ]
        );
        assert_eq!(resumed.settings.effort.as_deref(), Some("high"));
    }

    #[test]
    fn a_resume_keeps_the_persons_own_setting_arguments() {
        let created = resolve(
            Kind::ClaudePty,
            "claude",
            &settings(None, Some("high"), None, None),
            &args(&[
                "--model",
                "sonnet",
                "--permission-mode=plan",
                "--effort",
                "low",
            ]),
        );
        let previous = previous(Kind::ClaudePty, created);
        let resumed = carry(&previous, &AgentSettings::NONE);
        assert_eq!(
            resumed.provider_args,
            [
                "--model",
                "sonnet",
                "--permission-mode=plan",
                "--effort",
                "low",
                "--effort",
                "high"
            ]
        );
    }

    /// Only an agent without a parent is told when to suspend, and only
    /// while the experiment is on: off, the spec is what it always was.
    #[test]
    fn an_agent_without_a_parent_is_told_when_to_suspend_while_the_experiment_is_on() {
        use std::time::Duration;

        use settings::experiments::{Enabled, SUSPENSION};
        let resolved = resolve(Kind::ClaudeSdk, "claude", &AgentSettings::NONE, &[]);
        let spec = |launch: &Launch, parent: Option<AgentRef>| {
            build(
                launch,
                Incarnation {
                    agent_id: b"agent",
                    kind: Kind::ClaudeSdk,
                    cwd: "/tmp",
                    name: "agent",
                    parent,
                    resolved: &resolved,
                    created_at_ms: 0,
                    incarnation: 1,
                    bootstrap: None,
                    offered: None,
                    base_branch: None,
                    fork: None,
                },
            )
            .unwrap()
        };
        let mut launch = Launch {
            experiments: Enabled::of(&[&SUSPENSION]),
            ..Launch::default()
        };
        launch.suspension.after = Duration::from_secs(300);
        let parentless = spec(&launch, None);
        assert_eq!(
            wire::experimental::SUSPENSION.get(&parentless.experimental),
            Some(wire::experimental::SuspensionSpec { after_ms: 300_000 })
        );
        assert!(
            spec(&launch, Some(AgentRef::default()))
                .experimental
                .is_empty()
        );
        launch.experiments = Enabled::none();
        assert!(spec(&launch, None).experimental.is_empty());
    }

    /// A launch that knows one ACP agent, `mine`, whose program is in a
    /// folder of its own.
    fn acp_launch(dir: &Path) -> Launch {
        let program = dir.join(format!("mine-agent{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(&program, "#!/bin/sh\n").unwrap();
        os::fs::make_executable(&program).unwrap();
        Launch {
            agents: vec![crate::agents::AgentProgram {
                id: "mine".into(),
                name: "Mine".into(),
                command: program.to_string_lossy().into_owned(),
                args: vec!["serve".into(), "--stdio".into()],
                env: [("MINE_TOKEN".into(), "t".into())].into(),
                built_in: false,
            }],
            provider_env: [("SHARED".into(), "s".into())].into(),
            ..Launch::default()
        }
    }

    fn acp_spec(launch: &Launch, resolved: &Resolved) -> Result<AgentSpec, RegistryError> {
        build(
            launch,
            Incarnation {
                agent_id: b"agent",
                kind: Kind::Acp,
                cwd: "/tmp",
                name: "agent",
                parent: None,
                resolved,
                created_at_ms: 0,
                incarnation: 1,
                bootstrap: None,
                offered: None,
                base_branch: None,
                fork: None,
            },
        )
    }

    /// An ACP agent's spec names the agent by its id, starts the program
    /// its id names where it was found, with that program's arguments and
    /// its environment over the host's, and carries the model, effort and
    /// mode for the interpreter to set in the session.
    #[test]
    fn an_acp_spec_runs_the_program_its_agent_id_names() {
        let dir = tempfile::tempdir().unwrap();
        let launch = acp_launch(dir.path());
        let resolved = resolve(
            Kind::Acp,
            "mine",
            &settings(Some("fast"), Some("high"), None, Some("plan")),
            &[],
        );
        let spec = acp_spec(&launch, &resolved).unwrap();
        assert_eq!(spec.kind(), Kind::Acp);
        assert_eq!(spec.provider, "mine");
        assert_eq!(
            spec.provider_command,
            dir.path()
                .join(format!("mine-agent{}", std::env::consts::EXE_SUFFIX))
                .to_string_lossy()
        );
        assert_eq!(spec.provider_args, ["serve", "--stdio"]);
        assert_eq!(
            spec.provider_env,
            HashMap::from([
                ("SHARED".to_owned(), "s".to_owned()),
                ("MINE_TOKEN".to_owned(), "t".to_owned()),
            ])
        );
        let config = spec.config.unwrap().settings.unwrap();
        assert_eq!(
            (config.model, config.effort, config.mode, config.permission),
            (
                Some("fast".into()),
                Some("high".into()),
                Some("plan".into()),
                None
            )
        );
    }

    /// An agent no program is configured for, or whose program is gone, is
    /// refused rather than started as something else.
    #[test]
    fn an_acp_agent_this_host_cannot_start_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let mut launch = acp_launch(dir.path());
        let unknown = resolve(Kind::Acp, "theirs", &AgentSettings::NONE, &[]);
        assert!(matches!(
            acp_spec(&launch, &unknown),
            Err(RegistryError::UnknownAgent(id)) if id == "theirs"
        ));
        launch.agents[0].command = dir.path().join("gone").to_string_lossy().into_owned();
        let mine = resolve(Kind::Acp, "mine", &AgentSettings::NONE, &[]);
        assert!(matches!(
            acp_spec(&launch, &mine),
            Err(RegistryError::AgentNotInstalled { name, .. }) if name == "Mine"
        ));
    }

    /// A resumed ACP agent is the same agent with the model and effort it
    /// last ran with, started as its program is configured now: the
    /// previous incarnation's command and arguments are not carried.
    #[test]
    fn a_resumed_acp_agent_is_started_as_its_program_is_configured_now() {
        let dir = tempfile::tempdir().unwrap();
        let mut launch = acp_launch(dir.path());
        let created = resolve(
            Kind::Acp,
            "mine",
            &settings(Some("fast"), None, None, Some("plan")),
            &[],
        );
        let previous = acp_spec(&launch, &created).unwrap();
        launch.agents[0].args = vec!["acp".into()];
        let resumed = carry(&previous, &settings(Some("slow"), Some("low"), None, None));
        let spec = acp_spec(&launch, &resumed).unwrap();
        assert_eq!(spec.provider, "mine");
        assert_eq!(spec.provider_args, ["acp"]);
        let config = spec.config.unwrap().settings.unwrap();
        assert_eq!(
            (config.model, config.effort, config.mode),
            (Some("slow".into()), Some("low".into()), Some("plan".into()))
        );
    }

    #[test]
    fn a_claude_permission_is_its_permission_mode_argument() {
        let resolved = resolve(
            Kind::ClaudeSdk,
            "claude",
            &settings(None, None, Some("acceptEdits"), None),
            &[],
        );
        assert_eq!(resolved.provider_args, ["--permission-mode", "acceptEdits"]);
        assert_eq!(resolved.settings.permission.as_deref(), Some("acceptEdits"));
    }

    #[test]
    fn a_codex_permission_is_the_settings_it_names_and_its_mode_rides_along() {
        let resolved = resolve(
            Kind::Codex,
            "codex",
            &settings(None, None, Some("auto"), Some("plan")),
            &[],
        );
        assert_eq!(
            resolved.provider_args,
            [
                "--config",
                "approval_policy=\"on-request\"",
                "--config",
                "sandbox_mode=\"workspace-write\"",
                "--config",
                "approvals_reviewer=\"auto_review\"",
            ]
        );
        assert_eq!(resolved.settings.mode.as_deref(), Some("plan"));

        let unnamed = resolve(
            Kind::Codex,
            "codex",
            &settings(None, None, Some("on-request/read-only"), None),
            &[],
        );
        assert!(unnamed.provider_args.is_empty(), "{unnamed:?}");
    }

    fn remembered() -> settings::NewAgentSettings {
        let mut remembered = settings::NewAgentSettings::default();
        remembered.set(
            "claude",
            settings::NewAgentDefaults {
                model: Some("sonnet".into()),
                effort: Some("xhigh".into()),
            },
        );
        remembered.set(
            "opencode",
            settings::NewAgentDefaults {
                model: Some("anthropic/sonnet".into()),
                effort: Some("high".into()),
            },
        );
        remembered
    }

    fn model(model: Option<&str>) -> AgentSettings {
        settings(model, None, None, None)
    }

    fn offered(efforts: &[&str]) -> wire::Catalogue {
        wire::Catalogue {
            models: vec![wire::OfferedModel {
                value: "sonnet".into(),
                efforts: efforts.iter().map(|effort| (*effort).to_owned()).collect(),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn remembered_picks_fill_only_what_is_not_chosen() {
        let filled = with_remembered(
            Kind::ClaudeSdk,
            "claude",
            &model(None),
            &[],
            &remembered(),
            Some(&offered(&["high", "xhigh"])),
        );
        assert_eq!(filled.model.as_deref(), Some("sonnet"));
        assert_eq!(filled.effort.as_deref(), Some("xhigh"));

        let chosen = with_remembered(
            Kind::ClaudeSdk,
            "claude",
            &model(Some("haiku")),
            &[],
            &remembered(),
            None,
        );
        assert_eq!(chosen.model.as_deref(), Some("haiku"));
    }

    #[test]
    fn claude_in_its_own_terminal_keeps_to_claudes_settings() {
        let filled = with_remembered(
            Kind::ClaudePty,
            "claude",
            &model(None),
            &[],
            &remembered(),
            None,
        );
        assert_eq!(filled, model(None));
    }

    #[test]
    fn a_persons_arguments_beat_a_remembered_pick() {
        let filled = with_remembered(
            Kind::ClaudeSdk,
            "claude",
            &model(None),
            &args(&["--model=opus", "--effort", "low"]),
            &remembered(),
            None,
        );
        assert_eq!((filled.model, filled.effort), (None, None));
    }

    #[test]
    fn a_remembered_effort_the_model_cannot_take_is_left_out() {
        let filled = with_remembered(
            Kind::ClaudeSdk,
            "claude",
            &model(None),
            &[],
            &remembered(),
            Some(&offered(&["low", "high"])),
        );
        assert_eq!(filled.effort, None);
    }

    #[test]
    fn only_a_marked_creation_keeps_its_picks() {
        let chosen = settings(Some("sonnet"), Some("high"), None, None);
        let picked =
            |kind, model, effort| picked_at_creation(kind, "claude", &chosen, model, effort);
        assert_eq!(picked(Kind::ClaudeSdk, false, false), None);
        assert_eq!(
            picked(Kind::ClaudeSdk, true, false),
            Some(Picked {
                kind: Kind::ClaudeSdk,
                provider: "claude".into(),
                model: Some("sonnet".into()),
                effort: None,
            })
        );
        assert_eq!(picked(Kind::ClaudePty, true, true), None);
    }

    #[test]
    fn an_acp_agents_picks_are_remembered_by_its_id_but_never_its_mode() {
        let filled = with_remembered(
            Kind::Acp,
            "opencode",
            &AgentSettings::NONE,
            &[],
            &remembered(),
            None,
        );
        assert_eq!(filled.model.as_deref(), Some("anthropic/sonnet"));
        assert_eq!(filled.effort.as_deref(), Some("high"));
        assert_eq!(filled.mode, None);

        // Another agent's picks are its own.
        let other = with_remembered(
            Kind::Acp,
            "gemini",
            &AgentSettings::NONE,
            &[],
            &remembered(),
            None,
        );
        assert!(other.is_none());

        let chosen = settings(Some("anthropic/opus"), None, None, Some("plan"));
        assert_eq!(
            picked_at_creation(Kind::Acp, "opencode", &chosen, true, true),
            Some(Picked {
                kind: Kind::Acp,
                provider: "opencode".into(),
                model: Some("anthropic/opus".into()),
                effort: None,
            })
        );
    }
}
