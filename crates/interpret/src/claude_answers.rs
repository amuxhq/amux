//! Turning an answer through amux into the decision Claude takes, for
//! either way Claude runs: headless Claude reads it as a control response,
//! terminal Claude as its PermissionRequest or Elicitation hook's output.
//! Both ask the same questions with the same inputs, so one answer reads
//! the same to both.

use std::collections::BTreeMap;

use claude_protocol::stream::{
    ElicitationResult, PermissionMode, PermissionResult, PermissionUpdate,
    PermissionUpdateDestination,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use wire::{AskClosed, ClaudeGrant, DecisionOutcome, FormAction, claude_answer, permission_answer};

use crate::claude_common::{Verdict, claude_grant, permission_scopes};
use crate::{ask_item, serde_pb};

/// What answering an ask decided, for the call it held.
pub(crate) enum Decided {
    Tool(ToolDecisionState),
    Plan(Verdict),
    Question(AskClosed),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct ToolDecisionState {
    pub(crate) outcome: i32,
    #[serde(with = "serde_pb::opt_msg")]
    pub(crate) grant: Option<ClaudeGrant>,
    pub(crate) note: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) enum AnswerShape {
    Permission,
    /// The permission the ask's switch choice moves to.
    Plan {
        switch: String,
    },
    /// The question texts, and each question's option labels.
    Question(Vec<(String, Vec<String>, bool)>),
    Form,
    Link,
}

/// AskUserQuestion's input with the person's answers: Claude hands it to
/// the tool.
#[derive(Serialize)]
struct AnsweringInput<'a> {
    #[serde(flatten)]
    input: serde_json::Map<String, Value>,
    /// The picked labels and typed answers, by each question's text.
    answers: BTreeMap<&'a str, String>,
    /// Notes per question, by its text.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    annotations: BTreeMap<&'a str, QuestionNote<'a>>,
}

/// The note a person wrote on a question, as Claude takes it.
#[derive(Serialize)]
struct QuestionNote<'a> {
    notes: &'a str,
}

impl AnswerShape {
    /// A tool server's form or link, which is an item of its own rather
    /// than a call's.
    pub(crate) fn is_dialog(&self) -> bool {
        matches!(self, Self::Form | Self::Link)
    }
}

/// What Claude is handed as the decision: a permission result, or a tool
/// server's form or link answered.
pub(crate) enum Answer {
    Permission(PermissionResult),
    Elicitation(ElicitationResult),
}

/// Claude's own words for a tool use the person rejected.
pub(crate) const REJECTED: &str = "The user doesn't want to proceed with this tool use. The tool use was rejected (eg. if it was a file edit, the new_string was NOT written to the file).";

/// The decision an answer hands Claude and the one it puts on the call's
/// row, or None when the answer does not fit the ask. `input` is the
/// call's input as Claude asked it, and `suggestions` each scope choice it
/// offered, as it sent them.
pub(crate) fn decide(
    shape: &AnswerShape,
    input: &str,
    suggestions: &[String],
    asked: Option<&wire::QuestionAsk>,
    answer: claude_answer::Of,
) -> Option<(Answer, Option<Decided>)> {
    let input = serde_json::from_str::<Value>(input).unwrap_or(Value::Null);
    let decided = |outcome: DecisionOutcome, grant: Option<ClaudeGrant>, note: &str| {
        Some(Decided::Tool(ToolDecisionState {
            outcome: outcome as i32,
            grant,
            note: note.to_owned(),
        }))
    };
    let allow = |input: Value, permissions: Option<Vec<PermissionUpdate>>| {
        Answer::Permission(PermissionResult::Allow {
            updated_input: Some(input),
            updated_permissions: permissions,
            tool_use_id: None,
        })
    };
    // Claude hands the message to the model as the tool's error. A bare
    // note there reads as a hook's refusal; worded as Claude words a
    // person's rejection in its own terminal, it reads as the person's.
    let deny = |note: &str, stop: bool| {
        let mut message = REJECTED.to_owned();
        if !note.is_empty() {
            message.push_str(" To tell you how to proceed, the user said:\n");
            message.push_str(note);
        }
        Answer::Permission(PermissionResult::Deny {
            message,
            interrupt: Some(stop),
            tool_use_id: None,
        })
    };
    match (shape, answer) {
        (AnswerShape::Permission, claude_answer::Of::Permission(permission)) => match permission
            .of?
        {
            permission_answer::Of::Allow(chosen) => {
                let mut permissions = None;
                let mut grant = None;
                if let Some(index) = chosen.scope {
                    let suggestion =
                        serde_json::from_str::<PermissionUpdate>(suggestions.get(index as usize)?)
                            .ok()?;
                    let scopes = permission_scopes(std::slice::from_ref(&suggestion));
                    grant = scopes.first().map(claude_grant);
                    permissions = Some(vec![suggestion]);
                }
                Some((
                    allow(input, permissions),
                    decided(DecisionOutcome::Allowed, grant, ""),
                ))
            }
            permission_answer::Of::Deny(no) => Some((
                deny(&no.note, no.stop),
                decided(DecisionOutcome::Denied, None, &no.note),
            )),
        },
        (AnswerShape::Plan { switch }, claude_answer::Of::Plan(plan)) => {
            let start = |mode: PermissionMode, verdict: Verdict| {
                let update = PermissionUpdate::SetMode {
                    mode,
                    destination: PermissionUpdateDestination::Session,
                };
                Some((
                    allow(input.clone(), Some(vec![update])),
                    Some(Decided::Plan(verdict)),
                ))
            };
            match wire::PlanChoice::try_from(plan.choice).ok()? {
                wire::PlanChoice::Start => start(
                    PermissionMode::Default,
                    Verdict::of(wire::PlanVerdict::Approved),
                ),
                // The switch the ask offered, the one the person was shown.
                wire::PlanChoice::StartSwitching if !switch.is_empty() => {
                    start(PermissionMode::parse(switch), Verdict::switching(switch))
                }
                wire::PlanChoice::StartSwitching => None,
                wire::PlanChoice::KeepPlanning => {
                    let note = plan.note.unwrap_or_default();
                    Some((
                        deny(&note, false),
                        Some(Decided::Plan(Verdict::sent_back(&note))),
                    ))
                }
                wire::PlanChoice::Unspecified => None,
            }
        }
        (AnswerShape::Question(questions), claude_answer::Of::Question(answer)) => {
            if answer.answers.len() != questions.len() {
                return None;
            }
            // A skipped question is left out of the answers, as Claude's
            // own interface does; Claude takes notes per question, by its
            // text, whether or not it was answered.
            let mut answers = BTreeMap::new();
            let mut annotations = BTreeMap::new();
            for ((question, labels, multi), response) in questions.iter().zip(&answer.answers) {
                let mut picked = Vec::new();
                for index in &response.selected {
                    picked.push(labels.get(*index as usize)?.clone());
                }
                picked.extend(response.other.clone());
                if !multi && picked.len() > 1 {
                    return None;
                }
                if !picked.is_empty() {
                    answers.insert(question.as_str(), picked.join(", "));
                }
                if let Some(note) = response
                    .note
                    .as_deref()
                    .filter(|note| !note.trim().is_empty())
                {
                    annotations.insert(question.as_str(), QuestionNote { notes: note });
                }
            }
            let updated = AnsweringInput {
                input: match input {
                    Value::Object(input) => input,
                    _ => serde_json::Map::new(),
                },
                answers,
                annotations,
            };
            let closed = ask_item::answered(asked?, &answer)?;
            Some((
                allow(
                    serde_json::to_value(updated).expect("an answered input serializes"),
                    None,
                ),
                Some(Decided::Question(closed)),
            ))
        }
        // The question is refused with the person's own words, which
        // Claude hands the model as the call's result.
        (AnswerShape::Question(questions), claude_answer::Of::Reply(reply)) => {
            if reply.text.trim().is_empty()
                || (!reply.answers_so_far.is_empty()
                    && reply.answers_so_far.len() != questions.len())
            {
                return None;
            }
            let closed = ask_item::replied(asked?, &reply)?;
            Some((
                Answer::Permission(PermissionResult::Deny {
                    message: reply.text,
                    interrupt: Some(false),
                    tool_use_id: None,
                }),
                Some(Decided::Question(closed)),
            ))
        }
        (AnswerShape::Form, claude_answer::Of::Form(form)) => {
            let result = match FormAction::try_from(form.action).ok()? {
                FormAction::Accept => ElicitationResult::Accept {
                    content: Some(
                        serde_json::from_slice(&form.content_json)
                            .unwrap_or_else(|_| Value::Object(serde_json::Map::new())),
                    ),
                    extensions: Default::default(),
                },
                action => form_action(action)?,
            };
            Some((Answer::Elicitation(result), None))
        }
        (AnswerShape::Link, claude_answer::Of::Link(link)) => Some((
            Answer::Elicitation(form_action(FormAction::try_from(link.action).ok()?)?),
            None,
        )),
        _ => None,
    }
}

/// The answer an action gives a tool server's form or link, without content.
fn form_action(action: FormAction) -> Option<ElicitationResult> {
    let extensions = Default::default();
    Some(match action {
        FormAction::Accept => ElicitationResult::Accept {
            content: None,
            extensions,
        },
        FormAction::Decline => ElicitationResult::Decline { extensions },
        FormAction::Cancel => ElicitationResult::Cancel { extensions },
        FormAction::Unspecified => return None,
    })
}
