//! Who a link socket's stand-ins stand for, and where each peer's packets
//! go.
//!
//! A profile has one UDP socket, and every link of the profile is a QUIC
//! connection on it. quinn never sees the network: it sends to a stand-in
//! address, one per peer, and the socket puts each packet on the path chosen
//! for that peer: straight to one of its addresses, through a forwarding
//! link (the relay's or a paired daemon's) behind a header naming the peer,
//! or over a byte stream. Arrivals are mapped back to the peer's stand-in
//! whichever path brought them, so a path change is invisible to quinn.
//!
//! [`Peers`] holds every decision that takes: which peer an arrival came
//! from, which path a peer's packets go out on, what a dial's first flight
//! has sent so far and where it must go again. It does no I/O and reads no
//! clock. A transmit or an arrival goes in, and what to send where comes
//! out; the node's link socket does the sending, and the models explore
//! these same decisions over a world of their own.
//!
//! The socket's own datagrams (the forwarding header, probes and their
//! answers) are told apart from QUIC by the fixed bit, which QUIC sets on
//! every packet unless the peer agreed to grease it; [`parse`] reads them.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt::Debug;
use std::hash::{Hash, Hasher};
use std::net::{IpAddr, Ipv6Addr, SocketAddr};

pub use path_core::{LinkId, Path, PathId};

pub type HostId = uuid::Uuid;

/// The largest QUIC datagram on every path. QUIC's smallest, so it fits on
/// every IPv6 path even behind the forwarding header (1,217 of the 1,232
/// bytes IPv6's minimum MTU leaves for UDP), and a path switch never needs
/// a new path MTU search.
pub const MAX_DATAGRAM: usize = 1200;

/// QUIC sets this bit in the first byte of every packet, long header or
/// short; our own datagrams clear it. No endpoint of ours offers to grease
/// it, so a clear fixed bit always means one of ours.
const FIXED_BIT: u8 = 0x40;
const KIND_FORWARDED: u8 = 0x00;
const KIND_PROBE: u8 = 0x01;
const KIND_PROBE_ANSWER: u8 = 0x02;
const KIND_DELIVERED: u8 = 0x03;

/// The bytes forwarding adds in front of a QUIC packet: the kind and a
/// host id, the same on the way to a forwarder and from it.
pub const FORWARD_HEADER: usize = 1 + 16;
pub const PROBE_LEN: usize = 1 + 16 + 8;

/// One datagram on the socket, told apart by its first byte.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PacketKind<'a> {
    Quic(&'a [u8]),
    /// A packet on its way to a forwarder, for it to forward to `host`.
    /// The payload is a QUIC packet or a probe.
    Forwarded {
        host: HostId,
        payload: &'a [u8],
    },
    /// A packet a forwarder delivers: `host` sent it. A kind of its own,
    /// since a daemon both forwards for its paired hosts and sends through
    /// them, so where a packet arrives from cannot say which it is.
    Delivered {
        host: HostId,
        payload: &'a [u8],
    },
    Probe {
        from: HostId,
        nonce: u64,
    },
    ProbeAnswer {
        from: HostId,
        nonce: u64,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Malformed {
    Empty,
    UnknownKind(u8),
    /// The datagram is not the length its kind requires.
    Length,
}

pub fn parse(datagram: &[u8]) -> Result<PacketKind<'_>, Malformed> {
    let Some(&first) = datagram.first() else {
        return Err(Malformed::Empty);
    };
    if first & FIXED_BIT != 0 {
        return Ok(PacketKind::Quic(datagram));
    }
    match first {
        KIND_FORWARDED | KIND_DELIVERED => {
            if datagram.len() <= FORWARD_HEADER {
                return Err(Malformed::Length);
            }
            let (host, payload) = (host_at(datagram), &datagram[FORWARD_HEADER..]);
            Ok(if first == KIND_FORWARDED {
                PacketKind::Forwarded { host, payload }
            } else {
                PacketKind::Delivered { host, payload }
            })
        }
        KIND_PROBE | KIND_PROBE_ANSWER => {
            if datagram.len() != PROBE_LEN {
                return Err(Malformed::Length);
            }
            let from = host_at(datagram);
            let nonce = u64::from_be_bytes(datagram[17..25].try_into().expect("eight bytes"));
            Ok(if first == KIND_PROBE {
                PacketKind::Probe { from, nonce }
            } else {
                PacketKind::ProbeAnswer { from, nonce }
            })
        }
        other => Err(Malformed::UnknownKind(other)),
    }
}

fn host_at(datagram: &[u8]) -> HostId {
    HostId::from_bytes(datagram[1..17].try_into().expect("sixteen bytes"))
}

/// `payload` on its way to a forwarder, for `dst`.
pub fn encode_forwarded(dst: HostId, payload: &[u8]) -> Vec<u8> {
    encode_behind_header(KIND_FORWARDED, dst, payload)
}

/// `payload` as a forwarder delivers it, from `sender`.
pub fn encode_delivered(sender: HostId, payload: &[u8]) -> Vec<u8> {
    encode_behind_header(KIND_DELIVERED, sender, payload)
}

fn encode_behind_header(kind: u8, host: HostId, payload: &[u8]) -> Vec<u8> {
    let mut datagram = Vec::with_capacity(FORWARD_HEADER + payload.len());
    datagram.push(kind);
    datagram.extend_from_slice(host.as_bytes());
    datagram.extend_from_slice(payload);
    datagram
}

pub fn encode_probe(from: HostId, nonce: u64) -> [u8; PROBE_LEN] {
    encode_probe_kind(KIND_PROBE, from, nonce)
}

pub fn encode_probe_answer(from: HostId, nonce: u64) -> [u8; PROBE_LEN] {
    encode_probe_kind(KIND_PROBE_ANSWER, from, nonce)
}

fn encode_probe_kind(kind: u8, from: HostId, nonce: u64) -> [u8; PROBE_LEN] {
    let mut datagram = [0; PROBE_LEN];
    datagram[0] = kind;
    datagram[1..17].copy_from_slice(from.as_bytes());
    datagram[17..].copy_from_slice(&nonce.to_be_bytes());
    datagram
}

/// How long a peer the socket learned of only from what arrived is kept
/// without being named or heard from. Every link's keepalives arrive more
/// often (twenty seconds at the longest, a phone's) and its idle timeout
/// is no longer (a phone's sixty), so a peer silent this long has no link.
pub const UNCLAIMED_PEER_TIMEOUT_MS: u64 = 60_000;
/// How often arrivals look for peers past [`UNCLAIMED_PEER_TIMEOUT_MS`].
pub const UNCLAIMED_PEER_SWEEP_MS: u64 = 5_000;

/// How often an arrival on one path from one host is reported to the path
/// machines: often enough to keep a working path from being probed, rare
/// enough that a busy link costs nothing.
pub const ARRIVAL_REPORT_EVERY_MS: u64 = 1_000;

/// How much of a link's first flight the socket keeps to send again on a
/// path that joins it late, as the relay joins once its own connection
/// exists: the opening packets with the Hello and the first calls, and
/// no more than a relay holds for one sender.
pub const FIRST_FLIGHT_PACKETS: usize = 32;
pub const FIRST_FLIGHT_BYTES: usize = 64 * 1024;

/// What a forwarding link is, as far as the socket needs to tell: whether
/// it reaches the relay, whose links join every first flight sent through
/// the relay.
pub trait Forwarding: Copy + Eq + Debug {
    fn relayed(&self) -> bool;
}

/// Stand-ins live in a unique-local IPv6 prefix (`fd61:6d75:7800::/48`)
/// that no real path uses, so a stand-in is never mistaken for an address.
/// The next 16 bits say what the last 64 hold: a counter, with every
/// stand-in on one port, or the IP of the address a sender was heard at.
pub const STAND_IN_PREFIX: [u16; 3] = [0xfd61, 0x6d75, 0x7800];
const STAND_IN_PORT: u16 = 1;
const COUNTED: u16 = 0;
const FROM_IPV4: u16 = 4;
const FROM_IPV6: u16 = 6;

fn stand_in_ip(kind: u16, bits: u64) -> IpAddr {
    let [a, b, c] = STAND_IN_PREFIX;
    IpAddr::V6(Ipv6Addr::new(
        a,
        b,
        c,
        kind,
        (bits >> 48) as u16,
        (bits >> 32) as u16,
        (bits >> 16) as u16,
        bits as u16,
    ))
}

fn counted_stand_in(index: u64) -> SocketAddr {
    SocketAddr::new(stand_in_ip(COUNTED, index), STAND_IN_PORT)
}

/// The one stand-in IP every sender from `ip` is given, each on a port of
/// its own. quinn validates an address by its IP alone (a Retry or
/// NEW_TOKEN token names the IP it was sent to), so a sender that comes
/// back from a new port on the same IP, as one does when its NAT gives it
/// a new mapping, presents a token the server honours. An IPv6 address is
/// folded into the 64 bits, so two could share a stand-in IP; their
/// stand-ins still differ by port.
fn stand_in_ip_for(ip: IpAddr) -> IpAddr {
    match ip.to_canonical() {
        IpAddr::V4(v4) => stand_in_ip(FROM_IPV4, u32::from(v4).into()),
        IpAddr::V6(v6) => {
            let bits = u128::from(v6);
            stand_in_ip(FROM_IPV6, (bits >> 64) as u64 ^ bits as u64)
        }
    }
}

/// Who a stand-in stands for. A sender that has not said who it is (the
/// first packets of a link someone dialled to us) is known only by the path
/// it came on, until [`Peers::name`] gives it its host.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Peer {
    Host(HostId),
    Unnamed(Path),
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
enum Route {
    /// Nowhere to send yet: quinn's packets are dropped, as a lost packet
    /// would be, and quinn sends them again.
    #[default]
    Unset,
    Carrying(Path),
    /// Every candidate at once, until a packet from the peer arrives on
    /// one, which carries the link from then on. The peer drops the copies
    /// as duplicate packets.
    All(Vec<Path>),
}

impl Route {
    fn paths(&self) -> &[Path] {
        match self {
            Route::Unset => &[],
            Route::Carrying(path) => std::slice::from_ref(path),
            Route::All(paths) => paths,
        }
    }

    /// The route no longer takes `path`.
    fn drop_path(&mut self, path: Path) {
        match self {
            Route::Carrying(carrying) if *carrying == path => *self = Route::Unset,
            Route::All(paths) => paths.retain(|candidate| *candidate != path),
            _ => {}
        }
    }
}

/// The largest QUIC datagram one path carries. Every path carries
/// [`MAX_DATAGRAM`]: the forwarding header was left room for under IPv6's
/// minimum MTU, and byte streams carry whatever they are given.
fn path_datagram(path: Path) -> usize {
    match path {
        Path::Direct(_) | Path::Punched(_) | Path::Forwarded(_) | Path::Framed(_) => MAX_DATAGRAM,
    }
}

/// A path that is a network address of the peer's own, rather than a
/// forwarder or a byte stream.
fn addressed(path: Path) -> bool {
    matches!(path, Path::Direct(_) | Path::Punched(_))
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct PeerState {
    /// Where quinn hears the peer now. A host's earlier stand-ins (an
    /// unnamed sender's it was later known by) stay mapped to it, so a
    /// connection quinn still holds on one keeps reaching it.
    stand_in: SocketAddr,
    route: Route,
    /// When a packet last came from the peer, or it was last dialled.
    heard_ms: u64,
    expiry: Expiry,
    /// What went out while the route sends on every candidate, for a
    /// path that joins it late.
    flight: Option<Flight>,
    /// The host's last link went: its route still sends what its closing
    /// connection has left, but no dial trusts it.
    released: bool,
}

/// A link's first flight, while no one path carries it yet.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
struct Flight {
    /// The relay's forwarding links join the candidates as they come.
    through_relay: bool,
    sent: VecDeque<Vec<u8>>,
    bytes: usize,
}

impl Flight {
    fn keep(&mut self, packet: &[u8]) {
        self.sent.push_back(packet.to_vec());
        self.bytes += packet.len();
        while self.sent.len() > FIRST_FLIGHT_PACKETS || self.bytes > FIRST_FLIGHT_BYTES {
            let Some(oldest) = self.sent.pop_front() else {
                break;
            };
            self.bytes -= oldest.len();
        }
    }
}

/// When the socket lets a peer go by itself. A sender anyone can make the
/// socket mint a stand-in for, by sending a packet from a new address or
/// delivering one in a new host's name, is let go once it has nothing
/// left to say, so a burst of them leaves nothing behind.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Expiry {
    /// Kept until it is forgotten: a host this host named, routed or
    /// dialled, or a sender on a byte stream, which goes with the stream.
    Kept,
    /// Let go after [`UNCLAIMED_PEER_TIMEOUT_MS`] without a packet: an
    /// address dialled before the handshake says who answers there, a host
    /// first heard of in a forwarder's delivery, and a host whose last link
    /// to the relay went.
    Silent,
    /// An unnamed sender that dialled this host: let go when silent, or
    /// when its handshake has not named it [`UNCLAIMED_PEER_TIMEOUT_MS`]
    /// after it was first heard.
    Unnamed { first_ms: u64 },
}

/// Where an arrival came from, before the stand-in mapping.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Arrival {
    Udp(SocketAddr),
    Framed(PathId),
}

/// What the socket saw that the path machines act on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PathEvent<K> {
    /// A forwarding link this host sends through came up: through the
    /// relay, or through a paired daemon.
    ForwardingUp {
        link: LinkId,
        kind: K,
    },
    ForwardingDown {
        link: LinkId,
    },
    /// A QUIC packet from `peer` arrived on `path`; reported at most once
    /// a second per path and peer.
    Arrived {
        peer: HostId,
        path: Path,
    },
}

/// Where one datagram goes on the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Wire {
    pub to: WireTo,
    /// Sent to a forwarder for it to forward to this host: the datagram
    /// goes behind the forwarding header naming it.
    pub behind: Option<HostId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum WireTo {
    Udp(SocketAddr),
    Framed(PathId),
}

/// Why a datagram for a stand-in this socket never gave out has nowhere to
/// go.
pub const NOT_A_STAND_IN: &str = "not a stand-in address";

/// Why a datagram has nowhere to go on a path.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NoPath(pub &'static str);

/// Where one of quinn's datagrams goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Transmit {
    /// The destination is no stand-in of this socket's.
    NotStandIn,
    /// The peer has no route yet: the datagram is dropped, as a lost one
    /// would be, and quinn sends it again.
    Nowhere,
    One(Result<Wire, NoPath>),
    /// On every candidate of a first flight; one refusing is a lost copy.
    All(Vec<Result<Wire, NoPath>>),
}

/// A first flight to send again, on the path that now carries it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Again {
    pub peer: Peer,
    pub path: Path,
    pub sent: Vec<Vec<u8>>,
}

/// A QUIC packet that arrived: the stand-in quinn hears it from, a first
/// flight it moved that must go again, and what the path machines hear of
/// it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Heard<K> {
    pub stand_in: SocketAddr,
    pub again: Option<Again>,
    pub event: Option<PathEvent<K>>,
}

/// What a forwarding link coming up, or pointing somewhere new, changed:
/// what the path machines hear, and the first flights to send through it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForwardingAdded<K> {
    pub events: Vec<PathEvent<K>>,
    pub again: Vec<Again>,
}

/// Every peer one link socket knows: stand-ins, routes, addresses,
/// forwarding links and byte-stream paths.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Peers<K> {
    next_stand_in: u64,
    /// The port the last stand-in given for an address took.
    next_port: u16,
    /// When arrivals next look for unclaimed peers past their time.
    next_sweep_ms: Option<u64>,
    peers: HashMap<Peer, PeerState>,
    by_stand_in: HashMap<SocketAddr, Peer>,
    /// The host each direct or punched address leads to, learned from the
    /// routes this host chose and the senders it named. An address is one
    /// host's at a time, and a host lets go of its addresses when it is
    /// named at a new one or its route is removed, so an address a host
    /// left and another took (a reused LAN address, a NAT's port given to
    /// someone else) never maps to the host that left.
    by_addr: HashMap<SocketAddr, HostId>,
    by_framed: HashMap<PathId, HostId>,
    /// Each forwarding link's forwarder, by a stand-in of the forwarder's,
    /// and what kind of path a link carried through it is.
    forwarding: HashMap<LinkId, (SocketAddr, K)>,
    framed: HashSet<PathId>,
    next_path: u64,
    /// Whether anything hears arrivals: none are reported while nothing
    /// does.
    reporting: bool,
    /// When an arrival on each path from each host was last reported.
    reported: HashMap<(HostId, Path), u64>,
}

impl<K> Default for Peers<K> {
    fn default() -> Self {
        Self {
            next_stand_in: 0,
            next_port: 0,
            next_sweep_ms: None,
            peers: HashMap::new(),
            by_stand_in: HashMap::new(),
            by_addr: HashMap::new(),
            by_framed: HashMap::new(),
            forwarding: HashMap::new(),
            framed: HashSet::new(),
            next_path: 0,
            reporting: false,
            reported: HashMap::new(),
        }
    }
}

/// Hashed in key order, so equal states hash equally whatever order their
/// maps were filled in: the models keep states in a set.
impl<K: Hash> Hash for Peers<K> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        fn sorted<'a, A: Ord + 'a, B: 'a>(
            map: impl Iterator<Item = (&'a A, &'a B)>,
        ) -> Vec<(&'a A, &'a B)> {
            let mut entries: Vec<_> = map.collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            entries
        }
        self.next_stand_in.hash(state);
        self.next_port.hash(state);
        self.next_sweep_ms.hash(state);
        sorted(self.peers.iter()).hash(state);
        sorted(self.by_stand_in.iter()).hash(state);
        sorted(self.by_addr.iter()).hash(state);
        sorted(self.by_framed.iter()).hash(state);
        sorted(self.forwarding.iter()).hash(state);
        let mut framed: Vec<_> = self.framed.iter().collect();
        framed.sort();
        framed.hash(state);
        self.next_path.hash(state);
        self.reporting.hash(state);
        sorted(self.reported.iter()).hash(state);
    }
}

impl<K: Forwarding> Peers<K> {
    /// The address quinn dials and hears `host` at. It changes only when
    /// the host is named afresh at a stand-in it was first heard at
    /// unnamed; the earlier one keeps reaching it.
    pub fn stand_in(&mut self, host: HostId, now_ms: u64) -> SocketAddr {
        self.claim(Peer::Host(host), now_ms)
    }

    /// The address quinn dials to reach whoever answers at `addr`, before
    /// the handshake says who that is: the stand-in arrivals from `addr`
    /// map to, so the dial and its answers meet on one address.
    pub fn stand_in_at(&mut self, addr: SocketAddr, now_ms: u64) -> SocketAddr {
        let peer = self.sender(Arrival::Udp(addr));
        self.claim(peer, now_ms)
    }

    /// The address quinn dials to reach whoever answers on the byte-stream
    /// path `path`: the stand-in arrivals on that path map to.
    pub fn stand_in_on(&mut self, path: PathId, now_ms: u64) -> SocketAddr {
        let peer = self.sender(Arrival::Framed(path));
        self.claim(peer, now_ms)
    }

    /// The host behind a stand-in, if it has said who it is.
    pub fn peer_of(&self, stand_in: SocketAddr) -> Option<HostId> {
        match self.by_stand_in.get(&stand_in)? {
            Peer::Host(host) => Some(*host),
            Peer::Unnamed(_) => None,
        }
    }

    /// The network address behind a stand-in, where it is one: an unnamed
    /// sender's address, or the address carrying a host's link. None for a
    /// byte stream or a forwarder.
    pub fn address_of(&self, stand_in: SocketAddr) -> Option<SocketAddr> {
        match self.path_of(stand_in)? {
            Path::Direct(addr) | Path::Punched(addr) => Some(addr),
            Path::Forwarded(_) | Path::Framed(_) => None,
        }
    }

    /// The path a stand-in's packets go out on, where there is one.
    pub fn path_of(&self, stand_in: SocketAddr) -> Option<Path> {
        match self.by_stand_in.get(&stand_in)? {
            Peer::Unnamed(path) => Some(*path),
            peer @ Peer::Host(_) => match &self.peers.get(peer)?.route {
                Route::Carrying(path) => Some(*path),
                Route::Unset | Route::All(_) => None,
            },
        }
    }

    /// Gives an unnamed sender's stand-in its host once the handshake says
    /// who it is; quinn keeps the same address, and the host's packets on
    /// that path arrive from it.
    ///
    /// A host already known is named afresh when the path is an address
    /// and the host's route is unset or another address: a host that
    /// connected again from elsewhere, or whose connection quinn moved to
    /// the address its packets now come from after its NAT rebound or its
    /// network changed. Its earlier stand-in goes on reaching it at the new
    /// address. A host carried some other way (through a forwarder, over a
    /// byte stream, or on every candidate) keeps that route, and this
    /// sender stays unnamed; its handshake is done, so it is let go only
    /// once it goes silent, not for going unnamed.
    ///
    /// A stand-in another host was last heard at, at an address, is taken
    /// from it: the address was reused (a LAN address given out again, a
    /// NAT's port given to someone else), and the host that left is
    /// forgotten until it is heard again.
    ///
    /// True when the stand-in is the host's.
    pub fn name(&mut self, stand_in: SocketAddr, host: HostId, now_ms: u64) -> bool {
        let (path, left) = match self.by_stand_in.get(&stand_in).copied() {
            Some(Peer::Unnamed(path)) => (path, None),
            Some(Peer::Host(named)) if named == host => return true,
            Some(Peer::Host(left)) => {
                let held = &self.peers[&Peer::Host(left)];
                match held.route {
                    Route::Carrying(path) if addressed(path) && held.stand_in == stand_in => {
                        (path, Some(left))
                    }
                    _ => return false,
                }
            }
            None => return false,
        };
        if let Some(known) = self.peers.get(&Peer::Host(host)) {
            let readdressed = match &known.route {
                Route::Unset => true,
                Route::Carrying(carrying) => {
                    known.released || (addressed(*carrying) && addressed(path))
                }
                Route::All(_) => false,
            };
            if !readdressed {
                if let Some(sender) = self.peers.get_mut(&Peer::Unnamed(path)) {
                    sender.expiry = Expiry::Silent;
                }
                return false;
            }
        }
        match left {
            Some(left) => self.forget(Peer::Host(left)),
            None => {
                self.peers.remove(&Peer::Unnamed(path));
            }
        }
        self.mint(Peer::Host(host), now_ms, Expiry::Kept).stand_in = stand_in;
        self.by_stand_in.insert(stand_in, Peer::Host(host));
        if addressed(path) {
            self.unlearn_addresses(host);
        }
        self.set_route(host, Route::Carrying(path), now_ms);
        true
    }

    /// `host`'s last link went. Its route stays, so what the closing
    /// connection still sends goes out, but no dial trusts it: the next
    /// dial's first flight goes out on every candidate rather than only on
    /// the path the old link last used, which may be dead, and a connection
    /// from the host on any path names it afresh. Its next link, or any
    /// route set, takes the release back.
    pub fn release(&mut self, host: HostId) {
        if let Some(held) = self.peers.get_mut(&Peer::Host(host)) {
            held.released = true;
        }
    }

    /// `host`'s last link to this relay went: it is let go, with every
    /// stand-in it was known by and its addresses, once it has been silent
    /// for [`UNCLAIMED_PEER_TIMEOUT_MS`]. Until then a link from it finds
    /// its stand-in where it was. A relay sees a host at a new stand-in each
    /// time its NAT rebinds or it relaunches, so a host it never let go of
    /// would leave one more behind each time, for the life of the relay.
    pub fn let_go(&mut self, host: HostId) {
        if let Some(held) = self.peers.get_mut(&Peer::Host(host)) {
            held.expiry = Expiry::Silent;
        }
    }

    /// `host` holds a link to this relay again: it is kept until it is let
    /// go.
    pub fn keep(&mut self, host: HostId) {
        if let Some(held) = self.peers.get_mut(&Peer::Host(host)) {
            held.expiry = Expiry::Kept;
        }
    }

    /// `host` has a link again: its route is trusted once more.
    pub fn reclaim(&mut self, host: HostId) {
        if let Some(held) = self.peers.get_mut(&Peer::Host(host)) {
            held.released = false;
        }
    }

    /// Whether `host`'s route is one a dial should trust: something
    /// carries it on one path and its last link has not gone since.
    pub fn trusted_route(&self, host: HostId) -> Option<Path> {
        let held = self.peers.get(&Peer::Host(host))?;
        match held.route {
            Route::Carrying(path) if !held.released => Some(path),
            _ => None,
        }
    }

    /// The path carrying `host` now, when one path does.
    pub fn carrying(&self, host: HostId) -> Option<Path> {
        match &self.peers.get(&Peer::Host(host))?.route {
            Route::Carrying(path) => Some(*path),
            Route::Unset | Route::All(_) => None,
        }
    }

    /// Carries `host`'s packets on one path.
    pub fn set_carrying(&mut self, host: HostId, path: Path, now_ms: u64) {
        self.set_route(host, Route::Carrying(path), now_ms);
    }

    /// Sends none of `host`'s packets until a path is named again: the one
    /// carrying them is one this host may no longer use. A packet from the
    /// host on it does not bring it back.
    pub fn stop_carrying(&mut self, host: HostId) {
        if let Some(held) = self.peers.get_mut(&Peer::Host(host)) {
            held.route = Route::Unset;
            held.flight = None;
        }
    }

    /// Sends each of `host`'s packets on every one of `paths` until a
    /// packet from the host arrives on one, which carries the link from
    /// then on: a link's first flight, before anything says which path
    /// works. With `through_relay`, every forwarding link to the relay is
    /// a candidate too, one that comes up later as well; a path that joins
    /// late is sent the first flight's packets that went out before it.
    pub fn send_on_all(
        &mut self,
        host: HostId,
        mut paths: Vec<Path>,
        through_relay: bool,
        now_ms: u64,
    ) {
        if through_relay {
            for (link, (_, kind)) in &self.forwarding {
                let path = Path::Forwarded(*link);
                if kind.relayed() && !paths.contains(&path) {
                    paths.push(path);
                }
            }
        }
        self.set_route(host, Route::All(paths), now_ms);
        self.peers
            .get_mut(&Peer::Host(host))
            .expect("route just set")
            .flight = Some(Flight {
            through_relay,
            ..Flight::default()
        });
    }

    /// Dials `host` on every one of `paths` at once, and through the relay
    /// when `through_relay`, until a packet from the host arrives on one: a
    /// launch's first flight to a host, before anything says which path
    /// works. A host some path already carries, with a link up since its
    /// last went, is dialled on it. Answers the stand-in quinn dials.
    pub fn dial_on_all(
        &mut self,
        host: HostId,
        paths: Vec<Path>,
        through_relay: bool,
        now_ms: u64,
    ) -> SocketAddr {
        let stand_in = self.stand_in(host, now_ms);
        if self.trusted_route(host).is_none() {
            self.send_on_all(host, paths, through_relay, now_ms);
        }
        stand_in
    }

    /// Dials `host` through the forwarder at the far end of `through`: the
    /// relay, or a paired daemon. The packets go behind the header naming
    /// the host until a path of its own carries the link; a host some path
    /// already carries is dialled on it. A route left by a link that has
    /// gone may be a dead path: the dial goes through the forwarder it was
    /// asked to go through. Answers the stand-in quinn dials.
    pub fn dial_through(&mut self, host: HostId, through: LinkId, now_ms: u64) -> SocketAddr {
        let stand_in = self.stand_in(host, now_ms);
        if self.trusted_route(host).is_none() {
            self.set_carrying(host, Path::Forwarded(through), now_ms);
        }
        stand_in
    }

    /// A link to `host` came up on its connection at `stand_in`: whether
    /// that connection is what carries the host here; the caller then
    /// trusts its route again, under whatever orders that against the
    /// host's other links going (see [`Peers::reclaim`]). A link this host dialled by address (a pairing's, a
    /// discovered host's or a remembered address's) is still at the
    /// stand-in of whoever answered there: the handshake proved that is
    /// `host`, so the stand-in becomes its, and quinn keeps the address it
    /// dialled. A host carried some other way than this connection is not.
    pub fn linked(&mut self, host: HostId, stand_in: SocketAddr, now_ms: u64) -> bool {
        self.peer_of(stand_in) == Some(host)
            || (self.peer_of(stand_in).is_none()
                && matches!(self.path_of(stand_in), Some(Path::Direct(_)))
                && self.name(stand_in, host, now_ms))
    }

    /// The largest datagram every path of `host` carries: the smallest of
    /// its paths', so a switch between them never needs a smaller one.
    pub fn max_datagram(&self, host: HostId) -> usize {
        self.peers
            .get(&Peer::Host(host))
            .map(|held| held.route.paths())
            .unwrap_or_default()
            .iter()
            .copied()
            .map(path_datagram)
            .min()
            .unwrap_or(MAX_DATAGRAM)
    }

    /// Registers a byte-stream path; its packets arrive as
    /// [`Arrival::Framed`].
    pub fn add_framed(&mut self) -> PathId {
        let id = PathId(self.next_path);
        self.next_path += 1;
        self.framed.insert(id);
        id
    }

    /// Forgets a byte-stream path, the sender on it if it was never named,
    /// and the path of every host it carried: a host that comes back some
    /// other way is named where it comes back.
    pub fn remove_framed(&mut self, path: PathId) {
        self.framed.remove(&path);
        self.by_framed.remove(&path);
        self.forget(Peer::Unnamed(Path::Framed(path)));
        self.unroute(Path::Framed(path));
    }

    /// Names the forwarder a forwarding link reaches by its stand-in on
    /// this socket: packets for a peer carried on `Path::Forwarded(link)`
    /// go behind the header on whatever path carries the forwarder, UDP or
    /// a byte stream, and packets it delivers are answered through it.
    ///
    /// A link to the relay at a stand-in another link to the relay holds
    /// is that link named afresh: a connection to the relay is a
    /// forwarding link as soon as it carries data, so that links' first
    /// flights go through it at once, and is named again once the relay
    /// admits it. Routes through the old name follow.
    ///
    /// A link to the relay joins the first flight of every peer sending
    /// on all its candidates through the relay, and is sent what that
    /// flight sent before it. So is a link to the relay pointed somewhere
    /// new, as a relay dial moves it from the attempt that said its Hello
    /// first to the one it keeps: what went to the old address may have
    /// gone nowhere.
    pub fn add_forwarding(&mut self, link: LinkId, via: SocketAddr, kind: K) -> ForwardingAdded<K> {
        let mut events = Vec::new();
        let renamed: Vec<LinkId> = self
            .forwarding
            .iter()
            .filter(|(old, (at, was))| {
                **old != link && *at == via && was.relayed() && kind.relayed()
            })
            .map(|(old, _)| *old)
            .collect();
        for old in renamed {
            self.forwarding.remove(&old);
            events.push(PathEvent::ForwardingDown { link: old });
            for held in self.peers.values_mut() {
                match &mut held.route {
                    Route::Carrying(path) if *path == Path::Forwarded(old) => {
                        *path = Path::Forwarded(link);
                    }
                    Route::All(paths) => {
                        for path in paths.iter_mut() {
                            if *path == Path::Forwarded(old) {
                                *path = Path::Forwarded(link);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        let previous = self.forwarding.insert(link, (via, kind));
        if previous.is_none() {
            events.push(PathEvent::ForwardingUp { link, kind });
        }
        let moved = previous.is_some_and(|(at, _)| at != via);
        let mut again = Vec::new();
        if kind.relayed() {
            let path = Path::Forwarded(link);
            for (peer, held) in &mut self.peers {
                if let (Route::All(paths), Some(flight)) = (&mut held.route, &held.flight)
                    && flight.through_relay
                    && (moved || !paths.contains(&path))
                {
                    if !paths.contains(&path) {
                        paths.push(path);
                    }
                    again.push(Again {
                        peer: *peer,
                        path,
                        sent: flight.sent.iter().cloned().collect(),
                    });
                }
            }
        }
        ForwardingAdded { events, again }
    }

    /// Forgets a forwarding link, and with it the path through it of every
    /// peer it carried: a link dialled to one of them later goes through
    /// whichever forwarding link reaches the forwarder then.
    ///
    /// A host known only from the forwarder's deliveries, which this host
    /// never named, routed or dialled, is forgotten with it.
    pub fn remove_forwarding(&mut self, link: LinkId) -> Option<PathEvent<K>> {
        let removed = self
            .forwarding
            .remove(&link)
            .map(|_| PathEvent::ForwardingDown { link });
        self.unroute(Path::Forwarded(link));
        removed
    }

    /// The forwarding link whose forwarder stands behind `stand_in`.
    pub fn forwarding_link(&self, stand_in: SocketAddr) -> Option<LinkId> {
        self.forwarding_of(*self.by_stand_in.get(&stand_in)?)
    }

    /// What kind of path a link carried through `link` is.
    pub fn forwarded_kind(&self, link: LinkId) -> Option<K> {
        self.forwarding.get(&link).map(|(_, kind)| *kind)
    }

    /// Every forwarding link up now, with its kind.
    pub fn forwarding_links(&self) -> impl Iterator<Item = (LinkId, K)> + '_ {
        self.forwarding
            .iter()
            .map(|(link, (_, kind))| (*link, *kind))
    }

    /// Whether anything hears arrivals from now on.
    pub fn report_arrivals(&mut self, reporting: bool) {
        self.reporting = reporting;
    }

    /// Where one of quinn's datagrams to `destination` goes. While a first
    /// flight goes out on every candidate it keeps the datagram, for a path
    /// that joins it late.
    pub fn transmit(&mut self, destination: SocketAddr, contents: &[u8]) -> Transmit {
        let Some(peer) = self.by_stand_in.get(&destination).copied() else {
            return Transmit::NotStandIn;
        };
        let held = self.peers.get_mut(&peer).expect("a stand-in's peer");
        if let Some(flight) = &mut held.flight {
            flight.keep(contents);
        }
        match &held.route {
            Route::Unset => Transmit::Nowhere,
            Route::Carrying(path) => {
                let path = *path;
                Transmit::One(self.wire(peer, path))
            }
            Route::All(paths) => {
                let paths = paths.clone();
                Transmit::All(
                    paths
                        .into_iter()
                        .map(|path| self.wire(peer, path))
                        .collect(),
                )
            }
        }
    }

    /// Where a datagram for the peer behind `stand_in` goes as it is, not
    /// through quinn, on the path carrying it: how a forwarder puts a
    /// packet it forwards on the destination's link.
    pub fn raw(&self, stand_in: SocketAddr) -> Result<Wire, NoPath> {
        let peer = *self
            .by_stand_in
            .get(&stand_in)
            .ok_or(NoPath(NOT_A_STAND_IN))?;
        let path = match (&peer, &self.peers[&peer].route) {
            (Peer::Unnamed(path), _) | (Peer::Host(_), Route::Carrying(path)) => *path,
            _ => return Err(NoPath("no one path carries this peer")),
        };
        self.wire(peer, path)
    }

    /// Where a datagram for `peer` on `path` goes: straight to an address,
    /// over a byte stream, or behind the header to the forwarder a
    /// forwarding link reaches, on whatever path carries the forwarder.
    /// Forwarding is one hop: a forwarder is never reached through another.
    pub fn wire(&self, peer: Peer, path: Path) -> Result<Wire, NoPath> {
        match path {
            Path::Direct(addr) | Path::Punched(addr) => Ok(Wire {
                to: WireTo::Udp(addr),
                behind: None,
            }),
            Path::Framed(id) => self.framed_wire(id).map(|to| Wire { to, behind: None }),
            Path::Forwarded(link) => {
                let Peer::Host(host) = peer else {
                    return Err(NoPath(
                        "an unnamed sender cannot be reached through a forwarder",
                    ));
                };
                let (via, _) = self
                    .forwarding
                    .get(&link)
                    .ok_or(NoPath("no such forwarding link"))?;
                let forwarder = self
                    .by_stand_in
                    .get(via)
                    .ok_or(NoPath("the forwarder is gone"))?;
                let carrying = match (forwarder, &self.peers[forwarder].route) {
                    (Peer::Unnamed(path), _) | (Peer::Host(_), Route::Carrying(path)) => *path,
                    _ => return Err(NoPath("no one path carries the forwarder")),
                };
                let to = match carrying {
                    Path::Direct(addr) | Path::Punched(addr) => WireTo::Udp(addr),
                    Path::Framed(id) => self.framed_wire(id)?,
                    Path::Forwarded(_) => {
                        return Err(NoPath("a forwarder reached through a forwarder"));
                    }
                };
                Ok(Wire {
                    to,
                    behind: Some(host),
                })
            }
        }
    }

    fn framed_wire(&self, id: PathId) -> Result<WireTo, NoPath> {
        if self.framed.contains(&id) {
            Ok(WireTo::Framed(id))
        } else {
            Err(NoPath("no such byte-stream path"))
        }
    }

    /// A QUIC packet arrived straight from an address or on a byte stream.
    /// While the sender's first flight goes out on every candidate, the
    /// path it came on carries it from now on; a flight that never named
    /// that path goes again on it.
    pub fn arrived_quic(&mut self, arrival: Arrival, now_ms: u64) -> Heard<K> {
        let stand_in = self.heard_from(arrival, now_ms);
        let peer = self.sender(arrival);
        let path = match arrival {
            Arrival::Udp(addr) => Path::Direct(addr),
            Arrival::Framed(id) => Path::Framed(id),
        };
        let again = self
            .carried_by(peer, path)
            .map(|sent| Again { peer, path, sent });
        let event = self.arrived(peer, path, now_ms);
        Heard {
            stand_in,
            again,
            event,
        }
    }

    /// A QUIC packet `host` sent arrived through a forwarder. A link
    /// someone dialled to us through a forwarder is answered the way it
    /// came until a route is chosen. So is a handshake through a forwarder
    /// while another path carries the peer: the route is its last link's,
    /// and a host started afresh (an app opened again) may know no path to
    /// this one but the forwarder, while its last link, dead, has yet to
    /// time out here. The path machine moves the link back once the other
    /// path answers.
    pub fn arrived_delivered(
        &mut self,
        arrival: Arrival,
        host: HostId,
        handshake: bool,
        now_ms: u64,
    ) -> Heard<K> {
        self.sweep(now_ms);
        let stand_in = self.heard(Peer::Host(host), now_ms, Expiry::Silent);
        let mut again = None;
        let mut event = None;
        if let Some(link) = self.forwarder_of(arrival) {
            let path = Path::Forwarded(link);
            again = self.carried_by(Peer::Host(host), path).map(|sent| Again {
                peer: Peer::Host(host),
                path,
                sent,
            });
            event = self.arrived(Peer::Host(host), path, now_ms);
            let peer = self.peers.get_mut(&Peer::Host(host)).expect("stand-in");
            match peer.route {
                Route::Unset => peer.route = Route::Carrying(path),
                Route::Carrying(carrying) if carrying != path && handshake => {
                    self.set_route(host, Route::Carrying(path), now_ms);
                }
                _ => {}
            }
        }
        Heard {
            stand_in,
            again,
            event,
        }
    }

    /// A packet arrived for this host to forward: the stand-in of its
    /// sender, an unnamed one minted to wait for its handshake if it has
    /// none.
    pub fn arrived_to_forward(&mut self, arrival: Arrival, now_ms: u64) -> SocketAddr {
        self.heard_from(arrival, now_ms)
    }

    /// A known host probed this host, or answered its probe, from an
    /// address: its packets from there are its own from now on. A host's
    /// path machine probes a path before it carries the link there, and a
    /// packet it sends on a path to another of this host's addresses may
    /// leave from another of its own, which this host must know it by
    /// before the link's packets follow. An address a dial this host made
    /// is still unnamed at keeps its packets until that connection's
    /// handshake names it: quinn would drop its answers arriving as the
    /// host's. One that only sent this host something unnamed does not:
    /// a host started afresh hears from its peer's last connection to its
    /// last run at the address that peer then probes from.
    pub fn probed_from(&mut self, host: HostId, arrival: Arrival) {
        let dialled = |state: &PeerState| !matches!(state.expiry, Expiry::Unnamed { .. });
        if let Arrival::Udp(addr) = arrival
            && self.peers.contains_key(&Peer::Host(host))
            && self.by_addr.get(&addr) != Some(&host)
            && !self
                .peers
                .get(&Peer::Unnamed(Path::Direct(addr)))
                .is_some_and(dialled)
        {
            self.learn(host, Path::Direct(addr));
        }
    }

    /// What the socket holds for its peers: their states, stand-ins,
    /// addresses and forwarding links.
    pub fn held(&self) -> (usize, usize, usize, usize) {
        (
            self.peers.len(),
            self.by_stand_in.len(),
            self.by_addr.len(),
            self.forwarding.len(),
        )
    }

    /// The peer's stand-in, minted if it has none. This host asking for
    /// it claims a host and a dialled address: a host is kept from then
    /// on, an address until it goes silent.
    fn claim(&mut self, peer: Peer, now_ms: u64) -> SocketAddr {
        let claimed = match peer {
            Peer::Host(_) | Peer::Unnamed(Path::Framed(_)) => Expiry::Kept,
            Peer::Unnamed(_) => Expiry::Silent,
        };
        let state = self.mint(peer, now_ms, claimed);
        state.heard_ms = now_ms;
        if state.expiry != Expiry::Kept {
            state.expiry = claimed;
        }
        state.stand_in
    }

    /// The stand-in of a peer a packet just came from, minted as `minted`
    /// if it has none; the packet counts as hearing from it.
    fn heard(&mut self, peer: Peer, now_ms: u64, minted: Expiry) -> SocketAddr {
        let state = self.mint(peer, now_ms, minted);
        state.heard_ms = now_ms;
        state.stand_in
    }

    /// The peer's state, a stand-in minted for it with `expiry` if it has
    /// none.
    fn mint(&mut self, peer: Peer, now_ms: u64, expiry: Expiry) -> &mut PeerState {
        if !self.peers.contains_key(&peer) {
            let stand_in = self.new_stand_in(peer);
            let route = match peer {
                Peer::Host(_) => Route::Unset,
                Peer::Unnamed(path) => Route::Carrying(path),
            };
            self.by_stand_in.insert(stand_in, peer);
            self.peers.insert(
                peer,
                PeerState {
                    stand_in,
                    route,
                    heard_ms: now_ms,
                    expiry,
                    flight: None,
                    released: false,
                },
            );
        }
        self.peers.get_mut(&peer).expect("just minted")
    }

    /// A stand-in no peer holds: for a sender known by its address, one
    /// sharing the IP of every sender from that address's IP.
    fn new_stand_in(&mut self, peer: Peer) -> SocketAddr {
        if let Peer::Unnamed(Path::Direct(addr) | Path::Punched(addr)) = peer {
            let ip = stand_in_ip_for(addr.ip());
            for _ in 0..u16::MAX {
                self.next_port = self.next_port.wrapping_add(1).max(1);
                let stand_in = SocketAddr::new(ip, self.next_port);
                if !self.by_stand_in.contains_key(&stand_in) {
                    return stand_in;
                }
            }
        }
        let stand_in = counted_stand_in(self.next_stand_in);
        self.next_stand_in += 1;
        stand_in
    }

    /// The stand-in of the sender of an arrival, an unnamed one minted to
    /// wait for its handshake if it has none.
    fn heard_from(&mut self, arrival: Arrival, now_ms: u64) -> SocketAddr {
        self.sweep(now_ms);
        let peer = self.sender(arrival);
        let minted = match peer {
            Peer::Unnamed(Path::Framed(_)) => Expiry::Kept,
            _ => Expiry::Unnamed { first_ms: now_ms },
        };
        self.heard(peer, now_ms, minted)
    }

    /// Lets go of the peers past their time, at most once a sweep
    /// interval: a peer is only ever minted by an arrival or by this host,
    /// so arrivals are when there can be more of them.
    fn sweep(&mut self, now_ms: u64) {
        if self.next_sweep_ms.is_some_and(|next| now_ms < next) {
            return;
        }
        self.next_sweep_ms = Some(now_ms + UNCLAIMED_PEER_SWEEP_MS);
        let gone: Vec<Peer> = self
            .peers
            .iter()
            .filter(|(_, state)| {
                let silent = now_ms.saturating_sub(state.heard_ms) >= UNCLAIMED_PEER_TIMEOUT_MS;
                match state.expiry {
                    Expiry::Kept => false,
                    Expiry::Silent => silent,
                    Expiry::Unnamed { first_ms } => {
                        silent || now_ms.saturating_sub(first_ms) >= UNCLAIMED_PEER_TIMEOUT_MS
                    }
                }
            })
            .map(|(peer, _)| *peer)
            .collect();
        for peer in gone {
            self.forget(peer);
        }
    }

    /// Arrivals on `path` are `host`'s from now on. Whoever the address
    /// led to before loses it from its route: one address, one host.
    fn learn(&mut self, host: HostId, path: Path) {
        let previous = match path {
            Path::Direct(addr) | Path::Punched(addr) => self.by_addr.insert(addr, host),
            Path::Framed(id) => self.by_framed.insert(id, host),
            Path::Forwarded(_) => None,
        };
        if let Some(previous) = previous.filter(|previous| *previous != host)
            && let Some(peer) = self.peers.get_mut(&Peer::Host(previous))
        {
            peer.route.drop_path(path);
        }
    }

    /// No address leads to `host` any more: it left them all.
    fn unlearn_addresses(&mut self, host: HostId) {
        self.by_addr.retain(|_, owner| *owner != host);
    }

    /// Forgets `peer`: its stand-ins, a host's addresses and the
    /// forwarding links it was the forwarder of.
    fn forget(&mut self, peer: Peer) {
        let by_stand_in = &self.by_stand_in;
        self.forwarding
            .retain(|_, (via, _)| by_stand_in.get(via) != Some(&peer));
        self.by_stand_in.retain(|_, held| *held != peer);
        self.peers.remove(&peer);
        if let Peer::Host(host) = peer {
            self.unlearn_addresses(host);
            self.reported.retain(|(reported, _), _| *reported != host);
        }
    }

    /// Reports a packet from `peer` on `path`, unless one was this past
    /// second or nothing hears arrivals.
    fn arrived(&mut self, peer: Peer, path: Path, now_ms: u64) -> Option<PathEvent<K>> {
        let Peer::Host(host) = peer else {
            return None;
        };
        if !self.reporting {
            return None;
        }
        if self
            .reported
            .get(&(host, path))
            .is_some_and(|last| now_ms.saturating_sub(*last) < ARRIVAL_REPORT_EVERY_MS)
        {
            return None;
        }
        self.reported.insert((host, path), now_ms);
        Some(PathEvent::Arrived { peer: host, path })
    }

    /// Carries `host` on `route`. Addresses its earlier routes took still
    /// lead to it, so packets still in flight on a path it switched from
    /// arrive as its own.
    fn set_route(&mut self, host: HostId, route: Route, now_ms: u64) {
        self.claim(Peer::Host(host), now_ms);
        for path in route.paths() {
            self.learn(host, *path);
        }
        let peer = self
            .peers
            .get_mut(&Peer::Host(host))
            .expect("stand-in just made");
        if !matches!(route, Route::All(_)) {
            peer.flight = None;
        }
        peer.route = route;
        peer.released = false;
    }

    /// A packet from `peer` arrived on `path` while its first flight goes
    /// out on every candidate: that path carries it from now on, whether
    /// or not the flight named it, since it is where the peer answers (a
    /// host known at another address, or dialling this host itself while
    /// its dial found no candidate).
    ///
    /// Answers the flight sent so far when the flight never named that
    /// path, to be sent again on it: what went out only on the others,
    /// such as through a relay address nothing answers at, would otherwise
    /// wait out a retransmit. The packet that moved the route may not even
    /// be the dial's own, but the stray of a connection the peer still
    /// holds from this host's last run.
    fn carried_by(&mut self, peer: Peer, path: Path) -> Option<Vec<Vec<u8>>> {
        let held = self.peers.get_mut(&peer)?;
        let Route::All(paths) = &held.route else {
            return None;
        };
        let named = paths
            .iter()
            .copied()
            .find(|candidate| match (candidate, path) {
                (Path::Direct(a) | Path::Punched(a), Path::Direct(b) | Path::Punched(b)) => *a == b,
                (candidate, path) => *candidate == path,
            });
        held.route = Route::Carrying(named.unwrap_or(path));
        let flight = held.flight.take()?;
        named.is_none().then(|| flight.sent.into())
    }

    /// No route takes `gone` any more. A host carried only on it is left
    /// unrouted, and forgotten if this host never named, routed or dialled
    /// it.
    fn unroute(&mut self, gone: Path) {
        let mut left = Vec::new();
        for (peer, held) in &mut self.peers {
            match &mut held.route {
                Route::Carrying(path) if *path == gone => {
                    held.route = Route::Unset;
                    if held.expiry != Expiry::Kept {
                        left.push(*peer);
                    }
                }
                Route::All(paths) => paths.retain(|path| *path != gone),
                _ => {}
            }
        }
        for peer in left {
            self.forget(peer);
        }
    }

    /// The forwarding link whose forwarder is `peer`, if one is.
    fn forwarding_of(&self, peer: Peer) -> Option<LinkId> {
        self.forwarding
            .iter()
            .find(|(_, (via, _))| self.by_stand_in.get(via) == Some(&peer))
            .map(|(link, _)| *link)
    }

    /// The forwarding link whose forwarder sent this arrival, if one did.
    fn forwarder_of(&self, arrival: Arrival) -> Option<LinkId> {
        self.forwarding_of(self.sender(arrival))
    }

    /// The peer a QUIC packet came from: the host that path leads to, or an
    /// unnamed sender known by the path.
    fn sender(&self, arrival: Arrival) -> Peer {
        match arrival {
            Arrival::Udp(addr) => match self.by_addr.get(&addr) {
                Some(host) => Peer::Host(*host),
                None => Peer::Unnamed(Path::Direct(addr)),
            },
            Arrival::Framed(id) => match self.by_framed.get(&id) {
                Some(host) => Peer::Host(*host),
                None => Peer::Unnamed(Path::Framed(id)),
            },
        }
    }
}
