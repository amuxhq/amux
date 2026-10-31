//! A client of Codex's app-server, for driving live Codex and replaying its
//! recordings: it starts the server on stdio (or takes lines already open),
//! performs the handshake and routes what the server sends to the thread it
//! names.
//!
//! Every message it sends or reads is a `codex_protocol` type; the agent
//! itself speaks to Codex through `codex::host` and the interpreter instead.

pub mod config;
pub(crate) mod dispatch;
pub mod error;
pub mod event;
pub mod server;
pub mod thread;
pub mod thread_event_stream;
pub mod transport;

pub use config::CodexConfig;
pub use error::Error;
pub use event::{Event, ThreadEvent};
pub use server::Codex;
pub use thread::{Thread, text_input};
pub use thread_event_stream::ThreadEventStream;

/// Spawn a codex app-server subprocess, perform the initialize handshake,
/// and return a ready-to-use `Codex` handle.
pub async fn connect(config: CodexConfig) -> Result<Codex, Error> {
    Codex::connect(config).await
}
