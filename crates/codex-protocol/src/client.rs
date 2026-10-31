//! What amux sends to Codex's server: requests, the one notification, and
//! answers to the server's requests.
//!
//! An optional field amux leaves out when it has nothing is skipped when
//! `None`, so the bytes written are the bytes amux has always written.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::Extra;
use crate::items::{ToolOutputContent, UserInput};
use crate::macros::{method_enum, string_enum, tagged_enum};
use crate::server::{CommandDecision, PermissionProfile};
use crate::thread::{
    ApprovalsReviewer, AskForApproval, CollaborationMode, ReasoningEffort, ReasoningSummary,
    SandboxMode, SandboxPolicy,
};

method_enum! {
    pub enum ClientRequest {
        "initialize" => Initialize(InitializeParams),
        "thread/start" => ThreadStart(ThreadStartParams),
        "thread/resume" => ThreadResume(ThreadResumeParams),
        "thread/unarchive" => ThreadUnarchive(ThreadIdParams),
        "thread/unsubscribe" => ThreadUnsubscribe(ThreadIdParams),
        "thread/name/set" => ThreadSetName(ThreadSetNameParams),
        "thread/read" => ThreadRead(ThreadReadParams),
        "thread/list" => ThreadList(ThreadListParams),
        "thread/compact/start" => ThreadCompactStart(ThreadIdParams),
        "thread/inject_items" => ThreadInjectItems(InjectItemsParams),
        "thread/backgroundTerminals/list" => BackgroundTerminalsList(BackgroundTerminalsListParams),
        "thread/backgroundTerminals/terminate" => BackgroundTerminalsTerminate(BackgroundTerminalsTerminateParams),
        "turn/start" => TurnStart(TurnStartParams),
        "turn/steer" => TurnSteer(TurnSteerParams),
        "turn/interrupt" => TurnInterrupt(TurnInterruptParams),
        "review/start" => ReviewStart(ReviewStartParams),
        "model/list" => ModelList(ModelListParams),
        "skills/list" => SkillsList(SkillsListParams),
        "account/read" => AccountRead(AccountReadParams),
    }
}

method_enum! {
    pub enum ClientNotification {
        "initialized" => Initialized(()),
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeParams {
    pub client_info: ClientInfo,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<Capabilities>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClientInfo {
    pub name: String,
    #[serde(default)]
    pub title: Option<String>,
    pub version: String,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub experimental_api: Option<bool>,
    /// Notifications the server is not to send this client.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub opt_out_notification_methods: Vec<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadStartParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_provider: Option<String>,
    /// Codex configuration for this thread, keyed by dotted path, as
    /// `codex --config` takes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<Extra>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_policy: Option<AskForApproval>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approvals_reviewer: Option<ApprovalsReviewer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<SandboxMode>,
    /// Tools amux defines for the thread.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dynamic_tools: Option<Vec<DynamicTool>>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// A function tool the thread may call; Codex asks amux to run it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DynamicTool {
    pub name: String,
    pub description: String,
    /// The JSON schema of the tool's arguments.
    pub input_schema: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub defer_loading: Option<bool>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadResumeParams {
    pub thread_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_policy: Option<AskForApproval>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<SandboxMode>,
    /// Answer without the turns, for a client that pages them from the
    /// history on disk, as Codex's own app does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclude_turns: Option<bool>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadSetNameParams {
    pub thread_id: String,
    pub name: String,
    #[serde(flatten)]
    pub extra: Extra,
}

/// Reading a loaded thread with its turns writes it to disk if it is not
/// there yet: a thread that has run no turn has nothing on disk otherwise.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadReadParams {
    pub thread_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_turns: Option<bool>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// Every filter is written, null when unset.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadListParams {
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub limit: Option<i64>,
    #[serde(default)]
    pub sort_key: Option<String>,
    #[serde(default)]
    pub model_providers: Option<Vec<String>>,
    #[serde(default)]
    pub source_kinds: Option<Vec<String>>,
    #[serde(default)]
    pub archived: Option<bool>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub search_term: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadIdParams {
    pub thread_id: String,
    #[serde(flatten)]
    pub extra: Extra,
}

/// Items recorded into the thread without starting a turn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InjectItemsParams {
    pub thread_id: String,
    pub items: Vec<InjectedItem>,
    #[serde(flatten)]
    pub extra: Extra,
}

tagged_enum! {
    /// An item in the model's own conversation format.
    pub enum InjectedItem {
        "message" => Message(InjectedMessage),
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InjectedMessage {
    pub role: String,
    pub content: Vec<InjectedContent>,
    #[serde(flatten)]
    pub extra: Extra,
}

tagged_enum! {
    pub enum InjectedContent {
        "input_text" => InputText(crate::items::TextContent),
    }
}

/// Codex's own review of changes, run as a turn on the thread (inline,
/// Codex's default delivery).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewStartParams {
    pub thread_id: String,
    pub target: ReviewTarget,
    #[serde(flatten)]
    pub extra: Extra,
}

tagged_enum! {
    /// What a review looks at.
    pub enum ReviewTarget {
        /// The working tree: staged, unstaged and untracked files.
        "uncommittedChanges" => UncommittedChanges(Extra),
        /// The reviewer's instructions in the person's words.
        "custom" => Custom(CustomReview),
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CustomReview {
    pub instructions: String,
    #[serde(flatten)]
    pub extra: Extra,
}

/// Ends one command the thread runs in the background, by the process id
/// its list gave.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackgroundTerminalsTerminateParams {
    pub thread_id: String,
    pub process_id: String,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackgroundTerminalsListParams {
    pub thread_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnStartParams {
    pub thread_id: String,
    pub input: Vec<UserInput>,
    /// amux's id for the prompt; Codex echoes it as the user message's
    /// `clientId`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_user_message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// `Some(None)` writes null: back to the model's default effort.
    #[serde(
        default,
        deserialize_with = "crate::present_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub effort: Option<Option<ReasoningEffort>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<ReasoningSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_policy: Option<AskForApproval>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approvals_reviewer: Option<ApprovalsReviewer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox_policy: Option<SandboxPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collaboration_mode: Option<CollaborationMode>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnSteerParams {
    pub thread_id: String,
    pub expected_turn_id: String,
    pub input: Vec<UserInput>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_user_message_id: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnInterruptParams {
    pub thread_id: String,
    pub turn_id: String,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelListParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SkillsListParams {
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountReadParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<bool>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// amux's answer to one of the server's requests. The shapes do not
/// overlap, so a written answer reads back as the variant it was.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ClientResponse {
    CommandApproval(CommandApprovalResponse),
    PermissionsApproval(PermissionsApprovalResponse),
    ToolCall(ToolCallResponse),
    Elicitation(ElicitationResponse),
    UserInput(UserInputResponse),
}

/// The answer to a command or file change approval.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandApprovalResponse {
    pub decision: CommandDecision,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PermissionsApprovalResponse {
    pub permissions: PermissionProfile,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<GrantScope>,
    #[serde(flatten)]
    pub extra: Extra,
}

string_enum! {
    pub enum GrantScope {
        Turn = "turn",
        Session = "session",
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallResponse {
    pub content_items: Vec<ToolOutputContent>,
    pub success: bool,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ElicitationResponse {
    pub action: ElicitationAction,
    /// The form's answers; null unless accepted.
    pub content: Option<Value>,
    #[serde(rename = "_meta", default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<ElicitationResponseMeta>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ElicitationResponseMeta {
    /// How long an acceptance lasts, from what the request offered:
    /// "session", "always".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persist: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

string_enum! {
    pub enum ElicitationAction {
        Accept = "accept",
        Decline = "decline",
        Cancel = "cancel",
    }
}

/// Answers keyed by question id.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserInputResponse {
    pub answers: std::collections::BTreeMap<String, Answer>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Answer {
    pub answers: Vec<String>,
    #[serde(flatten)]
    pub extra: Extra,
}
