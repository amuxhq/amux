//! Every integration suite of the `testnet` crate, one module each, in one
//! test binary: one link per crate instead of one per suite.

mod stories;
mod support;

mod ctl;
mod family_stories;
mod harness;
mod harness_stories;
mod journal_fixtures;
mod replication_stories;
mod sim_harness;
mod sim_seeds;
mod spec_analytics;
mod spec_edge;
mod spec_families;
mod spec_inventory;
mod spec_links;
mod spec_network;
mod spec_replication;
mod spec_reports;
