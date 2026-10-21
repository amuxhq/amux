//! Local-network host discovery.
//!
//! Advertisements are dial hints only. Trust is established separately by the
//! pinned handshake when a caller pairs with or reconnects to a found host.

use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::sync::RwLock;
use std::time::{Duration, Instant};

use tokio::sync::broadcast;

use crate::HostId;

#[cfg(not(target_os = "ios"))]
mod mdns;
#[cfg(target_os = "ios")]
mod mdns_stub;
mod scripted;

#[cfg(not(target_os = "ios"))]
pub use mdns::MdnsDiscovery;
#[cfg(target_os = "ios")]
pub use mdns_stub::MdnsDiscovery;
pub use scripted::ScriptedDiscovery;

/// The DNS-SD service type advertised by an amux LAN listener.
pub const SERVICE_TYPE: &str = "_amux._udp.local.";

/// The TXT properties an advertisement carries. Everything that publishes one
/// writes these, so a browser reads the same record whoever put it up.
pub fn txt_properties(advert: &Advertisement) -> [(&'static str, String); 4] {
    [
        ("v", advert.version.to_string()),
        // Hyphenated: the iPhone app parses this with Foundation's
        // `UUID(uuidString:)`, which rejects the 32-digit simple form.
        ("hid", advert.host_id.hyphenated().to_string()),
        // The host's own name, apart from the instance name: a responder
        // that meets another instance of the same name renames this one
        // "desk (2)", and a browser reading that would offer one machine as
        // if it were the second of two.
        ("name", advert.name.clone()),
        ("scope", advert.scope.clone()),
    ]
}

/// A monotonic pause longer than this is treated as a system wake.
pub const WAKE_GAP: Duration = Duration::from_secs(30);

pub(crate) const EVENT_CAPACITY: usize = 128;

/// The addresses of both families a listener on every interface answers
/// at. IPv6 link-local addresses are left out: one names a peer only with
/// its interface, which differs on the dialling machine.
#[cfg(not(target_os = "ios"))]
pub(crate) fn local_pairing_addrs(port: u16) -> Vec<SocketAddr> {
    let mut addrs = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .map(|interface| SocketAddr::new(interface.ip(), port))
        .filter(|addr| !addr.ip().is_unspecified() && !is_ipv6_link_local(addr.ip()))
        .collect::<Vec<_>>();
    addrs.sort_unstable();
    addrs.dedup();
    addrs
}

fn is_ipv6_link_local(ip: std::net::IpAddr) -> bool {
    matches!(ip, std::net::IpAddr::V6(ip) if ip.segments()[0] & 0xffc0 == 0xfe80)
}

#[cfg(target_os = "ios")]
pub(crate) fn local_pairing_addrs(_port: u16) -> Vec<SocketAddr> {
    Vec::new()
}

/// The untrusted connection hints published by one host.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Advertisement {
    pub host_id: HostId,
    pub name: String,
    pub version: u32,
    pub addrs: Vec<SocketAddr>,
    /// The advertiser's discovery scope. A browser lists only hosts whose
    /// scope equals its own, matched exactly: real machines leave it empty,
    /// and a worktree or a test network sets its own so the two never meet.
    pub scope: String,
}

/// An update from a standing discovery subscription.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DiscoveryEvent {
    /// A resolved service. Known hosts are emitted again when their addresses
    /// change or a query resolves them again.
    Found(Advertisement),
    /// A goodbye or TTL expiry for a previously resolved service.
    Lost { host_id: HostId },
}

/// A source of untrusted local-network connection hints.
pub trait Discovery: Send + Sync + 'static {
    /// Registers the service while the listener is up. A later call replaces
    /// this discovery instance's previous record.
    fn advertise(&self, advert: Advertisement) -> Result<(), DiscoveryError>;

    /// Sends a goodbye for this discovery instance's record. Idempotent.
    fn withdraw(&self);

    /// Subscribes to discovery updates until the returned receiver is dropped.
    fn browse(&self) -> broadcast::Receiver<DiscoveryEvent>;

    /// Re-issues the discovery query immediately.
    fn requery(&self);

    /// Replaces what this browser has found with the set an outside browser
    /// resolved, announcing what is new and saying goodbye to what is gone.
    ///
    /// A platform whose browsing lives outside this process — an iPhone,
    /// where only the system may browse the local network — hands its whole
    /// resolved set over each time it changes, because a browser that reports
    /// a set has no separate word for a machine that left. A discovery that
    /// browses for itself ignores it.
    fn hand_over(&self, _found: Vec<Advertisement>) {}
}

#[derive(Debug, thiserror::Error)]
pub enum DiscoveryError {
    #[error("failed to start local discovery: {0}")]
    Bind(#[source] io::Error),
    #[error("failed to register local discovery service: {0}")]
    Register(String),
    #[error("local discovery is unavailable: {0}")]
    Unavailable(String),
}

#[derive(Clone)]
struct FoundHost {
    advert: Advertisement,
    last_seen: Instant,
}

/// The latest resolved advertisement for each host.
#[derive(Default)]
pub struct FoundHosts {
    hosts: RwLock<HashMap<HostId, FoundHost>>,
}

impl FoundHosts {
    /// Applies one update from a discovery subscription.
    pub fn apply(&self, event: DiscoveryEvent) {
        match event {
            DiscoveryEvent::Found(advert) => self.record(advert, Instant::now()),
            DiscoveryEvent::Lost { host_id } => {
                self.hosts.write().unwrap().remove(&host_id);
            }
        }
    }

    /// Records a resolved advertisement.
    pub fn found(&self, advert: Advertisement) {
        self.record(advert, Instant::now());
    }

    /// Removes a host after a goodbye or TTL expiry.
    pub fn lost(&self, host_id: HostId) {
        self.hosts.write().unwrap().remove(&host_id);
    }

    /// Replaces the whole set with what one browse resolved.
    ///
    /// A browser that reports a set has no separate word for a machine that
    /// left, so what is missing from the set is what is gone.
    pub fn hand_over(&self, found: Vec<Advertisement>) {
        let now = Instant::now();
        let mut hosts = self.hosts.write().unwrap();
        hosts.retain(|host_id, _| found.iter().any(|advert| advert.host_id == *host_id));
        for advert in found {
            hosts.insert(
                advert.host_id,
                FoundHost {
                    advert,
                    last_seen: now,
                },
            );
        }
    }

    /// Returns found hosts with the most recently resolved first.
    pub fn candidates(&self) -> Vec<Advertisement> {
        let mut hosts = self
            .hosts
            .read()
            .unwrap()
            .values()
            .cloned()
            .collect::<Vec<_>>();
        hosts.sort_by(|left, right| {
            right
                .last_seen
                .cmp(&left.last_seen)
                .then_with(|| left.advert.name.cmp(&right.advert.name))
                .then_with(|| left.advert.host_id.cmp(&right.advert.host_id))
        });
        hosts.into_iter().map(|host| host.advert).collect()
    }

    /// Returns the latest resolved socket addresses for `host_id`.
    pub fn addrs_for(&self, host_id: HostId) -> Vec<SocketAddr> {
        self.hosts
            .read()
            .unwrap()
            .get(&host_id)
            .map(|host| host.advert.addrs.clone())
            .unwrap_or_default()
    }

    fn record(&self, advert: Advertisement, last_seen: Instant) {
        self.hosts
            .write()
            .unwrap()
            .insert(advert.host_id, FoundHost { advert, last_seen });
    }
}

/// The browser's clock and its sleep detector, desktop only: the phone's
/// browser is the stub, and the app hears about sleep from the system.
#[cfg(not(target_os = "ios"))]
mod wake {
    use std::sync::{Arc, Mutex};
    use std::time::Instant;

    use super::WAKE_GAP;

    pub(crate) trait MonotonicClock: Send + Sync + 'static {
        fn now(&self) -> Instant;
    }

    pub(crate) struct SystemClock;

    impl MonotonicClock for SystemClock {
        fn now(&self) -> Instant {
            Instant::now()
        }
    }

    /// Detects sleep/wake without depending on platform lifecycle notifications.
    pub(crate) struct WakeMonitor {
        clock: Arc<dyn MonotonicClock>,
        last_tick: Mutex<Instant>,
    }

    impl WakeMonitor {
        pub(crate) fn new(clock: Arc<dyn MonotonicClock>) -> Self {
            let last_tick = clock.now();
            Self {
                clock,
                last_tick: Mutex::new(last_tick),
            }
        }

        pub(crate) fn tick(&self, requery: impl FnOnce()) {
            let now = self.clock.now();
            let mut last_tick = self.last_tick.lock().unwrap();
            if now.saturating_duration_since(*last_tick) > WAKE_GAP {
                requery();
            }
            *last_tick = now;
        }
    }
}

#[cfg(not(target_os = "ios"))]
pub(crate) use wake::{MonotonicClock, SystemClock, WakeMonitor};

#[cfg(test)]
mod tests;
