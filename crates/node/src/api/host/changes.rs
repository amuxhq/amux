//! What changed in this host's agents' working trees: ReviewService.

use std::sync::Weak;

use tonic::service::RoutesBuilder;

use crate::api::Entry;
use crate::runtime::ProfileRuntime;

/// Mounts ReviewService for `entry`.
pub(in crate::api) fn mount(
    _routes: &mut RoutesBuilder,
    _runtime: &Weak<ProfileRuntime>,
    _entry: Entry,
) {
}
