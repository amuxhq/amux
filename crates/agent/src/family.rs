//! Everything the agent process starts, contained: its provider, what the
//! provider runs (tool shells, dev servers, amux's own tool server) and the
//! git the agent reads its folder's facts with.
//!
//! The agent's first act is [`Family::contain_self`]
//! ([`os::process::Family`]). On Windows that puts this process in a Job
//! Object of its own, so everything it starts afterwards is born inside it
//! and dies when it dies, however it dies. On Unix each provider leads a
//! process group of its own, tethered to this process, and the census is
//! every process descended from this one. Killing the family is how a stop
//! that the provider did not finish in time ends, and what an agent's exit
//! does to whatever its provider left running: headless Claude's and
//! Codex's tool shells each lead a session of their own, so ending the
//! provider alone would leave them running.

use std::io;
use std::sync::Arc;

/// This process's family, shared by what starts and stops its members. An
/// agent whose process could not be contained ([`Family::uncontained`])
/// starts its provider plainly and kills the provider alone.
#[derive(Clone, Debug, Default)]
pub struct Family(Option<Arc<os::process::Family>>);

impl Family {
    /// Contains this process and everything it starts from now on. Call it
    /// once, first, before starting anything. A process that cannot be
    /// contained (an enclosing job that forbids it, a process table that
    /// can't be read) says so on its standard error and runs uncontained.
    pub fn contain_self() -> Family {
        match os::process::Family::contain_self() {
            Ok(family) => Family(Some(Arc::new(family))),
            Err(error) => {
                eprintln!(
                    "amux: what this process starts is not contained, and can outlive it: {error}"
                );
                Family::uncontained()
            }
        }
    }

    /// No family: each provider is started and killed on its own.
    pub fn uncontained() -> Family {
        Family(None)
    }

    /// Readies `command` to start a member (Unix: a process group of its
    /// own).
    pub(crate) fn prepare(&self, command: &mut tokio::process::Command) {
        if let Some(family) = &self.0 {
            family.prepare(command);
        }
    }

    /// Takes in a child just started from a command [`Family::prepare`]
    /// readied (Unix: tethers its group). Release the member once the child
    /// has exited; None for an uncontained agent.
    pub(crate) fn join(
        &self,
        child: &tokio::process::Child,
    ) -> io::Result<Option<os::process::Member>> {
        self.0.as_ref().map(|family| family.join(child)).transpose()
    }

    /// Counts the family as it stands, before a stop asks the provider to
    /// finish: on Unix what the provider runs in a session of its own is
    /// reparented away once the provider exits, and only a count taken
    /// while it is still below the agent lets [`Family::kill_others`] find
    /// it then.
    pub(crate) fn count(&self) {
        if let Some(family) = &self.0 {
            family.count();
        }
    }

    /// Ends every member at once and returns once they are gone, so no
    /// member still holds the agent's folders when its exit is seen. False
    /// for an uncontained agent, whose caller ends what it can reach
    /// itself.
    pub(crate) fn kill_others(&self) -> bool {
        let Some(family) = &self.0 else {
            return false;
        };
        if let Err(error) = family.kill_others() {
            eprintln!("amux: ending what this process started: {error}");
        }
        true
    }
}
