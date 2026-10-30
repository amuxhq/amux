//! Every integration suite of the `models` crate, one module each, in one
//! test binary: one link per crate instead of one per suite.

mod client;
mod composed;
mod family;
mod lifecycle;
mod path;
mod presence;
mod relay;
mod replication;
mod single_daemon;
mod socket;
mod supervisor;
mod switch;
