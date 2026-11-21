//! Grok's own requests that wait on the person, outside ACP, and their
//! answers. Grok sends them as vendor methods (`_x.ai/...`); a client that
//! does not know one answers "method not found", and the tool that asked
//! fails.
//!
//! The answer's shape is not published. It was read from what Grok 1.0.92
//! accepts, answering its question tool live: it names a missing or wrong
//! field in its tool's error, and reports back what it took.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::Extra;
use crate::macros::string_enum;

/// Grok's question tool.
pub const ASK_USER_QUESTION: &str = "_x.ai/ask_user_question";

/// Reads the params of a vendor request this crate types, as `method`'s,
/// so a probe hears the names they type. Other methods' params stay
/// vendor data.
pub(crate) fn read_params(method: &str, params: &serde_json::Value) {
    if method == ASK_USER_QUESTION {
        let _ = AskUserQuestionRequest::deserialize(params);
    }
}

/// The params of `_x.ai/ask_user_question`: questions with options to pick
/// from, for the tool call that asks them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AskUserQuestionRequest {
    pub session_id: String,
    pub tool_call_id: String,
    pub questions: Vec<Question>,
    /// The session's mode as Grok names it (`default`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Question {
    pub question: String,
    pub options: Vec<QuestionOption>,
    /// Several may be picked; Grok writes null for one.
    #[serde(
        default,
        deserialize_with = "crate::present_nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub multi_select: Option<Option<bool>>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuestionOption {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// The answer to `_x.ai/ask_user_question`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AskUserQuestionResponse {
    pub outcome: QuestionOutcome,
    /// Each question answered, by its text: the label picked, the labels
    /// picked of one that takes several, or the person's own words. Grok
    /// requires it with `accepted`, and reads a question left out as
    /// unanswered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answers: Option<BTreeMap<String, QuestionAnswer>>,
    #[serde(flatten)]
    pub extra: Extra,
}

impl AskUserQuestionResponse {
    pub fn new(
        outcome: QuestionOutcome,
        answers: Option<BTreeMap<String, QuestionAnswer>>,
    ) -> Self {
        Self {
            outcome,
            answers,
            extra: Extra::new(),
        }
    }
}

string_enum! {
    /// How the person left the questions, as Grok tells its model.
    pub enum QuestionOutcome {
        /// Answered: `answers` holds what was picked or typed.
        Accepted = "accepted",
        /// The person wants to talk about the questions first; Grok asks
        /// what they would like to clarify.
        ChatAboutThis = "chat_about_this",
        /// Enough answers for Grok's plan interview: Grok stops asking and
        /// finishes the plan.
        SkipInterview = "skip_interview",
        /// Declined: Grok goes on with its best judgment.
        Cancelled = "cancelled",
    }
}

/// One question's answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum QuestionAnswer {
    One(String),
    Many(Vec<String>),
}

/// A client's answer to one of the vendor requests this crate types, by
/// the method it answers.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum ExtensionAnswer {
    AskUserQuestion(AskUserQuestionResponse),
}

impl ExtensionAnswer {
    /// Reads `result` as the answer to a `method` request. None when the
    /// method is not one this crate types.
    pub fn read(
        method: &str,
        result: &serde_json::Value,
    ) -> Option<Result<Self, serde_json::Error>> {
        Some(match method {
            ASK_USER_QUESTION => {
                AskUserQuestionResponse::deserialize(result).map(Self::AskUserQuestion)
            }
            _ => return None,
        })
    }

    /// [`read`](Self::read), failing where it would keep anything unknown.
    pub fn strict_read(
        method: &str,
        result: &serde_json::Value,
    ) -> Option<Result<Self, serde_json::Error>> {
        crate::macros::strictly(|| Self::read(method, result))
    }

    /// The first answer type that reads `result`, as a line that does not
    /// say which request it answers is read.
    pub(crate) fn first(result: &serde_json::Value) -> Result<Self, serde_json::Error> {
        AskUserQuestionResponse::deserialize(result).map(Self::AskUserQuestion)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{ClientMessage, RequestId};

    /// Grok's question, as Grok 1.0.92 sends it, reads strictly and writes
    /// back as it was.
    #[test]
    fn grok_questions_read_as_written() {
        let params = json!({
            "sessionId": "s", "toolCallId": "call-1", "mode": "default",
            "questions": [{
                "question": "Do you prefer tea or coffee?",
                "options": [
                    {"label": "Tea", "description": "You prefer tea."},
                    {"label": "Coffee", "description": "You prefer coffee."}
                ],
                "multiSelect": null
            }]
        });
        let read: AskUserQuestionRequest =
            crate::macros::strictly(|| AskUserQuestionRequest::deserialize(&params)).unwrap();
        assert_eq!(read.questions[0].options[1].label, "Coffee");
        assert_eq!(read.questions[0].multi_select, Some(None));
        assert_eq!(serde_json::to_value(&read).unwrap(), params);
    }

    /// The answers Grok took, one label, several and typed words, encode as
    /// it read them, and a client line holding one reads back strictly as
    /// that answer and no ACP one.
    #[test]
    fn an_answer_is_a_vendor_answer_both_ways() {
        let answers = BTreeMap::from([
            (
                "Tea or coffee?".to_owned(),
                QuestionAnswer::One("Coffee".into()),
            ),
            (
                "Which drinks?".to_owned(),
                QuestionAnswer::Many(vec!["Tea".into(), "Juice".into()]),
            ),
        ]);
        let message = ClientMessage::ExtensionAnswer {
            id: RequestId::Integer(0),
            answer: ExtensionAnswer::AskUserQuestion(AskUserQuestionResponse::new(
                QuestionOutcome::Accepted,
                Some(answers),
            )),
            extra: Extra::new(),
        };
        let line = crate::encode(&message);
        let written: serde_json::Value = serde_json::from_slice(&line).unwrap();
        assert_eq!(
            written["result"],
            json!({"outcome": "accepted", "answers": {
                "Tea or coffee?": "Coffee", "Which drinks?": ["Tea", "Juice"]
            }})
        );
        assert_eq!(crate::strict_client(&line).unwrap(), message);
        let declined = br#"{"jsonrpc":"2.0","id":3,"result":{"outcome":"cancelled"}}"#;
        assert!(matches!(
            crate::strict_client(declined).unwrap(),
            ClientMessage::ExtensionAnswer { .. }
        ));
        // An ACP answer is still read as one.
        let permission =
            br#"{"jsonrpc":"2.0","id":4,"result":{"outcome":{"outcome":"cancelled"}}}"#;
        assert!(matches!(
            crate::strict_client(permission).unwrap(),
            ClientMessage::Response { .. }
        ));
    }
}
