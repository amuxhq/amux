//! The shell's copy of the host set the profile's core publishes on the
//! inventory (see `daemon_core::host_set`), for the shell's own reads: a
//! host's name, capabilities and whether its rows are current. The core is
//! told this host's own entry as the shell describes it, whenever what it
//! is made of moves: the edge starting, signing in or out, a provider's
//! offer read again.

use wire::{HostEntry, Presence, Trust};

use crate::runtime::ProfileRuntime;

impl ProfileRuntime {
    /// This host's entry: always trusted and online to itself, and signed
    /// in or out of its account as it is now, so a client can say a host
    /// only the relay reaches is offline because this machine signed out.
    pub fn host_entry(&self) -> HostEntry {
        let edge = self.edge();
        HostEntry {
            host_id: self.host().as_bytes().to_vec(),
            name: edge
                .as_ref()
                .map(|edge| edge.host_name().to_owned())
                .unwrap_or_default(),
            account_signed_in: edge.as_ref().and_then(|edge| edge.account_signed_in()),
            generation: self.generation(),
            trust: Trust::Trusted as i32,
            presence: Presence::Online as i32,
            version: Some(crate::version().to_owned()),
            capabilities: edge.as_ref().map(|edge| edge.capabilities()),
            platform: Some(crate::routing::local_platform().to_owned()),
            current: Some(true),
            providers: self.offers().on_host(),
            ..HostEntry::default()
        }
    }

    /// Tells the core this host's own entry as it stands now.
    pub(crate) fn tell_own_host(&self) {
        self.driver()
            .report(daemon_core::Input::OwnHost(Box::new(self.host_entry())));
    }

    /// `host`'s entry as last published.
    pub fn host_entry_of(&self, host: crate::HostId) -> Option<HostEntry> {
        self.hosts
            .lock()
            .unwrap()
            .iter()
            .find(|entry| entry.host_id == host.as_bytes())
            .cloned()
    }

    /// The host set as last published: what an inventory opening lists.
    pub(crate) fn published_hosts(&self) -> Vec<HostEntry> {
        self.hosts.lock().unwrap().clone()
    }

    /// The core published the host set anew.
    pub(crate) fn hosts_published(&self, entries: Vec<HostEntry>) {
        *self.hosts.lock().unwrap() = entries;
    }
}
