//! Every integration suite of the `provider-fakes` crate, one module each, in one
//! test binary: one link per crate instead of one per suite.

mod support;

mod acp;
mod codex;
mod conformance;
mod faults;
mod pty;
mod scripted;
mod scripts;
mod sdk;
