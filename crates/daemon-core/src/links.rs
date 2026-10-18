//! Links to peers and the channels over them, as the core knows them.
//!
//! The driver owns every connection and stream. It reports a link when it
//! comes up and when it goes, and hands over the link control frames that
//! say which hosts the peer reaches: its hello and each neighbor coming up
//! or going down. A channel is opened only at the core's word, with the key
//! the core gives for its handshake, and the driver reports how it went.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use wire::message::Body;
use wire::{HelloAck, hello_ack};

use crate::Daemon;
use crate::ids::{ChannelId, HostId, LinkId};
use crate::io::Output;
use crate::payload::{CarrierKind, ChannelClass, ChannelError, LinkFrame, PublicKey};

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
    /// The hosts the peer says it reaches, other than this host and itself.
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
}

impl Daemon {
    /// The links and channels as the core knows them.
    pub fn links(&self) -> &LinkRegistry {
        &self.routing.links
    }

    pub(crate) fn link_up(&mut self, link: LinkId, peer: HostId, kind: CarrierKind) {
        self.routing.links.links.insert(
            link,
            Link {
                peer,
                kind,
                claims: BTreeSet::new(),
            },
        );
    }

    /// The link is gone, and with it every channel it carried: the driver
    /// ends their streams with the connection.
    pub(crate) fn link_down(&mut self, link: LinkId) {
        let registry = &mut self.routing.links;
        registry.links.remove(&link);
        registry.channels.retain(|_, channel| channel.link != link);
    }

    /// A link control frame from the peer. Its hello carries the hosts it
    /// reaches as the link came up; each neighbor frame after says one
    /// came or went. Anything else on the control stream is the driver's.
    pub(crate) fn link_control(&mut self, link: LinkId, frame: LinkFrame) {
        let own = HostId::from_slice(self.store.own_host());
        let Some(entry) = self.routing.links.links.get_mut(&link) else {
            return;
        };
        let peer = entry.peer;
        let other = |host: &HostId| Some(*host) != own && *host != peer;
        match frame.body {
            Some(Body::Hello(hello)) => {
                entry.claims = hosts(&hello.neighbors).filter(other).collect();
            }
            Some(Body::HelloAck(HelloAck {
                outcome: Some(hello_ack::Outcome::Accepted(accepted)),
            })) => {
                entry.claims = hosts(&accepted.neighbors).filter(other).collect();
            }
            Some(Body::NeighborUp(up)) => {
                if let Some(host) = up
                    .host
                    .as_ref()
                    .and_then(|host| HostId::from_slice(&host.host_id))
                    .filter(other)
                {
                    entry.claims.insert(host);
                }
            }
            Some(Body::NeighborDown(down)) => {
                if let Some(host) = HostId::from_slice(&down.host_id) {
                    entry.claims.remove(&host);
                }
            }
            Some(Body::HelloAck(_) | Body::Reauth(_) | Body::LinkClose(_)) | None => {}
        }
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
        let registry = &mut self.routing.links;
        if !registry.links.contains_key(&link) {
            out.push(refused(ChannelError::NoLink));
            return;
        }
        let Some(pin) = pin else {
            out.push(refused(ChannelError::Unpinned));
            return;
        };
        registry.next_channel += 1;
        let channel = ChannelId(registry.next_channel);
        registry.channels.insert(
            channel,
            Channel {
                link,
                peer,
                class,
                open: false,
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
        if let Some(channel) = self.routing.links.channels.get_mut(&channel) {
            channel.open = true;
        }
    }

    pub(crate) fn channel_closed(&mut self, channel: ChannelId) {
        self.routing.links.channels.remove(&channel);
    }
}

/// The ids of the hosts a frame lists; one that is not an id names no host.
fn hosts(hosts: &[wire::Host]) -> impl Iterator<Item = HostId> + '_ {
    hosts
        .iter()
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
}
