pub mod ios_verify;
pub mod swift_types;

/// The repository's wall-clock bound wrapper (`scripts/bounded`), addressed
/// from this crate so the tools work from any working directory. It defers to
/// `timeout` when one is on the path.
pub const BOUNDED: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../scripts/bounded");
