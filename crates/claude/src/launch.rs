//! Shared Claude Code process launch construction.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::Value;
use uuid::Uuid;

/// The variable naming the folder Claude reads an organization's managed
/// settings from, in place of the platform's own.
pub const MANAGED_SETTINGS_PATH_ENV: &str = "CLAUDE_CODE_MANAGED_SETTINGS_PATH";

/// Whether an organization manages Claude's MCP servers: a
/// `managed-mcp.json` in the managed-settings folder, the one `folder`
/// names (from [`MANAGED_SETTINGS_PATH_ENV`]) or else the platform's.
/// Claude then refuses `--strict-mcp-config` and any server added on its
/// command line, and does not start.
pub fn managed_mcp(folder: Option<&Path>) -> bool {
    let platform = if cfg!(target_os = "macos") {
        Some(Path::new("/Library/Application Support/ClaudeCode"))
    } else if cfg!(windows) {
        Some(Path::new(r"C:\Program Files\ClaudeCode"))
    } else if cfg!(unix) {
        Some(Path::new("/etc/claude-code"))
    } else {
        None
    };
    folder
        .or(platform)
        .is_some_and(|folder| folder.join("managed-mcp.json").exists())
}

/// Claude variables inherited from a parent Claude session that must not
/// reach a newly hosted session.
pub const CHILD_SESSION_ENV_SCRUB: &[&str] = &[
    "CLAUDECODE",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_PID",
    "CLAUDE_EFFORT",
    "AI_AGENT",
    "TRACEPARENT",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_EXECPATH",
    "CLAUDE_CODE_BRIDGE_SESSION_ID",
    "CLAUDE_CODE_MESSAGING_SOCKET",
    "CLAUDE_CODE_MESSAGING_TOKEN",
];

/// A named MCP server inserted into Claude's managed launch configuration.
#[derive(Debug, Clone, PartialEq)]
pub struct McpServerConfig {
    pub name: String,
    pub config: Value,
}

/// Provider settings amux pins after user settings have been read.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ManagedSettings {
    pub hook_command: Vec<String>,
    pub mcp_servers: Vec<McpServerConfig>,
    pub permissions_allow: Vec<String>,
    /// Let messages from other sessions on the messaging socket through at
    /// once. Without it a session in bypass-permissions mode holds them
    /// behind an approval nobody is there to give.
    pub accept_cross_session: bool,
}

/// Fully merged settings passed as Claude's one managed `--settings` value.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MergedSettings(Value);

impl MergedSettings {
    pub fn empty() -> Self {
        Self(Value::Object(serde_json::Map::new()))
    }

    pub fn as_value(&self) -> &Value {
        &self.0
    }

    pub fn into_value(self) -> Value {
        self.0
    }
}

/// Everything shared launch construction needs for either Claude driver.
#[derive(Debug, Clone)]
pub struct Launch {
    pub binary: PathBuf,
    pub cwd: PathBuf,
    pub args: Vec<String>,
    pub session_id: Uuid,
    pub resume: bool,
    pub settings: MergedSettings,
    pub hook_command: Vec<String>,
    pub mcp_servers: Vec<McpServerConfig>,
    pub env_scrub: &'static [&'static str],
}

/// Read each user `--settings` source relative to `cwd` and merge it in order.
pub fn load_user_settings(cwd: &Path, sources: &[String]) -> Result<Option<Value>> {
    let mut merged = Value::Object(serde_json::Map::new());
    for source in sources {
        deep_merge(&mut merged, read_settings(cwd, source)?);
    }
    Ok((!sources.is_empty()).then_some(merged))
}

/// Merge managed hooks and MCP settings after user settings so the provider's
/// routing cannot be replaced accidentally.
pub fn merged_settings(user: Option<Value>, managed: &ManagedSettings) -> MergedSettings {
    let mut merged = user.unwrap_or_else(|| Value::Object(serde_json::Map::new()));
    if !managed.hook_command.is_empty() {
        deep_merge(&mut merged, managed_hook_settings(&managed.hook_command));
    }
    if !managed.mcp_servers.is_empty() {
        let servers = managed
            .mcp_servers
            .iter()
            .map(|server| (server.name.clone(), server.config.clone()))
            .collect();
        deep_merge(
            &mut merged,
            serde_json::json!({"mcpServers": Value::Object(servers)}),
        );
    }
    if managed.accept_cross_session {
        deep_merge(
            &mut merged,
            serde_json::json!({"crossSessionInbound": "accept"}),
        );
    }
    if !managed.permissions_allow.is_empty() {
        deep_merge(
            &mut merged,
            serde_json::json!({"permissions": {"allow": managed.permissions_allow}}),
        );
    }
    MergedSettings(merged)
}

/// Remove and return every user settings source from an argument list.
pub fn take_settings_args(args: &mut Vec<String>) -> Result<Vec<String>> {
    let mut retained = Vec::with_capacity(args.len());
    let mut settings = Vec::new();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--settings" => {
                let value = args
                    .get(index + 1)
                    .filter(|value| !value.starts_with('-'))
                    .context("Claude --settings requires a JSON string or file path")?;
                settings.push(value.clone());
                index += 2;
            }
            arg if arg.starts_with("--settings=") => {
                settings.push(arg["--settings=".len()..].to_string());
                index += 1;
            }
            _ => {
                retained.push(args[index].clone());
                index += 1;
            }
        }
    }
    *args = retained;
    Ok(settings)
}

/// Build PTY-driver arguments. Host-owned name and messaging arguments may be
/// supplied in `Launch::args`; callers decide them from runtime facts.
pub fn pty_spawn_args(launch: &Launch) -> Vec<String> {
    let mut args = Vec::new();
    if launch.resume {
        args.extend(["--resume".to_string(), launch.session_id.to_string()]);
    }
    args.extend(launch.args.clone());
    append_managed(&mut args, launch);
    args
}

fn append_managed(args: &mut Vec<String>, launch: &Launch) {
    if !launch.mcp_servers.is_empty() {
        let servers: BTreeMap<_, _> = launch
            .mcp_servers
            .iter()
            .map(|server| (server.name.clone(), server.config.clone()))
            .collect();
        args.extend([
            "--mcp-config".to_string(),
            serde_json::json!({"mcpServers": servers}).to_string(),
        ]);
        args.extend([
            "--allowedTools".to_string(),
            launch
                .mcp_servers
                .iter()
                .map(|server| format!("mcp__{}__*", server.name))
                .collect::<Vec<_>>()
                .join(","),
        ]);
    }
    args.extend([
        "--settings".to_string(),
        launch.settings.as_value().to_string(),
    ]);
}

fn read_settings(cwd: &Path, source: &str) -> Result<Value> {
    let value: Value = match serde_json::from_str(source) {
        Ok(value) => value,
        Err(json_error) => {
            let path = Path::new(source);
            let path = if path.is_absolute() {
                path.to_path_buf()
            } else {
                cwd.join(path)
            };
            let contents = std::fs::read_to_string(&path).with_context(|| {
                format!(
                    "Claude --settings value is neither JSON ({json_error}) nor a readable file at {}",
                    path.display()
                )
            })?;
            serde_json::from_str(&contents).with_context(|| {
                format!(
                    "failed to parse Claude settings file {} as JSON",
                    path.display()
                )
            })?
        }
    };
    if !value.is_object() {
        anyhow::bail!("Claude --settings must contain a JSON object");
    }
    Ok(value)
}

fn deep_merge(destination: &mut Value, addition: Value) {
    match (destination, addition) {
        (Value::Object(destination), Value::Object(addition)) => {
            for (key, value) in addition {
                match destination.get_mut(&key) {
                    Some(existing) => deep_merge(existing, value),
                    None => {
                        destination.insert(key, value);
                    }
                }
            }
        }
        (Value::Array(destination), Value::Array(mut addition)) => {
            destination.append(&mut addition);
        }
        (destination, addition) => *destination = addition,
    }
}

/// The hook events terminal Claude reports to the agent. MessageDisplay
/// streams the reply as Claude draws it; PermissionRequest and Elicitation
/// are answered (see [`crate::hooks::ANSWERED_EVENTS`]).
pub const HOOK_EVENTS: &[&str] = &[
    "SessionStart",
    "SessionEnd",
    "UserPromptSubmit",
    "PermissionRequest",
    "PermissionDenied",
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
    "Stop",
    "SubagentStop",
    "Elicitation",
    "MessageDisplay",
    "PostModelSwitch",
];

/// How long Claude waits on a hook amux answers before giving up on it
/// and leaving the question to its own dialog: a week, so an ask left
/// overnight can still be answered from any client.
const ANSWER_TIMEOUT_SECS: u64 = 7 * 24 * 60 * 60;

/// The command runs as given, with no shell between Claude and it.
fn managed_hook_settings(command: &[String]) -> Value {
    let (program, args) = command.split_first().expect("a hook command");
    let hooks: serde_json::Map<String, Value> = HOOK_EVENTS
        .iter()
        .map(|event| {
            let mut hook = serde_json::json!({
                "type": "command",
                "command": program,
                "args": args,
            });
            if crate::hooks::ANSWERED_EVENTS.contains(event) {
                hook["timeout"] = ANSWER_TIMEOUT_SECS.into();
            }
            ((*event).to_owned(), serde_json::json!([{"hooks": [hook]}]))
        })
        .collect();
    serde_json::json!({ "hooks": hooks })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn managed_settings_append_user_hooks_and_pin_managed_values() {
        let user = serde_json::json!({
            "theme": "dark",
            "hooks": {"Stop": [{"hooks": [{"command": "user-hook"}]}]},
            "mcpServers": {"amux": {"command": "spoofed"}}
        });
        let managed = ManagedSettings {
            hook_command: vec![
                "/Applications/amux tool".into(),
                "hooks".into(),
                "claude".into(),
            ],
            mcp_servers: vec![McpServerConfig {
                name: "amux".into(),
                config: serde_json::json!({"command":"/bin/amux","args":["mcp","agent"]}),
            }],
            permissions_allow: vec!["Read(/managed/artifacts/**)".into()],
            accept_cross_session: true,
        };
        let settings = merged_settings(Some(user), &managed).into_value();
        assert_eq!(settings["theme"], "dark");
        assert_eq!(settings["crossSessionInbound"], "accept");
        assert_eq!(settings["mcpServers"]["amux"]["command"], "/bin/amux");
        assert_eq!(settings["hooks"]["Stop"].as_array().unwrap().len(), 2);
        let start = &settings["hooks"]["SessionStart"][0]["hooks"][0];
        assert_eq!(start["command"], "/Applications/amux tool");
        assert_eq!(start["args"], serde_json::json!(["hooks", "claude"]));
        assert!(start.get("timeout").is_none());
        assert_eq!(
            settings["hooks"]["PermissionRequest"][0]["hooks"][0]["timeout"],
            ANSWER_TIMEOUT_SECS
        );
        assert!(settings["hooks"]["PreToolUse"].is_array());
        assert!(settings["hooks"]["PostToolUse"].is_array());
        assert_eq!(
            settings["permissions"]["allow"],
            serde_json::json!(["Read(/managed/artifacts/**)"])
        );
    }

    #[test]
    fn user_settings_load_inline_and_relative_files_in_order() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("settings.json"),
            r#"{"nested":{"file":true},"choice":"file"}"#,
        )
        .unwrap();
        let sources = vec![
            r#"{"nested":{"inline":true},"choice":"inline"}"#.to_string(),
            "settings.json".to_string(),
        ];
        let loaded = load_user_settings(dir.path(), &sources).unwrap().unwrap();
        assert_eq!(loaded["nested"]["inline"], true);
        assert_eq!(loaded["nested"]["file"], true);
        assert_eq!(loaded["choice"], "file");
    }

    #[test]
    fn scrub_is_explicit_and_preserves_user_configuration() {
        assert!(CHILD_SESSION_ENV_SCRUB.contains(&"CLAUDE_CODE_CHILD_SESSION"));
        assert!(CHILD_SESSION_ENV_SCRUB.contains(&"CLAUDE_CODE_MESSAGING_TOKEN"));
        assert!(!CHILD_SESSION_ENV_SCRUB.contains(&"CLAUDE_CONFIG_DIR"));
    }
}
