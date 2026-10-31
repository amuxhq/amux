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

use crate::stream::{
    ElicitationResult, Extensions, PermissionMode, PermissionResult, PermissionUpdate,
};
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
        "PermissionDenied" => PermissionDenied(PermissionDenied),
        "Notification" => Notification(Notification),
        "Elicitation" => Elicitation(Elicitation),
        "MessageDisplay" => MessageDisplay(MessageDisplay),
        "PostModelSwitch" => PostModelSwitch(PostModelSwitch),
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
            Self::PermissionDenied(payload) => &payload.common,
            Self::Notification(payload) => &payload.common,
            Self::Elicitation(payload) => &payload.common,
            Self::MessageDisplay(payload) => &payload.common,
            Self::PostModelSwitch(payload) => &payload.common,
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
    /// Set when a subagent, not the main conversation, raised the hook.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
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

/// Auto mode refused a call; `reason` names the rule, as Claude words it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionDenied {
    #[serde(flatten)]
    pub common: Common,
    pub tool_name: String,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub tool_input: Value,
    pub tool_use_id: String,
    #[serde(default)]
    pub reason: String,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notification {
    #[serde(flatten)]
    pub common: Common,
    #[serde(flatten)]
    pub extensions: Extensions,
}

/// A tool server asking the person for input: a form (`mode` `form` or
/// absent) or a link to open (`url`). Claude shows its own dialog only
/// once this hook has returned without answering.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Elicitation {
    #[serde(flatten)]
    pub common: Common,
    #[serde(default)]
    pub mcp_server_name: String,
    #[serde(default)]
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elicitation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_schema: Option<Value>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

/// What a hook Claude waits on prints as its decision.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HookOutput {
    pub hook_specific_output: HookDecision,
}

/// The decision itself, named by the event it answers.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "hookEventName")]
pub enum HookDecision {
    /// A permission request's decision, as a control reply would carry it.
    PermissionRequest { decision: PermissionResult },
    /// A tool server's form or link: the action and the form's content.
    Elicitation(ElicitationResult),
}

/// Lines of the assistant's reply as Claude draws them: `delta` holds the
/// lines completed since the last batch of the same `message_id`, and
/// `final` marks the message's last batch, which may be empty. The ids are
/// Claude's display ids, not the transcript's.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageDisplay {
    #[serde(flatten)]
    pub common: Common,
    #[serde(default)]
    pub turn_id: String,
    #[serde(default)]
    pub message_id: String,
    #[serde(default)]
    pub index: u64,
    #[serde(default, rename = "final")]
    pub last: bool,
    #[serde(default)]
    pub delta: String,
    #[serde(flatten)]
    pub extensions: Extensions,
}

/// The session's model changed: a switch the person asked for, a fallback,
/// or plan mode's own model (`source` `auto`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostModelSwitch {
    #[serde(flatten)]
    pub common: Common,
    pub to_model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

/// The end of a turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stop {
    #[serde(flatten)]
    pub common: Common,
    /// The jobs still running in the background.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background_tasks: Option<Vec<BackgroundTask>>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

/// A subagent finished; its agent id is the common `agent_id`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubagentStop {
    #[serde(flatten)]
    pub common: Common,
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
