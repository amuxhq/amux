//! `agents/<id>/spec.<n>`: one immutable file per incarnation, written by
//! the daemon before it spawns that incarnation and never rewritten.
//!
//! A spec holds everything the agent process needs to start, so any later
//! daemon can start it identically and a person can read why it was
//! started the way it was. A resume writes the next file from the current
//! configuration; config is frozen per incarnation.

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use prost::Message as _;
use wire::{
    AgentRef, AgentSpec, Bootstrap, Catalogue, EffectiveConfig, Kind, PromptSubmission, bootstrap,
    create_agent_request,
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
    pub provider_args: Vec<String>,
    pub model: Option<String>,
    pub permission: Option<String>,
    pub mode: Option<String>,
    pub effort: Option<String>,
}

/// The settings an agent last ran with, as its snapshot says them; each
/// one absent is carried from the previous incarnation's spec.
#[derive(Clone, Debug, Default)]
pub struct Settings {
    pub model: Option<String>,
    pub permission: Option<String>,
    pub mode: Option<String>,
    pub effort: Option<String>,
}

/// What a resumed incarnation starts with: the previous one's extra
/// arguments, and the model, permission, mode and effort the agent last
/// ran with, not the ones it was created with. A setting changed in the
/// chat survives a stop and a resume.
pub fn carry(previous: &AgentSpec, latest: Settings) -> Resolved {
    use create_agent_request::Agent;
    let config = previous.config.clone().unwrap_or_default();
    let args = without_settings(&previous.provider_args, &config);
    let model = latest.model.or(config.model);
    let permission = latest.permission.or(config.permission);
    let mode = latest.mode.or(config.mode);
    let effort = latest.effort.or(config.effort);
    match previous.kind() {
        Kind::Codex => resolve(&Agent::Codex(wire::CodexCreateConfig {
            model,
            effort,
            permission,
            mode,
        })),
        _ => resolve(&Agent::ClaudeSdk(wire::ClaudeCreateConfig {
            args,
            model,
            effort,
            permission,
        })),
    }
}

/// Claude's arguments without the settings [`resolve`] appended for the
/// previous incarnation, leaving exactly the ones the person gave, their
/// own `--model` or `--effort` among them: [`resolve`] appends the
/// settings again, after the person's, so the ones last run with win.
fn without_settings(args: &[String], config: &EffectiveConfig) -> Vec<String> {
    let appended: Vec<String> = [
        ("--model", &config.model),
        ("--permission-mode", &config.permission),
        ("--effort", &config.effort),
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

/// Resolves a creation's per-provider settings. Claude takes its model and
/// permission as arguments; Codex takes the model in its thread start, the
/// settings a permission names as configuration overrides, and its mode in
/// the first turn the interpreter starts.
pub fn resolve(agent: &create_agent_request::Agent) -> Resolved {
    use create_agent_request::Agent;
    match agent {
        Agent::ClaudePty(claude) | Agent::ClaudeSdk(claude) => {
            let claude = claude.clone();
            let mut args = claude.args.clone();
            if let Some(model) = &claude.model {
                args.extend(["--model".to_owned(), model.clone()]);
            }
            if let Some(permission) = &claude.permission {
                args.extend(["--permission-mode".to_owned(), permission.clone()]);
            }
            if let Some(effort) = &claude.effort {
                args.extend(["--effort".to_owned(), effort.clone()]);
            }
            Resolved {
                provider_args: args,
                model: claude.model,
                permission: claude.permission,
                mode: None,
                effort: claude.effort,
            }
        }
        Agent::Codex(codex) => {
            let codex = codex.clone();
            let mut args = Vec::new();
            let named = codex
                .permission
                .as_deref()
                .and_then(interpret::codex::permission);
            for (key, value) in [
                ("approval_policy", named.map(|named| named.approval_policy)),
                ("sandbox_mode", named.map(|named| named.sandbox)),
                (
                    "approvals_reviewer",
                    named.map(|named| named.approvals_reviewer),
                ),
                ("model_reasoning_effort", codex.effort.as_deref()),
            ] {
                if let Some(value) = value {
                    args.extend([
                        "--config".to_owned(),
                        format!("{key}={}", serde_json::json!(value)),
                    ]);
                }
            }
            Resolved {
                provider_args: args,
                model: codex.model,
                permission: codex.permission,
                mode: codex.mode,
                effort: codex.effort,
            }
        }
    }
}

/// A person's model or effort pick, for the host to start later agents of
/// its kind on. None leaves that setting as it was.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Picked {
    pub kind: Kind,
    pub model: Option<String>,
    pub effort: Option<String>,
}

impl Picked {
    /// A pick worth keeping: only amux's own chat keeps them, since Claude
    /// in its own terminal follows Claude's settings.
    fn worth_keeping(self) -> Option<Self> {
        (matches!(self.kind, Kind::ClaudeSdk | Kind::Codex)
            && (self.model.is_some() || self.effort.is_some()))
        .then_some(self)
    }

    /// The pick a mid-chat input makes, if it makes one.
    pub fn of_input(input: &wire::Input) -> Option<Self> {
        use wire::{KindInput, claude_sdk_input as sdk, codex_input as codex};
        let (kind, model, effort) = match input.decode_body()? {
            KindInput::ClaudeSdk(input) => match input.of? {
                sdk::Of::Model(set) => (Kind::ClaudeSdk, set.model, None),
                sdk::Of::Effort(set) => (Kind::ClaudeSdk, None, set.effort),
                _ => return None,
            },
            KindInput::Codex(input) => match input.of? {
                codex::Of::Model(set) => (Kind::Codex, set.model, None),
                codex::Of::Effort(set) => (Kind::Codex, None, set.effort),
                _ => return None,
            },
            KindInput::ClaudePty(_) => return None,
        };
        Self {
            kind,
            model,
            effort,
        }
        .worth_keeping()
    }
}

/// The pick a creation asks the host to keep, if it asks.
pub fn picked_at_creation(
    agent: &create_agent_request::Agent,
    request: &wire::CreateAgentRequest,
) -> Option<Picked> {
    use create_agent_request::Agent;
    let (model, effort) = match agent {
        Agent::ClaudePty(claude) | Agent::ClaudeSdk(claude) => (&claude.model, &claude.effort),
        Agent::Codex(codex) => (&codex.model, &codex.effort),
    };
    Picked {
        kind: agent.kind(),
        model: model.clone().filter(|_| request.remember_model),
        effort: effort.clone().filter(|_| request.remember_effort),
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

/// `agent` with what it does not choose filled from the host's remembered
/// picks. Claude in its own terminal keeps to Claude's settings, arguments
/// the person passed win, and a remembered effort its model does not take is
/// left for the provider to decide.
pub fn with_remembered(
    agent: &create_agent_request::Agent,
    remembered: &settings::NewAgentSettings,
    offered: Option<&wire::Catalogue>,
) -> create_agent_request::Agent {
    use create_agent_request::Agent;
    let named = |args: &[String], flag: &str| {
        args.iter()
            .any(|arg| arg == flag || arg.starts_with(&format!("{flag}=")))
    };
    let mut filled = agent.clone();
    let (model, effort, defaults, passed_model, passed_effort) = match &mut filled {
        Agent::ClaudePty(_) => return filled,
        Agent::ClaudeSdk(claude) => {
            let passed_model = named(&claude.args, "--model");
            let passed_effort = named(&claude.args, "--effort");
            (
                &mut claude.model,
                &mut claude.effort,
                &remembered.claude,
                passed_model,
                passed_effort,
            )
        }
        Agent::Codex(codex) => (
            &mut codex.model,
            &mut codex.effort,
            &remembered.codex,
            false,
            false,
        ),
    };
    if model.is_none() && !passed_model {
        model.clone_from(&defaults.model);
    }
    if effort.is_none() && !passed_effort {
        let starting = model
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
        *effort = defaults.effort.clone().filter(taken);
    }
    filled
}

/// The provider command as configured for a kind.
pub fn provider_command(launch: &Launch, kind: Kind) -> String {
    match kind {
        Kind::Codex => launch.codex_command.clone(),
        _ => launch.claude_command.clone(),
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
}

pub fn build(launch: &Launch, at: Incarnation<'_>) -> AgentSpec {
    let command = provider_command(launch, at.kind);
    let agent = &launch.agent;
    AgentSpec {
        agent_id: at.agent_id.to_vec(),
        kind: at.kind as i32,
        cwd: at.cwd.to_owned(),
        name: at.name.to_owned(),
        parent: at.parent,
        provider_args: at.resolved.provider_args.clone(),
        provider_env: launch
            .provider_env
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect::<HashMap<_, _>>(),
        provider_command: command,
        config: Some(EffectiveConfig {
            grace_ms: millis(agent.grace_secs),
            drain_ms: millis(agent.drain_secs),
            facts_ring_bytes: agent.facts_ring_mib.saturating_mul(1024 * 1024),
            model: at.resolved.model.clone(),
            permission: at.resolved.permission.clone(),
            mode: at.resolved.mode.clone(),
            effort: at.resolved.effort.clone(),
            install_path: launch.install_path.to_string_lossy().into_owned(),
            step_budget_micros: launch.agent_step_budget_micros,
            // The agent's default; only its own tests write a size here.
            journal_segment_bytes: 0,
        }),
        created_at_ms: at.created_at_ms,
        incarnation: at.incarnation,
        bootstrap: at.bootstrap,
        offered: at.offered,
        base_branch: at.base_branch,
    }
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
    use wire::create_agent_request::Agent;
    use wire::{ClaudeCreateConfig, CodexCreateConfig};

    use super::*;

    #[test]
    fn a_resume_carries_the_persons_arguments_and_the_settings_last_run_with() {
        let created = resolve(&Agent::ClaudePty(ClaudeCreateConfig {
            args: vec!["--verbose".into(), "--add-dir".into(), "/tmp".into()],
            model: Some("opus".into()),
            permission: Some("plan".into()),
            effort: None,
        }));
        let previous = AgentSpec {
            kind: Kind::ClaudePty as i32,
            provider_args: created.provider_args,
            config: Some(EffectiveConfig {
                model: created.model,
                permission: created.permission,
                ..EffectiveConfig::default()
            }),
            ..AgentSpec::default()
        };
        let resumed = carry(
            &previous,
            Settings {
                permission: Some("acceptEdits".into()),
                effort: Some("high".into()),
                ..Settings::default()
            },
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
        assert_eq!(resumed.effort.as_deref(), Some("high"));
    }

    #[test]
    fn a_resume_keeps_the_persons_own_setting_arguments() {
        let created = resolve(&Agent::ClaudePty(ClaudeCreateConfig {
            args: vec![
                "--model".into(),
                "sonnet".into(),
                "--permission-mode=plan".into(),
                "--effort".into(),
                "low".into(),
            ],
            model: None,
            permission: None,
            effort: Some("high".into()),
        }));
        let previous = AgentSpec {
            kind: Kind::ClaudePty as i32,
            provider_args: created.provider_args,
            config: Some(EffectiveConfig {
                model: created.model,
                permission: created.permission,
                effort: created.effort,
                ..EffectiveConfig::default()
            }),
            ..AgentSpec::default()
        };
        let resumed = carry(&previous, Settings::default());
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

    #[test]
    fn a_claude_permission_is_its_permission_mode_argument() {
        let resolved = resolve(&Agent::ClaudeSdk(ClaudeCreateConfig {
            permission: Some("acceptEdits".into()),
            ..ClaudeCreateConfig::default()
        }));
        assert_eq!(resolved.provider_args, ["--permission-mode", "acceptEdits"]);
        assert_eq!(resolved.permission.as_deref(), Some("acceptEdits"));
    }

    #[test]
    fn a_codex_permission_is_the_settings_it_names_and_its_mode_rides_along() {
        let resolved = resolve(&Agent::Codex(CodexCreateConfig {
            permission: Some("auto".into()),
            mode: Some("plan".into()),
            ..CodexCreateConfig::default()
        }));
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
        assert_eq!(resolved.mode.as_deref(), Some("plan"));

        let unnamed = resolve(&Agent::Codex(CodexCreateConfig {
            permission: Some("on-request/read-only".into()),
            ..CodexCreateConfig::default()
        }));
        assert!(unnamed.provider_args.is_empty(), "{unnamed:?}");
    }

    fn remembered() -> settings::NewAgentSettings {
        settings::NewAgentSettings {
            claude: settings::NewAgentDefaults {
                model: Some("sonnet".into()),
                effort: Some("xhigh".into()),
            },
            ..Default::default()
        }
    }

    fn claude(model: Option<&str>) -> ClaudeCreateConfig {
        ClaudeCreateConfig {
            model: model.map(str::to_owned),
            ..Default::default()
        }
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
            &Agent::ClaudeSdk(claude(None)),
            &remembered(),
            Some(&offered(&["high", "xhigh"])),
        );
        let Agent::ClaudeSdk(filled) = filled else {
            panic!("the kind stays");
        };
        assert_eq!(filled.model.as_deref(), Some("sonnet"));
        assert_eq!(filled.effort.as_deref(), Some("xhigh"));

        let chosen = with_remembered(
            &Agent::ClaudeSdk(claude(Some("haiku"))),
            &remembered(),
            None,
        );
        let Agent::ClaudeSdk(chosen) = chosen else {
            panic!("the kind stays");
        };
        assert_eq!(chosen.model.as_deref(), Some("haiku"));
    }

    #[test]
    fn claude_in_its_own_terminal_keeps_to_claudes_settings() {
        let agent = Agent::ClaudePty(claude(None));
        assert_eq!(with_remembered(&agent, &remembered(), None), agent);
    }

    #[test]
    fn a_persons_arguments_beat_a_remembered_pick() {
        let filled = with_remembered(
            &Agent::ClaudeSdk(ClaudeCreateConfig {
                args: vec!["--model=opus".into(), "--effort".into(), "low".into()],
                ..Default::default()
            }),
            &remembered(),
            None,
        );
        let Agent::ClaudeSdk(filled) = filled else {
            panic!("the kind stays");
        };
        assert_eq!((filled.model, filled.effort), (None, None));
    }

    #[test]
    fn a_remembered_effort_the_model_cannot_take_is_left_out() {
        let filled = with_remembered(
            &Agent::ClaudeSdk(claude(None)),
            &remembered(),
            Some(&offered(&["low", "high"])),
        );
        let Agent::ClaudeSdk(filled) = filled else {
            panic!("the kind stays");
        };
        assert_eq!(filled.effort, None);
    }

    #[test]
    fn only_a_marked_creation_keeps_its_picks() {
        let agent = Agent::ClaudeSdk(ClaudeCreateConfig {
            model: Some("sonnet".into()),
            effort: Some("high".into()),
            ..Default::default()
        });
        let request = |remember_model, remember_effort| wire::CreateAgentRequest {
            remember_model,
            remember_effort,
            ..Default::default()
        };
        assert_eq!(picked_at_creation(&agent, &request(false, false)), None);
        assert_eq!(
            picked_at_creation(&agent, &request(true, false)),
            Some(Picked {
                kind: Kind::ClaudeSdk,
                model: Some("sonnet".into()),
                effort: None,
            })
        );
        let terminal = Agent::ClaudePty(claude(Some("sonnet")));
        assert_eq!(picked_at_creation(&terminal, &request(true, true)), None);
    }
}
