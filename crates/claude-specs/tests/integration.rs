//! Every integration suite of the `claude-specs` crate, one module each, in one
//! test binary: one link per crate instead of one per suite.

mod capture_proxy;
mod driver_boundary;
mod keymap_provenance;
mod scrub;
mod sdk_boundary;
mod spec_replay;
