//! Links to peers and the channels over them, as the core knows them.
//!
//! The driver owns every connection and stream. It reports a link when it
//! comes up and when it goes, and hands over the link control frames that
//! say which hosts the peer reaches: its hello and each neighbor coming up
//! or going down. A channel is opened only at the core's word, with the key
//! the core gives for its handshake, and the driver reports how it went.
//!
//! The routes to a host are read off the links: its own links, newest
//! first, then each peer whose link claims to reach it.
//!
//! A resumed link is decided here before anything rides it: its ticket
//! names a key, and a key no longer pinned for the peer is refused. The
//! control frames also carry where each trusted host can be reached, the
//! address the relay sees this host at, and the forwards a relay or a
//! daemon refused, which presence reads.

use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;
use wire::message::Body;
use wire::{HelloAck, hello_ack};

use crate::Daemon;
use crate::ids::{ChannelId, HostId, LinkId, TimerId};
use crate::io::{Job, Output};
use crate::payload::{
    Advertisement, CarrierKind, ChannelClass, ChannelError, LinkFrame, PeerAddress, PublicKey,
    Source,
};
use crate::presence::Route;
use crate::trust::TrustChange;

/// How long before the relay credential expires its refresh comes.
pub const REFRESH_BEFORE_EXPIRY_MS: u64 = 5 * 60 * 1000;

/// How old a relay credential grows before it is refreshed. Credentials
/// last a week so a launch can reuse one, but the plan they carry is only
/// as fresh as their fetch: refreshing hourly bounds how long a lapse or a
/// purchase goes unnoticed.
pub const REFRESH_AFTER_MS: u64 = 60 * 60 * 1000;

/// Every link that is up, by id, with its peer and the hosts the peer
/// claims to reach, and the channels opened over them. The relay keeps one
/// too.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkRegistry {
    links: BTreeMap<LinkId, Link>,
    channels: BTreeMap<ChannelId, Channel>,
    /// The last channel id minted.
    next_channel: u64,
}

/// One link that is up.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Link {
    pub peer: HostId,
    pub kind: CarrierKind,
    /// The hosts the peer says it reaches, other than this host, itself
    /// and any relay.
    pub claims: BTreeSet<HostId>,
}

/// A channel the core asked for, until it closes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Channel {
    pub link: LinkId,
    pub peer: HostId,
    pub class: ChannelClass,
    /// Whether the driver reported its handshake done.
    pub open: bool,
    /// Where the catalogue's revocation notes stood as it was asked for.
    pub begun: u64,
}

impl LinkRegistry {
    /// The link, while it is up.
    pub fn link(&self, link: LinkId) -> Option<&Link> {
        self.links.get(&link)
    }

    /// Every link that is up.
    pub fn links(&self) -> impl Iterator<Item = (LinkId, &Link)> {
        self.links.iter().map(|(id, link)| (*id, link))
    }

    /// The channel, until it closes.
    pub fn channel(&self, channel: ChannelId) -> Option<&Channel> {
        self.channels.get(&channel)
    }

    /// Every channel asked for and not yet closed.
    pub fn channels(&self) -> impl Iterator<Item = (ChannelId, &Channel)> {
        self.channels.iter().map(|(id, channel)| (*id, channel))
    }

    /// Every route to `host`, the one a fresh call takes first: its own
    /// links, those on a path of their own before those a forwarder
    /// carries, and newest first within each, since a second link to a
    /// peer is a crossed dial whose newer link is the one both sides keep;
    /// then each peer that
    /// claims to reach it, in the order their links came up; and last the
    /// account's relay. The relay claims every host on the account and
    /// refuses to carry a channel while the plan has no connections between
    /// hosts, and a retry tries only the first route: listed first, it
    /// would keep a host a peer reaches reading away.
    pub fn routes_to(&self, host: HostId) -> Vec<Route> {
        let mut routes: Vec<Route> = self
            .links
            .iter()
            .rev()
            .filter(|(_, link)| link.peer == host)
            .map(|(id, _)| Route::Direct(*id))
            .collect();
        routes.sort_by_key(|route| self.rank(*route));
        let relayed = |link: &&Link| link.kind == CarrierKind::Relay;
        let peers = self.links.values().filter(|link| !relayed(link));
        for link in peers.chain(self.links.values().filter(relayed)) {
            let via = Route::Via(link.peer);
            if link.claims.contains(&host) && !routes.contains(&via) {
                routes.push(via);
            }
        }
        routes
    }

    /// How far `route` is from the host it reaches, lower being nearer: a
    /// link on a path of its own, then a link a forwarder carries, then a
    /// route through a peer that claims the host. A nearer route takes a
    /// host's traffic from a farther one.
    pub fn rank(&self, route: Route) -> u8 {
        match route {
            Route::Direct(link) => match self.links.get(&link) {
                Some(link) if link.kind == CarrierKind::Forwarded => 1,
                _ => 0,
            },
            Route::Via(_) => 2,
        }
    }

    /// Whether `host` is a relay this host links to: one whose claims are
    /// the hosts on the account, and which is never itself tried.
    pub fn is_relay(&self, host: HostId) -> bool {
        self.links
            .values()
            .any(|link| link.peer == host && link.kind == CarrierKind::Relay)
    }

    /// The route a channel over `link` to `peer` takes, while the link is up.
    fn route_over(&self, link: LinkId, peer: HostId) -> Option<Route> {
        let over = self.links.get(&link)?;
        Some(if over.peer == peer {
            Route::Direct(link)
        } else {
            Route::Via(over.peer)
        })
    }
}

impl Daemon {
    /// The links and channels as the core knows them.
    pub fn links(&self) -> &LinkRegistry {
        &self.routing.links
    }

    /// The addresses each trusted host last listed for itself, in its
    /// order, each with where it came from.
    pub fn peer_addresses(&self) -> &BTreeMap<HostId, Vec<PeerAddress>> {
        &self.routing.addresses
    }

    /// The address the relay last saw this host's packets come from.
    pub fn observed_address(&self) -> Option<SocketAddr> {
        self.routing.observed
    }

    /// A link came up. A resumed one whose ticket names a key not pinned
    /// for its peer is refused, and is never taken up: no route lists it,
    /// so nothing the peer sent early is served.
    pub(crate) fn link_up(
        &mut self,
        link: LinkId,
        peer: HostId,
        kind: CarrierKind,
        resumed: Option<&PublicKey>,
        out: &mut Vec<Output>,
    ) {
        if let Some(key) = resumed {
            let host = Uuid::from_bytes(peer.0);
            let pinned = self.trust().pubkey_for_host(host);
            if pinned != Some(key.0.as_slice()) {
                out.push(Output::RefuseResumed { link });
                return;
            }
        }
        let before = self.routes_before([peer]);
        self.routing.links.links.insert(
            link,
            Link {
                peer,
                kind,
                claims: BTreeSet::new(),
            },
        );
        self.routes_moved(before, out);
    }

    /// A link up is carried on a path of another kind now. Its routes stay
    /// as they are, so no call moves; the host reads by the new kind.
    pub(crate) fn link_carried(&mut self, link: LinkId, kind: CarrierKind) {
        let Some(up) = self.routing.links.links.get_mut(&link) else {
            return;
        };
        if up.kind == kind || matches!(up.kind, CarrierKind::Relay) {
            return;
        }
        up.kind = kind;
        self.touched.host_set = true;
    }

    /// Hosts the local network found. A paired host without a link of its
    /// own is dialed; one already linked directly needs no second link, and
    /// a host not paired is only a hint until it is.
    pub(crate) fn discovered(&mut self, found: Vec<Advertisement>, out: &mut Vec<Output>) {
        let own = HostId::from_slice(self.store.own_host());
        for advert in found {
            let linked = self
                .routing
                .links
                .routes_to(advert.host)
                .iter()
                .any(|route| matches!(route, Route::Direct(_)));
            if advert.trusted && Some(advert.host) != own && !linked {
                out.push(Output::Dial { host: advert.host });
            }
        }
    }

    /// The relay link presented a credential: its refresh is armed. On the
    /// free plan it comes on an interval, so a purchase reaches the link
    /// promptly; otherwise once the credential is an hour old, or shortly
    /// before it expires if that is sooner. A credential kept from an
    /// earlier launch may already be due, and is refreshed at once.
    pub(crate) fn credential(
        &mut self,
        link: LinkId,
        expires_at_ms: u64,
        fetched_at_ms: u64,
        free_every_ms: Option<u64>,
        out: &mut Vec<Output>,
    ) {
        let at_ms = match free_every_ms {
            Some(every) => self.timers.now_ms.saturating_add(every),
            None => fetched_at_ms
                .saturating_add(REFRESH_AFTER_MS)
                .min(expires_at_ms.saturating_sub(REFRESH_BEFORE_EXPIRY_MS)),
        };
        self.arm(TimerId::Refresh(link), at_ms, out);
    }

    /// The relay asked for a fresh credential: another link of the account
    /// presented a paid plan, so this one's may be out of date. It is
    /// refreshed at once in place of its timer, which is cancelled. Only a
    /// link holding a credential has a refresh armed, so a peer that is not
    /// the relay asks nothing of a link it has no say over.
    fn refresh_asked(&mut self, link: LinkId, out: &mut Vec<Output>) {
        if self.timers.armed.contains(&TimerId::Refresh(link)) {
            self.cancel(TimerId::Refresh(link), out);
            self.refresh(link, out);
        }
    }

    /// The relay link's credential is due for refresh. A timer cancelled
    /// after it rang, by a refresh the relay asked for, is not due.
    pub(crate) fn refresh_due(&mut self, link: LinkId, out: &mut Vec<Output>) {
        if self.timers.armed.remove(&TimerId::Refresh(link)) {
            self.refresh(link, out);
        }
    }

    fn refresh(&mut self, link: LinkId, out: &mut Vec<Output>) {
        if self.routing.links.links.contains_key(&link) {
            let id = self.pending.mint();
            out.push(Output::Job {
                id,
                op: Job::RefreshToken { link },
            });
        }
    }

    /// The link is gone, and with it every channel it carried: the driver
    /// ends their streams with the connection.
    pub(crate) fn link_down(&mut self, link: LinkId, out: &mut Vec<Output>) {
        self.cancel(TimerId::Refresh(link), out);
        let Some(gone) = self.routing.links.links.get(&link) else {
            return;
        };
        let hosts: Vec<HostId> = std::iter::once(gone.peer)
            .chain(gone.claims.iter().copied())
            .collect();
        let before = self.routes_before(hosts);
        let peer = gone.peer;
        let registry = &mut self.routing.links;
        registry.links.remove(&link);
        registry.channels.retain(|_, channel| channel.link != link);
        self.catalogue_link_down(link, peer);
        self.routes_moved(before, out);
    }

    /// A link control frame from the peer. Its hello carries the hosts it
    /// reaches as the link came up; each neighbor frame after says one
    /// came or went. Anything else on the control stream is the driver's.
    pub(crate) fn link_control(&mut self, link: LinkId, frame: LinkFrame, out: &mut Vec<Output>) {
        let own = HostId::from_slice(self.store.own_host());
        let Some(entry) = self.routing.links.links.get(&link) else {
            return;
        };
        let (peer, kind) = (entry.peer, entry.kind);
        if let Some(body) = &frame.body {
            self.catalogue_frame(link, peer, body, out);
            self.addressed(link, peer, kind, body, out);
        }
        let Some(entry) = self.routing.links.links.get(&link) else {
            return;
        };
        let mut claims = entry.claims.clone();
        let other = |host: &HostId| Some(*host) != own && *host != peer;
        match frame.body {
            Some(Body::Hello(hello)) => {
                claims = hosts(&hello.neighbors).filter(other).collect();
            }
            Some(Body::HelloAck(HelloAck {
                outcome: Some(hello_ack::Outcome::Accepted(accepted)),
            })) => {
                claims = hosts(&accepted.neighbors).filter(other).collect();
            }
            Some(Body::NeighborUp(up)) => {
                if let Some(host) = up
                    .host
                    .as_ref()
                    .filter(|host| !is_relay(host))
                    .and_then(|host| HostId::from_slice(&host.host_id))
                    .filter(other)
                {
                    claims.insert(host);
                }
            }
            Some(Body::NeighborDown(down)) => {
                if let Some(host) = HostId::from_slice(&down.host_id) {
                    claims.remove(&host);
                }
            }
            Some(Body::RefreshCredential(_)) => {
                self.refresh_asked(link, out);
                return;
            }
            Some(
                Body::HelloAck(_)
                | Body::Reauth(_)
                | Body::LinkClose(_)
                | Body::Addresses(_)
                | Body::ObservedAddress(_)
                | Body::ForwardRefused(_),
            )
            | None => return,
        }
        let before = self.routes_before(entry.claims.union(&claims).copied().collect::<Vec<_>>());
        if let Some(entry) = self.routing.links.links.get_mut(&link) {
            entry.claims = claims;
        }
        self.routes_moved(before, out);
    }

    /// What a control frame says of addresses and refused forwards: a
    /// trusted peer's own list and public address, on its hello, its
    /// acceptance or a later `Addresses`; the relay's sight of this host, on
    /// its acceptance or a later `ObservedAddress`; and a forward the peer
    /// refused.
    fn addressed(
        &mut self,
        link: LinkId,
        peer: HostId,
        kind: CarrierKind,
        body: &Body,
        out: &mut Vec<Output>,
    ) {
        let relay = kind == CarrierKind::Relay;
        match body {
            Body::Hello(hello) => {
                self.peer_listed(peer, kind, &hello.addrs, hello.public_addr.as_deref(), out)
            }
            Body::HelloAck(HelloAck {
                outcome: Some(hello_ack::Outcome::Accepted(accepted)),
            }) => {
                self.peer_listed(
                    peer,
                    kind,
                    &accepted.addrs,
                    accepted.public_addr.as_deref(),
                    out,
                );
                if relay && let Some(addr) = &accepted.observed_addr {
                    self.observed(addr, out);
                }
            }
            Body::Addresses(addresses) => self.peer_listed(
                peer,
                kind,
                &addresses.addrs,
                addresses.public_addr.as_deref(),
                out,
            ),
            Body::ObservedAddress(observed) if relay => self.observed(&observed.addr, out),
            Body::ForwardRefused(refused) => {
                if let Some(dst) = HostId::from_slice(&refused.dst) {
                    self.forward_refused(link, dst, refused.reason());
                }
            }
            Body::HelloAck(_)
            | Body::ObservedAddress(_)
            | Body::NeighborUp(_)
            | Body::NeighborDown(_)
            | Body::Reauth(_)
            | Body::LinkClose(_)
            | Body::RefreshCredential(_) => {}
        }
    }

    /// A peer listed where it can be reached. Only a trusted host's list is
    /// kept, and an address that does not read as one is passed over. Each
    /// address is tagged by where it came from: listed over a link of the
    /// peer's own, over a forwarded one, or the public address the relay
    /// told the peer. An address the peer listed both ways keeps the first
    /// tag; one already known as listed directly stays so when a forwarded
    /// link lists it again, so a host linked both ways does not flip it.
    /// A change is kept on the drive with the host's trust.
    fn peer_listed(
        &mut self,
        peer: HostId,
        kind: CarrierKind,
        listed: &[String],
        public: Option<&str>,
        out: &mut Vec<Output>,
    ) {
        let host = Uuid::from_bytes(peer.0);
        if self.trust().entry(host).is_none() {
            return;
        }
        let source = match kind {
            CarrierKind::Forwarded => Source::Forwarding,
            CarrierKind::Direct | CarrierKind::Ssh | CarrierKind::Relay => Source::Listed,
        };
        let known = self
            .routing
            .addresses
            .get(&peer)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let mut addrs: Vec<PeerAddress> = Vec::new();
        let tagged = listed
            .iter()
            .map(|addr| (addr.as_str(), source))
            .chain(public.map(|addr| (addr, Source::Observed)));
        for (addr, source) in tagged {
            let Ok(addr) = addr.parse::<SocketAddr>() else {
                continue;
            };
            if addrs.iter().any(|kept| kept.addr == addr) {
                continue;
            }
            let source = match source {
                Source::Forwarding
                    if known
                        .iter()
                        .any(|kept| kept.addr == addr && kept.source == Source::Listed) =>
                {
                    Source::Listed
                }
                source => source,
            };
            addrs.push(PeerAddress { addr, source });
        }
        if known == addrs.as_slice() {
            return;
        }
        if addrs.is_empty() {
            self.routing.addresses.remove(&peer);
        } else {
            self.routing.addresses.insert(peer, addrs.clone());
        }
        out.push(Output::PeerAddresses {
            host: peer,
            addrs: addrs.clone(),
        });
        self.change_trust(
            TrustChange::Listed {
                host,
                listed: addrs,
            },
            None,
            out,
        );
    }

    fn observed(&mut self, addr: &str, out: &mut Vec<Output>) {
        let Ok(addr) = addr.parse::<SocketAddr>() else {
            return;
        };
        if self.routing.observed != Some(addr) {
            self.routing.observed = Some(addr);
            out.push(Output::ObservedAddress { addr });
        }
    }

    /// The trust file as read at start: what each host listed when last
    /// heard is where it is looked for until it says otherwise.
    pub(crate) fn remember_addresses(&mut self) {
        let listed: BTreeMap<HostId, Vec<PeerAddress>> = self
            .trust()
            .entries()
            .filter(|(_, entry)| !entry.listed.is_empty())
            .map(|(host, entry)| (HostId(host.into_bytes()), entry.listed.clone()))
            .collect();
        self.routing.addresses = listed;
    }

    /// A host's trust ended: what it listed for itself goes with it.
    pub(crate) fn forget_addresses(&mut self, host: HostId) {
        self.routing.addresses.remove(&host);
    }

    /// The shell wants a channel: opened over a link that is up, to a peer
    /// whose key is pinned, or refused.
    pub(crate) fn want_channel(
        &mut self,
        link: LinkId,
        peer: HostId,
        class: ChannelClass,
        pin: Option<PublicKey>,
        out: &mut Vec<Output>,
    ) {
        let refused = |reason| Output::ChannelRefused {
            link,
            peer,
            class,
            reason,
        };
        if self.routing.links.route_over(link, peer).is_none() {
            out.push(refused(ChannelError::NoLink));
            return;
        }
        let Some(pin) = pin else {
            out.push(refused(ChannelError::Unpinned));
            return;
        };
        let begun = self.routing.catalogue.mark();
        let registry = &mut self.routing.links;
        registry.next_channel += 1;
        let channel = ChannelId(registry.next_channel);
        registry.channels.insert(
            channel,
            Channel {
                link,
                peer,
                class,
                open: false,
                begun,
            },
        );
        out.push(Output::OpenChannel {
            channel,
            link,
            peer,
            class,
            pin,
        });
    }

    /// The driver's handshake on a channel the core asked for verified its
    /// pin. One whose link went meanwhile is already forgotten.
    pub(crate) fn channel_opened(&mut self, channel: ChannelId) {
        if let Some(opened) = self.routing.links.channels.get_mut(&channel) {
            opened.open = true;
        }
    }

    /// A channel closed. Whatever it closed for says nothing of the host:
    /// its link going is what does.
    pub(crate) fn channel_closed(&mut self, channel: ChannelId) {
        self.routing.links.channels.remove(&channel);
    }
}

/// Whether a host a frame names says it is a relay.
fn is_relay(host: &wire::Host) -> bool {
    host.capabilities
        .as_ref()
        .is_some_and(|capabilities| capabilities.cloud_relay)
}

/// The ids of the hosts a frame lists, other than relays; one that is not
/// an id names no host.
fn hosts(hosts: &[wire::Host]) -> impl Iterator<Item = HostId> + '_ {
    hosts
        .iter()
        .filter(|host| !is_relay(host))
        .filter_map(|host| HostId::from_slice(&host.host_id))
}

#[cfg(test)]
mod tests {
    use store::Sqlite;
    use wire::{Hello, Message, NeighborDown, NeighborUp};

    use super::*;
    use crate::StartOptions;
    use crate::io::Input;
    use crate::payload::LinkCloseReason;

    const OWN: [u8; 16] = *b"own-host-0000000";
    const PEER: HostId = HostId([1; 16]);
    const LINK: LinkId = LinkId(7);

    fn daemon() -> Daemon {
        Daemon::new(
            Sqlite::open_in_memory(OWN).expect("a store opens"),
            StartOptions::default(),
        )
    }

    fn host(id: [u8; 16]) -> wire::Host {
        wire::Host {
            host_id: id.to_vec(),
            ..Default::default()
        }
    }

    fn step(daemon: &mut Daemon, input: Input) -> Vec<Output> {
        let mut out = Vec::new();
        daemon.step(input, &mut out);
        out
    }

    fn up(daemon: &mut Daemon) {
        step(
            daemon,
            Input::LinkUp {
                link: LINK,
                peer: PEER,
                kind: CarrierKind::Direct,
                resumed: None,
            },
        );
    }

    fn claims(daemon: &Daemon) -> Vec<HostId> {
        let link = daemon.links().link(LINK).expect("the link is up");
        link.claims.iter().copied().collect()
    }

    /// What the core said about the channel asked for.
    fn want(daemon: &mut Daemon, pin: Option<PublicKey>) -> Vec<Output> {
        step(
            daemon,
            Input::WantChannel {
                link: LINK,
                peer: PEER,
                class: ChannelClass::Calls,
                pin,
            },
        )
        .into_iter()
        .filter(|output| {
            matches!(
                output,
                Output::OpenChannel { .. } | Output::ChannelRefused { .. }
            )
        })
        .collect()
    }

    #[test]
    fn a_peers_hello_and_neighbor_frames_say_which_hosts_it_reaches() {
        let mut daemon = daemon();
        up(&mut daemon);
        let hello = Message {
            body: Some(Body::Hello(Hello {
                neighbors: vec![host([2; 16]), host(OWN), host(PEER.0), host([3; 16])],
                ..Default::default()
            })),
        };
        step(
            &mut daemon,
            Input::LinkControl {
                link: LINK,
                frame: hello,
            },
        );
        assert_eq!(claims(&daemon), vec![HostId([2; 16]), HostId([3; 16])]);

        let neighbor_up = Message {
            body: Some(Body::NeighborUp(NeighborUp {
                host: Some(host([4; 16])),
            })),
        };
        let neighbor_down = Message {
            body: Some(Body::NeighborDown(NeighborDown {
                host_id: vec![2; 16],
            })),
        };
        for frame in [neighbor_up, neighbor_down] {
            step(&mut daemon, Input::LinkControl { link: LINK, frame });
        }
        assert_eq!(claims(&daemon), vec![HostId([3; 16]), HostId([4; 16])]);
    }

    #[test]
    fn a_channel_opens_only_over_a_link_that_is_up_to_a_pinned_peer() {
        let mut daemon = daemon();
        let pin = PublicKey(vec![9; 32]);
        assert!(matches!(
            want(&mut daemon, Some(pin.clone())).as_slice(),
            [Output::ChannelRefused {
                reason: ChannelError::NoLink,
                ..
            }]
        ));
        up(&mut daemon);
        assert!(matches!(
            want(&mut daemon, None).as_slice(),
            [Output::ChannelRefused {
                reason: ChannelError::Unpinned,
                ..
            }]
        ));
        let out = want(&mut daemon, Some(pin.clone()));
        let [
            Output::OpenChannel {
                channel,
                link: LINK,
                peer: PEER,
                class: ChannelClass::Calls,
                pin: given,
            },
        ] = out.as_slice()
        else {
            panic!("the channel is opened: {out:?}");
        };
        assert_eq!(
            *given, pin,
            "the handshake checks the key pinned for the peer"
        );
        let channel = *channel;
        step(
            &mut daemon,
            Input::ChannelOpened {
                channel,
                link: LINK,
                peer: PEER,
                class: ChannelClass::Calls,
            },
        );
        assert!(daemon.links().channel(channel).is_some_and(|c| c.open));

        // A second channel gets an id of its own; closing one forgets it.
        let out = want(&mut daemon, Some(pin));
        let [
            Output::OpenChannel {
                channel: second, ..
            },
        ] = out.as_slice()
        else {
            panic!("the second channel is opened: {out:?}");
        };
        assert_ne!(*second, channel);
        step(
            &mut daemon,
            Input::ChannelClosed {
                channel,
                reason: ChannelError::Ended,
            },
        );
        assert!(daemon.links().channel(channel).is_none());

        // The link going takes its channels with it.
        step(
            &mut daemon,
            Input::LinkDown {
                link: LINK,
                reason: LinkCloseReason::UserShutdown,
            },
        );
        assert!(daemon.links().link(LINK).is_none());
        assert_eq!(daemon.links().channels().count(), 0);
    }

    /// A daemon trusting `PEER` under the key [`TrustStore::of_hosts`]
    /// makes for it.
    fn trusting() -> (Daemon, PublicKey) {
        let mut daemon = daemon();
        let store = crate::trust::TrustStore::of_hosts([PEER]);
        let key = store
            .pubkey_for_host(Uuid::from_bytes(PEER.0))
            .expect("the peer is pinned")
            .to_vec();
        {
            daemon.trust_for_test(store);
            step(&mut daemon, Input::Started)
        };
        (daemon, PublicKey(key))
    }

    fn resumed(daemon: &mut Daemon, peer: HostId, key: PublicKey) -> Vec<Output> {
        step(
            daemon,
            Input::LinkUp {
                link: LINK,
                peer,
                kind: CarrierKind::Direct,
                resumed: Some(key),
            },
        )
    }

    #[test]
    fn a_resumed_link_naming_the_key_pinned_for_its_peer_comes_up() {
        let (mut daemon, key) = trusting();
        let out = resumed(&mut daemon, PEER, key);
        assert!(
            !out.contains(&Output::RefuseResumed { link: LINK }),
            "{out:?}"
        );
        assert!(daemon.links().link(LINK).is_some());
        assert_eq!(daemon.links().routes_to(PEER), vec![Route::Direct(LINK)]);
    }

    #[test]
    fn a_resumed_link_naming_a_key_not_pinned_is_refused_before_anything_rides_it() {
        let (mut daemon, _) = trusting();
        for (peer, key) in [
            (PEER, PublicKey(vec![3; 32])),
            (HostId([5; 16]), PublicKey(vec![5; 32])),
        ] {
            let out = resumed(&mut daemon, peer, key);
            assert_eq!(out, vec![Output::RefuseResumed { link: LINK }]);
            assert!(daemon.links().link(LINK).is_none());
            assert!(daemon.links().routes_to(peer).is_empty());
        }

        // A host whose pin was removed is refused with its old key.
        let (mut daemon, key) = trusting();
        {
            daemon.trust_for_test(crate::trust::TrustStore::default());
            step(&mut daemon, Input::Started)
        };
        assert_eq!(
            resumed(&mut daemon, PEER, key),
            vec![Output::RefuseResumed { link: LINK }]
        );
    }

    #[test]
    fn an_address_list_heard_while_the_wal_moves_is_kept_once_it_has() {
        let (mut daemon, _) = trusting();
        up(&mut daemon);
        let started = step(
            &mut daemon,
            Input::Timer {
                id: crate::TimerId::Checkpoint,
            },
        );
        let wal = started
            .iter()
            .find_map(|output| match output {
                Output::Job {
                    id,
                    op: crate::io::Job::CheckpointWal,
                } => Some(*id),
                _ => None,
            })
            .expect("a WAL job");
        // A control frame is not held by the checkpoint, so its trust
        // change queues behind it.
        listed(&mut daemon, addresses(&["192.168.1.9:7000"], None));
        let stored = |daemon: &Daemon| daemon.store().trusted_hosts().unwrap()[0].listed.clone();
        assert_eq!(stored(&daemon), "[]", "nothing lands while the WAL moves");
        step(
            &mut daemon,
            Input::JobDone {
                job: wal,
                result: crate::payload::JobResult::Done,
            },
        );
        step(&mut daemon, Input::Checkpointed);
        let entry = daemon.trust().entry(Uuid::from_bytes(PEER.0)).unwrap();
        assert!(!entry.listed.is_empty(), "the served trust has the list");
        assert_ne!(stored(&daemon), "[]", "and so does the store");
    }

    fn listed(daemon: &mut Daemon, body: Body) -> Vec<Output> {
        listed_on(daemon, LINK, body)
    }

    /// What the core says of addresses on hearing `body` on `link`: the
    /// list it tells the driver and the trust change it writes.
    fn listed_on(daemon: &mut Daemon, link: LinkId, body: Body) -> Vec<Output> {
        step(
            daemon,
            Input::LinkControl {
                link,
                frame: Message { body: Some(body) },
            },
        )
        .into_iter()
        .filter(|output| {
            matches!(
                output,
                Output::PeerAddresses { .. } | Output::ObservedAddress { .. }
            )
        })
        .collect()
    }

    fn addrs(addrs: &[&str]) -> Vec<String> {
        addrs.iter().map(|addr| addr.to_string()).collect()
    }

    fn tagged(addrs: &[(&str, Source)]) -> Vec<PeerAddress> {
        addrs
            .iter()
            .map(|(addr, source)| PeerAddress {
                addr: addr.parse().unwrap(),
                source: *source,
            })
            .collect()
    }

    fn addresses(addrs: &[&str], public: Option<&str>) -> Body {
        Body::Addresses(wire::Addresses {
            addrs: self::addrs(addrs),
            public_addr: public.map(str::to_owned),
        })
    }

    #[test]
    fn a_trusted_hosts_latest_address_list_is_kept_and_told() {
        let (mut daemon, _) = trusting();
        up(&mut daemon);
        let hello = Body::Hello(Hello {
            addrs: addrs(&["192.168.1.4:7000", "not an address", "[2001:db8::4]:7000"]),
            ..Default::default()
        });
        let first = tagged(&[
            ("192.168.1.4:7000", Source::Listed),
            ("[2001:db8::4]:7000", Source::Listed),
        ]);
        assert_eq!(
            listed(&mut daemon, hello),
            vec![Output::PeerAddresses {
                host: PEER,
                addrs: first.clone(),
            }]
        );
        assert_eq!(daemon.peer_addresses().get(&PEER), Some(&first));

        let same = addresses(&["192.168.1.4:7000", "[2001:db8::4]:7000"], None);
        assert_eq!(listed(&mut daemon, same), Vec::new(), "nothing moved");

        let moved = addresses(&["10.0.0.9:7000"], None);
        let now = tagged(&[("10.0.0.9:7000", Source::Listed)]);
        assert_eq!(
            listed(&mut daemon, moved),
            vec![Output::PeerAddresses {
                host: PEER,
                addrs: now.clone(),
            }]
        );
        assert_eq!(daemon.peer_addresses().get(&PEER), Some(&now));

        let accepted = Body::HelloAck(HelloAck {
            outcome: Some(hello_ack::Outcome::Accepted(wire::HelloAccepted {
                addrs: Vec::new(),
                ..Default::default()
            })),
        });
        assert_eq!(
            listed(&mut daemon, accepted),
            vec![Output::PeerAddresses {
                host: PEER,
                addrs: Vec::new(),
            }]
        );
        assert_eq!(daemon.peer_addresses().get(&PEER), None);

        // Forgetting the host forgets what it listed.
        listed(&mut daemon, addresses(&["10.0.0.9:7000"], None));
        step(&mut daemon, Input::HostForgotten { host: PEER });
        assert_eq!(daemon.peer_addresses().get(&PEER), None);
    }

    /// The public address a peer passes on is the only one tagged as the
    /// relay's observation; what it lists over a forwarded link is tagged
    /// so, unless a link of its own already listed it.
    #[test]
    fn each_address_a_peer_lists_is_tagged_by_where_it_came_from() {
        let (mut daemon, _) = trusting();
        let forwarded = LinkId(5);
        step(
            &mut daemon,
            Input::LinkUp {
                link: forwarded,
                peer: PEER,
                kind: CarrierKind::Forwarded,
                resumed: None,
            },
        );
        let both = addresses(&["192.168.1.4:7000"], Some("203.0.113.9:41000"));
        assert_eq!(
            listed_on(&mut daemon, forwarded, both.clone()),
            vec![Output::PeerAddresses {
                host: PEER,
                addrs: tagged(&[
                    ("192.168.1.4:7000", Source::Forwarding),
                    ("203.0.113.9:41000", Source::Observed),
                ]),
            }]
        );

        up(&mut daemon);
        let direct = tagged(&[
            ("192.168.1.4:7000", Source::Listed),
            ("203.0.113.9:41000", Source::Observed),
        ]);
        assert_eq!(
            listed(&mut daemon, both.clone()),
            vec![Output::PeerAddresses {
                host: PEER,
                addrs: direct.clone(),
            }]
        );
        assert_eq!(
            listed_on(&mut daemon, forwarded, both),
            Vec::new(),
            "the forwarded link says nothing new"
        );
        assert_eq!(daemon.peer_addresses().get(&PEER), Some(&direct));

        // A public address the peer also lists directly is listed.
        let public_too = addresses(&["203.0.113.9:41000"], Some("203.0.113.9:41000"));
        assert_eq!(
            listed(&mut daemon, public_too),
            vec![Output::PeerAddresses {
                host: PEER,
                addrs: tagged(&[("203.0.113.9:41000", Source::Listed)]),
            }]
        );
    }

    /// Every change is written with the host's trust, and the trust file
    /// read at the next start brings the list back.
    #[test]
    fn a_peers_list_is_kept_with_its_trust_and_read_back_at_start() {
        let (mut daemon, _) = trusting();
        up(&mut daemon);
        let list = tagged(&[
            ("192.168.1.4:7000", Source::Listed),
            ("203.0.113.9:41000", Source::Observed),
        ]);
        let out = step(
            &mut daemon,
            Input::LinkControl {
                link: LINK,
                frame: Message {
                    body: Some(addresses(&["192.168.1.4:7000"], Some("203.0.113.9:41000"))),
                },
            },
        );
        let host = Uuid::from_bytes(PEER.0);
        let held = crate::trust::TrustStore::load(daemon.store());
        assert_eq!(
            held.entry(host).map(|entry| &entry.listed),
            Some(&list),
            "{out:?}"
        );
        assert_eq!(
            daemon.trust().entry(host).map(|entry| &entry.listed),
            Some(&list)
        );

        let store = store::Sqlite::restore(&daemon.store().dump().unwrap()).unwrap();
        let mut restarted = Daemon::new(store, daemon.options.clone());
        step(&mut restarted, Input::Started);
        assert_eq!(restarted.peer_addresses().get(&PEER), Some(&list));
    }

    #[test]
    fn a_host_not_trusted_lists_nothing_kept() {
        let mut daemon = daemon();
        up(&mut daemon);
        let hello = addresses(&["192.168.1.4:7000"], None);
        assert_eq!(listed(&mut daemon, hello), Vec::new());
        assert!(daemon.peer_addresses().is_empty());
    }

    #[test]
    fn the_relay_tells_this_host_the_address_it_sees_it_at_and_a_peer_does_not() {
        let mut daemon = daemon();
        up(&mut daemon);
        let observed = |addr: &str| {
            Body::ObservedAddress(wire::ObservedAddress {
                addr: addr.to_string(),
            })
        };
        assert_eq!(
            listed(&mut daemon, observed("203.0.113.7:4433")),
            Vec::new(),
            "a peer's link is no relay"
        );

        let relay_link = LinkId(8);
        step(
            &mut daemon,
            Input::LinkUp {
                link: relay_link,
                peer: HostId([9; 16]),
                kind: CarrierKind::Relay,
                resumed: None,
            },
        );
        let on_relay = |daemon: &mut Daemon, body| {
            step(
                daemon,
                Input::LinkControl {
                    link: relay_link,
                    frame: Message { body: Some(body) },
                },
            )
        };
        let accepted = Body::HelloAck(HelloAck {
            outcome: Some(hello_ack::Outcome::Accepted(wire::HelloAccepted {
                observed_addr: Some("203.0.113.7:4433".to_string()),
                ..Default::default()
            })),
        });
        let first: SocketAddr = "203.0.113.7:4433".parse().unwrap();
        assert!(on_relay(&mut daemon, accepted).contains(&Output::ObservedAddress { addr: first }));
        assert_eq!(daemon.observed_address(), Some(first));
        assert!(
            !on_relay(&mut daemon, observed("203.0.113.7:4433"))
                .iter()
                .any(|output| matches!(output, Output::ObservedAddress { .. })),
            "the same address is not news"
        );
        let moved: SocketAddr = "203.0.113.7:5000".parse().unwrap();
        assert!(
            on_relay(&mut daemon, observed("203.0.113.7:5000"))
                .contains(&Output::ObservedAddress { addr: moved })
        );
        assert_eq!(daemon.observed_address(), Some(moved));
    }

    #[test]
    fn the_relay_credential_is_refreshed_on_the_free_interval_hourly_or_before_it_expires() {
        let mut daemon = daemon();
        up(&mut daemon);
        step(&mut daemon, Input::Tick { now_ms: 1_000 });
        let credential = |expires_at_ms, fetched_at_ms, free_every_ms| Input::Credential {
            link: LINK,
            expires_at_ms,
            fetched_at_ms,
            free_every_ms,
        };
        let refresh = TimerId::Refresh(LINK);
        let week = 7 * 24 * REFRESH_AFTER_MS;
        assert_eq!(
            step(&mut daemon, credential(week, 1_000, Some(60_000))),
            vec![Output::ArmTimer {
                id: refresh,
                at_ms: 61_000
            }],
            "the free plan's interval, from now"
        );
        assert_eq!(
            step(&mut daemon, credential(week, 1_000, None)),
            vec![Output::ArmTimer {
                id: refresh,
                at_ms: 1_000 + REFRESH_AFTER_MS
            }],
            "a week-long credential is refreshed once it is an hour old"
        );
        assert_eq!(
            step(&mut daemon, credential(600_000, 1_000, None)),
            vec![Output::ArmTimer {
                id: refresh,
                at_ms: 600_000 - REFRESH_BEFORE_EXPIRY_MS
            }],
            "a short credential is refreshed before it expires"
        );
        assert_eq!(
            step(&mut daemon, credential(week, 0, None)),
            vec![Output::ArmTimer {
                id: refresh,
                at_ms: REFRESH_AFTER_MS
            }],
            "a credential kept from an earlier launch counts its age from its fetch"
        );
        step(&mut daemon, credential(3_600_000, 0, None));
        let out = step(&mut daemon, Input::Timer { id: refresh });
        assert!(
            matches!(
                out.as_slice(),
                [Output::Job {
                    op: Job::RefreshToken { link: LINK },
                    ..
                }]
            ),
            "{out:?}"
        );

        // A link that went takes its refresh with it.
        step(&mut daemon, credential(week, 1_000, None));
        let out = step(
            &mut daemon,
            Input::LinkDown {
                link: LINK,
                reason: LinkCloseReason::UserShutdown,
            },
        );
        assert!(
            out.contains(&Output::CancelTimer { id: refresh }),
            "{out:?}"
        );
        assert_eq!(step(&mut daemon, Input::Timer { id: refresh }), Vec::new());
    }

    /// The relay asks for a fresh credential when another device of the
    /// account has just paid: the refresh its timer would make is made at
    /// once, and the timer is cancelled so it does not refresh again.
    #[test]
    fn the_relay_asking_refreshes_the_credential_at_once() {
        let mut daemon = daemon();
        up(&mut daemon);
        let ask = || Input::LinkControl {
            link: LINK,
            frame: Message {
                body: Some(Body::RefreshCredential(wire::RefreshCredential {})),
            },
        };
        assert_eq!(
            step(&mut daemon, ask()),
            Vec::new(),
            "a link holding no credential has nothing to refresh"
        );

        step(
            &mut daemon,
            Input::Credential {
                link: LINK,
                expires_at_ms: 7 * 24 * REFRESH_AFTER_MS,
                fetched_at_ms: 0,
                free_every_ms: Some(180_000),
            },
        );
        let refresh = TimerId::Refresh(LINK);
        let out = step(&mut daemon, ask());
        assert!(
            matches!(
                out.as_slice(),
                [
                    Output::CancelTimer { id },
                    Output::Job {
                        op: Job::RefreshToken { link: LINK },
                        ..
                    }
                ] if *id == refresh
            ),
            "the refresh is made at once in place of its timer: {out:?}"
        );
        assert_eq!(
            step(&mut daemon, ask()),
            Vec::new(),
            "one refresh until the fresh credential arms the next"
        );
        assert_eq!(
            step(&mut daemon, Input::Timer { id: refresh }),
            Vec::new(),
            "a timer that rang before it was cancelled refreshes nothing"
        );
    }
}
