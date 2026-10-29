use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

use crate::Drift;
use crate::stream::init::{AgentInfo, AppliedSettings, SlashCommand};
use crate::stream::options::{AgentDefinition, McpServerConfig};
use crate::stream::types::{Extensions, PermissionMode, PermissionUpdate, present_nullable};
use crate::strictness::{self, tagged_enum};

/// A control request, in either direction: amux asking Claude to change or
/// report something, or Claude asking amux to decide.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlRequest {
    pub request_id: String,
    pub request: ControlRequestBody,
    #[serde(flatten)]
    pub extensions: Extensions,
}

impl ControlRequest {
    pub fn new(request_id: impl Into<String>, request: ControlRequestBody) -> Self {
        Self {
            request_id: request_id.into(),
            request,
            extensions: Extensions::new(),
        }
    }
}

/// Claude withdrawing a request it asked amux, which needs no answer now.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlCancelRequest {
    pub request_id: String,
    #[serde(flatten)]
    pub extensions: Extensions,
}

tagged_enum! {
    pub enum ControlRequestBody by "subtype" {
        // What amux asks of Claude.
        "initialize" => Initialize(InitializeRequestBody),
        "interrupt" => Interrupt(InterruptRequest),
        "set_permission_mode" => SetPermissionMode(SetPermissionModeRequest),
        "set_mcp_permission_mode_override" => SetMcpPermissionModeOverride(McpPermissionModeOverrideRequest),
        "set_model" => SetModel(SetModelRequest),
        "apply_flag_settings" => ApplyFlagSettings(ApplyFlagSettingsRequest),
        "mcp_status" => McpStatus(Extensions),
        "get_context_usage" => GetContextUsage(Extensions),
        "get_settings" => GetSettings(Extensions),
        "reload_plugins" => ReloadPlugins(Extensions),
        "reload_skills" => ReloadSkills(Extensions),
        "rewind_files" => RewindFiles(RewindFilesRequest),
        "seed_read_state" => SeedReadState(SeedReadStateRequest),
        "mcp_reconnect" => McpReconnect(McpServerRequest),
        "mcp_toggle" => McpToggle(McpToggleRequest),
        "mcp_set_servers" => McpSetServers(McpSetServersRequest),
        "stop_task" => StopTask(StopTaskRequest),
        "background_tasks" => BackgroundTasks(BackgroundTasksRequest),
        // What Claude asks of amux.
        "can_use_tool" => CanUseTool(CanUseToolRequest),
        "hook_callback" => HookCallback(HookCallbackRequest),
        "mcp_message" => McpMessage(McpMessageRequest),
        "elicitation" => Elicitation(ElicitationRequestBody),
        "request_user_dialog" => UserDialog(UserDialogRequestBody),
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeRequestBody {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sdk_mcp_servers: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hooks: Option<BTreeMap<String, Vec<HookMatcherConfig>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub json_schema: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub append_system_prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_mode_instructions: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_aliases: Option<BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclude_dynamic_sections: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agents: Option<BTreeMap<String, AgentDefinition>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_suggestions: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_progress_summaries: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forward_subagent_text: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supported_dialog_kinds: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_task_stop_affordance: Option<bool>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HookMatcherConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matcher: Option<String>,
    pub hook_callback_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<f64>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InterruptRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancel_queued: Option<bool>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetPermissionModeRequest {
    pub mode: PermissionMode,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpPermissionModeOverrideRequest {
    #[serde(rename = "serverName")]
    pub server_name: String,
    pub mode: Option<McpPermissionMode>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SetModelRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApplyFlagSettingsRequest {
    pub settings: FlagSettings,
    #[serde(flatten)]
    pub extensions: Extensions,
}

/// Settings a session applies over its settings files.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FlagSettings {
    /// The effort to run at; null clears the session's own, back to the
    /// settings files'.
    #[serde(
        rename = "effortLevel",
        default,
        deserialize_with = "present_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub effort_level: Option<Option<String>>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RewindFilesRequest {
    pub user_message_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dry_run: Option<bool>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeedReadStateRequest {
    pub path: String,
    pub mtime: u64,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerRequest {
    #[serde(rename = "serverName")]
    pub server_name: String,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToggleRequest {
    #[serde(rename = "serverName")]
    pub server_name: String,
    pub enabled: bool,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpSetServersRequest {
    pub servers: BTreeMap<String, McpServerConfig>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StopTaskRequest {
    pub task_id: String,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BackgroundTasksRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

/// Claude asking whether a tool may run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanUseToolRequest {
    pub tool_name: String,
    pub input: serde_json::Value,
    pub tool_use_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_suggestions: Option<Vec<PermissionUpdate>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_reason_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires_user_interaction: Option<bool>,
    /// The subagent whose call this is; absent for the main thread's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    /// No persistent rule may be offered: Claude narrows or drops one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suppress_always_allow_rule: Option<bool>,
    /// One stray keystroke must not approve the call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_to_no: Option<bool>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

/// Claude running one of the hooks amux registered at initialize.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookCallbackRequest {
    pub callback_id: String,
    /// The hook's input, shaped by its event.
    pub input: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

/// A JSON-RPC message for an MCP server amux hosts in its own process.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpMessageRequest {
    pub server_name: String,
    pub message: serde_json::Value,
    #[serde(flatten)]
    pub extensions: Extensions,
}

/// An MCP server asking the person something through Claude.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ElicitationRequestBody {
    pub mcp_server_name: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_schema: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elicitation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

/// Claude asking the person through one of its own dialogs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserDialogRequestBody {
    pub dialog_kind: String,
    pub payload: serde_json::Map<String, serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum McpPermissionMode {
    Default,
    Auto,
}

/// The answer to a control request, in either direction. What `response`
/// holds depends on the request; [`ControlResponse::result`] reads it as
/// that request's result type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlResponse {
    pub response: ControlResponseInner,
    #[serde(flatten)]
    pub extensions: Extensions,
}

impl ControlResponse {
    /// A successful answer carrying `response`.
    pub fn success(request_id: impl Into<String>, response: &impl Serialize) -> Self {
        Self {
            response: ControlResponseInner {
                subtype: ControlOutcome::Success,
                request_id: request_id.into(),
                response: Some(serde_json::to_value(response).expect("a response serializes")),
                error: None,
                extensions: Extensions::new(),
            },
            extensions: Extensions::new(),
        }
    }

    /// A refusal carrying why.
    pub fn error(request_id: impl Into<String>, error: impl Into<String>) -> Self {
        Self {
            response: ControlResponseInner {
                subtype: ControlOutcome::Error,
                request_id: request_id.into(),
                response: None,
                error: Some(error.into()),
                extensions: Extensions::new(),
            },
            extensions: Extensions::new(),
        }
    }

    pub fn request_id(&self) -> &str {
        &self.response.request_id
    }

    /// Reads a successful answer as the result type of its request.
    pub fn result<T: serde::de::DeserializeOwned>(&self) -> Option<Result<T, serde_json::Error>> {
        self.response.response.as_ref().map(T::deserialize)
    }

    /// [`result`](Self::result), failing on anything the decode would keep
    /// without knowing it, as [`strict`](crate::stream::strict) does for a
    /// whole line.
    pub fn strict_result<T: serde::de::DeserializeOwned>(&self) -> Option<Result<T, Drift>> {
        let response = self.response.response.as_ref()?;
        let (decoded, mut reasons) = strictness::noticing(|| T::deserialize(response));
        Some(match decoded {
            Ok(result) if reasons.is_empty() => Ok(result),
            decoded => {
                if let Err(error) = decoded {
                    reasons.insert(0, error.to_string());
                }
                Err(Drift::Unknown {
                    kind: Some("control_response".into()),
                    reasons,
                })
            }
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlResponseInner {
    pub subtype: ControlOutcome,
    pub request_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ControlOutcome {
    Success,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterruptResult {
    pub still_queued: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancelled: Option<Vec<String>>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

/// The answer to `set_permission_mode`: the mode Claude applied.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetPermissionModeResult {
    pub mode: PermissionMode,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpPermissionModeOverrideResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerStatus {
    pub name: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_info: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<serde_json::Value>>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

/// The answer to `mcp_status`: every tool server and how it stands.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpStatusResult {
    #[serde(rename = "mcpServers")]
    pub mcp_servers: Vec<McpServerStatus>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

/// The answer to `get_settings`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SettingsResult {
    pub applied: AppliedSettings,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpSetServersResult {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub errors: HashMap<String, String>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RewindFilesResult {
    pub can_rewind: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files_changed: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub insertions: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deletions: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skipped_links: Option<u64>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReloadPluginsResult {
    pub commands: Vec<SlashCommand>,
    pub agents: Vec<AgentInfo>,
    pub plugins: Vec<PluginInfo>,
    #[serde(rename = "mcpServers")]
    pub mcp_servers: Vec<McpServerStatus>,
    pub error_count: u64,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginInfo {
    pub name: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReloadSkillsResult {
    pub skills: Vec<SlashCommand>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackgroundTaskSummary {
    pub id: String,
    pub r#type: String,
    pub status: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(flatten)]
    pub extensions: Extensions,
}
