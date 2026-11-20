//! `session/update`: everything the agent streams about a session, from
//! message text to tool calls, plans, settings and usage.

use serde::{Deserialize, Serialize};
use serde_json::{Number, Value};

use crate::Extra;
use crate::content::ContentBlock;
use crate::macros::{string_enum, tagged_enum};
use crate::session::SessionConfigOption;

/// The params of `session/update`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionNotification {
    pub session_id: String,
    pub update: SessionUpdate,
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
    /// One thing that happened in a session. ACP's unstable updates
    /// (subagents, cursors, prompt suggestions, plan items) are not typed
    /// and read as `Unknown`.
    pub enum SessionUpdate by "sessionUpdate" {
        /// A chunk of the person's prompt, as `session/load` replays it.
        "user_message_chunk" => UserMessageChunk(ContentChunk),
        "agent_message_chunk" => AgentMessageChunk(ContentChunk),
        /// A chunk of the agent's reasoning.
        "agent_thought_chunk" => AgentThoughtChunk(ContentChunk),
        /// A tool call begins.
        "tool_call" => ToolCall(ToolCall),
        /// A tool call changes: only the fields it carries change.
        "tool_call_update" => ToolCallUpdate(ToolCallUpdate),
        /// The agent's whole plan, replacing any before it.
        "plan" => Plan(Plan),
        /// The slash commands the agent offers now.
        "available_commands_update" => AvailableCommandsUpdate(AvailableCommandsUpdate),
        "current_mode_update" => CurrentModeUpdate(CurrentModeUpdate),
        /// Every config option, after one changed.
        "config_option_update" => ConfigOptionUpdate(ConfigOptionUpdate),
        /// The session's title or time of last change.
        "session_info_update" => SessionInfoUpdate(SessionInfoUpdate),
        /// The context window's use, and what the session cost.
        "usage_update" => UsageUpdate(UsageUpdate),
        /// Something for the person that is not part of the conversation.
        "notice" => Notice(Notice),
        /// The context is being, or was, compacted.
        "compaction_update" => CompactionUpdate(CompactionUpdate),
        /// A piece of a compaction's summary.
        "compaction_summary_chunk" => CompactionSummaryChunk(CompactionSummaryChunk),
    }
}

/// A piece of a message or thought.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentChunk {
    pub content: ContentBlock,
    /// The message the chunk belongs to, where the agent says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
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

/// A tool call as it begins.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCall {
    pub tool_call_id: String,
    /// For people to read: what the call does.
    pub title: String,
    /// The tool's own name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<ToolKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<ToolCallStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<Vec<ToolCallContent>>,
    /// The files the call touches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locations: Option<Vec<ToolCallLocation>>,
    #[serde(
        default,
        deserialize_with = "crate::present",
        skip_serializing_if = "Option::is_none"
    )]
    pub raw_input: Option<Value>,
    #[serde(
        default,
        deserialize_with = "crate::present",
        skip_serializing_if = "Option::is_none"
    )]
    pub raw_output: Option<Value>,
    /// `rawInput` again, as Antigravity also writes it.
    #[serde(
        default,
        deserialize_with = "crate::present",
        skip_serializing_if = "Option::is_none"
    )]
    pub args: Option<Value>,
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

/// A change to a tool call, or the call a permission request is about:
/// only its id is always there.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallUpdate {
    pub tool_call_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<ToolKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<ToolCallStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Replaces the call's content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<Vec<ToolCallContent>>,
    /// Replaces the call's locations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locations: Option<Vec<ToolCallLocation>>,
    #[serde(
        default,
        deserialize_with = "crate::present",
        skip_serializing_if = "Option::is_none"
    )]
    pub raw_input: Option<Value>,
    #[serde(
        default,
        deserialize_with = "crate::present",
        skip_serializing_if = "Option::is_none"
    )]
    pub raw_output: Option<Value>,
    /// `rawInput` again, as Antigravity also writes it.
    #[serde(
        default,
        deserialize_with = "crate::present",
        skip_serializing_if = "Option::is_none"
    )]
    pub args: Option<Value>,
    /// The kind of update this was streamed as: Antigravity asks for
    /// permission with the update it streamed, tag and all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_update: Option<String>,
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
    /// What a tool does, so a client can show it apt to its kind.
    pub enum ToolKind {
        Read = "read",
        Edit = "edit",
        Delete = "delete",
        Move = "move",
        Search = "search",
        Execute = "execute",
        Think = "think",
        Fetch = "fetch",
        SwitchMode = "switch_mode",
        /// Any other: ACP's default.
        Generic = "other",
    }
}

string_enum! {
    pub enum ToolCallStatus {
        /// Not started: its input is still streaming, or it waits for
        /// approval.
        Pending = "pending",
        InProgress = "in_progress",
        Completed = "completed",
        Failed = "failed",
    }
}

tagged_enum! {
    /// Something a tool call shows.
    pub enum ToolCallContent by "type" {
        "content" => Content(Content),
        /// A change to a file.
        "diff" => Diff(Diff),
        /// A terminal the client runs for the agent.
        "terminal" => Terminal(Terminal),
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Content {
    pub content: ContentBlock,
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
pub struct Diff {
    pub path: String,
    /// None for a new file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_text: Option<String>,
    pub new_text: String,
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
pub struct Terminal {
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

/// A file a tool call touches, and the line, where it says.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallLocation {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
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
pub struct Plan {
    pub entries: Vec<PlanEntry>,
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
pub struct PlanEntry {
    pub content: String,
    pub priority: PlanEntryPriority,
    pub status: PlanEntryStatus,
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
    pub enum PlanEntryPriority {
        High = "high",
        Medium = "medium",
        Low = "low",
    }
}

string_enum! {
    pub enum PlanEntryStatus {
        Pending = "pending",
        InProgress = "in_progress",
        Completed = "completed",
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AvailableCommandsUpdate {
    pub available_commands: Vec<AvailableCommand>,
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

/// A slash command, named without its slash.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AvailableCommand {
    pub name: String,
    pub description: String,
    /// Grok writes null for a command that takes nothing.
    #[serde(
        default,
        deserialize_with = "crate::present_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub input: Option<Option<AvailableCommandInput>>,
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

/// What a command takes after its name: free text, with a hint of what.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AvailableCommandInput {
    pub hint: String,
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
pub struct CurrentModeUpdate {
    pub current_mode_id: String,
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
pub struct ConfigOptionUpdate {
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

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfoUpdate {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
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

/// Tokens in the context window, of how many it holds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageUpdate {
    pub used: u64,
    pub size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<Cost>,
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

/// What the session has cost so far.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Cost {
    pub amount: Number,
    /// ISO 4217, such as "USD".
    pub currency: String,
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
pub struct Notice {
    pub severity: NoticeSeverity,
    pub title: String,
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

string_enum! {
    pub enum NoticeSeverity {
        Info = "info",
        Warning = "warning",
        Error = "error",
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionUpdate {
    pub compaction_id: String,
    pub status: CompactionStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<Vec<ContentBlock>>,
    /// Why a compaction failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
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
    pub enum CompactionStatus {
        InProgress = "in_progress",
        Completed = "completed",
        Failed = "failed",
        Cancelled = "cancelled",
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionSummaryChunk {
    pub compaction_id: String,
    pub content: ContentBlock,
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
