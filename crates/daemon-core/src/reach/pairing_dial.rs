//! Reaching a host to pair with: which addresses, in what order, how long
//! each is given, and the forwarders after them.
//!
//! A host to pair with has no key this host pins yet, so its dial is not a
//! paired host's attempt: the driver makes it, a connection per address,
//! on the plan the core gives. Every address is dialled at once, those the
//! local network found the host at first and then those the request named.
//! An earlier address that answers is taken over a later one while it is
//! within its short try, so the address discovery vouches for wins a race
//! it would lose only by a hair; a later answer is taken once every earlier
//! address has failed or been silent that long. Each address has the
//! attempt's whole deadline, which outlasts QUIC sending a lost first
//! packet twice more. With no address answering, the forwarders that claim
//! the host are tried in turn, paired hosts before the relay, since they
//! forward on any plan and the relay only on a paid one.

use std::net::SocketAddr;

use serde::{Deserialize, Serialize};

use crate::Daemon;
use crate::ids::HostId;

/// How long an earlier address is preferred over a later one that answered.
pub const PAIRING_SHORT_TRY_MS: u64 = 2_000;
/// How long a pairing dial to one address may take: past QUIC's resends of
/// a lost first packet at about one second and then three.
pub const PAIRING_DIAL_MS: u64 = 5_000;

/// How to reach a host to pair with.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingDial {
    /// The addresses, in the order they are preferred, each with whether
    /// the local network found the host there.
    pub addrs: Vec<(SocketAddr, bool)>,
    /// How long an earlier address is preferred over a later answer.
    pub short_try_ms: u64,
    /// How long each address's dial may take.
    pub deadline_ms: u64,
    /// The hosts claiming to reach it, in the order they are tried, each
    /// with whether it is the relay.
    pub forwarders: Vec<(HostId, bool)>,
}

impl Daemon {
    /// How to reach `host` to pair with it, the request naming `requested`
    /// addresses; a request naming no host has only those.
    pub fn pairing_dial(&self, host: Option<HostId>, requested: &[SocketAddr]) -> PairingDial {
        let mut addrs: Vec<(SocketAddr, bool)> = host
            .map(|host| self.host_set.found_at(host))
            .unwrap_or_default()
            .into_iter()
            .map(|addr| (addr, true))
            .collect();
        for addr in requested {
            if !addrs.iter().any(|(known, _)| known == addr) {
                addrs.push((*addr, false));
            }
        }
        let links = self.links();
        let forwarders = host
            .map(|host| {
                links
                    .routes_to(host)
                    .into_iter()
                    .filter_map(|route| match route {
                        crate::presence::Route::Via(forwarder) => {
                            Some((forwarder, links.is_relay(forwarder)))
                        }
                        crate::presence::Route::Direct(_) => None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        PairingDial {
            addrs,
            short_try_ms: PAIRING_SHORT_TRY_MS,
            deadline_ms: PAIRING_DIAL_MS,
            forwarders,
        }
    }
}
