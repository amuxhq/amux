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
    AgentParent, AgentSpec, Bootstrap, Catalogue, ClaudeCreateConfig, CodexCreateConfig,
    EffectiveConfig, Kind, PromptSubmission, bootstrap,
};

use crate::runtime::Launch;

/// A client's first prompt as what seeds the new incarnation's queue.
pub fn prompt_bootstrap(prompt: PromptSubmission) -> Bootstrap {
    Bootstrap {
        of: Some(bootstrap::Of::Prompt(prompt)),
    }
}

/// A creation's settings that do not decode as its kind's: the call is
/// refused rather than resolved as some other provider's settings.
#[derive(Debug, thiserror::Error)]
#[error("the creation settings are not {kind:?} settings: {error}")]
pub struct ConfigError {
    pub kind: Kind,
    pub error: prost::DecodeError,
}

/// What a creation asked for, resolved into provider arguments and the
/// agent's configuration. Carried from one incarnation's spec to the next.
#[derive(Clone, Debug, Default)]
pub struct Resolved {
    pub provider_args: Vec<String>,
    pub model: Option<String>,
    pub permission: Option<String>,
    pub mode: Option<String>,
}

/// Resolves a creation's per-provider settings, decoded as the message
/// `kind` names: the kind decides, never the bytes. Claude takes its model
/// and permission as arguments; Codex takes the model in its thread start,
/// the settings a permission names as configuration overrides, and its mode
/// in the first turn the interpreter starts.
pub fn resolve(kind: Kind, config: &[u8]) -> Result<Resolved, ConfigError> {
    let refused = |error| ConfigError { kind, error };
    Ok(match kind {
        Kind::ClaudePty | Kind::ClaudeSdk => {
            let claude = ClaudeCreateConfig::decode(config).map_err(refused)?;
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
            }
        }
        Kind::Codex => {
            let codex = CodexCreateConfig::decode(config).map_err(refused)?;
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
            }
        }
        Kind::Unspecified => Resolved::default(),
    })
}

/// The provider command as configured for a kind.
pub fn provider_command(launch: &Launch, kind: Kind) -> String {
    match kind {
        Kind::Codex => launch.codex_command.clone(),
        _ => launch.claude_command.clone(),
    }
}

/// Where a command resolves on the PATH, the way the agent process will
/// launch it; the command itself when it names a path or is not found.
pub fn resolve_binary(command: &str) -> String {
    let path = Path::new(command);
    if path.components().count() > 1 {
        return command.to_owned();
    }
    let Some(dirs) = std::env::var_os("PATH") else {
        return command.to_owned();
    };
    for dir in std::env::split_paths(&dirs) {
        let candidate = dir.join(format!("{command}{}", std::env::consts::EXE_SUFFIX));
        if candidate.is_file() {
            return candidate.to_string_lossy().into_owned();
        }
    }
    command.to_owned()
}

/// Everything one incarnation's spec is built from.
pub struct Incarnation<'a> {
    pub agent_id: &'a [u8],
    pub profile_id: &'a [u8],
    pub kind: Kind,
    pub cwd: &'a str,
    pub name: &'a str,
    pub parent: Option<AgentParent>,
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
        profile_id: at.profile_id.to_vec(),
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
        provider_binary: resolve_binary(&command),
        provider_command: command,
        config: Some(EffectiveConfig {
            grace_ms: millis(agent.grace_secs),
            drain_ms: millis(agent.drain_secs),
            facts_ring_bytes: agent.facts_ring_mib.saturating_mul(1024 * 1024),
            journal_segment_bytes: launch.journal_segment_bytes,
            model: at.resolved.model.clone(),
            permission: at.resolved.permission.clone(),
            mode: at.resolved.mode.clone(),
            install_path: launch.install_path.to_string_lossy().into_owned(),
        }),
        daemon_version: crate::version().to_owned(),
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
    use super::*;

    fn claude(config: ClaudeCreateConfig) -> Vec<u8> {
        config.encode_to_vec()
    }

    fn codex(config: CodexCreateConfig) -> Vec<u8> {
        config.encode_to_vec()
    }

    #[test]
    fn a_claude_permission_is_its_permission_mode_argument() {
        let resolved = resolve(
            Kind::ClaudeSdk,
            &claude(ClaudeCreateConfig {
                permission: Some("acceptEdits".into()),
                ..ClaudeCreateConfig::default()
            }),
        )
        .unwrap();
        assert_eq!(resolved.provider_args, ["--permission-mode", "acceptEdits"]);
        assert_eq!(resolved.permission.as_deref(), Some("acceptEdits"));
    }

    #[test]
    fn a_codex_permission_is_the_settings_it_names_and_its_mode_rides_along() {
        let resolved = resolve(
            Kind::Codex,
            &codex(CodexCreateConfig {
                permission: Some("auto".into()),
                mode: Some("plan".into()),
                ..CodexCreateConfig::default()
            }),
        )
        .unwrap();
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

        let unnamed = resolve(
            Kind::Codex,
            &codex(CodexCreateConfig {
                permission: Some("on-request/read-only".into()),
                ..CodexCreateConfig::default()
            }),
        )
        .unwrap();
        assert!(unnamed.provider_args.is_empty(), "{unnamed:?}");
    }

    /// The kind decides how the settings are read. Claude's and Codex's
    /// settings are both strings on the wire, so one decodes as the other;
    /// what matters is that it is then read as the kind's own settings and
    /// never handed to the other provider, and that bytes which are not the
    /// kind's settings at all refuse the creation.
    #[test]
    fn config_must_match_kind() {
        let claude_settings = claude(ClaudeCreateConfig {
            args: vec!["--dangerously-skip-permissions".into()],
            model: Some("opus".into()),
            effort: None,
            permission: Some("bypassPermissions".into()),
        });
        let as_codex = resolve(Kind::Codex, &claude_settings).unwrap();
        assert!(
            !as_codex
                .provider_args
                .iter()
                .any(|arg| arg.starts_with("--dangerously") || arg == "--permission-mode"),
            "a Codex agent never takes Claude's arguments: {as_codex:?}"
        );
        assert_ne!(
            as_codex.permission.as_deref(),
            Some("bypassPermissions"),
            "read as Codex settings, Claude's permission is not Codex's"
        );

        let codex_settings = codex(CodexCreateConfig {
            permission: Some("full-access".into()),
            ..CodexCreateConfig::default()
        });
        for kind in [Kind::ClaudePty, Kind::ClaudeSdk] {
            let as_claude = resolve(kind, &codex_settings).unwrap();
            assert!(
                !as_claude.provider_args.iter().any(|arg| arg == "--config"),
                "a Claude agent never takes Codex's overrides: {as_claude:?}"
            );
        }

        // A string field holding bytes that are not text, and a field cut
        // short: neither is any kind's settings.
        for bytes in [vec![0x12, 0x01, 0xff], vec![0x0a, 0x05, b'x']] {
            for kind in [Kind::ClaudePty, Kind::ClaudeSdk, Kind::Codex] {
                let refused = resolve(kind, &bytes).unwrap_err();
                assert_eq!(refused.kind, kind);
            }
        }
    }
}
