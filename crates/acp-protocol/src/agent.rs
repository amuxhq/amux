//! What the agent sends: notifications, requests that wait for amux's
//! answer, and the answers to amux's requests.

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use crate::client::AgentResponse;
use crate::client::{
    CancelRequestNotification, CreateTerminalResponse, ReadTextFileResponse,
    RequestPermissionResponse, TerminalExitStatus, TerminalOutputResponse,
};
use crate::elicitation::{
    CompleteElicitationNotification, CreateElicitationRequest, CreateElicitationResponse,
};
use crate::macros::{method_enum, string_enum};
use crate::session::{
    AgentCapabilities, AuthMethod, EnvVariable, Implementation, SessionConfigOption, SessionInfo,
    SessionModeState, SessionModelState,
};
use crate::update::{SessionNotification, ToolCallUpdate};
use crate::{Empty, Extra};

method_enum! {
    pub enum AgentRequest answered by ClientResponse {
        /// Asks the person to approve a tool call, with the options to
        /// choose from.
        "session/request_permission" => RequestPermission(RequestPermissionRequest) -> RequestPermissionResponse,
        "elicitation/create" => ElicitationCreate(CreateElicitationRequest) -> CreateElicitationResponse,
        // A client offers files and terminals to the agent only when it
        // says so at `initialize`; amux does not, so these come only from
        // an agent that ignores that.
        "fs/read_text_file" => ReadTextFile(ReadTextFileRequest) -> ReadTextFileResponse,
        "terminal/create" => TerminalCreate(CreateTerminalRequest) -> CreateTerminalResponse,
        "terminal/output" => TerminalOutput(TerminalRequest) -> TerminalOutputResponse,
        // The answers that hold nothing come last, so an answer read
        // without its request is the first whose fields it fills.
        "fs/write_text_file" => WriteTextFile(WriteTextFileRequest) -> Empty,
        "terminal/release" => TerminalRelease(TerminalRequest) -> Empty,
        "terminal/kill" => TerminalKill(TerminalRequest) -> Empty,
        "terminal/wait_for_exit" => TerminalWaitForExit(TerminalRequest) -> TerminalExitStatus,
    }
}

method_enum! {
    pub enum AgentNotification {
        "session/update" => SessionUpdate(SessionNotification),
        /// The page of a `url` elicitation is done with.
        "elicitation/complete" => ElicitationComplete(CompleteElicitationNotification),
        /// The agent's sign-in changed. Still unstable in ACP.
        "auth/status_update" => AuthStatusUpdate(AuthStatusResponse),
        "$/cancel_request" => CancelRequest(CancelRequestNotification),
    }
}

impl AgentRequest {
    /// The session the request is about, when it names one.
    pub fn session_id(&self) -> Option<&str> {
        match self {
            Self::RequestPermission(request) => Some(&request.session_id),
            Self::ElicitationCreate(CreateElicitationRequest::Form(form)) => {
                form.session_id.as_deref()
            }
            Self::ElicitationCreate(CreateElicitationRequest::Url(page)) => {
                page.session_id.as_deref()
            }
            Self::ElicitationCreate(CreateElicitationRequest::Unknown(_)) => None,
            Self::ReadTextFile(request) => Some(&request.session_id),
            Self::WriteTextFile(request) => Some(&request.session_id),
            Self::TerminalCreate(request) => Some(&request.session_id),
            Self::TerminalOutput(request)
            | Self::TerminalRelease(request)
            | Self::TerminalKill(request)
            | Self::TerminalWaitForExit(request) => Some(&request.session_id),
        }
    }
}

impl AgentNotification {
    /// The session the notification is about, when it names one.
    pub fn session_id(&self) -> Option<&str> {
        match self {
            Self::SessionUpdate(notification) => Some(&notification.session_id),
            Self::ElicitationComplete(_) | Self::AuthStatusUpdate(_) | Self::CancelRequest(_) => {
                None
            }
        }
    }
}

/// The params of `session/request_permission`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestPermissionRequest {
    pub session_id: String,
    /// The call asking, with what the agent knows of it so far.
    pub tool_call: ToolCallUpdate,
    pub options: Vec<PermissionOption>,
    #[serde(
        rename = "_meta",
        default,
        deserialize_with = "crate::present",
        skip_serializing_if = "Option::is_none"
    )]
    pub meta: Option<Value>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// A choice a permission request offers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionOption {
    /// What the answer names.
    pub option_id: String,
    /// For people to read.
    pub name: String,
    pub kind: PermissionOptionKind,
    #[serde(
        rename = "_meta",
        default,
        deserialize_with = "crate::present",
        skip_serializing_if = "Option::is_none"
    )]
    pub meta: Option<Value>,
    #[serde(flatten)]
    pub extra: Extra,
}

string_enum! {
    /// What a choice does, so a client can show and place it.
    pub enum PermissionOptionKind {
        AllowOnce = "allow_once",
        /// Allow, and let the agent remember it.
        AllowAlways = "allow_always",
        RejectOnce = "reject_once",
        RejectAlways = "reject_always",
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadTextFileRequest {
    pub session_id: String,
    pub path: String,
    /// The first line to read, from 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(
        rename = "_meta",
        default,
        deserialize_with = "crate::present",
        skip_serializing_if = "Option::is_none"
    )]
    pub meta: Option<Value>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteTextFileRequest {
    pub session_id: String,
    pub path: String,
    pub content: String,
    #[serde(
        rename = "_meta",
        default,
        deserialize_with = "crate::present",
        skip_serializing_if = "Option::is_none"
    )]
    pub meta: Option<Value>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateTerminalRequest {
    pub session_id: String,
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<Vec<EnvVariable>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_byte_limit: Option<u64>,
    #[serde(
        rename = "_meta",
        default,
        deserialize_with = "crate::present",
        skip_serializing_if = "Option::is_none"
    )]
    pub meta: Option<Value>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// The params of every request about one terminal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalRequest {
    pub session_id: String,
    pub terminal_id: String,
    #[serde(
        rename = "_meta",
        default,
        deserialize_with = "crate::present",
        skip_serializing_if = "Option::is_none"
    )]
    pub meta: Option<Value>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// The answer to `initialize`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResponse {
    /// The version both sides speak: the client's, or an older one the
    /// agent offers instead.
    pub protocol_version: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_capabilities: Option<AgentCapabilities>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_methods: Option<Vec<AuthMethod>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_info: Option<Implementation>,
    #[serde(
        rename = "_meta",
        default,
        deserialize_with = "crate::present",
        skip_serializing_if = "Option::is_none"
    )]
    pub meta: Option<Value>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// The answer to `session/new` and `session/fork`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewSessionResponse {
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modes: Option<SessionModeState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub models: Option<SessionModelState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_options: Option<Vec<SessionConfigOption>>,
    #[serde(
        rename = "_meta",
        default,
        deserialize_with = "crate::present",
        skip_serializing_if = "Option::is_none"
    )]
    pub meta: Option<Value>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// The answer to `session/load`, `session/resume` and `session/options`:
/// the session's settings.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSetup {
    /// The session answered for, as Antigravity echoes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modes: Option<SessionModeState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub models: Option<SessionModelState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_options: Option<Vec<SessionConfigOption>>,
    #[serde(
        rename = "_meta",
        default,
        deserialize_with = "crate::present",
        skip_serializing_if = "Option::is_none"
    )]
    pub meta: Option<Value>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListSessionsResponse {
    pub sessions: Vec<SessionInfo>,
    /// Where the next page starts, when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    #[serde(
        rename = "_meta",
        default,
        deserialize_with = "crate::present",
        skip_serializing_if = "Option::is_none"
    )]
    pub meta: Option<Value>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// Every config option, after one was set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetSessionConfigOptionResponse {
    pub config_options: Vec<SessionConfigOption>,
    #[serde(
        rename = "_meta",
        default,
        deserialize_with = "crate::present",
        skip_serializing_if = "Option::is_none"
    )]
    pub meta: Option<Value>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// The answer to `session/prompt`: the turn is over.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptResponse {
    pub stop_reason: StopReason,
    /// The turn's tokens. Still unstable in ACP.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    #[serde(
        rename = "_meta",
        default,
        deserialize_with = "crate::present",
        skip_serializing_if = "Option::is_none"
    )]
    pub meta: Option<Value>,
    #[serde(flatten)]
    pub extra: Extra,
}

string_enum! {
    /// Why a turn ended.
    pub enum StopReason {
        EndTurn = "end_turn",
        /// The model's token limit.
        MaxTokens = "max_tokens",
        /// The limit on model requests in one turn.
        MaxTurnRequests = "max_turn_requests",
        Refusal = "refusal",
        /// amux sent `session/cancel`.
        Cancelled = "cancelled",
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    pub total_tokens: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thought_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_read_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_write_tokens: Option<u64>,
    #[serde(
        rename = "_meta",
        default,
        deserialize_with = "crate::present",
        skip_serializing_if = "Option::is_none"
    )]
    pub meta: Option<Value>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// The answer to `auth/status`, and the params of `auth/status_update`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthStatusResponse {
    pub state: AuthState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accounts: Option<Vec<AuthAccount>>,
    #[serde(
        rename = "_meta",
        default,
        deserialize_with = "crate::present",
        skip_serializing_if = "Option::is_none"
    )]
    pub meta: Option<Value>,
    #[serde(flatten)]
    pub extra: Extra,
}

string_enum! {
    pub enum AuthState {
        /// Credentials are there; they may still be refused.
        Authenticated = "authenticated",
        Unauthenticated = "unauthenticated",
        NotRequired = "not_required",
        Unknown = "unknown",
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthAccount {
    /// For people to read: an email, an organization.
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organization: Option<String>,
    #[serde(
        rename = "_meta",
        default,
        deserialize_with = "crate::present",
        skip_serializing_if = "Option::is_none"
    )]
    pub meta: Option<Value>,
    #[serde(flatten)]
    pub extra: Extra,
}
