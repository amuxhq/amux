//! What every decoder in this crate reports when a line is not readable or,
//! in a strict decode, not fully known.

use std::fmt;

use serde_json::{Map, Value};

use crate::strictness;

/// A line that is not a JSON object.
#[derive(Debug)]
pub struct DecodeError(serde_json::Error);

impl DecodeError {
    pub(crate) fn new(error: serde_json::Error) -> Self {
        Self(error)
    }
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "not a JSON object: {}", self.0)
    }
}

impl std::error::Error for DecodeError {}

/// What a strict decode refuses: a line some part of which this crate does
/// not know.
#[derive(Debug)]
pub enum Drift {
    NotAnObject(DecodeError),
    Unknown {
        kind: Option<String>,
        reasons: Vec<String>,
    },
}

impl fmt::Display for Drift {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAnObject(error) => error.fmt(f),
            Self::Unknown { kind, reasons } => write!(
                f,
                "{}: {}",
                kind.as_deref().unwrap_or("a line with no type"),
                reasons.join("; ")
            ),
        }
    }
}

impl std::error::Error for Drift {}

/// The line as a JSON object.
pub(crate) fn object(line: &[u8]) -> Result<Value, DecodeError> {
    serde_json::from_slice::<Map<String, Value>>(line)
        .map(Value::Object)
        .map_err(DecodeError::new)
}

/// Decodes an object told apart by `field`, failing on anything the decode
/// kept without knowing it.
pub(crate) fn strict<T>(
    line: &[u8],
    field: &str,
    decode: impl FnOnce(Value) -> T,
) -> Result<T, Drift> {
    let object = object(line).map_err(Drift::NotAnObject)?;
    let kind = object.get(field).and_then(Value::as_str).map(str::to_owned);
    let (decoded, reasons) = strictness::noticing(|| decode(object));
    if reasons.is_empty() {
        Ok(decoded)
    } else {
        Err(Drift::Unknown { kind, reasons })
    }
}
