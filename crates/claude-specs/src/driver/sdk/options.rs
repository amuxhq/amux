//! What the driver starts Claude Code with.

use std::collections::HashMap;
use std::path::PathBuf;

use claude_protocol::stream::PermissionMode;
pub use claude_protocol::stream::options::*;

pub struct QueryOptions {
    /// Model override. `None` truthfully leaves model selection to Claude Code.
    pub model: Option<String>,

    // CLI binary path (defaults to "claude" on PATH)
    pub cli_path: Option<PathBuf>,
    /// Replace the subprocess environment when set; otherwise inherit it.
    pub env: Option<HashMap<String, String>>,
    /// Extra Claude Code flags, without the leading `--`.
    pub extra_args: HashMap<String, Option<String>>,

    // Session behavior
    pub cwd: Option<PathBuf>,
    pub system_prompt: Option<SystemPrompt>,
    pub max_turns: Option<u32>,
    pub max_budget_usd: Option<f64>,
    pub effort: Option<Effort>,
    pub thinking: Option<ThinkingConfig>,
    pub include_partial_messages: bool,
    pub persist_session: Option<bool>,
    pub session_id: Option<String>,
    pub resume: Option<String>,
    pub fork_session: bool,
    pub prompt_suggestions: bool,
    pub agent_progress_summaries: Option<bool>,
    pub forward_subagent_text: Option<bool>,
    pub fallback_model: Option<String>,
    pub debug: bool,
    pub title: Option<String>,
    pub resume_session_at: Option<String>,

    // Permissions
    pub permission_mode: Option<PermissionMode>,
    pub allowed_tools: Vec<String>,
    pub disallowed_tools: Vec<String>,
    pub supported_dialog_kinds: Vec<String>,
    pub per_task_stop_affordance: Option<bool>,
    pub allow_dangerously_skip_permissions: bool,

    // Tools & MCP
    pub tools: Option<ToolsConfig>,
    pub tool_config: Option<ToolConfig>,
    pub mcp_servers: HashMap<String, McpServerConfig>,
    pub strict_mcp_config: bool,

    // Agents
    pub agents: HashMap<String, AgentDefinition>,
    pub tool_aliases: HashMap<String, String>,
    pub skills: Option<SkillsConfig>,

    // Settings & directories
    pub additional_directories: Vec<PathBuf>,
    pub setting_sources: Vec<SettingSource>,

    pub settings: Option<SettingsConfig>,
    pub managed_settings: Option<serde_json::Value>,

    // Sandbox
    pub sandbox: Option<SandboxSettings>,

    // Betas
    pub betas: Vec<SdkBeta>,

    // Hooks
    pub hook_subscriptions: Vec<HookSubscription>,
    pub include_hook_events: bool,
    pub plan_mode_instructions: Option<String>,
}

impl QueryOptions {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: Some(model.into()),
            cli_path: None,
            env: None,
            extra_args: HashMap::new(),
            cwd: None,
            system_prompt: None,
            max_turns: None,
            max_budget_usd: None,
            effort: None,
            thinking: None,
            include_partial_messages: false,
            persist_session: None,
            session_id: None,
            resume: None,
            fork_session: false,
            prompt_suggestions: false,
            agent_progress_summaries: None,
            forward_subagent_text: None,
            fallback_model: None,
            debug: false,
            title: None,
            resume_session_at: None,
            permission_mode: Some(PermissionMode::Default),
            allowed_tools: Vec::new(),
            disallowed_tools: Vec::new(),
            supported_dialog_kinds: Vec::new(),
            per_task_stop_affordance: None,
            allow_dangerously_skip_permissions: false,
            tools: None,
            tool_config: None,
            mcp_servers: HashMap::new(),
            strict_mcp_config: false,
            agents: HashMap::new(),
            tool_aliases: HashMap::new(),
            skills: None,
            additional_directories: Vec::new(),
            setting_sources: Vec::new(),
            settings: None,
            managed_settings: None,
            sandbox: None,
            betas: Vec::new(),
            hook_subscriptions: Vec::new(),
            include_hook_events: false,
            plan_mode_instructions: None,
        }
    }

    pub fn validate(&self) -> Result<(), crate::driver::sdk::Error> {
        use crate::driver::sdk::Error;

        if self.permission_mode == Some(PermissionMode::BypassPermissions)
            && !self.allow_dangerously_skip_permissions
        {
            return Err(Error::InvalidOptions(
                "permission_mode bypassPermissions requires allow_dangerously_skip_permissions"
                    .into(),
            ));
        }
        if self.fork_session && self.resume.is_none() {
            return Err(Error::InvalidOptions("fork_session requires resume".into()));
        }
        if self.session_id.is_some() && self.resume.is_some() && !self.fork_session {
            return Err(Error::InvalidOptions(
                "session_id cannot be combined with resume unless fork_session is set".into(),
            ));
        }
        if self.resume_session_at.is_some() && self.resume.is_none() {
            return Err(Error::InvalidOptions(
                "resume_session_at requires resume".into(),
            ));
        }
        for (agent_name, agent) in &self.agents {
            if let Some(mcp_servers) = &agent.mcp_servers {
                for spec in mcp_servers {
                    if let AgentMcpServerSpec::Inline(servers) = spec
                        && servers.values().any(|config| {
                            matches!(config, claude_protocol::stream::McpServerConfig::Sdk { .. })
                        })
                    {
                        return Err(Error::InvalidOptions(format!(
                            "agent `{agent_name}` cannot contain an in-process SDK MCP server"
                        )));
                    }
                }
            }
        }
        if let (Some(model), Some(fallback)) = (&self.model, &self.fallback_model)
            && model == fallback
        {
            return Err(Error::InvalidOptions(
                "fallback_model must differ from model".into(),
            ));
        }
        for (name, value) in [
            ("session_id", self.session_id.as_deref()),
            ("resume", self.resume.as_deref()),
            ("resume_session_at", self.resume_session_at.as_deref()),
        ] {
            if let Some(value) = value
                && uuid::Uuid::parse_str(value).is_err()
            {
                return Err(Error::InvalidOptions(format!(
                    "{name} must be a valid UUID"
                )));
            }
        }
        if self.max_turns == Some(0) {
            return Err(Error::InvalidOptions("max_turns must be positive".into()));
        }
        if self
            .max_budget_usd
            .is_some_and(|budget| !budget.is_finite() || budget < 0.0)
        {
            return Err(Error::InvalidOptions(
                "max_budget_usd must be finite and non-negative".into(),
            ));
        }
        if self.settings.as_ref().is_some_and(
            |settings| matches!(settings, SettingsConfig::Inline(value) if !value.is_object()),
        ) {
            return Err(Error::InvalidOptions(
                "inline settings must be a JSON object".into(),
            ));
        }
        if self
            .managed_settings
            .as_ref()
            .is_some_and(|settings| !settings.is_object())
        {
            return Err(Error::InvalidOptions(
                "managed_settings must be a JSON object".into(),
            ));
        }
        Ok(())
    }
}

impl Default for QueryOptions {
    fn default() -> Self {
        let mut options = Self::new("");
        options.model = None;
        options
    }
}
