use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::stream::types::{
    CompactTrigger, ConfigChangeSource, Extensions, PermissionMode, PermissionResult,
    PermissionUpdate, RawFrame, SessionStartSource, SetupTrigger,
};

#[derive(Debug, Clone)]
pub enum SettingsConfig {
    Inline(serde_json::Value),
    Path(PathBuf),
}

// ── SystemPrompt ────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SystemPrompt {
    Custom(String),
    Blocks(Vec<String>),
    Preset {
        preset: SystemPromptPreset,
        append: Option<String>,
        #[serde(default)]
        exclude_dynamic_sections: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SystemPromptPreset {
    ClaudeCode,
}

// ── ThinkingConfig ──────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThinkingConfig {
    Adaptive {
        display: Option<ThinkingDisplay>,
    },
    Enabled {
        budget_tokens: Option<u32>,
        display: Option<ThinkingDisplay>,
    },
    Disabled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThinkingDisplay {
    Summarized,
    Omitted,
}

// ── Effort ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effort {
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl Effort {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
            Self::Max => "max",
        }
    }
}

// ── ToolsConfig ─────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ToolsConfig {
    List(Vec<String>),
    Preset { preset: ToolsPreset },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolsPreset {
    ClaudeCode,
}

// ── ToolConfig ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolConfig {
    pub ask_user_question: Option<AskUserQuestionConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AskUserQuestionConfig {
    pub preview_format: Option<PreviewFormat>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreviewFormat {
    Markdown,
    Html,
}

// ── McpServerConfig ─────────────────────────────────────────────────

/// One MCP server as Claude Code reads it. `Sdk` names a server the program
/// driving Claude serves itself, answering Claude's `mcp_message` control
/// requests; only its name crosses the wire.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum McpServerConfig {
    Stdio(McpStdioServerConfig),
    Sse(McpSseServerConfig),
    Http(McpHttpServerConfig),
    Sdk { name: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpStdioServerConfig {
    pub command: String,
    /// Always serialized, even when empty.
    ///
    /// The published type makes this optional, and Claude Code accepts it
    /// missing when a server is configured at startup. The control that
    /// replaces the server set on a running session does not: it spreads the
    /// argument list without defaulting it first, and answers a config that
    /// omitted the key with `Spread syntax requires ...iterable not be null or
    /// undefined` - an error naming nothing a caller could act on. Sending the
    /// empty list means the same thing and avoids it.
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub env: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "alwaysLoad"
    )]
    pub always_load: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpSseServerConfig {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "alwaysLoad"
    )]
    pub always_load: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpHttpServerConfig {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "alwaysLoad"
    )]
    pub always_load: Option<bool>,
}

// ── AgentDefinition ─────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentDefinition {
    pub description: String,
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disallowed_tools: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_servers: Option<Vec<AgentMcpServerSpec>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<PermissionMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observer_message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AgentMcpServerSpec {
    Name(String),
    Inline(HashMap<String, McpServerConfig>),
}

// ── SdkBeta ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SdkBeta {
    #[serde(rename = "context-1m-2025-08-07")]
    Context1M,
}

#[derive(Debug, Clone)]
pub enum SkillsConfig {
    All,
    Selected(Vec<String>),
}

// ── SettingSource ───────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettingSource {
    User,
    Project,
    Local,
}

// ── SandboxSettings ─────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxSettings {
    pub enabled: Option<bool>,
    pub auto_allow_bash_if_sandboxed: Option<bool>,
    pub excluded_commands: Vec<String>,
    pub allow_unsandboxed_commands: Option<bool>,
    pub network: Option<SandboxNetworkConfig>,
    pub filesystem: Option<SandboxFilesystemConfig>,
    pub ignore_violations: HashMap<String, Vec<String>>,
    pub enable_weaker_nested_sandbox: Option<bool>,
    pub ripgrep: Option<RipgrepConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RipgrepConfig {
    pub command: String,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxNetworkConfig {
    pub allowed_domains: Vec<String>,
    pub allow_managed_domains_only: Option<bool>,
    pub allow_local_binding: Option<bool>,
    pub allow_unix_sockets: Vec<String>,
    pub allow_all_unix_sockets: Option<bool>,
    pub http_proxy_port: Option<u16>,
    pub socks_proxy_port: Option<u16>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxFilesystemConfig {
    pub allow_write: Vec<String>,
    pub deny_write: Vec<String>,
    pub deny_read: Vec<String>,
}

// ── HookEvent ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookEvent {
    PreToolUse,
    PostToolUse,
    PostToolUseFailure,
    PostToolBatch,
    Notification,
    UserPromptSubmit,
    UserPromptExpansion,
    SessionStart,
    SessionEnd,
    Stop,
    StopFailure,
    SubagentStart,
    SubagentStop,
    PreCompact,
    PostCompact,
    PermissionRequest,
    PermissionDenied,
    Setup,
    TeammateIdle,
    TaskCreated,
    TaskCompleted,
    Elicitation,
    ElicitationResult,
    ConfigChange,
    WorktreeCreate,
    WorktreeRemove,
    InstructionsLoaded,
    CwdChanged,
    FileChanged,
    DirectoryAdded,
    MessageDisplay,
}

impl HookEvent {
    pub fn wire_name(&self) -> &'static str {
        match self {
            HookEvent::PreToolUse => "PreToolUse",
            HookEvent::PostToolUse => "PostToolUse",
            HookEvent::PostToolUseFailure => "PostToolUseFailure",
            HookEvent::PostToolBatch => "PostToolBatch",
            HookEvent::Notification => "Notification",
            HookEvent::UserPromptSubmit => "UserPromptSubmit",
            HookEvent::UserPromptExpansion => "UserPromptExpansion",
            HookEvent::SessionStart => "SessionStart",
            HookEvent::SessionEnd => "SessionEnd",
            HookEvent::Stop => "Stop",
            HookEvent::StopFailure => "StopFailure",
            HookEvent::SubagentStart => "SubagentStart",
            HookEvent::SubagentStop => "SubagentStop",
            HookEvent::PreCompact => "PreCompact",
            HookEvent::PostCompact => "PostCompact",
            HookEvent::PermissionRequest => "PermissionRequest",
            HookEvent::PermissionDenied => "PermissionDenied",
            HookEvent::Setup => "Setup",
            HookEvent::TeammateIdle => "TeammateIdle",
            HookEvent::TaskCreated => "TaskCreated",
            HookEvent::TaskCompleted => "TaskCompleted",
            HookEvent::Elicitation => "Elicitation",
            HookEvent::ElicitationResult => "ElicitationResult",
            HookEvent::ConfigChange => "ConfigChange",
            HookEvent::WorktreeCreate => "WorktreeCreate",
            HookEvent::WorktreeRemove => "WorktreeRemove",
            HookEvent::InstructionsLoaded => "InstructionsLoaded",
            HookEvent::CwdChanged => "CwdChanged",
            HookEvent::FileChanged => "FileChanged",
            HookEvent::DirectoryAdded => "DirectoryAdded",
            HookEvent::MessageDisplay => "MessageDisplay",
        }
    }
}

/// A hook matcher Claude Code should forward as an [`SdkEvent`](crate::stream::SdkEvent).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookSubscription {
    pub event: HookEvent,
    pub matcher: Option<String>,
}

#[derive(Debug, Clone)]
pub struct HookCallbackContext {
    pub request_id: String,
    pub tool_use_id: Option<String>,
    pub extensions: Extensions,
}

// ── HookInput ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookInput {
    pub session_id: String,
    pub transcript_path: String,
    pub cwd: String,
    pub prompt_id: Option<String>,
    pub permission_mode: Option<String>,
    pub agent_id: Option<String>,
    pub agent_type: Option<String>,
    pub effort: Option<HookEffort>,
    pub event: HookEventData,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookEffort {
    pub level: String,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HookEventData {
    PreToolUse {
        tool_name: String,
        tool_input: serde_json::Value,
        tool_use_id: String,
    },
    PostToolUse {
        tool_name: String,
        tool_input: serde_json::Value,
        tool_response: serde_json::Value,
        tool_use_id: String,
    },
    PostToolUseFailure {
        tool_name: String,
        tool_input: serde_json::Value,
        tool_use_id: String,
        error: String,
        is_interrupt: Option<bool>,
    },
    PostToolBatch {
        tool_calls: Vec<PostToolBatchToolCall>,
    },
    Notification {
        message: String,
        title: Option<String>,
        notification_type: String,
    },
    UserPromptSubmit {
        prompt: String,
    },
    UserPromptExpansion {
        expansion_type: String,
        command_name: String,
        command_args: String,
        command_source: Option<String>,
        prompt: String,
    },
    SessionStart {
        source: SessionStartSource,
        model: Option<String>,
    },
    SessionEnd {
        reason: String,
    },
    Stop {
        stop_hook_active: bool,
        last_assistant_message: Option<String>,
    },
    StopFailure {
        error: String,
        error_details: Option<String>,
        last_assistant_message: Option<String>,
    },
    SubagentStart {
        agent_id: String,
        agent_type: String,
    },
    SubagentStop {
        stop_hook_active: bool,
        agent_id: String,
        agent_transcript_path: String,
        agent_type: String,
        last_assistant_message: Option<String>,
    },
    PreCompact {
        trigger: CompactTrigger,
        custom_instructions: Option<String>,
    },
    PostCompact {
        trigger: CompactTrigger,
        compact_summary: String,
    },
    PermissionRequest {
        tool_name: String,
        tool_input: serde_json::Value,
        permission_suggestions: Option<Vec<PermissionUpdate>>,
    },
    PermissionDenied {
        tool_name: String,
        tool_input: serde_json::Value,
        tool_use_id: String,
        reason: String,
    },
    Setup {
        trigger: SetupTrigger,
    },
    TeammateIdle {
        teammate_name: String,
        team_name: String,
    },
    TaskCreated {
        task_id: String,
        task_subject: String,
        task_description: Option<String>,
        teammate_name: Option<String>,
        team_name: Option<String>,
    },
    TaskCompleted {
        task_id: String,
        task_subject: String,
        task_description: Option<String>,
        teammate_name: Option<String>,
        team_name: Option<String>,
    },
    Elicitation {
        mcp_server_name: String,
        message: String,
        mode: Option<ElicitationMode>,
        url: Option<String>,
        elicitation_id: Option<String>,
        requested_schema: Option<serde_json::Value>,
    },
    ElicitationResult {
        mcp_server_name: String,
        elicitation_id: Option<String>,
        mode: Option<ElicitationMode>,
        action: String,
        content: Option<serde_json::Value>,
    },
    ConfigChange {
        source: ConfigChangeSource,
        file_path: Option<String>,
    },
    WorktreeCreate {
        name: String,
    },
    WorktreeRemove {
        worktree_path: String,
    },
    InstructionsLoaded {
        file_path: String,
        memory_type: String,
        load_reason: String,
        globs: Option<Vec<String>>,
        trigger_file_path: Option<String>,
        parent_file_path: Option<String>,
    },
    CwdChanged {
        old_cwd: String,
        new_cwd: String,
    },
    FileChanged {
        file_path: String,
        event: String,
    },
    DirectoryAdded {
        directory: String,
        source: String,
    },
    MessageDisplay {
        turn_id: String,
        message_id: String,
        index: u64,
        final_delta: bool,
        delta: String,
    },
    #[serde(untagged)]
    Unknown(RawFrame),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostToolBatchToolCall {
    pub tool_name: String,
    pub tool_input: serde_json::Value,
    pub tool_use_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_response: Option<serde_json::Value>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

// ── HookOutput ──────────────────────────────────────────────────────

/// amux's answer to a `hook_callback`.
#[derive(Debug, Clone)]
pub enum HookOutput {
    /// The hook goes on in the background; Claude waits at most `timeout`
    /// for it. Written `{"async": true, "asyncTimeout": milliseconds}`.
    Async {
        timeout: Option<std::time::Duration>,
    },
    Sync(SyncHookOutput),
}

impl Serialize for HookOutput {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Async { timeout } => {
                let mut object = serde_json::Map::new();
                object.insert("async".into(), true.into());
                if let Some(timeout) = timeout {
                    let millis = u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX);
                    object.insert("asyncTimeout".into(), millis.into());
                }
                object.serialize(serializer)
            }
            Self::Sync(sync) => sync.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for HookOutput {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;
        let mut object = serde_json::Map::<String, serde_json::Value>::deserialize(deserializer)?;
        if object.get("async") != Some(&serde_json::Value::Bool(true)) {
            return SyncHookOutput::deserialize(serde_json::Value::Object(object))
                .map(Self::Sync)
                .map_err(D::Error::custom);
        }
        object.remove("async");
        let timeout = match object.remove("asyncTimeout") {
            None => None,
            Some(millis) => Some(std::time::Duration::from_millis(
                millis
                    .as_u64()
                    .ok_or_else(|| D::Error::custom("asyncTimeout is not milliseconds"))?,
            )),
        };
        if let Some(field) = object.keys().next() {
            return Err(D::Error::custom(format!(
                "an async hook answer with {field:?}"
            )));
        }
        Ok(Self::Async { timeout })
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncHookOutput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#continue: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suppress_output: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<HookDecision>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hook_specific_output: Option<HookSpecificOutput>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookDecision {
    Approve,
    Block,
}

/// What a hook answers for its own event, told apart by `hookEventName`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "hookEventName", rename_all_fields = "camelCase")]
pub enum HookSpecificOutput {
    PreToolUse {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        permission_decision: Option<HookPermissionDecision>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        permission_decision_reason: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        updated_input: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        additional_context: Option<String>,
    },
    UserPromptSubmit {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        additional_context: Option<String>,
    },
    UserPromptExpansion {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        additional_context: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        suppress_original_prompt: Option<bool>,
    },
    SessionStart {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        additional_context: Option<String>,
    },
    Setup {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        additional_context: Option<String>,
    },
    SubagentStart {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        additional_context: Option<String>,
    },
    Stop {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        additional_context: Option<String>,
    },
    SubagentStop {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        additional_context: Option<String>,
    },
    PostToolUse {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        additional_context: Option<String>,
        #[serde(
            rename = "updatedMCPToolOutput",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        updated_mcp_tool_output: Option<serde_json::Value>,
    },
    PostToolUseFailure {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        additional_context: Option<String>,
    },
    PostToolBatch {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        additional_context: Option<String>,
    },
    Notification {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        additional_context: Option<String>,
    },
    PermissionRequest {
        decision: PermissionResult,
    },
    PermissionDenied {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        retry: Option<bool>,
    },
    Elicitation {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        action: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content: Option<serde_json::Value>,
    },
    ElicitationResult {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        action: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content: Option<serde_json::Value>,
    },
    CwdChanged {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        watch_paths: Option<Vec<String>>,
    },
    FileChanged {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        watch_paths: Option<Vec<String>>,
    },
    WorktreeCreate {
        worktree_path: String,
    },
    MessageDisplay {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        display_content: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookPermissionDecision {
    Allow,
    Deny,
    Ask,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ElicitationRequest {
    pub server_name: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<ElicitationMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elicitation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_schema: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ElicitationMode {
    Form,
    Url,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ElicitationResult {
    Accept {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content: Option<serde_json::Value>,
        #[serde(flatten)]
        extensions: Extensions,
    },
    Decline {
        #[serde(flatten)]
        extensions: Extensions,
    },
    Cancel {
        #[serde(flatten)]
        extensions: Extensions,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserDialogRequest {
    pub dialog_kind: String,
    pub payload: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "behavior", rename_all = "snake_case")]
pub enum UserDialogResult {
    Completed {
        result: serde_json::Value,
        #[serde(flatten)]
        extensions: Extensions,
    },
    Cancelled {
        #[serde(flatten)]
        extensions: Extensions,
    },
}
