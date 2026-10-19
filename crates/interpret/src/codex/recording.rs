//! Recorded app-server sessions (`codex_io`: a codex-specs recording,
//! `io.jsonl`) as interpreter events.
//!
//! What the server wrote is facts. What the recording's host wrote becomes
//! the inputs that would make this interpreter write the same: a turn with
//! text is a prompt, after the model, effort, permission and mode changes
//! it carries, a steer a prompt then its send-now, an injected item an agent
//! message, an interrupt an interrupt (with a question waiting and the next
//! turn's words, a reply instead), a compaction a `/compact` prompt, and
//! a response to a server request an answer. The handshake and the host's
//! own introspection are left out; their responses stay in as facts. An
//! empty turn after an inject is the interpreter's own kick and is left out
//! too.
//!
//! A recording of two clients on one server marks each line with the
//! client it went to or came from. Only amux's lines are read: what the
//! other client caused reaches amux as what Codex tells it. amux's prompts
//! and steers carry their input id in hex as Codex's client message id, and
//! the inputs read here take that id back, so the interpreter's echo
//! matching sees what it would live.
//!
//! The interpreter numbers its requests `amux-<n>`, so the ids of the
//! host's requests it stands in for are renumbered the same way, in order,
//! on the requests' responses. Time is microseconds since the recording
//! began, placed on the wall clock the first `emittedAtMs` gives.

use std::collections::BTreeMap;

use codex_protocol::client::{ElicitationAction, GrantScope, InjectedContent, InjectedItem};
use codex_protocol::server::{CommandDecision, Decision as Offered};
use codex_protocol::{
    ClientMessage, ClientRequest, ClientResponse, RequestId, ServerMessage, ServerNotification,
    ServerRequest,
};
use serde::Deserialize;
use wire::{
    CodexAnswer, CodexInput, Envelope, FormAction, FormAnswer, GrantAnswer, Input, Interrupt,
    LinkAnswer, PromptInput, QuestionAnswer, QuestionResponse, codex_answer, codex_input,
};

use crate::{Channel, Event, Fact};

/// One line of a recording.
#[derive(Deserialize)]
struct Recorded {
    /// Microseconds since the recording began.
    #[serde(default)]
    us: i64,
    /// `stdout` for what the server wrote; anything else is the host's.
    #[serde(default)]
    dir: String,
    /// The JSON-RPC line; an exit has none.
    #[serde(default)]
    line: Option<String>,
    /// The client a two-client recording's line belongs to.
    #[serde(default)]
    transport_id: Option<String>,
}

/// The transport amux's own client is recorded under when two share a
/// server; a recording of one client marks none.
const AMUX: &str = "amux";

/// A recorded line, read in the direction it went.
enum Line {
    Server(ServerMessage),
    Host(ClientMessage),
}

pub(super) fn read(format: &str, bytes: &[u8]) -> Result<Vec<Event>, String> {
    if format != "codex_io" {
        return Err(format!("codex reads codex_io, not {format:?}"));
    }
    let lines = std::str::from_utf8(bytes)
        .map_err(|error| error.to_string())?
        .lines()
        .filter(|line| !line.trim().is_empty())
        .enumerate()
        .map(|(index, line)| {
            serde_json::from_str::<Recorded>(line)
                .map_err(|error| format!("line {}: {error}", index + 1))
        })
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .filter(|recorded| recorded.transport_id.as_deref().is_none_or(|id| id == AMUX))
        .collect::<Vec<_>>();
    let parsed = |recorded: &Recorded| {
        let line = recorded.line.as_deref()?.as_bytes();
        Some(if recorded.dir == "stdout" {
            Line::Server(codex_protocol::decode(line).ok()?)
        } else {
            Line::Host(codex_protocol::decode_client(line).ok()?)
        })
    };
    let origin = lines
        .iter()
        .find_map(|recorded| match parsed(recorded)? {
            Line::Server(ServerMessage::Notification {
                emitted_at_ms: Some(at),
                ..
            }) => Some(at - recorded.us / 1000),
            _ => None,
        })
        .unwrap_or(0);

    let mut events = Vec::new();
    let mut now = None;
    // Server requests by their key, to read the host's responses as answers.
    let mut asked = BTreeMap::<String, ServerRequest>::new();
    // Host request ids the interpreter stands in for, renumbered.
    let mut renumbered = BTreeMap::<String, String>::new();
    let mut inputs = 0;
    let mut injected_while_idle = false;
    let mut busy = false;
    // Server requests the host answered, by key.
    let mut responded = std::collections::BTreeSet::<String>::new();
    // Turn starts read as part of a reply instead.
    let mut replied = std::collections::BTreeSet::<usize>::new();
    // The plan item Codex completed since the host last started a turn.
    let mut planned = None::<String>;
    for (at, recorded) in lines.iter().enumerate() {
        if replied.contains(&at) {
            continue;
        }
        let Some(line) = parsed(recorded) else {
            continue;
        };
        let at_ms = origin + recorded.us / 1000;
        let mut push = |event: Event| {
            if now.is_none_or(|now| at_ms > now) {
                now = Some(at_ms);
                events.push(Event::Tick { at_ms });
            }
            events.push(event);
        };
        let message = match line {
            Line::Server(mut message) => {
                match &mut message {
                    ServerMessage::Response { id, .. } => {
                        if let Some(ours) = renumbered.get(&key(id)) {
                            *id = RequestId::String(ours.clone());
                        }
                    }
                    ServerMessage::Request { id, request, .. } => {
                        asked.insert(key(id), request.clone());
                    }
                    ServerMessage::Notification {
                        notification: ServerNotification::TurnStarted(_),
                        ..
                    } => busy = true,
                    ServerMessage::Notification {
                        notification: ServerNotification::TurnCompleted(_),
                        ..
                    } => busy = false,
                    ServerMessage::Notification {
                        notification: ServerNotification::ItemCompleted(completed),
                        ..
                    } => {
                        if let codex_protocol::items::ThreadItem::Plan(plan) = &completed.item {
                            planned = Some(plan.id.clone());
                        }
                    }
                    _ => {}
                }
                push(Event::Fact(Fact {
                    channel: Channel::Rpc,
                    payload: codex_protocol::encode_server(&message),
                }));
                continue;
            }
            Line::Host(message) => message,
        };

        if let ClientMessage::Request { id, request, .. } = &message
            && matches!(
                request,
                ClientRequest::TurnStart(_)
                    | ClientRequest::TurnSteer(_)
                    | ClientRequest::TurnInterrupt(_)
                    | ClientRequest::ThreadInjectItems(_)
                    | ClientRequest::ThreadCompactStart(_)
            )
        {
            // The interpreter sends these itself.
            renumbered.insert(key(id), format!("amux-{}", renumbered.len() + 1));
        }
        inputs += 1;
        let numbered = format!("stdin-{inputs}").into_bytes();
        let input_id = sent_as(&message).unwrap_or(numbered);
        let arm =
            match message {
                ClientMessage::Request {
                    request: ClientRequest::TurnStart(params),
                    ..
                } => {
                    let text = input_text(&params.input);
                    if text.is_empty() && std::mem::take(&mut injected_while_idle) {
                        continue;
                    }
                    // A plan waiting, then a turn leaving plan mode with
                    // Codex's own words for carrying it out, is how amux
                    // starts on a plan: one answer, not a mode change and
                    // then a prompt.
                    let started = planned.take().filter(|_| {
                        text == super::IMPLEMENT
                            && params
                                .collaboration_mode
                                .as_ref()
                                .is_some_and(|mode| mode.mode.as_str() == "default")
                    });
                    // Overrides the host put on the turn are inputs before it.
                    if let Some(model) = params.model {
                        push(Event::Input(Input::new(
                            format!("stdin-{inputs}-model").into_bytes(),
                            CodexInput {
                                of: Some(codex_input::Of::Model(wire::SetModel {
                                    model: Some(model),
                                })),
                            },
                        )));
                    }
                    if let Some(Some(effort)) = params.effort {
                        push(Event::Input(Input::new(
                            format!("stdin-{inputs}-effort").into_bytes(),
                            CodexInput {
                                of: Some(codex_input::Of::Effort(wire::SetEffort {
                                    effort: Some(effort.as_str().to_owned()),
                                })),
                            },
                        )));
                    }
                    if let (Some(approval), Some(sandbox)) =
                        (&params.approval_policy, &params.sandbox_policy)
                        && let Some(value) = super::facts::named_permission(
                            approval,
                            sandbox,
                            params
                                .approvals_reviewer
                                .as_ref()
                                .map(|reviewer| reviewer.as_str()),
                        )
                    {
                        push(Event::Input(Input::new(
                            format!("stdin-{inputs}-permission").into_bytes(),
                            CodexInput {
                                of: Some(codex_input::Of::Permission(wire::SetPermission {
                                    value: value.to_owned(),
                                })),
                            },
                        )));
                    }
                    if let Some(plan) = started {
                        codex_input::Of::Answer(CodexAnswer {
                            ask_key: super::plan_ask_key(&plan),
                            of: Some(codex_answer::Of::Plan(wire::PlanAnswer {
                                choice: wire::PlanChoice::Start as i32,
                                note: None,
                            })),
                        })
                    } else {
                        if let Some(collaboration) = &params.collaboration_mode {
                            push(Event::Input(Input::new(
                                format!("stdin-{inputs}-mode").into_bytes(),
                                CodexInput {
                                    of: Some(codex_input::Of::Mode(wire::SetMode {
                                        value: collaboration.mode.as_str().to_owned(),
                                    })),
                                },
                            )));
                        }
                        codex_input::Of::Prompt(PromptInput {
                            text,
                            ..Default::default()
                        })
                    }
                }
                ClientMessage::Request {
                    request: ClientRequest::TurnSteer(params),
                    ..
                } => {
                    let queued = params
                        .client_user_message_id
                        .as_deref()
                        .and_then(|id| crate::from_hex(id).ok())
                        .unwrap_or_else(|| format!("stdin-{inputs}-queued").into_bytes());
                    push(Event::Input(Input::new(
                        queued.clone(),
                        CodexInput {
                            of: Some(codex_input::Of::Prompt(PromptInput {
                                text: input_text(&params.input),
                                ..Default::default()
                            })),
                        },
                    )));
                    codex_input::Of::SendNow(wire::SendQueuedNow {
                        queued_input_id: queued,
                    })
                }
                ClientMessage::Request {
                    request: ClientRequest::TurnInterrupt(_),
                    ..
                } => {
                    // An interrupt while a question waits, then the next turn's
                    // words, is how amux replies instead of answering.
                    let open = asked.iter().find(|(key, request)| {
                        matches!(request, ServerRequest::RequestUserInput(_))
                            && !responded.contains(*key)
                    });
                    let next_turn = lines.iter().enumerate().skip(at + 1).find_map(
                        |(later, line)| match parsed(line)? {
                            Line::Host(ClientMessage::Request {
                                request: ClientRequest::TurnStart(params),
                                ..
                            }) => Some((later, params)),
                            _ => None,
                        },
                    );
                    match (open, next_turn) {
                        (Some((key, _)), Some((later, params)))
                            if !input_text(&params.input).is_empty() =>
                        {
                            let key = key.clone();
                            responded.insert(key.clone());
                            replied.insert(later);
                            let sent = params
                                .client_user_message_id
                                .as_deref()
                                .and_then(|id| crate::from_hex(id).ok());
                            push(Event::Input(Input::new(
                                sent.unwrap_or(input_id),
                                CodexInput {
                                    of: Some(codex_input::Of::Answer(CodexAnswer {
                                        ask_key: key,
                                        of: Some(codex_answer::Of::Reply(wire::ReplyInstead {
                                            text: input_text(&params.input),
                                            answers_so_far: Vec::new(),
                                        })),
                                    })),
                                },
                            )));
                            continue;
                        }
                        _ => codex_input::Of::Interrupt(Interrupt {}),
                    }
                }
                ClientMessage::Request {
                    request: ClientRequest::ThreadCompactStart(_),
                    ..
                } => codex_input::Of::Prompt(PromptInput {
                    text: "/compact".into(),
                    ..Default::default()
                }),
                ClientMessage::Request {
                    request: ClientRequest::ThreadInjectItems(params),
                    ..
                } => {
                    injected_while_idle = !busy;
                    let text = params
                        .items
                        .iter()
                        .flat_map(|item| match item {
                            InjectedItem::Message(message) => message.content.as_slice(),
                            InjectedItem::Unknown(_) => &[],
                        })
                        .map(|part| match part {
                            InjectedContent::InputText(text) => text.text.as_str(),
                            InjectedContent::Unknown(_) => "",
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    push(Event::AgentMessage(Envelope {
                        id: input_id,
                        body: Some(wire::envelope::Body::Message(wire::MessageBody {
                            text,
                            attachments: Vec::new(),
                        })),
                        ..Default::default()
                    }));
                    continue;
                }
                ClientMessage::Response {
                    id,
                    response: Ok(response),
                    ..
                } => {
                    let key = key(&id);
                    responded.insert(key.clone());
                    match asked
                        .get(&key)
                        .and_then(|asked| answer(asked, key, &response))
                    {
                        Some(arm) => arm,
                        None => continue,
                    }
                }
                _ => continue,
            };
        push(Event::Input(Input::new(
            input_id,
            CodexInput { of: Some(arm) },
        )));
    }
    Ok(events)
}

/// The input id amux sent a prompt under, from its client message id. A
/// steer's id names the queued prompt, not the send-now that sends it.
fn sent_as(message: &ClientMessage) -> Option<Vec<u8>> {
    match message {
        ClientMessage::Request {
            request: ClientRequest::TurnStart(params),
            ..
        } => crate::from_hex(params.client_user_message_id.as_deref()?).ok(),
        _ => None,
    }
}

/// A request id as the interpreter keys its asks: the JSON it was sent as.
fn key(id: &RequestId) -> String {
    serde_json::to_string(id).expect("a request id serializes")
}

fn input_text(input: &[codex_protocol::items::UserInput]) -> String {
    input
        .iter()
        .filter_map(|part| match part {
            codex_protocol::items::UserInput::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The host's response to a server request, as the input that sends it.
fn answer(
    request: &ServerRequest,
    key: String,
    response: &ClientResponse,
) -> Option<codex_input::Of> {
    let body = |of: codex_answer::Of| {
        Some(codex_input::Of::Answer(CodexAnswer {
            ask_key: key.clone(),
            of: Some(of),
        }))
    };
    let approve = |decision: wire::Decision| body(codex_answer::Of::Approve(decision as i32));
    match (request, response) {
        (
            ServerRequest::CommandApproval(_) | ServerRequest::FileChangeApproval(_),
            ClientResponse::CommandApproval(answer),
        ) => approve(match &answer.decision {
            CommandDecision::Plain(Offered::Accept) => wire::Decision::Approve,
            CommandDecision::Plain(Offered::AcceptForSession) => wire::Decision::ApproveSession,
            CommandDecision::Plain(Offered::Decline) => wire::Decision::Deny,
            CommandDecision::Plain(Offered::Cancel) => wire::Decision::Abort,
            CommandDecision::Plain(Offered::Other(_)) => return None,
            CommandDecision::AcceptWithExecpolicyAmendment { .. } => wire::Decision::ApproveSimilar,
            CommandDecision::ApplyNetworkPolicyAmendment { .. } => wire::Decision::ApproveNetwork,
        }),
        (ServerRequest::PermissionsApproval(_), ClientResponse::PermissionsApproval(grant)) => {
            let files = grant.permissions.file_system.clone().flatten();
            let files = files.unwrap_or_default();
            body(codex_answer::Of::Grant(GrantAnswer {
                read: files.read.unwrap_or_default(),
                write: files.write.unwrap_or_default(),
                network: grant
                    .permissions
                    .network
                    .clone()
                    .flatten()
                    .and_then(|network| network.enabled)
                    .unwrap_or(false),
                for_session: grant.scope == Some(GrantScope::Session),
            }))
        }
        (ServerRequest::RequestUserInput(params), ClientResponse::UserInput(answered)) => {
            let responses = params
                .questions
                .iter()
                .map(|question| {
                    let labels = question
                        .options
                        .iter()
                        .flatten()
                        .map(|option| option.label.as_str())
                        .collect::<Vec<_>>();
                    let mut response = QuestionResponse::default();
                    for picked in answered
                        .answers
                        .get(&question.id)
                        .map(|answer| answer.answers.as_slice())
                        .unwrap_or_default()
                    {
                        if let Some(noted) = picked.strip_prefix(super::USER_NOTE) {
                            response.note = Some(noted.to_owned());
                            continue;
                        }
                        match labels.iter().position(|label| label == picked) {
                            Some(index) => response.selected.push(index as u32),
                            None => response.other = Some(picked.clone()),
                        }
                    }
                    response
                })
                .collect();
            body(codex_answer::Of::Question(QuestionAnswer {
                answers: responses,
            }))
        }
        (ServerRequest::Elicitation(params), ClientResponse::Elicitation(answered)) => {
            let action = match answered.action {
                ElicitationAction::Accept => FormAction::Accept,
                ElicitationAction::Decline => FormAction::Decline,
                ElicitationAction::Cancel => FormAction::Cancel,
                ElicitationAction::Other(_) => return None,
            };
            let approval_kind = params
                .meta
                .as_ref()
                .and_then(|meta| meta.codex_approval_kind.as_deref());
            if approval_kind == Some("mcp_tool_call") {
                let session = answered
                    .meta
                    .as_ref()
                    .is_some_and(|meta| meta.persist.as_deref() == Some("session"));
                approve(match action {
                    FormAction::Accept if session => wire::Decision::ApproveSession,
                    FormAction::Accept => wire::Decision::Approve,
                    FormAction::Decline => wire::Decision::Deny,
                    _ => wire::Decision::Abort,
                })
            } else if params.mode == "url" {
                body(codex_answer::Of::Link(LinkAnswer {
                    action: action as i32,
                }))
            } else {
                body(codex_answer::Of::Form(FormAnswer {
                    action: action as i32,
                    content_json: answered
                        .content
                        .as_ref()
                        .filter(|content| !content.is_null())
                        .map(|content| content.to_string().into_bytes())
                        .unwrap_or_default(),
                }))
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(dir: &str, us: i64, message: serde_json::Value) -> String {
        serde_json::json!({"us": us, "dir": dir, "line": message.to_string()}).to_string()
    }

    fn inputs(events: Vec<Event>) -> Vec<codex_input::Of> {
        events
            .into_iter()
            .filter_map(|event| match event {
                Event::Input(input) => match input.decode_body()? {
                    wire::KindInput::Codex(CodexInput { of: Some(of) }) => Some(of),
                    _ => None,
                },
                _ => None,
            })
            .collect()
    }

    /// amux replies instead of answering Codex's question by interrupting
    /// the turn and starting the next with the person's words; read back,
    /// that is the reply, under the id the words were sent with.
    #[test]
    fn an_interrupt_while_a_question_waits_then_words_is_a_reply_instead() {
        let question = serde_json::json!({
            "id": 0,
            "method": "item/tool/requestUserInput",
            "params": {
                "threadId": "t1",
                "turnId": "turn-1",
                "itemId": "q1",
                "questions": [{"id": "shape", "header": "Shape", "question": "Which shape?"}],
            },
        });
        let interrupt = serde_json::json!({
            "id": "host-1",
            "method": "turn/interrupt",
            "params": {"threadId": "t1", "turnId": "turn-1"},
        });
        let words = serde_json::json!({
            "id": "host-2",
            "method": "turn/start",
            "params": {
                "threadId": "t1",
                "clientUserMessageId": "7231",
                "input": [{"type": "text", "text": "Never mind.", "text_elements": []}],
            },
        });
        let recording = [
            line("stdout", 1_000, question),
            line("stdin", 2_000, interrupt.clone()),
            line("stdin", 3_000, words),
        ]
        .join("\n");
        let got = inputs(read("codex_io", recording.as_bytes()).unwrap());
        let [codex_input::Of::Answer(answer)] = got.as_slice() else {
            panic!("one answer, not {got:?}");
        };
        assert_eq!(answer.ask_key, "0");
        assert_eq!(
            answer.of,
            Some(codex_answer::Of::Reply(wire::ReplyInstead {
                text: "Never mind.".into(),
                answers_so_far: Vec::new(),
            }))
        );

        // With the question answered first, an interrupt is an interrupt.
        let answered = serde_json::json!({"id": 0, "result": {"answers": {}}});
        let recording = [
            line(
                "stdout",
                1_000,
                serde_json::json!({
                    "id": 0,
                    "method": "item/tool/requestUserInput",
                    "params": {"threadId": "t1", "turnId": "turn-1", "itemId": "q1", "questions": []},
                }),
            ),
            line("stdin", 1_500, answered),
            line("stdin", 2_000, interrupt),
        ]
        .join("\n");
        let got = inputs(read("codex_io", recording.as_bytes()).unwrap());
        assert!(
            matches!(got.last(), Some(codex_input::Of::Interrupt(_))),
            "{got:?}"
        );
    }

    /// amux starts on a plan by leaving plan mode with Codex's own words for
    /// carrying it out; read back, that is the plan's start answer, under
    /// the id the words were sent with, not a mode change and then a prompt.
    #[test]
    fn a_turn_leaving_plan_mode_to_implement_a_plan_is_the_start_answer() {
        let plan = serde_json::json!({
            "method": "item/completed",
            "params": {
                "threadId": "t1",
                "turnId": "turn-1",
                "item": {"type": "plan", "id": "p1", "text": "Change the line."},
            },
        });
        let turn = |text: &str| {
            serde_json::json!({
                "id": "host-1",
                "method": "turn/start",
                "params": {
                    "threadId": "t1",
                    "clientUserMessageId": "7231",
                    "collaborationMode": {"mode": "default", "settings": {"model": "m"}},
                    "input": [{"type": "text", "text": text, "text_elements": []}],
                },
            })
        };
        let recording = [
            line("stdout", 1_000, plan.clone()),
            line("stdin", 2_000, turn(super::super::IMPLEMENT)),
        ]
        .join("\n");
        let events = read("codex_io", recording.as_bytes()).unwrap();
        let ids = events
            .iter()
            .filter_map(|event| match event {
                Event::Input(input) => Some(input.input_id.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(ids, [vec![0x72, 0x31]]);
        let got = inputs(events);
        let [codex_input::Of::Answer(answer)] = got.as_slice() else {
            panic!("one answer, not {got:?}");
        };
        assert_eq!(answer.ask_key, "plan:p1");
        assert_eq!(
            answer.of,
            Some(codex_answer::Of::Plan(wire::PlanAnswer {
                choice: wire::PlanChoice::Start as i32,
                note: None,
            }))
        );

        // Other words after a plan are the person's own prompt.
        let recording = [
            line("stdout", 1_000, plan),
            line("stdin", 2_000, turn("Something else.")),
        ]
        .join("\n");
        let got = inputs(read("codex_io", recording.as_bytes()).unwrap());
        assert!(
            matches!(
                got.as_slice(),
                [codex_input::Of::Mode(_), codex_input::Of::Prompt(_)]
            ),
            "{got:?}"
        );
    }
}
