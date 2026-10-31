//! Recorded headless Claude sessions (`claude_sdk_io`: a claude-specs
//! provider recording, `io.jsonl`) as interpreter events.
//!
//! Stdout lines are facts. What the recording's host wrote to stdin becomes
//! the inputs that would make this interpreter write the same thing: user
//! messages are prompts, answers to permission and elicitation requests are
//! answers (a question refused with words is a reply instead), and model,
//! mode, effort and interrupt requests are those inputs. A user
//! message that carries a uuid gets the input id that makes the interpreter
//! write that same uuid, so Claude's replay of it correlates. One written
//! while a turn runs, at default priority, is Claude folding it into that
//! turn, which this interpreter writes only for a prompt sent now: it becomes
//! the prompt followed by sending it now. The
//! host's own handshake and settings requests are left out; their
//! responses stay in as facts. Time is microseconds since the recording
//! began, placed on the wall clock the first timestamped line uses.

use claude_protocol::stream::{
    self, ControlRequestBody, ControlResponse, ElicitationResult, Input as Written, Message,
    Output, PermissionResult, PermissionUpdate,
};
use serde::Deserialize;
use wire::{
    ClaudeAnswer, ClaudeSdkInput, FormAction, FormAnswer, Input, Interrupt, LinkAnswer,
    PermissionAllow, PermissionAnswer, PermissionDeny, PlanAnswer, PlanChoice, PromptInput,
    QuestionAnswer, QuestionResponse, ReplyInstead, SendQueuedNow, SetEffort, SetModel,
    SetPermission, claude_answer, claude_sdk_input, permission_answer,
};

use crate::claude_common::{AnsweredResult, PLAN_TOOL, QUESTION_TOOL, QuestionInput, message_text};
use crate::{Channel, Event, Fact};

/// One line of a recording: when, which way, and what.
#[derive(Deserialize)]
struct Record {
    #[serde(default)]
    us: i64,
    #[serde(default)]
    dir: String,
    #[serde(default)]
    line: Option<String>,
}

/// A recorded line Claude wrote or read, decoded.
#[allow(clippy::large_enum_variant)]
enum Line {
    Stdout(Output),
    Stdin(Written),
}

pub(super) fn read(format: &str, bytes: &[u8]) -> Result<Vec<Event>, String> {
    if format != "claude_sdk_io" {
        return Err(format!("claude_sdk reads claude_sdk_io, not {format:?}"));
    }
    let text_bytes = std::str::from_utf8(bytes).map_err(|error| error.to_string())?;
    let records = text_bytes
        .lines()
        .filter(|line| !line.trim().is_empty())
        .enumerate()
        .map(|(index, line)| {
            serde_json::from_str::<Record>(line)
                .map_err(|error| format!("line {}: {error}", index + 1))
        })
        .collect::<Result<Vec<_>, _>>()?;
    // Each line Claude wrote or read as a JSON value, the form the fact
    // carries, and decoded.
    let lines = records
        .iter()
        .map(|record| {
            let written = record.line.as_deref()?;
            let value = serde_json::from_str::<serde_json::Value>(written).ok()?;
            let line = if record.dir == "stdout" {
                Line::Stdout(stream::decode(written.as_bytes()).ok()?)
            } else {
                Line::Stdin(stream::decode_input(written.as_bytes()).ok()?)
            };
            Some((record.us, value, line))
        })
        .collect::<Vec<_>>();
    let origin = lines
        .iter()
        .flatten()
        .find_map(|(us, _, line)| Some(timestamp_ms(line)? - us / 1000))
        .unwrap_or(0);
    let mut events = Vec::new();
    let mut now = None;
    let mut requests = std::collections::BTreeMap::<String, ControlRequestBody>::new();
    let mut inputs = 0;
    let mut turn_running = false;
    for (us, value, line) in lines.into_iter().flatten() {
        let at_ms = origin + us / 1000;
        let mut push = |event: Event| {
            if now.is_none_or(|now| at_ms > now) {
                now = Some(at_ms);
                events.push(Event::Tick { at_ms });
            }
            events.push(event);
        };
        let written = match line {
            Line::Stdout(output) => {
                match output {
                    Output::Message(Message::Result(_)) => turn_running = false,
                    Output::ControlRequest(request) => {
                        requests.insert(request.request_id, request.request);
                    }
                    _ => {}
                }
                push(Event::Fact(Fact {
                    channel: Channel::Stream,
                    payload: value.to_string().into_bytes(),
                }));
                continue;
            }
            Line::Stdin(written) => written,
        };
        inputs += 1;
        let mut id = format!("stdin-{inputs}").into_bytes();
        let mut send_now = None;
        if let Written::User(user) = &written {
            if let Some(uuid) = &user.uuid {
                id = crate::serde_pb::from_hex(&uuid.replace('-', ""))?;
            }
            if turn_running && user.priority.is_none() {
                send_now = Some(Input::new(
                    format!("stdin-{inputs}-now").into_bytes(),
                    ClaudeSdkInput {
                        of: Some(claude_sdk_input::Of::SendNow(SendQueuedNow {
                            queued_input_id: id.clone(),
                        })),
                    },
                ));
            }
            turn_running = true;
        }
        let arm = match written {
            Written::User(user) => Some(claude_sdk_input::Of::Prompt(PromptInput {
                text: message_text(&user.message.content),
                ..Default::default()
            })),
            Written::ControlRequest(request) => match request.request {
                ControlRequestBody::Interrupt(_) => {
                    Some(claude_sdk_input::Of::Interrupt(Interrupt {}))
                }
                ControlRequestBody::SetModel(set) => {
                    Some(claude_sdk_input::Of::Model(SetModel { model: set.model }))
                }
                ControlRequestBody::SetPermissionMode(set) => {
                    Some(claude_sdk_input::Of::Permission(SetPermission {
                        value: set.mode.as_str().to_owned(),
                    }))
                }
                ControlRequestBody::ApplyFlagSettings(apply) => apply
                    .settings
                    .effort_level
                    .map(|effort| claude_sdk_input::Of::Effort(SetEffort { effort })),
                _ => None,
            },
            Written::ControlResponse(response) => {
                let request_id = response.request_id().to_owned();
                requests
                    .get(&request_id)
                    .and_then(|request| answer(request, &response))
                    .map(|body| {
                        claude_sdk_input::Of::Answer(ClaudeAnswer {
                            ask_key: request_id,
                            of: Some(body),
                        })
                    })
            }
            Written::Unknown(_) => None,
        };
        if let Some(arm) = arm {
            push(Event::Input(Input::new(
                id,
                ClaudeSdkInput { of: Some(arm) },
            )));
        }
        if let Some(send_now) = send_now {
            push(Event::Input(send_now));
        }
    }
    Ok(events)
}

/// When Claude wrote a line, for the lines that say.
fn timestamp_ms(line: &Line) -> Option<i64> {
    let stamp = match line {
        Line::Stdout(Output::Message(Message::Assistant(message))) => &message.timestamp,
        Line::Stdout(Output::Message(Message::User(message))) => &message.timestamp,
        Line::Stdout(Output::Message(Message::UserReplay(message))) => &message.timestamp,
        _ => return None,
    };
    chrono::DateTime::parse_from_rfc3339(stamp.as_deref()?)
        .ok()
        .map(|at| at.timestamp_millis())
}

/// The answer a recorded control response gave to the request it answers.
fn answer(request: &ControlRequestBody, response: &ControlResponse) -> Option<claude_answer::Of> {
    match request {
        ControlRequestBody::Elicitation(asked) => {
            let (action, content) = match response.result::<ElicitationResult>() {
                Some(Ok(ElicitationResult::Accept { content, .. })) => {
                    (FormAction::Accept, content)
                }
                Some(Ok(ElicitationResult::Decline { .. })) => (FormAction::Decline, None),
                _ => (FormAction::Cancel, None),
            };
            // A link carries no content: the person went to the page.
            if asked.mode.as_deref() == Some("url") {
                return Some(claude_answer::Of::Link(LinkAnswer {
                    action: action as i32,
                }));
            }
            Some(claude_answer::Of::Form(FormAnswer {
                action: action as i32,
                content_json: content
                    .map(|content| content.to_string().into_bytes())
                    .unwrap_or_default(),
            }))
        }
        ControlRequestBody::CanUseTool(asked) => {
            let result = response.result::<PermissionResult>().and_then(Result::ok);
            let (allow, updated_input, chosen, message, stop) = match result {
                Some(PermissionResult::Allow {
                    updated_input,
                    updated_permissions,
                    ..
                }) => (
                    true,
                    updated_input,
                    updated_permissions.and_then(|chosen| chosen.into_iter().next()),
                    String::new(),
                    false,
                ),
                Some(PermissionResult::Deny {
                    message, interrupt, ..
                }) => (false, None, None, message, interrupt.unwrap_or(false)),
                None => (false, None, None, String::new(), false),
            };
            match asked.tool_name.as_str() {
                PLAN_TOOL => Some(claude_answer::Of::Plan(if allow {
                    match chosen {
                        Some(PermissionUpdate::SetMode { mode, .. })
                            if !matches!(
                                mode,
                                stream::PermissionMode::Default | stream::PermissionMode::Plan
                            ) =>
                        {
                            PlanAnswer {
                                choice: PlanChoice::StartSwitching as i32,
                                note: None,
                            }
                        }
                        _ => PlanAnswer {
                            choice: PlanChoice::Start as i32,
                            note: None,
                        },
                    }
                } else {
                    PlanAnswer {
                        choice: PlanChoice::KeepPlanning as i32,
                        note: Some(message).filter(|note| !note.is_empty()),
                    }
                })),
                QUESTION_TOOL if allow => {
                    let answered = AnsweredResult::deserialize(updated_input.as_ref()?).ok()?;
                    let questions = QuestionInput::deserialize(&asked.input).ok()?;
                    Some(claude_answer::Of::Question(QuestionAnswer {
                        answers: questions
                            .questions
                            .iter()
                            .map(|question| {
                                let picked = answered
                                    .answers
                                    .get(&question.question)
                                    .map(String::as_str)
                                    .unwrap_or_default();
                                let mut selected = Vec::new();
                                let mut other = Vec::new();
                                for pick in picked.split(", ").filter(|pick| !pick.is_empty()) {
                                    match question
                                        .options
                                        .iter()
                                        .position(|option| option.label == pick)
                                    {
                                        Some(index) => selected.push(index as u32),
                                        None => other.push(pick),
                                    }
                                }
                                QuestionResponse {
                                    selected,
                                    other: (!other.is_empty()).then(|| other.join(", ")),
                                    note: answered
                                        .annotations
                                        .get(&question.question)
                                        .map(|note| note.notes.clone())
                                        .filter(|note| !note.is_empty()),
                                }
                            })
                            .collect(),
                    }))
                }
                // Refused with words of the person's own: replied instead.
                QUESTION_TOOL if !message.is_empty() && !stop => {
                    Some(claude_answer::Of::Reply(ReplyInstead {
                        text: message,
                        answers_so_far: Vec::new(),
                    }))
                }
                _ => Some(claude_answer::Of::Permission(PermissionAnswer {
                    of: Some(if allow {
                        let offered = asked.permission_suggestions.as_deref().unwrap_or_default();
                        let scope = chosen
                            .and_then(|chosen| offered.iter().position(|offer| *offer == chosen))
                            .map(|index| index as u32);
                        permission_answer::Of::Allow(PermissionAllow { scope })
                    } else {
                        permission_answer::Of::Deny(PermissionDeny {
                            note: message,
                            stop,
                        })
                    }),
                })),
            }
        }
        _ => None,
    }
}
