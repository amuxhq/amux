use std::process::Stdio;
use std::{env, fs};

use tokio::io::BufReader;
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};

use crate::driver::sdk::Error;
use crate::driver::sdk::options::{
    Effort, QueryOptions, SdkBeta, SettingSource, ThinkingConfig, ToolsConfig,
};
use crate::driver::sdk::types::PermissionMode;

// ── CliProcess ─────────────────────────────────────────────────────

/// Handle to a running `claude` CLI subprocess.
/// Provides typed access to stdin (for sending messages) and stdout (for reading responses).
pub(crate) struct CliProcess {
    child: Child,
    stdin: ChildStdin,
    pub(crate) stdout: BufReader<ChildStdout>,
    pub(crate) stderr: BufReader<ChildStderr>,
}

impl CliProcess {
    pub(crate) fn new(
        child: Child,
        stdin: ChildStdin,
        stdout: ChildStdout,
        stderr: ChildStderr,
    ) -> Self {
        Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            stderr: BufReader::new(stderr),
        }
    }

    /// Decompose into raw parts for use by the background reader/writer tasks.
    pub(crate) fn into_parts(
        self,
    ) -> (
        Child,
        ChildStdin,
        BufReader<ChildStdout>,
        BufReader<ChildStderr>,
    ) {
        (self.child, self.stdin, self.stdout, self.stderr)
    }
}

// ── Spawning ───────────────────────────────────────────────────────

/// Spawn a prepared CLI command with owned, bidirectional process I/O.
pub(crate) fn spawn_command(mut cmd: Command) -> Result<CliProcess, Error> {
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd
        .spawn()
        .map_err(|e| Error::Process(format!("failed to spawn claude: {e}")))?;

    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| Error::Process("failed to capture stdin".into()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| Error::Process("failed to capture stdout".into()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| Error::Process("failed to capture stderr".into()))?;

    Ok(CliProcess::new(child, stdin, stdout, stderr))
}

/// Build the CLI invocation independently of starting the process.
pub(crate) fn query_command(session_id: &str, options: &QueryOptions) -> Result<Command, Error> {
    let cli = options
        .cli_path
        .as_deref()
        .unwrap_or_else(|| std::path::Path::new("claude"));
    let mut cmd = Command::new(cli);

    // Bidirectional JSON streaming: --verbose is required for stream-json
    cmd.arg("--output-format")
        .arg("stream-json")
        .arg("--input-format")
        .arg("stream-json")
        .arg("--verbose");

    cmd.arg("--permission-prompt-tool").arg("stdio");

    if let Some(ref resume_id) = options.resume {
        cmd.arg("--resume").arg(resume_id);
        if options.fork_session {
            cmd.arg("--session-id").arg(session_id);
        }
    } else {
        cmd.arg("--session-id").arg(session_id);
    }
    if options.fork_session {
        cmd.arg("--fork-session");
    }
    if let Some(resume_at) = &options.resume_session_at {
        cmd.arg(format!("--resume-session-at={resume_at}"));
    }

    if let Some(environment) = &options.env {
        cmd.env_clear();
        cmd.envs(environment);
    }

    apply_common_options(&mut cmd, options)?;

    if let Some(cwd) = &options.cwd {
        cmd.current_dir(cwd);
    }

    Ok(cmd)
}

// ── Shared option builders ──────────────────────────────────────────

/// Apply `QueryOptions` fields as CLI arguments for stream-JSON mode.
///
/// Handles: model, system prompt, max turns, budget, effort, permissions,
/// allowed/disallowed tools, MCP servers, debug flags, etc.
fn apply_common_options(cmd: &mut Command, options: &QueryOptions) -> Result<(), Error> {
    if let Some(model) = &options.model {
        cmd.arg("--model").arg(model);
    }

    // Turn and budget limits
    if let Some(n) = options.max_turns {
        cmd.arg("--max-turns").arg(n.to_string());
    }
    if let Some(budget) = options.max_budget_usd {
        cmd.arg("--max-budget-usd").arg(budget.to_string());
    }

    // Effort level
    if let Some(ref effort) = options.effort {
        let s = match effort {
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
            Effort::Xhigh => "xhigh",
            Effort::Max => "max",
        };
        cmd.arg("--effort").arg(s);
    }

    // Permission mode
    if let Some(ref mode) = options.permission_mode {
        let s = match mode {
            PermissionMode::Default => "default",
            PermissionMode::AcceptEdits => "acceptEdits",
            PermissionMode::BypassPermissions => "bypassPermissions",
            PermissionMode::Plan => "plan",
            PermissionMode::DontAsk => "dontAsk",
            PermissionMode::Auto => "auto",
            PermissionMode::Unknown(value) => value,
        };
        cmd.arg("--permission-mode").arg(s);
    }

    // Tool base set
    if let Some(ref tools) = options.tools {
        match tools {
            ToolsConfig::List(list) => {
                cmd.arg("--tools").arg(list.join(","));
            }
            ToolsConfig::Preset { .. } => {
                cmd.arg("--tools").arg("default");
            }
        }
    }

    // Tool allow/deny lists
    let mut allowed_tools = options.allowed_tools.clone();
    if let Some(skills) = &options.skills {
        match skills {
            crate::driver::sdk::options::SkillsConfig::All => allowed_tools.push("Skill".into()),
            crate::driver::sdk::options::SkillsConfig::Selected(names) => {
                allowed_tools.extend(names.iter().map(|name| format!("Skill({name})")))
            }
        }
    }
    if !allowed_tools.is_empty() {
        cmd.arg("--allowedTools").arg(allowed_tools.join(","));
    }
    if !options.disallowed_tools.is_empty() {
        cmd.arg("--disallowedTools")
            .arg(options.disallowed_tools.join(","));
    }

    // Skip all permission prompts (dangerous)
    if options.allow_dangerously_skip_permissions {
        cmd.arg("--dangerously-skip-permissions");
    }

    // MCP server configuration (inline JSON)
    if !options.mcp_servers.is_empty() {
        let wrapper = serde_json::json!({ "mcpServers": options.mcp_servers });
        if let Ok(json) = serde_json::to_string(&wrapper) {
            cmd.arg("--mcp-config").arg(json);
        }
    }
    if options.strict_mcp_config {
        cmd.arg("--strict-mcp-config");
    }

    // Additional working directories
    for dir in &options.additional_directories {
        cmd.arg("--add-dir").arg(dir);
    }

    // Session persistence opt-out
    if let Some(false) = options.persist_session {
        cmd.arg("--no-session-persistence");
    }

    // Fallback model
    if let Some(ref fallback) = options.fallback_model {
        cmd.arg("--fallback-model").arg(fallback);
    }

    // Settings JSON / file path, merged with sandbox settings when needed.
    if let Some(settings_value) = build_settings_value(options)? {
        cmd.arg("--settings").arg(settings_value);
    }

    // Setting sources
    if !options.setting_sources.is_empty() {
        let sources: Vec<&str> = options
            .setting_sources
            .iter()
            .map(|s| match s {
                SettingSource::User => "user",
                SettingSource::Project => "project",
                SettingSource::Local => "local",
            })
            .collect();
        cmd.arg("--setting-sources").arg(sources.join(","));
    }

    if options.include_partial_messages {
        cmd.arg("--include-partial-messages");
    }

    if let Some(thinking) = &options.thinking {
        match thinking {
            ThinkingConfig::Adaptive { display } => {
                cmd.arg("--thinking").arg("adaptive");
                if let Some(display) = display {
                    cmd.arg("--thinking-display").arg(match display {
                        crate::driver::sdk::options::ThinkingDisplay::Summarized => "summarized",
                        crate::driver::sdk::options::ThinkingDisplay::Omitted => "omitted",
                    });
                }
            }
            ThinkingConfig::Enabled {
                budget_tokens,
                display,
            } => {
                if let Some(budget) = budget_tokens {
                    cmd.arg("--max-thinking-tokens").arg(budget.to_string());
                } else {
                    cmd.arg("--thinking").arg("adaptive");
                }
                if let Some(display) = display {
                    cmd.arg("--thinking-display").arg(match display {
                        crate::driver::sdk::options::ThinkingDisplay::Summarized => "summarized",
                        crate::driver::sdk::options::ThinkingDisplay::Omitted => "omitted",
                    });
                }
            }
            ThinkingConfig::Disabled => {
                cmd.arg("--thinking").arg("disabled");
            }
        }
    }

    // Debug mode
    if options.debug {
        cmd.arg("--debug");
    }

    // Beta features
    if !options.betas.is_empty() {
        let betas = options
            .betas
            .iter()
            .map(|beta| match beta {
                SdkBeta::Context1M => "context-1m-2025-08-07",
            })
            .collect::<Vec<_>>()
            .join(",");
        cmd.arg("--betas").arg(betas);
    }

    if let Some(preview_format) = options
        .tool_config
        .as_ref()
        .and_then(|config| config.ask_user_question.as_ref())
        .and_then(|config| config.preview_format.as_ref())
    {
        cmd.env(
            "CLAUDE_CODE_QUESTION_PREVIEW_FORMAT",
            match preview_format {
                crate::driver::sdk::options::PreviewFormat::Markdown => "markdown",
                crate::driver::sdk::options::PreviewFormat::Html => "html",
            },
        );
    }

    if options.include_hook_events {
        cmd.arg("--include-hook-events");
    }

    if let Some(managed_settings) = &options.managed_settings {
        cmd.arg("--managed-settings")
            .arg(managed_settings.to_string());
    }

    for (name, value) in &options.extra_args {
        cmd.arg(format!("--{name}"));
        if let Some(value) = value {
            cmd.arg(value);
        }
    }

    if let Ok(current_dir) = env::current_dir() {
        cmd.env("PWD", options.cwd.as_ref().unwrap_or(&current_dir));
    }

    Ok(())
}

fn build_settings_value(options: &QueryOptions) -> Result<Option<String>, Error> {
    let has_settings = options.settings.is_some();
    let has_sandbox = options.sandbox.is_some();
    if !has_settings && !has_sandbox {
        return Ok(None);
    }

    if has_settings && !has_sandbox {
        return Ok(options.settings.as_ref().map(settings_argument));
    }

    let mut settings = serde_json::Map::new();
    if let Some(ref configured_settings) = options.settings {
        match configured_settings {
            crate::driver::sdk::options::SettingsConfig::Inline(value) => match value {
                serde_json::Value::Object(obj) => {
                    settings = obj.clone();
                }
                _ => {
                    return Err(Error::Process(
                        "settings JSON must be an object when merged with sandbox settings".into(),
                    ));
                }
            },
            crate::driver::sdk::options::SettingsConfig::Path(path) => {
                let contents = fs::read_to_string(path).map_err(|error| {
                    Error::Process(format!(
                        "failed to read settings file {}: {error}",
                        path.display()
                    ))
                })?;
                match serde_json::from_str::<serde_json::Value>(&contents)? {
                    serde_json::Value::Object(obj) => {
                        settings = obj;
                    }
                    _ => {
                        return Err(Error::Process(format!(
                            "settings file {} must contain a JSON object when merged with sandbox settings",
                            path.display()
                        )));
                    }
                }
            }
        }
    }

    if let Some(ref sandbox) = options.sandbox {
        settings.insert(
            "sandbox".to_string(),
            serde_json::to_value(sandbox).unwrap_or_default(),
        );
    }

    Ok(Some(serde_json::Value::Object(settings).to_string()))
}

fn settings_argument(settings: &crate::driver::sdk::options::SettingsConfig) -> String {
    match settings {
        crate::driver::sdk::options::SettingsConfig::Inline(value) => value.to_string(),
        crate::driver::sdk::options::SettingsConfig::Path(path) => {
            path.to_string_lossy().into_owned()
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::driver::sdk::options::{
        Effort, SandboxFilesystemConfig, SandboxNetworkConfig, SandboxSettings, SystemPrompt,
        SystemPromptPreset,
    };
    use crate::driver::sdk::types::PermissionMode;

    // ── apply_common_options tests ──────────────────────────────────

    /// Helper to extract the value following a flag in an argument list.
    fn arg_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1))
            .map(|s| s.as_str())
    }

    /// Helper to collect all args from a Command into a Vec<String>.
    fn collect_args(cmd: &Command) -> Vec<String> {
        cmd.as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn new_and_resumed_commands_preserve_identity_and_stream_transport() {
        for resumed in [false, true] {
            let options = QueryOptions {
                cli_path: Some("custom-claude".into()),
                cwd: Some("workspace".into()),
                env: Some(HashMap::from([("SCENARIO".into(), "test".into())])),
                resume: resumed.then(|| "source-session".to_owned()),
                ..QueryOptions::default()
            };
            let command = query_command("target-session", &options).unwrap();
            let args = collect_args(&command);
            assert_eq!(command.as_std().get_program(), "custom-claude");
            assert!(command.as_std().get_envs().any(|(key, value)| {
                key == "SCENARIO" && value == Some(std::ffi::OsStr::new("test"))
            }));
            assert_eq!(
                command.as_std().get_current_dir(),
                Some(std::path::Path::new("workspace"))
            );
            assert_eq!(arg_value(&args, "--input-format"), Some("stream-json"));
            assert_eq!(arg_value(&args, "--output-format"), Some("stream-json"));
            if resumed {
                assert_eq!(arg_value(&args, "--resume"), Some("source-session"));
                assert_eq!(arg_value(&args, "--session-id"), None);
            } else {
                assert_eq!(arg_value(&args, "--session-id"), Some("target-session"));
                assert_eq!(arg_value(&args, "--resume"), None);
            }
        }
    }

    #[test]
    fn apply_common_options_sets_model_and_limits() {
        let mut opts = QueryOptions::new("claude-sonnet-4-6");
        opts.max_turns = Some(10);
        opts.max_budget_usd = Some(1.5);
        opts.effort = Some(Effort::High);
        opts.debug = true;
        opts.fallback_model = Some("claude-haiku-4-5-20251001".into());

        let mut cmd = Command::new("claude");
        apply_common_options(&mut cmd, &opts).unwrap();
        let args = collect_args(&cmd);

        assert_eq!(arg_value(&args, "--model"), Some("claude-sonnet-4-6"));
        assert_eq!(arg_value(&args, "--max-turns"), Some("10"));
        assert_eq!(arg_value(&args, "--max-budget-usd"), Some("1.5"));
        assert_eq!(arg_value(&args, "--effort"), Some("high"));
        assert_eq!(
            arg_value(&args, "--fallback-model"),
            Some("claude-haiku-4-5-20251001")
        );
        assert!(args.contains(&"--debug".to_string()));
    }

    #[test]
    fn apply_common_options_handles_permission_mode_and_tools() {
        let mut opts = QueryOptions::new("m");
        opts.permission_mode = Some(PermissionMode::AcceptEdits);
        opts.allowed_tools = vec!["Bash".into(), "Read".into()];
        opts.disallowed_tools = vec!["Write".into()];
        opts.allow_dangerously_skip_permissions = true;

        let mut cmd = Command::new("claude");
        apply_common_options(&mut cmd, &opts).unwrap();
        let args = collect_args(&cmd);

        assert_eq!(arg_value(&args, "--permission-mode"), Some("acceptEdits"));
        assert_eq!(arg_value(&args, "--allowedTools"), Some("Bash,Read"));
        assert_eq!(arg_value(&args, "--disallowedTools"), Some("Write"));
        assert!(args.contains(&"--dangerously-skip-permissions".to_string()));
    }

    #[test]
    fn apply_common_options_leaves_system_prompts_for_initialize_control() {
        let mut opts = QueryOptions::new("m");
        opts.system_prompt = Some(SystemPrompt::Custom("Be brief.".into()));
        let mut cmd = Command::new("claude");
        apply_common_options(&mut cmd, &opts).unwrap();
        let args = collect_args(&cmd);
        assert!(arg_value(&args, "--system-prompt").is_none());

        // Preset with append
        let mut opts = QueryOptions::new("m");
        opts.system_prompt = Some(SystemPrompt::Preset {
            preset: SystemPromptPreset::ClaudeCode,
            append: Some("Extra instructions".into()),
            exclude_dynamic_sections: false,
        });
        let mut cmd = Command::new("claude");
        apply_common_options(&mut cmd, &opts).unwrap();
        let args = collect_args(&cmd);
        assert!(arg_value(&args, "--append-system-prompt").is_none());
        assert!(arg_value(&args, "--system-prompt").is_none());
    }

    #[test]
    fn apply_common_options_uses_stable_permission_default() {
        let opts = QueryOptions::new("m");
        let mut cmd = Command::new("claude");
        apply_common_options(&mut cmd, &opts).unwrap();
        let args = collect_args(&cmd);

        assert_eq!(args, vec!["--model", "m", "--permission-mode", "default"]);
    }

    #[test]
    fn control_default_options_omit_model_override() {
        let opts = QueryOptions::default();
        let mut cmd = Command::new("claude");
        apply_common_options(&mut cmd, &opts).unwrap();
        let args = collect_args(&cmd);

        assert!(arg_value(&args, "--model").is_none());
        assert_eq!(arg_value(&args, "--permission-mode"), Some("default"));
    }

    #[test]
    fn stream_delta_partial_messages_are_explicitly_enabled_or_disabled() {
        let mut options = QueryOptions::new("m");
        let mut command = Command::new("claude");
        apply_common_options(&mut command, &options).unwrap();
        assert!(!collect_args(&command).contains(&"--include-partial-messages".to_owned()));

        options.include_partial_messages = true;
        let mut command = Command::new("claude");
        apply_common_options(&mut command, &options).unwrap();
        assert!(collect_args(&command).contains(&"--include-partial-messages".to_owned()));
    }

    #[test]
    fn apply_common_options_sets_session_persistence_opt_out() {
        let mut opts = QueryOptions::new("m");
        opts.persist_session = Some(false);
        let mut cmd = Command::new("claude");
        apply_common_options(&mut cmd, &opts).unwrap();
        let args = collect_args(&cmd);
        assert!(args.contains(&"--no-session-persistence".to_string()));

        // persist_session = Some(true) should NOT add the flag
        let mut opts = QueryOptions::new("m");
        opts.persist_session = Some(true);
        let mut cmd = Command::new("claude");
        apply_common_options(&mut cmd, &opts).unwrap();
        let args = collect_args(&cmd);
        assert!(!args.contains(&"--no-session-persistence".to_string()));
    }

    #[test]
    fn apply_common_options_errors_when_merged_settings_file_is_missing() {
        let mut opts = QueryOptions::new("m");
        let missing_settings =
            std::env::temp_dir().join(format!("missing-settings-{}.json", std::process::id()));
        opts.settings = Some(crate::driver::sdk::options::SettingsConfig::Path(
            missing_settings,
        ));
        opts.sandbox = Some(SandboxSettings {
            enabled: Some(true),
            auto_allow_bash_if_sandboxed: None,
            excluded_commands: Vec::new(),
            allow_unsandboxed_commands: None,
            network: Some(SandboxNetworkConfig {
                allowed_domains: Vec::new(),
                allow_managed_domains_only: None,
                allow_local_binding: None,
                allow_unix_sockets: Vec::new(),
                allow_all_unix_sockets: None,
                http_proxy_port: None,
                socks_proxy_port: None,
            }),
            filesystem: Some(SandboxFilesystemConfig {
                allow_write: Vec::new(),
                deny_write: Vec::new(),
                deny_read: Vec::new(),
            }),
            ignore_violations: HashMap::new(),
            enable_weaker_nested_sandbox: None,
            ripgrep: None,
        });

        let mut cmd = Command::new("claude");
        let err = apply_common_options(&mut cmd, &opts).unwrap_err();
        assert!(matches!(err, Error::Process(_)));
        assert!(
            err.to_string().contains("failed to read settings file"),
            "unexpected error: {err}"
        );
    }
}
