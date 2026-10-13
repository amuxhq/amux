use std::collections::HashMap;
use std::io;
use std::net::{IpAddr, SocketAddr, SocketAddrV4, SocketAddrV6};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use mdns_sd::{ResolvedService, ScopedIp, ServiceDaemon, ServiceEvent, ServiceInfo};
use tokio::sync::broadcast;

use super::{
    Advertisement, Discovery, DiscoveryError, DiscoveryEvent, EVENT_CAPACITY, MonotonicClock,
    SERVICE_TYPE, SystemClock, WakeMonitor,
};
use crate::HostId;

const WAKE_TICK: Duration = Duration::from_secs(5);

/// Desktop DNS-SD discovery backed by the platform's multicast interfaces.
pub struct MdnsDiscovery {
    inner: Arc<Inner>,
    advertised_fullname: Mutex<Option<String>>,
}

struct Inner {
    daemon: ServiceDaemon,
    events: broadcast::Sender<DiscoveryEvent>,
    resolved_hosts: Mutex<HashMap<String, HostId>>,
    browse_generation: AtomicU64,
}

impl MdnsDiscovery {
    pub fn new() -> Result<Self, DiscoveryError> {
        Self::new_with_clock(Arc::new(SystemClock))
    }

    fn new_with_clock(clock: Arc<dyn MonotonicClock>) -> Result<Self, DiscoveryError> {
        let daemon = ServiceDaemon::new()
            .map_err(|error| DiscoveryError::Bind(io::Error::other(error.to_string())))?;
        let receiver = daemon
            .browse(SERVICE_TYPE)
            .map_err(|error| DiscoveryError::Unavailable(error.to_string()))?;
        let (events, _) = broadcast::channel(EVENT_CAPACITY);
        let inner = Arc::new(Inner {
            daemon,
            events,
            resolved_hosts: Mutex::new(HashMap::new()),
            browse_generation: AtomicU64::new(0),
        });

        spawn_browser(Arc::downgrade(&inner), receiver, 0);
        spawn_wake_monitor(Arc::downgrade(&inner), clock);

        Ok(Self {
            inner,
            advertised_fullname: Mutex::new(None),
        })
    }
}

impl Discovery for MdnsDiscovery {
    fn advertise(&self, advert: Advertisement) -> Result<(), DiscoveryError> {
        let service = service_info(&advert)?;
        let fullname = service.get_fullname().to_string();

        let mut current = self.advertised_fullname.lock().unwrap();
        if let Some(previous) = current.as_ref().filter(|previous| **previous != fullname) {
            self.inner
                .daemon
                .unregister(previous)
                .map_err(|error| DiscoveryError::Register(error.to_string()))?;
            *current = None;
        }
        self.inner
            .daemon
            .register(service)
            .map_err(|error| DiscoveryError::Register(error.to_string()))?;
        *current = Some(fullname);
        Ok(())
    }

    fn withdraw(&self) {
        let Some(fullname) = self.advertised_fullname.lock().unwrap().take() else {
            return;
        };
        if let Err(error) = self.inner.daemon.unregister(&fullname) {
            tracing::warn!(%error, %fullname, "failed to withdraw mDNS service");
        }
    }

    fn browse(&self) -> broadcast::Receiver<DiscoveryEvent> {
        self.inner.events.subscribe()
    }

    fn requery(&self) {
        self.inner.requery();
    }
}

impl Drop for MdnsDiscovery {
    fn drop(&mut self) {
        self.withdraw();
    }
}

impl Inner {
    fn requery(self: &Arc<Self>) {
        match self.daemon.browse(SERVICE_TYPE) {
            Ok(receiver) => {
                let generation = self.browse_generation.fetch_add(1, Ordering::AcqRel) + 1;
                spawn_browser(Arc::downgrade(self), receiver, generation);
            }
            Err(error) => tracing::warn!(%error, "failed to requery mDNS services"),
        }
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        let _ = self.daemon.stop_browse(SERVICE_TYPE);
        let _ = self.daemon.shutdown();
    }
}

fn spawn_browser(inner: Weak<Inner>, receiver: mdns_sd::Receiver<ServiceEvent>, generation: u64) {
    std::thread::Builder::new()
        .name("amux-mdns-browser".to_string())
        .spawn(move || {
            while let Ok(event) = receiver.recv() {
                let Some(inner) = inner.upgrade() else {
                    break;
                };
                if inner.browse_generation.load(Ordering::Acquire) != generation {
                    break;
                }
                inner.handle_event(event);
            }
        })
        .expect("failed to spawn mDNS browser thread");
}

impl Inner {
    fn handle_event(&self, event: ServiceEvent) {
        match event {
            ServiceEvent::ServiceResolved(service) => {
                let fullname = service.get_fullname().to_ascii_lowercase();
                match advertisement_from_service(&service) {
                    Ok(advert) => {
                        self.resolved_hosts
                            .lock()
                            .unwrap()
                            .insert(fullname, advert.host_id);
                        let _ = self.events.send(DiscoveryEvent::Found(advert));
                    }
                    Err(error) => {
                        tracing::debug!(%error, "ignoring malformed amux mDNS service");
                    }
                }
            }
            ServiceEvent::ServiceRemoved(_, fullname) => {
                if let Some(host_id) = self
                    .resolved_hosts
                    .lock()
                    .unwrap()
                    .remove(&fullname.to_ascii_lowercase())
                {
                    let _ = self.events.send(DiscoveryEvent::Lost { host_id });
                }
            }
            _ => {}
        }
    }
}

fn spawn_wake_monitor(inner: Weak<Inner>, clock: Arc<dyn MonotonicClock>) {
    std::thread::Builder::new()
        .name("amux-mdns-wake".to_string())
        .spawn(move || {
            let monitor = WakeMonitor::new(clock);
            loop {
                std::thread::sleep(WAKE_TICK);
                let Some(inner) = inner.upgrade() else {
                    break;
                };
                monitor.tick(|| inner.requery());
            }
        })
        .expect("failed to spawn mDNS wake monitor thread");
}

fn service_info(advert: &Advertisement) -> Result<ServiceInfo, DiscoveryError> {
    let Some(port) = advert.addrs.first().map(SocketAddr::port) else {
        return Err(DiscoveryError::Register(
            "an advertisement needs at least one listener address".to_string(),
        ));
    };
    if advert.addrs.iter().any(|addr| addr.port() != port) {
        return Err(DiscoveryError::Register(
            "all advertised addresses must use the listener's SRV port".to_string(),
        ));
    }

    let ips = advert
        .addrs
        .iter()
        .map(SocketAddr::ip)
        .filter(|ip| !ip.is_unspecified())
        .collect::<Vec<IpAddr>>();
    let properties = super::txt_properties(advert);
    let hostname = format!("amux-{}.local.", advert.host_id.simple());
    let service = ServiceInfo::new(
        SERVICE_TYPE,
        &advert.name,
        &hostname,
        ips.as_slice(),
        port,
        &properties[..],
    )
    .map_err(|error| DiscoveryError::Register(error.to_string()))?;

    Ok(if ips.is_empty() {
        service.enable_addr_auto()
    } else {
        service
    })
}

fn advertisement_from_service(service: &ResolvedService) -> Result<Advertisement, String> {
    let version = service
        .get_property_val_str("v")
        .ok_or_else(|| "missing TXT property v".to_string())?
        .parse::<u32>()
        .map_err(|_| "TXT property v is not a protocol version".to_string())?;
    let host_id = service
        .get_property_val_str("hid")
        .ok_or_else(|| "missing TXT property hid".to_string())?
        .parse::<HostId>()
        .map_err(|_| "TXT property hid is not a host id".to_string())?;
    let instance = instance_name(service.get_fullname())
        .ok_or_else(|| "service fullname is outside the amux service type".to_string())?;
    // The record's own name, because conflict resolution may have renamed
    // the instance ("desk (2)"); the instance name only for a record that
    // carries none.
    let name = service
        .get_property_val_str("name")
        .filter(|name| !name.is_empty())
        .map_or(instance, str::to_owned);
    // A host that names no scope is in the empty one.
    let scope = service
        .get_property_val_str("scope")
        .unwrap_or_default()
        .to_owned();
    let port = service.get_port();
    let mut addrs = service
        .get_addresses()
        .iter()
        .filter_map(|addr| match addr {
            ScopedIp::V4(addr) => Some(SocketAddr::V4(SocketAddrV4::new(*addr.addr(), port))),
            ScopedIp::V6(addr) => Some(SocketAddr::V6(SocketAddrV6::new(
                *addr.addr(),
                port,
                0,
                addr.scope_id().index,
            ))),
            _ => None,
        })
        .collect::<Vec<_>>();
    addrs.sort();
    addrs.dedup();

    Ok(Advertisement {
        host_id,
        name,
        version,
        addrs,
        scope,
    })
}

fn instance_name(fullname: &str) -> Option<String> {
    let escaped = fullname.strip_suffix(SERVICE_TYPE)?.strip_suffix('.')?;
    let mut decoded = String::with_capacity(escaped.len());
    let mut chars = escaped.chars().peekable();
    while let Some(character) = chars.next() {
        if character != '\\' {
            decoded.push(character);
            continue;
        }

        let mut digits = String::new();
        while digits.len() < 3 && chars.peek().is_some_and(char::is_ascii_digit) {
            digits.push(chars.next().unwrap());
        }
        if digits.len() == 3 {
            let byte = digits.parse::<u8>().ok()?;
            decoded.push(char::from(byte));
        } else {
            let escaped_character = chars.next()?;
            decoded.push(escaped_character);
        }
    }
    Some(decoded)
}

#[cfg(test)]
pub(super) fn service_info_for_test(advert: &Advertisement) -> Result<ServiceInfo, DiscoveryError> {
    service_info(advert)
}

#[cfg(test)]
pub(super) fn advertisement_from_service_for_test(
    service: &ResolvedService,
) -> Result<Advertisement, String> {
    advertisement_from_service(service)
}
