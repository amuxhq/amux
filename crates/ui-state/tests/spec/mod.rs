//! The client-model spec: authored records, snapshots, inputs and driver
//! messages in; the session and fleet state out, with the window, run and
//! changed-key invariants checked after every message.

mod harness;

mod activity;
mod asks;
mod fleet;
mod following;
mod inputs;
mod open;
mod properties;
mod runs;
mod snapshot;
mod window;
