use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

pub type Extensions = serde_json::Map<String, serde_json::Value>;

/// A valid protocol object whose discriminant is not known to this SDK.
/// The complete object is retained so newer producers remain observable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RawFrame {
    pub raw: serde_json::Value,
}

impl RawFrame {
    pub fn new(raw: serde_json::Value) -> Self {
        Self { raw }
    }

    /// A frame a decoder keeps without knowing it; a strict decode refuses
    /// the line.
    pub(crate) fn unknown(what: &str, raw: serde_json::Value) -> Self {
        crate::strictness::unknown(|| {
            let kind = raw.get("type").and_then(|kind| kind.as_str()).unwrap_or("");
            format!("unknown {what} {kind:?}")
        });
        Self { raw }
    }

    pub fn field(&self, name: &str) -> Option<&serde_json::Value> {
        self.raw.get(name)
    }
}

macro_rules! open_string_enum {
    ($name:ident { $($variant:ident => $wire:literal),+ $(,)? }) => {
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub enum $name {
            $($variant,)+
            Unknown(String),
        }

        impl $name {
            /// The value a spelling names, an unknown one kept as is.
            pub fn parse(value: &str) -> Self {
                match value {
                    $($wire => Self::$variant,)+
                    _ => Self::Unknown(value.to_owned()),
                }
            }

            /// The spelling on the wire.
            pub fn as_str(&self) -> &str {
                match self {
                    $(Self::$variant => $wire,)+
                    Self::Unknown(value) => value,
                }
            }
        }

        /// No spelling: what a field Claude always writes reads as when a
        /// line leaves it out.
        impl Default for $name {
            fn default() -> Self {
                Self::Unknown(String::new())
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.serialize_str(self.as_str())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Ok(match value.as_str() {
                    $($wire => Self::$variant,)+
                    _ => {
                        $crate::strictness::unknown(|| format!(concat!("unknown ", stringify!($name), " {:?}"), value));
                        Self::Unknown(value)
                    }
                })
            }
        }
    };
}
// ── PermissionMode ──────────────────────────────────────────────────

open_string_enum!(PermissionMode {
    Default => "default",
    AcceptEdits => "acceptEdits",
    BypassPermissions => "bypassPermissions",
    Plan => "plan",
    DontAsk => "dontAsk",
    Auto => "auto",
});

// ── Role ────────────────────────────────────────────────────────────

open_string_enum!(Role {
    User => "user",
    Assistant => "assistant",
});

// ── StopReason ──────────────────────────────────────────────────────

open_string_enum!(StopReason {
    EndTurn => "end_turn",
    MaxTokens => "max_tokens",
    StopSequence => "stop_sequence",
    ToolUse => "tool_use",
    Refusal => "refusal",
    PauseTurn => "pause_turn",
});

// ── ContentBlock ────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
        #[serde(flatten)]
        extensions: Extensions,
    },
    Image {
        source: ImageSource,
        #[serde(flatten)]
        extensions: Extensions,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
        #[serde(flatten)]
        extensions: Extensions,
    },
    ToolResult {
        tool_use_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content: Option<ToolResultBody>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        is_error: Option<bool>,
        #[serde(flatten)]
        extensions: Extensions,
    },
    Thinking {
        thinking: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
        #[serde(flatten)]
        extensions: Extensions,
    },
    RedactedThinking {
        #[serde(flatten)]
        extensions: Extensions,
    },
    #[serde(untagged)]
    Unknown(RawFrame),
}

impl ContentBlock {
    /// The block's `type` as written.
    pub fn kind(&self) -> &str {
        match self {
            Self::Text { .. } => "text",
            Self::Image { .. } => "image",
            Self::ToolUse { .. } => "tool_use",
            Self::ToolResult { .. } => "tool_result",
            Self::Thinking { .. } => "thinking",
            Self::RedactedThinking { .. } => "redacted_thinking",
            Self::Unknown(raw) => raw
                .field("type")
                .and_then(|kind| kind.as_str())
                .unwrap_or(""),
        }
    }
}

impl<'de> Deserialize<'de> for ContentBlock {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = serde_json::Value::deserialize(deserializer)?;
        let kind = required_discriminant::<D::Error>(&raw, "type")?.to_owned();
        match kind.as_str() {
            "text" => parse_payload(raw, "type")
                .map(|value: TextContentBlock| Self::Text {
                    text: value.text,
                    extensions: value.extensions,
                })
                .map_err(serde::de::Error::custom),
            "image" => parse_payload(raw, "type")
                .map(|value: ImageContentBlock| Self::Image {
                    source: value.source,
                    extensions: value.extensions,
                })
                .map_err(serde::de::Error::custom),
            "tool_use" => parse_payload(raw, "type")
                .map(|value: ToolUseContentBlock| Self::ToolUse {
                    id: value.id,
                    name: value.name,
                    input: value.input,
                    extensions: value.extensions,
                })
                .map_err(serde::de::Error::custom),
            "tool_result" => parse_payload(raw, "type")
                .map(|value: ToolResultContentBlock| Self::ToolResult {
                    tool_use_id: value.tool_use_id,
                    content: value.content,
                    is_error: value.is_error,
                    extensions: value.extensions,
                })
                .map_err(serde::de::Error::custom),
            "thinking" => parse_payload(raw, "type")
                .map(|value: ThinkingContentBlock| Self::Thinking {
                    thinking: value.thinking,
                    signature: value.signature,
                    extensions: value.extensions,
                })
                .map_err(serde::de::Error::custom),
            "redacted_thinking" => parse_payload(raw, "type")
                .map(|value: ExtensionOnly| Self::RedactedThinking {
                    extensions: value.extensions,
                })
                .map_err(serde::de::Error::custom),
            _ => Ok(Self::Unknown(RawFrame::unknown("ContentBlock", raw))),
        }
    }
}

/// A tool result as Claude wrote it: plain text, or content blocks.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ToolResultBody {
    Text(String),
    Blocks(Vec<ToolResultContent>),
}

impl ToolResultBody {
    /// The result's text: the plain text, or every text block joined.
    pub fn text(&self) -> String {
        match self {
            Self::Text(text) => text.clone(),
            Self::Blocks(blocks) => blocks
                .iter()
                .filter_map(|block| match block {
                    ToolResultContent::Text { text, .. } => Some(text.as_str()),
                    _ => None,
                })
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolResultContent {
    Text {
        text: String,
        #[serde(flatten)]
        extensions: Extensions,
    },
    /// A tool the result makes available, named.
    ToolReference {
        tool_name: String,
        #[serde(flatten)]
        extensions: Extensions,
    },
    Image {
        source: ImageSource,
        #[serde(flatten)]
        extensions: Extensions,
    },
    #[serde(untagged)]
    Unknown(RawFrame),
}

impl<'de> Deserialize<'de> for ToolResultContent {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = serde_json::Value::deserialize(deserializer)?;
        let kind = required_discriminant::<D::Error>(&raw, "type")?.to_owned();
        match kind.as_str() {
            "text" => parse_payload(raw, "type")
                .map(|value: TextContentBlock| Self::Text {
                    text: value.text,
                    extensions: value.extensions,
                })
                .map_err(serde::de::Error::custom),
            "image" => parse_payload(raw, "type")
                .map(|value: ImageContentBlock| Self::Image {
                    source: value.source,
                    extensions: value.extensions,
                })
                .map_err(serde::de::Error::custom),
            "tool_reference" => parse_payload(raw, "type")
                .map(|value: ToolReferenceBlock| Self::ToolReference {
                    tool_name: value.tool_name,
                    extensions: value.extensions,
                })
                .map_err(serde::de::Error::custom),
            _ => Ok(Self::Unknown(RawFrame::unknown("ToolResultContent", raw))),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageSource {
    pub r#type: ImageSourceType,
    pub media_type: String,
    pub data: String,
    #[serde(flatten)]
    pub extensions: Extensions,
}

open_string_enum!(ImageSourceType { Base64 => "base64" });

// ── Usage ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_creation_input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_input_tokens: Option<u64>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

// ── ModelUsage ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelUsage {
    pub context_window: u64,
    #[serde(flatten)]
    pub extensions: Extensions,
}

// ── ApiMessage ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiMessage {
    #[serde(default = "crate::absent::id")]
    pub id: String,
    #[serde(default = "crate::absent::message_type")]
    pub r#type: String,
    #[serde(default = "crate::absent::role")]
    pub role: Role,
    #[serde(default = "crate::absent::content")]
    pub content: Vec<ContentBlock>,
    #[serde(default = "crate::absent::model")]
    pub model: String,
    /// Outer None means absent; Some(None) preserves an explicit null at stream start.
    #[serde(
        default,
        deserialize_with = "present_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub stop_reason: Option<Option<StopReason>>,
    /// Distinguishes an omitted field from an explicit null on the wire.
    #[serde(
        default,
        deserialize_with = "present_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub stop_sequence: Option<Option<String>>,
    #[serde(default = "crate::absent::usage")]
    pub usage: Usage,
    #[serde(flatten)]
    pub extensions: Extensions,
}

pub fn present_nullable<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::deserialize(deserializer).map(Some)
}

// ── MessageParam ────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageParam {
    pub role: Role,
    pub content: MessageContent,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    Blocks(Vec<ContentBlock>),
}

impl Default for MessageContent {
    fn default() -> Self {
        Self::Text(String::new())
    }
}

// ── ApiKeySource ────────────────────────────────────────────────────

open_string_enum!(ApiKeySource {
    AnthropicApiKey => "ANTHROPIC_API_KEY",
    ApiKeyHelper => "apiKeyHelper",
    LoginManagedKey => "/login managed key",
    None => "none",
    User => "user",
    Project => "project",
    Org => "org",
    Temporary => "temporary",
    OAuth => "oauth",
});

// ── AssistantMessageError ───────────────────────────────────────────

open_string_enum!(AssistantMessageError {
    AuthenticationFailed => "authentication_failed",
    OauthOrgNotAllowed => "oauth_org_not_allowed",
    AccountOnHold => "account_on_hold",
    BillingError => "billing_error",
    RateLimit => "rate_limit",
    Overloaded => "overloaded",
    InvalidRequest => "invalid_request",
    ModelNotFound => "model_not_found",
    ServerError => "server_error",
    UnknownError => "unknown",
    MaxOutputTokens => "max_output_tokens",
});

// ── RawMessageStreamEvent ───────────────────────────────────────────

/// One event from the Messages API lifecycle carried by a `stream_event`
/// envelope. Known events retain extension fields; newer events remain
/// available through [`StreamEvent::Unknown`].
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamEvent {
    MessageStart {
        message: ApiMessage,
        #[serde(flatten)]
        extensions: Extensions,
    },
    ContentBlockStart {
        index: u32,
        content_block: ContentBlock,
        #[serde(flatten)]
        extensions: Extensions,
    },
    ContentBlockDelta {
        index: u32,
        delta: StreamDelta,
        #[serde(flatten)]
        extensions: Extensions,
    },
    ContentBlockStop {
        index: u32,
        #[serde(flatten)]
        extensions: Extensions,
    },
    MessageDelta {
        delta: StreamMessageDelta,
        usage: StreamMessageUsage,
        #[serde(flatten)]
        extensions: Extensions,
    },
    MessageStop {
        #[serde(flatten)]
        extensions: Extensions,
    },
    #[serde(untagged)]
    Unknown(RawFrame),
}

impl<'de> Deserialize<'de> for StreamEvent {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = serde_json::Value::deserialize(deserializer)?;
        let kind = required_discriminant::<D::Error>(&raw, "type")?.to_owned();
        match kind.as_str() {
            "message_start" => parse_payload(raw, "type")
                .map(|value: MessageStartEvent| Self::MessageStart {
                    message: value.message,
                    extensions: value.extensions,
                })
                .map_err(serde::de::Error::custom),
            "content_block_start" => parse_payload(raw, "type")
                .map(|value: ContentBlockStartEvent| Self::ContentBlockStart {
                    index: value.index,
                    content_block: value.content_block,
                    extensions: value.extensions,
                })
                .map_err(serde::de::Error::custom),
            "content_block_delta" => parse_payload(raw, "type")
                .map(|value: ContentBlockDeltaEvent| Self::ContentBlockDelta {
                    index: value.index,
                    delta: value.delta,
                    extensions: value.extensions,
                })
                .map_err(serde::de::Error::custom),
            "content_block_stop" => parse_payload(raw, "type")
                .map(|value: ContentBlockStopEvent| Self::ContentBlockStop {
                    index: value.index,
                    extensions: value.extensions,
                })
                .map_err(serde::de::Error::custom),
            "message_delta" => parse_payload(raw, "type")
                .map(|value: MessageDeltaEvent| Self::MessageDelta {
                    delta: value.delta,
                    usage: value.usage,
                    extensions: value.extensions,
                })
                .map_err(serde::de::Error::custom),
            "message_stop" => parse_payload(raw, "type")
                .map(|value: ExtensionOnly| Self::MessageStop {
                    extensions: value.extensions,
                })
                .map_err(serde::de::Error::custom),
            _ => Ok(Self::Unknown(RawFrame::unknown("StreamEvent", raw))),
        }
    }
}

/// A typed incremental change to a content block.
///
/// The variant names are spelled out rather than derived from the Rust names,
/// so that serializing a delta produces the discriminant the wire uses and this
/// same type can read it back. Deriving them gave `thinking` where the wire
/// says `thinking_delta`, which only showed up when a recorded frame was parsed
/// and re-serialized.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum StreamDelta {
    #[serde(rename = "text_delta")]
    Text {
        text: String,
        #[serde(flatten)]
        extensions: Extensions,
    },
    #[serde(rename = "thinking_delta")]
    Thinking {
        thinking: String,
        #[serde(flatten)]
        extensions: Extensions,
    },
    #[serde(rename = "signature_delta")]
    Signature {
        signature: String,
        #[serde(flatten)]
        extensions: Extensions,
    },
    #[serde(rename = "input_json_delta")]
    ToolInput {
        partial_json: String,
        #[serde(flatten)]
        extensions: Extensions,
    },
    #[serde(untagged)]
    Unknown(RawFrame),
}

impl<'de> Deserialize<'de> for StreamDelta {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = serde_json::Value::deserialize(deserializer)?;
        let kind = required_discriminant::<D::Error>(&raw, "type")?.to_owned();
        match kind.as_str() {
            "text_delta" => parse_payload(raw, "type")
                .map(|value: TextStreamDelta| Self::Text {
                    text: value.text,
                    extensions: value.extensions,
                })
                .map_err(serde::de::Error::custom),
            "thinking_delta" => parse_payload(raw, "type")
                .map(|value: ThinkingStreamDelta| Self::Thinking {
                    thinking: value.thinking,
                    extensions: value.extensions,
                })
                .map_err(serde::de::Error::custom),
            "signature_delta" => parse_payload(raw, "type")
                .map(|value: SignatureStreamDelta| Self::Signature {
                    signature: value.signature,
                    extensions: value.extensions,
                })
                .map_err(serde::de::Error::custom),
            "input_json_delta" => parse_payload(raw, "type")
                .map(|value: ToolInputStreamDelta| Self::ToolInput {
                    partial_json: value.partial_json,
                    extensions: value.extensions,
                })
                .map_err(serde::de::Error::custom),
            _ => Ok(Self::Unknown(RawFrame::unknown("StreamDelta", raw))),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamMessageDelta {
    #[serde(deserialize_with = "required_stream_option")]
    pub stop_reason: Option<StopReason>,
    #[serde(deserialize_with = "required_stream_option")]
    pub stop_sequence: Option<String>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamMessageUsage {
    pub output_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_creation_input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_input_tokens: Option<u64>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Deserialize)]
struct MessageStartEvent {
    message: ApiMessage,
    #[serde(flatten)]
    extensions: Extensions,
}

#[derive(Deserialize)]
struct ContentBlockStartEvent {
    index: u32,
    content_block: ContentBlock,
    #[serde(flatten)]
    extensions: Extensions,
}

#[derive(Deserialize)]
struct ContentBlockDeltaEvent {
    index: u32,
    delta: StreamDelta,
    #[serde(flatten)]
    extensions: Extensions,
}

#[derive(Deserialize)]
struct ContentBlockStopEvent {
    index: u32,
    #[serde(flatten)]
    extensions: Extensions,
}

#[derive(Deserialize)]
struct MessageDeltaEvent {
    delta: StreamMessageDelta,
    usage: StreamMessageUsage,
    #[serde(flatten)]
    extensions: Extensions,
}

#[derive(Deserialize)]
struct TextStreamDelta {
    text: String,
    #[serde(flatten)]
    extensions: Extensions,
}

#[derive(Deserialize)]
struct ThinkingStreamDelta {
    thinking: String,
    #[serde(flatten)]
    extensions: Extensions,
}

#[derive(Deserialize)]
struct SignatureStreamDelta {
    signature: String,
    #[serde(flatten)]
    extensions: Extensions,
}

#[derive(Deserialize)]
struct ToolInputStreamDelta {
    partial_json: String,
    #[serde(flatten)]
    extensions: Extensions,
}

fn required_stream_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::deserialize(deserializer)
}

#[derive(Deserialize)]
struct TextContentBlock {
    text: String,
    #[serde(flatten)]
    extensions: Extensions,
}

#[derive(Deserialize)]
struct ImageContentBlock {
    source: ImageSource,
    #[serde(flatten)]
    extensions: Extensions,
}

#[derive(Deserialize)]
struct ToolUseContentBlock {
    id: String,
    name: String,
    input: serde_json::Value,
    #[serde(flatten)]
    extensions: Extensions,
}

#[derive(Deserialize)]
struct ToolReferenceBlock {
    tool_name: String,
    #[serde(flatten)]
    extensions: Extensions,
}

#[derive(Deserialize)]
struct ToolResultContentBlock {
    tool_use_id: String,
    #[serde(default)]
    content: Option<ToolResultBody>,
    #[serde(default)]
    is_error: Option<bool>,
    #[serde(flatten)]
    extensions: Extensions,
}

#[derive(Deserialize)]
struct ThinkingContentBlock {
    thinking: String,
    #[serde(default)]
    signature: Option<String>,
    #[serde(flatten)]
    extensions: Extensions,
}

#[derive(Deserialize)]
struct ExtensionOnly {
    #[serde(flatten)]
    extensions: Extensions,
}

fn required_discriminant<'a, E>(raw: &'a serde_json::Value, field: &str) -> Result<&'a str, E>
where
    E: serde::de::Error,
{
    let object = raw
        .as_object()
        .ok_or_else(|| E::custom("protocol variant must be a JSON object"))?;
    object
        .get(field)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| E::custom(format!("protocol variant requires string field `{field}`")))
}

fn parse_payload<T: DeserializeOwned>(
    mut raw: serde_json::Value,
    discriminant: &str,
) -> Result<T, serde_json::Error> {
    if let Some(object) = raw.as_object_mut() {
        object.remove(discriminant);
    }
    serde_json::from_value(raw)
}

// ── Permission types ────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum PermissionUpdate {
    AddRules {
        rules: Vec<PermissionRuleValue>,
        behavior: PermissionBehavior,
        destination: PermissionUpdateDestination,
    },
    ReplaceRules {
        rules: Vec<PermissionRuleValue>,
        behavior: PermissionBehavior,
        destination: PermissionUpdateDestination,
    },
    RemoveRules {
        rules: Vec<PermissionRuleValue>,
        behavior: PermissionBehavior,
        destination: PermissionUpdateDestination,
    },
    SetMode {
        mode: PermissionMode,
        destination: PermissionUpdateDestination,
    },
    AddDirectories {
        directories: Vec<String>,
        destination: PermissionUpdateDestination,
    },
    RemoveDirectories {
        directories: Vec<String>,
        destination: PermissionUpdateDestination,
    },
    /// A change this crate does not know, as written.
    #[serde(untagged)]
    Unknown(UnknownPermissionUpdate),
}

impl PermissionUpdate {
    /// Where the change would be saved, as written.
    pub fn destination(&self) -> Option<&str> {
        Some(match self {
            Self::AddRules { destination, .. }
            | Self::ReplaceRules { destination, .. }
            | Self::RemoveRules { destination, .. }
            | Self::SetMode { destination, .. }
            | Self::AddDirectories { destination, .. }
            | Self::RemoveDirectories { destination, .. } => destination.as_str(),
            Self::Unknown(unknown) => unknown.0.field("destination")?.as_str()?,
        })
    }
}

/// A permission change this crate does not know; a strict decode refuses
/// the line.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(transparent)]
pub struct UnknownPermissionUpdate(pub RawFrame);

impl<'de> Deserialize<'de> for UnknownPermissionUpdate {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = serde_json::Value::deserialize(deserializer)?;
        Ok(Self(RawFrame::unknown("PermissionUpdate", raw)))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionRuleValue {
    pub tool_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule_content: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionBehavior {
    Allow,
    Deny,
    Ask,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PermissionUpdateDestination {
    UserSettings,
    ProjectSettings,
    LocalSettings,
    Session,
    CliArg,
}

impl PermissionUpdateDestination {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::UserSettings => "userSettings",
            Self::ProjectSettings => "projectSettings",
            Self::LocalSettings => "localSettings",
            Self::Session => "session",
            Self::CliArg => "cliArg",
        }
    }
}

// ── PermissionResult ────────────────────────────────────────────────

/// amux's answer to `can_use_tool`, and the decision a `PermissionRequest`
/// hook returns.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "behavior",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum PermissionResult {
    Allow {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        updated_input: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        updated_permissions: Option<Vec<PermissionUpdate>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_use_id: Option<String>,
    },
    Deny {
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        interrupt: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_use_id: Option<String>,
    },
}

// ── Hook enums used in multiple modules ─────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStartSource {
    Startup,
    Resume,
    Clear,
    Compact,
}

open_string_enum!(CompactTrigger {
    Manual => "manual",
    Auto => "auto",
});

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SetupTrigger {
    Init,
    Maintenance,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigChangeSource {
    UserSettings,
    ProjectSettings,
    LocalSettings,
    PolicySettings,
    Skills,
}
