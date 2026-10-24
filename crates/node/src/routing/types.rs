//! Core routing types: hosts, link identities, and routes.
//!
//! A link is identified locally as `(peer host_id, connection instance)`;
//! nothing about a link's identity crosses the wire. A route is either
//! `Direct` (a link of our own) or `Via` (any adjacent relay).

use std::sync::atomic::{AtomicU64, Ordering};

use uuid::Uuid;
use wire::pb;

use crate::HostId;

/// What a host's link handshake says it can do: feature flags and the agent
/// kinds it runs.
pub type Capabilities = pb::Capabilities;

/// A host as its link handshake describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Host {
    pub id: HostId,
    pub name: String,
    pub version: String,
    pub capabilities: Capabilities,
    /// Whether this host's profile is bound to an account.
    pub signed_in: Option<bool>,
    /// The operating system this machine runs, or unspecified when it
    /// does not say.
    pub platform: pb::Platform,
}

/// Local identity of one link: the authenticated peer plus a connection
/// instance. Two links to the same peer differ only in `instance`, which is
/// unique within the process, so it alone names the link to the core.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LinkId {
    pub(crate) peer: HostId,
    pub(crate) instance: u64,
}

impl LinkId {
    pub(crate) fn new(peer: HostId) -> Self {
        static INSTANCES: AtomicU64 = AtomicU64::new(1);
        Self {
            peer,
            instance: INSTANCES.fetch_add(1, Ordering::Relaxed),
        }
    }

    pub(crate) fn peer(&self) -> HostId {
        self.peer
    }

    /// The link as the profile's core names it.
    pub(crate) fn core(&self) -> daemon_core::LinkId {
        daemon_core::LinkId(self.instance)
    }

    /// The link to `peer` the profile's core names `link`.
    pub(crate) fn from_core(peer: HostId, link: daemon_core::LinkId) -> Self {
        Self {
            peer,
            instance: link.0,
        }
    }
}

impl std::fmt::Display for LinkId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}#{}", self.peer.as_simple(), self.instance)
    }
}

/// A host as the profile's core names it.
pub(crate) fn core_host(host: HostId) -> daemon_core::HostId {
    daemon_core::HostId(*host.as_bytes())
}

/// The host the profile's core names `host`.
pub(crate) fn host_from_core(host: daemon_core::HostId) -> HostId {
    HostId::from_bytes(host.0)
}

/// How this daemon reaches a host: over its own link, or through one
/// adjacent relay. There are no longer routes than this — forwarding is
/// non-recursive by construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Route {
    /// Over a direct link of our own; the call's tunnel is pinned to it.
    Direct(LinkId),
    /// Through an adjacent relay that claims adjacency to the host.
    Via(HostId),
}

impl Route {
    /// The route to `host` the profile's core names `route`.
    pub(crate) fn from_core(host: HostId, route: daemon_core::Route) -> Self {
        match route {
            daemon_core::Route::Direct(link) => Route::Direct(LinkId::from_core(host, link)),
            daemon_core::Route::Via(relay) => Route::Via(host_from_core(relay)),
        }
    }

    /// The route as the profile's core names it.
    pub(crate) fn core(self) -> daemon_core::Route {
        match self {
            Route::Direct(link) => daemon_core::Route::Direct(link.core()),
            Route::Via(relay) => daemon_core::Route::Via(core_host(relay)),
        }
    }
}

impl LiveRoute {
    /// The live route the kind the profile's core reads names.
    pub(crate) fn from_wire(kind: pb::RouteKind) -> Self {
        match kind {
            pb::RouteKind::Direct => Self::Direct,
            pb::RouteKind::Relay => Self::Relay,
            pb::RouteKind::Ssh => Self::Ssh,
            pb::RouteKind::Unspecified => Self::Offline,
        }
    }
}

impl std::fmt::Display for Route {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Route::Direct(link) => write!(f, "direct({link})"),
            Route::Via(relay) => write!(f, "via({})", relay.as_simple()),
        }
    }
}

/// How a host is reached now: the live route new calls take.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LiveRoute {
    Direct,
    Relay,
    Ssh,
    #[default]
    Offline,
}

impl LiveRoute {
    pub fn to_wire(self) -> pb::RouteKind {
        match self {
            Self::Direct => pb::RouteKind::Direct,
            Self::Relay => pb::RouteKind::Relay,
            Self::Ssh => pb::RouteKind::Ssh,
            Self::Offline => pb::RouteKind::Unspecified,
        }
    }
}

pub fn host_to_wire(host: &Host) -> pb::Host {
    pb::Host {
        host_id: host.id.as_bytes().to_vec(),
        name: host.name.clone(),
        version: host.version.clone(),
        capabilities: Some(host.capabilities.clone()),
        account_signed_in: host.signed_in,
        platform: host.platform as i32,
    }
}

pub(crate) fn host_from_wire(host: pb::Host) -> Result<Host, String> {
    let platform = host.platform();
    Ok(Host {
        id: uuid_from_bytes("host_id", host.host_id)?,
        name: host.name,
        version: host.version,
        capabilities: host.capabilities.unwrap_or_default(),
        signed_in: host.account_signed_in,
        platform,
    })
}

fn uuid_from_bytes(name: &str, bytes: Vec<u8>) -> Result<Uuid, String> {
    let bytes: [u8; 16] = bytes
        .try_into()
        .map_err(|bytes: Vec<u8>| format!("{name} must be 16 bytes, got {}", bytes.len()))?;
    Ok(Uuid::from_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_ids_to_one_peer_differ_by_instance() {
        let peer = HostId::from_u128(7);
        let first = LinkId::new(peer);
        let second = LinkId::new(peer);

        assert_eq!(first.peer(), peer);
        assert_ne!(first, second);
    }
}
