//! Hosting Claude Code: its launch settings and the environment it scrubs,
//! hook payloads carried from the hook command to the agent process over a
//! socket, Claude's messaging socket, its session files and its version.
//! Every message type comes from `claude_protocol`; nothing here is shared
//! with the Codex host.

pub mod history;
pub mod hooks;
pub mod launch;
pub mod messaging;
pub mod version;
