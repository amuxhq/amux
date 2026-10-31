use serde::{Deserialize, Serialize};

use crate::stream::types::Extensions;

/// Complete response to the stream-JSON `initialize` control request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitializationResult {
    pub commands: Vec<SlashCommand>,
    pub models: Vec<ModelInfo>,
    pub account: AccountInfo,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SlashCommand {
    pub name: String,
    pub description: String,
    pub argument_hint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aliases: Option<Vec<String>>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_model: Option<String>,
    pub display_name: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supported_effort_levels: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_auto_mode: Option<bool>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subscription_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_provider: Option<String>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

/// What `get_settings` says the session runs with now: the model and
/// effort Claude applied from every settings source and the session's own
/// changes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppliedSettings {
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
    #[serde(flatten)]
    pub extensions: Extensions,
}
