//! Recorded ACP sessions (`acp_io`: a live recording's `io.jsonl`, the
//! lines of its first agent process; `acp_io:<process>` for another) as
//! interpreter events.
//!
//! What the agent wrote is facts. What the recording's client wrote becomes
//! the inputs that would make this interpreter write the same: a prompt is
//! a prompt (a second one while the first runs is a prompt and its
//! send-now), a setting change the setting, `session/cancel` an interrupt,
//! and a response to a permission or form the person's answer. The
//! handshake is the agent process's own, so it is left out, its answers
//! kept under the ids the agent process gives it. The client's setting
//! changes in a later process before its first prompt re-applied what the
//! session lost on reopening, which the interpreter does itself.
//!
//! The interpreter names its requests (`amux-prompt-<input id>`,
//! `amux-set-<n>`, `amux-apply-<option>`), so the answers to the requests it
//! stands in for are renamed the same way. Each process ends with its exit.
//! Time is microseconds since the recording began.

use std::collections::BTreeMap;

use acp_protocol::agent::{NewSessionResponse, SetSessionConfigOptionResponse};
use acp_protocol::client::RequestPermissionOutcome;
use acp_protocol::elicitation::CreateElicitationResponse;
use acp_protocol::session::SessionConfigOption;
use acp_protocol::{
    AgentMessage, AgentNotification, AgentRequest, ClientMessage, ClientNotification,
    ClientRequest, ClientResponse, ContentBlock, RequestId, SessionUpdate, xai,
};
use base64::Engine as _;
use serde::Deserialize;
use sha2::Digest as _;
use wire::{AcpAnswer, AcpInput, FormAction, Input, acp_answer, acp_input};

use super::{
    Category, INITIALIZE_REQUEST, LOAD_REQUEST, NEW_SESSION_REQUEST, RESUME_REQUEST, apply_request,
    prompt_request, set_request,
};
use crate::{Channel, Event, Fact};

#[derive(Deserialize)]
struct Recorded {
    #[serde(default)]
    us: i64,
    /// `stdout` for what the agent wrote, `stdin` for the client's.
    #[serde(default)]
    dir: String,
    #[serde(default)]
    line: Option<String>,
    #[serde(default)]
    process: String,
}

/// What the agent asked, by ask key, to read the client's answer by.
enum Asked {
    Permission,
    Form,
    Link,
    /// Grok's questions, to read the labels it was answered with by.
    Question(Vec<xai::Question>),
}

fn key(id: &RequestId) -> String {
    serde_json::to_string(id).expect("a request id serializes")
}

fn input(id: String, of: acp_input::Of) -> Event {
    Event::Input(Input::new(id.into_bytes(), AcpInput { of: Some(of) }))
}

/// The settings the options name, by option id.
fn learn(categories: &mut BTreeMap<String, Category>, options: &[SessionConfigOption]) {
    for choice in super::choices(Some(options), None, None) {
        categories.insert(choice.id, choice.category);
    }
}

/// A prompt's image, as the client sent it, as the attachment amux holds.
fn image(data: &str, mime: &str) -> Option<wire::Attachment> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .ok()?;
    Some(wire::Attachment {
        of: Some(wire::attachment::Of::Image(wire::BlobRef {
            hash: sha2::Sha256::digest(&bytes).to_vec(),
            name: String::new(),
            mime: mime.to_owned(),
            size: bytes.len() as u64,
        })),
        ..Default::default()
    })
}

pub(super) fn read(format: &str, bytes: &[u8]) -> Result<Vec<Event>, String> {
    let process = match format {
        "acp_io" => "1",
        _ => format
            .strip_prefix("acp_io:")
            .ok_or_else(|| format!("ACP reads acp_io, not {format:?}"))?,
    };
    let lines = std::str::from_utf8(bytes)
        .map_err(|error| error.to_string())?
        .lines()
        .filter(|line| !line.trim().is_empty())
        .enumerate()
        .map(|(index, line)| {
            serde_json::from_str::<Recorded>(line)
                .map_err(|error| format!("line {}: {error}", index + 1))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let later = process != "1";
    // The interpreter numbers every ask the agent's life brought, earlier
    // processes' included.
    let earlier = |recorded: &&Recorded| {
        recorded.dir == "stdout"
            && recorded.process.parse::<u32>().ok() < process.parse::<u32>().ok()
    };
    let mut asks = lines
        .iter()
        .filter(earlier)
        .filter_map(|recorded| recorded.line.as_deref())
        .filter(|line| asked_for(line).is_some())
        .count() as u64;

    let mut events = Vec::new();
    let mut now = None;
    let mut renamed = BTreeMap::<String, String>::new();
    let mut asked = BTreeMap::<String, (Asked, String)>::new();
    let mut categories = BTreeMap::<String, Category>::new();
    let mut outstanding = Vec::<String>::new();
    let mut prompted = false;
    let mut prompts = 0;
    let mut sets = 0;
    let mut dropped = 0;
    let mut last_ms = 0;
    for recorded in lines.iter().filter(|recorded| recorded.process == process) {
        let Some(line) = recorded.line.as_deref() else {
            continue;
        };
        let at_ms = recorded.us / 1000;
        last_ms = at_ms;
        let mut push = |event: Event| {
            if now.is_none_or(|now| at_ms > now) {
                now = Some(at_ms);
                events.push(Event::Tick { at_ms });
            }
            events.push(event);
        };
        if recorded.dir == "stdout" {
            let mut payload = line.as_bytes().to_vec();
            match acp_protocol::decode(line.as_bytes()) {
                Ok(AgentMessage::Response { id, result, extra }) => {
                    if let Ok(value) = &result {
                        if let Ok(answer) = acp_protocol::result::<NewSessionResponse>(value) {
                            learn(
                                &mut categories,
                                &answer.config_options.flatten().unwrap_or_default(),
                            );
                        } else if let Ok(answer) =
                            acp_protocol::result::<SetSessionConfigOptionResponse>(value)
                        {
                            learn(&mut categories, &answer.config_options);
                        }
                    }
                    outstanding.retain(|prompt| *prompt != key(&id));
                    if let Some(name) = renamed.remove(&key(&id)) {
                        payload = acp_protocol::encode_server(&AgentMessage::Response {
                            id: RequestId::String(name),
                            result,
                            extra,
                        });
                    }
                }
                Ok(AgentMessage::Request { id, .. }) => {
                    if let Some(what) = asked_for(line) {
                        asks += 1;
                        asked.insert(key(&id), (what, super::ask_key(asks)));
                    }
                }
                Ok(AgentMessage::Extension { extension, .. })
                    if extension.method == xai::ASK_USER_QUESTION =>
                {
                    if let (Some(id), Ok(request)) = (
                        extension.id,
                        serde_json::from_value::<xai::AskUserQuestionRequest>(extension.params),
                    ) {
                        asks += 1;
                        asked.insert(
                            key(&id),
                            (Asked::Question(request.questions), super::ask_key(asks)),
                        );
                    }
                }
                Ok(AgentMessage::Notification {
                    notification: AgentNotification::SessionUpdate(update),
                    ..
                }) => {
                    if let SessionUpdate::ConfigOptionUpdate(update) = update.update {
                        learn(&mut categories, &update.config_options);
                    }
                }
                _ => {}
            }
            push(Event::Fact(Fact {
                channel: Channel::Rpc,
                payload,
            }));
            continue;
        }
        let Ok(message) = acp_protocol::decode_client(line.as_bytes()) else {
            continue;
        };
        match message {
            ClientMessage::Request { id, request, .. } => {
                let name = match request {
                    ClientRequest::Initialize(_) => INITIALIZE_REQUEST.to_owned(),
                    ClientRequest::SessionNew(_) => NEW_SESSION_REQUEST.to_owned(),
                    ClientRequest::SessionLoad(_) => LOAD_REQUEST.to_owned(),
                    ClientRequest::SessionResume(_) => RESUME_REQUEST.to_owned(),
                    ClientRequest::SessionPrompt(prompt) => {
                        prompts += 1;
                        let input_id = format!("p{process}-{prompts}");
                        let mut text = Vec::new();
                        let mut attachments = Vec::new();
                        for block in &prompt.prompt {
                            match block {
                                ContentBlock::Text(block) => text.push(block.text.as_str()),
                                ContentBlock::Image(block) => {
                                    attachments.extend(image(&block.data, &block.mime_type))
                                }
                                _ => {}
                            }
                        }
                        push(input(
                            input_id.clone(),
                            acp_input::Of::Prompt(wire::PromptInput {
                                text: text.join("\n"),
                                attachments,
                            }),
                        ));
                        if !outstanding.is_empty() {
                            prompts += 1;
                            push(input(
                                format!("p{process}-{prompts}"),
                                acp_input::Of::SendNow(wire::SendQueuedNow {
                                    queued_input_id: input_id.clone().into_bytes(),
                                }),
                            ));
                        }
                        prompted = true;
                        outstanding.push(key(&id));
                        prompt_request(input_id.as_bytes())
                    }
                    ClientRequest::SessionSetConfigOption(set) => {
                        let value = match &set.value {
                            acp_protocol::client::ConfigValue::Id(value) => value.clone(),
                            _ => String::new(),
                        };
                        let category = categories.get(&set.config_id).copied();
                        if later && !prompted {
                            apply_request(&set.config_id)
                        } else if let Some(category) = category {
                            sets += 1;
                            prompts += 1;
                            push(input(
                                format!("p{process}-{prompts}"),
                                setting(category, value),
                            ));
                            set_request(sets)
                        } else {
                            dropped += 1;
                            format!("amux-host-{dropped}")
                        }
                    }
                    ClientRequest::SessionSetMode(set) => {
                        if later && !prompted {
                            apply_request("mode")
                        } else {
                            sets += 1;
                            prompts += 1;
                            push(input(
                                format!("p{process}-{prompts}"),
                                setting(Category::Mode, set.mode_id),
                            ));
                            set_request(sets)
                        }
                    }
                    // The client's own housekeeping (closing the session at
                    // the end), which nothing in the interpreter stands for.
                    _ => {
                        dropped += 1;
                        format!("amux-host-{dropped}")
                    }
                };
                renamed.insert(key(&id), name);
            }
            ClientMessage::Notification {
                notification: ClientNotification::SessionCancel(_),
                ..
            } => {
                prompts += 1;
                push(input(
                    format!("p{process}-{prompts}"),
                    acp_input::Of::Interrupt(wire::Interrupt {}),
                ));
            }
            ClientMessage::Response {
                id,
                response: Ok(response),
                ..
            } => {
                let Some((what, ask_key)) = asked.remove(&key(&id)) else {
                    continue;
                };
                let of = match (Some(what), response) {
                    (Some(Asked::Permission), ClientResponse::RequestPermission(answer)) => {
                        match answer.outcome {
                            RequestPermissionOutcome::Selected(selected) => {
                                Some(acp_answer::Of::Option(selected.option_id))
                            }
                            // Sent by the interpreter itself on a cancel.
                            RequestPermissionOutcome::Cancelled(_)
                            | RequestPermissionOutcome::Unknown(_) => None,
                        }
                    }
                    (Some(Asked::Form), ClientResponse::ElicitationCreate(answer)) => {
                        form_answer(answer).map(acp_answer::Of::Form)
                    }
                    (Some(Asked::Link), ClientResponse::ElicitationCreate(answer)) => {
                        form_answer(answer).map(|form| {
                            acp_answer::Of::Link(wire::LinkAnswer {
                                action: form.action,
                            })
                        })
                    }
                    _ => None,
                };
                if let Some(of) = of {
                    prompts += 1;
                    push(input(
                        format!("p{process}-{prompts}"),
                        acp_input::Of::Answer(AcpAnswer {
                            ask_key,
                            of: Some(of),
                        }),
                    ));
                }
            }
            ClientMessage::ExtensionAnswer {
                id,
                answer: xai::ExtensionAnswer::AskUserQuestion(answer),
                ..
            } => {
                let Some((Asked::Question(questions), ask_key)) = asked.remove(&key(&id)) else {
                    continue;
                };
                if let Some(answers) = question_answer(&questions, &answer) {
                    prompts += 1;
                    push(input(
                        format!("p{process}-{prompts}"),
                        acp_input::Of::Answer(AcpAnswer {
                            ask_key,
                            of: Some(acp_answer::Of::Question(answers)),
                        }),
                    ));
                }
            }
            // Refusals of the agent's requests, which the interpreter makes
            // itself, and anything else the client wrote.
            _ => {}
        }
    }
    if now.is_none_or(|now| last_ms > now) {
        events.push(Event::Tick { at_ms: last_ms });
    }
    events.push(Event::ProviderExit { code: Some(0) });
    Ok(events)
}

/// The person's answers to Grok's questions as the client sent them: each
/// label picked by its place, other words as typed, and a question left out
/// skipped. None for an answer the interpreter sends itself (cancelled) or
/// one no person's answer stands for.
fn question_answer(
    questions: &[xai::Question],
    answer: &xai::AskUserQuestionResponse,
) -> Option<wire::QuestionAnswer> {
    let given = match answer.outcome {
        xai::QuestionOutcome::Accepted => answer.answers.clone().unwrap_or_default(),
        xai::QuestionOutcome::SkipInterview => BTreeMap::new(),
        _ => return None,
    };
    let answers = questions
        .iter()
        .map(|question| {
            let said = match given.get(&question.question) {
                None => Vec::new(),
                Some(xai::QuestionAnswer::One(one)) => vec![one.clone()],
                Some(xai::QuestionAnswer::Many(many)) => many.clone(),
            };
            let mut response = wire::QuestionResponse::default();
            let mut other = Vec::new();
            for said in said {
                match question
                    .options
                    .iter()
                    .position(|option| option.label == said)
                {
                    Some(at) => response.selected.push(at as u32),
                    None => other.push(said),
                }
            }
            response.other = (!other.is_empty()).then(|| other.join(", "));
            response
        })
        .collect();
    Some(wire::QuestionAnswer { answers })
}

/// What a line of the agent's asks the person for, if it is an ask.
fn asked_for(line: &str) -> Option<Asked> {
    use acp_protocol::elicitation::CreateElicitationRequest;
    match acp_protocol::decode(line.as_bytes()).ok()? {
        AgentMessage::Request {
            request: AgentRequest::RequestPermission(_),
            ..
        } => Some(Asked::Permission),
        AgentMessage::Request {
            request: AgentRequest::ElicitationCreate(CreateElicitationRequest::Form(_)),
            ..
        } => Some(Asked::Form),
        AgentMessage::Request {
            request: AgentRequest::ElicitationCreate(CreateElicitationRequest::Url(_)),
            ..
        } => Some(Asked::Link),
        _ => None,
    }
}

fn setting(category: Category, value: String) -> acp_input::Of {
    match category {
        Category::Model => acp_input::Of::Model(wire::SetModel { model: Some(value) }),
        Category::Effort => acp_input::Of::Effort(wire::SetEffort {
            effort: Some(value),
        }),
        Category::Mode => acp_input::Of::Mode(wire::SetMode { value }),
    }
}

fn form_answer(answer: CreateElicitationResponse) -> Option<wire::FormAnswer> {
    Some(match answer {
        CreateElicitationResponse::Accept(accept) => wire::FormAnswer {
            action: FormAction::Accept as i32,
            content_json: accept
                .content
                .flatten()
                .map(|content| serde_json::to_vec(&content).unwrap_or_default())
                .unwrap_or_default(),
        },
        CreateElicitationResponse::Decline(_) => wire::FormAnswer {
            action: FormAction::Decline as i32,
            content_json: Vec::new(),
        },
        CreateElicitationResponse::Cancel(_) => wire::FormAnswer {
            action: FormAction::Cancel as i32,
            content_json: Vec::new(),
        },
        CreateElicitationResponse::Unknown(_) => return None,
    })
}
