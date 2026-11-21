//! Every integration suite of the `node` crate, one module each, in one
//! test binary: one link per crate instead of one per suite.

mod support;

mod addresses;
mod agent_names;
mod analytics;
mod blobs;
mod carried;
mod catalogue;
mod changes;
mod dump;
mod front_door;
mod host_acts;
mod host_acts_link;
mod host_acts_messages;
mod host_acts_usage;
mod host_acts_workspaces;
mod host_service;
mod hosts;
mod local_services;
mod messaging;
mod onboarding;
mod peer_server;
mod push;
mod registry;
mod replication_calls;
mod resumption;
mod review_files;
mod reviews;
mod settings;
mod single_daemon;
mod subagents;
mod supervisor;
mod suspension;
mod upgrade;
mod workspaces;
mod worktree;
