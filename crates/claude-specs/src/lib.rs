//! Recorded Claude protocol corpus checks kept out of product dependency graphs.

pub mod scrub;

/// Live captures amux's own agent drove in the live qualification lane:
/// `sdk/` for headless Claude, `pty/` for terminal Claude.
pub fn live_fixtures_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/live")
}
