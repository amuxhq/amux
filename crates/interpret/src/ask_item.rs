//! Asks that are the work: a question, a tool server's form or link, an
//! access grant, or a dialog this build cannot read. Each is its own item, written when the ask opens and
//! revised once with how it closed, so the decision row outlives the ask.
//! The kinds call these helpers so every carrier records a close the same
//! way.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;
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
}
