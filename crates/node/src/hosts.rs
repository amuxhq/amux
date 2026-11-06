//! The shell's copy of the host set the profile's core publishes on the
//! inventory (see `daemon_core::host_set`), for the shell's own reads: a
//! host's name, capabilities and whether its rows are current. The core is
//! told this host's own entry as the shell describes it, whenever what it
//! is made of moves: the edge starting, signing in or out, a provider's
//! offer read again, the settings changing its name or its Claude kind.

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
            providers: self.offers().on_host(),
            standing: Some(wire::host_entry::Standing::Trusted(wire::Trusted {
                reach: Some(wire::trusted::Reach::Online(
                    wire::RouteKind::Unspecified as i32,
                )),
                current: true,
            })),
            claude_kind: self.launch_claude_kind() as i32,
            push_key: self
                .push_key
                .lock()
                .unwrap()
                .clone()
                .map(|public_key| wire::PushKey {
                    suite: wire::PushSuite::HpkeX25519Chacha20 as i32,
                    public_key,
                }),
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
    /// runs on iOS.
    pub(crate) fn trusts_a_phone(&self) -> bool {
        self.hosts.borrow().iter().any(|entry| {
            matches!(entry.standing, Some(wire::host_entry::Standing::Trusted(_)))
                && entry.host.as_ref().is_some_and(|host| {
                    host.platform == wire::Platform::Ios as i32
                        && host.host_id != self.host().as_bytes()
                })
        })
    }

    /// The core published the host set anew.
    pub(crate) fn hosts_published(&self, entries: Vec<HostEntry>) {
        self.hosts.send_replace(entries);
    }
}
