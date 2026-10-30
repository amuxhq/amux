//! Every integration suite of the `agent` crate, one module each, in one
//! test binary: one link per crate instead of one per suite.

mod support;

mod attach;
mod codex_thread_name;
mod dump;
mod lifecycle;
mod providers;
mod tools;
