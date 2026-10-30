//! Every integration suite of the `node` crate, one module each, in one
//! test binary: one link per crate instead of one per suite.

mod support;

mod addresses;
mod agent_names;
mod analytics;
mod blobs;
mod catalogue;
mod diff;
mod dump;
mod forwarded_send;
mod front_door;
mod host_catalogue;
mod hosts;
mod messaging;
mod peer_service;
mod push;
mod registry;
mod resumption;
mod single_daemon;
mod supervisor;
mod worktree;
