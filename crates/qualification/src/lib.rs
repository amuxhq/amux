//! Environment-dependent qualification: performance workloads measured
//! on enrolled machines against reviewed baselines, and the live provider
//! lane's recordings, which its two probes (`claude-probe`, `codex-probe`)
//! capture in front of the real providers and join to the corpora.

pub mod perf;
pub mod recording;
