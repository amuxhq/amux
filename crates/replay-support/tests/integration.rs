//! Every integration suite of the `replay-support` crate, one module each, in one
//! test binary: one link per crate instead of one per suite.

mod bundle;
mod core_replay;
mod dump_show;
mod strict_replay;
mod support;
