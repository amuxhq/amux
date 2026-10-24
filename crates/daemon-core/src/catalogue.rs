//! The host catalogue: what each host a link or a neighbor's claim makes
//! present said of itself in its handshake, which neighbors claim it, the
//! hosts whose trust is being replaced, and the hosts that said they no
//! longer trust this one.
//!
//! The routes these hosts make are [`crate::links`]'s and how they read is
//! [`crate::presence`]'s; the catalogue keeps what a client is told of a
//! host beside that: its version and what it runs.
//!
//! Untrusted hosts are held up to [`HOST_CAP`]: a relay may claim every
//! host of its account, and nothing bounds an account. A new claim past the
//! cap evicts the oldest claimed host this profile does not trust and holds
//! no link to, or is not held. A direct link is never refused here: a link
//! is admitted by certificate before the core hears of it, so its host is
//! one this host dialed or paired with, and its entry counts toward the
//! cap without being evictable.
//!
//! A host said it no longer trusts this one by closing a link saying so or
//! by refusing this host's certificate on a channel. The mark comes off
//! when the host takes this host again — a link to it comes up, or a
//! channel begun after the mark has its certificate taken — or when a
//! change of this profile's trust in it commits; then a trusted host is
//! tried again at once, rather than after a backoff grown on refusals.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::Daemon;
use crate::ids::{ChannelId, HostId, LinkId};
use crate::io::Output;
use crate::payload::CarrierKind;
use crate::trust::{Reachability, TrustChange};

/// How many hosts this profile does not trust it holds descriptions of.
pub const HOST_CAP: usize = 1000;

/// What a host said of itself in a handshake or a neighbor's claim.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Said {
    pub version: String,
    pub cloud_relay: bool,
    pub kinds: Vec<i32>,
    pub platform: wire::Platform,
    pub signed_in: Option<bool>,
}

impl Said {
    pub fn from_wire(host: &wire::Host) -> Self {
        let capabilities = host.capabilities.clone().unwrap_or_default();
        Self {
            version: host.version.clone(),
            cloud_relay: capabilities.cloud_relay,
            kinds: capabilities.kinds,
            platform: host.platform(),
            signed_in: host.account_signed_in,
        }
    }

    pub fn capabilities(&self) -> wire::Capabilities {
        wire::Capabilities {
            cloud_relay: self.cloud_relay,
            kinds: self.kinds.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Direct {
    said: Said,
    links: BTreeSet<LinkId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Claimed {
    said: Said,
    /// The neighbors claiming it, in claim order.
    relays: Vec<HostId>,
    /// When it was first claimed: the cap evicts the oldest.
    order: u64,
}

/// What updating the catalogue came to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Inserted,
    AlreadyKnown,
    /// Past the cap with nothing to evict: not held.
    RejectedByCap,
    /// The host's trust is being replaced: nothing about it is held until
    /// the replacement ends.
    Replacing,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Catalogue {
    directs: BTreeMap<HostId, Direct>,
    claims: BTreeMap<HostId, Claimed>,
    replacing: BTreeSet<HostId>,
    next_claim: u64,
    /// The hosts that said they no longer trust this one, each with the
    /// count of notes at which it said so.
    revoked: BTreeMap<HostId, u64>,
    notes: u64,
}

impl Catalogue {
    fn is_present(&self, host: HostId) -> bool {
        self.directs.contains_key(&host) || self.claims.contains_key(&host)
    }

    /// What `host` said of itself while a link or a claim holds it.
    pub fn said(&self, host: HostId) -> Option<&Said> {
        self.directs
            .get(&host)
            .map(|direct| &direct.said)
            .or_else(|| self.claims.get(&host).map(|claimed| &claimed.said))
    }

    /// The neighbors claiming `host`, in claim order.
    pub fn relays_to(&self, host: HostId) -> Vec<HostId> {
        self.claims
            .get(&host)
            .map(|claimed| claimed.relays.clone())
            .unwrap_or_default()
    }

    /// Whether `host` said it no longer trusts this one, and has not taken
    /// it back.
    pub fn revoked(&self, host: HostId) -> bool {
        self.revoked.contains_key(&host)
    }

    /// Where the notes stand: a channel begun now is told apart from one
    /// begun before a later note.
    pub(crate) fn mark(&self) -> u64 {
        self.notes
    }

    pub(crate) fn note_revoked(&mut self, host: HostId) {
        self.notes += 1;
        self.revoked.insert(host, self.notes);
    }

    /// Takes the mark off; true when there was one.
    pub(crate) fn clear_revoked(&mut self, host: HostId) -> bool {
        self.revoked.remove(&host).is_some()
    }

    /// `host` took this host's certificate on a channel begun at `begun`:
    /// the mark comes off unless it was noted since. A channel begun before
    /// may carry the host's answer from while it still trusted this one,
    /// read after its refusal arrived on another path.
    pub(crate) fn clear_answered(&mut self, host: HostId, begun: u64) -> bool {
        let before = self.revoked.get(&host).is_some_and(|&at| at <= begun);
        if before {
            self.revoked.remove(&host);
        }
        before
    }

    fn direct_up(
        &mut self,
        host: HostId,
        said: Said,
        link: LinkId,
        trusted: &BTreeSet<HostId>,
    ) -> Outcome {
        if self.replacing.contains(&host) {
            return Outcome::Replacing;
        }
        // Never refused: see the module's word on the cap. Evicting a claim
        // keeps the count of untrusted hosts held at the cap.
        if !self.is_present(host) && !trusted.contains(&host) && self.untrusted(trusted) >= HOST_CAP
        {
            self.evict(trusted);
        }
        let direct = self.directs.entry(host).or_insert_with(|| Direct {
            said: said.clone(),
            links: BTreeSet::new(),
        });
        direct.said = said;
        if direct.links.insert(link) {
            Outcome::Inserted
        } else {
            Outcome::AlreadyKnown
        }
    }

    fn claim_up(
        &mut self,
        relay: HostId,
        host: HostId,
        said: Said,
        trusted: &BTreeSet<HostId>,
    ) -> Outcome {
        if self.replacing.contains(&host) {
            return Outcome::Replacing;
        }
        if !self.is_present(host)
            && !trusted.contains(&host)
            && self.untrusted(trusted) >= HOST_CAP
            && self.evict(trusted).is_none()
        {
            return Outcome::RejectedByCap;
        }
        let order = self.next_claim;
        let claimed = self.claims.entry(host).or_insert_with(|| Claimed {
            said: said.clone(),
            relays: Vec::new(),
            order,
        });
        if claimed.order == order {
            self.next_claim += 1;
        }
        claimed.said = said;
        if claimed.relays.contains(&relay) {
            return Outcome::AlreadyKnown;
        }
        claimed.relays.push(relay);
        Outcome::Inserted
    }

    fn claim_down(&mut self, relay: HostId, host: HostId) {
        let Some(claimed) = self.claims.get_mut(&host) else {
            return;
        };
        claimed.relays.retain(|known| *known != relay);
        if claimed.relays.is_empty() {
            self.claims.remove(&host);
        }
    }

    fn direct_down(&mut self, host: HostId, link: LinkId) {
        let Some(direct) = self.directs.get_mut(&host) else {
            return;
        };
        direct.links.remove(&link);
        if direct.links.is_empty() {
            self.directs.remove(&host);
        }
    }

    /// Every claim `relay` made: its last link went.
    fn relay_gone(&mut self, relay: HostId) {
        let claimed: Vec<HostId> = self
            .claims
            .iter()
            .filter(|(_, claimed)| claimed.relays.contains(&relay))
            .map(|(host, _)| *host)
            .collect();
        for host in claimed {
            self.claim_down(relay, host);
        }
    }

    fn untrusted(&self, trusted: &BTreeSet<HostId>) -> usize {
        self.directs
            .keys()
            .chain(
                self.claims
                    .keys()
                    .filter(|host| !self.directs.contains_key(host)),
            )
            .filter(|host| !trusted.contains(host))
            .count()
    }

    fn evict(&mut self, trusted: &BTreeSet<HostId>) -> Option<HostId> {
        let oldest = self
            .claims
            .iter()
            .filter(|(host, _)| !trusted.contains(host) && !self.directs.contains_key(host))
            .min_by_key(|(_, claimed)| claimed.order)
            .map(|(host, _)| *host)?;
        self.claims.remove(&oldest);
        Some(oldest)
    }
}

impl Daemon {
    fn trusted_hosts(&self) -> BTreeSet<HostId> {
        self.trust.store().paired_hosts().into_iter().collect()
    }

    /// What a link's handshake or a neighbor frame says of the hosts it
    /// makes present: the link's peer, said by itself, and the hosts it
    /// claims, said by it.
    pub(crate) fn catalogue_frame(
        &mut self,
        link: LinkId,
        peer: HostId,
        body: &wire::pb::message::Body,
        out: &mut Vec<Output>,
    ) {
        use wire::pb::message::Body;
        use wire::pb::{HelloAck, hello_ack};

        let own = HostId::from_slice(self.store.own_host());
        let other = |host: HostId| Some(host) != own && host != peer;
        let (said, claimed): (Option<&wire::Host>, Vec<&wire::Host>) = match body {
            Body::Hello(hello) => (hello.host.as_ref(), hello.neighbors.iter().collect()),
            Body::HelloAck(HelloAck {
                outcome: Some(hello_ack::Outcome::Accepted(accepted)),
            }) => (accepted.host.as_ref(), accepted.neighbors.iter().collect()),
            Body::NeighborUp(up) => (None, up.host.iter().collect()),
            Body::NeighborDown(down) => {
                if let Some(host) = HostId::from_slice(&down.host_id) {
                    self.routing.catalogue.claim_down(peer, host);
                    self.touched.host_set = true;
                }
                return;
            }
            _ => return,
        };
        let trusted = self.trusted_hosts();
        if let Some(said) = said.filter(|said| said.host_id == peer.0) {
            self.remember_said(peer, said, out);
            let said = Said::from_wire(said);
            if self.routing.catalogue.direct_up(peer, said, link, &trusted) != Outcome::Replacing
                && self.routing.catalogue.clear_revoked(peer)
            {
                self.revocation_cleared(peer, out);
            }
        }
        let relayed = self
            .routing
            .links
            .link(link)
            .is_some_and(|up| up.kind == CarrierKind::Relay);
        for host in claimed {
            let Some(id) = HostId::from_slice(&host.host_id).filter(|id| other(*id)) else {
                continue;
            };
            self.remember_said(id, host, out);
            if relayed {
                self.remember_on_the_account(id, out);
            }
            self.routing
                .catalogue
                .claim_up(peer, id, Said::from_wire(host), &trusted);
        }
        self.touched.host_set = true;
    }

    /// The link went: its peer's entry loses it, and a peer no link of
    /// which is left claims nothing any more.
    pub(crate) fn catalogue_link_down(&mut self, link: LinkId, peer: HostId) {
        self.routing.catalogue.direct_down(peer, link);
        if !self.routing.links.links().any(|(_, up)| up.peer == peer) {
            self.routing.catalogue.relay_gone(peer);
        }
        self.touched.host_set = true;
    }

    /// Keeps that the relay claimed a trusted host, so the next launch's
    /// first flight sends through the relay too, however the host paired.
    fn remember_on_the_account(&mut self, host: HostId, out: &mut Vec<Output>) {
        let uuid = Uuid::from_bytes(host.0);
        let known = self
            .trust
            .store()
            .entry(uuid)
            .is_none_or(|entry| entry.reachabilities.contains(&Reachability::Cloud));
        if !known {
            self.change_trust(TrustChange::OnTheAccount { host: uuid }, None, out);
        }
    }

    /// Keeps what a trusted host said of itself, when it moved.
    fn remember_said(&mut self, host: HostId, said: &wire::Host, out: &mut Vec<Output>) {
        let uuid = Uuid::from_bytes(host.0);
        let moved = self.trust.store().entry(uuid).is_some_and(|entry| {
            entry.signed_in != said.account_signed_in
                || (said.platform() != wire::Platform::Unspecified
                    && entry.platform != said.platform())
        });
        if moved {
            self.change_trust(
                TrustChange::Remember {
                    host: uuid,
                    signed_in: said.account_signed_in,
                    platform: said.platform(),
                },
                None,
                out,
            );
        }
    }

    /// The driver found that `host` no longer trusts this one.
    pub(crate) fn peer_revoked(&mut self, host: HostId) {
        self.routing.catalogue.note_revoked(host);
        self.touched.host_set = true;
    }

    /// The far host took this host's certificate on `channel`.
    pub(crate) fn channel_answered(&mut self, channel: ChannelId, out: &mut Vec<Output>) {
        let Some((peer, begun)) = self
            .routing
            .links
            .channel(channel)
            .map(|channel| (channel.peer, channel.begun))
        else {
            return;
        };
        if self.routing.catalogue.clear_answered(peer, begun) {
            self.revocation_cleared(peer, out);
        }
    }

    /// A change of this profile's trust in `host` committed: whatever the
    /// host said of trusting this one is out of date.
    pub(crate) fn trust_committed(&mut self, host: HostId, out: &mut Vec<Output>) {
        if self.routing.catalogue.clear_revoked(host) {
            self.revocation_cleared(host, out);
        }
    }

    /// The host's key is being replaced: nothing about it is held, and no
    /// link or claim makes it present, until the replacement ends.
    pub(crate) fn begin_replacement(&mut self, host: HostId) {
        let catalogue = &mut self.routing.catalogue;
        catalogue.replacing.insert(host);
        catalogue.directs.remove(&host);
        catalogue.claims.remove(&host);
        catalogue.revoked.remove(&host);
        self.touched.host_set = true;
    }

    pub(crate) fn finish_replacement(&mut self, host: HostId) {
        self.routing.catalogue.replacing.remove(&host);
    }

    /// The mark came off: a trusted host is dialled and its sources open
    /// again at once.
    fn revocation_cleared(&mut self, host: HostId, out: &mut Vec<Output>) {
        self.touched.host_set = true;
        if self.sources.trusts(host) {
            self.try_again(host, out);
            self.sources_try_again(host, out);
        }
    }

    /// Whether the core asks for no dial to `host`: one this profile does
    /// not trust, or one that said it no longer trusts this host. Pairing
    /// again opens a link of its own.
    pub(crate) fn dial_barred(&self, host: HostId) -> bool {
        (self.sources.knows_trust() && !self.sources.trusts(host))
            || self.routing.catalogue.revoked(host)
    }
}

#[cfg(test)]
mod tests;
