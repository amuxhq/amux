//! Every integration suite of the `interpret` crate, one module each, in one
//! test binary: one link per crate instead of one per suite.

mod claude_pty;
mod claude_sdk;
mod codex;
mod coverage;
mod redaction;
