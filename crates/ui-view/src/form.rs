//! A tool server's form: its fields read once from the server's schema, what
//! each holds, and the one check and encoding every client submits through.
//! A client only draws the fields, holds what the person entered as
//! [`FormValue`]s and words the [`FieldProblem`]s; whether the answers can go
//! and the JSON object that goes are decided here, so two clients given the
//! same entries send the same thing or refuse for the same reason.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value};
use wire::{ClaudeAnswer, CodexAnswer, claude_answer};

use crate::ask::{Answer, AskBody, AskCard, ChoiceOutcome};

/// One field of a tool server's form, read from its JSON schema.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FormField {
    /// The property's name, which the answer's content is keyed by.
    pub name: String,
    /// The schema's title, else the name.
    pub title: String,
    pub description: String,
    pub required: bool,
    pub kind: FormFieldKind,
    /// What the field holds before it is touched: the schema's default,
    /// else nothing entered, nothing chosen, and a toggle off.
    pub initial: FormValue,
}

/// What a field takes, with the limits its schema sets.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub enum FormFieldKind {
    /// Typed text, its length in characters.
    Text {
        min_length: Option<u32>,
        max_length: Option<u32>,
    },
    /// A typed number; `integer` when it must be whole.
    Number {
        integer: bool,
        minimum: Option<f64>,
        maximum: Option<f64>,
    },
    /// Yes or no.
    Toggle,
    /// One of the options.
    Choice { options: Vec<String> },
    /// Any of the options: an array of enum values.
    Many {
        options: Vec<String>,
        min_items: Option<u32>,
        max_items: Option<u32>,
    },
}

/// What a field holds as the person left it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub enum FormValue {
    /// A text or number field's characters as typed. A number is held as
    /// typed so a half-typed one ("3.", "-") survives, and is read only when
    /// the form is checked.
    Text(String),
    /// A toggle: untouched it is off, and off is an answer.
    Toggle(bool),
    /// A choice's option by position; none while nothing is chosen.
    Choice(Option<u32>),
    /// The picked options of a field that takes several, by position.
    Many(Vec<u32>),
}

/// Why a field's value cannot go as it is.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub enum FieldProblem {
    /// Required, and nothing was entered or chosen.
    Required,
    /// Typed text that reads as no finite number.
    NotANumber,
    /// A number where the field takes only whole ones.
    NotWholeNumber,
    BelowMinimum {
        minimum: f64,
    },
    AboveMaximum {
        maximum: f64,
    },
    /// Fewer characters than the field takes.
    TooShort {
        min_length: u32,
    },
    TooLong {
        max_length: u32,
    },
    /// Fewer options picked than the field takes.
    TooFew {
        min_items: u32,
    },
    TooMany {
        max_items: u32,
    },
}

/// A field's problem, the field by its position in the form.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FormProblem {
    pub field: u32,
    pub problem: FieldProblem,
}

/// A JSON object's members in the order written. A parsed `Value` keeps
/// its keys sorted, and a form asks its fields in its schema's order.
struct Members(Vec<(String, Value)>);

impl<'de> Deserialize<'de> for Members {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Members, D::Error> {
        struct Visit;
        impl<'de> serde::de::Visitor<'de> for Visit {
            type Value = Members;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("an object")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<Members, A::Error> {
                let mut members = Vec::new();
                while let Some(member) = map.next_entry()? {
                    members.push(member);
                }
                Ok(Members(members))
            }
        }
        deserializer.deserialize_map(Visit)
    }
}

/// A form schema's fields, in the schema's own order; none when it is not
/// an object schema with properties.
pub fn form_fields(schema_json: &[u8]) -> Vec<FormField> {
    #[derive(Deserialize)]
    struct Schema {
        #[serde(default)]
        properties: Option<Members>,
        #[serde(default)]
        required: Vec<String>,
    }
    let Ok(Schema {
        properties: Some(Members(properties)),
        required,
    }) = serde_json::from_slice::<Schema>(schema_json)
    else {
        return Vec::new();
    };
    properties
        .iter()
        .map(|(name, property)| {
            let kind = field_kind(property);
            FormField {
                title: property
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or(name)
                    .to_owned(),
                description: property
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                required: required.contains(name),
                name: name.clone(),
                initial: initial(property.get("default"), &kind),
                kind,
            }
        })
        .collect()
}

fn field_kind(property: &Value) -> FormFieldKind {
    let names = |options: &Vec<Value>| options.iter().map(option_name).collect::<Vec<_>>();
    let count = |key: &str| {
        property
            .get(key)
            .and_then(Value::as_u64)
            .map(|n| u32::try_from(n).unwrap_or(u32::MAX))
    };
    let typed = property.get("type").and_then(Value::as_str);
    let many = property
        .get("items")
        .and_then(|items| items.get("enum"))
        .and_then(Value::as_array)
        .filter(|_| typed == Some("array"));
    if let Some(options) = many {
        return FormFieldKind::Many {
            options: names(options),
            min_items: count("minItems"),
            max_items: count("maxItems"),
        };
    }
    if let Some(options) = property.get("enum").and_then(Value::as_array) {
        return FormFieldKind::Choice {
            options: names(options),
        };
    }
    match typed {
        Some("boolean") => FormFieldKind::Toggle,
        Some(number @ ("number" | "integer")) => FormFieldKind::Number {
            integer: number == "integer",
            minimum: property.get("minimum").and_then(Value::as_f64),
            maximum: property.get("maximum").and_then(Value::as_f64),
        },
        _ => FormFieldKind::Text {
            min_length: count("minLength"),
            max_length: count("maxLength"),
        },
    }
}

/// An enum value as its option reads: a string as written, anything else
/// as its JSON.
fn option_name(option: &Value) -> String {
    match option {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// What a field starts at: the schema's default where it fits the field.
/// Nothing else is filled in for the person; in particular a choice with no
/// default starts unchosen, so a required one is answered only by choosing.
fn initial(default: Option<&Value>, kind: &FormFieldKind) -> FormValue {
    let position = |options: &[String], value: &Value| {
        let name = option_name(value);
        options
            .iter()
            .position(|option| *option == name)
            .map(|at| at as u32)
    };
    match kind {
        FormFieldKind::Text { .. } | FormFieldKind::Number { .. } => {
            FormValue::Text(match default {
                Some(Value::String(s)) => s.clone(),
                Some(Value::Number(n)) => n.to_string(),
                _ => String::new(),
            })
        }
        FormFieldKind::Toggle => FormValue::Toggle(default.and_then(Value::as_bool) == Some(true)),
        FormFieldKind::Choice { options } => {
            FormValue::Choice(default.and_then(|value| position(options, value)))
        }
        FormFieldKind::Many { options, .. } => {
            let mut picked: Vec<u32> = default
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|value| position(options, value))
                .collect();
            picked.sort_unstable();
            picked.dedup();
            FormValue::Many(picked)
        }
    }
}

/// Every field's problem with the values entered, in field order: one per
/// field at most. `values` is in field order; a field with no value there
/// holds its `initial`. An empty list means the form can be submitted.
pub fn form_problems(fields: &[FormField], values: &[FormValue]) -> Vec<FormProblem> {
    checked(fields, values)
        .into_iter()
        .enumerate()
        .filter_map(|(at, read)| match read {
            Err(problem) => Some(FormProblem {
                field: at as u32,
                problem,
            }),
            Ok(_) => None,
        })
        .collect()
}

/// The answer that submits the card's form with the values entered (as
/// for [`form_problems`]), or every field's problem when it cannot go.
/// None when the card is not a form.
pub fn form_answer(
    card: &AskCard,
    values: &[FormValue],
) -> Result<Option<Answer>, Vec<FormProblem>> {
    let AskBody::Form { fields, .. } = &card.body else {
        return Ok(None);
    };
    let Some(submit) = card
        .choices
        .iter()
        .find(|choice| choice.outcome == ChoiceOutcome::Submit)
    else {
        return Ok(None);
    };
    let mut content = Map::new();
    let mut problems = Vec::new();
    for (at, (field, read)) in fields.iter().zip(checked(fields, values)).enumerate() {
        match read {
            Ok(Some(value)) => {
                content.insert(field.name.clone(), value);
            }
            Ok(None) => {}
            Err(problem) => problems.push(FormProblem {
                field: at as u32,
                problem,
            }),
        }
    }
    if !problems.is_empty() {
        return Err(problems);
    }
    let bytes = serde_json::to_vec(&Value::Object(content)).unwrap_or_default();
    Ok(Some(with_form_content(&submit.answer, bytes)))
}

/// Each field read: the JSON it sends, none when it is left out (not
/// required and nothing entered), or its problem.
fn checked(fields: &[FormField], values: &[FormValue]) -> Vec<Result<Option<Value>, FieldProblem>> {
    fields
        .iter()
        .enumerate()
        .map(|(at, field)| {
            let value = values.get(at).unwrap_or(&field.initial);
            match read(field, value)? {
                Some(json) => Ok(Some(json)),
                None if field.required => Err(FieldProblem::Required),
                None => Ok(None),
            }
        })
        .collect()
}

/// A field's value as the JSON the schema describes; none when nothing is
/// entered. A value of another field's shape counts as nothing entered.
fn read(field: &FormField, value: &FormValue) -> Result<Option<Value>, FieldProblem> {
    match (&field.kind, value) {
        (
            FormFieldKind::Text {
                min_length,
                max_length,
            },
            FormValue::Text(typed),
        ) => {
            // Surrounding spaces are never what a field means, and a field
            // of only spaces holds nothing.
            let typed = typed.trim();
            if typed.is_empty() {
                return Ok(None);
            }
            let length = typed.chars().count();
            if let Some(min_length) = *min_length
                && length < min_length as usize
            {
                return Err(FieldProblem::TooShort { min_length });
            }
            if let Some(max_length) = *max_length
                && length > max_length as usize
            {
                return Err(FieldProblem::TooLong { max_length });
            }
            Ok(Some(Value::String(typed.to_owned())))
        }
        (
            FormFieldKind::Number {
                integer,
                minimum,
                maximum,
            },
            FormValue::Text(typed),
        ) => {
            let typed = typed.trim();
            if typed.is_empty() {
                return Ok(None);
            }
            // "inf" and "NaN" parse as floats but are no number JSON can
            // carry.
            let number = typed
                .parse::<f64>()
                .ok()
                .filter(|n| n.is_finite())
                .ok_or(FieldProblem::NotANumber)?;
            let whole = number.fract() == 0.0;
            if *integer && !whole {
                return Err(FieldProblem::NotWholeNumber);
            }
            if let Some(minimum) = *minimum
                && number < minimum
            {
                return Err(FieldProblem::BelowMinimum { minimum });
            }
            if let Some(maximum) = *maximum
                && number > maximum
            {
                return Err(FieldProblem::AboveMaximum { maximum });
            }
            // A whole number goes as one (3, not 3.0) while it is exact.
            const EXACT: f64 = 9_007_199_254_740_992.0;
            Ok(Some(if whole && number.abs() <= EXACT {
                Value::from(number as i64)
            } else {
                Number::from_f64(number).map_or(Value::Null, Value::Number)
            }))
        }
        (FormFieldKind::Toggle, FormValue::Toggle(on)) => Ok(Some(Value::Bool(*on))),
        // A toggle never touched is off.
        (FormFieldKind::Toggle, _) => Ok(Some(Value::Bool(false))),
        (FormFieldKind::Choice { options }, FormValue::Choice(Some(at))) => Ok(options
            .get(*at as usize)
            .map(|option| Value::String(option.clone()))),
        (
            FormFieldKind::Many {
                options,
                min_items,
                max_items,
            },
            FormValue::Many(picked),
        ) => {
            let mut picked: Vec<u32> = picked
                .iter()
                .copied()
                .filter(|at| (*at as usize) < options.len())
                .collect();
            picked.sort_unstable();
            picked.dedup();
            if picked.is_empty() {
                return Ok(None);
            }
            if let Some(min_items) = *min_items
                && picked.len() < min_items as usize
            {
                return Err(FieldProblem::TooFew { min_items });
            }
            if let Some(max_items) = *max_items
                && picked.len() > max_items as usize
            {
                return Err(FieldProblem::TooMany { max_items });
            }
            Ok(Some(Value::Array(
                picked
                    .iter()
                    .map(|at| Value::String(options[*at as usize].clone()))
                    .collect(),
            )))
        }
        _ => Ok(None),
    }
}

/// A form's Submit carrying the field values as the JSON object the tool
/// server's schema describes. Any other answer comes back as it was.
fn with_form_content(answer: &Answer, content_json: Vec<u8>) -> Answer {
    let mut answer = answer.clone();
    match &mut answer {
        Answer::Claude(ClaudeAnswer {
            of: Some(claude_answer::Of::Form(form)),
            ..
        })
        | Answer::Codex(CodexAnswer {
            of: Some(wire::codex_answer::Of::Form(form)),
            ..
        }) => form.content_json = content_json,
        _ => {}
    }
    answer
}
