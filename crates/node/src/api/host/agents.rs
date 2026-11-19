//! This host's own agents: AgentService.

use std::sync::Weak;

use tonic::service::RoutesBuilder;

use crate::api::Entry;
use crate::runtime::ProfileRuntime;

/// Mounts AgentService for `entry`.
pub(in crate::api) fn mount(
    _routes: &mut RoutesBuilder,
    _runtime: &Weak<ProfileRuntime>,
    _entry: Entry,
) {
}
