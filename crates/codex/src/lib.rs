//! Codex's app-server as one agent hosts it. [`host`] starts the server on a
//! socket other clients can join (stdio on Windows) and carries whole
//! messages to and from it; every message is a `codex_protocol` type, read
//! by the agent and its interpreter.

pub mod host;
