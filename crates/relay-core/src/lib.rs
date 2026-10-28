//! The relay's admission and forwarding decisions as one function of its
//! state and an input.
//!
//! The driver verifies a token's signature before the core sees a link, and
//! reads only the header of a forwarded packet: [`Relay::step`] judges the
//! claims, the tier and the adjacency, and answers with a pair of links to
//! forward between or a refusal. The driver forwards from its table of pairs
//! without asking again until the core removes the pair; no byte of a packet
//! enters the core. Every step ends by checking the cheap invariants, and a
//! violation is reported as [`Output::Dump`]. See the principles in
//! `docs/ARCHITECTURE.md`.
//!
//! A pair is installed only between two links that are up on one account,
//! both on a paid plan or both admitted by a pinned key, and it lasts until
//! a link of it goes down, lapses to the free plan or its token expires.
//! A daemon forwarding between its own paired hosts is a relay with one
//! account whose links are all pinned, so its driver asks this core too.
//!
//! The core also keeps the adjacency discipline every host's links follow:
//! it says which of two links between the same pair of processes both
//! sides keep, and it is the only source of what a link is told of its
//! neighbors. A link is told when another host of its account gains its
//! first link here or loses its last, and a new link is told the
//! difference between the neighbors its handshake listed and the ones
//! there are, so a link's picture of its neighbors is never anything but
//! this host's own links.

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::hash::{Hash, Hasher};
use std::time::Instant;

use daemon_core::payload::ForwardRefusal;
use daemon_core::{Check, DumpReason, HostId, LinkId, StepBudget, Violation};

/// An account, by the id its tokens carry. A daemon's links are all one
/// account's.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct UserId(pub [u8; 16]);

/// What an account's plan lets its hosts do through the relay, as its
/// connection tokens name it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    /// Hosts reach the relay, but no packet is forwarded between them.
    Free,
    /// Packets between the account's hosts are forwarded.
    Pro,
}

/// How long a refusal to one destination stays unrepeated on a sender's
/// link: a host keeps sending until it is told, and it is told again after
/// this long if it still sends.
pub const REFUSAL_REPEAT_MS: u64 = 5_000;

/// Which of the two links a crossed dial makes both sides keep: each pair
/// of hosts agrees on it from their ids alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DialOrder {
    /// The link both sides keep when both directions connect.
    Preferred,
    /// The link kept only while it is the one there is.
    Fallback,
}

impl DialOrder {
    /// The order of a link `local` holds to `peer`, dialled by `local`
    /// when `dialled` and by `peer` otherwise: the host with the lower id
    /// dials the preferred link.
    pub fn for_hosts(local: HostId, peer: HostId, dialled: bool) -> Self {
        if (local < peer) == dialled {
            Self::Preferred
        } else {
            Self::Fallback
        }
    }
}

/// What a direct link between two hosts' processes says of how it was
/// made, for settling which link of the pair stays.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Direct {
    pub order: DialOrder,
    /// The life of the peer's process the link was opened by.
    pub instance: [u8; 16],
}

/// Everything that can happen to the relay, decoded by the driver.
#[derive(Clone, Debug, PartialEq)]
pub enum Input {
    /// A link authenticated: by a token naming its account and plan, or by
    /// a pinned key, which no plan governs (`tier: None`). `direct` is set
    /// for a host's own network link to a peer device, and `advertised` is
    /// the neighbors its handshake listed to the peer.
    LinkUp {
        link: LinkId,
        user: UserId,
        host: HostId,
        tier: Option<Tier>,
        direct: Option<Direct>,
        advertised: Vec<HostId>,
    },
    /// The link is gone. Its pairs end with it, or move to another link
    /// its host still holds here.
    LinkDown { link: LinkId },
    /// A packet arrived on `from` naming `dst`. The driver asks once per
    /// pair it has not been told about, and holds or drops the packet
    /// itself until it is.
    Packet { from: LinkId, dst: HostId },
    /// A token-authenticated link presented a fresh token with this plan.
    TierChanged { link: LinkId, tier: Tier },
    /// A token-authenticated link's token expired before a fresh one came:
    /// nothing is forwarded for it until one does.
    Expired { link: LinkId },
    /// The clock, as an input: the refusal repeat is measured by it.
    Tick { now_ms: u64 },
}

/// Everything the relay wants done.
#[derive(Clone, Debug, PartialEq)]
pub enum Output {
    /// Forward packets between `a`, the link that sent the packet asked
    /// about, and `b` until [`Output::Unforward`] names the pair.
    Forward { a: LinkId, b: LinkId },
    /// Stop forwarding between `a` and `b`. With `tell_both`, every end
    /// still up is told on its control link that the other end's host is
    /// refused for `reason`; without it the pair moved to another link of
    /// the same host, which the next output installs, and nobody is told.
    Unforward {
        a: LinkId,
        b: LinkId,
        reason: ForwardRefusal,
        tell_both: bool,
    },
    /// Tell `link` on its control link that packets for `dst` are refused.
    Refuse {
        link: LinkId,
        dst: HostId,
        reason: ForwardRefusal,
    },
    /// Do not keep the link that just came up: the same process of its
    /// peer holds a link that wins against it. The core never held it.
    RefuseLink { link: LinkId },
    /// Close the link as superseded: a link that came up replaces it. The
    /// core has forgotten it and its pairs, which move to the link
    /// replacing it, and tells no neighbor, since its host is still here.
    Displace { link: LinkId },
    /// Tell `to` that `host` now holds a link here (`up`) or holds none.
    Neighbor { to: LinkId, host: HostId, up: bool },
    /// Take a dump: the core saw something it must not carry on from.
    Dump { reason: DumpReason },
}

/// The invariants [`Relay::check_step_invariants`] asserts, as the
/// sentences a violation names.
pub const TOLD_ONLY_OF_ITS_NEIGHBORS: Check = Check {
    id: "relay.told_only_of_its_neighbors",
    sentence: "a link that is up is told only of other hosts on its own account",
};
pub const FORWARDS_ONLY_PAID_PAIRS: Check = Check {
    id: "relay.forwards_only_paid_pairs",
    sentence: "packets flow between two links only while both are up, on one account and paid, or both pinned",
};
pub const NOTHING_BEFORE_ADMISSION: Check = Check {
    id: "relay.nothing_before_admission",
    sentence: "nothing is forwarded for a link not yet admitted, held packets included",
};
pub const LAPSE_TELLS_BOTH: Check = Check {
    id: "relay.lapse_tells_both",
    sentence: "a lapse or expiry removes every pair through the link and tells both ends",
};

/// Every check the relay core makes at the end of a step.
pub const CHECKS: &[Check] = &[
    TOLD_ONLY_OF_ITS_NEIGHBORS,
    FORWARDS_ONLY_PAID_PAIRS,
    NOTHING_BEFORE_ADMISSION,
    LAPSE_TELLS_BOTH,
];

/// One link that is up.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct RelayLink {
    user: UserId,
    host: HostId,
    tier: Option<Tier>,
    direct: Option<Direct>,
    /// Its token expired and no fresh one has come.
    expired: bool,
}

impl RelayLink {
    /// What it may be paired as: `Some(true)` paid, `Some(false)` pinned,
    /// `None` when nothing may be forwarded for it.
    fn forwards_as(&self) -> Option<bool> {
        match (self.tier, self.expired) {
            (None, _) => Some(false),
            (Some(Tier::Pro), false) => Some(true),
            (Some(Tier::Free), _) | (Some(Tier::Pro), true) => None,
        }
    }
}

/// Which links each account's packets may be forwarded between: per
/// account, per host, the links that host holds here.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Adjacency {
    hosts: BTreeMap<UserId, BTreeMap<HostId, BTreeSet<LinkId>>>,
}

impl Adjacency {
    fn add(&mut self, user: UserId, host: HostId, link: LinkId) {
        self.hosts
            .entry(user)
            .or_default()
            .entry(host)
            .or_default()
            .insert(link);
    }

    fn remove(&mut self, user: UserId, host: HostId, link: LinkId) {
        let Some(hosts) = self.hosts.get_mut(&user) else {
            return;
        };
        if let Some(links) = hosts.get_mut(&host) {
            links.remove(&link);
            if links.is_empty() {
                hosts.remove(&host);
            }
        }
        if hosts.is_empty() {
            self.hosts.remove(&user);
        }
    }

    /// The link packets for `host` on `user`'s account are forwarded onto:
    /// its newest, since a second link to a host replaces the first.
    fn route(&self, user: UserId, host: HostId) -> Option<LinkId> {
        self.hosts.get(&user)?.get(&host)?.last().copied()
    }

    fn holds(&self, user: UserId, host: HostId) -> bool {
        self.hosts
            .get(&user)
            .is_some_and(|hosts| hosts.contains_key(&host))
    }

    fn links_of(&self, user: UserId, host: HostId) -> Vec<LinkId> {
        self.hosts
            .get(&user)
            .and_then(|hosts| hosts.get(&host))
            .map(|links| links.iter().copied().collect())
            .unwrap_or_default()
    }

    /// Every link on `user`'s account but `host`'s, with its host.
    fn others(&self, user: UserId, host: HostId) -> Vec<(LinkId, HostId)> {
        self.hosts
            .get(&user)
            .into_iter()
            .flatten()
            .filter(|(other, _)| **other != host)
            .flat_map(|(other, links)| links.iter().map(|link| (*link, *other)))
            .collect()
    }

    /// The hosts on `user`'s account holding a link here.
    fn neighbors(&self, user: UserId) -> Vec<HostId> {
        self.hosts
            .get(&user)
            .map(|hosts| hosts.keys().copied().collect())
            .unwrap_or_default()
    }
}

/// What a link coming up says of itself.
#[derive(Clone, Copy, Debug)]
struct Arrival {
    user: UserId,
    host: HostId,
    tier: Option<Tier>,
    direct: Option<Direct>,
}

/// What the current step touched; cleared as each step begins.
#[derive(Clone, Debug, Default)]
struct Touched {
    links: Vec<LinkId>,
    /// Each neighbor told this step: the link told, and the host it was
    /// told of with that host's account.
    told: Vec<Told>,
    /// Each pair installed this step.
    forwarded: Vec<(LinkId, LinkId)>,
    /// Each link that lapsed or expired this step, with every pair removed
    /// through it and whether both ends were told.
    lapsed: Vec<(LinkId, Vec<(LinkId, bool)>)>,
}

#[derive(Clone, Copy, Debug)]
struct Told {
    to: LinkId,
    user: UserId,
    host: HostId,
}

/// The relay's whole state: the links up, which hosts of one account may
/// reach each other, the pairs forwarded between, the refusals recently
/// told, and its step budget. Cloned and printed by a driver keeping its
/// recent history for a dump.
#[derive(Clone, Debug)]
pub struct Relay {
    /// The relay's own host: a packet addressed to it has nothing to reach.
    own: HostId,
    links: BTreeMap<LinkId, RelayLink>,
    adjacency: Adjacency,
    /// The pairs forwarded between, by each link of them: symmetric.
    pairs: BTreeMap<LinkId, BTreeSet<LinkId>>,
    /// When each sender's link was last told a destination is refused.
    refused: BTreeMap<(LinkId, HostId), u64>,
    now_ms: u64,
    touched: Touched,
    budget: StepBudget,
}

/// Two relays are equal when they hold the same links, pairs and refusals
/// at the same time: what the last step touched and the step budget are not
/// the relay's state, so a model exploring the core sees one state where the
/// same decisions were reached by different steps.
impl PartialEq for Relay {
    fn eq(&self, other: &Self) -> bool {
        self.own == other.own
            && self.links == other.links
            && self.adjacency == other.adjacency
            && self.pairs == other.pairs
            && self.refused == other.refused
            && self.now_ms == other.now_ms
    }
}

impl Eq for Relay {}

impl Hash for Relay {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.own.hash(state);
        self.links.hash(state);
        self.adjacency.hash(state);
        self.pairs.hash(state);
        self.refused.hash(state);
        self.now_ms.hash(state);
    }
}

impl Relay {
    /// Tighter than the daemon's: admission must never queue.
    pub const BUDGET: StepBudget = StepBudget { micros: 1_000 };

    /// A relay with no link up, whose own host is `own`.
    pub fn new(own: HostId) -> Self {
        Self::with_budget(own, Self::BUDGET)
    }

    /// A relay with another step budget: [`StepBudget::UNLIMITED`] for a
    /// run that must be a function of its inputs alone.
    pub fn with_budget(own: HostId, budget: StepBudget) -> Self {
        Self {
            own,
            links: BTreeMap::new(),
            adjacency: Adjacency::default(),
            pairs: BTreeMap::new(),
            refused: BTreeMap::new(),
            now_ms: 0,
            touched: Touched::default(),
            budget,
        }
    }

    /// One input in, outputs appended. Ends by checking the cheap
    /// invariants; a violation appends [`Output::Dump`], as does a step
    /// over its budget.
    pub fn step(&mut self, input: Input, out: &mut Vec<Output>) {
        let started = Instant::now();
        self.touched = Touched::default();
        match input {
            Input::LinkUp {
                link,
                user,
                host,
                tier,
                direct,
                advertised,
            } => self.link_up(
                link,
                Arrival {
                    user,
                    host,
                    tier,
                    direct,
                },
                &advertised,
                out,
            ),
            Input::LinkDown { link } => self.link_down(link, out),
            Input::Packet { from, dst } => self.packet(from, dst, out),
            Input::TierChanged { link, tier } => self.tier_changed(link, tier, out),
            Input::Expired { link } => self.expired(link, out),
            Input::Tick { now_ms } => self.now_ms = self.now_ms.max(now_ms),
        }
        if let Err(violation) = self.check_step_invariants() {
            out.push(Output::Dump {
                reason: DumpReason::Violation(violation),
            });
        }
        let micros = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
        if micros > self.budget.micros {
            out.push(Output::Dump {
                reason: DumpReason::OverBudget {
                    micros,
                    budget: self.budget,
                },
            });
        }
    }

    /// A link came up. Of two direct links between the same two processes
    /// the preferred one stays, whichever came first; a link from a later
    /// life of the peer's process replaces the earlier life's links, which
    /// are dead whether or not they have closed yet.
    fn link_up(
        &mut self,
        link: LinkId,
        arrival: Arrival,
        advertised: &[HostId],
        out: &mut Vec<Output>,
    ) {
        let Arrival {
            user,
            host,
            tier,
            direct,
        } = arrival;
        let first = !self.adjacency.holds(user, host);
        // The pairs of the links this one replaces, with the host each was.
        let mut orphaned: Vec<(UserId, HostId, LinkId, BTreeSet<LinkId>)> = Vec::new();
        if let Some((again, partners)) = self.forget_link(link) {
            orphaned.push((again.user, again.host, link, partners));
        }
        if let Some(direct) = direct {
            let duplicates: Vec<(LinkId, Direct)> = self
                .adjacency
                .links_of(user, host)
                .into_iter()
                .filter_map(|other| Some((other, self.links.get(&other)?.direct?)))
                .collect();
            if duplicates.iter().any(|(_, held)| {
                held.instance == direct.instance
                    && (held.order == DialOrder::Preferred || held.order == direct.order)
            }) {
                out.push(Output::RefuseLink { link });
                for (user, host, gone, partners) in orphaned {
                    self.relink(user, host, gone, partners, out);
                }
                return;
            }
            for (displaced, _) in duplicates {
                if let Some((gone, partners)) = self.forget_link(displaced) {
                    orphaned.push((gone.user, gone.host, displaced, partners));
                }
                out.push(Output::Displace { link: displaced });
            }
        }
        let advertised: BTreeSet<HostId> = advertised.iter().copied().collect();
        let current = self.adjacency.neighbors(user);
        for neighbor in &current {
            if *neighbor != host && !advertised.contains(neighbor) {
                self.tell(link, user, *neighbor, true, out);
            }
        }
        for gone in &advertised {
            if *gone != host && !current.contains(gone) {
                self.tell(link, user, *gone, false, out);
            }
        }
        if first {
            for (other, _) in self.adjacency.others(user, host) {
                self.tell(other, user, host, true, out);
            }
        }
        self.touched.links.push(link);
        self.links.insert(
            link,
            RelayLink {
                user,
                host,
                tier,
                direct,
                expired: false,
            },
        );
        self.adjacency.add(user, host, link);
        for (user, host, gone, partners) in orphaned {
            self.relink(user, host, gone, partners, out);
        }
    }

    /// A link went down: when it was its host's last, every other link on
    /// the account is told the host is gone.
    fn link_down(&mut self, link: LinkId, out: &mut Vec<Output>) {
        let Some((gone, partners)) = self.forget_link(link) else {
            return;
        };
        self.relink(gone.user, gone.host, link, partners, out);
        if !self.adjacency.holds(gone.user, gone.host) {
            for (other, _) in self.adjacency.others(gone.user, gone.host) {
                self.tell(other, gone.user, gone.host, false, out);
            }
        }
    }

    /// Drops a link and its pairs, telling nobody; returns the link and
    /// the partners of its pairs.
    fn forget_link(&mut self, link: LinkId) -> Option<(RelayLink, BTreeSet<LinkId>)> {
        let gone = self.links.remove(&link)?;
        self.touched.links.push(link);
        self.adjacency.remove(gone.user, gone.host, link);
        self.refused.retain(|(sender, _), _| *sender != link);
        let partners = self.unpair_all(link);
        Some((gone, partners))
    }

    /// Removes every pair through `link` from the table, returning the
    /// other end of each.
    fn unpair_all(&mut self, link: LinkId) -> BTreeSet<LinkId> {
        let partners = self.pairs.remove(&link).unwrap_or_default();
        for partner in &partners {
            if let Some(theirs) = self.pairs.get_mut(partner) {
                theirs.remove(&link);
                if theirs.is_empty() {
                    self.pairs.remove(partner);
                }
            }
        }
        partners
    }

    /// The pairs `gone`, a link of `host`, was in: each moves to the link
    /// `host` still holds here when that link may be paired with the
    /// partner, telling nobody; the rest end and the partner is told the
    /// host is out of reach.
    fn relink(
        &mut self,
        user: UserId,
        host: HostId,
        gone: LinkId,
        partners: BTreeSet<LinkId>,
        out: &mut Vec<Output>,
    ) {
        for partner in partners {
            let moved = self
                .adjacency
                .route(user, host)
                .filter(|to| self.may_pair(partner, *to).is_ok());
            out.push(Output::Unforward {
                a: partner,
                b: gone,
                reason: ForwardRefusal::NoRoute,
                tell_both: moved.is_none(),
            });
            match moved {
                Some(to) => self.pair(partner, to, out),
                None => self.mark_refused(partner, host),
            }
        }
    }

    /// Installs the pair and says so.
    fn pair(&mut self, a: LinkId, b: LinkId, out: &mut Vec<Output>) {
        self.pairs.entry(a).or_default().insert(b);
        self.pairs.entry(b).or_default().insert(a);
        self.touched.links.extend([a, b]);
        self.touched.forwarded.push((a, b));
        out.push(Output::Forward { a, b });
    }

    fn mark_refused(&mut self, sender: LinkId, dst: HostId) {
        self.refused.insert((sender, dst), self.now_ms);
    }

    /// Whether packets may flow between two links: both up, on one account,
    /// and both paid or both pinned.
    fn may_pair(&self, a: LinkId, b: LinkId) -> Result<(), ForwardRefusal> {
        let (Some(from), Some(to)) = (self.links.get(&a), self.links.get(&b)) else {
            return Err(ForwardRefusal::NoRoute);
        };
        if a == b || from.user != to.user || from.host == to.host {
            return Err(ForwardRefusal::NoRoute);
        }
        if from.tier == Some(Tier::Free) || to.tier == Some(Tier::Free) {
            return Err(ForwardRefusal::PaymentRequired);
        }
        match (from.forwards_as(), to.forwards_as()) {
            (Some(paid), Some(other)) if paid == other => Ok(()),
            _ => Err(ForwardRefusal::NoRoute),
        }
    }

    /// A packet for a pair the driver has not been told about. A sender not
    /// up is not admitted, so it is told nothing; one already told of this
    /// destination within [`REFUSAL_REPEAT_MS`] is not told again.
    fn packet(&mut self, from: LinkId, dst: HostId, out: &mut Vec<Output>) {
        let Some(sender) = self.links.get(&from) else {
            return;
        };
        self.touched.links.push(from);
        let verdict = if dst == self.own {
            Err(ForwardRefusal::NoRoute)
        } else {
            match self.adjacency.route(sender.user, dst) {
                Some(to) => self.may_pair(from, to).map(|()| to),
                None => Err(ForwardRefusal::NoRoute),
            }
        };
        match verdict {
            Ok(to) => self.pair(from, to, out),
            Err(reason) => {
                if self
                    .refused
                    .get(&(from, dst))
                    .is_some_and(|at| self.now_ms < at.saturating_add(REFUSAL_REPEAT_MS))
                {
                    return;
                }
                self.mark_refused(from, dst);
                out.push(Output::Refuse {
                    link: from,
                    dst,
                    reason,
                });
            }
        }
    }

    /// Ends every pair through a link that may no longer be forwarded for,
    /// telling both ends: a lapse or an expiry.
    fn lapse(&mut self, link: LinkId, reason: ForwardRefusal, out: &mut Vec<Output>) {
        let Some(host) = self.links.get(&link).map(|lapsed| lapsed.host) else {
            return;
        };
        self.touched.links.push(link);
        let mut removed = Vec::new();
        for partner in self.unpair_all(link) {
            out.push(Output::Unforward {
                a: partner,
                b: link,
                reason,
                tell_both: true,
            });
            if let Some(partner_host) = self.links.get(&partner).map(|other| other.host) {
                self.mark_refused(link, partner_host);
            }
            self.mark_refused(partner, host);
            removed.push((partner, true));
        }
        self.touched.lapsed.push((link, removed));
    }

    /// A token link's token ran out: nothing is forwarded for it until a
    /// fresh token comes, and its host is out of reach meanwhile. A pinned
    /// key does not expire.
    fn expired(&mut self, link: LinkId, out: &mut Vec<Output>) {
        let Some(expired) = self.links.get_mut(&link) else {
            return;
        };
        if expired.tier.is_none() {
            return;
        }
        expired.expired = true;
        self.lapse(link, ForwardRefusal::NoRoute, out);
    }

    fn tell(&mut self, to: LinkId, user: UserId, host: HostId, up: bool, out: &mut Vec<Output>) {
        self.touched.told.push(Told { to, user, host });
        out.push(Output::Neighbor { to, host, up });
    }

    /// The hosts on `user`'s account holding a link here: what a handshake
    /// lists as this host's neighbors.
    pub fn neighbors(&self, user: UserId) -> Vec<HostId> {
        self.adjacency.neighbors(user)
    }

    /// A fresh token: it ends an expiry. A lapse to the free plan ends
    /// every pair through the link and tells both ends.
    fn tier_changed(&mut self, link: LinkId, tier: Tier, out: &mut Vec<Output>) {
        let Some(changed) = self.links.get_mut(&link) else {
            return;
        };
        if changed.tier.is_none() {
            return;
        }
        changed.tier = Some(tier);
        changed.expired = false;
        self.touched.links.push(link);
        if tier != Tier::Free {
            return;
        }
        self.lapse(link, ForwardRefusal::PaymentRequired, out);
    }

    /// The cheap predicates checked after every step, each in constant time
    /// per link the step touched.
    pub fn check_step_invariants(&self) -> Result<(), Violation> {
        for told in &self.touched.told {
            let fits = self
                .links
                .get(&told.to)
                .is_some_and(|to| to.host != told.host && to.user == told.user);
            if !fits {
                return Err(Violation {
                    invariant: TOLD_ONLY_OF_ITS_NEIGHBORS,
                    detail: format!(
                        "link {} was told of a host that is itself, on another account, or while it is down",
                        told.to.0
                    ),
                });
            }
        }
        for (a, b) in &self.touched.forwarded {
            for link in [a, b] {
                if !self.links.contains_key(link) {
                    return Err(Violation {
                        invariant: NOTHING_BEFORE_ADMISSION,
                        detail: format!("link {} was paired before it was admitted", link.0),
                    });
                }
            }
        }
        for link in &self.touched.links {
            for partner in self.pairs.get(link).into_iter().flatten() {
                let installed_back = self
                    .pairs
                    .get(partner)
                    .is_some_and(|theirs| theirs.contains(link));
                if !installed_back || self.may_pair(*link, *partner).is_err() {
                    return Err(Violation {
                        invariant: FORWARDS_ONLY_PAID_PAIRS,
                        detail: format!(
                            "links {} and {} are paired while one is down, unpaid, expired or on another account",
                            link.0, partner.0
                        ),
                    });
                }
            }
        }
        for (link, removed) in &self.touched.lapsed {
            if self.pairs.contains_key(link) || removed.iter().any(|(_, told)| !told) {
                return Err(Violation {
                    invariant: LAPSE_TELLS_BOTH,
                    detail: format!(
                        "link {} lapsed or expired with a pair left through it or an end untold",
                        link.0
                    ),
                });
            }
        }
        Ok(())
    }

    /// Every pair forwarded between, each once, the lower link first.
    pub fn pairs(&self) -> impl Iterator<Item = (LinkId, LinkId)> + '_ {
        self.pairs.iter().flat_map(|(a, partners)| {
            partners
                .iter()
                .filter(move |b| *a < **b)
                .map(move |b| (*a, *b))
        })
    }

    /// The host a link that is up belongs to.
    pub fn host_of(&self, link: LinkId) -> Option<HostId> {
        self.links.get(&link).map(|up| up.host)
    }

    /// How long one step may take before it is reported.
    pub fn budget(&self) -> StepBudget {
        self.budget
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RELAY: HostId = HostId([0xee; 16]);
    const ALICE: UserId = UserId([1; 16]);
    const BOB: UserId = UserId([2; 16]);
    const DESK: HostId = HostId([0xd0; 16]);
    const LAPTOP: HostId = HostId([0x1a; 16]);

    fn relay() -> Relay {
        Relay::with_budget(RELAY, StepBudget::UNLIMITED)
    }

    fn step(relay: &mut Relay, input: Input) -> Vec<Output> {
        let mut out = Vec::new();
        relay.step(input, &mut out);
        out
    }

    /// Brings a link up whose handshake listed every neighbor there is, so
    /// it is told of none.
    fn up(relay: &mut Relay, link: u64, user: UserId, host: HostId, tier: Option<Tier>) {
        let advertised = relay.neighbors(user);
        let out = step(
            relay,
            Input::LinkUp {
                link: LinkId(link),
                user,
                host,
                tier,
                direct: None,
                advertised,
            },
        );
        assert!(
            out.iter()
                .all(|output| matches!(output, Output::Neighbor { up: true, .. })),
            "{out:?}"
        );
    }

    fn direct_up(
        relay: &mut Relay,
        link: u64,
        host: HostId,
        order: DialOrder,
        instance: u8,
        advertised: &[HostId],
    ) -> Vec<Output> {
        step(
            relay,
            Input::LinkUp {
                link: LinkId(link),
                user: ALICE,
                host,
                tier: None,
                direct: Some(Direct {
                    order,
                    instance: [instance; 16],
                }),
                advertised: advertised.to_vec(),
            },
        )
    }

    fn told(to: u64, host: HostId, up: bool) -> Output {
        Output::Neighbor {
            to: LinkId(to),
            host,
            up,
        }
    }

    const PHONE: HostId = HostId([0x9f; 16]);

    #[test]
    fn a_hosts_first_link_is_announced_to_its_accounts_other_links_and_its_last_link_going_is_too()
    {
        let mut relay = relay();
        up(&mut relay, 1, ALICE, DESK, None);
        up(&mut relay, 9, BOB, PHONE, None);
        let out = step(
            &mut relay,
            Input::LinkUp {
                link: LinkId(2),
                user: ALICE,
                host: LAPTOP,
                tier: None,
                direct: None,
                advertised: vec![DESK],
            },
        );
        assert_eq!(out, vec![told(1, LAPTOP, true)]);
        let second = step(
            &mut relay,
            Input::LinkUp {
                link: LinkId(3),
                user: ALICE,
                host: LAPTOP,
                tier: None,
                direct: None,
                advertised: vec![DESK],
            },
        );
        assert_eq!(second, Vec::new(), "a second link to a host is not news");
        assert_eq!(
            step(&mut relay, Input::LinkDown { link: LinkId(2) }),
            Vec::new()
        );
        assert_eq!(
            step(&mut relay, Input::LinkDown { link: LinkId(3) }),
            vec![told(1, LAPTOP, false)]
        );
    }

    #[test]
    fn a_new_link_is_told_the_difference_between_its_handshake_and_the_neighbors_there_are() {
        let mut relay = relay();
        up(&mut relay, 1, ALICE, DESK, None);
        let out = step(
            &mut relay,
            Input::LinkUp {
                link: LinkId(2),
                user: ALICE,
                host: LAPTOP,
                tier: None,
                direct: None,
                advertised: vec![PHONE, LAPTOP],
            },
        );
        assert_eq!(
            out,
            vec![
                told(2, DESK, true),
                told(2, PHONE, false),
                told(1, LAPTOP, true)
            ]
        );
    }

    #[test]
    fn the_host_with_the_lower_id_dials_the_preferred_link() {
        assert_eq!(
            DialOrder::for_hosts(DESK, LAPTOP, false),
            DialOrder::Preferred
        );
        assert_eq!(
            DialOrder::for_hosts(LAPTOP, DESK, true),
            DialOrder::Preferred
        );
        assert_eq!(
            DialOrder::for_hosts(LAPTOP, DESK, false),
            DialOrder::Fallback
        );
        assert_eq!(
            DialOrder::for_hosts(DESK, LAPTOP, true),
            DialOrder::Fallback
        );
    }

    fn violation(out: &[Output]) -> Option<Check> {
        out.iter().find_map(|output| match output {
            Output::Dump {
                reason: DumpReason::Violation(violation),
            } => Some(violation.invariant),
            _ => None,
        })
    }

    fn packet(relay: &mut Relay, from: u64, dst: HostId) -> Vec<Output> {
        step(
            relay,
            Input::Packet {
                from: LinkId(from),
                dst,
            },
        )
    }

    fn forward(a: u64, b: u64) -> Output {
        Output::Forward {
            a: LinkId(a),
            b: LinkId(b),
        }
    }

    fn unforward(a: u64, b: u64, reason: ForwardRefusal, tell_both: bool) -> Output {
        Output::Unforward {
            a: LinkId(a),
            b: LinkId(b),
            reason,
            tell_both,
        }
    }

    fn refuse(link: u64, dst: HostId, reason: ForwardRefusal) -> Vec<Output> {
        vec![Output::Refuse {
            link: LinkId(link),
            dst,
            reason,
        }]
    }

    fn pairs(relay: &Relay) -> Vec<(u64, u64)> {
        relay.pairs().map(|(a, b)| (a.0, b.0)).collect()
    }

    const TABLET: HostId = HostId([0x7a; 16]);

    #[test]
    fn a_packet_is_paired_with_the_newest_link_of_its_destination() {
        let mut relay = relay();
        up(&mut relay, 1, ALICE, DESK, Some(Tier::Pro));
        up(&mut relay, 2, ALICE, LAPTOP, Some(Tier::Pro));
        up(&mut relay, 3, ALICE, LAPTOP, Some(Tier::Pro));
        assert_eq!(packet(&mut relay, 1, LAPTOP), vec![forward(1, 3)]);
    }

    #[test]
    fn a_free_end_of_either_side_is_refused_for_payment() {
        let mut relay = relay();
        up(&mut relay, 1, ALICE, DESK, Some(Tier::Free));
        up(&mut relay, 2, ALICE, LAPTOP, Some(Tier::Pro));
        assert_eq!(
            packet(&mut relay, 1, LAPTOP),
            refuse(1, LAPTOP, ForwardRefusal::PaymentRequired)
        );
        assert_eq!(
            packet(&mut relay, 2, DESK),
            refuse(2, DESK, ForwardRefusal::PaymentRequired)
        );
        assert_eq!(pairs(&relay), Vec::new());
    }

    #[test]
    fn hosts_out_of_reach_are_refused_as_no_route() {
        let mut relay = relay();
        up(&mut relay, 1, ALICE, DESK, Some(Tier::Pro));
        up(&mut relay, 2, BOB, LAPTOP, Some(Tier::Pro));
        assert_eq!(
            packet(&mut relay, 1, LAPTOP),
            refuse(1, LAPTOP, ForwardRefusal::NoRoute),
            "another account's host"
        );
        assert_eq!(
            packet(&mut relay, 1, PHONE),
            refuse(1, PHONE, ForwardRefusal::NoRoute),
            "a host with no link here"
        );
        assert_eq!(
            packet(&mut relay, 1, RELAY),
            refuse(1, RELAY, ForwardRefusal::NoRoute),
            "the relay itself"
        );
        assert_eq!(
            packet(&mut relay, 1, DESK),
            refuse(1, DESK, ForwardRefusal::NoRoute),
            "the sender itself"
        );
    }

    #[test]
    fn a_paid_link_and_a_pinned_link_are_never_paired() {
        let mut relay = relay();
        up(&mut relay, 1, ALICE, DESK, Some(Tier::Pro));
        up(&mut relay, 2, ALICE, LAPTOP, None);
        assert_eq!(
            packet(&mut relay, 1, LAPTOP),
            refuse(1, LAPTOP, ForwardRefusal::NoRoute)
        );
        assert_eq!(
            packet(&mut relay, 2, DESK),
            refuse(2, DESK, ForwardRefusal::NoRoute)
        );
    }

    #[test]
    fn a_refusal_repeats_at_most_once_a_window_per_destination() {
        let mut relay = relay();
        up(&mut relay, 1, ALICE, DESK, Some(Tier::Pro));
        assert_eq!(
            packet(&mut relay, 1, LAPTOP),
            refuse(1, LAPTOP, ForwardRefusal::NoRoute)
        );
        assert_eq!(packet(&mut relay, 1, LAPTOP), Vec::new());
        assert_eq!(
            packet(&mut relay, 1, PHONE),
            refuse(1, PHONE, ForwardRefusal::NoRoute),
            "another destination is told at once"
        );
        step(
            &mut relay,
            Input::Tick {
                now_ms: REFUSAL_REPEAT_MS - 1,
            },
        );
        assert_eq!(packet(&mut relay, 1, LAPTOP), Vec::new());
        step(
            &mut relay,
            Input::Tick {
                now_ms: REFUSAL_REPEAT_MS,
            },
        );
        assert_eq!(
            packet(&mut relay, 1, LAPTOP),
            refuse(1, LAPTOP, ForwardRefusal::NoRoute)
        );
        // Once the destination arrives, the pair is installed at once.
        up(&mut relay, 2, ALICE, LAPTOP, Some(Tier::Pro));
        assert_eq!(packet(&mut relay, 1, LAPTOP), vec![forward(1, 2)]);
    }

    #[test]
    fn a_link_going_down_ends_its_pairs_and_tells_the_ends_still_up() {
        let mut relay = relay();
        up(&mut relay, 1, ALICE, DESK, Some(Tier::Pro));
        up(&mut relay, 2, ALICE, LAPTOP, Some(Tier::Pro));
        up(&mut relay, 3, ALICE, TABLET, Some(Tier::Pro));
        packet(&mut relay, 1, LAPTOP);
        packet(&mut relay, 3, LAPTOP);
        assert_eq!(pairs(&relay), vec![(1, 2), (2, 3)]);
        assert_eq!(
            step(&mut relay, Input::LinkDown { link: LinkId(2) }),
            vec![
                unforward(1, 2, ForwardRefusal::NoRoute, true),
                unforward(3, 2, ForwardRefusal::NoRoute, true),
                told(3, LAPTOP, false),
                told(1, LAPTOP, false),
            ]
        );
        assert_eq!(pairs(&relay), Vec::new());
        assert_eq!(
            packet(&mut relay, 1, LAPTOP),
            Vec::new(),
            "the end told counts as refused"
        );
    }

    #[test]
    fn a_pair_moves_to_the_other_link_of_a_host_whose_link_went_down() {
        let mut relay = relay();
        up(&mut relay, 1, ALICE, DESK, Some(Tier::Pro));
        up(&mut relay, 2, ALICE, LAPTOP, Some(Tier::Pro));
        up(&mut relay, 3, ALICE, LAPTOP, Some(Tier::Pro));
        assert_eq!(packet(&mut relay, 1, LAPTOP), vec![forward(1, 3)]);
        assert_eq!(
            step(&mut relay, Input::LinkDown { link: LinkId(3) }),
            vec![
                unforward(1, 3, ForwardRefusal::NoRoute, false),
                forward(1, 2)
            ]
        );
        assert_eq!(pairs(&relay), vec![(1, 2)]);
    }

    #[test]
    fn a_lapse_to_free_ends_every_pair_through_the_link_and_tells_both_ends() {
        let mut relay = relay();
        up(&mut relay, 1, ALICE, DESK, Some(Tier::Pro));
        up(&mut relay, 2, ALICE, LAPTOP, Some(Tier::Pro));
        up(&mut relay, 3, ALICE, TABLET, Some(Tier::Pro));
        packet(&mut relay, 2, DESK);
        packet(&mut relay, 2, TABLET);
        packet(&mut relay, 1, TABLET);
        assert_eq!(
            step(
                &mut relay,
                Input::TierChanged {
                    link: LinkId(2),
                    tier: Tier::Pro,
                },
            ),
            Vec::new(),
            "a renewal ends nothing"
        );
        assert_eq!(
            step(
                &mut relay,
                Input::TierChanged {
                    link: LinkId(2),
                    tier: Tier::Free,
                },
            ),
            vec![
                unforward(1, 2, ForwardRefusal::PaymentRequired, true),
                unforward(3, 2, ForwardRefusal::PaymentRequired, true),
            ]
        );
        assert_eq!(pairs(&relay), vec![(1, 3)], "pairs not through it stay");
        assert_eq!(
            packet(&mut relay, 1, LAPTOP),
            Vec::new(),
            "both ends were told already"
        );
        assert_eq!(packet(&mut relay, 2, DESK), Vec::new());
        step(
            &mut relay,
            Input::Tick {
                now_ms: REFUSAL_REPEAT_MS,
            },
        );
        assert_eq!(
            packet(&mut relay, 1, LAPTOP),
            refuse(1, LAPTOP, ForwardRefusal::PaymentRequired)
        );
    }

    #[test]
    fn an_expired_token_ends_every_pair_through_the_link_until_a_fresh_one_comes() {
        let mut relay = relay();
        up(&mut relay, 1, ALICE, DESK, Some(Tier::Pro));
        up(&mut relay, 2, ALICE, LAPTOP, Some(Tier::Pro));
        packet(&mut relay, 1, LAPTOP);
        assert_eq!(
            step(&mut relay, Input::Expired { link: LinkId(2) }),
            vec![unforward(1, 2, ForwardRefusal::NoRoute, true)]
        );
        assert_eq!(pairs(&relay), Vec::new());
        step(
            &mut relay,
            Input::Tick {
                now_ms: REFUSAL_REPEAT_MS,
            },
        );
        assert_eq!(
            packet(&mut relay, 2, DESK),
            refuse(2, DESK, ForwardRefusal::NoRoute),
            "nothing is forwarded for an expired link"
        );
        step(
            &mut relay,
            Input::TierChanged {
                link: LinkId(2),
                tier: Tier::Pro,
            },
        );
        assert_eq!(packet(&mut relay, 1, LAPTOP), vec![forward(1, 2)]);
    }

    #[test]
    fn a_daemon_forwards_between_its_pinned_links_whose_keys_neither_lapse_nor_expire() {
        let mut relay = relay();
        up(&mut relay, 1, ALICE, DESK, None);
        up(&mut relay, 2, ALICE, LAPTOP, None);
        assert_eq!(packet(&mut relay, 1, LAPTOP), vec![forward(1, 2)]);
        assert_eq!(
            step(
                &mut relay,
                Input::TierChanged {
                    link: LinkId(1),
                    tier: Tier::Free,
                },
            ),
            Vec::new()
        );
        assert_eq!(
            step(&mut relay, Input::Expired { link: LinkId(1) }),
            Vec::new()
        );
        assert_eq!(pairs(&relay), vec![(1, 2)]);
        assert_eq!(
            step(&mut relay, Input::LinkDown { link: LinkId(1) }),
            vec![
                unforward(2, 1, ForwardRefusal::NoRoute, true),
                told(2, DESK, false)
            ]
        );
    }

    #[test]
    fn a_displaced_direct_link_hands_its_pairs_to_the_link_replacing_it() {
        let mut relay = relay();
        up(&mut relay, 9, ALICE, LAPTOP, None);
        direct_up(&mut relay, 1, DESK, DialOrder::Fallback, 7, &[LAPTOP]);
        assert_eq!(packet(&mut relay, 9, DESK), vec![forward(9, 1)]);
        assert_eq!(
            direct_up(&mut relay, 2, DESK, DialOrder::Preferred, 7, &[LAPTOP]),
            vec![
                Output::Displace { link: LinkId(1) },
                unforward(9, 1, ForwardRefusal::NoRoute, false),
                forward(9, 2),
            ]
        );
        assert_eq!(pairs(&relay), vec![(2, 9)]);
    }

    #[test]
    fn a_pair_through_a_free_link_asks_for_a_dump() {
        let mut relay = relay();
        up(&mut relay, 1, ALICE, DESK, Some(Tier::Pro));
        up(&mut relay, 2, ALICE, LAPTOP, Some(Tier::Free));
        // A bug that installs a pair without asking whether it may.
        relay.pairs.entry(LinkId(1)).or_default().insert(LinkId(2));
        relay.pairs.entry(LinkId(2)).or_default().insert(LinkId(1));
        let out = step(&mut relay, Input::Expired { link: LinkId(9) });
        assert_eq!(
            violation(&out),
            None,
            "a step touching neither link checks neither"
        );
        let out = packet(&mut relay, 1, LAPTOP);
        assert_eq!(violation(&out), Some(FORWARDS_ONLY_PAID_PAIRS), "{out:?}");
    }

    #[test]
    fn a_pair_onto_a_link_that_is_down_asks_for_a_dump() {
        let mut relay = relay();
        up(&mut relay, 1, ALICE, DESK, Some(Tier::Pro));
        // A bug that leaves a link in the account's adjacency after it went.
        relay.adjacency.add(ALICE, LAPTOP, LinkId(9));
        let mut out = Vec::new();
        relay.pair(LinkId(1), LinkId(9), &mut out);
        assert_eq!(
            relay.check_step_invariants().map_err(|v| v.invariant),
            Err(NOTHING_BEFORE_ADMISSION)
        );
    }

    #[test]
    fn a_step_over_its_budget_asks_for_a_dump() {
        let mut relay = Relay::with_budget(RELAY, StepBudget { micros: 0 });
        let mut out = Vec::new();
        // Any step takes longer than nothing, given a clock fine enough;
        // spin until one is measured over.
        while out.is_empty() {
            relay.step(Input::Tick { now_ms: 1 }, &mut out);
        }
        assert!(matches!(
            out[..],
            [Output::Dump {
                reason: DumpReason::OverBudget { .. }
            }]
        ));
    }
}
