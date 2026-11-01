//! The local network between the net's hosts, and between them and a client
//! outside the net, made of gates the net can cut and degrade.
//!
//! Hosts on the same machine reach each other's LAN listeners over
//! loopback, where nothing can lose or delay a datagram, or cut one pair of
//! hosts while the rest talk on. So the net puts a gate in each path it can
//! steer: one per ordered pair of hosts, which a host dials in its peer's
//! place, and one in front of each host for a client outside the net (the
//! phone). A host learns the gate's address the way it learns any address:
//! from the scripted discovery bus, whose advertisements the net rewrites
//! for each browser, from the address it was given to pair with, or from a
//! Bonjour record that names the front gate. A route the net never
//! rewrote, such as a client dialling a listener's own port, bypasses the
//! gates.

use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, OnceLock};

use node::discovery::{Advertisement, Discovery, DiscoveryError, DiscoveryEvent};
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::gate::{Faults, UdpGate};
use crate::topology::link_key;

#[derive(Default)]
struct State {
    names: BTreeMap<Uuid, String>,
    fronts: BTreeMap<String, Arc<UdpGate>>,
    /// By (dialler, listener).
    pairs: BTreeMap<(String, String), Arc<UdpGate>>,
    faults: BTreeMap<String, Faults>,
    /// Pairs of hosts, either way round, whose gates drop everything.
    cut: BTreeSet<(String, String)>,
}

impl State {
    fn faults(&self, host: &str) -> Faults {
        self.faults.get(host).copied().unwrap_or_default()
    }

    fn is_cut(&self, a: &str, b: &str) -> bool {
        self.cut.contains(&link_key(a, b))
    }
}

/// Every gate on the net's LAN, and what each is set to do.
#[derive(Default)]
pub(crate) struct Lan(Mutex<State>);

impl Lan {
    pub(crate) fn learn(&self, host: &str, id: Uuid) {
        self.0.lock().unwrap().names.insert(id, host.to_owned());
    }

    /// Drops every gate to or from `host`, and what it was set to do.
    pub(crate) fn forget(&self, host: &str) {
        let mut state = self.0.lock().unwrap();
        state.names.retain(|_, name| name != host);
        state.fronts.remove(host);
        state
            .pairs
            .retain(|(from, to), _| from != host && to != host);
        state.faults.remove(host);
        state.cut.retain(|(a, b)| a != host && b != host);
    }

    pub(crate) fn name_of(&self, id: Uuid) -> Option<String> {
        self.0.lock().unwrap().names.get(&id).cloned()
    }

    /// The gate in front of `host` for a client outside the net, pointed
    /// at `upstream` when given; made the first time it is asked for.
    pub(crate) fn front(
        &self,
        host: &str,
        upstream: Option<SocketAddr>,
        on: Option<std::net::IpAddr>,
    ) -> std::io::Result<SocketAddr> {
        let mut state = self.0.lock().unwrap();
        let gate = match state.fronts.get(host) {
            Some(gate) => gate.clone(),
            None => {
                let gate = Arc::new(match on {
                    Some(on) => UdpGate::bind_on(on, upstream)?,
                    None => UdpGate::bind(upstream)?,
                });
                gate.set_faults(state.faults(host));
                state.fronts.insert(host.to_owned(), gate.clone());
                gate
            }
        };
        if let Some(upstream) = upstream {
            gate.set_upstream(upstream);
        }
        Ok(gate.addr())
    }

    /// Sets what `host`'s front gate alone does, leaving its pairings as
    /// they are.
    pub(crate) fn set_front_faults(&self, host: &str, faults: Faults) {
        if let Some(front) = self.0.lock().unwrap().fronts.get(host) {
            front.set_faults(faults);
        }
    }

    /// Whether `host` has a front gate yet.
    pub(crate) fn has_front(&self, host: &str) -> bool {
        self.0.lock().unwrap().fronts.contains_key(host)
    }

    /// The gate `from` dials to reach `to`'s listener at `upstream`; made
    /// the first time, cut or degraded as the pair is now.
    pub(crate) fn pair(
        &self,
        from: &str,
        to: &str,
        upstream: SocketAddr,
    ) -> std::io::Result<SocketAddr> {
        let mut state = self.0.lock().unwrap();
        let key = (from.to_owned(), to.to_owned());
        let gate = match state.pairs.get(&key) {
            Some(gate) => gate.clone(),
            None => {
                let gate = Arc::new(UdpGate::bind(Some(upstream))?);
                gate.set_faults(state.faults(from).then(state.faults(to)));
                gate.block(state.is_cut(from, to));
                state.pairs.insert(key, gate.clone());
                gate
            }
        };
        gate.set_upstream(upstream);
        Ok(gate.addr())
    }

    /// Cuts every gate between `a` and `b`, both ways, now and any made
    /// later, or lets them through again. Answers how many gates there are.
    pub(crate) fn cut(&self, a: &str, b: &str, cut: bool) -> usize {
        let mut state = self.0.lock().unwrap();
        if cut {
            state.cut.insert(link_key(a, b));
        } else {
            state.cut.remove(&link_key(a, b));
        }
        let mut gates = 0;
        for ((from, to), gate) in &state.pairs {
            if (from == a && to == b) || (from == b && to == a) {
                gate.block(cut);
                gates += 1;
            }
        }
        gates
    }

    /// Makes `host`'s network bad: every gate it sends or listens through
    /// takes `faults`, combined with the other end's. Answers how many
    /// gates changed.
    pub(crate) fn set_faults(&self, host: &str, faults: Faults) -> usize {
        let mut state = self.0.lock().unwrap();
        state.faults.insert(host.to_owned(), faults);
        let mut gates = 0;
        if let Some(front) = state.fronts.get(host) {
            front.set_faults(faults);
            gates += 1;
        }
        for ((from, to), gate) in &state.pairs {
            if from == host || to == host {
                gate.set_faults(state.faults(from).then(state.faults(to)));
                gates += 1;
            }
        }
        gates
    }

    pub(crate) fn faults(&self, host: &str) -> Faults {
        self.0.lock().unwrap().faults(host)
    }
}

/// The scripted discovery bus as one host sees it through the net's gates:
/// each other host it finds is announced at the gate the net keeps for the
/// pair, so the host dials through it.
pub(crate) struct Gated {
    inner: Arc<dyn Discovery>,
    me: String,
    lan: Arc<Lan>,
    events: broadcast::Sender<DiscoveryEvent>,
    forwarding: OnceLock<()>,
}

impl Gated {
    pub(crate) fn new(inner: Arc<dyn Discovery>, me: &str, lan: Arc<Lan>) -> Self {
        Self {
            inner,
            me: me.to_owned(),
            lan,
            events: broadcast::channel(64).0,
            forwarding: OnceLock::new(),
        }
    }

    fn rewrite(&self, mut advert: Advertisement) -> Advertisement {
        let Some(name) = self.lan.name_of(advert.host_id) else {
            return advert;
        };
        if name == self.me {
            return advert;
        }
        let gated: Vec<SocketAddr> = advert
            .addrs
            .iter()
            .filter(|addr| addr.ip().is_loopback())
            .take(1)
            .filter_map(|addr| self.lan.pair(&self.me, &name, *addr).ok())
            .collect();
        if !gated.is_empty() {
            advert.addrs = gated;
        }
        advert
    }
}

impl Discovery for Gated {
    fn advertise(&self, advert: Advertisement) -> Result<(), DiscoveryError> {
        self.inner.advertise(advert)
    }

    fn withdraw(&self) {
        self.inner.withdraw();
    }

    fn browse(&self) -> broadcast::Receiver<DiscoveryEvent> {
        let receiver = self.events.subscribe();
        self.forwarding.get_or_init(|| {
            let mut found = self.inner.browse();
            let events = self.events.clone();
            let this = Gated {
                inner: self.inner.clone(),
                me: self.me.clone(),
                lan: self.lan.clone(),
                events: self.events.clone(),
                forwarding: OnceLock::new(),
            };
            tokio::spawn(async move {
                loop {
                    let event = match found.recv().await {
                        Ok(event) => event,
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(broadcast::error::RecvError::Closed) => return,
                    };
                    let event = match event {
                        DiscoveryEvent::Found(advert) => {
                            DiscoveryEvent::Found(this.rewrite(advert))
                        }
                        lost => lost,
                    };
                    let _ = events.send(event);
                }
            });
        });
        receiver
    }

    fn requery(&self) {
        self.inner.requery();
    }
}

/// A Bonjour advertiser that names the host's front gate instead of its
/// listener, so a client outside the net dials through the gate.
pub(crate) struct FrontAdvert {
    inner: Arc<dyn Discovery>,
    me: String,
    lan: Arc<Lan>,
    /// Where the gate listens for a client outside this Mac; loopback when
    /// none is named.
    on: Option<std::net::IpAddr>,
}

impl FrontAdvert {
    pub(crate) fn new(
        inner: Arc<dyn Discovery>,
        me: &str,
        lan: Arc<Lan>,
        on: Option<std::net::IpAddr>,
    ) -> Self {
        Self {
            inner,
            me: me.to_owned(),
            lan,
            on,
        }
    }
}

impl Discovery for FrontAdvert {
    fn advertise(&self, mut advert: Advertisement) -> Result<(), DiscoveryError> {
        if let Some(listener) = advert.addrs.first().copied() {
            let front = self
                .lan
                .front(&self.me, Some(listener), self.on)
                .map_err(|error| DiscoveryError::Register(error.to_string()))?;
            advert.addrs = vec![front];
        }
        self.inner.advertise(advert)
    }

    fn withdraw(&self) {
        self.inner.withdraw();
    }

    fn browse(&self) -> broadcast::Receiver<DiscoveryEvent> {
        self.inner.browse()
    }

    fn requery(&self) {
        self.inner.requery();
    }
}
