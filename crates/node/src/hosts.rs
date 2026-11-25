//! The shell's copy of the host set the profile's core publishes on the
//! inventory (see `daemon_core::host_set`), for the shell's own reads: a
//! host's name, capabilities and whether its rows are current. The core is
//! told this host's own entry as the shell describes it, whenever what it
//! is made of moves: the edge starting, signing in or out, a provider's
//! offer read again, the settings changing its name, its Claude kind or
//! its agents.

use wire::HostEntry;

use crate::runtime::ProfileRuntime;

impl ProfileRuntime {
    /// This host's entry as the core lists it: always trusted and online
    /// to itself, and signed in or out of its account as it is now, so a
    /// client can say a host only the relay reaches is offline because this
    /// machine signed out.
    pub fn host_entry(&self) -> HostEntry {
        let edge = self.edge();
        HostEntry {
            host: Some(wire::Host {
                host_id: self.host().as_bytes().to_vec(),
                name: edge
                    .as_ref()
                    .map(|edge| edge.host_name())
                    .unwrap_or_default(),
                version: crate::version().to_owned(),
                capabilities: edge.as_ref().map(|edge| edge.capabilities()),
                platform: crate::routing::local_platform() as i32,
                account_signed_in: edge.as_ref().and_then(|edge| edge.cloud().signed_in()),
            }),
            generation: self.generation(),
            providers: self.offers().on_host(&self.launch()),
            standing: Some(wire::host_entry::Standing::Trusted(wire::Trusted {
                reach: Some(wire::trusted::Reach::Here(wire::Here {})),
                current: true,
            })),
            folders: self.folders(),
            push_key: self
                .push_key
                .lock()
                .unwrap()
                .clone()
                .map(|public_key| wire::PushKey {
                    suite: wire::PushSuite::HpkeX25519Chacha20 as i32,
                    public_key,
                }),
            experimental: Default::default(),
        }
    }

    /// Says on this host's own entry whether it wants notifications, and the
    /// key they are sealed to. The owner of the private half says so at every
    /// start, so nothing here is kept.
    pub fn want_notifications(&self, public_key: Option<Vec<u8>>) {
        *self.push_key.lock().unwrap() = public_key;
        self.tell_own_host();
    }

    /// Tells the core this host's own entry as it stands now.
    pub(crate) fn tell_own_host(&self) {
        self.driver()
            .report(daemon_core::Input::OwnHost(Box::new(self.host_entry())));
    }

    /// `host`'s entry as last published.
    pub fn host_entry_of(&self, host: crate::HostId) -> Option<HostEntry> {
        self.hosts
            .borrow()
            .iter()
            .find(|entry| entry.host_id() == host.as_bytes())
            .cloned()
    }

    /// The host set as last published: what an inventory opening lists.
    pub(crate) fn published_hosts(&self) -> Vec<HostEntry> {
        self.hosts.borrow().clone()
    }

    /// Rings whenever the core publishes the host set anew.
    pub(crate) fn subscribe_hosts(&self) -> tokio::sync::watch::Receiver<Vec<HostEntry>> {
        self.hosts.subscribe()
    }

    /// Whether the profile trusts a phone: a host whose own entry says it
    /// runs on iOS or Android.
    pub(crate) fn trusts_a_phone(&self) -> bool {
        hosts_trust_a_phone(&self.hosts.borrow(), self.host())
    }

    /// The core published the host set anew.
    pub(crate) fn hosts_published(&self, entries: Vec<HostEntry>) {
        self.hosts.send_replace(entries);
    }
}

fn hosts_trust_a_phone(entries: &[HostEntry], self_host: crate::HostId) -> bool {
    entries.iter().any(|entry| {
        entry.trusted().is_some()
            && entry.platform().is_phone()
            && entry.host_id() != self_host.as_bytes()
    })
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;

    fn entry(host_id: Uuid, platform: wire::Platform, trusted: bool) -> HostEntry {
        HostEntry {
            host: Some(wire::Host {
                host_id: host_id.as_bytes().to_vec(),
                name: "peer".to_owned(),
                platform: platform as i32,
                ..wire::Host::default()
            }),
            standing: Some(if trusted {
                wire::host_entry::Standing::Trusted(wire::Trusted::default())
            } else {
                wire::host_entry::Standing::Candidate(wire::Candidate::default())
            }),
            ..HostEntry::default()
        }
    }

    #[test]
    fn trusts_a_phone_recognizes_trusted_ios_and_android_peers() {
        let desktop = Uuid::from_u128(1);
        let phone = Uuid::from_u128(2);

        assert!(!hosts_trust_a_phone(
            &[entry(phone, wire::Platform::Macos, true)],
            desktop
        ));
        assert!(!hosts_trust_a_phone(
            &[entry(phone, wire::Platform::Android, false)],
            desktop
        ));
        assert!(!hosts_trust_a_phone(
            &[entry(desktop, wire::Platform::Android, true)],
            desktop
        ));
        assert!(hosts_trust_a_phone(
            &[entry(phone, wire::Platform::Ios, true)],
            desktop
        ));
        assert!(hosts_trust_a_phone(
            &[entry(phone, wire::Platform::Android, true)],
            desktop
        ));
    }
}
