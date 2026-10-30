//! Every integration suite of the `provider-fakes` crate, one module each, in one
//! test binary: one link per crate instead of one per suite.

mod support;

mod codex;
mod conformance;
mod pty;
mod scripted;
mod scripts;
mod sdk;
