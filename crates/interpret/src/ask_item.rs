//! Asks that are the work: a question, a tool server's form or link, an
//! access grant, or a dialog this build cannot read. Each is its own item, written when the ask opens and
//! revised once with how it closed, so the decision row outlives the ask.
//! The kinds call these helpers so every carrier records a close the same
//! way.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::{Map, Value};
use wire::{
    AnsweredQuestion, AskClosed, AskItem, AskOutcome, FormAction, FormAnswer, GrantAnswer,
    Question, QuestionAnswer, QuestionAsk, QuestionResponse, ReplyInstead, ask_item,
};

use crate::claude_common::AnswerNote;

/// The key of the item an ask that is the work is drawn on.
pub(crate) fn key(ask_key: &str) -> String {
    format!("ask:{ask_key}")
}

pub(crate) fn opened(ask: ask_item::Ask) -> AskItem {
    AskItem {
        ask: Some(ask),
        closed: None,
    }
}

/// Closed by a fact that did not say how: withdrawn, answered elsewhere, or
/// the provider gone.
pub(crate) fn dismissed() -> AskClosed {
    outcome(AskOutcome::Dismissed)
}

pub(crate) fn close(item: AskItem, closed: AskClosed) -> AskItem {
    AskItem {
        closed: Some(closed),
        ..item
    }
}

/// Closed with the outcome the provider reported and nothing more.
pub(crate) fn outcome(outcome: AskOutcome) -> AskClosed {
    AskClosed {
        outcome: outcome as i32,
        ..Default::default()
    }
}

/// One question's response as recorded: the picked labels, the typed
/// answer unless the question is secret, and the note. None when an index
/// names no option.
fn recorded(question: &Question, response: &QuestionResponse) -> Option<AnsweredQuestion> {
    let picked = response
        .selected
        .iter()
        .map(|index| {
            question
                .options
                .get(*index as usize)
                .map(|option| option.label.clone())
        })
        .collect::<Option<Vec<_>>>()?;
    let hidden = question.secret && response.other.is_some();
    Some(AnsweredQuestion {
        picked,
        other: if hidden { None } else { response.other.clone() },
        hidden,
        note: response.note.clone().filter(|note| !note.trim().is_empty()),
    })
}

fn all_recorded(
    asked: &QuestionAsk,
    responses: &[QuestionResponse],
) -> Option<Vec<AnsweredQuestion>> {
    asked
        .questions
        .iter()
        .zip(responses)
        .map(|(question, response)| recorded(question, response))
        .collect()
}

/// A question answered, each question with what was picked or typed and
/// its note; a question with neither was skipped. None when an index names
/// no option.
pub(crate) fn answered(asked: &QuestionAsk, answer: &QuestionAnswer) -> Option<AskClosed> {
    Some(AskClosed {
        outcome: AskOutcome::Answered as i32,
        answers: all_recorded(asked, &answer.answers)?,
        ..Default::default()
    })
}

/// The person replied in their own words instead, with what they had
/// answered so far.
pub(crate) fn replied(asked: &QuestionAsk, reply: &ReplyInstead) -> Option<AskClosed> {
    let none = vec![QuestionResponse::default(); asked.questions.len()];
    let so_far = if reply.answers_so_far.is_empty() {
        &none
    } else {
        &reply.answers_so_far
    };
    Some(AskClosed {
        outcome: AskOutcome::Replied as i32,
        answers: all_recorded(asked, so_far)?,
        reply: reply.text.clone(),
        ..Default::default()
    })
}

/// Questions answered in Claude's own interface, from the answers and
/// notes Claude recorded, each by its question's text. Claude joins picked
/// labels with ", " and adds a typed answer after them; a question it has
/// no answer for was skipped.
pub(crate) fn recorded_by_claude(
    asked: &QuestionAsk,
    answers: &BTreeMap<String, String>,
    notes: &BTreeMap<String, AnswerNote>,
) -> AskClosed {
    let answers = asked
        .questions
        .iter()
        .map(|question| {
            let text = answers
                .get(&question.question)
                .map(String::as_str)
                .unwrap_or_default();
            let (picked, other) = claude_parts(question, text);
            AnsweredQuestion {
                picked,
                other,
                hidden: false,
                note: notes
                    .get(&question.question)
                    .map(|note| note.notes.clone())
                    .filter(|note| !note.trim().is_empty()),
            }
        })
        .collect();
    AskClosed {
        outcome: AskOutcome::Answered as i32,
        answers,
        ..Default::default()
    }
}

/// The labels at the front of Claude's answer text, and what was typed
/// after them.
fn claude_parts(question: &Question, text: &str) -> (Vec<String>, Option<String>) {
    // Longest first, so a label that begins with another label wins.
    let mut labels: Vec<&str> = question
        .options
        .iter()
        .map(|option| option.label.as_str())
        .filter(|label| !label.is_empty())
        .collect();
    labels.sort_by_key(|label| std::cmp::Reverse(label.len()));
    let mut picked = Vec::new();
    let mut rest = text.trim();
    while !rest.is_empty() && (question.multi_select || picked.is_empty()) {
        let found = labels.iter().find_map(|label| {
            let after = rest.strip_prefix(*label)?;
            let after = after
                .strip_prefix(" (Recommended)")
                .or_else(|| after.strip_prefix("(Recommended)"))
                .unwrap_or(after);
            match after.strip_prefix(',') {
                Some(more) if question.multi_select => Some((*label, more.trim_start())),
                _ if after.is_empty() => Some((*label, after)),
                _ => None,
            }
        });
        let Some((label, after)) = found else {
            break;
        };
        picked.push(label.to_owned());
        rest = after;
    }
    (picked, (!rest.is_empty()).then(|| rest.to_owned()))
}

/// A form answered: sent with its field names, declined or cancelled. The
/// names follow the order `schema` asks for them in; the values are not
/// kept.
pub(crate) fn form_sent(form: &FormAnswer, schema: &[u8]) -> AskClosed {
    let mut closed = link_answered(form.action);
    if closed.outcome == AskOutcome::Answered as i32 {
        let sent = serde_json::from_slice::<Value>(&form.content_json)
            .ok()
            .and_then(|content| match content {
                Value::Object(fields) => Some(fields),
                _ => None,
            })
            .unwrap_or_default();
        let asked = serde_json::from_slice::<FormSchema>(schema)
            .map(|schema| schema.properties.0)
            .unwrap_or_default();
        closed.fields = asked
            .iter()
            .filter(|name| sent.contains_key(*name))
            .cloned()
            .chain(sent.keys().filter(|name| !asked.contains(name)).cloned())
            .collect();
    }
    closed
}

/// A form's schema, for the order its fields are asked in.
#[derive(Deserialize)]
struct FormSchema {
    #[serde(default)]
    properties: Names,
}

/// An object's keys in the order they were written; a parsed `Value`
/// would sort them.
#[derive(Default)]
struct Names(Vec<String>);

impl<'de> Deserialize<'de> for Names {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = Names;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("an object")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<Names, A::Error> {
                let mut names = Vec::new();
                while let Some((name, serde::de::IgnoredAny)) = map.next_entry::<String, _>()? {
                    names.push(name);
                }
                Ok(Names(names))
            }
        }
        deserializer.deserialize_map(Visitor)
    }
}

/// The member a rewritten choice keeps its constants in, in the order of
/// the titles its `enum` offers: the option's identity is its constant, and
/// a title is only how it reads. Clients ignore what they do not know.
const CONSTANTS: &str = "x-amux-constants";

/// A form schema with each choice written as titled constants, one
/// (`"oneOf": [{"const": "1", "title": "Tea"}, ...]`, as MCP elicitation
/// and Antigravity's question tool write one) or several (an array whose
/// `items` are `"anyOf"` titled constants), also given as an `enum` of its
/// titles, so every client offers the titles to choose from; read
/// otherwise, such a field takes typed text and nothing tells the person
/// what the agent will accept. Two options titled alike read apart ("Other",
/// "Other (2)"), since a client sends the title it shows; a default names
/// its option's title, so the field starts on it. The constants go beside
/// the titles, and [`constants_for_titles`] turns a title chosen back into
/// its option's constant. A field that already has an `enum` is left as it
/// is, and the rest is kept as written, the properties in their order.
pub(crate) fn titled_choices(schema: &[u8]) -> Vec<u8> {
    fn rewritten(schema: &[u8]) -> Option<Vec<u8>> {
        let mut top: Ordered = serde_json::from_slice(schema).ok()?;
        let at = top.0.iter().position(|(key, _)| key == "properties")?;
        let mut properties: Ordered = serde_json::from_str(top.0[at].1.get()).ok()?;
        let mut changed = false;
        for (_, property) in &mut properties.0 {
            let Ok(Value::Object(mut fields)) = serde_json::from_str::<Value>(property.get())
            else {
                continue;
            };
            if fields.get("type").and_then(Value::as_str) == Some("array") {
                // Several of the titled constants: the choices are the
                // items', and a default is a list of constants.
                let Some(Value::Object(items)) = fields.get_mut("items") else {
                    continue;
                };
                let Some(titles) = offer_titles(items, "anyOf") else {
                    continue;
                };
                if let Some(Value::Array(defaults)) = fields.get_mut("default") {
                    for default in defaults {
                        if let Some(title) = title_of(&titles, default) {
                            *default = title;
                        }
                    }
                }
            } else {
                let Some(titles) = offer_titles(&mut fields, "oneOf") else {
                    continue;
                };
                if let Some(default) = fields.get_mut("default")
                    && let Some(title) = title_of(&titles, default)
                {
                    *default = title;
                }
            }
            *property = serde_json::value::to_raw_value(&fields).ok()?;
            changed = true;
        }
        if !changed {
            return None;
        }
        top.0[at].1 = serde_json::value::to_raw_value(&properties).ok()?;
        serde_json::to_vec(&top).ok()
    }
    rewritten(schema).unwrap_or_else(|| schema.to_vec())
}

/// Offers the titled constants in `choices` (`oneOf` for one, `anyOf` for
/// the items of several, as ACP's form schema writes them) as an `enum` of
/// their titles beside the constants, unless an `enum` is already there:
/// the titles and their constants, by place, when it did.
fn offer_titles(fields: &mut Map<String, Value>, choices: &str) -> Option<Vec<(Value, Value)>> {
    if fields.contains_key("enum") {
        return None;
    }
    let options = fields.get(choices).and_then(titled)?;
    let titles = distinct(options.iter().map(|(title, _)| title.clone()).collect());
    let constants: Vec<Value> = options.into_iter().map(|(_, constant)| constant).collect();
    fields.insert("enum".to_owned(), Value::from(titles.clone()));
    fields.insert(CONSTANTS.to_owned(), Value::from(constants.clone()));
    Some(titles.into_iter().map(Value::from).zip(constants).collect())
}

/// The title offered for `constant`.
fn title_of(titles: &[(Value, Value)], constant: &Value) -> Option<Value> {
    titles
        .iter()
        .find(|(_, offered)| offered == constant)
        .map(|(title, _)| title.clone())
}

/// A form's answer with each title [`titled_choices`] offered in place of
/// the constant at its place, as the agent's schema asked for it, one by
/// one for a field of several; fields it did not rewrite, and a value that
/// is none of the titles, as sent.
pub(crate) fn constants_for_titles(content_json: &[u8], schema: &[u8]) -> Vec<u8> {
    let (Ok(Value::Object(mut content)), Ok(Value::Object(top))) = (
        serde_json::from_slice::<Value>(content_json),
        serde_json::from_slice::<Value>(schema),
    ) else {
        return content_json.to_vec();
    };
    let properties = top.get("properties").and_then(Value::as_object);
    let mut changed = false;
    for (name, value) in &mut content {
        let Some(property) = properties.and_then(|properties| properties.get(name)) else {
            continue;
        };
        let offered = match value {
            Value::Array(_) => property.get("items").unwrap_or(&Value::Null),
            _ => property,
        };
        let (Some(titles), Some(constants)) = (
            offered.get("enum").and_then(Value::as_array),
            offered.get(CONSTANTS).and_then(Value::as_array),
        ) else {
            continue;
        };
        let constant_of = |value: &Value| {
            titles
                .iter()
                .position(|title| title == value)
                .and_then(|at| constants.get(at))
                .cloned()
        };
        let chosen: Vec<&mut Value> = match value {
            Value::Array(values) => values.iter_mut().collect(),
            value => vec![value],
        };
        for value in chosen {
            if let Some(constant) = constant_of(value) {
                *value = constant;
                changed = true;
            }
        }
    }
    if changed {
        serde_json::to_vec(&Value::Object(content)).unwrap_or_else(|_| content_json.to_vec())
    } else {
        content_json.to_vec()
    }
}

/// Each option of a `oneOf` that is a list of constants, as (title,
/// constant): a constant with no title reads as itself, a string as
/// written and anything else as its JSON. None when it is anything else.
fn titled(options: &Value) -> Option<Vec<(String, Value)>> {
    let options = options.as_array().filter(|options| !options.is_empty())?;
    options
        .iter()
        .map(|option| {
            let constant = option.get("const")?;
            let title = match (option.get("title").and_then(Value::as_str), constant) {
                (Some(title), _) => title.to_owned(),
                (None, Value::String(constant)) => constant.clone(),
                (None, constant) => constant.to_string(),
            };
            Some((title, constant.clone()))
        })
        .collect()
}

/// `titles` with each one already used told apart by its count ("Other",
/// "Other (2)"), so each names one option.
fn distinct(titles: Vec<String>) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    for title in titles {
        let mut candidate = title.clone();
        let mut n = 1;
        while seen.contains(&candidate) {
            n += 1;
            candidate = format!("{title} ({n})");
        }
        seen.push(candidate);
    }
    seen
}

/// An object's members in the order they were written, each kept as
/// written.
struct Ordered(Vec<(String, Box<serde_json::value::RawValue>)>);

impl<'de> Deserialize<'de> for Ordered {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = Ordered;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("an object")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<Ordered, A::Error> {
                let mut members = Vec::new();
                while let Some(member) = map.next_entry()? {
                    members.push(member);
                }
                Ok(Ordered(members))
            }
        }
        deserializer.deserialize_map(Visitor)
    }
}

impl serde::Serialize for Ordered {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap as _;
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (key, value) in &self.0 {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}

/// A link opened, declined or cancelled.
pub(crate) fn link_answered(action: i32) -> AskClosed {
    outcome(
        match FormAction::try_from(action).unwrap_or(FormAction::Unspecified) {
            FormAction::Accept => AskOutcome::Answered,
            FormAction::Decline => AskOutcome::Declined,
            FormAction::Cancel => AskOutcome::Cancelled,
            FormAction::Unspecified => AskOutcome::Dismissed,
        },
    )
}

/// An access grant answered: what it granted, or declined when nothing.
pub(crate) fn granted(grant: &GrantAnswer) -> AskClosed {
    if grant.read.is_empty() && grant.write.is_empty() && !grant.network {
        return outcome(AskOutcome::Declined);
    }
    AskClosed {
        outcome: AskOutcome::Answered as i32,
        grant: Some(grant.clone()),
        ..Default::default()
    }
}

/// An ask item as goldens show it: the ask was described when it opened in
/// the snapshot, so only its shape and how it closed.
pub(crate) fn describe(item: &AskItem) -> String {
    let asked = match &item.ask {
        Some(ask_item::Ask::Question(question)) => {
            format!("question×{}", question.questions.len())
        }
        Some(ask_item::Ask::Form(form)) => format!("form {}", form.server),
        Some(ask_item::Ask::Link(link)) => format!("link {}", link.server),
        Some(ask_item::Ask::Access(access)) => format!(
            "access read={:?} write={:?} network={}",
            access.read, access.write, access.network
        ),
        Some(ask_item::Ask::Unanswerable(unanswerable)) => format!(
            "unanswerable {}",
            Value::String(unanswerable.reason.clone())
        ),
        None => "none".into(),
    };
    let Some(closed) = &item.closed else {
        return format!("{asked} open");
    };
    let mut text = format!(
        "{asked} {}",
        AskOutcome::try_from(closed.outcome).map_or("?", |outcome| outcome.as_str_name())
    );
    if !closed.answers.is_empty() {
        let answers = closed
            .answers
            .iter()
            .map(|answer| {
                let mut parts = answer.picked.clone();
                if let Some(other) = &answer.other {
                    parts.push(Value::String(other.clone()).to_string());
                }
                if answer.hidden {
                    parts.push("(hidden)".into());
                }
                if parts.is_empty() {
                    parts.push("(skipped)".into());
                }
                let mut text = parts.join("+");
                if let Some(note) = &answer.note {
                    text.push_str(&format!(" note={}", Value::String(note.clone())));
                }
                text
            })
            .collect::<Vec<_>>();
        let given = closed
            .answers
            .iter()
            .filter(|answer| !answer.picked.is_empty() || answer.other.is_some() || answer.hidden)
            .count();
        text.push_str(&format!(
            " answered {given} of {} [{}]",
            closed.answers.len(),
            answers.join("; ")
        ));
    }
    if !closed.reply.is_empty() {
        text.push_str(&format!(" reply={}", Value::String(closed.reply.clone())));
    }
    if !closed.fields.is_empty() {
        text.push_str(&format!(" fields={:?}", closed.fields));
    }
    if let Some(grant) = &closed.grant {
        text.push_str(&format!(
            " granted read={:?} write={:?} network={}{}",
            grant.read,
            grant.write,
            grant.network,
            if grant.for_session {
                " session"
            } else {
                " turn"
            }
        ));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sent_form_lists_its_fields_in_the_forms_order_without_values() {
        let schema = br#"{"type":"object","properties":{"title":{"type":"string"},"body":{"type":"string"},"assignee":{"type":"string"}}}"#;
        let form = FormAnswer {
            action: FormAction::Accept as i32,
            content_json: br#"{"assignee":"jo","title":"Fix it","extra":1}"#.to_vec(),
        };
        let closed = form_sent(&form, schema);
        assert_eq!(closed.outcome, AskOutcome::Answered as i32);
        assert_eq!(closed.fields, ["title", "assignee", "extra"]);
        assert!(!format!("{closed:?}").contains("Fix it"));
    }

    /// Antigravity's question, as its question tool sends it
    /// (`agy-question`).
    const QUESTION: &[u8] = br#"{"type":"object","properties":{"q1":{"type":"string","title":"Do you prefer tea or coffee?","oneOf":[{"const":"1","title":"Tea"},{"const":"2","title":"Coffee"}]},"q1_other":{"type":"string","title":"Other"}}}"#;

    #[test]
    fn titled_constants_are_offered_by_their_titles_and_answered_by_their_constants() {
        let offered = titled_choices(QUESTION);
        let schema: Value = serde_json::from_slice(&offered).unwrap();
        assert_eq!(
            schema["properties"]["q1"]["enum"],
            serde_json::json!(["Tea", "Coffee"])
        );
        // Everything else as the agent wrote it, its fields in its order.
        assert_eq!(
            schema["properties"]["q1"]["oneOf"],
            serde_json::from_slice::<Value>(QUESTION).unwrap()["properties"]["q1"]["oneOf"]
        );
        let text = String::from_utf8(offered.clone()).unwrap();
        assert!(text.find("\"q1\"").unwrap() < text.find("\"q1_other\"").unwrap());
        let sent = constants_for_titles(br#"{"q1":"Coffee","q1_other":"Tea"}"#, &offered);
        assert_eq!(
            serde_json::from_slice::<Value>(&sent).unwrap(),
            serde_json::json!({"q1": "2", "q1_other": "Tea"})
        );
        // A value that is no title goes as it was typed.
        let typed = constants_for_titles(br#"{"q1":"Juice"}"#, &offered);
        assert_eq!(typed, br#"{"q1":"Juice"}"#);
    }

    /// Two options titled alike are told apart, and each sends its own
    /// constant: the second "Other" sends the second value.
    #[test]
    fn options_titled_alike_each_send_their_own_constant() {
        let schema = br#"{"properties":{"q":{"type":"string","oneOf":[{"const":"a","title":"Other"},{"const":"b","title":"Other"},{"const":"c","title":"Tea"}]}}}"#;
        let offered = titled_choices(schema);
        let read: Value = serde_json::from_slice(&offered).unwrap();
        assert_eq!(
            read["properties"]["q"]["enum"],
            serde_json::json!(["Other", "Other (2)", "Tea"])
        );
        let sent = |title: &str| {
            let content = serde_json::to_vec(&serde_json::json!({ "q": title })).unwrap();
            serde_json::from_slice::<Value>(&constants_for_titles(&content, &offered)).unwrap()["q"]
                .clone()
        };
        assert_eq!(sent("Other"), "a");
        assert_eq!(sent("Other (2)"), "b");
        assert_eq!(sent("Tea"), "c");
    }

    /// A default names its option's constant; offered, it names that
    /// option's title, so the field starts on it, and left untouched it
    /// goes back as the constant. Constants need not be strings.
    #[test]
    fn a_default_follows_its_option() {
        let schema = br#"{"properties":{"q":{"type":"integer","default":2,"oneOf":[{"const":1,"title":"Tea"},{"const":2,"title":"Coffee"}]}}}"#;
        let offered = titled_choices(schema);
        let read: Value = serde_json::from_slice(&offered).unwrap();
        assert_eq!(read["properties"]["q"]["default"], "Coffee");
        assert_eq!(
            serde_json::from_slice::<Value>(&constants_for_titles(br#"{"q":"Coffee"}"#, &offered))
                .unwrap(),
            serde_json::json!({"q": 2})
        );
    }

    /// Several of titled constants (ACP's titled multi-select: an array whose
    /// items are `anyOf` constants with titles) are offered as several of
    /// the titles, each option's constant its identity: a default list
    /// starts on its titles, and the titles picked go back as their
    /// constants, two titled alike apart.
    #[test]
    fn several_titled_constants_are_offered_by_title_and_sent_as_constants() {
        let schema = br#"{"properties":{"drinks":{"type":"array","default":["c"],"items":{"anyOf":[{"const":"t","title":"Tea"},{"const":"c","title":"Coffee"},{"const":"o","title":"Other"},{"const":"p","title":"Other"}]}}}}"#;
        let offered = titled_choices(schema);
        let read: Value = serde_json::from_slice(&offered).unwrap();
        let drinks = &read["properties"]["drinks"];
        assert_eq!(
            drinks["items"]["enum"],
            serde_json::json!(["Tea", "Coffee", "Other", "Other (2)"])
        );
        assert_eq!(drinks["default"], serde_json::json!(["Coffee"]));
        let sent = constants_for_titles(br#"{"drinks":["Tea","Other (2)"]}"#, &offered);
        assert_eq!(
            serde_json::from_slice::<Value>(&sent).unwrap(),
            serde_json::json!({"drinks": ["t", "p"]})
        );
    }

    /// A field that already offered an `enum` was not rewritten, so its
    /// answer goes as chosen, even where its `oneOf` titles another value.
    #[test]
    fn a_field_that_had_an_enum_is_answered_as_chosen() {
        let schema = br#"{"properties":{"q":{"type":"string","enum":["Tea","x"],"oneOf":[{"const":"x","title":"Tea"}]}}}"#;
        let offered = titled_choices(schema);
        assert_eq!(offered, schema);
        assert_eq!(
            constants_for_titles(br#"{"q":"Tea"}"#, &offered),
            br#"{"q":"Tea"}"#
        );
    }

    #[test]
    fn a_schema_without_titled_constants_is_kept_as_written() {
        let plain = br#"{"type":"object","properties":{"b":{"type":"string"},"a":{"type":"string","enum":["x","y"],"oneOf":[{"const":"x"}]}}}"#;
        assert_eq!(titled_choices(plain), plain);
        assert_eq!(titled_choices(b"not json"), b"not json");
        let mixed = br#"{"properties":{"a":{"oneOf":[{"const":"x"},{"type":"string"}]}}}"#;
        assert_eq!(titled_choices(mixed), mixed);
    }
}
