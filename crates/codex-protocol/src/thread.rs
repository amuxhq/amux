//! Threads, turns and the settings and usage Codex reports about them.
//!
//! A field Codex always writes, null or not, is a plain `Option`; one it
//! leaves out when it has nothing is skipped when `None`. That keeps a
//! decoded line's JSON equal to what was written.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::Extra;
use crate::items::ThreadItem;
use crate::macros::{string_enum, tagged_enum};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Thread {
    pub id: String,
    /// The thread this one was forked from.
    #[serde(default)]
    pub forked_from_id: Option<String>,
    /// The version of Codex that made the thread.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cli_version: Option<String>,
    #[serde(default)]
    pub turns: Vec<Turn>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Turn {
    pub id: String,
    #[serde(default)]
    pub items: Vec<ThreadItem>,
    #[serde(default)]
    pub status: TurnStatus,
    #[serde(default)]
    pub error: Option<TurnError>,
    #[serde(default)]
    pub duration_ms: Option<i64>,
    #[serde(flatten)]
    pub extra: Extra,
}

string_enum! {
    pub enum TurnStatus {
        InProgress = "inProgress",
        Completed = "completed",
        Interrupted = "interrupted",
        Failed = "failed",
    }
}

/// Read when the status is left out: no known status.
impl Default for TurnStatus {
    fn default() -> Self {
        Self::Other(String::new())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnError {
    pub message: String,
    #[serde(default)]
    pub codex_error_info: Option<CodexErrorInfo>,
    #[serde(default)]
    pub additional_details: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// Codex's kind of error: a bare name, or a name with details.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CodexErrorInfo {
    Named(String),
    Detailed(serde_json::Map<String, Value>),
}

impl CodexErrorInfo {
    /// The variant's name, with or without details.
    pub fn kind(&self) -> &str {
        match self {
            Self::Named(kind) => kind,
            Self::Detailed(object) => object.keys().next().map(String::as_str).unwrap_or(""),
        }
    }

    /// The HTTP status a detailed error carries.
    pub fn http_status_code(&self) -> Option<i64> {
        match self {
            Self::Named(_) => None,
            Self::Detailed(object) => object.values().next()?.get("httpStatusCode")?.as_i64(),
        }
    }
}

/// How much Codex asks before acting: a named policy, or a granular one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AskForApproval {
    Named(ApprovalPolicy),
    Granular { granular: GranularApproval },
}

/// Which kinds of approval reach the client; the rest Codex decides alone.
/// Written in snake case, unlike the rest of the protocol.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GranularApproval {
    pub sandbox_approval: bool,
    pub rules: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skill_approval: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_permissions: Option<bool>,
    pub mcp_elicitations: bool,
    #[serde(flatten)]
    pub extra: Extra,
}

string_enum! {
    pub enum ApprovalPolicy {
        Untrusted = "untrusted",
        OnRequest = "on-request",
        Never = "never",
    }
}

string_enum! {
    pub enum ApprovalsReviewer {
        User = "user",
        AutoReview = "auto_review",
        GuardianSubagent = "guardian_subagent",
    }
}

string_enum! {
    /// The sandbox as thread start and resume name it.
    pub enum SandboxMode {
        ReadOnly = "read-only",
        WorkspaceWrite = "workspace-write",
        DangerFullAccess = "danger-full-access",
    }
}

tagged_enum! {
    /// The sandbox as Codex reports it and turn start sets it.
    pub enum SandboxPolicy {
        "dangerFullAccess" => DangerFullAccess(Extra),
        "readOnly" => ReadOnly(Extra),
        "workspaceWrite" => WorkspaceWrite(Extra),
    }
}

impl SandboxMode {
    /// The same sandbox as turn start sets it.
    pub fn policy(&self) -> SandboxPolicy {
        match self {
            Self::ReadOnly => SandboxPolicy::ReadOnly(Extra::new()),
            Self::WorkspaceWrite => SandboxPolicy::WorkspaceWrite(Extra::new()),
            Self::DangerFullAccess => SandboxPolicy::DangerFullAccess(Extra::new()),
            Self::Other(kind) => {
                SandboxPolicy::Unknown(Extra::from_iter([("type".into(), kind.clone().into())]))
            }
        }
    }
}

impl SandboxPolicy {
    /// The same sandbox as thread start names it.
    pub fn mode(&self) -> Option<SandboxMode> {
        match self {
            Self::DangerFullAccess(_) => Some(SandboxMode::DangerFullAccess),
            Self::ReadOnly(_) => Some(SandboxMode::ReadOnly),
            Self::WorkspaceWrite(_) => Some(SandboxMode::WorkspaceWrite),
            Self::Unknown(_) => None,
        }
    }
}

string_enum! {
    pub enum ReasoningEffort {
        None = "none",
        Minimal = "minimal",
        Low = "low",
        Medium = "medium",
        High = "high",
        XHigh = "xhigh",
        Max = "max",
        Ultra = "ultra",
    }
}

string_enum! {
    pub enum ModeKind {
        Plan = "plan",
        Default = "default",
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CollaborationMode {
    pub mode: ModeKind,
    pub settings: CollaborationSettings,
    #[serde(flatten)]
    pub extra: Extra,
}

/// Written in snake case, unlike the rest of the protocol.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CollaborationSettings {
    pub model: String,
    #[serde(default)]
    pub reasoning_effort: Option<ReasoningEffort>,
    #[serde(default)]
    pub developer_instructions: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// A thread's settings, as `thread/settings/updated` reports them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadSettings {
    pub approval_policy: AskForApproval,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approvals_reviewer: Option<ApprovalsReviewer>,
    pub collaboration_mode: CollaborationMode,
    #[serde(default)]
    pub effort: Option<ReasoningEffort>,
    pub model: String,
    pub sandbox_policy: SandboxPolicy,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadTokenUsage {
    pub last: TokenUsageBreakdown,
    #[serde(default)]
    pub model_context_window: Option<i64>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenUsageBreakdown {
    pub total_tokens: i64,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RateLimitSnapshot {
    #[serde(default)]
    pub limit_id: Option<String>,
    #[serde(default)]
    pub primary: Option<RateLimitWindow>,
    #[serde(default)]
    pub secondary: Option<RateLimitWindow>,
    #[serde(default)]
    pub credits: Option<CreditsSnapshot>,
    #[serde(default)]
    pub plan_type: Option<PlanType>,
    #[serde(default)]
    pub rate_limit_reached_type: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RateLimitWindow {
    pub used_percent: i64,
    #[serde(default)]
    pub window_duration_mins: Option<i64>,
    #[serde(default)]
    pub resets_at: Option<i64>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreditsSnapshot {
    pub unlimited: bool,
    #[serde(default)]
    pub balance: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

string_enum! {
    pub enum PlanType {
        Free = "free",
        Go = "go",
        Plus = "plus",
        Pro = "pro",
        ProLite = "prolite",
        ProMax = "promax",
        Team = "team",
        SelfServeBusinessProLite = "self_serve_business_prolite",
        SelfServeBusinessUsageBased = "self_serve_business_usage_based",
        Business = "business",
        Ent26 = "ent26",
        EnterpriseCbpAutomation = "enterprise_cbp_automation",
        EnterpriseCbpUsageBased = "enterprise_cbp_usage_based",
        Enterprise = "enterprise",
        Edu = "edu",
        EduPlus = "edu_plus",
        EduPro = "edu_pro",
        Unknown = "unknown",
    }
}

string_enum! {
    pub enum AuthMode {
        ApiKey = "apikey",
        Chatgpt = "chatgpt",
        ChatgptAuthTokens = "chatgptAuthTokens",
        Headers = "headers",
        AgentIdentity = "agentIdentity",
        PersonalAccessToken = "personalAccessToken",
        BedrockApiKey = "bedrockApiKey",
        BedrockAccessKeys = "bedrockAccessKeys",
    }
}

/// One step of a turn's plan.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnPlanStep {
    pub step: String,
    pub status: TurnPlanStepStatus,
    #[serde(flatten)]
    pub extra: Extra,
}

string_enum! {
    pub enum TurnPlanStepStatus {
        Pending = "pending",
        InProgress = "inProgress",
        Completed = "completed",
    }
}
