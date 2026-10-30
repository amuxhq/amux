//! Every integration suite of the `amux` crate, one module each, in one
//! test binary: one link per crate instead of one per suite.

// Every suite here runs the built binary on Unix, and so does its support.
#[cfg(unix)]
mod support;

mod agent_process;
mod attach;
mod cli_words;
mod codex_attach;
mod overlap;
mod pairing;
mod process;
mod relay;
mod supervise_cli;
mod system_journeys;
mod term_fails_fast;
