//! What this node holds, for the clients attached to it: InventoryService,
//! SessionService and DraftService.

use std::sync::Weak;

use tonic::service::RoutesBuilder;

use crate::api::Entry;
use crate::runtime::ProfileRuntime;

/// Mounts the local services for `entry`, which is never a link.
pub(in crate::api) fn mount(
    _routes: &mut RoutesBuilder,
    _runtime: &Weak<ProfileRuntime>,
    _entry: Entry,
) {
}
