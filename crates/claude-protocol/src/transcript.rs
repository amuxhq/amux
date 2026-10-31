//! The transcript file terminal Claude writes: one JSON row per line,
//! appended as the conversation goes.
//!
//! Rows are told apart by `type`. The conversation itself is in user,
//! assistant, system and attachment rows; the rest are bookkeeping Claude
//! keeps for its own interface (titles, modes, file history, its input
//! queue). Unknown row types, system subtypes and attachment types are kept
//! whole and refused by [`strict`]; unknown fields of a known row are kept
//! in its `extensions`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::stream::{
    ApiMessage, CompactTrigger, Extensions, MessageContent, MessageParam, PermissionMode,
};
use crate::strictness::tagged_enum;
use crate::{DecodeError, Drift, decoding};

tagged_enum! {
    /// One row of the transcript.
    #[allow(clippy::large_enum_variant)]
    pub enum Row by "type" {
        "user" => User(UserRow),
        "assistant" => Assistant(AssistantRow),
        "system" => System(SystemRow),
        "attachment" => Attachment(AttachmentRow),
        "mode" => Mode(ModeRow),
        "permission-mode" => PermissionMode(PermissionModeRow),
        "atis-latch" => AtisLatch(AtisLatchRow),
        "ai-title" => AiTitle(AiTitleRow),
        "agent-name" => AgentName(AgentNameRow),
        "last-prompt" => LastPrompt(LastPromptRow),
        "queue-operation" => QueueOperation(QueueOperationRow),
        "file-history-snapshot" => FileHistorySnapshot(FileHistorySnapshotRow),
        "file-history-delta" => FileHistoryDelta(FileHistoryDeltaRow),
        "tools-used" => ToolsUsed(ToolsUsedRow),
    }
}

impl Row {
    /// Where the row sits in the conversation, for the rows that are part
    /// of it.
    pub fn envelope(&self) -> Option<&Envelope> {
        match self {
            Self::User(row) => Some(&row.envelope),
            Self::Assistant(row) => Some(&row.envelope),
            Self::System(row) => Some(row.envelope()?),
            Self::Attachment(row) => Some(&row.envelope),
            _ => None,
        }
    }

    /// When Claude wrote the row, for the rows that say.
    pub fn timestamp(&self) -> Option<&str> {
        match self {
            Self::QueueOperation(row) => Some(&row.timestamp),
            Self::FileHistoryDelta(row) => Some(&row.timestamp),
            Self::System(SystemRow::Unknown(raw)) | Self::Unknown(raw) => {
                raw.field("timestamp")?.as_str()
            }
            _ => self.envelope().map(|envelope| envelope.timestamp.as_str()),
        }
    }

    /// The Claude session the row belongs to, when it names one.
    pub fn session_id(&self) -> Option<&str> {
        match self {
            Self::Mode(row) => Some(&row.session_id),
            Self::PermissionMode(row) => Some(&row.session_id),
            Self::AtisLatch(row) => Some(&row.session_id),
            Self::AiTitle(row) => Some(&row.session_id),
            Self::AgentName(row) => Some(&row.session_id),
            Self::LastPrompt(row) => Some(&row.session_id),
            Self::QueueOperation(row) => Some(&row.session_id),
            Self::System(SystemRow::Unknown(raw)) | Self::Unknown(raw) => {
                raw.field("sessionId")?.as_str()
            }
            _ => self.envelope().map(|envelope| envelope.session_id.as_str()),
        }
    }

    /// The version of Claude that wrote the row, for the rows that say.
    pub fn version(&self) -> Option<&str> {
        match self {
            Self::System(SystemRow::Unknown(raw)) | Self::Unknown(raw) => {
                raw.field("version")?.as_str()
            }
            _ => self.envelope().map(|envelope| envelope.version.as_str()),
        }
    }
}

/// Decodes one row. Fails only when the line is not a JSON object.
pub fn decode(line: &[u8]) -> Result<Row, DecodeError> {
    let object = decoding::object(line)?;
    Ok(Row::deserialize(object).expect("a row decodes from any object"))
}

/// Decodes one row, failing on anything [`decode`] would keep without
/// knowing it.
pub fn strict(line: &[u8]) -> Result<Row, Drift> {
    decoding::strict(line, "type", |object| {
        Row::deserialize(object).expect("a row decodes from any object")
    })
}

/// The fields every row of the conversation carries.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Envelope {
    #[serde(default = "crate::absent::uuid")]
    pub uuid: String,
    /// The row this one follows; null at the start of a conversation.
    #[serde(default = "crate::absent::parent_uuid")]
    pub parent_uuid: Option<String>,
    #[serde(default = "crate::absent::row_session_id")]
    pub session_id: String,
    #[serde(default = "crate::absent::timestamp")]
    pub timestamp: String,
    /// Claude's version.
    #[serde(default = "crate::absent::version")]
    pub version: String,
    #[serde(default = "crate::absent::cwd")]
    pub cwd: PathBuf,
    /// The session's plan-file name, once it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
}

/// A person's prompt, tool results, or text Claude adds on the person's
/// side of the conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserRow {
    #[serde(flatten)]
    pub envelope: Envelope,
    pub message: MessageParam,
    #[serde(default = "crate::absent::prompt_id")]
    pub prompt_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<PermissionMode>,
    /// Who the prompt came from; absent on tool results and on a slash
    /// command as typed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<Origin>,
    /// How the prompt reached Claude: typed, queued, or Claude's own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_source: Option<String>,
    /// What the tool returned, in Claude's own shape for each tool.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_use_result: Option<Value>,
    /// Text Claude adds for the model, never shown as a prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_meta: Option<bool>,
    /// The summary that replaces the conversation after a compaction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_compact_summary: Option<bool>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

/// Who a prompt came from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Origin {
    /// `human`, or `task-notification` for Claude's own report of a
    /// background job.
    pub kind: String,
    #[serde(flatten)]
    pub extensions: Extensions,
}

/// One block of an assistant message as Claude received it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantRow {
    #[serde(flatten)]
    pub envelope: Envelope,
    pub message: ApiMessage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// A message Claude wrote itself to report a failed request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_api_error_message: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

tagged_enum! {
    /// A row of Claude's own about the conversation, told apart by
    /// `subtype`.
    pub enum SystemRow by "subtype" {
        "turn_duration" => TurnDuration(TurnDuration),
        "stop_hook_summary" => StopHookSummary(StopHookSummary),
        "compact_boundary" => CompactBoundary(CompactBoundary),
        "local_command" => LocalCommand(LocalCommand),
    }
}

impl SystemRow {
    pub fn envelope(&self) -> Option<&Envelope> {
        match self {
            Self::TurnDuration(row) => Some(&row.envelope),
            Self::StopHookSummary(row) => Some(&row.envelope),
            Self::CompactBoundary(row) => Some(&row.envelope),
            Self::LocalCommand(row) => Some(&row.envelope),
            Self::Unknown(_) => None,
        }
    }
}

/// How long a turn took, written when it ends.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnDuration {
    #[serde(flatten)]
    pub envelope: Envelope,
    #[serde(default = "crate::absent::duration_ms")]
    pub duration_ms: Option<u64>,
    #[serde(default = "crate::absent::is_meta")]
    pub is_meta: bool,
    #[serde(flatten)]
    pub extensions: Extensions,
}

/// What the stop hooks did at the end of a turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StopHookSummary {
    #[serde(flatten)]
    pub envelope: Envelope,
    #[serde(flatten)]
    pub extensions: Extensions,
}

/// Where Claude compacted the conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactBoundary {
    #[serde(flatten)]
    pub envelope: Envelope,
    #[serde(default = "crate::absent::content")]
    pub content: String,
    #[serde(default = "crate::absent::level")]
    pub level: String,
    #[serde(default = "crate::absent::compact_metadata")]
    pub compact_metadata: CompactMetadata,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactMetadata {
    /// Whether Claude compacted on its own or was asked to.
    #[serde(default = "crate::absent::trigger")]
    pub trigger: CompactTrigger,
    /// Tokens in the context before and after.
    #[serde(default = "crate::absent::pre_tokens")]
    pub pre_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

/// A slash command Claude ran itself, or its output, in the same
/// `<command-name>` and `<local-command-stdout>` wording a user row uses.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalCommand {
    #[serde(flatten)]
    pub envelope: Envelope,
    #[serde(default = "crate::absent::content")]
    pub content: String,
    #[serde(default = "crate::absent::level")]
    pub level: String,
    #[serde(default = "crate::absent::is_meta")]
    pub is_meta: bool,
    #[serde(flatten)]
    pub extensions: Extensions,
}

/// Something Claude attached to the conversation for the model: context,
/// reminders, a queued prompt.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentRow {
    #[serde(flatten)]
    pub envelope: Envelope,
    pub attachment: Attachment,
    /// The text the model was given for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rendered: Option<Vec<Value>>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

tagged_enum! {
    /// What an attachment row carries, told apart by `type`. Most are
    /// context for the model that only Claude reads; their fields are kept
    /// as written.
    pub enum Attachment by "type" {
        "queued_command" => QueuedCommand(QueuedCommand),
        "plan_mode" => PlanMode(Extensions),
        "plan_mode_exit" => PlanModeExit(Extensions),
        "auto_mode" => AutoMode(Extensions),
        "total_tokens_reminder" => TotalTokensReminder(Extensions),
        "deferred_tools_delta" => DeferredToolsDelta(Extensions),
        "deferred_tools_record" => DeferredToolsRecord(Extensions),
        "agent_listing_delta" => AgentListingDelta(Extensions),
        "mcp_instructions_delta" => McpInstructionsDelta(Extensions),
        "skill_listing" => SkillListing(Extensions),
        "prompt_snapshot" => PromptSnapshot(Extensions),
        // A hook decided a permission: its event, the call and the decision.
        "hook_permission_decision" => HookPermissionDecision(Extensions),
        "environment" => Environment(Extensions),
        "model" => Model(Extensions),
        "session_context" => SessionContext(Extensions),
        "date" => Date(Extensions),
        "credential_org" => CredentialOrg(Extensions),
        "remote_session_change" => RemoteSessionChange(Extensions),
    }
}

/// A prompt the person queued while Claude was busy, joined into the turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueuedCommand {
    /// The prompt's text, or its blocks when it carries a pasted image.
    #[serde(default = "crate::absent::prompt")]
    pub prompt: MessageContent,
    /// `prompt` for a prompt; anything else is a command.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_mode: Option<String>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

/// The mode of Claude's input line.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModeRow {
    pub mode: String,
    pub session_id: String,
    #[serde(flatten)]
    pub extensions: Extensions,
}

/// The session's permission mode, written when it changes.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionModeRow {
    #[serde(default = "crate::absent::permission_mode")]
    pub permission_mode: PermissionMode,
    #[serde(default = "crate::absent::row_session_id")]
    pub session_id: String,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AtisLatchRow {
    pub session_id: String,
    #[serde(flatten)]
    pub extensions: Extensions,
}

/// The title Claude gave the session.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiTitleRow {
    pub session_id: String,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentNameRow {
    pub session_id: String,
    #[serde(flatten)]
    pub extensions: Extensions,
}

/// Every tool the session has called so far, in first-use order: written
/// ahead of the message that first calls a tool (Claude 2.1.312).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolsUsedRow {
    pub session_id: String,
    #[serde(flatten)]
    pub extensions: Extensions,
}

/// The newest prompt, for Claude's resume list.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LastPromptRow {
    pub session_id: String,
    #[serde(flatten)]
    pub extensions: Extensions,
}

/// A change to Claude's queue of prompts typed while it was busy.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueOperationRow {
    pub session_id: String,
    pub timestamp: String,
    #[serde(flatten)]
    pub extensions: Extensions,
}

/// The files Claude has backed up, as of a message.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileHistorySnapshotRow {
    #[serde(flatten)]
    pub extensions: Extensions,
}

/// One file Claude backed up before changing it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileHistoryDeltaRow {
    pub timestamp: String,
    #[serde(flatten)]
    pub extensions: Extensions,
}
