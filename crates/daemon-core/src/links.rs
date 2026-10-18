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

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use wire::message::Body;
use wire::{HelloAck, hello_ack};

use crate::Daemon;
use crate::ids::{ChannelId, HostId, LinkId, TimerId};
use crate::io::{Job, Output};
use crate::payload::{
    Advertisement, CarrierKind, ChannelClass, ChannelError, LinkFrame, PublicKey,
};
use crate::presence::Route;

/// How long before the relay credential expires its refresh comes.
pub const REFRESH_BEFORE_EXPIRY_MS: u64 = 5 * 60 * 1000;

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
    /// The route it opens on: the peer's own link, or a link to a host
    /// that claims to reach the peer.
    pub route: Route,
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
    /// links, newest first, since a second link to a peer is a crossed dial
    /// whose newer link is the one both sides keep; then each peer that
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

    pub(crate) fn link_up(
        &mut self,
        link: LinkId,
        peer: HostId,
        kind: CarrierKind,
        out: &mut Vec<Output>,
    ) {
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

    /// The relay link presented a fresh credential: its refresh is armed.
    /// On the free plan it comes on an interval, so a purchase reaches the
    /// link promptly; otherwise shortly before the credential expires.
    pub(crate) fn credential(
        &mut self,
        link: LinkId,
        expires_at_ms: u64,
        free_every_ms: Option<u64>,
        out: &mut Vec<Output>,
    ) {
        let at_ms = match free_every_ms {
            Some(every) => self.timers.now_ms.saturating_add(every),
            None => expires_at_ms.saturating_sub(REFRESH_BEFORE_EXPIRY_MS),
        };
        self.arm(TimerId::Refresh(link), at_ms, out);
    }

    /// The relay link's credential is due for refresh.
    pub(crate) fn refresh_due(&mut self, link: LinkId, out: &mut Vec<Output>) {
        self.timers.armed.remove(&TimerId::Refresh(link));
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
        let peer = entry.peer;
        if let Some(body) = &frame.body {
            self.catalogue_frame(link, peer, body, out);
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
            Some(Body::HelloAck(_) | Body::Reauth(_) | Body::LinkClose(_)) | None => return,
        }
        let before = self.routes_before(entry.claims.union(&claims).copied().collect::<Vec<_>>());
        if let Some(entry) = self.routing.links.links.get_mut(&link) {
            entry.claims = claims;
        }
        self.routes_moved(before, out);
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
        let Some(route) = self.routing.links.route_over(link, peer) else {
            out.push(refused(ChannelError::NoLink));
            return;
        };
        let Some(pin) = pin else {
            out.push(refused(ChannelError::Unpinned));
            // As a handshake that found no key would: the attempt failed.
            self.concluded(peer, route, Err(&ChannelError::Unpinned), out);
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
                route,
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
    /// pin: an attempt on its route that reached the host. One whose link
    /// went meanwhile is already forgotten.
    pub(crate) fn channel_opened(&mut self, channel: ChannelId, out: &mut Vec<Output>) {
        let Some(opened) = self.routing.links.channels.get_mut(&channel) else {
            return;
        };
        opened.open = true;
        let (peer, route) = (opened.peer, opened.route);
        self.concluded(peer, route, Ok(()), out);
    }

    /// A channel closed: one that never opened is an attempt on its route
    /// that failed, and one that did says nothing of the host by ending.
    pub(crate) fn channel_closed(
        &mut self,
        channel: ChannelId,
        reason: &ChannelError,
        out: &mut Vec<Output>,
    ) {
        let Some(closed) = self.routing.links.channels.remove(&channel) else {
            return;
        };
        if !closed.open {
            self.concluded(closed.peer, closed.route, Err(reason), out);
        }
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

    #[test]
    fn the_relay_credential_is_refreshed_on_the_free_interval_or_before_it_expires() {
        let mut daemon = daemon();
        up(&mut daemon);
        step(&mut daemon, Input::Tick { now_ms: 1_000 });
        let credential = |free_every_ms| Input::Credential {
            link: LINK,
            expires_at_ms: 3_600_000,
            free_every_ms,
        };
        let refresh = TimerId::Refresh(LINK);
        assert_eq!(
            step(&mut daemon, credential(Some(60_000))),
            vec![Output::ArmTimer {
                id: refresh,
                at_ms: 61_000
            }],
            "the free plan's interval, from now"
        );
        assert_eq!(
            step(&mut daemon, credential(None)),
            vec![Output::ArmTimer {
                id: refresh,
                at_ms: 3_600_000 - REFRESH_BEFORE_EXPIRY_MS
            }],
            "a fresh credential moves the refresh to before it expires"
        );
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
        step(&mut daemon, credential(None));
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
}
