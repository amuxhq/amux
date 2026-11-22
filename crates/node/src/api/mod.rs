//! The services as each entry serves them, sorted by who answers.
//!
//! Local services (inventory, sessions, drafts) are answered by this node
//! from what it holds, for clients attached to it: never on a link. Host
//! services (agents, what the host offers, changes) are answered by this
//! host for what it owns, on every entry; the reviews experiment's, only
//! while this host runs it and never on an agent's tools socket. The peer
//! service is answered to a paired daemon asking for itself, on links only.

mod caller;
mod entry;
mod host {
    pub(super) mod agents;
    pub(super) mod changes;
    pub(super) mod offers;
    pub(super) mod reviews;
}
mod local;
mod peer;
pub(crate) mod testing;

use std::sync::Weak;

#[cfg(test)]
pub(crate) use caller::Surface;
pub(crate) use caller::{AgentRef as CallingAgent, Caller, surface_named};
#[cfg(test)]
pub(crate) use entry::client_entrance;
pub(crate) use entry::{Entrance, Entry, entrance, layer};
pub use local::Owner;
use tonic::service::RoutesBuilder;

use crate::runtime::ProfileRuntime;

/// The services `entry` serves, for the profile `runtime` runs.
pub(crate) fn routes(runtime: &Weak<ProfileRuntime>, entry: Entry) -> RoutesBuilder {
    let mut routes = RoutesBuilder::default();
    if entry != Entry::Link {
        local::mount(&mut routes, runtime, entry);
    }
    host::agents::mount(&mut routes, runtime, entry);
    host::offers::mount(&mut routes, runtime, entry);
    host::changes::mount(&mut routes, runtime, entry);
    host::reviews::mount(&mut routes, runtime, entry);
    if entry == Entry::Link {
        peer::mount(&mut routes, runtime);
    }
    routes
}
