//! Claude Code's protocols as types, and nothing else.
//!
//! [`stream`] is the headless stream amux drives Claude through;
//! [`transcript`] the rows of the transcript file terminal Claude writes;
//! [`hooks`] the payloads its hook command forwards; [`messaging`] the
//! lines written to its messaging socket. Decoding is tolerant in
//! production and strict in the recording checks, the same rule as Codex's
//! protocol crate. The crate is pure: no process, socket or async code, so
//! the interpreter the phone links can depend on it.

mod absent;
mod decoding;
mod probe;
mod strictness;

pub mod hooks;
pub mod messaging;
pub mod stream;
pub mod transcript;

pub use decoding::{DecodeError, Drift};
pub use probe::{Fields, is_map};

/// The subtrees of a frame, by path from its root (`[]` an array's
/// element), whose fields are the data's rather than this protocol's: a
/// tool's input and result, a server's schema, per-model tallies, the files
/// a snapshot names. What they hold is whatever the tool, server or person
/// put there, so nothing in them is read as this protocol's field.
pub const DATA_SUBTREES: &[&str] = &[
    "message.content[].input",
    "message.content[].content",
    "tool_use_result",
    "toolUseResult",
    "modelUsage",
    "model_usage_since_last_result",
    "wire_tool_inputs",
    "wireToolInputs",
    "request.input",
    "request.requested_schema",
    "request.permission_suggestions",
    "response.response",
    "tool_input",
    "tool_response",
    "permission_denials[].tool_input",
    "permission_suggestions",
    "event.content_block.input",
    "attachment",
    "snapshot",
    "requested_schema",
];

/// The maps of a frame keyed by the data, whose values this protocol
/// types: a rate limit's windows, by a window's or a model's name.
pub const DATA_MAPS: &[&str] = &["rate_limit_info.unifiedWindows"];
