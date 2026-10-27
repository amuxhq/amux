//! The agent process: `amux agent <dir>` hosts one provider for one agent.
//!
//! It takes `<dir>/lock` for its lifetime, reads the newest `spec.<n>`,
//! listens on `ctl.sock` (and `pty.sock` and `private/hooks.sock` for the
//! kinds that have them), starts the provider child and runs the kind's
//! interpreter, writing every step to `journal/` and raw terminal bytes to
//! `pty/`. It never dials the daemon: the daemon dials `ctl.sock`, gets a
//! Hello and then a Nudge whenever the journal grows, and sends inputs and
//! stops, each input answered with the interpreter's verdict.
//!
//! The lifecycle rules (grace, drain, stop modes, the one-shot exit) are the
//! agent core's (the `agent-core` crate); the host module drives it. The
//! clock it runs on is injected, so tests drive every deadline by hand.

#![deny(unsafe_op_in_unsafe_fn)]

pub mod attach;
pub mod catalogue;
mod dir;
mod dump;
mod git;
mod host;
mod notebook;
mod provider;
mod ready;
mod terminal;
mod tools;
mod tree;

use std::path::PathBuf;
use std::sync::Arc;

pub use agent_core::ExitCause;
pub use agent_dir::{
    Clock, ManualClock, SystemClock, clock, ctl, local_socket, read_frame, read_message,
    write_frame, write_message,
};
use interpret::claude_pty::ClaudePty;
use interpret::claude_sdk::ClaudeSdk;
use interpret::codex::Codex;

pub use crate::dir::{BLOBS, CTL_SOCK, HOOKS_SOCK, JOURNAL, LOCK, PRIVATE, PTY, PTY_SOCK};
pub use crate::dump::DUMP_ERRORS;
pub use crate::tools::{
    NOT_RUNNING, RETRY_WINDOW, TOOLS_SOCK, ToolsConfig, ToolsError, serve_tools, serve_tools_on,
};

/// The version this agent reports in its Hello and stamps on what it writes.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("another agent process holds {0}")]
    Locked(PathBuf),
    #[error("{0} holds no spec")]
    NoSpec(PathBuf),
    #[error("agents of kind {0} are unknown")]
    UnknownKind(i32),
    #[error("reading the agent directory: {0}")]
    Dir(std::io::Error),
    #[error("listening: {0}")]
    Socket(std::io::Error),
    #[error("writing the journal: {0}")]
    Journal(std::io::Error),
}

/// Runs the agent in `dir` until it exits.
pub async fn run(dir: PathBuf, clock: impl Clock) -> Result<ExitCause, AgentError> {
    let dir = std::path::absolute(&dir).map_err(AgentError::Dir)?;
    let _lock = dir::lock(&dir)
        .map_err(AgentError::Dir)?
        .ok_or_else(|| AgentError::Locked(dir.join(dir::LOCK)))?;
    let (_, spec) = dir::newest_spec(&dir)
        .map_err(AgentError::Dir)?
        .ok_or_else(|| AgentError::NoSpec(dir.clone()))?;
    dir::private_dir(&dir.join(dir::PRIVATE)).map_err(AgentError::Dir)?;
    let clock: Arc<dyn Clock> = Arc::new(clock);
    let trimming = tokio::spawn(trim_logs(dir.clone()));
    let codex_sock = dir.join(dir::PRIVATE).join(dir::CODEX_SOCK);
    let ended = match wire::Kind::try_from(spec.kind) {
        Ok(wire::Kind::ClaudeSdk) => host::run::<ClaudeSdk>(dir, spec, clock).await,
        Ok(wire::Kind::ClaudePty) => host::run::<ClaudePty>(dir, spec, clock).await,
        Ok(wire::Kind::Codex) => host::run::<Codex>(dir, spec, clock).await,
        Ok(wire::Kind::Unspecified) | Err(_) => Err(AgentError::UnknownKind(spec.kind)),
    };
    trimming.abort();
    // Codex binds its socket itself and leaves it behind when it exits.
    let _ = std::fs::remove_file(codex_sock);
    ended
}

/// How often the agent's logs are held to their cap.
const LOG_TRIM_EVERY: std::time::Duration = std::time::Duration::from_secs(30);

/// Keeps the agent's own log and its provider's under their cap. Others
/// write both (this process's standard error, which the daemon opened, and
/// the provider's), so they are trimmed now and then, not on each write.
async fn trim_logs(dir: PathBuf) {
    let logs = [
        dir.join(agent_dir::AGENT_LOG),
        dir.join(dir::PRIVATE).join(dir::PROVIDER_LOG),
    ];
    loop {
        for log in &logs {
            if let Err(error) = log_file::trim(log, log_file::AGENT_CAP) {
                eprintln!("amux agent: trimming {}: {error}", log.display());
            }
        }
        tokio::time::sleep(LOG_TRIM_EVERY).await;
    }
}

/// `amux agent <dir>`: runs the agent on the wall clock and returns the
/// process exit code. The runtime is shut down without waiting for its
/// blocking threads, since a terminal reader can stay blocked for as long
/// as a straggler the provider left behind holds the terminal.
pub fn main(dir: PathBuf) -> i32 {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("amux agent: {error}");
            return 1;
        }
    };
    let code = match runtime.block_on(run(dir, SystemClock)) {
        Ok(_) => 0,
        Err(error) => {
            eprintln!("amux agent: {error}");
            1
        }
    };
    runtime.shutdown_background();
    code
}
