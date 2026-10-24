use tokio::sync::broadcast;

use super::{Advertisement, Discovery, DiscoveryError, DiscoveryEvent};

/// iOS uses the native `NWBrowser` adapter and feeds a `ScriptedDiscovery`.
pub struct MdnsDiscovery;

impl MdnsDiscovery {
    pub fn new() -> Result<Self, DiscoveryError> {
        Err(DiscoveryError::Unavailable(
            "iOS discovery is provided by NWBrowser".to_string(),
        ))
    }
}

/// iOS has no daemon to share; see [`MdnsDiscovery`].
#[derive(Default)]
pub struct MdnsShared;

impl MdnsShared {
    pub fn handle(&self) -> Result<MdnsDiscovery, DiscoveryError> {
        MdnsDiscovery::new()
    }
}

impl Discovery for MdnsDiscovery {
    fn advertise(&self, _advert: Advertisement) -> Result<(), DiscoveryError> {
        Err(DiscoveryError::Unavailable(
            "iOS discovery is provided by NWBrowser".to_string(),
        ))
    }

    fn withdraw(&self) {}

    fn browse(&self) -> broadcast::Receiver<DiscoveryEvent> {
        broadcast::channel(1).1
    }

    fn requery(&self) {}
}
