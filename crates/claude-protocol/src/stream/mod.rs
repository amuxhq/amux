//! Claude Code's headless stream: one JSON object per line, Claude's output
//! on stdout and amux's input on stdin.
//!
//! [`decode`] reads a line Claude printed and [`encode`] writes one amux
//! sends; [`decode_input`] and [`encode_output`] are the other two
//! directions, for the fakes and the recording checks. Decoding keeps what
//! it does not know (a frame type, a content block, a stream event, an enum
//! spelling, a field) so encoding writes the same JSON back; [`strict`]
//! refuses a line with any of those, which is how the recording checks
//! catch drift.

pub mod control;
mod error;
pub mod init;
pub mod message;
pub mod types;

pub use control::{
    ControlCancelRequest, ControlRequest, ControlRequestBody, ControlResponse, ElicitationResult,
    FlagSettings, InterruptResult, SetPermissionModeResult, SettingsResult,
};
pub use error::ProtocolError;
pub use init::InitializationResult;
pub use message::*;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use serde_json::{Map, Value};
pub use types::*;

use crate::strictness;
pub use crate::{DecodeError, Drift};

/// One line Claude prints.
#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum Output {
    /// A conversation, status or result frame.
    Message(Message),
    ControlRequest(ControlRequest),
    /// Claude withdrawing a request it asked, unanswered.
    ControlCancelRequest(ControlCancelRequest),
    ControlResponse(ControlResponse),
    Unknown(Unknown),
}

/// One line amux writes to Claude.
#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum Input {
    User(UserInput),
    ControlRequest(ControlRequest),
    ControlResponse(ControlResponse),
    /// Only [`decode_input`] produces this: a line amux would not write.
    Unknown(Unknown),
}

/// A prompt or a message from another agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserInput {
    pub message: MessageParam,
    /// Always written; null for a prompt of the main conversation.
    pub parent_tool_use_id: Option<String>,
    pub session_id: String,
    /// The sender's id for the message; Claude hands it back on the echo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uuid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<String>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

/// A whole line this crate could not type, kept so production can log it
/// and carry on.
#[derive(Debug, Clone)]
pub struct Unknown {
    /// The line's `type`, when it has one.
    pub kind: Option<String>,
    pub raw: Box<RawValue>,
}

/// Decodes one line Claude printed. Fails only when the line is not a JSON
/// object.
pub fn decode(line: &[u8]) -> Result<Output, DecodeError> {
    let (object, raw) = parse(line)?;
    Ok(decode_output(object, raw))
}

/// Decodes one line Claude printed, failing on anything [`decode`] would
/// have to keep without knowing it.
pub fn strict(line: &[u8]) -> Result<Output, Drift> {
    let (object, raw) = parse(line).map_err(Drift::NotAnObject)?;
    let kind = kind_of(&object);
    let (output, mut reasons) = strictness::noticing(|| decode_output(object, raw));
    if let Output::Unknown(_) = output {
        reasons.insert(0, "a frame this crate does not know".into());
    }
    if reasons.is_empty() {
        Ok(output)
    } else {
        Err(Drift::Unknown { kind, reasons })
    }
}

/// Decodes one line amux wrote to Claude.
pub fn decode_input(line: &[u8]) -> Result<Input, DecodeError> {
    let (object, raw) = parse(line)?;
    Ok(decode_input_object(object, raw))
}

/// [`decode_input`], failing on anything it would keep without knowing it.
pub fn strict_input(line: &[u8]) -> Result<Input, Drift> {
    let (object, raw) = parse(line).map_err(Drift::NotAnObject)?;
    let kind = kind_of(&object);
    let (input, mut reasons) = strictness::noticing(|| decode_input_object(object, raw));
    if let Input::Unknown(_) = input {
        reasons.insert(0, "a frame this crate does not know".into());
    }
    if reasons.is_empty() {
        Ok(input)
    } else {
        Err(Drift::Unknown { kind, reasons })
    }
}

/// One line for Claude, with no trailing newline. Keys are written in
/// sorted order, as `serde_json` writes any object.
pub fn encode(input: &Input) -> Vec<u8> {
    match input {
        Input::User(user) => framed("user", user),
        Input::ControlRequest(request) => framed("control_request", request),
        Input::ControlResponse(response) => framed("control_response", response),
        Input::Unknown(unknown) => unknown.raw.get().as_bytes().to_vec(),
    }
}

/// One line as Claude would print it, for the fakes.
pub fn encode_output(output: &Output) -> Vec<u8> {
    match output {
        Output::Message(message) => {
            serde_json::to_vec(&to_value(message)).expect("a JSON object serializes")
        }
        Output::ControlRequest(request) => framed("control_request", request),
        Output::ControlCancelRequest(cancel) => framed("control_cancel_request", cancel),
        Output::ControlResponse(response) => framed("control_response", response),
        Output::Unknown(unknown) => unknown.raw.get().as_bytes().to_vec(),
    }
}

fn to_value(value: &impl Serialize) -> Value {
    serde_json::to_value(value).expect("protocol types serialize")
}

fn framed(kind: &str, payload: &impl Serialize) -> Vec<u8> {
    let mut object = match to_value(payload) {
        Value::Object(object) => object,
        _ => unreachable!("frames are objects"),
    };
    object.insert("type".into(), Value::String(kind.into()));
    serde_json::to_vec(&object).expect("a JSON object serializes")
}

fn parse(line: &[u8]) -> Result<(Map<String, Value>, Box<RawValue>), DecodeError> {
    let object = serde_json::from_slice::<Map<String, Value>>(line).map_err(DecodeError::new)?;
    let raw = serde_json::from_slice::<Box<RawValue>>(line).map_err(DecodeError::new)?;
    Ok((object, raw))
}

fn kind_of(object: &Map<String, Value>) -> Option<String> {
    object
        .get("type")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn unknown(object: &Map<String, Value>, raw: Box<RawValue>) -> Unknown {
    Unknown {
        kind: kind_of(object),
        raw,
    }
}

/// The frame's fields without its `type`.
fn fields<T: for<'de> Deserialize<'de>>(object: &Map<String, Value>) -> Option<T> {
    let mut fields = object.clone();
    fields.remove("type");
    T::deserialize(Value::Object(fields))
        .inspect_err(|error| strictness::unknown(|| format!("malformed frame: {error}")))
        .ok()
}

fn decode_output(object: Map<String, Value>, raw: Box<RawValue>) -> Output {
    let decoded = match kind_of(&object).as_deref() {
        Some("control_request") => fields(&object).map(Output::ControlRequest),
        Some("control_cancel_request") => fields(&object).map(Output::ControlCancelRequest),
        Some("control_response") => fields(&object).map(Output::ControlResponse),
        _ => match Message::parse(Value::Object(object.clone())) {
            Ok(Message::Unknown(_) | Message::UnknownSystem(_)) => None,
            Ok(message) => Some(Output::Message(message)),
            Err(error) => {
                strictness::unknown(|| error.to_string());
                None
            }
        },
    };
    decoded.unwrap_or_else(|| Output::Unknown(unknown(&object, raw)))
}

fn decode_input_object(object: Map<String, Value>, raw: Box<RawValue>) -> Input {
    let decoded = match kind_of(&object).as_deref() {
        Some("user") => fields(&object).map(Input::User),
        Some("control_request") => fields(&object).map(Input::ControlRequest),
        Some("control_response") => fields(&object).map(Input::ControlResponse),
        _ => None,
    };
    decoded.unwrap_or_else(|| Input::Unknown(unknown(&object, raw)))
}
