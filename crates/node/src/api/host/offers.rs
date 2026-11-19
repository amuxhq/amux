//! What this host offers before any agent exists: HostService.

use std::sync::Weak;

use tonic::service::RoutesBuilder;

use crate::api::Entry;
use crate::runtime::ProfileRuntime;

/// Mounts HostService for `entry`.
pub(in crate::api) fn mount(
    _routes: &mut RoutesBuilder,
    _runtime: &Weak<ProfileRuntime>,
    _entry: Entry,
) {
}
