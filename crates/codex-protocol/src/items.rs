//! The items of a turn, and the input a person or amux gives one.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::Extra;
use crate::macros::{string_enum, tagged_enum};
use crate::thread::ReasoningEffort;

tagged_enum! {
    pub enum ThreadItem {
        "userMessage" => UserMessage(UserMessageItem),
        "hookPrompt" => HookPrompt(IdItem),
        "agentMessage" => AgentMessage(TextItem),
        "plan" => Plan(TextItem),
        "reasoning" => Reasoning(ReasoningItem),
        "commandExecution" => CommandExecution(CommandExecutionItem),
        "fileChange" => FileChange(FileChangeItem),
        "mcpToolCall" => McpToolCall(McpToolCallItem),
        "dynamicToolCall" => DynamicToolCall(DynamicToolCallItem),
        "collabAgentToolCall" => CollabAgentToolCall(CollabAgentToolCallItem),
        "subAgentActivity" => SubAgentActivity(IdItem),
        "webSearch" => WebSearch(WebSearchItem),
        "imageView" => ImageView(ImageViewItem),
        "sleep" => Sleep(IdItem),
        "imageGeneration" => ImageGeneration(ImageGenerationItem),
        "enteredReviewMode" => EnteredReviewMode(ReviewItem),
        "exitedReviewMode" => ExitedReviewMode(ReviewItem),
        "contextCompaction" => ContextCompaction(IdItem),
        "functionCallOutput" => FunctionCallOutput(IdItem),
    }
}

impl ThreadItem {
    /// The item's id; empty for an unknown item without one.
    pub fn id(&self) -> &str {
        match self {
            Self::UserMessage(item) => &item.id,
            Self::HookPrompt(item)
            | Self::SubAgentActivity(item)
            | Self::Sleep(item)
            | Self::ContextCompaction(item)
            | Self::FunctionCallOutput(item) => &item.id,
            Self::AgentMessage(item) | Self::Plan(item) => &item.id,
            Self::Reasoning(item) => &item.id,
            Self::CommandExecution(item) => &item.id,
            Self::FileChange(item) => &item.id,
            Self::McpToolCall(item) => &item.id,
            Self::DynamicToolCall(item) => &item.id,
            Self::CollabAgentToolCall(item) => &item.id,
            Self::WebSearch(item) => &item.id,
            Self::ImageView(item) => &item.id,
            Self::ImageGeneration(item) => &item.id,
            Self::EnteredReviewMode(item) | Self::ExitedReviewMode(item) => &item.id,
            Self::Unknown(object) => object.get("id").and_then(Value::as_str).unwrap_or(""),
        }
    }
}

/// An item amux reads nothing from but its id.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IdItem {
    pub id: String,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserMessageItem {
    pub id: String,
    pub content: Vec<UserInput>,
    /// The client message id the prompt or steer was sent with.
    #[serde(default)]
    pub client_id: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// An agent message or a plan.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextItem {
    pub id: String,
    pub text: String,
    /// Which part of the turn an agent message belongs to; plans have none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<MessagePhase>,
    #[serde(flatten)]
    pub extra: Extra,
}

string_enum! {
    pub enum MessagePhase {
        /// Said on the way, before the turn's answer.
        Commentary = "commentary",
        FinalAnswer = "final_answer",
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReasoningItem {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<Vec<String>>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandExecutionItem {
    pub id: String,
    pub command: String,
    pub command_actions: Vec<CommandAction>,
    pub cwd: String,
    pub status: ToolStatus,
    #[serde(default)]
    pub aggregated_output: Option<String>,
    #[serde(default)]
    pub exit_code: Option<i64>,
    #[serde(default)]
    pub duration_ms: Option<i64>,
    #[serde(flatten)]
    pub extra: Extra,
}

string_enum! {
    /// The status of a command, a file change or a tool call.
    pub enum ToolStatus {
        InProgress = "inProgress",
        Completed = "completed",
        Failed = "failed",
        Declined = "declined",
        Interrupted = "interrupted",
    }
}

tagged_enum! {
    /// What Codex makes of a command it runs.
    pub enum CommandAction {
        "read" => Read(Extra),
        "listFiles" => ListFiles(Extra),
        "search" => Search(Extra),
        "unknown" => Unrecognized(Extra),
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileChangeItem {
    pub id: String,
    pub changes: Vec<FileUpdateChange>,
    pub status: ToolStatus,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileUpdateChange {
    pub path: String,
    pub kind: PatchChangeKind,
    pub diff: String,
    #[serde(flatten)]
    pub extra: Extra,
}

tagged_enum! {
    pub enum PatchChangeKind {
        "add" => Add(Extra),
        "delete" => Delete(Extra),
        "update" => Update(PatchUpdate),
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PatchUpdate {
    #[serde(default)]
    pub move_path: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpToolCallItem {
    pub id: String,
    pub server: String,
    pub tool: String,
    pub status: ToolStatus,
    /// The tool's arguments, as the model wrote them.
    pub arguments: Value,
    #[serde(default)]
    pub result: Option<McpToolCallResult>,
    #[serde(default)]
    pub error: Option<McpToolCallError>,
    #[serde(default)]
    pub read_only_hint: Option<bool>,
    #[serde(default)]
    pub duration_ms: Option<i64>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpToolCallResult {
    /// MCP content blocks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<Vec<Value>>,
    #[serde(flatten)]
    pub extra: Extra,
}

impl McpToolCallResult {
    /// The text of each content block that has some.
    pub fn texts(&self) -> impl Iterator<Item = &str> {
        self.content
            .iter()
            .flatten()
            .filter_map(|block| block.get("text").and_then(Value::as_str))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpToolCallError {
    pub message: String,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DynamicToolCallItem {
    pub id: String,
    /// The namespace amux gave the tool, when it gave one.
    #[serde(default)]
    pub namespace: Option<String>,
    pub tool: String,
    pub status: ToolStatus,
    /// The tool's arguments, as the model wrote them.
    pub arguments: Value,
    #[serde(default)]
    pub content_items: Option<Vec<ToolOutputContent>>,
    #[serde(default)]
    pub success: Option<bool>,
    #[serde(default)]
    pub duration_ms: Option<i64>,
    #[serde(flatten)]
    pub extra: Extra,
}

tagged_enum! {
    /// What a dynamic tool answers with.
    pub enum ToolOutputContent {
        "inputText" => Text(TextContent),
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextContent {
    pub text: String,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CollabAgentToolCallItem {
    pub id: String,
    pub tool: CollabAgentTool,
    pub status: ToolStatus,
    pub receiver_thread_ids: Vec<String>,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub reasoning_effort: Option<ReasoningEffort>,
    #[serde(flatten)]
    pub extra: Extra,
}

string_enum! {
    pub enum CollabAgentTool {
        SpawnAgent = "spawnAgent",
        SendInput = "sendInput",
        ResumeAgent = "resumeAgent",
        Wait = "wait",
        CloseAgent = "closeAgent",
        SendMessage = "sendMessage",
        FollowupTask = "followupTask",
        InterruptAgent = "interruptAgent",
        ListAgents = "listAgents",
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WebSearchItem {
    pub id: String,
    pub query: String,
    #[serde(default)]
    pub action: Option<WebSearchAction>,
    #[serde(flatten)]
    pub extra: Extra,
}

tagged_enum! {
    pub enum WebSearchAction {
        "search" => Search(WebSearchQuery),
        "openPage" => OpenPage(WebPage),
        "findInPage" => FindInPage(WebPageFind),
        "other" => Other(Extra),
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WebSearchQuery {
    #[serde(default)]
    pub query: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WebPage {
    #[serde(default)]
    pub url: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WebPageFind {
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub pattern: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImageViewItem {
    pub id: String,
    pub path: String,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageGenerationItem {
    pub id: String,
    #[serde(default)]
    pub status: String,
    /// Where Codex saved the image, once it has.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saved_path: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReviewItem {
    pub id: String,
    pub review: String,
    #[serde(flatten)]
    pub extra: Extra,
}

tagged_enum! {
    /// One piece of what a person or amux gives a turn.
    pub enum UserInput {
        "text" => Text(TextInput),
        "image" => Image(ImageInput),
        "localImage" => LocalImage(LocalImageInput),
        "skill" => Skill(NamedPath),
    }
}

impl UserInput {
    /// Plain text, written as amux writes it.
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text(TextInput {
            text: text.into(),
            text_elements: Some(Vec::new()),
            extra: Extra::new(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextInput {
    pub text: String,
    /// Marked spans of the text; amux writes an empty list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_elements: Option<Vec<Value>>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// An image by URL, a data URL included.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImageInput {
    pub url: String,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LocalImageInput {
    pub path: String,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NamedPath {
    pub name: String,
    pub path: String,
    #[serde(flatten)]
    pub extra: Extra,
}
