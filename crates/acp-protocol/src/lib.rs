//! The Agent Client Protocol (ACP, version 1) as types, and nothing else.
//!
//! ACP is JSON-RPC 2.0 over an agent program's stdin and stdout, one
//! message per line. Its two sides are the *client*, the editor (here
//! amux), and the *agent*, the program amux runs. The names follow ACP's
//! own schema and say who sends a message: [`ClientRequest`],
//! [`ClientNotification`] and [`ClientResponse`] are what amux writes;
//! [`AgentRequest`], [`AgentNotification`] and [`AgentResponse`] what the
//! agent writes. Next to the other protocol crates, the agent is the
//! server: [`decode`] reads what the agent sends and [`encode`] writes what
//! amux sends; [`decode_client`] and [`encode_server`] are the other two
//! directions, for the fakes and the recording checks.
//!
//! | here | `codex-protocol` |
//! |---|---|
//! | [`AgentMessage`] | `ServerMessage` |
//! | [`ClientMessage`] | `ClientMessage` |
//! | [`AgentRequest`], [`AgentNotification`] | `ServerRequest`, `ServerNotification` |
//! | [`AgentResponse`] (read with [`AgentResponse::read`]) | each `*Response` read with `result` |
//!
//! Decoding is tolerant: a method this crate does not know is
//! [`AgentMessage::Unknown`] with the whole line kept; an extension method
//! (one starting `_`, which ACP sets aside for vendors) is
//! [`AgentMessage::Extension`] with its params kept; an unknown tag of a
//! nested object (a session update kind, a content block type) is that
//! enum's `Unknown`; an unknown spelling of a string enum is its `Other`;
//! and unknown fields of a known object are kept in its `extra` map. Each
//! object's `_meta`, ACP's place for vendor data, is kept as written. A
//! field the schema lets be null is an `Option<Option<T>>` (read with
//! [`present_nullable`]): `None` when absent, `Some(None)` when null, as
//! ACP gives some nulls a meaning of their own (a terminal killed by a
//! signal has a null exit code). Encoding writes all of it back, so
//! decoding then encoding a line gives the same JSON. [`strict`] turns every one of those fallbacks into a
//! [`Drift`]: a method, tag, spelling or field this crate does not type, or
//! an extension not listed in [`EXTENSIONS`]. That is how the recording
//! checks catch an agent changing what it writes.
//!
//! The crate is pure: no process, pipe or async code, so the interpreter
//! the phone links can depend on it.

mod macros;
mod probe;

pub mod agent;
pub mod client;
pub mod content;
pub mod elicitation;
pub mod session;
pub mod update;
pub mod xai;

use std::fmt;

pub use agent::{AgentNotification, AgentRequest, AgentResponse};
pub use client::{ClientNotification, ClientRequest, ClientResponse};
pub use content::ContentBlock;
pub use probe::{Fields, is_map};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use serde_json::{Map, Value};
pub use update::SessionUpdate;

/// The subtrees of a line whose fields are the data's rather than this
/// protocol's, each a path from the line's root (`[]` an array's element)
/// or `*.<name>`, any field called `name` at any depth: vendor data under
/// `_meta`, which ACP allows on nearly every object; a tool call's raw
/// input and output, whatever the tool took and gave (and Antigravity's
/// copy of the input as `args`); an error's details; and a form's answers,
/// keyed by the form's own field names.
pub const DATA_SUBTREES: &[&str] = &[
    "*._meta",
    "*.rawInput",
    "*.rawOutput",
    "params.update.args",
    "params.toolCall.args",
    "error.data",
    "result.content",
];

/// The maps of a line keyed by the data, whose values this protocol types
/// (`*.<name>` any field called `name`): a form's fields by their names,
/// and the environment a sign-in command runs with, by variable.
pub const DATA_MAPS: &[&str] = &[
    "params.requestedSchema.properties",
    "result.authMethods[].env",
];

/// The lists of items that hold the item a single message carries, as
/// Codex's turns hold its items. ACP sends every update on its own, so it
/// has none.
pub const ITEM_LISTS: &[&str] = &[];

/// The extension methods agents are known to send, which [`strict`] accepts:
/// each was seen in a recording of the agent named beside it. Their params
/// are vendor data and stay untyped, except a request [`xai`] types.
pub const EXTENSIONS: &[&str] = &[
    // Grok: its product announcements and settings, at start.
    "_x.ai/announcements/update",
    "_x.ai/settings/update",
    // Grok: requests for the person. Its questions with options to pick are
    // typed in [`xai`]; an MCP server's form, whose answer was never
    // recorded, a client that does not know it answers "method not found".
    "_x.ai/ask_user_question",
    "_x.ai/mcp/elicit",
    // Grok: its MCP servers starting, and each server's state.
    "_x.ai/mcp/init_progress",
    "_x.ai/mcp/server_status",
    "_x.ai/mcp/servers_updated",
    "_x.ai/mcp_initialized",
    // Grok: the models it offers, with their reasoning efforts.
    "_x.ai/models/update",
    // Grok: its own queue of prompts, which it fills while a turn runs.
    "_x.ai/queue/changed",
    // Grok: a turn's end, beside the `session/prompt` answer.
    "_x.ai/session/prompt_complete",
    // Grok: where `session/new` or `session/load` has got to.
    "_x.ai/session/setup",
    // Grok: its own session events (a hook ran, the model changed),
    // beside ACP's `session/update`.
    "_x.ai/session/update",
    "_x.ai/session_notification",
    // Grok: its sessions on this computer, after one changes.
    "_x.ai/sessions/changed",
];

/// The JSON-RPC version every line carries.
const JSONRPC: &str = "2.0";

/// The fields of an object this crate does not type, kept as written. A
/// strict decode refuses any.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(transparent)]
pub struct Extra(pub Map<String, Value>);

impl Extra {
    pub fn new() -> Self {
        Self::default()
    }
}

impl std::ops::Deref for Extra {
    type Target = Map<String, Value>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for Extra {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl From<Map<String, Value>> for Extra {
    fn from(fields: Map<String, Value>) -> Self {
        Self(fields)
    }
}

impl FromIterator<(String, Value)> for Extra {
    fn from_iter<I: IntoIterator<Item = (String, Value)>>(fields: I) -> Self {
        Self(fields.into_iter().collect())
    }
}

impl IntoIterator for Extra {
    type Item = (String, Value);
    type IntoIter = serde_json::map::IntoIter;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<'a> IntoIterator for &'a Extra {
    type Item = (&'a String, &'a Value);
    type IntoIter = serde_json::map::Iter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl<'de> Deserialize<'de> for Extra {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut fields = Map::deserialize(deserializer)?;
        probe::heard(&mut fields);
        if macros::is_strict() && !fields.is_empty() {
            return Err(serde::de::Error::custom(untyped(&fields)));
        }
        Ok(Self(fields))
    }
}

fn untyped(fields: &Map<String, Value>) -> String {
    let names: Vec<&str> = fields.keys().map(String::as_str).collect();
    format!("fields this crate does not type: {}", names.join(", "))
}

/// An object with nothing in it but `_meta`: the answer to a request that
/// returns nothing, the params of one that takes nothing, and a capability
/// whose presence is all it says.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Empty {
    #[serde(
        rename = "_meta",
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub meta: Option<Value>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// A JSON-RPC request id. ACP lets either side pick numbers or strings,
/// and an error answering a line nobody could read carries null.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RequestId {
    Integer(i64),
    String(String),
    Null,
}

impl fmt::Display for RequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Integer(id) => write!(f, "{id}"),
            Self::String(id) => f.write_str(id),
            Self::Null => f.write_str("null"),
        }
    }
}

/// A JSON-RPC error answer. ACP's own codes are in [`error_code`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub data: Option<Value>,
    #[serde(flatten)]
    pub extra: Extra,
}

/// The error codes ACP gives a meaning, beside JSON-RPC's own.
pub mod error_code {
    pub const PARSE_ERROR: i64 = -32700;
    pub const INVALID_REQUEST: i64 = -32600;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
    pub const INTERNAL_ERROR: i64 = -32603;
    /// A request cancelled with `$/cancel_request`.
    pub const REQUEST_CANCELLED: i64 = -32800;
    /// The agent needs a sign-in before it starts a session.
    pub const AUTH_REQUIRED: i64 = -32000;
    /// A session, or another thing named by id, the agent does not know.
    pub const RESOURCE_NOT_FOUND: i64 = -32002;
}

/// A whole line this crate could not type, kept so production can log it
/// and carry on.
#[derive(Debug, Clone)]
pub struct Unknown {
    /// The line's method, when it has one.
    pub method: Option<String>,
    /// The line's id, when it has one: a request nobody can read still
    /// needs an answer, or the agent waits on it.
    pub id: Option<RequestId>,
    pub raw: Box<RawValue>,
}

impl Unknown {
    fn new(raw: &RawValue, method: Option<String>) -> Self {
        #[derive(Deserialize)]
        struct Envelope {
            #[serde(default)]
            id: Option<RequestId>,
        }
        let id = serde_json::from_str::<Envelope>(raw.get())
            .ok()
            .and_then(|envelope| envelope.id);
        Self {
            method,
            id,
            raw: raw.to_owned(),
        }
    }

    /// The session the line names in its params, when it names one.
    pub fn session_id(&self) -> Option<String> {
        session_named(self.raw.get())
    }
}

fn session_named(line: &str) -> Option<String> {
    #[derive(Deserialize)]
    struct Envelope {
        params: Params,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Params {
        session_id: String,
    }
    serde_json::from_str::<Envelope>(line)
        .ok()
        .map(|envelope| envelope.params.session_id)
}

impl PartialEq for Unknown {
    fn eq(&self, other: &Self) -> bool {
        self.method == other.method && self.id == other.id && self.raw.get() == other.raw.get()
    }
}

/// A vendor's own method, one starting `_`, with its params as written.
#[derive(Debug, Clone, PartialEq)]
pub struct Extension {
    pub method: String,
    /// Present for a request, which waits for an answer.
    pub id: Option<RequestId>,
    /// `Null` when the line has none.
    pub params: Value,
}

impl Extension {
    /// Whether [`strict`] accepts it: whether it is one of [`EXTENSIONS`].
    pub fn is_known(&self) -> bool {
        EXTENSIONS.contains(&self.method.as_str())
    }
}

/// One line from the agent.
#[derive(Debug, Clone, PartialEq)]
pub enum AgentMessage {
    Notification {
        notification: AgentNotification,
        extra: Extra,
    },
    Request {
        id: RequestId,
        request: AgentRequest,
        extra: Extra,
    },
    /// An answer to one of amux's requests. The result's shape depends on
    /// the request; [`AgentResponse::read`] reads it by that request's
    /// method.
    Response {
        id: RequestId,
        result: Result<Value, RpcError>,
        extra: Extra,
    },
    Extension {
        extension: Extension,
        extra: Extra,
    },
    Unknown(Unknown),
}

/// One line amux sends to the agent. Each is built, encoded and dropped at
/// once, so a large request is not worth boxing.
#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::large_enum_variant)]
pub enum ClientMessage {
    Request {
        id: RequestId,
        request: ClientRequest,
        extra: Extra,
    },
    Response {
        id: RequestId,
        response: Result<ClientResponse, RpcError>,
        extra: Extra,
    },
    Notification {
        notification: ClientNotification,
        extra: Extra,
    },
    Extension {
        extension: Extension,
        extra: Extra,
    },
    /// The answer to a vendor's request this crate types
    /// ([`xai::ExtensionAnswer`]). Read only where the line is no answer to
    /// an ACP request.
    ExtensionAnswer {
        id: RequestId,
        answer: xai::ExtensionAnswer,
        extra: Extra,
    },
    /// Only [`decode_client`] produces this: a line amux would not write.
    Unknown(Unknown),
}

impl ClientMessage {
    pub fn request(id: RequestId, request: ClientRequest) -> Self {
        Self::Request {
            id,
            request,
            extra: Extra::new(),
        }
    }

    pub fn response(id: RequestId, response: ClientResponse) -> Self {
        Self::Response {
            id,
            response: Ok(response),
            extra: Extra::new(),
        }
    }

    pub fn error(id: RequestId, error: RpcError) -> Self {
        Self::Response {
            id,
            response: Err(error),
            extra: Extra::new(),
        }
    }

    pub fn notification(notification: ClientNotification) -> Self {
        Self::Notification {
            notification,
            extra: Extra::new(),
        }
    }
}

/// A line that is not a JSON object.
#[derive(Debug)]
pub struct DecodeError(serde_json::Error);

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "not a JSON object: {}", self.0)
    }
}

impl std::error::Error for DecodeError {}

/// What [`strict`] refuses: a line some part of which this crate does not
/// know.
#[derive(Debug)]
pub enum Drift {
    NotAnObject(DecodeError),
    /// `method` is None for a line with no method (an answer).
    Unknown {
        method: Option<String>,
        reason: String,
    },
}

impl fmt::Display for Drift {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAnObject(error) => error.fmt(f),
            Self::Unknown {
                method: Some(method),
                reason,
            } => write!(f, "{method}: {reason}"),
            Self::Unknown {
                method: None,
                reason,
            } => write!(f, "a line with no method: {reason}"),
        }
    }
}

impl std::error::Error for Drift {}

/// Decodes one line from the agent. Fails only when the line is not a JSON
/// object.
pub fn decode(line: &[u8]) -> Result<AgentMessage, DecodeError> {
    let (object, raw) = parse(line)?;
    Ok(decode_agent_object(object, raw)
        .unwrap_or_else(|(unknown, _)| AgentMessage::Unknown(unknown)))
}

/// Decodes one line from the agent, failing on anything [`decode`] would
/// have to keep as unknown.
pub fn strict(line: &[u8]) -> Result<AgentMessage, Drift> {
    let (object, raw) = parse(line).map_err(Drift::NotAnObject)?;
    macros::strictly(|| decode_agent_object(object, raw)).map_err(|(unknown, reason)| {
        Drift::Unknown {
            method: unknown.method,
            reason,
        }
    })
}

/// Decodes one line amux sent to the agent.
pub fn decode_client(line: &[u8]) -> Result<ClientMessage, DecodeError> {
    let (object, raw) = parse(line)?;
    Ok(decode_client_object(object, raw)
        .unwrap_or_else(|(unknown, _)| ClientMessage::Unknown(unknown)))
}

/// [`decode_client`], failing on anything it would keep as unknown.
pub fn strict_client(line: &[u8]) -> Result<ClientMessage, Drift> {
    let (object, raw) = parse(line).map_err(Drift::NotAnObject)?;
    macros::strictly(|| decode_client_object(object, raw)).map_err(|(unknown, reason)| {
        Drift::Unknown {
            method: unknown.method,
            reason,
        }
    })
}

/// The field names [`decode`] types at each place of a line from the agent
/// (see [`Fields`]); None for a line that is not a JSON object. An answer
/// is read as each answer type this crate has, as nothing in it says which
/// request it answers: a name is typed where any of them types it.
pub fn fields(line: &[u8]) -> Option<Fields> {
    probe::probe(line, DATA_SUBTREES, DATA_MAPS, |line| {
        let message = match decode(line) {
            Ok(AgentMessage::Unknown(_)) => return envelope_only(line),
            Err(_) => return,
            Ok(message) => message,
        };
        let extra = match &message {
            AgentMessage::Notification { extra, .. }
            | AgentMessage::Request { extra, .. }
            | AgentMessage::Response { extra, .. }
            | AgentMessage::Extension { extra, .. } => extra,
            AgentMessage::Unknown(_) => return,
        };
        probe::heard_at("", extra.keys().cloned());
        match &message {
            AgentMessage::Response {
                result: Ok(result), ..
            } => AgentResponse::read_as_each(result),
            AgentMessage::Extension { extension, .. } => {
                xai::read_params(&extension.method, &extension.params);
            }
            _ => {}
        }
    })
}

/// The field names [`decode_client`] types at each place of a line amux
/// wrote to the agent.
pub fn client_fields(line: &[u8]) -> Option<Fields> {
    probe::probe(line, DATA_SUBTREES, DATA_MAPS, |line| {
        let extra = match decode_client(line) {
            Ok(
                ClientMessage::Request { extra, .. }
                | ClientMessage::Response { extra, .. }
                | ClientMessage::Notification { extra, .. }
                | ClientMessage::Extension { extra, .. }
                | ClientMessage::ExtensionAnswer { extra, .. },
            ) => extra,
            Ok(ClientMessage::Unknown(_)) => return envelope_only(line),
            Err(_) => return,
        };
        probe::heard_at("", extra.keys().cloned());
    })
}

/// A line whose method this crate does not know still has the envelope
/// every line has.
fn envelope_only(line: &[u8]) {
    const ENVELOPE: &[&str] = &["jsonrpc", "id", "method", "params", "result", "error"];
    if let Ok(object) = serde_json::from_slice::<Map<String, Value>>(line) {
        probe::heard_at(
            "",
            object
                .keys()
                .filter(|key| !ENVELOPE.contains(&key.as_str()))
                .cloned(),
        );
    }
}

/// One line for the agent, with no trailing newline. Keys are written in
/// sorted order, as `serde_json` writes any object.
pub fn encode(message: &ClientMessage) -> Vec<u8> {
    let object = match message {
        ClientMessage::Request { id, request, extra } => {
            envelope(extra, Some(request.method()), Some(id), request.params())
        }
        ClientMessage::Response {
            id,
            response,
            extra,
        } => {
            let mut object = envelope(extra, None, Some(id), Value::Null);
            match response {
                Ok(response) => object.insert("result".into(), to_value(response)),
                Err(error) => object.insert("error".into(), to_value(error)),
            };
            object
        }
        ClientMessage::Notification {
            notification,
            extra,
        } => envelope(
            extra,
            Some(notification.method()),
            None,
            notification.params(),
        ),
        ClientMessage::Extension { extension, extra } => extension_envelope(extension, extra),
        ClientMessage::ExtensionAnswer { id, answer, extra } => {
            let mut object = envelope(extra, None, Some(id), Value::Null);
            object.insert("result".into(), to_value(answer));
            object
        }
        ClientMessage::Unknown(unknown) => return unknown.raw.get().as_bytes().to_vec(),
    };
    serde_json::to_vec(&object).expect("a JSON object serializes")
}

/// One line as the agent would write it, for the fakes.
pub fn encode_server(message: &AgentMessage) -> Vec<u8> {
    let object = match message {
        AgentMessage::Notification {
            notification,
            extra,
        } => envelope(
            extra,
            Some(notification.method()),
            None,
            notification.params(),
        ),
        AgentMessage::Request { id, request, extra } => {
            envelope(extra, Some(request.method()), Some(id), request.params())
        }
        AgentMessage::Response { id, result, extra } => {
            let mut object = envelope(extra, None, Some(id), Value::Null);
            match result {
                Ok(result) => object.insert("result".into(), result.clone()),
                Err(error) => object.insert("error".into(), to_value(error)),
            };
            object
        }
        AgentMessage::Extension { extension, extra } => extension_envelope(extension, extra),
        AgentMessage::Unknown(unknown) => return unknown.raw.get().as_bytes().to_vec(),
    };
    serde_json::to_vec(&object).expect("a JSON object serializes")
}

/// Reads a response's result as a type of the caller's choosing.
pub fn result<T: serde::de::DeserializeOwned>(result: &Value) -> Result<T, serde_json::Error> {
    T::deserialize(result)
}

/// [`result`], failing where it would keep anything unknown, as [`strict`]
/// does for a whole line.
pub fn strict_result<T: serde::de::DeserializeOwned>(
    result: &Value,
) -> Result<T, serde_json::Error> {
    macros::strictly(|| T::deserialize(result))
}

/// Reads a field that may be absent or null, keeping which: `None` when
/// absent, `Some(None)` when null. With `skip_serializing_if =
/// "Option::is_none"` it writes back what it read.
pub fn present_nullable<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::deserialize(deserializer).map(Some)
}

/// Reads a field whose type has a value for null (any JSON, a request
/// id), keeping a null as that value: `None` only when the field is
/// absent, so it writes back what it read.
pub fn present<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

fn to_value(value: &impl Serialize) -> Value {
    serde_json::to_value(value).expect("protocol types serialize")
}

fn envelope(
    extra: &Extra,
    method: Option<&str>,
    id: Option<&RequestId>,
    params: Value,
) -> Map<String, Value> {
    let mut object = extra.0.clone();
    object
        .entry("jsonrpc")
        .or_insert_with(|| Value::String(JSONRPC.into()));
    if let Some(id) = id {
        object.insert("id".into(), to_value(id));
    }
    if let Some(method) = method {
        object.insert("method".into(), Value::String(method.into()));
    }
    if !params.is_null() {
        object.insert("params".into(), params);
    }
    object
}

fn extension_envelope(extension: &Extension, extra: &Extra) -> Map<String, Value> {
    envelope(
        extra,
        Some(&extension.method),
        extension.id.as_ref(),
        extension.params.clone(),
    )
}

fn parse(line: &[u8]) -> Result<(Map<String, Value>, Box<RawValue>), DecodeError> {
    let object = serde_json::from_slice::<Map<String, Value>>(line).map_err(DecodeError)?;
    let raw = serde_json::from_slice::<Box<RawValue>>(line).map_err(DecodeError)?;
    Ok((object, raw))
}

type Failed = (Unknown, String);

/// The parts every message shares, taken out of the object; what remains
/// is the message's `extra`.
struct Parts {
    method: Option<String>,
    id: Option<RequestId>,
    params: Value,
    rest: Map<String, Value>,
}

fn parts(mut object: Map<String, Value>, raw: &RawValue) -> Result<Parts, Failed> {
    let fail =
        |method: Option<String>, reason: &str| (Unknown::new(raw, method), reason.to_owned());
    let method = match object.remove("method") {
        None => None,
        Some(Value::String(method)) => Some(method),
        Some(_) => return Err(fail(None, "a method that is not a string")),
    };
    // Every line says it is JSON-RPC 2.0, so the envelope writes it back
    // rather than keeping it; anything else stays with the line's extras.
    match object.get("jsonrpc") {
        Some(Value::String(version)) if version == JSONRPC => {
            object.remove("jsonrpc");
        }
        _ if macros::is_strict() => {
            return Err(fail(method, "a line that does not say jsonrpc 2.0"));
        }
        _ => {}
    }
    let id = match object.remove("id") {
        None => None,
        Some(id) => Some(serde_json::from_value::<RequestId>(id).map_err(|_| {
            fail(
                method.clone(),
                "an id that is neither a number nor a string",
            )
        })?),
    };
    let params = object.remove("params").unwrap_or(Value::Null);
    Ok(Parts {
        method,
        id,
        params,
        rest: object,
    })
}

fn unknown(raw: &RawValue, method: Option<String>, reason: impl fmt::Display) -> Failed {
    (Unknown::new(raw, method), reason.to_string())
}

fn typed<T>(
    raw: &RawValue,
    method: String,
    decoded: Option<Result<T, serde_json::Error>>,
) -> Result<T, Failed> {
    match decoded {
        Some(Ok(value)) => Ok(value),
        Some(Err(error)) => Err(unknown(raw, Some(method), error)),
        None => Err(unknown(
            raw,
            Some(method),
            "a method this crate does not know",
        )),
    }
}

/// The envelope's own fields that are left, refused by a strict decode.
fn envelope_extra(
    raw: &RawValue,
    method: Option<&str>,
    rest: Map<String, Value>,
) -> Result<Extra, Failed> {
    if macros::is_strict() && !rest.is_empty() {
        return Err(unknown(raw, method.map(str::to_owned), untyped(&rest)));
    }
    Ok(Extra(rest))
}

/// A vendor's method: ACP keeps every method starting `_` for extensions.
fn extension(
    raw: &RawValue,
    method: String,
    id: Option<RequestId>,
    params: Value,
) -> Result<Extension, Failed> {
    let extension = Extension { method, id, params };
    if macros::is_strict() && !extension.is_known() {
        return Err(unknown(
            raw,
            Some(extension.method),
            "an extension this crate does not list",
        ));
    }
    Ok(extension)
}

fn decode_agent_object(
    object: Map<String, Value>,
    raw: Box<RawValue>,
) -> Result<AgentMessage, Failed> {
    let Parts {
        method,
        id,
        params,
        mut rest,
    } = parts(object, &raw)?;
    match (method, id) {
        (Some(method), id) if method.starts_with('_') => {
            let extra = envelope_extra(&raw, Some(&method), rest)?;
            let extension = extension(&raw, method, id, params)?;
            Ok(AgentMessage::Extension { extension, extra })
        }
        (Some(method), Some(id)) => {
            let request = typed(
                &raw,
                method.clone(),
                AgentRequest::from_parts(&method, params),
            )?;
            let extra = envelope_extra(&raw, Some(&method), rest)?;
            Ok(AgentMessage::Request { id, request, extra })
        }
        (Some(method), None) => {
            let notification = typed(
                &raw,
                method.clone(),
                AgentNotification::from_parts(&method, params),
            )?;
            let extra = envelope_extra(&raw, Some(&method), rest)?;
            Ok(AgentMessage::Notification {
                notification,
                extra,
            })
        }
        (None, Some(id)) => {
            let result = answer(&raw, &mut rest, params, Ok)?;
            let extra = envelope_extra(&raw, None, rest)?;
            Ok(AgentMessage::Response { id, result, extra })
        }
        (None, None) => Err(unknown(&raw, None, "neither a method nor an id")),
    }
}

fn decode_client_object(
    object: Map<String, Value>,
    raw: Box<RawValue>,
) -> Result<ClientMessage, Failed> {
    let Parts {
        method,
        id,
        params,
        mut rest,
    } = parts(object, &raw)?;
    match (method, id) {
        (Some(method), id) if method.starts_with('_') => {
            let extra = envelope_extra(&raw, Some(&method), rest)?;
            let extension = extension(&raw, method, id, params)?;
            Ok(ClientMessage::Extension { extension, extra })
        }
        (Some(method), Some(id)) => {
            let request = typed(
                &raw,
                method.clone(),
                ClientRequest::from_parts(&method, params),
            )?;
            let extra = envelope_extra(&raw, Some(&method), rest)?;
            Ok(ClientMessage::Request { id, request, extra })
        }
        (Some(method), None) => {
            let notification = typed(
                &raw,
                method.clone(),
                ClientNotification::from_parts(&method, params),
            )?;
            let extra = envelope_extra(&raw, Some(&method), rest)?;
            Ok(ClientMessage::Notification {
                notification,
                extra,
            })
        }
        (None, Some(id)) => {
            // An answer to a vendor's request is one that reads exactly as
            // a vendor answer this crate types and as none of ACP's, which
            // a tolerant read alone would take for an empty answer.
            let vendor = rest
                .get("result")
                .filter(|result| macros::strictly(|| ClientResponse::deserialize(*result)).is_err())
                .and_then(|result| macros::strictly(|| xai::ExtensionAnswer::first(result)).ok());
            if let Some(answer) = vendor {
                rest.remove("result");
                if !params.is_null() {
                    rest.insert("params".into(), params);
                }
                let extra = envelope_extra(&raw, None, rest)?;
                return Ok(ClientMessage::ExtensionAnswer { id, answer, extra });
            }
            let response = answer(&raw, &mut rest, params, |result| {
                serde_json::from_value::<ClientResponse>(result).map_err(|error| {
                    unknown(
                        &raw,
                        None,
                        format!("an answer this crate does not know: {error}"),
                    )
                })
            })?;
            let extra = envelope_extra(&raw, None, rest)?;
            Ok(ClientMessage::Response {
                id,
                response,
                extra,
            })
        }
        (None, None) => Err(unknown(&raw, None, "neither a method nor an id")),
    }
}

/// A response's result or error. `params` is put back: a response has none,
/// so a `params` key is just another unknown field.
fn answer<T>(
    raw: &RawValue,
    rest: &mut Map<String, Value>,
    params: Value,
    read: impl FnOnce(Value) -> Result<T, Failed>,
) -> Result<Result<T, RpcError>, Failed> {
    if !params.is_null() {
        rest.insert("params".into(), params);
    }
    if let Some(result) = rest.remove("result") {
        return read(result).map(Ok);
    }
    match rest.remove("error") {
        Some(error) => serde_json::from_value::<RpcError>(error)
            .map(Err)
            .map_err(|error| unknown(raw, None, format!("an error that does not decode: {error}"))),
        None => Err(unknown(
            raw,
            None,
            "an id with neither a result nor an error",
        )),
    }
}
