//! What both Claude fakes share: the launch flags they honour, ids and
//! timestamps in Claude's formats.

use std::path::{Path, PathBuf};

use serde_json::Value;

/// The launch arguments the fake honours; everything else is accepted and
/// ignored, as a provider ignores flags a host passes for other versions.
#[derive(Clone, Debug, Default)]
pub struct Args {
    pub settings: Vec<Value>,
    pub session_id: Option<String>,
    pub resume: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub messaging_socket: Option<PathBuf>,
    pub permission_mode: Option<String>,
    pub replay_user_messages: bool,
    pub include_partial_messages: bool,
    /// Every `--mcp-config` value, read from its file when it is a path.
    pub mcp_config: Vec<Value>,
    pub strict_mcp_config: bool,
}

impl Args {
    pub fn parse(args: &[String]) -> Self {
        let mut parsed = Args::default();
        let mut index = 0;
        while index < args.len() {
            let (flag, inline) = match args[index].split_once('=') {
                Some((flag, value)) if flag.starts_with("--") => (flag, Some(value.to_owned())),
                _ => (args[index].as_str(), None),
            };
            let mut value = || {
                inline.clone().or_else(|| {
                    index += 1;
                    args.get(index).cloned()
                })
            };
            match flag {
                "--settings" => {
                    if let Some(source) = value() {
                        let settings = serde_json::from_str(&source).or_else(|_| {
                            std::fs::read(&source)
                                .ok()
                                .and_then(|bytes| serde_json::from_slice(&bytes).ok())
                                .ok_or(())
                        });
                        if let Ok(settings) = settings {
                            parsed.settings.push(settings);
                        }
                    }
                }
                "--mcp-config" => {
                    if let Some(source) = value() {
                        let config = serde_json::from_str(&source).ok().or_else(|| {
                            std::fs::read(&source)
                                .ok()
                                .and_then(|bytes| serde_json::from_slice(&bytes).ok())
                        });
                        parsed.mcp_config.extend(config);
                    }
                }
                "--session-id" => parsed.session_id = value(),
                "--resume" | "-r" => parsed.resume = value(),
                "--model" => parsed.model = value(),
                "--effort" => parsed.effort = value(),
                "--messaging-socket-path" => parsed.messaging_socket = value().map(PathBuf::from),
                "--permission-mode" => parsed.permission_mode = value(),
                "--replay-user-messages" => parsed.replay_user_messages = true,
                "--include-partial-messages" => parsed.include_partial_messages = true,
                "--strict-mcp-config" => parsed.strict_mcp_config = true,
                _ => {}
            }
            index += 1;
        }
        parsed
    }

    /// Why Claude refuses to start where an organization's managed MCP
    /// configuration is in force: it takes neither `--strict-mcp-config`
    /// nor a server added on the command line. The fake reads only the
    /// folder `CLAUDE_CODE_MANAGED_SETTINGS_PATH` names, never the
    /// machine's own.
    pub fn managed_refusal(&self) -> Option<&'static str> {
        let folder = std::env::var_os("CLAUDE_CODE_MANAGED_SETTINGS_PATH")?;
        if !Path::new(&folder).join("managed-mcp.json").exists() {
            return None;
        }
        let added = self.mcp_config.iter().any(|config| {
            config["mcpServers"]
                .as_object()
                .is_some_and(|servers| !servers.is_empty())
        });
        if self.strict_mcp_config {
            Some("You cannot use --strict-mcp-config when an enterprise MCP config is present")
        } else if added {
            Some(
                "You cannot dynamically configure MCP servers when an enterprise MCP config is present",
            )
        } else {
            None
        }
    }

    /// Every hook command registered for `event`, in settings order.
    pub fn hook_commands(&self, event: &str) -> Vec<String> {
        self.settings
            .iter()
            .filter_map(|settings| settings.get("hooks")?.get(event)?.as_array())
            .flatten()
            .filter_map(|matcher| matcher.get("hooks")?.as_array())
            .flatten()
            .filter_map(|hook| hook.get("command")?.as_str().map(str::to_owned))
            .collect()
    }
}

/// `claude --version`, which a host runs before it launches a session:
/// true when that is what was asked, after answering it as Claude does.
pub fn answered_version(args: &[String]) -> bool {
    if args.first().map(String::as_str) != Some("--version") {
        return false;
    }
    println!("{} (Claude Code)", crate::scripted_version(VERSION));
    true
}

/// A timestamp as Claude writes one: UTC, milliseconds, `Z`.
pub fn timestamp() -> String {
    chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string()
}

pub fn uuid() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Ids in the provider's shapes, numbered so a run is easy to read. Like
/// the real provider's they never repeat across processes: a resumed
/// session's new messages must not reuse the ids of the ones before it.
#[derive(Default)]
pub struct Ids {
    next: u64,
}

impl Ids {
    pub fn next(&mut self, prefix: &str) -> String {
        self.next += 1;
        numbered(prefix, self.next)
    }
}

/// `prefix` then 24 digits: this process's tag, then `n`.
pub fn numbered(prefix: &str, n: u64) -> String {
    format!("{prefix}{:08x}{n:016}", process_tag())
}

/// A tag for this process that another fake process is unlikely to share.
fn process_tag() -> u32 {
    static TAG: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *TAG.get_or_init(|| uuid::Uuid::new_v4().as_u128() as u32)
}

/// The Claude Code version the fakes report: the newest one the corpora
/// were recorded against.
pub const VERSION: &str = "2.1.283";

/// Carries out a file tool's change, as Claude's own Write and Edit do: a
/// Write writes its content, an Edit replaces its old string (every one
/// with `replace_all`). A relative path is the working directory's.
pub fn apply_file_tool(name: &str, input: &serde_json::Value) -> std::io::Result<()> {
    let text = |key: &str| input.get(key).and_then(serde_json::Value::as_str);
    let Some(path) = text("file_path") else {
        return Ok(());
    };
    let path = std::path::Path::new(path);
    match name {
        "Write" => {
            if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(path, text("content").unwrap_or_default())
        }
        "Edit" => {
            let old = text("old_string").unwrap_or_default();
            let new = text("new_string").unwrap_or_default();
            let was = std::fs::read_to_string(path)?;
            let all = input
                .get("replace_all")
                .and_then(serde_json::Value::as_bool)
                == Some(true);
            let now = if all {
                was.replace(old, new)
            } else {
                was.replacen(old, new, 1)
            };
            std::fs::write(path, now)
        }
        _ => Ok(()),
    }
}
