//! What Codex's server sends: notifications, requests that wait for amux's
//! answer, and the results of amux's requests.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::items::{CommandAction, ThreadItem};
use crate::macros::{method_enum, string_enum, tagged_enum};
use crate::thread::{
    ApprovalsReviewer, AskForApproval, AuthMode, CollaborationMode, PlanType, RateLimitSnapshot,
    ReasoningEffort, SandboxPolicy, Thread, ThreadSettings, ThreadStatus, ThreadTokenUsage, Turn,
    TurnError, TurnPlanStep,
};
use crate::{Extra, RequestId};

method_enum! {
    pub enum ServerNotification {
        "error" => Error(ErrorNotification),
        "warning" => Warning(WarningNotification),
        "guardianWarning" => GuardianWarning(GuardianWarningNotification),
        "thread/started" => ThreadStarted(ThreadStarted),
        "thread/status/changed" => ThreadStatusChanged(ThreadStatusChanged),
        "thread/name/updated" => ThreadNameUpdated(ThreadNameUpdated),
        "thread/settings/updated" => ThreadSettingsUpdated(ThreadSettingsUpdated),
        "thread/tokenUsage/updated" => ThreadTokenUsageUpdated(ThreadTokenUsageUpdated),
        "thread/archived" => ThreadArchived(ThreadOnly),
        "thread/unarchived" => ThreadUnarchived(ThreadOnly),
        "thread/closed" => ThreadClosed(ThreadOnly),
        "thread/goal/updated" => ThreadGoalUpdated(ThreadOnly),
        "thread/goal/cleared" => ThreadGoalCleared(ThreadOnly),
        "thread/readState/changed" => ThreadReadStateChanged(ThreadOnly),
        "thread/compacted" => ThreadCompacted(TurnScoped),
        "turn/started" => TurnStarted(TurnNotification),
        "turn/completed" => TurnCompleted(TurnNotification),
        "turn/diff/updated" => TurnDiffUpdated(TurnDiffUpdated),
        "turn/plan/updated" => TurnPlanUpdated(TurnPlanUpdated),
        "turn/moderationMetadata" => TurnModerationMetadata(TurnScoped),
        "item/started" => ItemStarted(ItemNotification),
        "item/completed" => ItemCompleted(ItemNotification),
        "item/agentMessage/delta" => AgentMessageDelta(Delta),
        "item/plan/delta" => PlanDelta(Delta),
        "item/reasoning/textDelta" => ReasoningTextDelta(Delta),
        "item/reasoning/summaryTextDelta" => ReasoningSummaryTextDelta(SummaryDelta),
        "item/reasoning/summaryPartAdded" => ReasoningSummaryPartAdded(SummaryPartAdded),
        "item/commandExecution/outputDelta" => CommandOutputDelta(Delta),
        "item/commandExecution/terminalInteraction" => TerminalInteraction(TerminalInteraction),
        "item/fileChange/outputDelta" => FileChangeOutputDelta(Delta),
        "item/autoApprovalReview/started" => AutoApprovalReviewStarted(AutoApprovalReview),
        "item/autoApprovalReview/completed" => AutoApprovalReviewCompleted(AutoApprovalReview),
        "serverRequest/resolved" => ServerRequestResolved(ServerRequestResolved),
        "model/rerouted" => ModelRerouted(ModelRerouted),
        "model/verification" => ModelVerification(ModelVerification),
        "model/safetyBuffering/updated" => SafetyBufferingUpdated(SafetyBufferingUpdated),
        "autoApprovalReview/strictReviewRequired" => StrictReviewRequired(ThreadOnly),
        "hook/started" => HookStarted(ThreadOnly),
        "hook/completed" => HookCompleted(ThreadOnly),
        "account/updated" => AccountUpdated(AccountUpdated),
        "account/rateLimits/updated" => AccountRateLimitsUpdated(AccountRateLimitsUpdated),
        "account/login/completed" => AccountLoginCompleted(AccountLoginCompleted),
        "mcpServer/startupStatus/updated" => McpServerStatusUpdated(McpServerStatusUpdated),
        "remoteControl/status/changed" => RemoteControlStatusChanged(RemoteControlStatusChanged),
        "skills/changed" => SkillsChanged(Extra),
        "deprecationNotice" => DeprecationNotice(DeprecationNotice),
        "configWarning" => ConfigWarning(DeprecationNotice),
    }
}

impl ServerNotification {
    /// The thread the notification is about, when it names one.
    pub fn thread_id(&self) -> Option<&str> {
        match self {
            Self::Error(n) => Some(&n.thread_id),
            Self::Warning(n) => n.thread_id.as_deref(),
            Self::GuardianWarning(n) => Some(&n.thread_id),
            Self::ThreadStarted(n) => Some(&n.thread.id),
            Self::ThreadStatusChanged(n) => Some(&n.thread_id),
            Self::ThreadNameUpdated(n) => Some(&n.thread_id),
            Self::ThreadSettingsUpdated(n) => Some(&n.thread_id),
            Self::ThreadTokenUsageUpdated(n) => Some(&n.thread_id),
            Self::ThreadArchived(n)
            | Self::ThreadUnarchived(n)
            | Self::ThreadClosed(n)
            | Self::ThreadGoalUpdated(n)
            | Self::ThreadGoalCleared(n)
            | Self::ThreadReadStateChanged(n) => Some(&n.thread_id),
            Self::ThreadCompacted(n) | Self::TurnModerationMetadata(n) => Some(&n.thread_id),
            Self::TurnStarted(n) | Self::TurnCompleted(n) => Some(&n.thread_id),
            Self::TurnDiffUpdated(n) => Some(&n.thread_id),
            Self::TurnPlanUpdated(n) => Some(&n.thread_id),
            Self::ItemStarted(n) | Self::ItemCompleted(n) => Some(&n.thread_id),
            Self::AgentMessageDelta(n)
            | Self::PlanDelta(n)
            | Self::ReasoningTextDelta(n)
            | Self::CommandOutputDelta(n)
            | Self::FileChangeOutputDelta(n) => Some(&n.thread_id),
            Self::ReasoningSummaryTextDelta(n) => Some(&n.thread_id),
            Self::ReasoningSummaryPartAdded(n) => Some(&n.thread_id),
            Self::TerminalInteraction(n) => Some(&n.thread_id),
            Self::AutoApprovalReviewStarted(n) | Self::AutoApprovalReviewCompleted(n) => {
                Some(&n.thread_id)
            }
            Self::ServerRequestResolved(n) => Some(&n.thread_id),
            Self::ModelRerouted(n) => Some(&n.thread_id),
            Self::ModelVerification(n) => Some(&n.thread_id),
            Self::SafetyBufferingUpdated(n) => Some(&n.thread_id),
            Self::StrictReviewRequired(n) | Self::HookStarted(n) | Self::HookCompleted(n) => {
                Some(&n.thread_id)
            }
            Self::McpServerStatusUpdated(n) => n.thread_id.as_deref(),
            Self::AccountUpdated(_)
            | Self::AccountRateLimitsUpdated(_)
            | Self::AccountLoginCompleted(_)
            | Self::RemoteControlStatusChanged(_)
            | Self::SkillsChanged(_)
            | Self::DeprecationNotice(_)
            | Self::ConfigWarning(_) => None,
        }
    }
}

/// Checks Codex ran on the model's answers to this thread; it names them,
/// as `trustedAccessForCyber`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelVerification {
    pub thread_id: String,
    #[serde(default)]
    pub verifications: Vec<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// Codex holds a turn's answer for a slower safety check while
/// `show_buffering_ui` is true.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SafetyBufferingUpdated {
    pub thread_id: String,
    pub turn_id: String,
    pub show_buffering_ui: bool,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorNotification {
    pub thread_id: String,
    pub turn_id: String,
    pub error: TurnError,
    pub will_retry: bool,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WarningNotification {
    pub message: String,
    #[serde(default)]
    pub thread_id: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuardianWarningNotification {
    pub thread_id: String,
    pub message: String,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThreadStarted {
    pub thread: Thread,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadStatusChanged {
    pub thread_id: String,
    pub status: ThreadStatus,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadNameUpdated {
    pub thread_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_name: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadSettingsUpdated {
    pub thread_id: String,
    pub thread_settings: ThreadSettings,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadTokenUsageUpdated {
    pub thread_id: String,
    pub turn_id: String,
    pub token_usage: ThreadTokenUsage,
    #[serde(flatten)]
    pub extra: Extra,
}

/// Something the server says is going away, with what to do instead; or,
/// as `configWarning`, configuration it found and did not apply.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeprecationNotice {
    pub summary: String,
    pub details: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadOnly {
    pub thread_id: String,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnScoped {
    pub thread_id: String,
    pub turn_id: String,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnNotification {
    pub thread_id: String,
    pub turn: Turn,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnDiffUpdated {
    pub thread_id: String,
    pub turn_id: String,
    pub diff: String,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnPlanUpdated {
    pub thread_id: String,
    pub turn_id: String,
    #[serde(default)]
    pub explanation: Option<String>,
    pub plan: Vec<TurnPlanStep>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// `item/started` and `item/completed`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemNotification {
    pub thread_id: String,
    pub turn_id: String,
    pub item: ThreadItem,
    /// When the item started; on `item/started`, from Codex 0.157.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<i64>,
    /// When the item ended; on `item/completed`, from Codex 0.157.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at_ms: Option<i64>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// A piece of an item's text as it streams.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Delta {
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
    pub delta: String,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SummaryDelta {
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
    pub delta: String,
    pub summary_index: i64,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SummaryPartAdded {
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
    pub summary_index: i64,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalInteraction {
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
    pub process_id: String,
    pub stdin: String,
    #[serde(flatten)]
    pub extra: Extra,
}

/// Codex's own reviewer deciding an approval in the person's place.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoApprovalReview {
    pub thread_id: String,
    pub turn_id: String,
    pub review_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_item_id: Option<String>,
    pub review: Review,
    pub action: ReviewAction,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at_ms: Option<i64>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Review {
    pub status: ReviewStatus,
    #[serde(default)]
    pub rationale: Option<String>,
    #[serde(default)]
    pub risk_level: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

string_enum! {
    pub enum ReviewStatus {
        InProgress = "inProgress",
        Approved = "approved",
        Denied = "denied",
        TimedOut = "timedOut",
        Aborted = "aborted",
    }
}

tagged_enum! {
    /// What Codex's reviewer was asked to approve.
    pub enum ReviewAction {
        "command" => Command(Extra),
        "execve" => Execve(Extra),
        "writeStdin" => WriteStdin(Extra),
        "applyPatch" => ApplyPatch(Extra),
        "networkAccess" => NetworkAccess(Extra),
        "mcpToolCall" => McpToolCall(Extra),
        "requestPermissions" => RequestPermissions(Extra),
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerRequestResolved {
    pub thread_id: String,
    pub request_id: RequestId,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRerouted {
    pub thread_id: String,
    pub turn_id: String,
    pub from_model: String,
    pub to_model: String,
    pub reason: String,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountUpdated {
    #[serde(default)]
    pub auth_mode: Option<AuthMode>,
    #[serde(default)]
    pub plan_type: Option<PlanType>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountRateLimitsUpdated {
    pub rate_limits: RateLimitSnapshot,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountLoginCompleted {
    pub success: bool,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerStatusUpdated {
    pub name: String,
    pub status: McpServerStartupState,
    #[serde(default)]
    pub error: Option<String>,
    /// Why it failed, when Codex knows: `reauthenticationRequired`.
    #[serde(default)]
    pub failure_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

string_enum! {
    pub enum McpServerStartupState {
        Starting = "starting",
        Ready = "ready",
        Failed = "failed",
        Cancelled = "cancelled",
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteControlStatusChanged {
    pub status: RemoteControlStatus,
    #[serde(flatten)]
    pub extra: Extra,
}

string_enum! {
    pub enum RemoteControlStatus {
        Disabled = "disabled",
        Connecting = "connecting",
        Connected = "connected",
        Errored = "errored",
    }
}

method_enum! {
    pub enum ServerRequest {
        "item/commandExecution/requestApproval" => CommandApproval(CommandApprovalParams),
        "item/fileChange/requestApproval" => FileChangeApproval(FileChangeApprovalParams),
        "item/permissions/requestApproval" => PermissionsApproval(PermissionsApprovalParams),
        "item/tool/requestUserInput" => RequestUserInput(RequestUserInputParams),
        "item/tool/call" => ToolCall(ToolCallParams),
        "mcpServer/elicitation/request" => Elicitation(ElicitationParams),
    }
}

impl ServerRequest {
    pub fn thread_id(&self) -> &str {
        match self {
            Self::CommandApproval(p) => &p.thread_id,
            Self::FileChangeApproval(p) => &p.thread_id,
            Self::PermissionsApproval(p) => &p.thread_id,
            Self::RequestUserInput(p) => &p.thread_id,
            Self::ToolCall(p) => &p.thread_id,
            Self::Elicitation(p) => &p.thread_id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandApprovalParams {
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
    /// When Codex started asking, from Codex 0.157.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_actions: Option<Vec<CommandAction>>,
    /// The answers Codex offers; when absent, the four plain decisions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub available_decisions: Option<Vec<CommandDecision>>,
    /// The command prefix "approve similar" would allow.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposed_execpolicy_amendment: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_approval_context: Option<NetworkApprovalContext>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NetworkApprovalContext {
    pub host: String,
    pub protocol: String,
    #[serde(flatten)]
    pub extra: Extra,
}

/// An answer to a command approval: a plain decision, or one carrying the
/// rule it adds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CommandDecision {
    Plain(Decision),
    #[serde(rename_all = "camelCase")]
    AcceptWithExecpolicyAmendment {
        accept_with_execpolicy_amendment: ExecpolicyAmendment,
    },
    #[serde(rename_all = "camelCase")]
    ApplyNetworkPolicyAmendment {
        apply_network_policy_amendment: NetworkPolicyChoice,
    },
}

/// A persistent network rule an approval can add. Written in snake case,
/// unlike the rest of the protocol.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NetworkPolicyChoice {
    pub network_policy_amendment: NetworkPolicyAmendment,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NetworkPolicyAmendment {
    pub host: String,
    /// `allow` or `deny`.
    pub action: String,
    #[serde(flatten)]
    pub extra: Extra,
}

/// The rule an approval adds: commands starting with these words run
/// without asking. Written in snake case, unlike the rest of the protocol.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecpolicyAmendment {
    pub execpolicy_amendment: Vec<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

string_enum! {
    pub enum Decision {
        Accept = "accept",
        AcceptForSession = "acceptForSession",
        Decline = "decline",
        Cancel = "cancel",
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileChangeApprovalParams {
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub grant_root: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionsApprovalParams {
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub reason: Option<String>,
    pub permissions: PermissionProfile,
    #[serde(flatten)]
    pub extra: Extra,
}

/// Extra file and network access, asked for or granted. Codex writes a
/// part it does not ask for as null; amux leaves out a part it does not
/// grant.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionProfile {
    #[serde(
        default,
        deserialize_with = "crate::present_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub file_system: Option<Option<FileSystemPermissions>>,
    #[serde(
        default,
        deserialize_with = "crate::present_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub network: Option<Option<NetworkPermissions>>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FileSystemPermissions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub write: Option<Vec<String>>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct NetworkPermissions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestUserInputParams {
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
    pub questions: Vec<Question>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Question {
    pub id: String,
    pub header: String,
    pub question: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_other: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_secret: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<QuestionOption>>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuestionOption {
    pub label: String,
    pub description: String,
    #[serde(flatten)]
    pub extra: Extra,
}

/// A call of one of the dynamic tools amux gave the thread.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallParams {
    pub thread_id: String,
    pub turn_id: String,
    pub call_id: String,
    pub tool: String,
    #[serde(default)]
    pub namespace: Option<String>,
    /// The tool's arguments, as the model wrote them.
    pub arguments: Value,
    #[serde(flatten)]
    pub extra: Extra,
}

/// An MCP server asking the person something through Codex.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ElicitationParams {
    pub thread_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    pub server_name: String,
    pub mode: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// The JSON schema of the form, as the server wrote it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_schema: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(rename = "_meta", default)]
    pub meta: Option<ElicitationMeta>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ElicitationMeta {
    /// How long an acceptance may last: "session", "always".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persist: Option<Persist>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex_approval_kind: Option<String>,
    /// The tool whose call is being approved, from codex-cli 0.163.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_description: Option<String>,
    /// The arguments of the tool call being approved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_params: Option<Value>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// The lifetimes an approval offers. Codex writes a single string when
/// it offers one and a list when it offers both.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Persist {
    One(String),
    Many(Vec<String>),
}

impl Persist {
    pub fn scopes(&self) -> &[String] {
        match self {
            Persist::One(scope) => std::slice::from_ref(scope),
            Persist::Many(scopes) => scopes,
        }
    }

    pub fn offers(&self, scope: &str) -> bool {
        self.scopes().iter().any(|offered| offered == scope)
    }
}

// ── Results of amux's requests ───────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResponse {
    pub user_agent: String,
    #[serde(flatten)]
    pub extra: Extra,
}

/// The answer to `thread/start` and `thread/resume`. Only the thread is
/// required: the agent's thread starts on this answer, so a setting Codex
/// stops sending must not stop it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadResponse {
    pub thread: Thread,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub approval_policy: Option<AskForApproval>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approvals_reviewer: Option<ApprovalsReviewer>,
    #[serde(default)]
    pub sandbox: Option<SandboxPolicy>,
    #[serde(default)]
    pub reasoning_effort: Option<ReasoningEffort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collaboration_mode: Option<CollaborationMode>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnStartResponse {
    pub turn: Turn,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnSteerResponse {
    pub turn_id: String,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelListResponse {
    pub data: Vec<Model>,
    #[serde(default)]
    pub next_cursor: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Model {
    pub id: String,
    pub model: String,
    pub display_name: String,
    pub description: String,
    pub hidden: bool,
    pub is_default: bool,
    pub default_reasoning_effort: ReasoningEffort,
    pub supported_reasoning_efforts: Vec<ReasoningEffortOption>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReasoningEffortOption {
    pub reasoning_effort: ReasoningEffort,
    pub description: String,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkillsListResponse {
    pub data: Vec<SkillsListEntry>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkillsListEntry {
    pub cwd: String,
    pub skills: Vec<Skill>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Skill {
    pub name: String,
    pub description: String,
    /// The skill's SKILL.md, which a skill input item names.
    #[serde(default)]
    pub path: String,
    pub enabled: bool,
    pub scope: String,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadListResponse {
    pub data: Vec<Thread>,
    #[serde(default)]
    pub next_cursor: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThreadReadResponse {
    pub thread: Thread,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountReadResponse {
    #[serde(default)]
    pub account: Option<Account>,
    pub requires_openai_auth: bool,
    #[serde(flatten)]
    pub extra: Extra,
}

tagged_enum! {
    pub enum Account {
        "apiKey" => ApiKey(Extra),
        "chatgpt" => Chatgpt(ChatgptAccount),
        "amazonBedrock" => AmazonBedrock(Extra),
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatgptAccount {
    #[serde(default)]
    pub email: Option<String>,
    pub plan_type: PlanType,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackgroundTerminalsResponse {
    pub data: Vec<BackgroundTerminal>,
    #[serde(default)]
    pub next_cursor: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// Whether `thread/backgroundTerminals/terminate` found the command to end.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackgroundTerminalTerminateResponse {
    pub terminated: bool,
    #[serde(flatten)]
    pub extra: Extra,
}

/// A command still running in the thread's background.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackgroundTerminal {
    pub item_id: String,
    pub process_id: String,
    pub command: String,
    pub cwd: String,
    #[serde(flatten)]
    pub extra: Extra,
}
