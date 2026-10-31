use std::collections::BTreeMap;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer};

use crate::stream::types::{
    ApiKeySource, ApiMessage, AssistantMessageError, CompactTrigger, Extensions, MessageParam,
    ModelUsage, PermissionMode, RawFrame, StreamEvent, Usage, present_nullable,
};

macro_rules! wire_struct {
    ($name:ident { $($fields:tt)* }) => {
        #[derive(Debug, Clone, Deserialize)]
        pub struct $name {
            $($fields)*
            #[serde(flatten)]
            pub extensions: Extensions,
        }
    };
}

#[derive(Debug, Clone)]
pub enum Message {
    Assistant(AssistantMessage),
    User(UserMessageOutput),
    UserReplay(UserMessageReplay),
    Result(ResultMessage),
    System(SystemInitMessage),
    StreamEvent(StreamEventMessage),
    CompactBoundary(CompactBoundaryMessage),
    Status(StatusMessage),
    ApiRetry(ApiRetryMessage),
    ModelRefusalFallback(ModelRefusalFallbackMessage),
    ModelRefusalNoFallback(ModelRefusalNoFallbackMessage),
    LocalCommandOutput(LocalCommandOutputMessage),
    AuthStatus(AuthStatusMessage),
    TaskNotification(TaskNotificationMessage),
    TaskStarted(TaskStartedMessage),
    TaskUpdated(TaskUpdatedMessage),
    TaskProgress(TaskProgressMessage),
    BackgroundTasksChanged(BackgroundTasksChangedMessage),
    ThinkingTokens(ThinkingTokensMessage),
    CommandsChanged(CommandsChangedMessage),
    RateLimit(RateLimitEvent),
    ElicitationComplete(ElicitationCompleteMessage),
    PermissionDenied(PermissionDeniedMessage),
    ConversationReset(ConversationResetMessage),
    CommandLifecycle(CommandLifecycleMessage),
    UnknownSystem(RawFrame),
    Unknown(RawFrame),
}

impl Message {
    pub fn parse(raw: serde_json::Value) -> Result<Self, String> {
        match required_string(&raw, "type")?.to_owned().as_str() {
            "assistant" => parse_known(raw, false).map(Self::Assistant),
            "user" => match raw.get("isReplay") {
                Some(serde_json::Value::Bool(true)) => {
                    parse_known(raw, false).map(Self::UserReplay)
                }
                Some(serde_json::Value::Bool(false)) | None => {
                    parse_known(raw, false).map(Self::User)
                }
                Some(_) => Err("known `user` frame has non-boolean `isReplay`".into()),
            },
            "result" => ResultMessage::parse(raw).map(Self::Result),
            "system" => Self::parse_system(raw),
            "stream_event" => parse_known(raw, false).map(Self::StreamEvent),
            "auth_status" => parse_known(raw, false).map(Self::AuthStatus),
            "rate_limit_event" => parse_known(raw, false).map(Self::RateLimit),
            "conversation_reset" => parse_known(raw, false).map(Self::ConversationReset),
            "command_lifecycle" => parse_known(raw, false).map(Self::CommandLifecycle),
            _ => Ok(Self::Unknown(RawFrame::unknown("frame", raw))),
        }
    }

    fn parse_system(raw: serde_json::Value) -> Result<Self, String> {
        match required_string(&raw, "subtype")?.to_owned().as_str() {
            "init" => parse_known(raw, true).map(Self::System),
            "compact_boundary" => parse_known(raw, true).map(Self::CompactBoundary),
            "status" => parse_known(raw, true).map(Self::Status),
            "api_retry" => parse_known(raw, true).map(Self::ApiRetry),
            "model_refusal_fallback" => parse_known(raw, true).map(Self::ModelRefusalFallback),
            "model_refusal_no_fallback" => parse_known(raw, true).map(Self::ModelRefusalNoFallback),
            "local_command_output" => parse_known(raw, true).map(Self::LocalCommandOutput),
            "task_notification" => parse_known(raw, true).map(Self::TaskNotification),
            "task_started" => parse_known(raw, true).map(Self::TaskStarted),
            "task_updated" => parse_known(raw, true).map(Self::TaskUpdated),
            "task_progress" => parse_known(raw, true).map(Self::TaskProgress),
            "background_tasks_changed" => parse_known(raw, true).map(Self::BackgroundTasksChanged),
            "thinking_tokens" => parse_known(raw, true).map(Self::ThinkingTokens),
            "commands_changed" => parse_known(raw, true).map(Self::CommandsChanged),
            "elicitation_complete" => parse_known(raw, true).map(Self::ElicitationComplete),
            "permission_denied" => parse_known(raw, true).map(Self::PermissionDenied),
            _ => Ok(Self::UnknownSystem(RawFrame::unknown("system frame", raw))),
        }
    }
}

impl<'de> Deserialize<'de> for Message {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::parse(serde_json::Value::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

wire_struct!(AssistantMessage {
    pub uuid: String,
    #[serde(default="crate::absent::session_id")] pub session_id: String,
    pub message: ApiMessage,
    #[serde(default="crate::absent::parent_tool_use_id",deserialize_with="required_option")] pub parent_tool_use_id: Option<String>,
    #[serde(default)] pub error: Option<AssistantMessageError>,
    #[serde(default)] pub request_id: Option<String>,
    #[serde(default)] pub user_message_uuid: Option<String>,
    #[serde(default)] pub timestamp: Option<String>,
});

wire_struct!(UserMessageOutput {
    #[serde(default)] pub uuid: Option<String>,
    #[serde(default)] pub session_id: Option<String>,
    pub message: MessageParam,
    #[serde(default="crate::absent::parent_tool_use_id",deserialize_with="required_option")] pub parent_tool_use_id: Option<String>,
    #[serde(default)] pub tool_use_result: Option<serde_json::Value>,
    #[serde(default)] pub priority: Option<String>,
    #[serde(default)] pub origin: Option<serde_json::Value>,
    #[serde(default)] pub timestamp: Option<String>,
});

wire_struct!(UserMessageReplay {
    pub uuid: String,
    #[serde(default="crate::absent::session_id")] pub session_id: String,
    pub message: MessageParam,
    #[serde(default="crate::absent::parent_tool_use_id",deserialize_with="required_option")] pub parent_tool_use_id: Option<String>,
    #[serde(default)] pub tool_use_result: Option<serde_json::Value>,
    #[serde(default)] pub timestamp: Option<String>,
});

#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum ResultMessage {
    Success(ResultSuccess),
    ErrorDuringExecution(ResultError),
    ErrorMaxTurns(ResultError),
    ErrorMaxBudgetUsd(ResultError),
    ErrorMaxStructuredOutputRetries(ResultError),
    Unknown(RawFrame),
}

impl ResultMessage {
    fn parse(raw: serde_json::Value) -> Result<Self, String> {
        match required_string(&raw, "subtype")?.to_owned().as_str() {
            "success" => parse_known(raw, true).map(Self::Success),
            "error_during_execution" => parse_known(raw, true).map(Self::ErrorDuringExecution),
            "error_max_turns" => parse_known(raw, true).map(Self::ErrorMaxTurns),
            "error_max_budget_usd" => parse_known(raw, true).map(Self::ErrorMaxBudgetUsd),
            "error_max_structured_output_retries" => {
                parse_known(raw, true).map(Self::ErrorMaxStructuredOutputRetries)
            }
            _ => Ok(Self::Unknown(RawFrame::unknown("result", raw))),
        }
    }
    /// The result's `subtype` as written.
    pub fn subtype(&self) -> &str {
        match self {
            Self::Success(_) => "success",
            Self::ErrorDuringExecution(_) => "error_during_execution",
            Self::ErrorMaxTurns(_) => "error_max_turns",
            Self::ErrorMaxBudgetUsd(_) => "error_max_budget_usd",
            Self::ErrorMaxStructuredOutputRetries(_) => "error_max_structured_output_retries",
            Self::Unknown(v) => v.field("subtype").and_then(|v| v.as_str()).unwrap_or(""),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ResultCommon {
    pub uuid: String,
    #[serde(default = "crate::absent::session_id")]
    pub session_id: String,
    pub duration_ms: u64,
    #[serde(default = "crate::absent::is_error")]
    pub is_error: bool,
    #[serde(default = "crate::absent::usage")]
    pub usage: Usage,
    #[serde(rename = "modelUsage", default = "crate::absent::model_usage")]
    pub model_usage: Option<BTreeMap<String, ModelUsage>>,
    #[serde(default)]
    pub user_message_uuid: Option<String>,
    /// Every client uuid whose prompt this turn consumed.
    #[serde(default)]
    pub user_message_uuids: Option<Vec<String>>,
    #[serde(default)]
    pub terminal_reason: Option<String>,
    #[serde(default)]
    pub origin: Option<ResultOrigin>,
}
// Who the turn a result ends was for: `human` for a person's prompt.
wire_struct!(ResultOrigin {
    #[serde(default)] pub kind: Option<String>,
});
wire_struct!(ResultSuccess {
    #[serde(flatten)] pub common:ResultCommon,#[serde(default="crate::absent::result")] pub result:String,
});
wire_struct!(ResultError { #[serde(flatten)] pub common:ResultCommon,#[serde(default="crate::absent::errors")] pub errors:Vec<String>, });

wire_struct!(SystemInitMessage {
    pub uuid:String,#[serde(default="crate::absent::session_id")] pub session_id:String,
    #[serde(default)] pub agents:Option<Vec<String>>,
    #[serde(rename="apiKeySource",default="crate::absent::api_key_source")] pub api_key_source:ApiKeySource,
    #[serde(default="crate::absent::cwd")] pub cwd:String,
    #[serde(default="crate::absent::tools")] pub tools:Vec<String>,
    #[serde(default="crate::absent::mcp_servers")] pub mcp_servers:Option<Vec<McpServerInfo>>,
    #[serde(default="crate::absent::model")] pub model:String,
    #[serde(rename="permissionMode",default="crate::absent::permission_mode")] pub permission_mode:PermissionMode,
    #[serde(default="crate::absent::skills")] pub skills:Vec<String>,
    #[serde(default="crate::absent::plugins")] pub plugins:Vec<PluginInfo>,
    #[serde(default)] pub capabilities:Option<Vec<String>>,
    #[serde(default,deserialize_with="present_nullable")] pub effort:Option<Option<String>>,
});
wire_struct!(McpServerInfo { pub name:String,pub status:String, });
wire_struct!(PluginInfo { pub name:String,pub path:String,#[serde(default)] pub version:Option<String>, });
wire_struct!(StreamEventMessage {
    pub event:StreamEvent,#[serde(default="crate::absent::parent_tool_use_id",deserialize_with="required_option")] pub parent_tool_use_id:Option<String>,
    #[serde(default="crate::absent::uuid")] pub uuid:String,#[serde(default="crate::absent::session_id")] pub session_id:String,
    #[serde(default)] pub user_message_uuid:Option<String>,
});
wire_struct!(CompactBoundaryMessage { pub uuid:String,#[serde(default="crate::absent::session_id")] pub session_id:String,pub compact_metadata:CompactMetadata, });
wire_struct!(CompactMetadata {
    pub trigger:CompactTrigger,pub pre_tokens:u64,
    #[serde(default)] pub post_tokens:Option<u64>,
    #[serde(default)] pub duration_ms:Option<u64>,
});
wire_struct!(StatusMessage {
    #[serde(deserialize_with="required_option")] pub status:Option<String>,
    #[serde(rename="permissionMode",default)] pub permission_mode:Option<PermissionMode>,
    pub uuid:String,#[serde(default="crate::absent::session_id")] pub session_id:String,
});
wire_struct!(ApiRetryMessage {
    pub attempt:u32,pub max_retries:u32,pub retry_delay_ms:u64,
    #[serde(deserialize_with="required_option")] pub error_status:Option<u16>,
    pub error:AssistantMessageError,pub uuid:String,#[serde(default="crate::absent::session_id")] pub session_id:String,
});
wire_struct!(ModelRefusalFallbackMessage {
    pub trigger:String,pub direction:String,
    #[serde(default)] pub scope:Option<String>,
    pub original_model:String,pub fallback_model:String,
    #[serde(deserialize_with="required_option")] pub request_id:Option<String>,
    #[serde(default)] pub api_refusal_category:Option<String>,
    #[serde(default)] pub api_refusal_explanation:Option<String>,
    pub content:String,pub uuid:String,#[serde(default="crate::absent::session_id")] pub session_id:String,
});
wire_struct!(ModelRefusalNoFallbackMessage {
    pub original_model:String,#[serde(deserialize_with="required_option")] pub request_id:Option<String>,
    #[serde(default)] pub api_refusal_category:Option<String>,
    #[serde(default)] pub api_refusal_explanation:Option<String>,
    pub content:String,pub uuid:String,#[serde(default="crate::absent::session_id")] pub session_id:String,
});
wire_struct!(LocalCommandOutputMessage { pub content:String,pub uuid:String,#[serde(default="crate::absent::session_id")] pub session_id:String, });
wire_struct!(AuthStatusMessage {
    #[serde(rename="isAuthenticating")] pub is_authenticating:bool,pub output:Vec<String>,
    #[serde(default)] pub error:Option<String>,pub uuid:String,#[serde(default="crate::absent::session_id")] pub session_id:String,
});
wire_struct!(TaskUsage { pub total_tokens:u64,pub tool_uses:u32,pub duration_ms:u64, });
wire_struct!(TaskNotificationMessage {
    pub task_id:String,#[serde(default)] pub tool_use_id:Option<String>,
    pub status:String,pub output_file:String,pub summary:String,
    #[serde(default)] pub usage:Option<TaskUsage>,
    #[serde(default)] pub skip_transcript:Option<bool>,
    #[serde(default)] pub ambient:Option<bool>,pub uuid:String,#[serde(default="crate::absent::session_id")] pub session_id:String,
});
wire_struct!(TaskStartedMessage {
    pub task_id:String,#[serde(default)] pub tool_use_id:Option<String>,pub description:String,
    #[serde(default)] pub subagent_type:Option<String>,
    #[serde(default)] pub is_backgrounded:Option<bool>,
    #[serde(default)] pub task_type:Option<String>,
    #[serde(default)] pub prompt:Option<String>,
    #[serde(default)] pub ambient:Option<bool>,pub uuid:String,#[serde(default="crate::absent::session_id")] pub session_id:String,
});
wire_struct!(TaskPatch {
    #[serde(default)] pub status:Option<String>,
    #[serde(default)] pub description:Option<String>,
    #[serde(default)] pub error:Option<String>,
    #[serde(default)] pub is_backgrounded:Option<bool>,
});
wire_struct!(TaskUpdatedMessage { pub task_id:String,pub patch:TaskPatch,pub uuid:String,#[serde(default="crate::absent::session_id")] pub session_id:String, });
wire_struct!(TaskProgressMessage {
    pub task_id:String,#[serde(default)] pub tool_use_id:Option<String>,pub description:String,
    #[serde(default)] pub subagent_type:Option<String>,pub usage:TaskUsage,
    #[serde(default)] pub last_tool_name:Option<String>,
    #[serde(default)] pub summary:Option<String>,pub uuid:String,#[serde(default="crate::absent::session_id")] pub session_id:String,
});
wire_struct!(BackgroundTask { #[serde(default="crate::absent::task_id")] pub task_id:String,#[serde(default="crate::absent::task_type")] pub task_type:String,#[serde(default="crate::absent::description")] pub description:String,#[serde(default)] pub ambient:Option<bool>, });
wire_struct!(BackgroundTasksChangedMessage { #[serde(default="crate::absent::tasks")] pub tasks:Option<Vec<BackgroundTask>>,pub uuid:String,#[serde(default="crate::absent::session_id")] pub session_id:String, });
wire_struct!(ThinkingTokensMessage { pub estimated_tokens:u64,pub estimated_tokens_delta:u64,pub uuid:String,#[serde(default="crate::absent::session_id")] pub session_id:String, });
wire_struct!(CommandsChangedMessage { pub commands:Vec<crate::stream::init::SlashCommand>,pub uuid:String,#[serde(default="crate::absent::session_id")] pub session_id:String, });
wire_struct!(RateLimitInfo {
    pub status:String,
    #[serde(rename="resetsAt",default)] pub resets_at:Option<u64>,
    #[serde(rename="rateLimitType",default)] pub rate_limit_type:Option<String>,
    #[serde(default)] pub utilization:Option<f64>,
    /// Each window's use by its name (`five_hour`, `seven_day`, a model's).
    #[serde(rename="unifiedWindows",default)] pub unified_windows:Option<BTreeMap<String,RateLimitWindow>>,
});
wire_struct!(RateLimitWindow {
    #[serde(default)] pub utilization:Option<f64>,
    #[serde(rename="resetsAt",default)] pub resets_at:Option<i64>,
});
wire_struct!(RateLimitEvent { pub rate_limit_info:RateLimitInfo,pub uuid:String,#[serde(default="crate::absent::session_id")] pub session_id:String, });
wire_struct!(ElicitationCompleteMessage { pub mcp_server_name:String,pub elicitation_id:String,pub uuid:String,#[serde(default="crate::absent::session_id")] pub session_id:String, });
wire_struct!(PermissionDeniedMessage {
    pub tool_name:String,pub tool_use_id:String,
    #[serde(default)] pub agent_id:Option<String>,
    #[serde(default)] pub decision_reason:Option<String>,
    pub message:String,pub uuid:String,#[serde(default="crate::absent::session_id")] pub session_id:String,
});
// Where a queued prompt is in its life, named by the sender's uuid.
wire_struct!(CommandLifecycleMessage { pub command_uuid:String,pub state:String,#[serde(default="crate::absent::uuid")] pub uuid:String,#[serde(default="crate::absent::session_id")] pub session_id:String, });
// The session's title, as set by the caller's `title` option or changed since.
wire_struct!(ConversationResetMessage { pub new_conversation_id:String,pub uuid:String,#[serde(default="crate::absent::session_id")] pub session_id:String, });

fn required_string<'a>(raw: &'a serde_json::Value, field: &str) -> Result<&'a str, String> {
    raw.get(field)
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("Claude protocol frame requires string field `{field}`"))
}
fn parse_known<T: DeserializeOwned>(
    mut raw: serde_json::Value,
    has_subtype: bool,
) -> Result<T, String> {
    if let Some(object) = raw.as_object_mut() {
        object.remove("type");
        if has_subtype {
            object.remove("subtype");
        }
    }
    serde_json::from_value(raw).map_err(|error| format!("malformed known Claude frame: {error}"))
}
fn required_option<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> Result<Option<T>, D::Error> {
    Option::<T>::deserialize(deserializer)
}
