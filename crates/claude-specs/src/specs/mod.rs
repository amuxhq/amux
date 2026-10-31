//! Executable specifications for terminal Claude.
//!
//! A specification is one async function that drives Claude in a terminal
//! and states what must hold. The same function runs in two modes: against a
//! live Claude it records the session, and against that recording it
//! verifies the claim. Nothing is authored in between, which is what stops a
//! specification describing traffic that was never observed.
//!
//! A specification asserts only what survives a re-capture; everything
//! transcript-shaped stays in the recording and is enforced instead by strict
//! replay, which requires every recorded frame to be consumed.
//!
//! Headless Claude has no specifications: its corpus is recorded through
//! amux's own interpreter in the live lane (`fixtures/live/sdk`).

pub mod pty;

/// Specifications reach for Haiku first: shorter turns make smaller, more
/// stable recordings, and nothing below needs a larger model to be true.
pub(crate) const HAIKU: &str = "claude-haiku-4-5-20251001";
pub const MINIMUM_SUPPORTED: &str = "2.1.247";

/// A 32×32 solid red PNG that image scenarios hand to the provider, and that
/// live capture writes into the working directory as `square.png`.
pub const SQUARE_PNG: &[u8] = include_bytes!("../../assets/square.png");
pub const ALLOWED_MODELS: &[&str] = &[HAIKU, "claude-sonnet-5"];

/// Live captures amux's own interpreter drove, which no specification
/// replays: `sdk/` and `pty/`.
pub fn live_fixtures_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/live")
}

#[derive(Debug, thiserror::Error)]
#[error("specification {spec} failed: {claim}")]
pub struct SpecFailure {
    pub spec: String,
    pub claim: String,
}
