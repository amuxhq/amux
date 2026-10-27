//! The relay's forwarding, explored over the real `relay_core::Relay`.
//!
//! The world is three hosts reaching the relay: a desk and a laptop on one
//! account and a phone on another. A host's link connects and is admitted
//! once its token checks out, or gives up first; while it waits its packets
//! arrive and the driver holds them, and on admission it asks the core
//! about each. A host may connect again while its first link is still up,
//! as a host does after its network changes. Admitted links send packets
//! for any host, whenever the driver has no pair that carries them; an
//! account's plan lapses and is bought again, and each link learns of it
//! when it presents its next token; a link's token expires before a fresh
//! one comes. When a link presents a paid plan, the relay asks its
//! account's links still on the free plan for a fresh credential. Each link opens its handshake with the neighbors its host was
//! told of, and its host forgets them once it holds no link here. At any
//! point the world may stop changing.
//!
//! The same machine with every tier `None` is a daemon forwarding between
//! its own paired hosts, all on its one account, whose pinned keys neither
//! lapse nor expire. Its links are its own network links to those hosts,
//! each dialled by one end or the other: both ends may dial at once, and a
//! host's process may restart, leaving the links of its earlier life to
//! be found dead.
//!
//! What the properties read is the driver's own table of pairs, built only
//! from what the core told it, against the world's record of each link: the
//! core's view of itself is not consulted.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use daemon_core::{HostId, LinkId, StepBudget};
use relay_core::{
    ASKS_ONLY_FREE_LINKS, DialOrder, Direct, FORWARDS_ONLY_PAID_PAIRS, Input, LAPSE_TELLS_BOTH,
    NOTHING_BEFORE_ADMISSION, Output, Relay, Tier, UserId,
};

use crate::{Machine, Property};

const RELAY: HostId = HostId([0xee; 16]);
const ALICE: UserId = UserId([1; 16]);
const BOB: UserId = UserId([2; 16]);
const DESK: HostId = HostId([0xd0; 16]);
const LAPTOP: HostId = HostId([0x1a; 16]);
const PHONE: HostId = HostId([0x50; 16]);
const HOSTS: [HostId; 3] = [DESK, LAPTOP, PHONE];

/// How many links one host may hold open at once, admitted or not.
const LINKS_PER_HOST: usize = 2;

/// The relay's world, bounded by how many times each thing may happen.
#[derive(Clone, Debug)]
pub struct RelayModel {
    /// Links that connect, all told.
    pub links: u8,
    /// Packets hosts send that the driver asks the core about.
    pub packets: u8,
    /// Times an account's plan lapses or is bought again.
    pub plan_changes: u8,
    /// Tokens that expire before a fresh one comes.
    pub expiries: u8,
    /// Every link is a daemon's paired host, admitted by its pinned key,
    /// on the daemon's own network link to it.
    pub pinned: bool,
    /// Times a paired host's process restarts.
    pub restarts: u8,
    /// The accounts are paying when the world begins; otherwise their one
    /// plan change is a purchase.
    pub starts_paid: bool,
}

impl RelayModel {
    /// The bounds `just test` explores.
    pub fn ci() -> Self {
        Self {
            links: 3,
            packets: 3,
            plan_changes: 1,
            expiries: 1,
            pinned: false,
            restarts: 0,
            starts_paid: true,
        }
    }

    /// The `just test` bounds with both accounts starting on the free plan,
    /// so a plan change is a purchase reaching links already up.
    pub fn purchase() -> Self {
        Self {
            starts_paid: false,
            ..Self::ci()
        }
    }

    /// The nightly bounds.
    pub fn nightly() -> Self {
        Self {
            links: 4,
            packets: 3,
            plan_changes: 1,
            expiries: 1,
            pinned: false,
            restarts: 0,
            starts_paid: true,
        }
    }

    /// A daemon forwarding between its paired hosts, at `just test` bounds:
    /// with no plan to change and no token to expire, its links may cross
    /// and a host's process may restart.
    pub fn daemon() -> Self {
        Self {
            pinned: true,
            restarts: 1,
            ..Self::ci()
        }
    }

    /// The daemon's nightly bounds: a link more.
    pub fn daemon_nightly() -> Self {
        Self {
            links: 4,
            ..Self::daemon()
        }
    }

    fn account(&self, host: HostId) -> UserId {
        if host == PHONE && !self.pinned {
            BOB
        } else {
            ALICE
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// A new link of the host connects; it is not admitted yet. A
    /// daemon's link to a paired host was dialled by the end `order` says.
    Connect {
        host: HostId,
        order: Option<DialOrder>,
    },
    /// The host's process restarts: links of its earlier life that are not
    /// up never will be, and those up are dead.
    Restart(HostId),
    /// The link's token, or its pinned key, checks out.
    Admit(LinkId),
    /// The link goes, admitted or not.
    Down(LinkId),
    /// A packet arrives on the link for the host.
    Packet { from: LinkId, dst: HostId },
    /// The account's plan lapses, or is bought again.
    Plan(UserId),
    /// The link presents a fresh token carrying its account's plan.
    Token(LinkId),
    /// The link's token expires.
    Expire(LinkId),
    /// The world stops changing.
    Rest,
}

/// One link, as the world knows it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Link {
    host: HostId,
    /// Admitted by the core; until then the destinations of the packets the
    /// driver holds for it.
    admitted: bool,
    held: BTreeSet<HostId>,
    /// The plan its last token carried; `None` for a pinned key.
    tier: Option<Tier>,
    expired: bool,
    /// How a daemon's own link to the host was dialled, and in which life
    /// of the host's process.
    direct: Option<Direct>,
}

impl Link {
    /// Whether its last token, or its pinned key, lets it be forwarded for.
    fn paid(&self) -> Option<bool> {
        match (self.tier, self.expired) {
            (None, _) => Some(false),
            (Some(Tier::Pro), false) => Some(true),
            _ => None,
        }
    }
}

/// How much of the bounds a path has used.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
struct Used {
    links: u8,
    packets: u8,
    plan_changes: u8,
    expiries: u8,
    restarts: u8,
}

#[derive(Clone, PartialEq, Eq, Hash)]
pub struct State {
    relay: Relay,
    next_link: u64,
    /// The links connected and not gone.
    links: BTreeMap<LinkId, Link>,
    /// Each account's plan pays for forwarding.
    paid: BTreeMap<UserId, bool>,
    /// The driver's table of pairs it forwards between, lower link first.
    table: BTreeSet<(LinkId, LinkId)>,
    /// What each host was told of its neighbors here, while it holds a link.
    pictures: BTreeMap<HostId, BTreeSet<HostId>>,
    /// The pairs a lapse or expiry removed whose ends are still to be told.
    owed: BTreeSet<(LinkId, LinkId)>,
    rested: bool,
    used: Used,
    /// The first pair installed for a link the world had not admitted.
    early: Option<String>,
    /// The first link told of a host on another account, of its own host, or
    /// told anything while not admitted.
    foreign: Option<String>,
    /// The current life of each host's process.
    lives: BTreeMap<HostId, u8>,
    /// The first time the core kept a fallback link of a host's life over
    /// a preferred one of the same life.
    kept_fallback: Option<String>,
    /// The core asked for a dump.
    dumped: Option<String>,
    /// The first link asked for a fresh credential while not up on the
    /// free plan beside a paid link of its account.
    misasked: Option<String>,
    /// The first paid presentation that left a free link of its account
    /// unasked.
    unasked: Option<String>,
    /// A free link was asked for a fresh credential.
    asked: bool,
    forwarded: bool,
    crossed: bool,
    /// The core refused a direct link, or displaced one.
    refused: bool,
    displaced: bool,
}

impl fmt::Debug for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("State")
            .field("links", &self.links)
            .field("paid", &self.paid)
            .field("table", &self.table)
            .field("core_pairs", &self.relay.pairs().collect::<Vec<_>>())
            .field("pictures", &self.pictures)
            .field("owed", &self.owed)
            .field("rested", &self.rested)
            .field("early", &self.early)
            .field("foreign", &self.foreign)
            .field("lives", &self.lives)
            .field("kept_fallback", &self.kept_fallback)
            .field("dumped", &self.dumped)
            .field("misasked", &self.misasked)
            .field("unasked", &self.unasked)
            .finish()
    }
}

fn ordered(a: LinkId, b: LinkId) -> (LinkId, LinkId) {
    if a < b { (a, b) } else { (b, a) }
}

impl State {
    fn admitted(&self, link: LinkId) -> Option<&Link> {
        self.links.get(&link).filter(|link| link.admitted)
    }

    /// Why packets must not flow between the pair, if they must not: an end
    /// not admitted, two accounts, one host, or ends neither both paid nor
    /// both pinned.
    fn unfit(&self, model: &RelayModel, (a, b): (LinkId, LinkId)) -> Option<String> {
        let (Some(left), Some(right)) = (self.admitted(a), self.admitted(b)) else {
            return Some(format!(
                "links {} and {} are paired while one is not up",
                a.0, b.0
            ));
        };
        if model.account(left.host) != model.account(right.host) {
            return Some(format!("links {} and {} are on two accounts", a.0, b.0));
        }
        if left.host == right.host {
            return Some(format!("links {} and {} are one host's", a.0, b.0));
        }
        match (left.paid(), right.paid()) {
            (Some(one), Some(other)) if one == other => None,
            _ => Some(format!(
                "links {} and {} are paired as {:?} and {:?}",
                a.0, b.0, left.tier, right.tier
            )),
        }
    }

    /// Whether the driver's table carries a packet on `from` for `dst`.
    fn carries(&self, from: LinkId, dst: HostId) -> bool {
        self.table.iter().any(|(a, b)| {
            let other = if *a == from {
                *b
            } else if *b == from {
                *a
            } else {
                return false;
            };
            self.links.get(&other).is_some_and(|link| link.host == dst)
        })
    }

    fn open_links_of(&self, host: HostId) -> usize {
        self.links.values().filter(|link| link.host == host).count()
    }

    /// The direct links of one life of `host` the core holds up, besides
    /// `link`.
    fn same_life(&self, link: LinkId) -> Vec<(LinkId, Direct)> {
        let Some(this) = self.links.get(&link) else {
            return Vec::new();
        };
        let Some(direct) = this.direct else {
            return Vec::new();
        };
        self.links
            .iter()
            .filter(|(id, other)| **id != link && other.admitted && other.host == this.host)
            .filter_map(|(id, other)| Some((*id, other.direct?)))
            .filter(|(_, other)| other.instance == direct.instance)
            .collect()
    }
}

impl RelayModel {
    /// Steps the core with `input` and keeps the driver's table and the
    /// hosts' pictures from what it says.
    fn step(&self, s: &mut State, input: Input) {
        let label = format!("{input:?}");
        let presenting = match input {
            Input::LinkUp {
                link,
                tier: Some(Tier::Pro),
                ..
            }
            | Input::TierChanged {
                link,
                tier: Tier::Pro,
            } => Some(link),
            _ => None,
        };
        let mut out = Vec::new();
        s.relay.step(input, &mut out);
        let mut asked = BTreeSet::new();
        for output in out {
            match output {
                Output::Forward { a, b } => {
                    for link in [a, b] {
                        if s.admitted(link).is_none() && s.early.is_none() {
                            s.early = Some(format!("paired {} and {} on {label}", a.0, b.0));
                        }
                    }
                    s.table.insert(ordered(a, b));
                    s.forwarded = true;
                }
                Output::Unforward {
                    a, b, tell_both, ..
                } => {
                    s.table.remove(&ordered(a, b));
                    if tell_both {
                        s.owed.remove(&ordered(a, b));
                    }
                }
                Output::Neighbor { to, host, up } => {
                    let fits = s.admitted(to).is_some_and(|link| {
                        link.host != host && self.account(link.host) == self.account(host)
                    });
                    if !fits && s.foreign.is_none() {
                        s.foreign = Some(format!(
                            "link {} told of {host:?} (up: {up}) on {label}",
                            to.0
                        ));
                    }
                    if let Some(link) = s.links.get(&to) {
                        let picture = s.pictures.entry(link.host).or_default();
                        if up {
                            picture.insert(host);
                        } else {
                            picture.remove(&host);
                        }
                    }
                }
                Output::Refuse { link, .. } => {
                    if s.admitted(link).is_none() && s.foreign.is_none() {
                        s.foreign = Some(format!("link {} told of a refusal on {label}", link.0));
                    }
                }
                Output::Dump { reason } => {
                    if s.dumped.is_none() {
                        s.dumped = Some(format!("{reason:?} on {label}"));
                    }
                }
                Output::RefuseLink { link } => {
                    s.refused = true;
                    let refused = s.links.get(&link).and_then(|link| link.direct);
                    let kept = s.same_life(link);
                    if refused.is_some_and(|refused| refused.order == DialOrder::Preferred)
                        && !kept.is_empty()
                        && kept
                            .iter()
                            .all(|(_, direct)| direct.order == DialOrder::Fallback)
                        && s.kept_fallback.is_none()
                    {
                        s.kept_fallback = Some(format!("refused link {} on {label}", link.0));
                    }
                    if let Some(link) = s.links.get_mut(&link) {
                        link.admitted = false;
                    }
                    self.forget(s, link);
                }
                Output::RefreshCredential { link } => {
                    // Kept only where a property reads it, so it does not
                    // split the states of the other worlds.
                    if !self.starts_paid {
                        s.asked = true;
                    }
                    asked.insert(link);
                    if !self.beside_paid(s, link) && s.misasked.is_none() {
                        s.misasked = Some(format!("asked link {} on {label}", link.0));
                    }
                }
                Output::Displace { link } => {
                    s.displaced = true;
                    let kept = s.same_life(link);
                    if let Some(displaced) = s.links.get(&link).and_then(|link| link.direct)
                        && displaced.order == DialOrder::Preferred
                        && !kept.is_empty()
                        && kept
                            .iter()
                            .all(|(_, direct)| direct.order == DialOrder::Fallback)
                        && s.kept_fallback.is_none()
                    {
                        s.kept_fallback = Some(format!("displaced link {} on {label}", link.0));
                    }
                    self.forget(s, link);
                }
            }
        }
        if let Some(paid) = presenting.and_then(|link| s.admitted(link)) {
            let account = self.account(paid.host);
            let left: Vec<LinkId> = s
                .links
                .iter()
                .filter(|(id, link)| {
                    link.admitted
                        && link.tier == Some(Tier::Free)
                        && self.account(link.host) == account
                        && !asked.contains(*id)
                })
                .map(|(id, _)| *id)
                .collect();
            if !left.is_empty() && s.unasked.is_none() {
                s.unasked = Some(format!("free links {left:?} left unasked on {label}"));
            }
        }
    }

    /// Whether `link` is up on the free plan beside an unexpired paid link
    /// of its account: the only link the relay may ask for a fresh
    /// credential.
    fn beside_paid(&self, s: &State, link: LinkId) -> bool {
        let Some(asked) = s
            .admitted(link)
            .filter(|asked| asked.tier == Some(Tier::Free))
        else {
            return false;
        };
        let account = self.account(asked.host);
        s.links.values().any(|other| {
            other.admitted && other.paid() == Some(true) && self.account(other.host) == account
        })
    }

    /// The link went: the host forgets its neighbors once it holds none.
    fn forget(&self, s: &mut State, link: LinkId) {
        let Some(gone) = s.links.remove(&link) else {
            return;
        };
        if !s
            .links
            .values()
            .any(|other| other.host == gone.host && other.admitted)
        {
            s.pictures.remove(&gone.host);
        }
    }

    /// Records the pairs through `link` whose ends a lapse must tell.
    fn owe(s: &mut State, link: LinkId) {
        let through: Vec<(LinkId, LinkId)> = s
            .table
            .iter()
            .filter(|(a, b)| *a == link || *b == link)
            .copied()
            .collect();
        s.owed.extend(through);
    }
}

impl Machine for RelayModel {
    type State = State;
    type Action = Action;

    fn init(&self) -> State {
        let mut paid = BTreeMap::new();
        paid.insert(ALICE, self.starts_paid || self.pinned);
        if !self.pinned {
            paid.insert(BOB, self.starts_paid);
        }
        State {
            relay: Relay::with_budget(RELAY, StepBudget::UNLIMITED),
            next_link: 1,
            links: BTreeMap::new(),
            paid,
            table: BTreeSet::new(),
            pictures: BTreeMap::new(),
            owed: BTreeSet::new(),
            rested: false,
            used: Used::default(),
            early: None,
            foreign: None,
            dumped: None,
            misasked: None,
            unasked: None,
            asked: false,
            lives: BTreeMap::new(),
            kept_fallback: None,
            forwarded: false,
            crossed: false,
            refused: false,
            displaced: false,
        }
    }

    fn actions(&self, s: &State, out: &mut Vec<Action>) {
        if s.rested {
            return;
        }
        out.push(Action::Rest);
        if s.used.links < self.links {
            for host in HOSTS {
                if s.open_links_of(host) < LINKS_PER_HOST {
                    if self.pinned {
                        for order in [DialOrder::Preferred, DialOrder::Fallback] {
                            out.push(Action::Connect {
                                host,
                                order: Some(order),
                            });
                        }
                    } else {
                        out.push(Action::Connect { host, order: None });
                    }
                }
            }
        }
        if s.used.restarts < self.restarts {
            for host in HOSTS {
                if s.open_links_of(host) > 0 {
                    out.push(Action::Restart(host));
                }
            }
        }
        if !self.pinned && s.used.plan_changes < self.plan_changes {
            out.extend(s.paid.keys().map(|user| Action::Plan(*user)));
        }
        for (id, link) in &s.links {
            out.push(Action::Down(*id));
            if !link.admitted {
                out.push(Action::Admit(*id));
            }
            if s.used.packets < self.packets {
                for dst in HOSTS {
                    let asks = if link.admitted {
                        !s.carries(*id, dst)
                    } else {
                        !link.held.contains(&dst)
                    };
                    if dst != link.host && asks {
                        out.push(Action::Packet { from: *id, dst });
                    }
                }
            }
            if link.admitted && link.tier.is_some() {
                let plan = s.paid[&self.account(link.host)];
                if link.expired || link.tier != Some(tier(plan)) {
                    out.push(Action::Token(*id));
                }
                if !link.expired && s.used.expiries < self.expiries {
                    out.push(Action::Expire(*id));
                }
            }
        }
    }

    fn next(&self, s: &State, action: &Action) -> Option<State> {
        let mut next = s.clone();
        match action {
            Action::Rest => next.rested = true,
            Action::Connect { host, order } => {
                next.used.links += 1;
                let id = LinkId(next.next_link);
                next.next_link += 1;
                let life = next.lives.get(host).copied().unwrap_or_default();
                let direct = order.map(|order| Direct {
                    order,
                    instance: [life; 16],
                });
                next.links.insert(
                    id,
                    Link {
                        host: *host,
                        admitted: false,
                        held: BTreeSet::new(),
                        tier: None,
                        expired: false,
                        direct,
                    },
                );
            }
            Action::Restart(host) => {
                next.used.restarts += 1;
                *next.lives.entry(*host).or_default() += 1;
                let unborn: Vec<LinkId> = next
                    .links
                    .iter()
                    .filter(|(_, link)| link.host == *host && !link.admitted)
                    .map(|(id, _)| *id)
                    .collect();
                for id in unborn {
                    self.forget(&mut next, id);
                }
            }
            Action::Admit(id) => {
                let host = next.links.get(id)?.host;
                let user = self.account(host);
                let tier = (!self.pinned).then(|| tier(next.paid[&user]));
                let advertised: Vec<HostId> = next
                    .pictures
                    .get(&host)
                    .into_iter()
                    .flatten()
                    .copied()
                    .collect();
                let link = next.links.get_mut(id)?;
                link.admitted = true;
                link.tier = tier;
                let direct = link.direct;
                let held = std::mem::take(&mut link.held);
                if direct.is_some_and(|direct| {
                    next.same_life(*id)
                        .iter()
                        .any(|(_, other)| other.order != direct.order)
                }) {
                    next.crossed = true;
                }
                self.step(
                    &mut next,
                    Input::LinkUp {
                        link: *id,
                        user,
                        host,
                        tier,
                        direct,
                        advertised,
                    },
                );
                // A link the core refused is closed, and what it held with it.
                if !next.links.contains_key(id) {
                    return Some(next);
                }
                for dst in held {
                    if !next.carries(*id, dst) {
                        self.step(&mut next, Input::Packet { from: *id, dst });
                    }
                }
            }
            Action::Down(id) => {
                self.step(&mut next, Input::LinkDown { link: *id });
                self.forget(&mut next, *id);
            }
            Action::Packet { from, dst } => {
                next.used.packets += 1;
                let link = next.links.get_mut(from)?;
                if !link.admitted {
                    link.held.insert(*dst);
                }
                self.step(
                    &mut next,
                    Input::Packet {
                        from: *from,
                        dst: *dst,
                    },
                );
            }
            Action::Plan(user) => {
                next.used.plan_changes += 1;
                let paid = next.paid.get_mut(user)?;
                *paid = !*paid;
            }
            Action::Token(id) => {
                let host = next.links.get(id)?.host;
                let tier = tier(next.paid[&self.account(host)]);
                if tier == Tier::Free {
                    Self::owe(&mut next, *id);
                }
                let link = next.links.get_mut(id)?;
                link.tier = Some(tier);
                link.expired = false;
                self.step(&mut next, Input::TierChanged { link: *id, tier });
            }
            Action::Expire(id) => {
                next.used.expiries += 1;
                Self::owe(&mut next, *id);
                next.links.get_mut(id)?.expired = true;
                self.step(&mut next, Input::Expired { link: *id });
            }
        }
        Some(next)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        vec![
            Property::always(FORWARDS_ONLY_PAID_PAIRS, |model: &RelayModel, s: &State| {
                s.table.iter().all(|pair| s.unfit(model, *pair).is_none())
            }),
            Property::always(NOTHING_BEFORE_ADMISSION, |_, s: &State| s.early.is_none()),
            Property::eventually(LAPSE_TELLS_BOTH, |model: &RelayModel, s: &State| {
                s.rested
                    && s.owed.is_empty()
                    && s.table.iter().all(|pair| s.unfit(model, *pair).is_none())
            }),
            Property::always(
                "a link is only told of other hosts on its own account, and only once admitted",
                |_, s: &State| s.foreign.is_none(),
            ),
            Property::always("the relay core never asks for a dump", |_, s: &State| {
                s.dumped.is_none()
            }),
            Property::always(ASKS_ONLY_FREE_LINKS, |_, s: &State| s.misasked.is_none()),
            Property::always(
                "a link presenting a paid plan asks every free link of its account for a fresh credential",
                |_, s: &State| s.unasked.is_none(),
            ),
            Property::sometimes(
                "a free link is sometimes asked for a fresh credential",
                |model: &RelayModel, s: &State| model.pinned || model.starts_paid || s.asked,
            ),
            Property::sometimes("forwarding is sometimes in place", |_, s: &State| {
                s.forwarded && !s.table.is_empty()
            }),
            Property::always(
                "a host holds at most one direct link here, of its process's latest life",
                |_, s: &State| {
                    HOSTS.iter().all(|host| {
                        let up: Vec<Direct> = s
                            .links
                            .values()
                            .filter(|link| link.host == *host && link.admitted)
                            .filter_map(|link| link.direct)
                            .collect();
                        let life = s.lives.get(host).copied().unwrap_or_default();
                        up.len() <= 1
                            && up.iter().all(|direct| {
                                direct.instance == [life; 16]
                                    || !s.links.values().any(|link| {
                                        link.host == *host
                                            && link.admitted
                                            && link.direct.is_some_and(|d| d.instance == [life; 16])
                                    })
                            })
                    })
                },
            ),
            Property::always(
                "of a crossed dial both ends keep the preferred link",
                |_, s: &State| s.kept_fallback.is_none(),
            ),
            Property::sometimes(
                "a daemon's links sometimes cross",
                |model: &RelayModel, s: &State| !model.pinned || s.crossed,
            ),
            Property::sometimes(
                "a daemon sometimes refuses one link of a crossed dial",
                |model: &RelayModel, s: &State| !model.pinned || s.refused,
            ),
            Property::sometimes(
                "a daemon sometimes displaces a link of a host's earlier life",
                |model: &RelayModel, s: &State| !model.pinned || model.restarts == 0 || s.displaced,
            ),
        ]
    }
}

fn tier(paid: bool) -> Tier {
    if paid { Tier::Pro } else { Tier::Free }
}
