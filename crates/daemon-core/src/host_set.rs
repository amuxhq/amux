//! The one host set the inventory carries: this host, every host the
//! profile trusts with its presence and the generation this store last
//! recorded for it, and the machines discovery has found in this profile's
//! scope that nobody has paired, as candidates.
//!
//! A trusted host is in every snapshot whatever its presence; only
//! untrusting it removes it, so no reconnect can make a client forget an
//! agent because its host went quiet. A trusted host that said it no
//! longer trusts this one reads offline, whatever route still reaches it:
//! the relay keeps saying it is online, which pairing again needs and a
//! client must not read as a working host. Candidates come and go with
//! discovery.
//!
//! A step that moves anything an entry is made of recomputes the set as it
//! ends and tells every inventory stream what changed, so an opening and
//! the deltas after it describe one sequence.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use uuid::Uuid;
use wire::host_entry::Standing;
use wire::{HostEntry, HostRemoved, RouteKind, inventory_event, trusted};

use crate::catalogue::Said;
use crate::ids::HostId;
use crate::io::{Output, ShellWork};
use crate::payload::CarrierKind;
use crate::presence::{Presence, Route};
use crate::serve::inventory;
use crate::{Daemon, codec};

/// A machine discovery found in this profile's scope.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Candidate {
    pub host: HostId,
    pub name: String,
    pub addrs: Vec<String>,
}

/// What the providers on a trusted host offer, as its own entry said.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct Providers(#[serde(with = "codec::messages")] Vec<wire::ProviderOnHost>);

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct HostSet {
    /// This host's entry as the driver describes it.
    #[serde(with = "codec::optional")]
    own: Option<HostEntry>,
    candidates: Vec<Candidate>,
    /// Why the last dial to each host failed, until a route to it comes up.
    dial_errors: BTreeMap<HostId, String>,
    providers: BTreeMap<HostId, Providers>,
    /// The set as last published, by host id.
    #[serde(with = "codec::messages")]
    published: Vec<HostEntry>,
}

impl HostSet {
    /// The set as last published: what an inventory opening lists.
    pub fn published(&self) -> &[HostEntry] {
        &self.published
    }
}

impl Daemon {
    /// The host set as last published.
    pub fn host_set(&self) -> &[HostEntry] {
        self.host_set.published()
    }

    pub(crate) fn own_host_said(&mut self, entry: HostEntry) {
        self.host_set.own = Some(entry);
        self.touched.host_set = true;
    }

    pub(crate) fn candidates_found(&mut self, found: Vec<Candidate>) {
        self.host_set.candidates = found;
        self.touched.host_set = true;
    }

    pub(crate) fn dial_error(&mut self, host: HostId, error: Option<String>) {
        let before = match &error {
            Some(error) => self.host_set.dial_errors.insert(host, error.clone()),
            None => self.host_set.dial_errors.remove(&host),
        };
        if before != error {
            self.touched.host_set = true;
        }
    }

    /// Why the last dial to `host` failed, until a route to it comes up.
    pub fn last_dial_error(&self, host: HostId) -> Option<&str> {
        self.host_set.dial_errors.get(&host).map(String::as_str)
    }

    /// What a trusted host's own entry said its providers offer.
    pub(crate) fn providers_said(&mut self, host: HostId, providers: Vec<wire::ProviderOnHost>) {
        let providers = Providers(providers);
        if self.host_set.providers.get(&host) != Some(&providers) {
            self.host_set.providers.insert(host, providers);
            self.touched.host_set = true;
        }
    }

    /// Recomputes the set, when the step moved anything it is made of, and
    /// tells every inventory stream what changed.
    pub(crate) fn publish_host_set(&mut self, out: &mut Vec<Output>) {
        if !std::mem::take(&mut self.touched.host_set) {
            return;
        }
        let fresh = self.compute_host_set();
        let held: BTreeMap<&[u8], &HostEntry> = self
            .host_set
            .published
            .iter()
            .map(|entry| (entry.host_id(), entry))
            .collect();
        let mut changed = false;
        for entry in fresh.values() {
            if held.get(entry.host_id()) != Some(&entry) {
                changed = true;
                self.publish_inventory(
                    Arc::new(inventory(inventory_event::Of::Host(entry.clone()))),
                    out,
                );
            }
        }
        for host in held.keys().filter(|host| !fresh.contains_key(**host)) {
            changed = true;
            self.publish_inventory(
                Arc::new(inventory(inventory_event::Of::HostRemoved(HostRemoved {
                    host_id: host.to_vec(),
                }))),
                out,
            );
        }
        if changed {
            self.host_set.published = fresh.into_values().collect();
            out.push(Output::Shell(ShellWork::Hosts {
                entries: self.host_set.published.clone(),
            }));
        }
    }

    fn compute_host_set(&self) -> BTreeMap<Vec<u8>, HostEntry> {
        let own_host = self.store.own_host().to_vec();
        let mut fresh = BTreeMap::new();
        for (uuid, trusted) in self.trust.store().entries() {
            let host = HostId(*uuid.as_bytes());
            if host.0.as_slice() == own_host {
                continue;
            }
            let said = self.routing.catalogue.said(host);
            fresh.insert(
                host.0.to_vec(),
                HostEntry {
                    host: Some(wire::Host {
                        host_id: host.0.to_vec(),
                        name: trusted.name.clone(),
                        version: said.map(|said| said.version.clone()).unwrap_or_default(),
                        capabilities: said.map(Said::capabilities),
                        platform: trusted.platform as i32,
                        account_signed_in: trusted.signed_in,
                    }),
                    generation: self
                        .store
                        .host_generation(&host.0)
                        .ok()
                        .flatten()
                        .unwrap_or_default(),
                    providers: self
                        .host_set
                        .providers
                        .get(&host)
                        .map(|providers| providers.0.clone())
                        .unwrap_or_default(),
                    standing: Some(Standing::Trusted(wire::Trusted {
                        reach: Some(self.reads(host)),
                        current: self.sources.ready(host),
                    })),
                },
            );
        }
        for candidate in &self.host_set.candidates {
            let id = candidate.host.0.to_vec();
            if id == own_host
                || fresh.contains_key(&id)
                || self
                    .trust
                    .store()
                    .entry(Uuid::from_bytes(candidate.host.0))
                    .is_some()
            {
                continue;
            }
            fresh.insert(
                id.clone(),
                HostEntry {
                    host: Some(wire::Host {
                        host_id: id,
                        name: candidate.name.clone(),
                        ..wire::Host::default()
                    }),
                    standing: Some(Standing::Candidate(wire::Candidate {
                        addrs: candidate.addrs.clone(),
                    })),
                    ..HostEntry::default()
                },
            );
        }
        // This host is always trusted, online and current to itself.
        let mut own = self.host_set.own.clone().unwrap_or_default();
        own.host.get_or_insert_with(wire::Host::default).host_id = own_host.clone();
        own.generation = self.options.generation;
        own.standing = Some(Standing::Trusted(wire::Trusted {
            reach: Some(trusted::Reach::Online(RouteKind::Unspecified as i32)),
            current: true,
        }));
        fresh.insert(own_host, own);
        fresh
    }

    /// The kind of route calls to `host` take now, however it reads:
    /// unspecified while none lists it.
    pub fn route_kind(&self, host: HostId) -> RouteKind {
        match self.presence.view(host).route() {
            Some(Route::Via(_)) => RouteKind::Relay,
            Some(Route::Direct(link)) => match self.routing.links.link(link).map(|up| up.kind) {
                Some(CarrierKind::Direct) => RouteKind::Direct,
                Some(CarrierKind::Ssh) => RouteKind::Ssh,
                Some(CarrierKind::Relay | CarrierKind::Forwarded) => RouteKind::Relay,
                None => RouteKind::Unspecified,
            },
            None => RouteKind::Unspecified,
        }
    }

    /// How `host` reads, as every client is told: online over the kind of
    /// route its calls take while a link of its own is up, away while the
    /// relay refuses to forward to it for the account's plan, and offline
    /// otherwise, with why the last dial failed.
    /// A host that said it no longer trusts this one reads offline as
    /// revoked, whatever route still reaches it.
    pub fn reads(&self, host: HostId) -> trusted::Reach {
        let revoked = self.routing.catalogue.revoked(host);
        let via = self.route_kind(host);
        match self.presence.view(host).presence {
            Presence::Online if !revoked && via != RouteKind::Unspecified => {
                trusted::Reach::Online(via as i32)
            }
            Presence::Away if !revoked && via != RouteKind::Unspecified => {
                trusted::Reach::Away(wire::Empty {})
            }
            _ => trusted::Reach::Offline(wire::Offline {
                last_dial_error: self.host_set.dial_errors.get(&host).cloned(),
                revoked,
            }),
        }
    }
}
