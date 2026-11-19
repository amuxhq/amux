//! What a paired daemon asks this host for itself: PeerService.

use std::sync::Weak;

use tonic::service::RoutesBuilder;

use crate::runtime::ProfileRuntime;

/// Mounts PeerService, which only links serve.
pub(in crate::api) fn mount(_routes: &mut RoutesBuilder, _runtime: &Weak<ProfileRuntime>) {}
