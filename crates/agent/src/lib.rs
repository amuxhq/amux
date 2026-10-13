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
//! The lifecycle rules (grace, drain, stop modes, the one-shot exit) are
//! described in the host module; the clock they run on is injected, so tests
//! drive every deadline by hand.

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
mod ring;
mod terminal;
mod tools;
mod tree;

use std::path::PathBuf;
use std::sync::Arc;

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

/// Why an agent process ended: the cause its final step records, on the
/// boundary and as the incarnation's end the daemon puts on the row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExitCause {
    /// The provider exited on its own, with its code when it had one.
    ProviderExited(Option<i32>),
    /// The daemon went away and did not come back within the grace.
    DaemonLost,
    /// An ask stayed open past the drain deadline with no daemon to carry an
    /// answer.
    Orphaned,
    /// Stopped: the turn was cancelled the provider's own way, then the
    /// agent exited.
    Stopped,
    /// Killed: the process group ended at once; nothing more was written.
    Killed,
    /// One-shot: a child agent finished its work.
    Finished,
    /// The provider could not be started.
    Unstarted(String),
    /// The interpreter ended the incarnation.
    Interpreter(String),
    /// A write to the agent directory failed, most often because the disk
    /// is full.
    WriteFailed(String),
}

impl ExitCause {
    /// The cause as the wire carries it, with its own words as the detail.
    pub fn end(&self) -> wire::IncarnationEnd {
        let (cause, detail) = match self {
            ExitCause::ProviderExited(code) => return interpret::provider_exit(*code),
            ExitCause::DaemonLost => (wire::ExitCause::DaemonLost, None),
            ExitCause::Orphaned => (wire::ExitCause::Orphaned, None),
            ExitCause::Stopped => (wire::ExitCause::Stopped, None),
            ExitCause::Killed => (wire::ExitCause::Killed, None),
            ExitCause::Finished => (wire::ExitCause::Finished, None),
            ExitCause::Unstarted(why) => (
                wire::ExitCause::CouldNotStart,
                Some(format!("the provider: {why}")),
            ),
            // No interpreter asks to exit today; were one to, its words are
            // all that says why.
            ExitCause::Interpreter(why) => (wire::ExitCause::EndedUnexpectedly, Some(why.clone())),
            ExitCause::WriteFailed(why) => (
                wire::ExitCause::DirectoryFailed,
                Some(format!("could not write to it: {why}")),
            ),
        };
        wire::IncarnationEnd {
            cause: cause as i32,
            detail,
        }
    }
}

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
    /// The provider's own state in private/ could not be written.
    #[error("writing {}: {source}", path.display())]
    Private {
        path: PathBuf,
        source: std::io::Error,
    },
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
    match wire::Kind::try_from(spec.kind) {
        Ok(wire::Kind::ClaudeSdk) => host::run::<ClaudeSdk>(dir, spec, clock).await,
        Ok(wire::Kind::ClaudePty) => host::run::<ClaudePty>(dir, spec, clock).await,
        Ok(wire::Kind::Codex) => host::run::<Codex>(dir, spec, clock).await,
        Ok(wire::Kind::Unspecified) | Err(_) => Err(AgentError::UnknownKind(spec.kind)),
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
