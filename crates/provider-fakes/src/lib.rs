//! Scripted stand-ins for the provider binaries amux hosts.
//!
//! Three binaries speak the real protocols: `fake-claude-pty` (a terminal
//! session that writes Claude's transcript JSONL, runs its hooks and serves
//! its messaging socket, without drawing a TUI), `fake-claude-sdk`
//! (stream-JSON over stdio) and `fake-codex` (the app server over stdio or,
//! for several clients at once, a Unix socket; and `resume <thread>`, the
//! terminal view an attach opens, a client of that socket with `--remote`).
//! Each plays an authored [`Script`] named by [`SCRIPT_ENV`], so tests run
//! whole agents offline and deterministically, or plays a recorded session
//! back verbatim ([`PLAYBACK_ENV`]).
//!
//! Two properties hold each fake to its provider. Every recording in the
//! claude-specs and codex-specs corpora plays back through the fake binary
//! over the transport hosts use (Codex's socket on Unix, stdio elsewhere)
//! byte for byte ([`conformance`]), so the fake's
//! transports and a recording's expressiveness as a script are the
//! provider's. And every frame the fake composes for an authored script
//! has a shape some recorded frame of the same kind has ([`shape`]), so a
//! fake that invents or drops fields drifts from its corpus and fails.

pub mod binaries;
pub mod claude;
pub mod clients;
pub mod codex;
pub mod codex_view;
pub mod conformance;
pub mod lines;
pub mod mcp;
pub mod playback;
pub mod pty;
pub mod script;
pub mod sdk;
pub mod shape;

pub use conformance::{Drift, conformance};
pub use script::{
    Ask, INPUT_LOG_ENV, OfferedCommand, OfferedModel, Outcome, PLAYBACK_ENV, SCRIPT_ENV, Script,
    ScriptError, Step, Tool, ToolClass,
};

/// Which provider a fake stands in for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    ClaudePty,
    ClaudeSdk,
    Codex,
}

impl Kind {
    pub fn binary_name(self) -> &'static str {
        match self {
            Kind::ClaudePty => "fake-claude-pty",
            Kind::ClaudeSdk => "fake-claude-sdk",
            Kind::Codex => "fake-codex",
        }
    }
}

/// What a fake binary does, decided from its environment.
pub enum Mode {
    Script(Script),
    Playback(playback::Process),
}

/// Read the mode from [`PLAYBACK_ENV`] or [`SCRIPT_ENV`]; neither set is an
/// empty script, a provider that answers its handshake and never speaks.
pub fn mode_from_env() -> Result<Mode, String> {
    if let Some(selector) = std::env::var_os(PLAYBACK_ENV) {
        let (dir, transport) = playback::parse_selector(&selector.to_string_lossy());
        let process = if transport.is_empty() {
            playback::load(&dir)
                .map_err(|error| error.to_string())?
                .into_iter()
                .next()
                .ok_or_else(|| format!("{} has no process", dir.display()))?
        } else {
            playback::process(&dir, &transport).map_err(|error| error.to_string())?
        };
        return Ok(Mode::Playback(process));
    }
    match std::env::var_os(SCRIPT_ENV) {
        Some(path) => Script::load(std::path::Path::new(&path))
            .map(Mode::Script)
            .map_err(|error| error.to_string()),
        None => Ok(Mode::Script(Script::default())),
    }
}

/// The version the script in [`SCRIPT_ENV`] names, else `own`.
pub fn scripted_version(own: &str) -> String {
    std::env::var_os(SCRIPT_ENV)
        .and_then(|path| Script::load(std::path::Path::new(&path)).ok())
        .and_then(|script| script.version)
        .unwrap_or_else(|| own.to_owned())
}

/// Exit status a fake uses when the host did not do what the recording or
/// protocol says it must.
pub const DRIFT_EXIT: i32 = 70;
