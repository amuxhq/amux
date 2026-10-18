//! The one host set the inventory carries: this host, every host the
//! profile trusts with its presence and the generation this store last
//! recorded for it, and the machines discovery has found in this profile's
//! scope that nobody has paired, as candidates.
//!
//! A trusted host is in every snapshot whatever its presence; only
//! untrusting it removes it, so no reconnect can make a client forget an
//! agent because its host went quiet. Candidates come and go with
//! discovery. The set is recomputed on the replication manager's tick and
//! published as HostEntry and HostRemoved deltas on the core thread, where
//! SubscribeInventory reads the published set, so an opening and the
//! deltas after it describe one sequence.

use std::collections::{BTreeMap, HashSet};

use wire::{HostEntry, HostRemoved, Presence, ProviderOnHost, Trust, inventory_event};

use crate::routing::LiveRoute;
use crate::runtime::ProfileRuntime;
use crate::serve::inventory;

/// Host entries by host id.
pub(crate) type HostSet = BTreeMap<Vec<u8>, HostEntry>;

impl ProfileRuntime {
    /// Tells inventory subscribers of a change to the host set.
    fn publish_hosts(&self, of: inventory_event::Of) {
        let event = std::sync::Arc::new(inventory(of));
        self.hand_over(daemon_core::FromShell::Hosts(event));
    }

    /// This host's entry: always trusted and online to itself, and signed
    /// in or out of its account as it is now, so a client can say a host
    /// only the relay reaches is offline because this machine signed out. The
    /// replication manager recomputes it every tick, so signing out or in
    /// republishes it without any link changing.
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

    /// `host`'s entry as last published.
    pub fn host_entry_of(&self, host: crate::HostId) -> Option<HostEntry> {
        self.published_hosts()
            .into_iter()
            .find(|entry| entry.host_id == host.as_bytes())
    }

    /// The host set as last published: what an inventory opening lists.
    pub(crate) fn published_hosts(&self) -> Vec<HostEntry> {
        let mut hosts = self.hosts.lock().unwrap();
        let own = self.host().as_bytes().to_vec();
        hosts.entry(own).or_insert_with(|| self.host_entry());
        hosts.values().cloned().collect()
    }

    /// For tests: puts `entry` in the published host set and publishes it,
    /// as a recomputed host set would, until the next recomputation.
    #[doc(hidden)]
    pub fn publish_host(&self, entry: HostEntry) {
        let mut published = self.hosts.lock().unwrap();
        published.insert(entry.host_id.clone(), entry.clone());
        self.publish_hosts(inventory_event::Of::Host(entry));
    }

    /// Recomputes the host set and publishes what changed.
    pub(crate) async fn sync_hosts(&self) {
        let mut fresh = HostSet::new();
        let own = self.host_entry();
        let own_host = own.host_id.clone();
        let mut trusted = HashSet::new();
        if let Some(edge) = self.edge() {
            for (host, name, _) in edge.trusted() {
                trusted.insert(host);
                // A host that said it no longer trusts this one takes no
                // call from it, whatever route still reaches it: the relay
                // keeps saying it is online, which pairing again needs and
                // a client must not read as a working host.
                let revoked = edge.revoked(host);
                let (presence, via) = if revoked {
                    (Presence::Offline, LiveRoute::Offline)
                } else {
                    edge.presence(host).await
                };
                let (version, capabilities) = edge.described(host).await.unzip();
                fresh.insert(
                    host.as_bytes().to_vec(),
                    HostEntry {
                        host_id: host.as_bytes().to_vec(),
                        name,
                        version,
                        capabilities,
                        last_dial_error: edge.last_dial_error(host).await,
                        via: via.to_wire() as i32,
                        account_signed_in: edge.signed_in(host),
                        platform: edge.platform(host),
                        revoked: revoked.then_some(true),
                        trust: Trust::Trusted as i32,
                        presence: presence as i32,
                        current: Some(self.host_ready(host)),
                        providers: self
                            .peer_providers
                            .lock()
                            .unwrap()
                            .get(host.as_bytes().as_slice())
                            .cloned()
                            .unwrap_or_default(),
                        ..HostEntry::default()
                    },
                );
            }
            for advert in edge.candidates() {
                if trusted.contains(&advert.host_id) {
                    continue;
                }
                fresh.insert(
                    advert.host_id.as_bytes().to_vec(),
                    HostEntry {
                        host_id: advert.host_id.as_bytes().to_vec(),
                        name: advert.name,
                        trust: Trust::Candidate as i32,
                        presence: Presence::Online as i32,
                        addrs: advert.addrs.iter().map(ToString::to_string).collect(),
                        ..HostEntry::default()
                    },
                );
            }
        }
        fresh.insert(own.host_id.clone(), own);

        // A trusted host's generation changes only where the store records
        // it, which updates the published entry in the same breath; the
        // store is read for hosts not published yet.
        let mut unknown = false;
        {
            let published = self.hosts.lock().unwrap();
            for (host, entry) in fresh.iter_mut() {
                if entry.trust != Trust::Trusted as i32 || *host == own_host {
                    continue;
                }
                match published.get(host) {
                    Some(held) => entry.generation = held.generation,
                    None => unknown = true,
                }
            }
            if !unknown && *published == fresh {
                return;
            }
        }
        // On the core thread, between its steps, so the generations read
        // and the deltas published are one point in its order.
        let Some(me) = self.me.upgrade() else {
            return;
        };
        let _ = self
            .read_store(move |store| me.publish_host_set(store, fresh, &own_host))
            .await;
    }

    /// Publishes `fresh` as the host set, each trusted host with the
    /// generation `store` records for it, as deltas from the one published.
    fn publish_host_set(&self, store: &store::Sqlite, mut fresh: HostSet, own_host: &[u8]) {
        for (host, entry) in fresh.iter_mut() {
            if entry.trust == Trust::Trusted as i32 && *host != own_host {
                entry.generation = store
                    .host_generation(host)
                    .ok()
                    .flatten()
                    .unwrap_or_default();
            }
        }
        let mut published = self.hosts.lock().unwrap();
        for (host, entry) in &fresh {
            if published.get(host) != Some(entry) {
                self.publish_hosts(inventory_event::Of::Host(entry.clone()));
            }
        }
        for host in published.keys() {
            if !fresh.contains_key(host) {
                self.publish_hosts(inventory_event::Of::HostRemoved(HostRemoved {
                    host_id: host.clone(),
                }));
            }
        }
        *published = fresh;
    }

    /// Keeps what a trusted host said its providers offer; true when it
    /// changed.
    pub(crate) fn peer_providers_said(&self, host: &[u8], providers: Vec<ProviderOnHost>) -> bool {
        let mut said = self.peer_providers.lock().unwrap();
        if said.get(host) == Some(&providers) {
            return false;
        }
        said.insert(host.to_vec(), providers);
        true
    }

    /// Records whether a trusted host's inventory is current on its
    /// published entry, and publishes it. Called on the core thread, by
    /// the step whose catch-up made it current or whose loss made it stale,
    /// so a client reads the change in the same sequence as the agents it
    /// wrote.
    pub(crate) fn host_current_changed(&self, host: &[u8], current: bool) {
        let mut published = self.hosts.lock().unwrap();
        if let Some(entry) = published.get_mut(host)
            && entry.current != Some(current)
        {
            entry.current = Some(current);
            self.publish_hosts(inventory_event::Of::Host(entry.clone()));
        }
    }

    /// Records a trusted host's new generation on its published entry, and
    /// publishes it. Called on the core thread, by the step that wrote the
    /// generation.
    pub(crate) fn host_generation_changed(&self, host: &[u8], generation: u64) {
        let mut published = self.hosts.lock().unwrap();
        if let Some(entry) = published.get_mut(host)
            && entry.generation != generation
        {
            entry.generation = generation;
            self.publish_hosts(inventory_event::Of::Host(entry.clone()));
        }
    }
}
