use serde::{Deserialize, Serialize};

use crate::Drift;
use crate::stream::init::AppliedSettings;
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
        "set_model" => SetModel(SetModelRequest),
        "apply_flag_settings" => ApplyFlagSettings(ApplyFlagSettingsRequest),
        "get_settings" => GetSettings(Extensions),
        "stop_task" => StopTask(StopTaskRequest),
        // What Claude asks of amux.
        "can_use_tool" => CanUseTool(CanUseToolRequest),
        "elicitation" => Elicitation(ElicitationRequestBody),
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeRequestBody {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_task_stop_affordance: Option<bool>,
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
pub struct StopTaskRequest {
    pub task_id: String,
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
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_suggestions: Option<Vec<PermissionUpdate>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_reason: Option<String>,
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

/// The person's answer to an elicitation.
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

/// The answer to `get_settings`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SettingsResult {
    pub applied: AppliedSettings,
    #[serde(flatten)]
    pub extensions: Extensions,
}
