//! What an agent's record keeps of each event and conclusion: its kind, its
//! size and the names of its schema, never a value. Reading those takes
//! the event's content, so it happens here, beside the interpreters that
//! read the rest; the record holds only the shapes.
//!
//! A tool server chooses its own tool names and input fields, which can
//! say more than a schema should, so they are returned as
//! [`Name::Server`] for the record to alias. The provider's own names are
//! [`Name::Own`]; a key that does not look like a schema's field name is
//! aliased all the same, since some objects are maps keyed by data.

use std::borrow::Cow;
use std::collections::BTreeMap;

use prost::Message as _;
use serde::Deserialize;
use serde::de::IgnoredAny;
use serde_json::value::RawValue;

use crate::{Fact, claude_common, skeleton};

/// A name the record keeps.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Name {
    /// The provider's own: a built-in tool's name, a field of its schema.
    Own(String),
    /// A tool server's own choice, which the record aliases.
    Server(String),
}

/// A tool call or result inside a fact.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolShape {
    pub result: bool,
    /// The tool, when the fact names it.
    pub name: Option<Name>,
    /// The top-level keys of its input, or of its result.
    pub keys: Vec<Name>,
}

/// A provider fact as the record keeps it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FactShape {
    /// The kind its protocol tags it as, joined with `/`: `assistant`,
    /// `stream_event/content_block_delta/text_delta`, `hook/PreToolUse`,
    /// `item/started/commandExecution`, `response`. `unreadable` when the
    /// payload is not a JSON object.
    pub variant: String,
    pub tools: Vec<ToolShape>,
    /// The provider's version, on a fact that reports it.
    pub version: Option<String>,
}

/// A person's input as the record keeps it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InputShape {
    /// The input's arm: `prompt`, `answer`, `interrupt`, ...
    pub arm: String,
    /// For an answer, its own arm, and the ask it answers.
    pub answer: Option<String>,
    pub ask_key: Option<String>,
    /// For a withdraw or a send-now, the queued input it names.
    pub target: Option<Vec<u8>>,
}

/// An item body as the record keeps it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ItemShape {
    pub arm: String,
    /// No append may follow.
    pub complete: bool,
}

/// The interpreter's state as the record keeps it at a checkpoint: what is
/// open and pending, by key.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Structure {
    /// Every open item's key and arm.
    pub open_items: Vec<(String, String)>,
    /// The queue's input ids, in order.
    pub queue: Vec<Vec<u8>>,
    /// Every open ask's key and the item key it is drawn on.
    pub asks: Vec<(String, String)>,
    /// Prompts handed to the provider and not yet recorded by it.
    pub submitted: Vec<Vec<u8>>,
    pub pending_messages: Vec<Vec<u8>>,
    /// The running turn's number.
    pub turn: Option<u64>,
    pub phase: i32,
}

/// The fields of a provider fact the shape reads, across Claude's stream,
/// its transcript and hooks, and Codex's JSON-RPC. Everything else is
/// skipped unread.
#[derive(Deserialize, Default)]
#[serde(default)]
struct Top<'a> {
    #[serde(rename = "type", borrow)]
    ty: Option<Cow<'a, str>>,
    #[serde(borrow)]
    subtype: Option<Cow<'a, str>>,
    #[serde(borrow)]
    method: Option<Cow<'a, str>>,
    #[serde(borrow)]
    hook_event_name: Option<Cow<'a, str>>,
    id: Option<IgnoredAny>,
    error: Option<IgnoredAny>,
    #[serde(borrow)]
    result: Option<&'a RawValue>,
    #[serde(borrow)]
    message: Option<&'a RawValue>,
    #[serde(borrow)]
    event: Option<&'a RawValue>,
    #[serde(borrow)]
    request: Option<&'a RawValue>,
    #[serde(borrow)]
    response: Option<&'a RawValue>,
    #[serde(borrow)]
    params: Option<&'a RawValue>,
    #[serde(borrow)]
    tool_name: Option<Cow<'a, str>>,
    #[serde(borrow)]
    tool_input: Option<&'a RawValue>,
    #[serde(borrow)]
    tool_response: Option<&'a RawValue>,
    #[serde(borrow, alias = "toolUseResult")]
    tool_use_result: Option<&'a RawValue>,
    #[serde(borrow)]
    claude_code_version: Option<Cow<'a, str>>,
    #[serde(borrow)]
    version: Option<Cow<'a, str>>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Tagged<'a> {
    #[serde(rename = "type", borrow)]
    ty: Option<Cow<'a, str>>,
    #[serde(borrow)]
    subtype: Option<Cow<'a, str>>,
    #[serde(borrow)]
    name: Option<Cow<'a, str>>,
    #[serde(borrow)]
    server: Option<Cow<'a, str>>,
    #[serde(borrow)]
    tool: Option<Cow<'a, str>>,
    #[serde(borrow)]
    tool_name: Option<Cow<'a, str>>,
    #[serde(borrow)]
    input: Option<&'a RawValue>,
    #[serde(borrow)]
    arguments: Option<&'a RawValue>,
    #[serde(borrow)]
    content: Option<&'a RawValue>,
    #[serde(borrow)]
    delta: Option<&'a RawValue>,
    #[serde(borrow)]
    content_block: Option<&'a RawValue>,
    #[serde(borrow)]
    item: Option<&'a RawValue>,
    #[serde(borrow, rename = "userAgent")]
    user_agent: Option<Cow<'a, str>>,
}

fn tagged(raw: Option<&RawValue>) -> Tagged<'_> {
    raw.and_then(|raw| serde_json::from_str(raw.get()).ok())
        .unwrap_or_default()
}

/// The top-level keys of an object; none for anything else.
fn keys(raw: Option<&RawValue>) -> Vec<String> {
    raw.and_then(|raw| serde_json::from_str::<BTreeMap<String, IgnoredAny>>(raw.get()).ok())
        .map(|map| map.into_keys().collect())
        .unwrap_or_default()
}

/// A version string as a provider prints one, or nothing.
fn version(text: &str) -> Option<String> {
    let first = text.split_whitespace().next()?;
    (first.len() <= 64
        && first
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._/+-".contains(&b)))
    .then(|| first.to_owned())
}

/// A tool named the way Claude names one: a tool server's own as
/// `mcp__<server>__<tool>`, amux's own server excepted.
fn claude_tool(name: &str) -> Name {
    let (server, _) = claude_common::split_tool_name(name);
    if server.is_empty() || server == crate::AMUX_TOOL_SERVER {
        Name::Own(name.to_owned())
    } else {
        Name::Server(name.to_owned())
    }
}

/// A tool's keys: a tool server's all its own, a built-in tool's its
/// schema's only when `schema` lists them (None when they are the
/// protocol's own fields), and any other the data's, aliased as a server's.
fn names(keys: Vec<String>, server: bool, schema: Option<&[&str]>) -> Vec<Name> {
    keys.into_iter()
        .map(|key| {
            if !server && schema.is_none_or(|schema| schema.contains(&key.as_str())) {
                Name::Own(key)
            } else {
                Name::Server(key)
            }
        })
        .collect()
}

fn join(parts: &[Option<&str>]) -> String {
    parts
        .iter()
        .flatten()
        .copied()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("/")
}

/// The shape of any kind's fact. The providers' JSON shares its tags (a
/// `type`, a `method`, a hook's event name), so one reader serves them all.
pub fn fact(fact: &Fact) -> FactShape {
    let Ok(top) = serde_json::from_slice::<Top>(&fact.payload) else {
        return FactShape {
            variant: "unreadable".into(),
            ..FactShape::default()
        };
    };
    let mut shape = FactShape::default();
    let mut tools = Vec::new();
    shape.version = top
        .claude_code_version
        .as_deref()
        .or(top.version.as_deref())
        .and_then(version);
    if let Some(hook) = top.hook_event_name.as_deref() {
        shape.variant = join(&[Some("hook"), Some(hook)]);
        let server = top.tool_name.as_deref().map(claude_tool);
        let own = !matches!(server, Some(Name::Server(_)));
        if top.tool_input.is_some() {
            tools.push(ToolShape {
                result: false,
                name: server.clone(),
                keys: names(keys(top.tool_input), !own, Some(skeleton::TOOL_INPUT)),
            });
        }
        if top.tool_response.is_some() {
            tools.push(ToolShape {
                result: true,
                name: server,
                keys: names(keys(top.tool_response), !own, Some(skeleton::TOOL_RESULT)),
            });
        }
    } else if let Some(method) = top.method.as_deref() {
        // Codex's JSON-RPC: a notification, or a request of the server's
        // when it carries an id.
        let params = tagged(top.params);
        let item = tagged(params.item);
        let request = top.id.is_some().then_some("request");
        shape.variant = join(&[request, Some(method), item.ty.as_deref()]);
        if let Some(ty) = item.ty.as_deref()
            && WORK.contains(&ty)
        {
            let server =
                ty == "mcpToolCall" && item.server.as_deref() != Some(crate::AMUX_TOOL_SERVER);
            let name = if ty == "mcpToolCall" {
                Name::Server(join(&[item.server.as_deref(), item.tool.as_deref()]))
            } else {
                Name::Own(ty.to_owned())
            };
            let completed = method.ends_with("completed");
            tools.push(ToolShape {
                result: completed,
                name: Some(name),
                keys: if ty == "mcpToolCall" {
                    names(keys(item.arguments), server, Some(skeleton::TOOL_INPUT))
                } else {
                    names(keys(params.item), false, None)
                },
            });
        }
    } else if top.id.is_some() && (top.result.is_some() || top.error.is_some()) {
        shape.variant = if top.error.is_some() {
            "response/error".into()
        } else {
            "response".into()
        };
        shape.version = tagged(top.result)
            .user_agent
            .as_deref()
            .and_then(version)
            .or(shape.version);
    } else {
        let event = tagged(top.event);
        let delta = tagged(event.delta);
        let block = tagged(event.content_block);
        let request = tagged(top.request);
        let response = tagged(top.response);
        shape.variant = join(&[
            top.ty.as_deref(),
            top.subtype.as_deref(),
            event.ty.as_deref(),
            delta.ty.as_deref().or(block.ty.as_deref()),
            request.subtype.as_deref(),
            response.subtype.as_deref(),
        ]);
        if shape.variant.is_empty() {
            shape.variant = "untyped".into();
        }
        if let Some(name) = block.name.as_deref() {
            tools.push(ToolShape {
                result: false,
                name: Some(claude_tool(name)),
                keys: Vec::new(),
            });
        }
        if let Some(name) = request.tool_name.as_deref() {
            let name = claude_tool(name);
            let server = matches!(name, Name::Server(_));
            tools.push(ToolShape {
                result: false,
                name: Some(name),
                keys: names(keys(request.input), server, Some(skeleton::TOOL_INPUT)),
            });
        }
        let message = tagged(top.message);
        if let Some(blocks) = message
            .content
            .and_then(|raw| serde_json::from_str::<Vec<Tagged>>(raw.get()).ok())
        {
            for block in blocks {
                match block.ty.as_deref() {
                    // Only the call block Claude's protocol crate types:
                    // one it holds as written (`server_tool_use`,
                    // `mcp_tool_use`) keeps none of its names in a record.
                    Some("tool_use") => {
                        let name = block.name.as_deref().map(claude_tool);
                        let server = matches!(name, Some(Name::Server(_)));
                        tools.push(ToolShape {
                            result: false,
                            name,
                            keys: names(keys(block.input), server, Some(skeleton::TOOL_INPUT)),
                        });
                    }
                    Some(ty) if ty.ends_with("tool_result") => tools.push(ToolShape {
                        result: true,
                        name: None,
                        keys: Vec::new(),
                    }),
                    _ => {}
                }
            }
        }
        if top.tool_use_result.is_some() {
            tools.push(ToolShape {
                result: true,
                name: None,
                keys: names(
                    keys(top.tool_use_result),
                    false,
                    Some(skeleton::TOOL_RESULT),
                ),
            });
        }
    }
    shape.tools = tools;
    shape
}

/// Codex's items that are tool calls.
const WORK: &[&str] = &[
    "commandExecution",
    "fileChange",
    "mcpToolCall",
    "webSearch",
    "imageView",
    "imageGeneration",
    "collabAgentToolCall",
];

fn answer_of(ask_key: String, arm: &str) -> InputShape {
    InputShape {
        arm: "answer".into(),
        answer: Some(arm.into()),
        ask_key: Some(ask_key),
        target: None,
    }
}

fn plain(arm: &str) -> InputShape {
    InputShape {
        arm: arm.into(),
        ..InputShape::default()
    }
}

fn targeting(arm: &str, target: &[u8]) -> InputShape {
    InputShape {
        arm: arm.into(),
        target: Some(target.to_vec()),
        ..InputShape::default()
    }
}

fn claude_answer(answer: wire::ClaudeAnswer) -> InputShape {
    use wire::claude_answer::Of;
    let arm = match answer.of {
        None => "none",
        Some(Of::Permission(_)) => "permission",
        Some(Of::Question(_)) => "question",
        Some(Of::Plan(_)) => "plan",
        Some(Of::Form(_)) => "form",
        Some(Of::Link(_)) => "link",
        Some(Of::Reply(_)) => "reply",
    };
    answer_of(answer.ask_key, arm)
}

/// Headless Claude's input.
pub fn claude_sdk_input(body: &[u8]) -> InputShape {
    use wire::claude_sdk_input::Of;
    match wire::ClaudeSdkInput::decode(body)
        .ok()
        .and_then(|input| input.of)
    {
        None => plain("unreadable"),
        Some(Of::Prompt(_)) => plain("prompt"),
        Some(Of::Answer(answer)) => claude_answer(answer),
        Some(Of::Interrupt(_)) => plain("interrupt"),
        Some(Of::Permission(_)) => plain("permission"),
        Some(Of::Model(_)) => plain("model"),
        Some(Of::Clear(_)) => plain("clear"),
        Some(Of::Withdraw(withdraw)) => targeting("withdraw", &withdraw.queued_input_id),
        Some(Of::Effort(_)) => plain("effort"),
        Some(Of::SendNow(send)) => targeting("send_now", &send.queued_input_id),
        Some(Of::StopJob(_)) => plain("stop_job"),
    }
}

/// Terminal Claude's input.
pub fn claude_pty_input(body: &[u8]) -> InputShape {
    use wire::claude_pty_input::Of;
    match wire::ClaudePtyInput::decode(body)
        .ok()
        .and_then(|input| input.of)
    {
        None => plain("unreadable"),
        Some(Of::Prompt(_)) => plain("prompt"),
        Some(Of::Answer(answer)) => claude_answer(answer),
        Some(Of::CyclePermissionMode(_)) => plain("cycle_permission_mode"),
        Some(Of::Clear(_)) => plain("clear"),
        Some(Of::Interrupt(_)) => plain("interrupt"),
        Some(Of::Withdraw(withdraw)) => targeting("withdraw", &withdraw.queued_input_id),
        Some(Of::SendNow(send)) => targeting("send_now", &send.queued_input_id),
        Some(Of::SwitchBack(_)) => plain("switch_back"),
    }
}

/// Codex's input.
pub fn codex_input(body: &[u8]) -> InputShape {
    use wire::codex_input::Of;
    match wire::CodexInput::decode(body)
        .ok()
        .and_then(|input| input.of)
    {
        None => plain("unreadable"),
        Some(Of::Prompt(_)) => plain("prompt"),
        Some(Of::Answer(answer)) => {
            use wire::codex_answer::Of;
            let arm = match answer.of {
                None => "none",
                Some(Of::Approve(_)) => "approve",
                Some(Of::Question(_)) => "question",
                Some(Of::Form(_)) => "form",
                Some(Of::Link(_)) => "link",
                Some(Of::Grant(_)) => "grant",
                Some(Of::Plan(_)) => "plan",
                Some(Of::Reply(_)) => "reply",
            };
            answer_of(answer.ask_key, arm)
        }
        Some(Of::Interrupt(_)) => plain("interrupt"),
        Some(Of::Permission(_)) => plain("permission"),
        Some(Of::Mode(_)) => plain("mode"),
        Some(Of::Model(_)) => plain("model"),
        Some(Of::Withdraw(withdraw)) => targeting("withdraw", &withdraw.queued_input_id),
        Some(Of::Effort(_)) => plain("effort"),
        Some(Of::SendNow(send)) => targeting("send_now", &send.queued_input_id),
        Some(Of::StopJob(_)) => plain("stop_job"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Channel;

    fn shape(channel: Channel, json: serde_json::Value) -> FactShape {
        fact(&Fact {
            channel,
            payload: serde_json::to_vec(&json).unwrap(),
        })
    }

    #[test]
    fn a_tool_call_keeps_its_name_and_keys_and_never_a_value() {
        let call = shape(
            Channel::Stream,
            serde_json::json!({
                "type": "assistant",
                "message": {"content": [
                    {"type": "text", "text": "secret words"},
                    {"type": "tool_use", "id": "toolu_1", "name": "Bash",
                     "input": {"command": "cat secret.txt", "description": "secret"}},
                    {"type": "tool_use", "id": "toolu_2", "name": "mcp__acme__lookup",
                     "input": {"customer": "secret"}},
                ]},
            }),
        );
        assert_eq!(call.variant, "assistant");
        assert_eq!(
            call.tools,
            [
                ToolShape {
                    result: false,
                    name: Some(Name::Own("Bash".into())),
                    keys: vec![Name::Own("command".into()), Name::Own("description".into())],
                },
                ToolShape {
                    result: false,
                    name: Some(Name::Server("mcp__acme__lookup".into())),
                    keys: vec![Name::Server("customer".into())],
                },
            ]
        );
        assert!(!format!("{call:?}").contains("secret"));
    }

    #[test]
    fn each_providers_tags_name_the_variant() {
        let hook = shape(
            Channel::Hook,
            serde_json::json!({"hook_event_name": "PostToolUse", "tool_name": "Read",
                "tool_input": {"file_path": "/x"}, "tool_response": {"file": {}}}),
        );
        assert_eq!(hook.variant, "hook/PostToolUse");
        assert_eq!(hook.tools.len(), 2);
        let rpc = shape(
            Channel::Rpc,
            serde_json::json!({"method": "item/completed", "params": {"item":
                {"type": "commandExecution", "id": "1", "command": "ls", "status": "completed"}}}),
        );
        assert_eq!(rpc.variant, "item/completed/commandExecution");
        assert!(rpc.tools[0].result);
        let delta = shape(
            Channel::Stream,
            serde_json::json!({"type": "stream_event", "event":
                {"type": "content_block_delta", "delta": {"type": "text_delta", "text": "hi"}}}),
        );
        assert_eq!(delta.variant, "stream_event/content_block_delta/text_delta");
        let init = shape(
            Channel::Stream,
            serde_json::json!({"type": "system", "subtype": "init", "claude_code_version": "2.1.312"}),
        );
        assert_eq!(
            (init.variant.as_str(), init.version.as_deref()),
            ("system/init", Some("2.1.312"))
        );
        let response = shape(
            Channel::Rpc,
            serde_json::json!({"id": 1, "result": {"userAgent": "codex_cli_rs/0.47.0 (Mac OS)"}}),
        );
        assert_eq!(response.version.as_deref(), Some("codex_cli_rs/0.47.0"));
        assert_eq!(
            fact(&Fact {
                channel: Channel::Stream,
                payload: b"not json".to_vec()
            })
            .variant,
            "unreadable"
        );
    }
}
