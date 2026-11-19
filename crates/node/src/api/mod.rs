//! The services as each entry serves them, sorted by who answers.
//!
//! Local services (inventory, sessions, drafts) are answered by this node
//! from what it holds, for clients attached to it: never on a link. Host
//! services (agents, what the host offers, changes) are answered by this
//! host for what it owns, on every entry. The peer service is answered to a
//! paired daemon asking for itself, on links only.

#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the caller is read by the handlers each service brings with it"
    )
)]
mod caller;
mod entry;
mod host {
    pub(super) mod agents;
    pub(super) mod changes;
    pub(super) mod offers;
}
mod local;
mod peer;

use std::sync::Weak;

pub(crate) use caller::Caller;
#[cfg(test)]
pub(crate) use caller::Surface;
#[cfg(test)]
pub(crate) use entry::client_entrance;
pub(crate) use entry::{Entrance, Entry, entrance, layer};
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
    if entry == Entry::Link {
        peer::mount(&mut routes, runtime);
    }
    routes
}
