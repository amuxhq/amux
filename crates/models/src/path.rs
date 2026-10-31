//! The path machine: which path one link's packets take, explored over the
//! real `path_core::Paths`.
//!
//! The world is one link to a peer that can be reached three ways: at its
//! address on the local network, which works while this host is on that
//! network; at the address the relay saw it at, reached by punching, which
//! works until this host's network changes; and through the relay, which
//! forwards while it is up and the plan is paid. A fourth address, one that
//! worked on a network this host has left, never answers. The machine
//! learns the addresses late, one at a time; its probes are answered by
//! whichever path works, or lost; the peer's address on the local network
//! stops answering for long enough that several probes on it go unanswered,
//! and answers again; the network changes; the relay link goes
//! down and comes back; the plan lapses and is bought again, and the relay
//! refuses a lapsed plan's packets once it sees them; the link comes up once
//! the first flight reaches the peer and goes when no path works; and time
//! passes to whatever the machine asked to be woken at. Router mappings
//! expiring play out as lost probes on the punched path. At any point the
//! world may stop changing; time still passes, so the machine settles, and
//! its liveness is judged there.
//!
//! The world may also say more than its probes do: one of the link's
//! packets arrives, on a path it sends on or on a byte stream it has not
//! heard on; a paired daemon's forwarding link comes and goes; an answer
//! comes for a path with no probe of it outstanding; the relay refuses for
//! a reason other than the plan; the peer stops listing an address, or
//! lists an IPv6 one as near as its IPv4 one. Each bound says how often.
//!
//! What the properties read is the world's own record of what the machine
//! asked for (each probe, the paths it carried and sent on) against what
//! the world knows of each path, not the machine's view of itself.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use path_core::{
    CARRIES_ONLY_ANSWERED, ForwardRefusal, HOLD_DOWN_MS, Input, LinkId,
    NO_CROSS_NETWORK_AFTER_LAPSE, NO_SWING, Output, PROBE_INTERVAL_MS, PROBE_TIMEOUT_MS, Path,
    PathId, Paths, RELAY_READY_WHILE_DIRECT, Source, StepBudget,
};

use crate::{Machine, Property};

/// The relay's forwarding link.
const RELAY: LinkId = LinkId(1);
/// A paired daemon's forwarding link: forwards between pinned hosts
/// whatever the plan.
const VIA: LinkId = LinkId(2);
/// A byte stream to the peer, as an SSH link is: it works while it lasts.
const FRAMED: Path = Path::Framed(PathId(3));
/// The peer on the local network this host starts on.
const LAN: SocketAddr = addr([192, 168, 1, 20]);
/// The same peer's IPv6 address on that network, as near as the IPv4 one.
const LAN6: SocketAddr = SocketAddr::new(
    IpAddr::V6(Ipv6Addr::new(0xfd00, 0, 0, 0, 0, 0, 0, 0x20)),
    7000,
);
/// The peer as the relay saw it: reached by punching.
const PUNCH: SocketAddr = addr([203, 0, 113, 20]);
/// Where the peer was on a network this host has left.
const STALE: SocketAddr = addr([10, 9, 9, 20]);

/// How many times time may pass after the world stops changing: enough for
/// a lost probe to time out, its path to be probed again after the
/// interval, and the hold-down to pass.
const REST_TICKS: u8 = 8;
/// How many times time may pass while the local address is silent: enough
/// for [`LONG_OUTAGE_PROBES`] probes on it to time out with the relay's
/// probes between them.
const OUTAGE_TICKS: u8 = 6;
/// Unanswered probes in a row that make an outage long: past them a machine
/// that gave up on the address would never come back to it.
const LONG_OUTAGE_PROBES: u8 = 3;

/// Every path an answer can name with no probe of it outstanding.
const STALE_ANSWERABLE: [Path; 8] = [
    Path::Direct(LAN),
    Path::Direct(LAN6),
    Path::Forwarded(VIA),
    FRAMED,
    Path::Direct(PUNCH),
    Path::Punched(PUNCH),
    Path::Direct(STALE),
    Path::Forwarded(RELAY),
];

fn rtt_ms(path: Path) -> u32 {
    match path {
        Path::Forwarded(RELAY) => 60,
        Path::Forwarded(_) => 40,
        Path::Framed(_) => 20,
        Path::Direct(addr) | Path::Punched(addr) if addr == LAN || addr == LAN6 => 2,
        _ => 30,
    }
}

const fn addr(ip: [u8; 4]) -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(ip[0], ip[1], ip[2], ip[3])), 7000)
}

/// The path machine's world, bounded by how many times each thing may happen.
#[derive(Clone, Debug)]
pub struct PathModel {
    /// Addresses learned, of the three the world has.
    pub candidates: u8,
    pub probes_lost: u8,
    pub network_changes: u8,
    /// Plan changes: a lapse, or buying the plan again.
    pub lapses: u8,
    /// Times the clock moves on before the world stops changing.
    pub ticks: u8,
    /// Times the relay link goes down.
    pub forwarding_drops: u8,
    /// Times the peer's local address goes silent and answers again.
    pub outages: u8,
    /// Packets of the link arriving on a path that works.
    pub arrivals: u8,
    /// Addresses the peer stops listing.
    pub forgets: u8,
    /// Answers that come with no probe of the path outstanding: late,
    /// duplicated, or from before the network changed.
    pub stale_answers: u8,
    /// Refusals by the relay for a reason other than the plan: no route to
    /// the peer, the rate limit, or the relay shutting down.
    pub refusals: u8,
    /// Times a paired daemon's forwarding link comes up (and may go again).
    pub second_forwarder: u8,
    /// Whether the world offers a byte-stream path, which the machine
    /// learns of when one of the link's packets arrives on it.
    pub framed: bool,
    /// Whether the peer lists an IPv6 address on the local network beside
    /// its IPv4 one.
    pub ipv6: bool,
}

impl PathModel {
    /// The bounds `just test` explores: the addresses, losses, network,
    /// plan and relay link changing, and the local address going silent.
    pub fn ci() -> Self {
        Self {
            candidates: 2,
            probes_lost: 1,
            network_changes: 1,
            lapses: 1,
            ticks: 2,
            forwarding_drops: 1,
            outages: 1,
            arrivals: 0,
            forgets: 0,
            stale_answers: 0,
            refusals: 0,
            second_forwarder: 0,
            framed: false,
            ipv6: false,
        }
    }

    /// The bounds `just test` explores for the peer's own packets beside
    /// those: one arriving, on a byte stream the machine has not heard on or
    /// a path it sends on, and a paired daemon's forwarding link coming and
    /// going. The network stays put, the relay link stays up and the local
    /// address keeps answering, or the run would not fit its budget.
    pub fn ci_arrivals() -> Self {
        Self {
            network_changes: 0,
            forwarding_drops: 0,
            outages: 0,
            arrivals: 1,
            second_forwarder: 1,
            framed: true,
            ..Self::ci()
        }
    }

    /// The bounds `just test` explores for what the peer and the relay say
    /// beside the probes: an answer with no probe outstanding, a refusal for
    /// no route, the rate limit or shutting down, an address the peer stops
    /// listing, and an IPv6 address as near as the IPv4 one. The relay link
    /// stays up and the local address keeps answering.
    pub fn ci_answers() -> Self {
        Self {
            forwarding_drops: 0,
            outages: 0,
            forgets: 1,
            stale_answers: 1,
            refusals: 1,
            ipv6: true,
            ..Self::ci()
        }
    }

    /// The nightly bounds of [`PathModel::ci`].
    pub fn nightly() -> Self {
        Self {
            candidates: 3,
            lapses: 2,
            ticks: 3,
            ..Self::ci()
        }
    }

    /// The nightly bounds of [`PathModel::ci_arrivals`]: the network may
    /// change.
    pub fn nightly_arrivals() -> Self {
        Self {
            network_changes: 1,
            ..Self::ci_arrivals()
        }
    }

    /// The nightly bounds of [`PathModel::ci_answers`]: the relay link may
    /// drop.
    pub fn nightly_answers() -> Self {
        Self {
            forwarding_drops: 1,
            ..Self::ci_answers()
        }
    }
}

/// One of the addresses the world can give the machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Candidate {
    /// Listed by the peer over a direct link.
    Lan,
    /// Listed too: the peer's IPv6 address on the same network.
    Lan6,
    /// Observed by the relay.
    Punch,
    /// Remembered as having worked.
    Stale,
}

impl Candidate {
    const ALL: [Candidate; 4] = [
        Candidate::Lan,
        Candidate::Lan6,
        Candidate::Punch,
        Candidate::Stale,
    ];

    fn addr(self) -> SocketAddr {
        match self {
            Candidate::Lan => LAN,
            Candidate::Lan6 => LAN6,
            Candidate::Punch => PUNCH,
            Candidate::Stale => STALE,
        }
    }

    fn source(self) -> Source {
        match self {
            Candidate::Lan | Candidate::Lan6 => Source::Listed,
            Candidate::Punch => Source::Observed,
            Candidate::Stale => Source::LastWorked,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// The machine learns an address.
    Learn(Candidate),
    /// The peer stops listing an address the machine holds.
    Forget(Candidate),
    /// A path answers the probe in flight on it.
    Answer(Path),
    /// An answer arrives on a path with no probe of it outstanding.
    StaleAnswer(Path),
    /// One of the link's packets arrives on a path that works.
    Arrive(Path),
    /// The relay refuses the link's packets for a reason other than the
    /// plan.
    RelayRefusesFor(ForwardRefusal),
    /// The probe at this place in flight is lost.
    Lose(usize),
    /// Time passes to the machine's next wake.
    Tick { ms: u64 },
    /// This host moves to another network.
    NetworkChanged,
    /// The relay link goes down.
    ForwardingDown,
    /// The relay link comes back.
    ForwardingUp,
    /// A paired daemon's forwarding link comes up.
    ViaUp,
    /// It goes down.
    ViaDown,
    /// The peer's local address stops answering, this host staying on its
    /// network.
    LanSilent,
    /// The peer's local address answers again, and the world stops
    /// changing.
    LanBack,
    /// The plan lapses, or is bought again.
    Plan { paid: bool },
    /// The relay refuses the link's packets for the lapsed plan.
    RelayRefuses,
    /// The first flight reached the peer on a path that works.
    LinkUp,
    /// No path works, and the link idles out.
    LinkDown,
    /// The world stops changing.
    Rest,
}

/// How much of the bounds a path has used.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
struct Used {
    candidates: u8,
    probes_lost: u8,
    network_changes: u8,
    lapses: u8,
    ticks: u8,
    forwarding_drops: u8,
    rest_ticks: u8,
    outages: u8,
    outage_ticks: u8,
    arrivals: u8,
    forgets: u8,
    stale_answers: u8,
    refusals: u8,
    second_forwarder: u8,
}

#[derive(Clone, PartialEq, Eq, Hash)]
pub struct State {
    paths: Paths,
    now_ms: u64,
    /// The last wake the machine asked for.
    armed: Option<u64>,
    /// The addresses the machine has been told and not dropped.
    learned: BTreeSet<Candidate>,
    /// Each probe the machine sent and the world has not answered, lost or
    /// let time out, with when it went.
    in_flight: BTreeMap<Path, u64>,
    /// Each probe the world will never answer that the machine still waits
    /// on, with when it went: an answer on its path now would be taken for
    /// its own, so none comes (the driver passes only the outstanding
    /// probe's nonce). Kept only at bounds with stale answers.
    lost: BTreeMap<Path, u64>,
    /// The paths whose probe the world answered since the network last
    /// changed, since the link last went down and since the path was last
    /// forgotten.
    answered: BTreeSet<PathKey>,
    /// The paths the machine last said to send on all at once, until it
    /// carries the link on one.
    on_all: Vec<Path>,
    /// When the relay path last proved itself to the machine (a probe on it
    /// answered, a packet on it arrived) or last gave the machine a reason
    /// to probe it within the interval (a probe on it lost, timed out or
    /// refused, the relay link coming up, the link coming up, the network
    /// changing, the plan bought): a probe sent and not yet answered counts
    /// for nothing.
    relay_ready_ms: Option<u64>,
    /// The path carrying the link, and since when.
    since: Option<(PathKey, u64)>,
    /// The paths whose last probe was lost, timed out or refused, until one
    /// on them is answered.
    failed: BTreeSet<PathKey>,
    /// This host is on the peer's local network.
    on_lan: bool,
    /// The peer's local address is silent though this host is on its
    /// network.
    lan_silent: bool,
    /// Probes on the local address lost or timed out in a row during the
    /// outage, up to [`LONG_OUTAGE_PROBES`].
    outage_misses: u8,
    /// The link came back to the local address after a long outage.
    returned: bool,
    /// This host's network has changed since the relay saw the peer, so
    /// the punched address no longer reaches it.
    moved: bool,
    relay_up: bool,
    /// A paired daemon's forwarding link is up.
    via_up: bool,
    paid: bool,
    /// The relay refused the link's packets for the plan since it was last
    /// bought and since the relay link last came up.
    relay_refused: bool,
    link_up: bool,
    rested: bool,
    used: Used,
    chose_direct: bool,
    chose_relayed: bool,
    /// The link was carried on a byte stream, through a paired daemon, and
    /// on the IPv6 address.
    chose_framed: bool,
    chose_via: bool,
    chose_v6: bool,
    /// The first time the machine put a packet on a path across networks
    /// after a lapse.
    crossed: Option<String>,
    /// The first time it carried the link on a path no probe was answered on.
    unanswered: Option<String>,
    /// The first time it left a path that still worked, as far as it could
    /// know, for another within the hold-down.
    swung: Option<String>,
    /// The first time it chose an IPv4 address while an IPv6 one as near
    /// was working.
    chose_v4: Option<String>,
    /// The machine asked for a dump.
    dumped: Option<String>,
}

/// A path apart from its kind, as the machine keys it: an address is one
/// path whether punched or direct.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum PathKey {
    Addr(SocketAddr),
    Forwarded(LinkId),
    Other,
}

impl From<Path> for PathKey {
    fn from(path: Path) -> Self {
        match path {
            Path::Direct(addr) | Path::Punched(addr) => PathKey::Addr(addr),
            Path::Forwarded(link) => PathKey::Forwarded(link),
            Path::Framed(_) => PathKey::Other,
        }
    }
}

impl State {
    /// Whether the path carries packets to the peer in the world now.
    fn works(&self, path: Path) -> bool {
        match path {
            Path::Direct(addr) | Path::Punched(addr) if addr == LAN => {
                self.on_lan && !self.lan_silent
            }
            Path::Direct(addr) | Path::Punched(addr) if addr == LAN6 => self.on_lan,
            Path::Direct(addr) | Path::Punched(addr) if addr == PUNCH => !self.moved,
            Path::Forwarded(RELAY) => self.relay_up && self.paid,
            Path::Forwarded(VIA) => self.via_up,
            Path::Framed(_) => path == FRAMED,
            Path::Direct(_) | Path::Punched(_) | Path::Forwarded(_) => false,
        }
    }

    /// Whether the machine may use the path at all: an address it was
    /// told and still holds, punching only while paid, and the relay while
    /// up and not refusing the plan.
    fn usable(&self, candidate: Candidate) -> bool {
        self.learned.contains(&candidate) && (candidate != Candidate::Punch || self.paid)
    }

    /// The paths the world would carry the link on now, that the machine
    /// may use: direct ones first.
    fn usable_working(&self) -> (bool, bool) {
        let direct = Candidate::ALL
            .iter()
            .any(|candidate| self.usable(*candidate) && self.works(Path::Direct(candidate.addr())));
        let relayed = (self.relay_up && !self.relay_refused && self.works(Path::Forwarded(RELAY)))
            || self.works(Path::Forwarded(VIA));
        (direct, relayed)
    }

    fn carrying(&self) -> Option<Path> {
        self.paths.carrying().copied()
    }

    /// Whether a packet on `path` crosses networks after a lapse: a punched
    /// path, or the relay once it refused the plan.
    fn crosses(&self, path: Path) -> bool {
        !self.paid
            && (matches!(path, Path::Punched(_))
                || (path == Path::Forwarded(RELAY) && self.relay_refused))
    }

    /// Whether the path works as far as the machine can know: a probe on it
    /// was answered since the network changed, none since was lost or timed
    /// out, and nothing took it away (a lapse, a refusal, a drop).
    fn still_working(&self, path: Path) -> bool {
        let key = PathKey::from(path);
        let held = match path {
            Path::Direct(addr) | Path::Punched(addr) => self
                .learned
                .iter()
                .any(|candidate| candidate.addr() == addr),
            Path::Forwarded(RELAY) => self.relay_up && !self.relay_refused,
            Path::Forwarded(_) => self.via_up,
            Path::Framed(_) => true,
        };
        held && self.answered.contains(&key) && !self.failed.contains(&key) && !self.crosses(path)
    }

    /// The world will not answer the probe on `path`, which the machine
    /// may still be waiting on.
    fn unanswerable(&mut self, path: Path, keep: bool) {
        if let Some(sent) = self.in_flight.remove(&path)
            && keep
        {
            self.lost.insert(path, sent);
        }
    }

    /// A probe on `path` was lost or timed out.
    fn missed(&mut self, path: Path) {
        self.failed.insert(path.into());
        if path == Path::Forwarded(RELAY) {
            self.relay_ready_ms = Some(self.now_ms);
        }
        if self.lan_silent && PathKey::from(path) == PathKey::Addr(LAN) {
            self.outage_misses = (self.outage_misses + 1).min(LONG_OUTAGE_PROBES);
        }
    }

    /// Whether the machine has put the link's packets on the relay since the
    /// plan lapsed: what makes the relay refuse.
    fn sent_on_relay(&self) -> bool {
        let relay = Path::Forwarded(RELAY);
        self.in_flight.contains_key(&relay)
            || self.carrying() == Some(relay)
            || self.on_all.contains(&relay)
    }
}

impl fmt::Debug for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("State")
            .field("now_ms", &self.now_ms)
            .field("carrying", &self.carrying())
            .field("on_all", &self.on_all)
            .field("in_flight", &self.in_flight)
            .field("answered", &self.answered)
            .field("learned", &self.learned)
            .field("on_lan", &self.on_lan)
            .field("lan_silent", &self.lan_silent)
            .field("moved", &self.moved)
            .field("relay_up", &self.relay_up)
            .field("paid", &self.paid)
            .field("relay_refused", &self.relay_refused)
            .field("link_up", &self.link_up)
            .field("rested", &self.rested)
            .field("crossed", &self.crossed)
            .field("unanswered", &self.unanswered)
            .field("swung", &self.swung)
            .field("via_up", &self.via_up)
            .field("chose_v4", &self.chose_v4)
            .field("dumped", &self.dumped)
            .finish()
    }
}

impl PathModel {
    /// Whether the world at these bounds has `path` at all.
    fn offers(&self, path: Path) -> bool {
        match path {
            Path::Direct(addr) | Path::Punched(addr) => addr != LAN6 || self.ipv6,
            Path::Forwarded(link) => link == RELAY || self.second_forwarder > 0,
            Path::Framed(_) => self.framed,
        }
    }

    /// Steps the machine with `input` and records what it asked for.
    fn step(&self, s: &mut State, input: Input) {
        let label = format!("{input:?}");
        let before = s.carrying();
        let mut out = Vec::new();
        s.paths.step(input, &mut out);
        for output in out {
            match output {
                Output::Probe { path } => {
                    if s.crosses(path) && s.crossed.is_none() {
                        s.crossed = Some(format!("probed {path:?} after {label}"));
                    }
                    s.in_flight.insert(path, s.now_ms);
                    s.lost.remove(&path);
                }
                Output::Carry { path } => {
                    if !s.answered.contains(&path.into()) && s.unanswered.is_none() {
                        s.unanswered = Some(format!("carried on {path:?} after {label}"));
                    }
                    if PathKey::from(path) == PathKey::Addr(LAN)
                        && s.still_working(Path::Direct(LAN6))
                        && s.chose_v4.is_none()
                    {
                        s.chose_v4 = Some(format!("carried on {path:?} after {label}"));
                    }
                    if path.is_forwarded() {
                        s.chose_relayed = true;
                    } else {
                        s.chose_direct = true;
                    }
                    match PathKey::from(path) {
                        PathKey::Other => s.chose_framed = true,
                        PathKey::Forwarded(VIA) => s.chose_via = true,
                        PathKey::Addr(LAN6) => s.chose_v6 = true,
                        _ => {}
                    }
                    if PathKey::from(path) == PathKey::Addr(LAN)
                        && !s.lan_silent
                        && s.outage_misses >= LONG_OUTAGE_PROBES
                    {
                        s.returned = true;
                    }
                    s.on_all.clear();
                }
                Output::SendOnAll { paths } => s.on_all = paths,
                // The world still knows an address the machine forgot: only
                // a lapse may take one away, and only one reached by
                // punching. Any other drop leaves the address usable, so a
                // machine that gave up on it fails to come back to it.
                Output::DropCandidate { addr } => {
                    if addr == PUNCH && !s.paid {
                        s.learned.remove(&Candidate::Punch);
                    }
                    s.answered.remove(&PathKey::Addr(addr));
                }
                Output::Arm { at_ms } => s.armed = Some(at_ms),
                Output::Dump { reason } => {
                    if s.dumped.is_none() {
                        s.dumped = Some(format!("{reason:?} after {label}"));
                    }
                }
            }
        }
        let after = s.carrying();
        if let (Some(from), Some(to)) = (before, after)
            && PathKey::from(from) != PathKey::from(to)
            && let Some((_, since)) = s.since
            && s.still_working(from)
            && s.now_ms < since + HOLD_DOWN_MS
            && s.swung.is_none()
        {
            s.swung = Some(format!(
                "left {from:?} for {to:?} {} ms after it began carrying, on {label}",
                s.now_ms - since
            ));
        }
        let carried = after.map(PathKey::from);
        if carried != s.since.map(|(key, _)| key) {
            s.since = carried.map(|key| (key, s.now_ms));
        }
        let still: Vec<Path> = s
            .carrying()
            .into_iter()
            .chain(s.on_all.iter().copied())
            .collect();
        if let Some(path) = still.into_iter().find(|path| s.crosses(*path))
            && s.crossed.is_none()
        {
            s.crossed = Some(format!("still sending on {path:?} after {label}"));
        }
    }

    /// Whether time may pass now, and to when.
    fn wake(&self, s: &State) -> Option<u64> {
        let left = if s.rested {
            s.used.rest_ticks < REST_TICKS
        } else {
            s.used.ticks < self.ticks || (s.lan_silent && s.used.outage_ticks < OUTAGE_TICKS)
        };
        // An answer comes within its round trip: time passing a working
        // path's probe deadline would lose the probe without the fault.
        let answerable = s
            .in_flight
            .iter()
            .filter(|(path, _)| s.works(**path))
            .map(|(_, sent)| sent + PROBE_TIMEOUT_MS)
            .min();
        s.armed
            .filter(|at| left && *at > s.now_ms && answerable.is_none_or(|deadline| *at < deadline))
    }
}

impl Machine for PathModel {
    type State = State;
    type Action = Action;

    fn init(&self) -> State {
        let mut s = State {
            paths: Paths::with_budget(0, true, StepBudget::UNLIMITED),
            now_ms: 0,
            armed: None,
            learned: BTreeSet::new(),
            in_flight: BTreeMap::new(),
            lost: BTreeMap::new(),
            answered: BTreeSet::new(),
            on_all: Vec::new(),
            relay_ready_ms: None,
            since: None,
            failed: BTreeSet::new(),
            on_lan: true,
            lan_silent: false,
            outage_misses: 0,
            returned: false,
            moved: false,
            relay_up: true,
            via_up: false,
            paid: true,
            relay_refused: false,
            link_up: false,
            rested: false,
            used: Used::default(),
            chose_direct: false,
            chose_relayed: false,
            chose_framed: false,
            chose_via: false,
            chose_v6: false,
            crossed: None,
            unanswered: None,
            swung: None,
            chose_v4: None,
            dumped: None,
        };
        self.step(&mut s, Input::ForwardingUp { link: RELAY });
        s
    }

    fn actions(&self, s: &State, out: &mut Vec<Action>) {
        // While the local address is silent nothing else changes, and once
        // it answers again the world rests, so the outage's many ticks do
        // not multiply every other schedule.
        let world = !s.rested && !s.lan_silent;
        if s.lan_silent && !s.rested {
            out.push(Action::Rest);
            out.push(Action::LanBack);
        }
        if world {
            out.push(Action::Rest);
            if s.used.candidates < self.candidates {
                for candidate in Candidate::ALL {
                    if !s.learned.contains(&candidate)
                        && (self.ipv6 || candidate != Candidate::Lan6)
                    {
                        out.push(Action::Learn(candidate));
                    }
                }
            }
            if s.used.probes_lost < self.probes_lost {
                out.extend((0..s.in_flight.len()).map(Action::Lose));
            }
            if s.used.network_changes < self.network_changes {
                out.push(Action::NetworkChanged);
            }
            if s.relay_up && s.used.forwarding_drops < self.forwarding_drops {
                out.push(Action::ForwardingDown);
            }
            if s.carrying() == Some(Path::Direct(LAN)) && s.used.outages < self.outages {
                out.push(Action::LanSilent);
            }
            if s.used.lapses < self.lapses {
                out.push(Action::Plan { paid: !s.paid });
            }
            if s.used.forgets < self.forgets {
                out.extend(s.learned.iter().map(|candidate| Action::Forget(*candidate)));
            }
            if s.used.stale_answers < self.stale_answers {
                out.extend(
                    STALE_ANSWERABLE
                        .into_iter()
                        .filter(|path| {
                            let key = PathKey::from(*path);
                            self.offers(*path)
                                && !s
                                    .in_flight
                                    .keys()
                                    .chain(s.lost.keys())
                                    .any(|waiting| PathKey::from(*waiting) == key)
                        })
                        .map(Action::StaleAnswer),
                );
            }
            if s.link_up && s.used.arrivals < self.arrivals {
                let sending = s.carrying().into_iter().chain(s.on_all.iter().copied());
                let mut arriving: Vec<Path> = sending.filter(|path| s.works(*path)).collect();
                // The peer may send on a byte stream this host has not
                // heard on yet.
                if self.framed && !arriving.contains(&FRAMED) {
                    arriving.push(FRAMED);
                }
                arriving.dedup();
                out.extend(arriving.into_iter().map(Action::Arrive));
            }
            if s.via_up {
                out.push(Action::ViaDown);
            } else if s.used.second_forwarder < self.second_forwarder {
                out.push(Action::ViaUp);
            }
            if s.relay_up && s.sent_on_relay() && s.used.refusals < self.refusals {
                out.extend(
                    [
                        ForwardRefusal::NoRoute,
                        ForwardRefusal::RateLimited,
                        ForwardRefusal::ShuttingDown,
                    ]
                    .map(Action::RelayRefusesFor),
                );
            }
        }
        for path in s.in_flight.keys() {
            if s.works(*path) {
                out.push(Action::Answer(*path));
            }
        }
        if let Some(at) = self.wake(s) {
            out.push(Action::Tick { ms: at - s.now_ms });
        }
        if !s.relay_up {
            out.push(Action::ForwardingUp);
        }
        if !s.paid && s.relay_up && !s.relay_refused && s.sent_on_relay() {
            out.push(Action::RelayRefuses);
        }
        let sending: Vec<Path> = s
            .carrying()
            .into_iter()
            .chain(s.on_all.iter().copied())
            .collect();
        if !s.link_up && sending.iter().any(|path| s.works(*path)) {
            out.push(Action::LinkUp);
        }
        let (direct, relayed) = s.usable_working();
        if s.link_up && !direct && !relayed && !s.carrying().is_some_and(|path| s.works(path)) {
            out.push(Action::LinkDown);
        }
    }

    fn next(&self, s: &State, action: &Action) -> Option<State> {
        let mut next = s.clone();
        let keep = self.stale_answers > 0;
        let input = match action {
            Action::Rest => {
                next.rested = true;
                return Some(next);
            }
            Action::Learn(candidate) => {
                next.used.candidates += 1;
                // A free plan's machine ignores the relay's observation.
                if *candidate != Candidate::Punch || next.paid {
                    next.learned.insert(*candidate);
                }
                Input::Candidate {
                    addr: candidate.addr(),
                    source: candidate.source(),
                }
            }
            Action::Forget(candidate) => {
                next.used.forgets += 1;
                next.learned.remove(candidate);
                let key = PathKey::Addr(candidate.addr());
                let gone: Vec<Path> = next
                    .in_flight
                    .keys()
                    .filter(|path| PathKey::from(**path) == key)
                    .copied()
                    .collect();
                for path in gone {
                    next.unanswerable(path, keep);
                }
                next.answered.remove(&key);
                next.failed.remove(&key);
                Input::CandidateGone {
                    addr: candidate.addr(),
                }
            }
            Action::Answer(path) => {
                next.in_flight.remove(path);
                next.answered.insert((*path).into());
                next.failed.remove(&(*path).into());
                if *path == Path::Forwarded(RELAY) {
                    next.relay_ready_ms = Some(next.now_ms);
                }
                Input::ProbeAnswered {
                    path: *path,
                    rtt_ms: rtt_ms(*path),
                }
            }
            // The world records nothing: an answer to no outstanding probe
            // proves nothing about the path now.
            Action::StaleAnswer(path) => {
                next.used.stale_answers += 1;
                Input::ProbeAnswered {
                    path: *path,
                    rtt_ms: rtt_ms(*path),
                }
            }
            Action::Arrive(path) => {
                next.used.arrivals += 1;
                if *path == Path::Forwarded(RELAY) {
                    next.relay_ready_ms = Some(next.now_ms);
                }
                Input::PacketArrived { path: *path }
            }
            // The machine stops waiting on the relay's probe.
            Action::RelayRefusesFor(reason) => {
                next.used.refusals += 1;
                next.in_flight.remove(&Path::Forwarded(RELAY));
                next.failed.insert(PathKey::Forwarded(RELAY));
                next.relay_ready_ms = Some(next.now_ms);
                Input::ForwardRefused {
                    link: RELAY,
                    reason: *reason,
                }
            }
            Action::Lose(index) => {
                let path = *next.in_flight.keys().nth(*index)?;
                next.unanswerable(path, keep);
                next.missed(path);
                next.used.probes_lost += 1;
                return Some(next);
            }
            Action::Tick { ms } => {
                if next.rested {
                    next.used.rest_ticks += 1;
                } else if next.lan_silent && next.used.outage_ticks < OUTAGE_TICKS {
                    next.used.outage_ticks += 1;
                } else {
                    next.used.ticks += 1;
                }
                next.now_ms += ms;
                let now = next.now_ms;
                let expired: Vec<Path> = next
                    .in_flight
                    .iter()
                    .filter(|(_, sent)| **sent + PROBE_TIMEOUT_MS <= now)
                    .map(|(path, _)| *path)
                    .collect();
                for path in expired {
                    next.in_flight.remove(&path);
                    next.missed(path);
                }
                next.lost.retain(|_, sent| *sent + PROBE_TIMEOUT_MS > now);
                Input::Tick { now_ms: now }
            }
            Action::NetworkChanged => {
                next.used.network_changes += 1;
                next.on_lan = !next.on_lan;
                next.moved = true;
                next.relay_ready_ms = Some(next.now_ms);
                next.in_flight.clear();
                next.lost.clear();
                next.answered.clear();
                next.failed.clear();
                Input::NetworkChanged
            }
            Action::ForwardingDown => {
                next.used.forwarding_drops += 1;
                next.relay_up = false;
                next.relay_refused = false;
                next.unanswerable(Path::Forwarded(RELAY), keep);
                next.answered.remove(&PathKey::Forwarded(RELAY));
                next.relay_ready_ms = None;
                Input::ForwardingDown { link: RELAY }
            }
            Action::ViaUp => {
                next.used.second_forwarder += 1;
                next.via_up = true;
                Input::ForwardingUp { link: VIA }
            }
            Action::ViaDown => {
                next.via_up = false;
                next.unanswerable(Path::Forwarded(VIA), keep);
                next.answered.remove(&PathKey::Forwarded(VIA));
                next.failed.remove(&PathKey::Forwarded(VIA));
                Input::ForwardingDown { link: VIA }
            }
            Action::ForwardingUp => {
                next.relay_up = true;
                next.relay_ready_ms = Some(next.now_ms);
                Input::ForwardingUp { link: RELAY }
            }
            // Nothing tells the machine: it learns only from its probes.
            Action::LanSilent => {
                next.used.outages += 1;
                next.lan_silent = true;
                return Some(next);
            }
            Action::LanBack => {
                next.lan_silent = false;
                next.rested = true;
                return Some(next);
            }
            Action::Plan { paid } => {
                next.used.lapses += 1;
                next.paid = *paid;
                if *paid {
                    next.relay_refused = false;
                    next.relay_ready_ms = Some(next.now_ms);
                }
                Input::Plan { paid: *paid }
            }
            Action::RelayRefuses => {
                next.relay_refused = true;
                next.unanswerable(Path::Forwarded(RELAY), keep);
                next.failed.insert(PathKey::Forwarded(RELAY));
                Input::ForwardRefused {
                    link: RELAY,
                    reason: ForwardRefusal::PaymentRequired,
                }
            }
            Action::LinkUp => {
                next.link_up = true;
                next.relay_ready_ms = Some(next.now_ms);
                Input::LinkUp
            }
            // The machine stops waiting on its probes, and what answered for
            // the link that went proves nothing for the next, which probes
            // afresh.
            Action::LinkDown => {
                next.link_up = false;
                next.in_flight.clear();
                next.lost.clear();
                next.answered.clear();
                Input::LinkDown
            }
        };
        self.step(&mut next, input);
        Some(next)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        vec![
            Property::always(CARRIES_ONLY_ANSWERED.sentence, |_, s: &State| {
                s.unanswered.is_none()
                    && s.carrying()
                        .is_none_or(|path| s.link_up && s.answered.contains(&path.into()))
            }),
            Property::always(NO_CROSS_NETWORK_AFTER_LAPSE.sentence, |_, s: &State| {
                s.crossed.is_none()
            }),
            Property::eventually(
                "if any path works the link eventually uses one",
                |_, s: &State| {
                    let (direct, relayed) = s.usable_working();
                    s.rested
                        && (!(direct || relayed)
                            || (s.link_up && s.carrying().is_some_and(|path| s.works(path))))
                },
            ),
            Property::eventually(
                "a direct path that keeps answering is eventually chosen",
                |_, s: &State| {
                    let (direct, _) = s.usable_working();
                    s.rested
                        && (!direct
                            || (s.link_up
                                && s.carrying()
                                    .is_some_and(|path| !path.is_forwarded() && s.works(path))))
                },
            ),
            Property::always(NO_SWING.sentence, |_, s: &State| s.swung.is_none()),
            Property::always(
                "of two addresses as near, the link never chooses the IPv4 one while the IPv6 one works",
                |_, s: &State| s.chose_v4.is_none(),
            ),
            Property::always(RELAY_READY_WHILE_DIRECT.sentence, |_, s: &State| {
                let direct = s.link_up && s.carrying().is_some_and(|path| !path.is_forwarded());
                // The next probe is due an interval after the relay last
                // proved itself, and its answer comes within the probe
                // timeout.
                let ready = s
                    .relay_ready_ms
                    .is_some_and(|at| s.now_ms < at + PROBE_INTERVAL_MS + PROBE_TIMEOUT_MS);
                !direct || !s.relay_up || s.relay_refused || ready
            }),
            Property::always("the path machine never asks for a dump", |_, s: &State| {
                s.dumped.is_none()
            }),
            Property::sometimes(
                "direct and relayed are each sometimes chosen",
                |_, s: &State| s.chose_direct && s.chose_relayed,
            ),
            Property::sometimes(
                "the link is sometimes carried on a byte stream, where the world has one",
                |model: &PathModel, s: &State| !model.framed || s.chose_framed,
            ),
            Property::sometimes(
                "the link is sometimes carried through a paired daemon, where the world has one",
                |model: &PathModel, s: &State| model.second_forwarder == 0 || s.chose_via,
            ),
            Property::sometimes(
                "the link is sometimes carried on an IPv6 address, where the world has one",
                |model: &PathModel, s: &State| !model.ipv6 || s.chose_v6,
            ),
            Property::sometimes(
                "the link sometimes returns to an address after three probes in a row on it went unanswered",
                |model: &PathModel, s: &State| model.outages == 0 || s.returned,
            ),
        ]
    }
}
