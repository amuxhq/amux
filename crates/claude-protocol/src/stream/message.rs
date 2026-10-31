use std::collections::BTreeMap;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::stream::error::ProtocolError;
use crate::stream::types::{
    ApiKeySource, ApiMessage, AssistantMessageError, CompactTrigger, Extensions, MessageParam,
    ModelUsage, PermissionMode, RawFrame, StreamEvent, Usage, present_nullable,
};

macro_rules! wire_struct {
    ($name:ident { $($fields:tt)* }) => {
        #[derive(Debug, Clone, Serialize, Deserialize)]
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
    pub fn parse(raw: serde_json::Value) -> Result<Self, ProtocolError> {
        match required_string(&raw, "type")?.to_owned().as_str() {
            "assistant" => parse_known(raw, false).map(Self::Assistant),
            "user" => match raw.get("isReplay") {
                Some(serde_json::Value::Bool(true)) => {
                    parse_known(raw, false).map(Self::UserReplay)
                }
                Some(serde_json::Value::Bool(false)) | None => {
                    parse_known(raw, false).map(Self::User)
                }
                Some(_) => Err(ProtocolError::with_frame(
                    "known `user` frame has non-boolean `isReplay`",
                    raw,
                )),
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

    fn parse_system(raw: serde_json::Value) -> Result<Self, ProtocolError> {
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

    pub fn kind(&self) -> &str {
        match self {
            Self::Assistant(_) => "assistant",
            Self::User(_) => "user",
            Self::UserReplay(_) => "user.replay",
            Self::Result(v) => v.kind(),
            Self::System(_) => "system.init",
            Self::StreamEvent(_) => "stream_event",
            Self::CompactBoundary(_) => "system.compact_boundary",
            Self::Status(_) => "system.status",
            Self::ApiRetry(_) => "system.api_retry",
            Self::ModelRefusalFallback(_) => "system.model_refusal_fallback",
            Self::ModelRefusalNoFallback(_) => "system.model_refusal_no_fallback",
            Self::LocalCommandOutput(_) => "system.local_command_output",
            Self::AuthStatus(_) => "auth_status",
            Self::TaskNotification(_) => "system.task_notification",
            Self::TaskStarted(_) => "system.task_started",
            Self::TaskUpdated(_) => "system.task_updated",
            Self::TaskProgress(_) => "system.task_progress",
            Self::BackgroundTasksChanged(_) => "system.background_tasks_changed",
            Self::ThinkingTokens(_) => "system.thinking_tokens",
            Self::CommandsChanged(_) => "system.commands_changed",
            Self::RateLimit(_) => "rate_limit_event",
            Self::ElicitationComplete(_) => "system.elicitation_complete",
            Self::PermissionDenied(_) => "system.permission_denied",
            Self::ConversationReset(_) => "conversation_reset",
            Self::CommandLifecycle(_) => "command_lifecycle",
            Self::UnknownSystem(v) => v
                .field("subtype")
                .and_then(|v| v.as_str())
                .unwrap_or("system.unknown"),
            Self::Unknown(v) => v
                .field("type")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown"),
        }
    }

    pub fn session_id(&self) -> Option<&str> {
        match self {
            Self::User(v) => v.session_id.as_deref(),
            Self::Result(v) => v.session_id(),
            Self::UnknownSystem(v) | Self::Unknown(v) => {
                v.field("session_id").and_then(|v| v.as_str())
            }
            Self::Assistant(v) => Some(&v.session_id),
            Self::UserReplay(v) => Some(&v.session_id),
            Self::System(v) => Some(&v.session_id),
            Self::StreamEvent(v) => Some(&v.session_id),
            Self::CompactBoundary(v) => Some(&v.session_id),
            Self::Status(v) => Some(&v.session_id),
            Self::ApiRetry(v) => Some(&v.session_id),
            Self::ModelRefusalFallback(v) => Some(&v.session_id),
            Self::ModelRefusalNoFallback(v) => Some(&v.session_id),
            Self::LocalCommandOutput(v) => Some(&v.session_id),
            Self::AuthStatus(v) => Some(&v.session_id),
            Self::TaskNotification(v) => Some(&v.session_id),
            Self::TaskStarted(v) => Some(&v.session_id),
            Self::TaskUpdated(v) => Some(&v.session_id),
            Self::TaskProgress(v) => Some(&v.session_id),
            Self::BackgroundTasksChanged(v) => Some(&v.session_id),
            Self::ThinkingTokens(v) => Some(&v.session_id),
            Self::CommandsChanged(v) => Some(&v.session_id),
            Self::RateLimit(v) => Some(&v.session_id),
            Self::ElicitationComplete(v) => Some(&v.session_id),
            Self::PermissionDenied(v) => Some(&v.session_id),
            Self::ConversationReset(v) => Some(&v.session_id),
            Self::CommandLifecycle(v) => Some(&v.session_id),
        }
    }
}

impl<'de> Deserialize<'de> for Message {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::parse(serde_json::Value::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

impl Serialize for Message {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        macro_rules! env {
            ($v:expr, $ty:expr, $sub:expr) => {
                serialize_envelope($v, $ty, $sub, serializer)
            };
        }
        match self {
            Self::Assistant(v) => env!(v, "assistant", None),
            Self::User(v) => env!(v, "user", None),
            Self::UserReplay(v) => env!(v, "user", None),
            Self::Result(v) => v.serialize(serializer),
            Self::System(v) => env!(v, "system", Some("init")),
            Self::StreamEvent(v) => env!(v, "stream_event", None),
            Self::CompactBoundary(v) => env!(v, "system", Some("compact_boundary")),
            Self::Status(v) => env!(v, "system", Some("status")),
            Self::ApiRetry(v) => env!(v, "system", Some("api_retry")),
            Self::ModelRefusalFallback(v) => env!(v, "system", Some("model_refusal_fallback")),
            Self::ModelRefusalNoFallback(v) => env!(v, "system", Some("model_refusal_no_fallback")),
            Self::LocalCommandOutput(v) => env!(v, "system", Some("local_command_output")),
            Self::AuthStatus(v) => env!(v, "auth_status", None),
            Self::TaskNotification(v) => env!(v, "system", Some("task_notification")),
            Self::TaskStarted(v) => env!(v, "system", Some("task_started")),
            Self::TaskUpdated(v) => env!(v, "system", Some("task_updated")),
            Self::TaskProgress(v) => env!(v, "system", Some("task_progress")),
            Self::BackgroundTasksChanged(v) => env!(v, "system", Some("background_tasks_changed")),
            Self::ThinkingTokens(v) => env!(v, "system", Some("thinking_tokens")),
            Self::CommandsChanged(v) => env!(v, "system", Some("commands_changed")),
            Self::RateLimit(v) => env!(v, "rate_limit_event", None),
            Self::ElicitationComplete(v) => env!(v, "system", Some("elicitation_complete")),
            Self::PermissionDenied(v) => env!(v, "system", Some("permission_denied")),
            Self::ConversationReset(v) => env!(v, "conversation_reset", None),
            Self::CommandLifecycle(v) => env!(v, "command_lifecycle", None),
            Self::UnknownSystem(v) | Self::Unknown(v) => v.serialize(serializer),
        }
    }
}

wire_struct!(AssistantMessage {
    pub uuid: String,
    #[serde(default="crate::absent::session_id")] pub session_id: String,
    pub message: ApiMessage,
    #[serde(default="crate::absent::parent_tool_use_id",deserialize_with="required_option")] pub parent_tool_use_id: Option<String>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub error: Option<AssistantMessageError>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub request_id: Option<String>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub user_message_uuid: Option<String>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub resumed_from_incomplete_thinking: Option<bool>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub supersedes: Option<Vec<String>>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub aborted: Option<bool>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub subagent_type: Option<String>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub task_description: Option<String>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub timestamp: Option<String>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub context_usage: Option<serde_json::Value>,
});

wire_struct!(UserMessageOutput {
    #[serde(default,skip_serializing_if="Option::is_none")] pub uuid: Option<String>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub session_id: Option<String>,
    pub message: MessageParam,
    #[serde(default="crate::absent::parent_tool_use_id",deserialize_with="required_option")] pub parent_tool_use_id: Option<String>,
    #[serde(rename="isSynthetic",default,skip_serializing_if="Option::is_none")] pub is_synthetic: Option<bool>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub tool_use_result: Option<serde_json::Value>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub priority: Option<String>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub origin: Option<serde_json::Value>,
    #[serde(rename="shouldQuery",default,skip_serializing_if="Option::is_none")] pub should_query: Option<bool>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub timestamp: Option<String>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub subagent_type: Option<String>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub task_description: Option<String>,
});

wire_struct!(UserMessageReplay {
    pub uuid: String,
    #[serde(default="crate::absent::session_id")] pub session_id: String,
    pub message: MessageParam,
    #[serde(default="crate::absent::parent_tool_use_id",deserialize_with="required_option")] pub parent_tool_use_id: Option<String>,
    #[serde(rename="isReplay")] pub is_replay: bool,
    #[serde(rename="isSynthetic",default,skip_serializing_if="Option::is_none")] pub is_synthetic: Option<bool>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub tool_use_result: Option<serde_json::Value>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub file_attachments: Option<Vec<serde_json::Value>>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub timestamp: Option<String>,
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
    fn parse(raw: serde_json::Value) -> Result<Self, ProtocolError> {
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
    pub fn kind(&self) -> &str {
        match self {
            Self::Success(_) => "result.success",
            Self::ErrorDuringExecution(_) => "result.error_during_execution",
            Self::ErrorMaxTurns(_) => "result.error_max_turns",
            Self::ErrorMaxBudgetUsd(_) => "result.error_max_budget_usd",
            Self::ErrorMaxStructuredOutputRetries(_) => {
                "result.error_max_structured_output_retries"
            }
            Self::Unknown(v) => v
                .field("subtype")
                .and_then(|v| v.as_str())
                .unwrap_or("result.unknown"),
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
    pub fn session_id(&self) -> Option<&str> {
        match self {
            Self::Success(v) => Some(&v.common.session_id),
            Self::ErrorDuringExecution(v)
            | Self::ErrorMaxTurns(v)
            | Self::ErrorMaxBudgetUsd(v)
            | Self::ErrorMaxStructuredOutputRetries(v) => Some(&v.common.session_id),
            Self::Unknown(v) => v.field("session_id").and_then(|v| v.as_str()),
        }
    }
}

impl Serialize for ResultMessage {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Success(v) => serialize_envelope(v, "result", Some("success"), serializer),
            Self::ErrorDuringExecution(v) => {
                serialize_envelope(v, "result", Some("error_during_execution"), serializer)
            }
            Self::ErrorMaxTurns(v) => {
                serialize_envelope(v, "result", Some("error_max_turns"), serializer)
            }
            Self::ErrorMaxBudgetUsd(v) => {
                serialize_envelope(v, "result", Some("error_max_budget_usd"), serializer)
            }
            Self::ErrorMaxStructuredOutputRetries(v) => serialize_envelope(
                v,
                "result",
                Some("error_max_structured_output_retries"),
                serializer,
            ),
            Self::Unknown(v) => v.serialize(serializer),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResultCommon {
    pub uuid: String,
    #[serde(default = "crate::absent::session_id")]
    pub session_id: String,
    pub duration_ms: u64,
    #[serde(default = "crate::absent::is_error")]
    pub is_error: bool,
    #[serde(
        default = "crate::absent::stop_reason",
        deserialize_with = "required_option"
    )]
    pub stop_reason: Option<String>,
    /// Written as Claude wrote it: `0` and `0.0` both occur.
    #[serde(
        default = "crate::absent::total_cost_usd",
        skip_serializing_if = "Option::is_none"
    )]
    pub total_cost_usd: Option<serde_json::Number>,
    #[serde(default = "crate::absent::usage")]
    pub usage: Usage,
    #[serde(
        rename = "modelUsage",
        default = "crate::absent::model_usage",
        skip_serializing_if = "Option::is_none"
    )]
    pub model_usage: Option<BTreeMap<String, ModelUsage>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_message_uuid: Option<String>,
    /// Every client uuid whose prompt this turn consumed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_message_uuids: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<ResultOrigin>,
}
// Who the turn a result ends was for: `human` for a person's prompt.
wire_struct!(ResultOrigin {
    #[serde(default,skip_serializing_if="Option::is_none")] pub kind: Option<String>,
});
wire_struct!(ResultSuccess {
    #[serde(flatten)] pub common:ResultCommon,#[serde(default="crate::absent::result")] pub result:String,
});
wire_struct!(ResultError { #[serde(flatten)] pub common:ResultCommon,#[serde(default="crate::absent::errors")] pub errors:Vec<String>, });

wire_struct!(SystemInitMessage {
    pub uuid:String,#[serde(default="crate::absent::session_id")] pub session_id:String,
    #[serde(default,skip_serializing_if="Option::is_none")] pub agents:Option<Vec<String>>,
    #[serde(rename="apiKeySource",default="crate::absent::api_key_source")] pub api_key_source:ApiKeySource,
    #[serde(default,skip_serializing_if="Option::is_none")] pub betas:Option<Vec<String>>,
    #[serde(default="crate::absent::claude_code_version")] pub claude_code_version:String,
    #[serde(default="crate::absent::cwd")] pub cwd:String,
    #[serde(default="crate::absent::tools")] pub tools:Vec<String>,
    #[serde(default="crate::absent::mcp_servers",skip_serializing_if="Option::is_none")] pub mcp_servers:Option<Vec<McpServerInfo>>,
    #[serde(default="crate::absent::model")] pub model:String,
    #[serde(rename="permissionMode",default="crate::absent::permission_mode")] pub permission_mode:PermissionMode,
    #[serde(default="crate::absent::skills")] pub skills:Vec<String>,
    #[serde(default="crate::absent::plugins")] pub plugins:Vec<PluginInfo>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub capabilities:Option<Vec<String>>,
    #[serde(default,deserialize_with="present_nullable",skip_serializing_if="Option::is_none")] pub effort:Option<Option<String>>,
});
wire_struct!(McpServerInfo { pub name:String,pub status:String, });
wire_struct!(PluginInfo { pub name:String,pub path:String,#[serde(default,skip_serializing_if="Option::is_none")] pub version:Option<String>, });
wire_struct!(StreamEventMessage {
    pub event:StreamEvent,#[serde(default="crate::absent::parent_tool_use_id",deserialize_with="required_option")] pub parent_tool_use_id:Option<String>,
    #[serde(default="crate::absent::uuid")] pub uuid:String,#[serde(default="crate::absent::session_id")] pub session_id:String,
    #[serde(default,skip_serializing_if="Option::is_none")] pub ttft_ms:Option<u64>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub user_message_uuid:Option<String>,
});
wire_struct!(CompactBoundaryMessage { pub uuid:String,#[serde(default="crate::absent::session_id")] pub session_id:String,pub compact_metadata:CompactMetadata, });
wire_struct!(CompactMetadata {
    pub trigger:CompactTrigger,pub pre_tokens:u64,
    #[serde(default,skip_serializing_if="Option::is_none")] pub post_tokens:Option<u64>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub duration_ms:Option<u64>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub preserved_segment:Option<serde_json::Value>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub preserved_messages:Option<serde_json::Value>,
});
wire_struct!(StatusMessage {
    #[serde(deserialize_with="required_option")] pub status:Option<String>,
    #[serde(rename="permissionMode",default,skip_serializing_if="Option::is_none")] pub permission_mode:Option<PermissionMode>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub compact_result:Option<String>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub compact_error:Option<String>,
    pub uuid:String,#[serde(default="crate::absent::session_id")] pub session_id:String,
});
wire_struct!(ApiRetryMessage {
    pub attempt:u32,pub max_retries:u32,pub retry_delay_ms:u64,
    #[serde(deserialize_with="required_option")] pub error_status:Option<u16>,
    pub error:AssistantMessageError,pub uuid:String,#[serde(default="crate::absent::session_id")] pub session_id:String,
});
wire_struct!(ModelRefusalFallbackMessage {
    pub trigger:String,pub direction:String,
    #[serde(default,skip_serializing_if="Option::is_none")] pub scope:Option<String>,
    pub original_model:String,pub fallback_model:String,
    #[serde(deserialize_with="required_option")] pub request_id:Option<String>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub api_refusal_category:Option<String>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub api_refusal_explanation:Option<String>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub retracted_message_uuids:Option<Vec<String>>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub refused_user_message_uuid:Option<String>,
    pub content:String,pub uuid:String,#[serde(default="crate::absent::session_id")] pub session_id:String,
});
wire_struct!(ModelRefusalNoFallbackMessage {
    pub original_model:String,#[serde(deserialize_with="required_option")] pub request_id:Option<String>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub api_refusal_category:Option<String>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub api_refusal_explanation:Option<String>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub refused_user_message_uuid:Option<String>,
    pub content:String,pub uuid:String,#[serde(default="crate::absent::session_id")] pub session_id:String,
});
wire_struct!(LocalCommandOutputMessage { pub content:String,pub uuid:String,#[serde(default="crate::absent::session_id")] pub session_id:String, });
wire_struct!(AuthStatusMessage {
    #[serde(rename="isAuthenticating")] pub is_authenticating:bool,pub output:Vec<String>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub error:Option<String>,pub uuid:String,#[serde(default="crate::absent::session_id")] pub session_id:String,
});
wire_struct!(TaskUsage { pub total_tokens:u64,pub tool_uses:u32,pub duration_ms:u64, });
wire_struct!(TaskNotificationMessage {
    pub task_id:String,#[serde(default,skip_serializing_if="Option::is_none")] pub tool_use_id:Option<String>,
    pub status:String,pub output_file:String,pub summary:String,
    #[serde(default,skip_serializing_if="Option::is_none")] pub usage:Option<TaskUsage>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub skip_transcript:Option<bool>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub ambient:Option<bool>,pub uuid:String,#[serde(default="crate::absent::session_id")] pub session_id:String,
});
wire_struct!(TaskStartedMessage {
    pub task_id:String,#[serde(default,skip_serializing_if="Option::is_none")] pub tool_use_id:Option<String>,pub description:String,
    #[serde(default,skip_serializing_if="Option::is_none")] pub subagent_type:Option<String>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub is_backgrounded:Option<bool>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub spawn_depth:Option<u32>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub task_type:Option<String>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub workflow_name:Option<String>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub prompt:Option<String>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub skip_transcript:Option<bool>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub ambient:Option<bool>,pub uuid:String,#[serde(default="crate::absent::session_id")] pub session_id:String,
});
wire_struct!(TaskPatch {
    #[serde(default,skip_serializing_if="Option::is_none")] pub status:Option<String>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub description:Option<String>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub end_time:Option<u64>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub total_paused_ms:Option<u64>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub error:Option<String>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub is_backgrounded:Option<bool>,
});
wire_struct!(TaskUpdatedMessage { pub task_id:String,pub patch:TaskPatch,pub uuid:String,#[serde(default="crate::absent::session_id")] pub session_id:String, });
wire_struct!(TaskProgressMessage {
    pub task_id:String,#[serde(default,skip_serializing_if="Option::is_none")] pub tool_use_id:Option<String>,pub description:String,
    #[serde(default,skip_serializing_if="Option::is_none")] pub subagent_type:Option<String>,pub usage:TaskUsage,
    #[serde(default,skip_serializing_if="Option::is_none")] pub last_tool_name:Option<String>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub summary:Option<String>,pub uuid:String,#[serde(default="crate::absent::session_id")] pub session_id:String,
});
wire_struct!(BackgroundTask { #[serde(default="crate::absent::task_id")] pub task_id:String,#[serde(default="crate::absent::task_type")] pub task_type:String,#[serde(default="crate::absent::description")] pub description:String,#[serde(default,skip_serializing_if="Option::is_none")] pub ambient:Option<bool>, });
wire_struct!(BackgroundTasksChangedMessage { #[serde(default="crate::absent::tasks",skip_serializing_if="Option::is_none")] pub tasks:Option<Vec<BackgroundTask>>,pub uuid:String,#[serde(default="crate::absent::session_id")] pub session_id:String, });
wire_struct!(ThinkingTokensMessage { pub estimated_tokens:u64,pub estimated_tokens_delta:u64,pub uuid:String,#[serde(default="crate::absent::session_id")] pub session_id:String, });
wire_struct!(CommandsChangedMessage { pub commands:Vec<crate::stream::init::SlashCommand>,pub uuid:String,#[serde(default="crate::absent::session_id")] pub session_id:String, });
wire_struct!(RateLimitInfo {
    pub status:String,
    #[serde(rename="resetsAt",default,skip_serializing_if="Option::is_none")] pub resets_at:Option<u64>,
    #[serde(rename="rateLimitType",default,skip_serializing_if="Option::is_none")] pub rate_limit_type:Option<String>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub utilization:Option<f64>,
    #[serde(rename="surpassedThreshold",default,skip_serializing_if="Option::is_none")] pub surpassed_threshold:Option<f64>,
    #[serde(rename="errorCode",default,skip_serializing_if="Option::is_none")] pub error_code:Option<String>,
    #[serde(rename="canUserPurchaseCredits",default,skip_serializing_if="Option::is_none")] pub can_user_purchase_credits:Option<bool>,
    #[serde(rename="hasChargeableSavedPaymentMethod",default,skip_serializing_if="Option::is_none")] pub has_chargeable_saved_payment_method:Option<bool>,
    /// Each window's use by its name (`five_hour`, `seven_day`, a model's).
    #[serde(rename="unifiedWindows",default,skip_serializing_if="Option::is_none")] pub unified_windows:Option<BTreeMap<String,RateLimitWindow>>,
});
wire_struct!(RateLimitWindow {
    #[serde(default,skip_serializing_if="Option::is_none")] pub utilization:Option<f64>,
    #[serde(rename="resetsAt",default,skip_serializing_if="Option::is_none")] pub resets_at:Option<i64>,
});
wire_struct!(RateLimitEvent { pub rate_limit_info:RateLimitInfo,pub uuid:String,#[serde(default="crate::absent::session_id")] pub session_id:String, });
wire_struct!(ElicitationCompleteMessage { pub mcp_server_name:String,pub elicitation_id:String,pub uuid:String,#[serde(default="crate::absent::session_id")] pub session_id:String, });
wire_struct!(PermissionDeniedMessage {
    pub tool_name:String,pub tool_use_id:String,
    #[serde(default,skip_serializing_if="Option::is_none")] pub agent_id:Option<String>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub decision_reason_type:Option<String>,
    #[serde(default,skip_serializing_if="Option::is_none")] pub decision_reason:Option<String>,
    pub message:String,pub uuid:String,#[serde(default="crate::absent::session_id")] pub session_id:String,
});
// Where a queued prompt is in its life, named by the sender's uuid.
wire_struct!(CommandLifecycleMessage { pub command_uuid:String,pub state:String,#[serde(default="crate::absent::uuid")] pub uuid:String,#[serde(default="crate::absent::session_id")] pub session_id:String, });
// The session's title, as set by the caller's `title` option or changed since.
wire_struct!(ConversationResetMessage { pub new_conversation_id:String,pub uuid:String,#[serde(default="crate::absent::session_id")] pub session_id:String, });

fn required_string<'a>(raw: &'a serde_json::Value, field: &str) -> Result<&'a str, ProtocolError> {
    let object = raw.as_object().ok_or_else(|| {
        ProtocolError::with_frame("Claude protocol frame must be a JSON object", raw.clone())
    })?;
    object.get(field).and_then(|v| v.as_str()).ok_or_else(|| {
        ProtocolError::with_frame(
            format!("Claude protocol frame requires string field `{field}`"),
            raw.clone(),
        )
    })
}
fn parse_known<T: DeserializeOwned>(
    mut raw: serde_json::Value,
    has_subtype: bool,
) -> Result<T, ProtocolError> {
    let original = raw.clone();
    if let Some(object) = raw.as_object_mut() {
        object.remove("type");
        if has_subtype {
            object.remove("subtype");
        }
    }
    serde_json::from_value(raw).map_err(|error| {
        ProtocolError::with_frame(format!("malformed known Claude frame: {error}"), original)
    })
}
fn required_option<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> Result<Option<T>, D::Error> {
    Option::<T>::deserialize(deserializer)
}
fn serialize_envelope<T: Serialize, S: Serializer>(
    payload: &T,
    kind: &str,
    subtype: Option<&str>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    let mut raw = serde_json::to_value(payload).map_err(serde::ser::Error::custom)?;
    let object = raw
        .as_object_mut()
        .ok_or_else(|| serde::ser::Error::custom("protocol payload must serialize as object"))?;
    object.insert("type".into(), kind.into());
    if let Some(subtype) = subtype {
        object.insert("subtype".into(), subtype.into());
    }
    raw.serialize(serializer)
}
