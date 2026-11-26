//! Every integration suite of the `amux` crate, one module each, in one
//! test binary: one link per crate instead of one per suite.

mod support;

mod agent_process;
mod attach;
mod browser;
mod cli_words;
mod codex_attach;
mod new_worktree;
mod overlap;
mod pairing;
mod process;
mod relay;
mod supervise_cli;
mod system_journeys;
#[cfg(windows)] // Windows' installer
mod windows_install;
