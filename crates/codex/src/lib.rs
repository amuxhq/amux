//! Starting Codex's app-server and talking to it as a client.
//!
//! Every message this crate sends or reads is a `codex_protocol` type; this
//! crate owns only the process, the lines in and out, and routing what the
//! server sends to the thread it names. [`host`] is how an agent runs one
//! server on a socket that other clients can join.

pub mod config;
pub(crate) mod dispatch;
pub mod error;
pub mod event;
pub mod host;
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
