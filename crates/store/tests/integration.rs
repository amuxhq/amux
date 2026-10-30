//! Every integration suite of the `store` crate, one module each, in one
//! test binary: one link per crate instead of one per suite.

mod conformance;
mod explore;
mod retention;
mod sqlite;
