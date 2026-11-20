//! `elicitation/create`: the agent asks the person for structured input,
//! by a form the client draws or a page the client opens.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Number, Value};

use crate::macros::{string_enum, tagged_enum};
use crate::{Empty, Extra, RequestId};

tagged_enum! {
    /// The params of `elicitation/create`. Each mode is scoped to a session
    /// (with the tool call asking, where there is one) or to a request the
    /// agent is still answering, such as `session/new`.
    pub enum CreateElicitationRequest by "mode" {
        /// A form, drawn from a schema.
        "form" => Form(ElicitationForm),
        /// A page the client opens; the agent says when it is done with
        /// `elicitation/complete`.
        "url" => Url(ElicitationUrl),
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ElicitationForm {
    /// What the agent asks, for people to read.
    pub message: String,
    pub requested_schema: ElicitationSchema,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(
        default,
        deserialize_with = "crate::present",
        skip_serializing_if = "Option::is_none"
    )]
    pub request_id: Option<RequestId>,
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
pub struct ElicitationUrl {
    pub message: String,
    pub elicitation_id: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(
        default,
        deserialize_with = "crate::present",
        skip_serializing_if = "Option::is_none"
    )]
    pub request_id: Option<RequestId>,
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

/// A form's fields, as a restricted JSON Schema object.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ElicitationSchema {
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<ElicitationSchemaType>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The fields by their names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub properties: Option<BTreeMap<String, ElicitationPropertySchema>>,
    /// The names of the fields an answer must fill.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required: Option<Vec<String>>,
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
    pub enum ElicitationSchemaType {
        Object = "object",
    }
}

tagged_enum! {
    /// One field of a form.
    pub enum ElicitationPropertySchema by "type" {
        /// Text, or one choice of several where it lists them.
        "string" => String(StringPropertySchema),
        "number" => Number(NumberPropertySchema),
        "integer" => Integer(IntegerPropertySchema),
        "boolean" => Boolean(BooleanPropertySchema),
        /// Several choices of a list.
        "array" => MultiSelect(MultiSelectPropertySchema),
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StringPropertySchema {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_length: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_length: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<StringFormat>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    /// The choices, by value.
    #[serde(rename = "enum", default, skip_serializing_if = "Option::is_none")]
    pub values: Option<Vec<String>>,
    /// The choices, each with a title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub one_of: Option<Vec<EnumOption>>,
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
    pub enum StringFormat {
        Email = "email",
        Uri = "uri",
        Date = "date",
        DateTime = "date-time",
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NumberPropertySchema {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimum: Option<Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maximum: Option<Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Number>,
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
pub struct IntegerPropertySchema {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimum: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maximum: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<i64>,
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
pub struct BooleanPropertySchema {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<bool>,
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
pub struct MultiSelectPropertySchema {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_items: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_items: Option<u64>,
    pub items: MultiSelectItems,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Vec<String>>,
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
    /// The choices of a multi-select.
    pub enum MultiSelectItems by "type" {
        /// By value.
        "string" => Strings(StringMultiSelectItems),
    }
    /// Each with a title.
    without Titled(TitledMultiSelectItems)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StringMultiSelectItems {
    #[serde(rename = "enum")]
    pub values: Vec<String>,
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
pub struct TitledMultiSelectItems {
    pub any_of: Vec<EnumOption>,
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

/// A choice and its title.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnumOption {
    /// The value an answer carries.
    #[serde(rename = "const")]
    pub value: String,
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

tagged_enum! {
    /// The answer to `elicitation/create`.
    pub enum CreateElicitationResponse by "action" {
        /// The person answered; for a form, with its fields.
        "accept" => Accept(ElicitationAccept),
        "decline" => Decline(Empty),
        /// Dismissed without a choice, or the turn ended.
        "cancel" => Cancel(Empty),
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ElicitationAccept {
    /// A form's answers, by field name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<BTreeMap<String, ElicitationValue>>,
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

/// The value of one field of a form's answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ElicitationValue {
    Boolean(bool),
    Integer(i64),
    Number(Number),
    String(String),
    /// A multi-select's choices.
    Strings(Vec<String>),
}

/// The params of `elicitation/complete`: the page of a `url` elicitation
/// is done with.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompleteElicitationNotification {
    pub elicitation_id: String,
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
