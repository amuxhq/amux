//! The provider's process tree, for stopping it.
//!
//! Killing the provider's process group is not enough to end what it runs:
//! headless Claude's and Codex's tool shells each lead a session, and so a
//! group, of their own, and headless Claude does not even exit on the end
//! of its input while a background shell runs. So the agent takes a census
//! of every process descended from the provider when it asks the provider
//! to finish, and once the provider has exited (or is killed) kills
//! whatever of that census still runs. A command the provider cleaned up
//! itself on SIGTERM is gone by then and is not touched.
//!
//! The census itself lives in `os::process` ([`os::process::descendants`])
//! now, beside the agent's `Family` that replaces it; this keeps the
//! agent's calls as they were until they move.

/// One process found below the provider.
pub(crate) use os::process::Descendant;

/// Every process descended from `root`, not `root` itself. Empty where
/// the table cannot be read.
pub(crate) fn descendants(root: u32) -> Vec<Descendant> {
    os::process::descendants(root)
}

/// SIGKILLs each of `census` that is still the process it was.
pub(crate) fn kill(census: &[Descendant]) {
    os::process::kill_descendants(census);
}
