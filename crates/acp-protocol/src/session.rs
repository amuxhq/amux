//! What a session is set up with and offers: the capabilities each side
//! advertises at `initialize`, sign-in methods, MCP servers, modes, models
//! and config options, and a listed session.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::content::ContentBlock;
use crate::macros::{string_enum, tagged_enum};
use crate::{Empty, Extra};

/// A program's name and version, as each side tells the other at
/// `initialize`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Implementation {
    pub name: String,
    /// For people to read; `name` when absent.
    #[serde(
        default,
        deserialize_with = "crate::present_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub title: Option<Option<String>>,
    pub version: String,
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

/// What amux offers the agent. amux runs no files or terminals for the
/// agent, so it leaves `fs` and `terminal` out.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientCapabilities {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fs: Option<FileSystemCapabilities>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal: Option<bool>,
    #[serde(
        default,
        deserialize_with = "crate::present_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub session: Option<Option<ClientSessionCapabilities>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<AuthCapabilities>,
    #[serde(
        default,
        deserialize_with = "crate::present_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub elicitation: Option<Option<ElicitationCapabilities>>,
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
pub struct FileSystemCapabilities {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_text_file: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub write_text_file: Option<bool>,
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

/// The session updates the client can show beyond the base set.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientSessionCapabilities {
    #[serde(
        default,
        deserialize_with = "crate::present_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub compaction: Option<Option<Empty>>,
    #[serde(
        default,
        deserialize_with = "crate::present_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub config_options: Option<Option<SessionConfigOptionsCapabilities>>,
    #[serde(
        default,
        deserialize_with = "crate::present_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub notices: Option<Option<Empty>>,
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

/// The kinds of config option the client can show beyond selects.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionConfigOptionsCapabilities {
    #[serde(
        default,
        deserialize_with = "crate::present_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub boolean: Option<Option<Empty>>,
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

/// Whether the client can run a sign-in method of the `terminal` type.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthCapabilities {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal: Option<bool>,
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

/// The kinds of `elicitation/create` the client answers.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ElicitationCapabilities {
    #[serde(
        default,
        deserialize_with = "crate::present_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub form: Option<Option<Empty>>,
    #[serde(
        default,
        deserialize_with = "crate::present_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub url: Option<Option<Empty>>,
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

/// What the agent supports, from its `initialize` answer. A method amux
/// may call only when it is advertised here.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentCapabilities {
    /// Whether `session/load` is offered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub load_session: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_capabilities: Option<PromptCapabilities>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_capabilities: Option<McpCapabilities>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_capabilities: Option<SessionCapabilities>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<AgentAuthCapabilities>,
    /// `loadSession` again, as Antigravity also writes it.
    #[serde(
        rename = "load_session",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub load_session_again: Option<bool>,
    /// `sessionCapabilities` again, as Antigravity also writes it.
    #[serde(
        rename = "session_capabilities",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub session_capabilities_again: Option<SessionCapabilities>,
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

/// The content a prompt may hold beyond text and resource links.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptCapabilities {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<bool>,
    /// Whether a prompt may embed a resource's contents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedded_context: Option<bool>,
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

/// The MCP transports the agent connects to beyond stdio.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpCapabilities {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sse: Option<bool>,
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

/// The session methods the agent offers beyond the base set, each present
/// when offered. `fork`, `options`, `status`, `archive` and `title` are
/// still unstable in ACP.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionCapabilities {
    /// `session/load`, which ACP advertises as `loadSession`; Antigravity
    /// lists it here too.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub load: Option<Empty>,
    #[serde(
        default,
        deserialize_with = "crate::present_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub list: Option<Option<Empty>>,
    #[serde(
        default,
        deserialize_with = "crate::present_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub delete: Option<Option<Empty>>,
    #[serde(
        default,
        deserialize_with = "crate::present_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub additional_directories: Option<Option<Empty>>,
    #[serde(
        default,
        deserialize_with = "crate::present_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub system_prompt: Option<Option<SessionSystemPromptCapabilities>>,
    #[serde(
        default,
        deserialize_with = "crate::present_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub resume: Option<Option<Empty>>,
    #[serde(
        default,
        deserialize_with = "crate::present_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub close: Option<Option<Empty>>,
    #[serde(
        default,
        deserialize_with = "crate::present_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub fork: Option<Option<Empty>>,
    #[serde(
        default,
        deserialize_with = "crate::present_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub options: Option<Option<Empty>>,
    #[serde(
        default,
        deserialize_with = "crate::present_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub status: Option<Option<Empty>>,
    #[serde(
        default,
        deserialize_with = "crate::present_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub archive: Option<Option<Empty>>,
    #[serde(
        default,
        deserialize_with = "crate::present_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub title: Option<Option<Empty>>,
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

/// A system prompt the client may append; with `mode.override`, one that
/// replaces the agent's own.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSystemPromptCapabilities {
    #[serde(
        default,
        deserialize_with = "crate::present_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub mode: Option<Option<SessionSystemPromptModeCapabilities>>,
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
pub struct SessionSystemPromptModeCapabilities {
    #[serde(
        rename = "override",
        default,
        deserialize_with = "crate::present_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub replace: Option<Option<Empty>>,
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

/// The account methods the agent offers beyond `authenticate`. `status`
/// (`auth/status`) is still unstable in ACP.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentAuthCapabilities {
    #[serde(
        default,
        deserialize_with = "crate::present_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub logout: Option<Option<Empty>>,
    #[serde(
        default,
        deserialize_with = "crate::present_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub status: Option<Option<Empty>>,
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

tagged_enum! {
    /// A way to sign in to the agent, as `initialize` lists them.
    pub enum AuthMethod by "type" {
        /// The client runs the agent's own program in a terminal to sign
        /// in.
        "terminal" => Terminal(AuthMethodTerminal),
    }
    /// The agent signs in itself when asked with `authenticate`; its
    /// description often says what a person should run instead.
    without Agent(AuthMethodAgent)
}

impl AuthMethod {
    /// The id `authenticate` names the method by.
    pub fn id(&self) -> Option<&str> {
        match self {
            Self::Terminal(method) => Some(&method.id),
            Self::Agent(method) => Some(&method.id),
            Self::Unknown(_) => None,
        }
    }

    /// What a person is told to do to sign in.
    pub fn description(&self) -> Option<&str> {
        match self {
            Self::Terminal(method) => method.description.as_ref()?.as_deref(),
            Self::Agent(method) => method.description.as_ref()?.as_deref(),
            Self::Unknown(_) => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthMethodAgent {
    pub id: String,
    pub name: String,
    #[serde(
        default,
        deserialize_with = "crate::present_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub description: Option<Option<String>>,
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
pub struct AuthMethodTerminal {
    pub id: String,
    pub name: String,
    #[serde(
        default,
        deserialize_with = "crate::present_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub description: Option<Option<String>>,
    /// Arguments to the agent's own command that run the sign-in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<BTreeMap<String, String>>,
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

tagged_enum! {
    /// An MCP server for the agent to connect to in a session.
    pub enum McpServer by "type" {
        "http" => Http(McpServerHttp),
        "sse" => Sse(McpServerSse),
    }
    /// A server the agent starts itself, as a command speaking MCP over
    /// stdio: written with no type.
    without Stdio(McpServerStdio)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerStdio {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: Vec<EnvVariable>,
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
pub struct McpServerHttp {
    pub name: String,
    pub url: String,
    pub headers: Vec<HttpHeader>,
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
pub struct McpServerSse {
    pub name: String,
    pub url: String,
    pub headers: Vec<HttpHeader>,
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
pub struct EnvVariable {
    pub name: String,
    pub value: String,
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
pub struct HttpHeader {
    pub name: String,
    pub value: String,
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

/// Instructions the client gives the agent for a session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemPrompt {
    pub mode: SystemPromptMode,
    pub content: Vec<ContentBlock>,
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
    pub enum SystemPromptMode {
        /// After the agent's own instructions.
        Append = "append",
        /// Instead of them, when the agent offers that.
        Override = "override",
    }
}

/// The session's modes and the one it is in.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionModeState {
    pub current_mode_id: String,
    pub available_modes: Vec<SessionMode>,
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
pub struct SessionMode {
    pub id: String,
    pub name: String,
    #[serde(
        default,
        deserialize_with = "crate::present_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub description: Option<Option<String>>,
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

/// The session's models and the one it runs on. ACP's schema dropped this
/// for config options of the `model` category, but agents still send it
/// beside them, and some send only this.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionModelState {
    pub current_model_id: String,
    pub available_models: Vec<ModelInfo>,
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
pub struct ModelInfo {
    pub model_id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
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

tagged_enum! {
    /// One of a session's settings: a model, thought level, mode or any
    /// other the agent offers, with its current value.
    pub enum SessionConfigOption by "type" {
        /// A choice of one value from a list.
        "select" => Select(SessionConfigSelect),
        /// On or off.
        "boolean" => Boolean(SessionConfigBoolean),
    }
}

impl SessionConfigOption {
    /// The id `session/set_config_option` names the option by.
    pub fn id(&self) -> Option<&str> {
        match self {
            Self::Select(option) => Some(&option.id),
            Self::Boolean(option) => Some(&option.id),
            Self::Unknown(_) => None,
        }
    }

    /// What the option sets, when the agent says.
    pub fn category(&self) -> Option<&SessionConfigOptionCategory> {
        match self {
            Self::Select(option) => option.category.as_ref()?.as_ref(),
            Self::Boolean(option) => option.category.as_ref()?.as_ref(),
            Self::Unknown(_) => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionConfigSelect {
    pub id: String,
    pub name: String,
    #[serde(
        default,
        deserialize_with = "crate::present_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub description: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "crate::present_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub category: Option<Option<SessionConfigOptionCategory>>,
    /// The value of the option chosen now.
    pub current_value: String,
    pub options: SessionConfigSelectOptions,
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
pub struct SessionConfigBoolean {
    pub id: String,
    pub name: String,
    #[serde(
        default,
        deserialize_with = "crate::present_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub description: Option<Option<String>>,
    #[serde(
        default,
        deserialize_with = "crate::present_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub category: Option<Option<SessionConfigOptionCategory>>,
    pub current_value: bool,
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
    /// What a config option sets, so a client can show it in its place.
    pub enum SessionConfigOptionCategory {
        Mode = "mode",
        Model = "model",
        /// A setting of the model other than which model it is.
        ModelConfig = "model_config",
        /// How hard the model thinks: amux's effort.
        ThoughtLevel = "thought_level",
        /// Still unstable in ACP.
        Speed = "speed",
    }
}

/// A select's values, in a list or in named groups.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SessionConfigSelectOptions {
    Ungrouped(Vec<SessionConfigSelectOption>),
    Grouped(Vec<SessionConfigSelectGroup>),
}

impl SessionConfigSelectOptions {
    /// Every value, groups flattened in order.
    pub fn values(&self) -> Vec<&SessionConfigSelectOption> {
        match self {
            Self::Ungrouped(options) => options.iter().collect(),
            Self::Grouped(groups) => groups.iter().flat_map(|group| &group.options).collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionConfigSelectOption {
    pub value: String,
    pub name: String,
    #[serde(
        default,
        deserialize_with = "crate::present_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub description: Option<Option<String>>,
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
pub struct SessionConfigSelectGroup {
    pub group: String,
    pub name: String,
    pub options: Vec<SessionConfigSelectOption>,
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

/// A session `session/list` names.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    pub session_id: String,
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_directories: Option<Vec<String>>,
    #[serde(
        default,
        deserialize_with = "crate::present_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub title: Option<Option<String>>,
    /// When it last changed, in ISO 8601.
    #[serde(
        default,
        deserialize_with = "crate::present_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub updated_at: Option<Option<String>>,
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
