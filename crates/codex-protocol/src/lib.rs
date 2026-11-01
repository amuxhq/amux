//! Codex's app-server protocol as types, and nothing else.
//!
//! One line is one JSON-RPC message. [`decode`] reads what Codex's server
//! sends and [`encode`] writes what amux sends; [`decode_client`] and
//! [`encode_server`] are the other two directions, for the fakes and the
//! recording checks.
//!
//! Decoding is tolerant: a method this crate does not know is
//! [`ServerMessage::Unknown`] with the whole line kept, an unknown `type` of
//! a nested object is that enum's `Unknown`, an unknown spelling of a string
//! enum is its `Other`, and unknown fields of a known message are kept in its
//! `extra` map. Encoding writes all of it back, so decoding then encoding a
//! line gives the same JSON. [`strict`] turns every one of those fallbacks
//! into a [`Drift`], which is how the recording checks catch a provider
//! change.
//!
//! The crate is pure: no process, socket or async code, so the interpreter
//! the phone links can depend on it.

mod macros;

pub mod client;
pub mod items;
pub mod server;
pub mod thread;

use std::fmt;

pub use client::{ClientNotification, ClientRequest, ClientResponse};
pub use items::ThreadItem;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use serde_json::{Map, Value};
pub use server::{ServerNotification, ServerRequest};
pub use thread::{Thread, Turn};

/// The fields of an object this crate does not type, kept as written.
pub type Extra = Map<String, Value>;

/// A JSON-RPC request id: Codex numbers its own, amux names some of its.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RequestId {
    Integer(i64),
    String(String),
}

impl fmt::Display for RequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Integer(id) => write!(f, "{id}"),
            Self::String(id) => f.write_str(id),
        }
    }
}

/// A JSON-RPC error answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    #[serde(flatten)]
    pub extra: Extra,
}

/// A whole line this crate could not type, kept so production can log it
/// and carry on.
#[derive(Debug, Clone)]
pub struct Unknown {
    /// The line's method, when it has one.
    pub method: Option<String>,
    /// The line's id, when it has one: a request nobody can read still
    /// needs an answer, or the server waits on it.
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

    /// The thread the line names in its params, when it names one.
    pub fn thread_id(&self) -> Option<String> {
        #[derive(Deserialize)]
        struct Envelope {
            params: Params,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Params {
            thread_id: String,
        }
        serde_json::from_str::<Envelope>(self.raw.get())
            .ok()
            .map(|envelope| envelope.params.thread_id)
    }
}

impl PartialEq for Unknown {
    fn eq(&self, other: &Self) -> bool {
        self.method == other.method && self.id == other.id && self.raw.get() == other.raw.get()
    }
}

/// One line from Codex's server.
#[derive(Debug, Clone, PartialEq)]
pub enum ServerMessage {
    Notification {
        notification: ServerNotification,
        /// When the server sent it, in milliseconds since the epoch.
        emitted_at_ms: Option<i64>,
        extra: Extra,
    },
    Request {
        id: RequestId,
        request: ServerRequest,
        extra: Extra,
    },
    /// An answer to one of amux's requests. The result's shape depends on
    /// the request; [`result`] reads it as that request's response type.
    Response {
        id: RequestId,
        result: Result<Value, RpcError>,
        extra: Extra,
    },
    Unknown(Unknown),
}

/// One line amux sends to Codex's server. Each is built, encoded and
/// dropped at once, so a large request is not worth boxing.
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
    /// `method` is None for a line with neither a method nor a result.
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

/// Decodes one line from Codex's server. Fails only when the line is not a
/// JSON object.
pub fn decode(line: &[u8]) -> Result<ServerMessage, DecodeError> {
    let (object, raw) = parse(line)?;
    Ok(decode_server_object(object, raw)
        .unwrap_or_else(|(unknown, _)| ServerMessage::Unknown(unknown)))
}

/// Decodes one line from Codex's server, failing on anything [`decode`]
/// would have to keep as unknown.
pub fn strict(line: &[u8]) -> Result<ServerMessage, Drift> {
    let (object, raw) = parse(line).map_err(Drift::NotAnObject)?;
    macros::strictly(|| decode_server_object(object, raw)).map_err(|(unknown, reason)| {
        Drift::Unknown {
            method: unknown.method,
            reason,
        }
    })
}

/// Decodes one line amux sent to Codex's server.
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

/// One line for Codex's server, with no trailing newline. Keys are written
/// in sorted order, as `serde_json` writes any object.
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
        ClientMessage::Unknown(unknown) => return unknown.raw.get().as_bytes().to_vec(),
    };
    serde_json::to_vec(&object).expect("a JSON object serializes")
}

/// One line as Codex's server would write it, for the fakes.
pub fn encode_server(message: &ServerMessage) -> Vec<u8> {
    let object = match message {
        ServerMessage::Notification {
            notification,
            emitted_at_ms,
            extra,
        } => {
            let mut object = envelope(
                extra,
                Some(notification.method()),
                None,
                notification.params(),
            );
            if let Some(at) = emitted_at_ms {
                object.insert("emittedAtMs".into(), Value::from(*at));
            }
            object
        }
        ServerMessage::Request { id, request, extra } => {
            envelope(extra, Some(request.method()), Some(id), request.params())
        }
        ServerMessage::Response { id, result, extra } => {
            let mut object = envelope(extra, None, Some(id), Value::Null);
            match result {
                Ok(result) => object.insert("result".into(), result.clone()),
                Err(error) => object.insert("error".into(), to_value(error)),
            };
            object
        }
        ServerMessage::Unknown(unknown) => return unknown.raw.get().as_bytes().to_vec(),
    };
    serde_json::to_vec(&object).expect("a JSON object serializes")
}

/// Reads a response's result as the response type of the request it
/// answers.
pub fn result<T: serde::de::DeserializeOwned>(result: &Value) -> Result<T, serde_json::Error> {
    T::deserialize(result)
}

/// [`result`], failing where it would keep an unknown enum spelling or
/// object type, as [`strict`] does for a whole line.
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

fn to_value(value: &impl Serialize) -> Value {
    serde_json::to_value(value).expect("protocol types serialize")
}

fn envelope(
    extra: &Extra,
    method: Option<&str>,
    id: Option<&RequestId>,
    params: Value,
) -> Map<String, Value> {
    let mut object = extra.clone();
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

fn decode_server_object(
    object: Map<String, Value>,
    raw: Box<RawValue>,
) -> Result<ServerMessage, Failed> {
    let Parts {
        method,
        id,
        params,
        mut rest,
    } = parts(object, &raw)?;
    match (method, id) {
        (Some(method), Some(id)) => {
            let request = typed(
                &raw,
                method.clone(),
                ServerRequest::from_parts(&method, params),
            )?;
            Ok(ServerMessage::Request {
                id,
                request,
                extra: rest,
            })
        }
        (Some(method), None) => {
            let notification = typed(
                &raw,
                method.clone(),
                ServerNotification::from_parts(&method, params),
            )?;
            let emitted_at_ms = match rest.remove("emittedAtMs") {
                None => None,
                Some(at) => match at.as_i64() {
                    Some(at) => Some(at),
                    None => {
                        return Err(unknown(&raw, Some(method), "emittedAtMs is not an integer"));
                    }
                },
            };
            Ok(ServerMessage::Notification {
                notification,
                emitted_at_ms,
                extra: rest,
            })
        }
        (None, Some(id)) => {
            let result = answer(&raw, &mut rest, params, Ok)?;
            Ok(ServerMessage::Response {
                id,
                result,
                extra: rest,
            })
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
        (Some(method), Some(id)) => {
            let request = typed(
                &raw,
                method.clone(),
                ClientRequest::from_parts(&method, params),
            )?;
            Ok(ClientMessage::Request {
                id,
                request,
                extra: rest,
            })
        }
        (Some(method), None) => {
            let notification = typed(
                &raw,
                method.clone(),
                ClientNotification::from_parts(&method, params),
            )?;
            Ok(ClientMessage::Notification {
                notification,
                extra: rest,
            })
        }
        (None, Some(id)) => {
            let response = answer(&raw, &mut rest, params, |result| {
                serde_json::from_value::<ClientResponse>(result).map_err(|error| {
                    unknown(
                        &raw,
                        None,
                        format!("a response this crate does not know: {error}"),
                    )
                })
            })?;
            Ok(ClientMessage::Response {
                id,
                response,
                extra: rest,
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
