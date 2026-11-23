//! What amux sends the agent: requests, notifications, and answers to the
//! agent's requests.
//!
//! An optional field amux leaves out when it has nothing is skipped when
//! `None`, so a line written holds only what amux set.

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use crate::agent::ClientResponse;
use crate::agent::{
    AuthStatusResponse, InitializeResponse, ListSessionsResponse, NewSessionResponse,
    PromptResponse, SessionSetup, SetSessionConfigOptionResponse,
};
use crate::content::ContentBlock;
use crate::macros::{method_enum, string_enum, tagged_enum};
use crate::session::{ClientCapabilities, Implementation, McpServer, SystemPrompt};
use crate::{Empty, Extra, RequestId};

/// The protocol version this crate speaks.
pub const PROTOCOL_VERSION: u16 = 1;

method_enum! {
    pub enum ClientRequest answered by AgentResponse {
        "initialize" => Initialize(InitializeRequest) -> InitializeResponse,
        "authenticate" => Authenticate(AuthenticateRequest) -> Empty,
        "logout" => Logout(Empty) -> Empty,
        "session/new" => SessionNew(NewSessionRequest) -> NewSessionResponse,
        "session/load" => SessionLoad(LoadSessionRequest) -> SessionSetup,
        "session/resume" => SessionResume(ResumeSessionRequest) -> SessionSetup,
        "session/close" => SessionClose(SessionRequest) -> Empty,
        "session/list" => SessionList(ListSessionsRequest) -> ListSessionsResponse,
        "session/delete" => SessionDelete(SessionRequest) -> Empty,
        "session/prompt" => SessionPrompt(PromptRequest) -> PromptResponse,
        "session/set_mode" => SessionSetMode(SetSessionModeRequest) -> Empty,
        "session/set_config_option" => SessionSetConfigOption(SetSessionConfigOptionRequest) -> SetSessionConfigOptionResponse,
        // Still unstable in ACP, each called only where the agent
        // advertises it.
        "session/options" => SessionOptions(SessionOptionsRequest) -> SessionSetup,
        "session/fork" => SessionFork(ForkSessionRequest) -> NewSessionResponse,
        "auth/status" => AuthStatus(Empty) -> AuthStatusResponse,
    }
}

method_enum! {
    pub enum ClientNotification {
        /// Stops the session's running turn; its `session/prompt` answers
        /// `cancelled`.
        "session/cancel" => SessionCancel(SessionRequest),
        "$/cancel_request" => CancelRequest(CancelRequestNotification),
    }
}

/// The params of `initialize`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeRequest {
    /// The newest version the client speaks.
    pub protocol_version: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_capabilities: Option<ClientCapabilities>,
    #[serde(
        default,
        deserialize_with = "crate::present_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub client_info: Option<Option<Implementation>>,
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
pub struct AuthenticateRequest {
    /// One of the `authMethods` the agent listed.
    pub method_id: String,
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

/// The params of `session/new`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewSessionRequest {
    /// The session's folder, as an absolute path.
    pub cwd: String,
    /// More folders the session may work in, where the agent offers it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_directories: Option<Vec<String>>,
    pub mcp_servers: Vec<McpServer>,
    #[serde(
        default,
        deserialize_with = "crate::present_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub system_prompt: Option<Option<SystemPrompt>>,
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

/// The params of `session/load`: the agent replays the session's history
/// as `session/update`s before it answers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadSessionRequest {
    pub mcp_servers: Vec<McpServer>,
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_directories: Option<Vec<String>>,
    pub session_id: String,
    #[serde(
        default,
        deserialize_with = "crate::present_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub system_prompt: Option<Option<SystemPrompt>>,
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

/// The params of `session/resume`: the session goes on without its
/// history replayed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResumeSessionRequest {
    pub session_id: String,
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_directories: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_servers: Option<Vec<McpServer>>,
    #[serde(
        default,
        deserialize_with = "crate::present_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub system_prompt: Option<Option<SystemPrompt>>,
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

/// The params of every request and notification that names only a
/// session: `session/close`, `session/delete`, `session/cancel`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionRequest {
    pub session_id: String,
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

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListSessionsRequest {
    /// Only the sessions of this folder.
    #[serde(
        default,
        deserialize_with = "crate::present_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub cwd: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "crate::present_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub cursor: Option<Option<String>>,
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

/// The params of `session/prompt`, which the agent answers when the turn
/// ends.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptRequest {
    pub session_id: String,
    pub prompt: Vec<ContentBlock>,
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
pub struct SetSessionModeRequest {
    pub session_id: String,
    pub mode_id: String,
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

/// The params of `session/set_config_option`. A select's value is one of
/// its options' values; a boolean's is written with `type: "boolean"`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetSessionConfigOptionRequest {
    pub session_id: String,
    pub config_id: String,
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<ConfigValueKind>,
    pub value: ConfigValue,
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

impl SetSessionConfigOptionRequest {
    /// Sets a select option to one of its values.
    pub fn select(session_id: &str, config_id: &str, value: &str) -> Self {
        Self {
            session_id: session_id.to_owned(),
            config_id: config_id.to_owned(),
            kind: None,
            value: ConfigValue::Id(value.to_owned()),
            meta: None,
            extra: Extra::new(),
        }
    }

    /// Turns a boolean option on or off.
    pub fn boolean(session_id: &str, config_id: &str, value: bool) -> Self {
        Self {
            session_id: session_id.to_owned(),
            config_id: config_id.to_owned(),
            kind: Some(ConfigValueKind::Boolean),
            value: ConfigValue::Boolean(value),
            meta: None,
            extra: Extra::new(),
        }
    }
}

string_enum! {
    pub enum ConfigValueKind {
        Boolean = "boolean",
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ConfigValue {
    Boolean(bool),
    /// A select option's value.
    Id(String),
}

/// The params of `session/options`: the config options a session would
/// start with, without starting one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionOptionsRequest {
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_directories: Option<Vec<String>>,
    /// The session whose options to resolve; a new one's when absent.
    #[serde(
        default,
        deserialize_with = "crate::present_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub session_id: Option<Option<String>>,
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

/// The params of `session/fork`: a new session from a copy of another's
/// history.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForkSessionRequest {
    pub session_id: String,
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_directories: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_servers: Option<Vec<McpServer>>,
    #[serde(
        default,
        deserialize_with = "crate::present_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub system_prompt: Option<Option<SystemPrompt>>,
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

/// Cancels one request this side sent; the other answers it, with
/// `-32800` if it stopped.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelRequestNotification {
    pub request_id: RequestId,
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

/// The answer to `session/request_permission`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestPermissionResponse {
    pub outcome: RequestPermissionOutcome,
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

impl RequestPermissionResponse {
    /// The option of the request's that the person chose.
    pub fn selected(option_id: &str) -> Self {
        Self {
            outcome: RequestPermissionOutcome::Selected(SelectedPermissionOutcome {
                option_id: option_id.to_owned(),
                meta: None,
                extra: Extra::new(),
            }),
            meta: None,
            extra: Extra::new(),
        }
    }

    /// The turn ended before anyone chose: the only answer once amux sent
    /// `session/cancel`.
    pub fn cancelled() -> Self {
        Self {
            outcome: RequestPermissionOutcome::Cancelled(Empty::default()),
            meta: None,
            extra: Extra::new(),
        }
    }
}

tagged_enum! {
    pub enum RequestPermissionOutcome by "outcome" {
        "cancelled" => Cancelled(Empty),
        "selected" => Selected(SelectedPermissionOutcome),
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectedPermissionOutcome {
    pub option_id: String,
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
pub struct ReadTextFileResponse {
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
pub struct CreateTerminalResponse {
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalOutputResponse {
    pub output: String,
    /// Whether the output was cut to the request's byte limit.
    pub truncated: bool,
    /// Present once the command has exited.
    #[serde(
        default,
        deserialize_with = "crate::present_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub exit_status: Option<Option<TerminalExitStatus>>,
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

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalExitStatus {
    #[serde(
        default,
        deserialize_with = "crate::present_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub exit_code: Option<Option<u32>>,
    #[serde(
        default,
        deserialize_with = "crate::present_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub signal: Option<Option<String>>,
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
