//! What terminal Claude hands a hook command on stdin, which amux's hook
//! command forwards to the agent process unchanged.
//!
//! Payloads are told apart by `hook_event_name`. An event this crate does
//! not know, or a known one whose fields do not decode, is kept whole as
//! `Unknown` and refused by [`strict`]; unknown fields of a known payload
//! are kept in its `extensions` and written back by [`encode`].

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::stream::{Extensions, PermissionMode, PermissionUpdate};
use crate::strictness::tagged_enum;
use crate::{DecodeError, Drift, decoding};

tagged_enum! {
    /// One hook payload.
    pub enum Payload by "hook_event_name" {
        "SessionStart" => SessionStart(SessionStart),
        "SessionEnd" => SessionEnd(SessionEnd),
        "UserPromptSubmit" => UserPromptSubmit(UserPromptSubmit),
        "PreToolUse" => PreToolUse(PreToolUse),
        "PermissionRequest" => PermissionRequest(PermissionRequest),
        "PostToolUse" => PostToolUse(PostToolUse),
        "PostToolUseFailure" => PostToolUseFailure(PostToolUseFailure),
        "Notification" => Notification(Notification),
        "Stop" => Stop(Stop),
        "SubagentStop" => SubagentStop(SubagentStop),
    }
}

impl Payload {
    /// The hook event's name.
    pub fn name(&self) -> &str {
        self.kind()
    }

    /// The fields every known payload carries.
    pub fn common(&self) -> Option<&Common> {
        Some(match self {
            Self::SessionStart(payload) => &payload.common,
            Self::SessionEnd(payload) => &payload.common,
            Self::UserPromptSubmit(payload) => &payload.common,
            Self::PreToolUse(payload) => &payload.common,
            Self::PermissionRequest(payload) => &payload.common,
            Self::PostToolUse(payload) => &payload.common,
            Self::PostToolUseFailure(payload) => &payload.common,
            Self::Notification(payload) => &payload.common,
            Self::Stop(payload) => &payload.common,
            Self::SubagentStop(payload) => &payload.common,
            Self::Unknown(_) => return None,
        })
    }
}

/// Decodes one payload. Fails only when it is not a JSON object.
pub fn decode(bytes: &[u8]) -> Result<Payload, DecodeError> {
    let object = decoding::object(bytes)?;
    Ok(Payload::deserialize(object).expect("a payload decodes from any object"))
}

/// Decodes one payload, failing on anything [`decode`] would keep without
/// knowing it.
pub fn strict(bytes: &[u8]) -> Result<Payload, Drift> {
    decoding::strict(bytes, "hook_event_name", |object| {
        Payload::deserialize(object).expect("a payload decodes from any object")
    })
}

/// One payload as Claude would write it.
pub fn encode(payload: &Payload) -> Vec<u8> {
    decoding::encode(payload)
}

/// The fields every hook payload carries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Common {
    #[serde(default = "crate::absent::session_id")]
    pub session_id: String,
    /// The transcript the session is writing; it moves when Claude clears
    /// or starts a new conversation.
    #[serde(default = "crate::absent::transcript_path")]
    pub transcript_path: PathBuf,
    #[serde(default = "crate::absent::cwd")]
    pub cwd: PathBuf,
    /// Absent at the start and end of a session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<PermissionMode>,
    /// The prompt the turn started from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scratchpad_dir: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<Effort>,
    /// Set when a subagent, not the main conversation, raised the hook.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Effort {
    pub level: String,
    #[serde(flatten)]
    pub extensions: Extensions,
}

/// A session starting, resuming, or starting over after a clear or a
/// compaction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionStart {
    #[serde(flatten)]
    pub common: Common,
    /// `startup`, `resume`, `clear` or `compact`.
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionEnd {
    #[serde(flatten)]
    pub common: Common,
    pub reason: String,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserPromptSubmit {
    #[serde(flatten)]
    pub common: Common,
    pub prompt: String,
    #[serde(flatten)]
    pub extensions: Extensions,
}

/// A tool call about to run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreToolUse {
    #[serde(flatten)]
    pub common: Common,
    pub tool_name: String,
    pub tool_input: Value,
    pub tool_use_id: String,
    #[serde(flatten)]
    pub extensions: Extensions,
}

/// Claude asking the person whether a tool call may run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionRequest {
    #[serde(flatten)]
    pub common: Common,
    pub tool_name: String,
    pub tool_input: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
    /// The rules Claude offers to remember the answer as.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_suggestions: Option<Vec<PermissionUpdate>>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

/// A tool call that finished.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostToolUse {
    #[serde(flatten)]
    pub common: Common,
    pub tool_name: String,
    pub tool_input: Value,
    pub tool_response: Value,
    pub tool_use_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

/// A tool call that failed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostToolUseFailure {
    #[serde(flatten)]
    pub common: Common,
    pub tool_name: String,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub tool_input: Value,
    pub tool_use_id: String,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notification {
    #[serde(flatten)]
    pub common: Common,
    #[serde(default = "crate::absent::message")]
    pub message: String,
    /// What Claude is showing: `idle_prompt`, a permission prompt, or a
    /// tool server's form (`elicitation_dialog`) or link
    /// (`elicitation_url_dialog`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notification_type: Option<String>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

/// The end of a turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stop {
    #[serde(flatten)]
    pub common: Common,
    #[serde(default = "crate::absent::stop_hook_active")]
    pub stop_hook_active: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_assistant_message: Option<String>,
    /// The jobs still running in the background.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background_tasks: Option<Vec<BackgroundTask>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_crons: Option<Vec<Value>>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

/// A subagent finished; its agent id is the common `agent_id`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubagentStop {
    #[serde(flatten)]
    pub common: Common,
    #[serde(default = "crate::absent::stop_hook_active")]
    pub stop_hook_active: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_transcript_path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_assistant_message: Option<String>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackgroundTask {
    #[serde(default = "crate::absent::id")]
    pub id: String,
    /// `shell` or `subagent`.
    #[serde(default = "crate::absent::message_type")]
    pub r#type: String,
    #[serde(default = "crate::absent::status")]
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(flatten)]
    pub extensions: Extensions,
}
