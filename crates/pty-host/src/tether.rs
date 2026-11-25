//! Ties a provider to the agent process that started it. The tether lives
//! in `os::process` now, beside the family it is part of; this keeps its
//! old path working until its callers move.

pub use os::process::Tether;
