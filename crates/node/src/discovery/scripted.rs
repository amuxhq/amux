use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::broadcast;

use super::{Advertisement, Discovery, DiscoveryError, DiscoveryEvent, EVENT_CAPACITY};
use crate::HostId;

/// A deterministic discovery bus for tests and platform adapters.
pub struct ScriptedDiscovery {
    bus: Arc<ScriptedBus>,
    advertised: Mutex<Option<HostId>>,
}

struct ScriptedBus {
    events: broadcast::Sender<DiscoveryEvent>,
    active: Mutex<HashMap<HostId, Advertisement>>,
}

impl ScriptedDiscovery {
    pub fn new() -> Self {
        Self::default()
    }

    /// Emits a resolved service to every current browser.
    pub fn announce(&self, advert: Advertisement) {
        self.bus
            .active
            .lock()
            .unwrap()
            .insert(advert.host_id, advert.clone());
        let _ = self.bus.events.send(DiscoveryEvent::Found(advert));
    }

    /// Emits a goodbye or expiry to every current browser.
    pub fn withdraw_host(&self, host_id: HostId) {
        self.bus.active.lock().unwrap().remove(&host_id);
        let _ = self.bus.events.send(DiscoveryEvent::Lost { host_id });
    }
}

impl Default for ScriptedDiscovery {
    fn default() -> Self {
        let (events, _) = broadcast::channel(EVENT_CAPACITY);
        Self {
            bus: Arc::new(ScriptedBus {
                events,
                active: Mutex::new(HashMap::new()),
            }),
            advertised: Mutex::new(None),
        }
    }
}

impl Clone for ScriptedDiscovery {
    fn clone(&self) -> Self {
        Self {
            bus: self.bus.clone(),
            advertised: Mutex::new(None),
        }
    }
}

impl Discovery for ScriptedDiscovery {
    fn advertise(&self, advert: Advertisement) -> Result<(), DiscoveryError> {
        let previous = self.advertised.lock().unwrap().replace(advert.host_id);
        if let Some(host_id) = previous.filter(|host_id| *host_id != advert.host_id) {
            self.withdraw_host(host_id);
        }
        self.announce(advert);
        Ok(())
    }

    fn withdraw(&self) {
        if let Some(host_id) = self.advertised.lock().unwrap().take() {
            self.withdraw_host(host_id);
        }
    }

    fn browse(&self) -> broadcast::Receiver<DiscoveryEvent> {
        self.bus.events.subscribe()
    }

    fn requery(&self) {
        let active = self
            .bus
            .active
            .lock()
            .unwrap()
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for advert in active {
            let _ = self.bus.events.send(DiscoveryEvent::Found(advert));
        }
    }
}
