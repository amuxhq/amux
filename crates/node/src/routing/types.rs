//! Core routing types: hosts, link identities, and routes.
//!
//! A link is identified locally as `(peer host_id, connection instance)`;
//! nothing about a link's identity crosses the wire. A route is either
//! `Direct` (a link of our own) or `Via` (any adjacent relay).

use std::sync::atomic::{AtomicU64, Ordering};

use uuid::Uuid;

use crate::HostId;

/// What a host's link handshake says it can do: feature flags and the agent
/// kinds it runs.
pub type Capabilities = wire::Capabilities;

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
    pub platform: wire::Platform,
}

/// Local identity of one link: the authenticated peer plus a connection
/// instance. Two links to the same peer differ only in `instance`, which is
/// unique within the process, so it alone names the link to the core.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LinkId {
    pub(crate) peer: HostId,
    pub(crate) instance: u64,
}

static INSTANCES: AtomicU64 = AtomicU64::new(1);

impl LinkId {
    pub(crate) fn new(peer: HostId) -> Self {
        Self {
            peer,
            instance: INSTANCES.fetch_add(1, Ordering::Relaxed),
        }
    }

    /// The instance the next link made will have: every link made before
    /// now has a lower one.
    pub(crate) fn next_instance() -> u64 {
        INSTANCES.load(Ordering::Relaxed)
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
}

impl LiveRoute {
    /// The live route the kind the profile's core reads names.
    pub(crate) fn from_wire(kind: wire::RouteKind) -> Self {
        match kind {
            wire::RouteKind::Direct => Self::Direct,
            wire::RouteKind::Relay => Self::Relay,
            wire::RouteKind::Ssh => Self::Ssh,
            wire::RouteKind::Unspecified => Self::Offline,
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
    pub fn to_wire(self) -> wire::RouteKind {
        match self {
            Self::Direct => wire::RouteKind::Direct,
            Self::Relay => wire::RouteKind::Relay,
            Self::Ssh => wire::RouteKind::Ssh,
            Self::Offline => wire::RouteKind::Unspecified,
        }
    }
}

pub fn host_to_wire(host: &Host) -> wire::Host {
    wire::Host {
        host_id: host.id.as_bytes().to_vec(),
        name: host.name.clone(),
        version: host.version.clone(),
        capabilities: Some(host.capabilities.clone()),
        account_signed_in: host.signed_in,
        platform: host.platform as i32,
    }
}

/// The most agent kinds a host's handshake may say it runs.
pub(crate) const MAX_KINDS: usize = 64;
/// The longest version a host's handshake may name.
pub(crate) const MAX_VERSION_BYTES: usize = 64;

/// A host another one described: in a handshake, of itself or a neighbor,
/// or announcing a neighbor. What it says is kept for the life of the link
/// (on a relay, copied to every other link of the account) and shown
/// wherever hosts are listed, so it is held to the bounds a real host
/// keeps: a non-nil id, a host name, a version, and no more kinds than
/// there are.
pub(crate) fn host_from_wire(host: wire::Host) -> Result<Host, String> {
    let platform = host.platform();
    let id = uuid_from_bytes("host_id", host.host_id)?;
    if id.is_nil() {
        return Err("host_id must not be nil".to_string());
    }
    let name = wire::HostName::parse(&host.name)
        .map_err(|error| format!("host name: {error}"))?
        .into_string();
    if host.version.is_empty() || host.version.len() > MAX_VERSION_BYTES {
        return Err(format!(
            "host version must be 1 to {MAX_VERSION_BYTES} bytes"
        ));
    }
    let capabilities = host.capabilities.unwrap_or_default();
    if capabilities.kinds.len() > MAX_KINDS {
        return Err(format!("a host runs at most {MAX_KINDS} agent kinds"));
    }
    Ok(Host {
        id,
        name,
        version: host.version,
        capabilities,
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

    fn described(name: &str) -> wire::Host {
        wire::Host {
            host_id: HostId::from_u128(7).as_bytes().to_vec(),
            name: name.to_owned(),
            version: "0.9.0".to_owned(),
            capabilities: Some(Capabilities {
                cloud_relay: false,
                kinds: vec![wire::Kind::ClaudeSdk as i32],
            }),
            account_signed_in: None,
            platform: wire::Platform::Macos as i32,
        }
    }

    #[test]
    fn a_described_host_is_held_to_what_a_real_host_says() {
        assert_eq!(
            host_from_wire(described("Jordan’s MacBook Pro"))
                .unwrap()
                .name,
            "Jordan’s MacBook Pro"
        );
        let refused = [
            wire::Host {
                host_id: vec![0; 16],
                ..described("desk")
            },
            described(""),
            described(&"a".repeat(wire::MAX_HOST_NAME_CHARS + 1)),
            described("desk\u{1b}[2J"),
            described("desk\u{202e}"),
            wire::Host {
                version: String::new(),
                ..described("desk")
            },
            wire::Host {
                version: "v".repeat(MAX_VERSION_BYTES + 1),
                ..described("desk")
            },
            wire::Host {
                capabilities: Some(Capabilities {
                    cloud_relay: false,
                    kinds: vec![1; MAX_KINDS + 1],
                }),
                ..described("desk")
            },
        ];
        for host in refused {
            assert!(host_from_wire(host.clone()).is_err(), "{host:?}");
        }
    }

    /// The first message's bound holds a Hello at every field's limit:
    /// well over a hundred neighbors, each at the longest name (in the
    /// widest characters) and version and with every kind it may list, an
    /// address for every interface a machine might have, and a token four
    /// times a real one's size.
    #[test]
    fn a_hello_at_its_fields_bounds_fits_the_first_message_bound() {
        use prost::Message as _;

        let widest = |id: u128| wire::Host {
            host_id: HostId::from_u128(id).as_bytes().to_vec(),
            name: "\u{1d54f}".repeat(wire::MAX_HOST_NAME_CHARS),
            version: "9".repeat(MAX_VERSION_BYTES),
            capabilities: Some(Capabilities {
                cloud_relay: true,
                kinds: vec![wire::Kind::Codex as i32; MAX_KINDS],
            }),
            account_signed_in: Some(true),
            platform: wire::Platform::Macos as i32,
        };
        host_from_wire(widest(1)).expect("a host at every bound is a host");
        let address = "[ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff]:65535".to_owned();
        let hello = wire::Message {
            body: Some(wire::message::Body::Hello(wire::Hello {
                host: Some(widest(1)),
                neighbors: (2..130).map(widest).collect(),
                auth_token: Some("t".repeat(4 * 1024)),
                runtime_instance_id: vec![0xff; 16],
                addrs: vec![address.clone(); 64],
                public_addr: Some(address.clone()),
                observed_addr: Some(address),
            })),
        };

        let bytes = hello.encoded_len();

        assert!(
            bytes <= wire::HELLO_SIZE_LIMIT,
            "{bytes} bytes; the bound is {}",
            wire::HELLO_SIZE_LIMIT
        );
    }
}
